use std::ffi::OsString;
use std::fs;

use tempfile::tempdir;

#[test]
fn clean_project_exits_successfully() {
    let directory = tempdir().expect("temporary directory");
    std::fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("manifest");
    std::fs::write(directory.path().join("lib.rs"), "pub struct Example;\n").expect("source");
    let arguments = [
        OsString::from("stylon"),
        directory.path().as_os_str().to_owned(),
    ];
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
}

#[test]
fn fix_is_clean_and_idempotent() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    fs::write(
        directory.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("lockfile");
    let source_path = directory.path().join("src/lib.rs");
    fs::write(&source_path, "fn helper() {}\npub struct Public;\n").expect("source");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments.clone()),
        std::process::ExitCode::SUCCESS
    );
    let fixed = fs::read(&source_path).expect("fixed source");
    assert_eq!(fixed, b"pub struct Public;\n\nfn helper() {}\n");
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(fs::read(source_path).expect("second source"), fixed);
}

#[test]
fn fix_extracts_inline_tests_transactionally() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='fixture_tests'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    fs::write(
        directory.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"fixture_tests\"\nversion = \"0.1.0\"\n",
    )
    .expect("lockfile");
    let source_path = directory.path().join("src/lib.rs");
    fs::write(
        &source_path,
        "pub fn answer() -> u8 { 42 }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn answer_is_known() { assert_eq!(answer(), 42); }\n}\n",
    )
    .expect("source");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments.clone()),
        std::process::ExitCode::SUCCESS
    );
    let source = fs::read_to_string(&source_path).expect("fixed source");
    assert!(source.contains("#[path = \"crate_root_tests.rs\"]"));
    assert!(!source.contains("mod tests {"));
    let tests = fs::read_to_string(directory.path().join("src/crate_root_tests.rs"))
        .expect("extracted tests");
    assert!(tests.contains("use crate::*;"));
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
}
