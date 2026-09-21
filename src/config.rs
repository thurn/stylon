use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::diagnostic::OperationalError;
use toml::Value;

pub const RULE_IDS: &[&str] = &[
    "cargo.dependency-order",
    "cargo.workspace-inheritance",
    "imports.absolute-crate-path",
    "imports.public-function",
    "imports.top-level",
    "items.blank-lines",
    "items.order",
    "path.enum-variant-qualification",
    "path.function-qualification",
    "path.type-qualification",
    "rustdoc.type-links",
    "tests.file-suffix",
    "tests.no-inline-module",
    "visibility.no-restricted",
];

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    exclude: GlobSet,
    rules: BTreeMap<String, bool>,
    overrides: Vec<Override>,
    pub validation_command: Vec<String>,
    pub validation_is_default: bool,
    pub constant_macros: HashSet<String>,
    pub test_attributes: HashSet<String>,
}

#[derive(Clone, Debug)]
struct Override {
    paths: GlobSet,
    rules: BTreeMap<String, bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u8,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    validation: Validation,
    #[serde(default)]
    macros: Macros,
    #[serde(default)]
    tests: Tests,
    #[serde(default)]
    rules: BTreeMap<String, bool>,
    #[serde(default)]
    overrides: Vec<OverrideFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Validation {
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Macros {
    #[serde(default)]
    constant_definitions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tests {
    #[serde(default)]
    additional_attributes: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideFile {
    paths: Vec<String>,
    #[serde(default)]
    rules: BTreeMap<String, bool>,
}

impl Config {
    pub fn load(requested: &Path, explicit: Option<&Path>) -> Result<Self, OperationalError> {
        let requested = canonicalize(requested, "discovery")?;
        let config_path = match explicit {
            Some(path) => Some(canonicalize(path, "configuration")?),
            None => find_ancestor_config(&requested)?,
        };
        let root = config_path.as_ref().map_or_else(
            || find_analysis_root(&requested),
            |path| {
                path.parent()
                    .expect("configuration has a parent")
                    .to_path_buf()
            },
        );
        if !requested.starts_with(&root) {
            return Err(error(
                "configuration",
                "the requested path is outside the configuration root",
                vec![requested],
            ));
        }

        let file = config_path
            .as_ref()
            .map(|path| read_config(path))
            .transpose()?
            .unwrap_or_default();
        if config_path.is_some() && file.version != 1 {
            return Err(error(
                "configuration",
                format!("configuration version must be 1, found {}", file.version),
                config_path.iter().cloned().collect(),
            ));
        }
        if file.validation.command.as_ref().is_some_and(Vec::is_empty) {
            return Err(error(
                "configuration",
                "validation command may not be empty",
                config_path.iter().cloned().collect(),
            ));
        }
        validate_rule_ids(&file.rules)?;

        let mut overrides = Vec::with_capacity(file.overrides.len());
        for entry in file.overrides {
            validate_rule_ids(&entry.rules)?;
            overrides.push(Override {
                paths: compile_globs(&entry.paths)?,
                rules: entry.rules,
            });
        }

        let mut constant_macros = HashSet::from(["thread_local".to_owned()]);
        constant_macros.extend(
            file.macros
                .constant_definitions
                .iter()
                .map(|name| normalize_macro(name)),
        );
        let mut test_attributes = HashSet::from([
            "test".to_owned(),
            "rstest".to_owned(),
            "test_case".to_owned(),
        ]);
        test_attributes.extend(
            file.tests
                .additional_attributes
                .iter()
                .map(|name| normalize_attribute(name)),
        );

        let validation_is_default = file.validation.command.is_none();
        Ok(Self {
            root,
            config_path,
            exclude: compile_globs(&file.exclude)?,
            rules: file.rules,
            overrides,
            validation_command: file.validation.command.unwrap_or_else(|| {
                vec![
                    "cargo".to_owned(),
                    "check".to_owned(),
                    "--workspace".to_owned(),
                    "--all-targets".to_owned(),
                    "--all-features".to_owned(),
                ]
            }),
            validation_is_default,
            constant_macros,
            test_attributes,
        })
    }

    pub fn relative(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root)
            .expect("selected path is below root")
            .to_path_buf()
    }

    pub fn is_excluded(&self, relative: &Path) -> bool {
        self.exclude.is_match(normalized(relative))
    }

    pub fn rule_enabled(&self, rule_id: &str, relative: &Path) -> bool {
        let mut enabled = self.rules.get(rule_id).copied().unwrap_or(true);
        let relative = normalized(relative);
        for entry in &self.overrides {
            if entry.paths.is_match(&relative) {
                enabled = entry.rules.get(rule_id).copied().unwrap_or(enabled);
            }
        }
        enabled
    }
}

fn canonicalize(path: &Path, category: &'static str) -> Result<PathBuf, OperationalError> {
    fs::canonicalize(path).map_err(|source| {
        error(
            category,
            format!("cannot access {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })
}

fn find_ancestor_config(requested: &Path) -> Result<Option<PathBuf>, OperationalError> {
    let start = if requested.is_dir() {
        requested
    } else {
        requested.parent().expect("file has a parent")
    };
    let paths: Vec<_> = start
        .ancestors()
        .map(|ancestor| ancestor.join("stylon.toml"))
        .filter(|path| path.is_file())
        .collect();
    if paths.len() > 1 {
        return Err(error(
            "configuration",
            "multiple ancestor stylon.toml files apply to the requested path",
            paths,
        ));
    }
    Ok(paths.into_iter().next())
}

fn find_analysis_root(requested: &Path) -> PathBuf {
    let start = if requested.is_dir() {
        requested
    } else {
        requested.parent().expect("file has a parent")
    };
    let ancestors: Vec<_> = start.ancestors().collect();
    let mut nearest_package = None;
    for ancestor in &ancestors {
        let manifest = ancestor.join("Cargo.toml");
        let Ok(source) = fs::read_to_string(manifest) else {
            continue;
        };
        let Ok(value) = toml::from_str::<Value>(&source) else {
            continue;
        };
        if value.get("workspace").is_some() {
            return (*ancestor).to_path_buf();
        }
        if nearest_package.is_none() && value.get("package").is_some() {
            nearest_package = Some(*ancestor);
        }
    }
    nearest_package
        .or_else(|| {
            ancestors
                .iter()
                .find(|path| path.join(".git").exists())
                .copied()
        })
        .unwrap_or(start)
        .to_path_buf()
}

fn read_config(path: &Path) -> Result<ConfigFile, OperationalError> {
    let source = fs::read_to_string(path).map_err(|source| {
        error(
            "configuration",
            format!("cannot read {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })?;
    toml::from_str(&source).map_err(|source| {
        error(
            "configuration",
            format!("invalid {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })
}

fn validate_rule_ids(rules: &BTreeMap<String, bool>) -> Result<(), OperationalError> {
    let unknown: Vec<_> = rules
        .keys()
        .filter(|id| !RULE_IDS.contains(&id.as_str()))
        .cloned()
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(error(
            "configuration",
            format!("unknown rule IDs: {}", unknown.join(", ")),
            Vec::new(),
        ))
    }
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, OperationalError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        validate_pattern(pattern)?;
        let pattern = pattern
            .strip_suffix('/')
            .map_or_else(|| pattern.clone(), |value| format!("{value}/**"));
        let glob = Glob::new(&pattern).map_err(|source| {
            error(
                "configuration",
                format!("invalid glob {pattern:?}: {source}"),
                Vec::new(),
            )
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| {
        error(
            "configuration",
            format!("cannot compile globs: {source}"),
            Vec::new(),
        )
    })
}

fn validate_pattern(pattern: &str) -> Result<(), OperationalError> {
    let path = Path::new(pattern);
    if path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(error(
            "configuration",
            format!("glob must be relative and may not contain `..`: {pattern:?}"),
            Vec::new(),
        ));
    }
    Ok(())
}

fn normalized(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_macro(name: &str) -> String {
    name.strip_suffix('!').unwrap_or(name).to_owned()
}

fn normalize_attribute(name: &str) -> String {
    name.trim_start_matches("#[")
        .split(['(', ']'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn error(
    category: &'static str,
    message: impl Into<String>,
    paths: Vec<PathBuf>,
) -> OperationalError {
    OperationalError {
        category,
        message: message.into(),
        paths,
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod tests;
