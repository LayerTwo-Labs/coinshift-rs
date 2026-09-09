//! Test that a node indexes the transactions and deposits of a block

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
use bitcoin::Amount;
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

const DEPOSIT_AMOUNT: Amount = Amount::from_sat(21_000_000);
const DEPOSIT_FEE: Amount = Amount::from_sat(1_000_000);
const TRANSFER_AMOUNT: u64 = 1_000_000;
const TRANSFER_FEE: u64 = 1_000;

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
    let () = activate_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    let () = fund_enforcer::<PostSetup>(&mut enforcer_post_setup).await?;
    Ok(enforcer_post_setup)
}

async fn block_index_task(
    bin_paths: BinPaths,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let mut enforcer_post_setup = setup(&bin_paths, res_tx.clone()).await?;
    let mut sidechain = PostSetup::setup(
        Init {
            coinshift_app: bin_paths.coinshift_app.clone(),
            data_dir_suffix: None,
        },
        &enforcer_post_setup,
        res_tx,
    )
    .await?;
    tracing::info!("Setup Coinshift node successfully");

    tracing::debug!("Checking that no height names a block yet");
    anyhow::ensure!(sidechain.rpc_client.get_block_hash(0).await?.is_none());

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

    tracing::debug!("Checking that a height names the deposit block");
    let height = sidechain.rpc_client.getblockcount().await?;
    anyhow::ensure!(height > 0);
    let deposit_block_hash = sidechain
        .rpc_client
        .get_block_hash(height - 1)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no block at height {}", height - 1))?;

    tracing::debug!("Checking that the deposit block names its deposit");
    let deposit_index = sidechain
        .rpc_client
        .get_block_index(deposit_block_hash)
        .await?;
    anyhow::ensure!(deposit_index.txs.is_empty());
    anyhow::ensure!(deposit_index.bundle_spends.is_empty());
    anyhow::ensure!(deposit_index.deposits.len() == 1);
    anyhow::ensure!(
        deposit_index.deposits[0].output.address.to_string()
            == sidechain.deposit_address.to_string()
    );

    let dest = sidechain.rpc_client.get_new_address().await?;
    let txid = sidechain
        .rpc_client
        .transfer(dest, TRANSFER_AMOUNT, TRANSFER_FEE)
        .await?;
    tracing::info!(%txid, "Created a transfer");
    let mempool = sidechain.rpc_client.list_mempool().await?;
    anyhow::ensure!(mempool.len() == 1);
    let mempool_entry = mempool[0].clone();

    tracing::debug!("BMM 1 block");
    let () = sidechain.bmm_single(&mut enforcer_post_setup).await?;
    let height = sidechain.rpc_client.getblockcount().await?;
    let block_hash = sidechain
        .rpc_client
        .get_block_hash(height - 1)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no block at height {}", height - 1))?;

    tracing::debug!("Checking that the block names each transaction");
    let block_index = sidechain.rpc_client.get_block_index(block_hash).await?;
    anyhow::ensure!(block_index.deposits.is_empty());
    anyhow::ensure!(block_index.bundle_spends.is_empty());
    anyhow::ensure!(block_index.txs.len() == 1);
    let indexed = &block_index.txs[0];
    anyhow::ensure!(indexed.txid == txid);
    anyhow::ensure!(indexed.size == mempool_entry.size);
    anyhow::ensure!(indexed.raw == mempool_entry.raw);

    tracing::debug!("Checking that the block body carries the same order");
    let block = sidechain
        .rpc_client
        .get_block(block_hash)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no block {block_hash}"))?;
    let body_txids: Vec<_> =
        block.body.transactions.iter().map(|tx| tx.txid()).collect();
    let index_txids: Vec<_> =
        block_index.txs.iter().map(|tx| tx.txid).collect();
    anyhow::ensure!(body_txids == index_txids);

    // An Esplora index serves this hex, so it must be the borsh encoding that
    // the txid hashes over.
    for (indexed, tx) in block_index.txs.iter().zip(&block.body.transactions) {
        anyhow::ensure!(indexed.raw == hex::encode(tx.canonical_encoding()));
        anyhow::ensure!(indexed.size == tx.canonical_size());
    }

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

async fn block_index(bin_paths: BinPaths) -> anyhow::Result<()> {
    let (res_tx, mut res_rx) = mpsc::unbounded();
    let _test_task: AbortOnDrop<()> = tokio::task::spawn({
        let res_tx = res_tx.clone();
        async move {
            let res = block_index_task(bin_paths, res_tx.clone()).await;
            let _send_err: Result<(), _> = res_tx.unbounded_send(res);
        }
        .in_current_span()
    })
    .into();
    res_rx.next().await.ok_or_else(|| {
        anyhow::anyhow!("Unexpected end of test task result stream")
    })?
}

pub fn block_index_trial(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "block_index",
        block_index(bin_paths).boxed(),
        file_registry,
        failure_collector,
    )
}
