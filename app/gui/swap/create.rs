use coinshift::types::{Address, ParentChainType};
use eframe::egui::{self, Button, Color32, ComboBox, RichText};

use crate::app::App;

#[derive(Debug)]
pub struct CreateSwap {
    parent_chain: ParentChainType,
    l1_recipient_address: String,
    l1_amount: String,
    l2_recipient: Option<String>,
    l2_amount: String,
    required_confirmations: String,
    is_open_swap: bool,
    error_message: Option<String>,
}

impl Default for CreateSwap {
    fn default() -> Self {
        Self {
            parent_chain: ParentChainType::BTC,
            l1_recipient_address: String::new(),
            l1_amount: String::new(),
            l2_recipient: None,
            l2_amount: String::new(),
            required_confirmations: String::new(),
            is_open_swap: false,
            error_message: None,
        }
    }
}

impl CreateSwap {
    pub fn show(&mut self, app: Option<&App>, ui: &mut egui::Ui) {
        ui.heading("Create Swap (L2 → L1)");
        ui.separator();

        // Parent chain selection
        ui.horizontal(|ui| {
            ui.label("Parent Chain:");
            ComboBox::from_id_salt("parent_chain")
                .selected_text(format!("{:?}", self.parent_chain))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.parent_chain,
                        ParentChainType::BTC,
                        "BTC",
                    );
                    ui.selectable_value(
                        &mut self.parent_chain,
                        ParentChainType::Signet,
                        "Signet",
                    );
                    ui.selectable_value(
                        &mut self.parent_chain,
                        ParentChainType::Regtest,
                        "Regtest",
                    );
                    ui.selectable_value(
                        &mut self.parent_chain,
                        ParentChainType::BCH,
                        "BCH",
                    );
                    ui.selectable_value(
                        &mut self.parent_chain,
                        ParentChainType::LTC,
                        "LTC",
                    );
                });
        });

        // L1 recipient address
        ui.horizontal(|ui| {
            ui.label("L1 Recipient Address:");
            ui.text_edit_singleline(&mut self.l1_recipient_address);
        });

        // L1 amount
        ui.horizontal(|ui| {
            ui.label("L1 Amount (BTC):");
            ui.text_edit_singleline(&mut self.l1_amount);
        });

        // Open swap checkbox
        ui.checkbox(&mut self.is_open_swap, "Open Swap (anyone can fill)");

        // L2 recipient (only if not open swap)
        if !self.is_open_swap {
            ui.horizontal(|ui| {
                ui.label("L2 Recipient Address:");
                ui.text_edit_singleline(
                    self.l2_recipient.get_or_insert_with(String::new),
                );
                if ui.button("Use My Address").clicked() {
                    if let Some(app) = app {
                        match app.wallet.get_new_address() {
                            Ok(addr) => {
                                self.l2_recipient = Some(addr.to_string());
                            }
                            Err(err) => {
                                tracing::error!(
                                    "Failed to get address: {err:#}"
                                );
                            }
                        }
                    }
                }
            });
        } else {
            self.l2_recipient = None;
        }

        // L2 amount
        ui.horizontal(|ui| {
            ui.label("L2 Amount (BTC):");
            ui.text_edit_singleline(&mut self.l2_amount);
        });

        // Required confirmations
        ui.horizontal(|ui| {
            ui.label("Required Confirmations:");
            ui.text_edit_singleline(&mut self.required_confirmations);
            ui.label(format!(
                "(Default: {})",
                self.parent_chain.default_confirmations()
            ));
        });

        ui.separator();

        // Display error message if any
        if let Some(error_msg) = &self.error_message {
            ui.add_space(5.0);
            ui.label(
                RichText::new(format!("Error: {}", error_msg))
                    .small()
                    .color(Color32::RED),
            );
            ui.separator();
        }

        // Parse inputs
        let l1_amount = bitcoin::Amount::from_str_in(
            &self.l1_amount,
            bitcoin::Denomination::Bitcoin,
        );
        let l2_amount = bitcoin::Amount::from_str_in(
            &self.l2_amount,
            bitcoin::Denomination::Bitcoin,
        );
        let required_confirmations = self
            .required_confirmations
            .parse::<u32>()
            .ok()
            .or_else(|| Some(self.parent_chain.default_confirmations()));

        let l2_recipient: Option<Address> = if self.is_open_swap {
            None
        } else {
            self.l2_recipient.as_ref().and_then(|s| s.parse().ok())
        };

        let is_valid = app.is_some()
            && !self.l1_recipient_address.is_empty()
            && l1_amount.is_ok()
            && l2_amount.is_ok()
            && (!self.is_open_swap && l2_recipient.is_some()
                || self.is_open_swap);

        if ui
            .add_enabled(is_valid, Button::new("Create Swap"))
            .clicked()
        {
            // Clear any previous error
            self.error_message = None;

            let app = app.unwrap();
            let accumulator = match app.node.get_tip_accumulator() {
                Ok(acc) => acc,
                Err(err) => {
                    let error_msg =
                        format!("Failed to get accumulator: {err:#}");
                    tracing::error!("{}", error_msg);
                    self.error_message = Some(error_msg);
                    return;
                }
            };

            // Extract amounts for logging (before they're moved)
            let l1_amount_val = l1_amount.expect("should not happen");
            let l2_amount_val = l2_amount.expect("should not happen");

            // Create a closure that checks if an outpoint is locked to a swap
            // We create a new read transaction each time to avoid lifetime issues
            let node = &app.node;
            let is_locked = |outpoint: &coinshift::types::OutPoint| -> bool {
                let rotxn = match node.env().read_txn() {
                    Ok(txn) => txn,
                    Err(_) => return false,
                };
                let state = node.state();
                state
                    .is_output_locked_to_swap(&rotxn, outpoint)
                    .map(|opt| opt.is_some())
                    .unwrap_or(false)
            };

            let (tx, swap_id) = match app.wallet.create_swap_create_tx(
                &accumulator,
                self.parent_chain,
                self.l1_recipient_address.clone(),
                l1_amount_val,
                l2_recipient,
                l2_amount_val,
                required_confirmations,
                bitcoin::Amount::ZERO,
                is_locked,
            ) {
                Ok(result) => {
                    let txid = result.0.txid();
                    tracing::info!(
                        swap_id = %result.1,
                        txid = %txid,
                        parent_chain = ?self.parent_chain,
                        l1_recipient = %self.l1_recipient_address,
                        l1_amount = %l1_amount_val,
                        l2_recipient = ?l2_recipient,
                        l2_amount = %l2_amount_val,
                        required_confirmations = ?required_confirmations,
                        is_open_swap = %self.is_open_swap,
                        "Successfully created swap transaction"
                    );
                    result
                }
                Err(err) => {
                    let error_msg =
                        format!("Failed to create swap transaction: {err:#}");
                    tracing::error!(
                        parent_chain = ?self.parent_chain,
                        l1_recipient = %self.l1_recipient_address,
                        l1_amount = %l1_amount_val,
                        l2_recipient = ?l2_recipient,
                        l2_amount = %l2_amount_val,
                        required_confirmations = ?required_confirmations,
                        is_open_swap = %self.is_open_swap,
                        error = %err,
                        error_debug = ?err,
                        "Failed to create swap transaction"
                    );
                    self.error_message = Some(error_msg);
                    return;
                }
            };

            let txid = tx.txid();
            tracing::info!(
                swap_id = %swap_id,
                txid = %txid,
                "Attempting to sign and send swap transaction"
            );
            if let Err(err) = app.sign_and_send(tx) {
                let error_msg = format!("Failed to send transaction: {err:#}");
                tracing::error!(
                    swap_id = %swap_id,
                    txid = %txid,
                    error = %err,
                    error_debug = ?err,
                    "Failed to send transaction: node error"
                );
                self.error_message = Some(error_msg);
                return;
            }

            tracing::info!("Swap created: swap_id={}, txid={}", swap_id, txid);
            *self = Self::default();
            self.parent_chain = ParentChainType::BTC; // Keep parent chain selection
        }
    }
}
