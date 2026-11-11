//! The note's header as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! While the desktop's bar hosts them, the content header leaves the window
//! and the bar shows the page tree's toggle, the note as the window's location, then New note and
//! the note's actions; search is the bar's own. The page tree's header
//! keeps only its title then, so New note stands once, in the bar. Outside
//! a Tilo session both headers stay in the window, New note in the tree's.

use std::rc::Rc;

use ui::framework::BarCommands;
use ui::window_toolbar::{ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::NotesApp;

/// What an entry of the bar asks the header for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum BarCommand {
    NewNote,
}

impl NotesApp {
    /// Whether this person may make a note in the notebook, as the workspace
    /// decides it: someone who may only look (a Viewer, or a person an
    /// item was shared with alone) is not offered New note.
    pub(super) fn may_add_notes(&self) -> bool {
        self.dispatcher.as_ref().is_none_or(|dispatcher| {
            !dispatcher.is_past()
                && dispatcher.may(&ui::tilo_actions::ActionNeed::of(
                    ui::tilo_actions::ActionKind::Undoable,
                ))
        })
    }

    /// The slots for the note shown (`title`, and the anchor `open` shows it
    /// at; `None`: no note is open).
    fn window_toolbar(
        bar: &BarCommands<BarCommand>,
        note: Option<(&str, String)>,
        sidebar_open: bool,
        adds_notes: bool,
    ) -> WindowToolbar {
        let title = note.as_ref().map(|(title, _)| *title);
        // New note is offered only to someone who may make one.
        let new_note = adds_notes.then(|| {
            bar.entry(
                "plus",
                ui::i18n!("notes-affordance-new-note"),
                BarCommand::NewNote,
            )
        });
        // The note's actions are declared for a note only: a menu that acts
        // on nothing has no place in the bar's own. They are rows of the
        // window's one "More" menu, so a note shows one "…" and not two.
        let note_actions = title.map(|_| crate::sidebar::note_menu_commands());
        WindowToolbar {
            sidebar: Some(sidebar_open),
            navigation: None,
            location: note.map(|(title, anchor)| ToolbarLocation {
                label: title.to_owned(),
                placeholder: ui::i18n!("notes-title-placeholder"),
                anchor: Some(anchor),
                // Unsaved work is marked by the window's framework, from the
                // record's save.
                ..ToolbarLocation::default()
            }),
            actions: new_note
                .into_iter()
                .chain(note_actions.into_iter().flatten())
                .collect(),
            ..WindowToolbar::default()
        }
    }

    /// Move the content header into the window's bar while it hosts the
    /// slots and back when it does not, tell the bar the slots, and turn
    /// what the person did there into the header's requests. Returns
    /// whether anything changed.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        let hosted = ui::window_toolbar::is_hosted();
        let adds = self.may_add_notes();
        let mut changed = false;
        if hosted != self.header_in_bar || adds != self.offers_new_note {
            self.header_in_bar = hosted;
            self.offers_new_note = adds;
            self.shell.set_sidebar_toggle_in_window_bar(hosted);
            let (tree_actions, actions) = if hosted {
                (Vec::new(), Vec::new())
            } else {
                (
                    if adds {
                        Self::page_tree_header_actions(&self.header)
                    } else {
                        Vec::new()
                    },
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
        let key = (title.zip(anchor), open, adds);
        if self.declared_bar.as_ref() != Some(&key) {
            let note = key
                .0
                .as_ref()
                .map(|(title, anchor)| (title.as_str(), anchor.clone()));
            ui::window_toolbar::declare(Some(Self::window_toolbar(&self.bar, note, open, adds)));
            self.declared_bar = Some(key);
        }
        let header = Rc::clone(&self.header);
        for event in ui::window_toolbar::take_events() {
            match (&event, self.bar.pressed(&event)) {
                (ToolbarEvent::ToggleSidebar, _) => self.shell.toggle_sidebar(),
                (_, Some(BarCommand::NewNote)) if adds => header.new_note.set(true),
                // The bar holds the note's commands too, as rows of the
                // window's "More" menu: a row chosen there runs the command
                // the page menu's row runs, on the note that is open.
                (ToolbarEvent::Action { id, .. }, _) => {
                    let Some(row) = crate::sidebar::note_command_row(id) else {
                        continue;
                    };
                    self.apply_note_action(row);
                }
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

    /// The page tree's header, where the note's own header starts and stops, and where the
    /// note's view starts, as laid out.
    fn tops(app: &mut TestApp<crate::NotesShell>) -> (f64, (f64, f64), f64) {
        app.with_surface::<NotesApp, _>(|notes| {
            let g = notes.shell.geometry();
            (
                g.sidebar_header().height(),
                (g.content_header().y0, g.content_header().y1),
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

    /// A window shows one bar. The row the window's controls stand in is the note's own shell's,
    /// drawn at the window's top, never a second row under a bar above it, and it stands at rest
    /// too: the note starts right under it. Only a docked page tree keeps its header, which
    /// holds its title (New note is the bar's).
    #[test]
    fn a_note_starts_right_under_the_window_bar() {
        let gutter = f64::from(ui::theme::Theme::dark().tokens.spacing.rhythm.group);
        for scenario in ["table", "import_success", "first_note"] {
            for (width, docked) in [(420, false), (1280, true)] {
                let mut notes = notes_at(scenario, (width, 800));
                let (tree, header, body) = tops(&mut notes);
                assert!(
                    (body - header.1).abs() < 1e-9,
                    "{scenario} at {width} px: the note's view starts right under the row, at \
                     {body} under {header:?}"
                );
                assert!(
                    notes.harness().frame_extent((width, 800)).0.abs() < 1e-9,
                    "{scenario} at {width} px: the window's bar is the row the note's shell \
                     draws, so no band of a bar stands above the note"
                );
                assert_eq!(
                    header.0.abs() < 1e-9 && header.1 > 0.0,
                    notes.harness().row_holds_bar(),
                    "{scenario} at {width} px: the row that stands at the window's top is the \
                     window's bar, never a row under one: {header:?}"
                );
                assert_eq!(
                    tree > 0.0,
                    docked,
                    "{scenario} at {width} px: the page tree's header shows only docked"
                );
                if scenario != "first_note" {
                    let title = title_top(&mut notes);
                    assert!(
                        (title - body - gutter).abs() < 0.5,
                        "{scenario} at {width} px: the title stands one gutter under the row, at {title}"
                    );
                }
            }
        }
    }

    fn new_note_in_window(app: &NotesApp) -> bool {
        ui::widget::find_test_id(&*app.shell, crate::app::NEW_NOTE_BUTTON_ID).is_some()
    }

    /// The actions the bar declares, by name and whether they are rows of the
    /// bar's own "More" menu.
    fn bar_actions(app: &mut TestApp<crate::NotesShell>) -> Vec<(String, bool)> {
        app.settle();
        ui::window_toolbar::declared()
            .expect("the note's slots are declared")
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.menu))
            .collect()
    }

    /// The note's commands are rows of the window's one "More" menu, and are
    /// declared only while a note is open: a note never gets a second "…" of
    /// its own beside the window's.
    #[test]
    fn the_bar_offers_the_notes_actions_only_for_a_note() {
        ui::i18n::I18nManager::init("en-US");
        let mut empty = notes_at("first_note", (1280, 800));
        assert_eq!(
            bar_actions(&mut empty),
            vec![(ui::i18n!("notes-affordance-new-note"), false)],
            "an empty notebook offers New note and no note actions"
        );
        let mut open = notes_at("table", (1280, 800));
        let actions = bar_actions(&mut open);
        assert_eq!(
            actions.first().cloned(),
            Some((ui::i18n!("notes-affordance-new-note"), false)),
            "New note stands in the row, before the menu's rows"
        );
        let rows: Vec<(String, bool)> = actions[1..].to_vec();
        assert_eq!(
            rows,
            vec![
                (ui::i18n!("notes-menu-new-child"), true),
                (ui::i18n!("notes-menu-rename"), true),
                (ui::i18n!("notes-menu-duplicate"), true),
                (ui::i18n!("notes-menu-copy-link"), true),
                (ui::i18n!("notes-menu-export"), true),
                (ui::i18n!("notes-menu-insert-image"), true),
                (ui::i18n!("notes-menu-delete"), true),
            ],
            "an open note's commands are the rows of the window's More menu"
        );
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
                .filter(|action| action.name == "New note")
                .count(),
            1,
            "the bar holds New note once"
        );
    }
}
