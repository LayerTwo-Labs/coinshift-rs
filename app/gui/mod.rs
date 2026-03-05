use std::task::Poll;

use coinshift::{util::Watchable, wallet::Wallet};
use eframe::egui::{self, RichText};
use strum::{EnumIter, IntoEnumIterator};
use util::{BITCOIN_LOGO_FA, BITCOIN_ORANGE, show_btc_amount};

use crate::{
    app::{self, App},
    cli::Config,
    line_buffer::LineBuffer,
    rpc_server,
    util::PromiseStream,
};

mod block_explorer;
mod coins;
mod console_logs;
mod fonts;
mod l1_config;
mod mempool_explorer;
mod miner;
mod parent_chain;
mod seed;
mod swap;
mod util;
mod withdrawals;

use block_explorer::BlockExplorer;
use coins::Coins;
use console_logs::ConsoleLogs;
use fonts::FONT_DEFINITIONS;
use l1_config::L1Config;
use mempool_explorer::MemPoolExplorer;
use miner::Miner;
use parent_chain::ParentChain;
use seed::SetSeed;
use swap::Swap;
use withdrawals::Withdrawals;

use self::util::UiExt;

pub struct EguiApp {
    app: Option<App>,
    block_explorer: BlockExplorer,
    bottom_panel: BottomPanel,
    coins: Coins,
    config: Config,
    console_logs: ConsoleLogs,
    l1_config: L1Config,
    mempool_explorer: MemPoolExplorer,
    miner: Miner,
    parent_chain: ParentChain,
    set_seed: SetSeed,
    swap: Swap,
    tab: Tab,
    /// When app failed to start, the error to show and allow retry after fixing L1 config.
    startup_error: Option<String>,
    withdrawals: Withdrawals,
}

#[derive(Default, EnumIter, Eq, PartialEq, strum::Display)]
enum Tab {
    #[default]
    #[strum(to_string = "Parent Chain")]
    ParentChain,
    #[strum(to_string = "Coins")]
    Coins,
    #[strum(to_string = "Swaps")]
    Swaps,
    #[strum(to_string = "Mempool Explorer")]
    MemPoolExplorer,
    #[strum(to_string = "Block Explorer")]
    BlockExplorer,
    #[strum(to_string = "Withdrawals")]
    Withdrawals,
    #[strum(to_string = "L1 Config")]
    L1Config,
    #[strum(to_string = "Console / Logs")]
    ConsoleLogs,
}

/// Bottom panel, if initialized
struct BottomPanelInitialized {
    app: App,
    wallet_updated: PromiseStream<<Wallet as Watchable<()>>::WatchStream>,
}

impl BottomPanelInitialized {
    fn new(app: App) -> Self {
        let wallet_updated = {
            let rt_guard = app.runtime.enter();
            let wallet_updated = PromiseStream::from(app.wallet.watch());
            drop(rt_guard);
            wallet_updated
        };
        Self {
            app,
            wallet_updated,
        }
    }
}

struct BottomPanel {
    initialized: Option<BottomPanelInitialized>,
    /// None if uninitialized
    /// Some(None) if failed to initialize
    balance: Option<Option<bitcoin::Amount>>,
}

impl BottomPanel {
    /// MUST be run from within a tokio runtime
    fn new(app: Option<App>) -> Self {
        let initialized = app.map(BottomPanelInitialized::new);
        if initialized.is_some() {
            tracing::info!("Initializing balance loading");
        }
        Self {
            initialized,
            balance: None,
        }
    }

    /// Replace the app (e.g. after retry startup). MUST be run from within a tokio runtime.
    fn set_app(&mut self, app: Option<App>) {
        self.initialized = app.map(BottomPanelInitialized::new);
        self.balance = None;
        if self.initialized.is_some() {
            tracing::info!("Initializing balance loading (after retry)");
        }
    }

    /// Updates values if the wallet has been updated
    fn update(&mut self) {
        let Some(initialized) = &mut self.initialized else {
            return;
        };
        let rt_guard = initialized.app.runtime.enter();
        match initialized.wallet_updated.poll_next() {
            Some(Poll::Ready(())) => {
                tracing::debug!("Wallet update detected, loading balance");
                self.balance = match initialized.app.wallet.get_balance() {
                    Ok(balance) => {
                        tracing::info!(
                            balance_sats = balance.total.to_sat(),
                            available_sats = balance.available.to_sat(),
                            "Balance loaded successfully"
                        );
                        Some(Some(balance.total))
                    }
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        tracing::error!("Failed to update balance: {err:#}");
                        Some(None)
                    }
                }
            }
            Some(Poll::Pending) => {
                if self.balance.is_none() {
                    tracing::trace!(
                        "Waiting for wallet update to load balance"
                    );
                }
            }
            None => {
                if self.balance.is_none() {
                    tracing::warn!(
                        "Wallet update stream ended before balance could be loaded"
                    );
                }
            }
        }
        drop(rt_guard)
    }

    fn show_balance(&self, ui: &mut egui::Ui) {
        match self.balance {
            Some(Some(balance)) => {
                ui.monospace(
                    RichText::new(BITCOIN_LOGO_FA.to_string())
                        .color(BITCOIN_ORANGE),
                );
                ui.monospace_selectable_singleline(
                    false,
                    format!("Balance: {}", show_btc_amount(balance)),
                );
            }
            Some(None) => {
                ui.monospace_selectable_singleline(
                    false,
                    "Balance error, check logs",
                );
            }
            None => {
                ui.monospace_selectable_singleline(false, "Loading balance");
                // Log periodically when still loading (but not on every frame)
                // This is handled in update() method
            }
        }
    }

    fn show(&mut self, miner: &mut Miner, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.update();
            self.show_balance(ui);
            // Fill center space,
            // see https://github.com/emilk/egui/discussions/3908#discussioncomment-8270353

            // this frame target width
            // == this frame initial max rect width - last frame others width
            let id_cal_target_size = egui::Id::new("cal_target_size");
            let this_init_max_width = ui.max_rect().width();
            let last_others_width = ui.data(|data| {
                data.get_temp(id_cal_target_size)
                    .unwrap_or(this_init_max_width)
            });
            // this is the total available space for expandable widgets, you can divide
            // it up if you have multiple widgets to expand, even with different ratios.
            let this_target_width = this_init_max_width - last_others_width;

            ui.add_space(this_target_width);
            ui.separator();
            miner.show(
                self.initialized
                    .as_ref()
                    .map(|initialized| &initialized.app),
                ui,
            );
            // this frame others width
            // == this frame final min rect width - this frame target width
            ui.data_mut(|data| {
                data.insert_temp(
                    id_cal_target_size,
                    ui.min_rect().width() - this_target_width,
                )
            });
        });
    }
}

impl EguiApp {
    pub fn new(
        app_result: Result<App, app::Error>,
        config: Config,
        cc: &eframe::CreationContext<'_>,
        logs_capture: LineBuffer,
        rpc_addr: url::Url,
    ) -> Self {
        let (app, startup_error) = match app_result {
            Ok(a) => (Some(a), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let tab = if app.is_none() {
            Tab::L1Config
        } else {
            Tab::default()
        };
        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        cc.egui_ctx.set_fonts(FONT_DEFINITIONS.clone());
        let bottom_panel = BottomPanel::new(app.clone());
        let coins = Coins::new(app.as_ref());
        let console_logs = ConsoleLogs::new(logs_capture, rpc_addr);
        let l1_config = L1Config::new(&cc.egui_ctx);
        let height = app
            .as_ref()
            .and_then(|app| app.node.try_get_height().ok().flatten())
            .unwrap_or(0);
        let parent_chain = ParentChain::new(app.as_ref());
        let swap = Swap::new(app.as_ref());
        Self {
            app,
            block_explorer: BlockExplorer::new(height),
            bottom_panel,
            coins,
            config,
            console_logs,
            l1_config,
            mempool_explorer: MemPoolExplorer::default(),
            miner: Miner::default(),
            parent_chain,
            set_seed: SetSeed::default(),
            swap,
            tab,
            startup_error,
            withdrawals: Withdrawals::default(),
        }
    }
}

impl eframe::App for EguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();
        if let Some(app) = self.app.as_ref()
            && !app.wallet.has_seed().unwrap_or(false)
        {
            egui::CentralPanel::default().show(ctx, |_ui| {
                egui::Window::new("Set Seed").show(ctx, |ui| {
                    self.set_seed.show(app, ui);
                });
            });
        } else {
            egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    Tab::iter().for_each(|tab_variant| {
                        let tab_name = tab_variant.to_string();
                        ui.selectable_value(
                            &mut self.tab,
                            tab_variant,
                            tab_name,
                        );
                    })
                });
            });
            egui::TopBottomPanel::bottom("bottom_panel")
                .show(ctx, |ui| self.bottom_panel.show(&mut self.miner, ui));
            egui::CentralPanel::default().show(ctx, |ui| {
                // When startup failed, show error banner and retry when on L1 Config
                let startup_error_msg = self.startup_error.clone();
                if let Some(ref err) = startup_error_msg {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::RED,
                            "Startup failed. Update L1 Config below, then retry.",
                        );
                        if ui.button("Retry startup").clicked() {
                            match App::new(&self.config) {
                                Ok(app) => {
                                    let app_for_rpc = app.clone();
                                    let rpc_addr = self.config.rpc_addr;
                                    app.runtime.spawn(async move {
                                        tracing::info!(
                                            "starting RPC server at `{}`",
                                            rpc_addr
                                        );
                                        if let Err(err) =
                                            rpc_server::run_server(
                                                app_for_rpc,
                                                rpc_addr,
                                            )
                                            .await
                                        {
                                            tracing::error!(
                                                "RPC server error: {err:#}"
                                            );
                                        }
                                    });
                                    self.app = Some(app.clone());
                                    self.bottom_panel.set_app(Some(app));
                                    self.startup_error = None;
                                    self.tab = Tab::default();
                                }
                                Err(e) => {
                                    self.startup_error =
                                        Some(e.to_string());
                                }
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::DARK_RED,
                        format!("Error: {err}"),
                    );
                    ui.add_space(8.0);
                }
                match self.tab {
                    Tab::ParentChain => {
                        self.parent_chain.show(self.app.as_ref(), ui)
                    }
                    Tab::Coins => {
                        self.coins.show(self.app.as_ref(), ui);
                    }
                    Tab::Swaps => {
                        self.swap.show(self.app.as_ref(), ui);
                    }
                    Tab::MemPoolExplorer => {
                        self.mempool_explorer.show(self.app.as_ref(), ui);
                    }
                    Tab::BlockExplorer => {
                        self.block_explorer.show(self.app.as_ref(), ui);
                    }
                    Tab::Withdrawals => {
                        self.withdrawals.show(self.app.as_ref(), ui);
                    }
                    Tab::L1Config => {
                        self.l1_config.show(ctx, ui);
                    }
                    Tab::ConsoleLogs => {
                        self.console_logs.show(self.app.as_ref(), ui);
                    }
                }
            });
        }
    }
}
