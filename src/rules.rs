use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;

use pulldown_cmark::{BrokenLink, Event, Options, Parser, Tag, TagEnd};
use ra_ap_syntax::ast::{self, AstNode, AstToken, HasModuleItem, HasName, HasVisibility};
use ra_ap_syntax::{Edition, SourceFile};
use toml_edit::{DocumentMut, Item, Table};

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use pulldown_cmark::CowStr;
use ra_ap_syntax::SyntaxNode;
use ra_ap_syntax::TextSize;
use ra_ap_syntax::ast::Comment;
use ra_ap_syntax::ast::Module;

static RULES: [&dyn Rule; 3] = [&RustdocTypeLinks, &ItemOrder, &BlankLines];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Edit {
    pub(crate) range: Range<usize>,
    pub(crate) replacement: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Finding {
    pub(crate) diagnostic: Diagnostic,
    pub(crate) edits: Vec<Edit>,
}

pub(crate) fn check(config: &Config, relative: &Path, source: &str) -> Vec<Finding> {
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    debug_assert!(parsed.errors().is_empty());
    let context = RuleContext {
        config,
        relative,
        source,
        file: parsed.tree(),
    };
    let mut findings = Vec::new();
    for rule in RULES {
        if config.rule_enabled(rule.id(), relative) {
            rule.check(&context, &mut findings);
        }
    }
    findings.extend(crate::qualification::check(config, relative, source));
    findings.extend(crate::imports::check_top_level(config, relative, source));
    findings
}

pub(crate) fn check_manifest(config: &Config, relative: &Path, source: &str) -> Vec<Finding> {
    if !config.rule_enabled("cargo.dependency-order", relative) {
        return Vec::new();
    }
    let Ok(mut document) = source.parse::<DocumentMut>() else {
        return Vec::new();
    };
    let mut first_misordered = None;
    if let Some(table) = document
        .get_mut("dependencies")
        .and_then(Item::as_table_mut)
    {
        first_misordered = sort_dependency_table(table);
    }
    if let Some(table) = document
        .get_mut("workspace")
        .and_then(Item::as_table_mut)
        .and_then(|workspace| workspace.get_mut("dependencies"))
        .and_then(Item::as_table_mut)
    {
        let workspace_misordered = sort_dependency_table(table);
        first_misordered = first_misordered.or(workspace_misordered);
    }
    let Some(range) = first_misordered else {
        return Vec::new();
    };
    vec![Finding {
        diagnostic: Diagnostic::new(
            "cargo.dependency-order",
            "path dependencies must come first and dependencies must be sorted by key",
            relative.to_path_buf(),
            source,
            range.start,
            range.end,
        ),
        edits: vec![Edit {
            range: 0..source.len(),
            replacement: document.to_string(),
        }],
    }]
}

struct RuleContext<'a> {
    config: &'a Config,
    relative: &'a Path,
    source: &'a str,
    file: ast::SourceFile,
}

trait Rule: Sync {
    fn id(&self) -> &'static str;
    fn check(&self, context: &RuleContext<'_>, findings: &mut Vec<Finding>);
}

struct ItemOrder;

struct BlankLines;

struct RustdocTypeLinks;

impl Rule for RustdocTypeLinks {
    fn id(&self) -> &'static str {
        "rustdoc.type-links"
    }

    fn check(&self, context: &RuleContext<'_>, findings: &mut Vec<Finding>) {
        let known = known_type_names(&context.file);
        for comment in context
            .file
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter_map(ast::Comment::cast)
            .filter(ast::Comment::is_doc)
        {
            check_doc_comment(context, &comment, &known, findings);
        }
    }
}

impl Rule for ItemOrder {
    fn id(&self) -> &'static str {
        "items.order"
    }

    fn check(&self, context: &RuleContext<'_>, findings: &mut Vec<Finding>) {
        let mut run = Vec::new();
        for item in context.file.items() {
            match item_role(context.config, &item) {
                ItemRole::Orderable(category) => run.push((item, category)),
                ItemRole::Fixed => {}
                ItemRole::Barrier => {
                    check_order_run(context, &run, findings);
                    run.clear();
                }
            }
        }
        check_order_run(context, &run, findings);
    }
}

impl Rule for BlankLines {
    fn id(&self) -> &'static str {
        "items.blank-lines"
    }

    fn check(&self, context: &RuleContext<'_>, findings: &mut Vec<Finding>) {
        let mut previous: Option<ast::Item> = None;
        for item in context.file.items() {
            if matches!(item_role(context.config, &item), ItemRole::Orderable(_)) {
                if let Some(before) = &previous
                    && !matches!((before, &item), (ast::Item::Const(_), ast::Item::Const(_)))
                {
                    check_spacing(context, before, &item, findings);
                }
                previous = Some(item);
            } else {
                previous = None;
            }
        }
    }
}

fn check_order_run(
    context: &RuleContext<'_>,
    run: &[(ast::Item, u8)],
    findings: &mut Vec<Finding>,
) {
    let mut sorted = run.to_vec();
    sorted.sort_by_key(|(_, category)| *category);
    if sorted
        .iter()
        .map(|(_, category)| category)
        .eq(run.iter().map(|(_, category)| category))
    {
        return;
    }

    let edits = run
        .iter()
        .zip(sorted)
        .filter_map(|((slot, _), (replacement, _))| {
            let slot_range = text_range(slot.syntax());
            let replacement = replacement.syntax().text().to_string();
            (context.source[slot_range.clone()] != replacement).then_some(Edit {
                range: slot_range,
                replacement,
            })
        })
        .collect();
    let first = run
        .iter()
        .zip(run.iter().skip(1))
        .find(|((_, left), (_, right))| left > right)
        .map_or(&run[0].0, |(_, (item, _))| item);
    let range = text_range(first.syntax());
    findings.push(Finding {
        diagnostic: Diagnostic::new(
            "items.order",
            "top-level items are not in the required order",
            context.relative.to_path_buf(),
            context.source,
            range.start,
            range.end,
        ),
        edits,
    });
}

fn check_spacing(
    context: &RuleContext<'_>,
    previous: &ast::Item,
    item: &ast::Item,
    findings: &mut Vec<Finding>,
) {
    let previous_range = text_range(previous.syntax());
    let range = text_range(item.syntax());
    let separator = &context.source[previous_range.end..range.start];
    if has_empty_line(separator) {
        return;
    }
    let line_ending = if context.source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    findings.push(Finding {
        diagnostic: Diagnostic::new(
            "items.blank-lines",
            "top-level code items must be separated by an empty line",
            context.relative.to_path_buf(),
            context.source,
            range.start,
            range.start,
        ),
        edits: vec![Edit {
            range: range.start..range.start,
            replacement: line_ending.to_owned(),
        }],
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ItemRole {
    Orderable(u8),
    Fixed,
    Barrier,
}

fn item_role(config: &Config, item: &ast::Item) -> ItemRole {
    match item {
        ast::Item::Use(_) => ItemRole::Fixed,
        ast::Item::Module(module) if module.item_list().is_none() && !is_test_module(module) => {
            ItemRole::Fixed
        }
        ast::Item::Module(module) if module.item_list().is_none() => ItemRole::Orderable(15),
        ast::Item::MacroCall(call) => {
            let path = call.path().map(|path| path.syntax().text().to_string());
            if path.is_some_and(|path| config.constant_macros.contains(path.trim())) {
                ItemRole::Orderable(3)
            } else {
                ItemRole::Barrier
            }
        }
        ast::Item::MacroDef(_) | ast::Item::MacroRules(_) | ast::Item::AsmExpr(_) => {
            ItemRole::Barrier
        }
        _ => ItemRole::Orderable(category(item)),
    }
}

fn category(item: &ast::Item) -> u8 {
    let visibility = visibility(item);
    match (visibility, item) {
        (Visibility::Private, ast::Item::Const(_)) => 1,
        (Visibility::Private, ast::Item::Static(_)) => 2,
        (Visibility::Public, ast::Item::TypeAlias(_)) => 4,
        (Visibility::Public, ast::Item::Const(_) | ast::Item::Static(_)) => 5,
        (Visibility::Public, ast::Item::Trait(_)) => 6,
        (Visibility::Public, ast::Item::Struct(_) | ast::Item::Enum(_) | ast::Item::Union(_)) => 7,
        (Visibility::Public, ast::Item::Fn(_)) => 8,
        (Visibility::Restricted, ast::Item::TypeAlias(_)) => 9,
        (Visibility::Restricted, ast::Item::Const(_) | ast::Item::Static(_)) => 10,
        (Visibility::Restricted, ast::Item::Trait(_)) => 11,
        (
            Visibility::Restricted,
            ast::Item::Struct(_) | ast::Item::Enum(_) | ast::Item::Union(_),
        ) => 12,
        (Visibility::Restricted, ast::Item::Fn(_)) => 13,
        _ => 14,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Visibility {
    Private,
    Public,
    Restricted,
}

fn visibility(item: &ast::Item) -> Visibility {
    let text = match item {
        ast::Item::Const(node) => node.visibility(),
        ast::Item::Enum(node) => node.visibility(),
        ast::Item::ExternCrate(node) => node.visibility(),
        ast::Item::Fn(node) => node.visibility(),
        ast::Item::Impl(node) => node.visibility(),
        ast::Item::MacroDef(node) => node.visibility(),
        ast::Item::MacroRules(node) => node.visibility(),
        ast::Item::Module(node) => node.visibility(),
        ast::Item::Static(node) => node.visibility(),
        ast::Item::Struct(node) => node.visibility(),
        ast::Item::Trait(node) => node.visibility(),
        ast::Item::TypeAlias(node) => node.visibility(),
        ast::Item::Union(node) => node.visibility(),
        ast::Item::Use(node) => node.visibility(),
        ast::Item::AsmExpr(_) | ast::Item::ExternBlock(_) | ast::Item::MacroCall(_) => None,
    }
    .map(|visibility| visibility.syntax().text().to_string());
    match text.as_deref().map(str::trim) {
        None => Visibility::Private,
        Some("pub") => Visibility::Public,
        Some(_) => Visibility::Restricted,
    }
}

fn is_test_module(module: &Module) -> bool {
    let Some(name) = module.name() else {
        return false;
    };
    let text = module
        .syntax()
        .text()
        .to_string()
        .replace(char::is_whitespace, "");
    name.text() == "tests"
        || text.contains("#[cfg(test)]")
        || text.contains("#[cfg(all(test,")
        || text.contains("#[cfg(all(") && text.contains(",test")
}

fn text_range(node: &SyntaxNode) -> Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

fn has_empty_line(separator: &str) -> bool {
    let lines: Vec<_> = separator.split('\n').collect();
    lines
        .iter()
        .skip(1)
        .take(lines.len().saturating_sub(2))
        .any(|line| line.trim_matches([' ', '\t', '\r']).is_empty())
}

fn check_doc_comment(
    context: &RuleContext<'_>,
    comment: &Comment,
    known: &HashSet<String>,
    findings: &mut Vec<Finding>,
) {
    let Some((content, prefix_offset)) = comment.doc_comment() else {
        return;
    };
    let callback = |link: BrokenLink<'_>| {
        Some((
            pulldown_cmark::CowStr::from(link.reference.to_string()),
            CowStr::Borrowed(""),
        ))
    };
    let parser = Parser::new_with_broken_link_callback(content, Options::empty(), Some(callback))
        .into_offset_iter();
    let mut links = Vec::new();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                links.push((dest_url.into_string(), range.start));
            }
            Event::End(TagEnd::Link) => {
                let Some((destination, start)) = links.pop() else {
                    continue;
                };
                let range = start..range.end;
                check_doc_link(
                    context,
                    comment,
                    prefix_offset,
                    content,
                    &destination,
                    range,
                    known,
                    findings,
                );
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_doc_link(
    context: &RuleContext<'_>,
    comment: &Comment,
    prefix_offset: TextSize,
    content: &str,
    destination: &str,
    markdown_range: Range<usize>,
    known: &HashSet<String>,
    findings: &mut Vec<Finding>,
) {
    let Some(target) = type_link_target(destination) else {
        return;
    };
    if target.contains("::")
        || known.contains(target)
        || generic_parameter(target)
        || primitive(target)
    {
        return;
    }
    let comment_start = usize::from(comment.syntax().text_range().start());
    let content_start = comment_start + usize::from(prefix_offset);
    let range = content_start + markdown_range.start..content_start + markdown_range.end;
    let raw = &content[markdown_range];
    let replacement = unlinked_display(raw, target);
    findings.push(Finding {
        diagnostic: Diagnostic::new(
            "rustdoc.type-links",
            format!("unresolved Rustdoc type link `{target}` must not be a link"),
            context.relative.to_path_buf(),
            context.source,
            range.start,
            range.end,
        ),
        edits: vec![Edit { range, replacement }],
    });
}

fn known_type_names(file: &ast::SourceFile) -> HashSet<String> {
    let mut names: HashSet<_> = [
        "Option", "Result", "String", "Vec", "Box", "Cow", "Rc", "Arc", "Pin",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for item in file.items() {
        let name = match &item {
            ast::Item::Enum(node) => node.name(),
            ast::Item::Struct(node) => node.name(),
            ast::Item::Trait(node) => node.name(),
            ast::Item::TypeAlias(node) => node.name(),
            ast::Item::Union(node) => node.name(),
            _ => None,
        };
        if let Some(name) = name {
            names.insert(name.text().to_string());
        }
        if let ast::Item::Use(node) = item {
            let text = node.syntax().text().to_string();
            names.extend(
                text.split(|character: char| !character.is_alphanumeric() && character != '_')
                    .filter(|word| word.chars().next().is_some_and(char::is_uppercase))
                    .map(str::to_owned),
            );
        }
    }
    names
}

fn type_link_target(destination: &str) -> Option<&str> {
    if destination.contains('/')
        || destination.contains('#')
        || destination.ends_with("()")
        || destination.starts_with("http:")
        || destination.starts_with("https:")
    {
        return None;
    }
    let destination = destination.trim_matches('`');
    let (disambiguator, target) = destination
        .split_once('@')
        .map_or((None, destination), |(kind, target)| (Some(kind), target));
    if disambiguator.is_some_and(|kind| matches!(kind, "fn" | "macro" | "mod" | "const" | "static"))
    {
        return None;
    }
    let leaf = target.rsplit("::").next()?;
    let first = leaf.chars().next()?;
    (first.is_uppercase() || primitive(leaf)).then_some(target)
}

fn primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    )
}

fn generic_parameter(name: &str) -> bool {
    name.len() <= 2 && name.chars().all(char::is_uppercase)
}

fn unlinked_display(raw: &str, target: &str) -> String {
    let display = raw
        .strip_prefix('[')
        .and_then(|value| value.split_once(']'))
        .map_or(target, |(display, _)| display);
    let display = display
        .split_once('@')
        .map_or(display, |(_, value)| value)
        .trim_matches('`');
    if display == target {
        format!("`{display}`")
    } else {
        display.to_owned()
    }
}

fn sort_dependency_table(table: &mut Table) -> Option<Range<usize>> {
    let entries: Vec<_> = table
        .iter()
        .map(|(key, item)| (key.to_owned(), dependency_is_path(item)))
        .collect();
    let mut expected = entries.clone();
    expected.sort_by(|left, right| (!left.1, &left.0).cmp(&(!right.1, &right.0)));
    let misplaced_key = entries
        .iter()
        .zip(&expected)
        .find(|(actual, expected)| actual != expected)
        .map(|((key, _), _)| key);
    let misplaced = misplaced_key
        .and_then(|key| table.key(key))
        .and_then(toml_edit::Key::span)
        .or_else(|| misplaced_key.map(|_| 0..0));
    if misplaced.is_some() {
        table.sort_values_by(|left_key, left, right_key, right| {
            (!dependency_is_path(left), left_key.get())
                .cmp(&(!dependency_is_path(right), right_key.get()))
        });
    }
    misplaced
}

fn dependency_is_path(item: &Item) -> bool {
    item.as_inline_table()
        .is_some_and(|table| table.contains_key("path"))
        || item
            .as_table()
            .is_some_and(|table| table.contains_key("path"))
}

#[path = "rules_tests.rs"]
#[cfg(test)]
mod tests;
