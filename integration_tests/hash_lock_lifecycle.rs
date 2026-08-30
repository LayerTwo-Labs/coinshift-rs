//! The Coinshift leg of an atomic swap, end to end on regtest.
//!
//! Covers both ways a hash lock can end, because a swap is only safe if both
//! are true:
//!
//! - **Claim.** The taker reveals the secret and is paid. This is the happy
//!   path, and it is also the only evidence that the commitment we publish is
//!   the one our own spend rules check.
//! - **Refund.** The deadline passes and the escrow returns to the maker. This
//!   is what makes a silent counterparty merely annoying instead of expensive.
//!
//! It also asserts the two rejections that carry the security property: a wrong
//! preimage buys nothing, and a refund before the deadline buys nothing. Those
//! matter more than the happy path — a swap system that pays out correctly but
//! also pays out incorrectly is not a swap system.
//!
//! What this does **not** cover is the Bitcoin leg. Nothing here funds or
//! spends a real HTLC; `lib/htlc.rs` builds the script and the address, and
//! `a_secret_that_opens_the_bitcoin_leg_opens_ours` checks the two legs agree
//! on the commitment, but a full cross-chain run needs a funded Bitcoin wallet
//! and is the next piece of work.

use bip300301_enforcer_integration_tests::{
    integration_test::deposit,
    setup::{PostSetup as EnforcerPostSetup, Sidechain as _},
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use coinshift_app_rpc_api::RpcClient as _;
use futures::{
    FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture,
};
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{setup::PostSetup, util::BinPaths};

const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(21_000_000);
const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000_000);
const LOCK_VALUE: u64 = 5_000_000;
const FEE: u64 = 1_000;

/// The secret, and the commitment both legs would share.
fn secret_and_commitment() -> ([u8; 32], String) {
    use bitcoin::hashes::{Hash as _, sha256};
    let secret = [0x5Au8; 32];
    let commitment = sha256::Hash::hash(&secret).to_byte_array();
    (secret, hex::encode(commitment))
}

/// Mine a block and let the node settle.
async fn advance(
    sidechain: &mut PostSetup,
    enforcer: &mut EnforcerPostSetup,
) -> anyhow::Result<()> {
    sidechain.bmm_single(enforcer).await?;
    sleep(std::time::Duration::from_millis(500)).await;
    Ok(())
}

/// The outpoint of the single hash lock this wallet holds.
async fn sole_lock(
    sidechain: &PostSetup,
) -> anyhow::Result<coinshift_app_rpc_api::HashLockInfo> {
    let locks = sidechain.rpc_client.list_hash_locks().await?;
    anyhow::ensure!(
        locks.len() == 1,
        "expected exactly one hash lock, found {}",
        locks.len()
    );
    Ok(locks.into_iter().next().expect("length checked"))
}

async fn hash_lock_lifecycle_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(
            &bin_paths,
            res_tx.clone(),
            "hash-lock-lifecycle",
        )
        .await?;

    let deposit_address = sidechain.get_deposit_address().await?;
    let () = deposit(
        &mut enforcer_post_setup,
        &mut sidechain,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("funded the wallet");

    let (secret, commitment_hex) = secret_and_commitment();
    let claimant = sidechain.rpc_client.get_new_address().await?;

    // ---- the deadline pair is derived, not chosen ----
    let height = sidechain.rpc_client.getblockcount().await?;
    let deadlines = sidechain
        .rpc_client
        .suggest_swap_deadlines(800_000, 100)
        .await?;
    anyhow::ensure!(
        deadlines.coinshift_timeout > height,
        "a suggested Coinshift deadline must be in the future: {} vs tip {height}",
        deadlines.coinshift_timeout
    );
    tracing::info!(
        bitcoin_timeout = deadlines.bitcoin_timeout,
        coinshift_timeout = deadlines.coinshift_timeout,
        "derived a safe deadline pair"
    );

    // ---- 1. lock, with a deadline far enough out to claim under ----
    let far_timeout = height + 1_000;
    let lock_txid = sidechain
        .rpc_client
        .create_hash_lock(
            LOCK_VALUE,
            commitment_hex.clone(),
            claimant,
            far_timeout,
            Some(FEE),
        )
        .await?;
    advance(&mut sidechain, &mut enforcer_post_setup).await?;
    tracing::info!(%lock_txid, "hash lock mined");

    let lock = sole_lock(&sidechain).await?;
    anyhow::ensure!(
        lock.value_sats == LOCK_VALUE
            && lock.commitment_hex == commitment_hex
            && lock.claimant == claimant
            && !lock.refundable_now,
        "the lock must match what was asked for, and not be refundable yet: {lock:?}"
    );

    // ---- 2. a wrong secret must buy nothing ----
    let wrong = sidechain
        .rpc_client
        .claim_hash_lock(lock.outpoint, hex::encode([0xFFu8; 32]), Some(FEE))
        .await;
    anyhow::ensure!(
        wrong.is_err(),
        "a claim with the wrong preimage must be refused, got {wrong:?}"
    );

    // ---- 3. refunding before the deadline must buy nothing ----
    let early = sidechain
        .rpc_client
        .refund_hash_lock(lock.outpoint, Some(FEE))
        .await;
    anyhow::ensure!(
        early.is_err(),
        "a refund before the deadline must be refused, got {early:?}"
    );
    tracing::info!("both unauthorised spends were refused");

    // ---- 4. the real claim ----
    let before = sidechain.rpc_client.balance().await?;
    let claim_txid = sidechain
        .rpc_client
        .claim_hash_lock(lock.outpoint, hex::encode(secret), Some(FEE))
        .await?;
    advance(&mut sidechain, &mut enforcer_post_setup).await?;
    tracing::info!(%claim_txid, "claim mined");

    anyhow::ensure!(
        sidechain.rpc_client.list_hash_locks().await?.is_empty(),
        "the lock must be gone once claimed"
    );
    let after = sidechain.rpc_client.balance().await?;
    anyhow::ensure!(
        after.total >= before.total,
        "claiming must not lose value: {} -> {}",
        before.total,
        after.total
    );

    // ---- 5. a second lock, this time left to time out ----
    let height = sidechain.rpc_client.getblockcount().await?;
    let soon = height + 2;
    let refund_lock_txid = sidechain
        .rpc_client
        .create_hash_lock(
            LOCK_VALUE,
            commitment_hex.clone(),
            claimant,
            soon,
            Some(FEE),
        )
        .await?;
    advance(&mut sidechain, &mut enforcer_post_setup).await?;
    tracing::info!(%refund_lock_txid, timeout = soon, "second lock mined");

    // Mine past the deadline.
    while sidechain.rpc_client.getblockcount().await? < soon {
        advance(&mut sidechain, &mut enforcer_post_setup).await?;
    }

    let lock = sole_lock(&sidechain).await?;
    anyhow::ensure!(
        lock.refundable_now,
        "past its deadline, the lock must report itself refundable: {lock:?}"
    );

    let refund_txid = sidechain
        .rpc_client
        .refund_hash_lock(lock.outpoint, Some(FEE))
        .await?;
    advance(&mut sidechain, &mut enforcer_post_setup).await?;
    tracing::info!(%refund_txid, "refund mined");

    anyhow::ensure!(
        sidechain.rpc_client.list_hash_locks().await?.is_empty(),
        "the lock must be gone once refunded"
    );

    tracing::info!(
        "hash lock lifecycle passed: claim and refund both work, and neither \
         a wrong secret nor an early refund does"
    );
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn hash_lock_lifecycle_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "hash_lock_lifecycle",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res =
                        hash_lock_lifecycle_task(bin_paths, res_tx.clone())
                            .await;
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
