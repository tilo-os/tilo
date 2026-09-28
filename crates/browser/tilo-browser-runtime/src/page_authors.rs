//! Who writes as each Automerge actor of the page's mounts.
//!
//! A record's writer is an actor minted for the mount that opened it, which
//! history alone cannot name. Every mount notes its writer's actor with its
//! author here, so a desktop's Activity names the changes other mounts of the
//! page made to the records it shows (a phone's Books, an agent's Writer)
//! instead of listing them as unknown participants.
use std::{cell::RefCell, collections::BTreeMap};
use ui::{
    history_source::{AuthorDirectory, short_id, stable_hue},
    widget::HistoryAuthor,
};

/// Actors one page remembers; a page mounts a handful.
const MAX_ACTORS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Author {
    name: String,
    agent: bool,
}

thread_local! {
    static AUTHORS: RefCell<BTreeMap<String, Author>> = const { RefCell::new(BTreeMap::new()) };
}

/// `actor` writes for `author`, or for the agent `agent` names (an agent
/// mount's display name). Mounts of one profile and device share a record's
/// writer, so an actor stays with the first mount that noted it.
pub(crate) fn note(actor: &automerge::ActorId, author: &str, agent: Option<&str>) {
    let author = match agent {
        Some(name) => Author {
            name: name.into(),
            agent: true,
        },
        None => Author {
            name: author.into(),
            agent: false,
        },
    };
    let actor = actor.to_hex_string();
    AUTHORS.with_borrow_mut(|authors| {
        if authors.len() < MAX_ACTORS {
            authors.entry(actor).or_insert(author);
        }
    });
}

/// Note a mount's writers: its actions are committed as its author's own
/// actor, everything else as `writer`, its record's actor.
pub(crate) fn note_mount(writer: Option<&automerge::ActorId>, author: &str, agent: Option<&str>) {
    note(&automerge::ActorId::from(author.as_bytes()), author, agent);
    if let Some(writer) = writer {
        note(writer, author, agent);
    }
}

/// Name every noted actor in `directory`. An agent mount's actor, and one
/// whose author is among `agents` (the workspace's agent members), is shown
/// as that agent.
pub(crate) fn register(directory: &mut AuthorDirectory, agents: &[String]) {
    AUTHORS.with_borrow(|authors| {
        for (actor, author) in authors {
            let name = if author.agent || agents.contains(&author.name) {
                format!("agent:{}", author.name)
            } else {
                author.name.clone()
            };
            directory.register(
                actor,
                HistoryAuthor::new(name, stable_hue(actor), short_id(actor)),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noted_writers_resolve_to_their_mounts_authors_and_agents_as_agents() {
        let phone = automerge::ActorId::random();
        let reader = automerge::ActorId::random();
        let agent = automerge::ActorId::random();
        let stranger = automerge::ActorId::random();
        note_mount(Some(&phone), "You", None);
        note(&reader, "Reading agent", None);
        note_mount(Some(&agent), "agent-task:reading", Some("Scribe"));
        let mut directory = AuthorDirectory::new();
        register(&mut directory, &["Reading agent".into()]);
        let name = |actor: &automerge::ActorId| directory.resolve(&actor.to_hex_string()).name;
        assert_eq!(name(&phone), "You");
        assert_eq!(
            name(&reader),
            "Reading agent · Agent",
            "an author the workspace admits as an agent is shown as one"
        );
        assert_eq!(
            name(&agent),
            "Scribe · Agent",
            "an agent mount's display name"
        );
        assert_eq!(
            name(&automerge::ActorId::from("agent-task:reading".as_bytes())),
            "Scribe · Agent",
            "and its actions"
        );
        assert_eq!(
            name(&stranger),
            ui::history_source::UNKNOWN_AUTHOR_NAME,
            "nobody noted it"
        );

        note(&phone, "Reading agent", None);
        let mut shared = AuthorDirectory::new();
        register(&mut shared, &[]);
        assert_eq!(
            shared.resolve(&phone.to_hex_string()).name,
            "You",
            "a writer another mount of the device shares stays its first mount's"
        );
        assert_eq!(
            shared.resolve(&reader.to_hex_string()).name,
            "Reading agent",
            "a person unless the workspace names an agent"
        );
    }
}
