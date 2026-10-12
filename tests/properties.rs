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
    Account, Amount, ClientId, DisputeState, Ledger, Rejection, Transaction, TxId,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;

/// Few clients, so accounts are shared and clients interleave; includes both edges.
const CLIENTS: [u16; 4] = [0, 1, 2, u16::MAX];
/// Never handed out as a fresh ID by `build`.
const UNKNOWN_TX: u32 = u32::MAX - 1;

#[derive(Debug, Clone, Copy)]
enum Step {
    Dispute,
    Resolve,
    Chargeback,
}

#[derive(Debug, Clone)]
enum Op {
    Deposit {
        client: usize,
        units: i64,
    },
    Withdrawal {
        client: usize,
        units: i64,
    },
    /// A dispute step on an earlier movement, sometimes naming the wrong client.
    OnEarlier {
        step: Step,
        target: Index,
        wrong_client: bool,
    },
    /// A dispute step on an ID that was never used.
    OnUnknown {
        step: Step,
        client: usize,
    },
    /// A deposit reusing an earlier movement's ID.
    Reuse {
        target: Index,
        client: usize,
        units: i64,
    },
}

fn units() -> impl Strategy<Value = i64> {
    prop_oneof![
        8 => 1..=1_000_000_i64,        // 0.0001 to 100.0000
        1 => Just(1_i64),              // the smallest amount
        1 => (i64::MAX - 10_000)..=i64::MAX, // near the maximum, to reach overflow
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![2 => Just(Step::Dispute), 1 => Just(Step::Resolve), 1 => Just(Step::Chargeback)]
}

fn op() -> impl Strategy<Value = Op> {
    let client = 0..CLIENTS.len();
    prop_oneof![
        4 => (client.clone(), units()).prop_map(|(client, units)| Op::Deposit { client, units }),
        2 => (client.clone(), units()).prop_map(|(client, units)| Op::Withdrawal { client, units }),
        6 => (step(), any::<Index>(), prop::bool::weighted(0.1))
            .prop_map(|(step, target, wrong_client)| Op::OnEarlier { step, target, wrong_client }),
        1 => (step(), client.clone()).prop_map(|(step, client)| Op::OnUnknown { step, client }),
        1 => (any::<Index>(), client, units())
            .prop_map(|(target, client, units)| Op::Reuse { target, client, units }),
    ]
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op(), 1..80)
}

fn dispute_step(step: Step, client: u16, tx: u32) -> Transaction {
    let (client, tx) = (ClientId::new(client), TxId::new(tx));
    match step {
        Step::Dispute => Transaction::Dispute { client, tx },
        Step::Resolve => Transaction::Resolve { client, tx },
        Step::Chargeback => Transaction::Chargeback { client, tx },
    }
}

/// Turns generated operations into transactions. Fresh IDs start with the extremes.
fn build(ops: &[Op]) -> Vec<Transaction> {
    let mut fresh_ids = [0, u32::MAX].into_iter().chain(1..);
    let mut movements: Vec<(u32, u16)> = Vec::new();
    let amount = |units: i64| Amount::from_units(units).unwrap();

    ops.iter()
        .map(|op| match *op {
            Op::Deposit { client, units } | Op::Withdrawal { client, units } => {
                let tx = fresh_ids.next().unwrap();
                let client = CLIENTS[client];
                movements.push((tx, client));
                let (client, tx, amount) = (ClientId::new(client), TxId::new(tx), amount(units));
                if matches!(op, Op::Deposit { .. }) {
                    Transaction::Deposit { client, tx, amount }
                } else {
                    Transaction::Withdrawal { client, tx, amount }
                }
            }
            Op::OnEarlier {
                step,
                target,
                wrong_client,
            } => {
                if movements.is_empty() {
                    return dispute_step(step, CLIENTS[0], UNKNOWN_TX);
                }
                let (tx, owner) = movements[target.index(movements.len())];
                let client = if wrong_client {
                    other_client(owner)
                } else {
                    owner
                };
                dispute_step(step, client, tx)
            }
            Op::OnUnknown { step, client } => dispute_step(step, CLIENTS[client], UNKNOWN_TX),
            Op::Reuse {
                target,
                client,
                units,
            } => {
                let tx = if movements.is_empty() {
                    fresh_ids.next().unwrap()
                } else {
                    movements[target.index(movements.len())].0
                };
                Transaction::Deposit {
                    client: ClientId::new(CLIENTS[client]),
                    tx: TxId::new(tx),
                    amount: amount(units),
                }
            }
        })
        .collect()
}

fn other_client(client: u16) -> u16 {
    let position = CLIENTS.iter().position(|c| *c == client).unwrap();
    CLIENTS[(position + 1) % CLIENTS.len()]
}

fn snapshot(ledger: &Ledger) -> BTreeMap<ClientId, Account> {
    ledger
        .accounts()
        .map(|(client, account)| (client, account.clone()))
        .collect()
}

/// Renders transactions as CSV. `blank_after[i]` blank or whitespace-only lines follow
/// row `i` (cycling), and `crlf` selects the line ending, so line tracking is exercised
/// as well as the ledger.
fn to_csv(transactions: &[Transaction], crlf: bool, blank_after: &[usize]) -> String {
    let newline = if crlf { "\r\n" } else { "\n" };
    let mut csv = format!("type,client,tx,amount{newline}");
    for (index, transaction) in transactions.iter().enumerate() {
        let amount = match transaction {
            Transaction::Deposit { amount, .. } | Transaction::Withdrawal { amount, .. } => {
                amount.to_string()
            }
            _ => String::new(),
        };
        csv.push_str(&format!(
            "{},{},{},{amount}{newline}",
            transaction.kind(),
            transaction.client(),
            transaction.tx()
        ));
        let blanks = blank_after
            .get(index % blank_after.len().max(1))
            .copied()
            .unwrap_or(0);
        for blank in 0..blanks {
            csv.push_str(if blank % 2 == 0 { "" } else { " \t" });
            csv.push_str(newline);
        }
    }
    csv
}

proptest! {
    #[test]
    fn ledger_invariants_hold_after_every_transaction(ops in ops()) {
        let mut ledger = Ledger::new();
        let mut charged_back = HashSet::new();
        let mut deposits: BTreeMap<TxId, Amount> = BTreeMap::new();

        for transaction in build(&ops) {
            let before = snapshot(&ledger);

            let result = ledger.apply(transaction);

            let after = snapshot(&ledger);
            for (client, account) in &after {
                prop_assert_eq!(
                    i128::from(account.total().units()),
                    i128::from(account.available().units()) + i128::from(account.held().units()),
                    "total != available + held for client {}", client
                );
                prop_assert!(account.held().units() >= 0, "negative held for client {}", client);
                if before.get(client).is_some_and(Account::is_locked) {
                    prop_assert!(account.is_locked(), "client {} unlocked", client);
                }
            }
            for tx in &charged_back {
                prop_assert_eq!(ledger.dispute_state(*tx), Some(DisputeState::ChargedBack));
            }

            let is_movement = matches!(
                transaction,
                Transaction::Deposit { .. } | Transaction::Withdrawal { .. }
            );
            match result {
                Err(_) => {
                    // Balances and lock status of existing accounts never change.
                    for (client, account) in &before {
                        prop_assert_eq!(after.get(client), Some(account));
                    }
                    // Only a movement may add an account, and it starts empty.
                    let added: Vec<_> = after.keys().filter(|c| !before.contains_key(c)).collect();
                    prop_assert!(added.is_empty() || is_movement);
                    for client in added {
                        prop_assert_eq!(&after[client], &Account::default());
                    }
                }
                Ok(()) => {
                    if !is_movement {
                        prop_assert!(before.contains_key(&transaction.client()));
                    }
                    if matches!(transaction, Transaction::Dispute { .. } | Transaction::Resolve { .. }) {
                        let client = transaction.client();
                        prop_assert_eq!(after[&client].total(), before[&client].total());
                    }
                    match transaction {
                        Transaction::Deposit { tx, amount, .. } => {
                            deposits.insert(tx, amount);
                        }
                        Transaction::Chargeback { client, tx } => {
                            // Total drops by exactly the deposit, and the account locks.
                            let amount = i128::from(deposits[&tx].units());
                            prop_assert_eq!(
                                i128::from(after[&client].total().units()),
                                i128::from(before[&client].total().units()) - amount
                            );
                            prop_assert!(after[&client].is_locked());
                            charged_back.insert(tx);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn without_disputes_totals_equal_accepted_deposits_minus_withdrawals(
        movements in prop::collection::vec((any::<bool>(), 0..CLIENTS.len(), units()), 0..60)
    ) {
        let ops: Vec<Op> = movements
            .into_iter()
            .map(|(is_deposit, client, units)| {
                if is_deposit { Op::Deposit { client, units } } else { Op::Withdrawal { client, units } }
            })
            .collect();
        let mut ledger = Ledger::new();
        let mut expected: BTreeMap<ClientId, i128> = BTreeMap::new();

        for transaction in build(&ops) {
            if ledger.apply(transaction).is_ok() {
                let total = expected.entry(transaction.client()).or_default();
                if let Transaction::Deposit { amount, .. } = transaction {
                    *total += i128::from(amount.units());
                } else if let Transaction::Withdrawal { amount, .. } = transaction {
                    *total -= i128::from(amount.units());
                }
            }
        }

        for (client, account) in ledger.accounts() {
            prop_assert_eq!(i128::from(account.total().units()), expected.get(&client).copied().unwrap_or(0));
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

/// Guards the generators: if a change stops them reaching a rule, this fails.
#[test]
fn generators_reach_every_rejection_and_dispute_transition() {
    let mut runner = TestRunner::deterministic();
    let mut codes = HashSet::new();
    let mut transitions = HashSet::new();

    for _ in 0..2_000 {
        let ops = ops().new_tree(&mut runner).unwrap().current();
        let mut ledger = Ledger::new();
        for transaction in build(&ops) {
            let before = ledger.dispute_state(transaction.tx());
            match ledger.apply(transaction) {
                Err(rejection) => {
                    codes.insert(rejection.code());
                }
                Ok(()) => {
                    if let (Some(from), Some(to)) = (before, ledger.dispute_state(transaction.tx()))
                    {
                        transitions.insert((from, to));
                    }
                }
            }
        }
    }

    let missing: Vec<_> = Rejection::ALL
        .iter()
        .map(|r| r.code())
        .filter(|c| !codes.contains(c))
        .collect();
    assert!(missing.is_empty(), "codes never reached: {missing:?}");
    for transition in [
        (DisputeState::Settled, DisputeState::Disputed),
        (DisputeState::Disputed, DisputeState::Resolved),
        (DisputeState::Resolved, DisputeState::Disputed),
        (DisputeState::Disputed, DisputeState::ChargedBack),
    ] {
        assert!(
            transitions.contains(&transition),
            "transition never reached: {transition:?}"
        );
    }
}
