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

#[test]
fn rejects_a_configuration_without_a_version() {
    let directory = tempdir().expect("temporary directory");
    fs::write(directory.path().join("stylon.toml"), "[rules]\n").expect("configuration");

    let error = Config::load(directory.path(), None).expect_err("missing version must fail");

    assert_eq!(error.category, "configuration");
    assert!(error.message.contains("missing field `version`"));
}

#[test]
fn selects_the_nearest_enclosing_workspace() {
    let directory = tempdir().expect("temporary directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers=['member']\n",
    )
    .expect("workspace manifest");
    let member = directory.path().join("member");
    let source = member.join("src");
    fs::create_dir_all(&source).expect("member source");
    fs::write(
        member.join("Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\n",
    )
    .expect("member manifest");

    let config = Config::load(&source, None).expect("configuration");

    assert_eq!(
        config.root,
        fs::canonicalize(directory.path()).expect("root")
    );
}
