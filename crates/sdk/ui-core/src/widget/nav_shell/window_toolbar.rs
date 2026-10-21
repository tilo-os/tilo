//! The shell's own buttons as slots in the window's bar
//! ([`NavShell::with_window_toolbar`]).

use super::{InspectorPlacement, NavShell};

/// The action ids of the shell's own slots in the window's bar
/// ([`NavShell::window_toolbar`]).
pub const WINDOW_BACK: &str = "nav-shell-back";
pub const WINDOW_INSPECTOR: &str = "nav-shell-inspector";

impl NavShell {
    /// Whether Back goes anywhere: the app's page stack
    /// ([`Self::with_back`], [`Self::set_can_go_back`]) or a narrow
    /// window's push stack.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.geometry.back_column.is_some()
    }

    /// The shell's own slots for the window's bar
    /// ([`Self::with_window_toolbar`]): the sidebar toggle while there is a
    /// sidebar, Back while it goes anywhere (leading), and the inspector's
    /// toggle while there is an inspector (the last action). The app adds
    /// its location, views and actions; the inspector's toggle stays last.
    #[must_use]
    pub fn window_toolbar(&self) -> crate::window_toolbar::WindowToolbar {
        use crate::window_toolbar::{ToolbarAction, WindowToolbar};
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let mut toolbar = WindowToolbar {
            sidebar: self.has_sidebar().then(|| self.is_sidebar_open()),
            ..WindowToolbar::default()
        };
        if self.can_go_back() {
            toolbar.leading.push(ToolbarAction::new(
                WINDOW_BACK,
                "arrow-left",
                lookup("window-toolbar-back"),
            ));
        }
        if self.geometry.inspector != InspectorPlacement::None {
            let open = self.is_inspector_open();
            toolbar.actions.push(
                ToolbarAction::new(
                    WINDOW_INSPECTOR,
                    "panel-right",
                    lookup(if open {
                        "window-toolbar-hide-inspector"
                    } else {
                        "window-toolbar-show-inspector"
                    }),
                )
                .on(open),
            );
        }
        toolbar
    }

    /// Add the app's own actions to [`Self::window_toolbar`]'s, before the
    /// inspector's toggle, which stays the last of the row.
    pub fn push_window_actions(
        toolbar: &mut crate::window_toolbar::WindowToolbar,
        actions: impl IntoIterator<Item = crate::window_toolbar::ToolbarAction>,
    ) {
        let inspector = toolbar
            .actions
            .iter()
            .position(|action| action.id == WINDOW_INSPECTOR)
            .map(|at| toolbar.actions.remove(at));
        toolbar.actions.extend(actions);
        if let Some(inspector) = inspector {
            // The toggle stands in the row; the app's menu-only actions
            // follow it in the menu.
            let at = toolbar.row_actions();
            toolbar.actions.insert(at, inspector);
        }
    }

    /// Run what the person did in the window's bar on the shell's own
    /// slots. Returns whether it was one of them; the app handles the rest.
    pub fn window_toolbar_event(&mut self, event: &crate::window_toolbar::ToolbarEvent) -> bool {
        use crate::window_toolbar::ToolbarEvent;
        match event {
            ToolbarEvent::ToggleSidebar => self.toggle_sidebar(),
            ToolbarEvent::Back => {
                self.back();
            }
            ToolbarEvent::Action { id } if id == WINDOW_BACK => {
                self.back();
            }
            ToolbarEvent::Action { id } if id == WINDOW_INSPECTOR => self.toggle_inspector(),
            _ => return false,
        }
        true
    }
}
