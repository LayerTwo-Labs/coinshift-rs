//! Test swap creation functionality

use bip300301_enforcer_integration_tests::{
    integration_test::{
        activate_sidechain, deposit, fund_enforcer, propose_sidechain,
    },
    setup::{
        Mode, Network, PostSetup as EnforcerPostSetup, PreSetup, SetupOpts,
        Sidechain as _,
    },
    util::{AbortOnDrop, AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use bip300301_enforcer_lib::bins::CommandExt as _;
use coinshift::{
    state::l1_proof::L1PaymentProof,
    types::{Address, ParentChainType, SwapId, SwapState},
};
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
    let pre_setup = PreSetup::new(&bin_paths.others, Network::Regtest)?;
    let setup_opts: SetupOpts = Default::default();
    let mut enforcer_post_setup = pre_setup
        .setup(Mode::Mempool, setup_opts, res_tx.clone())
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
const SWAP_FEE: u64 = 1_000;
/// Enough for the reserver to own an input, which `validate_swap_accept`
/// requires as proof they control the address they are reserving for.
const ACCEPT_FUNDING: u64 = 100_000;
const ACCEPT_FEE: u64 = 1_000; // 0.00001 BTC

/// Run a `bitcoin-cli` command against the regtest node and return stdout.
async fn bitcoin_cli(
    enforcer_post_setup: &EnforcerPostSetup,
    method: &str,
    args: impl IntoIterator<Item = String>,
) -> anyhow::Result<String> {
    Ok(enforcer_post_setup
        .bitcoin_cli
        .command::<String, _, _, _, _>([], method, args)
        .run_utf8()
        .await?
        .trim()
        .to_owned())
}

/// Make sure Bitcoin Core's own wallet has mature coins to pay a swap with.
async fn fund_core_wallet(
    enforcer_post_setup: &EnforcerPostSetup,
) -> anyhow::Result<()> {
    let balance: f64 = bitcoin_cli(enforcer_post_setup, "getbalance", [])
        .await?
        .parse()?;
    if balance >= 1.0 {
        return Ok(());
    }
    let address = bitcoin_cli(enforcer_post_setup, "getnewaddress", []).await?;
    // 101 blocks: one coinbase matures.
    let _hashes = bitcoin_cli(
        enforcer_post_setup,
        "generatetoaddress",
        ["101".to_owned(), address],
    )
    .await?;
    Ok(())
}

/// Pay `l1_amount_sats` to `l1_recipient` on regtest with the swap
/// commitment for `claimer` in an `OP_RETURN` output, confirm it in one
/// block, and return the txid.
async fn pay_swap_on_l1(
    enforcer_post_setup: &EnforcerPostSetup,
    sidechain: &PostSetup,
    swap_id: SwapId,
    claimer: Address,
    l1_recipient: &str,
    l1_amount_sats: u64,
) -> anyhow::Result<String> {
    let commitment = sidechain
        .rpc_client
        .l1_payment_commitment(swap_id, claimer)
        .await?;
    let outputs = serde_json::json!([
        { l1_recipient: bitcoin::Amount::from_sat(l1_amount_sats).to_btc() },
        { "data": commitment },
    ]);
    // `send` funds, signs and broadcasts in one call. An explicit fee rate:
    // a fresh regtest chain has no fee history for the estimator.
    let response = bitcoin_cli(
        enforcer_post_setup,
        "send",
        [
            outputs.to_string(),
            "null".to_owned(),
            "unset".to_owned(),
            "1".to_owned(),
        ],
    )
    .await?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    let txid = response["txid"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("send returned no txid: {response}"))?
        .to_owned();
    let mine_to = bitcoin_cli(enforcer_post_setup, "getnewaddress", []).await?;
    let _hashes = bitcoin_cli(
        enforcer_post_setup,
        "generatetoaddress",
        ["1".to_owned(), mine_to],
    )
    .await?;
    Ok(txid)
}

/// Build the claim's proof the way a taker without a configured parent-chain
/// RPC would: `gettxoutproof` plus `getrawtransaction`, borsh-encoded, hex.
async fn build_l1_proof_hex(
    enforcer_post_setup: &EnforcerPostSetup,
    txid: &str,
) -> anyhow::Result<String> {
    let merkle_block = hex::decode(
        bitcoin_cli(
            enforcer_post_setup,
            "gettxoutproof",
            [serde_json::json!([txid]).to_string()],
        )
        .await?,
    )?;
    let raw_tx = hex::decode(
        bitcoin_cli(
            enforcer_post_setup,
            "getrawtransaction",
            [txid.to_owned()],
        )
        .await?,
    )?;
    Ok(hex::encode(
        L1PaymentProof::new(merkle_block, raw_tx).to_bytes(),
    ))
}

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
        swap.l1_amount.to_sat() == expected_l1_amount,
        "L1 amount mismatch: expected {}, got {:?}",
        expected_l1_amount,
        swap.l1_amount.to_sat()
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
    let l1_recipient_address = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";

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

    let l1_recipient_address = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";

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

    let l1_recipient_address = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";

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

    // Bob reserves the swap on-chain BEFORE paying on L1. This is what entitles
    // him to the escrow: the reservation is block data every node agrees on,
    // unlike the L1 payment. Taking it after paying would let anyone watching
    // front-run him.
    let claimer_address = sidechain.rpc_client.get_new_address().await?;

    // Fund that address first. `validate_swap_accept` requires the reservation
    // to spend an input owned by the address it reserves for — that is what
    // proves control of it, and without the rule anyone could park every open
    // swap for free. A freshly generated address owns nothing, so coin
    // selection has nothing to offer and the accept fails with "not enough
    // funds". Paying Bob before he reserves is also what a real taker does:
    // he has to hold Coinshift coins to be in this trade at all.
    let () = sidechain
        .rpc_client
        .transfer(claimer_address, ACCEPT_FUNDING, ACCEPT_FEE)
        .await
        .map(|_| ())?;
    sidechain.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    let accept_txid = sidechain
        .rpc_client
        .accept_swap(swap_id, Some(claimer_address), Some(0))
        .await?;
    tracing::info!(swap_id = %swap_id, %accept_txid, "Reserved open swap");
    sidechain.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    // Now Bob pays on L1, for real: a regtest transaction to the swap's
    // recipient carrying the OP_RETURN commitment to this swap and to Bob's
    // L2 address, confirmed in a block.
    fund_core_wallet(&enforcer_post_setup).await?;
    let l1_txid = pay_swap_on_l1(
        &enforcer_post_setup,
        &sidechain,
        swap_id,
        claimer_address,
        l1_recipient_address,
        SWAP_L1_AMOUNT,
    )
    .await?;
    tracing::info!(swap_id = %swap_id, %l1_txid, "Paid the swap on L1");

    // Confirmations are measured from the mainchain block the sidechain tip
    // was built against, so a sidechain block has to be produced on top of
    // the payment's block before the proof is deep enough.
    sidechain.bmm_single(&mut enforcer_post_setup).await?;
    sleep(std::time::Duration::from_millis(500)).await;

    let proof_hex = build_l1_proof_hex(&enforcer_post_setup, &l1_txid).await?;

    // Front-running: someone else takes the (public) proof and claims to
    // their own address. The payment commits to Bob, so this is refused.
    let interloper = sidechain.rpc_client.get_new_address().await?;
    let stolen = sidechain
        .rpc_client
        .claim_swap(swap_id, Some(interloper), Some(proof_hex.clone()))
        .await;
    anyhow::ensure!(
        stolen.is_err(),
        "claiming to an address the L1 payment did not commit to must fail"
    );

    // A claim with no proof at all is refused too, whatever the local view.
    let unproven = sidechain.rpc_client.claim_swap(swap_id, None, None).await;
    anyhow::ensure!(
        unproven.is_err(),
        "claiming without a proof and without a parent-chain RPC must fail"
    );

    // Bob claims with the proof; the escrow goes to the committed address.
    // Retry briefly: the node learns of the payment's mainchain block through
    // the enforcer, which may lag the BMM by a moment.
    let mut claim_txid = None;
    let mut last_err = None;
    for _ in 0..30 {
        match sidechain
            .rpc_client
            .claim_swap(swap_id, None, Some(proof_hex.clone()))
            .await
        {
            Ok(txid) => {
                claim_txid = Some(txid);
                break;
            }
            Err(err) => {
                last_err = Some(err);
                sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
    let claim_txid = claim_txid.ok_or_else(|| {
        anyhow::anyhow!(
            "claim with a valid proof never succeeded: {last_err:?}"
        )
    })?;
    tracing::info!(swap_id = %swap_id, %claim_txid, "Claimed swap with L1 proof");

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

    anyhow::ensure!(
        completed_swap.l2_claimer_address == Some(claimer_address),
        "the committed claimer must be recorded on the swap: {:?}",
        completed_swap.l2_claimer_address
    );
    anyhow::ensure!(
        completed_swap.l1_txid.to_hex_rpc() == l1_txid,
        "the proven L1 txid must be recorded on the swap"
    );

    // Final report
    tracing::info!(
        swap_id = %swap_id,
        swap_create_txid = %swap_txid,
        l1_txid = %l1_txid,
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
