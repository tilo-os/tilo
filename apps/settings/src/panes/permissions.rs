//! Permissions & Roles pane — the entry-point seam to the OS permission
//! editor.
//!
//! The role + per-action permission editor lives in the separate
//! `permissions` crate (built in parallel). To avoid a hard build
//! dependency on a crate that's still in flight, this pane provides the
//! *entry point* rather than embedding the editor directly: a summary of
//! what the editor governs plus "Open editor" affordances for the Roles
//! and Members views.
//!
//! ## The seam
//!
//! The `permissions` crate exposes the integration contract Settings is
//! built to call:
//!
//! ```ignore
//! use permissions::seam::{open_editor, EditorIntent};
//! let view = open_editor(EditorIntent::roles(None));   // -> Box<EditorView>: Widget
//! // drop `view` straight into this pane's body.
//! ```
//!
//! When the `permissions` crate is wired into the Settings binary, the
//! single change is: replace [`build`]'s body with the embedded
//! `open_editor(...)` view, keyed off which "Open" affordance was pressed.
//! Until then the buttons carry CRDT keys ([`OPEN_ROLES_KEY`],
//! [`OPEN_MEMBERS_KEY`]); a host that *does* depend on `permissions` reads
//! those keys and swaps in the editor. The contract is intentionally a
//! data seam (intents + keys), not a compile-time coupling.

use crate::widgets::{card, caption, rows, value_row};
use ui::widget::{Button, Column, Row, Spacer, Text, Widget};

/// CRDT key the "Open roles editor" affordance writes. A host that embeds
/// the `permissions` crate watches this to mount `open_editor(roles)`.
pub const OPEN_ROLES_KEY: &str = "permissions.open_roles";
/// CRDT key for the "Open members" affordance.
pub const OPEN_MEMBERS_KEY: &str = "permissions.open_members";

pub fn build() -> Box<dyn Widget> {
    let mut col = Column::new().with_intrinsic_height();

    col = col.push(card(
        "Roles & permissions",
        rows(vec![
            Box::new(
                Text::new(
                    "Roles and per-action permissions govern what members \
                     of this workspace can do — inviting peers, installing \
                     apps, managing channels, and more. Open the editor to \
                     author roles and assign them.",
                )
                .with_size(12.0),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.permissions.open_roles",
                        "\u{f0bc6}  Open roles editor",
                        Some(OPEN_ROLES_KEY),
                    )))
                    .push(Box::new(Button::new(
                        "settings.permissions.open_members",
                        "\u{f0849}  Members",
                        Some(OPEN_MEMBERS_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ]),
    ));

    // A read-only snapshot of the kinds of permissions the editor manages,
    // so the panel is informative before the editor is wired in.
    col = col.push(card(
        "What the editor governs",
        rows(vec![
            value_row("Workspace", "Invite, remove, and manage members"),
            value_row("Apps", "Install, launch, and grant file access"),
            value_row("Collaboration", "Share clusters and inject input"),
            value_row("Per-app actions", "e.g. manage channels in Chat"),
        ]),
    ));

    col = col.push(card(
        "Integration",
        rows(vec![caption(
            "The editor is provided by the qOS permissions service. This \
             panel opens it in place; enforcement is handled by the \
             compositor and each owning app, not here.",
        )]),
    ));

    Box::new(col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    /// Buttons in `ui` expose their *label* as `test_id`, so we match on
    /// the label substring the entry-point buttons carry.
    fn has_button_labelled(w: &dyn Widget, needle: &str) -> bool {
        if w.name() == "Button" {
            if let Some(label) = w.test_id() {
                if label.contains(needle) {
                    return true;
                }
            }
        }
        w.children().iter().any(|c| has_button_labelled(*c, needle))
    }

    #[test]
    fn exposes_open_editor_entry_points() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        assert!(
            has_button_labelled(pane.as_ref(), "roles editor"),
            "roles entry point present"
        );
        assert!(
            has_button_labelled(pane.as_ref(), "Members"),
            "members entry point present"
        );
    }
}
