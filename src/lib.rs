//! Streaming payments engine: reads transactions from CSV, applies them to
//! client accounts and reports final balances.
//!
//! Layers, from the inside out:
//! - [`domain`]: pure money, identifiers and ledger rules; no I/O.
//! - [`io`]: CSV parsing and validation, and CSV output, at the edges.
//! - [`app`]: the run loop and diagnostics.

// Test code may use unsuffixed literals and plain arithmetic; production code may not.
#![cfg_attr(
    test,
    allow(clippy::default_numeric_fallback, clippy::arithmetic_side_effects)
)]

pub mod app;
pub mod domain;
pub mod io;

pub use app::{RunError, Summary, run};
