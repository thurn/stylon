use std::fs;

use tempfile::tempdir;

use crate::transaction::{Change, atomic_replace, prepare_journal, restore_transaction, truncated};

#[test]
fn truncates_large_validation_output() {
    let output = truncated(&vec![b'a'; 1024 * 1024 + 1]);
    assert!(output.ends_with("[output truncated at 1 MiB]"));
}

#[test]
fn recovery_restores_a_deleted_file() {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("module.rs");
    fs::write(&path, b"original\n").expect("original");
    let permissions = fs::metadata(&path).expect("metadata").permissions();
    let transaction = directory.path().join(".stylon-transaction");
    let change = Change {
        path: path.clone(),
        original: Some(b"original\n".to_vec()),
        replacement: None,
        permissions: Some(permissions),
    };

    prepare_journal(
        directory.path(),
        &transaction,
        std::slice::from_ref(&change),
    )
    .expect("journal");
    atomic_replace(&change).expect("deletion");
    assert!(!path.exists());
    restore_transaction(directory.path(), &transaction).expect("recovery");
    assert_eq!(fs::read(path).expect("restored file"), b"original\n");
}
