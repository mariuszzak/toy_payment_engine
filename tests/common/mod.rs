//! Helpers shared by the integration tests.

#![allow(dead_code)] // each test crate uses a different subset

use std::io::{self, Read, Write};

/// Runs the engine on `input` in memory and returns `(output, diagnostics)`.
pub(crate) fn run_bytes(input: &[u8]) -> (String, String) {
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    payments_engine::run(input, &mut output, &mut diagnostics).expect("run failed");
    (
        String::from_utf8(output).unwrap(),
        String::from_utf8(diagnostics).unwrap(),
    )
}

/// `(line, code)` for every row reported as not applied.
pub(crate) fn reason_codes(diagnostics: &str) -> Vec<(u64, String)> {
    diagnostics
        .lines()
        .filter_map(|line| line.strip_prefix("line "))
        .map(|rest| {
            let (number, rest) = rest.split_once(": ").unwrap();
            let code = rest.split_once('[').unwrap().1.split_once(']').unwrap().0;
            (number.parse().unwrap(), code.to_owned())
        })
        .collect()
}

/// Parses an `expected_codes.txt` file: one `line code` pair per line.
pub(crate) fn parse_expected_codes(text: &str) -> Vec<(u64, String)> {
    text.lines()
        .map(|line| {
            let (number, code) = line.split_once(' ').unwrap();
            (number.parse().unwrap(), code.to_owned())
        })
        .collect()
}

/// Returns `data`, then fails with an I/O error.
pub(crate) struct FailingReader {
    data: Vec<u8>,
    position: usize,
}

impl FailingReader {
    pub(crate) fn after(data: &[u8]) -> Self {
        Self {
            data: data.to_vec(),
            position: 0,
        }
    }
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = &self.data[self.position..];
        if remaining.is_empty() {
            return Err(io::Error::other("simulated read failure"));
        }
        let count = remaining.len().min(buf.len());
        buf[..count].copy_from_slice(&remaining[..count]);
        self.position += count;
        Ok(count)
    }
}

/// Returns at most `chunk` bytes per read, to split rows across buffer boundaries.
pub(crate) struct ChunkedReader<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl<'a> ChunkedReader<'a> {
    pub(crate) fn new(data: &'a [u8], chunk: usize) -> Self {
        assert!(chunk > 0);
        Self { data, chunk }
    }
}

impl Read for ChunkedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.data.len().min(self.chunk).min(buf.len());
        buf[..count].copy_from_slice(&self.data[..count]);
        self.data = &self.data[count..];
        Ok(count)
    }
}

/// When a [`FailingWriter`] fails.
#[derive(Clone, Copy)]
pub(crate) enum FailOn {
    Write,
    Flush,
}

/// A writer that fails on every write or on every flush.
pub(crate) struct FailingWriter {
    fail_on: FailOn,
    pub written: Vec<u8>,
}

impl FailingWriter {
    pub(crate) fn new(fail_on: FailOn) -> Self {
        Self {
            fail_on,
            written: Vec::new(),
        }
    }
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.fail_on {
            FailOn::Write => Err(io::Error::other("simulated write failure")),
            FailOn::Flush => self.written.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.fail_on {
            FailOn::Write => Ok(()),
            FailOn::Flush => Err(io::Error::other("simulated flush failure")),
        }
    }
}
