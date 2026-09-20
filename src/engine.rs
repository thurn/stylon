use std::ffi::OsStr;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use ra_ap_syntax::{Edition, SourceFile};
use rayon::prelude::*;

use crate::cargo_rules::{self, ManifestInput};
use crate::cli::{Cli, OutputFormat};
use crate::config::Config;
use crate::diagnostic::{Diagnostic, JsonOutput, OperationalError, Summary, Timings};
use crate::discovery;
use crate::imports::RustInput;
use crate::rules;
use crate::rules::Edit;
use crate::transaction::{self, Change};
use toml::Value;

pub(crate) fn run(cli: &Cli) -> ExitCode {
    let started = Instant::now();
    if let Ok(metadata) = fs::symlink_metadata(&cli.path)
        && metadata.file_type().is_symlink()
    {
        return finish(
            cli,
            Vec::new(),
            vec![OperationalError {
                category: "filesystem",
                message: format!(
                    "the explicitly selected path may not be a symbolic link: {}",
                    cli.path.display()
                ),
                paths: vec![cli.path.clone()],
            }],
            Summary::default(),
        );
    }
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
        .flat_map_iter(|file| {
            let relative = config.relative(&file.path);
            let findings = if file.path.extension() == Some(OsStr::new("rs")) {
                rules::check(&config, &relative, &file.source)
            } else {
                rules::check_manifest(&config, &relative, &file.source)
            };
            findings.into_iter().map(|finding| finding.diagnostic)
        })
        .collect();
    let manifest_inputs: Vec<_> = parsed
        .iter()
        .filter(|file| file.path.file_name() == Some(OsStr::new("Cargo.toml")))
        .map(|file| ManifestInput {
            path: &file.path,
            relative: config.relative(&file.path),
            source: &file.source,
        })
        .collect();
    let workspace = cargo_rules::analyze_workspace(&config, &manifest_inputs);
    diagnostics.extend(workspace.diagnostics);
    errors.extend(workspace.errors);
    let rust_inputs: Vec<_> = parsed
        .iter()
        .filter(|file| file.path.extension() == Some(OsStr::new("rs")))
        .map(|file| RustInput {
            path: &file.path,
            relative: config.relative(&file.path),
            source: &file.source,
        })
        .collect();
    let inline_tests = crate::tests_layout::analyze_inline_tests(&config, &rust_inputs);
    diagnostics.extend(inline_tests.diagnostics.clone());
    errors.extend(inline_tests.errors.clone());
    let inline_sources: Vec<_> = rust_inputs
        .iter()
        .map(|input| {
            inline_tests
                .replacements
                .get(input.path)
                .cloned()
                .unwrap_or_else(|| input.source.to_owned())
        })
        .collect();
    let mut inline_inputs: Vec<_> = rust_inputs
        .iter()
        .zip(&inline_sources)
        .map(|(input, source)| RustInput {
            path: input.path,
            relative: input.relative.clone(),
            source,
        })
        .collect();
    inline_inputs.extend(
        inline_tests
            .creations
            .iter()
            .map(|(path, source)| RustInput {
                path,
                relative: config.relative(path),
                source,
            }),
    );
    let mut virtual_manifests: std::collections::BTreeMap<_, _> = manifest_inputs
        .iter()
        .map(|input| (input.path.to_path_buf(), input.source.to_owned()))
        .collect();
    virtual_manifests.extend(workspace.replacements.clone());
    let mut file_suffix =
        crate::tests_layout::analyze_file_suffix(&config, &inline_inputs, &virtual_manifests);
    for (source, destination) in &file_suffix.moves {
        if let Some(replacement) = file_suffix.replacements.remove(source)
            && let Some(created) = file_suffix.creations.get_mut(destination)
        {
            *created = replacement;
        }
    }
    diagnostics.extend(file_suffix.diagnostics.clone());
    errors.extend(file_suffix.errors.clone());

    let mut structural_sources: std::collections::BTreeMap<_, _> = inline_inputs
        .iter()
        .map(|input| (input.path.to_path_buf(), input.source.to_owned()))
        .collect();
    structural_sources.extend(file_suffix.replacements.clone());
    for path in &file_suffix.deletions {
        structural_sources.remove(path);
    }
    structural_sources.extend(file_suffix.creations.clone());
    let structural_inputs: Vec<_> = structural_sources
        .iter()
        .map(|(path, source)| RustInput {
            path,
            relative: config.relative(path),
            source,
        })
        .collect();
    let public_functions = crate::imports::analyze_public_functions(&config, &structural_inputs);
    diagnostics.extend(public_functions.diagnostics);
    let mut project_replacements = workspace.replacements;
    project_replacements.extend(inline_tests.replacements);
    project_replacements.extend(file_suffix.replacements);
    let mut creations = inline_tests.creations;
    creations.extend(file_suffix.creations);
    for (path, replacement) in public_functions.replacements {
        if let Some(created) = creations.get_mut(&path) {
            *created = replacement;
        } else {
            project_replacements.insert(path, replacement);
        }
    }
    if let Some(timings) = &mut summary.timings {
        timings.rule_evaluation_ms = milliseconds(evaluation_started.elapsed());
    }

    if cli.fix && errors.is_empty() && !diagnostics.is_empty() {
        match plan_changes(
            &config,
            &parsed,
            &project_replacements,
            &creations,
            &file_suffix.deletions,
            &file_suffix.moves,
        ) {
            Ok(changes) => match verify_plan(&config, &parsed, &changes) {
                Ok(()) => {
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
            },
            Err(error) => errors.push(error),
        }
    }

    finish(cli, diagnostics, errors, summary)
}

fn verify_plan(
    config: &Config,
    parsed: &[ParsedFile],
    changes: &[Change],
) -> Result<(), OperationalError> {
    let mut files: std::collections::BTreeMap<_, _> = parsed
        .iter()
        .map(|file| (file.path.clone(), file.source.clone()))
        .collect();
    for change in changes {
        if let Some(replacement) = &change.replacement {
            let source = String::from_utf8(replacement.clone()).map_err(|_| OperationalError {
                category: "planning",
                message: format!(
                    "planned replacement for {} is not UTF-8",
                    config.relative(&change.path).display()
                ),
                paths: vec![config.relative(&change.path)],
            })?;
            files.insert(change.path.clone(), source);
        } else {
            files.remove(&change.path);
        }
    }

    let mut diagnostics = Vec::new();
    let mut errors = Vec::new();
    for (path, source) in &files {
        let relative = config.relative(path);
        if path.extension() == Some(OsStr::new("rs")) {
            ensure_parseable(&relative, source)?;
            diagnostics.extend(
                rules::check(config, &relative, source)
                    .into_iter()
                    .map(|finding| finding.diagnostic),
            );
        } else if path.file_name() == Some(OsStr::new("Cargo.toml")) {
            diagnostics.extend(
                rules::check_manifest(config, &relative, source)
                    .into_iter()
                    .map(|finding| finding.diagnostic),
            );
        }
    }
    let manifest_inputs: Vec<_> = files
        .iter()
        .filter(|(path, _)| path.file_name() == Some(OsStr::new("Cargo.toml")))
        .map(|(path, source)| ManifestInput {
            path,
            relative: config.relative(path),
            source,
        })
        .collect();
    let workspace = cargo_rules::analyze_workspace(config, &manifest_inputs);
    diagnostics.extend(workspace.diagnostics);
    errors.extend(workspace.errors);
    let rust_inputs: Vec<_> = files
        .iter()
        .filter(|(path, _)| path.extension() == Some(OsStr::new("rs")))
        .map(|(path, source)| RustInput {
            path,
            relative: config.relative(path),
            source,
        })
        .collect();
    let inline_tests = crate::tests_layout::analyze_inline_tests(config, &rust_inputs);
    diagnostics.extend(inline_tests.diagnostics);
    errors.extend(inline_tests.errors);
    let manifests = manifest_inputs
        .iter()
        .map(|input| (input.path.to_path_buf(), input.source.to_owned()))
        .collect();
    let file_suffix = crate::tests_layout::analyze_file_suffix(config, &rust_inputs, &manifests);
    diagnostics.extend(file_suffix.diagnostics);
    errors.extend(file_suffix.errors);
    let public_functions = crate::imports::analyze_public_functions(config, &rust_inputs);
    diagnostics.extend(public_functions.diagnostics);

    if errors.is_empty() && diagnostics.is_empty() {
        Ok(())
    } else {
        let mut paths: Vec<_> = diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.path)
            .chain(errors.into_iter().flat_map(|error| error.paths))
            .collect();
        paths.sort();
        paths.dedup();
        Err(OperationalError {
            category: "non-converging-fix",
            message: "the complete virtual fix still contains enabled findings or analysis errors"
                .to_owned(),
            paths,
        })
    }
}

#[derive(Debug)]
struct ParsedFile {
    path: PathBuf,
    source: String,
    syntax_nodes: usize,
}

fn plan_changes(
    config: &Config,
    parsed: &[ParsedFile],
    workspace_replacements: &std::collections::BTreeMap<PathBuf, String>,
    creations: &std::collections::BTreeMap<PathBuf, String>,
    deletions: &std::collections::BTreeSet<PathBuf>,
    moves: &std::collections::BTreeMap<PathBuf, PathBuf>,
) -> Result<Vec<Change>, OperationalError> {
    let mut changes: Vec<_> = parsed
        .iter()
        .filter(|file| !deletions.contains(&file.path))
        .filter_map(|file| {
            let relative = config.relative(&file.path);
            let initial = workspace_replacements
                .get(&file.path)
                .map_or(file.source.as_str(), String::as_str);
            let fixed = if file.path.extension() == Some(OsStr::new("rs")) {
                fixed_source(config, &relative, initial)
            } else {
                fixed_manifest(config, &relative, initial)
            };
            match fixed {
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
                        original: Some(file.source.as_bytes().to_vec()),
                        replacement: Some(replacement.into_bytes()),
                        permissions: Some(permissions),
                    }))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Result<_, _>>()?;
    for path in deletions {
        let file = parsed
            .iter()
            .find(|file| &file.path == path)
            .expect("deleted file came from the parsed snapshot");
        let permissions = fs::metadata(path).map_err(|source| OperationalError {
            category: "filesystem",
            message: format!(
                "cannot inspect {}: {source}",
                config.relative(path).display()
            ),
            paths: vec![config.relative(path)],
        })?;
        changes.push(Change {
            path: path.clone(),
            original: Some(file.source.as_bytes().to_vec()),
            replacement: None,
            permissions: Some(permissions.permissions()),
        });
    }
    for (path, source) in creations {
        let relative = config.relative(path);
        let module_relative = moves
            .iter()
            .find_map(|(source, destination)| {
                (destination == path).then(|| config.relative(source))
            })
            .unwrap_or_else(|| relative.clone());
        let replacement = fixed_source_with_module(config, &relative, &module_relative, source)?;
        changes.push(Change {
            path: path.clone(),
            original: None,
            replacement: Some(replacement.into_bytes()),
            permissions: None,
        });
    }
    Ok(changes)
}

fn fixed_manifest(
    config: &Config,
    relative: &std::path::Path,
    source: &str,
) -> Result<String, OperationalError> {
    let findings = rules::check_manifest(config, relative, source);
    let edits: Vec<_> = findings
        .into_iter()
        .flat_map(|finding| finding.edits)
        .collect();
    ensure_non_overlapping(relative, &edits)?;
    let mut fixed = source.to_owned();
    for edit in edits.into_iter().rev() {
        fixed.replace_range(edit.range, &edit.replacement);
    }
    if rules::check_manifest(config, relative, &fixed).is_empty() {
        Ok(fixed)
    } else {
        Err(OperationalError {
            category: "planning",
            message: format!("manifest fixes did not converge for {}", relative.display()),
            paths: vec![relative.to_path_buf()],
        })
    }
}

fn fixed_source(
    config: &Config,
    relative: &std::path::Path,
    original: &str,
) -> Result<String, OperationalError> {
    fixed_source_with_module(config, relative, relative, original)
}

fn fixed_source_with_module(
    config: &Config,
    relative: &std::path::Path,
    module_relative: &std::path::Path,
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
        source = apply_qualification_edits(config, relative, module_relative, source)?;
        ensure_parseable(relative, &source)?;
        source = apply_rule_edits(config, relative, source, "imports.top-level")?;
        ensure_parseable(relative, &source)?;
        source = apply_rule_edits(config, relative, source, "rustdoc.type-links")?;
        ensure_parseable(relative, &source)?;
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

fn apply_qualification_edits(
    config: &Config,
    relative: &std::path::Path,
    module_relative: &std::path::Path,
    mut source: String,
) -> Result<String, OperationalError> {
    let mut edits: Vec<_> =
        crate::qualification::check_with_module(config, relative, module_relative, &source)
            .into_iter()
            .flat_map(|finding| finding.edits)
            .collect();
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    ensure_non_overlapping(relative, &edits)?;
    for edit in edits.into_iter().rev() {
        source.replace_range(edit.range, &edit.replacement);
    }
    Ok(source)
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
    edits: &[Edit],
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
        toml::from_str::<Value>(&source).map_err(|source| OperationalError {
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

#[path = "engine_tests.rs"]
#[cfg(test)]
mod tests;
