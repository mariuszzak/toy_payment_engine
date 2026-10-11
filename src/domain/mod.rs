//! The pure core: money, identifiers, transactions and ledger rules.
//!
//! Nothing in this module performs I/O or knows about CSV.

mod ids;
mod money;

pub use ids::{ClientId, TxId};
pub use money::{Amount, Balance, DECIMAL_PLACES, MoneyError};
