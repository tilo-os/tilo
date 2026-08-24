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
    _broker: shell::shell::collab_broker::BrokerHandle,
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
        _broker: broker,
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
                    let mut probe = self.doc.clone();
                    if probe.load_incremental(&data).is_ok() {
                        self.doc = probe;
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
        if !session.offers(&session.view(None, 0)).is_empty() {
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
    let found: Vec<String> = session
        .offers(&session.view(None, 0))
        .iter()
        .map(|o| o.id())
        .collect();

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
