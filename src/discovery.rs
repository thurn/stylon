use std::fs;
use std::path::{Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};

use crate::config::Config;
use crate::diagnostic::OperationalError;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

pub(crate) fn discover(
    config: &Config,
    requested: &Path,
) -> Result<Vec<PathBuf>, Vec<OperationalError>> {
    let requested = fs::canonicalize(requested).map_err(|source| {
        vec![error(
            "discovery",
            format!("cannot access {}: {source}", requested.display()),
            requested,
        )]
    })?;
    if requested.is_file() {
        return selected_file(config, requested).map(|path| path.into_iter().collect());
    }

    let mut builder = WalkBuilder::new(&requested);
    builder
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .filter_entry({
            let root = config.root.clone();
            let config = config.clone();
            move |entry| include_entry(&config, &root, entry)
        });

    let mut files = Vec::new();
    let mut errors = Vec::new();
    for result in builder.build() {
        match result {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                let path = entry.into_path();
                if is_source(&path) {
                    match selected_file(config, path) {
                        Ok(Some(path)) => files.push(path),
                        Ok(None) => {}
                        Err(mut found) => errors.append(&mut found),
                    }
                }
            }
            Ok(_) => {}
            Err(source) => errors.push(OperationalError {
                category: "discovery",
                message: source.to_string(),
                paths: Vec::new(),
            }),
        }
    }
    files.sort();
    if errors.is_empty() {
        Ok(files)
    } else {
        Err(errors)
    }
}

fn include_entry(config: &Config, root: &Path, entry: &DirEntry) -> bool {
    let path = entry.path();
    if path == root {
        return true;
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    let name = entry.file_name().to_string_lossy();
    name != ".git" && name != "target" && !config.is_excluded(relative)
}

fn selected_file(config: &Config, path: PathBuf) -> Result<Option<PathBuf>, Vec<OperationalError>> {
    if !path.starts_with(&config.root) {
        return Err(vec![error(
            "discovery",
            "selected path is outside the analysis root",
            &path,
        )]);
    }
    let relative = config.relative(&path);
    if config.is_excluded(&relative) || !is_source(&path) {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(&path).map_err(|source| {
        vec![error(
            "filesystem",
            format!("cannot inspect {}: {source}", relative.display()),
            &relative,
        )]
    })?;
    if metadata.file_type().is_symlink() {
        return Err(vec![error(
            "filesystem",
            "selected files may not be symbolic links",
            &relative,
        )]);
    }
    #[cfg(unix)]
    {
        if metadata.nlink() > 1 {
            return Err(vec![error(
                "filesystem",
                "selected files may not have hard links",
                &relative,
            )]);
        }
    }
    if path.to_str().is_none() {
        return Err(vec![error(
            "filesystem",
            "selected paths must be valid UTF-8",
            &relative,
        )]);
    }
    Ok(Some(path))
}

fn is_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
        || path.file_name().is_some_and(|name| name == "Cargo.toml")
}

fn error(category: &'static str, message: impl Into<String>, path: &Path) -> OperationalError {
    OperationalError {
        category,
        message: message.into(),
        paths: vec![path.to_path_buf()],
    }
}
