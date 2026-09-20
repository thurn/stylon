use std::fs;

use tempfile::tempdir;

use crate::config::Config;
use crate::transaction::{
    Change, JournalState, atomic_replace, prepare_journal, recover_if_needed, restore_transaction,
    truncated, write_journal_state,
};

#[test]
fn truncates_large_validation_output() {
    let output = truncated(&vec![b'a'; 1024 * 1024 + 1]);
    assert!(output.ends_with("[output truncated at 1 MiB]"));
}

#[test]
fn startup_only_cleans_a_committed_journal() {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("module.rs");
    fs::write(&path, b"original\n").expect("original");
    let permissions = fs::metadata(&path).expect("metadata").permissions();
    let transaction = directory.path().join(".stylon-transaction");
    let change = Change {
        path: path.clone(),
        original: Some(b"original\n".to_vec()),
        replacement: Some(b"replacement\n".to_vec()),
        permissions: Some(permissions),
    };
    prepare_journal(
        directory.path(),
        &transaction,
        std::slice::from_ref(&change),
        &[],
    )
    .expect("journal");
    atomic_replace(&change).expect("replacement");
    write_journal_state(&transaction, JournalState::Committed).expect("commit marker");
    let config = Config::load(directory.path(), None).expect("configuration");

    recover_if_needed(&config, false).expect("committed cleanup");

    assert_eq!(fs::read(path).expect("committed file"), b"replacement\n");
    assert!(!transaction.exists());
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
        &[],
    )
    .expect("journal");
    atomic_replace(&change).expect("deletion");
    assert!(!path.exists());
    restore_transaction(directory.path(), &transaction).expect("recovery");
    assert_eq!(fs::read(path).expect("restored file"), b"original\n");
}
