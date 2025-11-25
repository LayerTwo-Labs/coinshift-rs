use eframe::egui::{self, Button, TextEdit, RichText, Color32};
use std::sync::{Arc, Mutex};
use poll_promise::Promise;
use serde_json::json;

fn l1_rpc_url_key() -> egui::Id {
    egui::Id::new("l1_rpc_url")
}

#[derive(Clone)]
enum ConnectionStatus {
    Unknown,
    Connected { block_height: u64 },
    Disconnected { error: String },
    Checking,
}

pub struct L1Config {
    rpc_url: String,
    saved_url: Option<String>,
    connection_status: Arc<Mutex<ConnectionStatus>>,
    status_promise: Option<Promise<anyhow::Result<u64>>>,
}

impl Default for L1Config {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            saved_url: None,
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

    fn load(&mut self, ctx: &egui::Context) {
        if let Some(url) = ctx.data(|data| data.get_temp::<String>(l1_rpc_url_key())) {
            let url = url.clone();
            self.rpc_url = url.clone();
            self.saved_url = Some(url);
        }
    }

    fn save(&mut self, ctx: &egui::Context) {
        let url_to_save = self.rpc_url.clone();
        ctx.data_mut(|data| {
            data.insert_temp(l1_rpc_url_key(), url_to_save.clone());
        });
        self.saved_url = Some(url_to_save.clone());
        // Auto-check connection when saving
        if !url_to_save.is_empty() {
            self.check_connection(&url_to_save);
        }
    }

    pub fn get_rpc_url(&self) -> Option<String> {
        self.saved_url.clone()
    }

    fn check_connection(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }

        let url = url.to_string();
        let status = self.connection_status.clone();
        
        *status.lock().unwrap() = ConnectionStatus::Checking;
        
        let promise = Promise::spawn_thread("l1_rpc_check", move || {
            Self::fetch_block_height(&url)
        });
        
        self.status_promise = Some(promise);
    }

    fn fetch_block_height(url: &str) -> anyhow::Result<u64> {
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

        let response = client
            .post(url)
            .json(&request)
            .send()?;

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
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("RPC URL:");
            ui.add(
                TextEdit::singleline(&mut self.rpc_url)
                    .hint_text("http://localhost:8332")
                    .desired_width(300.0),
            );
        });

        // Show current saved URL
        if let Some(saved) = &self.saved_url {
            ui.horizontal(|ui| {
                ui.label("Current saved URL:");
                use crate::gui::util::UiExt;
                ui.monospace_selectable_singleline(true, saved.as_str());
            });
        } else {
            ui.label("No RPC URL configured");
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
                let saved_url = self.saved_url.clone();
                if let Some(saved) = &saved_url {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(Color32::GRAY));
                        ui.label("Status: Unknown");
                        if ui.button("Check Connection").clicked() {
                            self.check_connection(saved);
                        }
                    });
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
                let saved_url = self.saved_url.clone();
                if let Some(saved) = &saved_url {
                    if ui.button("Refresh").clicked() {
                        self.check_connection(saved);
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
                let saved_url = self.saved_url.clone();
                if let Some(saved) = &saved_url {
                    if ui.button("Retry").clicked() {
                        self.check_connection(saved);
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
                ctx.data_mut(|data| {
                    data.remove::<String>(l1_rpc_url_key());
                });
                self.saved_url = None;
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
    }
}

