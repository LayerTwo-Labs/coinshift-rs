use std::sync::{Arc, Mutex};

use coinshift::l1::config::{
    self as l1_config, L1Auth, L1ChainConfig, L1ConfigFile,
};
use coinshift::parent_chain_rpc;
use coinshift::types::ParentChainType;
use eframe::egui::{self, Button, Color32, ComboBox, RichText, TextEdit};
use poll_promise::Promise;
use serde_json::json;

#[derive(Clone)]
enum ConnectionStatus {
    Unknown,
    Connected { block_height: u64 },
    Disconnected { error: String },
    Checking,
}

pub struct L1Config {
    selected_parent_chain: ParentChainType,
    rpc_url: String,
    rpc_user: String,
    rpc_password: String,
    configs: L1ConfigFile,
    connection_status: Arc<Mutex<ConnectionStatus>>,
    status_promise: Option<Promise<anyhow::Result<u64>>>,
}

impl Default for L1Config {
    fn default() -> Self {
        let supported = parent_chain_rpc::supported_l1_parent_chain_types();
        let first = supported
            .first()
            .copied()
            .unwrap_or(ParentChainType::Signet);
        Self {
            selected_parent_chain: first,
            rpc_url: String::new(),
            rpc_user: String::new(),
            rpc_password: String::new(),
            configs: L1ConfigFile::default(),
            connection_status: Arc::new(Mutex::new(ConnectionStatus::Unknown)),
            status_promise: None,
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
            None => self.load_predefined_for_selected(),
        }
    }

    /// Fill URL/user/password from the predefined config for the selected chain.
    fn load_predefined_for_selected(&mut self) {
        match parent_chain_rpc::supported_l1_configs()
            .into_iter()
            .find(|(chain, _)| *chain == self.selected_parent_chain)
        {
            Some((_, config)) => {
                self.rpc_url = config.url;
                self.rpc_user = config.auth.basic_user().to_string();
                self.rpc_password = config.auth.basic_password().to_string();
            }
            None => {
                self.rpc_url.clear();
                self.rpc_user.clear();
                self.rpc_password.clear();
            }
        }
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

        // Auto-check connection when saving
        if !config.url.is_empty() {
            let user = config.auth.basic_user().to_string();
            let password = config.auth.basic_password().to_string();
            self.check_connection(&config.url, &user, &password);
        }
    }

    fn load_selected_chain_config(&mut self) {
        self.load_predefined_for_selected();
        // Reset connection status when switching chains
        *self.connection_status.lock().unwrap() = ConnectionStatus::Unknown;
        self.status_promise = None;
    }

    fn check_connection(&mut self, url: &str, user: &str, password: &str) {
        if url.is_empty() {
            return;
        }

        tracing::info!(
            url = %url,
            has_auth = !user.is_empty(),
            "L1 Config: testing connection"
        );

        let url = url.to_string();
        let user = user.to_string();
        let password = password.to_string();
        let status = self.connection_status.clone();

        *status.lock().unwrap() = ConnectionStatus::Checking;

        let promise = Promise::spawn_thread("l1_rpc_check", move || {
            Self::fetch_block_height(&url, &user, &password)
        });

        self.status_promise = Some(promise);
    }

    fn fetch_block_height(
        url: &str,
        user: &str,
        password: &str,
    ) -> anyhow::Result<u64> {
        use std::time::Duration;

        // Use jsonrpc "1.0" to match nodes that accept curl-style requests (e.g. BCH test4)
        let request = json!({
            "jsonrpc": "1.0",
            "id": "coinshift",
            "method": "getblockchaininfo",
            "params": []
        });

        tracing::info!(
            url = %url,
            request = %serde_json::to_string(&request).unwrap_or_default(),
            "L1 Config: connection test request"
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        let mut request_builder = client.post(url).json(&request);

        // Add HTTP basic authentication if user and password are provided
        if !user.is_empty() {
            request_builder = request_builder.basic_auth(user, Some(password));
        }

        let response = request_builder.send()?;
        let status = response.status();
        let json: serde_json::Value = response.json()?;

        tracing::info!(
            status = %status,
            response = %serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string()),
            "L1 Config: connection test response"
        );

        if let Some(error) = json.get("error")
            && !error.is_null()
        {
            anyhow::bail!("RPC error: {}", error);
        }

        let result = json
            .get("result")
            .ok_or_else(|| anyhow::anyhow!("No result in response"))?;

        let blocks = result
            .get("blocks")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("No blocks field in response"))?;

        tracing::info!(block_height = blocks, "L1 Config: connection test OK");
        Ok(blocks)
    }

    fn update_status(&mut self) {
        if let Some(promise) = &self.status_promise
            && let Some(result) = promise.ready()
        {
            match result {
                Ok(block_height) => {
                    tracing::info!(
                        block_height = block_height,
                        "L1 Config: connection test succeeded"
                    );
                    *self.connection_status.lock().unwrap() =
                        ConnectionStatus::Connected {
                            block_height: *block_height,
                        };
                }
                Err(err) => {
                    tracing::info!(error = %err, "L1 Config: connection test failed");
                    *self.connection_status.lock().unwrap() =
                        ConnectionStatus::Disconnected {
                            error: format!("{err:#}"),
                        };
                }
            }
            self.status_promise = None;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
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

        // Parent chain selection (only supported options)
        ui.horizontal(|ui| {
            ui.label("Parent Chain:");
            let previous_chain = self.selected_parent_chain;
            let supported = parent_chain_rpc::supported_l1_parent_chain_types();
            ComboBox::from_id_salt("l1_config_parent_chain")
                .selected_text(self.selected_parent_chain.display_name())
                .show_ui(ui, |ui| {
                    for chain in supported {
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
            ui.add_enabled(
                false,
                TextEdit::singleline(&mut self.rpc_url)
                    .hint_text(
                        self.selected_parent_chain.default_rpc_url_hint(),
                    )
                    .desired_width(300.0),
            );
        });
        ui.label(
            RichText::new("Only predefined networks are supported. URL cannot be changed.")
                .small()
                .color(Color32::GRAY),
        );

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            ui.label("RPC User:");
            ui.add_enabled(
                false,
                TextEdit::singleline(&mut self.rpc_user)
                    .hint_text("rpcuser")
                    .desired_width(300.0),
            );
        });

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            ui.label("RPC Password:");
            ui.add_enabled(
                false,
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

        // Connection status
        self.update_status();

        let status = {
            let lock = self.connection_status.lock().unwrap();
            lock.clone()
        };

        match status {
            ConnectionStatus::Unknown => {
                // Allow check using current URL (predefined when chain selected) even if not saved yet
                if !self.rpc_url.is_empty() {
                    let url = self.rpc_url.clone();
                    let user = self.rpc_user.clone();
                    let password = self.rpc_password.clone();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(Color32::GRAY));
                        ui.label("Status: Unknown");
                        if ui.button("Check Connection").clicked() {
                            self.check_connection(&url, &user, &password);
                        }
                    });
                }
            }
            ConnectionStatus::Checking => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::YELLOW));
                    ui.label(
                        RichText::new("Checking connection...")
                            .color(Color32::YELLOW),
                    );
                });
            }
            ConnectionStatus::Connected { block_height } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::GREEN));
                    ui.label(
                        RichText::new("Connected")
                            .color(Color32::GREEN)
                            .strong(),
                    );
                    ui.label(format!("Latest Block Height: {}", block_height));
                });
                if !self.rpc_url.is_empty() {
                    let url = self.rpc_url.clone();
                    let user = self.rpc_user.clone();
                    let password = self.rpc_password.clone();
                    if ui.button("Refresh").clicked() {
                        self.check_connection(&url, &user, &password);
                    }
                }
            }
            ConnectionStatus::Disconnected { error } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::RED));
                    ui.label(
                        RichText::new("Disconnected")
                            .color(Color32::RED)
                            .strong(),
                    );
                });
                let error_msg = format!("Error: {}", error);
                ui.label(RichText::new(error_msg).small().color(Color32::RED));
                if !self.rpc_url.is_empty() {
                    let url = self.rpc_url.clone();
                    let user = self.rpc_user.clone();
                    let password = self.rpc_password.clone();
                    if ui.button("Retry").clicked() {
                        self.check_connection(&url, &user, &password);
                    }
                }
            }
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
                // Reset connection status
                *self.connection_status.lock().unwrap() =
                    ConnectionStatus::Unknown;
                self.status_promise = None;
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
