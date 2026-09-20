use crate::diagnostic::{Diagnostic, Position};

#[test]
fn positions_count_unicode_scalars_and_crlf_as_one_line() {
    let diagnostic = Diagnostic::new("test", "message", "a.rs".into(), "aé\r\nz", 5, 6);
    assert_eq!(diagnostic.range.start, Position { line: 2, column: 1 });
    assert_eq!(diagnostic.range.end, Position { line: 2, column: 2 });
}
