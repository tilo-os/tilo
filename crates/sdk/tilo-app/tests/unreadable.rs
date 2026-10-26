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

/// What the keyboard learned about the person, kept in their own record.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Learned {
    pub words: u32,
}

#[resource(kind = "test.learned", label = "Learned words")]
impl Learned {
    /// Forget everything the keyboard learned.
    #[action]
    pub fn forget(&mut self) {
        self.words = 0;
    }
}

struct LearnedApp;

impl ResourceApp for LearnedApp {
    type Resource = Memo;
    const ID: &'static str = "so.tilo.test-unreadable-person";
    const TITLE: &'static str = "Memo";

    fn records(records: &mut Records<'_>) {
        records.person::<Learned>();
    }

    fn view(cx: &mut ViewCx<Memo>) -> Element {
        let learned = cx.person::<Learned>();
        Box::new(tilo_app::column![
            cx.input(cx.paths().title).with_id("title"),
            cx.text(move |_: &Memo| match &learned {
                Some(record) => match record.unreadable() {
                    Some(why) => format!("unreadable: {why}"),
                    None => format!("learned {}", record.with(|learned| learned.words)),
                },
                None => "learned nothing".to_owned(),
            })
            .with_id("learned"),
        ])
    }
}

/// The action `action` on the resource published as `resource`, with why it
/// is unavailable when it is.
fn availability<A: ResourceApp>(
    app: &mut TestApp<A>,
    resource: &str,
    action: &str,
) -> Option<String> {
    app.actions()
        .unwrap()
        .into_iter()
        .find(|entry| entry.address.resource == resource && entry.address.action == action)
        .map(|entry| match entry.contract.availability {
            tilo_actions::Availability::Unavailable { reason, .. } => reason,
            tilo_actions::Availability::Available => String::new(),
        })
}

#[test]
fn a_persons_record_that_stops_reading_says_why_instead_of_going_quiet() {
    let mut app = TestApp::<LearnedApp>::for_person(&"ad".repeat(32)).unwrap();
    assert!(
        app.person::<Learned>().is_some(),
        "the person's record is open"
    );
    app.settle();
    let address = format!("person:{}", Learned::KIND);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("learned 0")
    );

    // A change the model cannot read arrives from another device, on the
    // route that carries the person's records.
    app.person::<Learned>()
        .expect("the person's record")
        .record()
        .edit(false, |doc| {
            doc.put(ROOT, "words", "many").map_err(|e| e.to_string())
        })
        .expect("the change lands");
    app.settle();
    let reason = availability(&mut app, &address, "forget").expect("the record is published");
    assert!(reason.contains("words"), "{reason}");
    let shown = app.harness().inspection_json().unwrap();
    assert!(
        !shown.contains("learned 0"),
        "the last state is not shown as current: {shown}"
    );
    assert!(shown.contains("unreadable: words"), "{shown}");
}
