use eframe::egui::{self, Button, ComboBox};
use coinshift::types::{Address, ParentChainType};

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
                    ui.selectable_value(&mut self.parent_chain, ParentChainType::BTC, "BTC");
                    ui.selectable_value(&mut self.parent_chain, ParentChainType::Signet, "Signet");
                    ui.selectable_value(&mut self.parent_chain, ParentChainType::Regtest, "Regtest");
                    ui.selectable_value(&mut self.parent_chain, ParentChainType::BCH, "BCH");
                    ui.selectable_value(&mut self.parent_chain, ParentChainType::LTC, "LTC");
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
                                tracing::error!("Failed to get address: {err:#}");
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
            self.l2_recipient
                .as_ref()
                .and_then(|s| s.parse().ok())
        };

        let is_valid = app.is_some()
            && !self.l1_recipient_address.is_empty()
            && l1_amount.is_ok()
            && l2_amount.is_ok()
            && (!self.is_open_swap && l2_recipient.is_some() || self.is_open_swap);

        if ui
            .add_enabled(is_valid, Button::new("Create Swap"))
            .clicked()
        {
            let app = app.unwrap();
            let accumulator = match app.node.get_tip_accumulator() {
                Ok(acc) => acc,
                Err(err) => {
                    tracing::error!("Failed to get accumulator: {err:#}");
                    return;
                }
            };

            let (tx, swap_id) = match app.wallet.create_swap_create_tx(
                &accumulator,
                self.parent_chain,
                self.l1_recipient_address.clone(),
                l1_amount.expect("should not happen"),
                l2_recipient,
                l2_amount.expect("should not happen"),
                required_confirmations,
                bitcoin::Amount::ZERO,
            ) {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!("Failed to create swap: {err:#}");
                    return;
                }
            };

            let txid = tx.txid();
            if let Err(err) = app.sign_and_send(tx) {
                tracing::error!("Failed to send transaction: {err:#}");
                return;
            }

            tracing::info!("Swap created: swap_id={}, txid={}", swap_id, txid);
            *self = Self::default();
            self.parent_chain = ParentChainType::BTC; // Keep parent chain selection
        }
    }
}

