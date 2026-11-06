//! An app's view side over its one connection: a reading in, an action out
//! (`docs/architecture/apps.md` §3), proved against a host that answers in
//! process — what a test's document host and a capture's fixture are.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::ActionAddress;
use tilo_resource::{Model, resource};

use super::{Answers, Ask, Connection};

/// A record whose reading and actions a stand-in host keeps: what the
/// session's document host keeps for an app, in memory.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub body: String,
}

#[resource(kind = "test.hosted-note", label = "Note")]
impl Note {
    /// Edit the note.
    #[action]
    pub fn edit(&mut self, text: String) {
        self.body = text;
    }
}

/// A host that keeps one `Note` and answers a `Connection` from it: the
/// reading it publishes, and the actions it admits, attributed to whoever
/// asked.
#[derive(Default)]
struct NoteHost {
    body: RefCell<String>,
    /// Who asked for what, in order, as the door recorded it: the person's
    /// gesture or the app's own request (`docs/architecture/apps.md` §4).
    asked: RefCell<Vec<(ActionAddress, bool)>>,
}

impl NoteHost {
    /// The host holding `body`: a record an app already has (`§8`'s data
    /// invariants: what a record held opens as it is).
    fn holding(body: &str) -> Rc<Self> {
        let host = Rc::new(Self::default());
        *host.body.borrow_mut() = body.to_owned();
        host
    }

    /// The reading the host publishes: the note as its model's JSON.
    fn published(&self) -> serde_json::Value {
        let note = Note {
            body: self.body.borrow().clone(),
        };
        serde_json::json!({ "body": note.body })
    }
}

impl Answers for NoteHost {
    fn reading(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == "test.hosted-note").then(|| self.published())
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        if ask.address != ActionAddress::new("test.hosted-note", "edit") {
            return Err("no such action is offered".to_owned());
        }
        let Some(text) = ask.arguments.get("text").and_then(|text| text.as_str()) else {
            return Err("the action needs its text".to_owned());
        };
        *self.body.borrow_mut() = text.to_owned();
        self.asked
            .borrow_mut()
            .push((ask.address.clone(), ask.in_person));
        Ok(())
    }
}

#[test]
fn a_views_state_is_the_readings_the_host_publishes() {
    let host = NoteHost::holding("Written by today's app");
    let connection = Connection::in_process(HostAnswers(Rc::clone(&host)));
    let state: Note = connection
        .state::<Note>("test.hosted-note")
        .expect("the host's reading");
    assert_eq!(state.body, "Written by today's app");
}

#[test]
fn an_edit_in_the_view_commits_through_the_door_as_the_person() {
    let host = NoteHost::holding("Before");
    let connection = Connection::in_process(HostAnswers(Rc::clone(&host)));
    let ask = Ask::new(
        "test.hosted-note",
        "edit",
        serde_json::Map::from_iter([("text".to_owned(), serde_json::json!("After"))]),
        true,
    );
    connection.submit(&ask).expect("the door admits the edit");
    // The host committed it, and it was the person's gesture.
    let reading = connection.reading("test.hosted-note").expect("a reading");
    assert_eq!(reading["body"], "After");
    assert_eq!(
        host.asked.borrow().as_slice(),
        &[(ActionAddress::new("test.hosted-note", "edit"), true)],
        "the door recorded the person acting in the view"
    );
}

#[test]
fn an_action_the_door_refuses_is_told_not_applied() {
    let host = NoteHost::holding("Unchanged");
    let connection = Connection::in_process(HostAnswers(Rc::clone(&host)));
    let ask = Ask::new(
        "test.hosted-note",
        "no-such-verb",
        serde_json::Map::new(),
        true,
    );
    let refused = connection.submit(&ask);
    assert!(
        refused.is_err(),
        "the door refuses an action it does not offer"
    );
    let reading = connection.reading("test.hosted-note").expect("a reading");
    assert_eq!(reading["body"], "Unchanged", "a refusal changes nothing");
}

/// A [`Connection`] over a [`NoteHost`] an assertion also reads.
#[derive(Clone)]
struct HostAnswers(Rc<NoteHost>);

impl Answers for HostAnswers {
    fn reading(&self, resource: &str) -> Option<serde_json::Value> {
        self.0.reading(resource)
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        self.0.submit(ask)
    }
}
