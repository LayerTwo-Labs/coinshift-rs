use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use coinshift::parent_chain_rpc::{ParentChainRpcClient, RpcConfig};
use coinshift::types::{ParentChainType, Swap, SwapId, SwapState, SwapTxId};
use eframe::egui::{self, Button, ScrollArea};

use crate::app::App;
use crate::gui::util::{show_l1_amount, show_l2_amount};

// ── Filters ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SwapStatusFilter {
    #[default]
    All,
    Pending,
    WaitingConfirmations,
    ReadyToClaim,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OwnershipFilter {
    #[default]
    All,
    YourSwaps,
    Bookmarked,
}

// ── SwapList ───────────────────────────────────────────────────────

#[derive(Default)]
pub struct SwapList {
    pub(crate) swaps: Option<Vec<Swap>>,
    selected_swap_id: Option<SwapId>,
    bookmarked: HashSet<SwapId>,
    status_filter: SwapStatusFilter,
    ownership_filter: OwnershipFilter,
    swap_id_search: String,
    search_error: Option<String>,
    // confirmation checking (kept here since it's background work)
    last_confirmation_check: Option<Instant>,
    checking_confirmations: bool,
}

// Fields all implement Default (Option=None, HashSet=empty, String=empty, bool=false)
// so we can derive Default instead of implementing it manually.

impl SwapList {
    pub fn new(app: Option<&App>) -> Self {
        let mut list = Self::default();
        if let Some(app) = app {
            list.refresh_swaps(app);
        }
        list
    }

    // ── data loading ───────────────────────────────────────────────

    pub fn refresh_swaps(&mut self, app: &App) {
        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get read transaction: {err:#}");
                return;
            }
        };

        let mut swaps_result = match app.node.state().load_all_swaps(&rotxn) {
            Ok(swaps) => swaps,
            Err(err) => {
                tracing::error!("Failed to list swaps: {err:#}");
                return;
            }
        };

        drop(rotxn);

        // Also get pending swaps from mempool
        if let Ok(mempool_txs) = app.node.get_all_transactions() {
            for tx in mempool_txs {
                if let coinshift::types::TxData::SwapCreate {
                    swap_id,
                    parent_chain,
                    l1_txid_bytes: _,
                    required_confirmations,
                    l2_recipient,
                    l2_amount,
                    l1_recipient_address,
                    l1_amount,
                } = &tx.transaction.data
                {
                    let swap_id_obj = coinshift::types::SwapId(*swap_id);
                    if !swaps_result.iter().any(|s| s.id == swap_id_obj) {
                        let l1_txid =
                            coinshift::types::SwapTxId::from_bytes(&[0u8; 32]);
                        let swap = coinshift::types::Swap::new(
                            swap_id_obj,
                            coinshift::types::SwapDirection::L2ToL1,
                            *parent_chain,
                            l1_txid,
                            Some(*required_confirmations),
                            *l2_recipient,
                            bitcoin::Amount::from_sat(*l2_amount),
                            l1_recipient_address.clone(),
                            bitcoin::Amount::from_sat(*l1_amount),
                            0,
                            None,
                            None,
                        );
                        swaps_result.push(swap);
                    }
                }
            }
        }

        self.swaps = Some(swaps_result);
    }

    // ── ownership helper ───────────────────────────────────────────

    fn is_own_swap(app: Option<&App>, swap: &Swap) -> bool {
        let Some(app) = app else { return false };
        if swap.created_at_height == 0 {
            app.node.is_created_pending_swap(&swap.id)
        } else {
            match &swap.l2_creator_address {
                Some(creator) => app
                    .wallet
                    .get_addresses()
                    .map(|addrs| addrs.iter().any(|a| a == creator))
                    .unwrap_or(false),
                None => false,
            }
        }
    }

    // ── main UI ────────────────────────────────────────────────────

    /// Returns `Some(swap)` when the user clicks "View" on a row (navigate to detail).
    pub fn show(
        &mut self,
        app: Option<&App>,
        ui: &mut egui::Ui,
    ) -> Option<Swap> {
        // Background confirmation checking
        let should_check = self
            .last_confirmation_check
            .map(|last| last.elapsed() >= Duration::from_secs(10))
            .unwrap_or(true);

        if should_check
            && !self.checking_confirmations
            && let Some(app) = app
        {
            self.check_confirmations_dynamically(app);
        }

        let mut navigate_to: Option<Swap> = None;

        // ── toolbar ────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.heading("Swap List");
            if ui.button("Refresh").clicked()
                && let Some(app) = app
            {
                self.refresh_swaps(app);
            }
            if self.checking_confirmations {
                ui.label(
                    egui::RichText::new("Checking confirmations...")
                        .small()
                        .color(egui::Color32::GRAY),
                );
            }
        });

        ui.add_space(4.0);

        // ── filters row ────────────────────────────────────────────
        ui.horizontal(|ui| {
            // Status filter
            ui.label("Status:");
            egui::ComboBox::from_id_salt("status_filter")
                .selected_text(match self.status_filter {
                    SwapStatusFilter::All => "All",
                    SwapStatusFilter::Pending => "Pending",
                    SwapStatusFilter::WaitingConfirmations => "Waiting Conf.",
                    SwapStatusFilter::ReadyToClaim => "Ready To Claim",
                    SwapStatusFilter::Completed => "Completed",
                    SwapStatusFilter::Cancelled => "Cancelled",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::All,
                        "All",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Pending,
                        "Pending",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::WaitingConfirmations,
                        "Waiting Confirmations",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::ReadyToClaim,
                        "Ready To Claim",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Completed,
                        "Completed",
                    );
                    ui.selectable_value(
                        &mut self.status_filter,
                        SwapStatusFilter::Cancelled,
                        "Cancelled",
                    );
                });

            ui.separator();

            // Ownership filter
            ui.label("Show:");
            egui::ComboBox::from_id_salt("ownership_filter")
                .selected_text(match self.ownership_filter {
                    OwnershipFilter::All => "All Swaps",
                    OwnershipFilter::YourSwaps => "Your Swaps",
                    OwnershipFilter::Bookmarked => "Bookmarked",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.ownership_filter,
                        OwnershipFilter::All,
                        "All Swaps",
                    );
                    ui.selectable_value(
                        &mut self.ownership_filter,
                        OwnershipFilter::YourSwaps,
                        "Your Swaps",
                    );
                    ui.selectable_value(
                        &mut self.ownership_filter,
                        OwnershipFilter::Bookmarked,
                        "Bookmarked",
                    );
                });
        });

        // ── search row ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label("Search:");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.swap_id_search)
                    .hint_text("Swap ID (hex)")
                    .desired_width(300.0),
            );
            if response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                // search triggered by enter - filtering happens below
            }
            if !self.swap_id_search.is_empty() && ui.button("Clear").clicked() {
                self.swap_id_search.clear();
                self.search_error = None;
            }
        });

        if let Some(err_msg) = &self.search_error {
            ui.label(
                egui::RichText::new(err_msg)
                    .color(egui::Color32::RED)
                    .small(),
            );
        }

        ui.separator();

        // ── build filtered list ────────────────────────────────────
        let swaps = match &self.swaps {
            Some(s) => s,
            None => {
                ui.label("No swaps loaded. Click Refresh.");
                return None;
            }
        };

        let search_lower = self.swap_id_search.trim().to_lowercase();
        let status_filter = self.status_filter;
        let ownership_filter = self.ownership_filter;
        let bookmarked = &self.bookmarked;

        let filtered: Vec<_> = swaps
            .iter()
            .filter(|swap| {
                // status filter
                match status_filter {
                    SwapStatusFilter::All => true,
                    SwapStatusFilter::Pending => {
                        matches!(swap.state, SwapState::Pending)
                    }
                    SwapStatusFilter::WaitingConfirmations => matches!(
                        swap.state,
                        SwapState::WaitingConfirmations(..)
                    ),
                    SwapStatusFilter::ReadyToClaim => {
                        matches!(swap.state, SwapState::ReadyToClaim)
                    }
                    SwapStatusFilter::Completed => {
                        matches!(swap.state, SwapState::Completed)
                    }
                    SwapStatusFilter::Cancelled => {
                        matches!(swap.state, SwapState::Cancelled)
                    }
                }
            })
            .filter(|swap| {
                // ownership filter
                match ownership_filter {
                    OwnershipFilter::All => true,
                    OwnershipFilter::YourSwaps => Self::is_own_swap(app, swap),
                    OwnershipFilter::Bookmarked => {
                        bookmarked.contains(&swap.id)
                    }
                }
            })
            .filter(|swap| {
                // text search
                if search_lower.is_empty() {
                    return true;
                }
                let id_hex = hex::encode(swap.id.0);
                id_hex.contains(&search_lower)
            })
            .collect();

        if filtered.is_empty() {
            ui.label("No swaps match current filters.");
            return None;
        }

        // ── count ───────────────────────────────────────────────────
        ui.label(
            egui::RichText::new(format!("{} swap(s)", filtered.len()))
                .small()
                .color(egui::Color32::from_rgb(160, 160, 160)),
        );

        ui.add_space(2.0);

        // ── table ──────────────────────────────────────────────────
        let header_color = egui::Color32::from_rgb(180, 180, 180);
        let stripe_a = egui::Color32::TRANSPARENT;
        let stripe_b = egui::Color32::from_rgba_premultiplied(255, 255, 255, 6);
        let selected_bg =
            egui::Color32::from_rgba_premultiplied(70, 90, 140, 60);

        ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("swap_list_grid")
                .num_columns(8)
                .spacing([12.0, 0.0])
                .min_col_width(0.0)
                .striped(false) // we handle striping manually for selection highlight
                .show(ui, |ui| {
                    // ── header row ─────────────────────────────────
                    ui.label(""); // bookmark col
                    ui.label(egui::RichText::new("Swap ID").color(header_color).strong().size(11.0));
                    ui.label(egui::RichText::new("Chain").color(header_color).strong().size(11.0));
                    ui.label(egui::RichText::new("State").color(header_color).strong().size(11.0));
                    ui.label(egui::RichText::new("L2 Amount").color(header_color).strong().size(11.0));
                    ui.label(egui::RichText::new("L1 Amount").color(header_color).strong().size(11.0));
                    ui.label(egui::RichText::new("Tags").color(header_color).strong().size(11.0));
                    ui.label(""); // action col
                    ui.end_row();

                    // thin separator after header
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.separator();
                    ui.end_row();

                    // ── data rows ──────────────────────────────────
                    for (i, swap) in filtered.iter().enumerate() {
                        let is_selected = self
                            .selected_swap_id
                            .as_ref()
                            .map(|id| id == &swap.id)
                            .unwrap_or(false);

                        let bg = if is_selected {
                            selected_bg
                        } else if i % 2 == 1 {
                            stripe_b
                        } else {
                            stripe_a
                        };

                        // Paint row background
                        let row_rect = ui.cursor();
                        let painter = ui.painter();
                        let bg_rect = egui::Rect::from_min_size(
                            row_rect.min,
                            egui::vec2(ui.available_width(), 22.0),
                        );
                        painter.rect_filled(bg_rect, 2.0, bg);

                        // Col 1: Bookmark
                        let is_bookmarked = self.bookmarked.contains(&swap.id);
                        let star_label = if is_bookmarked { "*" } else { "-" };
                        let star_color = if is_bookmarked {
                            egui::Color32::from_rgb(255, 200, 60)
                        } else {
                            egui::Color32::from_rgb(80, 80, 80)
                        };
                        if ui
                            .add(
                                Button::new(
                                    egui::RichText::new(star_label)
                                        .color(star_color)
                                        .size(14.0)
                                        .monospace(),
                                )
                                .frame(false),
                            )
                            .on_hover_text(if is_bookmarked {
                                "Remove bookmark"
                            } else {
                                "Add bookmark"
                            })
                            .clicked()
                        {
                            if is_bookmarked {
                                self.bookmarked.remove(&swap.id);
                            } else {
                                self.bookmarked.insert(swap.id);
                            }
                        }

                        // Col 2: Swap ID (truncated)
                        let id_hex = hex::encode(swap.id.0);
                        let short_id = &id_hex[..10.min(id_hex.len())];
                        ui.label(
                            egui::RichText::new(format!("{}...", short_id))
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_rgb(200, 210, 230)),
                        )
                        .on_hover_text(&id_hex);

                        // Col 3: Chain
                        ui.label(
                            egui::RichText::new(format!("{:?}", swap.parent_chain))
                                .size(11.0),
                        );

                        // Col 4: State
                        let (state_text, state_color) = state_display(&swap.state);
                        ui.label(
                            egui::RichText::new(state_text)
                                .color(state_color)
                                .strong()
                                .size(11.0),
                        );

                        // Col 5: L2 Amount
                        ui.label(
                            egui::RichText::new(show_l2_amount(swap.l2_amount))
                                .size(11.0),
                        );

                        // Col 6: L1 Amount
                        let l1_text = show_l1_amount(swap.l1_amount, swap.parent_chain);
                        ui.label(egui::RichText::new(l1_text).size(11.0));

                        // Col 7: Tags
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if swap.created_at_height == 0 {
                                ui.label(
                                    egui::RichText::new("mempool")
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(200, 160, 90))
                                        .background_color(egui::Color32::from_rgba_premultiplied(200, 160, 90, 25)),
                                );
                            }
                            if Self::is_own_swap(app, swap) {
                                ui.label(
                                    egui::RichText::new("yours")
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(100, 180, 255))
                                        .background_color(egui::Color32::from_rgba_premultiplied(100, 180, 255, 25)),
                                );
                            }
                            if swap.l2_recipient.is_none() {
                                ui.label(
                                    egui::RichText::new("open")
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(180, 140, 220))
                                        .background_color(egui::Color32::from_rgba_premultiplied(180, 140, 220, 25)),
                                );
                            }
                        });

                        // Col 8: View button
                        if ui
                            .add(
                                Button::new(
                                    egui::RichText::new("View")
                                        .size(11.0),
                                ),
                            )
                            .clicked()
                        {
                            self.selected_swap_id = Some(swap.id);
                            navigate_to = Some((*swap).clone());
                        }

                        ui.end_row();
                    }
                });
        });

        navigate_to
    }

    // ── background confirmation checking ───────────────────────────

    pub(crate) fn load_rpc_config(
        &self,
        parent_chain: ParentChainType,
    ) -> Option<RpcConfig> {
        use dirs;
        use serde::{Deserialize, Serialize};
        use std::path::PathBuf;

        #[derive(Clone, Serialize, Deserialize)]
        struct LocalRpcConfig {
            url: String,
            user: String,
            password: String,
        }

        let config_path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("coinshift")
            .join("l1_rpc_configs.json");

        if let Ok(file_content) = std::fs::read_to_string(&config_path)
            && let Ok(configs) = serde_json::from_str::<
                HashMap<ParentChainType, LocalRpcConfig>,
            >(&file_content)
            && let Some(local_config) = configs.get(&parent_chain)
        {
            return Some(RpcConfig {
                url: local_config.url.clone(),
                user: local_config.user.clone(),
                password: local_config.password.clone(),
            });
        }

        coinshift::parent_chain_rpc::supported_l1_configs()
            .into_iter()
            .find(|(c, _)| *c == parent_chain)
            .map(|(_, rpc)| RpcConfig {
                url: rpc.url,
                user: rpc.user,
                password: rpc.password,
            })
    }

    fn check_confirmations_dynamically(&mut self, app: &App) {
        if self.checking_confirmations {
            return;
        }

        self.checking_confirmations = true;
        self.last_confirmation_check = Some(Instant::now());

        let rotxn = match app.node.env().read_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get read transaction: {err:#}");
                self.checking_confirmations = false;
                return;
            }
        };

        let swaps = match app.node.state().load_all_swaps(&rotxn) {
            Ok(swaps) => swaps,
            Err(err) => {
                tracing::error!("Failed to load swaps: {err:#}");
                self.checking_confirmations = false;
                return;
            }
        };

        let swaps_to_check: Vec<_> = swaps
            .iter()
            .filter(|swap| {
                matches!(swap.state, SwapState::WaitingConfirmations(..))
                    && !matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == [0u8; 32])
                    && !matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0))
            })
            .collect();

        drop(rotxn);

        if swaps_to_check.is_empty() {
            self.checking_confirmations = false;
            return;
        }

        let work: Vec<_> = swaps_to_check
            .iter()
            .filter_map(|swap| {
                self.load_rpc_config(swap.parent_chain).map(|rpc_config| {
                    (swap.id, rpc_config, swap.l1_txid.to_hex())
                })
            })
            .collect();

        let results: Vec<(SwapId, u32)> = std::thread::spawn(move || {
            work.into_iter()
                .filter_map(|(swap_id, rpc_config, l1_txid_hex)| {
                    let client = ParentChainRpcClient::new(rpc_config);
                    client
                        .get_transaction_confirmations(&l1_txid_hex)
                        .ok()
                        .map(|c| (swap_id, c))
                })
                .collect()
        })
        .join()
        .unwrap_or_default();

        let mut updated_count = 0;
        let mut rwtxn = match app.node.env().write_txn() {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("Failed to get write transaction: {err:#}");
                self.checking_confirmations = false;
                return;
            }
        };

        let block_hash = match app.node.state().try_get_tip(&rwtxn) {
            Ok(Some(hash)) => hash,
            Ok(None) | Err(_) => {
                self.checking_confirmations = false;
                return;
            }
        };
        let block_height = match app.node.state().try_get_height(&rwtxn) {
            Ok(Some(height)) => height,
            Ok(None) | Err(_) => {
                self.checking_confirmations = false;
                return;
            }
        };

        for (swap_id, new_confirmations) in results {
            let current = swaps_to_check
                .iter()
                .find(|s| s.id == swap_id)
                .and_then(|s| match s.state {
                    SwapState::WaitingConfirmations(c, _) => Some(c),
                    _ => None,
                });
            let Some(current_confirmations) = current else {
                continue;
            };
            if new_confirmations <= current_confirmations {
                continue;
            }

            tracing::info!(
                swap_id = %swap_id,
                old = %current_confirmations,
                new = %new_confirmations,
                "Updating swap confirmations"
            );

            if let Err(err) = app.node.state().update_swap_confirmations(
                &mut rwtxn,
                &swap_id,
                new_confirmations,
                block_hash,
                block_height,
            ) {
                tracing::error!(swap_id = %swap_id, error = %err, "Failed to update confirmations");
            } else {
                updated_count += 1;
            }
        }

        if updated_count > 0 {
            if let Err(err) = rwtxn.commit() {
                tracing::error!("Failed to commit swap updates: {err:#}");
            } else {
                self.refresh_swaps(app);
            }
        } else {
            drop(rwtxn);
        }

        self.checking_confirmations = false;
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn state_display(state: &SwapState) -> (String, egui::Color32) {
    match state {
        SwapState::Pending => {
            ("Pending".into(), egui::Color32::from_rgb(130, 170, 255))
        }
        SwapState::WaitingConfirmations(cur, req) => (
            format!("Waiting {}/{}", cur, req),
            egui::Color32::from_rgb(255, 180, 100),
        ),
        SwapState::ReadyToClaim => {
            ("Ready".into(), egui::Color32::from_rgb(100, 220, 100))
        }
        SwapState::Completed => {
            ("Completed".into(), egui::Color32::from_rgb(140, 200, 140))
        }
        SwapState::Cancelled => {
            ("Cancelled".into(), egui::Color32::from_rgb(150, 150, 150))
        }
    }
}
