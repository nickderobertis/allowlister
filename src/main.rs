//! Thin binary entry point: parse arguments, run, map the typed result to a
//! process exit code, and report any error to stderr. All behavior lives in the
//! library so it can be tested directly.

use std::process::ExitCode;

fn main() -> ExitCode {
    match allowlister::run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("allowlister: {err}");
            ExitCode::FAILURE
        }
    }
}
