//! Property tests: ledger invariants over generated transaction sequences, plus
//! robustness of parsing and streaming against arbitrary input.
//!
//! Generators are shaped so dispute steps usually hit real transactions: movements get
//! fresh IDs, and dispute, resolve and chargeback rows pick an earlier movement. A small
//! share of rows are deliberately wrong (unknown ID, wrong client, reused ID), and some
//! amounts and IDs sit at the edges of their ranges. A guard test checks that every
//! ledger reason code and dispute transition is actually reached.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should stop with a panic and a clear message"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::default_numeric_fallback,
    reason = "test data uses small literal values, where overflow and inferred types cannot matter"
)]

mod common;

use std::collections::{BTreeMap, HashSet};
use std::io;

use payments_engine::domain::{
    Account, Amount, Balance, ClientId, DisputeState, Ledger, Rejection, Transaction, TxId,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::strategy::ValueTree;
use proptest::test_runner::{TestCaseError, TestRunner};

/// Few clients, so accounts are shared and clients interleave; includes both edges.
const CLIENTS: [u16; 4] = [0, 1, 2, u16::MAX];
/// Never handed out as a fresh ID by `Builder`.
const UNKNOWN_TX: u32 = u32::MAX - 1;

// ---------------------------------------------------------------------------------
// Strategies: what a generated operation looks like
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Step {
    Dispute,
    Resolve,
    Chargeback,
}

#[derive(Debug, Clone)]
enum Op {
    Deposit {
        client: u16,
        amount: Amount,
    },
    Withdrawal {
        client: u16,
        amount: Amount,
    },
    /// A dispute step on an earlier movement, sometimes naming the wrong client.
    DisputeEarlier {
        step: Step,
        target: Index,
        wrong_client: bool,
    },
    /// A dispute step on an ID that was never used.
    DisputeUnknown {
        step: Step,
        client: u16,
    },
    /// A deposit reusing an earlier movement's ID.
    ReuseId {
        target: Index,
        client: u16,
        amount: Amount,
    },
}

fn clients() -> impl Strategy<Value = u16> {
    prop::sample::select(CLIENTS.to_vec())
}

fn amounts() -> impl Strategy<Value = Amount> {
    prop_oneof![
        8 => 1..=1_000_000_i64,               // 0.0001 to 100.0000
        1 => Just(1_i64),                     // the smallest amount
        1 => (i64::MAX - 10_000)..=i64::MAX,  // near the maximum, to reach overflow
    ]
    .prop_map(|units| Amount::from_units(units).unwrap())
}

fn steps() -> impl Strategy<Value = Step> {
    prop_oneof![2 => Just(Step::Dispute), 1 => Just(Step::Resolve), 1 => Just(Step::Chargeback)]
}

/// A deposit (weight 2) or a withdrawal (weight 1).
fn movement() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (clients(), amounts()).prop_map(|(client, amount)| Op::Deposit { client, amount }),
        1 => (clients(), amounts()).prop_map(|(client, amount)| Op::Withdrawal { client, amount }),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => movement(),
        6 => (steps(), any::<Index>(), prop::bool::weighted(0.1)).prop_map(
            |(step, target, wrong_client)| Op::DisputeEarlier { step, target, wrong_client }
        ),
        1 => (steps(), clients()).prop_map(|(step, client)| Op::DisputeUnknown { step, client }),
        1 => (any::<Index>(), clients(), amounts())
            .prop_map(|(target, client, amount)| Op::ReuseId { target, client, amount }),
    ]
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op(), 1..80)
}

// ---------------------------------------------------------------------------------
// Building transactions from operations
// ---------------------------------------------------------------------------------

/// Turns generated operations into transactions, handing out fresh IDs and keeping
/// earlier movements so dispute steps can target them.
#[derive(Default)]
struct Builder {
    ids_issued: u32,
    /// `(tx, owner)` of every deposit and withdrawal so far.
    movements: Vec<(u32, u16)>,
}

impl Builder {
    fn build(&mut self, op: &Op) -> Transaction {
        match *op {
            Op::Deposit { client, amount } => {
                let tx = self.new_movement(client);
                Transaction::Deposit {
                    client: ClientId::new(client),
                    tx,
                    amount,
                }
            }
            Op::Withdrawal { client, amount } => {
                let tx = self.new_movement(client);
                Transaction::Withdrawal {
                    client: ClientId::new(client),
                    tx,
                    amount,
                }
            }
            Op::DisputeEarlier {
                step,
                target,
                wrong_client,
            } => self.dispute_earlier(step, target, wrong_client),
            Op::DisputeUnknown { step, client } => dispute_step(step, client, UNKNOWN_TX),
            Op::ReuseId {
                target,
                client,
                amount,
            } => Transaction::Deposit {
                client: ClientId::new(client),
                tx: TxId::new(self.earlier_tx(target).unwrap_or_else(|| self.fresh_id())),
                amount,
            },
        }
    }

    /// Fresh IDs start with the extremes, 0 and `u32::MAX`, then count up from 1.
    fn fresh_id(&mut self) -> u32 {
        let id = match self.ids_issued {
            0 => 0,
            1 => u32::MAX,
            n => n - 1,
        };
        self.ids_issued += 1;
        id
    }

    fn new_movement(&mut self, client: u16) -> TxId {
        let tx = self.fresh_id();
        self.movements.push((tx, client));
        TxId::new(tx)
    }

    fn earlier_movement(&self, target: Index) -> Option<(u32, u16)> {
        (!self.movements.is_empty()).then(|| self.movements[target.index(self.movements.len())])
    }

    fn earlier_tx(&self, target: Index) -> Option<u32> {
        self.earlier_movement(target).map(|(tx, _)| tx)
    }

    fn dispute_earlier(&self, step: Step, target: Index, wrong_client: bool) -> Transaction {
        let Some((tx, owner)) = self.earlier_movement(target) else {
            return dispute_step(step, CLIENTS[0], UNKNOWN_TX);
        };
        let client = if wrong_client {
            other_client(owner)
        } else {
            owner
        };
        dispute_step(step, client, tx)
    }
}

fn build(ops: &[Op]) -> Vec<Transaction> {
    let mut builder = Builder::default();
    ops.iter().map(|op| builder.build(op)).collect()
}

fn dispute_step(step: Step, client: u16, tx: u32) -> Transaction {
    let (client, tx) = (ClientId::new(client), TxId::new(tx));
    match step {
        Step::Dispute => Transaction::Dispute { client, tx },
        Step::Resolve => Transaction::Resolve { client, tx },
        Step::Chargeback => Transaction::Chargeback { client, tx },
    }
}

fn other_client(client: u16) -> u16 {
    let position = CLIENTS.iter().position(|c| *c == client).unwrap();
    CLIENTS[(position + 1) % CLIENTS.len()]
}

/// Renders transactions as CSV. After each row come `blank_after` blank or
/// whitespace-only lines (cycling through the list), and `crlf` selects the line
/// ending, so line tracking is exercised as well as the ledger.
fn to_csv(transactions: &[Transaction], crlf: bool, blank_after: &[usize]) -> String {
    let newline = if crlf { "\r\n" } else { "\n" };
    let mut blank_counts = blank_after.iter().copied().cycle();
    let mut csv = format!("type,client,tx,amount{newline}");
    for transaction in transactions {
        csv.push_str(&csv_row(transaction));
        csv.push_str(newline);
        for blank in 0..blank_counts.next().unwrap_or(0) {
            csv.push_str(if blank % 2 == 0 { "" } else { " \t" });
            csv.push_str(newline);
        }
    }
    csv
}

fn csv_row(transaction: &Transaction) -> String {
    let amount = match transaction {
        Transaction::Deposit { amount, .. } | Transaction::Withdrawal { amount, .. } => {
            amount.to_string()
        }
        _ => String::new(),
    };
    format!(
        "{},{},{},{amount}",
        transaction.kind(),
        transaction.client(),
        transaction.tx()
    )
}

// ---------------------------------------------------------------------------------
// Invariant checks
// ---------------------------------------------------------------------------------

type Snapshot = BTreeMap<ClientId, Account>;
type Check = Result<(), TestCaseError>;

fn snapshot(ledger: &Ledger) -> Snapshot {
    ledger
        .accounts()
        .map(|(client, account)| (client, account.clone()))
        .collect()
}

/// A balance widened so sums in assertions cannot overflow.
fn wide(balance: Balance) -> i128 {
    i128::from(balance.units())
}

fn is_movement(transaction: Transaction) -> bool {
    matches!(
        transaction,
        Transaction::Deposit { .. } | Transaction::Withdrawal { .. }
    )
}

/// Holds for every account after every transaction, accepted or not.
fn check_account_invariants(before: &Snapshot, after: &Snapshot) -> Check {
    for (client, account) in after {
        prop_assert_eq!(
            wide(account.total()),
            wide(account.available()) + wide(account.held()),
            "total != available + held for client {}",
            client
        );
        prop_assert!(
            account.held().units() >= 0,
            "negative held for client {}",
            client
        );
        if before.get(client).is_some_and(Account::is_locked) {
            prop_assert!(account.is_locked(), "client {} unlocked", client);
        }
    }
    Ok(())
}

/// A rejected transaction changes no existing account. Only a deposit or withdrawal
/// may add an account, and that account is empty.
fn check_nothing_changed(before: &Snapshot, after: &Snapshot, transaction: Transaction) -> Check {
    for (client, account) in before {
        prop_assert_eq!(after.get(client), Some(account));
    }
    let added: Vec<_> = after
        .keys()
        .filter(|client| !before.contains_key(client))
        .collect();
    prop_assert!(added.is_empty() || is_movement(transaction));
    for client in added {
        prop_assert_eq!(&after[client], &Account::default());
    }
    Ok(())
}

/// What the test remembers across transactions.
#[derive(Default)]
struct History {
    /// Amounts of accepted deposits.
    deposits: BTreeMap<TxId, Amount>,
    charged_back: HashSet<TxId>,
}

impl History {
    /// The effect of an accepted transaction on its client's account.
    fn check_accepted(
        &mut self,
        transaction: Transaction,
        before: &Snapshot,
        after: &Snapshot,
    ) -> Check {
        let client = transaction.client();
        match transaction {
            Transaction::Deposit { tx, amount, .. } => {
                self.deposits.insert(tx, amount);
            }
            Transaction::Withdrawal { .. } => {}
            Transaction::Dispute { .. } | Transaction::Resolve { .. } => {
                prop_assert!(before.contains_key(&client));
                prop_assert_eq!(after[&client].total(), before[&client].total());
            }
            Transaction::Chargeback { tx, .. } => {
                // Total drops by exactly the deposit, and the account locks.
                prop_assert!(before.contains_key(&client));
                let amount = i128::from(self.deposits[&tx].units());
                prop_assert_eq!(
                    wide(after[&client].total()),
                    wide(before[&client].total()) - amount
                );
                prop_assert!(after[&client].is_locked());
                self.charged_back.insert(tx);
            }
        }
        Ok(())
    }

    /// A charged-back deposit never changes state again.
    fn check_chargebacks_are_final(&self, ledger: &Ledger) -> Check {
        for tx in &self.charged_back {
            prop_assert_eq!(ledger.dispute_state(*tx), Some(DisputeState::ChargedBack));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------

proptest! {
    #[test]
    fn ledger_invariants_hold_after_every_transaction(ops in ops()) {
        let mut ledger = Ledger::new();
        let mut history = History::default();

        for transaction in build(&ops) {
            let before = snapshot(&ledger);
            let result = ledger.apply(transaction);
            let after = snapshot(&ledger);

            check_account_invariants(&before, &after)?;
            match result {
                Err(_) => check_nothing_changed(&before, &after, transaction)?,
                Ok(()) => history.check_accepted(transaction, &before, &after)?,
            }
            history.check_chargebacks_are_final(&ledger)?;
        }
    }

    #[test]
    fn without_disputes_totals_equal_accepted_deposits_minus_withdrawals(
        ops in prop::collection::vec(movement(), 0..60)
    ) {
        let mut ledger = Ledger::new();
        let mut expected: BTreeMap<ClientId, i128> = BTreeMap::new();

        for transaction in build(&ops) {
            if ledger.apply(transaction).is_err() {
                continue;
            }
            let total = expected.entry(transaction.client()).or_default();
            match transaction {
                Transaction::Deposit { amount, .. } => *total += i128::from(amount.units()),
                Transaction::Withdrawal { amount, .. } => *total -= i128::from(amount.units()),
                _ => {} // `movement()` generates only deposits and withdrawals
            }
        }

        for (client, account) in ledger.accounts() {
            prop_assert_eq!(wide(account.total()), expected.get(&client).copied().unwrap_or(0));
        }
    }

    #[test]
    fn amount_display_round_trips_through_parse(units in 1..=i64::MAX) {
        let amount = Amount::from_units(units).unwrap();

        prop_assert_eq!(amount.to_string().parse::<Amount>(), Ok(amount));
    }

    #[test]
    fn amount_parser_never_panics(text in prop_oneof![".*", "[0-9.+eE, -]{0,40}"]) {
        let _ = text.parse::<Amount>();
    }

    #[test]
    fn run_never_panics_on_arbitrary_bytes(
        body in prop::collection::vec(any::<u8>(), 0..2048), // past the 1024-byte line limit
        with_header in any::<bool>(),
    ) {
        let mut input = if with_header { b"type,client,tx,amount\n".to_vec() } else { Vec::new() };
        input.extend_from_slice(&body);

        let _ = payments_engine::run(input.as_slice(), io::sink(), io::sink());
    }

    #[test]
    fn any_chunking_gives_the_same_result(
        ops in ops(),
        chunk in 1..64_usize,
        crlf in any::<bool>(),
        blank_after in prop::collection::vec(0..3_usize, 0..8),
    ) {
        let csv = to_csv(&build(&ops), crlf, &blank_after);
        let expected = common::run_bytes(csv.as_bytes());
        let mut output = Vec::new();
        let mut diagnostics = Vec::new();

        payments_engine::run(
            common::ChunkedReader::new(csv.as_bytes(), chunk),
            &mut output,
            &mut diagnostics,
        )
        .unwrap();

        prop_assert_eq!(
            (String::from_utf8(output).unwrap(), String::from_utf8(diagnostics).unwrap()),
            expected
        );
    }
}

// ---------------------------------------------------------------------------------
// Guard test for the generators
// ---------------------------------------------------------------------------------

/// Every way a dispute step can move a deposit between states.
const REQUIRED_TRANSITIONS: [(DisputeState, DisputeState); 4] = [
    (DisputeState::Settled, DisputeState::Disputed),
    (DisputeState::Disputed, DisputeState::Resolved),
    (DisputeState::Resolved, DisputeState::Disputed),
    (DisputeState::Disputed, DisputeState::ChargedBack),
];

/// What a batch of generated sequences reached.
#[derive(Default)]
struct Coverage {
    codes: HashSet<&'static str>,
    transitions: HashSet<(DisputeState, DisputeState)>,
}

impl Coverage {
    fn record(&mut self, ledger: &mut Ledger, transaction: Transaction) {
        let state_before = ledger.dispute_state(transaction.tx());
        match ledger.apply(transaction) {
            Err(rejection) => {
                self.codes.insert(rejection.code());
            }
            Ok(()) => {
                let state_after = ledger.dispute_state(transaction.tx());
                if let (Some(from), Some(to)) = (state_before, state_after) {
                    self.transitions.insert((from, to));
                }
            }
        }
    }
}

/// Guards the generators: if a change stops them reaching a rule, this fails.
#[test]
fn generators_reach_every_rejection_and_dispute_transition() {
    let mut runner = TestRunner::deterministic();
    let mut coverage = Coverage::default();

    for _ in 0..2_000 {
        let ops = ops().new_tree(&mut runner).unwrap().current();
        let mut ledger = Ledger::new();
        for transaction in build(&ops) {
            coverage.record(&mut ledger, transaction);
        }
    }

    let missing_codes: Vec<_> = Rejection::ALL
        .iter()
        .map(|rejection| rejection.code())
        .filter(|code| !coverage.codes.contains(code))
        .collect();
    assert!(
        missing_codes.is_empty(),
        "codes never reached: {missing_codes:?}"
    );
    for transition in REQUIRED_TRANSITIONS {
        assert!(
            coverage.transitions.contains(&transition),
            "transition never reached: {transition:?}"
        );
    }
}
