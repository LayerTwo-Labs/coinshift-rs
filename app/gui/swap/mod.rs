use eframe::egui;
use strum::{EnumIter, IntoEnumIterator};

use crate::app::App;

mod create;
mod list;

use create::CreateSwap;
use list::SwapList;

#[derive(Default, EnumIter, Eq, PartialEq, strum::Display)]
enum Tab {
    #[default]
    #[strum(to_string = "Create Swap")]
    Create,
    #[strum(to_string = "My Swaps")]
    List,
}

pub struct Swap {
    create: CreateSwap,
    list: SwapList,
    tab: Tab,
}

impl Swap {
    pub fn new(app: Option<&App>) -> Self {
        Self {
            create: CreateSwap::default(),
            list: SwapList::new(app),
            tab: Tab::default(),
        }
    }

    pub fn show(&mut self, app: Option<&App>, ui: &mut egui::Ui) {
        egui::TopBottomPanel::top("swap_tabs").show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                Tab::iter().for_each(|tab_variant| {
                    let tab_name = tab_variant.to_string();
                    ui.selectable_value(&mut self.tab, tab_variant, tab_name);
                })
            });
        });
        egui::CentralPanel::default().show(ui.ctx(), |ui| match self.tab {
            Tab::Create => {
                self.create.show(app, ui);
            }
            Tab::List => {
                self.list.show(app, ui);
            }
        });
    }
}

