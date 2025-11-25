use eframe::egui::{self, Button, ScrollArea};
use coinshift::types::{Address, Swap, SwapId, SwapState, SwapTxId};

use crate::app::App;
use crate::gui::util::show_btc_amount;

#[derive(Default)]
pub struct SwapList {
    swaps: Option<Vec<Swap>>,
    selected_swap_id: Option<String>,
    l1_txid_input: String,
    confirmations_input: String,
    claimer_address_input: String,
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

        let swaps_result = app.node.state().load_all_swaps(&rotxn);
        drop(rotxn); // Release the transaction before storing swaps

        let swaps = match swaps_result {
            Ok(swaps) => swaps,
            Err(err) => {
                tracing::error!("Failed to list swaps: {err:#}");
                return;
            }
        };

        self.swaps = Some(swaps);
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

            // Action buttons based on state
            match &swap.state {
                SwapState::Pending => {
                    ui.horizontal(|ui| {
                        ui.label("Update L1 Transaction ID:");
                        ui.text_edit_singleline(&mut self.l1_txid_input);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.confirmations_input)
                                .hint_text("Confirmations")
                        );
                        if ui
                            .add_enabled(
                                app.is_some() && !self.l1_txid_input.is_empty(),
                                Button::new("Update"),
                            )
                            .clicked()
                        {
                            if let Some(app) = app {
                                let confirmations = self
                                    .confirmations_input
                                    .parse::<u32>()
                                    .unwrap_or(0);
                                let l1_txid_bytes = match hex::decode(&self.l1_txid_input) {
                                    Ok(bytes) => bytes,
                                    Err(err) => {
                                        tracing::error!("Invalid hex: {err}");
                                        return;
                                    }
                                };
                                let l1_txid = SwapTxId::from_bytes(&l1_txid_bytes);

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
                                    None,
                                ) {
                                    tracing::error!("Failed to update swap: {err:#}");
                                    return;
                                }

                                if let Err(err) = rwtxn.commit() {
                                    tracing::error!("Failed to commit: {err:#}");
                                    return;
                                }

                                self.l1_txid_input.clear();
                                self.confirmations_input.clear();
                                self.refresh_swaps(app);
                            }
                        }
                    });
                }
                SwapState::ReadyToClaim => {
                    if swap.l2_recipient.is_none() {
                        // Open swap - need claimer address
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
}

