use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ra_ap_syntax::ast::{self, AstNode, HasModuleItem};
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

pub(crate) fn check_top_level(config: &Config, relative: &Path, source: &str) -> Vec<Finding> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::config::Config;
    use crate::imports::check_top_level;

    #[test]
    fn hoists_function_imports_to_the_file_module() {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers=[]\n",
        )
        .expect("manifest");
        let config = Config::load(directory.path(), None).expect("configuration");
        let source = "fn read() {\n    use crate::io::load;\n    load();\n}\n";
        let findings = check_top_level(&config, Path::new("src/lib.rs"), source);
        assert_eq!(findings.len(), 1);
        let mut fixed = source.to_owned();
        let mut edits = findings[0].edits.clone();
        edits.sort_by_key(|edit| edit.range.start);
        for edit in edits.into_iter().rev() {
            fixed.replace_range(edit.range, &edit.replacement);
        }
        assert_eq!(fixed, "use crate::io::load;\nfn read() {\n    load();\n}\n");
    }

    #[test]
    fn retains_cfg_condition_when_hoisting() {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers=[]\n",
        )
        .expect("manifest");
        let config = Config::load(directory.path(), None).expect("configuration");
        let source = "fn links() {\n    #[cfg(unix)]\n    {\n        use std::os::unix::fs::MetadataExt;\n        let _ = 1;\n    }\n}\n";
        let findings = check_top_level(&config, Path::new("src/lib.rs"), source);
        let insertion = findings[0]
            .edits
            .iter()
            .find(|edit| edit.range.is_empty())
            .expect("insertion");
        assert_eq!(
            insertion.replacement,
            "#[cfg(unix)]\nuse std::os::unix::fs::MetadataExt;\n"
        );
    }
}
