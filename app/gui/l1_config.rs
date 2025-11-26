use eframe::egui::{self, Button, TextEdit, RichText, Color32, ComboBox};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::path::PathBuf;
use poll_promise::Promise;
use serde_json::json;
use coinshift::types::ParentChainType;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct RpcConfig {
    url: String,
    user: String,
    password: String,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            user: String::new(),
            password: String::new(),
        }
    }
}

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
    configs: HashMap<ParentChainType, RpcConfig>,
    connection_status: Arc<Mutex<ConnectionStatus>>,
    status_promise: Option<Promise<anyhow::Result<u64>>>,
}

impl Default for L1Config {
    fn default() -> Self {
        Self {
            selected_parent_chain: ParentChainType::BTC,
            rpc_url: String::new(),
            rpc_user: String::new(),
            rpc_password: String::new(),
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
        let config_path = Self::config_file_path();
        if let Ok(file_content) = std::fs::read_to_string(&config_path) {
            if let Ok(stored_configs) = serde_json::from_str::<HashMap<ParentChainType, RpcConfig>>(&file_content) {
                self.configs = stored_configs;
                // Load the currently selected parent chain's config
                if let Some(config) = self.configs.get(&self.selected_parent_chain) {
                    self.rpc_url = config.url.clone();
                    self.rpc_user = config.user.clone();
                    self.rpc_password = config.password.clone();
                }
            }
        }
    }

    fn save(&mut self, _ctx: &egui::Context) {
        let config = RpcConfig {
            url: self.rpc_url.clone(),
            user: self.rpc_user.clone(),
            password: self.rpc_password.clone(),
        };
        
        self.configs.insert(self.selected_parent_chain, config.clone());
        
        // Persist to file
        let config_path = Self::config_file_path();
        if let Some(parent_dir) = config_path.parent() {
            drop(std::fs::create_dir_all(parent_dir));
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.configs) {
            drop(std::fs::write(&config_path, json));
        }
        
        // Auto-check connection when saving
        if !config.url.is_empty() {
            self.check_connection(&config.url, &config.user, &config.password);
        }
    }

    fn load_selected_chain_config(&mut self) {
        if let Some(config) = self.configs.get(&self.selected_parent_chain) {
            self.rpc_url = config.url.clone();
            self.rpc_user = config.user.clone();
            self.rpc_password = config.password.clone();
        } else {
            self.rpc_url.clear();
            self.rpc_user.clear();
            self.rpc_password.clear();
        }
        // Reset connection status when switching chains
        *self.connection_status.lock().unwrap() = ConnectionStatus::Unknown;
        self.status_promise = None;
    }

    pub fn get_rpc_url(&self, parent_chain: ParentChainType) -> Option<String> {
        self.configs.get(&parent_chain).map(|c| c.url.clone())
    }

    pub fn get_rpc_user(&self, parent_chain: ParentChainType) -> Option<String> {
        self.configs.get(&parent_chain).map(|c| c.user.clone())
    }

    pub fn get_rpc_password(&self, parent_chain: ParentChainType) -> Option<String> {
        self.configs.get(&parent_chain).map(|c| c.password.clone())
    }

    fn check_connection(&mut self, url: &str, user: &str, password: &str) {
        if url.is_empty() {
            return;
        }

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

    fn fetch_block_height(url: &str, user: &str, password: &str) -> anyhow::Result<u64> {
        use std::time::Duration;
        
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getblockchaininfo",
            "params": []
        });

        let mut request_builder = client.post(url).json(&request);
        
        // Add HTTP basic authentication if user and password are provided
        if !user.is_empty() {
            request_builder = request_builder.basic_auth(user, Some(password));
        }

        let response = request_builder.send()?;

        let json: serde_json::Value = response.json()?;
        
        if let Some(error) = json.get("error") {
            anyhow::bail!("RPC error: {}", error);
        }
        
        let result = json.get("result")
            .ok_or_else(|| anyhow::anyhow!("No result in response"))?;
        
        let blocks = result.get("blocks")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("No blocks field in response"))?;
        
        Ok(blocks)
    }

    fn update_status(&mut self) {
        if let Some(promise) = &self.status_promise {
            if let Some(result) = promise.ready() {
                match result {
                    Ok(block_height) => {
                        *self.connection_status.lock().unwrap() = 
                            ConnectionStatus::Connected { block_height: *block_height };
                    }
                    Err(err) => {
                        *self.connection_status.lock().unwrap() = 
                            ConnectionStatus::Disconnected { 
                                error: format!("{err:#}") 
                            };
                    }
                }
                self.status_promise = None;
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.heading("L1 Node RPC Configuration");
        ui.separator();

        ui.label("Configure the RPC URL for the L1 node (e.g., Bitcoin Core RPC)");
        ui.label("This is used for monitoring L1 transactions for swaps.");
        ui.label("Each parent chain can have its own RPC configuration.");
        ui.add_space(10.0);

        // Parent chain selection
        ui.horizontal(|ui| {
            ui.label("Parent Chain:");
            let previous_chain = self.selected_parent_chain;
            ComboBox::from_id_salt("l1_config_parent_chain")
                .selected_text(format!("{:?}", self.selected_parent_chain))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected_parent_chain, ParentChainType::BTC, "BTC");
                    ui.selectable_value(&mut self.selected_parent_chain, ParentChainType::Signet, "Signet");
                    ui.selectable_value(&mut self.selected_parent_chain, ParentChainType::Regtest, "Regtest");
                    ui.selectable_value(&mut self.selected_parent_chain, ParentChainType::BCH, "BCH");
                    ui.selectable_value(&mut self.selected_parent_chain, ParentChainType::LTC, "LTC");
                });
            
            // Load config when parent chain changes
            if previous_chain != self.selected_parent_chain {
                self.load_selected_chain_config();
            }
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("RPC URL:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_url)
                    .hint_text("http://localhost:8332")
                    .desired_width(300.0),
            );
        });

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
        if let Some(saved_config) = self.configs.get(&self.selected_parent_chain) {
            ui.horizontal(|ui| {
                ui.label("Current saved URL:");
                use crate::gui::util::UiExt;
                ui.monospace_selectable_singleline(true, saved_config.url.as_str());
            });
            if !saved_config.user.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("Current saved User:");
                    use crate::gui::util::UiExt;
                    ui.monospace_selectable_singleline(true, saved_config.user.as_str());
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
                if let Some(saved_config) = self.configs.get(&self.selected_parent_chain) {
                    if !saved_config.url.is_empty() {
                        let url = saved_config.url.clone();
                        let user = saved_config.user.clone();
                        let password = saved_config.password.clone();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(Color32::GRAY));
                            ui.label("Status: Unknown");
                            if ui.button("Check Connection").clicked() {
                                self.check_connection(&url, &user, &password);
                            }
                        });
                    }
                }
            }
            ConnectionStatus::Checking => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::YELLOW));
                    ui.label(RichText::new("Checking connection...").color(Color32::YELLOW));
                });
            }
            ConnectionStatus::Connected { block_height } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::GREEN));
                    ui.label(RichText::new("Connected").color(Color32::GREEN).strong());
                    ui.label(format!("Latest Block Height: {}", block_height));
                });
                if let Some(saved_config) = self.configs.get(&self.selected_parent_chain) {
                    if !saved_config.url.is_empty() {
                        let url = saved_config.url.clone();
                        let user = saved_config.user.clone();
                        let password = saved_config.password.clone();
                        if ui.button("Refresh").clicked() {
                            self.check_connection(&url, &user, &password);
                        }
                    }
                }
            }
            ConnectionStatus::Disconnected { error } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::RED));
                    ui.label(RichText::new("Disconnected").color(Color32::RED).strong());
                });
                let error_msg = format!("Error: {}", error);
                ui.label(RichText::new(error_msg).small().color(Color32::RED));
                if let Some(saved_config) = self.configs.get(&self.selected_parent_chain) {
                    if !saved_config.url.is_empty() {
                        let url = saved_config.url.clone();
                        let user = saved_config.user.clone();
                        let password = saved_config.password.clone();
                        if ui.button("Retry").clicked() {
                            self.check_connection(&url, &user, &password);
                        }
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
                self.rpc_url.clear();
                self.rpc_user.clear();
                self.rpc_password.clear();
                self.configs.remove(&self.selected_parent_chain);
                // Persist the updated configs to file
                let config_path = Self::config_file_path();
                if let Some(parent_dir) = config_path.parent() {
                    drop(std::fs::create_dir_all(parent_dir));
                }
                if let Ok(json) = serde_json::to_string_pretty(&self.configs) {
                    drop(std::fs::write(&config_path, json));
                }
                // Reset connection status
                *self.connection_status.lock().unwrap() = ConnectionStatus::Unknown;
                self.status_promise = None;
            }
        });

        if !self.rpc_url.is_empty() && !url_valid {
            ui.label(egui::RichText::new("Invalid URL format").color(egui::Color32::RED));
        }

        ui.add_space(20.0);
        ui.separator();
        ui.label(egui::RichText::new("Note:").strong());
        ui.label("This RPC URL is used to monitor L1 transactions for swaps.");
        ui.label("Make sure the L1 node is running and accessible at this URL.");
        ui.label("Configuration is saved per parent chain and persists across sessions.");
    }
}


