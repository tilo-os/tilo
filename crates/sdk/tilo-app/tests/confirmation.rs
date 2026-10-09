#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A destructive verb a button submits is confirmed by the person and then
//! applied, however long they take to confirm it.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notes {
    pub notes: Collection<Note>,
}

#[resource(kind = "test.notes", label = "Notes")]
impl Notes {
    /// Add a note.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.notes.push(cx.id(), Note { text });
    }

    /// Remove a note.
    #[action(destructive)]
    pub fn remove(&mut self, notes: &Id) -> Result<(), Refusal> {
        self.notes
            .remove(notes)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such note"))
    }
}

struct NotesApp;

impl ResourceApp for NotesApp {
    type Resource = Notes;
    const ID: &'static str = "so.tilo.test-confirmation";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Notes>) -> Element {
        let first = cx.read(|notes| notes.and_then(|notes| notes.notes.ids().next().cloned()));
        Box::new(tilo_app::column![cx.button(
            "remove",
            Notes::REMOVE.with(first.unwrap_or_else(|| Id::from_raw("")))
        )])
    }
}

#[test]
fn a_destructive_verb_confirmed_later_still_applies() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    app.harness().click_named("remove").unwrap();
    assert_eq!(
        app.state().notes.len(),
        1,
        "nothing happens before confirming"
    );
    // The person reads the prompt before confirming it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    app.harness().type_text("CONFIRM");
    app.harness().key("Enter");
    assert!(
        app.state().notes.is_empty(),
        "the confirmed verb applied: {:?}",
        app.harness().control_failures()
    );
}
