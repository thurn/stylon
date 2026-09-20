pub(crate) mod cargo_rules;
pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod diagnostic;
pub(crate) mod discovery;
pub(crate) mod engine;
pub(crate) mod imports;
pub(crate) mod qualification;
pub(crate) mod rules;
pub(crate) mod transaction;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

pub fn run(arguments: impl IntoIterator<Item = OsString>) -> ExitCode {
    match Cli::try_parse_from(arguments) {
        Ok(cli) => engine::run(&cli),
        Err(error) => {
            let exit_code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            ExitCode::from(exit_code)
        }
    }
}
