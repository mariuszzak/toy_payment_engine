//! Reporting rows that were not applied.
//!
//! Each such row gets one line on the diagnostics stream with its line number and a
//! stable reason code. After the run, a summary gives totals and a count per code.
//! Writing diagnostics is best effort: a failure here never affects the output.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;

use crate::domain::{Rejection, Transaction};
use crate::io::input::RowError;

/// Why a row had no effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotApplied {
    /// The row failed validation.
    Skipped(RowError),
    /// The row was valid but a ledger rule refused it.
    Rejected {
        transaction: Transaction,
        reason: Rejection,
    },
}

impl NotApplied {
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Skipped(error) => error.code(),
            Self::Rejected { reason, .. } => reason.code(),
        }
    }
}

impl fmt::Display for NotApplied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skipped(error) => write!(f, "skipped [{}] {error}", self.code()),
            Self::Rejected {
                transaction,
                reason,
            } => write!(f, "rejected [{}] {transaction}: {reason}", self.code()),
        }
    }
}

/// Totals for one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Rows read, excluding the header and blank or whitespace-only lines.
    pub rows_read: u64,
    /// Rows that changed the ledger.
    pub applied: u64,
    /// Rows not applied, per reason code.
    pub not_applied: BTreeMap<&'static str, u64>,
}

impl Summary {
    /// Rows skipped or rejected, across all reason codes.
    pub fn not_applied_total(&self) -> u64 {
        self.not_applied
            .values()
            .fold(0, |total, count| total.saturating_add(*count))
    }

    /// Reason codes with their counts, most frequent first, ties in code order.
    fn codes_by_frequency(&self) -> Vec<(&'static str, u64)> {
        let mut counts: Vec<_> = self
            .not_applied
            .iter()
            .map(|(code, count)| (*code, *count))
            .collect();
        counts.sort_by_key(|&(code, count)| (Reverse(count), code));
        counts
    }
}

/// Writes one line per row not applied and keeps the counts for the summary.
#[derive(Debug)]
pub(crate) struct Diagnostics<W> {
    out: W,
    summary: Summary,
}

impl<W: Write> Diagnostics<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            summary: Summary::default(),
        }
    }

    /// Records the outcome of one row.
    pub(crate) fn record(&mut self, line: u64, outcome: Result<(), NotApplied>) {
        add_one(&mut self.summary.rows_read);
        match outcome {
            Ok(()) => add_one(&mut self.summary.applied),
            Err(not_applied) => {
                add_one(
                    self.summary
                        .not_applied
                        .entry(not_applied.code())
                        .or_default(),
                );
                // Best effort: diagnostics must never stop the run.
                let _ = writeln!(self.out, "line {line}: {not_applied}");
            }
        }
    }

    /// Writes the summary, most frequent reason first, and returns the totals.
    pub(crate) fn finish(mut self) -> Summary {
        let summary = self.summary;
        // Best effort, like every diagnostic.
        let _ = writeln!(
            self.out,
            "summary: {} rows read, {} applied, {} not applied",
            summary.rows_read,
            summary.applied,
            summary.not_applied_total()
        );
        for (code, count) in summary.codes_by_frequency() {
            let _ = writeln!(self.out, "summary: {code} {count}");
        }
        let _ = self.out.flush();
        summary
    }
}

/// Counters saturate rather than wrap; no realistic input gets near `u64::MAX`.
fn add_one(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::test_support::withdrawal;

    fn rejected(reason: Rejection) -> NotApplied {
        NotApplied::Rejected {
            transaction: withdrawal(2, 5, "3"),
            reason,
        }
    }

    #[test]
    fn reports_each_row_not_applied_and_a_summary_by_frequency() {
        let mut out = Vec::new();
        let mut diagnostics = Diagnostics::new(&mut out);

        diagnostics.record(2, Ok(()));
        diagnostics.record(3, Err(rejected(Rejection::InsufficientFunds)));
        diagnostics.record(4, Err(NotApplied::Skipped(RowError::MissingAmount)));
        diagnostics.record(5, Err(rejected(Rejection::InsufficientFunds)));
        let summary = diagnostics.finish();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "line 3: rejected [insufficient_funds] withdrawal tx 5 for client 2: insufficient available funds\n\
             line 4: skipped [missing_amount] amount is missing\n\
             line 5: rejected [insufficient_funds] withdrawal tx 5 for client 2: insufficient available funds\n\
             summary: 4 rows read, 1 applied, 3 not applied\n\
             summary: insufficient_funds 2\n\
             summary: missing_amount 1\n"
        );
        assert_eq!(summary.rows_read, 4);
        assert_eq!(summary.applied, 1);
        assert_eq!(summary.not_applied_total(), 3);
    }

    #[test]
    fn summary_of_clean_run_has_only_totals() {
        let mut out = Vec::new();
        let mut diagnostics = Diagnostics::new(&mut out);

        diagnostics.record(2, Ok(()));
        diagnostics.finish();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "summary: 1 rows read, 1 applied, 0 not applied\n"
        );
    }
}
