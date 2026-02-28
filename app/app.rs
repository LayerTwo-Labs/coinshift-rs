use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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

use crate::cli::Config;

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
    #[error("L1 config validation failed: {0}")]
    L1ConfigValidation(#[from] coinshift::parent_chain_rpc::Error),
}

impl From<node::Error> for Error {
    fn from(err: node::Error) -> Self {
        Self::Node(Box::new(err))
    }
}

fn update_wallet(node: &Node, wallet: &Wallet) -> Result<(), Error> {
    tracing::trace!("starting wallet update");
    let addresses = wallet.get_addresses()?;
    let mut utxos = node.get_utxos_by_addresses(&addresses)?;

    // Filter out SwapPending outputs - they should not be in the wallet's UTXO database
    // SwapPending outputs are locked and should only be spent in SwapClaim transactions
    let swap_pending_count = utxos
        .iter()
        .filter(|(_, output)| output.content.is_swap_pending())
        .count();
    if swap_pending_count > 0 {
        tracing::warn!(
            swap_pending_count = swap_pending_count,
            "Filtering out SwapPending outputs from wallet UTXOs"
        );
        utxos.retain(|_, output| !output.content.is_swap_pending());
    }

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
    pub miner: Option<Arc<TokioRwLock<Miner>>>,
    pub utxos: Arc<RwLock<HashMap<OutPoint, Output>>>,
    task: Arc<JoinHandle<()>>,
    pub transaction: Arc<RwLock<Transaction>>,
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub local_pool: LocalPoolHandle,
    /// Set by the L1 sync task: true when the mainchain (parentchain for mining) is reachable.
    /// Mining is only allowed when this is true so we can fetch blocks from the mainchain.
    #[allow(dead_code)]
    pub mainchain_reachable: Arc<AtomicBool>,
}

impl App {
    async fn task(
        node: Arc<Node>,
        utxos: Arc<RwLock<HashMap<OutPoint, Output>>>,
        wallet: Wallet,
    ) -> Result<(), Error> {
        let mut state_changes = node.watch_state();
        while let Some(()) = state_changes.next().await {
            let () = update(&node, &mut utxos.write(), &wallet)?;
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

    /// Periodic task to sync L1 blocks for deposit scanning.
    /// Updates mainchain_reachable so the GUI and mine() can require mainchain to be up.
    async fn l1_sync_task(
        node: Arc<Node>,
        mainchain_reachable: Arc<AtomicBool>,
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
                    mainchain_reachable.store(true, Ordering::SeqCst);
                    tracing::trace!(l1_tip = %hash, "L1 sync task: got L1 chain tip");
                    hash
                }
                Err(err) => {
                    mainchain_reachable.store(false, Ordering::SeqCst);
                    tracing::debug!(
                        error = %err,
                        "L1 sync task: Failed to get L1 chain tip (this is normal if mainchain is not available)"
                    );
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

    fn spawn_l1_sync_task(
        node: Arc<Node>,
        mainchain_reachable: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        spawn(
            Self::l1_sync_task(node, mainchain_reachable).unwrap_or_else(
                |err| {
                    let err = anyhow::Error::from(err);
                    tracing::error!("L1 sync task error: {err:#}")
                },
            ),
        )
    }

    /// Periodic task to check and update swap confirmations dynamically
    /// This works in both GUI and headless mode
    async fn swap_confirmation_check_task(
        node: Arc<Node>,
    ) -> Result<(), Error> {
        use coinshift::parent_chain_rpc::{ParentChainRpcClient, RpcConfig};
        use coinshift::types::{ParentChainType, SwapState, SwapTxId};
        use hex;
        use serde::{Deserialize, Serialize};
        use std::collections::HashMap;
        use std::path::PathBuf;
        use std::time::Duration;

        const CHECK_INTERVAL: Duration = Duration::from_secs(10);

        tracing::info!(
            "Swap confirmation check task started, will check every {} seconds",
            CHECK_INTERVAL.as_secs()
        );

        // Helper to load RPC config (same as in GUI)
        fn load_rpc_config(parent_chain: ParentChainType) -> Option<RpcConfig> {
            #[derive(Clone, Serialize, Deserialize)]
            struct LocalRpcConfig {
                url: String,
                user: String,
                password: String,
            }

            let config_path = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("coinshift")
                .join("l1_rpc_configs.json");

            if let Ok(file_content) = std::fs::read_to_string(&config_path)
                && let Ok(configs) = serde_json::from_str::<
                    HashMap<ParentChainType, LocalRpcConfig>,
                >(&file_content)
                && let Some(local_config) = configs.get(&parent_chain)
            {
                return Some(RpcConfig {
                    url: local_config.url.clone(),
                    user: local_config.user.clone(),
                    password: local_config.password.clone(),
                });
            }
            None
        }

        loop {
            tokio::time::sleep(CHECK_INTERVAL).await;
            tracing::trace!(
                "Swap confirmation check task: checking for swap confirmations"
            );

            // Get swaps from database
            let rotxn = match node.env().read_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    tracing::debug!("Failed to get read transaction: {err:#}");
                    continue;
                }
            };

            let swaps = match node.state().load_all_swaps(&rotxn) {
                Ok(swaps) => swaps,
                Err(err) => {
                    tracing::debug!("Failed to load swaps: {err:#}");
                    continue;
                }
            };

            // Filter swaps that are waiting for confirmations and have an L1 txid
            let swaps_to_check: Vec<_> = swaps
                .iter()
                .filter(|swap| {
                    matches!(swap.state, SwapState::WaitingConfirmations(..))
                        && !matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == [0u8; 32])
                        && !matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0))
                })
                .collect();

            drop(rotxn);

            if swaps_to_check.is_empty() {
                continue;
            }

            tracing::debug!(
                swap_count = swaps_to_check.len(),
                "Checking confirmations for {} swaps",
                swaps_to_check.len()
            );

            let mut updated_count = 0;
            let mut rwtxn = match node.env().write_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    tracing::debug!("Failed to get write transaction: {err:#}");
                    continue;
                }
            };

            for swap in swaps_to_check {
                // Get RPC config for this swap's parent chain
                if let Some(rpc_config) = load_rpc_config(swap.parent_chain) {
                    // Convert L1 txid to hex string for RPC query
                    let l1_txid_hex = match &swap.l1_txid {
                        SwapTxId::Hash32(hash) => {
                            use bitcoin::hashes::Hash;
                            let txid = bitcoin::Txid::from_slice(hash)
                                .unwrap_or_else(|_| bitcoin::Txid::all_zeros());
                            txid.to_string()
                        }
                        SwapTxId::Hash(bytes) => hex::encode(bytes),
                    };

                    // Fetch current confirmations from RPC
                    let client = ParentChainRpcClient::new(rpc_config);
                    match client.get_transaction_confirmations(&l1_txid_hex) {
                        Ok(new_confirmations) => {
                            // Get current confirmations from swap state
                            let current_confirmations = match swap.state {
                                SwapState::WaitingConfirmations(current, _) => {
                                    current
                                }
                                _ => 0,
                            };

                            // Only update if confirmations have increased
                            if new_confirmations > current_confirmations {
                                tracing::info!(
                                    swap_id = %swap.id,
                                    old_confirmations = %current_confirmations,
                                    new_confirmations = %new_confirmations,
                                    required = %swap.required_confirmations,
                                    "Updating swap confirmations dynamically (headless mode)"
                                );

                                // Get current block info for reference
                                let block_hash = match node
                                    .state()
                                    .try_get_tip(&rwtxn)
                                {
                                    Ok(Some(hash)) => hash,
                                    Ok(None) | Err(_) => {
                                        tracing::warn!(
                                            "Could not get block hash for swap update"
                                        );
                                        continue;
                                    }
                                };
                                let block_height = match node
                                    .state()
                                    .try_get_height(&rwtxn)
                                {
                                    Ok(Some(height)) => height,
                                    Ok(None) | Err(_) => {
                                        tracing::warn!(
                                            "Could not get block height for swap update"
                                        );
                                        continue;
                                    }
                                };

                                // Update swap with new confirmations
                                if let Err(err) =
                                    node.state().update_swap_l1_txid(
                                        &mut rwtxn,
                                        &swap.id,
                                        swap.l1_txid.clone(),
                                        new_confirmations,
                                        None, // l1_claimer_address - not needed for confirmation updates
                                        None, // l2_claimer_address - not changed on confirmation update
                                        block_hash,
                                        block_height,
                                    )
                                {
                                    tracing::error!(
                                        swap_id = %swap.id,
                                        error = %err,
                                        "Failed to update swap confirmations"
                                    );
                                } else {
                                    updated_count += 1;
                                }
                            }
                        }
                        Err(err) => {
                            tracing::debug!(
                                swap_id = %swap.id,
                                l1_txid = %l1_txid_hex,
                                error = %err,
                                "Failed to fetch confirmations from RPC (this is normal if RPC is unavailable)"
                            );
                        }
                    }
                }
            }

            if updated_count > 0 {
                if let Err(err) = rwtxn.commit() {
                    tracing::error!("Failed to commit swap updates: {err:#}");
                } else {
                    tracing::info!(
                        updated_swaps = updated_count,
                        "Dynamically updated confirmations for {} swaps (headless mode)",
                        updated_count
                    );
                }
            } else {
                drop(rwtxn);
            }
        }
    }

    fn spawn_swap_confirmation_check_task(node: Arc<Node>) -> JoinHandle<()> {
        spawn(
            Self::swap_confirmation_check_task(node).unwrap_or_else(|err| {
                let err = anyhow::Error::from(err);
                tracing::error!("Swap confirmation check task error: {err:#}")
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

        // Validate L1 config file before start: test all configured networks
        let l1_rpc_config_path = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("coinshift")
            .join("l1_rpc_configs.json");
        coinshift::parent_chain_rpc::validate_l1_config_file(
            &l1_rpc_config_path,
        )?;

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
        // Add a timeout to the connection check so the GUI can start even if mainchain isn't synced
        const CONNECTION_TIMEOUT: std::time::Duration =
            std::time::Duration::from_secs(5);
        let (cusf_mainchain, cusf_mainchain_wallet) = if runtime
            .block_on(tokio::time::timeout(
                CONNECTION_TIMEOUT,
                Self::check_proto_support(transport.clone()),
            ))
            .map_err(|_| Error::VerifyMainchainServices {
                url: Box::new(config.mainchain_grpc_url.clone()),
                source: Box::new(tonic::Status::deadline_exceeded(
                    "Connection check timed out after 5 seconds",
                )),
            })?
            .map_err(|err| Error::VerifyMainchainServices {
                url: Box::new(config.mainchain_grpc_url.clone()),
                source: Box::new(err),
            })? {
            (
                mainchain::ValidatorClient::new(transport.clone()),
                Some(mainchain::WalletClient::new(transport)),
            )
        } else {
            (mainchain::ValidatorClient::new(transport), None)
        };
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
            cusf_mainchain_wallet,
            network: config.network,
            wallet: Some(Arc::new(wallet.clone())),
            l1_rpc_config_path: Some(l1_rpc_config_path),
        };
        let node = Node::new(node_config, &runtime)?;
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
        let miner = miner.map(|miner| Arc::new(TokioRwLock::new(miner)));
        tracing::info!("Spawning wallet update task");
        let task =
            Self::spawn_task(node.clone(), utxos.clone(), wallet.clone());
        tracing::info!("Wallet update task spawned");

        // Spawn L1 sync task to periodically check for new deposits and mainchain reachability
        tracing::info!("Spawning L1 sync task for deposit scanning");
        let mainchain_reachable = Arc::new(AtomicBool::new(false));
        let _l1_sync_task =
            Self::spawn_l1_sync_task(node.clone(), mainchain_reachable.clone());
        tracing::info!("L1 sync task spawned");

        // Spawn swap confirmation check task to periodically update swap confirmations
        tracing::info!("Spawning swap confirmation check task");
        let _swap_confirmation_task =
            Self::spawn_swap_confirmation_check_task(node.clone());
        tracing::info!("Swap confirmation check task spawned");

        tracing::debug!("Dropping runtime guard");
        drop(rt_guard);
        tracing::info!("App initialization complete");
        Ok(Self {
            node,
            wallet,
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
            mainchain_reachable,
        })
    }

    /// Update utxos & wallet
    fn update(&self) -> Result<(), Error> {
        update(self.node.as_ref(), &mut self.utxos.write(), &self.wallet)
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
        let Some(miner) = self.miner.as_ref() else {
            return Err(Error::NoCusfMainchainWalletClient);
        };
        let address = self.runtime.block_on({
            let miner = miner.clone();
            async move {
                let mut miner_write = miner.write().await;
                let cusf_mainchain = &mut miner_write.cusf_mainchain;
                let mainchain_info = cusf_mainchain.get_chain_info().await?;
                let cusf_mainchain_wallet =
                    &mut miner_write.cusf_mainchain_wallet;
                let res = cusf_mainchain_wallet
                    .create_new_address()
                    .await?
                    .require_network(mainchain_info.network)
                    .unwrap();
                drop(miner_write);
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
        let Some(miner) = self.miner.as_ref() else {
            return Err(Error::NoCusfMainchainWalletClient);
        };
        // Mining requires the mainchain (parentchain) to be up so we can fetch blocks.
        let prev_main_hash = {
            let mut miner_write = miner.write().await;
            let prev_main_hash = miner_write
                .cusf_mainchain
                .get_chain_tip()
                .await
                .map_err(|e| Error::MainchainUnreachable(Box::new(e)))?
                .block_hash;
            drop(miner_write);
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
        let mut miner_write = miner.write().await;
        let bmm_txid = miner_write
            .attempt_bmm(bribe.to_sat(), 0, header, body)
            .await?;

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

        drop(miner_write);
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
        let Some(miner) = self.miner.as_ref() else {
            return Err(Error::NoCusfMainchainWalletClient);
        };
        self.runtime.block_on(async {
            let mut miner_write = miner.write().await;
            let txid = miner_write
                .cusf_mainchain_wallet
                .create_deposit_tx(address, amount.to_sat(), fee.to_sat())
                .await?;
            drop(miner_write);
            Ok(txid)
        })
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.task.abort()
    }
}
