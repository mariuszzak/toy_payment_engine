//! Validated transactions: what the ledger accepts.

use std::fmt;

use super::{Amount, ClientId, TxId};

/// A transaction that passed every stateless check.
///
/// Movements carry an [`Amount`]; dispute events refer to an earlier transaction and
/// have no amount field at all, so neither can be built with the other's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transaction {
    Deposit {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Withdrawal {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Dispute {
        client: ClientId,
        tx: TxId,
    },
    Resolve {
        client: ClientId,
        tx: TxId,
    },
    Chargeback {
        client: ClientId,
        tx: TxId,
    },
}

impl Transaction {
    pub const fn client(&self) -> ClientId {
        match *self {
            Self::Deposit { client, .. }
            | Self::Withdrawal { client, .. }
            | Self::Dispute { client, .. }
            | Self::Resolve { client, .. }
            | Self::Chargeback { client, .. } => client,
        }
    }

    pub const fn tx(&self) -> TxId {
        match *self {
            Self::Deposit { tx, .. }
            | Self::Withdrawal { tx, .. }
            | Self::Dispute { tx, .. }
            | Self::Resolve { tx, .. }
            | Self::Chargeback { tx, .. } => tx,
        }
    }

    /// The transaction type as written in the input.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Deposit { .. } => "deposit",
            Self::Withdrawal { .. } => "withdrawal",
            Self::Dispute { .. } => "dispute",
            Self::Resolve { .. } => "resolve",
            Self::Chargeback { .. } => "chargeback",
        }
    }
}

/// For diagnostics, for example `withdrawal tx 5 for client 2`.
impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} tx {} for client {}",
            self.kind(),
            self.tx(),
            self.client()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::test_support::{chargeback, deposit, dispute, resolve, withdrawal};
    use rstest::rstest;

    #[rstest]
    #[case::deposit(deposit(1, 2, "3"), "deposit tx 2 for client 1")]
    #[case::withdrawal(withdrawal(1, 2, "3"), "withdrawal tx 2 for client 1")]
    #[case::dispute(dispute(1, 2), "dispute tx 2 for client 1")]
    #[case::resolve(resolve(1, 2), "resolve tx 2 for client 1")]
    #[case::chargeback(chargeback(1, 2), "chargeback tx 2 for client 1")]
    fn displays_kind_id_and_client(#[case] transaction: Transaction, #[case] expected: &str) {
        assert_eq!(transaction.to_string(), expected);
    }
}
