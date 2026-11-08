//! An app's record held as chunk records: the record keeps a document per chunk, one action
//! across chunks is one workspace commit every part of which an undo takes back, a change syncs
//! in its chunk alone through the routes the collab client joins, a replica that joins gets the
//! manifest and every chunk, and a record written before chunks opens as one chunk unchanged.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "index arithmetic over test data, and one test per story"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use serde_json::json;
use tilo_actions::ActionInvoker;
use tilo_automerge::ApplyIncoming as _;
use tilo_render_protocol::patch::local::{End, pair};
use tilo_resource::{Edit, EditOp, Model as _, Segment};
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::place::{Place, Placement};
use tilolang_runtime::store::{FieldSpec, Pred, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::chunk::{self, ChunkNo, Chunks};
use tilolang_tilo::host::{Host, Opening, RootView};
use tilolang_tilo::instance::record::InstanceRecord;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::items::Items;
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::runtime::{MemorySink, NameHost, Principal};
use tilolang_tilo::schema::{TypeSchema, lookup};

use crate::chunks::{BOARD, BOARD_APP, Docs, act, board, held_in, left};
use crate::fixtures::field_id;

type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;
type Members = BTreeMap<String, Vec<(String, Inbox)>>;

/// One document a replica handed the workspace: on which route, and the action's commit when
/// the change was a part of one.
#[derive(Clone, Debug)]
struct Sent {
    from: String,
    route: RouteSpec,
    mutation: collab_types::DocumentMutation,
    commit: Option<collab_types::WorkspaceCommit>,
}

/// The workspace's routes as a broker relays them: every replica joined to a route gets what
/// another sends on it, as the signed envelope the broker delivers, and a replica that joins a
/// route another one holds asks that one for the whole document, as the broker's read does.
#[derive(Clone, Default)]
struct Wire {
    members: Rc<RefCell<Members>>,
    sent: Rc<RefCell<Vec<Sent>>>,
}

impl Wire {
    fn join(&self, me: &str, route: &RouteSpec, mine: &Inbox) {
        let mut members = self.members.borrow_mut();
        let ends = members.entry(route.channel_id()).or_default();
        ends.retain(|(who, _)| who != me);
        let holder = ends.first().map(|(_, inbox)| Rc::clone(inbox));
        ends.push((me.to_owned(), Rc::clone(mine)));
        drop(members);
        if let Some(holder) = holder {
            holder
                .borrow_mut()
                .push_back(CollabEvent::DocumentSnapshotRequested {
                    route: route.clone(),
                });
        }
    }

    fn deliver(&self, from: &str, route: &RouteSpec, request: CollabRequest) -> Result<(), String> {
        let CollabRequest::Document {
            mutation, commit, ..
        } = request
        else {
            return Ok(());
        };
        self.sent.borrow_mut().push(Sent {
            from: from.to_owned(),
            route: route.clone(),
            mutation: mutation.clone(),
            commit,
        });
        let ends: Vec<Inbox> = self
            .members
            .borrow()
            .get(&route.channel_id())
            .map(|ends| {
                ends.iter()
                    .filter(|(who, _)| who != from)
                    .map(|(_, inbox)| Rc::clone(inbox))
                    .collect()
            })
            .unwrap_or_default();
        for inbox in ends {
            inbox
                .borrow_mut()
                .push_back(delivered(route, from, &mutation)?);
        }
        Ok(())
    }

    /// How many documents the replicas handed the workspace so far.
    fn count(&self) -> usize {
        self.sent.borrow().len()
    }

    /// What was handed the workspace since the `from`th document.
    fn since(&self, from: usize) -> Vec<Sent> {
        self.sent.borrow().get(from..).unwrap_or_default().to_vec()
    }
}

/// One replica's end of the routes, as a broker relays them.
struct Routed {
    me: String,
    wire: Wire,
    mine: Inbox,
    primary: Option<RouteSpec>,
    answer: Option<Result<RouteSpec, String>>,
}

impl PeerTransport for Routed {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match request {
            CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } => {
                let route = RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                };
                self.wire.join(&self.me, &route, &self.mine);
                self.primary = Some(route.clone());
                self.answer = Some(Ok(route));
                Ok(())
            }
            request => {
                let route = self.primary.clone().ok_or("no primary route")?;
                self.wire.deliver(&self.me, &route, request)
            }
        }
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.mine.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.answer.take()
    }

    fn join_route(
        &mut self,
        route: RouteSpec,
        _kind: collaboration::record::RecordKind,
    ) -> Result<(), String> {
        self.wire.join(&self.me, &route, &self.mine);
        Ok(())
    }

    fn send_to_route(&mut self, route: RouteSpec, request: CollabRequest) -> Result<(), String> {
        self.wire.deliver(&self.me, &route, request)
    }
}

/// `mutation`, sent by `me` on `route`, as the signed document envelope the broker delivers to
/// the route's other replicas.
fn delivered(
    route: &RouteSpec,
    me: &str,
    mutation: &collab_types::DocumentMutation,
) -> Result<CollabEvent, String> {
    let record = route.record.as_deref().unwrap_or("");
    let workspace = route.cluster_id.to_string();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().map_err(|error| error.to_string())?,
        collab_types::document_artifact_id(&route.app_id, &workspace, record),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::document_attestation_id(&route.app_id, &workspace, record),
        collab_types::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        0,
    );
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    Ok(CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: me.to_owned(),
        decrypted_payload: None,
    })
}

/// The board, loaded with the ids the workspace's types were given.
struct Board {
    ids: RefCell<String>,
}

impl Platform for Board {
    fn load(&self, host: &Host) -> u64 {
        let package = host.load_builtin(BOARD, &self.ids.borrow()).unwrap();
        *self.ids.borrow_mut() = package.ids;
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-05T10:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
    }
}

/// One replica: its window over the board, and the record it holds the chunks in.
struct Window {
    instance: Instance,
    workspace: InstanceRecord,
    mine: Inbox,
    _patches: End,
}

/// A window of the board for `me`, holding `chunks`, committing on `route` through the wire.
fn window(
    me: &str,
    route: &RouteSpec,
    platform: &Rc<Board>,
    wire: &Wire,
    chunks: Chunks,
) -> Window {
    let mine: Inbox = Rc::default();
    let mut manager = CollaborationManager::with_peer_transport(Routed {
        me: me.to_owned(),
        wire: wire.clone(),
        mine: Rc::clone(&mine),
        primary: None,
        answer: None,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.record.clone(),
        record_kind: None,
        handle: Some(me.to_owned()),
    }));
    let _ = manager.poll();
    let (app_end, window_end) = pair();
    let instance = Instance::new(
        Config {
            app: BOARD_APP.to_owned(),
            me: me.to_owned(),
            mode: Mode::Live,
            device: None,
            index_dir: None,
            record_loader: None,
            root: RootView::default(),
            sink: Some(Box::new(MemorySink::new())),
        },
        Rc::clone(platform) as Rc<dyn Platform>,
        Rc::new(Alone),
        Box::new(app_end),
    );
    let workspace = InstanceRecord::of(chunks, manager, route.clone(), me);
    Window {
        instance,
        workspace,
        mine,
        _patches: window_end,
    }
}

/// The window reads what its record holds: the record's chunk, then the chunks above it, a
/// frame at a time, and derives them off the frame until its index covers the record.
fn catch_up(window: &mut Window) {
    window.instance.sync_chunks(window.workspace.chunks());
    window.instance.update();
    if let Some(host) = window.instance.host() {
        host.wait_index();
        window.instance.update();
    }
}

/// The board's cards, `n` of them in the order they were added, written straight into the chunk
/// of the run each belongs to, and each run recorded in the record's manifest: the documents the
/// store's planner would have written, one action at a time being minutes of work at this size.
/// Their ids.
fn bulk(docs: &mut Docs, meta: &Meta, root: &str, n: usize) -> Vec<String> {
    let field = field_id(meta, "Board", "left");
    let per = usize::try_from(chunk::ITEMS).unwrap();
    let ids: Vec<String> = (0..n).map(|index| format!("c{index}")).collect();
    for (round, run) in ids.chunks(per).enumerate() {
        let number = chunk::FIRST + u64::try_from(round).unwrap();
        insert(docs, meta, root, number, run, round * per);
        let last = format!("{:010}", round * per + run.len() - 1);
        let len = u64::try_from(run.len()).unwrap();
        let record = docs.doc(chunk::FIRST);
        chunk::run(record, root, &field, number, &last, len).unwrap();
        record.commit();
    }
    ids
}

/// The cards `ids` written into chunk `number` of the board `root`, as the items of its `left`
/// collection, their order keys starting at `offset`: one action, as the door would have made
/// them.
fn insert(
    docs: &mut Docs,
    meta: &Meta,
    root: &str,
    number: ChunkNo,
    ids: &[String],
    offset: usize,
) {
    let (field, ty) = (field_id(meta, "Board", "left"), card_type(meta));
    let schema = lookup(&ty).unwrap();
    let edits: Vec<DynEdit> = ids
        .iter()
        .enumerate()
        .map(|(at, id)| {
            DynEdit::Insert {
                id: root.to_owned(),
                item: id.clone(),
                record: DynRecord::empty(schema).unwrap().to_json(),
                at: Placement::At {
                    place: Place {
                        parent: None,
                        field: field.clone(),
                        key: format!("{:010}", offset + at),
                    },
                    was: None,
                }
                .encode(),
            }
            .in_chunk(number)
        })
        .collect();
    docs.apply(&DynAction {
        app: BOARD_APP.to_owned(),
        kind: ty,
        action_id: "add".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits,
    });
}

/// A record as workspaces written before chunks hold it: its cards in the record itself, and no
/// manifest naming anything else. Their ids.
fn pre_chunks(docs: &mut Docs, meta: &Meta, root: &str, n: usize) -> Vec<String> {
    let ids: Vec<String> = (0..n).map(|index| format!("c{index}")).collect();
    insert(docs, meta, root, chunk::FIRST, &ids, 0);
    docs.doc(chunk::FIRST).commit();
    ids
}

/// The id of the board's card type.
fn card_type(meta: &Meta) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == "Card")
        .unwrap()
        .id
        .clone()
}

/// The board's schema.
fn board_schema(meta: &Meta) -> TypeSchema {
    lookup(
        &meta
            .types
            .iter()
            .find(|declared| declared.name == "Board")
            .unwrap()
            .id,
    )
    .unwrap()
    .clone()
}

/// The chunks whose heads moved from `before` to `after`.
fn moved(before: &chunk::Stamps, after: &chunk::Stamps) -> Vec<ChunkNo> {
    after
        .iter()
        .filter(|(number, heads)| before.get(*number) != Some(*heads))
        .map(|(number, _)| *number)
        .collect()
}

#[test]
fn a_hundred_thousand_cards_open_and_list_across_their_chunks() {
    let (_host, mut docs, meta, root, _ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 100_000);

    // The record holds every chunk the runs filled, the record itself first.
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let record = InstanceRecord::of(
        docs.chunks(),
        CollaborationManager::with_peer_transport(Routed {
            me: "ada".to_owned(),
            wire: Wire::default(),
            mine: Rc::default(),
            primary: None,
            answer: None,
        }),
        route,
        "ada",
    );
    let per = usize::try_from(chunk::ITEMS).unwrap();
    let numbers: Vec<ChunkNo> = record.chunks().numbers().collect();
    assert_eq!(numbers.len(), 100_000usize.div_ceil(per));
    assert_eq!(numbers.first(), Some(&0));
    assert!(numbers.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert_eq!(
        record.chunks().placement().chunks().len(),
        numbers.len() - 1,
        "the manifest names every chunk above the record"
    );

    // Opening the record lists all of them, in the order they were added, under the ids they
    // were given, and each is read from the chunk its run holds.
    let schema = board_schema(&meta);
    let field = field_id(&meta, "Board", "left");
    let found = Items::across(record.chunks(), BOARD_APP, &root, &schema).resolve(&schema);
    assert_eq!(inserted.len(), 100_000);
    assert_eq!(found.items(None, &field), inserted);
    let held: Vec<usize> = numbers
        .iter()
        .map(|number| held_in(record.chunks(), *number, &root, &schema))
        .collect();
    assert_eq!(held.len(), numbers.len(), "every card is in one chunk");
    assert_eq!(held.iter().sum::<usize>(), inserted.len());
    assert!(held.iter().all(|n| *n <= per));
}

#[test]
fn a_retitle_writes_and_syncs_the_card_s_chunk_alone() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 4200);
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);

    let before = ada.workspace.marks();
    let handed = wire.count();
    let card = format!("{root}/{}", inserted[4150]);
    let action = act(
        &ada.instance.host().unwrap(),
        &meta,
        ("Card", &card),
        "retitle",
        json!({ "text": "changed" }),
    );
    assert_eq!(
        action
            .edits
            .iter()
            .filter_map(DynEdit::chunk)
            .collect::<Vec<_>>(),
        [2],
        "the edit is addressed to the card's chunk alone"
    );
    ada.workspace
        .apply_invoked(
            &mut ada.instance,
            &action,
            &ActionInvoker::actor("ada"),
            None,
        )
        .unwrap();
    catch_up(&mut ada);

    assert_eq!(
        moved(&before, &ada.workspace.marks()),
        [2],
        "only the card's chunk took the write"
    );
    // One change went out, on the card's chunk's route: a change of one chunk is no action
    // across units, so it travels as any change does, under no commit.
    let sent = wire.since(handed);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    assert!(matches!(
        sent[0].mutation,
        collab_types::DocumentMutation::Incremental(_)
    ));
    assert_eq!(sent[0].route, ada.workspace.chunk_route(2));
    assert_eq!(sent[0].commit, None);

    // Every card reads where it was, and the retitled one says what it was told.
    let host = ada.instance.host().unwrap();
    let field = field_id(&meta, "Board", "left");
    assert_eq!(left(&host, &root, &field), inserted);
    // The index reads the card's fields from the chunk that holds it: a retitle in a chunk
    // above the record's is the card's new text, not an empty one read from the record.
    let card_at =
        ResourceRef::root(&root, board_schema(&meta).id).item(&inserted[4150], card_type(&meta));
    assert_eq!(
        host.store().first_text(&card_at).map(|(text, _)| text),
        Some("changed".to_owned()),
    );
}

#[test]
fn a_card_moved_across_chunks_keeps_its_id_and_history_and_undo_brings_it_back() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 4200);
    let (left_field, right_field) = (
        field_id(&meta, "Board", "left"),
        field_id(&meta, "Board", "right"),
    );
    // The collection the card moves to has its run in a chunk of its own.
    {
        let record = docs.doc(chunk::FIRST);
        chunk::run(record, &root, &right_field, 1, "", 0).unwrap();
        record.commit();
    }

    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);

    // The card moves from the record's own chunk into the run's chunk in one action.
    let card = inserted[0].clone();
    let action = act(
        &ada.instance.host().unwrap(),
        &meta,
        ("Board", &root),
        "move_right",
        json!({ "c": format!("{root}/{card}") }),
    );
    let mut written: Vec<ChunkNo> = action.edits.iter().filter_map(DynEdit::chunk).collect();
    written.sort_unstable();
    written.dedup();
    assert_eq!(
        written,
        [0, 1],
        "the card leaves one chunk and lands in another"
    );

    let handed = wire.count();
    ada.workspace
        .apply_invoked(
            &mut ada.instance,
            &action,
            &ActionInvoker::actor("ada"),
            None,
        )
        .unwrap();
    catch_up(&mut ada);
    assert_eq!(ada.workspace.can_undo(), (true, false));

    // Its parts are one action of the workspace: every part carries the same commit, naming
    // both units.
    let sent = wire.since(handed);
    assert_eq!(sent.len(), 2, "{sent:#?}");
    let commit = sent[0]
        .commit
        .clone()
        .expect("one action across two chunks travels as one commit");
    assert_eq!(sent[1].commit.as_ref(), Some(&commit));
    assert_eq!(commit.parts.len(), 2);
    assert_eq!(
        sent.iter()
            .map(|part| part.route.clone())
            .collect::<Vec<_>>(),
        [ada.workspace.chunk_route(0), ada.workspace.chunk_route(1)]
    );
    assert_eq!(
        commit
            .parts
            .iter()
            .map(|part| part.record.clone())
            .collect::<Vec<_>>(),
        [String::new(), chunk::record_id("", 1)],
        "one part per chunk record"
    );

    // The card kept its id: it is read in the chunk it moved to, and no longer where it was.
    let moved_to = left(&ada.instance.host().unwrap(), &root, &right_field);
    assert_eq!(moved_to.as_slice(), std::slice::from_ref(&card));
    assert!(
        !left(&ada.instance.host().unwrap(), &root, &left_field).contains(&card),
        "the card left the collection it was in"
    );
    let in_chunk = ada
        .workspace
        .chunk(1)
        .expect("the card's new chunk is held")
        .committed_heads();
    assert!(
        !ada.workspace.chunk(0).unwrap().committed_heads().is_empty(),
        "the record's own chunk holds the manifest's change"
    );

    // Undo takes the whole move back, its parts as one action of its own: the card is back
    // under the same id, where it was.
    let before_undo = ada.workspace.marks();
    let handed = wire.count();
    let resource = tilolang_tilo::publish::package_resource_id(BOARD_APP);
    assert!(
        ada.workspace
            .undo(&mut ada.instance, &resource, BOARD_APP)
            .unwrap()
    );
    catch_up(&mut ada);
    let back = left(&ada.instance.host().unwrap(), &root, &left_field);
    assert_eq!(back.len(), inserted.len());
    assert!(
        back.contains(&card),
        "undo put the card back where it was, under its id"
    );
    assert!(
        left(&ada.instance.host().unwrap(), &root, &right_field).is_empty(),
        "and out of where it moved to"
    );
    assert_ne!(
        ada.workspace.chunk(1).unwrap().committed_heads(),
        in_chunk,
        "the undo is written in the chunk the move was made in"
    );
    let undo_parts = wire.since(handed);
    assert_eq!(undo_parts.len(), 2, "{undo_parts:#?}");
    let undo_commit = undo_parts[0]
        .commit
        .clone()
        .expect("undoing the action takes one action across its parts");
    assert_eq!(undo_parts[1].commit.as_ref(), Some(&undo_commit));
    assert_eq!(undo_commit.parts.len(), 2);
    assert!(
        !moved(&before_undo, &ada.workspace.marks()).is_empty(),
        "the undo is a change of the record"
    );
}

#[test]
fn a_record_written_before_chunks_opens_as_one_chunk_and_is_not_rewritten() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = pre_chunks(&mut docs, &meta, &root, 20);
    let written = docs.chunks();
    let before = written
        .first()
        .expect("the record")
        .committed_document()
        .save();
    let heads = written.first().unwrap().committed_heads();

    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);
    ada.workspace.receive(&mut ada.instance);

    // It is one chunk, the record itself, with no manifest naming another.
    assert_eq!(ada.workspace.chunks().numbers().collect::<Vec<_>>(), [0]);
    assert!(ada.workspace.chunks().placement().is_empty());
    assert!(ada.workspace.chunk(1).is_none());

    // Nothing is copied and nothing rewritten: the record is the bytes it was, and nothing was
    // handed the workspace on a route of its own.
    assert_eq!(ada.workspace.doc().committed_document().save(), before);
    assert_eq!(ada.workspace.doc().committed_heads(), heads);
    assert_eq!(wire.count(), 0);

    // It opens as it always did, listing what it holds under the same ids.
    let field = field_id(&meta, "Board", "left");
    assert_eq!(left(&ada.instance.host().unwrap(), &root, &field), inserted);
}

/// Two replicas that each placed the first resource into a chunk before they met made a manifest
/// of their own: the record keeps one of the two, and what the other places is placed just the
/// same. The next write folds them into the one the record keeps, on both.
#[test]
fn a_manifest_two_replicas_wrote_places_every_chunk_on_both() {
    let (_host, mut docs, _meta, _root, _ids) = board();
    docs.doc(chunk::FIRST).commit();
    let manifests = |doc: &AutoCommit| {
        use automerge::ReadDoc as _;
        tilolang_tilo::containers::maps(doc, &automerge::ROOT, chunk::CHUNKS_KEY)
            .into_iter()
            .filter(|manifest| doc.keys(manifest).next().is_some())
            .count()
    };
    assert_eq!(
        manifests(docs.doc(chunk::FIRST)),
        0,
        "the record written before chunks has no manifest"
    );
    let mut ada = docs.doc(chunk::FIRST).fork();
    let mut grace = ada.fork();
    chunk::place(&mut ada, "card-ada", 1).expect("ada places its chunk");
    ada.commit();
    chunk::place(&mut grace, "card-grace", 2).expect("grace places its chunk");
    grace.commit();
    ada.merge_incoming(&mut grace)
        .expect("ada takes grace's manifest");
    assert_eq!(manifests(&ada), 2, "the record holds two manifests");
    let placement = chunk::Placement::read(&ada);
    assert_eq!(placement.chunk("card-ada"), 1);
    assert_eq!(placement.chunk("card-grace"), 2, "both are placed");

    // A write folds the two into the one the record keeps, and every placement stays.
    chunk::place(&mut ada, "card-later", 3).expect("ada places another");
    ada.commit();
    assert_eq!(manifests(&ada), 1, "the record keeps one manifest");
    let placement = chunk::Placement::read(&ada);
    for (id, chunk) in [("card-ada", 1), ("card-grace", 2), ("card-later", 3)] {
        assert_eq!(placement.chunk(id), chunk, "{id}");
    }
    grace
        .merge_incoming(&mut ada)
        .expect("grace takes ada's manifest");
    assert_eq!(manifests(&grace), 1, "one manifest on grace as well");
    assert_eq!(chunk::Placement::read(&grace), placement);
}

#[test]
fn a_replica_that_joins_gets_the_manifest_and_every_chunk() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 4200);
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);

    // Grace joins the workspace holding none of it: the record comes first as the whole
    // document, and its manifest is what names the chunks she asks for.
    let mut grace = window(
        "grace",
        &route,
        &platform,
        &wire,
        Chunks::of(AutoCommit::new()),
    );
    catch_up(&mut grace);
    for _ in 0..8 {
        let ada_moved = ada.workspace.receive(&mut ada.instance);
        let grace_moved = grace.workspace.receive(&mut grace.instance);
        catch_up(&mut grace);
        if !ada_moved && !grace_moved {
            break;
        }
    }
    let numbers: Vec<ChunkNo> = ada.workspace.chunks().numbers().collect();
    assert_eq!(numbers, [0, 1, 2]);
    assert_eq!(
        grace.workspace.chunks().numbers().collect::<Vec<_>>(),
        numbers,
        "grace holds every chunk the workspace has"
    );
    for number in &numbers {
        assert_eq!(
            grace.workspace.chunk(*number).unwrap().committed_heads(),
            ada.workspace.chunk(*number).unwrap().committed_heads(),
            "chunk {number} converged"
        );
    }
    let field = field_id(&meta, "Board", "left");
    assert_eq!(
        left(&grace.instance.host().unwrap(), &root, &field),
        inserted,
        "grace's window lists the workspace's cards"
    );

    // A change in one chunk reaches her as a change of that chunk alone.
    let before = grace.workspace.marks();
    let handed = wire.count();
    let card = format!("{root}/{}", inserted[4150]);
    let action = act(
        &ada.instance.host().unwrap(),
        &meta,
        ("Card", &card),
        "retitle",
        json!({ "text": "changed" }),
    );
    ada.workspace
        .apply_invoked(
            &mut ada.instance,
            &action,
            &ActionInvoker::actor("ada"),
            None,
        )
        .unwrap();
    let sent = wire.since(handed);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    assert_eq!(sent[0].route, ada.workspace.chunk_route(2));
    assert_eq!(sent[0].from, "ada");

    assert_eq!(grace.mine.borrow().len(), 1, "one change reached grace");
    assert!(grace.workspace.receive(&mut grace.instance));
    catch_up(&mut grace);
    assert_eq!(
        moved(&before, &grace.workspace.marks()),
        [2],
        "the change moved the chunk it was made in, and no other"
    );
    assert_eq!(
        grace.workspace.chunk(2).unwrap().committed_heads(),
        ada.workspace.chunk(2).unwrap().committed_heads(),
        "and the chunk holds what ada wrote"
    );
}

#[test]
fn a_run_that_fails_leaves_nothing_in_any_chunk() {
    let (_host, mut docs, meta, root, ids) = board();
    bulk(&mut docs, &meta, &root, 4200);
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);

    // The board's failing action adds a card to the collection's newest run, whose chunk is the
    // last, and then fails there.
    let before = ada.workspace.marks();
    let handed = wire.count();
    let host = ada.instance.host().unwrap();
    let boom = host
        .package(BOARD_APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "boom")
        .unwrap()
        .id
        .clone();
    let tally = host.root_of(BOARD_APP).unwrap();
    let first = host.next_ticket();
    assert!(
        host.runtime()
            .invoke_action(
                &Principal::new("ada"),
                (
                    boom.as_str(),
                    vec![tilolang_tilo::runtime::Value::resource(&tally)]
                ),
            )
            .is_err(),
        "the run fails"
    );
    let queued =
        tilolang_tilo::instance::queue_commits(&host, &ada.instance.outgoing(), first, None);
    assert!(queued.is_empty(), "nothing of the failed run is queued");
    assert!(ada.workspace.commit(&mut ada.instance).unwrap().is_empty());
    assert_eq!(
        ada.workspace.marks(),
        before,
        "no chunk holds anything of the run"
    );
    assert_eq!(
        wire.count(),
        handed,
        "nothing of it was handed the workspace"
    );
}

#[test]
fn a_part_the_workspace_refuses_takes_back_the_chunks_it_wrote_before_it() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 4200);
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let wire = Wire::default();
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let mut ada = window("ada", &route, &platform, &wire, docs.chunks());
    catch_up(&mut ada);

    // One action with an edit of a card in the first run's chunk and one of a card that is not
    // there, addressed to the last chunk: the first is written, the second refused.
    let title = field_id(&meta, "Card", "text");
    let card = format!("{root}/{}", inserted[3000]);
    let good = DynEdit::Edit {
        id: card,
        edit: Edit::new(
            vec![Segment::Field(title.clone())],
            EditOp::Replace {
                text: "written".to_owned(),
            },
        ),
    }
    .in_chunk(1);
    let bad = DynEdit::Edit {
        id: format!("{root}/nope"),
        edit: Edit::new(
            vec![Segment::Field(title)],
            EditOp::Replace {
                text: "never".to_owned(),
            },
        ),
    }
    .in_chunk(2);
    let action = DynAction {
        app: BOARD_APP.to_owned(),
        kind: card_type(&meta),
        action_id: "retitle".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits: vec![good, bad],
    };

    let before = ada.workspace.marks();
    let handed = wire.count();
    assert!(
        ada.workspace
            .apply_invoked(
                &mut ada.instance,
                &action,
                &ActionInvoker::actor("ada"),
                None
            )
            .is_err(),
        "the workspace refuses the part of the card that is not there"
    );
    assert_eq!(
        ada.workspace.marks(),
        before,
        "the chunk written before the refused one is taken back"
    );
    assert_eq!(
        wire.count(),
        handed,
        "nothing of it was handed the workspace"
    );

    // The card the first part wrote says what it said, and no chunk holds the refused text.
    let field = field_id(&meta, "Board", "left");
    assert_eq!(left(&ada.instance.host().unwrap(), &root, &field), inserted);
    let written = ada.workspace.chunk(1).unwrap().committed_document().save();
    assert!(
        !String::from_utf8_lossy(&written).contains("written"),
        "the chunk the refused part's action wrote first holds none of it"
    );
}

/// A directory that goes with the test: where a store keeps an index a later open finds.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tilolang-chunks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host opened over the record `docs` holds, its indexes kept in `dir`, with the board loaded
/// from the package lock `ids`. Every chunk not `sent` comes from its chunk loader, which records
/// the numbers it was asked for.
fn opened(docs: &mut Docs, dir: &Path, ids: &str, sent: bool) -> (Host, Rc<RefCell<Vec<ChunkNo>>>) {
    let record = docs.doc(chunk::FIRST).clone();
    let host = Host::open(
        Opening {
            record: Some(&record),
            chunks: None,
            index_dir: Some(dir.to_owned()),
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    let asked: Rc<RefCell<Vec<ChunkNo>>> = Rc::default();
    let chunks = docs.chunks();
    let recorder = Rc::clone(&asked);
    host.set_chunk_loader(Box::new(move |number| {
        recorder.borrow_mut().push(number);
        chunks.mirror(number).cloned()
    }));
    host.load_builtin(BOARD, ids).unwrap();
    if sent {
        docs.sync(&host);
    } else {
        host.sync_from(&record);
    }
    host.wait_index();
    assert!(host.index_ready(), "the board's index is ready");
    (host, asked)
}

/// The chunks `host` holds, in order.
fn holding(host: &Host) -> Vec<ChunkNo> {
    host.store()
        .held_chunks()
        .into_iter()
        .map(|(number, _, _)| number)
        .collect()
}

/// The card `id` of the board `root`, as a window reads it, and the chunk its map is in: the
/// cards are written in order, a chunk of [`chunk::ITEMS`] each from the record's own chunk on.
fn card_at(meta: &Meta, root: &str, board: &str, id: &str, at: usize) -> (ResourceRef, ChunkNo) {
    let per = usize::try_from(chunk::ITEMS).unwrap();
    let number = ChunkNo::try_from(at / per).unwrap();
    (
        ResourceRef::root(root, board).item(id, card_type(meta)),
        number,
    )
}

/// The cards the board `root` lists, in order, as the index answers them.
fn listed(host: &Host, meta: &Meta, root: &str) -> Vec<String> {
    let field = FieldSpec {
        id: field_id(meta, "Board", "left"),
        kind: FieldKind::Collection,
    };
    let at = ResourceRef::root(root, board_schema(meta).id);
    host.store()
        .listing(&at, &field)
        .unwrap()
        .ids()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect()
}

#[test]
fn a_window_opens_over_the_manifest_and_its_index_and_reads_only_the_chunks_it_shows() {
    let (_host, mut docs, meta, root, ids) = board();
    let inserted = bulk(&mut docs, &meta, &root, 10_000);
    let dir = TempDir::new("index-first");

    // A load that was sent every chunk, keeping its index where the next open finds it.
    let (whole, asked_whole) = opened(&mut docs, dir.path(), &ids, true);
    let chunks = holding(&whole).len();
    assert!(
        chunks > 4,
        "the cards are spread over their chunks: {chunks}"
    );
    let kept = whole.store().index_contents(BOARD_APP).unwrap();
    assert!(asked_whole.borrow().is_empty(), "nothing had to be loaded");
    whole.persist_index();
    drop(whole);

    // Opened over the manifest and that index: the lists and queries answer as a full load
    // does, and no chunk is read for them.
    let (lazy, asked) = opened(&mut docs, dir.path(), &ids, false);
    assert_eq!(holding(&lazy), [chunk::FIRST]);
    assert_eq!(
        lazy.store().stats().builds,
        0,
        "the index was kept, not built"
    );
    assert_eq!(
        listed(&lazy, &meta, &root),
        inserted,
        "every card, in order"
    );
    let found = lazy
        .store()
        .select(&card_type(&meta), &Pred::Const(true), None)
        .unwrap();
    assert_eq!(found.len(), 10_000, "the index counts them all");
    assert!(
        asked.borrow().is_empty(),
        "the index answered without a chunk: {:?}",
        asked.borrow()
    );
    assert_eq!(
        lazy.store().index_contents(BOARD_APP).unwrap(),
        kept,
        "what it holds is what a full load derived"
    );

    // A row a window shows reads its own chunk, and only that one.
    let board = board_schema(&meta).id;
    let (card, number) = card_at(&meta, &root, &board, &inserted[5_000], 5_000);
    assert!(lazy.store().record_at(&card).is_ok());
    assert_eq!(*asked.borrow(), [number], "one chunk, the row's own");
    let (card, number) = card_at(&meta, &root, &board, &inserted[9_999], 9_999);
    assert!(lazy.store().record_at(&card).is_ok());
    assert_eq!(
        *asked.borrow(),
        [2, number],
        "one more chunk, the row's own"
    );

    // A write addressed to a chunk the store was not sent loads it, and only it.
    let (edited, edited_chunk) = card_at(&meta, &root, &board, &inserted[7_000], 7_000);
    assert!(!asked.borrow().contains(&edited_chunk));
    let retitle = meta
        .actions
        .iter()
        .find(|action| action.name == "retitle")
        .unwrap()
        .id
        .clone();
    lazy.runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                retitle.as_str(),
                vec![Value::resource(&edited), Value::Text("changed".into())],
            ),
        )
        .unwrap();
    let after = asked.borrow().clone();
    assert_eq!(after.len(), 3, "the write loaded its own chunk: {after:?}");
    assert_eq!(after[2], edited_chunk);
    let text = field_id(&meta, "Card", "text");
    assert_eq!(
        lazy.store().record_at(&edited).unwrap().get(&text).cloned(),
        Some(DynValue::Text("changed".into()))
    );
}

/// A workspace of `cards` cards over its chunks, in a directory of its own: what its first open
/// reads, and what an open over the index that first one keeps reads.
fn opens_at(cards: usize) -> (usize, usize, Duration, Duration, Vec<ChunkNo>, Vec<ChunkNo>) {
    let (_host, mut docs, meta, root, ids) = board();
    bulk(&mut docs, &meta, &root, cards);
    let filled = usize::try_from(u64::try_from(cards).unwrap().div_ceil(chunk::ITEMS)).unwrap();
    assert_eq!(
        docs.chunks().numbers().count(),
        filled,
        "the first run is written in the record's own chunk, the rest above it"
    );
    let dir = TempDir::new(&format!("open-{cards}"));

    // With no index kept over its chunks, a window reads every one of them: the workspace is
    // derived whole, as a store sent them all would have it, and keeps that index for the next
    // open.
    let first = Instant::now();
    let (cold, asked) = opened(&mut docs, dir.path(), &ids, false);
    let cold_open = first.elapsed();
    let cold_held = holding(&cold);
    let loaded = asked.borrow().len();
    assert_eq!(cold_held.len(), filled, "every chunk the manifest names");
    assert_eq!(loaded, filled - 1, "the record was opened with");
    cold.persist_index();
    drop(cold);

    // With one, it opens over the record and the index, reading a chunk as a row shows it.
    let warm = Instant::now();
    let (lazy, asked) = opened(&mut docs, dir.path(), &ids, false);
    let warm_open = warm.elapsed();
    let held = holding(&lazy);
    assert_eq!(
        held,
        [chunk::FIRST],
        "the window opened over the record alone"
    );
    assert!(asked.borrow().is_empty(), "no chunk was read");
    assert_eq!(lazy.store().stats().attaches, 1, "the record, once");
    assert_eq!(
        lazy.store().stats().builds,
        0,
        "the index was kept, not built"
    );
    assert!(
        lazy.store().record(&root).is_ok(),
        "the board reads from the index"
    );
    // A row far down the board reads its own chunk, and only that one.
    let board = board_schema(&meta).id;
    if cards > usize::try_from(chunk::ITEMS).unwrap() {
        let (card, number) = card_at(&meta, &root, &board, &ids_of(cards, cards - 1), cards - 1);
        assert!(lazy.store().record_at(&card).is_ok());
        assert_eq!(*asked.borrow(), [number], "the row's chunk alone");
    }
    (filled, loaded, cold_open, warm_open, cold_held, held)
}

/// The id `bulk` gives the card at `at`.
fn ids_of(_cards: usize, at: usize) -> String {
    format!("c{at}")
}

#[test]
fn a_workspace_opens_with_the_same_chunks_at_a_thousand_and_ten_thousand_cards() {
    for cards in [1_000_usize, 10_000] {
        let (filled, loaded, cold, warm, _, held) = opens_at(cards);
        println!(
            "{cards} cards over {filled} chunks: first open read {loaded} chunks in {cold:?}, \
             opened over the index in {warm:?} holding {held:?}"
        );
        // What a window shows is the record and the chunks its rows are in, which it reads as
        // they scroll: an open that read every chunk took minutes at ten thousand.
        assert!(
            warm < Duration::from_secs(10),
            "{cards} cards opened in {warm:?}"
        );
    }
}

/// The record a replica opens from the copies it keeps on disk, on the route `route`.
fn opened_from_disk(
    route: &RouteSpec,
    documents: &Rc<dyn durable_store::DurableStore>,
    me: &str,
) -> InstanceRecord {
    let mut manager = CollaborationManager::with_peer_transport(Routed {
        me: me.to_owned(),
        wire: Wire::default(),
        mine: Rc::default(),
        primary: None,
        answer: None,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.record.clone(),
        record_kind: None,
        handle: Some(me.to_owned()),
    }));
    let _ = manager.poll();
    InstanceRecord::open(manager, documents, me).unwrap()
}

#[test]
fn a_card_written_into_a_chunk_is_there_when_the_record_opens_again_from_disk() {
    let (_host, _docs, meta, root, ids) = board();
    let field = field_id(&meta, "Board", "left");
    let schema = board_schema(&meta);
    let route = RouteSpec::app(BOARD_APP, uuid::Uuid::new_v4());
    let profile = tempfile::tempdir().unwrap();
    let documents: Rc<dyn durable_store::DurableStore> =
        Rc::new(app_state::FsStore::new(profile.path()));
    let mut record = opened_from_disk(&route, &documents, "ada");

    // The board's root in the record's own chunk, then one card into chunk 2, as the door writes
    // them: nothing of this reaches another replica.
    let board_ty = meta
        .types
        .iter()
        .find(|declared| declared.name == "Board")
        .unwrap()
        .id
        .clone();
    let root_action = DynAction {
        app: BOARD_APP.to_owned(),
        kind: board_ty.clone(),
        action_id: "create".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits: vec![DynEdit::Create {
            id: root.clone(),
            record: DynRecord::empty(lookup(&board_ty).unwrap())
                .unwrap()
                .to_json(),
        }],
    };
    record
        .apply_change(&root_action, &ActionInvoker::actor("ada"), None)
        .unwrap();
    let card_ty = card_type(&meta);
    let card = "c0".to_owned();
    let put = DynAction {
        app: BOARD_APP.to_owned(),
        kind: card_ty.clone(),
        action_id: "add".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits: vec![
            DynEdit::RunChunk {
                holder: root.clone(),
                field: field.clone(),
                chunk: 2,
                last: "0000000000".to_owned(),
                len: 1,
            },
            DynEdit::Insert {
                id: root.clone(),
                item: card.clone(),
                record: DynRecord::empty(lookup(&card_ty).unwrap())
                    .unwrap()
                    .to_json(),
                at: Placement::At {
                    place: Place {
                        parent: None,
                        field: field.clone(),
                        key: "0000000000".to_owned(),
                    },
                    was: None,
                }
                .encode(),
            }
            .in_chunk(2),
        ],
    };
    record
        .apply_change(&put, &ActionInvoker::actor("ada"), None)
        .unwrap();
    assert!(
        record.chunk(2).is_some(),
        "the record holds the chunk it wrote"
    );
    record.flush();
    assert_eq!(
        record.save_status(),
        collaboration::record::RecordSaveStatus::Saved
    );
    drop(record);

    // Opened again from the same copies, with no change from anyone: the chunk comes with the
    // record, and the card is where it was written.
    let reopened = opened_from_disk(&route, &documents, "ada");
    assert!(
        reopened.chunk(2).is_some(),
        "the chunk's own copy was opened with the record"
    );
    assert_eq!(held_in(reopened.chunks(), 2, &root, &schema), 1);
    assert_eq!(
        Items::across(reopened.chunks(), BOARD_APP, &root, &schema)
            .resolve(&schema)
            .items(None, &field),
        vec![card.clone()]
    );

    // And a window over it reads the card through the record's change.
    let platform = Rc::new(Board {
        ids: RefCell::new(ids),
    });
    let (app_end, _patches) = pair();
    let mut instance = Instance::new(
        Config {
            app: BOARD_APP.to_owned(),
            me: "ada".to_owned(),
            mode: Mode::Live,
            device: None,
            index_dir: None,
            record_loader: None,
            root: RootView::default(),
            sink: Some(Box::new(MemorySink::new())),
        },
        Rc::clone(&platform) as Rc<dyn Platform>,
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync_chunks(reopened.chunks());
    instance.update();
    assert_eq!(left(&instance.host().unwrap(), &root, &field), [card]);
}

/// The same at a hundred thousand cards: minutes under a debug build, seconds under a release
/// one.
#[test]
#[ignore = "a hundred thousand cards take minutes to derive under a debug build"]
fn a_hundred_thousand_cards_open_over_their_index() {
    let (filled, loaded, cold, warm, _, held) = opens_at(100_000);
    println!(
        "100000 cards over {filled} chunks: first open read {loaded} chunks in {cold:?}, \
         opened over the index in {warm:?} holding {held:?}"
    );
    assert!(warm < Duration::from_secs(10), "opened in {warm:?}");
}
