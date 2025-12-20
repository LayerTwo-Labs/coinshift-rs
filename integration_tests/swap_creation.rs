//! Test swap creation functionality

use bip300301_enforcer_integration_tests::{
    integration_test::{
        activate_sidechain, deposit, fund_enforcer, propose_sidechain,
    },
    setup::{
        Mode, Network, PostSetup as EnforcerPostSetup, Sidechain as _,
        setup as setup_enforcer,
    },
    util::{AbortOnDrop, AsyncTrial},
};
use coinshift::types::{Address, ParentChainType, SwapId, SwapState};
use coinshift_app_rpc_api::RpcClient as _;
use futures::{FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture};
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    setup::{Init, PostSetup},
    util::BinPaths,
};

/// Initial setup for the test
async fn setup(
    bin_paths: &BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<EnforcerPostSetup> {
    let mut enforcer_post_setup = setup_enforcer(
        &bin_paths.others,
        Network::Regtest,
        Mode::Mempool,
        res_tx.clone(),
    )
    .await?;
    let () = propose_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Proposed sidechain successfully");
    let () = activate_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Activated sidechain successfully");
    let () = fund_enforcer::<PostSetup>(&mut enforcer_post_setup).await?;
    Ok(enforcer_post_setup)
}

const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(21_000_000);
const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000_000);
const SWAP_L2_AMOUNT: u64 = 10_000_000; // 0.1 BTC
const SWAP_L1_AMOUNT: u64 = 5_000_000; // 0.05 BTC
const SWAP_FEE: u64 = 1_000; // 0.00001 BTC

/// Verify that a swap was created successfully
async fn verify_swap_created(
    rpc_client: &jsonrpsee::http_client::HttpClient,
    swap_id: SwapId,
    expected_l2_amount: u64,
    expected_l1_amount: u64,
    expected_l2_recipient: Option<Address>,
) -> anyhow::Result<()> {
    let swap = rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found"))?;

    // Verify swap details
    anyhow::ensure!(
        swap.id == swap_id,
        "Swap ID mismatch: expected {:?}, got {:?}",
        swap_id,
        swap.id
    );
    anyhow::ensure!(
        swap.l2_amount.to_sat() == expected_l2_amount,
        "L2 amount mismatch: expected {}, got {}",
        expected_l2_amount,
        swap.l2_amount.to_sat()
    );
    anyhow::ensure!(
        swap.l1_amount.map(|a| a.to_sat()) == Some(expected_l1_amount),
        "L1 amount mismatch: expected {}, got {:?}",
        expected_l1_amount,
        swap.l1_amount.map(|a| a.to_sat())
    );
    anyhow::ensure!(
        swap.l2_recipient == expected_l2_recipient,
        "L2 recipient mismatch: expected {:?}, got {:?}",
        expected_l2_recipient,
        swap.l2_recipient
    );
    anyhow::ensure!(
        matches!(swap.state, SwapState::Pending),
        "Swap state should be Pending, got {:?}",
        swap.state
    );

    tracing::info!("Swap created successfully: {:?}", swap);
    Ok(())
}

/// Wait for swap transaction to be included in a block
async fn wait_for_swap_in_block(
    sidechain: &mut PostSetup,
    enforcer: &mut EnforcerPostSetup,
    swap_txid: coinshift::types::Txid,
    swap_id: SwapId,
) -> anyhow::Result<()> {
    // BMM a block to include the swap transaction
    tracing::debug!(
        swap_id = %swap_id,
        swap_txid = %swap_txid,
        "BMM 1 block to include swap transaction"
    );
    sidechain.bmm_single(enforcer).await?;

    // Verify swap is accessible after being included in block
    // Swaps are only saved to database when included in a block
    let swaps = sidechain.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        swaps.iter().any(|s| s.id == swap_id),
        "Swap {} not found in list_swaps after block inclusion",
        swap_id
    );

    tracing::info!(
        swap_id = %swap_id,
        "Swap transaction included in block and persisted"
    );
    Ok(())
}

/// Verify that UTXOs are locked for the swap
async fn verify_swap_locks_utxos(
    rpc_client: &jsonrpsee::http_client::HttpClient,
    swap_id: SwapId,
    expected_locked_amount: u64,
) -> anyhow::Result<()> {
    let utxos = rpc_client.list_utxos().await?;
    
    // Find locked outputs for this swap and calculate total locked amount
    let mut total_locked: u64 = 0;
    let mut has_locked_outputs = false;
    
    for utxo in &utxos {
        if let coinshift::types::OutputContent::SwapPending { value, swap_id: locked_swap_id } = &utxo.output.content {
            if *locked_swap_id == swap_id.0 {
                has_locked_outputs = true;
                total_locked += value.to_sat();
            }
        }
    }

    anyhow::ensure!(
        has_locked_outputs,
        "No locked outputs found for swap {}",
        swap_id
    );
    
    anyhow::ensure!(
        total_locked == expected_locked_amount,
        "Locked amount mismatch: expected {}, got {}",
        expected_locked_amount,
        total_locked
    );

    tracing::info!(
        "Verified swap locks UTXOs: {} sats locked for swap {}",
        total_locked,
        swap_id
    );
    Ok(())
}

async fn setup_swapper(
    bin_paths: &BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
    data_dir_suffix: &str,
) -> anyhow::Result<(PostSetup, EnforcerPostSetup)> {
    let mut enforcer_post_setup = setup(bin_paths, res_tx.clone()).await?;

    let sidechain = PostSetup::setup(
        Init {
            coinshift_app: bin_paths.coinshift_app.clone(),
            data_dir_suffix: Some(data_dir_suffix.to_owned()),
        },
        &enforcer_post_setup,
        res_tx,
    )
    .await?;
    tracing::info!(
        "Setup Coinshift swapper node successfully (suffix={})",
        data_dir_suffix
    );

    Ok((sidechain, enforcer_post_setup))
}

async fn cleanup_swapper(
    sidechain: PostSetup,
    mut enforcer_post_setup: EnforcerPostSetup,
) -> anyhow::Result<()> {
    drop(sidechain);
    tracing::info!("Removing {}", enforcer_post_setup.out_dir.path().display());
    drop(enforcer_post_setup.tasks);
    // Wait for tasks to die
    sleep(std::time::Duration::from_secs(1)).await;
    enforcer_post_setup.out_dir.cleanup()?;
    Ok(())
}

async fn swap_creation_fixed_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        setup_swapper(&bin_paths, res_tx.clone(), "swapper-fixed").await?;

    // Get deposit address and deposit funds
    let deposit_address = sidechain.get_deposit_address().await?;
    let () = deposit(
        &mut enforcer_post_setup,
        &mut sidechain,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("Deposited to sidechain successfully");

    // Get a new address for L2 recipient (pre-specified swap)
    let l2_recipient_address = sidechain.rpc_client.get_new_address().await?;
    
    // Generate a regtest address for L1 recipient
    let l1_recipient_address = "bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
    
    // Create a pre-specified swap (with l2_recipient)
    tracing::info!("Creating pre-specified swap");
    let (swap_id, swap_txid) = sidechain
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient_address.to_string(),
            SWAP_L1_AMOUNT,
            Some(l2_recipient_address),
            SWAP_L2_AMOUNT,
            Some(1), // required_confirmations
            SWAP_FEE,
        )
        .await?;
    tracing::info!(
        swap_id = %swap_id,
        swap_txid = %swap_txid,
        "Created pre-specified swap transaction"
    );

    // Wait for swap to be included in block (swaps are only saved when included in a block)
    wait_for_swap_in_block(&mut sidechain, &mut enforcer_post_setup, swap_txid, swap_id)
        .await?;
    
    // Wait for wallet update task to sync state changes (locked outputs, spent UTXOs)
    // This ensures the wallet's view is current before proceeding
    sleep(std::time::Duration::from_millis(500)).await;

    // Now verify swap was created and persisted (after block inclusion)
    verify_swap_created(
        &sidechain.rpc_client,
        swap_id,
        SWAP_L2_AMOUNT,
        SWAP_L1_AMOUNT,
        Some(l2_recipient_address),
    )
    .await?;

    // Verify UTXOs are locked
    verify_swap_locks_utxos(&sidechain.rpc_client, swap_id, SWAP_L2_AMOUNT)
        .await?;

    // Verify list_swaps and list_swaps_by_recipient contain the swap
    let all_swaps = sidechain.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        all_swaps.iter().any(|s| s.id == swap_id),
        "Pre-specified swap not found in list_swaps"
    );
    let recipient_swaps = sidechain
        .rpc_client
        .list_swaps_by_recipient(l2_recipient_address)
        .await?;
    anyhow::ensure!(
        recipient_swaps.iter().any(|s| s.id == swap_id),
        "Pre-specified swap not found in list_swaps_by_recipient"
    );

    tracing::info!("Fixed swap creation test passed");

    cleanup_swapper(sidechain, enforcer_post_setup).await
}

async fn swap_creation_open_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        setup_swapper(&bin_paths, res_tx.clone(), "swapper-open").await?;

    // Get deposit address and deposit funds
    let deposit_address = sidechain.get_deposit_address().await?;
    let () = deposit(
        &mut enforcer_post_setup,
        &mut sidechain,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("Deposited to sidechain successfully");

    let l1_recipient_address = "bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";

    // Create an open swap (without l2_recipient)
    tracing::info!("Creating open swap");
    let (swap_id, swap_txid) = sidechain
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient_address.to_string(),
            SWAP_L1_AMOUNT,
            None, // None = open swap
            SWAP_L2_AMOUNT,
            Some(1),
            SWAP_FEE,
        )
        .await?;
    tracing::info!(
        swap_id = %swap_id,
        swap_txid = %swap_txid,
        "Created open swap transaction"
    );

    // Wait for open swap to be included in block (swaps are only saved when included in a block)
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        swap_txid,
        swap_id,
    )
    .await?;

    // Wait for wallet update task to sync state changes (locked outputs, spent UTXOs)
    sleep(std::time::Duration::from_millis(500)).await;

    // Now verify open swap was created and persisted (after block inclusion)
    verify_swap_created(
        &sidechain.rpc_client,
        swap_id,
        SWAP_L2_AMOUNT,
        SWAP_L1_AMOUNT,
        None, // Open swap has no l2_recipient
    )
    .await?;
    
    // Verify open swap locks UTXOs as expected
    verify_swap_locks_utxos(&sidechain.rpc_client, swap_id, SWAP_L2_AMOUNT)
        .await?;

    // Verify open swap has no l2_recipient and is listed
    let open_swap = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Open swap not found after block inclusion"))?;
    anyhow::ensure!(
        open_swap.l2_recipient.is_none(),
        "Open swap should have no l2_recipient"
    );

    let all_swaps = sidechain.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        all_swaps.iter().any(|s| s.id == swap_id),
        "Open swap not found in list_swaps"
    );

    tracing::info!("Open swap creation test passed");

    cleanup_swapper(sidechain, enforcer_post_setup).await
}

async fn swap_creation_fixed(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = swap_creation_fixed_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

async fn swap_creation_open(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = swap_creation_open_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

pub fn swap_creation_fixed_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("swap_creation_fixed", swap_creation_fixed(bin_paths).boxed())
}

pub fn swap_creation_open_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("swap_creation_open", swap_creation_open(bin_paths).boxed())
}

