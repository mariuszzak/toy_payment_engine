//! Fixed-point money.
//!
//! Every value is a whole number of units, where one unit is 0.0001. Text is parsed
//! straight into units, so no value ever passes through floating point, and every
//! arithmetic operation is checked.

use std::fmt;
use std::str::FromStr;

/// Decimal places carried by every amount and balance.
const DECIMAL_PLACES: usize = 4;

/// Units per whole currency unit: 10^DECIMAL_PLACES (checked by a unit test).
const SCALE: i64 = 10_000;
const SCALE_UNSIGNED: u64 = SCALE.unsigned_abs();

/// Why a piece of text is not a valid amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("not a plain decimal number")]
    InvalidFormat,
    #[error("non-zero digits beyond 4 decimal places")]
    TooPrecise,
    #[error("too large")]
    OutOfRange,
    #[error("must be greater than zero")]
    NotPositive,
}

/// A strictly positive amount of money, exact to 0.0001.
///
/// Transaction amounts are always of this type, so a zero or negative deposit or
/// withdrawal cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(i64);

impl Amount {
    /// The largest amount, 922337203685477.5807.
    #[cfg(test)]
    pub(crate) const MAX: Self = Self(i64::MAX);

    /// Builds an amount from a number of 0.0001 units.
    pub fn from_units(units: i64) -> Result<Self, MoneyError> {
        if units > 0 {
            Ok(Self(units))
        } else {
            Err(MoneyError::NotPositive)
        }
    }

    /// The amount as a number of 0.0001 units.
    pub const fn units(self) -> i64 {
        self.0
    }
}

/// Parses a plain decimal such as `1`, `1.`, `.5` or `0012.3400`.
///
/// Signs, exponents, separators and whitespace are rejected. Digits beyond the fourth
/// decimal place are accepted only when they are zeros, so no value is ever rounded.
impl FromStr for Amount {
    type Err = MoneyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_units(parse_units(text)?)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_units(f, self.0)
    }
}

/// A signed account balance, exact to 0.0001. Can be negative after a dispute or
/// chargeback of funds that were already withdrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Balance(i64);

impl Balance {
    /// Builds a balance from a number of 0.0001 units.
    #[cfg(test)]
    pub(crate) const fn from_units(units: i64) -> Self {
        Self(units)
    }

    /// The balance as a number of 0.0001 units.
    pub const fn units(self) -> i64 {
        self.0
    }

    /// `self + amount`, or `None` on overflow.
    pub fn checked_add(self, amount: Amount) -> Option<Self> {
        self.0.checked_add(amount.0).map(Self)
    }

    /// `self - amount`, or `None` on overflow.
    pub fn checked_sub(self, amount: Amount) -> Option<Self> {
        self.0.checked_sub(amount.0).map(Self)
    }

    /// `self + other`, or `None` on overflow.
    pub fn checked_sum(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Whether the balance is at least `amount`.
    pub fn covers(self, amount: Amount) -> bool {
        self.0 >= amount.0
    }
}

impl fmt::Display for Balance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_units(f, self.0)
    }
}

/// Writes units as a decimal with exactly four places, for example `-8.0000`.
fn write_units(f: &mut fmt::Formatter<'_>, units: i64) -> fmt::Result {
    let sign = if units < 0 { "-" } else { "" };
    // unsigned_abs cannot overflow, even for i64::MIN.
    let magnitude = units.unsigned_abs();
    let whole = magnitude / SCALE_UNSIGNED;
    let fraction = magnitude % SCALE_UNSIGNED;
    write!(
        f,
        "{sign}{whole}.{fraction:0width$}",
        width = DECIMAL_PLACES
    )
}

/// Converts a plain decimal such as `12.5` into units of 0.0001 (`125000`).
fn parse_units(text: &str) -> Result<i64, MoneyError> {
    // 1. Split at the decimal point: "12.5" gives "12" and "5"; "12" has no fraction.
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));

    // 2. Both parts must be plain ASCII digits, and at least one must be present
    //    ("1.", ".5" and "1" are fine; ".", "-1" and "1e3" are not).
    let has_digits = !whole.is_empty() || !fraction.is_empty();
    if !has_digits || !is_digits(whole) || !is_digits(fraction) {
        return Err(MoneyError::InvalidFormat);
    }

    // 3. Keep at most four decimal places. Any digits after them must be zeros, so the
    //    value never needs rounding.
    let fraction = within_decimal_places(fraction)?;

    // 4. Pad the fraction to exactly four digits: ".5" means 5000 units, not 5.
    let fraction_units = digits_value(&format!("{fraction:0<DECIMAL_PLACES$}"))?;

    // 5. units = whole * 10_000 + fraction, failing instead of overflowing.
    digits_value(whole)?
        .checked_mul(SCALE)
        .and_then(|units| units.checked_add(fraction_units))
        .ok_or(MoneyError::OutOfRange)
}

/// The fraction's first four digits. Digits after them are accepted only if they are
/// all zeros (`5000` in `1.50000`), because anything else would need rounding.
fn within_decimal_places(fraction: &str) -> Result<&str, MoneyError> {
    // `fraction` is ASCII digits, so splitting at a byte position is safe.
    let Some((kept, excess)) = fraction.split_at_checked(DECIMAL_PLACES) else {
        return Ok(fraction); // four digits or fewer
    };
    if excess.bytes().all(|digit| digit == b'0') {
        Ok(kept)
    } else {
        Err(MoneyError::TooPrecise)
    }
}

fn is_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The value of a string of ASCII digits; `0` for an empty string.
fn digits_value(digits: &str) -> Result<i64, MoneyError> {
    if digits.is_empty() {
        return Ok(0);
    }
    // The caller has already checked these are ASCII digits, so the only way parsing
    // can fail is a value too large for `i64`.
    digits.parse().map_err(|_| MoneyError::OutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::whole_number("1", 10_000)]
    #[case::one_place("1.5", 15_000)]
    #[case::four_places("1.2345", 12_345)]
    #[case::trailing_dot("1.", 10_000)]
    #[case::leading_dot(".5", 5_000)]
    #[case::smallest(".0001", 1)]
    #[case::leading_zeros("007.5", 75_000)]
    #[case::zero_padding_beyond_four_places("1.50000000", 15_000)]
    #[case::largest("922337203685477.5807", i64::MAX)]
    fn parses_plain_decimals_exactly(#[case] text: &str, #[case] units: i64) {
        let amount: Amount = text.parse().unwrap();

        assert_eq!(amount.units(), units);
    }

    #[rstest]
    #[case::empty("", MoneyError::InvalidFormat)]
    #[case::lone_dot(".", MoneyError::InvalidFormat)]
    #[case::negative("-1", MoneyError::InvalidFormat)]
    #[case::explicit_plus("+1", MoneyError::InvalidFormat)]
    #[case::exponent("1e3", MoneyError::InvalidFormat)]
    #[case::thousands_comma("1,000", MoneyError::InvalidFormat)]
    #[case::underscore("1_000", MoneyError::InvalidFormat)]
    #[case::inner_space("1 .5", MoneyError::InvalidFormat)]
    #[case::two_dots("1.2.3", MoneyError::InvalidFormat)]
    #[case::not_a_number("NaN", MoneyError::InvalidFormat)]
    #[case::infinity("inf", MoneyError::InvalidFormat)]
    #[case::non_ascii_digit("١", MoneyError::InvalidFormat)]
    #[case::non_ascii_in_excess("1.0000é", MoneyError::InvalidFormat)]
    #[case::fifth_place("1.23456", MoneyError::TooPrecise)]
    #[case::non_zero_far_digit("1.00000001", MoneyError::TooPrecise)]
    #[case::zero("0", MoneyError::NotPositive)]
    #[case::zero_with_places("0.0000", MoneyError::NotPositive)]
    #[case::one_unit_above_max("922337203685477.5808", MoneyError::OutOfRange)]
    #[case::huge_whole_part("99999999999999999999", MoneyError::OutOfRange)]
    fn rejects_invalid_amounts(#[case] text: &str, #[case] expected: MoneyError) {
        let result = text.parse::<Amount>();

        assert_eq!(result, Err(expected));
    }

    #[rstest]
    #[case::whole(10_000, "1.0000")]
    #[case::fraction(15_000, "1.5000")]
    #[case::smallest(1, "0.0001")]
    #[case::zero(0, "0.0000")]
    #[case::negative(-80_000, "-8.0000")]
    #[case::negative_fraction(-1, "-0.0001")]
    #[case::minimum(i64::MIN, "-922337203685477.5808")]
    #[case::maximum(i64::MAX, "922337203685477.5807")]
    fn balance_displays_four_decimal_places(#[case] units: i64, #[case] expected: &str) {
        let balance = Balance::from_units(units);

        assert_eq!(balance.to_string(), expected);
    }

    #[test]
    fn scale_matches_decimal_places() {
        let places = u32::try_from(DECIMAL_PLACES).unwrap();

        assert_eq!(SCALE, 10_i64.pow(places));
    }

    #[test]
    fn amount_from_units_rejects_zero_and_negative() {
        assert_eq!(Amount::from_units(0), Err(MoneyError::NotPositive));
        assert_eq!(Amount::from_units(-1), Err(MoneyError::NotPositive));
    }

    #[test]
    fn balance_arithmetic_reports_overflow() {
        let max = Balance::from_units(i64::MAX);
        let min = Balance::from_units(i64::MIN);

        let smallest = Amount::from_units(1).unwrap();

        assert_eq!(max.checked_add(smallest), None);
        assert_eq!(min.checked_sub(smallest), None);
        assert_eq!(max.checked_sum(max), None);
    }

    #[test]
    fn balance_arithmetic_is_exact() {
        let amount: Amount = "0.0001".parse().unwrap();

        let balance = Balance::default().checked_add(amount).unwrap();

        assert_eq!(balance.to_string(), "0.0001");
        assert_eq!(balance.checked_sub(amount), Some(Balance::default()));
    }

    #[rstest]
    #[case::more(20_000, true)]
    #[case::equal(10_000, true)]
    #[case::less(9_999, false)]
    #[case::negative(-10_000, false)]
    fn covers_compares_balance_with_amount(#[case] units: i64, #[case] expected: bool) {
        let amount = Amount::from_units(10_000).unwrap();

        assert_eq!(Balance::from_units(units).covers(amount), expected);
    }
}
