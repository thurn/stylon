use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Parser)]
#[command(version, about)]
pub(crate) struct Cli {
    /// Apply all fixes and validate the result.
    #[arg(long)]
    pub(crate) fix: bool,

    /// Select the output format.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,

    /// Use this configuration instead of searching parent directories.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Report phase timings.
    #[arg(long)]
    pub(crate) timings: bool,

    /// File or directory to scan.
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
}
