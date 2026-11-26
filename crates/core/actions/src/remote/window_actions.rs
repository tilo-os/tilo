//! The view actions every app's window publishes: "Show this window", showing the item at an
//! anchor, and reading what the window shows (`outline`).

/// The shared view action that asks the platform to reveal a running window.
pub const APPLICATION_ACTIVATE_ACTION: &str = "activate-window";
/// Encoded operation of [`APPLICATION_ACTIVATE_ACTION`].
pub const APPLICATION_ACTIVATE_TYPE: &str = "tilo.app.activate-window";
/// Encoded operation of the shared view action that shows the item at an
/// anchor, as picking that item's search hit does.
pub const APPLICATION_OPEN_TYPE: &str = "tilo.app.open";
/// The view action every app publishes on its own resource that shows what a
/// link names (`geo:52.5,13.4`, `settings://keyboard`): a shell hands a link to
/// the app that claims its scheme through it, whether the app runs or not.
pub const APPLICATION_OPEN_LINK_ACTION: &str = "open-link";
/// Encoded operation of [`APPLICATION_OPEN_LINK_ACTION`].
pub const APPLICATION_OPEN_LINK_TYPE: &str = "tilo.app.open-link";
/// The parameter of [`APPLICATION_OPEN_LINK_ACTION`] naming the link.
pub const OPEN_LINK_PARAM: &str = "link";

/// The arguments of [`APPLICATION_OPEN_LINK_ACTION`] for `link`.
#[must_use]
pub fn open_link_arguments(link: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        OPEN_LINK_PARAM.to_owned(),
        serde_json::Value::String(link.to_owned()),
    );
    arguments
}

/// The scheme of `link`, lowercase, as an app's manifest claims it
/// (`geo` for `geo:52.5,13.4`); `None` for text that names no scheme
/// (a path, a bare word).
#[must_use]
pub fn link_scheme(link: &str) -> Option<String> {
    let (scheme, rest) = link.split_once(':')?;
    let mut chars = scheme.chars();
    // One letter is a drive (`C:\notes`), never a scheme.
    let valid = scheme.len() > 1
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty();
    valid.then(|| scheme.to_ascii_lowercase())
}

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

/// The view action every app's window publishes that answers what it shows as lines an agent
/// reads: each part's words and value, and the action it offers.
pub const APPLICATION_OUTLINE_ACTION: &str = "outline";
/// Encoded operation of [`APPLICATION_OUTLINE_ACTION`].
pub const APPLICATION_OUTLINE_TYPE: &str = "tilo.app.outline";
/// The parameter of [`APPLICATION_OUTLINE_ACTION`] naming an item to show before reading.
pub const OUTLINE_ANCHOR_PARAM: &str = "anchor";
/// The key of an [`APPLICATION_OUTLINE_ACTION`] answer saying why it carries no picture where
/// one was asked for.
pub const OUTLINE_PICTURE_UNAVAILABLE: &str = "picture_unavailable";

/// [`APPLICATION_OUTLINE_ACTION`]: what the app's window shows now, as the lines of a view.
#[must_use]
pub fn outline_spec() -> crate::ActionSpec {
    let mut spec = crate::ActionSpec::typed_operation(
        APPLICATION_OUTLINE_ACTION,
        "Read the view",
        "Read the view",
        crate::OperationKind::View,
        APPLICATION_OUTLINE_TYPE,
        vec![
            crate::ActionParam::optional(
                OUTLINE_ANCHOR_PARAM,
                crate::ParamType::Text,
                "An item of the app's to show first: the anchor of a search hit, or an item \
                 resource's id",
            )
            .nullable(),
            crate::ActionParam::optional(
                "picture",
                crate::ParamType::Boolean,
                "Also answer the view as a picture of it, where its window is drawn (default: \
                 text only)",
            )
            .nullable(),
            crate::ActionParam::optional(
                "scale",
                crate::ParamType::Number,
                "Pixels per point of the picture, 0.5 to 2 (default 1)",
            )
            .nullable(),
        ],
        |args| {
            crate::EncodedAction::encode_operation(
                APPLICATION_OUTLINE_TYPE,
                &serde_json::json!({
                    "picture": args.boolean("picture"),
                    "scale": args.number("scale"),
                    OUTLINE_ANCHOR_PARAM: args.text(OUTLINE_ANCHOR_PARAM),
                }),
            )
            .unwrap_or_else(|_| crate::EncodedAction {
                type_id: APPLICATION_OUTLINE_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(crate::ActionKind::Read)
    .with_description(
        "Read what the app's window shows as lines: every label, value and control in reading \
         order, and the action each control runs, which is called by the address the line \
         names. With picture, also the view as a picture. With an anchor, shows that item first.",
    )
    // How an agent looks at what the person sees: never a row of the person's.
    .for_audience(crate::Audience::Agents);
    spec.type_mut().contract.result = crate::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

/// The item an [`APPLICATION_OUTLINE_TYPE`] payload asks to be shown first, when it asks for one.
#[must_use]
pub fn outline_anchor(payload: &[u8]) -> Option<String> {
    let args: serde_json::Value = serde_json::from_slice(payload).ok()?;
    args.get(OUTLINE_ANCHOR_PARAM)
        .and_then(serde_json::Value::as_str)
        .filter(|anchor| !anchor.is_empty())
        .map(str::to_owned)
}

/// What an [`APPLICATION_OUTLINE_TYPE`] payload asks besides the lines: a picture of the view,
/// and its scale.
///
/// # Errors
/// The payload is no JSON, or its scale is outside 0.5 to 2.
pub fn outline_asked(payload: &[u8]) -> Result<(bool, f64), String> {
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(payload).map_err(|error| error.to_string())?
    };
    let scale = args
        .get("scale")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(1.0);
    if !(0.5..=2.0).contains(&scale) {
        return Err("scale must be within 0.5..=2".into());
    }
    let picture = args
        .get("picture")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok((picture, scale))
}

#[cfg(test)]
mod tests {
    use super::link_scheme;

    #[test]
    fn a_link_names_its_scheme_and_nothing_else_does() {
        assert_eq!(link_scheme("geo:52.5,13.4").as_deref(), Some("geo"));
        assert_eq!(
            link_scheme("Settings://keyboard").as_deref(),
            Some("settings")
        );
        assert_eq!(
            link_scheme("web+search:cats").as_deref(),
            Some("web+search")
        );
        for none in [
            "/home/a/b.txt",
            "cafe",
            "geo:",
            "C:\\notes",
            "1x:y",
            ":a",
            "ht tp://x",
        ] {
            assert_eq!(link_scheme(none), None, "{none}");
        }
    }
}
