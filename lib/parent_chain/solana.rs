//! [`ParentChainClient`] for Solana.
//!
//! Solana is the first parent chain that is not a Bitcoin fork, and it differs
//! in every way the trait was designed to absorb:
//!
//! - **No UTXOs.** A payment is a *balance delta* on an account, not an output
//!   paying an address. We diff `preBalances`/`postBalances` rather than parsing
//!   instructions, which is correct for plain transfers, CPI transfers and
//!   multi-transfer transactions alike.
//! - **No confirmation depth.** Finality is a commitment level — `processed`,
//!   `confirmed`, `finalized` — so the depth reported to the swap logic is
//!   *synthesized* (see [`ladder`]). Age is measured in slots instead, which is
//!   why [`L1Payment`] carries the two separately.
//! - **64-byte base58 signatures**, not 32-byte reversed hex txids.
//! - **JSON-RPC 2.0** and no HTTP basic auth; hosted providers take an API key
//!   in a header or query parameter.
//!
//! # Rate limits
//!
//! The public endpoints are strict — roughly 100 requests per 10s per IP, with
//! `getSignaturesForAddress` among the most throttled — and a naive per-swap
//! poll gets 429'd quickly. Three things keep the traffic down: a minimum
//! interval between requests, a per-address cursor so each poll only asks for
//! signatures newer than the last one seen, and treating 429 as "no information"
//! rather than an error worth retrying immediately.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{ChainIdentity, Error, L1Payment, ParentChainClient, PaymentQuery};
use crate::{
    l1::config::{L1Auth, L1ChainConfig},
    types::{ParentChainType, SwapTxId},
};

/// Signatures to ask for per poll. Kept small because the cursor means we only
/// ever need the ones since last time.
const SIGNATURE_PAGE: u64 = 25;

/// Minimum gap between requests to one endpoint.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(120);

/// The name reported for any Solana cluster.
///
/// Solana has no `getblockchaininfo`-style network name, so identity rests
/// entirely on the genesis hash — which, unlike for Bitcoin Cash, is exact.
pub const SOLANA_NETWORK_NAME: &str = "solana";

/// Map a commitment level onto a confirmation depth.
///
/// [`crate::types::SwapState::WaitingConfirmations`] is Borsh-encoded into the
/// database and `required_confirmations` is part of the block body, so neither
/// can change shape to express a commitment level. Synthesizing a depth keeps
/// both intact.
///
/// The mapping never reaches `required` before the transaction is genuinely
/// finalized, including when `required` is 1 — in that case `confirmed` reports
/// 0 and the swap is not even bound to the transaction yet.
fn ladder(commitment: Option<&str>, required: u32) -> u32 {
    match commitment {
        Some("finalized") => required,
        Some("confirmed") => required.saturating_sub(1).min(1),
        // `processed`, unknown, or absent: not usable.
        _ => 0,
    }
}

/// Lamports credited to `address` by this transaction, if it is a usable
/// payment to it.
///
/// Returns `None` when the transaction failed, does not touch the address, or
/// the address is the fee payer — in that last case the transaction fee is
/// folded into the balance delta, so no exact amount can be attributed.
fn credited_lamports(tx: &Value, address: &str) -> Option<u64> {
    let meta = tx.get("meta")?;
    if !meta.get("err").is_none_or(Value::is_null) {
        return None;
    }
    let keys = tx
        .get("transaction")?
        .get("message")?
        .get("accountKeys")?
        .as_array()?;
    // `jsonParsed` yields objects; other encodings yield plain strings.
    let index = keys.iter().position(|key| {
        key.as_str().or_else(|| key.get("pubkey")?.as_str()) == Some(address)
    })?;
    if index == 0 {
        return None;
    }
    let pre = meta.get("preBalances")?.as_array()?.get(index)?.as_u64()?;
    let post = meta.get("postBalances")?.as_array()?.get(index)?.as_u64()?;
    post.checked_sub(pre).filter(|credited| *credited > 0)
}

/// Best-effort sender: the fee payer, which is `accountKeys[0]`.
fn fee_payer(tx: &Value) -> Option<String> {
    let key = tx
        .get("transaction")?
        .get("message")?
        .get("accountKeys")?
        .as_array()?
        .first()?;
    key.as_str()
        .or_else(|| key.get("pubkey")?.as_str())
        .map(str::to_string)
}

pub struct SolanaClient {
    chain: ParentChainType,
    config: L1ChainConfig,
    client: reqwest::Client,
    /// Earliest time the next request may be sent.
    next_request_at: Mutex<Instant>,
    /// Newest signature already seen per address, used as `until:`.
    cursors: Mutex<HashMap<String, String>>,
}

impl SolanaClient {
    pub fn new(chain: ParentChainType, config: L1ChainConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout())
            .build()
            .expect("Failed to create HTTP client");
        Self {
            chain,
            config,
            client,
            next_request_at: Mutex::new(Instant::now()),
            cursors: Mutex::new(HashMap::new()),
        }
    }

    /// Wait until this endpoint may be called again.
    async fn pace(&self) {
        let sleep_for = {
            let mut next = self.next_request_at.lock().await;
            let now = Instant::now();
            let wait = next.saturating_duration_since(now);
            *next = now.max(*next) + MIN_REQUEST_INTERVAL;
            wait
        };
        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
    }

    async fn call<T: for<'de> serde::Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, Error> {
        self.pace().await;
        let request = json!({
            "jsonrpc": "2.0",
            "id": "coinshift",
            "method": method,
            "params": params,
        });
        let builder = apply_auth(
            self.client.post(&self.config.url).json(&request),
            &self.config.auth,
        );
        let response = builder.send().await?;

        // A rate-limited request tells us nothing about the chain; report it as
        // an ordinary failure so the caller leaves the swap where it is.
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unspecified")
                .to_string();
            tracing::warn!(
                url = %self.config.url,
                %method,
                %retry_after,
                "Solana endpoint rate-limited this request"
            );
            return Err(Error::Rpc(format!(
                "rate limited (retry after {retry_after})"
            )));
        }

        let body: Value = response.json().await?;
        if let Some(error) = body.get("error").filter(|err| !err.is_null()) {
            return Err(Error::Rpc(error.to_string()));
        }
        let result = body.get("result").ok_or(Error::InvalidResponse)?;
        serde_json::from_value(result.clone()).map_err(Error::from)
    }

    /// Signature records for `address` newer than the last poll.
    async fn recent_signatures(
        &self,
        address: &str,
    ) -> Result<Vec<Value>, Error> {
        let until = self.cursors.lock().await.get(address).cloned();
        let mut options = json!({ "limit": SIGNATURE_PAGE });
        if let Some(until) = &until {
            options["until"] = json!(until);
        }
        let signatures: Vec<Value> = self
            .call("getSignaturesForAddress", json!([address, options]))
            .await?;

        // The response is newest-first, so the first entry becomes the cursor.
        if let Some(newest) =
            signatures.first().and_then(|entry| entry.get("signature"))
            && let Some(newest) = newest.as_str()
        {
            self.cursors
                .lock()
                .await
                .insert(address.to_string(), newest.to_string());
        }
        Ok(signatures)
    }

    async fn get_transaction(
        &self,
        signature: &str,
    ) -> Result<Option<Value>, Error> {
        let tx: Value = self
            .call(
                "getTransaction",
                json!([
                    signature,
                    {
                        "encoding": "jsonParsed",
                        "maxSupportedTransactionVersion": 0,
                        "commitment": "confirmed",
                    }
                ]),
            )
            .await?;
        Ok((!tx.is_null()).then_some(tx))
    }

    /// Build a payment from a signature record plus its transaction.
    fn to_payment(
        &self,
        signature: &str,
        commitment: Option<&str>,
        slot: u64,
        tip: u64,
        tx: &Value,
        query: &PaymentQuery,
    ) -> Option<L1Payment> {
        let credited = credited_lamports(tx, &query.address);
        let txid = SwapTxId::from_base58(signature).ok()?;
        let confirmations = ladder(commitment, query.required_confirmations);
        Some(L1Payment {
            txid_display: signature.to_string(),
            txid,
            amount: credited.unwrap_or(0),
            matches_query: credited == Some(query.amount),
            sender: fee_payer(tx),
            confirmations,
            // Age is measured in slots; a tip behind the transaction (possible
            // across endpoints) is clamped rather than wrapping.
            age: tip.saturating_sub(slot),
            included: commitment.is_some(),
            height: Some(slot),
        })
    }
}

fn apply_auth(
    builder: reqwest::RequestBuilder,
    auth: &L1Auth,
) -> reqwest::RequestBuilder {
    match auth {
        L1Auth::None => builder,
        // Solana endpoints do not use basic auth, but honour it rather than
        // silently ignoring a configured credential.
        L1Auth::Basic { user, password } => {
            builder.basic_auth(user, Some(password))
        }
        L1Auth::Bearer { token } => builder.bearer_auth(token),
        L1Auth::Header { name, value } => builder.header(name, value),
        L1Auth::QueryParam { name, value } => builder.query(&[(name, value)]),
    }
}

#[async_trait::async_trait]
impl ParentChainClient for SolanaClient {
    async fn identify(&self) -> Result<ChainIdentity, Error> {
        let genesis: String = self.call("getGenesisHash", json!([])).await?;
        Ok(ChainIdentity {
            chain_name: SOLANA_NETWORK_NAME.to_string(),
            genesis: Some(genesis),
        })
    }

    async fn tip(&self) -> Result<u64, Error> {
        self.call("getSlot", json!([{ "commitment": "finalized" }]))
            .await
    }

    async fn find_payments(
        &self,
        query: &PaymentQuery,
    ) -> Result<Vec<L1Payment>, Error> {
        let tip = self.tip().await?;
        let signatures = self.recent_signatures(&query.address).await?;
        let mut payments = Vec::new();
        for entry in signatures {
            // A failed transaction never paid anyone.
            if !entry.get("err").is_none_or(Value::is_null) {
                continue;
            }
            let Some(signature) =
                entry.get("signature").and_then(Value::as_str)
            else {
                continue;
            };
            let slot = entry.get("slot").and_then(Value::as_u64).unwrap_or(0);
            let commitment =
                entry.get("confirmationStatus").and_then(Value::as_str);
            let tx = match self.get_transaction(signature).await {
                Ok(Some(tx)) => tx,
                Ok(None) => continue,
                Err(err) => {
                    tracing::debug!(
                        %signature,
                        error = %err,
                        "Could not fetch Solana transaction while scanning"
                    );
                    continue;
                }
            };
            if let Some(payment) = self
                .to_payment(signature, commitment, slot, tip, &tx, query)
                .filter(|payment| payment.matches_query)
            {
                payments.push(payment);
            }
        }
        Ok(payments)
    }

    async fn get_payment(
        &self,
        txid: &SwapTxId,
        query: &PaymentQuery,
    ) -> Result<Option<L1Payment>, Error> {
        let signature = txid.display_for_chain(self.chain);
        // Cheap status first: it carries the commitment level, which
        // getTransaction does not.
        let statuses: Value = self
            .call(
                "getSignatureStatuses",
                json!([[&signature], { "searchTransactionHistory": true }]),
            )
            .await?;
        let status = statuses
            .get("value")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .filter(|status| !status.is_null());
        let Some(status) = status else {
            return Ok(None);
        };
        let commitment =
            status.get("confirmationStatus").and_then(Value::as_str);
        let slot = status.get("slot").and_then(Value::as_u64).unwrap_or(0);

        let Some(tx) = self.get_transaction(&signature).await? else {
            return Ok(None);
        };
        let tip = self.tip().await?;
        Ok(self.to_payment(&signature, commitment, slot, tip, &tx, query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(address: &str, amount: u64) -> PaymentQuery {
        PaymentQuery {
            address: address.to_string(),
            amount,
            required_confirmations: 2,
            max_age: 432_000,
        }
    }

    fn client() -> SolanaClient {
        SolanaClient::new(
            ParentChainType::SolanaDevnet,
            L1ChainConfig::basic("https://api.devnet.solana.com", "", ""),
        )
    }

    /// A transfer of `lamports` to `alice`, who is not the fee payer.
    fn transfer_tx() -> Value {
        json!({
            "meta": {
                "err": null,
                "preBalances": [1_000_000_000u64, 5_000_000u64],
                "postBalances": [994_995_000u64, 10_000_000u64]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "payer", "signer": true},
                        {"pubkey": "alice", "signer": false}
                    ]
                }
            }
        })
    }

    /// Talks to the public devnet endpoint. Ignored by default so the suite
    /// stays offline; run with `--ignored` to check the adapter end to end.
    #[tokio::test]
    #[ignore]
    async fn devnet_identify_and_tip() {
        let client = client();
        let identity = client.identify().await.unwrap();
        assert_eq!(identity.chain_name, SOLANA_NETWORK_NAME);
        assert_eq!(
            identity.genesis.as_deref(),
            Some(crate::l1::identity::SOLANA_DEVNET_GENESIS)
        );
        // And the registry's identity check accepts it.
        assert_eq!(
            crate::l1::identity::verify(
                ParentChainType::SolanaDevnet,
                &identity,
                None
            ),
            Ok(())
        );
        // Pointing a mainnet swap at devnet must be rejected.
        assert!(
            crate::l1::identity::verify(
                ParentChainType::Solana,
                &identity,
                None
            )
            .is_err()
        );
        assert!(client.tip().await.unwrap() > 0);
    }

    #[test]
    fn ladder_never_reports_final_before_finalized() {
        // required = 2: confirmed shows progress, finalized completes.
        assert_eq!(ladder(Some("finalized"), 2), 2);
        assert_eq!(ladder(Some("confirmed"), 2), 1);
        assert_eq!(ladder(Some("processed"), 2), 0);
        assert_eq!(ladder(None, 2), 0);

        // required = 1 is the case that would be easy to get wrong: an
        // optimistically-confirmed transaction must NOT already count as final.
        assert_eq!(ladder(Some("confirmed"), 1), 0);
        assert_eq!(ladder(Some("finalized"), 1), 1);

        // Monotone in commitment for every requirement, which is what
        // `update_swap_confirmations` relies on.
        for required in 1..=10 {
            assert!(
                ladder(Some("processed"), required)
                    <= ladder(Some("confirmed"), required)
            );
            assert!(
                ladder(Some("confirmed"), required)
                    <= ladder(Some("finalized"), required)
            );
        }
    }

    #[test]
    fn credited_lamports_uses_the_balance_delta() {
        let tx = transfer_tx();
        assert_eq!(credited_lamports(&tx, "alice"), Some(5_000_000));
        assert_eq!(credited_lamports(&tx, "nobody"), None);
    }

    #[test]
    fn the_fee_payer_is_not_a_valid_recipient() {
        // The payer's delta includes the transaction fee, so no exact amount
        // can be attributed to a payment.
        let tx = transfer_tx();
        assert_eq!(credited_lamports(&tx, "payer"), None);
    }

    #[test]
    fn a_failed_transaction_credits_nobody() {
        let mut tx = transfer_tx();
        tx["meta"]["err"] = json!({"InstructionError": [0, "Custom"]});
        assert_eq!(credited_lamports(&tx, "alice"), None);
    }

    #[test]
    fn plain_string_account_keys_are_accepted() {
        // Encodings other than jsonParsed give bare pubkey strings.
        let mut tx = transfer_tx();
        tx["transaction"]["message"]["accountKeys"] = json!(["payer", "alice"]);
        assert_eq!(credited_lamports(&tx, "alice"), Some(5_000_000));
        assert_eq!(fee_payer(&tx).as_deref(), Some("payer"));
    }

    #[test]
    fn a_debit_is_not_a_payment() {
        let mut tx = transfer_tx();
        tx["meta"]["postBalances"] = json!([994_995_000u64, 1_000_000u64]);
        assert_eq!(credited_lamports(&tx, "alice"), None);
    }

    #[test]
    fn payment_matches_only_the_exact_amount() {
        let client = client();
        let tx = transfer_tx();
        let signature = bitcoin::base58::encode(&[3u8; 64]);

        let exact = client
            .to_payment(
                &signature,
                Some("finalized"),
                100,
                150,
                &tx,
                &query("alice", 5_000_000),
            )
            .unwrap();
        assert!(exact.matches_query);
        assert_eq!(exact.amount, 5_000_000);
        assert_eq!(exact.confirmations, 2, "finalized meets required");
        assert_eq!(exact.age, 50, "age is measured in slots");
        assert_eq!(exact.txid_display, signature, "base58, never reversed");
        assert_eq!(exact.sender.as_deref(), Some("payer"));

        let wrong = client
            .to_payment(
                &signature,
                Some("finalized"),
                100,
                150,
                &tx,
                &query("alice", 5_000_001),
            )
            .unwrap();
        assert!(!wrong.matches_query);
    }

    #[test]
    fn a_confirmed_payment_is_acceptable_but_not_final() {
        let client = client();
        let q = query("alice", 5_000_000);
        let payment = client
            .to_payment(
                &bitcoin::base58::encode(&[4u8; 64]),
                Some("confirmed"),
                100,
                150,
                &transfer_tx(),
                &q,
            )
            .unwrap();
        assert!(payment.is_acceptable_for(&q), "in a slot and recent enough");
        assert!(
            !payment.is_final_for(&q),
            "but optimistic confirmation must not be treated as final"
        );
    }

    #[test]
    fn an_old_payment_is_rejected_on_age_not_finality() {
        let client = client();
        let q = query("alice", 5_000_000);
        let payment = client
            .to_payment(
                &bitcoin::base58::encode(&[5u8; 64]),
                Some("finalized"),
                1,
                q.max_age + 2,
                &transfer_tx(),
                &q,
            )
            .unwrap();
        assert!(payment.is_final_for(&q), "deeply final");
        assert!(
            !payment.is_acceptable_for(&q),
            "yet older than max_age, so unusable as a fill"
        );
    }
}
