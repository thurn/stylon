use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ra_ap_syntax::ast::{self, AstNode, HasModuleItem, HasName};
use ra_ap_syntax::{Edition, SourceFile};

use crate::config::Config;
use crate::diagnostic::{Diagnostic, OperationalError};
use crate::imports::RustInput;
use ra_ap_syntax::SyntaxNode;

#[derive(Debug, Default)]
pub(crate) struct TestLayoutAnalysis {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) errors: Vec<OperationalError>,
    pub(crate) replacements: BTreeMap<PathBuf, String>,
    pub(crate) creations: BTreeMap<PathBuf, String>,
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
            if existing.contains(&destination) || analysis.creations.contains_key(&destination) {
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
