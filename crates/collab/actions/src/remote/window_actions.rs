//! The view actions every app's window publishes: "Show this window", and showing the item at
//! an anchor.

/// The shared view action that asks the platform to reveal a running window.
pub const APPLICATION_ACTIVATE_ACTION: &str = "activate-window";
/// Encoded operation of [`APPLICATION_ACTIVATE_ACTION`].
pub const APPLICATION_ACTIVATE_TYPE: &str = "tilo.app.activate-window";
/// Encoded operation of the shared view action that shows the item at an
/// anchor, as picking that item's search hit does.
pub const APPLICATION_OPEN_TYPE: &str = "tilo.app.open";

/// The shared view action [`APPLICATION_ACTIVATE_ACTION`] ("Show this
/// window"), called `label` and described by `description` in the reader's
/// language: every app's window publishes it, a framework app's and a
/// tilolang host's instance alike, and it answers
/// `{"activation_requested": true}` once the window was asked to come
/// forward.
#[must_use]
pub fn activate_spec(label: String, description: String) -> crate::ActionSpec {
    let mut spec = crate::ActionSpec::typed_operation(
        APPLICATION_ACTIVATE_ACTION,
        label.clone(),
        label,
        crate::OperationKind::View,
        APPLICATION_ACTIVATE_TYPE,
        Vec::new(),
        |_| crate::EncodedAction {
            type_id: APPLICATION_ACTIVATE_TYPE.into(),
            payload: b"{}".to_vec(),
        },
    )
    .of_kind(crate::ActionKind::Read)
    .with_description(description)
    // An agent brings the window forward; a person looking for it in the
    // window's own palette already has it.
    .for_audience(crate::Audience::Agents);
    spec.type_mut().contract.result = crate::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

/// What [`APPLICATION_ACTIVATE_ACTION`] answers once the window was asked to
/// come forward.
#[must_use]
pub fn activation_requested() -> crate::ActionOutput {
    crate::ActionOutput {
        value: Some(serde_json::json!({"activation_requested": true})),
        resources: Vec::new(),
    }
}
