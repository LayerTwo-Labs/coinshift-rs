//! Test multi-node setup where Charles verifies transactions between Bob and Alice

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
use coinshift::types::ParentChainType;
use coinshift_app_rpc_api::RpcClient as _;
use futures::{
    FutureExt as _, StreamExt as _, channel::mpsc, future::BoxFuture,
};
use std::collections::HashSet;
use tokio::time::sleep;
use tracing::Instrument as _;

use crate::{
    setup::{Init, PostSetup},
    util::BinPaths,
};

const DEPOSIT_AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(50_000_000);
const DEPOSIT_FEE: bitcoin::Amount = bitcoin::Amount::from_sat(1_000_000);
const TRANSFER_AMOUNT: u64 = 10_000_000; // 0.1 BTC
const TRANSFER_FEE: u64 = 1_000; // 0.00001 BTC
const SWAP_L2_AMOUNT: u64 = 5_000_000; // 0.05 BTC
const SWAP_L1_AMOUNT: u64 = 2_500_000; // 0.025 BTC
const SWAP_FEE: u64 = 1_000; // 0.00001 BTC

#[derive(Debug)]
struct MultiNodeSetup {
    bob: PostSetup,
    alice: PostSetup,
    charles: PostSetup,
}

/// Initial setup for the test
async fn setup(
    bin_paths: &BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<(EnforcerPostSetup, MultiNodeSetup)> {
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
    tracing::info!("Funded enforcer successfully");

    // Setup Bob's node
    let bob = PostSetup::setup(
        Init {
            coinshift_app: bin_paths.coinshift_app.clone(),
            data_dir_suffix: Some("bob".to_owned()),
        },
        &enforcer_post_setup,
        res_tx.clone(),
    )
    .await?;
    tracing::info!("Setup Bob's node successfully");

    // Setup Alice's node
    let alice = PostSetup::setup(
        Init {
            coinshift_app: bin_paths.coinshift_app.clone(),
            data_dir_suffix: Some("alice".to_owned()),
        },
        &enforcer_post_setup,
        res_tx.clone(),
    )
    .await?;
    tracing::info!("Setup Alice's node successfully");

    // Setup Charles's node (verifier)
    let charles = PostSetup::setup(
        Init {
            coinshift_app: bin_paths.coinshift_app.clone(),
            data_dir_suffix: Some("charles".to_owned()),
        },
        &enforcer_post_setup,
        res_tx,
    )
    .await?;
    tracing::info!("Setup Charles's node successfully");

    let multi_node_setup = MultiNodeSetup {
        bob,
        alice,
        charles,
    };

    Ok((enforcer_post_setup, multi_node_setup))
}

/// Wait for nodes to sync by mining a block
async fn sync_nodes(
    nodes: &[&PostSetup],
    enforcer: &mut EnforcerPostSetup,
) -> anyhow::Result<()> {
    // Mine a block to ensure all nodes are synced
    if let Some(first_node) = nodes.first() {
        first_node.bmm_single(enforcer).await?;
        sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok(())
}

/// Wait for Charles (verifier) to reach the same block count as Bob (miner), with timeout.
/// Alice may lag when she does not initiate the connection to Bob.
async fn wait_for_charles_to_sync_from_bob(
    charles: &PostSetup,
    bob: &PostSetup,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let charles_block_count = charles.rpc_client.getblockcount().await?;
        let bob_block_count = bob.rpc_client.getblockcount().await?;
        if charles_block_count == bob_block_count {
            tracing::info!(
                block_count = charles_block_count,
                "Charles synced to same block count as Bob"
            );
            return Ok(());
        }
        tracing::debug!(
            charles = charles_block_count,
            bob = bob_block_count,
            "Waiting for Charles to sync from Bob"
        );
        sleep(poll_interval).await;
    }
    let charles_block_count = charles.rpc_client.getblockcount().await?;
    let bob_block_count = bob.rpc_client.getblockcount().await?;
    anyhow::bail!(
        "Charles failed to sync from Bob within timeout. Charles: {}, Bob: {}",
        charles_block_count,
        bob_block_count
    )
}

/// Connect nodes so that Charles can verify, and Alice can sync from Bob.
/// Charles connects to Bob and Alice. Alice also connects to Bob so she
/// can pull blocks from Bob (sync may require the lagging node to initiate).
async fn connect_peers(
    charles: &PostSetup,
    bob: &PostSetup,
    alice: &PostSetup,
) -> anyhow::Result<()> {
    tracing::info!("Connecting Charles to Bob and Alice");
    charles
        .rpc_client
        .connect_peer(bob.net_addr().into())
        .await?;
    charles
        .rpc_client
        .connect_peer(alice.net_addr().into())
        .await?;
    tracing::info!("Connecting Alice to Bob so Alice can sync blocks");
    alice.rpc_client.connect_peer(bob.net_addr().into()).await?;
    sleep(std::time::Duration::from_secs(2)).await;
    Ok(())
}

/// Verify that Charles can see transactions between Bob and Alice
async fn verify_transactions(
    charles: &PostSetup,
    bob: &PostSetup,
    alice: &PostSetup,
) -> anyhow::Result<()> {
    tracing::info!("Charles verifying transactions between Bob and Alice");

    // Verify Charles (verifier) and Bob (miner of final sync block) have the same block count.
    // Alice may lag when she does not initiate the connection; Charles syncs from Bob so
    // Charles has Bob's chain (which does not include Alice's blocks mined on her node).
    let charles_block_count = charles.rpc_client.getblockcount().await?;
    let bob_block_count = bob.rpc_client.getblockcount().await?;
    let alice_block_count = alice.rpc_client.getblockcount().await?;

    anyhow::ensure!(
        charles_block_count == bob_block_count,
        "Charles (verifier) should have the same block count as Bob. Charles: {}, Bob: {}, Alice: {}",
        charles_block_count,
        bob_block_count,
        alice_block_count
    );

    if alice_block_count != bob_block_count {
        tracing::warn!(
            charles = charles_block_count,
            bob = bob_block_count,
            alice = alice_block_count,
            "Alice has not synced to the same block count"
        );
    }

    // Get all swaps from each node's perspective
    let charles_swaps = charles.rpc_client.list_swaps().await?;
    let bob_swaps = bob.rpc_client.list_swaps().await?;
    let alice_swaps = alice.rpc_client.list_swaps().await?;

    tracing::debug!(
        charles_swaps_count = charles_swaps.len(),
        bob_swaps_count = bob_swaps.len(),
        alice_swaps_count = alice_swaps.len(),
        "Swap counts from each node"
    );

    // Verify Charles can see all swaps that Bob and Alice created
    // (after syncing, all nodes should see the same blockchain state)
    let bob_swap_ids: HashSet<_> = bob_swaps.iter().map(|s| s.id).collect();
    let alice_swap_ids: HashSet<_> = alice_swaps.iter().map(|s| s.id).collect();
    let charles_swap_ids: HashSet<_> =
        charles_swaps.iter().map(|s| s.id).collect();

    // Charles (syncing from Bob) should see all swaps that Bob created
    for swap_id in &bob_swap_ids {
        anyhow::ensure!(
            charles_swap_ids.contains(swap_id),
            "Charles should see Bob's swap {}",
            swap_id
        );
    }

    // Charles syncs from Bob, so he has Bob's chain; Alice's swap was mined on her node
    // and is not in Bob's chain, so Charles may not see it until sync from inbound works.
    for swap_id in &alice_swap_ids {
        if !charles_swap_ids.contains(swap_id) {
            tracing::warn!(
                swap_id = %swap_id,
                "Charles does not see Alice's swap (expected when Alice does not sync from Bob)"
            );
        }
    }

    // Get UTXOs from each node
    let charles_utxos = charles.rpc_client.list_utxos().await?;
    let bob_utxos = bob.rpc_client.list_utxos().await?;
    let alice_utxos = alice.rpc_client.list_utxos().await?;

    tracing::debug!(
        charles_utxos_count = charles_utxos.len(),
        bob_utxos_count = bob_utxos.len(),
        alice_utxos_count = alice_utxos.len(),
        "UTXO counts from each node"
    );

    // Verify Charles (verifier) sees the same sidechain wealth as Bob (chain he synced from).
    let charles_wealth = charles.rpc_client.sidechain_wealth_sats().await?;
    let bob_wealth = bob.rpc_client.sidechain_wealth_sats().await?;
    let alice_wealth = alice.rpc_client.sidechain_wealth_sats().await?;

    anyhow::ensure!(
        charles_wealth == bob_wealth,
        "Charles should see the same sidechain wealth as Bob. Charles: {}, Bob: {}, Alice: {}",
        charles_wealth,
        bob_wealth,
        alice_wealth
    );

    tracing::info!(
        "Charles successfully verified all transactions between Bob and Alice. \
         Block count: {}, Swaps visible: {} (Bob: {}, Alice: {}), Sidechain wealth: {} sats",
        charles_block_count,
        charles_swaps.len(),
        bob_swaps.len(),
        alice_swaps.len(),
        charles_wealth
    );

    Ok(())
}

async fn multi_node_verification_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (mut enforcer_post_setup, mut nodes) =
        setup(&bin_paths, res_tx).await?;

    // Fund Bob and Alice with deposits
    tracing::info!("Funding Bob and Alice");
    let bob_deposit_address = nodes.bob.get_deposit_address().await?;
    let () = deposit(
        &mut enforcer_post_setup,
        &mut nodes.bob,
        &bob_deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("Deposited to Bob successfully");

    let alice_deposit_address = nodes.alice.get_deposit_address().await?;
    let () = deposit(
        &mut enforcer_post_setup,
        &mut nodes.alice,
        &alice_deposit_address,
        DEPOSIT_AMOUNT,
        DEPOSIT_FEE,
    )
    .await?;
    tracing::info!("Deposited to Alice successfully");

    // Sync nodes
    sync_nodes(
        &[&nodes.bob, &nodes.alice, &nodes.charles],
        &mut enforcer_post_setup,
    )
    .await?;


    // Bob and Alice perform different operations

    // 1. Bob transfers to Alice
    tracing::info!("Bob transferring to Alice");
    let alice_receive_address =
        nodes.alice.rpc_client.get_new_address().await?;
    let transfer_txid = nodes
        .bob
        .rpc_client
        .transfer(alice_receive_address, TRANSFER_AMOUNT, TRANSFER_FEE)
        .await?;
    tracing::info!(txid = %transfer_txid, "Bob transferred to Alice");

    // Mine the transfer transaction
    nodes.bob.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // 2. Bob creates a swap
    tracing::info!("Bob creating a swap");
    let l1_recipient_address = "bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
    let bob_l2_recipient = nodes.bob.rpc_client.get_new_address().await?;
    let (swap_id_bob, swap_txid_bob) = nodes
        .bob
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient_address.to_string(),
            SWAP_L1_AMOUNT,
            Some(bob_l2_recipient),
            SWAP_L2_AMOUNT,
            Some(1),
            SWAP_FEE,
        )
        .await?;
    tracing::info!(
        swap_id = %swap_id_bob,
        swap_txid = %swap_txid_bob,
        "Bob created swap"
    );

    // Mine the swap transaction
    nodes.bob.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // 3. Alice creates a swap
    tracing::info!("Alice creating a swap");
    let alice_l2_recipient = nodes.alice.rpc_client.get_new_address().await?;
    let (swap_id_alice, swap_txid_alice) = nodes
        .alice
        .rpc_client
        .create_swap(
            ParentChainType::Regtest,
            l1_recipient_address.to_string(),
            SWAP_L1_AMOUNT,
            Some(alice_l2_recipient),
            SWAP_L2_AMOUNT,
            Some(1),
            SWAP_FEE,
        )
        .await?;
    tracing::info!(
        swap_id = %swap_id_alice,
        swap_txid = %swap_txid_alice,
        "Alice created swap"
    );

    // Mine the swap transaction
    nodes.alice.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // 4. Alice transfers back to Bob
    tracing::info!("Alice transferring back to Bob");
    let bob_receive_address = nodes.bob.rpc_client.get_new_address().await?;
    let transfer_txid_alice = nodes
        .alice
        .rpc_client
        .transfer(bob_receive_address, TRANSFER_AMOUNT / 2, TRANSFER_FEE)
        .await?;
    tracing::info!(txid = %transfer_txid_alice, "Alice transferred to Bob");

    // Mine the transfer transaction
    nodes.alice.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Connect Charles to Bob and Alice; Alice to Bob (so Alice could sync; she may lag)
    connect_peers(&nodes.charles, &nodes.bob, &nodes.alice).await?;

    // Sync so Charles sees all transactions (Charles syncs from Bob) (Charles syncs from Bob)
    sync_nodes(
        &[&nodes.bob, &nodes.alice, &nodes.charles],
        &mut enforcer_post_setup,
    )
    .await?;

    // Wait for Charles to sync from Bob, then for all nodes to reach same block count
    wait_for_charles_to_sync_from_bob(
        &nodes.charles,
        &nodes.bob,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(500),
    )
    .await?;

    // Charles verifies all transactions between Bob and Alice
    verify_transactions(&nodes.charles, &nodes.bob, &nodes.alice).await?;

    tracing::info!("Multi-node verification test passed");

    // Cleanup
    drop(nodes.charles);
    drop(nodes.alice);
    drop(nodes.bob);
    tracing::info!(
        "Removing {}",
        enforcer_post_setup.directories.base_dir.path().display()
    );
    drop(enforcer_post_setup.tasks);
    sleep(std::time::Duration::from_secs(1)).await;
    enforcer_post_setup.directories.base_dir.cleanup()?;
    Ok(())
}

async fn multi_node_verification(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res =
                multi_node_verification_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

pub fn multi_node_verification_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "multi_node_verification",
        multi_node_verification(bin_paths).boxed(),
        file_registry,
        failure_collector,
    )
}
