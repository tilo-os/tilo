//! Installed bots on the room's host: what a bot reads of the records it
//! watches.

pub mod door;
pub mod held;
pub mod item_json;
pub mod participant;

use automerge::ChangeHash;
use tilo_automation::dispatch::{self, RecordEvent};

pub use item_json::item_json;

/// The changes of `doc` after `since` (hex heads), in causal order, as the
/// events their recorded entries describe, each with the heads after it.
///
/// Changes that carry no entry (a genesis, a merge with no action) yield
/// no event but still advance the heads, so a cursor set from the last
/// heads never reads them again. The item a change touched is read as it
/// was before (at the change's parents) and after (at the change).
#[must_use]
pub fn events_since(
    room: &str,
    record: &str,
    kind: &str,
    doc: &automerge::Automerge,
    since: &[String],
) -> Vec<(RecordEvent, Vec<String>)> {
    let since: Vec<ChangeHash> = since.iter().filter_map(|hex| hex.parse().ok()).collect();
    let mut heads: Vec<ChangeHash> = since.clone();
    let mut out = Vec::new();
    for change in doc.get_changes(&since) {
        let parents = change.deps().to_vec();
        let hash = change.hash();
        heads.retain(|head| !parents.contains(head));
        heads.push(hash);
        let Some(context) = change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message)
        else {
            continue;
        };
        let mut event = dispatch::event_of(room, record, kind, &hash.to_string(), &context);
        if let Some(item) = &event.item {
            event.before = item_json(doc, &parents, &item.id);
            event.after = item_json(doc, &[hash], &item.id);
        }
        out.push((event, heads.iter().map(ToString::to_string).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use automerge::transaction::{CommitOptions, Transactable as _};

    use super::*;

    fn hex(heads: &[ChangeHash]) -> Vec<String> {
        heads.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn events_after_a_cursor_carry_the_item_before_and_after() {
        let mut doc = automerge::AutoCommit::new();
        let messages = doc
            .put_object(automerge::ROOT, "messages", automerge::ObjType::Map)
            .unwrap();
        let m1 = doc
            .put_object(&messages, "m1", automerge::ObjType::Map)
            .unwrap();
        doc.put(&m1, "text", "hi").unwrap();
        doc.commit();
        let cursor = doc.get_heads();
        let mut context =
            tilo_actions::ActionContext::for_resource("chat.workspace", "chat.workspace", "Chat")
                .with_invoker(tilo_actions::ActionInvoker::actor("bob"));
        context.item = Some(tilo_actions::entry::ActionItem {
            id: "m1".into(),
            label: "hi".into(),
        });
        doc.put(&m1, "text", "edited").unwrap();
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
        let events = events_since(
            "room",
            "rec",
            "chat.workspace",
            doc.document(),
            &hex(&cursor),
        );
        assert_eq!(events.len(), 1);
        let (event, heads) = &events[0];
        assert_eq!(event.actor.id, "bob");
        assert_eq!(event.before.as_ref().unwrap()["text"], "hi");
        assert_eq!(event.after.as_ref().unwrap()["text"], "edited");
        assert_eq!(heads, &hex(&doc.get_heads()));
        assert!(
            events_since("room", "rec", "chat.workspace", doc.document(), heads).is_empty(),
            "nothing is read twice past the heads it returned"
        );
    }
}
