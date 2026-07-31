use coinshift::l1::config::{
    self as l1_config, L1Auth, L1ChainConfig, L1ConfigFile,
};
use coinshift::l1::status::L1ChainHealth;
use coinshift::types::ParentChainType;
use eframe::egui::{self, Button, Color32, ComboBox, RichText, TextEdit};

use crate::app::App;

pub struct L1Config {
    selected_parent_chain: ParentChainType,
    rpc_url: String,
    rpc_user: String,
    rpc_password: String,
    configs: L1ConfigFile,
    /// Set when the config changed and the node has yet to re-read it.
    registry_reload_needed: bool,
}

impl Default for L1Config {
    fn default() -> Self {
        Self {
            selected_parent_chain: ParentChainType::Signet,
            rpc_url: String::new(),
            rpc_user: String::new(),
            rpc_password: String::new(),
            configs: L1ConfigFile::default(),
            registry_reload_needed: false,
        }
    }
}

impl L1Config {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut config = Self::default();
        config.load(ctx);
        config
    }

    fn load(&mut self, _ctx: &egui::Context) {
        self.configs =
            L1ConfigFile::load_or_default(&l1_config::default_path());
        match self.configs.get(self.selected_parent_chain) {
            Some(config) => {
                self.rpc_url = config.url.clone();
                self.rpc_user = config.auth.basic_user().to_string();
                self.rpc_password = config.auth.basic_password().to_string();
            }
            None => self.clear_fields(),
        }
    }

    /// Start from an empty form. There is no predefined endpoint to prefill any
    /// more; the hint text carries the chain's conventional URL instead.
    fn clear_fields(&mut self) {
        self.rpc_url.clear();
        self.rpc_user.clear();
        self.rpc_password.clear();
    }

    /// Persist `self.configs`, logging rather than surfacing any write failure.
    fn persist(&self) {
        let path = l1_config::default_path();
        match self.configs.save(&path) {
            Ok(()) => tracing::info!(
                path = %path.display(),
                "L1 Config: configuration persisted to file"
            ),
            Err(err) => tracing::error!(
                path = %path.display(),
                error = %err,
                "L1 Config: failed to persist configuration"
            ),
        }
    }

    fn save(&mut self, _ctx: &egui::Context) {
        // Save the user's current input fields for the selected chain, keeping
        // any non-credential settings the entry already had.
        let existing = self.configs.get(self.selected_parent_chain).cloned();
        let config = L1ChainConfig {
            url: self.rpc_url.clone(),
            auth: L1Auth::basic(
                self.rpc_user.clone(),
                self.rpc_password.clone(),
            ),
            ..existing.unwrap_or_else(|| L1ChainConfig::basic("", "", ""))
        };

        tracing::info!(
            chain = ?self.selected_parent_chain,
            url = %config.url,
            has_auth = config.auth.has_secret(),
            "L1 Config: saving configuration"
        );

        self.configs
            .insert(self.selected_parent_chain, config.clone());
        self.persist();

        // Have the node re-read the config and re-probe on the next frame,
        // rather than testing the endpoint from the GUI ourselves.
        self.registry_reload_needed = true;
    }

    fn load_selected_chain_config(&mut self) {
        match self.configs.get(self.selected_parent_chain) {
            Some(config) => {
                self.rpc_url = config.url.clone();
                self.rpc_user = config.auth.basic_user().to_string();
                self.rpc_password = config.auth.basic_password().to_string();
            }
            None => self.clear_fields(),
        }
    }
    pub fn show(
        &mut self,
        app: Option<&App>,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
    ) {
        // A save or clear happened last frame: make the node pick it up.
        if self.registry_reload_needed
            && let Some(app) = app
        {
            app.node.l1().reload();
            self.registry_reload_needed = false;
        }
        ui.heading(format!(
            "{} Node RPC Configuration",
            self.selected_parent_chain.coin_name()
        ));
        ui.separator();

        ui.label(format!(
            "Configure the RPC URL for the {} node.",
            self.selected_parent_chain.coin_name()
        ));
        ui.label("This is used for monitoring L1 transactions for swaps.");
        ui.label("Each parent chain can have its own RPC configuration.");
        ui.add_space(10.0);

        // Parent chain selection
        ui.horizontal(|ui| {
            ui.label("Parent Chain:");
            let previous_chain = self.selected_parent_chain;
            ComboBox::from_id_salt("l1_config_parent_chain")
                .selected_text(self.selected_parent_chain.display_name())
                .show_ui(ui, |ui| {
                    for chain in ParentChainType::all() {
                        ui.selectable_value(
                            &mut self.selected_parent_chain,
                            *chain,
                            chain.display_name(),
                        );
                    }
                });

            // Load config when parent chain changes
            if previous_chain != self.selected_parent_chain {
                tracing::info!(
                    from = ?previous_chain,
                    to = ?self.selected_parent_chain,
                    "L1 Config: parent chain changed"
                );
                self.load_selected_chain_config();
            }
        });

        ui.add_space(10.0);

        // Show chain-specific info
        ui.horizontal(|ui| {
            ui.label(RichText::new("Default RPC Port:").weak());
            ui.label(format!(
                "{}",
                self.selected_parent_chain.default_rpc_port()
            ));
            ui.label(RichText::new("|").weak());
            ui.label(RichText::new("Required Confirmations:").weak());
            ui.label(format!(
                "{}",
                self.selected_parent_chain.default_confirmations()
            ));
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("RPC URL:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_url)
                    .hint_text(
                        self.selected_parent_chain.default_rpc_url_hint(),
                    )
                    .desired_width(300.0),
            );
        });
        ui.label(
            RichText::new(
                "Coinshift trusts this endpoint's answers about L1 payments. \
                 Point it only at a node you control or trust.",
            )
            .small()
            .color(Color32::GRAY),
        );

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            ui.label("RPC User:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_user)
                    .hint_text("rpcuser")
                    .desired_width(300.0),
            );
        });

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            ui.label("RPC Password:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_password)
                    .hint_text("rpcpassword")
                    .password(true)
                    .desired_width(300.0),
            );
        });

        // Show current saved configuration
        if let Some(saved_config) = self.configs.get(self.selected_parent_chain)
        {
            ui.horizontal(|ui| {
                ui.label("Current saved URL:");
                use crate::gui::util::UiExt;
                ui.monospace_selectable_singleline(
                    true,
                    saved_config.url.as_str(),
                );
            });
            let saved_user = saved_config.auth.basic_user();
            if !saved_user.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Current saved User:");
                    use crate::gui::util::UiExt;
                    ui.monospace_selectable_singleline(true, saved_user);
                });
            }
        } else {
            ui.label("No RPC URL configured for this parent chain");
        }

        ui.add_space(10.0);

        // Live health, straight from the node's registry. This panel used to
        // issue its own getblockchaininfo with a hand-rolled reqwest client --
        // a fourth copy of the RPC call -- and only when a button was pressed.
        ui.separator();
        ui.label(RichText::new("Parent chain status").strong());
        if let Some(app) = app {
            egui::Grid::new("l1_status_grid")
                .num_columns(3)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Chain").strong());
                    ui.label(RichText::new("Status").strong());
                    ui.label(RichText::new("Detail").strong());
                    ui.end_row();
                    for (chain, health) in app.node.l1().statuses() {
                        let (dot, color) = health_indicator(&health);
                        ui.label(chain.display_name());
                        ui.label(RichText::new(dot).color(color));
                        ui.label(RichText::new(health.summary(chain)).small());
                        ui.end_row();
                    }
                });
            ui.label(
                RichText::new(
                    "Health is re-checked in the background; saving a change \
                     applies it without a restart.",
                )
                .small()
                .color(Color32::GRAY),
            );
        } else {
            ui.label(
                RichText::new("Node is not running yet.")
                    .small()
                    .color(Color32::GRAY),
            );
        }

        ui.add_space(10.0);

        // Validate URL
        let url_valid = url::Url::parse(&self.rpc_url).is_ok();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.rpc_url.is_empty() && url_valid,
                    Button::new("Save"),
                )
                .clicked()
            {
                self.save(ctx);
            }

            if ui.button("Clear").clicked() {
                tracing::info!(
                    chain = ?self.selected_parent_chain,
                    "L1 Config: clearing configuration"
                );
                self.rpc_url.clear();
                self.rpc_user.clear();
                self.rpc_password.clear();
                self.configs.remove(self.selected_parent_chain);
                self.persist();
                self.registry_reload_needed = true;
            }
        });

        if !self.rpc_url.is_empty() && !url_valid {
            ui.label(
                egui::RichText::new("Invalid URL format")
                    .color(egui::Color32::RED),
            );
        }

        ui.add_space(20.0);
        ui.separator();
        ui.label(egui::RichText::new("Note:").strong());
        ui.label(format!(
            "This RPC URL is used to monitor {} transactions for swaps.",
            self.selected_parent_chain.coin_name()
        ));
        ui.label(format!(
            "Make sure the {} node is running and accessible at this URL.",
            self.selected_parent_chain.coin_name()
        ));
        ui.label("Configuration is saved per parent chain and persists across sessions.");

        // Chain-specific setup hints
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Setup Hints:").strong());
        ui.label(self.selected_parent_chain.setup_hint());
    }
}

/// A coloured dot summarising a chain's health.
fn health_indicator(health: &L1ChainHealth) -> (&'static str, Color32) {
    match health {
        L1ChainHealth::Healthy { .. } => ("connected", Color32::GREEN),
        L1ChainHealth::Probing => ("checking", Color32::YELLOW),
        L1ChainHealth::Unreachable { .. } => ("unreachable", Color32::RED),
        // Not transient: this endpoint will never be used for swaps.
        L1ChainHealth::WrongChain { .. } => ("wrong network", Color32::RED),
        L1ChainHealth::Disabled => ("disabled", Color32::GRAY),
        L1ChainHealth::Unconfigured => ("not configured", Color32::GRAY),
    }
}
