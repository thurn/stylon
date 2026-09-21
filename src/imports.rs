use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;

use ra_ap_syntax::ast::{self, AstNode, HasModuleItem, HasName, HasVisibility};
use ra_ap_syntax::{Edition, SourceFile};

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use crate::rules::{Edit, Finding};
use ra_ap_syntax::SyntaxNode;
use ra_ap_syntax::ast::BlockExpr;
use ra_ap_syntax::ast::Fn;
use ra_ap_syntax::ast::Item;
use ra_ap_syntax::ast::ItemList;
use ra_ap_syntax::ast::Module;
use ra_ap_syntax::ast::Use;
use ra_ap_syntax::ast::Visibility;

#[derive(Clone, Debug)]
pub struct RustInput<'a> {
    pub path: &'a Path,
    pub relative: PathBuf,
    pub source: &'a str,
}

#[derive(Debug, Default)]
pub struct PublicFunctionAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub replacements: BTreeMap<PathBuf, String>,
}

pub fn analyze_public_functions(
    config: &Config,
    inputs: &[RustInput<'_>],
) -> PublicFunctionAnalysis {
    let public_functions = public_function_index(inputs);
    let mut analysis = PublicFunctionAnalysis::default();
    for input in inputs {
        if !config.rule_enabled("imports.public-function", &input.relative) {
            continue;
        }
        let parsed = SourceFile::parse(input.source, Edition::Edition2024);
        let file = parsed.tree();
        let mut candidates = Vec::new();
        for import in file.syntax().descendants().filter_map(Use::cast) {
            if import.visibility().is_some() {
                continue;
            }
            let text = import.syntax().text().to_string();
            let Some((path, binding)) = simple_import(&text) else {
                continue;
            };
            let canonical = canonical_import_path(&path, input.relative.as_path());
            if !public_functions.contains(&canonical) && !standard_public_function(&canonical) {
                continue;
            }
            let parent = canonical
                .rsplit_once("::")
                .map(|(parent, _)| parent)
                .expect("function path has a parent");
            let qualifier = parent.rsplit("::").next().expect("module has a leaf");
            let function_name = canonical.rsplit("::").next().expect("function has a name");
            candidates.push(PublicFunctionImport {
                diagnostic_range: text_range(import.syntax()),
                removal_range: complete_line_range(input.source, text_range(import.syntax())),
                binding,
                function_name: function_name.to_owned(),
                qualifier: qualifier.to_owned(),
                module_path: parent.to_owned(),
            });
        }
        if candidates.is_empty() {
            continue;
        }
        let replacement = rewrite_public_function_imports(input.source, &file, &candidates);
        for candidate in &candidates {
            analysis.diagnostics.push(Diagnostic::new(
                "imports.public-function",
                "unrestricted public functions must be called through their module",
                input.relative.clone(),
                input.source,
                candidate.diagnostic_range.start,
                candidate.diagnostic_range.end,
            ));
        }
        analysis
            .replacements
            .insert(input.path.to_path_buf(), replacement);
    }
    analysis
}

pub fn check_top_level(config: &Config, relative: &Path, source: &str) -> Vec<Finding> {
    if !config.rule_enabled("imports.top-level", relative) {
        return Vec::new();
    }
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    let file = parsed.tree();
    let mut imports = Vec::new();
    for import in file.syntax().descendants().filter_map(ast::Use::cast) {
        if is_module_level(&import) {
            continue;
        }
        let text = import.syntax().text().to_string();
        imports.push(NestedImport {
            range: complete_line_range(source, text_range(import.syntax())),
            insertion: module_insertion(&file, &import, source),
            leaf: simple_leaf(&text),
            condition: cfg_attributes(&import),
            text,
        });
    }
    remove_conflicting_leaves(&mut imports);
    if imports.is_empty() {
        return Vec::new();
    }

    let mut edits: Vec<_> = imports
        .iter()
        .map(|import| Edit {
            range: import.range.clone(),
            replacement: String::new(),
        })
        .collect();
    let mut insertions: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for import in &imports {
        insertions
            .entry(import.insertion)
            .or_default()
            .insert(format!("{}{}\n", import.condition, import.text.trim()));
    }
    edits.extend(insertions.into_iter().map(|(offset, imports)| Edit {
        range: offset..offset,
        replacement: imports.into_iter().collect(),
    }));

    imports
        .into_iter()
        .enumerate()
        .map(|(index, import)| Finding {
            diagnostic: Diagnostic::new(
                "imports.top-level",
                "imports are permitted only at module top level",
                relative.to_path_buf(),
                source,
                import.range.start,
                import.range.end,
            ),
            edits: if index == 0 {
                std::mem::take(&mut edits)
            } else {
                Vec::new()
            },
        })
        .collect()
}

#[derive(Clone, Debug)]
struct NestedImport {
    range: std::ops::Range<usize>,
    insertion: usize,
    text: String,
    leaf: Option<String>,
    condition: String,
}

#[derive(Clone, Debug)]
struct PublicFunctionImport {
    diagnostic_range: std::ops::Range<usize>,
    removal_range: std::ops::Range<usize>,
    binding: String,
    function_name: String,
    qualifier: String,
    module_path: String,
}

fn public_function_index(inputs: &[RustInput<'_>]) -> BTreeSet<String> {
    let mut functions = BTreeSet::new();
    for input in inputs {
        let Some(module) = physical_module(&input.relative) else {
            continue;
        };
        let parsed = SourceFile::parse(input.source, Edition::Edition2024);
        collect_public_functions(parsed.tree().items().collect(), &module, &mut functions);
    }
    functions
}

fn collect_public_functions(items: Vec<Item>, module: &[String], functions: &mut BTreeSet<String>) {
    for item in items {
        match item {
            ast::Item::Fn(function) if unrestricted_public(function.visibility()) => {
                if let Some(name) = function.name() {
                    let mut path = vec!["crate".to_owned()];
                    path.extend_from_slice(module);
                    path.push(name.text().to_string());
                    functions.insert(path.join("::"));
                }
            }
            ast::Item::Module(module_item) => {
                if let Some(list) = module_item.item_list()
                    && let Some(name) = module_item.name()
                {
                    let mut nested = module.to_vec();
                    nested.push(name.text().to_string());
                    collect_public_functions(list.items().collect(), &nested, functions);
                }
            }
            _ => {}
        }
    }
}

fn unrestricted_public(visibility: Option<Visibility>) -> bool {
    visibility.is_some_and(|visibility| visibility.syntax().text().to_string().trim() == "pub")
}

fn simple_import(text: &str) -> Option<(String, String)> {
    if text.contains(['{', '*']) {
        return None;
    }
    let import = text
        .trim()
        .rsplit_once("use ")
        .map(|(_, import)| import)?
        .trim_end_matches(';');
    let (path, binding) = import.rsplit_once(" as ").map_or_else(
        || (import, import.rsplit("::").next().unwrap_or(import)),
        |(path, alias)| (path.trim(), alias.trim()),
    );
    Some((path.to_owned(), binding.to_owned()))
}

fn canonical_import_path(path: &str, relative: &Path) -> String {
    if path.starts_with("crate::") || path.starts_with("std::") {
        return path.to_owned();
    }
    if let Some(remainder) = path.strip_prefix("self::") {
        let mut module = physical_module(relative).unwrap_or_default();
        module.push(remainder.to_owned());
        return format!("crate::{}", module.join("::"));
    }
    path.to_owned()
}

fn rewrite_public_function_imports(
    source: &str,
    file: &ast::SourceFile,
    candidates: &[PublicFunctionImport],
) -> String {
    let mut edits = Vec::new();
    let mut modules = BTreeSet::new();
    for candidate in candidates {
        edits.push(Edit {
            range: candidate.removal_range.clone(),
            replacement: String::new(),
        });
        modules.insert(candidate.module_path.clone());
        for expression in file.syntax().descendants().filter_map(ast::PathExpr::cast) {
            let Some(path) = expression.path() else {
                continue;
            };
            if path.syntax().text().to_string() == candidate.binding
                && !expression
                    .syntax()
                    .ancestors()
                    .any(|ancestor| Use::can_cast(ancestor.kind()))
            {
                edits.push(Edit {
                    range: text_range(path.syntax()),
                    replacement: format!("{}::{}", candidate.qualifier, candidate.function_name),
                });
            }
        }
    }
    let insertion = insertion_for_items(file.items().collect(), source).unwrap_or(0);
    let imports = modules
        .into_iter()
        .filter(|module| !source.contains(&format!("use {module};")))
        .map(|module| format!("use {module};\n"))
        .collect::<String>();
    if !imports.is_empty() {
        edits.push(Edit {
            range: insertion..insertion,
            replacement: imports,
        });
    }
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    edits
        .dedup_by(|left, right| left.range == right.range && left.replacement == right.replacement);
    let mut replacement = source.to_owned();
    for edit in edits.into_iter().rev() {
        replacement.replace_range(edit.range, &edit.replacement);
    }
    replacement
}

fn standard_public_function(path: &str) -> bool {
    matches!(
        path,
        "std::cmp::max"
            | "std::cmp::min"
            | "std::convert::identity"
            | "std::fs::canonicalize"
            | "std::fs::copy"
            | "std::fs::create_dir"
            | "std::fs::create_dir_all"
            | "std::fs::read"
            | "std::fs::read_dir"
            | "std::fs::read_to_string"
            | "std::fs::remove_dir"
            | "std::fs::remove_dir_all"
            | "std::fs::remove_file"
            | "std::fs::rename"
            | "std::fs::set_permissions"
            | "std::fs::write"
            | "std::mem::drop"
            | "std::mem::forget"
            | "std::thread::sleep"
    )
}

fn physical_module(relative: &Path) -> Option<Vec<String>> {
    let components: Vec<_> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    let source = components.iter().position(|component| component == "src")?;
    let mut module = components[source + 1..].to_vec();
    let file = module.pop()?;
    match file.as_str() {
        "lib.rs" | "main.rs" | "mod.rs" => {}
        _ => module.push(file.trim_end_matches(".rs").to_owned()),
    }
    Some(module)
}

fn is_module_level(import: &Use) -> bool {
    import.syntax().parent().is_some_and(|parent| {
        ast::SourceFile::can_cast(parent.kind()) || ItemList::can_cast(parent.kind())
    })
}

fn module_insertion(file: &ast::SourceFile, import: &Use, source: &str) -> usize {
    if let Some(list) = import
        .syntax()
        .ancestors()
        .filter_map(ast::Module::cast)
        .find_map(|module| module.item_list())
    {
        return insertion_for_items(list.items().collect(), source).unwrap_or_else(|| {
            list.l_curly_token()
                .map_or(0, |token| usize::from(token.text_range().end()))
        });
    }
    insertion_for_items(file.items().collect(), source).unwrap_or(0)
}

fn insertion_for_items(items: Vec<Item>, source: &str) -> Option<usize> {
    let last_import = items.iter().rev().find_map(|item| match item {
        ast::Item::Use(import) => Some(text_range(import.syntax()).end),
        _ => None,
    });
    if let Some(end) = last_import {
        return Some(end + usize::from(source.as_bytes().get(end) == Some(&b'\n')));
    }
    items
        .first()
        .map(|item| line_start(source, text_range(item.syntax()).start))
}

fn complete_line_range(source: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = line_start(source, range.start);
    let end = source[range.end..]
        .find('\n')
        .map_or(range.end, |offset| range.end + offset + 1);
    start..end
}

fn line_start(source: &str, offset: usize) -> usize {
    source[..offset].rfind('\n').map_or(0, |index| index + 1)
}

fn simple_leaf(import: &str) -> Option<String> {
    if import.contains(['{', '*']) {
        return None;
    }
    let import = import.trim().strip_prefix("use ")?.trim_end_matches(';');
    Some(
        import
            .rsplit_once(" as ")
            .map_or_else(
                || import.rsplit("::").next().unwrap_or(import),
                |(_, alias)| alias,
            )
            .to_owned(),
    )
}

fn cfg_attributes(import: &Use) -> String {
    let mut attributes = BTreeSet::new();
    for ancestor in import.syntax().ancestors().skip(1) {
        if Module::can_cast(ancestor.kind()) || ast::SourceFile::can_cast(ancestor.kind()) {
            break;
        }
        if !BlockExpr::can_cast(ancestor.kind()) && !Fn::can_cast(ancestor.kind()) {
            continue;
        }
        let text = ancestor.text().to_string();
        let prefix = text
            .split_once('{')
            .map_or(text.as_str(), |(prefix, _)| prefix);
        let mut rest = prefix;
        while let Some(start) = rest.find("#[cfg") {
            let candidate = &rest[start..];
            let Some(end) = candidate.find(']') else {
                break;
            };
            attributes.insert(candidate[..=end].to_owned());
            rest = &candidate[end + 1..];
        }
    }
    attributes
        .into_iter()
        .map(|attribute| format!("{attribute}\n"))
        .collect()
}

fn remove_conflicting_leaves(imports: &mut Vec<NestedImport>) {
    let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for import in imports.iter() {
        if let Some(leaf) = &import.leaf {
            paths
                .entry(leaf.clone())
                .or_default()
                .insert(import.text.clone());
        }
    }
    let conflicts: BTreeSet<_> = paths
        .into_iter()
        .filter_map(|(leaf, paths)| (paths.len() > 1).then_some(leaf))
        .collect();
    imports.retain(|import| {
        import
            .leaf
            .as_ref()
            .is_none_or(|leaf| !conflicts.contains(leaf))
    });
}

fn text_range(node: &SyntaxNode) -> std::ops::Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

#[path = "imports_tests.rs"]
#[cfg(test)]
mod tests;
