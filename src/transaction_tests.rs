use crate::transaction::truncated;

#[test]
fn truncates_large_validation_output() {
    let output = truncated(&vec![b'a'; 1024 * 1024 + 1]);
    assert!(output.ends_with("[output truncated at 1 MiB]"));
}
