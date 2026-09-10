use coinshift::parent_chain_rpc::{
    ParentChainRpcClient, btc_rpc_value_to_sats,
};
use coinshift::types::{Address, Swap, SwapId, SwapState, SwapTxId};
use eframe::egui::{self, Button, ScrollArea};

use crate::app::App;
use crate::gui::util::{show_l1_amount, show_l2_amount};

use super::list::SwapList;

#[derive(Default)]
pub struct SwapDetail {
    swap: Option<Swap>,
    l1_txid_input: String,
    l2_recipient_input: String,
    claimer_address_input: String,
    /// Hex of a borsh-encoded `L1PaymentProof`, for claiming without a
    /// configured parent-chain RPC.
    l1_proof_input: String,
    fetching_confirmations: bool,
    success_message: Option<String>,
    claim_error: Option<String>,
}

impl SwapDetail {
    pub fn set_swap(&mut self, swap: Swap) {
        let changed =
            self.swap.as_ref().map(|s| s.id != swap.id).unwrap_or(true);
        self.swap = Some(swap);
        if changed {
            self.l1_txid_input.clear();
            self.l2_recipient_input.clear();
            self.claimer_address_input.clear();
            self.l1_proof_input.clear();
            self.success_message = None;
            self.claim_error = None;
        }
    }

    pub fn show(
        &mut self,
        app: Option<&App>,
        ui: &mut egui::Ui,
        list: &mut SwapList,
    ) {
        let swap = match &self.swap {
            Some(s) => s.clone(),
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        "Select a swap from the Swap List to view details.",
                    );
                });
                return;
            }
        };

        // Refresh swap data from state if possible
        if let Some(app) = app
            && swap.created_at_height != 0
            && let Some(rotxn) = app.node.env().read_txn().ok()
            && let Ok(Some(fresh)) = app.node.state().get_swap(&rotxn, &swap.id)
        {
            self.swap = Some(fresh);
        }

        let swap = self.swap.as_ref().unwrap().clone();
        let swap_id_str = swap.id.to_string();
        let can_manage = Self::can_manage(app, &swap);

        ScrollArea::vertical().show(ui, |ui| {
            // ── header ─────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.heading("Swap Detail");
                if ui.button("<< Back to List").clicked() {
                    self.swap = None;
                }
            });

            ui.add_space(8.0);

            // ── messages ───────────────────────────────────────────
            if let Some(msg) = self.success_message.clone() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(msg).color(egui::Color32::GREEN),
                    );
                    if ui.button("X").clicked() {
                        self.success_message = None;
                    }
                });
                ui.separator();
            }
            if let Some(err_msg) = self.claim_error.clone() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(err_msg).color(egui::Color32::RED),
                    );
                    if ui.button("X").clicked() {
                        self.claim_error = None;
                    }
                });
                ui.separator();
            }

            // ── identity ───────────────────────────────────────────
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Swap ID:").strong());
                    ui.label(
                        egui::RichText::new(&swap_id_str)
                            .monospace()
                            .size(12.0),
                    );
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(swap_id_str.clone());
                    }
                });

                if swap.created_at_height == 0 {
                    ui.label(
                        egui::RichText::new(
                            "PENDING - in mempool, not yet in a block",
                        )
                        .color(egui::Color32::RED),
                    );
                }
            });

            ui.add_space(4.0);

            // ── info grid ──────────────────────────────────────────
            ui.group(|ui| {
                ui.heading("Details");
                egui::Grid::new("swap_detail_grid")
                    .num_columns(2)
                    .spacing([20.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Chain:").strong());
                        ui.label(format!("{:?}", swap.parent_chain));
                        ui.end_row();

                        ui.label(egui::RichText::new("State:").strong());
                        let (state_text, state_color) =
                            state_display(&swap.state);
                        ui.label(
                            egui::RichText::new(state_text)
                                .color(state_color)
                                .strong(),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("L2 Amount:").strong());
                        ui.label(show_l2_amount(swap.l2_amount));
                        ui.end_row();

                        ui.label(egui::RichText::new("L1 Amount:").strong());
                        ui.label(show_l1_amount(
                            swap.l1_amount,
                            swap.parent_chain,
                        ));
                        ui.end_row();

                        ui.label(egui::RichText::new("L2 Recipient:").strong());
                        if let Some(addr) = &swap.l2_recipient {
                            ui.label(addr.to_string());
                        } else {
                            ui.label("Open Swap");
                        }
                        ui.end_row();

                        ui.label(egui::RichText::new("L1 Recipient:").strong());
                        ui.label(swap.l1_recipient_address.to_string());
                        ui.end_row();

                        if let Some(addr) = &swap.l1_claimer_address {
                            ui.label(
                                egui::RichText::new("L1 Claimer:").strong(),
                            );
                            ui.label(addr.to_string());
                            ui.end_row();
                        }

                        ui.label(egui::RichText::new("L1 TxID:").strong());
                        ui.label(
                            egui::RichText::new(swap.l1_txid.to_hex())
                                .monospace()
                                .size(11.0),
                        );
                        ui.end_row();

                        ui.label(
                            egui::RichText::new("Required Conf:").strong(),
                        );
                        ui.label(format!("{}", swap.required_confirmations));
                        ui.end_row();

                        if swap.created_at_height > 0 {
                            ui.label(
                                egui::RichText::new("Created at height:")
                                    .strong(),
                            );
                            ui.label(format!("{}", swap.created_at_height));
                            ui.end_row();
                        }

                        if let Some(expires) = swap.expires_at_height {
                            ui.label(
                                egui::RichText::new("Expires at height:")
                                    .strong(),
                            );
                            ui.label(format!("{}", expires));
                            ui.end_row();
                        }

                        if let Some(addr) = &swap.l2_creator_address {
                            ui.label(egui::RichText::new("Creator:").strong());
                            ui.label(addr.to_string());
                            ui.end_row();
                        }
                    });
            });

            ui.add_space(4.0);

            // ── management buttons ─────────────────────────────────
            ui.group(|ui| {
                ui.heading("Actions");
                ui.horizontal(|ui| {
                    if matches!(swap.state, SwapState::Pending)
                        && ui
                            .add_enabled(
                                can_manage,
                                Button::new(
                                    egui::RichText::new("Cancel Swap")
                                        .color(egui::Color32::ORANGE),
                                ),
                            )
                            .clicked()
                        && let Some(app) = app
                    {
                        self.cancel_swap(app, &swap, list);
                    }

                    if matches!(
                        swap.state,
                        SwapState::Pending | SwapState::Cancelled
                    ) && ui
                        .add_enabled(
                            can_manage,
                            Button::new(
                                egui::RichText::new("Discard Swap")
                                    .color(egui::Color32::RED),
                            ),
                        )
                        .clicked()
                        && let Some(app) = app
                    {
                        self.discard_unmined_swap(app, &swap, list);
                    }

                    if !can_manage
                        && app.is_some()
                        && matches!(
                            swap.state,
                            SwapState::Pending | SwapState::Cancelled
                        )
                    {
                        ui.label(
                            egui::RichText::new(
                                "(only swap creator can manage)",
                            )
                            .small()
                            .color(egui::Color32::GRAY),
                        );
                    }
                });
            });

            ui.add_space(4.0);

            // ── state-specific actions ─────────────────────────────
            match &swap.state {
                SwapState::Pending => {
                    self.show_pending_actions(app, &swap, ui, list);
                }
                SwapState::ReadyToClaim => {
                    self.show_claim_actions(app, &swap, ui, list);
                }
                SwapState::WaitingConfirmations(current, required) => {
                    self.show_confirmation_progress(ui, *current, *required);
                }
                _ => {}
            }
        });
    }

    // ── pending state UI ───────────────────────────────────────────

    fn show_pending_actions(
        &mut self,
        app: Option<&App>,
        swap: &Swap,
        ui: &mut egui::Ui,
        list: &mut SwapList,
    ) {
        ui.group(|ui| {
            ui.heading("L1 Transaction Detection");

            ui.label(
                egui::RichText::new(format!(
                    "IMPORTANT: Only send L1 tx after the swap has {} L2 confirmations.",
                    swap.required_confirmations
                ))
                .color(egui::Color32::RED),
            );

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "When someone sends an L1 transaction to fulfill this swap, update it here.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );

            ui.add_space(8.0);

            egui::Grid::new("pending_inputs")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("L2 Address (receiver):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.l2_recipient_input)
                            .desired_width(400.0),
                    );
                    ui.end_row();

                    ui.label("L1 Transaction ID (hex):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.l1_txid_input)
                            .desired_width(400.0),
                    );
                    ui.end_row();
                });

            if self.fetching_confirmations {
                ui.label(
                    egui::RichText::new("Fetching confirmations from L1 RPC...")
                        .color(egui::Color32::from_rgb(200, 160, 90)),
                );
            }

            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        app.is_some() && !self.l1_txid_input.is_empty(),
                        Button::new("Update Swap (Auto-fetch confirmations)"),
                    )
                    .clicked()
                && let Some(app) = app {
                    self.update_swap_with_auto_confirmations(app, swap, list);
                }

                if ui
                    .add_enabled(
                        app.is_some() && !self.l1_txid_input.is_empty(),
                        Button::new("Fetch Confirmations Only"),
                    )
                    .clicked()
                && let Some(app) = app {
                    self.fetch_confirmations_from_rpc(app, swap, list);
                }
            });

            ui.label(
                egui::RichText::new(
                    "Confirmations are auto-fetched from L1 RPC if configured in L1 Config.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
        });
    }

    // ── claim actions ──────────────────────────────────────────────

    fn show_claim_actions(
        &mut self,
        app: Option<&App>,
        swap: &Swap,
        ui: &mut egui::Ui,
        list: &mut SwapList,
    ) {
        ui.group(|ui| {
            ui.heading("Claim Swap");
            ui.label(
                "The claim carries a proof of the L1 payment. The node builds \
                 it from the configured parent-chain RPC using the swap's L1 \
                 txid; without one, paste the proof (gettxoutproof + raw tx, \
                 borsh, hex) below. The escrow goes to the L2 address the \
                 payment committed to.",
            );
            if swap.l2_recipient.is_none() {
                ui.horizontal(|ui| {
                    ui.label("Claimer Address (optional check):");
                    ui.text_edit_singleline(&mut self.claimer_address_input);
                });
                if let Some(stored) = swap.l2_claimer_address {
                    ui.label(format!("Committed claimer: {stored}"));
                }
            }
            ui.horizontal(|ui| {
                ui.label("L1 proof (hex, optional):");
                ui.text_edit_singleline(&mut self.l1_proof_input);
            });
            if ui
                .add_enabled(app.is_some(), Button::new("Claim"))
                .clicked()
                && let Some(app) = app
            {
                let requested = if self.claimer_address_input.trim().is_empty()
                {
                    None
                } else {
                    match self.claimer_address_input.trim().parse::<Address>() {
                        Ok(addr) => Some(addr),
                        Err(err) => {
                            self.claim_error =
                                Some(format!("Invalid address: {err}"));
                            return;
                        }
                    }
                };
                let proof = if self.l1_proof_input.trim().is_empty() {
                    None
                } else {
                    match hex::decode(self.l1_proof_input.trim()) {
                        Ok(bytes) => Some(bytes),
                        Err(err) => {
                            self.claim_error =
                                Some(format!("Invalid proof hex: {err}"));
                            return;
                        }
                    }
                };
                self.claim_swap(app, &swap.id, requested, proof, list);
            }
        });
    }

    // ── confirmation progress ──────────────────────────────────────

    fn show_confirmation_progress(
        &self,
        ui: &mut egui::Ui,
        current: u32,
        required: u32,
    ) {
        ui.group(|ui| {
            ui.heading("Waiting for Confirmations");

            ui.label(format!(
                "Current confirmations: {}/{}",
                current, required
            ));

            let progress = current as f32 / required as f32;
            ui.add(egui::ProgressBar::new(progress).show_percentage());

            if current < required {
                let remaining = required - current;
                ui.label(
                    egui::RichText::new(format!(
                        "Waiting for {} more confirmation(s)",
                        remaining
                    ))
                    .color(egui::Color32::from_rgb(200, 160, 90)),
                );
            }

            ui.label(
                egui::RichText::new(
                    "Confirmations are checked automatically every 10 seconds.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
        });
    }

    // ── swap operations ────────────────────────────────────────────

    fn can_manage(app: Option<&App>, swap: &Swap) -> bool {
        let Some(app) = app else { return false };
        if swap.created_at_height == 0 {
            app.node.is_created_pending_swap(&swap.id)
        } else {
            match &swap.l2_creator_address {
                Some(creator) => app
                    .wallet
                    .get_addresses()
                    .map(|addrs| addrs.iter().any(|a| a == creator))
                    .unwrap_or(false),
                None => false,
            }
        }
    }

    fn claim_swap(
        &mut self,
        app: &App,
        swap_id: &SwapId,
        l2_claimer_address: Option<Address>,
        l1_proof: Option<Vec<u8>>,
        list: &mut SwapList,
    ) {
        let tx = match app.build_swap_claim(
            *swap_id,
            l2_claimer_address,
            l1_proof,
        ) {
            Ok(tx) => tx,
            Err(err) => {
                self.claim_error =
                    Some(format!("Failed to build claim: {err:#}"));
                return;
            }
        };

        let txid = tx.txid();
        if let Err(err) = app.sign_and_send(tx) {
            self.claim_error =
                Some(format!("Failed to send transaction: {err:#}"));
            return;
        }

        self.claimer_address_input.clear();
        self.l1_proof_input.clear();
        self.claim_error = None;
        self.success_message = Some(format!("Swap claimed! TxID: {}", txid));
        list.refresh_swaps(app);
    }

    fn cancel_swap(&mut self, app: &App, swap: &Swap, list: &mut SwapList) {
        let swap_id = swap.id;

        if swap.created_at_height == 0 {
            if !app.node.is_created_pending_swap(&swap_id) {
                self.claim_error =
                    Some("Only the swap creator can cancel".into());
                return;
            }
            if let Ok(mempool_txs) = app.node.get_all_transactions() {
                for tx in mempool_txs {
                    if let coinshift::types::TxData::SwapCreate {
                        swap_id: tx_swap_id,
                        ..
                    } = &tx.transaction.data
                        && coinshift::types::SwapId(*tx_swap_id) == swap_id
                    {
                        let txid = tx.transaction.txid();
                        if let Err(err) = app.node.remove_from_mempool(txid) {
                            self.claim_error = Some(format!(
                                "Failed to remove from mempool: {err:#}"
                            ));
                            return;
                        }
                        app.node.remove_created_pending_swap(&swap_id);
                        self.success_message =
                            Some("Pending swap cancelled".into());
                        list.refresh_swaps(app);
                        self.swap = None;
                        return;
                    }
                }
            }
            self.claim_error = Some("Pending swap not found in mempool".into());
        } else {
            let creator = swap.l2_creator_address.as_ref();
            let mut rwtxn = match app.node.env().write_txn() {
                Ok(txn) => txn,
                Err(err) => {
                    self.claim_error =
                        Some(format!("Failed to get write txn: {err:#}"));
                    return;
                }
            };

            if let Err(err) =
                app.node.state().cancel_swap(&mut rwtxn, &swap_id, creator)
            {
                self.claim_error = Some(format!("Failed to cancel: {err:#}"));
                return;
            }

            if let Err(err) = rwtxn.commit() {
                self.claim_error = Some(format!("Failed to commit: {err:#}"));
                return;
            }

            self.success_message = Some("Swap cancelled".into());
            list.refresh_swaps(app);
        }
    }

    /// Discard a swap that is still only in this node's mempool.
    ///
    /// A swap that has made it into a block cannot be removed: it is consensus
    /// state derived from the chain, and dropping it locally would make this
    /// node reject blocks containing a claim on it. Such a swap can only be
    /// cancelled while Pending, or left to expire.
    fn discard_unmined_swap(
        &mut self,
        app: &App,
        swap: &Swap,
        list: &mut SwapList,
    ) {
        let swap_id = swap.id;

        if swap.created_at_height != 0 {
            self.claim_error = Some(
                "Swap is already in a block and cannot be discarded; cancel it \
                 while Pending, or let it expire"
                    .into(),
            );
            return;
        }

        if !app.node.is_created_pending_swap(&swap_id) {
            self.claim_error =
                Some("Only the swap creator can discard it".into());
            return;
        }
        if let Ok(mempool_txs) = app.node.get_all_transactions() {
            for tx in mempool_txs {
                if let coinshift::types::TxData::SwapCreate {
                    swap_id: tx_swap_id,
                    ..
                } = &tx.transaction.data
                    && coinshift::types::SwapId(*tx_swap_id) == swap_id
                {
                    let txid = tx.transaction.txid();
                    if let Err(err) = app.node.remove_from_mempool(txid) {
                        self.claim_error = Some(format!(
                            "Failed to remove from mempool: {err:#}"
                        ));
                        return;
                    }
                    app.node.remove_created_pending_swap(&swap_id);
                    self.success_message =
                        Some("Pending swap discarded".into());
                    list.refresh_swaps(app);
                    self.swap = None;
                    return;
                }
            }
        }
        self.claim_error = Some("Pending swap not found in mempool".into());
    }

    fn fetch_confirmations_from_rpc(
        &mut self,
        _app: &App,
        swap: &Swap,
        list: &SwapList,
    ) {
        if self.l1_txid_input.is_empty() {
            return;
        }

        if let Some(rpc_config) = list.load_rpc_config(swap.parent_chain) {
            self.fetching_confirmations = true;
            let txid_hex = self.l1_txid_input.clone();
            let txid_for_rpc = SwapTxId::from_hex(&txid_hex)
                .map(|t| t.to_hex())
                .or_else(|_| {
                    SwapTxId::from_hex_rpc(&txid_hex).map(|t| t.to_hex())
                })
                .unwrap_or(txid_hex);

            let client = ParentChainRpcClient::new(rpc_config);
            match client.get_transaction_confirmations(&txid_for_rpc) {
                Ok(confirmations) => {
                    tracing::info!(
                        swap_id = %swap.id,
                        confirmations = %confirmations,
                        "Fetched confirmations"
                    );
                }
                Err(err) => {
                    tracing::error!(
                        swap_id = %swap.id,
                        error = %err,
                        "Failed to fetch confirmations"
                    );
                }
            }
            self.fetching_confirmations = false;
        }
    }

    fn update_swap_with_auto_confirmations(
        &mut self,
        app: &App,
        swap: &Swap,
        list: &mut SwapList,
    ) {
        if self.l1_txid_input.is_empty() {
            return;
        }

        // Check if swap is in mempool (pending, not in a block yet)
        if swap.created_at_height == 0 {
            self.claim_error = Some(
                "Cannot update pending swap. Must be confirmed in a block first."
                    .into(),
            );
            return;
        }

        // Verify swap exists in database
        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                self.claim_error = Some(format!("Failed to read: {err:#}"));
                return;
            }
        };

        let swap_exists =
            matches!(app.node.state().get_swap(&rotxn, &swap.id), Ok(Some(_)));
        drop(rotxn);

        if !swap_exists {
            self.claim_error = Some("Swap not found in database".into());
            return;
        }

        let l1_txid = match SwapTxId::from_hex(&self.l1_txid_input) {
            Ok(txid) => txid,
            Err(err) => {
                self.claim_error =
                    Some(format!("Invalid L1 txid (64 hex chars): {err}"));
                return;
            }
        };

        let l1_txid_hex = l1_txid.to_hex();

        // Fetch and validate from RPC
        let confirmations = if let Some(rpc_config) =
            list.load_rpc_config(swap.parent_chain)
        {
            let client = ParentChainRpcClient::new(rpc_config);
            match client.get_transaction(&l1_txid_hex) {
                Ok(tx_info) => {
                    let conf = tx_info.confirmations;

                    // Validate outputs
                    let found = tx_info.vout.iter().any(|vout| {
                        let Some(sats) = btc_rpc_value_to_sats(vout.value)
                        else {
                            return false;
                        };
                        let addr_match = vout
                            .script_pub_key
                            .address
                            .as_ref()
                            .map(|a| a == &swap.l1_recipient_address)
                            .unwrap_or(false)
                            || vout
                                .script_pub_key
                                .addresses
                                .as_ref()
                                .map(|addrs| {
                                    addrs.contains(&swap.l1_recipient_address)
                                })
                                .unwrap_or(false);
                        addr_match && sats == swap.l1_amount.to_sat()
                    });

                    if !found {
                        self.claim_error = Some(
                            "L1 tx doesn't match expected recipient/amount"
                                .into(),
                        );
                        return;
                    }

                    conf
                }
                Err(err) => {
                    self.claim_error =
                        Some(format!("Failed to fetch from RPC: {err:#}"));
                    return;
                }
            }
        } else {
            0
        };

        let l1_claimer_address = None;

        let l2_claimer_address = if swap.l2_recipient.is_none() {
            let s = self.l2_recipient_input.trim();
            if s.is_empty() {
                self.claim_error = Some(
                    "Open swap requires L2 address when updating with L1 txid"
                        .into(),
                );
                return;
            }
            match s.parse() {
                Ok(addr) => Some(addr),
                Err(err) => {
                    self.claim_error =
                        Some(format!("Invalid L2 address: {err}"));
                    return;
                }
            }
        } else {
            None
        };

        let mut rwtxn = match app.node.env().write_txn() {
            Ok(txn) => txn,
            Err(err) => {
                self.claim_error =
                    Some(format!("Failed to get write txn: {err:#}"));
                return;
            }
        };

        let block_hash = match app.node.state().try_get_tip(&rwtxn) {
            Ok(Some(hash)) => hash,
            _ => {
                self.claim_error = Some("No tip found".into());
                return;
            }
        };
        let block_height = match app.node.state().try_get_height(&rwtxn) {
            Ok(Some(h)) => h,
            _ => {
                self.claim_error = Some("No tip height found".into());
                return;
            }
        };

        if let Err(err) = app.node.state().update_swap_l1_txid(
            &mut rwtxn,
            &swap.id,
            l1_txid,
            confirmations,
            l1_claimer_address,
            l2_claimer_address,
            block_hash,
            block_height,
        ) {
            self.claim_error = Some(format!("Failed to update swap: {err:#}"));
            return;
        }

        if let Err(err) = rwtxn.commit() {
            self.claim_error = Some(format!("Failed to commit: {err:#}"));
            return;
        }

        self.l1_txid_input.clear();
        self.l2_recipient_input.clear();
        self.success_message = Some(format!(
            "Swap updated with L1 txid ({} confirmations)",
            confirmations
        ));
        list.refresh_swaps(app);
    }
}

fn state_display(state: &SwapState) -> (String, egui::Color32) {
    match state {
        SwapState::Pending => {
            ("Pending".into(), egui::Color32::from_rgb(130, 170, 255))
        }
        SwapState::WaitingConfirmations(cur, req) => (
            format!("Waiting {}/{}", cur, req),
            egui::Color32::from_rgb(255, 180, 100),
        ),
        SwapState::ReadyToClaim => (
            "Ready to Claim".into(),
            egui::Color32::from_rgb(100, 220, 100),
        ),
        SwapState::Completed => {
            ("Completed".into(), egui::Color32::from_rgb(140, 200, 140))
        }
        SwapState::Cancelled => {
            ("Cancelled".into(), egui::Color32::from_rgb(150, 150, 150))
        }
    }
}
