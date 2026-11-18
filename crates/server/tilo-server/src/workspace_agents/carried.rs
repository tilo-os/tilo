//! What a message carries first, said to a turn in words.
//!
//! Chat says what a message asks in words and hands the first resource it carries apart
//! ([`chat::summons::request`]): on a device that resource is what the agent's work is started
//! on. A turn this server carries out is started with words alone, so the resource is said in
//! them here, the way Chat says every further one: what it is called, its kind and id, and
//! the app that keeps it. The turn reads it through its tools, as its rules allow.

use std::fmt::Write as _;

use tilo_actions::ActionResource;

/// What `message` asks of the agents it summons, in words, with the first resource it carries
/// said in them.
pub(super) fn request(message: &chat::chat::Message) -> String {
    let (instruction, carried) = chat::summons::request(message);
    let carried = carried
        .as_ref()
        .map(|carried| (&carried.resource, carried.app_id.as_str()));
    told(instruction, carried)
}

/// `instruction`, with `carried` said after it: a resource and the app that keeps it.
fn told(mut instruction: String, carried: Option<(&ActionResource, &str)>) -> String {
    if let Some((resource, app)) = carried {
        if !instruction.is_empty() {
            instruction.push('\n');
        }
        let _ = write!(
            instruction,
            "It carries {:?} ({} `{}`, in `{app}`).",
            resource.label, resource.kind, resource.id
        );
    }
    instruction
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource a message carries first is said after its words; a message that carries
    /// none says its words alone.
    #[test]
    fn the_first_resource_a_message_carries_is_said_in_words() {
        let note = ActionResource {
            id: "note:7".into(),
            kind: "notes.note".into(),
            label: "Launch plan".into(),
        };
        let carried = Some((&note, "so.tilo.notes"));
        assert_eq!(
            told("Is this ready?".to_owned(), carried),
            "Is this ready?\nIt carries \"Launch plan\" (notes.note `note:7`, in `so.tilo.notes`)."
        );
        assert_eq!(
            told(String::new(), carried),
            "It carries \"Launch plan\" (notes.note `note:7`, in `so.tilo.notes`)."
        );
        assert_eq!(told("Is this ready?".to_owned(), None), "Is this ready?");
    }
}
