use ui::prelude::{
    Button, LocalState, ModelView, QrWidget, Text, TextInput, TextRole, Widget, labeled_field,
    tilo_actions,
};

fn set_payload(payload: &LocalState<String>, value: String) {
    payload.set(value);
}

/// A complete IME input → typed state → generated visual output loop.
///
/// # Panics
///
/// Panics if the generated payload binding no longer matches its action.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
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
    let resource = ui::widget::resource_actions::typed_resource(
        "generated.qr-code",
        "QR code",
        "set-payload",
        "Set QR payload",
        vec![tilo_actions::ActionParam::required_owned(
            "payload",
            tilo_actions::ParamType::Text,
            "Text or URL to encode",
        )],
    );
    let input_binding = tilo_actions::ActionBinding::new(
        &resource,
        resource
            .actions
            .iter()
            .find(|action| action.id == "set-payload")
            .expect("generated QR resource exports set-payload"),
        |value| serde_json::Map::from_iter([("payload".into(), serde_json::json!(value))]),
    )
    .expect("QR input produces valid payload arguments");
    let input = TextInput::from_state(&payload)
        .with_id("qr-input")
        .with_placeholder("qr-input-placeholder")
        .on_action(input_binding);
    let reset_binding = tilo_actions::ActionBinding::static_json_for(
        &resource,
        "set-payload",
        serde_json::Map::from_iter([("payload".into(), serde_json::json!("https://qos.dev"))]),
    )
    .expect("generated default payload matches the exported QR action");
    let reset = Button::new("qr-reset", "Reset QR code", None).on_action(reset_binding);

    let root = Box::new(ui::column![
        Text::new("qr-title").with_role(TextRole::Title),
        labeled_field("qr-input-label", input),
        output,
        reset,
    ]);
    ui::widget::resource_actions::immediate(root, resource, move |bytes| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let value = args
            .get("payload")
            .and_then(|value| value.as_str())
            .ok_or("missing payload")?
            .to_owned();
        set_payload(&payload, value);
        Ok(tilo_actions::ActionOutput::default())
    })
}
