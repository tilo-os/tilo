//! Snippets' controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle and Back, what the window shows as the
//! location (the open snippet, else the chosen place), New snippet (unless
//! the library is empty and the window offers it itself), and the open
//! snippet's favorite toggle, with Duplicate and Delete in the bar's menu.
//! The bar's search is the library's search; the window draws no header row.

use ui::framework::{BarActions, BarCommands};
use ui::widget::{NavPane, NavShell};
use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

use super::{Action, SnippetsWindow};
use crate::model::Snippets;
use crate::tr;

/// The bar's entries: the library's verbs, and the commands that open
/// what they make.
#[derive(Default)]
pub(super) struct Bar {
    pub(super) actions: Option<BarActions<Snippets>>,
    commands: BarCommands<Action>,
}

impl SnippetsWindow {
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
        let commands = &self.bar.commands;
        // An empty library offers New snippet in the window itself; the bar
        // would only repeat it.
        let mut actions = Vec::new();
        if !core.library.snippets.is_empty() {
            actions.push(commands.entry("plus", tr("snippets-new"), Action::NewSnippet));
        }
        if let Some((id, open)) = core
            .open
            .as_ref()
            .and_then(|id| Some((id, core.library.get(id)?)))
        {
            let favorite = core.favorite_label(id);
            actions.push(
                commands
                    .entry("star", favorite, Action::ToggleFavorite(id.clone()))
                    .on(open.favorite),
            );
            let duplicate = tr("snippets-menu-duplicate");
            actions.push(
                commands
                    .entry("copy", duplicate, Action::Duplicate(id.clone()))
                    .in_menu(),
            );
            if let Some(bar) = &self.bar.actions {
                actions.push(
                    ToolbarAction {
                        name: tr("snippets-menu-delete"),
                        ..bar.action(Snippets::DELETE.with(id.clone()))
                    }
                    .in_menu(),
                );
            }
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
            if self.shell.window_toolbar_event(&event)
                || self
                    .bar
                    .actions
                    .as_ref()
                    .is_some_and(|bar| bar.press(&event))
            {
                continue;
            }
            if let Some(action) = self.bar.commands.pressed(&event) {
                self.requests.borrow_mut().push(action);
            }
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
