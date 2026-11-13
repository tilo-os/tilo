//! Scratchpad: one note, kept for you.

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

impl Note {
    /// What the record holds: the note's text.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "scratchpad.note",
    read_content = Note::read_document
)]
impl Note {}

const SAMPLE_NOTE: &str = "Notes for Thursday\n\nPick up the printed tickets before noon. Ask Maria whether the \
studio is free on the 14th, and send Tomas the revised floor plan.\n\nIdeas for the launch talk: start with the \
two-minute demo, keep the slides to a handful of images, and finish on the question the audience keeps asking.";

pub const NOTE_ROLE: TextRole = TextRole::Body;

/// The app.
pub struct Scratchpad;

impl ResourceApp for Scratchpad {
    type Resource = Note;
    const ID: &'static str = "so.tilo.scratchpad";
    const TITLE: &'static str = "Scratchpad";
    const TITLE_KEY: &'static str = "scratchpad-app-title";

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
            .with_accessible_name(crate::locales::CATALOG.t("scratchpad-note-name"))
            .with_plain_chrome()
            .with_text_role(NOTE_ROLE)
            .with_placeholder(crate::locales::CATALOG.t("scratchpad-placeholder"))
            .multiline(true);
        let page = Container::new()
            .readable()
            .centered()
            .with_inset(ui::tokens::Rhythm::Group)
            .with_child(Box::new(editor));
        let status = Text::from_state(&cx.save_status(), |status| match status {
            RecordSaveStatus::MemoryOnly => crate::locales::CATALOG.t("scratchpad-memory-only"),
            RecordSaveStatus::Saved | RecordSaveStatus::Saving | RecordSaveStatus::Failed(_) => {
                String::new()
            }
        })
        .with_id("save-status")
        .with_role(TextRole::Caption)
        .dimmed()
        .with_live(ui::a11y::Politeness::Polite);
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

/// What the window bar calls the note: its opening line, trimmed, with its whitespace
/// closed up, as far as a bar has room for; a note that has not been written yet is a note.
pub fn opening(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return crate::locales::CATALOG.t("scratchpad-note");
    }
    // A bar elides what it cannot show; the tab label stays short enough to read at a
    // glance.
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    title
}

const TITLE_CHARS: usize = 60;

ui::dev_reload_entry!(Scratchpad);

ui::reach_tests!(Scratchpad);
