//! Streaming payments engine: reads transactions from CSV, applies them to
//! client accounts and reports final balances.
//!
//! Layers, from the inside out:
//! - [`domain`]: pure money, identifiers and ledger rules; no I/O.
//! - `io` (internal): CSV parsing and validation, and CSV output, at the edges.
//! - `app` (internal): the run loop and diagnostics; [`run`] is the entry point.

// Test code may use unsuffixed literals and plain arithmetic; production code may not.
#![cfg_attr(
    test,
    allow(clippy::default_numeric_fallback, clippy::arithmetic_side_effects)
)]

mod app;
pub mod domain;
mod io;

pub use app::{RunError, Summary, run};
pub use io::input::InputError;
