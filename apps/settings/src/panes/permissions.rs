//! Permissions & Roles pane — an informational summary of the OS
//! permission model.
//!
//! The role + per-action permission editor lives in the separate
//! `permissions` service, which this standalone Settings binary does not
//! embed (to avoid a build dependency on a crate that's still in flight).
//! Rather than ship "Open editor" buttons that write a key nothing in this
//! app reads — a control that looks live but does nothing — this pane is a
//! truthful, read-only overview of what the editor governs and where
//! enforcement happens. Wiring the embedded editor is tracked as follow-up
//! work in the growth ledger.

use crate::widgets::{card, caption, cards, rows, value_row, El};
use ui::widget::{Text, Widget};

pub fn build() -> Box<dyn Widget> {
    let mut panel: Vec<El> = Vec::new();

    panel.push(card(
        "Roles & permissions",
        rows(vec![Box::new(
            Text::new(
                "Roles and per-action permissions govern what members of \
                 this workspace can do — inviting peers, installing apps, \
                 managing channels, and more. Roles are authored in the qOS \
                 permissions service and enforced by the compositor and each \
                 owning app.",
            )
            .with_size(12.0),
        )]),
    ));

    // A read-only snapshot of the kinds of permissions the model governs.
    panel.push(card(
        "What roles govern",
        rows(vec![
            value_row("Workspace", "Invite, remove, and manage members"),
            value_row("Apps", "Install, launch, and grant file access"),
            value_row("Collaboration", "Share workspaces and inject input"),
            value_row("Per-app actions", "e.g. manage channels in Chat"),
        ]),
    ));

    panel.push(card(
        "Where enforcement happens",
        rows(vec![caption(
            "Permissions are enforced by the compositor (file access, \
             collaboration) and by each owning app (its own actions), not by \
             this panel. This overview reflects the model those layers apply.",
        )]),
    ));

    cards(panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = if w.name() == name { 1 } else { 0 };
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    /// The pane is informational: it builds and carries no dead buttons that
    /// would write a key nothing reads.
    #[test]
    fn is_informational_with_no_dead_buttons() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        assert_eq!(
            count_named(pane.as_ref(), "Button"),
            0,
            "no non-functional buttons remain in the permissions overview"
        );
    }
}
