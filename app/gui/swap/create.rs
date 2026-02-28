use coinshift::parent_chain_rpc;
use coinshift::types::{Address, ParentChainType};
use eframe::egui::{self, Button, Color32, ComboBox, RichText, TextEdit};

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
        let supported = parent_chain_rpc::supported_l1_parent_chain_types();
        let first = supported
            .first()
            .copied()
            .unwrap_or(ParentChainType::Signet);
        Self {
            parent_chain: first,
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
        ui.add_space(4.0);
        ui.label(
            RichText::new("You offer L2 (sidechain) coins and request L1. Set where you want to receive L1, then who gets your L2 and the amounts.")
                .small()
                .color(Color32::GRAY),
        );
        ui.separator();

        // ─── What you want (L1) ─────────────────────────────────────────────
        ui.add_space(4.0);
        ui.label(RichText::new("What you want (L1)").strong());

        ui.horizontal(|ui| {
            ui.label("Parent chain:");
            let supported = parent_chain_rpc::supported_l1_parent_chain_types();
            let label = match self.parent_chain {
                ParentChainType::Signet => "Bitcoin Signet (sBTC)",
                ParentChainType::BCH => "Bitcoin Cash Testnet 4 (BCH)",
                _ => "Select network",
            };
            ComboBox::from_id_salt("parent_chain")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for chain in supported {
                        let option_label = match chain {
                            ParentChainType::Signet => "Bitcoin Signet (sBTC)",
                            ParentChainType::BCH => {
                                "Bitcoin Cash Testnet 4 (BCH)"
                            }
                            _ => continue,
                        };
                        ui.selectable_value(
                            &mut self.parent_chain,
                            *chain,
                            option_label,
                        );
                    }
                });
        });

        ui.horizontal(|ui| {
            ui.label("Your L1 address:");
            ui.add(
                TextEdit::singleline(&mut self.l1_recipient_address)
                    .hint_text("Where you receive the L1 coins (e.g. bc1...)"),
            );
        });

        ui.horizontal(|ui| {
            ui.label(format!(
                "Amount you want ({})",
                self.parent_chain.ticker()
            ));
            ui.add(
                TextEdit::singleline(&mut self.l1_amount)
                    .hint_text("e.g. 0.001"),
            );
        });

        ui.add_space(8.0);
        ui.label(RichText::new("What you offer (L2)").strong());

        ui.checkbox(
            &mut self.is_open_swap,
            "Open swap — anyone can fill (no specific claimer)",
        );

        if !self.is_open_swap {
            ui.horizontal(|ui| {
                ui.label("L2 claimer address:");
                ui.add(
                    TextEdit::singleline(
                        self.l2_recipient.get_or_insert_with(String::new),
                    )
                    .hint_text("Who can claim (sends L1, then gets L2)"),
                );
                if ui.button("Use My Address").clicked()
                    && let Some(app) = app
                {
                    match app.wallet.get_new_address() {
                        Ok(addr) => {
                            self.l2_recipient = Some(addr.to_string());
                        }
                        Err(err) => {
                            tracing::error!("Failed to get address: {err:#}");
                        }
                    }
                }
            });
        } else {
            self.l2_recipient = None;
        }

        ui.horizontal(|ui| {
            ui.label("L2 amount you offer:");
            ui.add(
                TextEdit::singleline(&mut self.l2_amount)
                    .hint_text("e.g. 0.001"),
            );
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Options").strong());

        ui.horizontal(|ui| {
            ui.label("Required L1 confirmations:");
            ui.add(
                TextEdit::singleline(&mut self.required_confirmations)
                    .hint_text("leave empty for default"),
            );
            ui.label(format!(
                "(default: {})",
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
            && (l2_recipient.is_some() || self.is_open_swap)
            && l2_amount.is_ok()
            && l1_amount.is_ok()
            && !self.l1_recipient_address.is_empty();

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
