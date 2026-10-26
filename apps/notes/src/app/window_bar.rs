//! The note's header as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! While the desktop's bar hosts them, the content header leaves the window
//! and the bar shows the page tree's toggle, the note as the window's location, then New note and
//! the note's actions; search is the bar's own. The page tree's header
//! keeps only its title then, so New note stands once, in the bar. Outside
//! a Tilo session both headers stay in the window, New note in the tree's.

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
            let (tree_actions, actions) = if hosted {
                (Vec::new(), Vec::new())
            } else {
                (
                    Self::page_tree_header_actions(&self.header),
                    Self::content_header_actions(&self.header),
                )
            };
            // window-bar: fallback, the bar holds New note while it hosts the window
            self.shell
                .set_sidebar_header(ui::i18n!("notes-title"), tree_actions);
            // window-bar: fallback, the bar holds the note's actions while it hosts the window
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
    use super::{NEW_NOTE, NotesApp};
    use ui::framework::testing::TestApp;

    /// Notes in its window bar at `size`, on `scenario`'s note.
    fn notes_at(scenario: &str, size: (u32, u32)) -> TestApp<crate::NotesShell> {
        let mut app =
            TestApp::<crate::NotesShell>::capture_at(Some(scenario), size).expect("Notes opens");
        app.harness().present_in_window_bar();
        app.settle();
        app
    }

    /// The page tree's header, the note's header and where the note's view
    /// starts, as laid out.
    fn tops(app: &mut TestApp<crate::NotesShell>) -> (f64, f64, f64) {
        app.with_surface::<NotesApp, _>(|notes| {
            let g = notes.shell.geometry();
            (
                g.sidebar_header().height(),
                g.content_header().height(),
                g.content_body().y0,
            )
        })
        .expect("the notes surface")
    }

    /// The note title's top, as laid out.
    fn title_top(app: &mut TestApp<crate::NotesShell>) -> f64 {
        app.with_surface::<NotesApp, _>(|notes| notes.main.title_rect_for_test().y0)
            .expect("the notes surface")
    }

    /// The window bar holds the note's controls, so the note has no header
    /// row of its own at any width: it starts right under the bar, or under
    /// its notice's strip. Only a docked page tree keeps its header, which
    /// holds its title (New note is the bar's).
    #[test]
    fn a_note_starts_right_under_the_window_bar() {
        let gutter = f64::from(ui::theme::Theme::dark().tokens.spacing.rhythm.group);
        for scenario in ["status", "import_success", "first_note"] {
            for (width, docked) in [(420, false), (1280, true)] {
                let mut notes = notes_at(scenario, (width, 800));
                let (tree, note, body) = tops(&mut notes);
                assert!(
                    note.abs() < 1e-9 && body.abs() < 1e-9,
                    "{scenario} at {width} px: the content starts at the top, its header {note} high, its view at {body}"
                );
                assert_eq!(
                    tree > 0.0,
                    docked,
                    "{scenario} at {width} px: the page tree's header shows only docked"
                );
                if scenario != "first_note" {
                    let title = title_top(&mut notes);
                    assert!(
                        (title - gutter).abs() < 0.5,
                        "{scenario} at {width} px: the title stands one gutter under the bar, at {title}"
                    );
                }
            }
        }
    }

    fn new_note_in_window(app: &NotesApp) -> bool {
        ui::widget::find_test_id(&app.shell, crate::app::NEW_NOTE_BUTTON_ID).is_some()
    }

    #[test]
    fn hosted_by_the_window_bar_new_note_stands_once_in_the_bar() {
        ui::i18n::I18nManager::init("en-US");
        let mut test = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
        ui::window_toolbar::set_hosted(false);
        test.surface_mut::<NotesApp>().sync_window_toolbar();
        // The shell puts a changed header in place when it lays out.
        test.settle();
        assert!(
            new_note_in_window(test.surface_mut::<NotesApp>()),
            "outside a Tilo session the page tree's header holds New note"
        );

        ui::window_toolbar::set_hosted(true);
        test.surface_mut::<NotesApp>().sync_window_toolbar();
        test.settle();
        assert!(
            !new_note_in_window(test.surface_mut::<NotesApp>()),
            "the page tree draws no New note of its own while the bar hosts the window"
        );
        let declared = ui::window_toolbar::declared().expect("the note's slots are declared");
        assert_eq!(
            declared
                .actions
                .iter()
                .filter(|action| action.id == NEW_NOTE)
                .count(),
            1,
            "the bar holds New note once"
        );
    }
}
