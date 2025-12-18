use bip300301_enforcer_integration_tests::{
    integration_test::deposit,
    setup::{Mode, Network, Sidechain},
    util::{AbortOnDrop, AsyncTrial},
};
use bitcoin::hashes::Hash;
use coinshift::types::ParentChainType;
use coinshift_app_rpc_api::RpcClient as _;
use futures::{FutureExt, future::BoxFuture, StreamExt as _, channel::mpsc};
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    ibd::ibd_trial,
    setup::{Init, PostSetup, setup_signet},
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
    
    // Setup signet network
    let mut complete_setup = setup_signet(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("swap-test".to_owned()),
        },
        res_tx.clone(),
    )
    .await?;
    tracing::info!("✓ Signet setup complete");
    
    // Get deposit address
    let deposit_address = complete_setup.coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    let () = deposit(
        &mut complete_setup.enforcer,
        &mut complete_setup.coinshift,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    let () = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await?;
    tracing::info!("✓ BMM complete");
    
    // Verify we have the deposit in UTXOs
    let utxos = complete_setup.coinshift.rpc_client.list_utxos().await?;
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
    let l2_recipient_address = complete_setup.coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    // Swap from sidechain to signet
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getnewaddress", [])
        .run_utf8()
        .await?;
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = complete_setup
        .coinshift
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
    let block_count_before = complete_setup.coinshift.rpc_client.getblockcount().await?;
    tracing::debug!("Block count before BMM: {}", block_count_before);
    
    // Mine a block to confirm the swap transaction
    // Retry BMM in case of network task cancellation errors
    tracing::info!("BMMing block to confirm swap transaction");
    let mut bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
    if bmm_result.is_err() {
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(3)).await;
        bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
        if bmm_result.is_err() {
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(3)).await;
            bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
        }
    }
    let () = bmm_result?;
    
    // Verify block count increased
    let block_count_after = complete_setup.coinshift.rpc_client.getblockcount().await?;
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
        swap_status = complete_setup
            .coinshift
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
    let all_swaps = complete_setup.coinshift.rpc_client.list_swaps().await?;
    anyhow::ensure!(
        all_swaps.iter().any(|s| s.id == swap_id),
        "Swap should appear in list_swaps"
    );
    tracing::info!("✓ Swap appears in swap list");
    
    // Cleanup - stop node gracefully first
    let _unused = complete_setup.coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(complete_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(complete_setup.enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    complete_setup.enforcer.out_dir.cleanup()?;
    
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
    
    // Setup signet network
    let mut complete_setup = setup_signet(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("read-swap-test".to_owned()),
        },
        res_tx.clone(),
    )
    .await?;
    tracing::info!("✓ Signet setup complete");
    
    // Get deposit address
    let deposit_address = complete_setup.coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    let () = deposit(
        &mut complete_setup.enforcer,
        &mut complete_setup.coinshift,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    let () = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await?;
    tracing::info!("✓ BMM complete");
    
    // Get a new address for the swap recipient
    let l2_recipient_address = complete_setup.coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getnewaddress", [])
        .run_utf8()
        .await?;
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = complete_setup
        .coinshift
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
    let mut bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
    if bmm_result.is_err() {
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(3)).await;
        bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
        if bmm_result.is_err() {
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(3)).await;
            bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
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
        swap_status = complete_setup
            .coinshift
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
    let _unused = complete_setup.coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(complete_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(complete_setup.enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    complete_setup.enforcer.out_dir.cleanup()?;
    
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
    
    // Setup signet network
    let mut complete_setup = setup_signet(
        &bin_paths,
        Init {
            coinshift_app: bin_paths.coinshift.clone(),
            data_dir_suffix: Some("fill-swap-test".to_owned()),
        },
        res_tx.clone(),
    )
    .await?;
    tracing::info!("✓ Signet setup complete");
    
    // Get deposit address
    let deposit_address = complete_setup.coinshift.get_deposit_address().await?;
    tracing::info!("✓ Got deposit address: {}", deposit_address);
    
    // Fund BTC from signet into the sidechain
    const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000_000); // 0.1 BTC
    const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000); // 0.00001 BTC fee
    
    tracing::info!("Depositing {} sats to sidechain", DEPOSIT_AMOUNT.to_sat());
    let () = deposit(
        &mut complete_setup.enforcer,
        &mut complete_setup.coinshift,
        &deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("✓ Deposited to sidechain successfully");
    
    // Confirm the deposit by BMMing a block
    tracing::info!("BMMing block to confirm deposit");
    let () = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await?;
    tracing::info!("✓ BMM complete");
    
    // Get initial balance
    let initial_balance = complete_setup.coinshift.rpc_client.balance().await?;
    tracing::info!("=== INITIAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", initial_balance.total.to_sat());
    tracing::info!("  Available: {} sats", initial_balance.available.to_sat());
    
    // Get a new address for the swap recipient
    let l2_recipient_address = complete_setup.coinshift.rpc_client.get_new_address().await?;
    tracing::info!("✓ Got L2 recipient address: {}", l2_recipient_address);
    
    // Create a swap (L2 → L1)
    const L1_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on signet
    const L2_AMOUNT_SATS: u64 = 5_000_000; // 0.05 BTC on sidechain
    const SWAP_FEE_SATS: u64 = 1_000; // Fee for swap transaction
    
    // Get a signet address for L1 recipient
    use bip300301_enforcer_lib::bins::CommandExt;
    let l1_recipient_address = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getnewaddress", [])
        .run_utf8()
        .await?;
    tracing::info!("✓ Got L1 recipient address: {}", l1_recipient_address);
    
    tracing::info!(
        "Creating swap: {} sats L2 → {} sats L1 (Signet)",
        L2_AMOUNT_SATS,
        L1_AMOUNT_SATS
    );
    let (swap_id, swap_txid) = complete_setup
        .coinshift
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
    let mut bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
    if bmm_result.is_err() {
        tracing::warn!("First BMM attempt failed, waiting longer and retrying...");
        sleep(std::time::Duration::from_secs(3)).await;
        bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
        if bmm_result.is_err() {
            tracing::warn!("Second BMM attempt also failed, waiting even longer and retrying once more...");
            sleep(std::time::Duration::from_secs(3)).await;
            bmm_result = complete_setup.coinshift.bmm_single(&mut complete_setup.enforcer).await;
        }
    }
    let () = bmm_result?;
    tracing::info!("✓ BMM complete");
    
    // Wait a bit for the block to be fully processed
    sleep(std::time::Duration::from_millis(500)).await;
    
    // Check balance after swap creation
    let balance_after_swap = complete_setup.coinshift.rpc_client.balance().await?;
    tracing::info!("=== BALANCE AFTER SWAP CREATION ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", balance_after_swap.total.to_sat());
    tracing::info!("  Available: {} sats", balance_after_swap.available.to_sat());
    
    // Read swap to get details
    let mut swap_status: Option<coinshift::types::Swap> = None;
    for attempt in 1..=5 {
        swap_status = complete_setup
            .coinshift
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
    
    // Send L1 transaction to fill the swap
    let l1_txid_str = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>(
            [],
            "sendtoaddress",
            [l1_recipient_address.clone(), L1_AMOUNT_SATS.to_string()],
        )
        .run_utf8()
        .await?;
    let l1_txid: bitcoin::Txid = l1_txid_str.trim().parse()?;
    tracing::info!("✓ L1 transaction sent (Signet TXID): {}", l1_txid);
    
    // Mine signet blocks to confirm the transaction
    tracing::info!("Mining signet blocks to confirm L1 transaction...");
    for i in 0..3 {
        use bip300301_enforcer_integration_tests::mine::mine;
        mine::<PostSetup>(&mut complete_setup.enforcer, 1, Some(true)).await?;
        tracing::info!("  Mined signet block {}", i + 1);
    }
    
    // Update swap with L1 transaction ID
    tracing::info!("Updating swap with L1 transaction ID...");
    let l1_txid_bytes: &[u8] = l1_txid.as_ref();
    complete_setup
        .coinshift
        .rpc_client
        .update_swap_l1_txid(swap_id, hex::encode(l1_txid_bytes), 3)
        .await?;
    tracing::info!("✓ Swap updated with L1 transaction ID");
    
    // Wait a bit for processing
    sleep(std::time::Duration::from_secs(1)).await;
    
    // Read swap again to check state
    let swap_after_fill = complete_setup
        .coinshift
        .rpc_client
        .get_swap_status(swap_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Swap not found after fill"))?;
    tracing::info!("✓ Swap state after fill: {:?}", swap_after_fill.state);
    
    // Check final balances
    let final_balance = complete_setup.coinshift.rpc_client.balance().await?;
    tracing::info!("=== FINAL BALANCES ===");
    tracing::info!("L2 Balance (Coinshift): {} sats", final_balance.total.to_sat());
    tracing::info!("  Available: {} sats", final_balance.available.to_sat());
    
    // Print all transaction IDs
    tracing::info!("=== TRANSACTION IDs ===");
    tracing::info!("Coinshift TXID (L2 swap creation): {:?}", swap_txid);
    tracing::info!("Signet TXID (L1 deposit/fill): {}", l1_txid);
    
    // Get signet block info to show it's on signet
    let signet_block_count: u32 = complete_setup
        .enforcer
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getblockcount", [])
        .run_utf8()
        .await?
        .parse()?;
    tracing::info!("Signet block count: {}", signet_block_count);
    
    // Get L2 block count
    let l2_block_count = complete_setup.coinshift.rpc_client.getblockcount().await?;
    tracing::info!("L2 (Coinshift) block count: {}", l2_block_count);
    
    // Cleanup - stop node gracefully first
    let _unused = complete_setup.coinshift.rpc_client.stop().await;
    sleep(std::time::Duration::from_secs(2)).await;
    drop(complete_setup.coinshift);
    sleep(std::time::Duration::from_secs(1)).await;
    drop(complete_setup.enforcer.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    complete_setup.enforcer.out_dir.cleanup()?;
    
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
        fill_swap_test_trial(bin_paths),
    ]
}
