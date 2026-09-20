use std::ops::Range;
use std::path::Path;

use ra_ap_syntax::ast::{self, AstNode, HasModuleItem, HasName, HasVisibility};
use ra_ap_syntax::{Edition, SourceFile};
use toml_edit::{DocumentMut, Item, Table};

use crate::config::Config;
use crate::diagnostic::Diagnostic;

static RULES: [&dyn Rule; 2] = [&ItemOrder, &BlankLines];

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

fn is_test_module(module: &ast::Module) -> bool {
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

fn text_range(node: &ra_ap_syntax::SyntaxNode) -> Range<usize> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{check, check_manifest};
    use crate::config::Config;

    fn config() -> (tempfile::TempDir, Config) {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers=[]\n",
        )
        .expect("manifest");
        let config = Config::load(directory.path(), None).expect("configuration");
        (directory, config)
    }

    #[test]
    fn orders_items_while_leaving_import_slots_fixed() {
        let (_directory, config) = config();
        let source = "fn private() {}\nuse std::fmt;\npub struct Public;\n";
        let findings = check(&config, Path::new("src/lib.rs"), source);
        let order = findings
            .iter()
            .find(|finding| finding.diagnostic.rule_id == "items.order")
            .expect("order finding");
        assert_eq!(order.edits[0].replacement, "pub struct Public;");
        assert_eq!(order.edits[1].replacement, "fn private() {}");
    }

    #[test]
    fn requires_blank_lines_except_between_constants() {
        let (_directory, config) = config();
        let source = "const A: u8 = 1;\nconst B: u8 = 2;\nfn private() {}\n";
        let findings = check(&config, Path::new("src/lib.rs"), source);
        assert_eq!(
            findings
                .iter()
                .filter(|finding| finding.diagnostic.rule_id == "items.blank-lines")
                .count(),
            1
        );
    }

    #[test]
    fn orders_path_and_external_dependencies_without_reformatting_values() {
        let (_directory, config) = config();
        let source = r#"[dependencies]
# external comment
serde = { version = "1", features = ["derive", "derive"] }
# internal comment
local = { path = "../local" }
anyhow = "1"
"#;
        let findings = check_manifest(&config, Path::new("Cargo.toml"), source);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].edits[0].replacement,
            r#"[dependencies]
# internal comment
local = { path = "../local" }
anyhow = "1"
# external comment
serde = { version = "1", features = ["derive", "derive"] }
"#
        );
    }
}
