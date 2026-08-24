//! The agent against a room it was told nothing about.
//!
//! Everything here runs over **the broker the compositor actually binds**
//! (`shell::shell::collab_broker`), with both of its transports up: the apps
//! connect on the capnp socket the launcher hands a sandboxed app, and the
//! agent connects on the JSON socket the compositor binds beside it for
//! exactly this — a participant that is not a packaged app. The two dialects
//! share one fan-out, and every frame in these tests crosses it.
//!
//! That choice is the point rather than a detail. The toolkit also ships an
//! in-process relay (`ui::collaboration::broker`), and it is not the product:
//! it fans every frame to every peer, applies no `(app, room, record)`
//! routing, and enforces nothing. A test on it would prove that the agent can
//! talk to a loopback, not that it can reach an app in a session — and it
//! would hide precisely the defects that only appear when a real app is on
//! capnp and something else is on JSON.
//!
//! Nothing here hands the agent an event directly. It is given a socket, and
//! what it learns about the room it learns by asking the room.

use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use qos_agent::surface;
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
use uuid::Uuid;

const WORD_PROCESSOR: &str = "dev.qos.word-processor";
const FILE_EXPLORER: &str = "dev.qos.file-explorer";
const AGENT: &str = "Scribe";

/// An app connects through the launcher's service fd, which is named by a
/// process-wide environment variable, so building two peers at once would
/// race on it. Held only while a peer is being constructed — by the time
/// `CollaborationManager::new` returns, it has dialled and announced.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// The room
// ---------------------------------------------------------------------------

/// The compositor's own broker, on both transports.
struct Room {
    _dir: tempfile::TempDir,
    json: PathBuf,
    capnp: PathBuf,
    id: Uuid,
    broker: shell::shell::collab_broker::BrokerHandle,
}

fn room() -> Room {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("collab-json.sock");
    let capnp = dir.path().join("collab.sock");
    let broker =
        shell::shell::collab_broker::spawn_full(json.clone(), Some(capnp.clone()), None)
            .expect("the broker binds both sockets");
    for path in [&json, &capnp] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(path).is_err() {
            assert!(Instant::now() < deadline, "broker never bound {path:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Room {
        _dir: dir,
        json,
        capnp,
        id: Uuid::new_v4(),
        broker,
    }
}

/// One app in the room, on the transport a launched app is given.
struct Peer {
    collab: CollaborationManager,
    doc: AutoCommit,
    /// Whether a document that is not this app's is refused rather than
    /// merged. The room carries several and the broker hands a
    /// cluster-scoped frame to everyone.
    mine: fn(&AutoCommit) -> bool,
}

impl Peer {
    fn new(room: &Room, app_id: &str, handle: &str, doc: AutoCommit, mine: fn(&AutoCommit) -> bool) -> Self {
        let collab = {
            let _guard = env_lock();
            let fd = UnixStream::connect(&room.capnp)
                .expect("connect the capnp socket")
                .into_raw_fd();
            // SAFETY: serialised by `env_lock`, and `CollaborationManager::new`
            // reads all three before it returns.
            unsafe {
                std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
                std::env::set_var("QOS_PACKAGE_ID", app_id);
                std::env::set_var("QOS_WORKSPACE_ID", room.id.to_string());
                std::env::set_var("QOS_PRESENCE_HANDLE", handle);
                // The legacy path wins when this is set, and it must not:
                // an app in a session takes the capnp descriptor.
                std::env::remove_var("QUICK_COLLAB_SOCK");
            }
            let m = CollaborationManager::new();
            assert!(m.is_connected(), "{app_id} must reach the broker over capnp");
            m
        };
        Self { collab, doc, mine }
    }

    /// Behave the way the app does: answer an arrival with the whole
    /// document, and take every change that belongs to it.
    fn pump(&mut self) {
        for event in self.collab.poll() {
            match event {
                CollabEvent::RemotePresenceUpdate {
                    payload: PresencePayload::Arrived,
                    ..
                } => {
                    let data = self.doc.save();
                    self.collab.send(CollabRequest::SyncCRDT { data });
                }
                CollabEvent::CRDTIncremental { data } => {
                    // A delta belonging to some *other* document turns up
                    // here, because a participant that announced no app id is
                    // cluster-scoped and its frames reach every subscriber.
                    // Taking one is not harmless: two unrelated documents
                    // routinely share an actor id — an app sets one actor for
                    // every document it opens — so the foreign change lands
                    // under an `(actor, seq)` this document then considers
                    // taken, and the *next* delta, the real one, is rejected
                    // as a duplicate and lost without a word.
                    //
                    // So the question the agent asks of a delta is asked here
                    // too: does it stand on history this replica already
                    // holds? A delta for another document does not.
                    let mut probe = AutoCommit::new();
                    if probe.load_incremental(&data).is_err() {
                        continue;
                    }
                    let ours = probe
                        .get_missing_deps(&[])
                        .iter()
                        .all(|h| self.doc.get_change_by_hash(h).is_some());
                    if !ours {
                        continue;
                    }
                    let mut next = self.doc.clone();
                    if next.load_incremental(&data).is_ok() {
                        self.doc = next;
                    }
                }
                CollabEvent::CRDTUpdate { data } => {
                    let Ok(mut incoming) = AutoCommit::load(&data) else {
                        continue;
                    };
                    if (self.mine)(&incoming) {
                        let _ = self.doc.merge(&mut incoming);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The capability an inviter issues an agent for one app.
fn granting(apps: &[&str]) -> PermissionContext {
    let subject = Subject::agent(AGENT);
    PermissionContext::from_capabilities(
        Actor::agent(AGENT),
        apps.iter()
            .map(|a| {
                CapOverride::grant(subject.clone(), Scope::App((*a).to_string()), Verb::Write)
                    .with_id(format!("test-{a}"))
            })
            .collect(),
    )
}

/// The agent, joined to the room rather than to one app: it announces no app
/// id, so the broker's fan-out stays cluster-scoped and every app's state
/// reaches it. That is what makes the surface the room's.
fn agent_cfg(room: &Room, permissions: PermissionContext, directory: Option<PathBuf>) -> Config {
    Config {
        socket: Some(room.json.clone()),
        app_id: String::new(),
        cluster: room.id,
        record: None,
        name: AGENT.to_string(),
        pace: Duration::from_millis(5),
        once: true,
        // Every test here proves the surface and the gate, not the choice, so
        // the decision is pinned to the deterministic floor: no network, no
        // key, and the same answer every run.
        model: false,
        model_timeout: Duration::from_secs(1),
        // Every page here is seeded whole before the agent connects, so
        // no line is mid-keystroke and there is nothing to wait out.
        // The test that *is* about a line being typed raises it itself.
        settle: Duration::ZERO,
        await_document: Duration::ZERO,
        directory,
        permissions,
    }
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// A page with a run of bullets above the instruction — the shape an
/// editorial change acts on.
fn page_with_bullets(instruction: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let h = dm.insert_block(None, "heading");
        dm.set_attr(&h, "level", 1i64);
        dm.splice_text(&h, 0, 0, "Launch plan");
        let mut after = h;
        for line in ["Venue - The Hall", "Date - 3 Sep", "Budget - 1200"] {
            let b = dm.insert_block(Some(&after), "list_item");
            dm.set_attr(&b, "ordered", false);
            dm.splice_text(&b, 0, 0, line);
            after = b;
        }
        let q = dm.insert_block(Some(&after), "paragraph");
        dm.splice_text(&q, 0, 0, instruction);
    }
    doc.commit();
    doc
}

fn page(heading: &str, instruction: Option<&str>) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let h = dm.insert_block(None, "heading");
        dm.set_attr(&h, "level", 1i64);
        dm.splice_text(&h, 0, 0, heading);
        let p = dm.insert_block(Some(&h), "paragraph");
        dm.splice_text(&p, 0, 0, "Some prose under it.");
        if let Some(text) = instruction {
            let q = dm.insert_block(Some(&p), "paragraph");
            dm.splice_text(&q, 0, 0, text);
        }
    }
    doc.commit();
    doc
}

/// The explorer's document: the shared inline-rename buffer, which is the
/// only thing it ever writes.
fn explorer_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("xaver".as_bytes()));
    {
        use automerge::transaction::Transactable;
        doc.put_object(&automerge::ROOT, "files.rename_query", automerge::ObjType::Text)
            .expect("seed the rename buffer");
    }
    doc.commit();
    doc
}

fn blocks(doc: &AutoCommit) -> Vec<qdoc::BlockView> {
    let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
    qdoc::model::materialize(doc, &c).blocks
}

fn attr_of(doc: &AutoCommit, kind: &str, key: &str) -> Option<String> {
    blocks(doc)
        .iter()
        .find(|b| b.kind == kind)
        .and_then(|b| match b.attrs.get(key) {
            Some(qdoc::AttrVal::Str(s)) => Some(s.clone()),
            _ => None,
        })
}

/// Nothing the agent writes into a document may name an address.
///
/// The separator is the tell, and asserting on it catches every spelling of
/// the mistake rather than the one example somebody thought of. An address is
/// how a dispatch is addressed; a person reading the page learns nothing from
/// it and concludes the machine is broken.
fn no_addresses_written(doc: &AutoCommit) {
    for b in blocks(doc) {
        assert!(
            !b.text.contains('#'),
            "an action address reached the document: {:?}",
            b.text
        );
        assert!(
            !b.text.contains("word-processor.") && !b.text.contains("files.entry:"),
            "a resource id reached the document: {:?}",
            b.text
        );
    }
}

fn rename_buffer(doc: &AutoCommit) -> String {
    use automerge::ReadDoc;
    match doc.get(&automerge::ROOT, "files.rename_query") {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Text), id))) => {
            doc.text(&id).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Run the agent and the room's apps together until `f` is satisfied or the
/// deadline passes. The agent runs on its own thread, as it does in a
/// session; the peers are pumped here.
fn run_room(
    cfg: Config,
    peers: &mut [&mut Peer],
    within: Duration,
    mut f: impl FnMut(&mut [&mut Peer]) -> bool,
) -> bool {
    let deadline = Instant::now() + within;
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("the agent connects");
        let acted = s
            .run_until(Instant::now() + within)
            .expect("the agent session");
        (acted, s.last_route().cloned())
    });
    let mut hit = false;
    while Instant::now() < deadline {
        for p in peers.iter_mut() {
            p.pump();
        }
        if f(peers) {
            hit = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the agent finish its turn, then drain whatever it sent last.
    let _ = agent.join();
    for _ in 0..20 {
        for p in peers.iter_mut() {
            p.pump();
        }
        if !hit && f(peers) {
            hit = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    hit
}

/// Stand the agent up on the room and let it absorb, without running an
/// instruction — for the tests that ask what it can see rather than what it
/// does.
///
/// Done when the agent is **holding** something. Not when its survey is
/// non-empty, which is what this waited on until the surface grew the
/// operations that open an application: those are offered precisely because
/// nothing has arrived, so a survey carrying only them is an agent that has
/// received nothing, and reading it as arrival returns an empty room dressed
/// up as a full one.
fn joined(cfg: Config, peers: &mut [&mut Peer], within: Duration) -> Session {
    let mut session = Session::connect(cfg).expect("the agent connects");
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        for p in peers.iter_mut() {
            p.pump();
        }
        session
            .run_until(Instant::now() + Duration::from_millis(60))
            .expect("the agent session");
        if !session.apps().is_empty() {
            break;
        }
    }
    session
}

// ---------------------------------------------------------------------------
// 1. Discovery
// ---------------------------------------------------------------------------

/// The agent enumerates an app nobody told it about, and what it finds is
/// exactly what that app publishes — not a subset, not a superset, and not a
/// list kept in the agent's own source.
///
/// The expectation is computed by asking the *app* the same question against
/// the peer's own replica, so this compares the agent's answer with the
/// app's, never with a literal written here. If the word processor grows or
/// withdraws an operation tomorrow, both sides move together and the test
/// still means something.
#[test]
fn the_agent_enumerates_an_app_it_was_never_told_about() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Launch plan", None),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let session = joined(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut [&mut peer],
        Duration::from_secs(20),
    );
    // Two different claims live in one survey, so they are separated before
    // either is asserted. What an app publishes is the app's; what is offered
    // as an application to *open* is published by nobody — it is the room's
    // own absences, and comparing it against a resource tree would be
    // comparing two different things.
    let (openable, found): (Vec<String>, Vec<String>) = session
        .offers(&session.view(None, 0))
        .iter()
        .map(|o| o.id())
        .partition(|id| {
            surface::decode_address(id).is_some_and(|a| surface::opens(&a).is_some())
        });

    // What the app itself publishes for the same document, through its own
    // provider — the agent's `View` supplies only the anchor, exactly as it
    // does in the run above.
    let anchor = blocks(&peer.doc)[0].id.clone();
    let published = word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor.clone(),
            caret_offset: 0,
            target_blocks: vec![anchor],
        },
        &peer.doc,
    );
    let expected: Vec<String> = qos_actions::door::survey(&published)
        .iter()
        .map(|d| surface::encode_address(&d.address))
        .collect();

    assert!(!expected.is_empty(), "the word processor exports nothing");
    assert_eq!(
        found, expected,
        "the agent must enumerate exactly what the app publishes"
    );
    // And the set is genuinely read off the document rather than fixed: the
    // heading in this page is an addressable section, and its address carries
    // the block id the document minted.
    assert!(
        found.iter().any(|id| id.starts_with("word-processor.section:")),
        "the document's heading must be addressable as a section: {found:?}"
    );
    // And the app whose operations these are is not also offered as something
    // to open. The agent holds it; opening it again would put a second window
    // over the first, so the offer has to be gone the moment its state lands.
    assert!(
        openable.is_empty(),
        "an app the agent is holding must not be offered as one to open, and \
         an app it may not open must not be offered at all: {openable:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. The word processor, through a discovered address
// ---------------------------------------------------------------------------

/// The regression guard on the migration: the agent still changes the word
/// processor's document, and now does it by invoking an operation it
/// discovered rather than one named in its own source.
///
/// Asserted on the **peer's** replica — the copy a person would be looking
/// at — so the change has crossed the broker.
#[test]
fn a_discovered_operation_changes_the_word_processors_document() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Launch plan", Some("Scribe: centre the Launch plan")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let done = run_room(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut [&mut peer],
        Duration::from_secs(25),
        |peers| attr_of(&peers[0].doc, "heading", "align").as_deref() == Some("center"),
    );
    assert!(
        done,
        "the heading was never centred on the peer's replica; it reads {:?}",
        attr_of(&peer.doc, "heading", "align")
    );
}

// ---------------------------------------------------------------------------
// 3. The file explorer, through a discovered address
// ---------------------------------------------------------------------------

/// The same path reaches an app whose subject is not a document at all: a
/// tree of filesystem paths, addressed by real path, exporting an operation
/// that collects text.
///
/// The instruction lives on the page, because that is where somebody writes
/// one; the operation it names belongs to the folder open beside it. Nothing
/// in the agent knows that a file is different from a paragraph.
#[test]
fn a_discovered_operation_reaches_the_file_explorer_too() {
    let files = tempfile::tempdir().expect("tempdir");
    std::fs::write(files.path().join("plan.md"), b"x").expect("seed a file");
    std::fs::write(files.path().join("notes.txt"), b"y").expect("seed a file");

    let room = room();
    let mut page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        // A heading that shares no word with the instruction, so the one
        // operation whose own labels answer it is the explorer's.
        page("Notes", Some(r#"Scribe: rename plan.md to "launch.md""#)),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut files_peer = Peer::new(
        &room,
        FILE_EXPLORER,
        "xaver",
        explorer_doc(),
        file_explorer::actions::is_explorer_document,
    );

    let done = run_room(
        agent_cfg(
            &room,
            granting(&["word-processor", "files"]),
            Some(files.path().to_path_buf()),
        ),
        &mut [&mut page_peer, &mut files_peer],
        Duration::from_secs(25),
        |peers| rename_buffer(&peers[1].doc) == "launch.md",
    );
    assert!(
        done,
        "the explorer's shared rename buffer reads {:?}",
        rename_buffer(&files_peer.doc)
    );
}

// ---------------------------------------------------------------------------
// 4. Attribution
// ---------------------------------------------------------------------------

/// The change the agent made is attributed to the agent.
///
/// Read off the peer's replica the way the history strip reads it: the
/// Automerge actor the change committed under. An edit made through the door
/// is named in the room's history exactly like an edit somebody typed.
#[test]
fn a_discovered_operation_commits_under_the_agents_own_actor() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Launch plan", Some("Scribe: centre the Launch plan")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let done = run_room(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut [&mut peer],
        Duration::from_secs(25),
        |peers| attr_of(&peers[0].doc, "heading", "align").as_deref() == Some("center"),
    );
    assert!(done, "the operation never landed, so there is no author to check");

    let agent_actor = hex::encode(AGENT.as_bytes());
    let authors: Vec<String> = qdoc::history::versions(&mut peer.doc)
        .into_iter()
        .map(|v| v.actor_hex)
        .collect();
    assert!(
        authors.contains(&agent_actor),
        "no change on the peer's replica is authored by the agent; authors were {authors:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Capability
// ---------------------------------------------------------------------------

/// Without the capability the operation is not merely refused — it is
/// absent, and nothing it would have changed changes.
///
/// Three claims, because a gate that only holds at one of them is advisory:
/// the survey offers nothing, the gate refuses the very address the app does
/// export, and the document the operation targets is untouched after a full
/// run.
#[test]
fn without_the_capability_the_operation_is_absent_and_never_performed() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Launch plan", Some("Scribe: centre the Launch plan")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    // The address the app really does export, taken from the app itself so
    // this is not a made-up id that would be refused anyway.
    let anchor = blocks(&peer.doc)[0].id.clone();
    let published = word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor.clone(),
            caret_offset: 0,
            target_blocks: vec![anchor],
        },
        &peer.doc,
    );
    let real = surface::encode_address(
        &qos_actions::door::survey(&published)
            .first()
            .expect("the app exports something")
            .address,
    );

    let ungranted = joined(
        agent_cfg(&room, granting(&[]), None),
        &mut [&mut peer],
        Duration::from_secs(15),
    );
    let offers = ungranted.offers(&ungranted.view(None, 0));
    assert!(
        offers.is_empty(),
        "an operation the agent may not perform must not be offered at all: {:?}",
        offers.iter().map(|o| o.id()).collect::<Vec<_>>()
    );
    assert_eq!(
        qos_agent::plan::Instruction::from_answer(&real, "centre it", None, &offers),
        Err(qos_agent::plan::Refused::NotExported(real.clone())),
        "the gate must refuse an address the agent holds no capability for"
    );
    drop(ungranted);

    let before = attr_of(&peer.doc, "heading", "align");
    run_room(
        agent_cfg(&room, granting(&[]), None),
        &mut [&mut peer],
        Duration::from_secs(15),
        |peers| attr_of(&peers[0].doc, "heading", "align").is_some(),
    );
    assert_eq!(
        attr_of(&peer.doc, "heading", "align"),
        before,
        "nothing the ungranted operation would have changed may change"
    );
}

// ---------------------------------------------------------------------------
// 6. Nothing fits
// ---------------------------------------------------------------------------

/// An instruction no exported operation answers is reported as such, and the
/// nearest available operation is not quietly performed instead.
#[test]
fn an_instruction_nothing_answers_is_reported_not_approximated() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Launch plan", Some("Scribe: juggle three plates")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let answered = run_room(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut [&mut peer],
        Duration::from_secs(25),
        |peers| {
            blocks(&peers[0].doc)
                .iter()
                .any(|b| b.text.starts_with("Scribe: I can't"))
        },
    );
    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(answered, "the agent never said it could not; the page reads {text:?}");
    assert!(
        text.iter().any(|t| t.contains("juggle")),
        "the refusal must name what was asked for: {text:?}"
    );
    no_addresses_written(&peer.doc);
    // The nearest thing it could have done, and did not.
    assert_eq!(
        attr_of(&peer.doc, "heading", "align"),
        None,
        "an instruction nothing answers must not be turned into the nearest operation"
    );
    assert!(
        !blocks(&peer.doc).iter().any(|b| b.kind == "cell"),
        "nor into a table: {text:?}"
    );
}

// ---------------------------------------------------------------------------
// 7. The deterministic floor
// ---------------------------------------------------------------------------

/// With no model reachable, an instruction that fits several operations
/// equally well is refused with the alternatives named — never resolved by
/// picking one — and the run says which path decided it.
///
/// This is the property the floor exists for. A vocabulary that guessed
/// would report success for work nobody asked for, and a run on the floor
/// would be indistinguishable from a live one afterwards.
#[test]
fn with_no_model_the_floor_refuses_an_ambiguous_instruction() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        // "align" is the word four of the document's operations carry, and
        // nothing in the instruction tells them apart.
        page("Launch plan", Some("Scribe: align the document")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let cfg = agent_cfg(&room, granting(&["word-processor"]), None);
    assert!(!cfg.model, "this test pins the run to the floor");

    let answered = run_room(
        cfg,
        &mut [&mut peer],
        Duration::from_secs(25),
        |peers| {
            blocks(&peers[0].doc)
                .iter()
                .any(|b| b.text.starts_with("Scribe: I can't"))
        },
    );
    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(answered, "the floor neither acted nor said why; the page reads {text:?}");
    assert!(
        text.iter().any(|t| t.contains("Which one?")),
        "an ambiguous instruction must be reported as ambiguous: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.to_lowercase().contains("align left")),
        "and it must name the alternatives in the app's own words: {text:?}"
    );
    no_addresses_written(&peer.doc);
    assert_eq!(
        attr_of(&peer.doc, "heading", "align"),
        None,
        "the floor must not resolve ambiguity by picking one"
    );

    // And the floor is reachable on its own terms: the same words, made
    // unambiguous, do resolve to exactly one address.
    let anchor = blocks(&peer.doc)[0].id.clone();
    let published = word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor.clone(),
            caret_offset: 0,
            target_blocks: vec![anchor],
        },
        &peer.doc,
    );
    let offers: Vec<qos_agent::Offer> = qos_actions::door::survey(&published)
        .into_iter()
        .map(|descriptor| qos_agent::Offer {
            app: "word-processor",
            descriptor,
        })
        .collect();
    let (chosen, argument) =
        surface::from_verbs("centre the Launch plan", &offers).expect("one clear winner");
    assert_eq!(argument, None);
    assert!(
        chosen.descriptor.action_label.to_lowercase().contains("centre"),
        "the floor chose {:?}",
        chosen.descriptor.action_label
    );
    match surface::from_verbs("align the document", &offers) {
        Err(surface::NoMatch::Ambiguous(ids)) => assert!(
            ids.len() > 1,
            "ambiguity means more than one candidate, got {ids:?}"
        ),
        other => panic!("the ambiguous case must stay ambiguous, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 8. The editorial recipes survive discovery
// ---------------------------------------------------------------------------

/// An instruction that names a composition still produces the composition.
///
/// "Make this a run sheet" is several of the word processor's operations over
/// blocks chosen by reading the page, and no single exported operation does
/// it. Discovery replaced the written-down catalogue of what the agent can be
/// asked for; it must not have quietly dropped the things a survey cannot
/// describe, which would turn the one instruction anybody actually types into
/// a refusal.
#[test]
fn an_editorial_change_is_still_answered_on_the_document_path() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page_with_bullets("Scribe: make this a run sheet"),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let done = run_room(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut [&mut peer],
        Duration::from_secs(25),
        |peers| blocks(&peers[0].doc).iter().any(|b| b.kind == "cell"),
    );
    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(
        done,
        "the bullets were never made into a table; the page reads {text:?}"
    );
    // The bullets became the table's contents rather than being left beside
    // it, and the instruction is gone because it was carried out.
    assert!(
        text.iter().any(|t| t.contains("The Hall")),
        "the bullets' contents must reach the table: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.starts_with("Scribe: I can't")),
        "an instruction the agent can carry out must not be refused: {text:?}"
    );
    no_addresses_written(&peer.doc);
}

// ---------------------------------------------------------------------------
// 9. A line that is still being typed
// ---------------------------------------------------------------------------

/// Type one character into `block` at the end and hand back the change to
/// broadcast — the shape one keystroke has in the word processor.
fn type_one(doc: &mut AutoCommit, block: &str, ch: char) -> Vec<u8> {
    let before = doc.get_heads();
    {
        let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
        let at = blocks(doc)
            .into_iter()
            .find(|b| b.id == block)
            .map(|b| b.text.chars().count())
            .unwrap_or(0);
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);
        let mut dm = qdoc::DocMut { doc, c: &c, ids: &mut ids };
        dm.splice_text(block, at, 0, &ch.to_string());
    }
    doc.commit();
    doc.save_after(&before)
}

/// An instruction is answered once, when it has been written — not while it
/// is being typed.
///
/// Every page in this file so far was seeded whole before the agent
/// connected, which is the one arrangement in which this cannot go wrong. A
/// person at a keyboard is the other arrangement, and it is the one that gets
/// filmed: the agent is already in the room, and it is handed a delta per
/// keystroke.
///
/// `Claude:` is a complete address with nothing asked after it, and it exists
/// for as long as it takes to reach for the next key. Reading it then produced
/// the frame this test exists to make impossible — a refusal quoting an empty
/// string, written over the top of a sentence still arriving — and, worse,
/// consumed the line, so the request the person finished typing was never
/// looked at again. Both halves are asserted, because the second is silent:
/// a run with the empty refusal deleted afterwards would still never act.
#[test]
fn a_line_still_being_typed_is_not_yet_a_request() {
    let room = room();
    // The instruction paragraph starts empty and is typed into below, the way
    // one is in a session.
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page_with_bullets(""),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let mut cfg = agent_cfg(&room, granting(&["word-processor"]), None);
    // Long enough to outlast the gaps between keystrokes below, short enough
    // that the test does not sit on it. The production default is longer for
    // the same reason a person's pauses are.
    cfg.settle = Duration::from_millis(400);

    let within = Duration::from_secs(30);
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("the agent connects");
        s.run_until(Instant::now() + within).expect("the agent session")
    });

    // Let it join and take the page *before* anything is typed. Without this
    // the agent absorbs a snapshot that already has the finished line in it,
    // and the race this is about never gets a chance to happen.
    let joined_by = Instant::now() + Duration::from_secs(3);
    while Instant::now() < joined_by {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let instruction = blocks(&peer.doc)
        .into_iter()
        .filter(|b| b.kind == "paragraph")
        .next_back()
        .expect("the empty instruction paragraph")
        .id;

    // Typed, with a pause after the colon. That pause is the defect's whole
    // window: it is where `Scribe:` sits addressed to the agent with nothing
    // asked, and a person reaching for the next key takes longer than this.
    for ch in "Scribe: make this a run sheet".chars() {
        let delta = type_one(&mut peer.doc, &instruction, ch);
        peer.collab.send(CollabRequest::SyncIncremental { data: delta });
        let pause = if ch == ':' { 250 } else { 20 };
        let until = Instant::now() + Duration::from_millis(pause);
        while Instant::now() < until {
            peer.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Now it may act, and the assertion is on the peer's own replica.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut made_a_table = false;
    while Instant::now() < deadline {
        peer.pump();
        if blocks(&peer.doc).iter().any(|b| b.kind == "cell") {
            made_a_table = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = agent.join();
    for _ in 0..25 {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    // The half-typed address was never answered. Asserted on its own rather
    // than folded into the refusal check below, because an empty quotation is
    // the signature of this fault specifically and says so when it fails.
    assert!(
        !text.iter().any(|t| t.contains("I can't \"\"")),
        "the agent answered a line that had only been addressed, not written: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.starts_with("Scribe: I can't")),
        "an instruction the agent can carry out must not be refused: {text:?}"
    );
    // And the request survived to be carried out. This is the half that would
    // otherwise fail silently: consuming the line marks it handled, so the
    // finished instruction is never read even though nothing on the page says
    // anything went wrong.
    assert!(
        made_a_table,
        "the finished instruction was never carried out; the page reads {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("The Hall")),
        "the bullets' contents must reach the table: {text:?}"
    );
    no_addresses_written(&peer.doc);
}

/// How the word processor keys a caret, so a peer here publishes one the way
/// the app does. Must match `block_presence_key` in its widget, and the
/// agent's own `block_key`, or the frame is presence about nothing.
fn caret_in(block: &str) -> ui::presence::PresenceKey {
    ui::presence::PresenceKey::Widget {
        path: vec![
            ui::presence::PathSegment::Field("wp_block".to_string()),
            ui::presence::PathSegment::Field(block.to_string()),
        ],
    }
}

/// Break the paragraph, the way Enter does: a new one after it, and the caret
/// moves into it. Hands back the change to broadcast.
fn press_enter(doc: &mut AutoCommit, after: &str) -> (Vec<u8>, String) {
    let before = doc.get_heads();
    let id = {
        let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);
        let mut dm = qdoc::DocMut { doc, c: &c, ids: &mut ids };
        dm.insert_block(Some(after), "paragraph")
    };
    doc.commit();
    (doc.save_after(&before), id)
}

/// A pause in the middle of a sentence does not make it a finished one.
///
/// The line stopping is the only thing the settle window can see, and a line
/// stops for two unrelated reasons: the writer finished it, or the writer's
/// next word has not arrived. On a machine running three builds the second
/// outlasts any window worth waiting — and what came of that is exact and on
/// record. A refusal replaced thirteen characters of the document, and
/// `Scribe: make ` is thirteen characters: the agent read the word `make`,
/// decided it could not do it, wrote `I can't "make" here` over the top, and
/// `this a run sheet` arrived afterwards and landed on the end of its reply.
///
/// So this types the way a person does — a delta per keystroke — and stalls
/// after `make ` for three times the settle window. Nothing about the text
/// distinguishes that stall from a finished line. What distinguishes it is
/// that the writer's caret is still sitting in the block, which is a fact
/// about the room rather than about the clock, and does not get less true
/// because the machine is busy.
///
/// The other half is the same claim from the other side: the sentence is
/// finished with Enter, the caret leaves, and the agent acts — without
/// waiting out a settle window it no longer has any reason to wait out.
#[test]
fn a_pause_in_the_middle_of_a_sentence_is_not_the_end_of_it() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page_with_bullets(""),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let mut cfg = agent_cfg(&room, granting(&["word-processor"]), None);
    // Deliberately short, and the stall below is deliberately longer. Making
    // this the *only* thing that decides is the fault being fixed, so a test
    // that merely set it above the stall would be asserting that the number
    // was picked well rather than that the agent knows who is writing.
    cfg.settle = Duration::from_millis(400);

    let within = Duration::from_secs(40);
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("the agent connects");
        s.run_until(Instant::now() + within).expect("the agent session")
    });

    // Join and take the page before anything is typed, so the line is one the
    // agent watches arrive rather than one it is handed whole.
    let joined_by = Instant::now() + Duration::from_secs(3);
    while Instant::now() < joined_by {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let instruction = blocks(&peer.doc)
        .into_iter()
        .filter(|b| b.kind == "paragraph")
        .next_back()
        .expect("the empty instruction paragraph")
        .id;

    // Type, publishing a caret after each keystroke — which is what the word
    // processor does, once per frame, off the same caret move.
    let typed = |peer: &mut Peer, text: &str, pause_ms: u64| {
        for ch in text.chars() {
            let delta = type_one(&mut peer.doc, &instruction, ch);
            peer.collab.send(CollabRequest::SyncIncremental { data: delta });
            let pos = blocks(&peer.doc)
                .iter()
                .find(|b| b.id == instruction)
                .map(|b| b.text.chars().count())
                .unwrap_or(0);
            peer.collab.send(CollabRequest::Presence {
                key: caret_in(&instruction),
                payload: PresencePayload::Caret { pos },
            });
            let until = Instant::now() + Duration::from_millis(pause_ms);
            while Instant::now() < until {
                peer.pump();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    typed(&mut peer, "Scribe: make ", 20);

    // The stall. Three times the settle window, with the room otherwise
    // healthy — the peer is still here, still pumping, its caret still in the
    // line. Only the next keystroke is late.
    let stalled_until = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < stalled_until {
        peer.pump();
        std::thread::sleep(Duration::from_millis(10));
    }

    // Read the page as it stands *before* the sentence is finished. This is
    // where the defect is visible and the only place it is unambiguous:
    // afterwards a refusal and a table can coexist and the page still looks
    // half right.
    let mid: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(
        !mid.iter().any(|t| t.contains("I can't")),
        "the agent answered a sentence that was still being written — the \
         writer's caret was in that very block: {mid:?}"
    );
    assert!(
        mid.iter().any(|t| t == "Scribe: make "),
        "the half-typed line was overwritten while the writer was still in \
         it: {mid:?}"
    );

    // The rest of the sentence, and then Enter — which is how an instruction
    // ends, and what takes the caret out of the block.
    typed(&mut peer, "this a run sheet", 20);
    let (delta, next) = press_enter(&mut peer.doc, &instruction);
    peer.collab.send(CollabRequest::SyncIncremental { data: delta });
    peer.collab.send(CollabRequest::Presence {
        key: caret_in(&next),
        payload: PresencePayload::Caret { pos: 0 },
    });

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut made_a_table = false;
    while Instant::now() < deadline {
        peer.pump();
        if blocks(&peer.doc).iter().any(|b| b.kind == "cell") {
            made_a_table = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = agent.join();
    for _ in 0..25 {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    // The signature of the fault, named exactly: the first word of the
    // instruction, quoted back as the whole of it.
    assert!(
        !text.iter().any(|t| t.contains("I can't \"make\"")),
        "the agent refused the first word of the instruction: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.starts_with("Scribe: I can't")),
        "an instruction the agent can carry out must not be refused: {text:?}"
    );
    assert!(
        made_a_table,
        "the finished instruction was never carried out; the page reads {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("The Hall")),
        "the bullets' contents must reach the table: {text:?}"
    );
    no_addresses_written(&peer.doc);
}

/// A finished line is not waited on.
///
/// The other half of the same rule, and the half that has to be proved
/// deliberately rather than measured: waiting is what a settle window does,
/// so a fix that only ever *added* patience would trade a wrong answer for
/// dead air — and this agent's latency already reads as dead air on camera.
///
/// So the window here is set to half a minute, far longer than the test will
/// run. Nothing about the clock can let this pass. The only thing that can
/// is the caret leaving the block, which is what Enter does and what every
/// instruction in the film ends with — and it means a committed line is acted
/// on sooner than it was before this change, not later.
#[test]
fn a_line_the_writer_has_left_is_not_waited_on() {
    let room = room();
    let mut peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page_with_bullets(""),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );

    let mut cfg = agent_cfg(&room, granting(&["word-processor"]), None);
    // Longer than this test can possibly run. If the timer is consulted at
    // all, nothing happens and the assertion below says so.
    cfg.settle = Duration::from_secs(30);

    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("the agent connects");
        s.run_until(Instant::now() + Duration::from_secs(25))
            .expect("the agent session")
    });

    let joined_by = Instant::now() + Duration::from_secs(3);
    while Instant::now() < joined_by {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let instruction = blocks(&peer.doc)
        .into_iter()
        .filter(|b| b.kind == "paragraph")
        .next_back()
        .expect("the empty instruction paragraph")
        .id;

    for ch in "Scribe: make this a run sheet".chars() {
        let delta = type_one(&mut peer.doc, &instruction, ch);
        peer.collab.send(CollabRequest::SyncIncremental { data: delta });
        let pos = blocks(&peer.doc)
            .iter()
            .find(|b| b.id == instruction)
            .map(|b| b.text.chars().count())
            .unwrap_or(0);
        peer.collab.send(CollabRequest::Presence {
            key: caret_in(&instruction),
            payload: PresencePayload::Caret { pos },
        });
        let until = Instant::now() + Duration::from_millis(10);
        while Instant::now() < until {
            peer.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Enter: the paragraph breaks and the caret goes with it.
    let (delta, next) = press_enter(&mut peer.doc, &instruction);
    peer.collab.send(CollabRequest::SyncIncremental { data: delta });
    peer.collab.send(CollabRequest::Presence {
        key: caret_in(&next),
        payload: PresencePayload::Caret { pos: 0 },
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut made_a_table = false;
    while Instant::now() < deadline {
        peer.pump();
        if blocks(&peer.doc).iter().any(|b| b.kind == "cell") {
            made_a_table = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = agent.join();
    for _ in 0..25 {
        peer.pump();
        std::thread::sleep(Duration::from_millis(20));
    }

    let text: Vec<String> = blocks(&peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(
        made_a_table,
        "the writer finished the line and left it, and the agent sat on a \
         settle window instead of acting; the page reads {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("The Hall")),
        "the bullets' contents must reach the table: {text:?}"
    );
    no_addresses_written(&peer.doc);
}

// ---------------------------------------------------------------------------
// 9. One instruction, two operations, two applications
// ---------------------------------------------------------------------------

const PHOTO_EDITOR: &str = "dev.qos.photos";

/// A real photograph on disk: a gradient, so a crop of it is provably
/// different content rather than the same picture under another name.
///
/// Written as a file because that is what the photo editor's document points
/// at — it holds a path and the edits in force, never the pixels — so an agent
/// that is going to hand the *result* to another application has to do what
/// the app does: read the file, and render the edits over it.
fn seed_photograph(dir: &std::path::Path) -> (PathBuf, u32, u32) {
    let (w, h) = (40u32, 24u32);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(&[(x * 6) as u8, (y * 10) as u8, 0x40, 0xff]);
        }
    }
    let img = photos::pixels::Rgba::new(w, h, pixels).expect("a picture");
    let png = photos::pixels::encode_png(&img).expect("encoded as a png");
    let path = dir.join("harbour.png");
    std::fs::write(&path, &png).expect("write the photograph");
    (path, w, h)
}

/// The photo editor's document: the file it has open, and no edits yet.
fn photo_doc(path: &std::path::Path, w: u32, h: u32) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("mira".as_bytes()));
    photos::doc::PhotosDoc::seed(&mut doc);
    photos::doc::PhotosDoc::set_image(
        &mut doc,
        Some(&photos::OpenImage::from_path(&path.to_string_lossy(), w, h)),
    );
    doc.commit();
    doc
}

/// The first image block in a document, with the descriptor the document
/// carries for it — the hash, and the intrinsic size read off the content.
fn placed_image(doc: &AutoCommit) -> Option<(String, i64, i64)> {
    let b = blocks(doc).into_iter().find(|b| b.kind == "image")?;
    let s = |k: &str| match b.attrs.get(k) {
        Some(qdoc::AttrVal::Str(v)) => Some(v.clone()),
        _ => None,
    };
    let n = |k: &str| match b.attrs.get(k) {
        Some(qdoc::AttrVal::Int(v)) => Some(*v),
        _ => None,
    };
    Some((s("blob")?, n("blob_w")?, n("blob_h")?))
}

/// The kinds of the document's blocks, in order — for asserting *where* a
/// placed image landed rather than only that one exists.
fn block_kinds(doc: &AutoCommit) -> Vec<String> {
    blocks(doc).into_iter().map(|b| b.kind).collect()
}

/// **The headline.** One sentence names two operations in two applications,
/// and the second is performed with what the first produced.
///
/// Every part of that is asserted separately, because the ways this can pass
/// while being broken all look like success from a distance:
///
/// * The photo editor's own document has the crop in it — step one happened,
///   in the app that owns the meaning of a crop.
/// * The word-processor document has an image under the named heading — step
///   two happened, in the other app, addressed by the heading's own id.
/// * The image's intrinsic size is the *cropped* size. This is the one that
///   distinguishes carrying the first step's result forward from placing the
///   original file: both put a picture on the page, and only one of them is
///   what was asked for.
#[test]
fn one_instruction_crops_in_one_app_and_places_the_result_in_the_other() {
    let files = tempfile::tempdir().expect("tempdir");
    let (path, w, h) = seed_photograph(files.path());
    let original = std::fs::read(&path).expect("the photograph is on disk");

    let room = room();
    let mut page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page(
            "Sound check",
            Some(
                r#"Scribe: crop the photograph to "0.25,0.25,0.5,0.5" and place image below Sound check"#,
            ),
        ),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut photo_peer = Peer::new(&room, PHOTO_EDITOR, "mira", photo_doc(&path, w, h), |d| {
        !photos::doc::PhotosDoc::is_empty(d)
    });

    let done = run_room(
        agent_cfg(&room, granting(&["word-processor", "photos"]), None),
        &mut [&mut page_peer, &mut photo_peer],
        Duration::from_secs(30),
        |peers| placed_image(&peers[0].doc).is_some(),
    );

    let text: Vec<String> = blocks(&page_peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(
        done,
        "no image reached the page; it reads {text:?} with kinds {:?}",
        block_kinds(&page_peer.doc)
    );

    // Step one, in the app that owns what a crop means.
    let edits = photos::doc::PhotosDoc::edits(&photo_peer.doc);
    assert!(
        !edits.is_identity(),
        "the photograph was never cropped on its own app's replica: {edits:?}"
    );
    assert!(
        (edits.crop.w - 0.5).abs() < 1e-6 && (edits.crop.h - 0.5).abs() < 1e-6,
        "the crop is not the one the instruction quoted: {:?}",
        edits.crop
    );

    // Step two, in the other app, under the heading the instruction named.
    let kinds = block_kinds(&page_peer.doc);
    let heading = kinds.iter().position(|k| k == "heading").expect("the heading");
    assert_eq!(
        kinds.get(heading + 1).map(String::as_str),
        Some("image"),
        "the image must go directly under the heading it was placed below: {kinds:?}"
    );

    // And it is the crop, not the file. The descriptor in the document is
    // read off the content itself, so this cannot be satisfied by anything
    // but the cropped bytes.
    let (hash, iw, ih) = placed_image(&page_peer.doc).expect("checked above");
    assert_eq!(
        (iw, ih),
        ((w / 2) as i64, (h / 2) as i64),
        "the placed image is {iw}x{ih}; the original is {w}x{h} and the crop \
         asked for is half of each side — so this is the original, not the \
         result of the first step"
    );
    assert_ne!(
        hash,
        qdoc::blobstore::hash_bytes(&original),
        "the placed content is the file on disk rather than the cropped result"
    );
    no_addresses_written(&page_peer.doc);

    // What a grouped undo would have to work from, pinned here because it is
    // a property of the dispatch rather than of any undo that exists yet.
    //
    // The step that touched this document is *one* change under the agent's
    // own actor: not a run of several to be found and gathered, and not a
    // change shared with whatever anybody else was doing. "Undo what the
    // agent did, here" is therefore addressable by actor.
    //
    // What it is not is addressable across both applications — the other step
    // is a change in the photo editor's document, a different CRDT, and no
    // undo in this one reaches it. Grouping the two is something above the
    // documents would have to record.
    let agent_changes = qdoc::history::versions(&mut page_peer.doc)
        .into_iter()
        .filter(|v| v.actor_hex == hex::encode(AGENT.as_bytes()))
        .count();
    assert_eq!(
        agent_changes, 1,
        "one step in this document should be one change to undo, not {agent_changes}"
    );
}

/// A step that cannot be carried out stops the sequence and says so, naming
/// what did happen as well as what did not.
///
/// The photograph's document names a file that is not there, so the crop
/// itself succeeds — it is a change to a document, and the document is here —
/// while the *result* of it cannot be produced at all. That is the honest
/// shape of a mid-sequence failure: the first change is real and stands, and
/// pretending otherwise by reverting somebody's document unasked would be a
/// larger decision than the one that failed.
#[test]
fn a_step_that_cannot_be_done_stops_the_sequence_and_names_both_halves() {
    let files = tempfile::tempdir().expect("tempdir");
    let missing = files.path().join("never-written.png");

    let room = room();
    let mut page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page(
            "Sound check",
            Some(
                r#"Scribe: crop the photograph to "0.25,0.25,0.5,0.5" and place image below Sound check"#,
            ),
        ),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut photo_peer = Peer::new(&room, PHOTO_EDITOR, "mira", photo_doc(&missing, 40, 24), |d| {
        !photos::doc::PhotosDoc::is_empty(d)
    });

    let answered = run_room(
        agent_cfg(&room, granting(&["word-processor", "photos"]), None),
        &mut [&mut page_peer, &mut photo_peer],
        Duration::from_secs(30),
        // Waited on by a phrase only the answer contains. The line that
        // *asked* also begins "Scribe:", and a check that matched it would
        // report success the instant the page loaded and then read the
        // instruction back as though it were the reply.
        |peers| {
            blocks(&peers[0].doc)
                .iter()
                .any(|b| b.text.contains("I couldn't"))
        },
    );
    let text: Vec<String> = blocks(&page_peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(answered, "the agent said nothing at all; the page reads {text:?}");

    let said = text
        .iter()
        .find(|t| t.contains("I couldn't"))
        .expect("checked above")
        .clone();
    assert!(
        !text.iter().any(|t| t.contains("and place image below Sound check")),
        "the answer must take the place of the line that asked, not sit beside          it: {text:?}"
    );
    // Both halves, in the app's own words. A reader looking at a document
    // that has changed must not be told only that something failed.
    assert!(
        said.to_lowercase().contains("crop"),
        "the answer must name the step that did happen: {said:?}"
    );
    assert!(
        said.to_lowercase().contains("place image below"),
        "and the step that did not: {said:?}"
    );
    assert!(
        said.contains("What is done stands"),
        "and it must say which of the two the reader is looking at: {said:?}"
    );
    // Nothing was placed. A refusal that left an image behind would be the
    // failure this is checking for.
    assert_eq!(
        placed_image(&page_peer.doc),
        None,
        "the step that was refused must have changed nothing: {:?}",
        block_kinds(&page_peer.doc)
    );
    // The step that did happen stands, on its own app's replica.
    assert!(
        !photos::doc::PhotosDoc::edits(&photo_peer.doc).is_identity(),
        "the first step is reported as done, so it has to have been done"
    );
    no_addresses_written(&page_peer.doc);
}

/// A request naming something the room cannot answer at all is refused whole,
/// before anything is dispatched.
///
/// This is the cheap half of the same principle: nothing has happened yet, so
/// there is no reason to find out the hard way. An agent that carried out the
/// part it could and then reported a failure would have edited a document on
/// the strength of half a sentence.
#[test]
fn a_sequence_with_a_part_nothing_answers_changes_nothing_at_all() {
    let files = tempfile::tempdir().expect("tempdir");
    let (path, w, h) = seed_photograph(files.path());

    let room = room();
    let mut page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page(
            "Sound check",
            Some(r#"Scribe: crop the photograph to "0.25,0.25,0.5,0.5" and juggle three plates"#),
        ),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut photo_peer = Peer::new(&room, PHOTO_EDITOR, "mira", photo_doc(&path, w, h), |d| {
        !photos::doc::PhotosDoc::is_empty(d)
    });

    let answered = run_room(
        agent_cfg(&room, granting(&["word-processor", "photos"]), None),
        &mut [&mut page_peer, &mut photo_peer],
        Duration::from_secs(30),
        |peers| {
            blocks(&peers[0].doc)
                .iter()
                .any(|b| b.text.starts_with("Scribe: I can't"))
        },
    );
    let text: Vec<String> = blocks(&page_peer.doc).iter().map(|b| b.text.clone()).collect();
    assert!(answered, "the agent neither acted nor said why; the page reads {text:?}");
    assert!(
        text.iter().any(|t| t.contains("juggle")),
        "the refusal must name the part it could not do: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("haven't done any of it")),
        "and say that none of it happened: {text:?}"
    );
    // The half it *could* have done must not have happened.
    assert!(
        photos::doc::PhotosDoc::edits(&photo_peer.doc).is_identity(),
        "a sequence refused whole must leave every part of it undone"
    );
    no_addresses_written(&page_peer.doc);
}

/// The photo editor is a fourth app of a fourth shape, and the agent holds it
/// the same way it holds the others: by being sent it, and asking the app
/// itself what it publishes.
#[test]
fn the_agent_enumerates_the_photo_editor_too() {
    let files = tempfile::tempdir().expect("tempdir");
    let (path, w, h) = seed_photograph(files.path());

    let room = room();
    let mut page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Sound check", None),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut photo_peer = Peer::new(&room, PHOTO_EDITOR, "mira", photo_doc(&path, w, h), |d| {
        !photos::doc::PhotosDoc::is_empty(d)
    });

    let mut session = Session::connect(agent_cfg(
        &room,
        granting(&["word-processor", "photos"]),
        None,
    ))
    .expect("the agent connects");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        page_peer.pump();
        photo_peer.pump();
        session
            .run_until(Instant::now() + Duration::from_millis(60))
            .expect("the agent session");
        if session.apps().contains(&"photos") {
            break;
        }
    }
    assert!(
        session.apps().contains(&"photos"),
        "the agent is holding {:?}, and the photo editor is not among them",
        session.apps()
    );
    let offers = session.offers(&session.view(None, 0));
    let crop = offers
        .iter()
        .find(|o| o.address().action == "crop")
        .unwrap_or_else(|| {
            panic!(
                "no crop among {:?}",
                offers.iter().map(|o| o.id()).collect::<Vec<_>>()
            )
        });
    assert_eq!(crop.app, "photos");
    assert!(
        crop.wants_text() && !crop.wants_content(),
        "a crop collects words — a rectangle somebody can say out loud"
    );

    let place = offers
        .iter()
        .find(|o| o.address().action == "place-image")
        .expect("the word processor exports placing an image under a heading");
    assert!(
        place.wants_content(),
        "placing an image takes content, not words — nothing anybody says names a picture"
    );
}

// ---------------------------------------------------------------------------
// 8. Opening an application that nobody has opened
// ---------------------------------------------------------------------------

/// The queue a session's desktop drains, standing in for the compositor's
/// frame loop.
///
/// The broker hands an `OpenApp` to whatever the session installed, on the
/// reader thread that decoded the frame. A real session parks it and answers
/// on its next frame, because the workspace, its capabilities and the launcher
/// all live on the thread that draws (`shell::shell::agent_opens`). This does
/// the same thing for the same reason: an app is started from the test thread,
/// where the environment a launched app inherits can be set safely.
type OpenQueue = std::sync::Arc<Mutex<Vec<shell::shell::collab_broker::OpenAppRequest>>>;

fn watching_for_opens(room: &Room) -> OpenQueue {
    let queue: OpenQueue = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&queue);
    room.broker.set_app_opener(Box::new(move |req| {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req);
    }));
    queue
}

fn taken(queue: &OpenQueue) -> Vec<shell::shell::collab_broker::OpenAppRequest> {
    let mut held = queue.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *held)
}

/// What the agent could see about the room by the end of its run.
struct Seen {
    acted: bool,
    apps: Vec<&'static str>,
    offers: Vec<String>,
}

/// Run the agent while the test plays the session that answers its requests.
///
/// `open` is what the desktop does with a request the broker hands over: it is
/// given the package id and returns the app, started. Returning `None` is a
/// session that will not open it, and the broker then says so.
fn run_room_that_opens(
    cfg: Config,
    peers: &mut Vec<Peer>,
    queue: &OpenQueue,
    within: Duration,
    mut open: impl FnMut(&str) -> Option<Peer>,
) -> Seen {
    let deadline = Instant::now() + within;
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("the agent connects");
        let acted = s
            .run_until(Instant::now() + within)
            .expect("the agent session");
        Seen {
            acted,
            apps: s.apps(),
            offers: s
                .offers(&s.view(None, 0))
                .iter()
                .map(qos_agent::Offer::id)
                .collect(),
        }
    });
    while Instant::now() < deadline {
        for req in taken(queue) {
            match open(&req.app_id) {
                Some(peer) => peers.push(peer),
                None => (),
            }
        }
        for p in peers.iter_mut() {
            p.pump();
        }
        std::thread::sleep(Duration::from_millis(20));
        if agent.is_finished() {
            break;
        }
    }
    // Whatever the agent sent last still has to reach the apps.
    for _ in 0..40 {
        for req in taken(queue) {
            if let Some(peer) = open(&req.app_id) {
                peers.push(peer);
            }
        }
        for p in peers.iter_mut() {
            p.pump();
        }
        std::thread::sleep(Duration::from_millis(20));
        if agent.is_finished() {
            break;
        }
    }
    let seen = agent.join().expect("the agent thread");
    for _ in 0..20 {
        for p in peers.iter_mut() {
            p.pump();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    seen
}

/// **The headline.** An instruction names an application that is not running,
/// and the agent opens it.
///
/// Every hop is the production one. The instruction is read off a document the
/// agent was never handed; the operation that answers it is one the agent
/// surveyed rather than one written down here; the request crosses the
/// compositor's own broker as a `CollabRequest::OpenApp`; and the app that
/// comes up joins on the transport a launched app is given. What the test
/// supplies is the one thing a test cannot have — a desktop with a launcher —
/// and it supplies it exactly where a session does, off a queue the frame loop
/// drains.
///
/// Four separate assertions, because this can pass while being broken in four
/// ways that all look like success from a distance:
///
/// * the session was asked, and asked for *that* application by package id;
/// * the app is in the room, so "opened" meant its state arrived rather than
///   a frame having been sent;
/// * the agent knows it is, which is what the next step in a sequence would
///   survey;
/// * and the offer that opened it is gone, replaced by that app's own
///   operations — an agent that still offered to open what is already open
///   would answer the next request by opening a second copy.
#[test]
fn an_instruction_opens_an_application_that_is_not_running() {
    let files = tempfile::tempdir().expect("tempdir");
    let (path, w, h) = seed_photograph(files.path());

    let room = room();
    let queue = watching_for_opens(&room);
    // Only the page. The photo editor is not in this room at all, and nothing
    // has told the agent it exists beyond the contract it speaks.
    let page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Sound check", Some("Scribe: open the photo editor")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut peers = vec![page_peer];

    let mut started: Vec<String> = Vec::new();
    let seen = run_room_that_opens(
        agent_cfg(&room, granting(&["word-processor", "photos"]), None),
        &mut peers,
        &queue,
        Duration::from_secs(40),
        |app_id| {
            started.push(app_id.to_string());
            (app_id == PHOTO_EDITOR).then(|| {
                Peer::new(&room, PHOTO_EDITOR, "mira", photo_doc(&path, w, h), |d| {
                    !photos::doc::PhotosDoc::is_empty(d)
                })
            })
        },
    );

    assert_eq!(
        started,
        vec![PHOTO_EDITOR.to_string()],
        "the session must have been asked for exactly that application, by \
         the package id a launched copy announces"
    );
    assert!(
        peers.len() == 2,
        "the photo editor never joined the room, so nothing was opened"
    );
    assert!(
        seen.acted,
        "the agent did not carry the instruction out; the page reads {:?}",
        blocks(&peers[0].doc)
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        seen.apps.contains(&"photos"),
        "the agent finished without holding the app it opened, so the step \
         after this one would survey a room that does not contain it: it \
         holds {:?}",
        seen.apps
    );
    // What opening it bought: the app's own operations, addressed by its own
    // ids, in the survey the next step would take.
    assert!(
        seen.offers.iter().any(|id| id.starts_with("photos.")
            && !id.starts_with("photos.app")),
        "opening the app must put its operations on the surface — that is the \
         whole point of opening it. The survey carries {:?}",
        seen.offers
    );
    assert!(
        !seen.offers.iter().any(|id| id == "photos.app#open"),
        "an app that is open must stop being offered as something to open, or \
         the next request is answered with a second window: {:?}",
        seen.offers
    );
    // And nothing was written over the instruction. A refusal is the failure
    // this whole path exists to avoid, and it looks like success to a test
    // that only checks that the app came up.
    no_addresses_written(&peers[0].doc);
}

/// An agent that may not open an application says so, and opens nothing.
///
/// Both halves matter and only one of them is obvious. Nothing must start —
/// the gate is not advisory. And the person who asked has to be told, naming
/// the application: a refusal that says only "I can't do that here" is true,
/// useless, and sends them looking for a better sentence rather than for the
/// permission that is actually missing.
#[test]
fn without_leave_to_open_it_the_agent_says_so_and_opens_nothing() {
    let room = room();
    let queue = watching_for_opens(&room);
    let page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Sound check", Some("Scribe: open the photo editor")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut peers = vec![page_peer];

    let mut started: Vec<String> = Vec::new();
    // Leave to act in the document, and nothing about the photo editor.
    let seen = run_room_that_opens(
        agent_cfg(&room, granting(&["word-processor"]), None),
        &mut peers,
        &queue,
        Duration::from_secs(25),
        |app_id| {
            started.push(app_id.to_string());
            None
        },
    );

    assert!(
        started.is_empty(),
        "nothing may be asked of the session on behalf of an agent that holds \
         no capability for it, yet it asked for {started:?}"
    );
    assert!(
        !seen.apps.contains(&"photos"),
        "the agent must not be holding an app it was never allowed to open"
    );

    let page: Vec<String> = blocks(&peers[0].doc)
        .iter()
        .map(|b| b.text.clone())
        .collect();
    let said = page
        .iter()
        .find(|t| t.contains("Scribe:"))
        .cloned()
        .unwrap_or_default();
    assert!(
        !said.contains("open the photo editor")
            || said.contains("I can't"),
        "the instruction is still sitting there unanswered; the page reads \
         {page:?}"
    );
    assert!(
        said.contains("I can't"),
        "a refusal has to be written where the request was, or silence and \
         refusal look the same: {page:?}"
    );
    assert!(
        said.contains("photo editor"),
        "the refusal must name the application it wanted, in the app's own \
         words rather than an address: {said:?}"
    );
    assert!(
        said.contains("leave to open"),
        "and it must say that a permission is what is missing, or the reader \
         goes looking for a better sentence instead of for the grant: {said:?}"
    );
}

/// A session with no desktop behind it refuses rather than going quiet.
///
/// The broker binds in places that host nothing — a relay, a two-peer test,
/// this one until an opener is installed. A request there has no possible
/// answer, and the only two behaviours are saying so and saying nothing. An
/// agent told nothing waits out its patience and then reports that the app
/// never opened, which is a claim about timing standing in for a claim about
/// the request.
#[test]
fn a_broker_with_no_desktop_says_so_rather_than_going_quiet() {
    let room = room();
    // Deliberately no `set_app_opener`: a broker nobody gave a desktop to.
    let page_peer = Peer::new(
        &room,
        WORD_PROCESSOR,
        "quinten",
        page("Sound check", Some("Scribe: open the photo editor")),
        |d| qdoc::model::open(d, &automerge::ROOT).is_ok(),
    );
    let mut peers = vec![page_peer];
    let queue = std::sync::Arc::new(Mutex::new(Vec::new()));

    let started = Instant::now();
    let seen = run_room_that_opens(
        agent_cfg(&room, granting(&["word-processor", "photos"]), None),
        &mut peers,
        &queue,
        Duration::from_secs(25),
        |_| None,
    );
    // The refusal has to arrive well inside the agent's own patience for an
    // app that is starting, or it is not the refusal doing the work.
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "the run took as long as the patience, so nothing answered it"
    );
    assert!(!seen.apps.contains(&"photos"));

    let page: Vec<String> = blocks(&peers[0].doc)
        .iter()
        .map(|b| b.text.clone())
        .collect();
    let said = page
        .iter()
        .find(|t| t.contains("Scribe:"))
        .cloned()
        .unwrap_or_default();
    assert!(
        said.contains("I can't"),
        "the agent must write down that it could not: {page:?}"
    );
    assert!(
        said.contains("cannot open applications"),
        "and it must repeat the session's own reason rather than substitute a \
         timeout for it: {said:?}"
    );
}
