//! Parent Chain RPC client for querying L1 blockchain transactions
//!
//! This module provides a generic RPC client that works with any Bitcoin-compatible
//! blockchain (Bitcoin, Bitcoin Cash, Litecoin, etc.) that implements the standard
//! Bitcoin Core JSON-RPC interface.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

use crate::types::ParentChainType;

/// Convert a Bitcoin Core-style RPC `vout.value` (BTC as `f64`) to satoshis.
///
/// Bitcoin Core JSON-RPC returns amounts as floats. Multiplying by 1e8 and
/// casting with `as u64` **truncates**, so values that are not exact in binary
/// floating point (for example `0.29`) become one sat too low and never match
/// an exact sat target. This helper **rounds** to the nearest sat instead.
///
/// Returns `None` for non-finite, negative, or out-of-range values.
pub fn btc_rpc_value_to_sats(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let sats = (value * 100_000_000.0).round();
    if sats < 0.0 || sats > u64::MAX as f64 {
        return None;
    }
    Some(sats as u64)
}

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
    /// Node's chain type does not match expected (e.g. expected Signet, got main)
    #[error(
        "Node chain mismatch: expected {expected:?}, node reported chain \"{chain}\""
    )]
    ChainMismatch {
        expected: ParentChainType,
        chain: String,
    },
    /// The RPC cookie file could not be read or parsed
    #[error("Failed to read RPC cookie file {path}: {reason}")]
    CookieFile { path: PathBuf, reason: String },
}

/// How to reach a parent-chain node's JSON-RPC interface.
///
/// The node is the swap subsystem's only source of truth about L1 payments,
/// so it must be one the user controls or trusts; see
/// `docs/COINSHIFT_HOW_IT_WORKS.md`. Nothing here restricts the URL: a user
/// may point at any node, over `http://` (loopback only, ideally) or
/// `https://`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RpcConfig {
    pub url: String,
    /// Basic-auth user. Ignored when `cookie_file` is set.
    #[serde(default)]
    pub user: String,
    /// Basic-auth password. Ignored when `cookie_file` is set.
    #[serde(default)]
    pub password: String,
    /// Path to a Bitcoin Core style `.cookie` file (`user:password` on one
    /// line). Read on every call, so a node restart that rotates the cookie
    /// needs no reconfiguration. Takes precedence over `user`/`password`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_file: Option<PathBuf>,
}

impl RpcConfig {
    /// Resolve the basic-auth credentials for a call, reading the cookie file
    /// if one is configured.
    pub fn credentials(&self) -> Result<Option<(String, String)>, Error> {
        if let Some(path) = &self.cookie_file {
            let contents = std::fs::read_to_string(path).map_err(|err| {
                Error::CookieFile {
                    path: path.clone(),
                    reason: err.to_string(),
                }
            })?;
            let (user, password) =
                contents.trim().split_once(':').ok_or_else(|| {
                    Error::CookieFile {
                        path: path.clone(),
                        reason: "expected `user:password`".to_string(),
                    }
                })?;
            return Ok(Some((user.to_owned(), password.to_owned())));
        }
        if self.user.is_empty() {
            return Ok(None);
        }
        Ok(Some((self.user.clone(), self.password.clone())))
    }

    /// True when the URL sends credentials and receives payment evidence in
    /// clear text over a network: plaintext `http://` to a host other than
    /// loopback. Such a node can be impersonated by anyone on the path, and
    /// what it says decides when a swap becomes claimable.
    pub fn is_plaintext_remote(&self) -> bool {
        let Ok(url) = url::Url::parse(&self.url) else {
            return false;
        };
        if url.scheme() != "http" {
            return false;
        }
        match url.host() {
            Some(url::Host::Domain(host)) => host != "localhost",
            Some(url::Host::Ipv4(ip)) => !ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => !ip.is_loopback(),
            None => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInfo {
    pub txid: String,
    pub confirmations: u32,
    pub blockheight: Option<u32>,
    pub vout: Vec<Vout>,
    pub vin: Vec<Vin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vout {
    pub value: f64,
    #[serde(rename = "scriptPubKey")]
    pub script_pub_key: ScriptPubKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptPubKey {
    pub address: Option<String>,
    pub addresses: Option<Vec<String>>,
    /// Raw script, hex. Needed to read the swap commitment out of an
    /// `OP_RETURN` output.
    #[serde(default)]
    pub hex: Option<String>,
}

impl TransactionInfo {
    /// The swap commitment carried by this transaction, if any output is an
    /// `OP_RETURN` in the `CSFT || swap_id || claimer` format.
    pub fn swap_commitment(
        &self,
    ) -> Option<(crate::types::SwapId, crate::types::Address)> {
        self.vout.iter().find_map(|vout| {
            let script_bytes =
                hex::decode(vout.script_pub_key.hex.as_ref()?).ok()?;
            crate::state::l1_proof::parse_commitment(
                bitcoin::Script::from_bytes(&script_bytes),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vin {
    pub txid: Option<String>,
    pub vout: Option<u32>,
}

/// RPC client for communicating with parent chain nodes (Bitcoin, Bitcoin Cash, Litecoin, etc.)
///
/// This client uses the standard Bitcoin Core JSON-RPC interface, which is compatible
/// with most Bitcoin-derivative blockchains.
pub struct ParentChainRpcClient {
    config: RpcConfig,
    client: reqwest::blocking::Client,
}

impl ParentChainRpcClient {
    pub fn new(config: RpcConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self { config, client }
    }

    pub(crate) fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, Error> {
        // Use jsonrpc "1.0" for compatibility with nodes that accept curl-style requests (e.g. BCH)
        let request = json!({
            "jsonrpc": "1.0",
            "id": "coinshift",
            "method": method,
            "params": params
        });

        tracing::debug!(
            url = %self.config.url,
            method = %method,
            params = %serde_json::to_string(&params).unwrap_or_else(|_| "invalid json".to_string()),
            "Making RPC call"
        );

        let mut request_builder =
            self.client.post(&self.config.url).json(&request);

        if let Some((user, password)) = self.config.credentials()? {
            request_builder = request_builder.basic_auth(user, Some(password));
        }

        let response = match request_builder.send() {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!(
                    url = %self.config.url,
                    method = %method,
                    error = %e,
                    "Failed to send RPC request"
                );
                return Err(Error::Http(e));
            }
        };

        let status = response.status();
        tracing::debug!(
            url = %self.config.url,
            method = %method,
            status = %status,
            "Received RPC response"
        );

        // Get response headers
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown");
        tracing::debug!(
            url = %self.config.url,
            method = %method,
            content_type = %content_type,
            "Response headers"
        );

        // Read the raw response body for debugging
        let response_text = match response.text() {
            Ok(text) => text,
            Err(e) => {
                tracing::error!(
                    url = %self.config.url,
                    method = %method,
                    status = %status,
                    error = %e,
                    "Failed to read response body as text"
                );
                return Err(Error::Http(e));
            }
        };

        tracing::debug!(
            url = %self.config.url,
            method = %method,
            status = %status,
            response_body = %response_text,
            "Raw RPC response body"
        );

        // Try to parse as JSON
        let json: RpcResponse<T> = match serde_json::from_str(&response_text) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::error!(
                    url = %self.config.url,
                    method = %method,
                    status = %status,
                    response_body = %response_text,
                    error = %e,
                    "Failed to parse response as JSON"
                );
                return Err(Error::Json(e));
            }
        };

        if let Some(error) = json.error {
            tracing::error!(
                url = %self.config.url,
                method = %method,
                rpc_error_code = %error.code,
                rpc_error_message = %error.message,
                "RPC returned error"
            );
            return Err(Error::Rpc(format!(
                "{}: {}",
                error.code, error.message
            )));
        }

        json.result.ok_or(Error::InvalidResponse)
    }

    /// Get transaction by ID
    pub fn get_transaction(
        &self,
        txid: &str,
    ) -> Result<TransactionInfo, Error> {
        tracing::debug!(
            txid = %txid,
            "Fetching transaction from RPC"
        );
        let result = self
            .call::<TransactionInfo>("getrawtransaction", json!([txid, true]));
        match &result {
            Ok(tx_info) => {
                tracing::debug!(
                    txid = %txid,
                    confirmations = %tx_info.confirmations,
                    blockheight = ?tx_info.blockheight,
                    "Successfully fetched transaction"
                );
            }
            Err(e) => {
                tracing::error!(
                    txid = %txid,
                    error = %e,
                    error_debug = ?e,
                    "Failed to fetch transaction"
                );
            }
        }
        result
    }

    /// Raw consensus-encoded transaction bytes (`getrawtransaction txid`
    /// with `verbose = false`).
    pub fn get_raw_transaction(&self, txid: &str) -> Result<Vec<u8>, Error> {
        let hex_str: String =
            self.call("getrawtransaction", json!([txid, false]))?;
        hex::decode(hex_str.trim()).map_err(|_| Error::InvalidResponse)
    }

    /// Consensus-encoded merkle block proving `txid`'s inclusion in the
    /// block that contains it (`gettxoutproof [txid]`). Needs `-txindex` on
    /// the node unless the output is still unspent.
    pub fn get_txout_proof(&self, txid: &str) -> Result<Vec<u8>, Error> {
        let hex_str: String = self.call("gettxoutproof", json!([[txid]]))?;
        hex::decode(hex_str.trim()).map_err(|_| Error::InvalidResponse)
    }

    /// Build the `SwapClaim` payment proof for an L1 transaction: the merkle
    /// block from `gettxoutproof` plus the raw transaction, borsh-encoded.
    pub fn build_l1_payment_proof(&self, txid: &str) -> Result<Vec<u8>, Error> {
        let merkle_block = self.get_txout_proof(txid)?;
        let raw_tx = self.get_raw_transaction(txid)?;
        Ok(
            crate::state::l1_proof::L1PaymentProof::new(merkle_block, raw_tx)
                .to_bytes(),
        )
    }

    /// Get confirmations for a transaction by ID
    pub fn get_transaction_confirmations(
        &self,
        txid: &str,
    ) -> Result<u32, Error> {
        let tx = self.get_transaction(txid)?;
        Ok(tx.confirmations)
    }

    /// Find transactions that currently have an unspent output paying
    /// `address`. Returns transaction IDs in RPC byte order.
    ///
    /// Discovery runs two sources and unions them:
    ///
    /// 1. `scantxoutset` with an `addr(...)` descriptor scans the node's whole
    ///    UTXO set, so it finds a payment regardless of whether the node's
    ///    wallet tracks the address. This is the source that works on a stock
    ///    node; nothing in the swap flow imports the counterparty's address.
    /// 2. `listunspent` for nodes without `scantxoutset`. It only returns
    ///    outputs the node's *wallet* knows about, and returns an empty list
    ///    rather than an error for any other address, so on its own it
    ///    silently never fires for the default flow.
    ///
    /// When only the wallet source is available and it finds nothing, that is
    /// logged as a warning so the operator knows detection may be blind and
    /// the manual `update_swap_l1_txid` path is needed. Both sources see
    /// unspent outputs only; once a txid is known callers should track it
    /// with [`Self::get_transaction`], which works for spent outputs given
    /// `-txindex`.
    pub fn list_transactions(
        &self,
        address: &str,
    ) -> Result<Vec<String>, Error> {
        let mut txids = std::collections::HashSet::new();

        let scan: Result<serde_json::Value, Error> = self.call(
            "scantxoutset",
            json!(["start", [format!("addr({address})")]]),
        );
        let chain_scan_available = match scan {
            Ok(result) => {
                if let Some(unspents) =
                    result.get("unspents").and_then(|v| v.as_array())
                {
                    for utxo in unspents {
                        if let Some(txid) =
                            utxo.get("txid").and_then(|v| v.as_str())
                        {
                            txids.insert(txid.to_string());
                        }
                    }
                }
                true
            }
            Err(err) => {
                tracing::debug!(
                    url = %self.config.url,
                    address = %address,
                    error = %err,
                    "scantxoutset unavailable; falling back to wallet-only listunspent"
                );
                false
            }
        };

        let wallet_scan: Result<Vec<serde_json::Value>, Error> =
            self.call("listunspent", json!([0, 999_999_999, [address]]));
        let wallet_found = match wallet_scan {
            Ok(unspent) => {
                let before = txids.len();
                for utxo in unspent {
                    if let Some(txid) =
                        utxo.get("txid").and_then(|v| v.as_str())
                    {
                        txids.insert(txid.to_string());
                    }
                }
                txids.len() > before
            }
            Err(err) => {
                if !chain_scan_available {
                    return Err(err);
                }
                false
            }
        };

        if !chain_scan_available && !wallet_found {
            tracing::warn!(
                url = %self.config.url,
                address = %address,
                "L1 fill detection is wallet-only on this node (no scantxoutset) \
                 and its wallet does not track this address: payments to it \
                 will not be detected automatically. Import the address as \
                 watch-only, use a node with scantxoutset, or record the fill \
                 with update_swap_l1_txid."
            );
        }

        Ok(txids.into_iter().collect())
    }

    /// Get current block height
    pub fn get_block_height(&self) -> Result<u32, Error> {
        let info: serde_json::Value =
            self.call("getblockchaininfo", json!([]))?;
        let blocks = info
            .get("blocks")
            .and_then(|v| v.as_u64())
            .ok_or(Error::InvalidResponse)?;
        Ok(blocks as u32)
    }

    /// Get the chain name from getblockchaininfo (e.g. "signet", "main", "testnet4", "test4").
    /// Used to detect if the node is Bitcoin Signet or Bitcoin Cash testnet4.
    /// Some BCH nodes report "test4" instead of "testnet4".
    pub fn get_blockchain_chain_name(&self) -> Result<String, Error> {
        let info: serde_json::Value =
            self.call("getblockchaininfo", json!([]))?;
        let chain = info
            .get("chain")
            .and_then(|v| v.as_str())
            .ok_or(Error::InvalidResponse)?;
        Ok(chain.to_lowercase())
    }

    /// Find transactions to an address matching a specific amount.
    /// Returns (sender_address, tx_info).
    /// Only includes transactions that are in a block (blockheight is Some).
    pub fn find_transactions_by_address_and_amount(
        &self,
        address: &str,
        amount_sats: u64,
    ) -> Result<Vec<(String, TransactionInfo)>, Error> {
        // Get all transactions for this address
        let txids = self.list_transactions(address)?;
        let mut matches = Vec::new();
        let _current_height = self.get_block_height()?;

        for txid in txids {
            match self.get_transaction(&txid) {
                Ok(tx) => {
                    // Check if any output matches the address and amount
                    for vout in &tx.vout {
                        let Some(vout_value_sats) =
                            btc_rpc_value_to_sats(vout.value)
                        else {
                            continue;
                        };
                        let matches_address = vout
                            .script_pub_key
                            .address
                            .as_ref()
                            .map(|addr| addr == address)
                            .unwrap_or(false)
                            || vout
                                .script_pub_key
                                .addresses
                                .as_ref()
                                .map(|addrs| {
                                    addrs.contains(&address.to_string())
                                })
                                .unwrap_or(false);

                        if matches_address && vout_value_sats == amount_sats {
                            // Extract sender address from first input
                            let sender_address = if let Some(vin) =
                                tx.vin.first()
                            {
                                if let (Some(input_txid), Some(input_vout)) =
                                    (&vin.txid, &vin.vout)
                                {
                                    // Get the input transaction to find sender
                                    match self.get_transaction(input_txid) {
                                        Ok(input_tx) => {
                                            if let Some(input_vout_data) =
                                                input_tx
                                                    .vout
                                                    .get(*input_vout as usize)
                                            {
                                                input_vout_data
                                                    .script_pub_key
                                                    .address
                                                    .clone()
                                                    .or_else(|| {
                                                        input_vout_data
                                                            .script_pub_key
                                                            .addresses
                                                            .as_ref()
                                                            .and_then(|addrs| {
                                                                addrs
                                                                    .first()
                                                                    .cloned()
                                                            })
                                                    })
                                            } else {
                                                None
                                            }
                                        }
                                        Err(_) => None,
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            let sender = sender_address
                                .unwrap_or_else(|| "unknown".to_string());
                            matches.push((sender, tx));
                            break; // Found a match, no need to check other outputs
                        }
                    }
                }
                Err(Error::TransactionNotFound) => {
                    // Transaction might have been spent, skip it
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Error getting transaction {}: {}", txid, e);
                    continue;
                }
            }
        }

        Ok(matches)
    }
}

/// Default L1 configs: a node on this machine, on the chain's default RPC
/// port, with no credentials filled in. These are starting points for the
/// user to edit, not endpoints anyone is expected to run for them. The
/// application never ships a third-party endpoint or a credential.
pub fn default_l1_configs() -> Vec<(ParentChainType, RpcConfig)> {
    let local = |port: u16| RpcConfig {
        url: format!("http://127.0.0.1:{port}"),
        ..RpcConfig::default()
    };
    vec![
        // Bitcoin Core `-signet`
        (ParentChainType::Signet, local(38332)),
        // Bitcoin Core `-regtest`
        (ParentChainType::Regtest, local(18443)),
    ]
}

/// Parent chain types that are allowed for L1 config (and swap creation).
///
/// Only chains whose payments consensus can verify are offered: a `SwapClaim`
/// proves its L1 payment against the mainchain headers the node validates
/// through the enforcer, so the parent chain has to be the one this sidechain
/// is anchored to (see `ParentChainType::supports_payment_proofs`). Bitcoin
/// Cash and Litecoin have no header relay and are not selectable.
pub fn supported_l1_parent_chain_types() -> &'static [ParentChainType] {
    use ParentChainType::{Regtest, Signet};
    &[Signet, Regtest]
}

/// Detect whether the node at the given config is Bitcoin Signet or Bitcoin Cash testnet4
/// by calling getblockchaininfo and checking the "chain" field.
/// Returns the detected chain type and the raw "chain" string from the node.
pub fn detect_chain_type(
    config: &RpcConfig,
) -> Result<(ParentChainType, String), Error> {
    let client = ParentChainRpcClient::new(config.clone());
    let chain = client.get_blockchain_chain_name()?;
    let detected = match chain.as_str() {
        "signet" => ParentChainType::Signet,
        "regtest" => ParentChainType::Regtest,
        "testnet4" | "test4" => ParentChainType::BCH,
        _ => {
            return Err(Error::ChainMismatch {
                expected: ParentChainType::Signet, // arbitrary for this error
                chain: chain.clone(),
            });
        }
    };
    Ok((detected, chain))
}

/// Read the whole L1 config file. A missing or unparseable file reads as
/// empty.
pub fn read_l1_config_file(
    path: &Path,
) -> std::collections::HashMap<ParentChainType, RpcConfig> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the whole L1 config file, creating the parent directory if needed.
pub fn write_l1_config_file_contents(
    path: &Path,
    configs: &std::collections::HashMap<ParentChainType, RpcConfig>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(configs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Add the default (local node) config for each chain in `chains_to_enable`
/// that has no entry yet. Existing entries, default or user-edited, are left
/// alone.
pub fn write_l1_config_file(
    path: &Path,
    chains_to_enable: &[ParentChainType],
) -> std::io::Result<()> {
    let defaults = default_l1_configs();
    let mut configs = read_l1_config_file(path);
    for chain in chains_to_enable {
        if let Some((_, rpc)) = defaults.iter().find(|(c, _)| c == chain) {
            configs.entry(*chain).or_insert_with(|| rpc.clone());
        }
    }
    write_l1_config_file_contents(path, &configs)
}

/// Validate the L1 config file before start: each configured node must
/// report the chain it is configured for. Plaintext HTTP to a non-loopback
/// host is allowed but logged as a warning, since that node's answers decide
/// when swaps become claimable and anyone on the path could forge them.
pub fn validate_l1_config_file(path: &Path) -> Result<(), Error> {
    for (parent_chain, rpc) in read_l1_config_file(path) {
        if rpc.is_plaintext_remote() {
            tracing::warn!(
                chain = ?parent_chain,
                url = %rpc.url,
                "L1 RPC is plaintext HTTP to a remote host: credentials and \
                 swap payment evidence can be read or forged on the network \
                 path. Use https:// or a node on this machine."
            );
        }
        let (detected, chain_name) = detect_chain_type(&rpc)?;
        if detected != parent_chain {
            return Err(Error::ChainMismatch {
                expected: parent_chain,
                chain: chain_name,
            });
        }
    }
    Ok(())
}

/// Load RPC config for a parent chain from a JSON file.
///
/// The file format is `{ "<ParentChainType>": { "url": "...", "user": "...",
/// "password": "...", "cookie_file": "..." }, ... }` (the format written by
/// the GUI and CLI to `l1_rpc_configs.json`).
pub fn load_rpc_config_from_path(
    path: &Path,
    parent_chain: ParentChainType,
) -> Option<RpcConfig> {
    read_l1_config_file(path).remove(&parent_chain)
}

/// Get RPC config for a parent chain
/// This is a placeholder - in practice, this should access the GUI's stored config
pub fn get_rpc_config(_parent_chain: ParentChainType) -> Option<RpcConfig> {
    // TODO: Access stored RPC config from GUI/app state
    // For now, return None to indicate no config available
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Truncating `as u64` after `* 1e8` is the bug this helper replaces.
    fn truncating_cast_sats(value: f64) -> u64 {
        (value * 100_000_000.0) as u64
    }

    #[test]
    fn btc_rpc_value_to_sats_rounds_classic_truncation_case() {
        // 0.29 is not exact in binary f64; truncation yields one sat short.
        assert_eq!(truncating_cast_sats(0.29), 28_999_999);
        assert_eq!(btc_rpc_value_to_sats(0.29), Some(29_000_000));
    }

    #[test]
    fn btc_rpc_value_to_sats_exact_and_whole_coins() {
        assert_eq!(btc_rpc_value_to_sats(1.0), Some(100_000_000));
        assert_eq!(btc_rpc_value_to_sats(0.0), Some(0));
        assert_eq!(btc_rpc_value_to_sats(0.00000001), Some(1));
        assert_eq!(btc_rpc_value_to_sats(50.0), Some(5_000_000_000));
    }

    #[test]
    fn btc_rpc_value_to_sats_rejects_invalid() {
        assert_eq!(btc_rpc_value_to_sats(f64::NAN), None);
        assert_eq!(btc_rpc_value_to_sats(f64::INFINITY), None);
        assert_eq!(btc_rpc_value_to_sats(f64::NEG_INFINITY), None);
        assert_eq!(btc_rpc_value_to_sats(-0.01), None);
    }

    #[test]
    fn btc_rpc_value_to_sats_matches_swap_target_where_cast_would_not() {
        // Automatic match and GUI claim both compare against exact sat targets.
        let target_sats = 29_000_000u64;
        let rpc_float = 0.29_f64;
        assert_ne!(
            truncating_cast_sats(rpc_float),
            target_sats,
            "precondition: cast must miss the target (documents the bug)"
        );
        assert_eq!(
            btc_rpc_value_to_sats(rpc_float),
            Some(target_sats),
            "rounded conversion must hit the exact sat target used by swaps"
        );
    }

    #[test]
    fn load_rpc_config_from_path_missing_file_returns_none() {
        let path = Path::new("/nonexistent/l1_rpc_configs.json");
        assert!(
            load_rpc_config_from_path(path, ParentChainType::Regtest).is_none()
        );
    }

    #[test]
    fn load_rpc_config_from_path_valid_file_returns_config() {
        let dir = std::env::temp_dir();
        let path = dir.join("coinshift_l1_rpc_test.json");
        let configs = serde_json::json!({
            "Regtest": { "url": "http://127.0.0.1:18443", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        let cfg = load_rpc_config_from_path(&path, ParentChainType::Regtest);
        drop(std::fs::remove_file(&path)); // best-effort cleanup
        assert!(cfg.is_some());
        let cfg = cfg.unwrap();
        assert_eq!(cfg.url, "http://127.0.0.1:18443");
        assert_eq!(cfg.user, "u");
        assert_eq!(cfg.password, "p");
    }

    #[test]
    fn load_rpc_config_from_path_wrong_chain_returns_none() {
        let dir = std::env::temp_dir();
        let path = dir.join("coinshift_l1_rpc_test2.json");
        let configs = serde_json::json!({
            "Signet": { "url": "http://127.0.0.1:38332", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        let cfg = load_rpc_config_from_path(&path, ParentChainType::Regtest);
        drop(std::fs::remove_file(&path)); // best-effort cleanup
        assert!(cfg.is_none());
    }

    /// Defaults point at this machine and carry no credentials: the
    /// application must never ship a third-party endpoint or a password.
    #[test]
    fn default_l1_configs_are_local_and_credential_free() {
        let configs = default_l1_configs();
        assert_eq!(configs.len(), 2);
        for (chain, rpc) in configs {
            assert!(
                !rpc.is_plaintext_remote(),
                "{chain:?} default must be a loopback URL, got {}",
                rpc.url
            );
            assert!(rpc.user.is_empty() && rpc.password.is_empty());
            assert!(rpc.cookie_file.is_none());
        }
    }

    #[test]
    fn plaintext_remote_detection() {
        let cfg = |url: &str| RpcConfig {
            url: url.to_owned(),
            ..RpcConfig::default()
        };
        assert!(!cfg("http://127.0.0.1:38332").is_plaintext_remote());
        assert!(!cfg("http://localhost:38332").is_plaintext_remote());
        assert!(!cfg("http://[::1]:38332").is_plaintext_remote());
        assert!(!cfg("https://node.example:28332").is_plaintext_remote());
        assert!(cfg("http://173.230.135.236:28332").is_plaintext_remote());
        assert!(cfg("http://node.example:28332").is_plaintext_remote());
    }

    #[test]
    fn cookie_file_takes_precedence_over_user_password() {
        let dir = temp_dir::TempDir::new().unwrap();
        let cookie = dir.path().join(".cookie");
        std::fs::write(&cookie, "__cookie__:s3cret\n").unwrap();
        let cfg = RpcConfig {
            url: "http://127.0.0.1:38332".to_owned(),
            user: "ignored".to_owned(),
            password: "ignored".to_owned(),
            cookie_file: Some(cookie),
        };
        assert_eq!(
            cfg.credentials().unwrap(),
            Some(("__cookie__".to_owned(), "s3cret".to_owned()))
        );
        let no_auth = RpcConfig {
            url: "http://127.0.0.1:38332".to_owned(),
            ..RpcConfig::default()
        };
        assert_eq!(no_auth.credentials().unwrap(), None);
        let bad = RpcConfig {
            cookie_file: Some(dir.path().join("missing")),
            ..no_auth
        };
        assert!(matches!(bad.credentials(), Err(Error::CookieFile { .. })));
    }

    /// The config file format written before `cookie_file` existed must still
    /// load, and user-edited entries must survive `write_l1_config_file`.
    #[test]
    fn write_l1_config_file_keeps_custom_entries() {
        let dir = temp_dir::TempDir::new().unwrap();
        let path = dir.path().join("l1_rpc_configs.json");
        let configs = serde_json::json!({
            "Signet": { "url": "https://my-node.example:38332", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        write_l1_config_file(
            &path,
            &[ParentChainType::Signet, ParentChainType::Regtest],
        )
        .unwrap();
        let signet =
            load_rpc_config_from_path(&path, ParentChainType::Signet).unwrap();
        assert_eq!(signet.url, "https://my-node.example:38332");
        assert_eq!(signet.user, "u");
        let regtest =
            load_rpc_config_from_path(&path, ParentChainType::Regtest).unwrap();
        assert_eq!(regtest.url, "http://127.0.0.1:18443");
        assert!(regtest.user.is_empty());
    }

    /// A minimal JSON-RPC server for one connection at a time. `respond`
    /// maps a method name to the `result` (or a JSON-RPC error when `Err`).
    pub(crate) fn fake_rpc<F>(
        respond: F,
    ) -> (String, std::thread::JoinHandle<Vec<String>>)
    where
        F: Fn(&str) -> Result<serde_json::Value, String> + Send + 'static,
    {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut methods = Vec::new();
            listener.set_nonblocking(false).unwrap();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return methods;
                    }
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                    {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).unwrap();
                let request: serde_json::Value =
                    serde_json::from_slice(&body).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                let response = match respond(&method) {
                    Ok(result) => {
                        json!({"result": result, "error": null, "id": "coinshift"})
                    }
                    Err(message) => json!({
                        "result": null,
                        "error": {"code": -32601, "message": message},
                        "id": "coinshift"
                    }),
                };
                let stop = method == "stop";
                methods.push(method);
                let body = response.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
                stream.flush().unwrap();
                if stop {
                    return methods;
                }
            }
            methods
        });
        (url, handle)
    }

    fn client_for(url: &str) -> ParentChainRpcClient {
        ParentChainRpcClient::new(RpcConfig {
            url: url.to_owned(),
            ..RpcConfig::default()
        })
    }

    /// Discovery must not depend on the node's wallet: a payment visible to
    /// `scantxoutset` is found even when `listunspent` (wallet-only) returns
    /// nothing, which is what a stock node returns for any address the
    /// wallet does not track.
    #[test]
    fn list_transactions_uses_utxo_set_scan_not_only_wallet() {
        let (url, server) = fake_rpc(|method| match method {
            "scantxoutset" => Ok(json!({
                "success": true,
                "unspents": [{"txid": "aa".repeat(32), "vout": 0}]
            })),
            "listunspent" => Ok(json!([])),
            "stop" => Ok(json!(true)),
            other => Err(format!("unexpected method {other}")),
        });
        let client = client_for(&url);
        let txids = client.list_transactions("tb1qexample").unwrap();
        assert_eq!(txids, vec!["aa".repeat(32)]);
        drop(client.call::<serde_json::Value>("stop", json!([])).unwrap());
        let methods = server.join().unwrap();
        assert!(methods.contains(&"scantxoutset".to_owned()));
        assert!(
            !methods.contains(&"getreceivedbyaddress".to_owned()),
            "the wallet-only getreceivedbyaddress probe is gone"
        );
    }

    /// Without `scantxoutset` the wallet source still works, and a wallet
    /// error is not swallowed.
    #[test]
    fn list_transactions_falls_back_to_wallet_when_scan_unavailable() {
        let (url, server) = fake_rpc(|method| match method {
            "scantxoutset" => Err("Method not found".to_owned()),
            "listunspent" => Ok(json!([{"txid": "bb".repeat(32), "vout": 1}])),
            "stop" => Ok(json!(true)),
            other => Err(format!("unexpected method {other}")),
        });
        let client = client_for(&url);
        let txids = client.list_transactions("tb1qexample").unwrap();
        assert_eq!(txids, vec!["bb".repeat(32)]);
        drop(client.call::<serde_json::Value>("stop", json!([])).unwrap());
        server.join().unwrap();

        let (url, server) = fake_rpc(|method| match method {
            "stop" => Ok(json!(true)),
            _ => Err("Method not found".to_owned()),
        });
        let client = client_for(&url);
        assert!(
            client.list_transactions("tb1qexample").is_err(),
            "no discovery source at all must be an error, not an empty list"
        );
        drop(client.call::<serde_json::Value>("stop", json!([])).unwrap());
        server.join().unwrap();
    }

    #[test]
    fn validate_l1_config_file_empty_or_missing_ok() {
        let path = Path::new("/nonexistent/l1_rpc_configs.json");
        assert!(validate_l1_config_file(path).is_ok());
    }
}
