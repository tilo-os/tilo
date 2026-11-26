//! `outline`: what an app's window shows now, as the lines an agent reads.
//!
//! The lines are the view's own semantic tree ([`ui_core::semantic_outline`]), read from the
//! widgets the window shows now, so a change the person makes — or an agent's own action —
//! shows in the next outline. Every host answers it: an app running in a page shows its
//! lines as one running on a computer does. A picture of the view is drawn where the host
//! can draw one ([`PICTURE_ASKED`]); a page cannot read a frame back, so it says so instead
//! of leaving the caller wondering.

use tilo_actions::ActionOutput;
use tilo_ui_base::widget::Widget;
use ui_core::automerge::AutoCommit;
use ui_core::automerge::transaction::Transactable as _;

/// The `outline` payload's parameter naming an item to show before reading.
pub(crate) const ANCHOR_PARAM: &str = "anchor";

/// The item a payload asks to be shown first, when it asks for one.
pub(crate) fn rendered_anchor(payload: &[u8]) -> Option<String> {
    let args: serde_json::Value = serde_json::from_slice(payload).ok()?;
    args.get(ANCHOR_PARAM)
        .and_then(serde_json::Value::as_str)
        .filter(|anchor| !anchor.is_empty())
        .map(str::to_owned)
}

/// `outline`: what the app's window shows now, as the lines of a view.
pub(crate) fn spec() -> tilo_actions::ActionSpec {
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "outline",
        "Read the view",
        "Read the view",
        tilo_actions::OperationKind::View,
        super::OUTLINE_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                ANCHOR_PARAM,
                tilo_actions::ParamType::Text,
                "An item of the app's to show first: the anchor of a search hit, or an item \
                 resource's id",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "picture",
                tilo_actions::ParamType::Boolean,
                "Also answer the view as a picture of it (default: text only)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "scale",
                tilo_actions::ParamType::Number,
                "Pixels per point of the picture, 0.5 to 2 (default 1)",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                super::OUTLINE_TYPE,
                &serde_json::json!({
                    "picture": args.boolean("picture"),
                    "scale": args.number("scale"),
                    ANCHOR_PARAM: args.text(ANCHOR_PARAM),
                }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: super::OUTLINE_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "Read what the app's window shows as lines: every label, value and control in reading \
         order, each control's action and where it stands. With picture, also the view as a \
         picture. With an anchor, shows that item first.",
    )
    // How an agent looks at what the person sees: never a row of the person's.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

/// What the `outline` payload asks besides the lines: a picture of the view, and its scale.
pub(crate) fn asked(payload: &[u8]) -> Result<(bool, f64), String> {
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

/// What `content` shows now, as the operation answers it, with no picture of it.
pub(crate) fn answer(
    content: &dyn Widget,
    doc: &AutoCommit,
    theme: &tilo_ui_base::Theme,
) -> ActionOutput {
    let outline = ui_core::semantic_outline::outline(content);
    let rect = content.get_rect();
    let value = serde_json::json!({
        "outline": outline.text,
        "tree": outline.tree,
        "nodes": outline.nodes,
        "omitted": outline.omitted,
        "viewport": [rect.width().round(), rect.height().round()],
        // Where the view sits in the app's window, in window pixels: a point of
        // a line's `rect` plus this is where it stands for a person.
        "window_origin": [rect.x0.round(), rect.y0.round()],
        "theme": theme.name,
        "revision": revision(doc),
    });
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

/// The document's heads, as a revision a caller compares: the version of what the view shows.
pub(crate) fn revision(doc: &AutoCommit) -> String {
    let mut heads: Vec<String> = doc
        .base_heads()
        .iter()
        .map(|head| head.to_string().chars().take(12).collect())
        .collect();
    heads.sort();
    heads.join("+")
}
