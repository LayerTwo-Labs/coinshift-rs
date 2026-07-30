use std::borrow::Borrow;

use eframe::egui::{self, Color32, InnerResponse, Response, Ui};

/// Bitcoin Orange Color
pub const BITCOIN_ORANGE: Color32 = Color32::from_rgb(0xf7, 0x93, 0x1a);

/// Unicode BTC symbol (U+20BF)
pub const BTC_UNICODE: char = '\u{20bf}';

/// Font-Awesome Bitcoin Logo symbol (U+F10F)
/// Note that this symbol is wider than other glyphs, often taking up as much
/// space as 3 chars.
pub const BITCOIN_LOGO_FA: char = '\u{f10f}';

/// Show a [`bitcoin::Amount`]
pub fn show_btc_amount(amount: bitcoin::Amount) -> String {
    format!(
        "{BTC_UNICODE}{}",
        amount.to_string_in(bitcoin::Denomination::Bitcoin)
    )
}

/// Show an L2 amount as a plain number (no prefix).
pub fn show_l2_amount(amount: bitcoin::Amount) -> String {
    amount.to_string_in(bitcoin::Denomination::Bitcoin)
}

/// Show an L1 amount with the parent-chain abbreviation prefix.
///
/// The amount is a count of the chain's base units, so it must be rendered with
/// that chain's decimals — not with Bitcoin's fixed 8.
pub fn show_l1_amount(
    amount: bitcoin::Amount,
    chain: coinshift::types::ParentChainType,
) -> String {
    format!(
        "{} {}",
        chain.ticker(),
        coinshift::types::format_l1_amount(amount.to_sat(), chain)
    )
}

// extension for InnerResponse<Response> and InnerResponse<Option<Response>>
pub trait InnerResponseExt {
    #[allow(dead_code)]
    fn join(self) -> Response;
}

impl InnerResponseExt for InnerResponse<Response> {
    fn join(self) -> Response {
        self.response | self.inner
    }
}

impl InnerResponseExt for InnerResponse<Option<Response>> {
    fn join(self) -> Response {
        match self.inner {
            Some(inner) => self.response | inner,
            None => self.response,
        }
    }
}

/// extension trait for egui::Ui
pub trait UiExt {
    fn monospace_selectable_singleline<Text>(
        &mut self,
        clip_text: bool,
        text: Text,
    ) -> Response
    where
        Text: Borrow<str>;

    fn monospace_selectable_multiline<Text>(&mut self, text: Text) -> Response
    where
        Text: Borrow<str>;
}

impl UiExt for Ui {
    fn monospace_selectable_singleline<Text>(
        &mut self,
        clip_text: bool,
        text: Text,
    ) -> Response
    where
        Text: Borrow<str>,
    {
        use egui::{TextEdit, TextStyle, Widget};
        let mut text: &str = text.borrow();
        TextEdit::singleline(&mut text)
            .font(TextStyle::Monospace)
            .clip_text(clip_text)
            .ui(self)
    }

    fn monospace_selectable_multiline<Text>(&mut self, text: Text) -> Response
    where
        Text: Borrow<str>,
    {
        use egui::{TextEdit, TextStyle, Widget};
        let mut text: &str = text.borrow();
        TextEdit::multiline(&mut text)
            .font(TextStyle::Monospace)
            .ui(self)
    }
}
