//! QR code: a generated visual that follows resource state.
//!
//! The text field edits the payload; the QR code below is drawn from it on
//! every change. `cx.value` follows one field of the resource into a
//! `LocalState`, and `ModelView::from_state` rebuilds only the part of the
//! view that depends on it, so typing never rebuilds the input itself.
//!
//! A picture is invisible to a screen reader, an agent or a test, so the
//! code carries alternative text naming what it encodes. Until there is a
//! payload the app shows an empty state explaining what to do, rather than a
//! code for some sample text.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.qr-code";
/// The window and launcher title.
pub const APP_TITLE: &str = "QR Code";

pub mod locales;

use locales::keys;
use ui::i18n::I18nManager;
use ui::prelude::*;

/// What the code encodes. Stored, synced and undoable.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Code {
    pub payload: String,
}

/// The payload is edited at its path (`Code::paths().payload`); clearing it
/// is a verb, so the palette and agents offer it by name.
#[resource(kind = "example.qr-code", label = "QR code")]
impl Code {
    /// Clear the payload.
    #[action]
    pub fn clear(&mut self) {
        self.payload.clear();
    }
}

fn code_view(payload: &str) -> Element {
    if payload.is_empty() {
        return Box::new(
            EmptyState::new(I18nManager::lookup_plain("qr-empty-title"))
                .with_message(I18nManager::lookup_plain("qr-empty-message")),
        );
    }
    let alt = I18nManager::format("qr-output-alt", &[("payload", payload.into())]);
    Box::new(
        QrWidget::new(payload.as_bytes().to_vec())
            .with_id("qr-output")
            .with_alt(alt),
    )
}

pub struct App;

impl ResourceApp for App {
    type Resource = Code;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        &["encoded"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Code>> {
        match name {
            Some("encoded") => Invoke::edit(&Code::paths().payload.replace("https://tilo.dev"))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Code>) -> Element {
        let p = cx.paths();
        let input = cx
            .input(p.payload.clone())
            .with_id("qr-input")
            .with_placeholder(keys::QR_INPUT_PLACEHOLDER);
        let payload = cx.value_at(&p.payload);
        let output = ModelView::from_state(&payload, |payload| code_view(payload));
        let clear = cx.button(keys::QR_CLEAR_BUTTON, Code::CLEAR.invoke());
        Box::new(ui::column![
            Text::new("qr-title").with_role(TextRole::Title),
            labeled_field("qr-input-label", input),
            output,
            clear,
        ])
    }
}

ui::dev_reload_entry!(App);
