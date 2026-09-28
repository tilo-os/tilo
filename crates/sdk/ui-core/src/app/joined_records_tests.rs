//! A widget holding a set of records: the host joins, merges, publishes and
//! indexes them.
use super::*;
use crate::record::{JoinedRecords, RecordCheckout, RecordId, RecordKind, RecordOwners};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "so.tilo.joined-fixture";
const ROOM: &str = "00000000-0000-0000-0000-000000000099";
const ME: [u8; 32] = [7; 32];
const THEM: [u8; 32] = [9; 32];

/// A conversation-like widget: it follows every comment on `parent` the room
/// lists, and posts its own.
struct Conversation {
    joined: JoinedRecords,
    parent: RecordId,
    seen_index: Rc<RefCell<usize>>,
}

impl Widget for Conversation {
    fn sync(&mut self, _state: &mut automerge::AutoCommit, _path: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &crate::theme::Theme,
        _node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy.new_leaf(taffy::Style::default()).unwrap()
    }
    fn apply_layout(
        &mut self,
        _taffy: &taffy::TaffyTree<()>,
        _node: taffy::NodeId,
        _origin: vello::kurbo::Point,
    ) {
    }
    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut crate::text_render::TextRenderer,
        _theme: &crate::theme::Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "Conversation"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn joined_records(&self) -> Option<&JoinedRecords> {
        Some(&self.joined)
    }
    fn room_index_changed(&mut self, index: &automerge::AutoCommit) {
        *self.seen_index.borrow_mut() += 1;
        let listed = crate::record::room::comments_on(index, &self.parent);
        self.joined.join(&listed);
    }
}

fn comment(author: [u8; 32], local: &str, parent: &RecordId) -> RecordCheckout {
    RecordCheckout::new(
        RecordId::for_comment(author, local, parent).unwrap(),
        RecordKind::new("fixture.message"),
    )
}

fn routed(
    route: &crate::collaboration::RouteSpec,
    author: [u8; 32],
    doc: &mut automerge::AutoCommit,
) -> collab_types::SignedOp {
    collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(doc.save())
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::Ed25519PublicKey::from_bytes(author),
        collab_types::document_attestation_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::OpClass::Comment,
        crate::time::SystemTime::UNIX_EPOCH,
        0,
    )
}

#[test]
fn a_widgets_joined_records_are_indexed_joined_published_and_merged() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(temp.path().to_path_buf());
    let store = owners.open(APP_ID, ROOM).unwrap();
    let parent = RecordId::parse("uuid:0c4e8a2f-1b3d-4f5a-9c7e-6d8b0a1c2e3f").unwrap();
    let seen = Rc::new(RefCell::new(0));
    let mut conversation = Conversation {
        joined: JoinedRecords::new(store.clone()),
        parent: parent.clone(),
        seen_index: Rc::clone(&seen),
    };
    conversation.joined.set_comment_author(Some(ME));
    let mine = comment(ME, "hello", &parent);
    conversation
        .joined
        .create(&mine, |doc| {
            doc.put(ROOT, "text", "mine").map_err(|e| e.to_string())
        })
        .unwrap();

    let mut index = automerge::AutoCommit::new();
    let theirs = comment(THEM, "reply", &parent);
    // Somebody else's comment is already in the room.
    assert!(crate::record::room::reference(
        &mut index,
        &theirs.id,
        &theirs.kind
    ));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(conversation))
        .with_records(crate::record::RecordSet::in_memory().with_home(ROOM), index);
    app.collaboration.manager = CollaborationManager::with_peer_transport(
        super::legacy_resource_tests::DocumentRouteTransport,
    );
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::app(APP_ID, uuid::Uuid::parse_str(ROOM).unwrap()),
        epoch: uuid::Uuid::new_v4(),
    });

    app.sync_joined_records();
    // The host brought the created record into the room, and the widget,
    // told the index changed, joined both comments on its parent.
    let index = app.room_index().unwrap();
    assert!(crate::record::room::holds(index, &mine.id));
    let listed = crate::record::room::comments_on(index, &parent);
    assert_eq!(listed.len(), 2);
    assert_eq!(*seen.borrow(), 1);
    app.sync_joined_records();
    assert_eq!(*seen.borrow(), 1, "an unchanged index is not news");

    app.refresh_document_routes();
    assert_eq!(
        app.document_routes.owners.len(),
        1,
        "own record is an owner"
    );
    assert_eq!(app.document_routes.followers.len(), 1, "theirs is followed");
    let follower_route = app.document_routes.followers[0].route.clone();
    assert_eq!(follower_route.record.as_deref(), Some(theirs.id.as_str()));

    // Own edits publish; nothing is ever published to a followed record.
    app.publish_owned_documents();
    assert_eq!(app.document_routes.sent.len(), 1);
    assert_eq!(
        app.document_routes.sent[0].0.record.as_deref(),
        Some(mine.id.as_str())
    );

    // Their change merges into the replica; a stranger's is refused.
    let mut remote = automerge::AutoCommit::new();
    remote.put(ROOT, "text", "theirs").unwrap();
    remote.commit();
    let stranger = routed(&follower_route, [1; 32], &mut remote);
    app.apply_signed_document_envelope(&stranger, None);
    let text = |app: &App| {
        app.root_widget
            .joined_records()
            .unwrap()
            .read(&theirs.id, |doc| {
                doc.get(ROOT, "text")
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_owned))
            })
            .flatten()
    };
    assert_eq!(text(&app), None, "only the author writes their record");
    let authored = routed(&follower_route, THEM, &mut remote);
    app.apply_signed_document_envelope(&authored, None);
    assert_eq!(text(&app).as_deref(), Some("theirs"));
    assert!(app.needs_sync);
}
