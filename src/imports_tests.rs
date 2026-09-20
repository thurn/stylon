use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::config::Config;
use crate::imports::{RustInput, analyze_public_functions, check_top_level};

#[test]
fn rewrites_direct_public_function_imports_through_the_module() {
    let directory = tempdir().expect("temporary directory");
    let manifest = directory.path().join("Cargo.toml");
    fs::write(&manifest, "[workspace]\nmembers=[]\n").expect("manifest");
    let config = Config::load(directory.path(), None).expect("configuration");
    let source_path = directory.path().join("src/lib.rs");
    let source = "pub mod cards { pub fn shuffle() {} }\nuse crate::cards::shuffle;\nfn play() { shuffle(); }\n";
    let inputs = [RustInput {
        path: &source_path,
        relative: "src/lib.rs".into(),
        source,
    }];

    let analysis = analyze_public_functions(&config, &inputs);

    assert_eq!(analysis.diagnostics.len(), 1);
    let fixed = &analysis.replacements[&source_path];
    assert!(fixed.contains("use crate::cards;"));
    assert!(!fixed.contains("use crate::cards::shuffle;"));
    assert!(fixed.contains("cards::shuffle();"));
}

#[test]
fn hoists_function_imports_to_the_file_module() {
    let directory = tempdir().expect("temporary directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers=[]\n",
    )
    .expect("manifest");
    let config = Config::load(directory.path(), None).expect("configuration");
    let source = "fn read() {\n    use crate::io::load;\n    load();\n}\n";
    let findings = check_top_level(&config, Path::new("src/lib.rs"), source);
    assert_eq!(findings.len(), 1);
    let mut fixed = source.to_owned();
    let mut edits = findings[0].edits.clone();
    edits.sort_by_key(|edit| edit.range.start);
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    assert_eq!(fixed, "use crate::io::load;\nfn read() {\n    load();\n}\n");
}

#[test]
fn retains_cfg_condition_when_hoisting() {
    let directory = tempdir().expect("temporary directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers=[]\n",
    )
    .expect("manifest");
    let config = Config::load(directory.path(), None).expect("configuration");
    let source = "fn links() {\n    #[cfg(unix)]\n    {\n        use std::os::unix::fs::MetadataExt;\n        let _ = 1;\n    }\n}\n";
    let findings = check_top_level(&config, Path::new("src/lib.rs"), source);
    let insertion = findings[0]
        .edits
        .iter()
        .find(|edit| edit.range.is_empty())
        .expect("insertion");
    assert_eq!(
        insertion.replacement,
        "#[cfg(unix)]\nuse std::os::unix::fs::MetadataExt;\n"
    );
}
