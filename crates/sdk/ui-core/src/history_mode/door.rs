//! The window's History door and control: the one resource every History
//! control, shortcut, menu and palette row goes through to open History,
//! open it at a change, or hide it, and the button that presents it.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, ResourceSpec,
};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::i18n::I18nManager;
use crate::theme::Theme;
use crate::widget::window_header::HistoryControl;
use crate::widget::{Button, HistoryEntryId};

pub(crate) const HISTORY_OPERATION: &str = "tilo.host.history.view";
pub(crate) const HISTORY_RESOURCE: &str = "tilo.host:history";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HistoryCommand {
    Open,
    /// Open History with one change already selected.
    OpenAt {
        entry: String,
    },
    /// Close History.
    Hide,
}

/// The look of the window's History control: the History glyph as a Plain
/// icon button, announced and tooltipped as "History" with its shortcut,
/// reading as on while History shows. Every History host draws this one
/// control, wherever it stands.
pub(crate) fn history_control_face(id: &str, control: HistoryControl) -> Button {
    let platform = crate::platform::PlatformConventions::default();
    let shortcut = crate::platform::HostShortcut::History
        .shortcut(platform)
        .label(platform);
    Button::icon_only(id, crate::icons::Icon::History, "history-mode-title")
        .tooltip(format!(
            "{} ({shortcut})",
            I18nManager::lookup_plain("history-mode-title")
        ))
        .toggled(control.is_open())
}

/// The window's History control, bound to the History door: closed it
/// opens History, open it closes it, exactly as the History shortcut and
/// the palette do.
#[must_use]
pub(crate) fn history_control_button(id: &str, control: HistoryControl) -> Button {
    history_control_face(id, control).on_action(history_control_binding(control))
}

/// The History door a press of the window's History control goes through:
/// Open while History is closed, Hide while it shows.
///
/// # Panics
///
/// Never: Open and Hide are fixed unit operations whose bindings always
/// serialize.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "fixed unit View operations and their nullary bindings serialize infallibly"
)]
pub(crate) fn history_control_binding(control: HistoryControl) -> ActionBinding<()> {
    match control {
        HistoryControl::Closed => ActionBinding::unit_for(&open_resource(), "open"),
        HistoryControl::Open => ActionBinding::unit_for(&history_resource(true, true), "hide"),
    }
    .expect("unit history action")
}

/// The edge under a bar of window chrome: the bar stands on the content
/// ground and is told from the app by one hairline at its bottom, never by
/// a raised fill.
pub(crate) fn bar_edge(scene: &mut Scene, transform: Affine, theme: &Theme, bar: Rect) {
    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
    if rule <= 0.0 || bar.height() <= 0.0 {
        return;
    }
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        crate::surface::edge_on(theme.tokens.colors.surfaces.content),
        None,
        &Rect::new(bar.x0, bar.y1 - rule, bar.x1, bar.y1),
    );
}

#[expect(
    clippy::expect_used,
    reason = "fixed View command payloads contain only serializable Rust values"
)]
fn operation(id: &str, label: &str, command: &HistoryCommand) -> ActionSpec {
    ActionSpec::operation(
        id,
        I18nManager::lookup_plain(label),
        OperationKind::View,
        HISTORY_OPERATION,
        command,
    )
    .expect("history command serializes")
}
fn unavailable(code: &str, reason: &str) -> tilo_actions::Availability {
    tilo_actions::Availability::Unavailable {
        code: code.into(),
        reason: I18nManager::lookup_plain(reason),
    }
}

fn open_resource() -> ResourceSpec {
    ResourceSpec::new(
        HISTORY_RESOURCE,
        "tilo.host.history",
        I18nManager::lookup_plain("history-mode-title"),
    )
    // Named by what it does: a row reads "Show History", never the
    // resource's own name twice.
    .with_action(operation(
        "open",
        "host-history-show",
        &HistoryCommand::Open,
    ))
    .with_action(open_at_operation())
}

/// The stable History door, whichever History serves it: Open while there
/// is a document to show, and Hide while History is showing.
pub(crate) fn history_resource(can_open: bool, open: bool) -> ResourceSpec {
    let mut resource = open_resource();
    if !can_open {
        for action in &mut resource.actions {
            action.type_mut().contract.availability =
                unavailable("history_unavailable", "host-history-unavailable");
        }
    }
    if open {
        resource = resource.with_action(operation(
            "hide",
            "host-history-hide",
            &HistoryCommand::Hide,
        ));
    }
    resource
}

/// Open at one change: what a recent change in a History menu does. Rows
/// are the palette's own History view, so it is offered to callers only.
#[expect(
    clippy::expect_used,
    reason = "typed History command payload contains only a validated JSON string"
)]
fn open_at_operation() -> ActionSpec {
    ActionSpec::typed_operation(
        "open_at",
        I18nManager::lookup_plain("history-mode-title"),
        I18nManager::lookup_plain("history-mode-select-hint"),
        OperationKind::View,
        HISTORY_OPERATION,
        vec![
            ActionParam::required_owned(
                "entry",
                ParamType::Text,
                I18nManager::lookup_plain("history-mode-entry-ids"),
            )
            .with_schema(serde_json::json!({"pattern": "^[0-9a-f]{32}$"})),
        ],
        |args| {
            let entry = args.text("entry").unwrap_or_default().to_owned();
            EncodedAction::encode_operation(HISTORY_OPERATION, &HistoryCommand::OpenAt { entry })
                .expect("validated history entry serializes")
        },
    )
    // Looking at a past state changes this viewer's view, not the record:
    // a read, so a reader, a replay window and an agent can step History.
    .of_kind(tilo_actions::ActionKind::Read)
    .for_audience(tilo_actions::Audience::Agents)
}
#[cfg(any(feature = "native", test))]
pub(crate) fn encode_entry(id: HistoryEntryId) -> String {
    crate::widget::history_group::hex16(&id)
}
/// The change `value` names in its 32 lowercase hex digits, if it is one.
pub(crate) fn decode_entry(value: &str) -> Option<HistoryEntryId> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut bytes = [0; 16];
    for (slot, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(HistoryEntryId(bytes))
}
