//! The shell's own buttons as slots in the window's bar
//! ([`NavShell::with_window_toolbar`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{InspectorPlacement, NavShell};
use crate::window_toolbar::{ToolbarEvent, WindowToolbar};

/// The shell's buttons, and an app inside it ([`WindowBarLink`]), report
/// presses here; the shell runs them at its next sync or update.
#[derive(Default)]
pub(super) struct Requests {
    pub(super) toggle: Cell<bool>,
    pub(super) back: Cell<bool>,
    pub(super) inspector: Cell<bool>,
    /// The sidebar open or closed, landed with no motion: a capture's
    /// staging ([`WindowBarLink::stage_sidebar`]).
    pub(super) staged_sidebar: Cell<Option<bool>>,
    /// Put a floating sidebar away ([`WindowBarLink::put_drawer_away`]).
    pub(super) put_drawer_away: Cell<bool>,
    /// The shell's own slots for the window's bar as of its last layout.
    slots: RefCell<WindowToolbar>,
}

/// The shell's own buttons in the window's bar, for an app that paints
/// inside the shell's content column and declares the bar itself: it
/// starts its declaration from [`Self::slots`] (the sidebar toggle, Back
/// while it goes anywhere, the inspector's toggle) and hands what the
/// person did in the bar to [`Self::event`] first, as an app that owns its
/// shell does with [`NavShell::window_toolbar`] and
/// [`NavShell::window_toolbar_event`].
#[derive(Clone)]
pub struct WindowBarLink(Rc<Requests>);

impl WindowBarLink {
    /// The shell's own slots, as it last laid out.
    #[must_use]
    pub fn slots(&self) -> WindowToolbar {
        self.0.slots.borrow().clone()
    }

    /// Whether the shell shows its sidebar, as it last laid out.
    #[must_use]
    pub fn is_sidebar_open(&self) -> bool {
        self.0.slots.borrow().sidebar == Some(true)
    }

    /// Show the sidebar open or closed at once, with no motion: what a
    /// screenshot scenario stages.
    pub fn stage_sidebar(&self, open: bool) {
        self.0.staged_sidebar.set(Some(open));
    }

    /// Put the sidebar away if it floats as a drawer over the content (the
    /// app opened something the drawer would cover); a docked sidebar
    /// stays.
    pub fn put_drawer_away(&self) {
        self.0.put_drawer_away.set(true);
    }

    /// Run what the person did in the window's bar on the shell's own
    /// slots. Returns whether it was one of them; the app handles the rest.
    pub fn event(&self, event: &ToolbarEvent) -> bool {
        let request = match event {
            ToolbarEvent::ToggleSidebar => &self.0.toggle,
            ToolbarEvent::Back => &self.0.back,
            ToolbarEvent::Action { id } if id == WINDOW_BACK => &self.0.back,
            ToolbarEvent::Action { id } if id == WINDOW_INSPECTOR => &self.0.inspector,
            _ => return false,
        };
        request.set(true);
        true
    }
}

/// The action ids of the shell's own slots in the window's bar
/// ([`NavShell::window_toolbar`]).
pub const WINDOW_BACK: &str = "nav-shell-back";
pub const WINDOW_INSPECTOR: &str = "nav-shell-inspector";

impl NavShell {
    /// The app's toolbar is its window bar's slots ([`crate::window_toolbar`]):
    /// the shell draws no header row of its own, in a Tilo session or out of
    /// it. Its own buttons (the sidebar toggle, Back, the inspector toggle)
    /// are slots too: the app starts its declaration from
    /// [`Self::window_toolbar`] and hands what the person did in the bar to
    /// [`Self::window_toolbar_event`] first. Column titles and header
    /// actions given to the shell are not shown: the app declares its
    /// location and actions as slots.
    #[must_use]
    pub fn with_window_toolbar(self) -> Self {
        {
            let mut inputs = self.inputs.borrow_mut();
            inputs.toolbar_in_bar = true;
            inputs.toggle_in_bar = true;
        }
        self
    }

    /// The app shows the sidebar toggle in the window's bar
    /// ([`crate::window_toolbar::WindowToolbar::sidebar`]) and runs it with
    /// [`Self::toggle_sidebar`]: the header row keeps no toggle of its own,
    /// and stands only for what else it holds.
    pub fn set_sidebar_toggle_in_window_bar(&mut self, on: bool) {
        let changed = {
            let mut inputs = self.inputs.borrow_mut();
            std::mem::replace(&mut inputs.toggle_in_bar, on) != on
        };
        if changed {
            self.structure_dirty = true;
            self.invalidate();
        }
    }

    /// Put the header row in the window's bar: while the window is
    /// maximized, the row hides with the window's controls and appears
    /// with them over the top of the content when the person reaches for
    /// the bar (`widget::top_edge_reveal`), as one bar. In a tiled window
    /// the row stands above the content as usual.
    #[must_use]
    pub fn with_header_in_window_bar(self) -> Self {
        self.inputs.borrow_mut().header_in_bar = true;
        self
    }

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
    pub fn window_toolbar(&self) -> WindowToolbar {
        use crate::window_toolbar::ToolbarAction;
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

    /// The link an app painting inside the content column uses to put the
    /// shell's own buttons in the window's bar ([`WindowBarLink`]). Use with
    /// [`Self::with_window_toolbar`].
    #[must_use]
    pub fn window_bar_link(&self) -> WindowBarLink {
        WindowBarLink(Rc::clone(&self.requests))
    }

    /// Keep the shell's own slots where its [`WindowBarLink`] reads them.
    pub(super) fn publish_window_slots(&self) {
        let slots = self.window_toolbar();
        if *self.requests.slots.borrow() != slots {
            *self.requests.slots.borrow_mut() = slots;
        }
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
    pub fn window_toolbar_event(&mut self, event: &ToolbarEvent) -> bool {
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
