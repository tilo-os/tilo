//! A person's record changes as trigger events.
//!
//! Each look reads, per record a shortcut of this device may watch, the
//! changes after the heads it last saw and turns each one that carries a
//! history entry into a [`RecordEvent`]. The first look at a record only
//! learns its heads: a shortcut saved today never fires on last year's
//! messages.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::ActionContext;
use tilo_automation::dispatch::{RecordEvent, event_of};
use ui::record::{RecordId, RecordStore};

/// The changes of `doc` after `since`, in causal order, as events.
#[must_use]
pub fn record_events(
    room: &str,
    record: &str,
    kind: &str,
    doc: &AutoCommit,
    since: &[ChangeHash],
) -> Vec<RecordEvent> {
    let committed = doc.committed_document();
    if committed.get_heads() == since {
        return Vec::new();
    }
    committed
        .get_changes(since)
        .into_iter()
        .filter_map(|change| {
            let context = ActionContext::from_change_message(change.message()?)?;
            Some(event_of(
                room,
                record,
                kind,
                &change.hash().to_string(),
                &context,
            ))
        })
        .collect()
}

/// Per-record cursors over what this device holds.
#[derive(Debug, Default)]
pub struct RecordFeed {
    cursors: BTreeMap<(String, String), Vec<ChangeHash>>,
}

impl RecordFeed {
    /// New events in `rooms` (room id, then record id to kind), in order.
    pub fn poll(
        &mut self,
        rooms: &[(String, BTreeMap<String, String>)],
        records: &RecordStore,
    ) -> Vec<RecordEvent> {
        let mut events = Vec::new();
        for (room, held) in rooms {
            for (record, kind) in held {
                let Some(id) = RecordId::parse(record) else {
                    continue;
                };
                let key = (room.clone(), record.clone());
                let since = self.cursors.get(&key);
                let read = records.read(&id, |doc| {
                    let heads = doc.committed_heads();
                    let new = since.map(|since| record_events(room, record, kind, doc, since));
                    (heads, new)
                });
                let Some((heads, new)) = read else {
                    continue;
                };
                events.extend(new.unwrap_or_default());
                self.cursors.insert(key, heads);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable as _};

    #[test]
    fn a_committed_verb_becomes_a_record_event_with_its_actor_and_facet() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "seed", 1).unwrap();
        let before = doc.get_heads();
        let context = ActionContext::for_app("so.tilo.chat", "Chat")
            .with_invoker(tilo_actions::ActionInvoker::actor("bob"))
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                [],
            )))
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::INCOMING_MESSAGE_FACET,
                "true",
            ));
        doc.put(automerge::ROOT, "text", "hi").unwrap();
        let hash = doc
            .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
            .unwrap();
        let events = record_events("room-1", "rec-1", "chat.workspace", &doc, &before);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.change, hash.to_string());
        assert_eq!(event.actor.id, "bob");
        assert!(event.incoming_message);
        assert_eq!(
            tilo_automation::dispatch::verbs_of(&event.did),
            vec!["post".to_string()]
        );
        // Nothing new after the last look.
        assert!(
            record_events(
                "room-1",
                "rec-1",
                "chat.workspace",
                &doc,
                &doc.committed_heads()
            )
            .is_empty()
        );
    }

    #[test]
    fn a_change_without_an_entry_is_no_event() {
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "x", 1).unwrap();
        doc.commit();
        assert!(record_events("r", "rec", "k", &doc, &before).is_empty());
    }
}
