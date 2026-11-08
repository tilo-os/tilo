//! What a window the session does not run can be asked of: its two read
//! actions, `look` and `read`.
//!
//! A window one of the session's own apps draws publishes what can be seen
//! and read of it itself, from its own record. An X11 or Wayland window of
//! another desktop has no surface of ours to ask, so the desk publishes the
//! two actions that only read: [`look`] pictures the window as it shows now,
//! [`read`] says what it calls itself. Neither acts on the window, and no
//! other action of the desk's window resource does either: a person works in
//! a foreign window with its own keyboard and pointer, where its own
//! accessibility tree and its own shortcuts are, never by an address.
//!
//! Both are [`ActionKind::Read`], and offered to agents: the person already
//! sees the window, and what an agent needs is a way to see it too.

use tilo_actions::{
    ActionArgs, ActionKind, ActionSpec, Audience, EncodedAction, OperationKind, ResultSpec,
};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::layout::TileContent;

/// `look` 's operation type: the window as a picture.
pub const LOOK: &str = "so.tilo.desktop.window.look";

/// `read` 's operation type: what the window says of itself.
pub const READ: &str = "so.tilo.desktop.window.read";

/// The address of tab `tab` 's resource, which the desk publishes as the window's own:
/// `desktop:tab:<tab>` .
#[must_use]
pub fn resource(tab: u64) -> String {
    format!("{RESOURCE_PREFIX}{tab}")
}

/// The prefix every window's resource address has, then its tab.
pub const RESOURCE_PREFIX: &str = "desktop:tab:";

/// The tab the window resource `resource` is of, when it is one of the desk's.
#[must_use]
pub fn resource_tab(resource: &str) -> Option<u64> {
    resource.strip_prefix(RESOURCE_PREFIX)?.parse().ok()
}

/// A window the desk shows that this session does not run: what `look` and `read` answer with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownWindow {
    /// Its title, as the desk names it in the bar, the strip and the catalogue.
    pub title: String,
    /// The app that showed it: its Wayland app id or X11 class.
    pub app_id: String,
    /// The window as the compositor knows it: what a picture of it is taken by.
    pub window: u64,
    /// The workspace it stands in, which its picture is kept as content of.
    pub workspace: String,
}

/// The window the desk shows in tab `tab` , when it is one this session does not run — a foreign
/// X11 or Wayland window — which the desk publishes the read actions of.
#[must_use]
pub fn shown(tree: &DesktopTree, tab: u64) -> Option<ShownWindow> {
    tree.workspaces.iter().find_map(|workspace| {
        let found = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|shown| shown.id == tab)?;
        let TileContent::Foreign { app_id, surface_id } = &found.tile else {
            return None;
        };
        Some(ShownWindow {
            title: tilo_desktop_search::names::window_name(app_id, &found.title),
            app_id: app_id.clone(),
            window: *surface_id,
            workspace: workspace.id().to_string(),
        })
    })
}

/// `look` : the window as it shows now, named `label` for the person reading the catalogue.
#[must_use]
pub fn look(label: String) -> ActionSpec {
    let spec = ActionSpec::typed_operation(
        "look",
        label,
        "See the window as a picture",
        OperationKind::View,
        LOOK,
        Vec::new(),
        |_: &ActionArgs| EncodedAction {
            type_id: LOOK.into(),
            payload: b"{}".to_vec(),
        },
    )
    .of_kind(ActionKind::Read)
    .with_description(
        "See a window this desktop does not run as it shows now: answers with the picture, \
         kept as content of the workspace the window stands in.",
    )
    .for_audience(Audience::Agents);
    result(spec)
}

/// `read` : what the window says of itself — its title and the app it belongs to — named `label`
/// for the person reading the catalogue.
#[must_use]
pub fn read(label: String) -> ActionSpec {
    let spec = ActionSpec::typed_operation(
        "read",
        label,
        "What the window calls itself",
        OperationKind::View,
        READ,
        Vec::new(),
        |_: &ActionArgs| EncodedAction {
            type_id: READ.into(),
            payload: b"{}".to_vec(),
        },
    )
    .of_kind(ActionKind::Read)
    .with_description(
        "What a window this desktop does not run says of itself: its title and the app it \
         belongs to.",
    )
    .for_audience(Audience::Agents);
    result(spec)
}

/// `spec` answering with an object, as both actions do.
fn result(mut spec: ActionSpec) -> ActionSpec {
    spec.type_mut().contract.result = ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}
