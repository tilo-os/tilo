//! Scratchpad: one note, kept for you.
//!
//! The whole app is a model (the note) and a view whose editor is bound to
//! the note's body: typing is the body's own field edit. Storage, sync,
//! collaboration, history, undo, permissions and the action palette come from
//! the framework.

pub mod locales;
mod native_probe;

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
            .with_accessible_name("Note")
            .with_plain_chrome()
            .with_text_role(TextRole::Reading)
            .with_placeholder("Start writing…")
            .multiline(true);
        // Saving is the system's job: the window says when a save stalls or
        // fails. Say only that the note is not kept on this device at all.
        let status = Text::from_state(&cx.save_status(), |status| match status {
            RecordSaveStatus::MemoryOnly => "Not saved on this device".to_owned(),
            RecordSaveStatus::Saved | RecordSaveStatus::Saving | RecordSaveStatus::Failed(_) => {
                String::new()
            }
        })
        .with_id("save-status")
        .with_role(TextRole::Caption)
        .dimmed()
        .with_live(ui::a11y::Politeness::Polite);
        // The note is writing, not a form field: the editor is a plain surface
        // on the content ground, so the scrolling text runs to the pane's own
        // edge. The status sits a related step under it; an idle note's status
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
                            .with_child(Box::new(editor)),
                    )),
            ))
            .push(Box::new(status));
        Box::new(native_probe::NativeProbe::new(content, scroll))
    }
}

ui::dev_reload_entry!(Scratchpad);
