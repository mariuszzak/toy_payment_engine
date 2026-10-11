//! The edges: CSV in, CSV out.

use std::io;

pub(crate) mod input;
pub(crate) mod output;

/// Keeps the csv crate out of public error types. With quoting off, flexible rows and
/// byte records, its only possible errors here are I/O failures; anything else is
/// wrapped as one.
fn into_io_error(error: csv::Error) -> io::Error {
    match error.into_kind() {
        csv::ErrorKind::Io(error) => error,
        other => io::Error::other(format!("unexpected CSV error: {other:?}")),
    }
}
