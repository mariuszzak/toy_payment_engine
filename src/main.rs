//! Command-line entry point: `payments-engine <transactions.csv>`.
//!
//! Exit codes: 0 on success (even if some rows were not applied), 1 when the input
//! cannot be read or the output cannot be written, 2 on wrong usage.

use std::env;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;

const USAGE: &str = "usage: payments-engine <transactions.csv>";

/// The input could not be read, or the output could not be written.
const EXIT_FAILURE: u8 = 1;
/// Not exactly one argument was given.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    // 1. Exactly one argument: the input file.
    let Some(path) = single_argument() else {
        report(USAGE);
        return ExitCode::from(EXIT_USAGE);
    };

    // 2. Process it; any error that stops the run is reported on stderr.
    match process_file(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&format!("error: {error:#}"));
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// The only command-line argument, or `None` if there are zero or several. Taken as an
/// `OsString`, so paths that are not valid UTF-8 still work.
fn single_argument() -> Option<PathBuf> {
    let mut args = env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(path), None) => Some(PathBuf::from(path)),
        _ => None,
    }
}

/// Streams the file through the engine: accounts to stdout, diagnostics to stderr.
fn process_file(path: &Path) -> anyhow::Result<()> {
    // `{:?}` escapes control characters in the path.
    let input = File::open(path).with_context(|| format!("cannot open {path:?}"))?;
    // The CSV writer buffers stdout itself and reports a failed flush.
    let output = io::stdout().lock();
    // Diagnostics are buffered here and flushed by the engine, best effort.
    let diagnostics = BufWriter::new(io::stderr().lock());

    payments_engine::run(input, output, diagnostics)
        .with_context(|| format!("cannot process {path:?}"))?;
    Ok(())
}

/// Writes a message to stderr, ignoring failures; `eprintln!` would panic instead.
fn report(message: &str) {
    let _ = writeln!(io::stderr(), "{message}");
}
