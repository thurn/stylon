use std::fs;

use tempfile::tempdir;

use crate::config::Config;
use crate::imports::RustInput;
use crate::tests_layout::{analyze_file_suffix, analyze_inline_tests};
use toml::Value;

#[test]
fn extracts_top_level_test_module() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers=[]\n",
    )
    .expect("manifest");
    let path = fs::canonicalize(directory.path().join("src"))
        .expect("source")
        .join("lib.rs");
    let source =
        "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn works() {}\n}\n";
    fs::write(&path, source).expect("source file");
    let config = Config::load(directory.path(), None).expect("configuration");
    let inputs = [RustInput {
        path: &path,
        relative: "src/lib.rs".into(),
        source,
    }];

    let analysis = analyze_inline_tests(&config, &inputs);

    assert!(analysis.errors.is_empty());
    assert_eq!(analysis.diagnostics.len(), 1);
    assert!(analysis.replacements[&path].contains("#[path = \"crate_root_tests.rs\"]"));
    let destination = path.parent().expect("parent").join("crate_root_tests.rs");
    assert_eq!(
        analysis.creations[&destination],
        "use crate::*;\n\n#[test]\nfn works() {}\n"
    );
}

#[test]
fn renames_an_ordinary_test_module_and_updates_its_parent() {
    let directory = tempdir().expect("temporary directory");
    let source_directory = directory.path().join("src");
    fs::create_dir(&source_directory).expect("source directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .expect("manifest");
    let parent = source_directory.join("lib.rs");
    let source_path = source_directory.join("parser.rs");
    fs::write(&parent, "mod parser;\n").expect("parent");
    let source = "#[test]\nfn parses() {}\n";
    fs::write(&source_path, source).expect("test source");
    let config = Config::load(directory.path(), None).expect("configuration");
    let parent_source = "mod parser;\n";
    let inputs = [
        RustInput {
            path: &parent,
            relative: "src/lib.rs".into(),
            source: parent_source,
        },
        RustInput {
            path: &source_path,
            relative: "src/parser.rs".into(),
            source,
        },
    ];

    let analysis = analyze_file_suffix(&config, &inputs, &std::collections::BTreeMap::new());

    assert!(analysis.errors.is_empty());
    assert_eq!(analysis.diagnostics.len(), 1);
    assert_eq!(
        analysis.replacements[&parent],
        "#[path = \"parser_tests.rs\"]\nmod parser;\n"
    );
    let destination = source_directory.join("parser_tests.rs");
    assert_eq!(analysis.creations[&destination], source);
    assert!(analysis.deletions.contains(&source_path));
}

#[test]
fn preserves_an_integration_test_target_name() {
    let directory = tempdir().expect("temporary directory");
    let tests_directory = directory.path().join("tests");
    fs::create_dir(&tests_directory).expect("tests directory");
    let manifest = directory.path().join("Cargo.toml");
    fs::write(&manifest, "[package]\nname='fixture'\nversion='0.1.0'\n").expect("manifest");
    let source_path = tests_directory.join("gameplay.rs");
    let source = "#[test]\nfn plays() {}\n";
    fs::write(&source_path, source).expect("test source");
    let config = Config::load(directory.path(), None).expect("configuration");
    let manifest = fs::canonicalize(manifest).expect("canonical manifest");
    let source_path = fs::canonicalize(source_path).expect("canonical source");
    let inputs = [RustInput {
        path: &source_path,
        relative: "tests/gameplay.rs".into(),
        source,
    }];

    let manifests = std::collections::BTreeMap::from([(
        manifest.clone(),
        fs::read_to_string(&manifest).expect("manifest source"),
    )]);
    let analysis = analyze_file_suffix(&config, &inputs, &manifests);

    assert!(analysis.errors.is_empty());
    assert!(analysis.replacements[&manifest].contains("name = \"gameplay\""));
    assert!(analysis.replacements[&manifest].contains("path = \"tests/gameplay_tests.rs\""));
}

#[test]
fn updates_an_explicit_integration_target_without_losing_options() {
    let directory = tempdir().expect("temporary directory");
    let tests_directory = directory.path().join("tests");
    fs::create_dir(&tests_directory).expect("tests directory");
    let manifest = directory.path().join("Cargo.toml");
    fs::write(
        &manifest,
        "[package]\nname='fixture'\nversion='0.1.0'\n\n[[test]]\nname='gameplay'\npath='tests/gameplay.rs'\nharness=false\nrequired-features=['extra']\n",
    )
    .expect("manifest");
    let source_path = tests_directory.join("gameplay.rs");
    let source = "#[test]\nfn plays() {}\n";
    fs::write(&source_path, source).expect("test source");
    let config = Config::load(directory.path(), None).expect("configuration");
    let manifest = fs::canonicalize(manifest).expect("canonical manifest");
    let source_path = fs::canonicalize(source_path).expect("canonical source");
    let inputs = [RustInput {
        path: &source_path,
        relative: "tests/gameplay.rs".into(),
        source,
    }];
    let manifests = std::collections::BTreeMap::from([(
        manifest.clone(),
        fs::read_to_string(&manifest).expect("manifest source"),
    )]);

    let analysis = analyze_file_suffix(&config, &inputs, &manifests);

    let replacement = &analysis.replacements[&manifest];
    let document: Value = toml::from_str(replacement).expect("updated manifest");
    let target = &document["test"][0];
    assert_eq!(target["path"].as_str(), Some("tests/gameplay_tests.rs"));
    assert_eq!(target["harness"].as_bool(), Some(false));
    assert_eq!(target["required-features"][0].as_str(), Some("extra"));
}
