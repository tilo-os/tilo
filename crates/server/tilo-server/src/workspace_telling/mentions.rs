//! Chat's mentions, told to the people they name.
//!
//! A message that names a person is addressed to them: it reaches every
//! device of theirs, whichever window each shows and whether any is on,
//! through their log ([`crate::workspaces::Workspaces::tell`]). The server reads
//! the part of Chat's messages a member just wrote to beside its questions
//! ([`super::questions::Feeder`]). A mention is told under an id of its own,
//! so reading a part again tells nothing twice; only a message written
//! lately is told at all, so a server that reads every part once as it starts
//! tells nobody of what they read long ago.

use tilo_actions::MemberId;
use tilo_actions::remote::person_telling::{Kind, Tell, Teller};
use tilo_resource::Id;

use crate::workspaces::ToTell;

/// How old a message may be and still be told as news of a mention: what a
/// server that was down or restarted for a while still tells once it is back.
const FRESH_MS: i64 = 15 * 60 * 1000;

/// One person a message of a part names, as their log is told it once the
/// author is named.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Named {
    /// The person named, by their root.
    pub(super) person: [u8; 32],
    /// Who wrote the message.
    pub(super) author: MemberId,
    /// The name the message keeps for an author that is not a person.
    pub(super) author_name: Option<String>,
    /// The id the mention is told under.
    pub(super) id: String,
    /// The message's words.
    pub(super) words: String,
    /// Where the message is.
    pub(super) message: Id,
}

/// The person `member` is, by their root, where it is a person.
pub(super) fn root_of(member: &MemberId) -> Option<[u8; 32]> {
    hex::decode(member.person_id()?).ok()?.try_into().ok()
}

/// Every person a message of `part` written since `now_ms - FRESH_MS` names,
/// other than its author, oldest message first.
pub(super) fn named(workspace: &str, part: &chat::parts::ChatMessages, now_ms: i64) -> Vec<Named> {
    let mut found = Vec::new();
    for (id, message) in part.messages.iter() {
        if now_ms.saturating_sub(message.ts) > FRESH_MS || message.text.trim().is_empty() {
            continue;
        }
        let mut told = std::collections::BTreeSet::new();
        for mention in &message.mentions {
            let Some(person) = root_of(&mention.user) else {
                continue;
            };
            if mention.user == message.author || !told.insert(person) {
                continue;
            }
            found.push(Named {
                person,
                author: message.author.clone(),
                author_name: message.author_name.clone(),
                id: format!("mention:{workspace}:{id}:{}", hex::encode(person)),
                words: message.text.clone(),
                message: id.clone(),
            });
        }
    }
    found
}

/// What the person `named` names is told: who mentioned them, by `author`,
/// their words, and the message, which opening it shows.
pub(super) fn telling(workspace: &str, named: Named, author: &str) -> ToTell {
    ToTell {
        id: Some(named.id),
        kind: Kind::Mention,
        tell: Tell {
            title: format!("{author} mentioned you"),
            words: named.words,
            about: Some(super::questions::at_message(workspace, &named.message)),
            ..Tell::default()
        },
        from: Some(Teller {
            agent: named.author.agent_id().unwrap_or_default().to_owned(),
            name: author.to_owned(),
        }),
        workspace: Some(workspace.to_owned()),
        words_stay: false,
        spender: None,
    }
}

#[cfg(test)]
mod tests {
    use chat::chat::Message;
    use chat::mention::Mention;

    use super::*;

    const ADA: [u8; 32] = [8; 32];
    const BEN: [u8; 32] = [9; 32];
    const WORKSPACE: &str = "6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10";
    const NOW: i64 = 1_763_366_400_000;

    fn person(root: [u8; 32]) -> MemberId {
        MemberId::person(hex::encode(root))
    }

    fn message(ts: i64, by: [u8; 32], naming: &[MemberId]) -> Message {
        let mut message = Message::new(
            Id::from_raw("general".to_owned()),
            person(by),
            ts,
            "@Ben can you look at the release notes?".to_owned(),
        );
        message.mentions = naming
            .iter()
            .map(|user| Mention {
                user: user.clone(),
                start: 0,
                len: 4,
                is_agent: user.is_agent(),
            })
            .collect();
        message
    }

    /// A message that names a person is told to them under one id per message and person; its
    /// author naming themselves, an agent, a message from long ago and a plain message tell
    /// nobody.
    #[test]
    fn a_parts_mentions_name_whom_they_are_told_to() {
        let mut part = chat::parts::ChatMessages::default();
        for (id, message) in [
            ("m0", message(NOW - 1_000, ADA, &[person(BEN), person(BEN)])),
            ("m1", message(NOW - 1_000, ADA, &[person(ADA)])),
            (
                "m2",
                message(NOW - 1_000, ADA, &[MemberId::agent("assistant")]),
            ),
            ("m3", message(NOW - 2 * FRESH_MS, ADA, &[person(BEN)])),
            ("m4", message(NOW, BEN, &[])),
        ] {
            part.messages.push(Id::from_raw(id.to_owned()), message);
        }
        let found = named(WORKSPACE, &part, NOW);
        assert_eq!(found.len(), 1, "{found:?}");
        let ben = &found[0];
        assert_eq!(ben.person, BEN);
        assert_eq!(ben.author, person(ADA));
        assert_eq!(
            ben.id,
            format!("mention:{WORKSPACE}:m0:{}", hex::encode(BEN))
        );

        let told = telling(WORKSPACE, found.into_iter().next().unwrap(), "Ada");
        assert_eq!(told.kind, Kind::Mention);
        assert_eq!(told.tell.title, "Ada mentioned you");
        assert_eq!(told.tell.words, "@Ben can you look at the release notes?");
        let about = told.tell.about.expect("it says where the message is");
        assert_eq!(
            about,
            crate::workspace_telling::questions::at_message(
                WORKSPACE,
                &Id::from_raw("m0".to_owned())
            )
        );
        assert_eq!(told.from.map(|from| from.name), Some("Ada".to_owned()));
    }
}
