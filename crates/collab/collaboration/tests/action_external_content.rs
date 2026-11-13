//! An action over the history budget, made on one replica and decoded on
//! another: its large parts travel as the record's sealed content, and the
//! replica that receives the change fetches them on the record's route.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collaboration::history_source::authors::{HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME};
use collaboration::history_source::verbs::step_summary;
use collaboration::history_source::words::HistoryWords;
use collaboration::record_content::{ContentLane, RecordContent};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContext, ActionError, ActionRegistry, Doc, EncodedAction, LocalTransport,
    MissingContent, Verified, dispatch_with_context, entry_of, verify_change,
};
use tilo_content::ContentStore;
use tilo_rt::wake::WakeScope;

const APP: &str = "so.tilo.action-external-content-test";

/// The words of a window reading the history: a step by the verb it ran.
struct Words;

impl ResolveAuthor for Words {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        HistoryAuthor::new(UNKNOWN_AUTHOR_NAME, 0.0, actor_hex)
    }
}

impl HistoryWords for Words {
    fn sentence(&self, entry: &ActionContext) -> String {
        match &entry.what {
            Some(tilo_actions::Did::Verb(verb)) => verb.action.action.clone(),
            _ => entry.purpose.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Paste(String);

impl Action for Paste {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "body", self.0.as_str())?;
        Ok(())
    }

    fn describe(&self, _doc: &Doc) -> Option<tilo_actions::Described> {
        Some(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("test", "test.paste", "paste"),
            [(
                "characters".to_owned(),
                i64::try_from(self.0.chars().count())
                    .unwrap_or(i64::MAX)
                    .into(),
            )],
        )))
    }
}

fn registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    registry.register::<Paste>("test.paste");
    registry
}

/// The content store of record `record`: sealed, and joined to the record's
/// route, as a window's record content is. A lane belongs to a record of this
/// process, so the two replicas here name their own.
fn record_store(record: &str) -> (ContentStore, Arc<ContentLane>) {
    let lane = ContentLane::of(APP, record);
    (
        RecordContent::store(ContentStore::memory(), Arc::clone(&lane)),
        lane,
    )
}

#[test]
fn an_action_over_the_budget_keeps_its_parts_sealed_and_decodes_on_the_replica_that_receives_it() {
    let body = "p".repeat(tilo_actions::context::HISTORY_BUDGET + 1);

    // Replica A: a window records an action too large to inline.
    let window = WakeScope::new();
    let (store_a, lane_a) = record_store("the note, on A");
    let mut origin = AutoCommit::new();
    {
        let _in_the_window = window.enter();
        tilo_actions::register(&window.handle(), store_a);
        let mut transport = LocalTransport::new();
        dispatch_with_context(
            &mut origin,
            &Paste(body.clone()),
            EncodedAction::encode("test.paste", &Paste(body.clone())).unwrap(),
            &mut transport,
            ActionContext::for_resource("r", "k", "l"),
        )
        .unwrap();
    }
    let hash = origin.get_heads()[0];
    let entry_a = entry_of(origin.committed_document(), hash).expect("the change names its action");
    let recorded = entry_a
        .action
        .clone()
        .expect("the change records its action");
    let parts = recorded.content_parts();
    assert_eq!(parts.len(), 1, "the long body is kept as content");
    assert!(tilo_content::is_sealed(&parts[0]), "{parts:?}");
    let offers = lane_a.take_requests().offers;
    let [(offered, sealed)] = offers.as_slice() else {
        panic!("one offer on the record's route, got {}", offers.len());
    };
    assert_eq!(
        Some(offered.as_str()),
        tilo_content::content_address(&parts[0]).as_deref(),
        "offered under the address its reference names",
    );
    assert!(tilo_content::verify(offered, sealed));
    assert!(
        !sealed
            .windows(body.len().min(64))
            .any(|window| window == &body.as_bytes()[..body.len().min(64)]),
        "no part in the clear"
    );

    // Replica B: it holds the change, not the bytes, and decodes through its
    // own record's content store, on no window's thread.
    let (store_b, lane_b) = record_store("the note, on B");
    let mut registry_b = registry();
    registry_b.connect_content(store_b);
    let mut peer = origin.fork();
    assert_eq!(
        registry_b.resolve(&recorded).unwrap(),
        Err(MissingContent(parts[0].clone())),
        "the part is not here yet"
    );
    assert_eq!(
        lane_b.take_requests().wants,
        vec![offered.clone()],
        "the replica asks its record's route for it"
    );
    for (address, sealed) in offers {
        lane_b.arrive(address, sealed);
    }

    let resolved = registry_b.resolve(&recorded).unwrap().unwrap();
    let action: Paste = serde_json::from_slice(&resolved.payload).unwrap();
    assert_eq!(action.0, body, "the action decodes whole");

    // Its history entry reads as the action the change ran, and the change
    // reproduces from it: what a peer checks a grant against.
    let entry_b = entry_of(peer.committed_document(), hash).expect("the change names its action");
    assert_eq!(entry_b.what, entry_a.what);
    assert_eq!(entry_b.action, entry_a.action);
    let row = step_summary(&entry_b, &Words).expect("the row says what it did");
    assert_eq!(
        row,
        step_summary(&entry_a, &Words).unwrap(),
        "both replicas read the change as the same action"
    );
    assert!(row.contains("paste"), "{row}");
    assert_eq!(
        verify_change(&mut peer, hash, &registry_b).unwrap(),
        Verified::Reproduced
    );
}
