use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiagnosticRange {
    pub byte_start: usize,
    pub byte_end: usize,
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub rule_id: &'static str,
    pub message: String,
    pub path: PathBuf,
    pub range: DiagnosticRange,
    pub fix: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub applied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationalError {
    pub category: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Summary {
    pub files: usize,
    pub findings: usize,
    pub fixed: usize,
    pub remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Timings {
    pub discovery_ms: u64,
    pub parsing_ms: u64,
    pub rule_evaluation_ms: u64,
    pub output_ms: u64,
    pub files: usize,
    pub bytes: usize,
    pub syntax_nodes: usize,
}

#[derive(Debug, Serialize)]
pub struct JsonOutput<'a> {
    pub schema_version: u8,
    pub diagnostics: &'a [Diagnostic],
    pub errors: &'a [OperationalError],
    pub summary: &'a Summary,
}

impl Diagnostic {
    pub fn new(
        rule_id: &'static str,
        message: impl Into<String>,
        path: PathBuf,
        source: &str,
        byte_start: usize,
        byte_end: usize,
    ) -> Self {
        Self {
            rule_id,
            message: message.into(),
            path,
            range: DiagnosticRange {
                byte_start,
                byte_end,
                start: position(source, byte_start),
                end: position(source, byte_end),
            },
            fix: "machine-applicable",
            applied: false,
        }
    }

    pub fn sort_key(&self) -> (&Path, usize, &str) {
        (&self.path, self.range.byte_start, self.rule_id)
    }

    pub fn without_fix(mut self) -> Self {
        self.fix = "none";
        self
    }
}

fn position(source: &str, offset: usize) -> Position {
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count() + 1;
    Position { line, column }
}

#[path = "diagnostic_tests.rs"]
#[cfg(test)]
mod tests;
