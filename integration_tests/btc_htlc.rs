//! The Bitcoin leg of an atomic swap, against a real regtest `bitcoind`.
//!
//! Everything else about the HTLC is checked by unit tests that assert on the
//! shapes we produce — the script has the right opcodes, the witness has the
//! right items, the refund sets a lock time. All of that is us marking our own
//! homework. Bitcoin either accepts the spend or it does not, and only bitcoind
//! can say which.
//!
//! So this funds the contract for real and spends it both ways:
//!
//! - **Claim** with the preimage, accepted immediately.
//! - **Refund** rejected before the timeout, then accepted after it.
//!
//! The rejection matters as much as the acceptances. A refund path that could
//! be taken early would let the maker reclaim their Bitcoin while the taker
//! still believed the swap was live — and since consensus on neither chain can
//! see the other leg, nothing would catch it.

use bip300301_enforcer_integration_tests::{
    setup::PostSetup as EnforcerPostSetup,
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use bip300301_enforcer_lib::bins::CommandExt as _;
use bitcoin::{Amount, Network, consensus::encode::serialize_hex};
use coinshift::htlc::{HtlcParams, Secret, SpendPath, SpendRequest};
use futures::{
    FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture,
};
use tracing::Instrument as _;

use crate::util::BinPaths;

const FUND: Amount = Amount::from_sat(1_000_000);
const FEE: Amount = Amount::from_sat(10_000);

/// Run a `bitcoin-cli` command and return its stdout.
async fn cli(
    enforcer: &EnforcerPostSetup,
    cmd: &str,
    args: Vec<String>,
) -> anyhow::Result<String> {
    let out = enforcer
        .bitcoin_cli
        .command::<String, _, _, _, _>([], cmd, args)
        .run_utf8()
        .await?;
    Ok(out.trim().to_string())
}

fn keypair(byte: u8) -> (bitcoin::secp256k1::SecretKey, bitcoin::PublicKey) {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let sk = bitcoin::secp256k1::SecretKey::from_slice(&[byte; 32])
        .expect("valid secret key");
    (sk, bitcoin::PublicKey::new(sk.public_key(&secp)))
}

/// Send `FUND` to `address`, mine it in, and return the funding outpoint.
///
/// Sets an explicit fee rate first: regtest has no fee history to estimate
/// from and `-fallbackfee` is off, so `sendtoaddress` otherwise fails with
/// "Fee estimation failed" before it ever looks at our address.
async fn fund(
    enforcer: &EnforcerPostSetup,
    address: &str,
) -> anyhow::Result<bitcoin::OutPoint> {
    let () = cli(enforcer, "settxfee", vec!["0.0001".to_string()])
        .await
        .map(|_| ())?;
    let txid_hex = cli(
        enforcer,
        "sendtoaddress",
        vec![address.to_string(), FUND.to_btc().to_string()],
    )
    .await?;
    let mining_address = enforcer.mining_address.to_string();
    let () = cli(
        enforcer,
        "generatetoaddress",
        vec!["1".to_string(), mining_address],
    )
    .await
    .map(|_| ())?;

    // Find which output pays our contract; bitcoind picks the change vout.
    let raw =
        cli(enforcer, "getrawtransaction", vec![txid_hex.clone()]).await?;
    let tx: bitcoin::Transaction =
        bitcoin::consensus::encode::deserialize_hex(&raw)?;
    let expected = address.parse::<bitcoin::Address<_>>()?.assume_checked();
    let vout = tx
        .output
        .iter()
        .position(|out| out.script_pubkey == expected.script_pubkey())
        .ok_or_else(|| {
            anyhow::anyhow!("funding tx pays nothing to the contract")
        })?;
    Ok(bitcoin::OutPoint {
        txid: tx.compute_txid(),
        vout: vout as u32,
    })
}

async fn btc_htlc_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(
            &bin_paths,
            res_tx.clone(),
            "btc-htlc",
        )
        .await?;

    let height: u32 = cli(&enforcer_post_setup, "getblockcount", vec![])
        .await?
        .parse()?;
    let (claim_sk, claim_pk) = keypair(11);
    let (refund_sk, refund_pk) = keypair(22);
    let secret = Secret::random();

    // A deadline close enough to reach by mining, but not yet passed.
    let timeout_height = height + 10;
    let params = HtlcParams {
        hash: secret.hash(),
        claim_pubkey: claim_pk,
        refund_pubkey: refund_pk,
        timeout_height,
    };
    let contract = params.address(Network::Regtest).to_string();
    tracing::info!(%contract, timeout_height, "built the HTLC");

    // bitcoind is the authority on whether our script is even well formed.
    let decoded = cli(
        &enforcer_post_setup,
        "decodescript",
        vec![hex::encode(params.witness_script().as_bytes())],
    )
    .await?;
    anyhow::ensure!(
        decoded.contains("OP_SHA256")
            && decoded.contains("OP_CHECKLOCKTIMEVERIFY"),
        "bitcoind should recognise both branches: {decoded}"
    );

    let payout = cli(&enforcer_post_setup, "getnewaddress", vec![])
        .await?
        .parse::<bitcoin::Address<_>>()?
        .assume_checked();

    // ---- 1. claim with the preimage ----
    let outpoint = fund(&enforcer_post_setup, &contract).await?;
    let claim = params.spend_transaction(
        SpendPath::Claim,
        SpendRequest {
            outpoint,
            value: FUND,
            to: &payout,
            fee: FEE,
        },
        &claim_sk,
        Some(&secret),
    )?;
    let claim_txid = cli(
        &enforcer_post_setup,
        "sendrawtransaction",
        vec![serialize_hex(&claim)],
    )
    .await?;
    tracing::info!(%claim_txid, "bitcoind accepted the claim");

    // ---- 2. a second contract, refunded ----
    let outpoint = fund(&enforcer_post_setup, &contract).await?;
    let refund = params.spend_transaction(
        SpendPath::Refund,
        SpendRequest {
            outpoint,
            value: FUND,
            to: &payout,
            fee: FEE,
        },
        &refund_sk,
        None,
    )?;
    let raw_refund = serialize_hex(&refund);

    // Before the deadline it must be refused — this is the property that keeps
    // the maker from reclaiming while the taker still thinks the swap is live.
    let early = cli(
        &enforcer_post_setup,
        "sendrawtransaction",
        vec![raw_refund.clone()],
    )
    .await;
    anyhow::ensure!(
        early.is_err(),
        "a refund before the timeout must be rejected, got {early:?}"
    );
    tracing::info!("bitcoind rejected the early refund, as it should");

    // Mine past the deadline, then it must be accepted.
    let mining_address = enforcer_post_setup.mining_address.to_string();
    loop {
        let now: u32 = cli(&enforcer_post_setup, "getblockcount", vec![])
            .await?
            .parse()?;
        if now >= timeout_height {
            break;
        }
        let () = cli(
            &enforcer_post_setup,
            "generatetoaddress",
            vec!["1".to_string(), mining_address.clone()],
        )
        .await
        .map(|_| ())?;
    }
    let refund_txid =
        cli(&enforcer_post_setup, "sendrawtransaction", vec![raw_refund])
            .await?;
    tracing::info!(%refund_txid, "bitcoind accepted the refund after the deadline");

    tracing::info!(
        "BTC HTLC passed: claimed with the preimage, refused early, refunded \
         after the deadline"
    );
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn btc_htlc_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "btc_htlc",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res = btc_htlc_task(bin_paths, res_tx.clone()).await;
                    drop(res_tx.unbounded_send(res));
                }
                .in_current_span()
            })
            .into();
            res_rx.next().await.ok_or_else(|| {
                anyhow::anyhow!("Unexpected end of test task result stream")
            })?
        }
        .boxed(),
        file_registry,
        failure_collector,
    )
}
