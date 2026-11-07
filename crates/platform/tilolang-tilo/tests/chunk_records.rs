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
use std::rc::Rc;
use std::sync::Arc;

use automerge::AutoCommit;
use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use serde_json::json;
use tilo_actions::ActionInvoker;
use tilo_render_protocol::patch::local::{End, pair};
use tilo_resource::{Edit, EditOp, Model as _, Segment};
use tilolang_runtime::meta::Meta;
use tilolang_runtime::place::{Place, Placement};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::chunk::{self, ChunkNo, Chunks};
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::record::InstanceRecord;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::items::Items;
use tilolang_tilo::record::DynRecord;
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

/// The window reads what its record holds: the record's chunk, then the chunks above it.
fn catch_up(window: &mut Window) {
    window.instance.sync_chunks(window.workspace.chunks());
    window.instance.update();
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
