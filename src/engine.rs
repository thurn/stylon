use std::ffi::OsStr;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use ra_ap_syntax::{Edition, SourceFile};
use rayon::prelude::*;

use crate::cli::{Cli, OutputFormat};
use crate::config::Config;
use crate::diagnostic::{Diagnostic, JsonOutput, OperationalError, Summary, Timings};
use crate::discovery;
use crate::rules;
use crate::transaction::{self, Change};

pub(crate) fn run(cli: &Cli) -> ExitCode {
    let started = Instant::now();
    let config = match Config::load(&cli.path, cli.config.as_deref()) {
        Ok(config) => config,
        Err(error) => return finish(cli, Vec::new(), vec![error], Summary::default()),
    };
    if let Err(error) = transaction::recover_if_needed(&config, cli.fix) {
        return finish(cli, Vec::new(), vec![error], Summary::default());
    }
    let requested = match fs::canonicalize(&cli.path) {
        Ok(path) => path,
        Err(source) => {
            return finish(
                cli,
                Vec::new(),
                vec![OperationalError {
                    category: "discovery",
                    message: format!("cannot access {}: {source}", cli.path.display()),
                    paths: vec![cli.path.clone()],
                }],
                Summary::default(),
            );
        }
    };
    let files = match discovery::discover(&config, &requested) {
        Ok(files) => files,
        Err(errors) => return finish(cli, Vec::new(), errors, Summary::default()),
    };
    let discovery_elapsed = started.elapsed();

    let parsing_started = Instant::now();
    let (parsed, mut errors) = parse_files(&config, &files);
    let parsing_elapsed = parsing_started.elapsed();
    let mut summary = Summary {
        files: files.len(),
        findings: 0,
        fixed: 0,
        remaining: 0,
        timings: cli.timings.then(|| Timings {
            discovery_ms: milliseconds(discovery_elapsed),
            parsing_ms: milliseconds(parsing_elapsed),
            rule_evaluation_ms: 0,
            output_ms: 0,
            files: files.len(),
            bytes: parsed.iter().map(|file| file.source.len()).sum(),
            syntax_nodes: parsed.iter().map(|file| file.syntax_nodes).sum(),
        }),
    };
    let evaluation_started = Instant::now();
    let mut diagnostics: Vec<_> = parsed
        .par_iter()
        .filter(|file| file.path.extension() == Some(OsStr::new("rs")))
        .flat_map_iter(|file| {
            rules::check(&config, &config.relative(&file.path), &file.source)
                .into_iter()
                .map(|finding| finding.diagnostic)
        })
        .collect();
    if let Some(timings) = &mut summary.timings {
        timings.rule_evaluation_ms = milliseconds(evaluation_started.elapsed());
    }

    if cli.fix && errors.is_empty() && !diagnostics.is_empty() {
        match plan_changes(&config, &parsed) {
            Ok(changes) => {
                let inventory = inventory(&config, &parsed);
                match transaction::apply(&config, &changes, &inventory) {
                    Ok(()) => {
                        for diagnostic in &mut diagnostics {
                            diagnostic.applied = true;
                        }
                        summary.fixed = diagnostics.len();
                    }
                    Err(error) => errors.push(error),
                }
            }
            Err(error) => errors.push(error),
        }
    }

    finish(cli, diagnostics, errors, summary)
}

#[derive(Debug)]
struct ParsedFile {
    path: PathBuf,
    source: String,
    syntax_nodes: usize,
}

fn plan_changes(config: &Config, parsed: &[ParsedFile]) -> Result<Vec<Change>, OperationalError> {
    parsed
        .iter()
        .filter(|file| file.path.extension() == Some(OsStr::new("rs")))
        .filter_map(|file| {
            let relative = config.relative(&file.path);
            match fixed_source(config, &relative, &file.source) {
                Ok(replacement) if replacement != file.source => {
                    let permissions = match fs::metadata(&file.path) {
                        Ok(metadata) => metadata.permissions(),
                        Err(source) => {
                            return Some(Err(OperationalError {
                                category: "filesystem",
                                message: format!("cannot inspect {}: {source}", relative.display()),
                                paths: vec![relative],
                            }));
                        }
                    };
                    Some(Ok(Change {
                        path: file.path.clone(),
                        original: file.source.as_bytes().to_vec(),
                        replacement: replacement.into_bytes(),
                        permissions,
                    }))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn fixed_source(
    config: &Config,
    relative: &std::path::Path,
    original: &str,
) -> Result<String, OperationalError> {
    let mut source = original.to_owned();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..8 {
        if !seen.insert(source.clone()) {
            return Err(OperationalError {
                category: "non-converging-fix",
                message: format!("fixes cycle for {}", relative.display()),
                paths: vec![relative.to_path_buf()],
            });
        }
        let before = source.clone();
        source = apply_rule_edits(config, relative, source, "items.order")?;
        ensure_parseable(relative, &source)?;
        source = apply_rule_edits(config, relative, source, "items.blank-lines")?;
        ensure_parseable(relative, &source)?;
        if source == before {
            if rules::check(config, relative, &source).is_empty() {
                return Ok(source);
            }
            return Err(OperationalError {
                category: "planning",
                message: format!("enabled findings remain in {}", relative.display()),
                paths: vec![relative.to_path_buf()],
            });
        }
    }
    Err(OperationalError {
        category: "non-converging-fix",
        message: format!("fixes exceeded eight passes for {}", relative.display()),
        paths: vec![relative.to_path_buf()],
    })
}

fn apply_rule_edits(
    config: &Config,
    relative: &std::path::Path,
    mut source: String,
    rule_id: &str,
) -> Result<String, OperationalError> {
    let mut edits: Vec<_> = rules::check(config, relative, &source)
        .into_iter()
        .filter(|finding| finding.diagnostic.rule_id == rule_id)
        .flat_map(|finding| finding.edits)
        .collect();
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    ensure_non_overlapping(relative, &edits)?;
    for edit in edits.into_iter().rev() {
        source.replace_range(edit.range, &edit.replacement);
    }
    Ok(source)
}

fn ensure_non_overlapping(
    relative: &std::path::Path,
    edits: &[rules::Edit],
) -> Result<(), OperationalError> {
    if edits
        .windows(2)
        .any(|pair| ranges_overlap(&pair[0].range, &pair[1].range))
    {
        Err(OperationalError {
            category: "planning",
            message: format!("fixes overlap in {}", relative.display()),
            paths: vec![relative.to_path_buf()],
        })
    } else {
        Ok(())
    }
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.end > right.start || left.start == right.start
}

fn ensure_parseable(relative: &std::path::Path, source: &str) -> Result<(), OperationalError> {
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    if parsed.errors().is_empty() {
        Ok(())
    } else {
        Err(OperationalError {
            category: "planning",
            message: format!("a planned edit makes {} invalid Rust", relative.display()),
            paths: vec![relative.to_path_buf()],
        })
    }
}

fn inventory(config: &Config, parsed: &[ParsedFile]) -> Vec<(PathBuf, Vec<u8>)> {
    let mut inventory: Vec<_> = parsed
        .iter()
        .map(|file| (file.path.clone(), file.source.as_bytes().to_vec()))
        .collect();
    if let Some(path) = &config.config_path
        && let Ok(bytes) = fs::read(path)
    {
        inventory.push((path.clone(), bytes));
    }
    let lockfile = config.root.join("Cargo.lock");
    if let Ok(bytes) = fs::read(&lockfile) {
        inventory.push((lockfile, bytes));
    }
    inventory
}

fn parse_files(config: &Config, paths: &[PathBuf]) -> (Vec<ParsedFile>, Vec<OperationalError>) {
    let results: Vec<_> = paths
        .par_iter()
        .map(|path| parse_file(config, path))
        .collect();
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(file) => parsed.push(file),
            Err(error) => errors.push(error),
        }
    }
    errors.sort_by(|left, right| left.paths.cmp(&right.paths));
    (parsed, errors)
}

fn parse_file(config: &Config, path: &PathBuf) -> Result<ParsedFile, OperationalError> {
    let relative = config.relative(path);
    let bytes = fs::read(path).map_err(|source| OperationalError {
        category: "filesystem",
        message: format!("cannot read {}: {source}", relative.display()),
        paths: vec![relative.clone()],
    })?;
    let source = String::from_utf8(bytes).map_err(|_| OperationalError {
        category: "filesystem",
        message: format!("{} is not valid UTF-8", relative.display()),
        paths: vec![relative.clone()],
    })?;

    if path.extension() == Some(OsStr::new("rs")) {
        let parsed = SourceFile::parse(&source, Edition::Edition2024);
        if !parsed.errors().is_empty() {
            let messages = parsed
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(OperationalError {
                category: "parse",
                message: format!("{}: {messages}", relative.display()),
                paths: vec![relative],
            });
        }
        Ok(ParsedFile {
            path: path.clone(),
            source,
            syntax_nodes: parsed.syntax_node().descendants().count(),
        })
    } else {
        toml::from_str::<toml::Value>(&source).map_err(|source| OperationalError {
            category: "parse",
            message: format!("{}: {source}", relative.display()),
            paths: vec![relative],
        })?;
        Ok(ParsedFile {
            path: path.clone(),
            source,
            syntax_nodes: 0,
        })
    }
}

fn finish(
    cli: &Cli,
    mut diagnostics: Vec<Diagnostic>,
    mut errors: Vec<OperationalError>,
    mut summary: Summary,
) -> ExitCode {
    diagnostics.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    errors.sort_by(|left, right| {
        left.paths
            .cmp(&right.paths)
            .then_with(|| left.category.cmp(right.category))
            .then_with(|| left.message.cmp(&right.message))
    });
    summary.findings = diagnostics.len();
    summary.remaining = diagnostics.iter().filter(|item| !item.applied).count();
    let output_started = Instant::now();
    match cli.format {
        OutputFormat::Human => print_human(&diagnostics, &errors),
        OutputFormat::Json => print_json(&diagnostics, &errors, &summary),
    }
    if let Some(timings) = &mut summary.timings {
        timings.output_ms = milliseconds(output_started.elapsed());
        if cli.format == OutputFormat::Human {
            print_timings(timings);
        }
    }
    if errors.is_empty() {
        ExitCode::from(u8::from(summary.remaining > 0))
    } else {
        ExitCode::from(2)
    }
}

fn print_human(diagnostics: &[Diagnostic], errors: &[OperationalError]) {
    for diagnostic in diagnostics {
        println!(
            "{}:{}:{}: {}: {}",
            diagnostic.path.display(),
            diagnostic.range.start.line,
            diagnostic.range.start.column,
            diagnostic.rule_id,
            diagnostic.message
        );
    }
    for error in errors {
        eprintln!("{}: {}", error.category, error.message);
    }
}

fn print_json(diagnostics: &[Diagnostic], errors: &[OperationalError], summary: &Summary) {
    let output = JsonOutput {
        schema_version: 1,
        diagnostics,
        errors,
        summary,
    };
    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(source) => eprintln!("json: cannot serialize output: {source}"),
    }
}

fn print_timings(timings: &Timings) {
    eprintln!(
        "timings: discovery={} parsing={} rules={} output={} files={} bytes={} nodes={}",
        display_duration(timings.discovery_ms),
        display_duration(timings.parsing_ms),
        display_duration(timings.rule_evaluation_ms),
        display_duration(timings.output_ms),
        timings.files,
        timings.bytes,
        timings.syntax_nodes
    );
}

fn display_duration(milliseconds: u64) -> String {
    format!(
        "{:.3}s",
        std::time::Duration::from_millis(milliseconds).as_secs_f64()
    )
}

fn milliseconds(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
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
}
