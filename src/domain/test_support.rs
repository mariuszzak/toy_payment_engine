//! Transaction builders shared by unit tests across the crate.

use super::{Amount, ClientId, Transaction, TxId};

pub(crate) fn amount(text: &str) -> Amount {
    text.parse().unwrap()
}

pub(crate) fn deposit(client: u16, tx: u32, value: &str) -> Transaction {
    Transaction::Deposit {
        client: ClientId::new(client),
        tx: TxId::new(tx),
        amount: amount(value),
    }
}

pub(crate) fn withdrawal(client: u16, tx: u32, value: &str) -> Transaction {
    Transaction::Withdrawal {
        client: ClientId::new(client),
        tx: TxId::new(tx),
        amount: amount(value),
    }
}

pub(crate) fn dispute(client: u16, tx: u32) -> Transaction {
    Transaction::Dispute {
        client: ClientId::new(client),
        tx: TxId::new(tx),
    }
}

pub(crate) fn resolve(client: u16, tx: u32) -> Transaction {
    Transaction::Resolve {
        client: ClientId::new(client),
        tx: TxId::new(tx),
    }
}

pub(crate) fn chargeback(client: u16, tx: u32) -> Transaction {
    Transaction::Chargeback {
        client: ClientId::new(client),
        tx: TxId::new(tx),
    }
}
