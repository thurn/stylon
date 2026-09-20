use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ra_ap_syntax::ast::{self, AstNode, HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{Edition, SourceFile};

use crate::config::Config;
use crate::diagnostic::{Diagnostic, OperationalError};
use crate::imports::RustInput;
use ra_ap_syntax::SyntaxNode;
use ra_ap_syntax::ast::Fn;
use toml_edit::{DocumentMut, Item};

#[derive(Debug, Default)]
pub(crate) struct TestLayoutAnalysis {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) errors: Vec<OperationalError>,
    pub(crate) replacements: BTreeMap<PathBuf, String>,
    pub(crate) creations: BTreeMap<PathBuf, String>,
    pub(crate) deletions: BTreeSet<PathBuf>,
}

pub(crate) fn analyze_file_suffix(
    config: &Config,
    inputs: &[RustInput<'_>],
    manifests: &BTreeMap<PathBuf, String>,
) -> TestLayoutAnalysis {
    let mut analysis = TestLayoutAnalysis::default();
    let declarations = parent_declaration_index(inputs);
    let existing: BTreeSet<_> = inputs
        .iter()
        .map(|input| input.path.to_path_buf())
        .collect();
    for input in inputs {
        if !config.rule_enabled("tests.file-suffix", &input.relative)
            || input
                .path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("_tests.rs"))
        {
            continue;
        }
        let parsed = SourceFile::parse(input.source, Edition::Edition2024);
        let functions: Vec<_> = parsed
            .tree()
            .items()
            .filter_map(|item| match item {
                ast::Item::Fn(function) if is_test_function(config, &function) => Some(function),
                _ => None,
            })
            .collect();
        if functions.is_empty() {
            continue;
        }
        let destination = input.path.with_file_name(format!(
            "{}_tests.rs",
            input
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("module")
        ));
        let range = text_range(functions[0].syntax());
        if is_special_entry(&input.relative) {
            extract_special_tests(config, input, &functions, destination, range, &mut analysis);
            continue;
        }
        if destination_conflicts(&destination, &existing, &analysis.creations) {
            analysis.errors.push(OperationalError {
                category: "planning",
                message: format!(
                    "test file destination already exists: {}",
                    destination.display()
                ),
                paths: vec![config.relative(&destination)],
            });
            continue;
        }
        if is_integration_test(&input.relative) {
            let Some((manifest, manifest_source)) = owning_manifest(input.path, manifests) else {
                analysis.errors.push(OperationalError {
                    category: "test-layout",
                    message: "an integration test move requires an owning Cargo.toml".to_owned(),
                    paths: vec![input.relative.clone()],
                });
                continue;
            };
            let target_name = input
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("UTF-8 paths were checked");
            let manifest_directory = manifest.parent().expect("manifest has a parent");
            let original_relative = input
                .path
                .strip_prefix(manifest_directory)
                .expect("test is below its owning manifest");
            let destination_relative = destination
                .strip_prefix(manifest_directory)
                .expect("test destination is below its owning manifest");
            let current_manifest = analysis
                .replacements
                .get(manifest)
                .unwrap_or(manifest_source);
            let Ok(replacement) = preserve_test_target(
                current_manifest,
                target_name,
                original_relative,
                destination_relative,
            ) else {
                analysis.errors.push(OperationalError {
                    category: "planning",
                    message: format!(
                        "Cargo test target `{target_name}` already points to a different path"
                    ),
                    paths: vec![config.relative(manifest), input.relative.clone()],
                });
                continue;
            };
            analysis.replacements.insert(manifest.clone(), replacement);
        } else if !rewrite_parent_declaration(
            &declarations,
            inputs,
            input.path,
            &destination,
            &mut analysis,
        ) {
            analysis.errors.push(OperationalError {
                category: "test-layout",
                message: format!(
                    "cannot find the external module declaration for {}",
                    input.relative.display()
                ),
                paths: vec![input.relative.clone()],
            });
            continue;
        }
        analysis
            .creations
            .insert(destination, input.source.to_owned());
        analysis.deletions.insert(input.path.to_path_buf());
        analysis.diagnostics.push(Diagnostic::new(
            "tests.file-suffix",
            "files containing test functions must end in `_tests.rs`",
            input.relative.clone(),
            input.source,
            range.start,
            range.end,
        ));
    }
    analysis
}

pub(crate) fn analyze_inline_tests(
    config: &Config,
    inputs: &[RustInput<'_>],
) -> TestLayoutAnalysis {
    let mut analysis = TestLayoutAnalysis::default();
    let existing: BTreeSet<_> = inputs
        .iter()
        .map(|input| input.path.to_path_buf())
        .collect();
    for input in inputs {
        if !config.rule_enabled("tests.no-inline-module", &input.relative) {
            continue;
        }
        let parsed = SourceFile::parse(input.source, Edition::Edition2024);
        let file = parsed.tree();
        let modules: Vec<_> = file
            .items()
            .filter_map(|item| match item {
                ast::Item::Module(module)
                    if module.name().is_some_and(|name| name.text() == "tests")
                        && module.item_list().is_some() =>
                {
                    Some(module)
                }
                _ => None,
            })
            .collect();
        if modules.is_empty() {
            continue;
        }
        let mut source = input.source.to_owned();
        let mut edits = Vec::new();
        for (index, module) in modules.iter().enumerate() {
            let destination = test_destination(input.path, index);
            if destination_conflicts(&destination, &existing, &analysis.creations) {
                analysis.errors.push(OperationalError {
                    category: "planning",
                    message: format!(
                        "inline test destination already exists: {}",
                        destination.display()
                    ),
                    paths: vec![config.relative(&destination)],
                });
                continue;
            }
            let Some(list) = module.item_list() else {
                continue;
            };
            let module_range = text_range(module.syntax());
            let list_range = text_range(list.syntax());
            let open = usize::from(
                list.l_curly_token()
                    .expect("item list has `{`")
                    .text_range()
                    .end(),
            );
            let close = usize::from(
                list.r_curly_token()
                    .expect("item list has `}`")
                    .text_range()
                    .start(),
            );
            let body = extracted_body(&input.source[open..close], &input.relative);
            let declaration = external_declaration(
                &input.source[module_range.clone()],
                destination.file_name().expect("destination has a name"),
            );
            edits.push((module_range.clone(), declaration));
            analysis.creations.insert(destination, body);
            analysis.diagnostics.push(Diagnostic::new(
                "tests.no-inline-module",
                "top-level inline test modules must be external files",
                input.relative.clone(),
                input.source,
                list_range.start,
                list_range.end,
            ));
        }
        edits.sort_by_key(|(range, _)| range.start);
        for (range, replacement) in edits.into_iter().rev() {
            source.replace_range(range, &replacement);
        }
        if source != input.source {
            analysis
                .replacements
                .insert(input.path.to_path_buf(), source);
        }
    }
    analysis
}

fn extract_special_tests(
    config: &Config,
    input: &RustInput<'_>,
    functions: &[Fn],
    destination: PathBuf,
    diagnostic_range: std::ops::Range<usize>,
    analysis: &mut TestLayoutAnalysis,
) {
    if functions
        .iter()
        .any(|function| !test_condition_implies_test(function))
    {
        analysis.errors.push(OperationalError {
            category: "test-layout",
            message: format!(
                "test functions in special Cargo entry {} must be conditional on `test`",
                input.relative.display()
            ),
            paths: vec![input.relative.clone()],
        });
        return;
    }
    if destination_conflicts(&destination, &BTreeSet::new(), &analysis.creations) {
        analysis.errors.push(OperationalError {
            category: "planning",
            message: format!(
                "test extraction destination already exists: {}",
                destination.display()
            ),
            paths: vec![config.relative(&destination)],
        });
        return;
    }
    let mut extracted = String::from("use crate::*;\n\n");
    let mut source = input.source.to_owned();
    let mut ranges: Vec<_> = functions
        .iter()
        .map(|function| text_range(function.syntax()))
        .collect();
    for range in &ranges {
        extracted.push_str(input.source[range.clone()].trim());
        extracted.push_str("\n\n");
    }
    extracted.truncate(extracted.trim_end().len());
    extracted.push('\n');
    ranges.sort_by_key(|range| range.start);
    for range in ranges.into_iter().rev() {
        let mut end = range.end;
        while input.source.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        source.replace_range(range.start..end, "");
    }
    if !source.ends_with('\n') {
        source.push('\n');
    }
    if !source.ends_with("\n\n") {
        source.push('\n');
    }
    let module_name = destination
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("UTF-8 paths were checked");
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .expect("UTF-8 paths were checked");
    source.push_str(&format!(
        "#[cfg(test)]\n#[path = \"{file_name}\"]\nmod {module_name};\n"
    ));
    analysis
        .replacements
        .insert(input.path.to_path_buf(), source);
    analysis.creations.insert(destination, extracted);
    analysis.diagnostics.push(Diagnostic::new(
        "tests.file-suffix",
        "test functions in Cargo entry files must be moved to a `_tests.rs` module",
        input.relative.clone(),
        input.source,
        diagnostic_range.start,
        diagnostic_range.end,
    ));
}

fn test_condition_implies_test(function: &Fn) -> bool {
    function.attrs().any(|attribute| {
        let compact: String = attribute
            .syntax()
            .text()
            .to_string()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        compact == "#[test]"
            || compact.starts_with("#[cfg(test)]")
            || compact
                .strip_prefix("#[cfg(all(")
                .is_some_and(|arguments| arguments.split([',', ')']).any(|part| part == "test"))
    })
}

fn is_test_function(config: &Config, function: &Fn) -> bool {
    function.attrs().any(|attribute| {
        let text = attribute.syntax().text().to_string();
        recognized_attribute(config, &text)
    })
}

fn recognized_attribute(config: &Config, text: &str) -> bool {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let inner = compact
        .strip_prefix("#[")
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(&compact);
    if let Some(arguments) = inner.strip_prefix("cfg_attr(") {
        return config.test_attributes.iter().any(|attribute| {
            arguments.contains(attribute)
                || attribute
                    .rsplit("::")
                    .next()
                    .is_some_and(|leaf| leaf == "test" && arguments.contains("::test"))
        });
    }
    let path = inner.split(['(', '=']).next().unwrap_or(inner).trim();
    config.test_attributes.contains(path) || path.rsplit("::").next() == Some("test")
}

fn is_integration_test(relative: &Path) -> bool {
    relative
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "tests")
}

fn owning_manifest<'a>(
    path: &Path,
    manifests: &'a BTreeMap<PathBuf, String>,
) -> Option<(&'a PathBuf, &'a String)> {
    manifests
        .iter()
        .filter(|(manifest, _)| {
            manifest
                .parent()
                .is_some_and(|directory| path.starts_with(directory))
        })
        .max_by_key(|(manifest, _)| manifest.components().count())
}

fn is_special_entry(relative: &Path) -> bool {
    let file = relative.file_name().and_then(|name| name.to_str());
    matches!(file, Some("lib.rs" | "main.rs" | "build.rs"))
        || relative.starts_with("src/bin")
        || relative.starts_with("examples")
        || relative.starts_with("benches")
}

fn preserve_test_target(
    source: &str,
    name: &str,
    original: &Path,
    destination: &Path,
) -> Result<String, ()> {
    let mut document = source.parse::<DocumentMut>().map_err(|_| ())?;
    let mut updated = false;
    if let Some(targets) = document
        .get_mut("test")
        .and_then(Item::as_array_of_tables_mut)
    {
        for target in targets.iter_mut() {
            if target.get("name").and_then(Item::as_str) != Some(name) {
                continue;
            }
            let path = target.get("path").and_then(Item::as_str);
            if path.is_some_and(|path| path != original.to_string_lossy()) {
                return Err(());
            }
            target["path"] = toml_edit::value(destination.to_string_lossy().as_ref());
            updated = true;
            break;
        }
    }
    if updated {
        return Ok(document.to_string());
    }
    let mut replacement = source.to_owned();
    if !replacement.ends_with('\n') {
        replacement.push('\n');
    }
    let name = toml_edit::Value::from(name).to_string();
    let path = toml_edit::Value::from(destination.to_string_lossy().as_ref()).to_string();
    replacement.push_str(&format!("\n[[test]]\nname = {name}\npath = {path}\n"));
    Ok(replacement)
}

fn destination_conflicts(
    destination: &Path,
    existing: &BTreeSet<PathBuf>,
    creations: &BTreeMap<PathBuf, String>,
) -> bool {
    let key = destination.to_string_lossy().to_lowercase();
    destination.exists()
        || existing
            .iter()
            .any(|path| path.to_string_lossy().to_lowercase() == key)
        || creations
            .keys()
            .any(|path| path.to_string_lossy().to_lowercase() == key)
}

fn rewrite_parent_declaration(
    declarations: &BTreeMap<PathBuf, Vec<ParentDeclaration>>,
    inputs: &[RustInput<'_>],
    source: &Path,
    destination: &Path,
    analysis: &mut TestLayoutAnalysis,
) -> bool {
    let Some(declaration) = declarations.get(source).and_then(|items| items.first()) else {
        return false;
    };
    let input = inputs
        .iter()
        .find(|input| input.path == declaration.owner)
        .expect("indexed parent input exists");
    let relative_destination = destination
        .strip_prefix(&declaration.directory)
        .expect("moved module stays in its module directory");
    let current = analysis
        .replacements
        .get(input.path)
        .map_or(input.source, String::as_str);
    let replacement = format!(
        "#[path = \"{}\"]\n{}",
        relative_destination.to_string_lossy(),
        declaration.text
    );
    let mut updated = current.to_owned();
    let current_start = updated
        .find(&declaration.text)
        .expect("external module declaration remains in virtual source");
    updated.replace_range(
        current_start..current_start + declaration.text.len(),
        &replacement,
    );
    analysis
        .replacements
        .insert(input.path.to_path_buf(), updated);
    true
}

#[derive(Clone, Debug)]
struct ParentDeclaration {
    owner: PathBuf,
    directory: PathBuf,
    text: String,
}

fn parent_declaration_index(inputs: &[RustInput<'_>]) -> BTreeMap<PathBuf, Vec<ParentDeclaration>> {
    let mut declarations: BTreeMap<PathBuf, Vec<ParentDeclaration>> = BTreeMap::new();
    for input in inputs {
        let parsed = SourceFile::parse(input.source, Edition::Edition2024);
        for module in parsed.tree().items().filter_map(|item| match item {
            ast::Item::Module(module) if module.item_list().is_none() => Some(module),
            _ => None,
        }) {
            let Some(name) = module.name().map(|name| name.text().to_string()) else {
                continue;
            };
            let directory = module_directory(input.path);
            let declaration = ParentDeclaration {
                owner: input.path.to_path_buf(),
                directory: directory.clone(),
                text: module.syntax().text().to_string(),
            };
            for candidate in [
                directory.join(format!("{name}.rs")),
                directory.join(name).join("mod.rs"),
            ] {
                declarations
                    .entry(candidate)
                    .or_default()
                    .push(declaration.clone());
            }
        }
    }
    declarations
}

fn module_directory(path: &Path) -> PathBuf {
    let parent = path.parent().expect("module file has a parent");
    match path.file_name().and_then(|name| name.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => parent.to_path_buf(),
        _ if is_automatic_target_root(path) => parent.to_path_buf(),
        _ => parent.join(path.file_stem().expect("module file has a stem")),
    }
}

fn is_automatic_target_root(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let directory = parent.file_name().and_then(|name| name.to_str());
    matches!(directory, Some("tests" | "examples" | "benches" | "bin"))
}

fn test_destination(source: &Path, index: usize) -> PathBuf {
    let parent = source.parent().expect("source has a parent");
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let stem = match file_name {
        "lib.rs" | "main.rs" | "build.rs" => "crate_root".to_owned(),
        "mod.rs" => parent
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("module")
            .to_owned(),
        _ => source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("module")
            .to_owned(),
    };
    let suffix = if index == 0 {
        String::new()
    } else {
        format!("_{}", index + 1)
    };
    parent.join(format!("{stem}{suffix}_tests.rs"))
}

fn external_declaration(module: &str, destination: &std::ffi::OsStr) -> String {
    let brace = module.find('{').expect("inline module has a brace");
    let prefix = module[..brace].trim_end();
    format!(
        "#[path = \"{}\"]\n{};",
        destination.to_string_lossy(),
        prefix
    )
}

fn extracted_body(body: &str, relative: &Path) -> String {
    let body = body.strip_prefix('\n').unwrap_or(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    let indent = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let body = body
        .lines()
        .map(|line| line.get(indent..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let parent = physical_module(relative);
    let parent_path = if parent.is_empty() {
        "crate".to_owned()
    } else {
        format!("crate::{}", parent.join("::"))
    };
    let mut body = rewrite_super_imports(&body, &parent_path);
    body.push('\n');
    body
}

fn rewrite_super_imports(body: &str, parent_path: &str) -> String {
    let parsed = SourceFile::parse(body, Edition::Edition2024);
    let mut edits: Vec<_> = parsed
        .syntax_node()
        .descendants()
        .filter_map(ast::Use::cast)
        .filter_map(|import| {
            let range = text_range(import.syntax());
            let text = &body[range.clone()];
            let offset = text.find("use super::")? + "use ".len();
            Some((
                range.start + offset..range.start + offset + "super".len(),
                parent_path,
            ))
        })
        .collect();
    edits.sort_by_key(|(range, _)| range.start);
    let mut rewritten = body.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        rewritten.replace_range(range, replacement);
    }
    rewritten
}

fn physical_module(relative: &Path) -> Vec<String> {
    let components: Vec<_> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    let Some(source) = components.iter().position(|component| component == "src") else {
        return Vec::new();
    };
    let mut module = components[source + 1..].to_vec();
    let Some(file) = module.pop() else {
        return Vec::new();
    };
    match file.as_str() {
        "lib.rs" | "main.rs" | "mod.rs" => {}
        _ => module.push(file.trim_end_matches(".rs").to_owned()),
    }
    module
}

fn text_range(node: &SyntaxNode) -> std::ops::Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

#[path = "tests_layout_tests.rs"]
#[cfg(test)]
mod tests;
