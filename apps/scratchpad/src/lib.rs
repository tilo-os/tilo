//! Scratchpad: one note, kept for you.
//!
//! The whole app is a model (the note) and a view whose editor is bound to
//! the note's body: typing is the body's own field edit. Storage, sync,
//! collaboration, history, undo, permissions and the action palette come from
//! the framework.
//!
//! The window bar shows where the window is: the note's opening line, the way
//! a document is named by its first words, so the bar says which note this is
//! instead of only which app. The window draws no header row and no title of
//! its own.

pub mod locales;
mod native_probe;
mod note_window;

use std::cell::Cell;
use std::rc::Rc;

use ui::prelude::*;

/// The note.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    /// Note text
    pub body: String,
}

#[resource(kind = "scratchpad.note", label = "Note")]
impl Note {}

/// What the capture scenarios write.
const SAMPLE_NOTE: &str = "Notes for Thursday\n\nPick up the printed tickets before noon. Ask Maria whether the \
studio is free on the 14th, and send Tomas the revised floor plan.\n\nIdeas for the launch talk: start with the \
two-minute demo, keep the slides to a handful of images, and finish on the question the audience keeps asking.";

/// The type role the note is set in: the UI face every other app writes
/// in, from the theme's type roles. It is always at hand, so every visitor
/// sees the note in the same face.
pub const NOTE_ROLE: TextRole = TextRole::Body;

/// The app.
pub struct Scratchpad;

impl ResourceApp for Scratchpad {
    type Resource = Note;
    const ID: &'static str = "so.tilo.scratchpad";
    const TITLE: &'static str = "Scratchpad";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (800, 600)
    }

    fn scenarios() -> &'static [&'static str] {
        &["note", "long-note"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Note>> {
        let text = match name {
            Some("note") => SAMPLE_NOTE.to_owned(),
            Some("long-note") => [SAMPLE_NOTE; 8].join("\n\n"),
            _ => return Vec::new(),
        };
        Invoke::edit(&Note::paths().body.replace(text))
            .into_iter()
            .collect()
    }

    fn view(cx: &mut ViewCx<Note>) -> Element {
        let scroll = Rc::new(Cell::new((0.0, 0.0)));
        let editor = cx
            .input(cx.paths().body)
            .with_id("note-editor")
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("scratchpad-note-name"))
            .with_plain_chrome()
            .with_text_role(NOTE_ROLE)
            .with_placeholder("Start writing…")
            .multiline(true);
        // The note is a centred column at the readable measure, inset from
        // the window by the page gutter: a wide window keeps lines short
        // enough to read instead of running them edge to edge.
        let page = Container::new()
            .readable()
            .centered()
            .with_inset(ui::tokens::Rhythm::Group)
            .with_child(Box::new(editor));
        // Saving is the system's job: the window says when a save stalls or
        // fails. Say only that the note is not kept on this device at all.
        let status = Text::from_state(&cx.save_status(), |status| match status {
            RecordSaveStatus::MemoryOnly => {
                ui::i18n::I18nManager::lookup_plain("scratchpad-memory-only")
            }
            RecordSaveStatus::Saved | RecordSaveStatus::Saving | RecordSaveStatus::Failed(_) => {
                String::new()
            }
        })
        .with_id("save-status")
        .with_role(TextRole::Caption)
        .dimmed()
        .with_live(ui::a11y::Politeness::Polite);
        // The note is writing, not a form field: the editor is a plain surface
        // on the content ground, and the pane scrolls at its own edge. The status sits a related step under it; an idle note's status
        // is empty and takes neither room nor the gap.
        let content = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_fill_remaining_height()
                    .push(Box::new(
                        Scrollable::new_vertical()
                            .with_persisted_offset(Rc::clone(&scroll))
                            .fill_parent_height()
                            .with_content_min_viewport_height()
                            .with_child(Box::new(page)),
                    )),
            ))
            .push(Box::new(status));
        let location = cx.value(|note: &Note| opening(&note.body));
        Box::new(note_window::NoteWindow::new(
            Box::new(native_probe::NativeProbe::new(content, scroll)),
            location,
        ))
    }
}

/// What the window bar calls the note: its opening line, trimmed, with its
/// whitespace closed up, as far as a bar has room for; a note that has not
/// been written yet is a note.
pub fn opening(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return ui::i18n::I18nManager::lookup_plain("scratchpad-note");
    }
    // A bar elides what it cannot show; the tab label stays short enough to
    // read at a glance.
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    title
}

/// How many characters of the note's opening the window bar's location
/// carries before it is elided.
const TITLE_CHARS: usize = 60;

ui::dev_reload_entry!(Scratchpad);

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::Scratchpad::scenarios() {
            let mut app = TestApp::<super::Scratchpad>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
