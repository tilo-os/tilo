//! Scratchpad: one note, kept for you.
//!
//! The whole app is a resource (the note), one action (edit it) and a view.
//! Storage, sync, collaboration, history, undo, permissions and the action
//! palette come from the framework.

mod native_probe;

use std::cell::Cell;
use std::rc::Rc;

use ui::prelude::*;

/// The note.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub body: String,
}

#[resource(kind = "scratchpad.note", label = "Note")]
impl Note {
    /// Edit note.
    #[action]
    pub fn edit(&mut self, #[param("Note text")] text: String) {
        self.body = text;
    }
}

/// The app.
pub struct Scratchpad;

impl ResourceApp for Scratchpad {
    type Resource = Note;
    const ID: &'static str = "so.tilo.scratchpad";
    const TITLE: &'static str = "Scratchpad";
    const SHOT_VIEWPORT: (u32, u32) = (800, 600);

    fn view(cx: &mut ViewCx<Note>) -> Element {
        let scroll = Rc::new(Cell::new((0.0, 0.0)));
        let editor = cx
            .input(|note| note.body.clone(), Note::EDIT)
            .with_id("note-editor")
            .with_placeholder("A thought worth keeping…")
            .with_font_size(18.0)
            .multiline(true);
        let status = Text::from_state(&cx.save_status(), |status| match status {
            RecordSaveStatus::Saved => "Saved".to_owned(),
            RecordSaveStatus::Saving => "Saving…".to_owned(),
            RecordSaveStatus::MemoryOnly => "Not saved on this device".to_owned(),
            RecordSaveStatus::Failed(error) => format!("Save failed: {error}"),
        })
        .with_id("save-status")
        .with_size(12.0)
        .dimmed()
        .with_live(ui::a11y::Politeness::Polite);
        let content = Column::new()
            .with_horizontal_padding(32.0, 32.0)
            .with_vertical_padding(28.0)
            .with_gap(18.0)
            .push(Box::new(
                Text::new("Scratchpad")
                    .with_size(30.0)
                    .with_heading_level(1),
            ))
            .push(Box::new(
                Text::new("A little space for your next idea.").dimmed(),
            ))
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
