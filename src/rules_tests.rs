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
fn rejects_crate_and_super_visibility_without_offering_a_fix() {
    let (_directory, config) = config();
    let source = "pub(crate) struct CrateItem;\nmod child { pub(super) fn parent_item() {} pub(self) fn local_item() {} }\npub struct Public;\n";

    let findings = check(&config, Path::new("src/lib.rs"), source);
    let restricted: Vec<_> = findings
        .iter()
        .filter(|finding| finding.diagnostic.rule_id == "visibility.no-restricted")
        .collect();

    assert_eq!(restricted.len(), 2);
    assert!(restricted.iter().all(|finding| finding.edits.is_empty()));
    assert!(
        restricted
            .iter()
            .all(|finding| finding.diagnostic.fix == "none")
    );
}

#[test]
fn allows_restricted_visibility_rule_to_be_disabled() {
    let (directory, _) = config();
    fs::write(
        directory.path().join("stylon.toml"),
        "version = 1\n\n[rules]\n\"visibility.no-restricted\" = false\n",
    )
    .expect("configuration");
    let config = Config::load(directory.path(), None).expect("configuration");

    let findings = check(
        &config,
        Path::new("src/lib.rs"),
        "pub(crate) struct AllowedHere;\n",
    );

    assert!(
        findings
            .iter()
            .all(|finding| finding.diagnostic.rule_id != "visibility.no-restricted")
    );
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

#[test]
fn spaces_impl_items_without_changing_indentation_or_attached_docs() {
    let (_directory, config) = config();
    let source = "impl ChessTest {\n  pub fn from_position() {}\n  /// Starts at the title.\n  #[inline]\n  pub fn title() {}\n  pub fn persisted() {}\n}\n";
    let expected = "impl ChessTest {\n  pub fn from_position() {}\n\n  /// Starts at the title.\n  #[inline]\n  pub fn title() {}\n\n  pub fn persisted() {}\n}\n";
    for ending in ["\n", "\r\n"] {
        let source = source.replace('\n', ending);
        let findings = check(&config, Path::new("tests/support/game.rs"), &source);
        let mut edits: Vec<_> = findings
            .iter()
            .filter(|finding| finding.diagnostic.rule_id == "items.blank-lines")
            .flat_map(|finding| finding.edits.clone())
            .collect();
        assert_eq!(edits.len(), 2);
        edits.sort_by_key(|edit| edit.range.start);
        let mut fixed = source;
        for edit in edits.into_iter().rev() {
            fixed.replace_range(edit.range, &edit.replacement);
        }
        assert_eq!(fixed, expected.replace('\n', ending));
        assert!(check(&config, Path::new("tests/support/game.rs"), &fixed).is_empty());
    }
}

#[test]
fn impl_spacing_respects_constants_macros_and_existing_blank_lines() {
    let (_directory, config) = config();
    for source in [
        "impl Example {\n  const A: u8 = 1;\n  const B: u8 = 2;\n\n  fn first() {}\n \t\n  fn second() {}\n}\n",
        "impl Example {\n  fn first() {}\n  opaque!();\n  fn second() {}\n}\n",
    ] {
        assert!(check(&config, Path::new("src/lib.rs"), source).is_empty());
    }
    for source in [
        "impl Example {\n  const A: u8 = 1;\n  fn first() {}\n}\n",
        "impl Trait for Example {\n  type Output = u8;\n  fn first() {}\n}\n",
        "mod nested { impl Example {\n  fn first() {}\n  fn second() {}\n} }\n",
        "impl Example { fn first() {} fn second() {} }\n",
    ] {
        let findings = check(&config, Path::new("src/lib.rs"), source);
        let spacing: Vec<_> = findings
            .iter()
            .filter(|finding| finding.diagnostic.rule_id == "items.blank-lines")
            .collect();
        assert_eq!(spacing.len(), 1, "{source}");
        let mut fixed = source.to_owned();
        for edit in &spacing[0].edits {
            fixed.replace_range(edit.range.clone(), &edit.replacement);
        }
        assert!(
            check(&config, Path::new("src/lib.rs"), &fixed)
                .iter()
                .all(|finding| finding.diagnostic.rule_id != "items.blank-lines")
        );
    }
}

#[test]
fn impl_spacing_obeys_test_directory_overrides() {
    let (directory, _) = config();
    fs::write(
        directory.path().join("stylon.toml"),
        "version = 1\n[[overrides]]\npaths = [\"tests/**\"]\n[overrides.rules]\n\"items.blank-lines\" = false\n",
    )
    .expect("configuration");
    let config = Config::load(directory.path(), None).expect("configuration");
    let source = "impl Example {\n  fn first() {}\n  fn second() {}\n}\n";
    assert!(check(&config, Path::new("tests/support/game.rs"), source).is_empty());
    assert!(
        check(&config, Path::new("src/lib.rs"), source)
            .iter()
            .any(|finding| finding.diagnostic.rule_id == "items.blank-lines")
    );
}
