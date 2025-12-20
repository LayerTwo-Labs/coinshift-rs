use std::error::Error;
use bip300301_enforcer_integration_tests::{
    integration_test::deposit,
    setup::{Mode, Network, Sidechain},
    util::{AbortOnDrop, AsyncTrial},
};
use bitcoin::hashes::Hash;
use coinshift::types::ParentChainType;
use coinshift_app_rpc_api::RpcClient as _;
use futures::{FutureExt, future::BoxFuture, StreamExt as _, channel::mpsc};
use serde_json;
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    ibd::ibd_trial,
    setup::{Init, PostSetup, get_or_init_shared_signet_setup},
    setup_test::{regtest_setup_trial, signet_setup_trial},
    unknown_withdrawal::unknown_withdrawal_trial,
    util::BinPaths,
};

fn deposit_withdraw_roundtrip(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("deposit_withdraw_roundtrip", async move {
        bip300301_enforcer_integration_tests::integration_test::deposit_withdraw_roundtrip::<PostSetup>(
                bin_paths.others, Network::Regtest, Mode::Mempool,
                Init {
                    coinshift_app: bin_paths.coinshift,
                    data_dir_suffix: None,
                },
            ).await
    }.boxed())
}

/// Test swap creation after funding BTC from signet into sidechain
async fn swap_test_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing swap creation with signet funding");
    
    // Get or initialize shared signet setup (enforcer + sidechain, set up once for all tests)
    let shared_setup = get_or_init_shared_signet_setup(&bin_paths, res_tx.clone()).await?;
    tracing::info!("✓ Got shared signet setup");
    
    // Create isolated coinshift instance for this test
    let mut coinshift = shared_setup.create_coinshift_instance(Some("swap-test".to_owned())).await?;
    tracing::info!("✓ Created isolated coinshift instance");
    
    // Get deposit address
    let deposit_address = coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    // Lock the shared enforcer for the deposit operation
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = deposit(
            &mut *enforcer_guard,
            &mut coinshift,
            &deposit_address,
            DEPOSIT_AMOUNT,
            DEPOSIT_FEE,
        )
        .await?;
    }
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = coinshift.bmm_single(&mut *enforcer_guard).await?;
    }
    tracing::info!("✓ BMM complete");
    
    // Verify we have the deposit in UTXOs
    let utxos = coinshift.rpc_client.list_utxos().await?;
    let deposit_utxos: Vec<_> = utxos
        .iter()
        .filter(|utxo| {
            matches!(utxo.outpoint, coinshift::types::OutPoint::Deposit(_))
                && matches!(utxo.output.content, coinshift::types::OutputContent::Value(_))
        })
        .collect();
    anyhow::ensure!(!deposit_utxos.is_empty(), "No deposit UTXOs found after deposit");
    tracing::info!("✓ Found {} deposit UTXO(s)", deposit_utxos.len());
    
    // Get a new address for the swap recipient
    let l2_recipient_address = coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    // Swap from sidechain to signet
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getnewaddress", [])
            .run_utf8()
            .await?
    };
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = coinshift
        .rpc_client
        .create_swap(
            ParentChainType::Signet,
            l1_recipient_address,
            L1_AMOUNT_SATS,
            Some(l2_recipient_address),
            L2_AMOUNT_SATS,
            None, // Use default confirmations
            SWAP_FEE_SATS,
        )
        .await?;
    tracing::info!("✓ Swap created successfully");
    tracing::info!("  Swap ID: {:?}", swap_id);
    tracing::info!("  Swap TXID: {:?}", swap_txid);
    
    // Wait for the transaction to be fully processed and the node to be ready
    // This helps avoid network task cancellation errors
    sleep(std::time::Duration::from_secs(2)).await;
    
    // Get current block count before BMM
    let block_count_before = coinshift.rpc_client.getblockcount().await?;
    tracing::debug!("Block count before BMM: {}", block_count_before);
    
    // Mine a block to confirm the swap transaction
    // Retry BMM in case of network task cancellation errors
    tracing::info!("BMMing block to confirm swap transaction");
    let mut bmm_result = {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        coinshift.bmm_single(&mut *enforcer_guard).await
    };
    if bmm_result.is_err() {
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(3)).await;
        bmm_result = {
            let mut enforcer_guard = shared_setup.enforcer.lock().await;
            coinshift.bmm_single(&mut *enforcer_guard).await
        };
        if bmm_result.is_err() {
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(3)).await;
            bmm_result = {
                let mut enforcer_guard = shared_setup.enforcer.lock().await;
                coinshift.bmm_single(&mut *enforcer_guard).await
            };
        }
    }
    let () = bmm_result?;
    
    // Verify block count increased
    let block_count_after = coinshift.rpc_client.getblockcount().await?;
    tracing::debug!("Block count after BMM: {}", block_count_after);
    anyhow::ensure!(
        block_count_after > block_count_before,
        "Block count should increase after BMM"
    );
    tracing::info!("✓ BMM complete, swap transaction confirmed (block {} -> {})", block_count_before, block_count_after);
    
    // Wait a bit for the block to be fully processed
    sleep(std::time::Duration::from_millis(500)).await;
    
    // Verify swap status (now available after confirmation)
    // Retry a few times in case block processing is still in progress
    let mut swap_status: Option<coinshift::types::Swap> = None;
    for attempt in 1..=5 {
        swap_status = coinshift
            .rpc_client
            .get_swap_status(swap_id)
            .await?;
        if swap_status.is_some() {
            break;
        }
        if attempt < 5 {
            tracing::debug!("Swap not yet available, retrying (attempt {}/{})", attempt, 5);
            sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    anyhow::ensure!(
        swap_status.is_some(),
        "Swap status should be available after creation and block confirmation"
    );
    let swap = swap_status.unwrap();
    tracing::info!("✓ Swap status retrieved");
    tracing::info!("  State: {:?}", swap.state);
    tracing::info!("  Parent chain: {:?}", swap.parent_chain);
    tracing::info!("  L1 amount: {} sats", swap.l1_amount.map(|a| a.to_sat()).unwrap_or(0));
    tracing::info!("  L2 amount: {} sats", swap.l2_amount.to_sat());
    
    // Verify swap appears in list
    let all_swaps = coinshift.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        all_swaps.iter().any(|s| s.id == swap_id),
        "Swap should appear in list_swaps"
    );
    tracing::info!("✓ Swap appears in swap list");
    
    // Cleanup - stop node gracefully first
    // Note: We don't cleanup the shared enforcer here as it's shared across tests
    let _unused = coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(coinshift);
    
    tracing::info!("✓ Swap test completed successfully");
    Ok(())
}

async fn swap_test(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = swap_test_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

fn swap_test_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("swap_test", swap_test(bin_paths).boxed())
}

/// Test reading swap by ID, printing details, and checking if it can be decoded
async fn read_swap_test_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing swap read and decode");
    
    // Get or initialize shared signet setup (enforcer + sidechain, set up once for all tests)
    let shared_setup = get_or_init_shared_signet_setup(&bin_paths, res_tx.clone()).await?;
    tracing::info!("✓ Got shared signet setup");
    
    // Create isolated coinshift instance for this test
    let mut coinshift = shared_setup.create_coinshift_instance(Some("read-swap-test".to_owned())).await?;
    tracing::info!("✓ Created isolated coinshift instance");
    
    // Get deposit address
    let deposit_address = coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    // Lock the shared enforcer for the deposit operation
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = deposit(
            &mut *enforcer_guard,
            &mut coinshift,
            &deposit_address,
            DEPOSIT_AMOUNT,
            DEPOSIT_FEE,
        )
        .await?;
    }
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = coinshift.bmm_single(&mut *enforcer_guard).await?;
    }
    tracing::info!("✓ BMM complete");
    
    // Get a new address for the swap recipient
    let l2_recipient_address = coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getnewaddress", [])
            .run_utf8()
            .await?
    };
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = coinshift
        .rpc_client
        .create_swap(
            ParentChainType::Signet,
            l1_recipient_address.clone(),
            L1_AMOUNT_SATS,
            Some(l2_recipient_address),
            L2_AMOUNT_SATS,
            None, // Use default confirmations
            SWAP_FEE_SATS,
        )
        .await?;
    tracing::info!("✓ Swap created successfully");
    tracing::info!("  Swap ID: {}", swap_id);
    tracing::info!("  Coinshift TXID (L2): {:?}", swap_txid);
    
    // Wait for the transaction to be fully processed
    sleep(std::time::Duration::from_secs(2)).await;
    
    // Mine a block to confirm the swap transaction
    tracing::info!("BMMing block to confirm swap transaction");
    let mut bmm_result = {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        coinshift.bmm_single(&mut *enforcer_guard).await
    };
    if bmm_result.is_err() {
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(3)).await;
        bmm_result = {
            let mut enforcer_guard = shared_setup.enforcer.lock().await;
            coinshift.bmm_single(&mut *enforcer_guard).await
        };
        if bmm_result.is_err() {
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(3)).await;
            bmm_result = {
                let mut enforcer_guard = shared_setup.enforcer.lock().await;
                coinshift.bmm_single(&mut *enforcer_guard).await
            };
        }
    }
    let () = bmm_result?;
    tracing::info!("✓ BMM complete");
    
    // Wait a bit for the block to be fully processed
    sleep(std::time::Duration::from_millis(500)).await;
    
    // Read swap by ID and print all details
    tracing::info!("Reading swap by ID: {}", swap_id);
    let mut swap_status: Option<coinshift::types::Swap> = None;
    for attempt in 1..=5 {
        swap_status = coinshift
            .rpc_client
            .get_swap_status(swap_id)
            .await?;
        if swap_status.is_some() {
            break;
        }
        if attempt < 5 {
            tracing::debug!("Swap not yet available, retrying (attempt {}/{})", attempt, 5);
            sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    anyhow::ensure!(
        swap_status.is_some(),
        "Swap status should be available after creation and block confirmation"
    );
    let swap = swap_status.unwrap();
    
    // Print all swap details
    tracing::info!("=== SWAP DETAILS ===");
    tracing::info!("Swap ID: {}", swap.id);
    tracing::info!("Direction: {:?}", swap.direction);
    tracing::info!("Parent Chain: {:?}", swap.parent_chain);
    tracing::info!("State: {:?}", swap.state);
    tracing::info!("L1 Recipient Address: {:?}", swap.l1_recipient_address);
    tracing::info!("L1 Amount: {} sats", swap.l1_amount.map(|a| a.to_sat()).unwrap_or(0));
    tracing::info!("L2 Recipient: {:?}", swap.l2_recipient);
    tracing::info!("L2 Amount: {} sats", swap.l2_amount.to_sat());
    tracing::info!("L1 Claimer Address: {:?}", swap.l1_claimer_address);
    tracing::info!("Required Confirmations: {}", swap.required_confirmations);
    tracing::info!("Created At Height: {}", swap.created_at_height);
    tracing::info!("Expires At Height: {:?}", swap.expires_at_height);
    
    // Print L1 transaction ID
    match &swap.l1_txid {
        coinshift::types::SwapTxId::Hash32(hash) => {
            let txid = bitcoin::Txid::from_byte_array(*hash);
            tracing::info!("L1 TXID (Signet): {}", txid);
        }
        coinshift::types::SwapTxId::Hash(bytes) => {
            tracing::info!("L1 TXID (Hash): {}", hex::encode(bytes));
        }
    }
    
    // Check if swap can be decoded (try to serialize/deserialize)
    tracing::info!("Testing swap decode/serialize...");
    let swap_json = serde_json::to_string(&swap)?;
    tracing::info!("✓ Swap serialized to JSON successfully ({} bytes)", swap_json.len());
    
    let decoded_swap: coinshift::types::Swap = serde_json::from_str(&swap_json)?;
    anyhow::ensure!(
        decoded_swap.id == swap.id,
        "Decoded swap ID should match original"
    );
    anyhow::ensure!(
        decoded_swap.l2_amount == swap.l2_amount,
        "Decoded swap L2 amount should match original"
    );
    tracing::info!("✓ Swap decoded successfully from JSON");
    
    // Also test Borsh serialization
    let swap_borsh = borsh::to_vec(&swap)?;
    tracing::info!("✓ Swap serialized to Borsh successfully ({} bytes)", swap_borsh.len());
    
    let decoded_swap_borsh = borsh::from_slice::<coinshift::types::Swap>(&swap_borsh)?;
    anyhow::ensure!(
        decoded_swap_borsh.id == swap.id,
        "Borsh decoded swap ID should match original"
    );
    anyhow::ensure!(
        decoded_swap_borsh.l2_amount == swap.l2_amount,
        "Borsh decoded swap L2 amount should match original"
    );
    tracing::info!("✓ Swap decoded successfully from Borsh");
    
    tracing::info!("=== END SWAP DETAILS ===");
    
    // Cleanup - stop node gracefully first
    // Note: We don't cleanup the shared enforcer here as it's shared across tests
    let _unused = coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(coinshift);
    
    tracing::info!("✓ Read swap test completed successfully");
    Ok(())
}

async fn read_swap_test(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = read_swap_test_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

fn read_swap_test_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("read_swap_test", read_swap_test(bin_paths).boxed())
}

/// Test filling swap with coins and checking balances
async fn fill_swap_test_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing swap fill and balance checking");
    
    // Get or initialize shared signet setup (enforcer + sidechain, set up once for all tests)
    let shared_setup = get_or_init_shared_signet_setup(&bin_paths, res_tx.clone()).await?;
    tracing::info!("✓ Got shared signet setup");
    
    // Create isolated coinshift instance for this test
    let mut coinshift = shared_setup.create_coinshift_instance(Some("fill-swap-test".to_owned())).await?;
    tracing::info!("✓ Created isolated coinshift instance");
    
    // Get deposit address
    let deposit_address = coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    // Lock the shared enforcer for the deposit operation
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = deposit(
            &mut *enforcer_guard,
            &mut coinshift,
            &deposit_address,
            DEPOSIT_AMOUNT,
            DEPOSIT_FEE,
        )
        .await?;
    }
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = coinshift.bmm_single(&mut *enforcer_guard).await?;
    }
    tracing::info!("✓ BMM complete");
    
    // Get initial balance
    let initial_balance = coinshift.rpc_client.balance().await?;
    tracing::info!("=== INITIAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", initial_balance.total.to_sat());
    tracing::info!("  Available: {} sats", initial_balance.available.to_sat());
    
    // Get a new address for the swap recipient
    let l2_recipient_address = coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getnewaddress", [])
            .run_utf8()
            .await?
    };
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = coinshift
        .rpc_client
        .create_swap(
            ParentChainType::Signet,
            l1_recipient_address.clone(),
            L1_AMOUNT_SATS,
            Some(l2_recipient_address),
            L2_AMOUNT_SATS,
            None, // Use default confirmations
            SWAP_FEE_SATS,
        )
        .await?;
    tracing::info!("✓ Swap created successfully");
    tracing::info!("  Swap ID: {}", swap_id);
    tracing::info!("  Coinshift TXID (L2): {:?}", swap_txid);
    
    // Wait longer for the swap transaction to be fully processed and the node to be ready
    // Swap transactions may need more time to be processed than regular transactions
    // Also need to ensure mainchain sync is complete before attempting to mine
    tracing::debug!("Waiting for swap transaction to be fully processed and mainchain sync...");
    
    // Wait and check mainchain sync status multiple times
    let mut mainchain_ready = false;
    for attempt in 1..=10 {
        sleep(std::time::Duration::from_secs(1)).await;
        let best_main_hash = coinshift.rpc_client.get_best_mainchain_block_hash().await?;
        if best_main_hash.is_some() {
            tracing::debug!("Mainchain appears synced (attempt {}/10)", attempt);
            mainchain_ready = true;
            break;
        }
        tracing::debug!("Waiting for mainchain sync (attempt {}/10)...", attempt);
    }
    if !mainchain_ready {
        tracing::warn!("Mainchain sync check incomplete, but proceeding with BMM");
    }
    
    // Additional wait to ensure transaction is in mempool and node is ready
    sleep(std::time::Duration::from_secs(3)).await;
    
    // Debug: Check state before BMM
    let block_count_before = coinshift.rpc_client.getblockcount().await?;
    tracing::debug!("Block count before BMM: {}", block_count_before);
    
    // Check swap status before BMM (may be None if not yet in a block)
    let swap_status_before = coinshift.rpc_client.get_swap_status(swap_id).await?;
    tracing::debug!("Swap status before BMM: {:?}", swap_status_before.as_ref().map(|s| &s.state));
    
    // Check UTXOs to see if swap transaction created any outputs
    let utxos_before = coinshift.rpc_client.list_utxos().await?;
    let swap_utxos: Vec<_> = utxos_before
        .iter()
        .filter(|utxo| {
            matches!(utxo.output.content, coinshift::types::OutputContent::SwapPending { .. })
        })
        .collect();
    tracing::debug!("Found {} swap pending UTXOs before BMM", swap_utxos.len());
    
    // Check balance before BMM
    let balance_before = coinshift.rpc_client.balance().await?;
    tracing::debug!("Balance before BMM: total={} sats, available={} sats", 
        balance_before.total.to_sat(), balance_before.available.to_sat());
    
    // Final mainchain sync check
    let best_main_hash = coinshift.rpc_client.get_best_mainchain_block_hash().await?;
    tracing::debug!("Best mainchain block hash before BMM: {:?}", best_main_hash);
    
    // Mine a block to confirm the swap transaction
    tracing::info!("BMMing block to confirm swap transaction");
    let mut bmm_result = {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        coinshift.bmm_single(&mut *enforcer_guard).await
    };
    if let Err(ref err) = bmm_result {
        tracing::error!("First BMM attempt failed with error: {:#}", err);
        tracing::error!("Error source chain: {:?}", err.source());
        tracing::warn!("First BMM attempt failed (likely mainchain task timeout), waiting longer for mainchain task to process and retrying...");
        // Wait longer to give mainchain task time to process any pending requests
        sleep(std::time::Duration::from_secs(10)).await;
        
        // Debug: Check state again before retry
        let block_count_retry = coinshift.rpc_client.getblockcount().await?;
        tracing::debug!("Block count before retry: {} (was {})", block_count_retry, block_count_before);
        
        bmm_result = {
            let mut enforcer_guard = shared_setup.enforcer.lock().await;
            coinshift.bmm_single(&mut *enforcer_guard).await
        };
        if let Err(ref err) = bmm_result {
            tracing::error!("Second BMM attempt failed with error: {:#}", err);
            tracing::error!("Error source chain: {:?}", err.source());
            tracing::warn!("Second BMM attempt also failed, waiting even longer for mainchain task to process and retrying once more...");
            // Wait even longer on second retry
            sleep(std::time::Duration::from_secs(10)).await;
            
            // Debug: Check state again before final retry
            let block_count_final = coinshift.rpc_client.getblockcount().await?;
            tracing::debug!("Block count before final retry: {} (was {})", block_count_final, block_count_before);
            
            bmm_result = {
                let mut enforcer_guard = shared_setup.enforcer.lock().await;
                coinshift.bmm_single(&mut *enforcer_guard).await
            };
            if let Err(ref err) = bmm_result {
                tracing::error!("Third BMM attempt failed with error: {:#}", err);
                tracing::error!("Error source chain: {:?}", err.source());
                // Final debug: Check state after all failures
                let block_count_after = coinshift.rpc_client.getblockcount().await?;
                tracing::error!("Block count after all BMM failures: {} (was {})", block_count_after, block_count_before);
                let swap_status_after = coinshift.rpc_client.get_swap_status(swap_id).await?;
                tracing::error!("Swap status after all BMM failures: {:?}", swap_status_after.as_ref().map(|s| &s.state));
            }
        }
    }
    let () = bmm_result?;
    
    // Verify block count increased
    let block_count_after = coinshift.rpc_client.getblockcount().await?;
    tracing::info!("✓ BMM complete (block {} -> {})", block_count_before, block_count_after);
    anyhow::ensure!(
        block_count_after > block_count_before,
        "Block count should increase after BMM (was {}, now {})",
        block_count_before,
        block_count_after
    );
    
    // Wait a bit for the block to be fully processed
    sleep(std::time::Duration::from_millis(500)).await;
    
    // Check balance after swap creation
    let balance_after_swap = coinshift.rpc_client.balance().await?;
    tracing::info!("=== BALANCE AFTER SWAP CREATION ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", balance_after_swap.total.to_sat());
    tracing::info!("  Available: {} sats", balance_after_swap.available.to_sat());
    
    // Read swap to get details
    let mut swap_status: Option<coinshift::types::Swap> = None;
    for attempt in 1..=5 {
        swap_status = coinshift
            .rpc_client
            .get_swap_status(swap_id)
            .await?;
        if swap_status.is_some() {
            break;
        }
        if attempt < 5 {
            sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    anyhow::ensure!(
        swap_status.is_some(),
        "Swap status should be available"
    );
    let swap = swap_status.unwrap();
    tracing::info!("✓ Swap read successfully");
    tracing::info!("  State: {:?}", swap.state);
    tracing::info!("  L1 Recipient: {}", swap.l1_recipient_address.as_ref().unwrap_or(&"None".to_string()));
    
    // Fill the swap by sending L1 transaction (Signet)
    tracing::info!("=== FILLING SWAP ===");
    tracing::info!("Sending {} sats to {} on Signet", L1_AMOUNT_SATS, l1_recipient_address);
    
    // Note: We don't check balance upfront - the transaction will fail with a clear error
    // if there are insufficient funds. The enforcer should be funded during test setup.
    
    // Send L1 transaction to fill the swap
    // Try sendtoaddress first, and if it fails with fee estimation error, 
    // use walletcreatefundedpsbt with explicit fee_rate as fallback
    // If insufficient funds, mine blocks to fund the wallet
    let l1_txid_str = loop {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        
        // First, try the simple sendtoaddress approach
        let result = enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>(
                [],
                "sendtoaddress",
                [l1_recipient_address.clone(), L1_AMOUNT_SATS.to_string()],
            )
            .run_utf8()
            .await;
        
        match result {
            Ok(txid) => break txid,
            Err(e) => {
                let err_str = format!("{}", e);
                // If it's an insufficient funds error, mine blocks to fund the wallet
                if err_str.contains("Insufficient funds") {
                    drop(enforcer_guard); // Release lock before mining
                    
                    tracing::info!("Enforcer has insufficient funds, attempting to fund by mining signet blocks...");
                    
                    // Mine blocks to fund the wallet
                    // On signet, each block gives ~0.00003125 BTC, so we need several blocks
                    // IMPORTANT: Use the enforcer's mining_address which is already in the wallet
                    let enforcer_guard_mut = shared_setup.enforcer.lock().await;
                    
                    // Use the enforcer's mining_address (already in wallet, no need to create new address)
                    let mining_address = enforcer_guard_mut.mining_address.to_string();
                    tracing::info!("=== MINING SETUP ===");
                    tracing::info!("Mining to enforcer wallet address: {}", mining_address);
                    
                    // Verify address is in wallet
                    let address_info = enforcer_guard_mut
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getaddressinfo", [mining_address.clone()])
                        .run_utf8()
                        .await;
                    match address_info {
                        Ok(info) => {
                            tracing::info!("Address info: {}", info.trim());
                        }
                        Err(e) => {
                            tracing::warn!("Could not get address info (may not be in wallet yet): {:#}", e);
                        }
                    }
                    
                    // Check block count before mining
                    let block_count_before: u32 = enforcer_guard_mut
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getblockcount", [])
                        .run_utf8()
                        .await?
                        .parse()?;
                    tracing::info!("Block count before mining: {}", block_count_before);
                    
                    // Check current balance first
                    let initial_balance_str = enforcer_guard_mut
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getbalance", [])
                        .run_utf8()
                        .await
                        .unwrap_or_else(|_| "0".to_string());
                    let initial_balance_btc: f64 = initial_balance_str.trim().parse().unwrap_or(0.0);
                    tracing::info!("Current enforcer balance: {} BTC ({} sats)", 
                        initial_balance_str.trim(), (initial_balance_btc * 100_000_000.0) as u64);
                    
                    let required_btc = (L1_AMOUNT_SATS as f64 / 100_000_000.0) + 0.0001; // Add buffer for fees
                    let blocks_to_mine = ((required_btc / 0.00003125).ceil() as u32).max(10); // At least 10 blocks
                    tracing::info!("Mining {} signet blocks to fund enforcer wallet (need {} BTC, have {} BTC)...", 
                        blocks_to_mine, required_btc, initial_balance_btc);
                    
                    // Mine blocks to the enforcer's wallet address using signet miner
                    // On signet, we must use the signet miner (generatetoaddress only works on regtest)
                    // Check balance every 5 blocks to detect when sufficient funds arrive
                    let required_btc_f64 = required_btc;
                    let mut enforcer_guard_current = enforcer_guard_mut;
                    let mut blocks_mined = 0;
                    tracing::info!("Starting mining loop: will attempt to mine {} blocks", blocks_to_mine);
                    for i in 0..blocks_to_mine {
                        // Use signet_miner to mine blocks (required for signet, not generatetoaddress)
                        tracing::debug!("Calling signet_miner for block {} (attempt {}/{})", i + 1, i + 1, blocks_to_mine);
                        let mine_output = enforcer_guard_current
                            .signet_miner
                            .command(
                                "generate",
                                vec![
                                    "--address",
                                    &mining_address,
                                    "--block-interval",
                                    "1",
                                ],
                            )
                            .run_utf8()
                            .await;
                        
                        match mine_output {
                            Ok(output) => {
                                blocks_mined += 1;
                                tracing::info!("✓ Mined funding block {} to address {} (output: {})", 
                                    blocks_mined, mining_address, output.trim());
                                
                                // Verify block count increased after each block
                                drop(enforcer_guard_current);
                                sleep(std::time::Duration::from_millis(500)).await;
                                let enforcer_guard_check = shared_setup.enforcer.lock().await;
                                let block_count_check: u32 = enforcer_guard_check
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getblockcount", [])
                                    .run_utf8()
                                    .await?
                                    .parse()?;
                                tracing::debug!("Block count after block {}: {} (was {})", 
                                    blocks_mined, block_count_check, block_count_before);
                                enforcer_guard_current = enforcer_guard_check;
                            }
                            Err(e) => {
                                tracing::error!("✗ Failed to mine block {}: {:#}", i + 1, e);
                                tracing::error!("Mining address used: {}", mining_address);
                                tracing::error!("Command: signet_miner generate --address {} --block-interval 1", mining_address);
                                return Err(anyhow::anyhow!("Failed to mine signet block {}: {:#}", i + 1, e));
                            }
                        }
                        
                        // Check balance every 5 blocks (even if enforcer is locked)
                        if (i + 1) % 5 == 0 || i == blocks_to_mine - 1 {
                            // Release lock briefly to check balance, then reacquire if needed
                            drop(enforcer_guard_current);
                            sleep(std::time::Duration::from_millis(2000)).await; // Wait for wallet to update
                            
                            let enforcer_guard_check = shared_setup.enforcer.lock().await;
                            
                            // Verify block count increased
                            let block_count: u32 = enforcer_guard_check
                                .bitcoin_cli
                                .command::<String, _, String, _, _>([], "getblockcount", [])
                                .run_utf8()
                                .await?
                                .parse()?;
                            let blocks_added = block_count.saturating_sub(block_count_before);
                            tracing::info!("Block count: {} (added {} blocks, started at {})", 
                                block_count, blocks_added, block_count_before);
                            
                            // Check if blocks were actually added
                            if blocks_added < blocks_mined {
                                tracing::warn!("Warning: Mined {} blocks but only {} blocks were added to chain!", 
                                    blocks_mined, blocks_added);
                            }
                            
                            let balance_str = enforcer_guard_check
                                .bitcoin_cli
                                .command::<String, _, String, _, _>([], "getbalance", [])
                                .run_utf8()
                                .await
                                .unwrap_or_else(|_| "0".to_string());
                            let balance_btc: f64 = balance_str.trim().parse().unwrap_or(0.0);
                            let balance_sats = (balance_btc * 100_000_000.0) as u64;
                            tracing::info!("Enforcer balance after {} blocks: {} BTC ({} sats) - required: {} BTC ({} sats)", 
                                blocks_mined, balance_str.trim(), balance_sats, required_btc_f64, (required_btc_f64 * 100_000_000.0) as u64);
                            
                            // Check received by address to verify funds arrived at mining address
                            let received_str = enforcer_guard_check
                                .bitcoin_cli
                                .command::<String, _, String, _, _>([], "getreceivedbyaddress", [mining_address.clone(), "0".to_string()])
                                .run_utf8()
                                .await
                                .unwrap_or_else(|_| "0".to_string());
                            let received_btc: f64 = received_str.trim().parse().unwrap_or(0.0);
                            tracing::info!("Total received by mining address {}: {} BTC ({} sats)", 
                                mining_address, received_str.trim(), (received_btc * 100_000_000.0) as u64);
                            
                            // If we have sufficient funds, we can stop mining early
                            if balance_btc >= required_btc_f64 {
                                tracing::info!("Sufficient funds received after {} blocks, stopping early", blocks_mined);
                                drop(enforcer_guard_check);
                                break;
                            }
                            
                            // Reacquire lock for next iteration
                            enforcer_guard_current = enforcer_guard_check;
                        }
                    }
                    
                    tracing::info!("=== MINING COMPLETE ===");
                    tracing::info!("Mined {} blocks total", blocks_mined);
                    
                    // Final balance check - wait a bit longer and ensure wallet is synced
                    sleep(std::time::Duration::from_secs(2)).await;
                    let enforcer_guard_final = shared_setup.enforcer.lock().await;
                    
                    // Verify final block count
                    let final_block_count: u32 = enforcer_guard_final
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getblockcount", [])
                        .run_utf8()
                        .await?
                        .parse()?;
                    let total_blocks_added = final_block_count.saturating_sub(block_count_before);
                    tracing::info!("Final block count: {} (added {} blocks total)", final_block_count, total_blocks_added);
                    
                    // Try to sync the wallet to ensure it sees the new blocks
                    let _sync_result = enforcer_guard_final
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "syncwithvalidationinterfacequeue", [])
                        .run_utf8()
                        .await;
                    
                    let balance_str = enforcer_guard_final
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getbalance", [])
                        .run_utf8()
                        .await
                        .unwrap_or_else(|_| "0".to_string());
                    let balance_btc_final: f64 = balance_str.trim().parse().unwrap_or(0.0);
                    let balance_sats_final = (balance_btc_final * 100_000_000.0) as u64;
                    tracing::info!("Final enforcer balance: {} BTC ({} sats) - started with {} BTC ({} sats)", 
                        balance_str.trim(), balance_sats_final, 
                        initial_balance_str.trim(), (initial_balance_btc * 100_000_000.0) as u64);
                    
                    // Check received by mining address
                    let received_final_str = enforcer_guard_final
                        .bitcoin_cli
                        .command::<String, _, String, _, _>([], "getreceivedbyaddress", [mining_address.clone(), "0".to_string()])
                        .run_utf8()
                        .await
                        .unwrap_or_else(|_| "0".to_string());
                    let received_final_btc: f64 = received_final_str.trim().parse().unwrap_or(0.0);
                    tracing::info!("Final received by mining address {}: {} BTC ({} sats)", 
                        mining_address, received_final_str.trim(), (received_final_btc * 100_000_000.0) as u64);
                    drop(enforcer_guard_final);
                    
                    // Continue loop to retry sendtoaddress
                    continue;
                }
                // If it's a fee estimation error, use PSBT approach with explicit fees
                if err_str.contains("Fee estimation failed") || err_str.contains("fallbackfee") {
                    tracing::debug!("Fee estimation failed, using PSBT with explicit fee_rate");
                    // Use walletcreatefundedpsbt with explicit fee_rate
                    let amount_btc = L1_AMOUNT_SATS as f64 / 100_000_000.0;
                    let outputs_json = format!(r#"[{{"{}": {}}}]"#, l1_recipient_address, amount_btc);
                    let options_json = r#"{"fee_rate": 1}"#; // 1 sat/vB
                    // Store these for potential retry after funding
                    let outputs_json_clone = outputs_json.clone();
                    let psbt_result = enforcer_guard
                        .bitcoin_cli
                        .command::<String, _, String, _, _>(
                            [],
                            "walletcreatefundedpsbt",
                            [
                                "[]".to_string(),
                                outputs_json,
                                "0".to_string(),
                                options_json.to_string(),
                                "false".to_string(),
                            ],
                        )
                        .run_utf8()
                        .await;
                    
                    let psbt = match psbt_result {
                        Ok(p) => p,
                        Err(psbt_err) => {
                            let psbt_err_str = format!("{}", psbt_err);
                            if psbt_err_str.contains("Insufficient funds") {
                                drop(enforcer_guard); // Release lock before mining
                                
                                // Try to fund the wallet by mining blocks
                                tracing::info!("Enforcer has insufficient funds in PSBT creation, attempting to fund by mining signet blocks...");
                                
                                // Mine blocks to fund the wallet
                                // IMPORTANT: Use the enforcer's mining_address which is already in the wallet
                                let enforcer_guard_mut = shared_setup.enforcer.lock().await;
                                
                                // Use the enforcer's mining_address (already in wallet, no need to create new address)
                                let mining_address = enforcer_guard_mut.mining_address.to_string();
                                tracing::info!("=== MINING SETUP (PSBT) ===");
                                tracing::info!("Mining to enforcer wallet address: {}", mining_address);
                                
                                // Verify address is in wallet
                                let address_info = enforcer_guard_mut
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getaddressinfo", [mining_address.clone()])
                                    .run_utf8()
                                    .await;
                                match address_info {
                                    Ok(info) => {
                                        tracing::info!("Address info: {}", info.trim());
                                    }
                                    Err(e) => {
                                        tracing::warn!("Could not get address info (may not be in wallet yet): {:#}", e);
                                    }
                                }
                                
                                // Check block count before mining
                                let block_count_before: u32 = enforcer_guard_mut
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getblockcount", [])
                                    .run_utf8()
                                    .await?
                                    .parse()?;
                                tracing::info!("Block count before mining: {}", block_count_before);
                                
                                // Check current balance first
                                let initial_balance_str = enforcer_guard_mut
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getbalance", [])
                                    .run_utf8()
                                    .await
                                    .unwrap_or_else(|_| "0".to_string());
                                let initial_balance_btc: f64 = initial_balance_str.trim().parse().unwrap_or(0.0);
                                tracing::info!("Current enforcer balance: {} BTC ({} sats)", 
                                    initial_balance_str.trim(), (initial_balance_btc * 100_000_000.0) as u64);
                                
                                let required_btc = (L1_AMOUNT_SATS as f64 / 100_000_000.0) + 0.0001; // Add buffer for fees
                                let blocks_to_mine = ((required_btc / 0.00003125).ceil() as u32).max(10); // At least 10 blocks
                                tracing::info!("Mining {} signet blocks to fund enforcer wallet (need {} BTC, have {} BTC)...", 
                                    blocks_to_mine, required_btc, initial_balance_btc);
                                
                                // Mine blocks to the enforcer's wallet address using signet miner
                                // On signet, we must use the signet miner (generatetoaddress only works on regtest)
                                // Check balance every 5 blocks to detect when sufficient funds arrive
                                let required_btc_f64 = required_btc;
                                let mut enforcer_guard_current = enforcer_guard_mut;
                                let mut blocks_mined = 0;
                                tracing::info!("Starting mining loop: will attempt to mine {} blocks", blocks_to_mine);
                                for i in 0..blocks_to_mine {
                                    // Use signet_miner to mine blocks (required for signet, not generatetoaddress)
                                    tracing::debug!("Calling signet_miner for block {} (attempt {}/{})", i + 1, i + 1, blocks_to_mine);
                                    let mine_output = enforcer_guard_current
                                        .signet_miner
                                        .command(
                                            "generate",
                                            vec![
                                                "--address",
                                                &mining_address,
                                                "--block-interval",
                                                "1",
                                            ],
                                        )
                                        .run_utf8()
                                        .await;
                                    
                                    match mine_output {
                                        Ok(output) => {
                                            blocks_mined += 1;
                                            tracing::info!("✓ Mined funding block {} to address {} (output: {})", 
                                                blocks_mined, mining_address, output.trim());
                                            
                                            // Verify block count increased after each block
                                            drop(enforcer_guard_current);
                                            sleep(std::time::Duration::from_millis(500)).await;
                                            let enforcer_guard_check = shared_setup.enforcer.lock().await;
                                            let block_count_check: u32 = enforcer_guard_check
                                                .bitcoin_cli
                                                .command::<String, _, String, _, _>([], "getblockcount", [])
                                                .run_utf8()
                                                .await?
                                                .parse()?;
                                            tracing::debug!("Block count after block {}: {} (was {})", 
                                                blocks_mined, block_count_check, block_count_before);
                                            enforcer_guard_current = enforcer_guard_check;
                                        }
                                        Err(e) => {
                                            tracing::error!("✗ Failed to mine block {}: {:#}", i + 1, e);
                                            tracing::error!("Mining address used: {}", mining_address);
                                            tracing::error!("Command: signet_miner generate --address {} --block-interval 1", mining_address);
                                            return Err(anyhow::anyhow!("Failed to mine signet block {}: {:#}", i + 1, e));
                                        }
                                    }
                                    
                                    // Check balance every 5 blocks (even if enforcer is locked)
                                    if (i + 1) % 5 == 0 || i == blocks_to_mine - 1 {
                                        // Release lock briefly to check balance, then reacquire if needed
                                        drop(enforcer_guard_current);
                                        sleep(std::time::Duration::from_millis(2000)).await; // Wait for wallet to update
                                        
                                        let enforcer_guard_check = shared_setup.enforcer.lock().await;
                                        
                                        // Verify block count increased
                                        let block_count: u32 = enforcer_guard_check
                                            .bitcoin_cli
                                            .command::<String, _, String, _, _>([], "getblockcount", [])
                                            .run_utf8()
                                            .await?
                                            .parse()?;
                                        let blocks_added = block_count.saturating_sub(block_count_before);
                                        tracing::info!("Block count: {} (added {} blocks, started at {})", 
                                            block_count, blocks_added, block_count_before);
                                        
                                        // Check if blocks were actually added
                                        if blocks_added < blocks_mined {
                                            tracing::warn!("Warning: Mined {} blocks but only {} blocks were added to chain!", 
                                                blocks_mined, blocks_added);
                                        }
                                        
                                        let balance_str = enforcer_guard_check
                                            .bitcoin_cli
                                            .command::<String, _, String, _, _>([], "getbalance", [])
                                            .run_utf8()
                                            .await
                                            .unwrap_or_else(|_| "0".to_string());
                                        let balance_btc: f64 = balance_str.trim().parse().unwrap_or(0.0);
                                        let balance_sats = (balance_btc * 100_000_000.0) as u64;
                                        tracing::info!("Enforcer balance after {} blocks: {} BTC ({} sats) - required: {} BTC ({} sats)", 
                                            blocks_mined, balance_str.trim(), balance_sats, required_btc_f64, (required_btc_f64 * 100_000_000.0) as u64);
                                        
                                        // Check received by address to verify funds arrived at mining address
                                        let received_str = enforcer_guard_check
                                            .bitcoin_cli
                                            .command::<String, _, String, _, _>([], "getreceivedbyaddress", [mining_address.clone(), "0".to_string()])
                                            .run_utf8()
                                            .await
                                            .unwrap_or_else(|_| "0".to_string());
                                        let received_btc: f64 = received_str.trim().parse().unwrap_or(0.0);
                                        tracing::info!("Total received by mining address {}: {} BTC ({} sats)", 
                                            mining_address, received_str.trim(), (received_btc * 100_000_000.0) as u64);
                                        
                                        // If we have sufficient funds, we can stop mining early
                                        if balance_btc >= required_btc_f64 {
                                            tracing::info!("Sufficient funds received after {} blocks, stopping early", blocks_mined);
                                            drop(enforcer_guard_check);
                                            break;
                                        }
                                        
                                        // Reacquire lock for next iteration
                                        enforcer_guard_current = enforcer_guard_check;
                                    }
                                }
                                
                                tracing::info!("=== MINING COMPLETE (PSBT) ===");
                                tracing::info!("Mined {} blocks total", blocks_mined);
                                
                                // Final balance check before retrying PSBT creation
                                // Wait a bit longer and ensure wallet is synced
                                sleep(std::time::Duration::from_secs(2)).await;
                                let enforcer_guard_retry = shared_setup.enforcer.lock().await;
                                
                                // Verify final block count
                                let final_block_count: u32 = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getblockcount", [])
                                    .run_utf8()
                                    .await?
                                    .parse()?;
                                let total_blocks_added = final_block_count.saturating_sub(block_count_before);
                                tracing::info!("Final block count: {} (added {} blocks total)", final_block_count, total_blocks_added);
                                
                                // Try to sync the wallet to ensure it sees the new blocks
                                let _sync_result = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "syncwithvalidationinterfacequeue", [])
                                    .run_utf8()
                                    .await;
                                
                                let balance_str = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getbalance", [])
                                    .run_utf8()
                                    .await
                                    .unwrap_or_else(|_| "0".to_string());
                                let balance_btc_final: f64 = balance_str.trim().parse().unwrap_or(0.0);
                                let balance_sats_final = (balance_btc_final * 100_000_000.0) as u64;
                                tracing::info!("Final enforcer balance: {} BTC ({} sats) - started with {} BTC ({} sats)", 
                                    balance_str.trim(), balance_sats_final,
                                    initial_balance_str.trim(), (initial_balance_btc * 100_000_000.0) as u64);
                                
                                // Check received by mining address
                                let received_final_str = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>([], "getreceivedbyaddress", [mining_address.clone(), "0".to_string()])
                                    .run_utf8()
                                    .await
                                    .unwrap_or_else(|_| "0".to_string());
                                let received_final_btc: f64 = received_final_str.trim().parse().unwrap_or(0.0);
                                tracing::info!("Final received by mining address {}: {} BTC ({} sats)", 
                                    mining_address, received_final_str.trim(), (received_final_btc * 100_000_000.0) as u64);
                                
                                // Try PSBT creation again after funding
                                let psbt_retry = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>(
                                        [],
                                        "walletcreatefundedpsbt",
                                        [
                                            "[]".to_string(),
                                            outputs_json_clone,
                                            "0".to_string(),
                                            options_json.to_string(),
                                            "false".to_string(),
                                        ],
                                    )
                                    .run_utf8()
                                    .await?;
                                
                                // Process the PSBT and send transaction
                                let psbt_json: serde_json::Value = serde_json::from_str(psbt_retry.trim())?;
                                let psbt_str = psbt_json["psbt"].as_str().ok_or_else(|| anyhow::anyhow!("PSBT not found"))?;
                                
                                let signed_psbt = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>(
                                        [],
                                        "walletprocesspsbt",
                                        [psbt_str.to_string()],
                                    )
                                    .run_utf8()
                                    .await?;
                                
                                let signed_psbt_json: serde_json::Value = serde_json::from_str(signed_psbt.trim())?;
                                let final_psbt = signed_psbt_json["psbt"].as_str().ok_or_else(|| anyhow::anyhow!("PSBT not found"))?;
                                
                                let final_tx = enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>(
                                        [],
                                        "finalizepsbt",
                                        [final_psbt.to_string()],
                                    )
                                    .run_utf8()
                                    .await?;
                                
                                let final_tx_json: serde_json::Value = serde_json::from_str(final_tx.trim())?;
                                let hex_tx = final_tx_json["hex"].as_str().ok_or_else(|| anyhow::anyhow!("Transaction hex not found"))?;
                                
                                // Send the transaction and break with the txid
                                break enforcer_guard_retry
                                    .bitcoin_cli
                                    .command::<String, _, String, _, _>(
                                        [],
                                        "sendrawtransaction",
                                        [hex_tx.to_string()],
                                    )
                                    .run_utf8()
                                    .await?;
                            }
                            return Err(psbt_err.into());
                        }
                    };
                    
                    let psbt_json: serde_json::Value = serde_json::from_str(psbt.trim())?;
                    let psbt_str = psbt_json["psbt"].as_str().ok_or_else(|| anyhow::anyhow!("PSBT not found"))?;
                    
                    let signed_psbt = enforcer_guard
                        .bitcoin_cli
                        .command::<String, _, String, _, _>(
                            [],
                            "walletprocesspsbt",
                            [psbt_str.to_string()],
                        )
                        .run_utf8()
                        .await?;
                    
                    let signed_psbt_json: serde_json::Value = serde_json::from_str(signed_psbt.trim())?;
                    let final_psbt = signed_psbt_json["psbt"].as_str().ok_or_else(|| anyhow::anyhow!("PSBT not found"))?;
                    
                    let final_tx = enforcer_guard
                        .bitcoin_cli
                        .command::<String, _, String, _, _>(
                            [],
                            "finalizepsbt",
                            [final_psbt.to_string()],
                        )
                        .run_utf8()
                        .await?;
                    
                    let final_tx_json: serde_json::Value = serde_json::from_str(final_tx.trim())?;
                    let hex_tx = final_tx_json["hex"].as_str().ok_or_else(|| anyhow::anyhow!("Transaction hex not found"))?;
                    
                    // Send the transaction and break with the txid
                    break enforcer_guard
                        .bitcoin_cli
                        .command::<String, _, String, _, _>(
                            [],
                            "sendrawtransaction",
                            [hex_tx.to_string()],
                        )
                        .run_utf8()
                        .await?;
                } else {
                    // For other errors, return the original error
                    return Err(e.into());
                }
            }
        }
    };
    let l1_txid: bitcoin::Txid = l1_txid_str.trim().parse()?;
    tracing::info!("✓ L1 transaction sent (Signet TXID): {}", l1_txid);
    
    // Mine signet blocks to confirm the transaction
    tracing::info!("Mining signet blocks to confirm L1 transaction...");
    for i in 0..3 {
        use bip300301_enforcer_integration_tests::mine::mine;
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        mine::<PostSetup>(&mut *enforcer_guard, 1, Some(true)).await?;
        tracing::info!("  Mined signet block {}", i + 1);
    }
    
    // Update swap with L1 transaction ID
    tracing::info!("Updating swap with L1 transaction ID...");
    let l1_txid_bytes: &[u8] = l1_txid.as_ref();
    coinshift
        .rpc_client
        .update_swap_l1_txid(swap_id, hex::encode(l1_txid_bytes), 3)
        .await?;
    tracing::info!("✓ Swap updated with L1 transaction ID");
    
    // Wait a bit for processing
    sleep(std::time::Duration::from_secs(1)).await;
    
    // Read swap again to check state
    let swap_after_fill = coinshift
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found after fill"))?;
    tracing::info!("✓ Swap state after fill: {:?}", swap_after_fill.state);
    
    // Check final balances
    let final_balance = coinshift.rpc_client.balance().await?;
    tracing::info!("=== FINAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", final_balance.total.to_sat());
    tracing::info!("  Available: {} sats", final_balance.available.to_sat());
    
    // Print all transaction IDs
    tracing::info!("=== TRANSACTION IDs ===");
    tracing::info!("Coinshift TXID (L2 swap creation): {:?}", swap_txid);
    tracing::info!("Signet TXID (L1 deposit/fill): {}", l1_txid);
    
    // Get signet block info to show it's on signet
    let signet_block_count: u32 = {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getblockcount", [])
            .run_utf8()
            .await?
            .parse()?
    };
    tracing::info!("Signet block count: {}", signet_block_count);
    
    // Get L2 block count
    let l2_block_count = coinshift.rpc_client.getblockcount().await?;
    tracing::info!("L2 (Coinshift) block count: {}", l2_block_count);
    
    // Cleanup - stop node gracefully first
    // Note: We don't cleanup the shared enforcer here as it's shared across tests
    let _unused = coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(coinshift);
    
    tracing::info!("✓ Fill swap test completed successfully");
    Ok(())
}

async fn fill_swap_test(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = fill_swap_test_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

fn fill_swap_test_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("fill_swap_test", fill_swap_test(bin_paths).boxed())
}

/// Test filling swap with regtest as parent_chain while sidechain uses signet
/// Alice creates a swap on the sidechain (signet) targeting regtest as parent_chain
/// Bob fills it using a transaction done on regtest
async fn fill_swap_regtest_parent_chain_test_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tracing::info!("Testing swap fill with regtest parent_chain (sidechain on signet)");
    
    // Get or initialize shared signet setup (enforcer + sidechain, set up once for all tests)
    let shared_setup = get_or_init_shared_signet_setup(&bin_paths, res_tx.clone()).await?;
    tracing::info!("✓ Got shared signet setup");
    
    // Create isolated coinshift instance for this test
    let mut coinshift = shared_setup.create_coinshift_instance(Some("fill-swap-regtest-parent-test".to_owned())).await?;
    tracing::info!("✓ Created isolated coinshift instance");
    
    // Get deposit address
    let deposit_address = coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    // Lock the shared enforcer for the deposit operation
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = deposit(
            &mut *enforcer_guard,
            &mut coinshift,
            &deposit_address,
            DEPOSIT_AMOUNT,
            DEPOSIT_FEE,
        )
        .await?;
    }
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        let () = coinshift.bmm_single(&mut *enforcer_guard).await?;
    }
    tracing::info!("✓ BMM complete");
    
    // Get initial balance
    let initial_balance = coinshift.rpc_client.balance().await?;
    tracing::info!("=== INITIAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", initial_balance.total.to_sat());
    tracing::info!("  Available: {} sats", initial_balance.available.to_sat());
    
    // Get a new address for the swap recipient
    let l2_recipient_address = coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1) with Regtest as parent_chain
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on regtest
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Set up a separate regtest instance for the parent_chain
    tracing::info!("Setting up regtest instance for parent_chain");
    use crate::setup::{Init, setup_regtest};
    use bip300301_enforcer_integration_tests::setup::Mode;
    let regtest_setup = setup_regtest(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("regtest-parent-chain".to_owned()),
        },
        Mode::Mempool,
        res_tx.clone(),
    )
    .await?;
    tracing::info!("✓ Regtest setup complete for parent_chain");
    
    // Get a regtest address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = {
        regtest_setup
            .enforcer
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getnewaddress", [])
            .run_utf8()
            .await?
    };
    tracing::info!("✓ Got L1 recipient address (Regtest): {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 (Signet sidechain) → {} sats L1 (Regtest parent_chain)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = coinshift
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,  // Use Regtest as parent_chain
            l1_recipient_address.clone(),
            L1_AMOUNT_SATS,
            Some(l2_recipient_address),
            L2_AMOUNT_SATS,
            None, // Use default confirmations
            SWAP_FEE_SATS,
        )
        .await?;
    tracing::info!("✓ Swap created successfully");
    tracing::info!("  Swap ID: {}", swap_id);
    tracing::info!("  Coinshift TXID (L2): {:?}", swap_txid);
    
    // Wait longer for the swap transaction to be fully processed and the node to be ready
    tracing::debug!("Waiting for swap transaction to be fully processed and mainchain sync...");
    
    // Wait and check mainchain sync status multiple times
    let mut mainchain_ready = false;
    for attempt in 1..=10 {
        sleep(std::time::Duration::from_secs(1)).await;
        let best_main_hash = coinshift.rpc_client.get_best_mainchain_block_hash().await?;
        if best_main_hash.is_some() {
            tracing::debug!("Mainchain appears synced (attempt {}/10)", attempt);
            mainchain_ready = true;
            break;
        }
        tracing::debug!("Waiting for mainchain sync (attempt {}/10)...", attempt);
    }
    if !mainchain_ready {
        tracing::warn!("Mainchain sync check incomplete, but proceeding with BMM");
    }
    
    // Additional wait to ensure transaction is in mempool and node is ready
    sleep(std::time::Duration::from_secs(3)).await;
    
    // Debug: Check state before BMM
    let block_count_before = coinshift.rpc_client.getblockcount().await?;
    tracing::debug!("Block count before BMM: {}", block_count_before);
    
    // Mine a block to confirm the swap transaction (on signet sidechain)
    tracing::info!("BMMing block to confirm swap transaction (signet sidechain)");
    let mut bmm_result = {
        let mut enforcer_guard = shared_setup.enforcer.lock().await;
        coinshift.bmm_single(&mut *enforcer_guard).await
    };
    if let Err(ref err) = bmm_result {
        tracing::error!("First BMM attempt failed with error: {:#}", err);
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(10)).await;
        
        bmm_result = {
            let mut enforcer_guard = shared_setup.enforcer.lock().await;
            coinshift.bmm_single(&mut *enforcer_guard).await
        };
        if let Err(ref err) = bmm_result {
            tracing::error!("Second BMM attempt failed with error: {:#}", err);
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(10)).await;
            
            bmm_result = {
                let mut enforcer_guard = shared_setup.enforcer.lock().await;
                coinshift.bmm_single(&mut *enforcer_guard).await
            };
        }
    }
    let () = bmm_result?;
    
    // Verify block count increased
    let block_count_after = coinshift.rpc_client.getblockcount().await?;
    tracing::info!("✓ BMM complete (block {} -> {})", block_count_before, block_count_after);
    anyhow::ensure!(
        block_count_after > block_count_before,
        "Block count should increase after BMM (was {}, now {})",
        block_count_before,
        block_count_after
    );
    
    // Wait a bit for the block to be fully processed
    sleep(std::time::Duration::from_millis(500)).await;
    
    // Check balance after swap creation
    let balance_after_swap = coinshift.rpc_client.balance().await?;
    tracing::info!("=== BALANCE AFTER SWAP CREATION ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", balance_after_swap.total.to_sat());
    tracing::info!("  Available: {} sats", balance_after_swap.available.to_sat());
    
    // Read swap to get details
    let mut swap_status: Option<coinshift::types::Swap> = None;
    for attempt in 1..=5 {
        swap_status = coinshift
            .rpc_client
            .get_swap_status(swap_id)
            .await?;
        if swap_status.is_some() {
            break;
        }
        if attempt < 5 {
            sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    anyhow::ensure!(
        swap_status.is_some(),
        "Swap status should be available"
    );
    let swap = swap_status.unwrap();
    tracing::info!("✓ Swap read successfully");
    tracing::info!("  State: {:?}", swap.state);
    tracing::info!("  Parent Chain: {:?}", swap.parent_chain);
    tracing::info!("  L1 Recipient: {}", swap.l1_recipient_address.as_ref().unwrap_or(&"None".to_string()));
    anyhow::ensure!(
        swap.parent_chain == ParentChainType::Regtest,
        "Swap parent_chain should be Regtest, got {:?}",
        swap.parent_chain
    );
    
    // Fill the swap by sending L1 transaction on Regtest
    tracing::info!("=== FILLING SWAP ===");
    tracing::info!("Sending {} sats to {} on Regtest", L1_AMOUNT_SATS, l1_recipient_address);
    
    // Fund the regtest wallet first by mining some blocks
    tracing::info!("Mining regtest blocks to fund wallet...");
    use bip300301_enforcer_integration_tests::mine::mine;
    // Save out_dir before moving enforcer
    let regtest_out_dir = regtest_setup.enforcer.out_dir.clone();
    let mut regtest_enforcer = regtest_setup.enforcer;
    for i in 0..101 {
        mine::<PostSetup>(&mut regtest_enforcer, 1, Some(true)).await?;
        if (i + 1) % 10 == 0 {
            tracing::debug!("Mined {} regtest blocks", i + 1);
        }
    }
    tracing::info!("✓ Mined 101 regtest blocks to fund wallet");
    
    // Send L1 transaction to fill the swap on regtest
    let l1_txid_str = loop {
        let result = regtest_enforcer
            .bitcoin_cli
            .command::<String, _, String, _, _>(
                [],
                "sendtoaddress",
                [l1_recipient_address.clone(), L1_AMOUNT_SATS.to_string()],
            )
            .run_utf8()
            .await;
        
        match result {
            Ok(txid) => break txid,
            Err(e) => {
                let err_str = format!("{}", e);
                if err_str.contains("Insufficient funds") {
                    tracing::info!("Insufficient funds, mining more regtest blocks...");
                    mine::<PostSetup>(&mut regtest_enforcer, 10, Some(true)).await?;
                    sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                } else {
                    return Err(e.into());
                }
            }
        }
    };
    let l1_txid: bitcoin::Txid = l1_txid_str.trim().parse()?;
    tracing::info!("✓ L1 transaction sent (Regtest TXID): {}", l1_txid);
    
    // Mine regtest blocks to confirm the transaction
    tracing::info!("Mining regtest blocks to confirm L1 transaction...");
    for i in 0..3 {
        mine::<PostSetup>(&mut regtest_enforcer, 1, Some(true)).await?;
        tracing::info!("  Mined regtest block {}", i + 1);
    }
    
    // Update swap with L1 transaction ID
    tracing::info!("Updating swap with L1 transaction ID (Regtest)...");
    let l1_txid_bytes: &[u8] = l1_txid.as_ref();
    coinshift
        .rpc_client
        .update_swap_l1_txid(swap_id, hex::encode(l1_txid_bytes), 3)
        .await?;
    tracing::info!("✓ Swap updated with L1 transaction ID");
    
    // Wait a bit for processing
    sleep(std::time::Duration::from_secs(1)).await;
    
    // Read swap again to check state
    let swap_after_fill = coinshift
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found after fill"))?;
    tracing::info!("✓ Swap state after fill: {:?}", swap_after_fill.state);
    
    // Check final balances
    let final_balance = coinshift.rpc_client.balance().await?;
    tracing::info!("=== FINAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", final_balance.total.to_sat());
    tracing::info!("  Available: {} sats", final_balance.available.to_sat());
    
    // Print all transaction IDs
    tracing::info!("=== TRANSACTION IDs ===");
    tracing::info!("Coinshift TXID (L2 swap creation on Signet sidechain): {:?}", swap_txid);
    tracing::info!("Regtest TXID (L1 fill on Regtest parent_chain): {}", l1_txid);
    
    // Get regtest block info to show it's on regtest
    let regtest_block_count: u32 = regtest_enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getblockcount", [])
        .run_utf8()
        .await?
        .parse()?;
    tracing::info!("Regtest block count: {}", regtest_block_count);
    
    // Get signet block info to show sidechain is on signet
    let signet_block_count: u32 = {
        let enforcer_guard = shared_setup.enforcer.lock().await;
        enforcer_guard
            .bitcoin_cli
            .command::<String, _, String, _, _>([], "getblockcount", [])
            .run_utf8()
            .await?
            .parse()?
    };
    tracing::info!("Signet block count (sidechain mainchain): {}", signet_block_count);
    
    // Get L2 block count
    let l2_block_count = coinshift.rpc_client.getblockcount().await?;
    tracing::info!("L2 (Coinshift) block count: {}", l2_block_count);
    
    // Cleanup - stop node gracefully first
    // Note: We don't cleanup the shared enforcer here as it's shared across tests
    let _unused = coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(coinshift);
    
    // Cleanup regtest setup
    drop(regtest_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(regtest_enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    regtest_out_dir.cleanup()?;
    
    tracing::info!("✓ Fill swap with regtest parent_chain test completed successfully");
    Ok(())
}

async fn fill_swap_regtest_parent_chain_test(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = fill_swap_regtest_parent_chain_test_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

fn fill_swap_regtest_parent_chain_test_trial(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("fill_swap_regtest_parent_chain_test", fill_swap_regtest_parent_chain_test(bin_paths).boxed())
}

pub fn tests(
    bin_paths: BinPaths,
) -> Vec<AsyncTrial<BoxFuture<'static, anyhow::Result<()>>>> {
    vec![
        signet_setup_trial(bin_paths.clone()),
        regtest_setup_trial(bin_paths.clone()),
        deposit_withdraw_roundtrip(bin_paths.clone()),
        ibd_trial(bin_paths.clone()),
        unknown_withdrawal_trial(bin_paths.clone()),
        swap_test_trial(bin_paths.clone()),
        read_swap_test_trial(bin_paths.clone()),
        fill_swap_test_trial(bin_paths.clone()),
        fill_swap_regtest_parent_chain_test_trial(bin_paths),
    ]
}
