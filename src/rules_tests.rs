use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::config::Config;
use crate::rules::{check, check_manifest};
use tempfile::TempDir;

fn config() -> (TempDir, Config) {
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

#[test]
fn unlinks_unresolved_rustdoc_types_and_keeps_known_types() {
    let (_directory, config) = config();
    let source = "pub struct Card;\n\n/// Uses [Card], [Widget], [the other][Other], and [`Missing`].\npub fn draw() {}\n";
    let findings = check(&config, Path::new("src/lib.rs"), source);
    let mut edits: Vec<_> = findings
        .into_iter()
        .filter(|finding| finding.diagnostic.rule_id == "rustdoc.type-links")
        .flat_map(|finding| finding.edits)
        .collect();
    assert_eq!(edits.len(), 3);
    edits.sort_by_key(|edit| edit.range.start);
    let mut fixed = source.to_owned();
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    assert_eq!(
        fixed,
        "pub struct Card;\n\n/// Uses [Card], `Widget`, the other, and `Missing`.\npub fn draw() {}\n"
    );
}

#[test]
fn resolves_reference_definitions_across_doc_comment_lines() {
    let (_directory, config) = config();
    let source =
        "/// Uses [Widget] or [None].\n///\n/// [Widget]: crate::Widget\npub fn draw() {}\n";

    let findings = check(&config, Path::new("src/lib.rs"), source);

    assert!(
        findings
            .iter()
            .all(|finding| finding.diagnostic.rule_id != "rustdoc.type-links")
    );
}
