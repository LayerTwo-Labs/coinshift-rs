//! Parent-chain observation, behind one trait.
//!
//! Coinshift never builds or broadcasts an L1 transaction. It only *watches* the
//! parent chain to answer a single question per swap: did the recipient address
//! receive the expected amount, and is that payment final enough and recent
//! enough to accept? [`ParentChainClient`] is that question, expressed so a
//! non-Bitcoin chain can answer it too.
//!
//! # Asynchrony
//!
//! The trait is `async` and dyn-compatible via `async_trait`, because L1
//! observation runs in its own task ([`crate::node::SwapObserver`]) rather than
//! on the block-connect path. Nothing here may block: an endpoint that takes ten
//! seconds to answer must not stall the runtime, the GUI, or block processing.

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
#[async_trait::async_trait]
pub trait ParentChainClient: Send + Sync {
    /// Which chain this endpoint is actually serving.
    async fn identify(&self) -> Result<ChainIdentity, Error>;

    /// Current tip, in the chain's own age unit (block height, slot, …).
    async fn tip(&self) -> Result<u64, Error>;

    /// Transactions that pay `query`'s address `query`'s amount.
    ///
    /// Callers still apply their own age and finality rules; this only filters
    /// on the payment itself.
    async fn find_payments(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error>;

    /// Re-read one already-known transaction, cheaply.
    ///
    /// Returns `Ok(None)` when the chain has no record of `txid`.
    async fn get_payment(
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
