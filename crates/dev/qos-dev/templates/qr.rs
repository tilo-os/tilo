use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{ModelView, QrWidget, Text, TextInput, Widget};

/// A complete IME input → typed state → generated visual output loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let payload = LocalState::new(String::from("https://qos.dev"));
    let output = ModelView::from_state(&payload, |payload| {
        let alt =
            ui::i18n::I18nManager::format("qr-output-alt", &[("payload", payload.as_str().into())]);
        Box::new(
            QrWidget::new(payload.as_bytes().to_vec())
                .with_id("qr-output")
                .with_alt(alt),
        )
    });
    let input = TextInput::local()
        .with_initial_value(&payload.get())
        .with_id("qr-input")
        .with_accessible_name("qr-input-label")
        .with_placeholder("qr-input-placeholder")
        .on_change(payload.text_sink());

    Box::new(ui::column![
        Text::new("qr-title").with_role(TextRole::Title),
        Text::new("qr-input-label").with_role(TextRole::Label),
        input,
        output,
    ])
}
