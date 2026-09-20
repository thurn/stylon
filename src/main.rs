use std::process::ExitCode;

fn main() -> ExitCode {
    stylon::run(std::env::args_os())
}
