use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Position {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagnosticRange {
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
    pub(crate) start: Position,
    pub(crate) end: Position,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Diagnostic {
    pub(crate) rule_id: &'static str,
    pub(crate) message: String,
    pub(crate) path: PathBuf,
    pub(crate) range: DiagnosticRange,
    pub(crate) fix: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) applied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct OperationalError {
    pub(crate) category: &'static str,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Summary {
    pub(crate) files: usize,
    pub(crate) findings: usize,
    pub(crate) fixed: usize,
    pub(crate) remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timings: Option<Timings>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Timings {
    pub(crate) discovery_ms: u64,
    pub(crate) parsing_ms: u64,
    pub(crate) rule_evaluation_ms: u64,
    pub(crate) output_ms: u64,
    pub(crate) files: usize,
    pub(crate) bytes: usize,
    pub(crate) syntax_nodes: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonOutput<'a> {
    pub(crate) schema_version: u8,
    pub(crate) diagnostics: &'a [Diagnostic],
    pub(crate) errors: &'a [OperationalError],
    pub(crate) summary: &'a Summary,
}

impl Diagnostic {
    pub(crate) fn new(
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

    pub(crate) fn sort_key(&self) -> (&Path, usize, &str) {
        (&self.path, self.range.byte_start, self.rule_id)
    }
}

fn position(source: &str, offset: usize) -> Position {
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count() + 1;
    Position { line, column }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Position};

    #[test]
    fn positions_count_unicode_scalars_and_crlf_as_one_line() {
        let diagnostic = Diagnostic::new("test", "message", "a.rs".into(), "aé\r\nz", 5, 6);
        assert_eq!(diagnostic.range.start, Position { line: 2, column: 1 });
        assert_eq!(diagnostic.range.end, Position { line: 2, column: 2 });
    }
}
