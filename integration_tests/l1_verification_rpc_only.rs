//! Test that swap L1 verification uses RPC only (no BMM / header chain / merkle proof).
//!
//! Documents COINSHIFT_HOW_IT_WORKS.md item 3: BMM reports, header chain, and
//! merkle proof are not used for swap L1 verification in this repo. A swap can
//! reach ReadyToClaim with only RPC-based L1 tx detection (or manual
//! update_swap_l1_txid).

use bip300301_enforcer_integration_tests::{
    integration_test::deposit,
    setup::{PostSetup as EnforcerPostSetup, Sidechain as _},
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use coinshift::types::{ParentChainType, SwapState};
use coinshift_app_rpc_api::RpcClient as _;
use futures::{
    FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture,
};
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{setup::PostSetup, util::BinPaths};

const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(21_000_000);
const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000_000);
const SWAP_L2_AMOUNT: u64 = 10_000_000;
const SWAP_L1_AMOUNT: u64 = 5_000_000;
const SWAP_FEE: u64 = 1_000;

async fn wait_for_swap_in_block(
    sidechain: &mut PostSetup,
    enforcer: &mut EnforcerPostSetup,
    _swap_txid: coinshift::types::Txid,
    swap_id: coinshift::types::SwapId,
) -> anyhow::Result<()> {
    sidechain.bmm_single(enforcer).await?;
    sleep(std::time::Duration::from_millis(500)).await;
    let swaps = sidechain.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        swaps.iter().any(|s| s.id == swap_id),
        "Swap {} not found in list_swaps after block",
        swap_id
    );
    Ok(())
}

/// Swap reaches ReadyToClaim with RPC-only L1 verification (no BMM/merkle).
async fn l1_verification_rpc_only_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(
            &bin_paths,
            res_tx.clone(),
            "l1-rpc-only",
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

    let l1_recipient = "bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";

    let (swap_id, txid) = sidechain
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient.to_string(),
            SWAP_L1_AMOUNT,
            None, // open swap
            SWAP_L2_AMOUNT,
            Some(1),
            SWAP_FEE,
        )
        .await?;
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        txid,
        swap_id,
    )
    .await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Set L1 txid via RPC (no BMM, no merkle proof) — L1 verification is RPC-only in this repo
    let fake_l1_txid_hex = "cc".repeat(32);
    sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id, fake_l1_txid_hex.clone(), 1, None)
        .await?;
    sleep(std::time::Duration::from_millis(300)).await;

    let status = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found"))?;
    anyhow::ensure!(
        matches!(status.state, SwapState::ReadyToClaim),
        "Swap should be ReadyToClaim after RPC-only L1 update (no BMM/merkle): {:?}",
        status.state
    );

    tracing::info!(
        "L1 verification RPC-only test passed: swap reached ReadyToClaim without BMM/header chain/merkle proof"
    );
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn l1_verification_rpc_only_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "l1_verification_rpc_only",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res = l1_verification_rpc_only_task(
                        bin_paths,
                        res_tx.clone(),
                    )
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
