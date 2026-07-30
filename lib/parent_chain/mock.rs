//! A scripted [`ParentChainClient`] for tests.
//!
//! Before this existed there was no way to exercise the swap detection logic at
//! all: the unit tests never touched a parent chain, and the integration tests
//! fabricate txids and feed them straight to `update_swap_l1_txid`, bypassing
//! the RPC path entirely. Everything in `query_and_update_swap` — address and
//! amount matching, the age cutoff, first-detection versus confirmation update,
//! and L1 txid uniqueness — was therefore untested.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::{ChainIdentity, Error, L1Payment, ParentChainClient, PaymentQuery};
use crate::types::{ParentChainType, SwapTxId};

/// What the mock should do when asked.
#[derive(Clone, Debug, Default)]
pub enum MockBehaviour {
    /// Report no matching payment.
    #[default]
    NoPayments,
    /// Report these payments, filtered to those matching the query.
    Payments(Vec<L1Payment>),
    /// Fail the way an unreachable node would.
    Unreachable,
}

/// A [`ParentChainClient`] that returns scripted results and counts its calls.
#[derive(Clone, Debug)]
pub struct MockParentChainClient {
    chain: ParentChainType,
    behaviour: MockBehaviour,
    tip: u64,
    calls: Arc<AtomicUsize>,
}

impl MockParentChainClient {
    pub fn new(chain: ParentChainType, behaviour: MockBehaviour) -> Self {
        Self {
            chain,
            behaviour,
            tip: 0,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// How many trait methods have been called on this client.
    ///
    /// Shared across clones, so a getter closure can hand out copies and the
    /// test can still assert on the total.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn record(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    fn payments_for(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error> {
        match &self.behaviour {
            MockBehaviour::NoPayments => Ok(Vec::new()),
            MockBehaviour::Unreachable => {
                Err(Error::Rpc("mock endpoint is unreachable".to_string()))
            }
            MockBehaviour::Payments(payments) => Ok(payments
                .iter()
                .filter(|payment| {
                    payment.matches_query && payment.amount == query.amount
                })
                .cloned()
                .collect()),
        }
    }
}

impl ParentChainClient for MockParentChainClient {
    fn identify(&self) -> Result<ChainIdentity, Error> {
        self.record();
        Ok(ChainIdentity {
            chain: self.chain,
            raw: "mock".to_string(),
        })
    }

    fn tip(&self) -> Result<u64, Error> {
        self.record();
        Ok(self.tip)
    }

    fn find_payments(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error> {
        self.record();
        self.payments_for(query)
    }

    fn get_payment(
        &self,
        txid: &SwapTxId,
        query: &PaymentQuery,
    ) -> Result<Option<L1Payment>, Error> {
        self.record();
        Ok(self
            .payments_for(query)?
            .into_iter()
            .find(|payment| payment.txid == *txid))
    }
}
