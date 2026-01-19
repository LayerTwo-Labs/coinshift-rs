use coinshift::bitcoin_rpc::{BitcoinRpcClient, RpcConfig};
use coinshift::types::{
    Address, ParentChainType, Swap, SwapId, SwapState, SwapTxId,
};
use eframe::egui::{self, Button, ScrollArea};
use hex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::gui::util::show_btc_amount;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwapStatusFilter {
    All,
    Pending,
    WaitingConfirmations,
    ReadyToClaim,
    Completed,
    Cancelled,
}

pub struct SwapList {
    swaps: Option<Vec<Swap>>,
    selected_swap_id: Option<String>,
    l1_txid_input: String,
    l2_recipient_input: String, // L2 address that will receive the L2 amount (for L1 transaction detection)
    fetching_confirmations: bool,
    claimer_address_input: String, // L2 claimer address when claiming (for open swaps)
    last_confirmation_check: Option<Instant>,
    checking_confirmations: bool,
    success_message: Option<String>, // Success message after claiming (contains txid)
    swap_id_search: String,          // Swap ID search input field
    searched_swap: Option<Swap>,     // Swap found by search
    status_filter: SwapStatusFilter, // Filter swaps by status
    search_error: Option<String>,    // Error message for search
}

impl Default for SwapList {
    fn default() -> Self {
        Self {
            swaps: None,
            selected_swap_id: None,
            l1_txid_input: String::new(),
            l2_recipient_input: String::new(),
            fetching_confirmations: false,
            claimer_address_input: String::new(),
            last_confirmation_check: None,
            checking_confirmations: false,
            success_message: None,
            swap_id_search: String::new(),
            searched_swap: None,
            status_filter: SwapStatusFilter::All,
            search_error: None,
        }
    }
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
                        let l1_txid =
                            coinshift::types::SwapTxId::from_bytes(&vec![
                                0u8;
                                32
                            ]);
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
                            0,    // Height 0 for pending (not yet in a block)
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
        // Periodically check confirmations for swaps in WaitingConfirmations state
        // Check every 10 seconds
        let should_check = self
            .last_confirmation_check
            .map(|last| last.elapsed() >= Duration::from_secs(10))
            .unwrap_or(true);

        if should_check && !self.checking_confirmations {
            if let Some(app) = app {
                self.check_confirmations_dynamically(app);
            }
        }

        ui.horizontal(|ui| {
            ui.heading("My Swaps");
            if ui.button("Refresh").clicked() {
                if let Some(app) = app {
                    self.refresh_swaps(app);
                }
            }
            if self.checking_confirmations {
                ui.label(
                    egui::RichText::new("🔄 Checking confirmations...")
                        .small()
                        .color(egui::Color32::GRAY),
                );
            }
        });

        // Show success message if present
        if let Some(msg) = self.success_message.clone() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("✅ ")
                        .size(16.0)
                        .color(egui::Color32::GREEN),
                );
                ui.label(egui::RichText::new(msg).color(egui::Color32::GREEN));
                if ui.button("✕").clicked() {
                    self.success_message = None;
                }
            });
            ui.separator();
        }

        // Swap ID search field
        ui.horizontal(|ui| {
            ui.label("Search Swap by ID:");
            ui.text_edit_singleline(&mut self.swap_id_search);
            if ui.button("Search").clicked() {
                if let Some(app) = app {
                    self.search_swap_by_id(app);
                }
            }
            if !self.swap_id_search.is_empty() && ui.button("Clear").clicked() {
                self.swap_id_search.clear();
                self.searched_swap = None;
                self.search_error = None;
            }
        });

        // Show search error if any
        if let Some(err_msg) = &self.search_error {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("❌ Error: {}", err_msg))
                        .color(egui::Color32::RED),
                );
            });
        }

        // Show searched swap if found
        if let Some(swap) = self.searched_swap.clone() {
            ui.separator();
            ui.label(
                egui::RichText::new("Searched Swap:")
                    .heading()
                    .color(egui::Color32::BLUE),
            );
            self.show_swap_row(&swap, app, ui);
            ui.separator();
        }

        ui.separator();

        // Status filter
        ui.horizontal(|ui| {
            ui.label("Filter by Status:");
            egui::ComboBox::from_id_salt("status_filter")
                .selected_text(match self.status_filter {
                    SwapStatusFilter::All => "All",
                    SwapStatusFilter::Pending => "Pending",
                    SwapStatusFilter::WaitingConfirmations => {
                        "Waiting Confirmations"
                    }
                    SwapStatusFilter::ReadyToClaim => "Ready To Claim",
                    SwapStatusFilter::Completed => "Completed",
                    SwapStatusFilter::Cancelled => "Cancelled",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::All,
                        "All",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Pending,
                        "Pending",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::WaitingConfirmations,
                        "Waiting Confirmations",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::ReadyToClaim,
                        "Ready To Claim",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Completed,
                        "Completed",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Cancelled,
                        "Cancelled",
                    );
                });
        });

        ui.separator();

        let swaps = match &self.swaps {
            Some(swaps) => swaps,
            None => {
                ui.label("No swaps loaded. Click Refresh to load swaps.");
                return;
            }
        };

        // Filter swaps by status
        let status_filter = self.status_filter;
        let filtered_swaps: Vec<_> = swaps
            .iter()
            .filter(|swap| match status_filter {
                SwapStatusFilter::All => true,
                SwapStatusFilter::Pending => {
                    matches!(swap.state, SwapState::Pending)
                }
                SwapStatusFilter::WaitingConfirmations => {
                    matches!(swap.state, SwapState::WaitingConfirmations(..))
                }
                SwapStatusFilter::ReadyToClaim => {
                    matches!(swap.state, SwapState::ReadyToClaim)
                }
                SwapStatusFilter::Completed => {
                    matches!(swap.state, SwapState::Completed)
                }
                SwapStatusFilter::Cancelled => {
                    matches!(swap.state, SwapState::Cancelled)
                }
            })
            .cloned()
            .collect();

        if filtered_swaps.is_empty() {
            if swaps.is_empty() {
                ui.label("No swaps found.");
                ui.separator();
                ui.label("Note: Swaps need to be included in a block before they appear in the state.");
                ui.label("If you just created a swap, it may be pending in the mempool.");
                ui.label("Mine a block or wait for one to be mined to confirm your swap.");
            } else {
                ui.label(format!(
                    "No swaps found with status: {:?}",
                    status_filter
                ));
                ui.label("Try selecting a different filter or click 'All' to see all swaps.");
            }
            return;
        }

        ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("swaps_grid")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for swap in &filtered_swaps {
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
            if ui
                .selectable_label(is_selected, &swap_id_str[..16])
                .clicked()
            {
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
                    ui.label(egui::RichText::new("⚠️ IMPORTANT: For users filling this swap").heading().color(egui::Color32::YELLOW));
                    ui.label(egui::RichText::new(format!("Only send the L1 transaction to fill this swap after the swap has {} confirmations from the L2 sidechain.", swap.required_confirmations)).color(egui::Color32::YELLOW));
                    ui.label(egui::RichText::new("This ensures the swap is fully confirmed before you commit your L1 funds.").small().color(egui::Color32::GRAY));
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
                SwapState::WaitingConfirmations(current, required) => {
                    ui.separator();
                    ui.label(egui::RichText::new("⏳ Waiting for Confirmations").heading().color(egui::Color32::YELLOW));
                    ui.label(format!("Current confirmations: {}/{}", current, required));

                    if *current < *required {
                        let remaining = required - current;
                        ui.label(egui::RichText::new(format!("⚠️ Still waiting for {} more confirmation(s) before coins can be claimed", remaining)).color(egui::Color32::YELLOW));
                        ui.label(egui::RichText::new("Coins will be released when the swap is claimed after reaching the required confirmations.").small().color(egui::Color32::GRAY));
                        ui.label(egui::RichText::new("💡 Confirmations are checked automatically every 10 seconds (works in both GUI and headless mode).").small().color(egui::Color32::GRAY));
                        ui.label(egui::RichText::new("Note: State change to ReadyToClaim doesn't require a block, but claiming the swap requires a SwapClaim transaction in a block.").small().color(egui::Color32::GRAY));
                    }

                    // Show progress bar
                    let progress = *current as f32 / *required as f32;
                    ui.add(egui::ProgressBar::new(progress).show_percentage());
                }
                _ => {}
            }
        });

        ui.end_row();
    }

    fn claim_swap(
        &mut self,
        app: &App,
        swap_id: &SwapId,
        l2_claimer_address: Option<Address>,
    ) {
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

        // Get locked outputs for this swap
        // Note: We must query the node directly, not the wallet, because the wallet
        // filters out SwapPending outputs. Locked outputs are identified by checking
        // if the output content is SwapPending with the matching swap_id.
        let all_utxos = match app.node.get_all_utxos() {
            Ok(utxos) => utxos,
            Err(err) => {
                tracing::error!("Failed to get all UTXOs from node: {err:#}");
                return;
            }
        };

        // Find locked outputs for this swap (same pattern as RPC server)
        let mut locked_outputs = Vec::new();
        for (outpoint, output) in all_utxos {
            match &output.content {
                coinshift::types::OutputContent::SwapPending {
                    swap_id: locked_swap_id,
                    ..
                } => {
                    if *locked_swap_id == swap_id.0 {
                        tracing::info!(
                            "Found locked output for swap {}: {:?}",
                            swap_id,
                            outpoint
                        );
                        locked_outputs.push((outpoint, output));
                    } else {
                        tracing::debug!(
                            "Output {:?} is SwapPending for different swap_id: {:?}",
                            outpoint,
                            locked_swap_id
                        );
                    }
                }
                _ => {
                    // Not a SwapPending output, skip
                }
            }
        }

        if locked_outputs.is_empty() {
            tracing::error!("No locked outputs found for swap");
            return;
        }

        // Add locked outputs to wallet temporarily so they can be used for signing
        // SwapPending outputs are normally filtered out, but we need them in the wallet
        // for the authorize() call to find the address and signing key
        use std::collections::HashMap;
        let locked_utxos: HashMap<_, _> =
            locked_outputs.iter().cloned().collect();
        if let Err(err) = app.wallet.put_utxos(&locked_utxos) {
            tracing::error!(
                "Failed to add locked outputs to wallet for signing: {err:#}"
            );
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
        self.success_message = Some(format!(
            "Swap claimed successfully! Transaction ID: {}",
            txid
        ));
        self.refresh_swaps(app);
    }

    fn search_swap_by_id(&mut self, app: &App) {
        self.search_error = None;
        self.searched_swap = None;

        if self.swap_id_search.trim().is_empty() {
            return;
        }

        let search_input = self.swap_id_search.trim();

        // Try to find swap by partial or full ID
        if search_input.len() == 64 {
            // Full swap ID - parse and search
            let swap_id = match hex::decode(search_input) {
                Ok(bytes) => {
                    if bytes.len() == 32 {
                        let mut id_bytes = [0u8; 32];
                        id_bytes.copy_from_slice(&bytes);
                        SwapId(id_bytes)
                    } else {
                        self.search_error = Some(format!(
                            "Invalid swap ID length: expected 32 bytes, got {}",
                            bytes.len()
                        ));
                        return;
                    }
                }
                Err(err) => {
                    self.search_error =
                        Some(format!("Invalid hex format: {}", err));
                    return;
                }
            };

            // Search for full swap ID
            let rotxn = match app.node.env().read_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    self.search_error = Some(format!(
                        "Failed to get read transaction: {}",
                        err
                    ));
                    return;
                }
            };

            match app.node.state().get_swap(&rotxn, &swap_id) {
                Ok(Some(swap)) => {
                    tracing::info!("Found swap by ID: {}", swap_id);
                    self.searched_swap = Some(swap);
                    return;
                }
                Ok(None) => {
                    // Also check mempool
                    if let Ok(mempool_txs) = app.node.get_all_transactions() {
                        for tx in mempool_txs {
                            if let coinshift::types::TxData::SwapCreate {
                                swap_id: tx_swap_id,
                                parent_chain,
                                l1_txid_bytes: _,
                                required_confirmations,
                                l2_recipient,
                                l2_amount,
                                l1_recipient_address,
                                l1_amount,
                            } = &tx.transaction.data
                            {
                                if coinshift::types::SwapId(*tx_swap_id)
                                    == swap_id
                                {
                                    let l1_txid =
                                        coinshift::types::SwapTxId::from_bytes(
                                            &vec![0u8; 32],
                                        );
                                    let swap = coinshift::types::Swap::new(
                                        swap_id,
                                        coinshift::types::SwapDirection::L2ToL1,
                                        *parent_chain,
                                        l1_txid,
                                        Some(*required_confirmations),
                                        *l2_recipient,
                                        bitcoin::Amount::from_sat(*l2_amount),
                                        l1_recipient_address.clone(),
                                        l1_amount
                                            .map(bitcoin::Amount::from_sat),
                                        0, // Height 0 for pending
                                        None,
                                    );
                                    self.searched_swap = Some(swap);
                                    tracing::info!(
                                        "Found swap in mempool: {}",
                                        swap_id
                                    );
                                    return;
                                }
                            }
                        }
                    }
                    self.search_error =
                        Some(format!("Swap not found: {}", swap_id));
                    return;
                }
                Err(err) => {
                    self.search_error =
                        Some(format!("Failed to get swap: {}", err));
                    return;
                }
            }
        } else if search_input.len() < 64 {
            // Partial ID - search through all swaps to find matches
            // Try to find swaps that start with this partial ID
            let rotxn = match app.node.env().read_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    self.search_error = Some(format!(
                        "Failed to get read transaction: {}",
                        err
                    ));
                    return;
                }
            };

            let all_swaps = match app.node.state().load_all_swaps(&rotxn) {
                Ok(swaps) => swaps,
                Err(err) => {
                    self.search_error =
                        Some(format!("Failed to load swaps: {}", err));
                    return;
                }
            };

            // Try to find swap by partial hex match
            let search_lower = search_input.to_lowercase();
            let matching_swaps: Vec<_> = all_swaps
                .iter()
                .filter(|swap| {
                    let swap_id_hex = hex::encode(swap.id.0);
                    swap_id_hex.starts_with(&search_lower)
                })
                .collect();

            if matching_swaps.is_empty() {
                // Also check mempool
                if let Ok(mempool_txs) = app.node.get_all_transactions() {
                    for tx in mempool_txs {
                        if let coinshift::types::TxData::SwapCreate {
                            swap_id: tx_swap_id,
                            ..
                        } = &tx.transaction.data
                        {
                            let swap_id_hex = hex::encode(tx_swap_id);
                            if swap_id_hex.starts_with(&search_lower) {
                                // Found in mempool, but we need to construct the swap
                                // For now, just show error that full ID is needed for mempool swaps
                                self.search_error = Some("Partial ID found in mempool. Please use full 64-character swap ID for mempool swaps.".to_string());
                                return;
                            }
                        }
                    }
                }
                self.search_error = Some(format!(
                    "No swap found starting with '{}'. Please enter full 64-character swap ID.",
                    search_input
                ));
                return;
            } else if matching_swaps.len() > 1 {
                self.search_error = Some(format!(
                    "Multiple swaps found starting with '{}'. Please enter more characters to narrow down.",
                    search_input
                ));
                return;
            } else {
                // Found exactly one match
                self.searched_swap = Some(matching_swaps[0].clone());
                return;
            }
        } else {
            self.search_error = Some(format!(
                "Swap ID too long: expected 64 hex characters, got {}",
                search_input.len()
            ));
            return;
        }
    }

    fn load_rpc_config(
        &self,
        parent_chain: ParentChainType,
    ) -> Option<RpcConfig> {
        use dirs;
        use serde::{Deserialize, Serialize};
        use std::path::PathBuf;

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
            if let Ok(configs) = serde_json::from_str::<
                HashMap<ParentChainType, LocalRpcConfig>,
            >(&file_content)
            {
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

    fn fetch_confirmations_from_rpc(&mut self, _app: &App, swap: &Swap) {
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
        let swap_in_mempool = app
            .node
            .get_all_transactions()
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
            let swap_ids: Vec<String> =
                swaps.iter().map(|s| s.id.to_string()).collect();
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
                let err_str = format!("{err:#}");
                let err_debug = format!("{err:?}");
                tracing::error!(
                    swap_id = %swap.id,
                    error = %err,
                    error_debug = ?err,
                    error_display = %err_str,
                    error_debug_str = %err_debug,
                    "Failed to check if swap exists in database. This may indicate a database corruption issue."
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

        // Fetch transaction from RPC to validate amount and recipient address
        let confirmations = if let Some(rpc_config) =
            self.load_rpc_config(swap.parent_chain)
        {
            tracing::debug!(
                swap_id = %swap.id,
                l1_txid = %self.l1_txid_input,
                rpc_url = %rpc_config.url,
                "Fetching transaction from RPC for validation and confirmations"
            );

            let client = BitcoinRpcClient::new(rpc_config);
            match client.get_transaction(&self.l1_txid_input) {
                Ok(tx_info) => {
                    let conf = tx_info.confirmations;

                    // Validate transaction matches swap requirements
                    let mut found_matching_output = false;

                    // Check if swap has expected L1 recipient and amount
                    if let (Some(expected_recipient), Some(expected_amount)) =
                        (&swap.l1_recipient_address, swap.l1_amount)
                    {
                        let expected_amount_sats = expected_amount.to_sat();

                        // Check all outputs for matching address and amount
                        for vout in &tx_info.vout {
                            let vout_value_sats =
                                (vout.value * 100_000_000.0) as u64;
                            let matches_address = vout
                                .script_pub_key
                                .address
                                .as_ref()
                                .map(|addr| addr == expected_recipient)
                                .unwrap_or(false)
                                || vout
                                    .script_pub_key
                                    .addresses
                                    .as_ref()
                                    .map(|addrs| {
                                        addrs.contains(expected_recipient)
                                    })
                                    .unwrap_or(false);

                            if matches_address
                                && vout_value_sats == expected_amount_sats
                            {
                                found_matching_output = true;
                                tracing::info!(
                                    swap_id = %swap.id,
                                    l1_txid = %self.l1_txid_input,
                                    recipient = %expected_recipient,
                                    amount_sats = %expected_amount_sats,
                                    "Transaction validated: matches swap requirements"
                                );
                                break;
                            }
                        }

                        if !found_matching_output {
                            tracing::error!(
                                swap_id = %swap.id,
                                l1_txid = %self.l1_txid_input,
                                expected_recipient = %expected_recipient,
                                expected_amount_sats = %expected_amount_sats,
                                "Transaction validation failed: No output matches expected recipient address and amount"
                            );
                            return;
                        }
                    } else {
                        // For swaps without expected recipient/amount, we can't validate
                        // This might be an open swap or a swap without L1 details
                        tracing::warn!(
                            swap_id = %swap.id,
                            l1_txid = %self.l1_txid_input,
                            "Cannot validate transaction: swap missing expected L1 recipient address or amount"
                        );
                    }

                    tracing::info!(
                        swap_id = %swap.id,
                        l1_txid = %self.l1_txid_input,
                        confirmations = %conf,
                        "Fetched transaction and confirmations from L1 RPC"
                    );

                    conf
                }
                Err(err) => {
                    tracing::error!(
                        swap_id = %swap.id,
                        l1_txid = %self.l1_txid_input,
                        error = %err,
                        error_debug = ?err,
                        "Failed to fetch transaction from RPC"
                    );
                    return;
                }
            }
        } else {
            tracing::warn!(
                swap_id = %swap.id,
                "No RPC config available, cannot validate transaction. Using 0 confirmations."
            );
            // Without RPC, we can't validate, but we'll allow the update
            // (user is manually providing the txid)
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

        // Get current sidechain block hash and height for reference
        let block_hash = match app.node.state().try_get_tip(&rwtxn) {
            Ok(Some(hash)) => hash,
            Ok(None) => {
                tracing::error!("No tip found");
                return;
            }
            Err(err) => {
                tracing::error!("Failed to get tip: {err:#}");
                return;
            }
        };
        let block_height = match app.node.state().try_get_height(&rwtxn) {
            Ok(Some(height)) => height,
            Ok(None) => {
                tracing::error!("No tip height found");
                return;
            }
            Err(err) => {
                tracing::error!("Failed to get height: {err:#}");
                return;
            }
        };

        if let Err(err) = app.node.state().update_swap_l1_txid(
            &mut rwtxn,
            &swap.id,
            l1_txid,
            confirmations,
            l1_claimer_address,
            block_hash,
            block_height,
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
        let is_pending = self
            .swaps
            .as_ref()
            .and_then(|swaps| swaps.iter().find(|s| s.id == *swap_id))
            .map(|s| s.created_at_height == 0)
            .unwrap_or(false);

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
                            if let Err(err) = app.node.remove_from_mempool(txid)
                            {
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
        let is_pending = self
            .swaps
            .as_ref()
            .and_then(|swaps| swaps.iter().find(|s| s.id == *swap_id))
            .map(|s| s.created_at_height == 0)
            .unwrap_or(false);

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
                            if let Err(err) = app.node.remove_from_mempool(txid)
                            {
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

            if let Err(err) = app.node.state().delete_swap(&mut rwtxn, swap_id)
            {
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

    /// Dynamically check and update confirmations for swaps in WaitingConfirmations state
    /// This runs periodically to keep confirmation counts up-to-date even when blocks aren't being processed
    fn check_confirmations_dynamically(&mut self, app: &App) {
        if self.checking_confirmations {
            return; // Already checking
        }

        self.checking_confirmations = true;
        self.last_confirmation_check = Some(Instant::now());

        // Get swaps from database
        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get read transaction: {err:#}");
                self.checking_confirmations = false;
                return;
            }
        };

        let swaps = match app.node.state().load_all_swaps(&rotxn) {
            Ok(swaps) => swaps,
            Err(err) => {
                tracing::error!("Failed to load swaps: {err:#}");
                self.checking_confirmations = false;
                return;
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
            self.checking_confirmations = false;
            return;
        }

        tracing::debug!(
            swap_count = swaps_to_check.len(),
            "Checking confirmations for {} swaps",
            swaps_to_check.len()
        );

        let mut updated_count = 0;
        let mut rwtxn = match app.node.env().write_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get write transaction: {err:#}");
                self.checking_confirmations = false;
                return;
            }
        };

        for swap in swaps_to_check {
            // Get RPC config for this swap's parent chain
            if let Some(rpc_config) = self.load_rpc_config(swap.parent_chain) {
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
                let client = BitcoinRpcClient::new(rpc_config);
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
                                "Updating swap confirmations dynamically"
                            );

                            // Get current block info for reference
                            let block_hash = match app
                                .node
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
                            let block_height = match app
                                .node
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
                                app.node.state().update_swap_l1_txid(
                                    &mut rwtxn,
                                    &swap.id,
                                    swap.l1_txid.clone(),
                                    new_confirmations,
                                    None, // l1_claimer_address - not needed for confirmation updates
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
                    "Dynamically updated confirmations for {} swaps",
                    updated_count
                );
                // Refresh the swap list to show updated states
                self.refresh_swaps(app);
            }
        } else {
            drop(rwtxn);
        }

        self.checking_confirmations = false;
    }
}
