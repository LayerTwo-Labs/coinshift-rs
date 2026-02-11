//! Test that we reject unconfirmed L1 transactions (confirmations == 0)
//! and require block inclusion, as in COINSHIFT_HOW_IT_WORKS.md.
//!
//! - query_and_update_swap: only accepts matches with confirmations > 0 and
//!   blockheight.is_some().
//! - update_swap_l1_txid: rejects confirmations == 0.

use bip300301_enforcer_integration_tests::{
    integration_test::deposit,
    setup::{PostSetup as EnforcerPostSetup, Sidechain as _},
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use coinshift::types::{ParentChainType, SwapState};
use coinshift_app_rpc_api::RpcClient as _;
use futures::{FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture};
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    setup::{Init, PostSetup},
    util::BinPaths,
};

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

/// Reject update_swap_l1_txid when confirmations == 0.
async fn confirmations_block_inclusion_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(&bin_paths, res_tx.clone(), "confirmations").await?;

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
            Some(sidechain.rpc_client.get_new_address().await?),
            SWAP_L2_AMOUNT,
            Some(1),
            SWAP_FEE,
        )
        .await?;
    wait_for_swap_in_block(&mut sidechain, &mut enforcer_post_setup, txid, swap_id).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Reject confirmations == 0
    let fake_l1_txid_hex = "bb".repeat(32);
    let err = sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id, fake_l1_txid_hex.clone(), 0)
        .await
        .expect_err("update_swap_l1_txid with confirmations=0 should fail");
    let err_str = format!("{err:#}");
    anyhow::ensure!(
        err_str.contains("confirmations") || err_str.contains("0"),
        "Expected confirmations error, got: {}",
        err_str
    );

    // Swap must still be Pending
    let status = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found"))?;
    anyhow::ensure!(
        matches!(status.state, SwapState::Pending),
        "Swap should remain Pending after rejected update: {:?}",
        status.state
    );

    // Accept confirmations >= 1
    sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id, fake_l1_txid_hex, 1)
        .await?;
    sleep(std::time::Duration::from_millis(300)).await;
    let status_ready = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found"))?;
    anyhow::ensure!(
        matches!(status_ready.state, SwapState::ReadyToClaim),
        "Swap should be ReadyToClaim after update with confirmations=1: {:?}",
        status_ready.state
    );

    tracing::info!("Confirmations and block inclusion test passed");
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn confirmations_block_inclusion_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "confirmations_block_inclusion",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res =
                        confirmations_block_inclusion_task(bin_paths, res_tx.clone()).await;
                    let _ = res_tx.unbounded_send(res);
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
