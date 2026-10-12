//! CSV output: final account balances.

use std::io::{self, Write};

use crate::domain::{Account, ClientId};

const HEADER: [&str; 5] = ["client", "available", "held", "total", "locked"];

/// Writes the header and one row per account, in the order given, then flushes.
///
/// Flushing here matters: a buffered writer that is only dropped discards its write
/// error, which would turn a failed write into a successful-looking truncated report.
pub(crate) fn write_accounts<'a, W: Write>(
    output: W,
    accounts: impl IntoIterator<Item = (ClientId, &'a Account)>,
) -> io::Result<()> {
    // 1. Records end with a plain `\n`.
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(output);

    // 2. Write every record into the CSV writer's buffer.
    write_records(&mut writer, accounts).map_err(super::into_io_error)?;

    // 3. Flush, so a failed write is reported instead of lost on drop.
    writer.flush()
}

fn write_records<'a, W: Write>(
    writer: &mut csv::Writer<W>,
    accounts: impl IntoIterator<Item = (ClientId, &'a Account)>,
) -> csv::Result<()> {
    writer.write_record(HEADER)?;
    for (client, account) in accounts {
        writer.write_record(account_row(client, account))?;
    }
    Ok(())
}

/// One output row; amounts always show four decimal places.
fn account_row(client: ClientId, account: &Account) -> [String; 5] {
    [
        client.to_string(),
        account.available().to_string(),
        account.held().to_string(),
        account.total().to_string(),
        account.is_locked().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Ledger;
    use crate::domain::test_support::deposit;

    #[test]
    fn writes_header_only_when_there_are_no_accounts() {
        let mut output = Vec::new();

        write_accounts(&mut output, []).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "client,available,held,total,locked\n"
        );
    }

    #[test]
    fn writes_one_row_per_account_with_four_decimal_places() {
        let mut ledger = Ledger::new();
        ledger.apply(deposit(2, 1, "1.5")).unwrap();
        let mut output = Vec::new();

        write_accounts(&mut output, ledger.accounts()).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "client,available,held,total,locked\n2,1.5000,0.0000,1.5000,false\n"
        );
    }
}
