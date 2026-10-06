//! Who writes as each Automerge actor of the page's mounts.
//!
//! Mounts of one profile and device share a record's document. Each writes
//! its edits as its author's own actor on it (`ui::record::author_writer`),
//! its actions as its author's handle, and the record's own writer does the
//! rest; history alone can name none of them. Every mount notes its actors
//! with its author here, so a desktop's Activity names the changes other
//! mounts of the page made to the records it shows (a phone's Books, an
//! agent's Writer) instead of listing them as unknown participants.
use std::{cell::RefCell, collections::BTreeMap};
use ui::history_source::{AuthorDirectory, author_coloured, short_id};

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
/// mount's display name). An actor stays with the first mount that noted
/// it: a record's own writer is shared by every mount of it.
pub(crate) fn note(actor: &ui::automerge::ActorId, author: &str, agent: Option<&str>) {
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

/// Note the actors a mount of `author` writes a record as, whose own writer
/// is `writer`: its author's handle for actions, its author's writer on the
/// record for edits, and the record's writer for what the record wrote
/// before (its creation, when this mount made it).
pub(crate) fn note_mount(
    writer: Option<&ui::automerge::ActorId>,
    author: &str,
    agent: Option<&str>,
) {
    note(
        &ui::automerge::ActorId::from(author.as_bytes()),
        author,
        agent,
    );
    if let Some(writer) = writer {
        note(&ui::record::author_writer(writer, author), author, agent);
        note(writer, author, agent);
    }
}

/// Name every noted actor in `directory`. An agent mount's actor, and one
/// whose author is among `agents` (the workspace's agent members), is shown
/// as that agent. All of one participant's actors share its identity, so
/// history groups and counts them as the one participant they are.
pub(crate) fn register(directory: &mut AuthorDirectory, agents: &[String]) {
    AUTHORS.with_borrow(|authors| {
        for (actor, author) in authors {
            let name = if author.agent || agents.contains(&author.name) {
                format!("agent:{}", author.name)
            } else {
                author.name.clone()
            };
            // The key a participant's own handle actor has, so an actor
            // history resolves by its handle alone agrees too.
            let identity = ui::automerge::ActorId::from(name.as_bytes()).to_hex_string();
            let rgb = ui::presence::user_color(&name);
            directory.register(actor, author_coloured(name, short_id(&identity), rgb));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noted_writers_resolve_to_their_mounts_authors_and_agents_as_agents() {
        let phone = ui::automerge::ActorId::random();
        let reader = ui::automerge::ActorId::random();
        let agent = ui::automerge::ActorId::random();
        let stranger = ui::automerge::ActorId::random();
        note_mount(Some(&phone), "You", None);
        note(&reader, "Reading agent", None);
        note_mount(Some(&agent), "agent-task:reading", Some("Scribe"));
        let mut directory = AuthorDirectory::new();
        register(&mut directory, &["Reading agent".into()]);
        let name = |actor: &ui::automerge::ActorId| directory.resolve(&actor.to_hex_string()).name;
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
            name(&ui::automerge::ActorId::from(
                "agent-task:reading".as_bytes()
            )),
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

    #[test]
    fn every_mount_names_its_own_writer_on_a_shared_record_and_one_identity_per_author() {
        let record = ui::automerge::ActorId::random();
        note_mount(Some(&record), "Ada", None);
        note_mount(Some(&record), "Scout", None);
        note_mount(Some(&record), "agent-task:scout", Some("Scout"));
        let mut directory = AuthorDirectory::new();
        register(&mut directory, &["Scout".into()]);
        let resolve = |actor: &ui::automerge::ActorId| directory.resolve(&actor.to_hex_string());
        let ada = resolve(&ui::record::author_writer(&record, "Ada"));
        assert_eq!(ada.name, "Ada");
        assert_eq!(
            resolve(&ui::record::author_writer(&record, "Scout")).name,
            "Scout · Agent",
            "a second mount's edits are its own, not the first mount's"
        );
        assert_eq!(
            resolve(&record).name,
            "Ada",
            "the record's writer stays the first"
        );
        let handle = resolve(&ui::automerge::ActorId::from("Ada".as_bytes()));
        assert_eq!(
            (handle.identity_short.as_str(), handle.avatar_hue),
            (ada.identity_short.as_str(), ada.avatar_hue),
            "one participant, one identity, whichever actor wrote"
        );
        let agent_task = resolve(&ui::record::author_writer(&record, "agent-task:scout"));
        let scout = resolve(&ui::record::author_writer(&record, "Scout"));
        assert_eq!(agent_task.name, scout.name);
        assert_eq!(agent_task.identity_short, scout.identity_short);
    }
}
