use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::config::Config;
use crate::diagnostic::{Diagnostic, OperationalError};

#[derive(Clone, Debug)]
pub(crate) struct ManifestInput<'a> {
    pub(crate) path: &'a Path,
    pub(crate) relative: PathBuf,
    pub(crate) source: &'a str,
}

#[derive(Debug, Default)]
pub(crate) struct WorkspaceAnalysis {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) errors: Vec<OperationalError>,
    pub(crate) replacements: BTreeMap<PathBuf, String>,
}

pub(crate) fn analyze_workspace(
    config: &Config,
    manifests: &[ManifestInput<'_>],
) -> WorkspaceAnalysis {
    let root_path = config.root.join("Cargo.toml");
    let Some(root_input) = manifests.iter().find(|input| input.path == root_path) else {
        return WorkspaceAnalysis::default();
    };
    let Ok(root_document) = root_input.source.parse::<DocumentMut>() else {
        return WorkspaceAnalysis::default();
    };
    if root_document.get("workspace").is_none() {
        return WorkspaceAnalysis::default();
    }
    match workspace_members(config, &root_path) {
        Ok(members) => build_workspace_plan(config, root_input, manifests, &members),
        Err(error) => WorkspaceAnalysis {
            errors: vec![error],
            ..WorkspaceAnalysis::default()
        },
    }
}

fn build_workspace_plan(
    config: &Config,
    root_input: &ManifestInput<'_>,
    manifests: &[ManifestInput<'_>],
    members: &BTreeSet<PathBuf>,
) -> WorkspaceAnalysis {
    let mut root_document = root_input
        .source
        .parse::<DocumentMut>()
        .expect("root manifest was parsed");
    let catalog: BTreeMap<_, _> = root_document
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Item::as_table)
        .map(|dependencies| {
            dependencies
                .iter()
                .map(|(key, item)| (key.to_owned(), Policy::from_item(item)))
                .collect()
        })
        .unwrap_or_default();
    let mut groups: BTreeMap<String, Vec<DependencySpec>> = BTreeMap::new();

    for input in manifests {
        if input.path == root_input.path || !members.contains(input.path) {
            continue;
        }
        let Ok(document) = input.source.parse::<DocumentMut>() else {
            continue;
        };
        let Some(dependencies) = document.get("dependencies").and_then(Item::as_table) else {
            continue;
        };
        for (key, item) in dependencies {
            if !config.rule_enabled("cargo.workspace-inheritance", &input.relative)
                || is_path(item)
                || inherits_workspace(item)
            {
                continue;
            }
            groups
                .entry(key.to_owned())
                .or_default()
                .push(DependencySpec {
                    manifest: input.relative.clone(),
                    policy: Policy::from_item(item),
                });
        }
    }

    let mut conflicts = Vec::new();
    for (key, specs) in &groups {
        let first = &specs[0].policy;
        let catalog_policy = catalog.get(key);
        let catalog_conflict = catalog_policy.is_some_and(|policy| policy != first);
        if specs.iter().any(|spec| spec.policy != *first) || catalog_conflict {
            conflicts.extend(specs.iter().map(|spec| spec.manifest.clone()));
            if catalog_conflict {
                conflicts.push(PathBuf::from("Cargo.toml"));
            }
        }
    }
    if !conflicts.is_empty() {
        conflicts.sort();
        conflicts.dedup();
        return WorkspaceAnalysis {
            errors: vec![OperationalError {
                category: "workspace-dependency-conflict",
                message: "workspace dependency specifications have incompatible source policies"
                    .to_owned(),
                paths: conflicts,
            }],
            ..WorkspaceAnalysis::default()
        };
    }

    let mut analysis = WorkspaceAnalysis::default();
    for (key, specs) in &groups {
        if !catalog.contains_key(key) {
            let source_manifest = config.root.join(&specs[0].manifest);
            let source_input = manifests
                .iter()
                .find(|input| input.path == source_manifest)
                .expect("dependency source manifest exists");
            let source_document = source_input
                .source
                .parse::<DocumentMut>()
                .expect("member manifest was parsed");
            let source_item = source_document
                .get("dependencies")
                .and_then(Item::as_table)
                .and_then(|dependencies| dependencies.get(key))
                .expect("dependency item exists");
            workspace_dependencies_mut(&mut root_document)
                .insert(key, workspace_policy_item(source_item));
        }
    }

    for input in manifests {
        if input.path == root_input.path || !members.contains(input.path) {
            continue;
        }
        let Ok(mut document) = input.source.parse::<DocumentMut>() else {
            continue;
        };
        let Some(dependencies) = document
            .get_mut("dependencies")
            .and_then(Item::as_table_mut)
        else {
            continue;
        };
        let mut changed = false;
        for (key, specs) in &groups {
            if let Some(item) = dependencies.get_mut(key)
                && !is_path(item)
                && !inherits_workspace(item)
            {
                *item = inherited_item(item);
                changed = true;
                let range = find_dependency_range(input.source, key);
                analysis.diagnostics.push(Diagnostic::new(
                    "cargo.workspace-inheritance",
                    "external workspace member dependencies must inherit workspace policy",
                    input.relative.clone(),
                    input.source,
                    range.start,
                    range.end,
                ));
                debug_assert!(specs.iter().any(|spec| spec.manifest == input.relative));
            }
        }
        if changed {
            analysis
                .replacements
                .insert(input.path.to_path_buf(), document.to_string());
        }
    }

    let root_replacement = root_document.to_string();
    if root_replacement != root_input.source {
        analysis
            .replacements
            .insert(root_input.path.to_path_buf(), root_replacement);
    }
    analysis
}

#[derive(Clone, Debug)]
struct DependencySpec {
    manifest: PathBuf,
    policy: Policy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Policy {
    version: Option<String>,
    registry: Option<String>,
    git: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    rev: Option<String>,
    package: Option<String>,
    default_features: bool,
}

impl Policy {
    fn from_item(item: &Item) -> Self {
        let string = item.as_str().map(str::to_owned);
        Self {
            version: string.or_else(|| string_field(item, "version")),
            registry: string_field(item, "registry"),
            git: string_field(item, "git"),
            branch: string_field(item, "branch"),
            tag: string_field(item, "tag"),
            rev: string_field(item, "rev"),
            package: string_field(item, "package"),
            default_features: bool_field(item, "default-features").unwrap_or(true),
        }
    }
}

fn workspace_members(
    config: &Config,
    manifest: &Path,
) -> Result<BTreeSet<PathBuf>, OperationalError> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--frozen",
            "--manifest-path",
        ])
        .arg(manifest)
        .current_dir(&config.root)
        .output()
        .map_err(|source| OperationalError {
            category: "cargo-metadata",
            message: format!("cannot run Cargo metadata: {source}"),
            paths: vec![PathBuf::from("Cargo.toml")],
        })?;
    if !output.status.success() {
        return Err(OperationalError {
            category: "cargo-metadata",
            message: format!(
                "Cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            paths: vec![PathBuf::from("Cargo.toml")],
        });
    }
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).map_err(|source| OperationalError {
            category: "cargo-metadata",
            message: format!("cannot parse Cargo metadata: {source}"),
            paths: vec![PathBuf::from("Cargo.toml")],
        })?;
    let member_ids: BTreeSet<_> = metadata.workspace_members.into_iter().collect();
    Ok(metadata
        .packages
        .into_iter()
        .filter(|package| member_ids.contains(&package.id))
        .map(|package| package.manifest_path)
        .collect())
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    workspace_members: Vec<String>,
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: PathBuf,
}

fn workspace_dependencies_mut(document: &mut DocumentMut) -> &mut Table {
    let workspace = document
        .entry("workspace")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .expect("workspace is a table");
    workspace
        .entry("dependencies")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .expect("workspace dependencies is a table")
}

fn workspace_policy_item(item: &Item) -> Item {
    let mut item = item.clone();
    if let Some(table) = item.as_inline_table_mut() {
        table.remove("features");
        table.remove("optional");
        table.remove("workspace");
        table.fmt();
    }
    if let Some(table) = item.as_table_mut() {
        table.remove("features");
        table.remove("optional");
        table.remove("workspace");
    }
    item
}

fn inherited_item(item: &Item) -> Item {
    let mut inherited = InlineTable::new();
    inherited.insert("workspace", Value::from(true));
    if let Some(table) = item.as_inline_table() {
        for key in ["features", "optional"] {
            if let Some(value) = table.get(key) {
                inherited.insert(key, value.clone());
            }
        }
    }
    if let Some(table) = item.as_table() {
        for key in ["features", "optional"] {
            if let Some(value) = table.get(key).and_then(Item::as_value) {
                inherited.insert(key, value.clone());
            }
        }
    }
    Item::Value(Value::InlineTable(inherited))
}

fn string_field(item: &Item, key: &str) -> Option<String> {
    item.as_inline_table()
        .and_then(|table| table.get(key).and_then(Value::as_str))
        .or_else(|| {
            item.as_table()
                .and_then(|table| table.get(key).and_then(Item::as_str))
        })
        .map(str::to_owned)
}

fn bool_field(item: &Item, key: &str) -> Option<bool> {
    item.as_inline_table()
        .and_then(|table| table.get(key).and_then(Value::as_bool))
        .or_else(|| {
            item.as_table()
                .and_then(|table| table.get(key).and_then(Item::as_bool))
        })
}

fn is_path(item: &Item) -> bool {
    item.as_inline_table()
        .is_some_and(|table| table.contains_key("path"))
        || item
            .as_table()
            .is_some_and(|table| table.contains_key("path"))
}

fn inherits_workspace(item: &Item) -> bool {
    bool_field(item, "workspace").unwrap_or(false)
}

fn find_dependency_range(source: &str, key: &str) -> std::ops::Range<usize> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with(key) && trimmed[key.len()..].trim_start().starts_with('=') {
            let start = offset + line.len() - trimmed.len();
            return start..start + key.len();
        }
        offset += line.len();
    }
    0..0
}

#[path = "cargo_rules_tests.rs"]
#[cfg(test)]
mod tests;
