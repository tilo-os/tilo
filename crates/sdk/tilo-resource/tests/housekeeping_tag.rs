#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A verb marked `#[action(housekeeping)]` says so in its declaration, so a
//! host reviewing held actions can tell housekeeping from what a person
//! asked for.

use tilo_actions::HOUSEKEEPING_TAG;
use tilo_resource::{Model, Resource, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channel {
    pub seen: u32,
    pub messages: Vec<String>,
}

#[resource(kind = "test.channel", label = "Channel")]
impl Channel {
    /// Mark as read.
    #[action(housekeeping)]
    pub fn mark_read(&mut self, seen: u32) {
        self.seen = seen;
    }

    /// Post a message.
    #[action]
    pub fn post(&mut self, text: String) {
        self.messages.push(text);
    }
}

#[test]
fn a_housekeeping_verb_is_tagged_in_its_declaration() {
    let declarations = Channel::declarations();
    let mark_read = declarations.iter().find(|d| d.id == "mark-read").unwrap();
    assert!(mark_read.tags.iter().any(|tag| tag == HOUSEKEEPING_TAG));
    let post = declarations.iter().find(|d| d.id == "post").unwrap();
    assert!(!post.tags.iter().any(|tag| tag == HOUSEKEEPING_TAG));
}
