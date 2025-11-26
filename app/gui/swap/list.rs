use eframe::egui::{self, Button, ScrollArea};
use coinshift::types::{Address, Swap, SwapId, SwapState, SwapTxId, ParentChainType};
use coinshift::bitcoin_rpc::{BitcoinRpcClient, RpcConfig};
use std::collections::HashMap;
use hex;

use crate::app::App;
use crate::gui::util::show_btc_amount;

#[derive(Default)]
pub struct SwapList {
    swaps: Option<Vec<Swap>>,
    selected_swap_id: Option<String>,
    l1_txid_input: String,
    l2_recipient_input: String,  // L2 address that will receive the L2 amount (for L1 transaction detection)
    fetching_confirmations: bool,
    claimer_address_input: String,  // L2 claimer address when claiming (for open swaps)
}

impl SwapList {
    pub fn new(app: Option<&App>) -> Self {
        let mut list = Self::default();
        if let Some(app) = app {
            list.refresh_swaps(app);
        }
        list
    }

    fn refresh_swaps(&mut self, app: &App) {
        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get read transaction: {err:#}");
                return;
            }
        };

        let mut swaps_result = match app.node.state().load_all_swaps(&rotxn) {
            Ok(swaps) => swaps,
            Err(err) => {
                tracing::error!("Failed to list swaps: {err:#}");
                return;
            }
        };

        // Also get pending swaps from mempool
        drop(rotxn); // Release the transaction before getting mempool transactions
        
        if let Ok(mempool_txs) = app.node.get_all_transactions() {
            for tx in mempool_txs {
                if let coinshift::types::TxData::SwapCreate {
                    swap_id,
                    parent_chain,
                    l1_txid_bytes: _,
                    required_confirmations,
                    l2_recipient,
                    l2_amount,
                    l1_recipient_address,
                    l1_amount,
                } = &tx.transaction.data
                {
                    // Check if this swap is already in the confirmed list
                    let swap_id_obj = coinshift::types::SwapId(*swap_id);
                    if !swaps_result.iter().any(|s| s.id == swap_id_obj) {
                        // Create a pending swap entry
                        let txid = tx.transaction.txid();
                        let l1_txid = coinshift::types::SwapTxId::from_bytes(&vec![0u8; 32]);
                        let swap = coinshift::types::Swap::new(
                            swap_id_obj,
                            coinshift::types::SwapDirection::L2ToL1,
                            *parent_chain,
                            l1_txid,
                            Some(*required_confirmations),
                            *l2_recipient,
                            bitcoin::Amount::from_sat(*l2_amount),
                            l1_recipient_address.clone(),
                            l1_amount.map(bitcoin::Amount::from_sat),
                            0, // Height 0 for pending (not yet in a block)
                            None, // No expiration
                        );
                        swaps_result.push(swap);
                        tracing::debug!(
                            swap_id = %swap_id_obj,
                            txid = %txid,
                            "Found pending swap in mempool"
                        );
                    }
                }
            }
        }

        self.swaps = Some(swaps_result);
    }

    pub fn show(&mut self, app: Option<&App>, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("My Swaps");
            if ui.button("Refresh").clicked() {
                if let Some(app) = app {
                    self.refresh_swaps(app);
                }
            }
        });
        ui.separator();

        let swaps = match &self.swaps {
            Some(swaps) => swaps,
            None => {
                ui.label("No swaps loaded. Click Refresh to load swaps.");
                return;
            }
        };

        if swaps.is_empty() {
            ui.label("No swaps found.");
            ui.separator();
            ui.label("Note: Swaps need to be included in a block before they appear in the state.");
            ui.label("If you just created a swap, it may be pending in the mempool.");
            ui.label("Mine a block or wait for one to be mined to confirm your swap.");
            return;
        }

        let swaps_clone = swaps.clone();
        ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("swaps_grid")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for swap in &swaps_clone {
                        self.show_swap_row(swap, app, ui);
                    }
                });
        });
    }

    fn show_swap_row(
        &mut self,
        swap: &Swap,
        app: Option<&App>,
        ui: &mut egui::Ui,
    ) {
        let swap_id_str = swap.id.to_string();
        let is_selected = self
            .selected_swap_id
            .as_ref()
            .map(|id| id == &swap_id_str)
            .unwrap_or(false);

        // Swap ID
        ui.horizontal(|ui| {
            if ui.selectable_label(is_selected, &swap_id_str[..16]).clicked() {
                if is_selected {
                    self.selected_swap_id = None;
                } else {
                    self.selected_swap_id = Some(swap_id_str.clone());
                }
            }
        });

        // Swap details
        ui.vertical(|ui| {
            // Show if swap is pending (not yet in a block)
            if swap.created_at_height == 0 {
                ui.label(egui::RichText::new("⚠️ PENDING (in mempool, not yet in block)").color(egui::Color32::RED));
                ui.label(egui::RichText::new("💡 Tip: Click 'Mine / Refresh Block' in the bottom panel to include this swap in a block").small().color(egui::Color32::GRAY));
            }
            ui.label(format!("Chain: {:?}", swap.parent_chain));
            ui.label(format!("State: {:?}", swap.state));
            ui.label(format!("L2 Amount: {}", show_btc_amount(swap.l2_amount)));
            if let Some(l1_amount) = swap.l1_amount {
                ui.label(format!("L1 Amount: {}", show_btc_amount(l1_amount)));
            }
            if let Some(addr) = &swap.l2_recipient {
                ui.label(format!("L2 Recipient: {}", addr));
            } else {
                ui.label("L2 Recipient: Open Swap");
            }
            if let Some(addr) = &swap.l1_recipient_address {
                ui.label(format!("L1 Recipient: {}", addr));
            }
            if let Some(addr) = &swap.l1_claimer_address {
                ui.label(format!("L1 Claimer: {}", addr));
            }

            // Show L1 transaction ID
            match &swap.l1_txid {
                SwapTxId::Hash32(hash) => {
                    // Convert [u8; 32] to Txid using from_slice
                    use bitcoin::hashes::Hash;
                    let txid = bitcoin::Txid::from_slice(hash).unwrap_or_else(|_| {
                        bitcoin::Txid::all_zeros()
                    });
                    ui.label(format!("L1 TxID: {}", txid));
                }
                SwapTxId::Hash(bytes) => {
                    ui.label(format!("L1 TxID: {}", hex::encode(bytes)));
                }
            }

            // Cancel and Delete buttons
            ui.separator();
            ui.horizontal(|ui| {
                // Cancel button (only for pending swaps)
                if matches!(swap.state, SwapState::Pending) {
                    if ui
                        .add_enabled(
                            app.is_some(),
                            Button::new(egui::RichText::new("❌ Cancel Swap").color(egui::Color32::ORANGE)),
                        )
                        .clicked()
                    {
                        if let Some(app) = app {
                            self.cancel_swap(app, &swap.id);
                        }
                    }
                    ui.label("(Unlocks outputs and marks as cancelled)");
                }
                
                // Delete button (only for pending or cancelled swaps)
                if matches!(swap.state, SwapState::Pending | SwapState::Cancelled) {
                    if ui
                        .add_enabled(
                            app.is_some(),
                            Button::new(egui::RichText::new("🗑️ Delete Swap").color(egui::Color32::RED)),
                        )
                        .clicked()
                    {
                        if let Some(app) = app {
                            self.delete_swap(app, &swap.id);
                        }
                    }
                    ui.label("(Permanently removes from database)");
                }
            });

            // Action buttons based on state
            match &swap.state {
                SwapState::Pending => {
                    ui.separator();
                    ui.label(egui::RichText::new("L1 Transaction Detection").heading());
                    ui.label("When someone sends an L1 transaction to fulfill this swap, update it here:");
                    ui.label(egui::RichText::new("Note: Automatic detection runs during block processing if RPC is configured. You can also manually update with the L1 txid - confirmations will be fetched automatically from RPC.").small().color(egui::Color32::GRAY));
                    
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("L2 Address (that will receive L2 amount):");
                            ui.add(egui::TextEdit::singleline(&mut self.l2_recipient_input).desired_width(400.0));
                        });
                        
                        ui.horizontal(|ui| {
                            ui.label("L1 Transaction ID (hex):");
                            ui.add(egui::TextEdit::singleline(&mut self.l1_txid_input).desired_width(400.0));
                        });
                        
                        if self.fetching_confirmations {
                            ui.label(egui::RichText::new("⏳ Fetching confirmations from L1 RPC...").color(egui::Color32::YELLOW));
                        }
                        
                        // Note: For open swaps, the L2 address will be used when claiming
                        // For pre-specified swaps, this field can be left empty (swap already has l2_recipient)
                        
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    app.is_some() && !self.l1_txid_input.is_empty(),
                                    Button::new("Update Swap (Auto-fetch confirmations from RPC)"),
                                )
                                .clicked()
                            {
                                if let Some(app) = app {
                                    self.update_swap_with_auto_confirmations(app, &swap);
                                }
                            }
                            
                            if ui
                                .add_enabled(
                                    app.is_some() && !self.l1_txid_input.is_empty(),
                                    Button::new("Fetch Confirmations Only"),
                                )
                                .clicked()
                            {
                                if let Some(app) = app {
                                    self.fetch_confirmations_from_rpc(app, &swap);
                                }
                            }
                        });
                        
                        ui.label(egui::RichText::new("Note: Confirmations will be automatically fetched from the L1 RPC node if configured in L1 Config.").small().color(egui::Color32::GRAY));
                    });
                }
                SwapState::ReadyToClaim => {
                    if swap.l2_recipient.is_none() {
                        // Open swap - need claimer address
                        // Pre-fill from L2 recipient input if available and claimer address is empty
                        if self.claimer_address_input.is_empty() && !self.l2_recipient_input.is_empty() {
                            self.claimer_address_input = self.l2_recipient_input.clone();
                        }
                        
                        ui.horizontal(|ui| {
                            ui.label("Claimer Address:");
                            ui.text_edit_singleline(&mut self.claimer_address_input);
                            if ui
                                .add_enabled(
                                    app.is_some() && !self.claimer_address_input.is_empty(),
                                    Button::new("Claim"),
                                )
                                .clicked()
                            {
                                if let Some(app) = app {
                                    let claimer_addr: Address = match self.claimer_address_input.parse() {
                                        Ok(addr) => addr,
                                        Err(err) => {
                                            tracing::error!("Invalid address: {err}");
                                            return;
                                        }
                                    };
                                    self.claim_swap(app, &swap.id, Some(claimer_addr));
                                }
                            }
                        });
                    } else {
                        // Regular swap - claim with recipient address
                        if ui
                            .add_enabled(app.is_some(), Button::new("Claim Swap"))
                            .clicked()
                        {
                            if let Some(app) = app {
                                self.claim_swap(app, &swap.id, None);
                            }
                        }
                    }
                }
                _ => {}
            }
        });

        ui.end_row();
    }

    fn claim_swap(&mut self, app: &App, swap_id: &SwapId, l2_claimer_address: Option<Address>) {
        let accumulator = match app.node.get_tip_accumulator() {
            Ok(acc) => acc,
            Err(err) => {
                tracing::error!("Failed to get accumulator: {err:#}");
                return;
            }
        };

        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get read transaction: {err:#}");
                return;
            }
        };

        let swap = match app.node.state().get_swap(&rotxn, swap_id) {
            Ok(Some(swap)) => swap,
            Ok(None) => {
                tracing::error!("Swap not found");
                return;
            }
            Err(err) => {
                tracing::error!("Failed to get swap: {err:#}");
                return;
            }
        };

        // Get locked outputs for this swap (from wallet UTXOs)
        let wallet_utxos = match app.wallet.get_utxos() {
            Ok(utxos) => utxos,
            Err(err) => {
                tracing::error!("Failed to get wallet UTXOs: {err:#}");
                return;
            }
        };

        let locked_outputs: Vec<_> = wallet_utxos
            .into_iter()
            .filter_map(|(outpoint, output)| {
                if app
                    .node
                    .state()
                    .is_output_locked_to_swap(&rotxn, &outpoint)
                    .ok()?
                    == Some(*swap_id)
                {
                    Some((outpoint, output))
                } else {
                    None
                }
            })
            .collect();

        if locked_outputs.is_empty() {
            tracing::error!("No locked outputs found for swap");
            return;
        }

        // Determine recipient: pre-specified swap uses swap.l2_recipient, open swap uses claimer address
        let recipient = swap
            .l2_recipient
            .or(l2_claimer_address)
            .ok_or_else(|| {
                tracing::error!("Open swap requires claimer address");
            })
            .ok();

        let recipient = match recipient {
            Some(addr) => addr,
            None => return,
        };

        let tx = match app.wallet.create_swap_claim_tx(
            &accumulator,
            *swap_id,
            recipient,
            locked_outputs,
            l2_claimer_address,
        ) {
            Ok(tx) => tx,
            Err(err) => {
                tracing::error!("Failed to create claim transaction: {err:#}");
                return;
            }
        };

        let txid = tx.txid();
        if let Err(err) = app.sign_and_send(tx) {
            tracing::error!("Failed to send transaction: {err:#}");
            return;
        }

        tracing::info!("Swap claimed: swap_id={}, txid={}", swap_id, txid);
        self.claimer_address_input.clear();
        self.refresh_swaps(app);
    }

    fn load_rpc_config(&self, parent_chain: ParentChainType) -> Option<RpcConfig> {
        use std::path::PathBuf;
        use dirs;
        use serde::{Deserialize, Serialize};
        
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
        
        if let Ok(file_content) = std::fs::read_to_string(&config_path) {
            if let Ok(configs) = serde_json::from_str::<HashMap<ParentChainType, LocalRpcConfig>>(&file_content) {
                if let Some(local_config) = configs.get(&parent_chain) {
                    return Some(RpcConfig {
                        url: local_config.url.clone(),
                        user: local_config.user.clone(),
                        password: local_config.password.clone(),
                    });
                }
            }
        }
        None
    }

    fn fetch_confirmations_from_rpc(&mut self, app: &App, swap: &Swap) {
        if self.l1_txid_input.is_empty() {
            tracing::warn!(
                swap_id = %swap.id,
                "Cannot fetch confirmations: L1 txid input is empty"
            );
            return;
        }

        tracing::debug!(
            swap_id = %swap.id,
            l1_txid_input = %self.l1_txid_input,
            parent_chain = ?swap.parent_chain,
            "Starting to fetch confirmations from RPC"
        );

        if let Some(rpc_config) = self.load_rpc_config(swap.parent_chain) {
            tracing::debug!(
                swap_id = %swap.id,
                rpc_url = %rpc_config.url,
                rpc_user = %rpc_config.user,
                "Loaded RPC config"
            );
            
            self.fetching_confirmations = true;
            let txid_hex = self.l1_txid_input.clone();
            
            // Spawn a thread to fetch confirmations
            let client = BitcoinRpcClient::new(rpc_config);
            match client.get_transaction_confirmations(&txid_hex) {
                Ok(confirmations) => {
                    tracing::info!(
                        swap_id = %swap.id,
                        l1_txid = %txid_hex,
                        confirmations = %confirmations,
                        "Fetched confirmations from L1 RPC"
                    );
                    // Show confirmations to user (could display in UI)
                    // For now, we'll use this when updating
                }
                Err(err) => {
                    tracing::error!(
                        swap_id = %swap.id,
                        l1_txid = %txid_hex,
                        error = %err,
                        error_debug = ?err,
                        "Failed to fetch confirmations from L1 RPC"
                    );
                }
            }
            self.fetching_confirmations = false;
        } else {
            tracing::warn!(
                swap_id = %swap.id,
                parent_chain = ?swap.parent_chain,
                "No RPC config found for parent chain"
            );
        }
    }

    fn update_swap_with_auto_confirmations(&mut self, app: &App, swap: &Swap) {
        tracing::debug!(
            swap_id = %swap.id,
            created_at_height = %swap.created_at_height,
            state = ?swap.state,
            parent_chain = ?swap.parent_chain,
            l1_txid = ?swap.l1_txid,
            l2_recipient = ?swap.l2_recipient,
            l1_txid_input = %self.l1_txid_input,
            "Starting update_swap_with_auto_confirmations"
        );

        if self.l1_txid_input.is_empty() {
            tracing::warn!(
                swap_id = %swap.id,
                "L1 txid input is empty, cannot update"
            );
            return;
        }

        // Check if swap is in mempool
        let swap_in_mempool = app.node.get_all_transactions()
            .ok()
            .map(|txs| {
                txs.iter().any(|tx| {
                    if let coinshift::types::TxData::SwapCreate {
                        swap_id: tx_swap_id,
                        ..
                    } = &tx.transaction.data
                    {
                        coinshift::types::SwapId(*tx_swap_id) == swap.id
                    } else {
                        false
                    }
                })
            })
            .unwrap_or(false);

        tracing::debug!(
            swap_id = %swap.id,
            swap_in_mempool = %swap_in_mempool,
            "Checked if swap is in mempool"
        );

        // Verify swap exists in database before attempting update
        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!(
                    swap_id = %swap.id,
                    error = %err,
                    "Failed to get read transaction"
                );
                return;
            }
        };

        // Debug: List all swaps in database for comparison
        let all_db_swaps = app.node.state().load_all_swaps(&rotxn);
        if let Ok(swaps) = &all_db_swaps {
            let swap_ids: Vec<String> = swaps.iter().map(|s| s.id.to_string()).collect();
            tracing::debug!(
                swap_id = %swap.id,
                total_swaps_in_db = %swaps.len(),
                db_swap_ids = ?swap_ids,
                "All swaps currently in database"
            );
        } else {
            tracing::warn!(
                swap_id = %swap.id,
                error = ?all_db_swaps.as_ref().unwrap_err(),
                "Failed to load all swaps from database for debugging"
            );
        }

        let swap_in_db = app.node.state().get_swap(&rotxn, &swap.id);
        let swap_exists_in_db = match swap_in_db {
            Ok(Some(db_swap)) => {
                tracing::debug!(
                    swap_id = %swap.id,
                    db_created_at_height = %db_swap.created_at_height,
                    db_state = ?db_swap.state,
                    db_l1_txid = ?db_swap.l1_txid,
                    "Swap found in database"
                );
                true
            }
            Ok(None) => {
                tracing::debug!(
                    swap_id = %swap.id,
                    "Swap not found in database"
                );
                false
            }
            Err(err) => {
                tracing::error!(
                    swap_id = %swap.id,
                    error = %err,
                    "Failed to check if swap exists in database"
                );
                drop(rotxn);
                return;
            }
        };
        drop(rotxn);

        tracing::debug!(
            swap_id = %swap.id,
            swap_exists_in_db = %swap_exists_in_db,
            swap_in_mempool = %swap_in_mempool,
            swap_created_at_height = %swap.created_at_height,
            "Swap status check complete"
        );

        // If swap is not in database and is pending (created_at_height == 0), reject
        if !swap_exists_in_db && swap.created_at_height == 0 {
            tracing::error!(
                swap_id = %swap.id,
                swap_in_mempool = %swap_in_mempool,
                swap_created_at_height = %swap.created_at_height,
                "Cannot update pending swap. The swap must be confirmed in a block first."
            );
            return;
        }

        // If swap doesn't exist in database, reject (even if created_at_height > 0, which shouldn't happen)
        if !swap_exists_in_db {
            tracing::error!(
                swap_id = %swap.id,
                swap_created_at_height = %swap.created_at_height,
                swap_in_mempool = %swap_in_mempool,
                "Swap not found in database. It may have been deleted or not yet confirmed."
            );
            return;
        }

        let l1_txid_bytes = match hex::decode(&self.l1_txid_input) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::error!("Invalid hex: {err}");
                return;
            }
        };
        let l1_txid = SwapTxId::from_bytes(&l1_txid_bytes);

        // Fetch confirmations from RPC if available
        let confirmations = if let Some(rpc_config) = self.load_rpc_config(swap.parent_chain) {
            tracing::debug!(
                swap_id = %swap.id,
                l1_txid = %self.l1_txid_input,
                rpc_url = %rpc_config.url,
                "Fetching confirmations from RPC for swap update"
            );
            
            let client = BitcoinRpcClient::new(rpc_config);
            match client.get_transaction_confirmations(&self.l1_txid_input) {
                Ok(conf) => {
                    tracing::info!(
                        swap_id = %swap.id,
                        l1_txid = %self.l1_txid_input,
                        confirmations = %conf,
                        "Fetched confirmations from L1 RPC"
                    );
                    conf
                }
                Err(err) => {
                    tracing::warn!(
                        swap_id = %swap.id,
                        l1_txid = %self.l1_txid_input,
                        error = %err,
                        error_debug = ?err,
                        "Failed to fetch confirmations from RPC, using 0"
                    );
                    0
                }
            }
        } else {
            tracing::warn!(
                swap_id = %swap.id,
                "No RPC config available, using 0 confirmations"
            );
            0
        };

        // For open swaps, we don't store the L1 sender address
        // The claimer will provide their L2 address when claiming
        let l1_claimer_address = None;

        let mut rwtxn = match app.node.env().write_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get write transaction: {err:#}");
                return;
            }
        };

        if let Err(err) = app.node.state().update_swap_l1_txid(
            &mut rwtxn,
            &swap.id,
            l1_txid,
            confirmations,
            l1_claimer_address,
        ) {
            tracing::error!("Failed to update swap: {err:#}");
            return;
        }

        if let Err(err) = rwtxn.commit() {
            tracing::error!("Failed to commit: {err:#}");
            return;
        }

        self.l1_txid_input.clear();
        self.l2_recipient_input.clear();
        self.refresh_swaps(app);
    }

    fn cancel_swap(&mut self, app: &App, swap_id: &SwapId) {
        // Check if this is a pending swap (in mempool, not yet in a block)
        let is_pending = self.swaps.as_ref().and_then(|swaps| {
            swaps.iter().find(|s| s.id == *swap_id)
        }).map(|s| s.created_at_height == 0).unwrap_or(false);

        if is_pending {
            // For pending swaps, remove from mempool
            if let Ok(mempool_txs) = app.node.get_all_transactions() {
                for tx in mempool_txs {
                    if let coinshift::types::TxData::SwapCreate {
                        swap_id: tx_swap_id,
                        ..
                    } = &tx.transaction.data
                    {
                        if coinshift::types::SwapId(*tx_swap_id) == *swap_id {
                            let txid = tx.transaction.txid();
                            if let Err(err) = app.node.remove_from_mempool(txid) {
                                tracing::error!(
                                    swap_id = %swap_id,
                                    txid = %txid,
                                    error = %err,
                                    "Failed to remove pending swap from mempool"
                                );
                                return;
                            }
                            tracing::info!(
                                swap_id = %swap_id,
                                txid = %txid,
                                "Removed pending swap from mempool"
                            );
                            self.refresh_swaps(app);
                            return;
                        }
                    }
                }
            }
            tracing::error!(
                swap_id = %swap_id,
                "Pending swap not found in mempool"
            );
        } else {
            // For confirmed swaps, use state database
            let mut rwtxn = match app.node.env().write_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    tracing::error!("Failed to get write transaction: {err:#}");
                    return;
                }
            };

            match app.node.state().cancel_swap(&mut rwtxn, swap_id) {
                Ok(()) => {
                    if let Err(err) = rwtxn.commit() {
                        tracing::error!("Failed to commit: {err:#}");
                        return;
                    }
                    tracing::info!(swap_id = %swap_id, "Cancelled swap");
                    self.refresh_swaps(app);
                }
                Err(err) => {
                    tracing::error!(
                        swap_id = %swap_id,
                        error = %err,
                        "Failed to cancel swap"
                    );
                }
            }
        }
    }

    fn delete_swap(&mut self, app: &App, swap_id: &SwapId) {
        // Check if this is a pending swap (in mempool, not yet in a block)
        let is_pending = self.swaps.as_ref().and_then(|swaps| {
            swaps.iter().find(|s| s.id == *swap_id)
        }).map(|s| s.created_at_height == 0).unwrap_or(false);

        if is_pending {
            // For pending swaps, remove from mempool
            if let Ok(mempool_txs) = app.node.get_all_transactions() {
                for tx in mempool_txs {
                    if let coinshift::types::TxData::SwapCreate {
                        swap_id: tx_swap_id,
                        ..
                    } = &tx.transaction.data
                    {
                        if coinshift::types::SwapId(*tx_swap_id) == *swap_id {
                            let txid = tx.transaction.txid();
                            if let Err(err) = app.node.remove_from_mempool(txid) {
                                tracing::error!(
                                    swap_id = %swap_id,
                                    txid = %txid,
                                    error = %err,
                                    "Failed to remove pending swap from mempool"
                                );
                                return;
                            }
                            tracing::info!(
                                swap_id = %swap_id,
                                txid = %txid,
                                "Removed pending swap from mempool"
                            );
                            self.refresh_swaps(app);
                            return;
                        }
                    }
                }
            }
            tracing::error!(
                swap_id = %swap_id,
                "Pending swap not found in mempool"
            );
        } else {
            // For confirmed swaps, use state database
            let mut rwtxn = match app.node.env().write_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    tracing::error!("Failed to get write transaction: {err:#}");
                    return;
                }
            };

            if let Err(err) = app.node.state().delete_swap(&mut rwtxn, swap_id) {
                tracing::error!("Failed to delete swap: {err:#}");
                return;
            }

            if let Err(err) = rwtxn.commit() {
                tracing::error!("Failed to commit: {err:#}");
                return;
            }

            tracing::info!(swap_id = %swap_id, "Deleted swap");
            self.refresh_swaps(app);
        }
    }
}

