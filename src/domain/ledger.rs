//! The ledger: every account and every claimed transaction ID, and the rules that
//! connect them.

use std::collections::{BTreeMap, HashMap};

use super::{Account, Amount, ClientId, Rejection, Transaction, TxId};

/// Where a deposit is in the dispute process.
///
/// ```text
/// Settled | Resolved --dispute--> Disputed --resolve--> Resolved
///                                 Disputed --chargeback--> ChargedBack (final)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisputeState {
    /// Accepted and never disputed.
    Settled,
    /// Under dispute: the amount is held.
    Disputed,
    /// The dispute ended and the held amount was released; it can be disputed again.
    Resolved,
    /// Reversed by a chargeback; final.
    ChargedBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisputeAction {
    Dispute,
    Resolve,
    Chargeback,
}

impl DisputeState {
    /// The state after `action`, or why the action is not allowed.
    fn next(self, action: DisputeAction) -> Result<Self, Rejection> {
        use DisputeAction as A;
        match (self, action) {
            (Self::Settled | Self::Resolved, A::Dispute) => Ok(Self::Disputed),
            (Self::Disputed, A::Dispute) => Err(Rejection::AlreadyDisputed),
            (Self::Disputed, A::Resolve) => Ok(Self::Resolved),
            (Self::Disputed, A::Chargeback) => Ok(Self::ChargedBack),
            (Self::Settled | Self::Resolved, A::Resolve | A::Chargeback) => {
                Err(Rejection::NotUnderDispute)
            }
            (Self::ChargedBack, _) => Err(Rejection::ChargedBack),
        }
    }
}

/// What the ledger remembers about a claimed transaction ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxRecord {
    /// An accepted deposit, which can be disputed.
    Deposit {
        client: ClientId,
        amount: Amount,
        state: DisputeState,
    },
    /// A withdrawal, or a deposit that was declined. Kept so the ID stays unique and the
    /// owner can be checked before the type.
    NotDisputable { client: ClientId },
}

impl TxRecord {
    const fn owner(&self) -> ClientId {
        match *self {
            Self::Deposit { client, .. } | Self::NotDisputable { client } => client,
        }
    }
}

/// All accounts and transaction records. Pure: no I/O.
#[derive(Debug, Default)]
pub struct Ledger {
    /// Ordered, so accounts are reported by client ID without a separate sort.
    accounts: BTreeMap<ClientId, Account>,
    transactions: HashMap<TxId, TxRecord>,
}

impl Ledger {
    /// An empty ledger: no accounts, no transactions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one transaction.
    ///
    /// On `Err`, no balance or lock status has changed. A deposit or withdrawal with a
    /// fresh ID is the one exception to "nothing changed": it claims its ID and creates
    /// the client's account before its own checks run, so a declined attempt still
    /// counts as having happened.
    pub fn apply(&mut self, transaction: Transaction) -> Result<(), Rejection> {
        match transaction {
            Transaction::Deposit { client, tx, amount } => self.deposit(client, tx, amount),
            Transaction::Withdrawal { client, tx, amount } => {
                self.register(client, tx)?.withdraw(amount)
            }
            Transaction::Dispute { client, tx } => {
                self.dispute_step(client, tx, DisputeAction::Dispute)
            }
            Transaction::Resolve { client, tx } => {
                self.dispute_step(client, tx, DisputeAction::Resolve)
            }
            Transaction::Chargeback { client, tx } => {
                self.dispute_step(client, tx, DisputeAction::Chargeback)
            }
        }
    }

    /// Accounts in ascending client order.
    pub fn accounts(&self) -> impl Iterator<Item = (ClientId, &Account)> {
        self.accounts
            .iter()
            .map(|(client, account)| (*client, account))
    }

    /// The client's account, if one has been created.
    pub fn account(&self, client: ClientId) -> Option<&Account> {
        self.accounts.get(&client)
    }

    /// The dispute state of a deposit, or `None` if `tx` is not an accepted deposit.
    pub fn dispute_state(&self, tx: TxId) -> Option<DisputeState> {
        match self.transactions.get(&tx) {
            Some(TxRecord::Deposit { state, .. }) => Some(*state),
            _ => None,
        }
    }

    fn deposit(&mut self, client: ClientId, tx: TxId, amount: Amount) -> Result<(), Rejection> {
        self.register(client, tx)?.deposit(amount)?;
        self.transactions.insert(
            tx,
            TxRecord::Deposit {
                client,
                amount,
                state: DisputeState::Settled,
            },
        );
        Ok(())
    }

    /// Claims a fresh transaction ID for a deposit or withdrawal and returns the
    /// client's account, creating it if needed. A reused ID changes nothing.
    fn register(&mut self, client: ClientId, tx: TxId) -> Result<&mut Account, Rejection> {
        if self.transactions.contains_key(&tx) {
            return Err(Rejection::DuplicateTx);
        }
        // Not disputable until the deposit is accepted; see `deposit`.
        self.transactions
            .insert(tx, TxRecord::NotDisputable { client });
        Ok(self.accounts.entry(client).or_default())
    }

    /// Dispute, resolve or chargeback. Checks run in a fixed order, which decides the
    /// reason code: existence, ownership, type, dispute state, then balances.
    fn dispute_step(
        &mut self,
        client: ClientId,
        tx: TxId,
        action: DisputeAction,
    ) -> Result<(), Rejection> {
        // 1. The transaction must exist.
        let record = self.transactions.get_mut(&tx).ok_or(Rejection::UnknownTx)?;

        // 2. It must belong to the client named on the row.
        if record.owner() != client {
            return Err(Rejection::ClientMismatch);
        }

        // 3. It must be an accepted deposit.
        let TxRecord::Deposit { amount, state, .. } = record else {
            return Err(Rejection::NotDisputable);
        };

        // 4. Its dispute state must allow this step.
        let next_state = state.next(action)?;

        // 5. Move the money; this fails, changing nothing, if a balance would overflow.
        //    The deposit created this account, so it exists; `or_default` only avoids an
        //    unreachable error path.
        let account = self.accounts.entry(client).or_default();
        match action {
            DisputeAction::Dispute => account.hold(*amount)?,
            DisputeAction::Resolve => account.release(*amount)?,
            DisputeAction::Chargeback => account.charge_back(*amount)?,
        }

        // 6. Only now that the money has moved, record the new state.
        *state = next_state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::test_support::{chargeback, deposit, dispute, resolve, withdrawal};
    use rstest::rstest;

    /// A ledger after applying `transactions`, each of which must be accepted.
    fn ledger_after(transactions: &[Transaction]) -> Ledger {
        let mut ledger = Ledger::new();
        for transaction in transactions {
            ledger
                .apply(*transaction)
                .unwrap_or_else(|e| panic!("arrange step {transaction} rejected: {e}"));
        }
        ledger
    }

    /// `(available, held, total, locked)` of a client, formatted for easy comparison.
    fn balances(ledger: &Ledger, client: u16) -> (String, String, String, bool) {
        let account = ledger.account(ClientId::new(client)).unwrap();
        (
            account.available().to_string(),
            account.held().to_string(),
            account.total().to_string(),
            account.is_locked(),
        )
    }

    fn expected(
        available: &str,
        held: &str,
        total: &str,
        locked: bool,
    ) -> (String, String, String, bool) {
        (available.into(), held.into(), total.into(), locked)
    }

    // Deposits and withdrawals

    #[test]
    fn deposit_creates_account_and_credits_available_and_total() {
        let mut ledger = Ledger::new();

        let result = ledger.apply(deposit(1, 1, "1.5"));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("1.5000", "0.0000", "1.5000", false)
        );
    }

    #[test]
    fn withdrawal_debits_available_and_total() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10")]);

        let result = ledger.apply(withdrawal(1, 2, "2.5"));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("7.5000", "0.0000", "7.5000", false)
        );
    }

    #[test]
    fn withdrawal_of_entire_available_balance_leaves_zero() {
        let mut ledger = ledger_after(&[deposit(1, 1, "3")]);

        let result = ledger.apply(withdrawal(1, 2, "3"));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("0.0000", "0.0000", "0.0000", false)
        );
    }

    #[test]
    fn withdrawal_exceeding_available_is_rejected_and_balances_unchanged() {
        let mut ledger = ledger_after(&[deposit(1, 1, "1")]);

        let result = ledger.apply(withdrawal(1, 2, "1.0001"));

        assert_eq!(result, Err(Rejection::InsufficientFunds));
        assert_eq!(
            balances(&ledger, 1),
            expected("1.0000", "0.0000", "1.0000", false)
        );
    }

    #[test]
    fn withdrawal_cannot_use_held_funds() {
        let mut ledger = ledger_after(&[deposit(1, 1, "5"), dispute(1, 1)]);

        let result = ledger.apply(withdrawal(1, 2, "1"));

        assert_eq!(result, Err(Rejection::InsufficientFunds));
    }

    #[test]
    fn declined_first_withdrawal_still_creates_a_zero_account() {
        let mut ledger = Ledger::new();

        let result = ledger.apply(withdrawal(7, 1, "1"));

        assert_eq!(result, Err(Rejection::InsufficientFunds));
        assert_eq!(
            balances(&ledger, 7),
            expected("0.0000", "0.0000", "0.0000", false)
        );
    }

    #[test]
    fn declined_withdrawal_still_claims_its_transaction_id() {
        let mut ledger = Ledger::new();
        let _ = ledger.apply(withdrawal(1, 1, "1"));

        let result = ledger.apply(deposit(1, 1, "5"));

        assert_eq!(result, Err(Rejection::DuplicateTx));
    }

    #[rstest]
    #[case::deposit_after_deposit(deposit(1, 1, "1"), deposit(1, 1, "2"))]
    #[case::withdrawal_after_deposit(deposit(1, 1, "5"), withdrawal(1, 1, "1"))]
    #[case::deposit_after_withdrawal(withdrawal(1, 1, "1"), deposit(1, 1, "2"))]
    #[case::other_client(deposit(1, 1, "1"), deposit(2, 1, "2"))]
    fn reused_transaction_id_is_rejected(#[case] first: Transaction, #[case] second: Transaction) {
        let mut ledger = Ledger::new();
        let _ = ledger.apply(first);

        let result = ledger.apply(second);

        assert_eq!(result, Err(Rejection::DuplicateTx));
    }

    #[test]
    fn reused_transaction_id_from_new_client_creates_no_account() {
        let mut ledger = ledger_after(&[deposit(1, 1, "1")]);

        let result = ledger.apply(deposit(2, 1, "1"));

        assert_eq!(result, Err(Rejection::DuplicateTx));
        assert!(ledger.account(ClientId::new(2)).is_none());
    }

    #[test]
    fn deposit_that_would_overflow_is_rejected_and_keeps_existing_balance() {
        let max = Amount::MAX.to_string();
        let mut ledger = ledger_after(&[deposit(1, 1, &max)]);

        let result = ledger.apply(deposit(1, 2, "0.0001"));

        assert_eq!(result, Err(Rejection::BalanceOverflow));
        assert_eq!(balances(&ledger, 1), expected(&max, "0.0000", &max, false));
        assert_eq!(ledger.dispute_state(TxId::new(2)), None);
    }

    #[test]
    fn dispute_that_would_overflow_held_is_rejected_and_changes_nothing() {
        let max = Amount::MAX.to_string();
        let mut ledger = ledger_after(&[
            deposit(1, 1, &max),
            withdrawal(1, 2, &max),
            deposit(1, 3, &max),
            dispute(1, 1),
        ]);
        let before = balances(&ledger, 1);

        let result = ledger.apply(dispute(1, 3));

        assert_eq!(result, Err(Rejection::BalanceOverflow));
        assert_eq!(balances(&ledger, 1), before);
        assert_eq!(
            ledger.dispute_state(TxId::new(3)),
            Some(DisputeState::Settled)
        );
    }

    // Dispute lifecycle

    #[test]
    fn dispute_moves_amount_from_available_to_held() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10"), deposit(1, 2, "5")]);

        let result = ledger.apply(dispute(1, 1));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("5.0000", "10.0000", "15.0000", false)
        );
        assert_eq!(
            ledger.dispute_state(TxId::new(1)),
            Some(DisputeState::Disputed)
        );
    }

    #[test]
    fn dispute_after_withdrawal_makes_available_negative() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10"), withdrawal(1, 2, "8")]);

        let result = ledger.apply(dispute(1, 1));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("-8.0000", "10.0000", "2.0000", false)
        );
    }

    #[test]
    fn resolve_releases_held_funds() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10"), dispute(1, 1)]);

        let result = ledger.apply(resolve(1, 1));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("10.0000", "0.0000", "10.0000", false)
        );
        assert_eq!(
            ledger.dispute_state(TxId::new(1)),
            Some(DisputeState::Resolved)
        );
    }

    #[test]
    fn chargeback_removes_held_funds_and_locks_account() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10"), deposit(1, 2, "3"), dispute(1, 1)]);

        let result = ledger.apply(chargeback(1, 1));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("3.0000", "0.0000", "3.0000", true)
        );
        assert_eq!(
            ledger.dispute_state(TxId::new(1)),
            Some(DisputeState::ChargedBack)
        );
    }

    #[test]
    fn chargeback_after_withdrawal_leaves_negative_total() {
        let mut ledger = ledger_after(&[deposit(1, 1, "10"), withdrawal(1, 2, "8"), dispute(1, 1)]);

        let result = ledger.apply(chargeback(1, 1));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("-8.0000", "0.0000", "-8.0000", true)
        );
    }

    #[test]
    fn resolved_deposit_can_be_disputed_again_and_charged_back() {
        let mut ledger = ledger_after(&[deposit(1, 1, "4"), dispute(1, 1), resolve(1, 1)]);

        let disputed = ledger.apply(dispute(1, 1));
        let charged_back = ledger.apply(chargeback(1, 1));

        assert_eq!(disputed, Ok(()));
        assert_eq!(charged_back, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("0.0000", "0.0000", "0.0000", true)
        );
    }

    #[test]
    fn several_disputes_accumulate_in_held() {
        let mut ledger = ledger_after(&[deposit(1, 1, "1"), deposit(1, 2, "2"), dispute(1, 1)]);

        let result = ledger.apply(dispute(1, 2));

        assert_eq!(result, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("0.0000", "3.0000", "3.0000", false)
        );
    }

    #[rstest]
    #[case::dispute_twice(&[dispute(1, 1)], dispute(1, 1), Rejection::AlreadyDisputed)]
    #[case::resolve_settled(&[], resolve(1, 1), Rejection::NotUnderDispute)]
    #[case::chargeback_settled(&[], chargeback(1, 1), Rejection::NotUnderDispute)]
    #[case::resolve_resolved(&[dispute(1, 1), resolve(1, 1)], resolve(1, 1), Rejection::NotUnderDispute)]
    #[case::chargeback_resolved(&[dispute(1, 1), resolve(1, 1)], chargeback(1, 1), Rejection::NotUnderDispute)]
    #[case::dispute_charged_back(&[dispute(1, 1), chargeback(1, 1)], dispute(1, 1), Rejection::ChargedBack)]
    #[case::resolve_charged_back(&[dispute(1, 1), chargeback(1, 1)], resolve(1, 1), Rejection::ChargedBack)]
    #[case::chargeback_charged_back(&[dispute(1, 1), chargeback(1, 1)], chargeback(1, 1), Rejection::ChargedBack)]
    fn dispute_step_not_allowed_in_current_state_is_rejected_and_changes_nothing(
        #[case] history: &[Transaction],
        #[case] step: Transaction,
        #[case] expected: Rejection,
    ) {
        let mut arrange = vec![deposit(1, 1, "5")];
        arrange.extend_from_slice(history);
        let mut ledger = ledger_after(&arrange);
        let before = (balances(&ledger, 1), ledger.dispute_state(TxId::new(1)));

        let result = ledger.apply(step);

        assert_eq!(result, Err(expected));
        assert_eq!(
            (balances(&ledger, 1), ledger.dispute_state(TxId::new(1))),
            before
        );
    }

    // References

    #[rstest]
    #[case::dispute(dispute(9, 99))]
    #[case::resolve(resolve(9, 99))]
    #[case::chargeback(chargeback(9, 99))]
    fn dispute_step_on_unknown_transaction_is_rejected_and_creates_no_account(
        #[case] step: Transaction,
    ) {
        let mut ledger = Ledger::new();

        let result = ledger.apply(step);

        assert_eq!(result, Err(Rejection::UnknownTx));
        assert_eq!(ledger.accounts().count(), 0);
    }

    #[rstest]
    #[case::dispute(dispute(2, 1))]
    #[case::resolve(resolve(2, 1))]
    #[case::chargeback(chargeback(2, 1))]
    fn dispute_step_by_another_client_is_rejected(#[case] step: Transaction) {
        let mut ledger = ledger_after(&[deposit(1, 1, "5"), deposit(2, 2, "5")]);

        let result = ledger.apply(step);

        assert_eq!(result, Err(Rejection::ClientMismatch));
        assert_eq!(
            balances(&ledger, 1),
            expected("5.0000", "0.0000", "5.0000", false)
        );
    }

    #[test]
    fn ownership_is_checked_before_transaction_type() {
        let mut ledger = ledger_after(&[deposit(1, 1, "5"), withdrawal(1, 2, "1")]);

        let result = ledger.apply(dispute(2, 2));

        assert_eq!(result, Err(Rejection::ClientMismatch));
    }

    #[test]
    fn dispute_of_withdrawal_is_rejected() {
        let mut ledger = ledger_after(&[deposit(1, 1, "5"), withdrawal(1, 2, "1")]);

        let result = ledger.apply(dispute(1, 2));

        assert_eq!(result, Err(Rejection::NotDisputable));
    }

    // Locked accounts

    fn locked_ledger() -> Ledger {
        ledger_after(&[
            deposit(1, 1, "10"),
            deposit(1, 2, "4"),
            dispute(1, 1),
            chargeback(1, 1),
        ])
    }

    #[rstest]
    #[case::deposit(deposit(1, 10, "1"))]
    #[case::withdrawal(withdrawal(1, 10, "1"))]
    fn locked_account_rejects_deposits_and_withdrawals(#[case] movement: Transaction) {
        let mut ledger = locked_ledger();

        let result = ledger.apply(movement);

        assert_eq!(result, Err(Rejection::AccountLocked));
        assert_eq!(
            balances(&ledger, 1),
            expected("4.0000", "0.0000", "4.0000", true)
        );
    }

    #[test]
    fn locked_account_still_processes_disputes_of_earlier_deposits() {
        let mut ledger = locked_ledger();

        let disputed = ledger.apply(dispute(1, 2));
        let charged_back = ledger.apply(chargeback(1, 2));

        assert_eq!(disputed, Ok(()));
        assert_eq!(charged_back, Ok(()));
        assert_eq!(
            balances(&ledger, 1),
            expected("0.0000", "0.0000", "0.0000", true)
        );
    }

    #[test]
    fn deposit_declined_on_locked_account_cannot_be_disputed() {
        let mut ledger = locked_ledger();
        let _ = ledger.apply(deposit(1, 10, "1"));

        let result = ledger.apply(dispute(1, 10));

        assert_eq!(result, Err(Rejection::NotDisputable));
    }

    // Reporting

    #[test]
    fn accounts_are_listed_in_client_order() {
        let ledger = ledger_after(&[deposit(3, 1, "1"), deposit(1, 2, "1"), deposit(2, 3, "1")]);

        let clients: Vec<u16> = ledger.accounts().map(|(client, _)| client.get()).collect();

        assert_eq!(clients, vec![1, 2, 3]);
    }

    // State machine

    #[rstest]
    #[case(
        DisputeState::Settled,
        DisputeAction::Dispute,
        Ok(DisputeState::Disputed)
    )]
    #[case(
        DisputeState::Settled,
        DisputeAction::Resolve,
        Err(Rejection::NotUnderDispute)
    )]
    #[case(
        DisputeState::Settled,
        DisputeAction::Chargeback,
        Err(Rejection::NotUnderDispute)
    )]
    #[case(
        DisputeState::Disputed,
        DisputeAction::Dispute,
        Err(Rejection::AlreadyDisputed)
    )]
    #[case(
        DisputeState::Disputed,
        DisputeAction::Resolve,
        Ok(DisputeState::Resolved)
    )]
    #[case(
        DisputeState::Disputed,
        DisputeAction::Chargeback,
        Ok(DisputeState::ChargedBack)
    )]
    #[case(
        DisputeState::Resolved,
        DisputeAction::Dispute,
        Ok(DisputeState::Disputed)
    )]
    #[case(
        DisputeState::Resolved,
        DisputeAction::Resolve,
        Err(Rejection::NotUnderDispute)
    )]
    #[case(
        DisputeState::Resolved,
        DisputeAction::Chargeback,
        Err(Rejection::NotUnderDispute)
    )]
    #[case(
        DisputeState::ChargedBack,
        DisputeAction::Dispute,
        Err(Rejection::ChargedBack)
    )]
    #[case(
        DisputeState::ChargedBack,
        DisputeAction::Resolve,
        Err(Rejection::ChargedBack)
    )]
    #[case(
        DisputeState::ChargedBack,
        DisputeAction::Chargeback,
        Err(Rejection::ChargedBack)
    )]
    fn dispute_state_allows_only_lifecycle_transitions(
        #[case] state: DisputeState,
        #[case] action: DisputeAction,
        #[case] expected: Result<DisputeState, Rejection>,
    ) {
        assert_eq!(state.next(action), expected);
    }
}
