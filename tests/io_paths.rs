//! I/O failure and streaming behaviour of `run`, using test doubles for the streams.

// Test code: panicking on failure and plain arithmetic are fine here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::default_numeric_fallback
)]

mod common;

use std::io::{self, Read};

use common::{ChunkedReader, FailOn, FailingReader, FailingWriter};
use payments_engine::{RunError, run};
use rstest::rstest;

const INPUT: &[u8] = b"type,client,tx,amount\n\
deposit,1,1,10\n\
deposit,2,2,5\n\
withdrawal,1,3,20\n\
dispute,1,1\n\
withdrawal,2,4,1.5\n";

#[rstest]
#[case::before_header(&[][..])]
#[case::mid_stream(&INPUT[..40])]
#[case::inside_last_row(&INPUT[..INPUT.len() - 3])]
fn read_error_is_fatal_and_writes_no_output(#[case] readable: &[u8]) {
    let mut output = Vec::new();

    let result = run(FailingReader::after(readable), &mut output, io::sink());

    assert!(matches!(result, Err(RunError::Input(_))), "{result:?}");
    assert!(output.is_empty());
}

#[test]
fn output_write_error_is_fatal() {
    let mut output = FailingWriter::new(FailOn::Write);

    let result = run(INPUT, &mut output, io::sink());

    assert!(matches!(result, Err(RunError::Output(_))), "{result:?}");
}

#[test]
fn output_flush_error_is_fatal() {
    let mut output = FailingWriter::new(FailOn::Flush);

    let result = run(INPUT, &mut output, io::sink());

    assert!(matches!(result, Err(RunError::Output(_))), "{result:?}");
}

#[test]
fn diagnostics_write_error_is_ignored() {
    let (expected_output, _) = common::run_bytes(INPUT);
    let mut output = Vec::new();

    let result = run(INPUT, &mut output, FailingWriter::new(FailOn::Write));

    assert_eq!(result.unwrap().not_applied_total(), 1);
    assert_eq!(String::from_utf8(output).unwrap(), expected_output);
}

#[rstest]
fn chunked_reads_give_the_same_result(#[values(1, 2, 7, 64)] chunk: usize) {
    let expected = common::run_bytes(INPUT);
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();

    run(
        ChunkedReader::new(INPUT, chunk),
        &mut output,
        &mut diagnostics,
    )
    .unwrap();

    assert_eq!(
        (
            String::from_utf8(output).unwrap(),
            String::from_utf8(diagnostics).unwrap()
        ),
        expected
    );
}

/// Produces `rows` deposit and withdrawal rows on the fly, never holding the input.
struct GeneratedInput {
    rows: u64,
    next: u64,
    pending: Vec<u8>,
}

impl GeneratedInput {
    fn new(rows: u64) -> Self {
        Self {
            rows,
            next: 0,
            pending: b"type,client,tx,amount\n".to_vec(),
        }
    }
}

impl Read for GeneratedInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pending.len() < buf.len() && self.next < self.rows {
            // Each client gets three deposits, then a withdrawal they can afford.
            let client = (self.next / 4) % 1000;
            let kind = if self.next % 4 == 3 {
                "withdrawal"
            } else {
                "deposit"
            };
            self.pending
                .extend_from_slice(format!("{kind},{client},{},1.0001\n", self.next).as_bytes());
            self.next += 1;
        }
        let count = self.pending.len().min(buf.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

/// Run with `just test-large`. Memory is checked by hand, for example with
/// `/usr/bin/time -l`, not asserted.
#[test]
#[ignore = "slow; run with `just test-large`"]
fn large_input_streams() {
    let rows = 10_000_000;

    let summary = run(GeneratedInput::new(rows), io::sink(), io::sink()).unwrap();

    assert_eq!(summary.rows_read, rows);
    assert_eq!(summary.applied, rows);
}
