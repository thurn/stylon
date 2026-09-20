use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::diagnostic::OperationalError;

const LOCK_FILE: &str = ".stylon.lock";
const TRANSACTION_DIRECTORY: &str = ".stylon-transaction";

#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub(crate) path: PathBuf,
    pub(crate) original: Vec<u8>,
    pub(crate) replacement: Vec<u8>,
    pub(crate) permissions: fs::Permissions,
}

pub(crate) fn recover_if_needed(config: &Config, fix: bool) -> Result<(), OperationalError> {
    let transaction = config.root.join(TRANSACTION_DIRECTORY);
    if !transaction.exists() {
        return Ok(());
    }
    if !fix {
        return Err(error(
            "recovery",
            "an unfinished transaction exists; run Stylon with --fix to recover it",
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        ));
    }
    let _lock = ProjectLock::acquire(&config.root)?;
    restore_transaction(&config.root, &transaction)?;
    fs::remove_dir_all(&transaction).map_err(|source| {
        error(
            "recovery",
            format!("cannot remove recovered transaction: {source}"),
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        )
    })?;
    Ok(())
}

pub(crate) fn apply(
    config: &Config,
    changes: &[Change],
    inventory: &[(PathBuf, Vec<u8>)],
) -> Result<(), OperationalError> {
    if changes.is_empty() {
        return Ok(());
    }
    if config.validation_is_default && !config.root.join("Cargo.lock").is_file() {
        return Err(error(
            "baseline-validation",
            "default validation requires an existing Cargo.lock; prepare the project or configure an explicit validation command",
            vec![PathBuf::from("Cargo.lock")],
        ));
    }
    let _lock = ProjectLock::acquire(&config.root)?;
    verify_inventory(inventory)?;
    validate(config, "baseline-validation")?;
    verify_inventory(inventory)?;

    let transaction = config.root.join(TRANSACTION_DIRECTORY);
    prepare_journal(&config.root, &transaction, changes)?;
    for change in changes {
        atomic_replace(change)?;
    }

    if let Err(validation_error) = validate(config, "validation") {
        match restore_transaction(&config.root, &transaction) {
            Ok(()) => {
                fs::remove_dir_all(&transaction).map_err(|source| {
                    error(
                        "recovery",
                        format!("validation failed and cleanup failed: {source}"),
                        vec![PathBuf::from(TRANSACTION_DIRECTORY)],
                    )
                })?;
                return Err(validation_error);
            }
            Err(recovery_error) => return Err(recovery_error),
        }
    }

    fs::remove_dir_all(&transaction).map_err(|source| {
        error(
            "filesystem",
            format!("fix succeeded but transaction cleanup failed: {source}"),
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        )
    })?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct Journal {
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct JournalEntry {
    relative: PathBuf,
    original: String,
    replacement: String,
}

fn prepare_journal(
    root: &Path,
    transaction: &Path,
    changes: &[Change],
) -> Result<(), OperationalError> {
    fs::create_dir(transaction).map_err(|source| {
        error(
            "filesystem",
            format!("cannot create transaction journal: {source}"),
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        )
    })?;
    set_private_permissions(transaction)?;

    let mut entries = Vec::with_capacity(changes.len());
    for (index, change) in changes.iter().enumerate() {
        let original = format!("{index}.original");
        let replacement = format!("{index}.replacement");
        durable_write(&transaction.join(&original), &change.original, None)?;
        durable_write(&transaction.join(&replacement), &change.replacement, None)?;
        entries.push(JournalEntry {
            relative: change
                .path
                .strip_prefix(root)
                .expect("changed path is below root")
                .to_path_buf(),
            original,
            replacement,
        });
    }
    let journal = serde_json::to_vec(&Journal { entries }).map_err(|source| {
        error(
            "filesystem",
            format!("cannot serialize transaction journal: {source}"),
            Vec::new(),
        )
    })?;
    durable_write(&transaction.join("journal.json"), &journal, None)?;
    sync_directory(transaction)?;
    Ok(())
}

fn restore_transaction(root: &Path, transaction: &Path) -> Result<(), OperationalError> {
    let journal_path = transaction.join("journal.json");
    let journal: Journal = serde_json::from_slice(&fs::read(&journal_path).map_err(|source| {
        error(
            "recovery",
            format!("cannot read transaction journal: {source}"),
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        )
    })?)
    .map_err(|source| {
        error(
            "recovery",
            format!("cannot parse transaction journal: {source}"),
            vec![PathBuf::from(TRANSACTION_DIRECTORY)],
        )
    })?;

    for entry in journal.entries {
        let path = root.join(&entry.relative);
        let original = fs::read(transaction.join(entry.original)).map_err(|source| {
            error(
                "recovery",
                format!(
                    "cannot read backup for {}: {source}",
                    entry.relative.display()
                ),
                vec![entry.relative.clone()],
            )
        })?;
        let replacement = fs::read(transaction.join(entry.replacement)).map_err(|source| {
            error(
                "recovery",
                format!(
                    "cannot read replacement for {}: {source}",
                    entry.relative.display()
                ),
                vec![entry.relative.clone()],
            )
        })?;
        let current = fs::read(&path).map_err(|source| {
            error(
                "recovery",
                format!("cannot inspect {}: {source}", entry.relative.display()),
                vec![entry.relative.clone()],
            )
        })?;
        if current != original && current != replacement {
            return Err(error(
                "recovery",
                format!(
                    "{} has an unknown third state; backup retained in {}",
                    entry.relative.display(),
                    transaction.display()
                ),
                vec![entry.relative],
            ));
        }
        if current != original {
            let permissions = fs::metadata(&path).map_err(|source| {
                error(
                    "recovery",
                    format!("cannot inspect {}: {source}", entry.relative.display()),
                    vec![entry.relative.clone()],
                )
            })?;
            replace_bytes(&path, &original, permissions.permissions())?;
        }
    }
    Ok(())
}

fn atomic_replace(change: &Change) -> Result<(), OperationalError> {
    replace_bytes(
        &change.path,
        &change.replacement,
        change.permissions.clone(),
    )
}

fn replace_bytes(
    path: &Path,
    replacement: &[u8],
    permissions: fs::Permissions,
) -> Result<(), OperationalError> {
    let parent = path.parent().expect("changed file has a parent");
    let name = path
        .file_name()
        .expect("changed file has a name")
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.stylon-{}", std::process::id()));
    durable_write(&temporary, replacement, Some(permissions))?;
    fs::rename(&temporary, path).map_err(|source| {
        error(
            "filesystem",
            format!("cannot replace {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })?;
    sync_directory(parent)
}

fn durable_write(
    path: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), OperationalError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path).map_err(|source| {
        error(
            "filesystem",
            format!("cannot create {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions).map_err(|source| {
            error(
                "filesystem",
                format!("cannot set permissions on {}: {source}", path.display()),
                vec![path.to_path_buf()],
            )
        })?;
    }
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| {
            error(
                "filesystem",
                format!("cannot write {}: {source}", path.display()),
                vec![path.to_path_buf()],
            )
        })
}

fn verify_inventory(inventory: &[(PathBuf, Vec<u8>)]) -> Result<(), OperationalError> {
    let mut changed = Vec::new();
    for (path, expected) in inventory {
        if !fs::read(path).is_ok_and(|actual| actual == *expected) {
            changed.push(path.clone());
        }
    }
    if changed.is_empty() {
        Ok(())
    } else {
        Err(error(
            "concurrent-modification",
            "project inputs changed during fix planning or validation",
            changed,
        ))
    }
}

fn validate(config: &Config, category: &'static str) -> Result<(), OperationalError> {
    let Some((program, arguments)) = config.validation_command.split_first() else {
        return Err(error(
            "configuration",
            "validation command may not be empty",
            Vec::new(),
        ));
    };
    let mut command = Command::new(program);
    command.args(arguments).current_dir(&config.root);
    if config.validation_is_default && config.root.join("Cargo.lock").is_file() {
        command.arg("--locked");
    }
    let output = command.output().map_err(|source| {
        error(
            category,
            format!("cannot run validation command {program:?}: {source}"),
            Vec::new(),
        )
    })?;
    if output.status.success() {
        Ok(())
    } else {
        let stdout = truncated(&output.stdout);
        let stderr = truncated(&output.stderr);
        Err(error(
            category,
            format!(
                "validation command exited with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            ),
            Vec::new(),
        ))
    }
}

fn truncated(bytes: &[u8]) -> String {
    const LIMIT: usize = 1024 * 1024;
    let end = bytes.len().min(LIMIT);
    let mut value = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if bytes.len() > LIMIT {
        value.push_str("\n[output truncated at 1 MiB]");
    }
    value
}

fn sync_directory(path: &Path) -> Result<(), OperationalError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            error(
                "filesystem",
                format!("cannot sync {}: {source}", path.display()),
                vec![path.to_path_buf()],
            )
        })
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), OperationalError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        error(
            "filesystem",
            format!("cannot protect {}: {source}", path.display()),
            vec![path.to_path_buf()],
        )
    })
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), OperationalError> {
    Ok(())
}

struct ProjectLock {
    file: File,
}

impl ProjectLock {
    fn acquire(root: &Path) -> Result<Self, OperationalError> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| {
                error(
                    "filesystem",
                    format!("cannot open project lock: {source}"),
                    vec![PathBuf::from(LOCK_FILE)],
                )
            })?;
        file.try_lock_exclusive().map_err(|source| {
            error(
                "lock",
                format!("another Stylon fixer owns the project lock: {source}"),
                vec![PathBuf::from(LOCK_FILE)],
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn error(
    category: &'static str,
    message: impl Into<String>,
    paths: Vec<PathBuf>,
) -> OperationalError {
    OperationalError {
        category,
        message: message.into(),
        paths,
    }
}

#[cfg(test)]
mod tests {
    use super::truncated;

    #[test]
    fn truncates_large_validation_output() {
        let output = truncated(&vec![b'a'; 1024 * 1024 + 1]);
        assert!(output.ends_with("[output truncated at 1 MiB]"));
    }
}
