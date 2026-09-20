use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::config::Config;
use crate::qualification::check;
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
fn shortens_type_variant_and_function_paths_with_shared_imports() {
    let (_directory, config) = config();
    let source =
        "fn f(value: cards::Card) { workflow::runner::run(); let _ = ui::Value::Ready; }\n";
    let findings = check(&config, Path::new("src/lib.rs"), source);
    assert_eq!(findings.len(), 3);
    let mut fixed = source.to_owned();
    let mut edits = findings[0].edits.clone();
    edits.sort_by_key(|edit| edit.range.start);
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    assert!(fixed.contains("use cards::Card;"));
    assert!(fixed.contains("use ui::Value;"));
    assert!(fixed.contains("use workflow::runner;"));
    assert!(fixed.contains("fn f(value: Card)"));
    assert!(fixed.contains("runner::run()"));
    assert!(fixed.contains("Value::Ready"));
}

#[test]
fn rewrites_relative_imports_from_the_file_module() {
    let (_directory, config) = config();
    let source = "use super::card::Card;\n";
    let findings = check(&config, Path::new("src/game/deck.rs"), source);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].edits[0].replacement, "crate::game::card::Card;");
}
