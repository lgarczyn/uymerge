//! Driver entry point.
//! SPEC section 5.
//! Packet P9.

use std::process::ExitCode;

use uymerge::cli;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    cli::run(&args)
}
