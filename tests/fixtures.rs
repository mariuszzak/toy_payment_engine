//! End-to-end scenarios: every directory under `tests/fixtures/` holds an
//! `input.csv`, the exact `expected.csv`, and `expected_codes.txt` listing each row
//! that must not be applied as `line code`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should stop with a panic and a clear message"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::default_numeric_fallback,
    reason = "test data uses small literal values, where overflow and inferred types cannot matter"
)]

mod common;

use std::fs;
use std::path::PathBuf;

use rstest::rstest;

#[rstest]
fn fixture_produces_expected_accounts_and_reason_codes(
    #[files("tests/fixtures/*/input.csv")] input: PathBuf,
) {
    let dir = input.parent().unwrap();
    let input = fs::read(&input).unwrap();
    let expected_output = fs::read_to_string(dir.join("expected.csv")).unwrap();
    let expected_codes =
        common::parse_expected_codes(&fs::read_to_string(dir.join("expected_codes.txt")).unwrap());

    let (output, diagnostics) = common::run_bytes(&input);

    assert_eq!(output, expected_output, "accounts for {}", dir.display());
    assert_eq!(
        common::reason_codes(&diagnostics),
        expected_codes,
        "reason codes for {}\n{diagnostics}",
        dir.display()
    );
}
