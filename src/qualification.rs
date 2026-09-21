use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path};

use ra_ap_syntax::ast::{self, AstNode, HasGenericParams, HasModuleItem, HasName};
use ra_ap_syntax::{Edition, SourceFile};

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use crate::rules::{Edit, Finding};
use ra_ap_syntax::SyntaxNode;
use ra_ap_syntax::ast::Enum;
use ra_ap_syntax::ast::Fn;
use ra_ap_syntax::ast::Impl;
use ra_ap_syntax::ast::Struct;
use ra_ap_syntax::ast::Trait;
use ra_ap_syntax::ast::TypeAlias;
use ra_ap_syntax::ast::Union;

pub fn check(config: &Config, relative: &Path, source: &str) -> Vec<Finding> {
    check_with_module(config, relative, relative, source)
}

pub fn check_with_module(
    config: &Config,
    relative: &Path,
    module_relative: &Path,
    source: &str,
) -> Vec<Finding> {
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    let file = parsed.tree();
    let bindings = imported_type_names(&file);
    let aliases = module_aliases(&file);
    let mut candidates = Vec::new();
    if config.rule_enabled("path.type-qualification", relative) {
        collect_type_paths(source, &file, &bindings, &aliases, &mut candidates);
    }
    collect_expression_paths(
        config,
        relative,
        &file,
        &bindings,
        &aliases,
        source,
        &mut candidates,
    );
    if config.rule_enabled("imports.absolute-crate-path", relative) {
        collect_relative_imports(module_relative, &file, &mut candidates);
    }
    remove_type_collisions(&mut candidates);
    candidates.sort_by_key(|candidate| candidate.range.start);
    candidates.dedup_by(|left, right| left.range == right.range);
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut edits: Vec<_> = candidates
        .iter()
        .map(|candidate| Edit {
            range: candidate.range.clone(),
            replacement: candidate.replacement.clone(),
        })
        .collect();
    let imports: BTreeSet<_> = candidates
        .iter()
        .filter_map(|candidate| candidate.import.clone())
        .collect();
    for (insertion, imports) in group_imports(imports) {
        let imports: Vec<_> = imports
            .into_iter()
            .filter(|import| !has_import_near(source, insertion, &import.path))
            .collect();
        if imports.is_empty() {
            continue;
        }
        edits.push(Edit {
            range: insertion..insertion,
            replacement: format_imports(&imports),
        });
    }

    candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| Finding {
            diagnostic: Diagnostic::new(
                candidate.rule_id,
                candidate.message,
                relative.to_path_buf(),
                source,
                candidate.range.start,
                candidate.range.end,
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
struct Candidate {
    rule_id: &'static str,
    message: &'static str,
    range: std::ops::Range<usize>,
    replacement: String,
    import: Option<PlannedImport>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PlannedImport {
    path: String,
    cfg_test: bool,
    insertion: usize,
    indent: String,
}

fn collect_type_paths(
    source: &str,
    file: &ast::SourceFile,
    bindings: &HashSet<String>,
    aliases: &BTreeMap<String, String>,
    candidates: &mut Vec<Candidate>,
) {
    for path_type in file.syntax().descendants().filter_map(ast::PathType::cast) {
        let Some(path) = path_type.path() else {
            continue;
        };
        let Some(segments) = simple_segments(&path) else {
            continue;
        };
        if segments.len() < 2
            || exempt_path_root(&segments[0], aliases)
            || generic_type_parameter(path.syntax(), &segments[0])
        {
            continue;
        }
        let leaf = segments.last().expect("path has a leaf").clone();
        if bindings.contains(&leaf) {
            continue;
        }
        candidates.push(Candidate {
            rule_id: "path.type-qualification",
            message: "type name must be unqualified",
            range: text_range(path.syntax()),
            replacement: leaf,
            import: Some(PlannedImport {
                path: canonical_path(&segments, aliases),
                cfg_test: false,
                insertion: scoped_import_insertion(file, path.syntax(), source).0,
                indent: scoped_import_insertion(file, path.syntax(), source).1,
            }),
        });
    }
}

fn collect_expression_paths(
    config: &Config,
    relative: &Path,
    file: &ast::SourceFile,
    bindings: &HashSet<String>,
    aliases: &BTreeMap<String, String>,
    source: &str,
    candidates: &mut Vec<Candidate>,
) {
    for expression in file.syntax().descendants().filter_map(ast::PathExpr::cast) {
        let Some(path) = expression.path() else {
            continue;
        };
        let Some(segments) = simple_segments(&path) else {
            continue;
        };
        if segments.len() < 3
            || exempt_path_root(&segments[0], aliases)
            || generic_type_parameter(path.syntax(), &segments[0])
        {
            continue;
        }
        let terminal = segments.last().expect("path has a terminal");
        let parent = &segments[segments.len() - 2];
        if bindings.contains(parent) {
            continue;
        }
        let is_call = expression
            .syntax()
            .parent()
            .and_then(ast::CallExpr::cast)
            .is_some();
        if parent.chars().next().is_some_and(char::is_uppercase) {
            if terminal.chars().next().is_some_and(char::is_uppercase)
                && config.rule_enabled("path.enum-variant-qualification", relative)
            {
                add_shortened(
                    candidates,
                    "path.enum-variant-qualification",
                    "enum variant may only be qualified by its enum type",
                    &path,
                    &segments,
                    aliases,
                    file,
                    source,
                );
            } else if is_call
                && config.rule_enabled("path.type-qualification", relative)
                && !matches!(
                    terminal.as_str(),
                    "default" | "from" | "try_from" | "from_str" | "from_iter"
                )
            {
                add_shortened(
                    candidates,
                    "path.type-qualification",
                    "associated type name must be unqualified",
                    &path,
                    &segments,
                    aliases,
                    file,
                    source,
                );
            }
        } else if is_call && config.rule_enabled("path.function-qualification", relative) {
            candidates.push(Candidate {
                rule_id: "path.function-qualification",
                message: "free function may have at most one module qualifier",
                range: text_range(path.syntax()),
                replacement: segments[segments.len() - 2..].join("::"),
                import: Some(PlannedImport {
                    path: canonical_path(&segments[..segments.len() - 1], aliases),
                    cfg_test: false,
                    insertion: scoped_import_insertion(file, path.syntax(), source).0,
                    indent: scoped_import_insertion(file, path.syntax(), source).1,
                }),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_shortened(
    candidates: &mut Vec<Candidate>,
    rule_id: &'static str,
    message: &'static str,
    path: &ast::Path,
    segments: &[String],
    aliases: &BTreeMap<String, String>,
    file: &ast::SourceFile,
    source: &str,
) {
    candidates.push(Candidate {
        rule_id,
        message,
        range: text_range(path.syntax()),
        replacement: segments[segments.len() - 2..].join("::"),
        import: Some(PlannedImport {
            path: canonical_path(&segments[..segments.len() - 1], aliases),
            cfg_test: false,
            insertion: scoped_import_insertion(file, path.syntax(), source).0,
            indent: scoped_import_insertion(file, path.syntax(), source).1,
        }),
    });
}

fn collect_relative_imports(
    relative: &Path,
    file: &ast::SourceFile,
    candidates: &mut Vec<Candidate>,
) {
    let Some(physical_module) = physical_module(relative) else {
        return;
    };
    for import in file.syntax().descendants().filter_map(ast::Use::cast) {
        let syntax = import.syntax().text().to_string();
        let Some(use_offset) = syntax.find("use ").map(|offset| offset + 4) else {
            continue;
        };
        let path_text = &syntax[use_offset..];
        let (parents, remainder) = if let Some(remainder) = path_text.strip_prefix("self::") {
            (0, remainder)
        } else {
            let count = path_text
                .as_bytes()
                .as_chunks::<7>()
                .0
                .iter()
                .take_while(|chunk| *chunk == b"super::")
                .count();
            if count == 0 {
                continue;
            }
            (count, &path_text[count * 7..])
        };
        let mut module = physical_module.clone();
        let mut inline: Vec<_> = import
            .syntax()
            .ancestors()
            .filter_map(ast::Module::cast)
            .filter_map(|module| module.name().map(|name| name.text().to_string()))
            .collect();
        inline.reverse();
        module.extend(inline);
        if parents > module.len() {
            continue;
        }
        module.truncate(module.len() - parents);
        let absolute = if module.is_empty() {
            format!("crate::{remainder}")
        } else {
            format!("crate::{}::{remainder}", module.join("::"))
        };
        let node_range = text_range(import.syntax());
        let start = node_range.start + use_offset;
        candidates.push(Candidate {
            rule_id: "imports.absolute-crate-path",
            message: "imports must use an absolute crate path",
            range: start..start + path_text.len(),
            replacement: absolute,
            import: None,
        });
    }
}

fn remove_type_collisions(candidates: &mut Vec<Candidate>) {
    let mut imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for candidate in candidates.iter() {
        if candidate.rule_id == "path.type-qualification"
            && !candidate.replacement.contains("::")
            && let Some(import) = &candidate.import
        {
            imports
                .entry(candidate.replacement.clone())
                .or_default()
                .insert(import.path.clone());
        }
    }
    let collisions: BTreeSet<_> = imports
        .into_iter()
        .filter_map(|(leaf, paths)| (paths.len() > 1).then_some(leaf))
        .collect();
    candidates.retain(|candidate| {
        candidate.rule_id != "path.type-qualification"
            || !collisions.contains(&candidate.replacement)
    });
}

fn imported_type_names(file: &ast::SourceFile) -> HashSet<String> {
    let mut names: HashSet<_> = ["Box", "Option", "Result", "String", "Vec"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    names.extend(file.items().filter_map(|item| match item {
        ast::Item::Enum(item) => item.name().map(|name| name.text().to_string()),
        ast::Item::Struct(item) => item.name().map(|name| name.text().to_string()),
        ast::Item::Trait(item) => item.name().map(|name| name.text().to_string()),
        ast::Item::TypeAlias(item) => item.name().map(|name| name.text().to_string()),
        ast::Item::Union(item) => item.name().map(|name| name.text().to_string()),
        _ => None,
    }));
    names.extend(
        file.syntax()
            .descendants()
            .filter_map(ast::PathType::cast)
            .filter_map(|path_type| path_type.path())
            .filter_map(|path| {
                let segments = simple_segments(&path)?;
                (segments.len() == 1).then(|| segments[0].clone())
            }),
    );
    names.extend(
        file.items()
            .filter_map(|item| match item {
                ast::Item::Use(import) => Some(import.syntax().text().to_string()),
                _ => None,
            })
            .flat_map(|text| {
                text.split(|character: char| !character.is_alphanumeric() && character != '_')
                    .filter(|word| word.chars().next().is_some_and(char::is_uppercase))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
    );
    names
}

fn module_aliases(file: &ast::SourceFile) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    for import in file.items().filter_map(|item| match item {
        ast::Item::Use(import) => Some(import.syntax().text().to_string()),
        _ => None,
    }) {
        let import = import
            .trim()
            .strip_prefix("use ")
            .unwrap_or_default()
            .trim_end_matches(';');
        if let Some((prefix, group)) = import.split_once("::{") {
            for entry in top_level_group_entries(group.trim_end_matches('}')) {
                let entry = entry.trim();
                if entry == "self" {
                    if let Some(leaf) = prefix.rsplit("::").next() {
                        aliases.insert(leaf.to_owned(), prefix.to_owned());
                    }
                } else if !entry.contains(['{', ':']) {
                    let (path, binding) = entry
                        .rsplit_once(" as ")
                        .map_or((entry, entry), |(path, alias)| (path.trim(), alias.trim()));
                    aliases.insert(binding.to_owned(), format!("{prefix}::{path}"));
                }
            }
        } else if let Some((path, alias)) = import.rsplit_once(" as ") {
            aliases.insert(alias.trim().to_owned(), path.trim().to_owned());
        } else if !import.contains('{')
            && let Some(leaf) = import.rsplit("::").next()
        {
            aliases.insert(leaf.to_owned(), import.to_owned());
        }
    }
    aliases
}

fn top_level_group_entries(group: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, character) in group.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                entries.push(&group[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    entries.push(&group[start..]);
    entries
}

fn canonical_path(segments: &[String], aliases: &BTreeMap<String, String>) -> String {
    aliases.get(&segments[0]).map_or_else(
        || segments.join("::"),
        |prefix| {
            if segments.len() == 1 {
                prefix.clone()
            } else {
                format!("{prefix}::{}", segments[1..].join("::"))
            }
        },
    )
}

fn simple_segments(path: &ast::Path) -> Option<Vec<String>> {
    let text = path.syntax().text().to_string();
    if text.contains(['<', '>', '(', ')']) {
        return None;
    }
    let segments: Vec<_> = path
        .segments()
        .filter_map(|segment| segment.name_ref())
        .map(|name| name.text().to_string())
        .collect();
    (!segments.is_empty()).then_some(segments)
}

fn exempt_root(root: &str) -> bool {
    matches!(
        root,
        "std" | "core" | "alloc" | "proc_macro" | "crate" | "self" | "super" | "Self"
    )
}

fn exempt_path_root(root: &str, aliases: &BTreeMap<String, String>) -> bool {
    exempt_root(root)
        || aliases
            .get(root)
            .is_some_and(|path| path.split("::").next().is_some_and(exempt_root))
}

fn generic_type_parameter(node: &SyntaxNode, root: &str) -> bool {
    node.ancestors().any(|ancestor| {
        let parameters = Fn::cast(ancestor.clone())
            .and_then(|item| item.generic_param_list())
            .or_else(|| Impl::cast(ancestor.clone()).and_then(|item| item.generic_param_list()))
            .or_else(|| Struct::cast(ancestor.clone()).and_then(|item| item.generic_param_list()))
            .or_else(|| Enum::cast(ancestor.clone()).and_then(|item| item.generic_param_list()))
            .or_else(|| Trait::cast(ancestor.clone()).and_then(|item| item.generic_param_list()))
            .or_else(|| {
                TypeAlias::cast(ancestor.clone()).and_then(|item| item.generic_param_list())
            })
            .or_else(|| Union::cast(ancestor).and_then(|item| item.generic_param_list()));
        parameters.is_some_and(|parameters| {
            parameters.generic_params().any(|parameter| {
                matches!(
                    parameter,
                    ast::GenericParam::TypeParam(type_parameter)
                        if type_parameter.name().is_some_and(|name| name.text() == root)
                )
            })
        })
    })
}

fn physical_module(relative: &Path) -> Option<Vec<String>> {
    let components: Vec<_> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    let source = components.iter().position(|component| component == "src")?;
    let mut module = components[source + 1..].to_vec();
    let file = module.pop()?;
    match file.as_str() {
        "lib.rs" | "main.rs" => {}
        "mod.rs" => {}
        "crate_root_tests.rs" => module.push("tests".to_owned()),
        _ if file.ends_with("_tests.rs") => {
            module.push(file.trim_end_matches("_tests.rs").to_owned());
            module.push("tests".to_owned());
        }
        _ => module.push(file.trim_end_matches(".rs").to_owned()),
    }
    Some(module)
}

fn group_imports(imports: BTreeSet<PlannedImport>) -> BTreeMap<usize, Vec<PlannedImport>> {
    let mut groups: BTreeMap<usize, Vec<PlannedImport>> = BTreeMap::new();
    for import in imports {
        groups.entry(import.insertion).or_default().push(import);
    }
    groups
}

#[derive(Default)]
struct ImportTree {
    terminal: bool,
    children: BTreeMap<String, ImportTree>,
}

fn format_imports(imports: &[PlannedImport]) -> String {
    let mut groups: BTreeMap<(bool, String), ImportTree> = BTreeMap::new();
    for import in imports {
        let tree = groups
            .entry((import.cfg_test, import.indent.clone()))
            .or_default();
        let mut node = tree;
        for segment in import.path.split("::") {
            node = node.children.entry(segment.to_owned()).or_default();
        }
        node.terminal = true;
    }
    let mut formatted = String::new();
    for ((cfg_test, indent), tree) in groups {
        for (root, node) in tree.children {
            if cfg_test {
                formatted.push_str(&format!("{indent}#[cfg(test)]\n"));
            }
            formatted.push_str(&format!(
                "{indent}use {};\n",
                render_import_branch(&root, &node)
            ));
        }
    }
    formatted
}

fn render_import_branch(segment: &str, node: &ImportTree) -> String {
    if !node.terminal && node.children.len() == 1 {
        let (child, child_node) = node.children.first_key_value().expect("one child exists");
        return format!("{segment}::{}", render_import_branch(child, child_node));
    }
    if node.children.is_empty() {
        return segment.to_owned();
    }
    let mut entries = Vec::new();
    if node.terminal {
        entries.push("self".to_owned());
    }
    entries.extend(
        node.children
            .iter()
            .map(|(child, child_node)| render_import_branch(child, child_node)),
    );
    format!("{segment}::{{{}}}", entries.join(", "))
}

fn has_import_near(source: &str, insertion: usize, path: &str) -> bool {
    let start = insertion.saturating_sub(4_096);
    source[start..insertion].contains(&format!("use {path}"))
}

fn scoped_import_insertion(
    file: &ast::SourceFile,
    node: &SyntaxNode,
    source: &str,
) -> (usize, String) {
    if let Some(list) = node
        .ancestors()
        .filter_map(ast::Module::cast)
        .find_map(|module| module.item_list())
    {
        let items: Vec<_> = list.items().collect();
        if let Some(import) = items.iter().rev().find_map(|item| match item {
            ast::Item::Use(import) => Some(import),
            _ => None,
        }) {
            let range = text_range(import.syntax());
            let insertion =
                range.end + usize::from(source.as_bytes().get(range.end) == Some(&b'\n'));
            return (insertion, line_indent(source, range.start));
        }
        if let Some(item) = items.first() {
            let start = text_range(item.syntax()).start;
            let indent = line_indent(source, start);
            return (start.saturating_sub(indent.len()), indent);
        }
        if let Some(brace) = list.l_curly_token() {
            let insertion = usize::from(brace.text_range().end());
            let parent_indent = line_indent(source, insertion.saturating_sub(1));
            return (insertion, format!("\n{parent_indent}    "));
        }
    }
    (import_insertion(file, source), String::new())
}

fn line_indent(source: &str, offset: usize) -> String {
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    source[line_start..offset]
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect()
}

fn import_insertion(file: &ast::SourceFile, source: &str) -> usize {
    let last_import = file
        .items()
        .filter_map(|item| match item {
            ast::Item::Use(import) => Some(text_range(import.syntax()).end),
            _ => None,
        })
        .last();
    last_import.map_or_else(
        || {
            file.items()
                .next()
                .map(|item| text_range(item.syntax()).start)
                .unwrap_or(0)
        },
        |end| end + usize::from(source.as_bytes().get(end) == Some(&b'\n')),
    )
}

fn text_range(node: &SyntaxNode) -> std::ops::Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

#[path = "qualification_tests.rs"]
#[cfg(test)]
mod tests;
