use std::fs;

use tempfile::tempdir;

use crate::config::Config;
use crate::discovery;

#[test]
fn rejects_nested_configuration_in_a_selected_directory() {
    let directory = tempdir().expect("temporary directory");
    fs::write(directory.path().join("stylon.toml"), "version = 1\n").expect("root configuration");
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).expect("nested directory");
    fs::write(nested.join("stylon.toml"), "version = 1\n").expect("nested configuration");
    fs::write(nested.join("module.rs"), "fn item() {}\n").expect("source");
    let config = Config::load(directory.path(), None).expect("configuration");
    let requested = fs::canonicalize(directory.path()).expect("requested path");

    let errors =
        discovery::discover(&config, &requested).expect_err("nested configuration must fail");

    assert_eq!(errors[0].category, "configuration");
    assert_eq!(
        errors[0].paths,
        [std::path::PathBuf::from("nested/stylon.toml")]
    );
}
