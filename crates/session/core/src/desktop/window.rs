//! What a window the session does not run can be asked of: its two read
//! actions, `look` and `read`.
//!
//! A window one of the session's own apps draws publishes what can be seen
//! and read of it itself, from its own record. An X11 or Wayland window of
//! another desktop has no surface of ours to ask, so the shell publishes the
//! two actions that only read: [`look`] pictures the window as it shows now,
//! [`read`] says what it calls itself. Neither acts on the window, and no
//! other action of the shell's window resource does either: a person works in
//! a foreign window with its own keyboard and pointer, where its own
//! accessibility tree and its own shortcuts are, never by an address.
//!
//! Both are [`tilo_actions::ActionKind::Read`], and offered to agents: the person already
//! sees the window, and what an agent needs is a way to see it too.

use tilo_actions::{ActionSpec, ResultSpec};
use tilo_desktop_model::Layout;
use tilo_desktop_model::layout::TileContent;
use tilo_resource::Effect as _;

/// `look` 's operation type: the window as a picture.
pub const LOOK: &str = "so.tilo.desktop.window.look";

/// `read` 's operation type: what the window says of itself.
pub const READ: &str = "so.tilo.desktop.window.read";

/// The address of tab `tab` 's resource, which the shell publishes as the window's own:
/// `desktop:tab:<tab>` .
#[must_use]
pub fn resource(tab: u64) -> String {
    format!("{RESOURCE_PREFIX}{tab}")
}

/// The prefix every window's resource address has, then its tab.
pub const RESOURCE_PREFIX: &str = "desktop:tab:";

/// The tab the window resource `resource` is of, when it is one of the shell's.
#[must_use]
pub fn resource_tab(resource: &str) -> Option<u64> {
    resource.strip_prefix(RESOURCE_PREFIX)?.parse().ok()
}

/// A window the shell shows that this session does not run: what `look` and `read` answer with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownWindow {
    /// Its title, as the shell names it in the bar, the strip and the catalogue.
    pub title: String,
    /// The app that showed it: its Wayland app id or X11 class.
    pub app_id: String,
    /// The window as the compositor knows it: what a picture of it is taken by.
    pub window: u64,
    /// The workspace it stands in, which its picture is kept as content of.
    pub workspace: String,
}

/// The window the shell shows in tab `tab` , when it is one this session does not run — a foreign
/// X11 or Wayland window — which the shell publishes the read actions of.
#[must_use]
pub fn shown(tree: &Layout, tab: u64) -> Option<ShownWindow> {
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

/// Looking at a window of a program on another computer, as a declared view.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.desktop.window.look")]
pub enum LookWindow {
    /// Look at the window
    #[effect(view, audience = "agents", label = "shell-look-window-action")]
    Look,
}

/// `look` : the window as it shows now, named `label` for the person reading the catalogue.
#[must_use]
pub fn look(label: String) -> ActionSpec {
    let mut spec = LookWindow::Look.bound();
    spec.type_mut().label = label;
    spec.type_mut().description = Some(crate::desktop::i18n::t("shell-look-window-detail"));
    result(spec)
}

/// Reading a window of a program on another computer, as a declared view.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.desktop.window.read")]
pub enum ReadWindow {
    /// Read the window
    #[effect(view, audience = "agents", label = "shell-read-window-action")]
    Read,
}

/// `read` : what the window says of itself — its title and the app it belongs to — named `label`
/// for the person reading the catalogue.
#[must_use]
pub fn read(label: String) -> ActionSpec {
    let mut spec = ReadWindow::Read.bound();
    spec.type_mut().label = label;
    spec.type_mut().description = Some(crate::desktop::i18n::t("shell-read-window-detail"));
    result(spec)
}

/// `spec` answering with an object, as both actions do.
fn result(mut spec: ActionSpec) -> ActionSpec {
    spec.type_mut().contract.result = ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two reads of a window this desktop only shows are for whoever
    /// cannot see it: never a person's palette, always an agent's listing,
    /// and they read rather than act.
    #[test]
    fn a_foreign_window_is_read_for_agents_alone() {
        for spec in [look("Report".to_owned()), read("Report".to_owned())] {
            assert_eq!(
                spec.audience(),
                tilo_actions::Audience::Agents,
                "{} is offered to whoever cannot see the window otherwise",
                spec.id
            );
            assert!(!spec.reaches_people(), "{} never lists for people", spec.id);
            assert!(spec.reaches_agents(), "{} stays a way to read it", spec.id);
            assert_eq!(
                spec.action_type().contract.kind,
                tilo_actions::ActionKind::Read,
                "{} reads and nothing else",
                spec.id
            );
        }
    }
}
