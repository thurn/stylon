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
fn fix_does_not_extract_inline_tests() {
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
        super::super::run(arguments),
        std::process::ExitCode::FAILURE
    );
    let source = fs::read_to_string(&source_path).expect("fixed source");
    assert!(source.contains("mod tests {"));
    assert!(!directory.path().join("src/crate_root_tests.rs").exists());
}

#[test]
fn fix_renames_integration_tests_without_changing_target_name() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join("src")).expect("source directory");
    fs::create_dir(directory.path().join("tests")).expect("tests directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='integration_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    fs::write(
        directory.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"integration_fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("lockfile");
    fs::write(
        directory.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("library");
    let original = directory.path().join("tests/gameplay.rs");
    fs::write(&original, "#[test]\nfn gameplay() { assert_eq!(1, 1); }\n")
        .expect("integration test");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments.clone()),
        std::process::ExitCode::SUCCESS
    );
    assert!(!original.exists());
    assert!(directory.path().join("tests/gameplay_tests.rs").is_file());
    let manifest = fs::read_to_string(directory.path().join("Cargo.toml")).expect("manifest");
    assert!(manifest.contains("name = \"gameplay\""));
    assert!(manifest.contains("path = \"tests/gameplay_tests.rs\""));
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
}

#[test]
fn fix_carries_child_module_edits_through_an_integration_test_move() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join("src")).expect("source directory");
    fs::create_dir_all(directory.path().join("tests/coverage")).expect("tests directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='nested_test_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    fs::write(
        directory.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"nested_test_fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("lockfile");
    fs::write(directory.path().join("src/lib.rs"), "pub fn library() {}\n").expect("library");
    fs::write(
        directory.path().join("tests/contract.rs"),
        "mod coverage;\n\nfn helper() {}\n\n#[test]\nfn contract() { helper(); }\n",
    )
    .expect("integration root");
    fs::write(
        directory.path().join("tests/coverage/mod.rs"),
        "use super::helper;\n\n#[test]\nfn covered() { helper(); }\n",
    )
    .expect("child module");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments.clone()),
        std::process::ExitCode::SUCCESS
    );
    let root =
        fs::read_to_string(directory.path().join("tests/contract_tests.rs")).expect("moved root");
    assert!(
        root.contains("#[path = \"coverage/mod_tests.rs\"]"),
        "{root}"
    );
    let child = fs::read_to_string(directory.path().join("tests/coverage/mod_tests.rs"))
        .expect("moved child");
    assert!(child.contains("use super::helper;"));
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
}

#[test]
fn fix_extracts_tests_from_a_cargo_entry_file() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='entry_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    fs::write(
        directory.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"entry_fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("lockfile");
    let source_path = directory.path().join("src/lib.rs");
    fs::write(
        &source_path,
        "pub fn answer() -> u8 { 42 }\n\n#[test]\nfn answer_is_known() { assert_eq!(answer(), 42); }\n",
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
    let source = fs::read_to_string(&source_path).expect("source");
    assert!(!source.contains("fn answer_is_known"));
    assert!(source.contains("mod lib_tests;"));
    let tests = fs::read_to_string(directory.path().join("src/lib_tests.rs")).expect("tests");
    assert!(tests.contains("fn answer_is_known"));
    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::SUCCESS
    );
}

#[test]
fn baseline_validation_failure_writes_no_source_changes() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("stylon.toml"),
        "version = 1\n[validation]\ncommand = [\"false\"]\n",
    )
    .expect("configuration");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='failure_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    let source_path = directory.path().join("src/lib.rs");
    let original = b"fn private() {}\npub struct Public;\n";
    fs::write(&source_path, original).expect("source");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::from(2)
    );
    assert_eq!(fs::read(source_path).expect("unchanged source"), original);
    assert!(!directory.path().join(".stylon-transaction").exists());
    assert!(!directory.path().join(".stylon.lock").exists());
}

#[test]
fn post_fix_validation_failure_restores_source_changes() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir(directory.path().join("src")).expect("source directory");
    fs::write(
        directory.path().join("stylon.toml"),
        "version = 1\n[validation]\ncommand = [\"sh\", \"-c\", \"if test -f validation-marker; then exit 1; else touch validation-marker; fi\"]\n",
    )
    .expect("configuration");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='rollback_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    let source_path = directory.path().join("src/lib.rs");
    let original = b"fn private() {}\npub struct Public;\n";
    fs::write(&source_path, original).expect("source");
    let arguments = [
        OsString::from("stylon"),
        OsString::from("--fix"),
        directory.path().as_os_str().to_owned(),
    ];

    assert_eq!(
        super::super::run(arguments),
        std::process::ExitCode::from(2)
    );
    assert_eq!(fs::read(source_path).expect("restored source"), original);
    assert!(!directory.path().join(".stylon-transaction").exists());
    assert!(!directory.path().join(".stylon.lock").exists());
}
