//! The shell's own buttons as slots in the window's bar, for an app that declares its toolbar
//! there itself ([`NavShell::with_window_toolbar`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{InspectorPlacement, NavPane, NavShell, SidebarPlacement};
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
    /// The push stack's front screen, landed with no motion
    /// ([`WindowBarLink::stage_pane`]).
    pub(super) staged_pane: Cell<Option<NavPane>>,
    /// Put a floating sidebar away ([`WindowBarLink::put_drawer_away`]).
    pub(super) put_drawer_away: Cell<bool>,
    /// A notice the app asked the window's bar to show ([`WindowBarLink::toast`]).
    pub(super) toast: RefCell<Option<(String, crate::widget::toast::ToastKind)>>,
    slots: RefCell<WindowToolbar>,
    /// The inspector's frame as of the last layout, and what an
    /// [`super::InspectorLink`] asked of it.
    pub(super) inspector_frame: Cell<super::InspectorFrame>,
    pub(super) inspector_wanted: Cell<Option<bool>>,
    pub(super) inspector_detent: Cell<Option<crate::widget::SheetDetent>>,
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
    #[must_use]
    pub fn slots(&self) -> WindowToolbar {
        self.0.slots.borrow().clone()
    }

    #[must_use]
    pub fn is_sidebar_open(&self) -> bool {
        self.0.slots.borrow().sidebar == Some(true)
    }

    /// Show the sidebar open or closed at once, with no motion: what a
    /// screenshot scenario stages.
    pub fn stage_sidebar(&self, open: bool) {
        self.0.staged_sidebar.set(Some(open));
    }

    /// Show `pane` in front at once, with no motion, when the window shows
    /// one column at a time: what a screenshot scenario of an opened item
    /// stages. A docked layout shows every column already.
    pub fn stage_pane(&self, pane: NavPane) {
        self.0.staged_pane.set(Some(pane));
    }

    /// Put the sidebar away if it stands over the content, as a drawer or as the narrow window's
    /// first screen (the app opened something it would cover); a docked sidebar stays.
    pub fn put_drawer_away(&self) {
        self.0.put_drawer_away.set(true);
    }

    /// Show a transient notice in the window's bar, as [`NavShell::toast`] does: what an
    /// operation the app ran came to.
    pub fn toast(&self, text: impl Into<String>, kind: crate::widget::toast::ToastKind) {
        *self.0.toast.borrow_mut() = Some((text.into(), kind));
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
pub const WINDOW_SIDEBAR: &str = "nav-shell-sidebar";
pub const WINDOW_INSPECTOR: &str = "nav-shell-inspector";

impl NavShell {
    /// Run what the shell's buttons and a linked app asked for since the
    /// last pass. Returns whether anything changed.
    pub(super) fn take_requests(&mut self) -> bool {
        let floats = crate::window_toolbar::floats();
        let floats_changed =
            std::mem::replace(&mut self.inputs.borrow_mut().floats, floats) != floats;
        if floats_changed {
            self.structure_dirty = true;
            self.invalidate();
        }
        let mut acted = floats_changed | self.requests.toggle.replace(false);
        let toast = self.requests.toast.borrow_mut().take();
        if let Some((text, kind)) = toast {
            self.toast(text, kind);
            acted = true;
        }
        if acted {
            self.toggle_sidebar();
        }
        if self.requests.back.replace(false) {
            acted |= self.back();
        }
        if self.requests.inspector.replace(false) {
            self.toggle_inspector();
            acted = true;
        }
        if crate::window_toolbar::take_notice_press(super::notices::WINDOW_NOTICE_ACTION) {
            acted |= self.press_notice_action();
        }
        acted |= self.take_inspector_requests();
        if self.built
            && let Some(open) = self.requests.staged_sidebar.take()
        {
            if self.geometry.sidebar == SidebarPlacement::Screen {
                self.show(if open {
                    NavPane::Sidebar
                } else {
                    NavPane::Content
                });
            } else if self.is_sidebar_open() != open {
                self.toggle_sidebar();
            }
            self.snap();
            acted = true;
        }
        if self.built
            && let Some(pane) = self.requests.staged_pane.take()
        {
            self.show(pane);
            self.snap();
            acted = true;
        }
        // Asked before the window shows its sidebar (before its first layout, or while it floats
        // as a card), the ask waits for it: a window that opens or snaps narrow on what the app
        // opened never shows its sidebar over it.
        if self.geometry.sidebar != SidebarPlacement::None && self.requests.put_drawer_away.take() {
            match self.geometry.sidebar {
                SidebarPlacement::Drawer if self.is_sidebar_open() => {
                    self.toggle_sidebar();
                    acted = true;
                }
                SidebarPlacement::Screen if self.is_sidebar_open() => {
                    self.show(self.content_pane());
                    acted = true;
                }
                _ => {}
            }
        }
        acted
    }

    /// The app's toolbar is its window bar's slots ([`crate::window_toolbar`]): the shell draws
    /// no header row of its own, in a Tilo session or out of it.
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
    /// ([`crate::window_toolbar::WindowToolbar::sidebar`]) and runs it
    /// with [`Self::toggle_sidebar`]: the header row keeps no toggle of its own,
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

    /// Whether Back goes anywhere: the app's page stack
    /// ([`Self::with_back`], [`Self::set_can_go_back`]), a narrow
    /// window's push stack, or the inspector's sheet to lower or put away.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.geometry.back_column.is_some() || self.sheet_shown()
    }

    /// The shell's own slots for the window's bar ([`Self::with_window_toolbar`]): the sidebar
    /// toggle while there is a sidebar, Back while it goes anywhere (leading), and the
    /// inspector's toggle while there is an inspector (the last action).
    #[must_use]
    pub fn window_toolbar(&self) -> WindowToolbar {
        use crate::window_toolbar::ToolbarAction;
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let back = self.can_go_back();
        let on_sidebar_screen = self.geometry.sidebar == SidebarPlacement::Screen
            && self.front_pane() == NavPane::Sidebar;
        let toggle_yields = back
            && self.geometry.sidebar != SidebarPlacement::Docked
            && (on_sidebar_screen || !self.is_sidebar_open());
        let mut toolbar = WindowToolbar {
            sidebar: (self.has_sidebar() && !self.sidebar_folded() && !toggle_yields)
                .then(|| self.is_sidebar_open()),
            ..WindowToolbar::default()
        };
        if back {
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
