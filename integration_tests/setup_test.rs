//! Tests for setup functions (signet and regtest)

use bip300301_enforcer_integration_tests::{
    setup::Mode,
    util::{AbortOnDrop, AsyncTrial},
};
use futures::{FutureExt, StreamExt as _, channel::mpsc, future::BoxFuture};
use coinshift_app_rpc_api::RpcClient as _;
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    setup::{Init, setup_regtest, setup_signet},
    util::BinPaths,
};

/// Test signet setup with 2 blocks
async fn test_signet_setup_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing signet setup with 2 blocks");
    
    // Verify coinshift binary exists
    tracing::info!("Checking coinshift binary path: {:?}", bin_paths.coinshift);
    if !bin_paths.coinshift.exists() {
        anyhow::bail!("Coinshift binary does not exist at: {:?}", bin_paths.coinshift);
    }
    tracing::info!("✓ Coinshift binary exists");
    
    tracing::info!("Starting setup_signet");
    let complete_setup = setup_signet(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("signet-test".to_owned()),
        },
        res_tx.clone(),
    )
    .await
    .map_err(|e| {
        tracing::error!("setup_signet failed: {:#}", e);
        e
    })?;
    tracing::info!("✓ setup_signet completed successfully");
    
    // Verify we have at least 2 blocks
    use bip300301_enforcer_lib::bins::CommandExt;
    let block_count: u32 = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getblockcount", [])
        .run_utf8()
        .await?
        .parse()?;
    
    anyhow::ensure!(block_count >= 2, "Expected at least 2 blocks, got {block_count}");
    tracing::info!("✓ Signet setup successful with {block_count} blocks");
    
    // Verify coinshift is running by checking block count
    let coinshift_blocks = complete_setup.coinshift.rpc_client.getblockcount().await?;
    tracing::info!("✓ Coinshift is running with {coinshift_blocks} blocks");
    
    // Cleanup
    drop(complete_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(complete_setup.enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    complete_setup.enforcer.out_dir.cleanup()?;
    
    Ok(())
}

async fn test_signet_setup(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = test_signet_setup_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

pub fn signet_setup_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("signet_setup", test_signet_setup(bin_paths).boxed())
}

/// Test regtest setup
async fn test_regtest_setup_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing regtest setup");
    
    // Verify coinshift binary exists
    tracing::info!("Checking coinshift binary path: {:?}", bin_paths.coinshift);
    if !bin_paths.coinshift.exists() {
        anyhow::bail!("Coinshift binary does not exist at: {:?}", bin_paths.coinshift);
    }
    tracing::info!("✓ Coinshift binary exists");
    
    tracing::info!("Starting setup_regtest");
    let complete_setup = setup_regtest(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("regtest-test".to_owned()),
        },
        Mode::Mempool,
        res_tx.clone(),
    )
    .await
    .map_err(|e| {
        tracing::error!("setup_regtest failed: {:#}", e);
        e
    })?;
    tracing::info!("✓ setup_regtest completed successfully");
    
    // Verify enforcer is running by checking block count
    use bip300301_enforcer_lib::bins::CommandExt;
    let block_count: u32 = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getblockcount", [])
        .run_utf8()
        .await?
        .parse()?;
    
    tracing::info!("✓ Regtest setup successful with {block_count} blocks");
    
    // Verify coinshift is running by checking block count
    let coinshift_blocks = complete_setup.coinshift.rpc_client.getblockcount().await?;
    tracing::info!("✓ Coinshift is running with {coinshift_blocks} blocks");
    
    // Test that we can get a deposit address
    let deposit_address = complete_setup.coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Can get deposit address: {deposit_address}");
    
    // Cleanup
    drop(complete_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(complete_setup.enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    complete_setup.enforcer.out_dir.cleanup()?;
    
    Ok(())
}

async fn test_regtest_setup(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = test_regtest_setup_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

pub fn regtest_setup_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("regtest_setup", test_regtest_setup(bin_paths).boxed())
}
