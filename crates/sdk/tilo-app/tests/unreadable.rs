#![allow(clippy::unwrap_used, reason = "test assertions")]

//! A record that does not read as the resource shows why, never the last
//! state as if it were current, and every action reports why it is
//! unavailable.

use tilo_app::automerge::transaction::Transactable as _;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::__private::ROOT;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Memo {
    pub title: String,
    pub pinned: bool,
}

#[resource(kind = "test.memo", label = "Memo")]
impl Memo {
    /// Clear the title.
    #[action]
    pub fn clear(&mut self) {
        self.title.clear();
    }
}

struct MemoApp;

impl ResourceApp for MemoApp {
    type Resource = Memo;
    const ID: &'static str = "so.tilo.test-unreadable";
    const TITLE: &'static str = "Memo";

    fn view(cx: &mut ViewCx<Memo>) -> Element {
        let p = cx.paths();
        Box::new(tilo_app::column![
            cx.input(p.title).with_id("title"),
            cx.checkbox("pinned", p.pinned),
            cx.button("clear", Memo::CLEAR.invoke()),
        ])
    }
}

fn unavailable_because(app: &mut TestApp<MemoApp>, action: &str) -> Option<String> {
    app.actions()
        .unwrap()
        .into_iter()
        .find(|entry| entry.address.action == action)
        .and_then(|entry| match entry.contract.availability {
            tilo_actions::Availability::Unavailable { reason, .. } => Some(reason),
            tilo_actions::Availability::Available => None,
        })
}

#[test]
fn a_record_that_stops_reading_shows_why_and_refuses_every_action() {
    let mut app = TestApp::<MemoApp>::new().unwrap();
    app.invoke(Invoke::edit(&Memo::paths().title.replace("Groceries")).unwrap())
        .unwrap();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Groceries")
    );

    // A collaborator's change leaves a value the model cannot read.
    app.merge_peer_edit(|doc| {
        doc.put(ROOT, "pinned", "not a flag").unwrap();
    });
    let shown = app.harness().inspection_json().unwrap();
    assert!(
        !shown.contains("Groceries"),
        "the last state is not shown as current: {shown}"
    );
    assert!(shown.contains("pinned"), "the view says why: {shown}");
    let reason = unavailable_because(&mut app, "clear").expect("clear is unavailable");
    assert!(reason.contains("pinned"), "{reason}");
    assert!(unavailable_because(&mut app, tilo_app::tilo_resource::FIELD_EDIT).is_some());
    assert!(app.invoke(Memo::CLEAR.invoke()).is_err());

    // Once it reads again, the view and its actions are back.
    app.merge_peer_edit(|doc| {
        doc.put(ROOT, "pinned", true).unwrap();
    });
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Groceries")
    );
    assert!(unavailable_because(&mut app, "clear").is_none());
    app.invoke(Memo::CLEAR.invoke()).unwrap();
    assert!(app.state().title.is_empty());
}

#[test]
fn a_record_unreadable_when_opened_shows_why_instead_of_the_view() {
    let mut app = TestApp::<MemoApp>::new().unwrap();
    app.invoke(Invoke::edit(&Memo::paths().title.replace("Groceries")).unwrap())
        .unwrap();
    app.merge_peer_edit(|doc| {
        doc.put(ROOT, "pinned", "not a flag").unwrap();
    });
    let mut app = app.reopen().unwrap();
    let shown = app.harness().inspection_json().unwrap();
    assert!(!shown.contains("Groceries"), "{shown}");
    assert!(shown.contains("pinned"), "{shown}");
    let reason = unavailable_because(&mut app, "clear").expect("clear is unavailable");
    assert!(reason.contains("pinned"), "{reason}");
}
