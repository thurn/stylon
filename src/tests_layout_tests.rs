use std::fs;

use tempfile::tempdir;

use crate::config::Config;
use crate::imports::RustInput;
use crate::tests_layout::analyze_inline_tests;

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
