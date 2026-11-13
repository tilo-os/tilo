//! A paste past the history budget, made in a native window, reaches another
//! device: its change arrives there without the body, the window asks the
//! record's route for it, and the store the window's code reads keeps what
//! arrives — its history shows the words and the action replays.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContext, ActionError, ActionRegistry, Doc, EncodedAction, LocalTransport,
    MissingContent, Verified, dispatch_with_context, entry_of, verify_change,
};
use tilo_content::ContentStore;
use tilo_ui::collaboration::record_content::{ContentLane, RecordContent, keep_lane};
use tilo_ui::collaboration::{CollabEvent, RouteSpec};
use tilo_ui::widget::{Column, Text};
use tilo_ui::{App, State, WakeScope};

const APP: &str = "so.tilo.record-content-route-arrival-test";

/// A note's body, pasted whole.
#[derive(Debug, Serialize, Deserialize)]
struct Paste(String);

impl Action for Paste {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "body", self.0.as_str())?;
        Ok(())
    }
}

/// A window showing the record `route` names: the store its code reads is
/// the host's local store joined to the lane the window keeps and offers on
/// the record's route, as a native window's framework lends it to the app.
fn window_store(
    route: &RouteSpec,
    local: ContentStore,
    window: &WakeScope,
) -> (ContentStore, Arc<ContentLane>) {
    let lane = ContentLane::of_window();
    keep_lane(&window.handle(), Arc::clone(&lane));
    ContentLane::show_on_route(&route.app_id, route.record.as_deref().unwrap_or(""), &lane);
    (RecordContent::store(local, Arc::clone(&lane)), lane)
}

/// A window of the app showing `route`'s record, as the session mounts one.
fn window() -> App {
    App::new(
        State::new(),
        Box::new(Column::new().push(Box::new(Text::new("the note")))),
    )
}

#[test]
fn a_paste_arriving_on_the_records_route_reaches_the_store_the_peer_window_reads() {
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    // The words Ada pastes: past a mebibyte, more than a change records whole.
    let body = "pasted words ".repeat(90_000);
    assert!(body.len() > 1 << 20, "the paste is past a mebibyte");

    // Ada's window: what its code records of the paste is kept as the
    // record's content, sealed, and offered on the record's route.
    let ada = WakeScope::new();
    let (ada_store, ada_lane) = {
        let _in_ada = ada.enter();
        window_store(&route, ContentStore::memory(), &ada)
    };
    let mut ada_doc = AutoCommit::new();
    {
        let _in_ada = ada.enter();
        tilo_actions::register(&ada.handle(), ada_store);
        let mut transport = LocalTransport::new();
        dispatch_with_context(
            &mut ada_doc,
            &Paste(body.clone()),
            EncodedAction::encode("test.paste", &Paste(body.clone())).unwrap(),
            &mut transport,
            ActionContext::for_resource("notes/the-note", "test.note", "the note"),
        )
        .unwrap();
    }
    let paste = ada_doc.get_heads()[0];
    let recorded = entry_of(ada_doc.committed_document(), paste)
        .expect("the change names its action")
        .action
        .expect("the change records its action");
    let parts = recorded.content_parts();
    assert!(
        !parts.is_empty(),
        "the body past the history budget is kept as content"
    );
    let offers = ada_lane.take_requests().offers;
    assert!(
        parts.iter().all(|part| {
            let address = tilo_content::content_address(part).unwrap_or_else(|| part.clone());
            offers.iter().any(|(offered, _)| offered == &address)
        }),
        "every part of the paste is offered on the record's route"
    );

    // Grace's device: her window holds the change and not the body, so the
    // action cannot decode there yet, and reading it asks the route for the
    // parts.
    let grace = WakeScope::new();
    let (grace_store, grace_lane) = {
        let _in_grace = grace.enter();
        window_store(&route, ContentStore::memory(), &grace)
    };
    let mut app = window();
    let mut registry = ActionRegistry::new();
    registry.register::<Paste>("test.paste");
    registry.connect_content(grace_store.clone());
    let missing = registry
        .resolve(&recorded)
        .expect("a recorded action resolves")
        .expect_err("the body is not on Grace's device yet");
    assert!(matches!(missing, MissingContent(_)), "{missing:?}");
    let wanted: Vec<String> = grace_lane.take_requests().wants;
    assert!(
        parts.iter().any(|part| {
            let address = tilo_content::content_address(part).unwrap_or_else(|| part.clone());
            wanted.contains(&address)
        }),
        "the window asks the record's route for the body: wanted {wanted:?}"
    );

    // The parts arrive on the record's route, as the window's collaboration
    // loop hands them over.
    for (address, sealed) in offers {
        app.apply_collab_event_for_test(CollabEvent::RecordContent {
            route: route.clone(),
            hash: address,
            bytes: sealed,
        });
    }

    assert!(
        parts.iter().all(|part| grace_store.has(part)),
        "the store Grace's window reads keeps every part of the paste"
    );
    let resolved = registry
        .resolve(&recorded)
        .expect("a recorded action resolves")
        .expect("the body is here now");
    let action: Paste = serde_json::from_slice(&resolved.payload).unwrap();
    assert_eq!(action.0, body, "the pasted words decode whole");

    // Her history can check the change against the action it ran: what a
    // grant on the record is checked against, and what its undo names.
    let mut peer = ada_doc.fork();
    assert_eq!(
        verify_change(&mut peer, paste, &registry).unwrap(),
        Verified::Reproduced,
        "the paste reproduces from its action on Grace's device"
    );
}
