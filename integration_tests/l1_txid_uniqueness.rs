//! Test that the same L1 txid cannot be associated with more than one swap.
//!
//! Enforces the check described in COINSHIFT_HOW_IT_WORKS.md: get_swap_by_l1_txid
//! is used before accepting an L1 tx (in query_and_update_swap and update_swap_l1_txid).

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

/// Verify that assigning the same L1 txid to a second swap fails (L1 txid uniqueness).
async fn l1_txid_uniqueness_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(
            &bin_paths,
            res_tx.clone(),
            "l1-uniqueness",
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

    // Create first swap (pre-specified)
    let (swap_id_a, txid_a) = sidechain
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
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        txid_a,
        swap_id_a,
    )
    .await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Create second swap with same L1 recipient and amount
    let (swap_id_b, txid_b) = sidechain
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
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        txid_b,
        swap_id_b,
    )
    .await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Assign a fake L1 txid to the first swap (succeeds)
    let fake_l1_txid_hex = "aa".repeat(32);
    sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id_a, fake_l1_txid_hex.clone(), 1, None)
        .await?;
    sleep(std::time::Duration::from_millis(300)).await;

    let status_a = sidechain
        .rpc_client
        .get_swap_status(swap_id_a)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap A not found"))?;
    anyhow::ensure!(
        matches!(status_a.state, SwapState::ReadyToClaim),
        "Swap A should be ReadyToClaim after L1 update: {:?}",
        status_a.state
    );

    // Assign the SAME L1 txid to the second swap — must fail (L1 txid already used)
    let err = sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id_b, fake_l1_txid_hex, 1, None)
        .await
        .expect_err("update_swap_l1_txid for second swap should fail");
    let err_str = format!("{err:#}");
    anyhow::ensure!(
        err_str.contains("already used")
            || err_str.contains("L1TxidAlreadyUsed"),
        "Expected L1TxidAlreadyUsed error, got: {}",
        err_str
    );

    // Second swap must still be Pending
    let status_b = sidechain
        .rpc_client
        .get_swap_status(swap_id_b)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap B not found"))?;
    anyhow::ensure!(
        matches!(status_b.state, SwapState::Pending),
        "Swap B should remain Pending after rejected L1 update: {:?}",
        status_b.state
    );

    tracing::info!(
        "L1 txid uniqueness test passed: same L1 txid cannot be used for two swaps"
    );
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn l1_txid_uniqueness_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "l1_txid_uniqueness",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res =
                        l1_txid_uniqueness_task(bin_paths, res_tx.clone())
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
