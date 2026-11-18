//! The messages kept in parts: where changes go, what servers are owed, and
//! what one change costs as the mailbox grows.

use std::collections::BTreeMap;

use ui::framework::Chain;

use super::{MessageStates, detached_chain, message_states, owed};
use crate::actions::{IngestBaseline, ServerUpdate};
use crate::model::Folder;
use ui::framework::PART_ITEMS as PART_MESSAGES;

fn id(n: usize) -> String {
    format!("m{n:08}")
}

/// A mailbox of `count` messages fetched from a server, arriving a page at
/// a time as a sync brings them.
fn mailbox(count: usize) -> Chain<MessageStates> {
    let chain = detached_chain().expect("a chain in memory");
    let mut next = 0;
    while next < count {
        let page = (next..count.min(next + 2_000))
            .map(|n| IngestBaseline {
                id: id(n),
                read: Some(false),
                starred: Some(false),
                folder: Some("inbox".into()),
                account: Some("me@example.test".into()),
                locator: Some(format!("{n}@example.test")),
                label: Some(format!("Message {n}, Sam, 2026-10-12")),
            })
            .collect();
        chain
            .invoke(&MessageStates::INGEST.with(message_states::Ingest { messages: page }))
            .expect("a page of mail arrives");
        next += 2_000;
    }
    chain
}

/// Each part's changes as one document holds them, by part.
fn heads(chain: &Chain<MessageStates>) -> BTreeMap<u64, Vec<ui::automerge::ChangeHash>> {
    chain
        .parts()
        .iter()
        .map(|(index, _)| {
            let mut doc =
                ui::automerge::AutoCommit::load(&chain.part_bytes(*index).unwrap()).unwrap();
            (*index, doc.get_heads())
        })
        .collect()
}

/// What `change` writes: the parts it changed, and the bytes of the changes
/// it added to them.
fn written(chain: &Chain<MessageStates>, change: impl FnOnce()) -> (Vec<u64>, usize) {
    let before = heads(chain);
    change();
    let mut parts = Vec::new();
    let mut bytes = 0;
    for (index, _) in chain.parts() {
        let mut doc = ui::automerge::AutoCommit::load(&chain.part_bytes(index).unwrap()).unwrap();
        let known = before.get(&index).cloned().unwrap_or_default();
        let added: usize = doc
            .get_changes(&known)
            .iter()
            .map(|change| change.raw_bytes().len())
            .sum();
        if added > 0 {
            parts.push(index);
            bytes += added;
        }
    }
    (parts, bytes)
}

fn mark_read(chain: &Chain<MessageStates>, ids: Vec<String>) {
    chain
        .invoke(&MessageStates::mark(ids, true))
        .expect("marked read");
}

/// One change at 1k, 10k and 50k messages writes one part and the same
/// bytes, and a bulk archive of a thousand writes only the parts holding them.
fn costs_at(count: usize) -> usize {
    let chain = mailbox(count);
    assert_eq!(
        chain.parts().len(),
        count.div_ceil(PART_MESSAGES),
        "the messages fill parts in turn"
    );
    let middle = id(count / 2);
    let (parts, bytes) = written(&chain, || mark_read(&chain, vec![middle.clone()]));
    assert_eq!(
        parts,
        [((count / 2) / PART_MESSAGES) as u64],
        "one part changed"
    );

    // What the server is owed is read off that part alone.
    let owing: Vec<ServerUpdate> = chain
        .parts()
        .iter()
        .flat_map(|(_, part)| owed(part))
        .map(|(_, update)| update)
        .collect();
    assert!(
        matches!(owing.as_slice(), [ServerUpdate::Flags { read: true, .. }]),
        "{owing:?}"
    );

    // A bulk archive of a thousand messages writes the parts holding them
    // (across two, where the mailbox has them), as one step.
    let start = if count >= 2 * PART_MESSAGES {
        PART_MESSAGES / 2
    } else {
        0
    };
    let holding: Vec<u64> =
        ((start / PART_MESSAGES) as u64..=((start + 999) / PART_MESSAGES) as u64).collect();
    let thousand: Vec<String> = (start..start + 1_000).map(id).collect();
    let (archived, archive_bytes) = written(&chain, || {
        chain
            .invoke(&MessageStates::ARCHIVE.with(message_states::Archive {
                messages: thousand.clone(),
            }))
            .expect("archived");
    });
    assert_eq!(archived, holding, "only the parts holding them");
    let all = MessageStates::of(&chain.parts());
    assert!(
        thousand
            .iter()
            .all(|id| all.message(id).is_some_and(|m| m.folder == Folder::Archive))
    );
    assert!(chain.undo_last(), "one undo step");
    let all = MessageStates::of(&chain.parts());
    assert!(
        thousand
            .iter()
            .all(|id| all.message(id).is_some_and(|m| m.folder == Folder::Inbox)),
        "Undo takes the archive back in both parts"
    );
    eprintln!(
        "mail scale: {count} messages, one change writes {bytes} bytes to one part; archiving \
         1000 writes {archive_bytes} bytes to parts {archived:?}"
    );
    bytes
}

#[test]
fn one_change_costs_the_same_at_a_thousand_and_ten_thousand_messages() {
    let small = costs_at(1_000);
    let large = costs_at(10_000);
    assert!(
        large <= small + small / 4,
        "a change at 10k messages writes {large} bytes, at 1k {small}"
    );
}

/// The same at a real mailbox's size: with `--ignored`, it builds fifty
/// thousand messages.
#[test]
#[ignore = "builds a mailbox of 50,000 messages"]
fn one_change_costs_the_same_at_fifty_thousand_messages() {
    let small = costs_at(1_000);
    let large = costs_at(50_000);
    assert!(
        large <= small + small / 4,
        "a change at 50k messages writes {large} bytes, at 1k {small}"
    );
}

#[test]
fn a_fetch_fills_the_newest_part_and_starts_the_next() {
    let chain = mailbox(PART_MESSAGES + 10);
    let parts = chain.parts();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].1.messages.len(), PART_MESSAGES);
    assert_eq!(parts[1].1.messages.len(), 10);
    // Fetched again, read on another client, a message changes where it is
    // kept.
    let (written, _) = written(&chain, || {
        chain
            .invoke(&MessageStates::INGEST.with(message_states::Ingest {
                messages: vec![IngestBaseline {
                    read: Some(true),
                    account: Some("me@example.test".into()),
                    locator: Some("3@example.test".into()),
                    ..IngestBaseline::register_only(id(3))
                }],
            }))
            .unwrap();
    });
    assert_eq!(written, [0]);
}

#[test]
fn a_confirmation_writes_only_the_part_holding_its_message() {
    let chain = mailbox(PART_MESSAGES * 2);
    mark_read(&chain, vec![id(PART_MESSAGES + 5)]);
    let [(_, update)] = chain
        .parts()
        .iter()
        .flat_map(|(_, part)| owed(part))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let (parts, _) = written(&chain, || {
        chain
            .invoke(
                &MessageStates::SERVER_APPLIED.with(message_states::ServerApplied {
                    updates: vec![update],
                }),
            )
            .unwrap();
    });
    assert_eq!(parts, [1]);
    assert!(
        chain.parts().iter().all(|(_, part)| owed(part).is_empty()),
        "nothing is owed once the server confirmed it"
    );
}
