use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

use bip300301_enforcer_integration_tests::{
    integration_test::{activate_sidechain, fund_enforcer, propose_sidechain},
    setup::{Mode, Network, PostSetup as EnforcerPostSetup, Sidechain, setup as setup_enforcer},
    util::AbortOnDrop,
};
use bip300301_enforcer_lib::{
    bins::CommandExt,
    types::SidechainNumber,
};
use futures::{TryFutureExt as _, channel::mpsc, future};
use reserve_port::ReservedPort;
use thiserror::Error;
use coinshift::types::{OutputContent, PointedOutput};
use coinshift_app_rpc_api::RpcClient as _;
use tokio::time::sleep;

use crate::util::{BinPaths, CoinshiftApp};

/// Verify that all required binary paths exist
fn verify_bin_paths(bin_paths: &BinPaths) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    tracing::info!("Verifying all binary paths exist (current dir: {})", current_dir);
    
    // Check coinshift
    let coinshift_path = if bin_paths.coinshift.is_relative() {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.join(&bin_paths.coinshift).canonicalize().ok())
            .unwrap_or_else(|| bin_paths.coinshift.clone())
    } else {
        bin_paths.coinshift.clone()
    };
    tracing::info!("  Checking coinshift: {:?} (resolved: {:?})", bin_paths.coinshift, coinshift_path);
    let coinshift_exists = coinshift_path.exists();
    if !coinshift_exists {
        anyhow::bail!("Coinshift binary does not exist at: {:?} (resolved from: {:?}, current dir: {})", 
            coinshift_path, bin_paths.coinshift, current_dir);
    }
    
    // Check enforcer binaries
    tracing::info!("  Checking bitcoind: {:?}", bin_paths.others.bitcoind);
    if !bin_paths.others.bitcoind.exists() {
        anyhow::bail!("Bitcoind binary does not exist at: {:?} (current dir: {})", 
            bin_paths.others.bitcoind, current_dir);
    }
    
    tracing::info!("  Checking bitcoin_cli: {:?}", bin_paths.others.bitcoin_cli);
    if !bin_paths.others.bitcoin_cli.exists() {
        anyhow::bail!("Bitcoin-cli binary does not exist at: {:?} (current dir: {})", 
            bin_paths.others.bitcoin_cli, current_dir);
    }
    
    tracing::info!("  Checking bitcoin_util: {:?}", bin_paths.others.bitcoin_util);
    if !bin_paths.others.bitcoin_util.exists() {
        anyhow::bail!("Bitcoin-util binary does not exist at: {:?} (current dir: {})", 
            bin_paths.others.bitcoin_util, current_dir);
    }
    
    tracing::info!("  Checking bip300301_enforcer: {:?}", bin_paths.others.bip300301_enforcer);
    if !bin_paths.others.bip300301_enforcer.exists() {
        anyhow::bail!("Bip300301_enforcer binary does not exist at: {:?} (current dir: {})", 
            bin_paths.others.bip300301_enforcer, current_dir);
    }
    
    tracing::info!("  Checking electrs: {:?}", bin_paths.others.electrs);
    if !bin_paths.others.electrs.exists() {
        anyhow::bail!("Electrs binary does not exist at: {:?} (current dir: {})", 
            bin_paths.others.electrs, current_dir);
    }
    
    tracing::info!("  Checking signet_miner: {:?}", bin_paths.others.signet_miner);
    if !bin_paths.others.signet_miner.exists() {
        anyhow::bail!("Signet miner script does not exist at: {:?} (current dir: {})", 
            bin_paths.others.signet_miner, current_dir);
    }
    
    tracing::info!("✓ All binary paths verified");
    Ok(())
}

#[derive(Debug)]
pub struct ReservedPorts {
    pub net: ReservedPort,
    pub rpc: ReservedPort,
}

impl ReservedPorts {
    pub fn new() -> Result<Self, reserve_port::Error> {
        Ok(Self {
            net: ReservedPort::random()?,
            rpc: ReservedPort::random()?,
        })
    }
}

#[derive(Debug)]
pub struct Init {
    pub coinshift_app: PathBuf,
    pub data_dir_suffix: Option<String>,
}

#[derive(Debug, Error)]
pub enum BmmError {
    #[error(transparent)]
    Mine(#[from] bip300301_enforcer_integration_tests::mine::MineError),
    #[error(transparent)]
    RpcClient(#[from] jsonrpsee::core::ClientError),
}

#[derive(Debug, Error)]
pub enum SetupError {
    #[error("Failed to create coinshift dir")]
    CreateCoinshiftDir(#[source] std::io::Error),
    #[error(transparent)]
    ReservePort(#[from] reserve_port::Error),
    #[error(transparent)]
    RpcClient(#[from] jsonrpsee::core::ClientError),
}

#[derive(Debug, Error)]
pub enum ConfirmDepositError {
    #[error(transparent)]
    Bmm(#[from] BmmError),
    #[error("Deposit not found with txid: `{txid}`")]
    DepositNotFound { txid: bitcoin::Txid },
    #[error(transparent)]
    RpcClient(#[from] jsonrpsee::core::ClientError),
}

#[derive(Debug, Error)]
pub enum CreateWithdrawalError {
    #[error(transparent)]
    Bmm(#[from] BmmError),
    #[error("Pending withdrawal bundle not found")]
    PendingWithdrawalBundleNotFound,
    #[error(transparent)]
    RpcClient(#[from] jsonrpsee::core::ClientError),
}

#[derive(Debug)]
pub struct PostSetup {
    // MUST occur before temp dirs and reserved ports in order to ensure that processes are dropped
    // before reserved ports are freed and temp dirs are cleared
    pub _coinshift_app_task: AbortOnDrop<()>,
    /// RPC client for coinshift_app
    pub rpc_client: jsonrpsee::http_client::HttpClient,
    /// Address for receiving deposits
    pub deposit_address: coinshift::types::Address,
    // MUST occur after tasks in order to ensure that tasks are dropped
    // before reserved ports are freed
    pub reserved_ports: ReservedPorts,
}

impl PostSetup {
    /// BMM a block
    pub async fn bmm_single(
        &self,
        post_setup: &mut EnforcerPostSetup,
    ) -> Result<(), BmmError> {
        use bip300301_enforcer_integration_tests::mine::mine;
        let ((), ()) = future::try_join(
            self.rpc_client.mine(None).map_err(BmmError::from),
            async {
                sleep(Duration::from_secs(1)).await;
                mine::<Self>(post_setup, 1, Some(true))
                    .await
                    .map_err(BmmError::from)
            },
        )
        .await?;
        Ok(())
    }

    /// BMM blocks
    pub async fn bmm(
        &self,
        post_setup: &mut EnforcerPostSetup,
        blocks: u32,
    ) -> Result<(), BmmError> {
        for i in 0..blocks {
            tracing::debug!("BMM block {}/{blocks}", i + 1);
            let () = self.bmm_single(post_setup).await?;
        }
        Ok(())
    }

    pub fn net_port(&self) -> u16 {
        self.reserved_ports.net.port()
    }

    pub fn net_addr(&self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.net_port())
    }
}

impl Sidechain for PostSetup {
    const SIDECHAIN_NUMBER: SidechainNumber =
        SidechainNumber(coinshift::types::THIS_SIDECHAIN);

    type Init = Init;

    type SetupError = SetupError;

    async fn setup(
        init: Self::Init,
        post_setup: &EnforcerPostSetup,
        res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
    ) -> Result<Self, Self::SetupError> {
        let reserved_ports = ReservedPorts::new()?;
        let coinshift_dir = if let Some(suffix) = init.data_dir_suffix {
            post_setup.out_dir.path().join(format!("coinshift-{suffix}"))
        } else {
            post_setup.out_dir.path().join("coinshift")
        };
        std::fs::create_dir(&coinshift_dir)
            .map_err(Self::SetupError::CreateCoinshiftDir)?;
        let coinshift_app = CoinshiftApp {
            path: init.coinshift_app,
            data_dir: coinshift_dir,
            log_level: Some(tracing::Level::TRACE),
            mainchain_grpc_port: post_setup
                .reserved_ports
                .enforcer_serve_grpc
                .port(),
            net_port: reserved_ports.net.port(),
            rpc_port: reserved_ports.rpc.port(),
        };
        let coinshift_app_task = coinshift_app
            .spawn_command_with_args::<String, String, _, _, _>([], [], {
                let res_tx = res_tx.clone();
                move |err| {
                    let _err: Result<(), _> = res_tx.unbounded_send(Err(err));
                }
            });
        tracing::debug!("Started coinshift");
        sleep(Duration::from_secs(1)).await;
        let rpc_client = jsonrpsee::http_client::HttpClient::builder()
            .build(format!("http://127.0.0.1:{}", reserved_ports.rpc.port()))?;
        tracing::debug!("Generating mnemonic seed phrase");
        let mnemonic = rpc_client.generate_mnemonic().await?;
        tracing::debug!("Setting mnemonic seed phrase");
        let () = rpc_client.set_seed_from_mnemonic(mnemonic).await?;
        tracing::debug!("Generating deposit address");
        let deposit_address = rpc_client.get_new_address().await?;
        Ok(Self {
            _coinshift_app_task: coinshift_app_task,
            rpc_client,
            deposit_address,
            reserved_ports,
        })
    }

    type GetDepositAddressError = std::convert::Infallible;

    async fn get_deposit_address(
        &self,
    ) -> Result<String, Self::GetDepositAddressError> {
        Ok(self.deposit_address.to_string())
    }

    type ConfirmDepositError = ConfirmDepositError;

    async fn confirm_deposit(
        &mut self,
        post_setup: &mut EnforcerPostSetup,
        address: &str,
        value: bitcoin::Amount,
        txid: bitcoin::Txid,
    ) -> Result<(), Self::ConfirmDepositError> {
        let is_expected = |utxo: &PointedOutput| {
            utxo.output.address.to_string() == address
                && match utxo.output.content {
                    OutputContent::Value(utxo_value) => utxo_value == value,
                    OutputContent::Withdrawal { .. } => false,
                    OutputContent::SwapPending { .. } => false,
                }
                && match utxo.outpoint {
                    coinshift::types::OutPoint::Deposit(outpoint) => {
                        outpoint.txid == txid
                    }
                    _ => false,
                }
        };
        let utxos = self.rpc_client.list_utxos().await?;
        if utxos.iter().any(is_expected) {
            return Ok(());
        }
        tracing::debug!("Deposit not found, BMM 1 block...");
        let () = self.bmm_single(post_setup).await?;
        let utxos = self.rpc_client.list_utxos().await?;
        if utxos.iter().any(is_expected) {
            Ok(())
        } else {
            Err(Self::ConfirmDepositError::DepositNotFound { txid })
        }
    }

    type CreateWithdrawalError = CreateWithdrawalError;

    async fn create_withdrawal(
        &mut self,
        post_setup: &mut EnforcerPostSetup,
        receive_address: &bitcoin::Address,
        value: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> Result<bip300301_enforcer_lib::types::M6id, Self::CreateWithdrawalError>
    {
        let _txid = self
            .rpc_client
            .withdraw(
                receive_address.as_unchecked().clone(),
                value.to_sat(),
                0,
                fee.to_sat(),
            )
            .await?;
        let blocks_to_mine = 'blocks_to_mine: {
            use coinshift::state::WITHDRAWAL_BUNDLE_FAILURE_GAP;
            let block_count = self.rpc_client.getblockcount().await?;
            let Some(block_height) = block_count.checked_sub(1) else {
                break 'blocks_to_mine WITHDRAWAL_BUNDLE_FAILURE_GAP;
            };
            let latest_failed_withdrawal_bundle_height = self
                .rpc_client
                .latest_failed_withdrawal_bundle_height()
                .await?
                .unwrap_or(0);
            match WITHDRAWAL_BUNDLE_FAILURE_GAP.saturating_sub(
                block_height - latest_failed_withdrawal_bundle_height,
            ) {
                0 => WITHDRAWAL_BUNDLE_FAILURE_GAP + 1,
                blocks_to_mine => blocks_to_mine,
            }
        };
        tracing::debug!(
            "Mining coinshift blocks until withdrawal bundle is broadcast"
        );
        let () = self.bmm(post_setup, blocks_to_mine).await?;
        let pending_withdrawal_bundle =
            self.rpc_client.pending_withdrawal_bundle().await?.ok_or(
                Self::CreateWithdrawalError::PendingWithdrawalBundleNotFound,
            )?;
        let m6id = pending_withdrawal_bundle.compute_m6id();
        Ok(bip300301_enforcer_lib::types::M6id(m6id.0))
    }
}

/// Complete setup result containing both enforcer and coinshift post-setup
// #[derive(Debug)]
pub struct CompleteSetup {
    pub enforcer: EnforcerPostSetup,
    pub coinshift: PostSetup,
}

/// Mine a single signet block
async fn mine_single_signet(
    signet_miner: &bip300301_enforcer_lib::bins::SignetMiner,
    mining_address: &bitcoin::Address,
) -> anyhow::Result<()> {
    let _mine_output = signet_miner
        .command(
            "generate",
            vec![
                "--address",
                &mining_address.to_string(),
                "--block-interval",
                "1",
            ],
        )
        .run_utf8()
        .await?;
    Ok(())
}

/// Setup signet network with 2 blocks mined
pub async fn setup_signet(
    bin_paths: &BinPaths,
    coinshift_init: Init,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<CompleteSetup> {
    tracing::info!("Setting up signet network");
    
    // Verify all binary paths exist
    verify_bin_paths(bin_paths)?;
    
    // Setup enforcer with signet
    // Note: Signet requires Mode::GetBlockTemplate because GenerateBlocks is not supported on Signet
    tracing::info!("Calling setup_enforcer for signet network with GetBlockTemplate mode");
    let mut enforcer_post_setup = setup_enforcer(
        &bin_paths.others,
        Network::Signet,
        Mode::GetBlockTemplate,
        res_tx.clone(),
    )
    .await
    .map_err(|e| {
        tracing::error!("setup_enforcer failed: {:#}", e);
        anyhow::anyhow!("setup_enforcer failed: {:#}", e)
    })?;
    tracing::info!("setup_enforcer completed successfully");
    
    tracing::info!("Enforcer setup complete, mining additional signet block to reach 2 blocks total");
    
    // First block is already mined during setup, so we need 1 more to reach 2 blocks
    let () = mine_single_signet(
        &enforcer_post_setup.signet_miner,
        &enforcer_post_setup.mining_address,
    )
    .await?;
    
    // Verify we have at least 2 blocks
    let block_count: u32 = enforcer_post_setup
        .bitcoin_cli
        .command::<String, _, String, _, _>([], "getblockcount", [])
        .run_utf8()
        .await?
        .parse()?;
    anyhow::ensure!(block_count >= 2, "Expected at least 2 blocks, got {block_count}");
    tracing::info!("Successfully mined 2 signet blocks (block count: {block_count})");
    
    // Setup coinshift sidechain
    tracing::info!("Setting up coinshift sidechain");
    let () = propose_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Proposed sidechain successfully");
    let () = activate_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Activated sidechain successfully");
    let () = fund_enforcer::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Funded enforcer successfully");
    
    let coinshift_post_setup = PostSetup::setup(
        coinshift_init,
        &enforcer_post_setup,
        res_tx,
    )
    .await?;
    tracing::info!("Coinshift setup complete");
    
    Ok(CompleteSetup {
        enforcer: enforcer_post_setup,
        coinshift: coinshift_post_setup,
    })
}

/// Setup regtest network
pub async fn setup_regtest(
    bin_paths: &BinPaths,
    coinshift_init: Init,
    mode: Mode,
    res_tx: mpsc::UnboundedSender<anyhow::Result<()>>,
) -> anyhow::Result<CompleteSetup> {
    tracing::info!("Setting up regtest network");
    
    // Verify all binary paths exist
    verify_bin_paths(bin_paths)?;
    
    // Setup enforcer with regtest
    tracing::info!("Calling setup_enforcer for regtest network");
    let mut enforcer_post_setup = setup_enforcer(
        &bin_paths.others,
        Network::Regtest,
        mode,
        res_tx.clone(),
    )
    .await
    .map_err(|e| {
        tracing::error!("setup_enforcer failed: {:#}", e);
        anyhow::anyhow!("setup_enforcer failed: {:#}", e)
    })?;
    tracing::info!("setup_enforcer completed successfully");
    
    tracing::info!("Enforcer setup complete");
    
    // Setup coinshift sidechain
    tracing::info!("Setting up coinshift sidechain");
    let () = propose_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Proposed sidechain successfully");
    let () = activate_sidechain::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Activated sidechain successfully");
    let () = fund_enforcer::<PostSetup>(&mut enforcer_post_setup).await?;
    tracing::info!("Funded enforcer successfully");
    
    let coinshift_post_setup = PostSetup::setup(
        coinshift_init,
        &enforcer_post_setup,
        res_tx,
    )
    .await?;
    tracing::info!("Coinshift setup complete");
    
    Ok(CompleteSetup {
        enforcer: enforcer_post_setup,
        coinshift: coinshift_post_setup,
    })
}
