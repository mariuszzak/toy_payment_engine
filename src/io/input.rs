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
    Read(#[source] io::Error),
    #[error("header is missing required column {0:?}")]
    MissingColumn(&'static str),
    /// The column name, already truncated and escaped.
    #[error("header has unknown column {0}")]
    UnknownColumn(String),
    #[error("header repeats column {0:?}")]
    DuplicateColumn(&'static str),
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

/// Validation of the fields. Checks run in column order: type, client, tx, amount.
/// An amount on a dispute, resolve or chargeback row is ignored. The reader checks the
/// row's shape (line length, field count, encoding) before this runs.
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
pub(crate) struct Row {
    pub line: u64,
    pub parsed: Result<Transaction, RowError>,
}

/// Longest line accepted, in bytes, not counting the line ending. A valid row needs
/// well under 100 bytes; the limit caps the memory one line can take.
pub(crate) const MAX_LINE_BYTES: u64 = 1024;

/// UTF-8 byte order mark. The csv crate drops it from the start of the input, so it is
/// not counted toward the first line's length either.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

const BARE_CARRIAGE_RETURN: &str =
    "carriage return without line feed; only LF or CRLF line endings are accepted";

/// Sits between the input and the CSV reader and checks every byte as it streams
/// through:
///
/// - **Line numbers.** The csv crate's own line counter ignores blank lines and is off
///   by one on CRLF input, and a record's byte position points at the start of any
///   blank lines skipped before it. The tracker records where each line ends so a
///   record's position can be turned into its physical line number.
/// - **Line length.** A line longer than [`MAX_LINE_BYTES`] is an error, raised before
///   the CSV reader has buffered the whole line, so a huge line never reaches memory.
/// - **Line endings.** A carriage return not followed by a line feed is an error: the
///   CSV reader would treat it as a line end, which makes the file's structure
///   ambiguous.
///
/// Both errors stop the run. Memory is bounded: blank and whitespace-only lines are
/// only counted, and line ends are kept only until the reader has passed them.
///
/// "Whitespace" is `u8::is_ascii_whitespace`, the definition the csv crate uses for
/// `Trim::All`, so a line the tracker counts as blank is exactly a record the reader
/// skips as whitespace only.
#[derive(Debug)]
struct LineTracker<R> {
    inner: R,
    /// Bytes read so far; the CSV reader sees exactly these bytes.
    bytes_read: u64,
    /// Lines with content whose end has been read but which have not been passed yet.
    pending: VecDeque<LineEnd>,
    /// Blank or whitespace-only lines read since the last line with content.
    blank_lines: u64,
    current: CurrentLine,
    /// The previous byte was a `\r`, so the next one must be `\n`.
    after_carriage_return: bool,
    /// Leading bytes that match the start of [`UTF8_BOM`], not yet counted; `None`
    /// once the start of the input has been checked.
    possible_bom: Option<u64>,
    lines_read: u64,
    lines_passed: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct CurrentLine {
    length: u64,
    has_content: bool,
}

#[derive(Debug, Clone, Copy)]
struct LineEnd {
    /// Position of the line's `\n` in the input.
    newline: u64,
    /// Blank or whitespace-only lines directly before this one.
    blank_lines_before: u64,
}

impl<R> LineTracker<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
            pending: VecDeque::new(),
            blank_lines: 0,
            current: CurrentLine::default(),
            after_carriage_return: false,
            possible_bom: Some(0),
            lines_read: 0,
            lines_passed: 0,
        }
    }

    /// The line on which a record starts, given the byte offset where the CSV reader
    /// began reading it. Offsets must not decrease.
    fn record_line(&mut self, offset: u64) -> u64 {
        // With CRLF endings the reader reports the offset of the previous line's `\n`,
        // so a newline at exactly `offset` belongs to the previous line.
        while let Some(end) = self.pending.front().copied() {
            if end.newline > offset {
                break;
            }
            self.pending.pop_front();
            self.lines_passed = self
                .lines_passed
                .saturating_add(end.blank_lines_before)
                .saturating_add(1);
        }
        let blank_lines_before = match self.pending.front() {
            Some(end) => end.blank_lines_before,
            // The record's line has no line ending yet: it is the last line.
            None => self.blank_lines,
        };
        self.lines_passed
            .saturating_add(blank_lines_before)
            .saturating_add(1)
    }

    /// Checks and records one input byte at position `offset`.
    fn accept(&mut self, byte: u8, offset: u64) -> io::Result<()> {
        if let Some(matched) = self.possible_bom {
            let expected = usize::try_from(matched)
                .ok()
                .and_then(|index| UTF8_BOM.get(index));
            if expected == Some(&byte) {
                let matched = matched.saturating_add(1);
                self.possible_bom = (matched < 3).then_some(matched);
                return Ok(());
            }
            // Not a byte order mark after all: count the bytes held back.
            self.possible_bom = None;
            if matched > 0 {
                self.current.length = matched;
                self.current.has_content = true;
            }
        }
        if self.after_carriage_return && byte != b'\n' {
            return Err(self.invalid_line(BARE_CARRIAGE_RETURN));
        }
        self.after_carriage_return = byte == b'\r';
        match byte {
            b'\n' => self.end_line(offset),
            b'\r' => {}
            _ => {
                let line = &mut self.current;
                line.length = line.length.saturating_add(1);
                line.has_content |= !byte.is_ascii_whitespace();
                if line.length > MAX_LINE_BYTES {
                    return Err(self.invalid_line(&format!("longer than {MAX_LINE_BYTES} bytes")));
                }
            }
        }
        Ok(())
    }

    fn end_line(&mut self, newline: u64) {
        if self.current.has_content {
            self.pending.push_back(LineEnd {
                newline,
                blank_lines_before: self.blank_lines,
            });
            self.blank_lines = 0;
        } else {
            self.blank_lines = self.blank_lines.saturating_add(1);
        }
        self.current = CurrentLine::default();
        self.lines_read = self.lines_read.saturating_add(1);
    }

    /// An error about the line being read; it stops the run.
    fn invalid_line(&self, problem: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("line {}: {problem}", self.lines_read.saturating_add(1)),
        )
    }
}

impl<R: Read> Read for LineTracker<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        // An empty `buf` also yields 0 without meaning end of input.
        if count == 0 && !buf.is_empty() && self.after_carriage_return {
            return Err(self.invalid_line(BARE_CARRIAGE_RETURN));
        }
        let mut offset = self.bytes_read;
        for byte in buf.get(..count).unwrap_or_default() {
            self.accept(*byte, offset)?;
            offset = offset.saturating_add(1);
        }
        self.bytes_read = offset;
        Ok(count)
    }
}

/// Streams rows from CSV input one at a time, reusing a single record buffer.
///
/// Quoting is disabled: the format never needs it, and with quoting on, one stray `"`
/// would silently merge every following row into a single field.
#[derive(Debug)]
pub(crate) struct TransactionReader<R> {
    reader: csv::Reader<LineTracker<R>>,
    headers: ByteRecord,
    record: ByteRecord,
}

impl<R: Read> TransactionReader<R> {
    /// Reads and checks the header: the first line that is not blank or whitespace
    /// only. Input with no such line is an empty batch, not an error.
    pub(crate) fn new(input: R) -> Result<Self, InputError> {
        let reader = ReaderBuilder::new()
            .has_headers(false)
            .quoting(false)
            .trim(Trim::All)
            .flexible(true)
            .from_reader(LineTracker::new(input));
        let mut this = Self {
            reader,
            headers: ByteRecord::new(),
            record: ByteRecord::new(),
        };
        if this.read_next_record()?.is_some() {
            validate_header(&this.record)?;
            std::mem::swap(&mut this.headers, &mut this.record);
        }
        Ok(this)
    }

    /// Reads the next record that is not whitespace only into `self.record` and returns
    /// the line it starts on.
    fn read_next_record(&mut self) -> Result<Option<u64>, InputError> {
        loop {
            match self.reader.read_byte_record(&mut self.record) {
                Ok(false) => return Ok(None),
                Ok(true) if is_whitespace_only(&self.record) => {}
                Ok(true) => {
                    let offset = self.record.position().map_or(0, csv::Position::byte);
                    return Ok(Some(self.reader.get_mut().record_line(offset)));
                }
                Err(error) => return Err(InputError::Read(super::into_io_error(error))),
            }
        }
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

    /// The next row, or an error that stops the run.
    fn next(&mut self) -> Option<Self::Item> {
        match self.read_next_record() {
            Ok(Some(line)) => Some(Ok(Row {
                line,
                parsed: self.parse_current(),
            })),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

fn is_whitespace_only(record: &ByteRecord) -> bool {
    record.len() == 1 && record.get(0).is_some_and(<[u8]>::is_empty)
}

/// Exactly the known columns, each at most once, in any order; `amount` is optional.
fn validate_header(headers: &ByteRecord) -> Result<(), InputError> {
    let mut seen: Vec<&'static str> = Vec::with_capacity(KNOWN_COLUMNS.len());
    for field in headers {
        let name = String::from_utf8_lossy(field);
        let known = KNOWN_COLUMNS
            .iter()
            .find(|column| **column == name)
            .ok_or_else(|| InputError::UnknownColumn(Excerpt::new(&name).to_string()))?;
        if seen.contains(known) {
            return Err(InputError::DuplicateColumn(known));
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
    use crate::domain::test_support::{chargeback, deposit, dispute, resolve, withdrawal};
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

    /// Rows as `(line, transaction or reason code)`.
    type Rows = Vec<(u64, Result<Transaction, &'static str>)>;

    /// Every row, or the fatal error with its causes.
    fn read_all(input: &[u8]) -> Result<Rows, String> {
        let reader = TransactionReader::new(input).map_err(|e| describe(&e))?;
        reader
            .map(|row| {
                row.map(|row| (row.line, row.parsed.map_err(|e| e.code())))
                    .map_err(|e| describe(&e))
            })
            .collect()
    }

    fn describe(error: &InputError) -> String {
        let mut text = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        text
    }

    fn parse(input: &str) -> Rows {
        read_all(input.as_bytes()).unwrap()
    }

    // TryFrom<RawRecord>

    #[rstest]
    #[case::deposit(raw("deposit", "1", "2", Some("3.5")), deposit(1, 2, "3.5"))]
    #[case::withdrawal(raw("withdrawal", "1", "2", Some("3")), withdrawal(1, 2, "3"))]
    #[case::dispute(raw("dispute", "1", "2", None), dispute(1, 2))]
    #[case::resolve(raw("resolve", "1", "2", None), resolve(1, 2))]
    #[case::chargeback(raw("chargeback", "1", "2", None), chargeback(1, 2))]
    #[case::dispute_amount_ignored(raw("dispute", "1", "2", Some("9.99")), dispute(1, 2))]
    #[case::dispute_invalid_amount_ignored(
        raw("dispute", "1", "2", Some("garbage")),
        dispute(1, 2)
    )]
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
    #[case::quoted_amount(raw("deposit", "1", "1", Some("\"5\"")), "invalid_amount")]
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

    #[rstest]
    fn every_transaction_kind_parses_back_from_its_name(
        #[values(
            deposit(1, 2, "3"),
            withdrawal(1, 2, "3"),
            dispute(1, 2),
            resolve(1, 2),
            chargeback(1, 2)
        )]
        transaction: Transaction,
    ) {
        let result = Transaction::try_from(raw(transaction.kind(), "1", "2", Some("3")));

        assert_eq!(result, Ok(transaction));
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

    // TransactionReader: line numbers

    #[rstest]
    #[case::consecutive("type,client,tx,amount\ndeposit,1,1,1\ndeposit,1,2,1\n", vec![2, 3])]
    #[case::blank_lines_lf("type,client,tx,amount\ndeposit,1,1,1\n\n\ndeposit,1,2,1\n", vec![2, 5])]
    #[case::blank_lines_crlf("type,client,tx,amount\r\ndeposit,1,1,1\r\n\r\ndeposit,1,2,1\r\n", vec![2, 4])]
    #[case::whitespace_lines("type,client,tx,amount\n   \n\t\ndeposit,1,1,1\n", vec![4])]
    #[case::leading_blank_lines("\n\ntype,client,tx,amount\ndeposit,1,1,1\n", vec![4])]
    #[case::leading_whitespace_lines("  \n\t\r\ntype,client,tx,amount\ndeposit,1,1,1\n", vec![4])]
    #[case::no_trailing_newline("type,client,tx,amount\n\ndeposit,1,1,1", vec![3])]
    fn line_numbers_are_physical_lines(#[case] input: &str, #[case] expected: Vec<u64>) {
        let lines: Vec<u64> = parse(input).into_iter().map(|(line, _)| line).collect();

        assert_eq!(lines, expected);
    }

    #[test]
    fn many_blank_lines_are_counted_not_stored() {
        let mut input = b"type,client,tx,amount\n".to_vec();
        input.extend(std::iter::repeat_n(b'\n', 100_000));
        input.extend_from_slice(b"deposit,1,1,1\n");
        let mut reader = TransactionReader::new(input.as_slice()).unwrap();

        let row = reader.next().unwrap().unwrap();

        assert_eq!(row.line, 100_002);
        assert!(reader.reader.get_ref().pending.len() <= 1);
    }

    // TransactionReader: shape of rows

    #[test]
    fn whitespace_only_lines_are_skipped() {
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

        assert_eq!(rows, vec![(2, Ok(dispute(1, 1)))]);
    }

    #[rstest]
    #[case::thousands_separator("deposit,1,7,1,000.00")]
    #[case::quoted_thousands_separator("deposit,1,7,\"1,000.00\"")]
    #[case::decimal_comma("deposit,1,7,1,5")]
    #[case::trailing_comma("deposit,1,7,1.0,")]
    fn row_with_more_fields_than_header_is_skipped(#[case] row: &str) {
        let rows = parse(&format!("type,client,tx,amount\n{row}\n"));

        assert_eq!(rows, vec![(2, Err("unexpected_field_count"))]);
    }

    #[test]
    fn unbalanced_quote_affects_only_its_own_row() {
        let rows =
            parse("type,client,tx,amount\ndeposit,2,2,\"5\ndeposit,1,3,10\nwithdrawal,1,4,5\n");

        assert_eq!(
            rows,
            vec![
                (2, Err("invalid_amount")),
                (3, Ok(deposit(1, 3, "10"))),
                (4, Ok(withdrawal(1, 4, "5"))),
            ]
        );
    }

    #[test]
    fn invalid_utf8_row_is_skipped_and_reading_continues() {
        let rows = read_all(b"type,client,tx,amount\ndeposit,1,1,\xff\ndeposit,1,2,1\n").unwrap();

        assert_eq!(
            rows,
            vec![(2, Err("malformed_record")), (3, Ok(deposit(1, 2, "1")))]
        );
    }

    #[rstest]
    #[case::long_amount(format!("deposit,1,1,{}1", "0".repeat(2000)))]
    #[case::whitespace_padding(format!("{}deposit,1,1,5", " ".repeat(1100)))]
    #[case::one_byte_over(format!("deposit,1,1,{}1", "0".repeat(1024 - 12)))]
    fn line_longer_than_limit_stops_the_run(#[case] row: String) {
        let input = format!("type,client,tx,amount\ndeposit,1,1,1\n{row}\ndeposit,1,2,1\n");

        let error = read_all(input.as_bytes()).unwrap_err();

        assert!(error.contains("line 3: longer than 1024 bytes"), "{error}");
    }

    #[test]
    fn huge_line_stops_the_run() {
        let mut input = b"type,client,tx,amount\ndeposit,1,1,".to_vec();
        input.extend(std::iter::repeat_n(b'0', 10_000_000));

        let error = read_all(&input).unwrap_err();

        assert!(error.contains("line 2: longer than 1024 bytes"), "{error}");
    }

    #[test]
    fn line_of_exactly_the_limit_is_accepted() {
        let prefix = "deposit,1,1,";
        let padding = "0".repeat(1024 - prefix.len() - 1);
        let input = format!("type,client,tx,amount\n{prefix}{padding}1\n");

        let rows = parse(&input);

        assert_eq!(rows, vec![(2, Ok(deposit(1, 1, "1")))]);
    }

    #[test]
    fn header_without_amount_column_is_accepted() {
        let rows = parse("type,client,tx\ndispute,1,1\ndeposit,1,2\n");

        assert_eq!(
            rows,
            vec![(2, Ok(dispute(1, 1))), (3, Err("missing_amount"))]
        );
    }

    #[rstest]
    #[case::empty("")]
    #[case::blank_lines("\n\n")]
    #[case::whitespace_lines("  \n\t\n")]
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

    #[test]
    fn byte_order_mark_does_not_count_toward_the_line_limit() {
        let header = format!("type,client,tx,amount{}", " ".repeat(1024 - 21));
        let input = format!("\u{feff}{header}\ndeposit,1,1,1\n");

        let rows = parse(&input);

        assert_eq!(rows, vec![(2, Ok(deposit(1, 1, "1")))]);
    }

    #[test]
    fn byte_order_mark_does_not_lift_the_line_limit() {
        let header = format!("type,client,tx,amount{}", " ".repeat(1025 - 21));
        let input = format!("\u{feff}{header}\n");

        let error = read_all(input.as_bytes()).unwrap_err();

        assert!(error.contains("line 1: longer than 1024 bytes"), "{error}");
    }

    #[test]
    fn partial_byte_order_mark_counts_toward_the_line_limit() {
        let mut input = vec![0xEF, 0xBB];
        input.extend(std::iter::repeat_n(b'x', 1023));
        input.push(b'\n');

        let error = read_all(&input).unwrap_err();

        assert!(error.contains("line 1: longer than 1024 bytes"), "{error}");
    }

    // TransactionReader: errors that stop the run

    #[rstest]
    #[case::missing_type("client,tx,amount\n", "missing required column \"type\"")]
    #[case::missing_tx("type,client,amount\n", "missing required column \"tx\"")]
    #[case::unknown_column("type,client,tx,amount,note\n", "unknown column \"note\"")]
    #[case::capitalised_column("Type,client,tx,amount\n", "unknown column \"Type\"")]
    #[case::repeated_column("type,client,tx,amount,amount\n", "repeats column \"amount\"")]
    #[case::no_header_row("deposit,1,1,1.0\n", "unknown column \"deposit\"")]
    fn invalid_header_stops_the_run(#[case] input: &str, #[case] message: &str) {
        let error = read_all(input.as_bytes()).unwrap_err();

        assert!(error.contains(message), "{error}");
    }

    #[rstest]
    #[case::trailing_padding(format!("\ntype,client,tx,amount{}\n", " ".repeat(2000)))]
    #[case::leading_padding(format!("\n{}type,client,tx,amount\n", " ".repeat(2000)))]
    fn header_longer_than_limit_stops_the_run(#[case] input: String) {
        let error = read_all(input.as_bytes()).unwrap_err();

        assert!(error.contains("line 2: longer than 1024 bytes"), "{error}");
    }

    #[rstest]
    #[case::cr_only_line_endings("type,client,tx,amount\rdeposit,1,1,1\r", 1)]
    #[case::cr_inside_row("type,client,tx,amount\ndeposit,1,1\r,1\n", 2)]
    #[case::cr_at_end_of_input("type,client,tx,amount\ndeposit,1,1,1\n\r", 3)]
    #[case::cr_before_crlf("type,client,tx,amount\r\r\n", 1)]
    fn carriage_return_without_line_feed_stops_the_run(#[case] input: &str, #[case] line: u64) {
        let error = read_all(input.as_bytes()).unwrap_err();

        assert!(
            error.contains(&format!("line {line}: carriage return without line feed")),
            "{error}"
        );
    }

    #[test]
    fn crlf_line_endings_are_accepted() {
        let rows = parse("type,client,tx,amount\r\ndeposit,1,1,1\r\n");

        assert_eq!(rows, vec![(2, Ok(deposit(1, 1, "1")))]);
    }
}
