//! A document saved somewhere new (Save As) is the file its window shows from
//! then on, so reopening the window comes back there instead of on the file the
//! document was opened from.

use std::cell::RefCell;
use tilo_app_test_host::TestApp;

use tilo_app::application_view::WindowShows;
use tilo_app::framework::{EffectCx, Effected};
use tilo_app::portal::{PickRequest, PortalOutcome};
use tilo_app::prelude::*;
use tilo_resource::Effect;

thread_local! {
    /// The window's own record of the file it shows, as its view took it.
    static SHOWS: RefCell<Option<WindowShows>> = const { RefCell::new(None) };
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Draft {
    pub text: String,
}

#[resource(kind = "test.draft", catalogue = "tests/locales")]
impl Draft {
    #[action]
    pub fn write(&mut self, text: String) {
        self.text = text;
    }
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.draft.effects", catalogue = "tests/locales")]
pub enum DraftEffect {
    /// Save the document where the person chooses.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    SaveAs,
    /// Save a copy of the document somewhere else.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    Export,
}

struct DraftApp;

impl ResourceApp for DraftApp {
    type Resource = Draft;
    const ID: &'static str = "so.tilo.test-draft";
    const TITLE: &'static str = "Draft";
    const OPENS: &'static [&'static str] = &["text/plain"];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Draft>) -> Element {
        SHOWS.with(|shows| *shows.borrow_mut() = cx.window_shows());
        let text = cx.live().with(|draft| draft.text.clone());
        cx.effects(
            move |effect: DraftEffect, cx: &mut EffectCx<Draft>| match effect {
                DraftEffect::SaveAs => {
                    let text = text.clone();
                    Ok(cx.save_document(
                        PickRequest::save("Save the draft as", "Save", "draft.txt"),
                        move |_| Ok(text.into_bytes()),
                        |(), saved, _| Ok(Effected::value(serde_json::json!(saved.is_some()))),
                    ))
                }
                DraftEffect::Export => Ok(cx.save_file(
                    PickRequest::save("Export the draft", "Export", "draft-copy.txt"),
                    text.clone().into_bytes(),
                    |(), saved, _| Ok(Effected::value(saved)),
                )),
            },
        );
        Box::new(Column::new())
    }
}

fn shown() -> Option<std::path::PathBuf> {
    SHOWS.with(|shows| shows.borrow().as_ref().and_then(WindowShows::shown))
}

#[test]
fn saving_the_document_somewhere_new_makes_it_the_file_the_window_shows() {
    let directory = tempfile::tempdir().unwrap();
    let from = directory.path().join("Draft.txt");
    std::fs::write(&from, "one line").unwrap();
    let (services, recorder) = tilo_app::process::ProcessServices::recording();
    let _host = tilo_app::process::enter(&services);
    let mut app = TestApp::<DraftApp>::new().unwrap();
    SHOWS.with(|shows| {
        let shows = shows.borrow();
        let shows = shows.as_ref().expect("a window says which file it shows");
        shows.show(&from);
    });
    app.settle();
    assert_eq!(shown(), Some(from), "the window shows the draft");

    let to = directory.path().join("Second draft.txt");
    recorder.respond_to_pick(PortalOutcome::Opened(to.clone()));
    let saved = app.effect(&DraftEffect::SaveAs).unwrap();
    assert_eq!(saved.value, Some(serde_json::json!(true)));
    assert_eq!(std::fs::read(&to).unwrap(), b"");
    assert_eq!(
        shown(),
        Some(to),
        "a document saved somewhere new is the file the window shows"
    );

    // Saving a copy writes where it was asked and leaves the window on its file.
    let copy = directory.path().join("Cover letter.txt");
    recorder.respond_to_pick(PortalOutcome::Opened(copy.clone()));
    let exported = app.effect(&DraftEffect::Export).unwrap();
    assert_eq!(exported.value, Some(serde_json::json!(true)));
    assert_eq!(std::fs::read(&copy).unwrap(), b"");
    assert_eq!(
        shown(),
        Some(directory.path().join("Second draft.txt")),
        "a copy is not the file the window shows"
    );
}
