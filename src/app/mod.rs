//! The run loop: stream rows from input through the ledger, report rows that were not
//! applied, then write the accounts.

mod diagnostics;

use std::io::{Read, Write};

use crate::domain::Ledger;
use crate::io::input::{InputError, TransactionReader};
use crate::io::output::write_accounts;

pub use diagnostics::Summary;
use diagnostics::{Diagnostics, NotApplied};

/// Problems that stop the run. No output has been written when one occurs, unless
/// writing the output itself failed part of the way through.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error("failed to write output")]
    Output(#[source] std::io::Error),
}

/// Processes `input` and writes the account CSV to `output`.
///
/// Rows are streamed one at a time. Output is written only after the whole input has
/// been read, so a read failure never leaves a partial report. Rows that are not
/// applied, and the final summary, go to `diagnostics`; failures writing there are
/// ignored.
pub fn run(
    input: impl Read,
    output: impl Write,
    diagnostics: impl Write,
) -> Result<Summary, RunError> {
    let mut diagnostics = Diagnostics::new(diagnostics);
    let mut ledger = Ledger::new();

    for row in TransactionReader::new(input)? {
        let row = row?;
        let outcome = match row.parsed {
            Err(error) => Err(NotApplied::Skipped(error)),
            Ok(transaction) => ledger
                .apply(transaction)
                .map_err(|reason| NotApplied::Rejected {
                    transaction,
                    reason,
                }),
        };
        diagnostics.record(row.line, outcome);
    }

    write_accounts(output, ledger.accounts()).map_err(RunError::Output)?;
    Ok(diagnostics.finish())
}
