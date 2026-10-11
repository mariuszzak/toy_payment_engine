//! Command-line entry point: `payments-engine <transactions.csv>`.
//!
//! Exit codes: 0 on success (even if some rows were not applied), 1 when the input
//! cannot be read or the output cannot be written, 2 on wrong usage.

use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;

const USAGE: &str = "usage: payments-engine <transactions.csv>";

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let [path] = args.as_slice() else {
        report(USAGE);
        return ExitCode::from(2);
    };
    match run_file(Path::new(path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&format!("error: {error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run_file(path: &Path) -> anyhow::Result<()> {
    // `{:?}` escapes control characters in the path.
    let input = File::open(path).with_context(|| format!("cannot open {path:?}"))?;

    // The CSV writer buffers and flushes stdout itself, and reports a failed flush.
    // stderr is buffered here; the diagnostics flush it, best effort.
    payments_engine::run(
        input,
        io::stdout().lock(),
        BufWriter::new(io::stderr().lock()),
    )
    .with_context(|| format!("cannot process {path:?}"))?;
    Ok(())
}

/// Writes a message to stderr, ignoring failures; `eprintln!` would panic instead.
fn report(message: &str) {
    let _ = writeln!(io::stderr(), "{message}");
}
