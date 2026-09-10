use crate::app::App;
use eframe::egui;

pub struct SetSeed {
    seed: String,
    passphrase: String,
    /// Show the seed words in clear text rather than masked.
    reveal_seed: bool,
}

impl Default for SetSeed {
    fn default() -> Self {
        Self {
            seed: "".into(),
            passphrase: "".into(),
            reveal_seed: false,
        }
    }
}

impl SetSeed {
    pub fn show(&mut self, app: &App, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Masked by default so the phrase is not readable over the
            // user's shoulder; the toggle lets them check what they typed.
            let seed_edit = egui::TextEdit::singleline(&mut self.seed)
                .hint_text("seed")
                .password(!self.reveal_seed)
                .clip_text(false);
            ui.add(seed_edit);
            ui.checkbox(&mut self.reveal_seed, "reveal");
            if ui.button("generate").clicked() {
                let mnemonic = bip39::Mnemonic::new(
                    bip39::MnemonicType::Words12,
                    bip39::Language::English,
                );
                self.seed = mnemonic.phrase().into();
                self.reveal_seed = true;
            }
        });
        let passphrase_edit = egui::TextEdit::singleline(&mut self.passphrase)
            .hint_text("passphrase (optional)")
            .password(true)
            .clip_text(false);
        ui.add(passphrase_edit);
        if !self.passphrase.is_empty() {
            ui.label(
                "The passphrase is part of the wallet key. Restoring this \
                 wallet needs the same mnemonic AND the same passphrase.",
            );
        }
        let mnemonic =
            bip39::Mnemonic::from_phrase(&self.seed, bip39::Language::English);
        if ui
            .add_enabled(mnemonic.is_ok(), egui::Button::new("set"))
            .clicked()
        {
            app.wallet
                .set_seed_from_mnemonic(
                    self.seed.as_str(),
                    self.passphrase.as_str(),
                )
                .expect("failed to set HD wallet seed");
        }
    }
}
