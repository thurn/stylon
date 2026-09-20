use std::collections::BTreeSet;
use std::fs;

use tempfile::tempdir;
use toml_edit::Item;

use crate::cargo_rules::{ManifestInput, Policy, build_workspace_plan, inherited_item};
use crate::config::Config;
use toml_edit::DocumentMut;

#[test]
fn inherited_entries_keep_only_member_features_and_optionality() {
    let item: Item = "value = { version = \"1\", default-features = false, optional = true, features = [\"x\"] }"
            .parse::<DocumentMut>()
            .expect("document")
            .remove("value")
            .expect("item");
    let inherited = inherited_item(&item).to_string();
    assert!(inherited.contains("workspace = true"));
    assert!(inherited.contains("optional = true"));
    assert!(inherited.contains("features = [\"x\"]"));
    assert!(!inherited.contains("version"));
    assert!(!inherited.contains("default-features"));
    assert!(!Policy::from_item(&item).default_features);
}

#[test]
fn promotes_compatible_member_policy_to_the_workspace() {
    let directory = tempdir().expect("temporary directory");
    let root_path = directory.path().join("Cargo.toml");
    let member_path = directory.path().join("member/Cargo.toml");
    fs::create_dir(member_path.parent().expect("member directory")).expect("directory");
    let root_source = "[workspace]\nmembers = [\"member\"]\n";
    let member_source = r#"[package]
name = "member"
version = "0.1.0"

[dependencies]
serde = { version = "1", optional = true, features = ["derive"] }
"#;
    fs::write(&root_path, root_source).expect("root manifest");
    fs::write(&member_path, member_source).expect("member manifest");
    let config = Config::load(directory.path(), None).expect("configuration");
    let root_path = fs::canonicalize(root_path).expect("canonical root");
    let member_path = fs::canonicalize(member_path).expect("canonical member");
    let inputs = [
        ManifestInput {
            path: &root_path,
            relative: "Cargo.toml".into(),
            source: root_source,
        },
        ManifestInput {
            path: &member_path,
            relative: "member/Cargo.toml".into(),
            source: member_source,
        },
    ];
    let members = BTreeSet::from([member_path.clone()]);

    let analysis = build_workspace_plan(&config, &inputs[0], &inputs, &members);

    assert!(analysis.errors.is_empty());
    assert_eq!(analysis.diagnostics.len(), 1);
    let root = &analysis.replacements[&root_path];
    assert!(root.contains("[workspace.dependencies]"));
    assert!(root.contains("serde = { version = \"1\" }"), "{root}");
    let member = &analysis.replacements[&member_path];
    assert!(member.contains("workspace = true"));
    assert!(member.contains("optional = true"));
    assert!(member.contains("features = [\"derive\"]"));
    assert!(!member.contains("version = \"1\""));
}
