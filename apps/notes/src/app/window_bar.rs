//! The note's header as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! While the desktop's bar hosts them, the content header leaves the window
//! and the bar shows the page tree's toggle, the note as the window's location, then New note and
//! the note's actions; search is the bar's own. The page tree keeps its
//! header. Outside a Tilo session the header stays in the window.

use std::rc::Rc;

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::NotesApp;

/// The action ids the note's slots answer to.
const NEW_NOTE: &str = "new-note";
const NOTE_ACTIONS: &str = "note-actions";

impl NotesApp {
    /// The slots for the note shown (`title`; `None`: no note is open).
    fn window_toolbar(title: Option<&str>, sidebar_open: bool) -> WindowToolbar {
        WindowToolbar {
            sidebar: Some(sidebar_open),
            navigation: None,
            location: title.map(|title| ToolbarLocation {
                label: title.to_owned(),
                placeholder: ui::i18n!("notes-title-placeholder"),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction {
                    id: NEW_NOTE.into(),
                    icon: "plus".into(),
                    name: ui::i18n!("notes-affordance-new-note"),
                    enabled: true,
                    on: false,
                },
                ToolbarAction {
                    id: NOTE_ACTIONS.into(),
                    icon: "more-horizontal".into(),
                    name: ui::i18n!("notes-affordance-note-actions"),
                    enabled: title.is_some(),
                    on: false,
                },
            ],
        }
    }

    /// Move the content header into the window's bar while it hosts the
    /// slots and back when it does not, tell the bar the slots, and turn
    /// what the person did there into the header's requests. Returns
    /// whether anything changed.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        let hosted = ui::window_toolbar::is_hosted();
        let mut changed = false;
        if hosted != self.header_in_bar {
            self.header_in_bar = hosted;
            self.shell.set_sidebar_toggle_in_window_bar(hosted);
            let actions = if hosted {
                Vec::new()
            } else {
                Self::content_header_actions(&self.header)
            };
            self.shell.set_content_header("", actions);
            if !hosted {
                ui::window_toolbar::declare(None);
                self.declared_bar = None;
            }
            changed = true;
        }
        if !hosted {
            return changed;
        }
        let title = self.main.title().map(str::to_owned);
        let open = self.shell.is_sidebar_open();
        let key = (title, open);
        if self.declared_bar.as_ref() != Some(&key) {
            ui::window_toolbar::declare(Some(Self::window_toolbar(key.0.as_deref(), open)));
            self.declared_bar = Some(key);
        }
        let header = Rc::clone(&self.header);
        for event in ui::window_toolbar::take_events() {
            match event {
                ToolbarEvent::ToggleSidebar => self.shell.toggle_sidebar(),
                ToolbarEvent::Action { id } => match id.as_str() {
                    NEW_NOTE => header.new_note.set(true),
                    NOTE_ACTIONS => header.note_menu.set(true),
                    _ => {}
                },
                _ => continue,
            }
            changed = true;
        }
        changed
    }
}
