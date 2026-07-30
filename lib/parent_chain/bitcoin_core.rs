//! [`ParentChainClient`] over the Bitcoin Core JSON-RPC interface.
//!
//! Works with any Bitcoin-derivative node that speaks the standard interface —
//! Bitcoin Core, Bitcoin Cash Node, Litecoin Core — using only
//! `getblockchaininfo`, `getrawtransaction` and `listunspent`.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ChainIdentity, Error, L1Payment, ParentChainClient, PaymentQuery};
use crate::{
    l1::config::{L1Auth, L1ChainConfig},
    types::{ParentChainType, SwapTxId},
};

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

/// Convert a decimal coin amount from JSON into base units.
///
/// **Rounds rather than truncates.** Bitcoin Core emits at most `decimals`
/// decimal places, so the true value is an exact multiple of one base unit and
/// rounding recovers it exactly; truncating does not. `0.29` parses to
/// `0.28999999999999998…`, and `(x * 1e8) as u64` yields 28_999_999 — one
/// satoshi short, which silently prevented a swap for that amount from ever
/// matching.
fn coins_to_base_units(value: f64, decimals: u8) -> u64 {
    let scale = 10f64.powi(i32::from(decimals));
    (value * scale).round() as u64
}

/// Whether `vout` pays `address`, accepting both the modern `address` field and
/// the legacy `addresses` array.
fn pays_address(vout: &Vout, address: &str) -> bool {
    vout.script_pub_key.address.as_deref() == Some(address)
        || vout
            .script_pub_key
            .addresses
            .as_ref()
            .is_some_and(|addrs| addrs.iter().any(|addr| addr == address))
}

pub struct BitcoinCoreClient {
    chain: ParentChainType,
    config: L1ChainConfig,
    client: reqwest::blocking::Client,
}

/// Apply the configured authentication scheme to an outgoing request.
///
/// Kept here rather than on [`L1Auth`] so the config module stays free of any
/// particular HTTP client.
fn apply_auth(
    builder: reqwest::blocking::RequestBuilder,
    auth: &L1Auth,
) -> reqwest::blocking::RequestBuilder {
    match auth {
        L1Auth::None => builder,
        L1Auth::Basic { user, password } => {
            builder.basic_auth(user, Some(password))
        }
        L1Auth::Bearer { token } => builder.bearer_auth(token),
        L1Auth::Header { name, value } => builder.header(name, value),
        L1Auth::QueryParam { name, value } => builder.query(&[(name, value)]),
    }
}

impl BitcoinCoreClient {
    pub fn new(chain: ParentChainType, config: L1ChainConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(config.timeout())
            .build()
            .expect("Failed to create HTTP client");

        Self {
            chain,
            config,
            client,
        }
    }

    /// Endpoint URL, for logging.
    pub fn url(&self) -> &str {
        &self.config.url
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, Error> {
        // jsonrpc "1.0" for compatibility with nodes that accept curl-style
        // requests (e.g. BCH)
        let request = json!({
            "jsonrpc": "1.0",
            "id": "coinshift",
            "method": method,
            "params": params
        });

        tracing::debug!(
            url = %self.config.url,
            method = %method,
            "Making RPC call"
        );

        let request_builder = apply_auth(
            self.client.post(&self.config.url).json(&request),
            &self.config.auth,
        );

        let response = request_builder.send().inspect_err(|err| {
            tracing::error!(
                url = %self.config.url,
                method = %method,
                error = %err,
                "Failed to send RPC request"
            );
        })?;

        let status = response.status();
        let response_text = response.text().inspect_err(|err| {
            tracing::error!(
                url = %self.config.url,
                method = %method,
                %status,
                error = %err,
                "Failed to read response body as text"
            );
        })?;

        let json: RpcResponse<T> = serde_json::from_str(&response_text)
            .inspect_err(|err| {
                tracing::error!(
                    url = %self.config.url,
                    method = %method,
                    %status,
                    response_body = %response_text,
                    error = %err,
                    "Failed to parse response as JSON"
                );
            })?;

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

    /// `getrawtransaction <txid> true`.
    pub fn get_transaction(
        &self,
        txid: &str,
    ) -> Result<TransactionInfo, Error> {
        self.call::<TransactionInfo>("getrawtransaction", json!([txid, true]))
    }

    /// The `chain` field of `getblockchaininfo`, lowercased.
    pub fn get_blockchain_chain_name(&self) -> Result<String, Error> {
        let info: serde_json::Value =
            self.call("getblockchaininfo", json!([]))?;
        let chain = info
            .get("chain")
            .and_then(|value| value.as_str())
            .ok_or(Error::InvalidResponse)?;
        Ok(chain.to_lowercase())
    }

    /// Transaction IDs touching `address`, via `listunspent`.
    ///
    /// This only sees **unspent** outputs and only for addresses the node
    /// watches, so a fill that has since been spent becomes invisible. That
    /// limitation is inherent to discovering payments this way and predates the
    /// trait.
    fn list_transactions(&self, address: &str) -> Result<Vec<String>, Error> {
        let unspent: Vec<serde_json::Value> =
            self.call("listunspent", json!([0, 999999, [address]]))?;

        let mut txids = std::collections::HashSet::new();
        for utxo in unspent {
            if let Some(txid) = utxo.get("txid").and_then(|v| v.as_str()) {
                txids.insert(txid.to_string());
            }
        }

        // Not all nodes support this; the result is unused, but calling it can
        // populate the node's internal index.
        let _result: Result<f64, _> =
            self.call("getreceivedbyaddress", json!([address, 0]));

        Ok(txids.into_iter().collect())
    }

    /// Resolve the sender as the address funding the transaction's first input.
    fn sender_of(&self, tx: &TransactionInfo) -> Option<String> {
        let vin = tx.vin.first()?;
        let (input_txid, input_vout) = (vin.txid.as_ref()?, vin.vout?);
        let input_tx = self.get_transaction(input_txid).ok()?;
        let prevout = input_tx.vout.get(input_vout as usize)?;
        prevout.script_pub_key.address.clone().or_else(|| {
            prevout
                .script_pub_key
                .addresses
                .as_ref()
                .and_then(|addrs| addrs.first().cloned())
        })
    }

    /// Build a payment from a fetched transaction, without resolving the sender.
    fn to_payment(
        &self,
        tx: &TransactionInfo,
        query: &PaymentQuery,
    ) -> L1Payment {
        let decimals = self.chain.decimals();
        let matched = tx.vout.iter().find_map(|vout| {
            let base_units = coins_to_base_units(vout.value, decimals);
            (pays_address(vout, &query.address) && base_units == query.amount)
                .then_some(base_units)
        });
        let txid = SwapTxId::from_hex_rpc(&tx.txid)
            .unwrap_or_else(|_| SwapTxId::from_bytes(tx.txid.as_bytes()));
        L1Payment {
            txid_display: txid.display_for_chain(self.chain),
            txid,
            amount: matched.unwrap_or(0),
            matches_query: matched.is_some(),
            sender: None,
            // Bitcoin-family chains measure finality and age with the same
            // quantity, so both come from the confirmation count.
            confirmations: tx.confirmations,
            age: u64::from(tx.confirmations),
            included: tx.blockheight.is_some(),
            height: tx.blockheight.map(u64::from),
        }
    }
}

impl ParentChainClient for BitcoinCoreClient {
    fn identify(&self) -> Result<ChainIdentity, Error> {
        let raw = self.get_blockchain_chain_name()?;
        // `main` is reported by Bitcoin, Bitcoin Cash and Litecoin alike, so it
        // cannot distinguish between them on its own. Phase 3 of
        // docs/PARENT_CHAIN_ROADMAP.md adds genesis/checkpoint probing; until
        // then a `main` node is taken at its configured word.
        let chain = match raw.as_str() {
            "main"
                if matches!(
                    self.chain,
                    ParentChainType::BTC
                        | ParentChainType::BCH
                        | ParentChainType::LTC
                ) =>
            {
                self.chain
            }
            "signet" => ParentChainType::Signet,
            "regtest" => ParentChainType::Regtest,
            "testnet4" | "test4" => ParentChainType::BCH,
            _ => {
                return Err(Error::ChainMismatch {
                    expected: self.chain,
                    chain: raw,
                });
            }
        };
        Ok(ChainIdentity { chain, raw })
    }

    fn tip(&self) -> Result<u64, Error> {
        let info: serde_json::Value =
            self.call("getblockchaininfo", json!([]))?;
        info.get("blocks")
            .and_then(|value| value.as_u64())
            .ok_or(Error::InvalidResponse)
    }

    fn find_payments(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error> {
        let mut matches = Vec::new();
        for txid in self.list_transactions(&query.address)? {
            let tx = match self.get_transaction(&txid) {
                Ok(tx) => tx,
                // A spent or unknown transaction is simply not a candidate.
                Err(Error::TransactionNotFound) => continue,
                Err(err) => {
                    tracing::warn!(
                        %txid,
                        error = %err,
                        "Error getting transaction while scanning for payments"
                    );
                    continue;
                }
            };
            let mut payment = self.to_payment(&tx, query);
            if !payment.matches_query {
                continue;
            }
            payment.sender = self.sender_of(&tx);
            matches.push(payment);
        }
        Ok(matches)
    }

    fn get_payment(
        &self,
        txid: &SwapTxId,
        query: &PaymentQuery,
    ) -> Result<Option<L1Payment>, Error> {
        // `display_for_chain` is `to_hex()` here, which is the form the existing
        // pollers already send. Note that it disagrees with the byte order the
        // automatic detection path stores via `from_hex_rpc` — see the txid
        // byte-order note in docs/PARENT_CHAIN_ROADMAP.md. Deliberately
        // preserved rather than "fixed" here, since changing it changes which
        // stored swaps can be looked up.
        match self.get_transaction(&txid.display_for_chain(self.chain)) {
            Ok(tx) => {
                let mut payment = self.to_payment(&tx, query);
                // Report the txid the caller asked about, so it can match the
                // payment back to its swap regardless of byte order.
                payment.txid_display = txid.display_for_chain(self.chain);
                payment.txid = txid.clone();
                Ok(Some(payment))
            }
            Err(Error::TransactionNotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(chain: ParentChainType) -> BitcoinCoreClient {
        BitcoinCoreClient::new(
            chain,
            L1ChainConfig::basic("http://localhost:18443", "u", "p"),
        )
    }

    fn query(address: &str, amount: u64) -> PaymentQuery {
        PaymentQuery {
            address: address.to_string(),
            amount,
            required_confirmations: 3,
            max_age: 500,
        }
    }

    fn parse(raw: &str) -> TransactionInfo {
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn coins_to_base_units_rounds_instead_of_truncating() {
        // 0.29 is not representable in binary floating point; multiplying by
        // 1e8 gives 28999999.999999996, which truncation turns into a value one
        // satoshi short of the real amount.
        assert_eq!(coins_to_base_units(0.29, 8), 29_000_000);
        assert_eq!((0.29f64 * 100_000_000.0) as u64, 28_999_999);

        assert_eq!(coins_to_base_units(0.0, 8), 0);
        assert_eq!(coins_to_base_units(1.0, 8), 100_000_000);
        assert_eq!(coins_to_base_units(0.00000001, 8), 1);
        assert_eq!(coins_to_base_units(21_000_000.0, 8), 2_100_000_000_000_000);
        // Decimals come from the chain, not a constant.
        assert_eq!(coins_to_base_units(1.5, 9), 1_500_000_000);
        assert_eq!(coins_to_base_units(1.5, 6), 1_500_000);
    }

    #[test]
    fn matches_an_exact_payment_to_the_address() {
        let tx = parse(
            r#"{
                "txid": "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace",
                "confirmations": 4,
                "blockheight": 120,
                "vin": [],
                "vout": [
                    {"value": 0.001, "scriptPubKey": {"address": "someone-else"}},
                    {"value": 0.29, "scriptPubKey": {"address": "alice"}}
                ]
            }"#,
        );
        let payment = client(ParentChainType::Regtest)
            .to_payment(&tx, &query("alice", 29_000_000));
        assert!(payment.matches_query);
        assert_eq!(payment.amount, 29_000_000);
        assert_eq!(payment.confirmations, 4);
        assert_eq!(payment.age, 4);
        assert!(payment.included);
        assert_eq!(payment.height, Some(120));
        // Canonical order for display, RPC order on the wire.
        assert_eq!(
            payment.txid_display,
            "ceaa5bbe14a2fe2658115f32ea90a11c073a5028df5713adbcdb35c70c3e9127"
        );
    }

    #[test]
    fn accepts_the_legacy_addresses_array() {
        let tx = parse(
            r#"{
                "txid": "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace",
                "confirmations": 1,
                "blockheight": 5,
                "vin": [],
                "vout": [
                    {"value": 0.5, "scriptPubKey": {"addresses": ["bob", "alice"]}}
                ]
            }"#,
        );
        let payment = client(ParentChainType::BCH)
            .to_payment(&tx, &query("alice", 50_000_000));
        assert!(payment.matches_query);
    }

    #[test]
    fn rejects_wrong_amount_or_wrong_address() {
        let tx = parse(
            r#"{
                "txid": "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace",
                "confirmations": 9,
                "blockheight": 30,
                "vin": [],
                "vout": [{"value": 0.5, "scriptPubKey": {"address": "alice"}}]
            }"#,
        );
        let client = client(ParentChainType::Regtest);
        assert!(
            !client
                .to_payment(&tx, &query("alice", 49_999_999))
                .matches_query
        );
        assert!(
            !client
                .to_payment(&tx, &query("bob", 50_000_000))
                .matches_query
        );
        // A non-matching transaction still reports zero, not the paid amount.
        assert_eq!(client.to_payment(&tx, &query("bob", 50_000_000)).amount, 0);
    }

    #[test]
    fn mempool_transaction_is_not_included() {
        let tx = parse(
            r#"{
                "txid": "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace",
                "confirmations": 0,
                "vin": [],
                "vout": [{"value": 0.5, "scriptPubKey": {"address": "alice"}}]
            }"#,
        );
        let payment = client(ParentChainType::Regtest)
            .to_payment(&tx, &query("alice", 50_000_000));
        assert!(payment.matches_query, "it does pay the right amount");
        assert!(!payment.included, "but it is not in a block");
        assert!(
            !payment.is_acceptable_for(&query("alice", 50_000_000)),
            "so it must not be accepted as a fill"
        );
    }

    #[test]
    fn age_and_finality_are_separate_checks() {
        let q = query("alice", 50_000_000);
        let tx = parse(
            r#"{
                "txid": "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace",
                "confirmations": 501,
                "blockheight": 30,
                "vin": [],
                "vout": [{"value": 0.5, "scriptPubKey": {"address": "alice"}}]
            }"#,
        );
        let payment = client(ParentChainType::Regtest).to_payment(&tx, &q);
        assert!(payment.is_final_for(&q), "deep enough to be final");
        assert!(
            !payment.is_acceptable_for(&q),
            "but older than max_age, so it must be rejected"
        );
    }
}
