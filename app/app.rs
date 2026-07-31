use std::{collections::HashMap, sync::Arc};

use coinshift::{
    miner::{self, Miner},
    node::{self, Node},
    types::{
        self, Address, FilledTransaction, OutPoint, Output, Transaction,
        proto::mainchain::{
            self,
            generated::{validator_service_server, wallet_service_server},
        },
    },
    wallet::{self, Wallet},
};
use fallible_iterator::FallibleIterator as _;
use futures::{StreamExt, TryFutureExt};
use parking_lot::RwLock;
use rustreexo::accumulator::proof::Proof;
use tokio::{spawn, sync::RwLock as TokioRwLock, task::JoinHandle};
use tokio_util::task::LocalPoolHandle;
use tonic_health::{
    ServingStatus,
    pb::{HealthCheckRequest, health_client::HealthClient},
};

use crate::{cli::Config, mainchain::MainchainMonitor};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("CUSF mainchain proto error")]
    CusfMainchain(#[from] coinshift::types::proto::Error),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("miner error")]
    Miner(#[from] miner::Error),
    #[error(transparent)]
    ModifyMemForest(#[from] coinshift::types::ModifyMemForestError),
    #[error("node error")]
    Node(#[source] Box<node::Error>),
    #[error(
        "Mainchain unreachable; mining requires the parentchain (mainchain) node to be up"
    )]
    MainchainUnreachable(#[source] Box<coinshift::types::proto::Error>),
    #[error("No CUSF mainchain wallet client")]
    NoCusfMainchainWalletClient,
    #[error("Failed to request mainchain ancestor info for {block_hash}")]
    RequestMainchainAncestorInfos { block_hash: bitcoin::BlockHash },
    #[error("Unable to verify existence of CUSF mainchain service(s) at {url}")]
    VerifyMainchainServices {
        url: Box<url::Url>,
        source: Box<tonic::Status>,
    },
    #[error("wallet error")]
    Wallet(#[from] wallet::Error),
    #[error(
        "--strict-l1-config was given, but these parent chains are not usable: {0}"
    )]
    StrictL1Config(String),
}

impl From<node::Error> for Error {
    fn from(err: node::Error) -> Self {
        Self::Node(Box::new(err))
    }
}

fn recover_wallet_addresses(node: &Node, wallet: &Wallet) -> Result<(), Error> {
    if !wallet.has_seed()? {
        return Ok(());
    }
    let all_utxos = node.get_all_utxos()?;
    if all_utxos.is_empty() {
        return Ok(());
    }
    let utxo_addresses: std::collections::HashSet<_> =
        all_utxos.values().map(|o| o.address).collect();
    let recovered = wallet.recover_addresses_from_utxo_set(&utxo_addresses)?;
    if recovered > 0 {
        tracing::info!(
            recovered,
            "Recovered wallet addresses from node UTXO set"
        );
    }
    Ok(())
}

fn update_wallet(node: &Node, wallet: &Wallet) -> Result<(), Error> {
    tracing::trace!("starting wallet update");
    let addresses = wallet.get_addresses()?;
    let utxos = node.get_utxos_by_addresses(&addresses)?;
    let outpoints: Vec<_> = wallet.get_utxos()?.into_keys().collect();
    let spent: Vec<_> = node
        .get_spent_utxos(&outpoints)?
        .into_iter()
        .map(|(outpoint, spent_output)| (outpoint, spent_output.inpoint))
        .collect();
    wallet.put_utxos(&utxos)?;
    wallet.spend_utxos(&spent)?;

    tracing::debug!("finished wallet update");
    Ok(())
}

/// Update utxos & wallet
fn update(
    node: &Node,
    utxos: &mut HashMap<OutPoint, Output>,
    wallet: &Wallet,
) -> Result<(), Error> {
    tracing::trace!("Updating wallet");
    let () = update_wallet(node, wallet)?;
    *utxos = wallet.get_utxos()?;
    tracing::trace!("Updated wallet");
    Ok(())
}

#[derive(Clone)]
pub struct App {
    pub node: Arc<Node>,
    pub wallet: Wallet,
    /// The miner, installable after startup: the enforcer may come up
    /// without its wallet service and gain it later.
    pub miner: Arc<TokioRwLock<Option<Miner>>>,
    /// Separate wallet client for deposits, so deposits don't block on the
    /// miner write lock (which is held for the entire BMM confirmation).
    cusf_mainchain_wallet:
        Option<mainchain::WalletClient<tonic::transport::Channel>>,
    pub utxos: Arc<RwLock<HashMap<OutPoint, Output>>>,
    task: Arc<JoinHandle<()>>,
    pub transaction: Arc<RwLock<Transaction>>,
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub local_pool: LocalPoolHandle,
    /// Reachability of the mainchain enforcer, for reporting and reconnection.
    /// Deliberately not consulted before mining -- see `crate::mainchain`.
    pub mainchain: MainchainMonitor,
}

impl App {
    async fn task(
        node: Arc<Node>,
        utxos: Arc<RwLock<HashMap<OutPoint, Output>>>,
        wallet: Wallet,
    ) -> Result<(), Error> {
        let mut state_changes = node.watch_state();
        // Track whether we've successfully recovered addresses.
        // If the chain was empty at startup, we need to recover
        // once blocks start arriving.
        let mut needs_recovery = wallet.get_addresses()?.is_empty()
            && wallet.has_seed().unwrap_or(false);
        while let Some(()) = state_changes.next().await {
            if needs_recovery {
                match recover_wallet_addresses(&node, &wallet) {
                    Ok(()) => {
                        needs_recovery = wallet
                            .get_addresses()
                            .map_or(true, |a| a.is_empty());
                    }
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        tracing::error!(
                            "Failed to recover wallet addresses: {err:#}"
                        );
                    }
                }
            }
            let update_result = update(&node, &mut utxos.write(), &wallet);
            if let Err(err) = update_result {
                let err = anyhow::Error::from(err);
                tracing::error!("Failed to update wallet: {err:#}");
            }
        }
        Ok(())
    }

    fn spawn_task(
        node: Arc<Node>,
        utxos: Arc<RwLock<HashMap<OutPoint, Output>>>,
        wallet: Wallet,
    ) -> JoinHandle<()> {
        spawn(Self::task(node, utxos, wallet).unwrap_or_else(|err| {
            let err = anyhow::Error::from(err);
            tracing::error!("{err:#}")
        }))
    }

    /// Periodic task to sync L1 blocks for deposit scanning, and to keep the
    /// mainchain connection state current.
    ///
    /// This already polled the enforcer every ten seconds, so reconnection
    /// rides along with it rather than needing a task of its own. When the
    /// enforcer is absent it also re-probes for the wallet service, which is
    /// the only way a `Miner` can appear after startup -- previously that was
    /// fixed forever at construction.
    async fn l1_sync_task(
        node: Arc<Node>,
        mainchain: MainchainMonitor,
        miner: Arc<TokioRwLock<Option<Miner>>>,
        transport: tonic::transport::Channel,
    ) -> Result<(), Error> {
        use futures::FutureExt;
        use std::time::Duration;
        const SYNC_INTERVAL: Duration = Duration::from_secs(10);

        tracing::info!(
            "L1 sync task started, will check every {} seconds",
            SYNC_INTERVAL.as_secs()
        );

        loop {
            tokio::time::sleep(SYNC_INTERVAL).await;
            tracing::trace!("L1 sync task: checking for new L1 blocks");

            // Get current L1 chain tip (mainchain must be up for mining and block sync)
            let l1_tip_hash = match node
                .with_cusf_mainchain(|client| {
                    client
                        .get_chain_tip()
                        .map(|res| {
                            res.map(|tip| tip.block_hash)
                                .map_err(Error::CusfMainchain)
                        })
                        .boxed()
                })
                .await
            {
                Ok(hash) => {
                    mainchain.record_connected();
                    // A late-arriving wallet service means we can finally mine.
                    if miner.read().await.is_none() {
                        Self::try_install_miner(&mainchain, &miner, &transport)
                            .await;
                    }
                    tracing::trace!(l1_tip = %hash, "L1 sync task: got L1 chain tip");
                    hash
                }
                Err(err) => {
                    let wait = mainchain.record_failure(err.to_string());
                    tracing::debug!(
                        error = %err,
                        retry_in = ?wait,
                        "L1 sync task: failed to get L1 chain tip"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
            };

            // Get current sidechain tip's mainchain verification (latest synced L1 block)
            let synced_main_hash = {
                let rotxn = node.env().read_txn().map_err(node::Error::from)?;
                if let Some(sidechain_tip) = node.try_get_best_hash()? {
                    let result = node
                        .archive()
                        .try_get_best_main_verification(&rotxn, sidechain_tip)
                        .map_err(node::Error::from)?;
                    tracing::trace!(
                        sidechain_tip = %sidechain_tip,
                        synced_main = ?result,
                        "L1 sync task: got synced main hash"
                    );
                    result
                } else {
                    tracing::trace!("L1 sync task: no sidechain tip found");
                    None
                }
            };

            // Check if we need to sync more L1 blocks
            // If we don't have a synced main hash yet, or if the L1 tip is ahead, sync
            let needs_sync = match synced_main_hash {
                Some(synced) => {
                    let needs = l1_tip_hash != synced;
                    tracing::trace!(
                        l1_tip = %l1_tip_hash,
                        synced_main = %synced,
                        needs_sync = %needs,
                        "L1 sync task: comparing tips"
                    );
                    needs
                }
                None => {
                    tracing::trace!(
                        "L1 sync task: no synced main hash, need to sync"
                    );
                    true // No synced main hash yet, need to sync
                }
            };

            if needs_sync {
                // Check if we already have the L1 tip in our archive
                let has_l1_tip = {
                    let rotxn =
                        node.env().read_txn().map_err(node::Error::from)?;
                    let result = node
                        .archive()
                        .try_get_main_header_info(&rotxn, &l1_tip_hash)
                        .map_err(node::Error::from)?
                        .is_some();
                    tracing::trace!(
                        l1_tip = %l1_tip_hash,
                        has_l1_tip = %result,
                        "L1 sync task: checked if L1 tip is in archive"
                    );
                    result
                };

                if !has_l1_tip {
                    tracing::info!(
                        l1_tip = %l1_tip_hash,
                        synced_main = ?synced_main_hash,
                        "L1 sync task: Syncing L1 blocks for deposit scanning"
                    );
                    // Request missing ancestor infos - this will trigger deposit scanning
                    // when 2WPD is processed
                    let start_time = std::time::Instant::now();
                    match node
                        .request_mainchain_ancestor_infos(l1_tip_hash)
                        .await
                    {
                        Ok(true) => {
                            let elapsed = start_time.elapsed();
                            tracing::info!(
                                l1_tip = %l1_tip_hash,
                                elapsed_secs = elapsed.as_secs_f64(),
                                "L1 sync task: Successfully requested L1 ancestor infos"
                            );
                        }
                        Ok(false) => {
                            let elapsed = start_time.elapsed();
                            tracing::warn!(
                                l1_tip = %l1_tip_hash,
                                elapsed_secs = elapsed.as_secs_f64(),
                                "L1 sync task: L1 ancestor infos request returned false (block not available)"
                            );
                        }
                        Err(err) => {
                            let elapsed = start_time.elapsed();
                            tracing::debug!(
                                error = %err,
                                l1_tip = %l1_tip_hash,
                                elapsed_secs = elapsed.as_secs_f64(),
                                "L1 sync task: Failed to request L1 ancestor infos (this is normal if mainchain is not available)"
                            );
                        }
                    }
                } else {
                    tracing::trace!(
                        l1_tip = %l1_tip_hash,
                        "L1 sync task: L1 tip already in archive, no sync needed"
                    );
                }
            } else {
                tracing::trace!(
                    "L1 sync task: L1 is up to date, no sync needed"
                );
            }
        }
    }

    /// Re-probe the enforcer and install a `Miner` if its wallet service is now
    /// available.
    async fn try_install_miner(
        mainchain: &MainchainMonitor,
        miner: &Arc<TokioRwLock<Option<Miner>>>,
        transport: &tonic::transport::Channel,
    ) {
        let has_wallet = match Self::check_proto_support(transport.clone())
            .await
        {
            Ok(has_wallet) => has_wallet,
            Err(err) => {
                tracing::debug!(error = %err, "Enforcer service check failed");
                return;
            }
        };
        mainchain.set_wallet_service(has_wallet);
        if !has_wallet {
            return;
        }
        let validator = mainchain::ValidatorClient::new(transport.clone());
        let wallet = mainchain::WalletClient::new(transport.clone());
        match Miner::new(validator, wallet) {
            Ok(new_miner) => {
                *miner.write().await = Some(new_miner);
                tracing::info!(
                    "Enforcer wallet service is available; mining and deposits \
                     are now enabled"
                );
            }
            Err(err) => {
                tracing::warn!(error = %err, "Failed to construct miner")
            }
        }
    }

    fn spawn_l1_sync_task(
        node: Arc<Node>,
        mainchain: MainchainMonitor,
        miner: Arc<TokioRwLock<Option<Miner>>>,
        transport: tonic::transport::Channel,
    ) -> JoinHandle<()> {
        spawn(
            Self::l1_sync_task(node, mainchain, miner, transport)
                .unwrap_or_else(|err| {
                    let err = anyhow::Error::from(err);
                    tracing::error!("L1 sync task error: {err:#}")
                }),
        )
    }

    async fn check_status_serving(
        client: &mut HealthClient<tonic::transport::Channel>,
        service_name: &str,
    ) -> Result<bool, tonic::Status> {
        match client
            .check(HealthCheckRequest {
                service: service_name.to_string(),
            })
            .await
        {
            Ok(res) => {
                let expected_status = ServingStatus::Serving;
                let status = res.into_inner().status;

                let as_expected = status == expected_status as i32;
                if !as_expected {
                    tracing::warn!(
                        "Expected status {} for {}, got {}",
                        expected_status,
                        service_name,
                        status
                    );
                }
                Ok(as_expected)
            }
            Err(status) if status.code() == tonic::Code::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Returns `true` if validator service AND wallet service are available,
    /// `false` if only validator service is available, and error if validator
    /// service is unavailable.
    async fn check_proto_support(
        transport: tonic::transport::channel::Channel,
    ) -> Result<bool, tonic::Status> {
        let mut client = HealthClient::new(transport);

        let validator_service_name = validator_service_server::SERVICE_NAME;
        let wallet_service_name = wallet_service_server::SERVICE_NAME;

        // The validator service MUST exist. We therefore error out here directly.
        if !Self::check_status_serving(&mut client, validator_service_name)
            .await?
        {
            return Err(tonic::Status::aborted(format!(
                "{validator_service_name} is not supported in mainchain client",
            )));
        }

        tracing::info!("Verified existence of {}", validator_service_name);

        // The wallet service is optional.
        let has_wallet_service =
            Self::check_status_serving(&mut client, wallet_service_name)
                .await?;

        tracing::info!(
            "Checked existence of {}: {}",
            wallet_service_name,
            has_wallet_service
        );
        Ok(has_wallet_service)
    }

    pub fn new(config: &Config) -> Result<Self, Error> {
        // Node launches some tokio tasks for p2p networking, that is why we need a tokio runtime
        // here.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        tracing::info!(
            "Instantiating wallet with data directory: {}",
            config.datadir.display()
        );

        // Startup deliberately does no L1 network I/O. A configured chain whose
        // node happens to be down used to abort startup here -- while a chain
        // that was not configured at all was fine -- so an unrelated service
        // being unavailable could lock an operator out of their own node. The
        // registry now probes in the background and gates *use* instead; see
        // `--strict-l1-config` to opt back into failing fast.
        let l1_rpc_config_path = coinshift::l1::config::default_path();

        let wallet = Wallet::new(&config.datadir.join("wallet.mdb"))?;
        if let Some(seed_phrase_path) = &config.mnemonic_seed_phrase_path {
            let mnemonic = std::fs::read_to_string(seed_phrase_path)?;
            let () = wallet.set_seed_from_mnemonic(mnemonic.as_str())?;
        }

        tracing::info!(
            "Connecting to mainchain at {}",
            config.mainchain_grpc_url
        );
        let rt_guard = runtime.enter();
        let transport = tonic::transport::channel::Channel::from_shared(
            format!("{}", config.mainchain_grpc_url),
        )
        .unwrap()
        .concurrency_limit(256)
        .connect_lazy();
        // Probe the enforcer once, but do not make startup depend on it. The
        // node genuinely cannot sync or mine without the enforcer, yet "cannot
        // mine" is not "cannot run": it can still serve wallet and swap RPC,
        // keep peers, and recover on its own. Requiring it here turned an
        // enforcer that was merely slow to start into a crash loop, with no way
        // to express "start me, wait for my dependency" in a supervisor.
        // `--require-mainchain` restores the old behaviour.
        let probe = runtime.block_on(tokio::time::timeout(
            config.mainchain_connect_timeout,
            Self::check_proto_support(transport.clone()),
        ));
        let wallet_service = match probe {
            Ok(Ok(has_wallet)) => Some(has_wallet),
            Ok(Err(err)) => {
                if config.require_mainchain {
                    return Err(Error::VerifyMainchainServices {
                        url: Box::new(config.mainchain_grpc_url.clone()),
                        source: Box::new(err),
                    });
                }
                tracing::warn!(
                    url = %config.mainchain_grpc_url,
                    error = %err,
                    "Mainchain enforcer did not answer; starting anyway and \
                     retrying in the background. Mining and deposits are \
                     unavailable until it responds."
                );
                None
            }
            Err(_) => {
                let source = tonic::Status::deadline_exceeded(format!(
                    "Connection check timed out after {:?}",
                    config.mainchain_connect_timeout
                ));
                if config.require_mainchain {
                    return Err(Error::VerifyMainchainServices {
                        url: Box::new(config.mainchain_grpc_url.clone()),
                        source: Box::new(source),
                    });
                }
                tracing::warn!(
                    url = %config.mainchain_grpc_url,
                    timeout = ?config.mainchain_connect_timeout,
                    "Mainchain enforcer did not answer in time; starting \
                     anyway and retrying in the background."
                );
                None
            }
        };

        let mainchain_monitor = match wallet_service {
            Some(has_wallet) => MainchainMonitor::connected(
                config.mainchain_grpc_url.clone(),
                has_wallet,
            ),
            None => MainchainMonitor::new(
                config.mainchain_grpc_url.clone(),
                "not reached during startup",
            ),
        };
        let transport_for_reconnect = transport.clone();
        let cusf_mainchain = mainchain::ValidatorClient::new(transport.clone());
        let cusf_mainchain_wallet = wallet_service
            .unwrap_or(false)
            .then(|| mainchain::WalletClient::new(transport));
        let miner = cusf_mainchain_wallet
            .clone()
            .map(|wallet| Miner::new(cusf_mainchain.clone(), wallet))
            .transpose()?;
        let local_pool = LocalPoolHandle::new(1);

        tracing::info!("Instantiating node struct");
        let node_start = std::time::Instant::now();
        let node_config = node::NodeConfig {
            datadir: config.datadir.clone(),
            bind_addr: config.net_addr,
            cusf_mainchain,
            cusf_mainchain_wallet: cusf_mainchain_wallet.clone(),
            network: config.network,
            wallet: Some(Arc::new(wallet.clone())),
            l1_rpc_config_path: Some(l1_rpc_config_path),
        };
        let node = Node::new(node_config, &runtime)?;
        if config.strict_l1_config {
            // Opt-in fail-fast for supervised deployments and CI: probe once
            // now, and refuse to start if any configured chain is unusable.
            runtime.block_on(node.l1().probe_all());
            let unhealthy = node.l1().unhealthy_configured();
            if !unhealthy.is_empty() {
                let detail = unhealthy
                    .iter()
                    .map(|(chain, health)| health.summary(*chain))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(Error::StrictL1Config(detail));
            }
        }
        let node_elapsed = node_start.elapsed();
        tracing::info!(
            elapsed_secs = node_elapsed.as_secs_f64(),
            "Node instantiated successfully"
        );

        tracing::debug!("Initializing UTXOs");
        let utxos_start = std::time::Instant::now();
        let utxos = {
            tracing::debug!("Getting wallet UTXOs");
            let mut utxos = wallet.get_utxos()?;
            tracing::debug!(utxo_count = utxos.len(), "Got wallet UTXOs");
            tracing::debug!("Getting all transactions from mempool");
            let transactions = node.get_all_transactions()?;
            tracing::debug!(
                transaction_count = transactions.len(),
                "Got all transactions from mempool"
            );
            for transaction in &transactions {
                for (outpoint, _) in &transaction.transaction.inputs {
                    utxos.remove(outpoint);
                }
            }
            tracing::debug!(
                final_utxo_count = utxos.len(),
                "UTXOs initialized after removing spent outputs"
            );
            Arc::new(RwLock::new(utxos))
        };
        let utxos_elapsed = utxos_start.elapsed();
        tracing::info!(
            elapsed_secs = utxos_elapsed.as_secs_f64(),
            "UTXOs initialized"
        );
        tracing::debug!("Wrapping node in Arc");
        let node = Arc::new(node);
        tracing::debug!("Node wrapped in Arc");

        // Check initial state
        tracing::debug!("Checking initial sidechain state");
        if let Ok(Some(tip)) = node.try_get_best_hash() {
            if let Ok(Some(height)) = node.try_get_height() {
                tracing::info!(
                    tip = %tip,
                    height = %height,
                    "Current sidechain tip"
                );
            }
        } else {
            tracing::info!("No sidechain tip found (chain is empty)");
        }

        // Recover wallet addresses from the UTXO set before the initial
        // wallet update. This handles restoring a wallet from mnemonic
        // against an already-synced chain: without this, the address
        // index would be empty and update_wallet would find no UTXOs.
        recover_wallet_addresses(node.as_ref(), &wallet)?;

        // Perform initial wallet update to populate wallet with all existing UTXOs
        tracing::info!(
            "Performing initial wallet update to load all past transactions"
        );
        let initial_wallet_update_start = std::time::Instant::now();
        update_wallet(node.as_ref(), &wallet).inspect_err(|err| {
            tracing::error!("Failed to perform initial wallet update: {err:#}");
        })?;
        let initial_wallet_update_elapsed =
            initial_wallet_update_start.elapsed();
        tracing::info!(
            elapsed_secs = initial_wallet_update_elapsed.as_secs_f64(),
            "Initial wallet update completed"
        );

        // Update the utxos after initial wallet update
        *utxos.write() = wallet.get_utxos()?;
        tracing::debug!(
            utxo_count = utxos.read().len(),
            "UTXOs updated after initial wallet sync"
        );

        tracing::debug!("Wrapping miner in Arc and TokioRwLock");
        let miner = Arc::new(TokioRwLock::new(miner));
        tracing::info!("Spawning wallet update task");
        let task =
            Self::spawn_task(node.clone(), utxos.clone(), wallet.clone());
        tracing::info!("Wallet update task spawned");

        // Spawn L1 sync task to periodically check for new deposits and mainchain reachability
        tracing::info!("Spawning L1 sync task for deposit scanning");
        let _l1_sync_task = Self::spawn_l1_sync_task(
            node.clone(),
            mainchain_monitor.clone(),
            miner.clone(),
            transport_for_reconnect,
        );
        tracing::info!("L1 sync task spawned");

        // Swap confirmations are refreshed by the node's swap observer, which
        // owns all parent-chain polling. There used to be a second copy of that
        // logic here, and two more in the GUI.

        tracing::debug!("Dropping runtime guard");
        drop(rt_guard);
        tracing::info!("App initialization complete");
        Ok(Self {
            node,
            wallet,
            cusf_mainchain_wallet,
            miner,
            utxos,
            task: Arc::new(task),
            transaction: Arc::new(RwLock::new(Transaction {
                inputs: vec![],
                proof: Proof::default(),
                outputs: vec![],
                data: coinshift::types::TxData::Regular,
            })),
            runtime: Arc::new(runtime),
            local_pool,
            mainchain: mainchain_monitor,
        })
    }

    /// Update utxos & wallet
    fn update(&self) -> Result<(), Error> {
        update(self.node.as_ref(), &mut self.utxos.write(), &self.wallet)
    }

    /// Returns true if an outpoint is locked to a swap, and may therefore only
    /// be spent by a SwapClaim transaction. A new read transaction is created
    /// for each check to avoid lifetime issues, so that the latest state is
    /// always read.
    pub fn is_output_locked_to_swap(&self, outpoint: &OutPoint) -> bool {
        let rotxn = match self.node.env().read_txn() {
            Ok(rotxn) => rotxn,
            Err(err) => {
                tracing::warn!(
                    outpoint = ?outpoint,
                    error = %err,
                    "Failed to create read transaction for locked output check"
                );
                return false;
            }
        };
        match self.node.state().is_output_locked_to_swap(&rotxn, outpoint) {
            Ok(locked) => locked.is_some(),
            Err(err) => {
                tracing::warn!(
                    outpoint = ?outpoint,
                    error = %err,
                    "Error checking if output is locked"
                );
                false
            }
        }
    }

    pub fn sign_and_send(&self, tx: Transaction) -> Result<(), Error> {
        let txid = tx.txid();
        tracing::debug!(%txid, "sign_and_send: Starting transaction signing and sending");

        let authorized_transaction = match self.wallet.authorize(tx) {
            Ok(auth_tx) => {
                tracing::debug!(%txid, "sign_and_send: Transaction authorized successfully");
                auth_tx
            }
            Err(err) => {
                tracing::error!(%txid, error = %err, "sign_and_send: Failed to authorize transaction");
                return Err(err.into());
            }
        };

        tracing::debug!(%txid, "sign_and_send: Submitting transaction to node");
        match self.node.submit_transaction(authorized_transaction) {
            Ok(()) => {
                tracing::debug!(%txid, "sign_and_send: Transaction submitted to node successfully");
            }
            Err(err) => {
                tracing::error!(
                    %txid,
                    error = %err,
                    error_debug = ?err,
                    "sign_and_send: Failed to submit transaction to node"
                );
                return Err(err.into());
            }
        }

        tracing::debug!(%txid, "sign_and_send: Updating wallet state");
        match self.update() {
            Ok(()) => {
                tracing::debug!(%txid, "sign_and_send: Wallet updated successfully");
            }
            Err(err) => {
                tracing::error!(
                    %txid,
                    error = %err,
                    error_debug = ?err,
                    "sign_and_send: Failed to update wallet"
                );
                return Err(err);
            }
        }

        tracing::info!(%txid, "sign_and_send: Transaction signed and sent successfully");
        Ok(())
    }

    pub fn get_new_main_address(
        &self,
    ) -> Result<bitcoin::Address<bitcoin::address::NetworkChecked>, Error> {
        let address = self.runtime.block_on({
            let miner = self.miner.clone();
            async move {
                let mut guard = miner.write().await;
                let miner_write =
                    guard.as_mut().ok_or(Error::NoCusfMainchainWalletClient)?;
                let cusf_mainchain = &mut miner_write.cusf_mainchain;
                let mainchain_info = cusf_mainchain.get_chain_info().await?;
                let cusf_mainchain_wallet =
                    &mut miner_write.cusf_mainchain_wallet;
                let res = cusf_mainchain_wallet
                    .create_new_address()
                    .await?
                    .require_network(mainchain_info.network)
                    .unwrap();
                drop(guard);
                Result::<_, Error>::Ok(res)
            }
        })?;
        Ok(address)
    }

    const EMPTY_BLOCK_BMM_BRIBE: bitcoin::Amount =
        bitcoin::Amount::from_sat(1000);

    pub async fn mine(
        &self,
        fee: Option<bitcoin::Amount>,
    ) -> Result<(), Error> {
        let miner = self.miner.clone();
        // Mining requires the mainchain (parentchain) to be up so we can fetch
        // blocks. Note this asks the enforcer rather than consulting the
        // connection monitor: a cached status must never refuse a mine that
        // would have succeeded.
        let prev_main_hash = {
            let mut guard = miner.write().await;
            let miner_write =
                guard.as_mut().ok_or(Error::NoCusfMainchainWalletClient)?;
            let prev_main_hash = miner_write
                .cusf_mainchain
                .get_chain_tip()
                .await
                .map_err(|e| Error::MainchainUnreachable(Box::new(e)))?
                .block_hash;
            drop(guard);
            prev_main_hash
        };
        let tip_hash = self.node.try_get_best_hash()?;
        // If `prev_side_hash` is not the best tip to mine on, then mine an
        // empty block.
        // This is a temporary fix, ideally we always choose the best tip to
        // mine on
        let prev_side_hash = if let Some(tip_hash) = tip_hash {
            let tip_header = self.node.get_header(tip_hash)?;
            let archive = self.node.archive();
            let prev_main_hash_header_in_archive = {
                let rotxn =
                    self.node.env().read_txn().map_err(node::Error::from)?;
                archive
                    .try_get_main_header_info(&rotxn, &prev_main_hash)
                    .map_err(node::Error::from)?
                    .is_some()
            };
            if !prev_main_hash_header_in_archive {
                // Request mainchain header info
                if !self
                    .node
                    .request_mainchain_ancestor_infos(prev_main_hash)
                    .await?
                {
                    return Err(Error::RequestMainchainAncestorInfos {
                        block_hash: prev_main_hash,
                    });
                }
            }
            let rotxn =
                self.node.env().read_txn().map_err(node::Error::from)?;
            let last_common_main_ancestor = archive
                .last_common_main_ancestor(
                    &rotxn,
                    prev_main_hash,
                    tip_header.prev_main_hash,
                )
                .map_err(node::Error::from)?;
            if last_common_main_ancestor == tip_header.prev_main_hash {
                Some(tip_hash)
            } else {
                // Find a tip to mine on
                archive
                    .ancestor_headers(&rotxn, tip_hash)
                    .find_map(|(block_hash, header)| {
                        if header.prev_main_hash == last_common_main_ancestor {
                            Ok(None)
                        } else if archive.is_main_descendant(
                            &rotxn,
                            header.prev_main_hash,
                            last_common_main_ancestor,
                        )? {
                            Ok(Some(block_hash))
                        } else {
                            Ok(None)
                        }
                    })
                    .map_err(node::Error::from)?
            }
        } else {
            None
        };
        let (bribe, header, body) = if prev_side_hash == tip_hash {
            const NUM_TRANSACTIONS: usize = 1000;
            let (txs, tx_fees) =
                self.node.get_transactions(NUM_TRANSACTIONS)?;
            let coinbase = match tx_fees {
                bitcoin::Amount::ZERO => Vec::new(),
                _ => vec![types::Output {
                    address: self.wallet.get_new_address()?,
                    content: types::OutputContent::Value(tx_fees),
                }],
            };
            let (merkle_root, roots) = {
                let mut accumulator = if let Some(tip_hash) = tip_hash {
                    let rotxn = self
                        .node
                        .env()
                        .read_txn()
                        .map_err(node::Error::from)?;
                    self.node
                        .archive()
                        .get_accumulator(&rotxn, tip_hash)
                        .map_err(node::Error::from)?
                } else {
                    types::Accumulator::default()
                };
                let merkle_root = coinshift::types::Body::modify_memforest(
                    &coinbase,
                    &txs,
                    &mut accumulator.0,
                )?;
                let roots = accumulator
                    .0
                    .get_roots()
                    .iter()
                    .map(|root| root.get_data())
                    .collect();
                (merkle_root, roots)
            };
            let body = types::Body::new(
                txs.into_iter().map(|tx| tx.into()).collect(),
                coinbase,
            );
            let header = types::Header {
                merkle_root,
                roots,
                prev_side_hash,
                prev_main_hash,
            };
            let bribe = fee.unwrap_or_else(|| {
                if tx_fees > bitcoin::Amount::ZERO {
                    tx_fees
                } else {
                    Self::EMPTY_BLOCK_BMM_BRIBE
                }
            });
            (bribe, header, body)
        } else {
            let coinbase = Vec::new();
            let (merkle_root, roots) = {
                let mut accumulator = if let Some(tip_hash) = tip_hash {
                    let rotxn = self
                        .node
                        .env()
                        .read_txn()
                        .map_err(node::Error::from)?;
                    self.node
                        .archive()
                        .get_accumulator(&rotxn, tip_hash)
                        .map_err(node::Error::from)?
                } else {
                    types::Accumulator::default()
                };
                let merkle_root =
                    coinshift::types::Body::modify_memforest::<
                        FilledTransaction,
                    >(&coinbase, &[], &mut accumulator.0)?;
                let roots = accumulator
                    .0
                    .get_roots()
                    .iter()
                    .map(|root| root.get_data())
                    .collect();
                (merkle_root, roots)
            };
            let body = types::Body::new(Vec::new(), coinbase);
            let header = types::Header {
                merkle_root,
                roots,
                prev_side_hash,
                prev_main_hash,
            };
            let bribe = Self::EMPTY_BLOCK_BMM_BRIBE;
            (bribe, header, body)
        };
        let mut guard = miner.write().await;
        let miner_write =
            guard.as_mut().ok_or(Error::NoCusfMainchainWalletClient)?;
        let bmm_txid = miner_write
            .attempt_bmm(bribe.to_sat(), 0, header, body)
            .await?;

        tracing::info!(%bmm_txid, "mine: BMM transaction sent, waiting for confirmation");
        tracing::debug!(%bmm_txid, "mine: confirming BMM...");
        if let Some((main_hash, header, body)) =
            miner_write.confirm_bmm().await.inspect_err(|err| {
                tracing::error!("{:#}", coinshift::util::ErrorChain::new(err))
            })?
        {
            tracing::debug!(
                %main_hash, side_hash = %header.hash(), "mine: confirmed BMM, submitting block",
            );
            match self
                .node
                .submit_block(main_hash, &header, &body)
                .await
                .inspect_err(|err| {
                    tracing::error!(
                        "{:#}",
                        coinshift::util::ErrorChain::new(err)
                    )
                })? {
                true => {
                    tracing::debug!(
                         %main_hash, "mine: BMM accepted as new tip",
                    );
                }
                false => {
                    tracing::warn!(
                        %main_hash, "mine: BMM not accepted as new tip",
                    );
                }
            }
        }

        let () = self.update()?;

        self.node
            .regenerate_proof(&mut self.transaction.write())
            .inspect_err(|err| {
                tracing::error!("mine: unable to regenerate proof: {err:#}");
            })?;
        Ok(())
    }

    pub fn deposit(
        &self,
        address: Address,
        amount: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> Result<bitcoin::Txid, Error> {
        tracing::debug!(
            "deposit parameters: address = {}, amount = {}, fee = {}",
            address,
            amount,
            fee
        );
        let Some(wallet_client) = self.cusf_mainchain_wallet.as_ref() else {
            return Err(Error::NoCusfMainchainWalletClient);
        };
        let mut wallet_client = wallet_client.clone();
        self.runtime.block_on(async {
            let txid = wallet_client
                .create_deposit_tx(address, amount.to_sat(), fee.to_sat())
                .await?;
            Ok(txid)
        })
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.task.abort()
    }
}
