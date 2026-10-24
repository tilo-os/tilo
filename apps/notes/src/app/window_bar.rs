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
    /// The slots for the note shown (`title`, and the anchor `open` shows it
    /// at; `None`: no note is open).
    fn window_toolbar(note: Option<(&str, String)>, sidebar_open: bool) -> WindowToolbar {
        let title = note.as_ref().map(|(title, _)| *title);
        WindowToolbar {
            sidebar: Some(sidebar_open),
            navigation: None,
            location: note.map(|(title, anchor)| ToolbarLocation {
                label: title.to_owned(),
                placeholder: ui::i18n!("notes-title-placeholder"),
                anchor: Some(anchor),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction {
                    id: NEW_NOTE.into(),
                    icon: "plus".into(),
                    name: ui::i18n!("notes-affordance-new-note"),
                    enabled: true,
                    ..ToolbarAction::default()
                },
                ToolbarAction {
                    id: NOTE_ACTIONS.into(),
                    icon: "more-horizontal".into(),
                    name: ui::i18n!("notes-affordance-note-actions"),
                    enabled: title.is_some(),
                    ..ToolbarAction::default()
                },
            ],
            ..WindowToolbar::default()
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
        let anchor = self.main.page_id().map(crate::actions::page_resource_id);
        let open = self.shell.is_sidebar_open();
        let key = (title.zip(anchor), open);
        if self.declared_bar.as_ref() != Some(&key) {
            let note = key
                .0
                .as_ref()
                .map(|(title, anchor)| (title.as_str(), anchor.clone()));
            ui::window_toolbar::declare(Some(Self::window_toolbar(note, open)));
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

#[cfg(test)]
mod tests {
    use super::NotesApp;
    use ui::framework::testing::TestApp;

    /// Notes in its window bar at `size`, on `scenario`'s note.
    fn notes_at(scenario: &str, size: (u32, u32)) -> TestApp<crate::NotesShell> {
        let mut app =
            TestApp::<crate::NotesShell>::capture_at(Some(scenario), size).expect("Notes opens");
        app.harness().present_in_window_bar();
        app.settle();
        app
    }

    /// The header row's height and the note title's top, as laid out.
    fn top_of_note(app: &mut TestApp<crate::NotesShell>) -> (f64, f64) {
        app.with_surface::<NotesApp, _>(|notes| {
            (
                notes.shell.geometry().header_height,
                notes.main.title_rect_for_test().y0,
            )
        })
        .expect("the notes surface")
    }

    /// A compact window has nothing for a header row to hold: the page
    /// tree's header is in its drawer, the note's in the window's bar. The
    /// note starts right under the bar, or under its notice's strip, and a
    /// wide window keeps the row the page tree's header stands in.
    #[test]
    fn a_compact_note_starts_right_under_the_window_bar() {
        let gutter = f64::from(ui::theme::Theme::dark().tokens.spacing.rhythm.group);
        for scenario in ["status", "import_success"] {
            let mut compact = notes_at(scenario, (420, 800));
            let (row, title) = top_of_note(&mut compact);
            assert!(
                row.abs() < 1e-9,
                "{scenario}: no header row at 420 px, got {row}"
            );
            assert!(
                (title - gutter).abs() < 0.5,
                "{scenario}: the title stands one gutter under the bar, at {title}"
            );
            let mut wide = notes_at(scenario, (1280, 800));
            let (row, title) = top_of_note(&mut wide);
            assert!(
                row > 0.0,
                "{scenario}: the page tree's header row stays wide"
            );
            assert!((title - (row + gutter)).abs() < 0.5, "{scenario}: {title}");
        }
    }
}
