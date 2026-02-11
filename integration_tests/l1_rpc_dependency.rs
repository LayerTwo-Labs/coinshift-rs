//! Test that swap L1 presence and confirmation count depend on configured RPC.
//!
//! Documents COINSHIFT_HOW_IT_WORKS.md item 4: without RPC config for the swap's
//! parent_chain, process_coinshift skips L1 lookup and the swap stays Pending.

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

/// Without RPC config, swap stays Pending after BMM (process_coinshift skips L1 lookup).
async fn l1_rpc_dependency_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        crate::swap_creation::setup_swapper(&bin_paths, res_tx.clone(), "l1-rpc-dep").await?;

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

    // BMM more blocks so process_coinshift runs (no L1 RPC configured in test)
    for _ in 0..3 {
        sidechain.bmm_single(&mut enforcer_post_setup).await?;
        sleep(std::time::Duration::from_millis(400)).await;
    }

    // Swap must still be Pending: L1 presence relies on configured RPC for swap target chain
    let status = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found"))?;
    anyhow::ensure!(
        matches!(status.state, SwapState::Pending),
        "Without RPC config, swap should stay Pending (L1 not detected): {:?}",
        status.state
    );

    tracing::info!(
        "L1 RPC dependency test passed: swap stayed Pending without RPC config"
    );
    crate::swap_creation::cleanup_swapper(sidechain, enforcer_post_setup).await
}

pub fn l1_rpc_dependency_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "l1_rpc_dependency",
        async move {
            let (res_tx, mut res_rx) = mpsc::unbounded();
            let _task: AbortOnDrop<()> = tokio::task::spawn({
                let res_tx = res_tx.clone();
                async move {
                    let res = l1_rpc_dependency_task(bin_paths, res_tx.clone()).await;
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
