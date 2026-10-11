//! A client's account: three balances and a lock.

use super::{Amount, Balance, Rejection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountStatus {
    #[default]
    Active,
    /// Set by a chargeback; never cleared.
    Locked,
}

/// Balances of one client.
///
/// Fields are private and change only through [`Account::update`], which computes all
/// three balances with checked arithmetic and writes them together, so
/// `total == available + held` always holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    available: Balance,
    held: Balance,
    total: Balance,
    status: AccountStatus,
}

/// A change to one balance.
#[derive(Debug, Clone, Copy)]
enum Change {
    Unchanged,
    Credit(Amount),
    Debit(Amount),
}

impl Change {
    fn apply(self, balance: Balance) -> Option<Balance> {
        match self {
            Self::Unchanged => Some(balance),
            Self::Credit(amount) => balance.checked_add(amount),
            Self::Debit(amount) => balance.checked_sub(amount),
        }
    }
}

impl Account {
    /// Funds the client can withdraw.
    pub const fn available(&self) -> Balance {
        self.available
    }

    /// Funds held by open disputes.
    pub const fn held(&self) -> Balance {
        self.held
    }

    /// `available + held`.
    pub const fn total(&self) -> Balance {
        self.total
    }

    pub const fn status(&self) -> AccountStatus {
        self.status
    }

    pub const fn is_locked(&self) -> bool {
        matches!(self.status, AccountStatus::Locked)
    }

    pub(super) fn deposit(&mut self, amount: Amount) -> Result<(), Rejection> {
        self.ensure_active()?;
        self.update(Change::Credit(amount), Change::Unchanged)
    }

    pub(super) fn withdraw(&mut self, amount: Amount) -> Result<(), Rejection> {
        self.ensure_active()?;
        if !self.available.covers(amount) {
            return Err(Rejection::InsufficientFunds);
        }
        self.update(Change::Debit(amount), Change::Unchanged)
    }

    /// Moves a disputed amount from available to held. Available may go negative.
    pub(super) fn hold(&mut self, amount: Amount) -> Result<(), Rejection> {
        self.update(Change::Debit(amount), Change::Credit(amount))
    }

    /// Moves a resolved amount from held back to available.
    pub(super) fn release(&mut self, amount: Amount) -> Result<(), Rejection> {
        self.update(Change::Credit(amount), Change::Debit(amount))
    }

    /// Removes a charged-back amount from held, and so from total, and locks.
    pub(super) fn charge_back(&mut self, amount: Amount) -> Result<(), Rejection> {
        self.update(Change::Unchanged, Change::Debit(amount))?;
        self.status = AccountStatus::Locked;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), Rejection> {
        if self.is_locked() {
            Err(Rejection::AccountLocked)
        } else {
            Ok(())
        }
    }

    /// The only place balances change. Nothing is written unless every new value fits.
    fn update(&mut self, available: Change, held: Change) -> Result<(), Rejection> {
        let available = available
            .apply(self.available)
            .ok_or(Rejection::BalanceOverflow)?;
        let held = held.apply(self.held).ok_or(Rejection::BalanceOverflow)?;
        let total = available
            .checked_sum(held)
            .ok_or(Rejection::BalanceOverflow)?;
        self.available = available;
        self.held = held;
        self.total = total;
        Ok(())
    }
}
