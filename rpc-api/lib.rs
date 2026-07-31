//! RPC API

#![allow(clippy::too_many_arguments)]

use std::net::SocketAddr;

use coinshift::{
    l1::L1ChainHealth,
    net::Peer,
    types::{
        Address, MerkleRoot, OutPoint, Output, OutputContent, ParentChainType,
        PointedOutput, Swap, SwapId, SwapState, Txid, WithdrawalBundle,
        schema as coinshift_schema,
    },
    wallet::Balance,
};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use l2l_openapi::open_api;

mod schema;

#[open_api(ref_schemas[
    Address, L1ChainHealth, MerkleRoot, OutPoint, Output, OutputContent,
    ParentChainType, Swap, SwapId, SwapState, Txid, schema::BitcoinTxid,
    coinshift_schema::BitcoinAddr, coinshift_schema::BitcoinOutPoint,
])]
#[rpc(client, server)]
pub trait Rpc {
    /// Get balance in sats
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "balance")]
    async fn balance(&self) -> RpcResult<Balance>;

    /// Connect to a peer
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "connect_peer")]
    async fn connect_peer(
        &self,
        #[open_api_method_arg(schema(
            PartialSchema = "coinshift_schema::SocketAddr"
        ))]
        addr: SocketAddr,
    ) -> RpcResult<()>;

    /// Deposit to address
    #[open_api_method(output_schema(PartialSchema = "schema::BitcoinTxid"))]
    #[method(name = "create_deposit")]
    async fn create_deposit(
        &self,
        address: Address,
        value_sats: u64,
        fee_sats: u64,
    ) -> RpcResult<bitcoin::Txid>;

    /// Format a deposit address
    #[method(name = "format_deposit_address")]
    async fn format_deposit_address(
        &self,
        address: Address,
    ) -> RpcResult<String>;

    /// Delete peer from known_peers DB.
    /// Connections to the peer are not terminated.
    #[method(name = "forget_peer")]
    async fn forget_peer(
        &self,
        #[open_api_method_arg(schema(
            PartialSchema = "coinshift_schema::SocketAddr"
        ))]
        addr: SocketAddr,
    ) -> RpcResult<()>;

    /// Generate a mnemonic seed phrase
    #[method(name = "generate_mnemonic")]
    async fn generate_mnemonic(&self) -> RpcResult<String>;

    /// Get the block with specified block hash, if it exists
    #[method(name = "get_block")]
    async fn get_block(
        &self,
        block_hash: coinshift::types::BlockHash,
    ) -> RpcResult<Option<coinshift::types::Block>>;

    /// Get mainchain blocks that commit to a specified block hash
    #[open_api_method(output_schema(
        PartialSchema = "coinshift_schema::BitcoinBlockHash"
    ))]
    #[method(name = "get_bmm_inclusions")]
    async fn get_bmm_inclusions(
        &self,
        block_hash: coinshift::types::BlockHash,
    ) -> RpcResult<Vec<bitcoin::BlockHash>>;

    /// Report reachability of the enforcer and every parent chain.
    ///
    /// Never includes credentials.
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "get_connectivity_status")]
    async fn get_connectivity_status(&self) -> RpcResult<ConnectivityStatus>;

    /// Show the L1 RPC config, with credentials removed.
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "get_l1_config")]
    async fn get_l1_config(
        &self,
        chain: Option<ParentChainType>,
    ) -> RpcResult<Vec<L1ChainConfigPublic>>;

    /// Point a parent chain at an endpoint, verifying it before saving.
    ///
    /// An endpoint serving a different network is refused and nothing is
    /// written; one that is merely unreachable is accepted, since configuring
    /// before starting a node is normal.
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "set_l1_config")]
    async fn set_l1_config(
        &self,
        chain: ParentChainType,
        url: String,
        user: Option<String>,
        password: Option<String>,
    ) -> RpcResult<L1ChainStatus>;

    /// Get the best mainchain block hash known by Coinshift
    #[open_api_method(output_schema(
        PartialSchema = "schema::Optional<coinshift_schema::BitcoinBlockHash>"
    ))]
    #[method(name = "get_best_mainchain_block_hash")]
    async fn get_best_mainchain_block_hash(
        &self,
    ) -> RpcResult<Option<bitcoin::BlockHash>>;

    /// Get the best sidechain block hash known by Coinshift
    #[open_api_method(output_schema(
        PartialSchema = "schema::Optional<coinshift::types::BlockHash>"
    ))]
    #[method(name = "get_best_sidechain_block_hash")]
    async fn get_best_sidechain_block_hash(
        &self,
    ) -> RpcResult<Option<coinshift::types::BlockHash>>;

    /// Get a new address
    #[method(name = "get_new_address")]
    async fn get_new_address(&self) -> RpcResult<Address>;

    /// Get wallet addresses, sorted by base58 encoding
    #[method(name = "get_wallet_addresses")]
    async fn get_wallet_addresses(&self) -> RpcResult<Vec<Address>>;

    /// Get wallet UTXOs
    #[method(name = "get_wallet_utxos")]
    async fn get_wallet_utxos(&self) -> RpcResult<Vec<PointedOutput>>;

    /// Get the current block count
    #[method(name = "getblockcount")]
    async fn getblockcount(&self) -> RpcResult<u32>;

    /// Get the height of the latest failed withdrawal bundle
    #[method(name = "latest_failed_withdrawal_bundle_height")]
    async fn latest_failed_withdrawal_bundle_height(
        &self,
    ) -> RpcResult<Option<u32>>;

    /// List peers
    #[method(name = "list_peers")]
    async fn list_peers(&self) -> RpcResult<Vec<Peer>>;

    /// List all UTXOs
    #[method(name = "list_utxos")]
    async fn list_utxos(&self) -> RpcResult<Vec<PointedOutput>>;

    /// Attempt to mine a sidechain block
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "mine")]
    async fn mine(&self, fee: Option<u64>) -> RpcResult<()>;

    /// Get OpenAPI schema
    #[open_api_method(output_schema(PartialSchema = "schema::OpenApi"))]
    #[method(name = "openapi_schema")]
    async fn openapi_schema(&self) -> RpcResult<utoipa::openapi::OpenApi>;

    /// Get pending withdrawal bundle
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "pending_withdrawal_bundle")]
    async fn pending_withdrawal_bundle(
        &self,
    ) -> RpcResult<Option<WithdrawalBundle>>;

    /// Remove a tx from the mempool
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "remove_from_mempool")]
    async fn remove_from_mempool(&self, txid: Txid) -> RpcResult<()>;

    /// Set the wallet seed from a mnemonic seed phrase
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "set_seed_from_mnemonic")]
    async fn set_seed_from_mnemonic(&self, mnemonic: String) -> RpcResult<()>;

    /// Get total sidechain wealth
    #[method(name = "sidechain_wealth")]
    async fn sidechain_wealth_sats(&self) -> RpcResult<u64>;

    /// Stop the node
    #[method(name = "stop")]
    async fn stop(&self);

    /// Transfer funds to the specified address
    #[method(name = "transfer")]
    async fn transfer(
        &self,
        dest: Address,
        value_sats: u64,
        fee_sats: u64,
    ) -> RpcResult<Txid>;

    /// Initiate a withdrawal to the specified mainchain address
    #[method(name = "withdraw")]
    async fn withdraw(
        &self,
        #[open_api_method_arg(schema(
            PartialSchema = "coinshift::types::schema::BitcoinAddr"
        ))]
        mainchain_address: bitcoin::Address<
            bitcoin::address::NetworkUnchecked,
        >,
        amount_sats: u64,
        fee_sats: u64,
        mainchain_fee_sats: u64,
    ) -> RpcResult<Txid>;

    /// Create a swap (L2 → L1)
    /// If l2_recipient is None, creates an open swap (anyone can fill it)
    #[open_api_method(output_schema(
        PartialSchema = "schema::Tuple<SwapId, Txid>"
    ))]
    #[method(name = "create_swap")]
    async fn create_swap(
        &self,
        parent_chain: ParentChainType,
        l1_recipient_address: String,
        l1_amount_sats: u64,
        l2_recipient: Option<Address>, // Optional - None = open swap
        l2_amount_sats: u64,
        required_confirmations: Option<u32>,
        fee_sats: u64,
    ) -> RpcResult<(SwapId, Txid)>;

    /// Reconstruct all swaps from the blockchain
    /// This is useful for recovering from database corruption or verifying swap integrity
    /// Returns the number of swaps reconstructed
    #[method(name = "reconstruct_swaps")]
    async fn reconstruct_swaps(&self) -> RpcResult<u32>;

    /// Update swap L1 transaction ID (called when L1 transaction is detected).
    /// For open swaps, pass l2_claimer_address so the claim is only valid for that address.
    #[method(name = "update_swap_l1_txid")]
    async fn update_swap_l1_txid(
        &self,
        swap_id: SwapId,
        l1_txid_hex: String,
        confirmations: u32,
        l2_claimer_address: Option<Address>,
    ) -> RpcResult<()>;

    /// Get swap status
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "get_swap_status")]
    async fn get_swap_status(&self, swap_id: SwapId)
    -> RpcResult<Option<Swap>>;

    /// Claim a swap (after L1 transaction has required confirmations)
    /// For open swaps, l2_claimer_address is required (the claimer's L2 address)
    #[method(name = "claim_swap")]
    async fn claim_swap(
        &self,
        swap_id: SwapId,
        l2_claimer_address: Option<Address>, // Required for open swaps
    ) -> RpcResult<Txid>;

    /// List all swaps
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "list_swaps")]
    async fn list_swaps(&self) -> RpcResult<Vec<Swap>>;

    /// List swaps for a specific recipient
    #[open_api_method(output_schema(ToSchema))]
    #[method(name = "list_swaps_by_recipient")]
    async fn list_swaps_by_recipient(
        &self,
        recipient: Address,
    ) -> RpcResult<Vec<Swap>>;

    /// Cancel a swap (unlock outputs and mark as cancelled).
    /// Only allowed for Pending swaps (before L1 transaction is detected).
    #[method(name = "cancel_swap")]
    async fn cancel_swap(&self, swap_id: SwapId) -> RpcResult<()>;

    /// Delete a swap from the database.
    /// Only allowed for Pending or Cancelled swaps.
    #[method(name = "delete_swap")]
    async fn delete_swap(&self, swap_id: SwapId) -> RpcResult<()>;
}

/// Reachability of the BIP300 enforcer.
#[derive(
    Clone, Debug, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
pub struct MainchainStatus {
    pub grpc_url: String,
    pub connected: bool,
    /// Why the last attempt failed. Absent while connected.
    pub last_error: Option<String>,
    /// Consecutive failed attempts; 0 while connected.
    pub reconnect_attempts: u32,
    /// Whether the enforcer offers its optional wallet service.
    pub wallet_service: bool,
    /// Whether mining and deposits are currently possible.
    pub can_mine: bool,
}

/// Reachability of one parent chain.
#[derive(
    Clone, Debug, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
pub struct L1ChainStatus {
    pub parent_chain: ParentChainType,
    /// Endpoint, with any credentials stripped. Absent when unconfigured.
    pub url: Option<String>,
    pub health: L1ChainHealth,
    /// Swaps on this chain still waiting for an L1 payment.
    pub swaps_awaiting: u32,
}

/// Everything the node knows about its external dependencies.
#[derive(
    Clone, Debug, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
pub struct ConnectivityStatus {
    pub mainchain: MainchainStatus,
    pub l1_chains: Vec<L1ChainStatus>,
}

/// A parent chain's configuration with its credentials removed.
#[derive(
    Clone, Debug, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
pub struct L1ChainConfigPublic {
    pub parent_chain: ParentChainType,
    pub url: String,
    /// Authentication scheme in use: `none`, `basic`, `bearer`, `header` or
    /// `query_param`. The secret itself is never returned.
    pub auth: String,
    pub enabled: bool,
    pub poll_interval_secs: Option<u64>,
    pub timeout_secs: Option<u64>,
}
