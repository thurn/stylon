use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::config::Config;

#[test]
fn applies_last_matching_rule_override() {
    let directory = tempdir().expect("temporary directory");
    fs::write(
        directory.path().join("stylon.toml"),
        r#"
version = 1
[rules]
"items.order" = false
[[overrides]]
paths = ["src/**"]
[overrides.rules]
"items.order" = true
"#,
    )
    .expect("write config");
    let source = directory.path().join("src");
    fs::create_dir(&source).expect("create source directory");

    let config = Config::load(&source, None).expect("load config");
    assert!(config.rule_enabled("items.order", Path::new("src/lib.rs")));
    assert!(!config.rule_enabled("items.order", Path::new("tests/a.rs")));
}
