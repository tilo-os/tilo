use ui::prelude::*;

const DEFAULT_PAYLOAD: &str = "https://qos.dev";

/// The app's state: what the code encodes. Stored, synced and undoable.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Code {
    pub payload: String,
}

impl Default for Code {
    fn default() -> Self {
        Self {
            payload: String::from(DEFAULT_PAYLOAD),
        }
    }
}

#[resource(kind = "generated.qr-code", label = "QR code")]
impl Code {
    /// Set the QR payload.
    #[action]
    pub fn set_payload(&mut self, #[param("Text or URL to encode")] payload: String) {
        self.payload = payload;
    }

    /// Reset the QR code.
    #[action]
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A complete IME input → resource → generated visual output loop.
pub struct App;

impl ResourceApp for App {
    type Resource = Code;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Code>) -> Element {
        crate::install_catalog();
        let input = cx
            .input(|code| code.payload.clone(), Code::SET_PAYLOAD)
            .with_id("qr-input")
            .with_placeholder("qr-input-placeholder");
        // The payload, followed from the resource, rebuilds only the QR output.
        let payload = cx.value(|code| code.payload.clone());
        let output = ModelView::from_state(&payload, |payload| {
            let alt = ui::i18n::I18nManager::format(
                "qr-output-alt",
                &[("payload", payload.as_str().into())],
            );
            Box::new(
                QrWidget::new(payload.as_bytes().to_vec())
                    .with_id("qr-output")
                    .with_alt(alt),
            )
        });
        let reset = cx.button("qr-reset", Code::RESET.invoke());
        Box::new(ui::column![
            Text::new("qr-title").with_role(TextRole::Title),
            labeled_field("qr-input-label", input),
            output,
            reset,
        ])
    }
}
