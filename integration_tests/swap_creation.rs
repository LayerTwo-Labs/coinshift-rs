//! Test swap creation functionality

use bip300301_enforcer_integration_tests::{
    integration_test::{
        activate_sidechain, deposit, fund_enforcer, propose_sidechain,
    },
    setup::{
        Mode, Network, PostSetup as EnforcerPostSetup, Sidechain as _,
        setup as setup_enforcer,
    },
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use coinshift::types::{Address, ParentChainType, SwapId, SwapState};
use coinshift_app_rpc_api::RpcClient as _;
use futures::{
    FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture,
};
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
        if let coinshift::types::OutputContent::SwapPending {
            value,
            swap_id: locked_swap_id,
        } = &utxo.output.content
            && *locked_swap_id == swap_id.0
        {
            has_locked_outputs = true;
            total_locked += value.to_sat();
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

/// Wait (with retries) for UTXOs to be locked for the swap
async fn wait_for_locked_utxos(
    rpc_client: &jsonrpsee::http_client::HttpClient,
    swap_id: SwapId,
    expected_locked_amount: u64,
) -> anyhow::Result<()> {
    const MAX_RETRIES: usize = 10;
    const RETRY_DELAY_MS: u64 = 200;

    for attempt in 0..MAX_RETRIES {
        let res = verify_swap_locks_utxos(
            rpc_client,
            swap_id,
            expected_locked_amount,
        )
        .await;
        if res.is_ok() {
            return Ok(());
        }
        tracing::debug!(
            attempt,
            swap_id = %swap_id,
            "Locked UTXOs not yet visible, retrying..."
        );
        sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
    }

    // Final attempt, propagate error
    verify_swap_locks_utxos(rpc_client, swap_id, expected_locked_amount).await
}

pub async fn setup_swapper(
    bin_paths: &BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
    data_dir_suffix: &str,
) -> anyhow::Result<(PostSetup, EnforcerPostSetup)> {
    let enforcer_post_setup = setup(bin_paths, res_tx.clone()).await?;

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

pub async fn cleanup_swapper(
    sidechain: PostSetup,
    enforcer_post_setup: EnforcerPostSetup,
) -> anyhow::Result<()> {
    drop(sidechain);
    tracing::info!(
        "Removing {}",
        enforcer_post_setup.directories.base_dir.path().display()
    );
    drop(enforcer_post_setup.tasks);
    // Wait for tasks to die
    sleep(std::time::Duration::from_secs(1)).await;
    enforcer_post_setup.directories.base_dir.cleanup()?;
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
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        swap_txid,
        swap_id,
    )
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
        .ok_or_else(|| {
            anyhow::anyhow!("Open swap not found after block inclusion")
        })?;
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

async fn swap_creation_open_fill_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut sidechain, mut enforcer_post_setup) =
        setup_swapper(&bin_paths, res_tx.clone(), "swapper-open-fill").await?;

    // Fund the wallet
    let deposit_address = sidechain.get_deposit_address().await?;
    deposit(
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
    tracing::info!("Creating open swap to later fill");
    let (swap_id, swap_txid) = sidechain
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient_address.to_string(),
            SWAP_L1_AMOUNT,
            None, // open swap
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

    // Include the swap create tx in a block
    wait_for_swap_in_block(
        &mut sidechain,
        &mut enforcer_post_setup,
        swap_txid,
        swap_id,
    )
    .await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Ensure it exists and is pending with locked UTXOs
    verify_swap_created(
        &sidechain.rpc_client,
        swap_id,
        SWAP_L2_AMOUNT,
        SWAP_L1_AMOUNT,
        None,
    )
    .await?;
    wait_for_locked_utxos(&sidechain.rpc_client, swap_id, SWAP_L2_AMOUNT)
        .await?;

    // Simulate Bob filling the swap: provide L1 txid and his L2 address (claim only valid for this address)
    let claimer_address = sidechain.rpc_client.get_new_address().await?;
    let fake_l1_txid_hex = "11".repeat(32);
    sidechain
        .rpc_client
        .update_swap_l1_txid(swap_id, fake_l1_txid_hex.clone(), 1, Some(claimer_address))
        .await?;
    // Allow wallet/state tasks to catch up
    sleep(std::time::Duration::from_millis(500)).await;
    wait_for_locked_utxos(&sidechain.rpc_client, swap_id, SWAP_L2_AMOUNT)
        .await?;

    tracing::info!(
        "Open swap state updated to ReadyToClaim with L2 claimer address. swap_id={}, claimer={}",
        swap_id,
        claimer_address
    );
    let status_ready = sidechain
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found after L1 update"))?;
    anyhow::ensure!(
        matches!(status_ready.state, SwapState::ReadyToClaim),
        "Swap not ReadyToClaim after L1 update: {:?}",
        status_ready.state
    );
    anyhow::ensure!(
        status_ready.l2_claimer_address == Some(claimer_address),
        "Stored l2_claimer_address should match: {:?}",
        status_ready.l2_claimer_address
    );

    // Claim the swap: recipient is taken from stored l2_claimer_address (can pass None)
    let claim_txid = sidechain
        .rpc_client
        .claim_swap(swap_id, None)
        .await?;
    tracing::info!(swap_id = %swap_id, claim_txid = %claim_txid, "Claimed swap");

    // Mine the claim transaction into a block
    sidechain.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Verify completion
    // Wait until the swap is marked completed
    const MAX_STATUS_RETRIES: usize = 10;
    const STATUS_DELAY_MS: u64 = 200;
    let mut completed = None;
    for _ in 0..MAX_STATUS_RETRIES {
        let status = sidechain.rpc_client.get_swap_status(swap_id).await?;
        if let Some(s) = status
            && matches!(s.state, SwapState::Completed)
        {
            completed = Some(s);
            break;
        }
        sleep(std::time::Duration::from_millis(STATUS_DELAY_MS)).await;
    }
    let completed_swap = completed.ok_or_else(|| {
        anyhow::anyhow!("Swap not marked Completed after claim")
    })?;

    // Locked outputs should be released
    let utxos_after = sidechain.rpc_client.list_utxos().await?;
    let still_locked = utxos_after.iter().any(|utxo| {
        matches!(
            utxo.output.content,
            coinshift::types::OutputContent::SwapPending { swap_id: locked, .. }
                if locked == swap_id.0
        )
    });
    anyhow::ensure!(
        !still_locked,
        "Expected no locked outputs after swap completion"
    );

    // Final report
    tracing::info!(
        swap_id = %swap_id,
        swap_create_txid = %swap_txid,
        fake_l1_txid_hex = %fake_l1_txid_hex,
        claim_txid = %claim_txid,
        l1_recipient = l1_recipient_address,
        l1_amount_sats = SWAP_L1_AMOUNT,
        l2_amount_sats = SWAP_L2_AMOUNT,
        claimer_address = %claimer_address,
        final_state = ?completed_swap.state,
        utxos_total = utxos_after.len(),
        "Open swap fill report: swap completed and locks released"
    );

    tracing::info!("Open swap fill and claim test passed");

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

async fn swap_creation_open_fill(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res =
                swap_creation_open_fill_task(bin_paths, res_tx.clone()).await;
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
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "swap_creation_fixed",
        swap_creation_fixed(bin_paths).boxed(),
        file_registry,
        failure_collector,
    )
}

pub fn swap_creation_open_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "swap_creation_open",
        swap_creation_open(bin_paths).boxed(),
        file_registry,
        failure_collector,
    )
}

pub fn swap_creation_open_fill_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "swap_creation_open_fill",
        swap_creation_open_fill(bin_paths).boxed(),
        file_registry,
        failure_collector,
    )
}
