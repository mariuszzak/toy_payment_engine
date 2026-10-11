//! CSV input: untrusted bytes in, validated transactions out.
//!
//! Only stateless checks happen here. Anything that depends on earlier rows, such as
//! duplicate IDs or available funds, is the ledger's job.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read};

use csv::{ByteRecord, ReaderBuilder, Trim};
use serde::Deserialize;

use crate::domain::{Amount, ClientId, MoneyError, Transaction, TxId};

const KNOWN_COLUMNS: [&str; 4] = ["type", "client", "tx", "amount"];
const REQUIRED_COLUMNS: [&str; 3] = ["type", "client", "tx"];

/// One row as read: untrusted text, already trimmed. Missing and empty fields are
/// both `None`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct RawRecord<'a> {
    #[serde(rename = "type", default, borrow)]
    pub kind: Option<&'a str>,
    #[serde(default, borrow)]
    pub client: Option<&'a str>,
    #[serde(default, borrow)]
    pub tx: Option<&'a str>,
    #[serde(default, borrow)]
    pub amount: Option<&'a str>,
}

/// Untrusted text quoted in a diagnostic: truncated, then escaped by `Debug`
/// formatting, so input can never inject control characters into a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt(String);

impl Excerpt {
    const MAX_CHARS: usize = 32;

    pub fn new(text: &str) -> Self {
        let mut chars = text.chars();
        let mut kept: String = chars.by_ref().take(Self::MAX_CHARS).collect();
        if chars.next().is_some() {
            kept.push('…');
        }
        Self(kept)
    }
}

impl fmt::Display for Excerpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

/// Why a row could not become a [`Transaction`]. The row is skipped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RowError {
    #[error("record could not be decoded (for example, it is not valid UTF-8)")]
    Malformed,
    #[error("row has {found} fields but the header has {expected}")]
    UnexpectedFieldCount { found: usize, expected: usize },
    #[error("transaction type {0} is not recognised")]
    InvalidType(Excerpt),
    #[error("client {0} is not a valid client ID")]
    InvalidClient(Excerpt),
    #[error("tx {0} is not a valid transaction ID")]
    InvalidTx(Excerpt),
    #[error("amount is missing")]
    MissingAmount,
    #[error("amount {value} is invalid: {reason}")]
    InvalidAmount { value: Excerpt, reason: MoneyError },
}

impl RowError {
    /// Stable reason code for diagnostics and tests.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Malformed => "malformed_record",
            Self::UnexpectedFieldCount { .. } => "unexpected_field_count",
            Self::InvalidType(_) => "invalid_type",
            Self::InvalidClient(_) => "invalid_client",
            Self::InvalidTx(_) => "invalid_tx",
            Self::MissingAmount => "missing_amount",
            Self::InvalidAmount { reason, .. } => match reason {
                MoneyError::InvalidFormat => "invalid_amount",
                MoneyError::TooPrecise => "amount_too_precise",
                MoneyError::OutOfRange => "amount_out_of_range",
                MoneyError::NotPositive => "amount_not_positive",
            },
        }
    }
}

/// Input problems that stop the run.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("failed to read input")]
    Read(#[source] csv::Error),
    #[error("header is missing required column {0:?}")]
    MissingColumn(&'static str),
    #[error("header has unknown column {0}")]
    UnknownColumn(Excerpt),
    #[error("header repeats column {0}")]
    DuplicateColumn(Excerpt),
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

impl Kind {
    /// Exact, lowercase match only.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "deposit" => Some(Self::Deposit),
            "withdrawal" => Some(Self::Withdrawal),
            "dispute" => Some(Self::Dispute),
            "resolve" => Some(Self::Resolve),
            "chargeback" => Some(Self::Chargeback),
            _ => None,
        }
    }
}

/// Full stateless validation. Checks run in column order: type, client, tx, amount.
/// An amount on a dispute, resolve or chargeback row is ignored.
impl TryFrom<RawRecord<'_>> for Transaction {
    type Error = RowError;

    fn try_from(raw: RawRecord<'_>) -> Result<Self, RowError> {
        let kind_text = raw.kind.unwrap_or_default();
        let kind =
            Kind::parse(kind_text).ok_or_else(|| RowError::InvalidType(Excerpt::new(kind_text)))?;
        let client = parse_id(raw.client)
            .map(ClientId::new)
            .ok_or_else(|| RowError::InvalidClient(Excerpt::new(raw.client.unwrap_or_default())))?;
        let tx = parse_id(raw.tx)
            .map(TxId::new)
            .ok_or_else(|| RowError::InvalidTx(Excerpt::new(raw.tx.unwrap_or_default())))?;

        Ok(match kind {
            Kind::Deposit => Self::Deposit {
                client,
                tx,
                amount: parse_amount(raw.amount)?,
            },
            Kind::Withdrawal => Self::Withdrawal {
                client,
                tx,
                amount: parse_amount(raw.amount)?,
            },
            Kind::Dispute => Self::Dispute { client, tx },
            Kind::Resolve => Self::Resolve { client, tx },
            Kind::Chargeback => Self::Chargeback { client, tx },
        })
    }
}

/// Plain ASCII digits within the target type's range. Rejects the leading `+` that
/// `str::parse` would accept.
fn parse_id<T: std::str::FromStr>(text: Option<&str>) -> Option<T> {
    text.filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
}

fn parse_amount(text: Option<&str>) -> Result<Amount, RowError> {
    let text = text.ok_or(RowError::MissingAmount)?;
    text.parse().map_err(|reason| RowError::InvalidAmount {
        value: Excerpt::new(text),
        reason,
    })
}

/// One row and its physical line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: u64,
    pub parsed: Result<Transaction, RowError>,
}

/// Passes input through unchanged and remembers where lines end, so a record's byte
/// offset can be turned into a physical line number.
///
/// The csv crate's own line counter ignores blank lines and is off by one on CRLF
/// input, and a record's byte position points at the start of any blank lines the
/// reader skipped before it. Both would send operators to the wrong line. Only lines
/// not yet passed are kept, so memory is bounded by the reader's read-ahead.
#[derive(Debug)]
struct LineTracker<R> {
    inner: R,
    bytes_read: u64,
    pending: VecDeque<LineEnd>,
    current_line_has_content: bool,
    lines_passed: u64,
}

#[derive(Debug, Clone, Copy)]
struct LineEnd {
    /// Offset of the line's `\n`.
    newline: u64,
    /// The line held nothing but an optional `\r`, so the CSV reader skips it.
    blank: bool,
}

impl<R> LineTracker<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
            pending: VecDeque::new(),
            current_line_has_content: false,
            lines_passed: 0,
        }
    }

    /// The 1-based line on which a record starts, given the byte offset where the CSV
    /// reader began reading it. Offsets must not decrease.
    fn record_line(&mut self, offset: u64) -> u64 {
        // With CRLF endings the reader reports the offset of the previous line's `\n`,
        // so a newline at exactly `offset` belongs to the previous line.
        while self
            .pending
            .front()
            .is_some_and(|end| end.newline <= offset)
        {
            self.pass_line();
        }
        while self.pending.front().is_some_and(|end| end.blank) {
            self.pass_line();
        }
        self.lines_passed.saturating_add(1)
    }

    fn pass_line(&mut self) {
        self.pending.pop_front();
        self.lines_passed = self.lines_passed.saturating_add(1);
    }
}

impl<R: Read> Read for LineTracker<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        let mut offset = self.bytes_read;
        for byte in buf.get(..count).unwrap_or_default() {
            match byte {
                b'\n' => {
                    self.pending.push_back(LineEnd {
                        newline: offset,
                        blank: !self.current_line_has_content,
                    });
                    self.current_line_has_content = false;
                }
                b'\r' => {}
                _ => self.current_line_has_content = true,
            }
            offset = offset.saturating_add(1);
        }
        self.bytes_read = offset;
        Ok(count)
    }
}

/// Streams rows from CSV input one at a time, reusing a single record buffer.
#[derive(Debug)]
pub struct TransactionReader<R> {
    reader: csv::Reader<LineTracker<R>>,
    headers: ByteRecord,
    record: ByteRecord,
}

impl<R: Read> TransactionReader<R> {
    /// Reads and checks the header. Empty input is an empty batch, not an error.
    pub fn new(input: R) -> Result<Self, InputError> {
        let mut reader = ReaderBuilder::new()
            .trim(Trim::All)
            .flexible(true)
            .from_reader(LineTracker::new(input));
        let headers = reader.byte_headers().map_err(InputError::Read)?.clone();
        if !headers.is_empty() {
            validate_header(&headers)?;
        }
        Ok(Self {
            reader,
            headers,
            record: ByteRecord::new(),
        })
    }

    /// The next row, `None` at the end of input, or an error that stops the run.
    /// Lines holding only whitespace are skipped like blank lines.
    pub fn next_row(&mut self) -> Result<Option<Row>, InputError> {
        loop {
            match self.reader.read_byte_record(&mut self.record) {
                Ok(true) if self.is_whitespace_only() => continue,
                result => return self.row_from(result),
            }
        }
    }

    fn is_whitespace_only(&self) -> bool {
        self.record.len() == 1 && self.record.get(0).is_some_and(<[u8]>::is_empty)
    }

    fn row_from(&mut self, result: csv::Result<bool>) -> Result<Option<Row>, InputError> {
        match result {
            Ok(false) => Ok(None),
            Ok(true) => Ok(Some(Row {
                line: self.line_of(self.record.position().cloned()),
                parsed: self.parse_current(),
            })),
            Err(error) if error.is_io_error() => Err(InputError::Read(error)),
            Err(error) => Ok(Some(Row {
                line: self.line_of(error.position().cloned()),
                parsed: Err(RowError::Malformed),
            })),
        }
    }

    fn line_of(&mut self, position: Option<csv::Position>) -> u64 {
        let offset = position.map_or(0, |position| position.byte());
        self.reader.get_mut().record_line(offset)
    }

    fn parse_current(&self) -> Result<Transaction, RowError> {
        if self.record.len() > self.headers.len() {
            return Err(RowError::UnexpectedFieldCount {
                found: self.record.len(),
                expected: self.headers.len(),
            });
        }
        let raw: RawRecord<'_> = self
            .record
            .deserialize(Some(&self.headers))
            .map_err(|_| RowError::Malformed)?;
        Transaction::try_from(raw)
    }
}

impl<R: Read> Iterator for TransactionReader<R> {
    type Item = Result<Row, InputError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_row().transpose()
    }
}

/// Exactly the known columns, each at most once, in any order; `amount` is optional.
fn validate_header(headers: &ByteRecord) -> Result<(), InputError> {
    let mut seen: Vec<&'static str> = Vec::with_capacity(KNOWN_COLUMNS.len());
    for field in headers {
        let name = String::from_utf8_lossy(field);
        let known = KNOWN_COLUMNS
            .iter()
            .find(|column| **column == name)
            .ok_or_else(|| InputError::UnknownColumn(Excerpt::new(&name)))?;
        if seen.contains(known) {
            return Err(InputError::DuplicateColumn(Excerpt::new(known)));
        }
        seen.push(known);
    }
    match REQUIRED_COLUMNS
        .iter()
        .find(|column| !seen.contains(column))
    {
        Some(missing) => Err(InputError::MissingColumn(missing)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn raw<'a>(
        kind: &'a str,
        client: &'a str,
        tx: &'a str,
        amount: Option<&'a str>,
    ) -> RawRecord<'a> {
        RawRecord {
            kind: Some(kind),
            client: Some(client),
            tx: Some(tx),
            amount,
        }
    }

    fn parse(input: &str) -> Vec<(u64, Result<Transaction, String>)> {
        TransactionReader::new(input.as_bytes())
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                (row.line, row.parsed.map_err(|e| e.code().to_owned()))
            })
            .collect()
    }

    fn deposit(client: u16, tx: u32, amount: &str) -> Transaction {
        Transaction::Deposit {
            client: ClientId::new(client),
            tx: TxId::new(tx),
            amount: amount.parse().unwrap(),
        }
    }

    // TryFrom<RawRecord>

    #[rstest]
    #[case::deposit(raw("deposit", "1", "2", Some("3.5")), deposit(1, 2, "3.5"))]
    #[case::withdrawal(
        raw("withdrawal", "1", "2", Some("3")),
        Transaction::Withdrawal { client: ClientId::new(1), tx: TxId::new(2), amount: "3".parse().unwrap() }
    )]
    #[case::dispute(raw("dispute", "1", "2", None), Transaction::Dispute { client: ClientId::new(1), tx: TxId::new(2) })]
    #[case::resolve(raw("resolve", "1", "2", None), Transaction::Resolve { client: ClientId::new(1), tx: TxId::new(2) })]
    #[case::chargeback(raw("chargeback", "1", "2", None), Transaction::Chargeback { client: ClientId::new(1), tx: TxId::new(2) })]
    #[case::dispute_amount_ignored(raw("dispute", "1", "2", Some("9.99")), Transaction::Dispute { client: ClientId::new(1), tx: TxId::new(2) })]
    #[case::dispute_invalid_amount_ignored(raw("dispute", "1", "2", Some("garbage")), Transaction::Dispute { client: ClientId::new(1), tx: TxId::new(2) })]
    #[case::smallest_ids(raw("deposit", "0", "0", Some("1")), deposit(0, 0, "1"))]
    #[case::largest_ids(
        raw("deposit", "65535", "4294967295", Some("1")),
        deposit(65535, 4_294_967_295, "1")
    )]
    #[case::leading_zero_ids(raw("deposit", "007", "0010", Some("1")), deposit(7, 10, "1"))]
    fn valid_raw_record_becomes_transaction(
        #[case] record: RawRecord<'_>,
        #[case] expected: Transaction,
    ) {
        let result = Transaction::try_from(record);

        assert_eq!(result, Ok(expected));
    }

    #[rstest]
    #[case::capitalised_type(raw("Deposit", "1", "1", Some("1")), "invalid_type")]
    #[case::upper_case_type(raw("DEPOSIT", "1", "1", Some("1")), "invalid_type")]
    #[case::unknown_type(raw("refund", "1", "1", Some("1")), "invalid_type")]
    #[case::missing_type(RawRecord { kind: None, ..raw("", "1", "1", Some("1")) }, "invalid_type")]
    #[case::client_plus_sign(raw("deposit", "+1", "1", Some("1")), "invalid_client")]
    #[case::client_negative(raw("deposit", "-1", "1", Some("1")), "invalid_client")]
    #[case::client_too_large(raw("deposit", "65536", "1", Some("1")), "invalid_client")]
    #[case::client_letters(raw("deposit", "a", "1", Some("1")), "invalid_client")]
    #[case::client_missing(RawRecord { client: None, ..raw("deposit", "", "1", Some("1")) }, "invalid_client")]
    #[case::tx_too_large(raw("deposit", "1", "4294967296", Some("1")), "invalid_tx")]
    #[case::tx_decimal(raw("deposit", "1", "1.0", Some("1")), "invalid_tx")]
    #[case::tx_missing(RawRecord { tx: None, ..raw("deposit", "1", "", Some("1")) }, "invalid_tx")]
    #[case::deposit_without_amount(raw("deposit", "1", "1", None), "missing_amount")]
    #[case::withdrawal_without_amount(raw("withdrawal", "1", "1", None), "missing_amount")]
    #[case::amount_with_separator(raw("deposit", "1", "1", Some("1,000.00")), "invalid_amount")]
    #[case::negative_amount(raw("deposit", "1", "1", Some("-1")), "invalid_amount")]
    #[case::too_precise(raw("deposit", "1", "1", Some("1.23456")), "amount_too_precise")]
    #[case::zero(raw("withdrawal", "1", "1", Some("0.0000")), "amount_not_positive")]
    #[case::too_large(
        raw("deposit", "1", "1", Some("922337203685477.5808")),
        "amount_out_of_range"
    )]
    fn invalid_raw_record_is_rejected_with_reason_code(
        #[case] record: RawRecord<'_>,
        #[case] code: &str,
    ) {
        let result = Transaction::try_from(record);

        assert_eq!(result.map_err(|e| e.code()), Err(code));
    }

    #[test]
    fn type_is_checked_before_other_fields() {
        let result = Transaction::try_from(raw("bogus", "x", "y", Some("z")));

        assert_eq!(result.map_err(|e| e.code()), Err("invalid_type"));
    }

    // Excerpt

    #[test]
    fn excerpt_escapes_control_characters() {
        let excerpt = Excerpt::new("a\u{1b}[31m\n");

        assert_eq!(excerpt.to_string(), r#""a\u{1b}[31m\n""#);
    }

    #[test]
    fn excerpt_truncates_long_text() {
        let excerpt = Excerpt::new(&"9".repeat(100));

        assert_eq!(excerpt.to_string(), format!("\"{}…\"", "9".repeat(32)));
    }

    // TransactionReader

    #[test]
    fn reads_rows_with_line_numbers() {
        let rows = parse("type,client,tx,amount\ndeposit,1,1,1.0\n\nwithdrawal,1,2,0.5\n");

        assert_eq!(
            rows,
            vec![
                (2, Ok(deposit(1, 1, "1.0"))),
                (
                    4,
                    Ok(Transaction::Withdrawal {
                        client: ClientId::new(1),
                        tx: TxId::new(2),
                        amount: "0.5".parse().unwrap()
                    })
                ),
            ]
        );
    }

    #[rstest]
    #[case::blank_lines_lf("type,client,tx,amount\ndeposit,1,1,1\n\n\ndeposit,1,2,1\n", vec![2, 5])]
    #[case::blank_lines_crlf("type,client,tx,amount\r\ndeposit,1,1,1\r\n\r\ndeposit,1,2,1\r\n", vec![2, 4])]
    #[case::quoted_newline("type,client,tx,amount\ndeposit,1,1,\"1\n\"\ndeposit,1,2,1\n", vec![2, 4])]
    #[case::leading_blank_lines("\n\ntype,client,tx,amount\ndeposit,1,1,1\n", vec![4])]
    fn line_numbers_are_physical_lines(#[case] input: &str, #[case] expected: Vec<u64>) {
        let lines: Vec<u64> = parse(input).into_iter().map(|(line, _)| line).collect();

        assert_eq!(lines, expected);
    }

    #[test]
    fn whitespace_only_lines_are_skipped_like_blank_lines() {
        let rows = parse("type,client,tx,amount\n   \n\t\ndeposit,1,1,1\n");

        assert_eq!(rows, vec![(4, Ok(deposit(1, 1, "1")))]);
    }

    #[test]
    fn trims_whitespace_around_headers_and_fields() {
        let rows = parse(" type , client ,tx, amount\n deposit ,\t1 , 1 ,  2.5 \n");

        assert_eq!(rows, vec![(2, Ok(deposit(1, 1, "2.5")))]);
    }

    #[test]
    fn columns_are_matched_by_name() {
        let rows = parse("amount,tx,type,client\n2.5,1,deposit,3\n");

        assert_eq!(rows, vec![(2, Ok(deposit(3, 1, "2.5")))]);
    }

    #[rstest]
    #[case::no_amount_field("dispute,1,1")]
    #[case::empty_amount_field("dispute,1,1,")]
    #[case::blank_amount_field("dispute,1,1,   ")]
    fn dispute_rows_may_omit_the_amount(#[case] row: &str) {
        let rows = parse(&format!("type,client,tx,amount\n{row}\n"));

        assert_eq!(
            rows,
            vec![(
                2,
                Ok(Transaction::Dispute {
                    client: ClientId::new(1),
                    tx: TxId::new(1)
                })
            )]
        );
    }

    #[rstest]
    #[case::thousands_separator("deposit,1,7,1,000.00")]
    #[case::decimal_comma("deposit,1,7,1,5")]
    #[case::trailing_comma("deposit,1,7,1.0,")]
    fn row_with_more_fields_than_header_is_skipped(#[case] row: &str) {
        let rows = parse(&format!("type,client,tx,amount\n{row}\n"));

        assert_eq!(rows, vec![(2, Err("unexpected_field_count".to_owned()))]);
    }

    #[test]
    fn invalid_utf8_row_is_skipped_and_reading_continues() {
        let mut input = b"type,client,tx,amount\ndeposit,1,1,\xff\n".to_vec();
        input.extend_from_slice(b"deposit,1,2,1\n");

        let rows: Vec<_> = TransactionReader::new(input.as_slice())
            .unwrap()
            .map(|row| row.unwrap())
            .map(|row| (row.line, row.parsed.map_err(|e| e.code())))
            .collect();

        assert_eq!(
            rows,
            vec![(2, Err("malformed_record")), (3, Ok(deposit(1, 2, "1")))]
        );
    }

    #[test]
    fn header_without_amount_column_is_accepted() {
        let rows = parse("type,client,tx\ndispute,1,1\ndeposit,1,2\n");

        assert_eq!(
            rows,
            vec![
                (
                    2,
                    Ok(Transaction::Dispute {
                        client: ClientId::new(1),
                        tx: TxId::new(1)
                    })
                ),
                (3, Err("missing_amount".to_owned())),
            ]
        );
    }

    #[rstest]
    #[case::empty("")]
    #[case::blank_lines("\n\n")]
    #[case::header_only("type,client,tx,amount\n")]
    fn input_without_rows_yields_nothing(#[case] input: &str) {
        let rows = parse(input);

        assert!(rows.is_empty());
    }

    #[test]
    fn utf8_byte_order_mark_is_accepted() {
        let rows = parse("\u{feff}type,client,tx,amount\ndeposit,1,1,1\n");

        assert_eq!(rows, vec![(2, Ok(deposit(1, 1, "1")))]);
    }

    #[rstest]
    #[case::missing_type("client,tx,amount\n", "missing required column \"type\"")]
    #[case::missing_tx("type,client,amount\n", "missing required column \"tx\"")]
    #[case::unknown_column("type,client,tx,amount,note\n", "unknown column \"note\"")]
    #[case::capitalised_column("Type,client,tx,amount\n", "unknown column \"Type\"")]
    #[case::repeated_column("type,client,tx,amount,amount\n", "repeats column \"amount\"")]
    #[case::no_header_row("deposit,1,1,1.0\n", "unknown column \"deposit\"")]
    fn invalid_header_stops_the_run(#[case] input: &str, #[case] message: &str) {
        let error = TransactionReader::new(input.as_bytes()).unwrap_err();

        assert!(error.to_string().contains(message), "{error}");
    }
}
