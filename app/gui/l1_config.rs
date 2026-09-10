use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use coinshift::parent_chain_rpc::{self, RpcConfig};
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
    rpc_cookie_file: String,
    configs: HashMap<ParentChainType, RpcConfig>,
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
            rpc_cookie_file: String::new(),
            configs: HashMap::new(),
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

    fn config_file_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("coinshift")
            .join("l1_rpc_configs.json")
    }

    fn load(&mut self, _ctx: &egui::Context) {
        self.configs =
            parent_chain_rpc::read_l1_config_file(&Self::config_file_path());
        self.load_fields_for_selected();
    }

    /// Fill the input fields for the selected chain from the saved config,
    /// or from the local-node default when nothing is saved.
    fn load_fields_for_selected(&mut self) {
        let saved = self.configs.get(&self.selected_parent_chain).cloned();
        let default = parent_chain_rpc::default_l1_configs()
            .into_iter()
            .find(|(c, _)| *c == self.selected_parent_chain)
            .map(|(_, rpc)| rpc);
        match saved.or(default) {
            Some(rpc) => self.set_fields(&rpc),
            None => self.set_fields(&RpcConfig::default()),
        }
    }

    fn set_fields(&mut self, rpc: &RpcConfig) {
        self.rpc_url = rpc.url.clone();
        self.rpc_user = rpc.user.clone();
        self.rpc_password = rpc.password.clone();
        self.rpc_cookie_file = rpc
            .cookie_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }

    fn current_config(&self) -> RpcConfig {
        RpcConfig {
            url: self.rpc_url.trim().to_owned(),
            user: self.rpc_user.clone(),
            password: self.rpc_password.clone(),
            cookie_file: {
                let path = self.rpc_cookie_file.trim();
                (!path.is_empty()).then(|| PathBuf::from(path))
            },
        }
    }

    fn persist(&self) {
        let config_path = Self::config_file_path();
        if let Err(err) = parent_chain_rpc::write_l1_config_file_contents(
            &config_path,
            &self.configs,
        ) {
            tracing::error!(
                path = %config_path.display(),
                error = %err,
                "L1 Config: failed to persist configuration"
            );
        } else {
            tracing::info!(
                path = %config_path.display(),
                "L1 Config: configuration persisted to file"
            );
        }
    }

    fn save(&mut self, _ctx: &egui::Context) {
        // Save the user's current input fields for the selected chain
        let config = self.current_config();

        tracing::info!(
            chain = ?self.selected_parent_chain,
            url = %config.url,
            user = %config.user,
            "L1 Config: saving configuration"
        );

        self.configs
            .insert(self.selected_parent_chain, config.clone());
        self.persist();

        // Auto-check connection when saving
        if !config.url.is_empty() {
            self.check_connection(&config);
        }
    }

    fn load_selected_chain_config(&mut self) {
        self.load_fields_for_selected();
        // Reset connection status when switching chains
        *self.connection_status.lock().unwrap() = ConnectionStatus::Unknown;
        self.status_promise = None;
    }

    fn check_connection(&mut self, config: &RpcConfig) {
        if config.url.is_empty() {
            return;
        }

        tracing::info!(
            url = %config.url,
            has_auth = !config.user.is_empty() || config.cookie_file.is_some(),
            "L1 Config: testing connection"
        );

        let config = config.clone();
        let status = self.connection_status.clone();

        *status.lock().unwrap() = ConnectionStatus::Checking;

        let promise = Promise::spawn_thread("l1_rpc_check", move || {
            Self::fetch_block_height(&config)
        });

        self.status_promise = Some(promise);
    }

    fn fetch_block_height(config: &RpcConfig) -> anyhow::Result<u64> {
        use std::time::Duration;

        let url = config.url.as_str();

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

        // Add HTTP basic authentication if credentials are configured
        if let Some((user, password)) = config.credentials()? {
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
            let label = match self.selected_parent_chain {
                ParentChainType::Signet => "Bitcoin Signet (sBTC)",
                ParentChainType::Regtest => "Bitcoin Regtest (local)",
                _ => "Select network",
            };
            ComboBox::from_id_salt("l1_config_parent_chain")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for chain in supported {
                        let option_label = match chain {
                            ParentChainType::Signet => "Bitcoin Signet (sBTC)",
                            ParentChainType::Regtest => {
                                "Bitcoin Regtest (local)"
                            }
                            _ => continue,
                        };
                        ui.selectable_value(
                            &mut self.selected_parent_chain,
                            *chain,
                            option_label,
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
            if ui.button("Use local default").clicked()
                && let Some((_, rpc)) = parent_chain_rpc::default_l1_configs()
                    .into_iter()
                    .find(|(c, _)| *c == self.selected_parent_chain)
            {
                self.set_fields(&rpc);
            }
        });
        ui.label(
            RichText::new(
                "Point this at a node you run or trust. Its answers decide \
                 when your swaps become claimable.",
            )
            .small()
            .color(Color32::GRAY),
        );
        if self.current_config().is_plaintext_remote() {
            ui.label(
                RichText::new(
                    "Warning: plaintext http:// to a remote host. Credentials \
                     and swap payment evidence can be read or forged on the \
                     network path. Use https:// or a node on this machine.",
                )
                .small()
                .color(Color32::from_rgb(230, 140, 0)),
            );
        }

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

        ui.add_space(5.0);

        ui.horizontal(|ui| {
            ui.label("Cookie file:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_cookie_file)
                    .hint_text("optional, e.g. ~/.bitcoin/signet/.cookie")
                    .desired_width(300.0),
            );
        });
        ui.label(
            RichText::new(
                "A cookie file, when set, is read on every call and takes \
                 precedence over user/password.",
            )
            .small()
            .color(Color32::GRAY),
        );

        // Show current saved configuration
        if let Some(saved_config) =
            self.configs.get(&self.selected_parent_chain)
        {
            ui.horizontal(|ui| {
                ui.label("Current saved URL:");
                use crate::gui::util::UiExt;
                ui.monospace_selectable_singleline(
                    true,
                    saved_config.url.as_str(),
                );
            });
            if !saved_config.user.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Current saved User:");
                    use crate::gui::util::UiExt;
                    ui.monospace_selectable_singleline(
                        true,
                        saved_config.user.as_str(),
                    );
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
                    let config = self.current_config();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(Color32::GRAY));
                        ui.label("Status: Unknown");
                        if ui.button("Check Connection").clicked() {
                            self.check_connection(&config);
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
                    let config = self.current_config();
                    if ui.button("Refresh").clicked() {
                        self.check_connection(&config);
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
                    let config = self.current_config();
                    if ui.button("Retry").clicked() {
                        self.check_connection(&config);
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
                self.set_fields(&RpcConfig::default());
                self.configs.remove(&self.selected_parent_chain);
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
        match self.selected_parent_chain {
            ParentChainType::BTC => {
                ui.label("Use Bitcoin Core with -txindex=1 for full transaction lookup.");
            }
            ParentChainType::BCH => {
                ui.label("Use Bitcoin Cash Node (BCHN) or Bitcoin ABC with -txindex=1.");
            }
            ParentChainType::LTC => {
                ui.label("Use Litecoin Core with -txindex=1 for full transaction lookup.");
            }
            ParentChainType::Signet => {
                ui.label("Use Bitcoin Core with -signet -txindex=1 flags.");
            }
            ParentChainType::Regtest => {
                ui.label("Use Bitcoin Core with -regtest -txindex=1 flags for local testing.");
            }
        }
    }
}
