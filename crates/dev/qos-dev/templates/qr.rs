use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{Button, ModelView, QrWidget, Text, TextInput, Widget, labeled_field};

fn set_payload(payload: &LocalState<String>, value: String) {
    payload.set(value);
}

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
    let input = TextInput::from_state(&payload)
        .with_id("qr-input")
        .with_placeholder("qr-input-placeholder");
    let reset_state = payload.clone();
    let reset = Button::new("qr-reset", "Reset QR code", None)
        .on_click(move || set_payload(&reset_state, "https://qos.dev".into()));

    let root = Box::new(ui::column![
        Text::new("qr-title").with_role(TextRole::Title),
        labeled_field("qr-input-label", input),
        output,
        reset,
    ]);
    crate::resource_actions::immediate(
        root,
        crate::resource_actions::typed_resource(
            "generated.qr-code",
            "QR code",
            "set-payload",
            "Set QR payload",
            vec![qos_actions::ActionParam::required_owned(
                "payload",
                qos_actions::ParamType::Text,
                "Text or URL to encode",
            )],
        ),
        move |bytes| {
            let args: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            let value = args
                .get("payload")
                .and_then(|value| value.as_str())
                .ok_or("missing payload")?
                .to_owned();
            set_payload(&payload, value);
            Ok(qos_actions::ActionOutput::default())
        },
    )
}
