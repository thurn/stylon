mod cargo_rules;
mod cli;
mod config;
mod diagnostic;
mod discovery;
mod engine;
mod imports;
mod qualification;
mod rules;
mod tests_layout;
mod transaction;

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
