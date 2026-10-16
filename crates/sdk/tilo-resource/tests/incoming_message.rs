#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A verb marked `#[action(incoming_message)]` tags its change, so an
//! incoming-message trigger fires on it without knowing the app's schema.

use tilo_actions::{INCOMING_MESSAGE_FACET, INCOMING_MESSAGE_TAG};
use tilo_resource::{Invoke, Model, Replica, Resource, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Inbox {
    pub messages: Vec<String>,
}

#[resource(kind = "test.inbox", label = "Inbox")]
impl Inbox {
    /// Receive a message.
    #[action(incoming_message)]
    pub fn receive(&mut self, text: String) {
        self.messages.push(text);
    }

    /// Note something down.
    #[action]
    pub fn note(&mut self, text: String) {
        self.messages.push(text);
    }
}

fn marked(replica: &mut Replica<Inbox>) -> bool {
    replica.versions().last().is_some_and(|version| {
        version.context.as_ref().is_some_and(|context| {
            context
                .facets
                .iter()
                .any(|facet| facet.key == INCOMING_MESSAGE_FACET && facet.value == "true")
        })
    })
}

#[test]
fn a_flagged_verb_marks_its_change_and_its_declaration() {
    let declarations = Inbox::declarations();
    let receive = declarations.iter().find(|d| d.id == "receive").unwrap();
    assert!(receive.tags.iter().any(|tag| tag == INCOMING_MESSAGE_TAG));
    let note = declarations.iter().find(|d| d.id == "note").unwrap();
    assert!(!note.tags.iter().any(|tag| tag == INCOMING_MESSAGE_TAG));

    let mut inbox = Replica::<Inbox>::new();
    inbox.apply(Inbox::RECEIVE.with("hi")).unwrap();
    assert!(marked(&mut inbox));
    inbox.apply(Inbox::NOTE.with("x")).unwrap();
    assert!(!marked(&mut inbox));
    inbox
        .apply(Invoke::group([
            Inbox::NOTE.with("y"),
            Inbox::RECEIVE.with("z"),
        ]))
        .unwrap();
    assert!(
        marked(&mut inbox),
        "a gesture that receives a message among others is marked"
    );
}
