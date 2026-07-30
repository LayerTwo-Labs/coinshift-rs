//! Parent-chain observation, behind one trait.
//!
//! Coinshift never builds or broadcasts an L1 transaction. It only *watches* the
//! parent chain to answer a single question per swap: did the recipient address
//! receive the expected amount, and is that payment final enough and recent
//! enough to accept? [`ParentChainClient`] is that question, expressed so a
//! non-Bitcoin chain can answer it too.
//!
//! # Why this is synchronous
//!
//! The only caller today is `state::two_way_peg_data::query_and_update_swap`,
//! which runs inside an LMDB write transaction on a synchronous call path, so an
//! `async` trait could not be awaited there and blocking on the runtime from
//! inside it would panic. Phase 4 of `docs/PARENT_CHAIN_ROADMAP.md` moves L1
//! observation into its own task, and the trait becomes `async` with it — a
//! mechanical change, since no signature other than the `async`/`.await` pair
//! differs.

pub mod bitcoin_core;
#[cfg(any(test, feature = "test-utils"))]
pub mod mock;
pub mod types;

use thiserror::Error;

use crate::{l1::config::L1ChainConfig, types::ParentChainType};

pub use bitcoin_core::BitcoinCoreClient;
pub use types::{ChainIdentity, L1Payment, PaymentQuery};

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Invalid response format")]
    InvalidResponse,
    #[error("Transaction not found")]
    TransactionNotFound,
}

/// Everything the swap logic needs from a parent chain.
pub trait ParentChainClient: Send + Sync {
    /// Which chain this endpoint is actually serving.
    fn identify(&self) -> Result<ChainIdentity, Error>;

    /// Current tip, in the chain's own age unit (block height, slot, …).
    fn tip(&self) -> Result<u64, Error>;

    /// Transactions that pay `query`'s address `query`'s amount.
    ///
    /// Callers still apply their own age and finality rules; this only filters
    /// on the payment itself.
    fn find_payments(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error>;

    /// Re-read one already-known transaction, cheaply.
    ///
    /// Returns `Ok(None)` when the chain has no record of `txid`.
    fn get_payment(
        &self,
        txid: &crate::types::SwapTxId,
        query: &PaymentQuery,
    ) -> Result<Option<L1Payment>, Error>;
}

/// Resolves the client to use for a parent chain, or `None` when that chain
/// cannot currently be observed — see [`crate::l1::L1Registry::verified_client`].
pub type ClientGetter<'a> =
    &'a dyn Fn(
        ParentChainType,
    ) -> Option<std::sync::Arc<dyn ParentChainClient>>;

/// Build the client for `chain`.
///
/// Every chain is Bitcoin Core-compatible today; this is the seam where a
/// Solana adapter is selected instead.
pub fn client_for(
    chain: ParentChainType,
    config: &L1ChainConfig,
) -> std::sync::Arc<dyn ParentChainClient> {
    match chain {
        ParentChainType::BTC
        | ParentChainType::BCH
        | ParentChainType::LTC
        | ParentChainType::Signet
        | ParentChainType::Regtest => {
            std::sync::Arc::new(BitcoinCoreClient::new(chain, config.clone()))
        }
    }
}
