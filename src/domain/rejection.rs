//! Why the ledger refused a valid transaction.

/// A ledger rule refused the transaction. The ledger's balances and lock status are
/// unchanged; see [`crate::domain::Ledger::apply`] for the one permitted side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("transaction ID already used")]
    DuplicateTx,
    #[error("account is locked")]
    AccountLocked,
    #[error("insufficient available funds")]
    InsufficientFunds,
    #[error("referenced transaction does not exist")]
    UnknownTx,
    #[error("referenced transaction belongs to another client")]
    ClientMismatch,
    #[error("only accepted deposits can be disputed")]
    NotDisputable,
    #[error("deposit is already under dispute")]
    AlreadyDisputed,
    #[error("deposit is not under dispute")]
    NotUnderDispute,
    #[error("deposit was already charged back")]
    ChargedBack,
    #[error("a balance would overflow")]
    BalanceOverflow,
}

impl Rejection {
    /// Stable reason code for diagnostics and tests.
    pub const fn code(self) -> &'static str {
        match self {
            Self::DuplicateTx => "duplicate_tx",
            Self::AccountLocked => "account_locked",
            Self::InsufficientFunds => "insufficient_funds",
            Self::UnknownTx => "unknown_tx",
            Self::ClientMismatch => "client_mismatch",
            Self::NotDisputable => "not_disputable",
            Self::AlreadyDisputed => "already_disputed",
            Self::NotUnderDispute => "not_under_dispute",
            Self::ChargedBack => "charged_back",
            Self::BalanceOverflow => "balance_overflow",
        }
    }

    /// Every variant. Public because the property tests in `tests/` check that their
    /// generators reach each one.
    pub const ALL: [Self; 10] = [
        Self::DuplicateTx,
        Self::AccountLocked,
        Self::InsufficientFunds,
        Self::UnknownTx,
        Self::ClientMismatch,
        Self::NotDisputable,
        Self::AlreadyDisputed,
        Self::NotUnderDispute,
        Self::ChargedBack,
        Self::BalanceOverflow,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_rejection_has_a_distinct_code() {
        let codes: HashSet<_> = Rejection::ALL.iter().map(|r| r.code()).collect();

        assert_eq!(codes.len(), Rejection::ALL.len());
    }
}
