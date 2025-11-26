//! Bitcoin RPC client for querying L1 blockchain transactions

use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::types::ParentChainType;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub url: String,
    pub user: String,
    pub password: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vin {
    pub txid: Option<String>,
    pub vout: Option<u32>,
}

pub struct BitcoinRpcClient {
    config: RpcConfig,
    client: reqwest::blocking::Client,
}

impl BitcoinRpcClient {
    pub fn new(config: RpcConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");
        
        Self { config, client }
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, Error> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });

        tracing::debug!(
            url = %self.config.url,
            method = %method,
            params = %serde_json::to_string(&params).unwrap_or_else(|_| "invalid json".to_string()),
            "Making RPC call"
        );

        let mut request_builder = self.client.post(&self.config.url).json(&request);
        
        if !self.config.user.is_empty() {
            request_builder = request_builder.basic_auth(
                &self.config.user,
                Some(&self.config.password),
            );
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
        let content_type = response.headers()
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
            return Err(Error::Rpc(format!("{}: {}", error.code, error.message)));
        }

        json.result.ok_or(Error::InvalidResponse)
    }

    /// Get transaction by ID
    pub fn get_transaction(&self, txid: &str) -> Result<TransactionInfo, Error> {
        tracing::debug!(
            txid = %txid,
            "Fetching transaction from RPC"
        );
        let result = self.call::<TransactionInfo>("getrawtransaction", json!([txid, true]));
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

    /// Get confirmations for a transaction by ID
    pub fn get_transaction_confirmations(&self, txid: &str) -> Result<u32, Error> {
        let tx = self.get_transaction(txid)?;
        Ok(tx.confirmations)
    }

    /// Get transactions for an address
    /// Returns list of transaction IDs
    pub fn list_transactions(&self, address: &str) -> Result<Vec<String>, Error> {
        // Use listunspent to find transactions (works for most cases)
        // For more comprehensive results, we'd need to use a block explorer API
        // or maintain our own index
        let unspent: Vec<serde_json::Value> = self.call(
            "listunspent",
            json!([0, 999999, [address]]),
        )?;

        let mut txids = std::collections::HashSet::new();
        for utxo in unspent {
            if let Some(txid) = utxo.get("txid").and_then(|v| v.as_str()) {
                txids.insert(txid.to_string());
            }
        }

        // Also try to get transactions from getreceivedbyaddress (if available)
        // This is a fallback, but not all nodes support it
        // Note: We don't use the result, but calling it may help populate the node's internal index
        let _result: Result<f64, _> = self.call(
            "getreceivedbyaddress",
            json!([address, 0]),
        );

        Ok(txids.into_iter().collect())
    }

    /// Get current block height
    pub fn get_block_height(&self) -> Result<u32, Error> {
        let info: serde_json::Value = self.call("getblockchaininfo", json!([]))?;
        let blocks = info
            .get("blocks")
            .and_then(|v| v.as_u64())
            .ok_or(Error::InvalidResponse)?;
        Ok(blocks as u32)
    }

    /// Find transactions to an address matching a specific amount
    pub fn find_transactions_by_address_and_amount(
        &self,
        address: &str,
        amount_sats: u64,
    ) -> Result<Vec<(String, u32, String)>, Error> {
        // Get all transactions for this address
        let txids = self.list_transactions(address)?;
        let mut matches = Vec::new();
        let _current_height = self.get_block_height()?;

        for txid in txids {
            match self.get_transaction(&txid) {
                Ok(tx) => {
                    // Check if any output matches the address and amount
                    for vout in &tx.vout {
                        let vout_value_sats = (vout.value * 100_000_000.0) as u64;
                        let matches_address = vout.script_pub_key.address.as_ref()
                            .map(|addr| addr == address)
                            .unwrap_or(false)
                            || vout.script_pub_key.addresses.as_ref()
                                .map(|addrs| addrs.contains(&address.to_string()))
                                .unwrap_or(false);

                        if matches_address && vout_value_sats == amount_sats {
                            // Extract sender address from first input
                            let sender_address = if let Some(vin) = tx.vin.first() {
                                if let (Some(input_txid), Some(input_vout)) = (&vin.txid, &vin.vout) {
                                    // Get the input transaction to find sender
                                    match self.get_transaction(input_txid) {
                                        Ok(input_tx) => {
                                            if let Some(input_vout_data) = input_tx.vout.get(*input_vout as usize) {
                                                input_vout_data.script_pub_key.address.clone()
                                                    .or_else(|| input_vout_data.script_pub_key.addresses.as_ref()
                                                        .and_then(|addrs| addrs.first().cloned()))
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

                            let confirmations = tx.confirmations;
                            let sender = sender_address.unwrap_or_else(|| "unknown".to_string());
                            matches.push((txid.clone(), confirmations, sender));
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

/// Get RPC config for a parent chain
/// This is a placeholder - in practice, this should access the GUI's stored config
pub fn get_rpc_config(_parent_chain: ParentChainType) -> Option<RpcConfig> {
    // TODO: Access stored RPC config from GUI/app state
    // For now, return None to indicate no config available
    None
}

