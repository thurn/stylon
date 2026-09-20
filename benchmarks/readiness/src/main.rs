use ra_ap_syntax::{Edition, SourceFile};
use rayon::prelude::*;
fn main() {
    let paths = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let counts: Vec<_> = paths
        .lines()
        .collect::<Vec<_>>()
        .par_iter()
        .map(|path| {
            let source = std::fs::read_to_string(path).unwrap();
            let parsed = SourceFile::parse(&source, Edition::Edition2024);
            (
                source.len(),
                parsed.syntax_node().descendants().count(),
                parsed.errors().len(),
            )
        })
        .collect();
    println!(
        "files={} bytes={} nodes={} errors={}",
        counts.len(),
        counts.iter().map(|x| x.0).sum::<usize>(),
        counts.iter().map(|x| x.1).sum::<usize>(),
        counts.iter().map(|x| x.2).sum::<usize>()
    );
}
