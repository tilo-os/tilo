//! Remote's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle, what the content shows as the location (the
//! list, the form, or the session's machine), and the screen's actions: New
//! connection on the list; Back (leaving the form) and Save on the form; a
//! live session's Fit to window or Actual size, Full screen and Disconnect.
//! The bar's search is the connections' search; the window draws no header
//! row and the form no title of its own.

use ui::i18n::I18nManager;
use ui::widget::NavShell;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{Action, Content, Destination, RemoteApp};

impl RemoteApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let name = |id| self.workspace.get(id).map(|c| c.name.clone());
        let bar = &self.bar;
        let (label, actions) = match self.content() {
            Content::List {
                destination,
                has_connections,
                ..
            } => (
                Some(String::from(match destination {
                    Destination::Pinned => "Pinned connections",
                    _ => "Connections",
                })),
                if has_connections {
                    vec![bar.entry("plus", "New connection", Action::NewConnection)]
                } else {
                    Vec::new()
                },
            ),
            Content::Form(model) => {
                toolbar.leading.push(
                    bar.entry(
                        "arrow-left",
                        I18nManager::lookup_plain("window-toolbar-back"),
                        Action::Back,
                    )
                    .navigating(),
                );
                (
                    Some(String::from(if model.editing.is_some() {
                        "Edit connection"
                    } else {
                        "New connection"
                    })),
                    vec![bar.entry("check", "Save", Action::SaveForm)],
                )
            }
            Content::Surface(id) => {
                let framebuffer = self
                    .workspace
                    .get(id)
                    .is_some_and(|c| c.kind.streams_framebuffer());
                (name(id), self.session_actions(framebuffer))
            }
            Content::Status { id, .. } => {
                let actions = if self.session.borrow().live_active() {
                    self.session_actions(false)
                } else {
                    Vec::new()
                };
                (name(id), actions)
            }
            Content::Missing => (None, Vec::new()),
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// A live session's actions: fitting and filling the window with a
    /// remote desktop, and ending the session.
    fn session_actions(&self, framebuffer: bool) -> Vec<ToolbarAction> {
        let mut actions = Vec::new();
        if framebuffer {
            let (icon, name) = if self.session.borrow().scale_to_fit {
                ("square", "Actual size")
            } else {
                ("minimize-2", "Fit to window")
            };
            actions.push(self.bar.entry(icon, name, Action::ToggleScale));
            actions.push(
                self.bar
                    .entry("maximize-2", "Full screen", Action::ToggleFullscreen),
            );
        }
        actions.push(self.bar.entry("unlink", "Disconnect", Action::Disconnect));
        actions
    }

    /// Queue what the person did in the window's bar as the action its
    /// control would have, after the shell's own slots. Back on the form
    /// leaves it.
    pub(super) fn take_window_toolbar_events(&mut self) {
        for event in ui::window_toolbar::take_events() {
            let on_form = matches!(self.content(), Content::Form(_));
            let action = match &event {
                ToolbarEvent::Back if on_form => Action::Back,
                _ if self.shell.window_toolbar_event(&event) => continue,
                _ => match self.bar.pressed(&event) {
                    Some(Action::Back) if !on_form => continue,
                    Some(action) => action,
                    None => continue,
                },
            };
            self.requests.borrow_mut().push(action);
        }
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}
