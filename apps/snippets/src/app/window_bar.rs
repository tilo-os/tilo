//! Snippets' controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle and Back, what the window shows as the
//! location (the open snippet, else the chosen place), New snippet (unless
//! the library is empty and the window offers it itself), and the open
//! snippet's favorite toggle, with Duplicate and Delete in the bar's menu.
//! The bar's search is the library's search; the window draws no header row.

use ui::widget::{NavPane, NavShell};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{Action, SnippetsApp};
use crate::tr;

pub(crate) const NEW_SNIPPET: &str = "snippets.new";
pub(crate) const FAVORITE: &str = "snippets.favorite";
pub(crate) const DUPLICATE: &str = "snippets.duplicate";
pub(crate) const DELETE: &str = "snippets.delete";

impl SnippetsApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let core = self.core.borrow();
        let one_at_a_time = self.shell.geometry().stack.len() > 1;
        let label = match (one_at_a_time, self.shell.front_pane()) {
            (true, NavPane::Sidebar) => None,
            (true, NavPane::Overview) => Some(core.selection_title()),
            _ => Some(core.open_title())
                .filter(|title| !title.is_empty())
                .or_else(|| Some(core.selection_title())),
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        // An empty library offers New snippet in the window itself; the bar
        // would only repeat it.
        let mut actions = Vec::new();
        if !core.library.snippets.is_empty() {
            actions.push(ToolbarAction::new(NEW_SNIPPET, "plus", tr("snippets-new")));
        }
        if let Some((id, open)) = core
            .open
            .as_ref()
            .and_then(|id| Some((id, core.library.get(id)?)))
        {
            actions.push(
                ToolbarAction::new(FAVORITE, "star", core.favorite_label(id)).on(open.favorite),
            );
            actions.push(
                ToolbarAction::new(DUPLICATE, "copy", tr("snippets-menu-duplicate")).in_menu(),
            );
            actions
                .push(ToolbarAction::new(DELETE, "trash-2", tr("snippets-menu-delete")).in_menu());
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the library's, applied with the window's other requests.
    /// Returns whether anything was asked.
    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            let open = self.core.borrow().open.clone();
            let action = match (id.as_str(), open) {
                (NEW_SNIPPET, _) => Action::NewSnippet,
                (FAVORITE, Some(open)) => Action::ToggleFavorite(open),
                (DUPLICATE, Some(open)) => Action::Duplicate(open),
                (DELETE, Some(open)) => Action::Delete(open),
                _ => continue,
            };
            self.requests.borrow_mut().push(action);
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
