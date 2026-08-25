//! The agent being spoken to in the room's conversation.
//!
//! ## The transport these run on, and why it is not the easy one
//!
//! qOS speaks two collaboration dialects. The legacy one is JSON over a Unix
//! socket; the one a sandboxed app actually gets is Cap'n Proto, over an fd
//! the launcher hands it in `QOS_SVC_COLLAB_FD`. Both are served by the same
//! broker and share one fan-out, which is exactly what makes a lazy test
//! dangerous: a proof that stands two JSON peers up passes whether or not the
//! path a real session takes works at all. Faults have hidden behind that gap
//! more than once.
//!
//! So the peers here are not fixtures. They are
//! [`ui::collaboration::CollaborationManager`] — the same type chat itself
//! constructs — connected the way the launcher connects one: over a
//! connected fd on the capnp socket, talking to
//! [`shell::shell::collab_broker::spawn_full`], the broker the compositor
//! binds, with both sockets bound. The agent joins the JSON socket because
//! that is the transport `qos-agent` itself has: its whole wire module is the
//! JSON dialect. So every one of these crosses the dialect boundary in the
//! direction the product does — a capnp app publishing, a JSON agent reading —
//! and none of them would pass on a same-dialect shortcut.
//!
//! Nothing here constructs a `CollabEvent` by hand.
//!
//! ## What is asserted
//!
//! The peer's replica, never the agent's own. An agent that convinces itself
//! it did something has proved nothing; the question is always what arrived
//! at the other end of the room.

use std::collections::HashSet;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use chat::doc::{ChatDoc, NewMessage};
use chat::mention::Mention;
use chat::model::Presence;
use qos_agent::{Config, Session};
use qos_reference::ResourceRef;
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
use uuid::Uuid;

/// The agent, as the room knows it.
const AGENT_ID: &str = "agent_claude";
const AGENT_NAME: &str = "Claude";
/// A person in the room. Not in the roster on purpose: a message carries its
/// author's id, and nothing here should depend on the sender being enrolled.
const ADA: &str = "u_ada";
const CHANNEL: &str = "c_general";

fn wait_for(path: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = UnixStream::connect(path) {
            return s;
        }
        assert!(Instant::now() < deadline, "the broker never bound {path:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A peer of `app` on the **capnp** socket, connected the way the launcher
/// connects a sandboxed app: over an already-connected fd, announcing its
/// package and its room.
///
/// The launcher passes those three in the environment, because a process it
/// spawned has nowhere else to read them from. This harness has somewhere
/// else — it bound the socket and minted the room itself — and writing them
/// into the environment anyway is what made this file unsafe to run in
/// parallel. `set_var` is process-wide, and from the second peer onwards
/// there are live shovel and broker threads reading the environment while it
/// is being rewritten. A mutex around the writers cannot close that, because
/// the readers belong to a `Room` some other test is still holding, outside
/// any lock this file could take; that is why these were green only at
/// `--test-threads=1`.
///
/// Passing the address removes the shared state instead of guarding it.
/// Nothing in this file writes the environment now, so nothing here needs a
/// lock. It settles the transport structurally too: `from_connected_fd` goes
/// straight to the capnp path, so a stray `QUICK_COLLAB_SOCK` can no longer
/// downgrade this peer to the dialect these tests exist to avoid.
fn capnp_peer(capnp_sock: &PathBuf, room: &Uuid, app: &str) -> CollaborationManager {
    let fd = wait_for(capnp_sock).into_raw_fd();
    let m = CollaborationManager::from_connected_fd(fd, app, *room);
    assert!(
        m.is_connected(),
        "the {app} peer did not reach the broker over capnp — these tests \
         prove nothing on a fallback transport"
    );
    m
}

/// The broker the compositor binds, with both dialects served.
///
/// The handle is returned rather than dropped, and that is not tidiness:
/// dropping it signals the accept loops to quit and unlinks both sockets, so a
/// harness that lets it go out of scope tears the broker down before the first
/// peer connects and then reports the silence as the agent's fault.
fn broker() -> (tempfile::TempDir, PathBuf, PathBuf, shell::shell::collab_broker::BrokerHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("collab-json.sock");
    let capnp = dir.path().join("collab.sock");
    let handle = shell::shell::collab_broker::spawn_full(json.clone(), Some(capnp.clone()), None)
        .expect("the broker binds both sockets");
    (dir, json, capnp, handle)
}

/// The same broker, standing for a machine the room has an address for.
///
/// [`broker`] has no network, so it has no room-wide address and announces
/// no participant of its own -- which is right for the tests above: they
/// are about the chat document's own membership, where the agent is
/// `agent_claude` because the room wrote that down.
///
/// A machine in a real room is not like that. Its peers address it as
/// `peer@<handle>`, minted from the handle it stamps on everything it
/// publishes, and a mention resolved on another machine carries that id and
/// no other. This is the broker that can say so.
fn broker_for_machine(
    handle: u64,
    label: shell::shell::collab_broker::PeerLabel,
) -> (tempfile::TempDir, PathBuf, PathBuf, shell::shell::collab_broker::BrokerHandle) {
    use shell::shell::collab_broker::{LocalIdentity, PeerLabels};
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("collab-json.sock");
    let capnp = dir.path().join("collab.sock");
    let identity = LocalIdentity::with_lookup(handle, move |_cluster: &str| {
        let mut m = PeerLabels::new();
        m.insert(handle, label.clone());
        m
    });
    let broker = shell::shell::collab_broker::spawn_full_with_identity(
        json.clone(),
        Some(capnp.clone()),
        None,
        identity,
    )
    .expect("the broker binds both sockets");
    (dir, json, capnp, broker)
}

/// A room's conversation: one workspace, one channel, and the agent enrolled
/// as an agent so an `@mention` of it can resolve to a participant id.
fn conversation() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("chat-app".as_bytes()));
    // Seeds the workspace and `c_general`, and records this participant as a
    // member — membership only, because scaffolding a room is still a peer
    // writing about itself and a name is not a peer's to declare.
    ChatDoc::seed_starter(&mut doc, AGENT_ID);
    ChatDoc::set_member_is_agent(&mut doc, AGENT_ID, true);
    ChatDoc::ensure_member(&mut doc, AGENT_ID, true, Presence::Online);
    // The name arrives the way every name in a room does: written *about* the
    // participant by the room, not by the participant. `me_in_chat` resolves
    // the agent's own id by matching this name, so a room that never names it
    // leaves it anonymous — which is the whole reason this is a separate call
    // and not an argument to the seed.
    ChatDoc::ensure_user(&mut doc, AGENT_ID, AGENT_NAME, "C", 0, Presence::Online);
    doc.commit();
    doc
}

/// A message, as chat's own document writes one.
fn say(doc: &mut AutoCommit, id: &str, author: &str, text: &str, ts: i64) {
    ChatDoc::push_message_with_id(
        doc,
        CHANNEL,
        id,
        NewMessage { author: author.into(), ts, text: text.into(), ..Default::default() },
    );
    doc.commit();
}

/// The same, with `@Claude` resolved the way the composer resolves it: the run
/// it covers, and the participant id behind it.
fn mention(doc: &mut AutoCommit, id: &str, text: &str, ts: i64, about: Option<ResourceRef>) {
    let at = text.find('@').expect("the fixture mentions somebody");
    let len = text[at..].find(' ').unwrap_or(text.len() - at);
    ChatDoc::push_message_with_id(
        doc,
        CHANNEL,
        id,
        NewMessage {
            author: ADA.into(),
            ts,
            text: text.into(),
            mentions: vec![Mention { user: AGENT_ID.into(), start: at, len, is_agent: true }],
            reference: about,
            ..Default::default()
        },
    );
    doc.commit();
}

/// A rough draft with two headings, so a section is a real, addressable thing
/// and — crucially — so there are *two* of them. One heading would make every
/// request unambiguous by accident, and the boundary these tests are about is
/// exactly when the agent may narrow to one of several.
fn draft() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("wp-app".as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let a = dm.insert_block(None, "heading");
        dm.set_attr(&a, "level", 1i64);
        dm.splice_text(&a, 0, 0, "Run sheet");
        let b = dm.insert_block(Some(&a), "heading");
        dm.set_attr(&b, "level", 1i64);
        dm.splice_text(&b, 0, 0, "Budget");
    }
    doc.commit();
    doc
}

/// The block ids of the draft's headings, in order.
fn headings(doc: &AutoCommit) -> Vec<String> {
    let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
    qdoc::model::materialize(doc, &c)
        .blocks
        .iter()
        .filter(|b| b.kind == "heading")
        .map(|b| b.id.clone())
        .collect()
}

/// The agent, granted the room's three apps.
fn agent(json: &PathBuf, room: Uuid) -> Session {
    let subject = Subject::agent(AGENT_NAME);
    let caps = ["word-processor", "files", "chat"]
        .iter()
        .map(|app| {
            CapOverride::grant(subject.clone(), Scope::App((*app).into()), Verb::Write)
                .with_id(format!("agent-{app}"))
        })
        .collect();
    Session::connect(Config {
        socket: Some(json.clone()),
        // Empty: the agent joins the *room*, not one app, which is what lets
        // it hold the conversation and the document at the same time.
        app_id: String::new(),
        cluster: room,
        record: None,
        name: AGENT_NAME.into(),
        pace: Duration::from_millis(0),
        // Whole beats, not "stop at the first thing done": a request that
        // borrows its subject only becomes answerable once an earlier one has
        // landed, and stopping early would make that case untestable.
        once: false,
        model: false,
        model_timeout: Duration::from_millis(1),
        // The instruction is in the page before the agent connects, so
        // there is no half-typed line here to wait out.
        settle: Duration::ZERO,
        await_document: Duration::ZERO,
        directory: None,
        permissions: PermissionContext::from_capabilities(Actor::agent(AGENT_NAME), caps),
    })
    .expect("the agent joins the room")
}

/// Everything a peer has been sent since last time, merged into `into`.
fn drain(peer: &mut CollaborationManager, into: &mut AutoCommit) {
    let mut arrived = false;
    for ev in peer.poll() {
        match ev {
            // Every frame in the room reaches a peer that named no record,
            // and most of them belong to somebody else's document. Applying
            // one of those does not merely fail: it leaves the replica in a
            // state where the *next* delta — the one that did belong here —
            // no longer materialises. That is what swallowed the agent's
            // answer, which it had demonstrably put on the wire.
            //
            // So a delta is tried on a copy and kept only if it took. The
            // real app has the same problem and solves it the same way, by
            // refusing a document that is not its own rather than merging it.
            CollabEvent::CRDTIncremental { data } => {
                let mut probe = into.clone();
                if probe.load_incremental(&data).is_ok() {
                    *into = probe;
                }
            }
            CollabEvent::CRDTUpdate { data } => {
                if let Ok(mut other) = AutoCommit::load(&data) {
                    let mut probe = into.clone();
                    if probe.merge(&mut other).is_ok() {
                        *into = probe;
                    }
                }
            }
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => arrived = true,
            _ => {}
        }
    }
    // Answer an arrival with the whole document, which is what every app in a
    // room does and what the agent's late-join depends on: the broker fans out
    // live and keeps nothing, so a snapshot sent before the agent subscribed
    // reaches nobody. Without this the agent joins an apparently empty room
    // and the silence reads as it having nothing to say.
    if arrived {
        peer.send(CollabRequest::SyncCRDT { data: into.save() });
    }
}

/// The whole beat: stand the room up, let the agent run, and hand back what
/// the chat peer ended up holding plus the agent itself.
struct Room {
    chat_doc: AutoCommit,
    session: Session,
    /// Held for the room's lifetime — see [`broker`].
    _broker: shell::shell::collab_broker::BrokerHandle,
    _dir: tempfile::TempDir,
}

impl Room {
    /// `conversation` is the channel as the chat app holds it; `document` is
    /// the page the word processor holds, when the test needs one.
    fn open(conversation: AutoCommit, document: Option<AutoCommit>) -> Self {
        Self::open_on(conversation, document, broker())
    }

    /// The same, on a named broker -- see [`broker_for_machine`].
    fn open_on(
        conversation: AutoCommit,
        document: Option<AutoCommit>,
        parts: (tempfile::TempDir, PathBuf, PathBuf, shell::shell::collab_broker::BrokerHandle),
    ) -> Self {
        let (dir, json, capnp, handle) = parts;
        let room = Uuid::new_v4();

        let mut chat = capnp_peer(&capnp, &room, "dev.qos.chat");
        let mut chat_doc = conversation;
        chat.send(CollabRequest::SyncCRDT { data: chat_doc.save() });

        let mut wp = document.map(|mut d| {
            let mut m = capnp_peer(&capnp, &room, "dev.qos.word-processor");
            m.send(CollabRequest::SyncCRDT { data: d.save() });
            (m, d)
        });

        let mut session = agent(&json, room);
        // The budget is generous because the peer's snapshot has to cross the
        // broker before there is anything to read at all.
        for _ in 0..260 {
            let _ = session
                .run_until(Instant::now() + Duration::from_millis(60))
                .expect("the agent's turn");
            drain(&mut chat, &mut chat_doc);
            if let Some((m, d)) = wp.as_mut() {
                drain(m, d);
            }
        }
        Self { chat_doc, session, _broker: handle, _dir: dir }
    }

    /// The agent's replies in the channel, by the message each answers.
    fn replies(&self) -> Vec<(String, String)> {
        ChatDoc::list_messages(&self.chat_doc, CHANNEL)
            .into_iter()
            .filter(|m| m.author == AGENT_ID)
            .filter_map(|m| m.parent.clone().map(|p| (p, m.text)))
            .collect()
    }

    /// The reply answering `msg`, if the agent wrote one.
    fn reply_to(&self, msg: &str) -> Option<String> {
        self.replies().into_iter().find(|(p, _)| p == msg).map(|(_, t)| t)
    }

    /// The reply answering `msg` from a given author.
    ///
    /// [`Self::replies`] reads the agent back as `agent_claude`, which is
    /// who it is in the chat document. A machine the room has an address
    /// for gives it a different one -- and it writes under whichever the
    /// room told it, which is the point.
    fn reply_to_from(&self, author: &str, msg: &str) -> Option<String> {
        ChatDoc::list_messages(&self.chat_doc, CHANNEL)
            .into_iter()
            .find(|m| m.author == author && m.parent.as_deref() == Some(msg))
            .map(|m| m.text)
    }

    /// What the agent recorded it did with `msg`, as another machine reads
    /// it: out of the shared conversation, through chat's own reader.
    fn record_for(&self, msg: &str) -> Option<chat::model::AgentWork> {
        ChatDoc::list_messages(&self.chat_doc, CHANNEL)
            .into_iter()
            .find(|m| m.parent.as_deref() == Some(msg) && m.work.is_some())
            .and_then(|m| m.work)
    }

    /// The resource id that reached dispatch, if an operation was performed.
    fn acted_on(&self) -> Option<String> {
        self.session.last_action().map(|l| l.resource.clone())
    }
}

/// Epoch seconds, the unit a message's `ts` is in.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------

/// 1. An `@mention` reaches the agent, and it acts.
///
/// The heading is called "Run sheet" on purpose. `Recipe::from_keywords`
/// matches that substring anywhere in the request, so this wording used to
/// come back as "ask me for a table in the document" — a word in the prose
/// colliding with a recipe's keyword and overriding an address the author had
/// explicitly attached. It stays as the pin for that.
#[test]
fn a_mention_of_the_agent_in_chat_is_noticed_and_acted_on() {
    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    // Names its own target, so nothing here depends on an inferred subject.
    mention(
        &mut chat,
        "m_ask",
        &format!("@Claude align centre the {} section", "Run sheet"),
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let room = Room::open(chat, Some(doc));
    assert_eq!(
        room.acted_on().as_deref(),
        Some(section.as_str()),
        "a mention in chat must reach the agent and be carried out. \
         Holding: {:?}; replies: {:?}",
        room.session.apps(),
        room.replies()
    );
    assert!(
        room.reply_to("m_ask").is_some(),
        "and it must say in the channel that it did. Answers put on the wire: \
         {}; replies the room can see: {:?}",
        room.session.answers_sent(),
        room.replies()
    );
}

/// A mention resolved on somebody else's machine reaches this agent.
///
/// The failure this covers had every link passing its own test. The gesture
/// opened a compose surface, the remark reached the room, the agent joined
/// and held the document -- and then sat there, because the remark was
/// addressed to `peer@<handle>` and the agent had never been told that this
/// was a name for itself. Nothing on its machine could tell it: the id is
/// minted by whoever *receives* a frame, and a machine does not receive its
/// own.
///
/// Three things are arranged so that only the new path can answer, because
/// each of them on its own is answerable by the old one.
///
/// The chat document does not enrol this agent, so the fallback that reads
/// the room's own membership has nothing to find. The room names this
/// machine's peer `Quinten` and as a person -- which is what a room says
/// about a machine nobody wrote an agent grant against -- so the fallback
/// that matches a roster row by name and mark cannot find it either. And
/// the person the conversation *does* hold is called **Claude**, the name
/// this agent runs under, so a reader that reaches for the name at all gets
/// a human being. That is the collision
/// `a_person_called_claude_is_not_the_agent_the_room_admitted` pins on the
/// roster, asserted here on the participant that acts on it.
///
/// What is left is the row's own mark, which is the only thing in the room
/// that says which participant this process is.
#[test]
fn a_mention_addressed_to_this_machine_reaches_the_agent_that_is_it() {
    // The handle the room knows this machine's peer by, and so the id every
    // other machine in it resolves a mention of this agent to.
    const HANDLE: u64 = 2_126_281_246_283_428_914;
    let me = format!("peer@{HANDLE}");

    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);

    // A conversation this agent is not a member of, holding a person who
    // happens to be called what it is called.
    let mut chat = AutoCommit::new();
    chat.set_actor(automerge::ActorId::from("chat-app".as_bytes()));
    ChatDoc::seed_starter(&mut chat, ADA);
    ChatDoc::ensure_member(&mut chat, ADA, true, Presence::Online);
    ChatDoc::ensure_user(&mut chat, ADA, "Ada", "A", 0, Presence::Online);
    ChatDoc::ensure_member(&mut chat, "u_claude", true, Presence::Online);
    ChatDoc::ensure_user(&mut chat, "u_claude", AGENT_NAME, "C", 1, Presence::Online);
    chat.commit();

    // The remark, addressed the way the compose surface on another machine
    // addresses one: to the participant id that machine's roster holds.
    let text = format!("@{AGENT_NAME} align centre the Run sheet section");
    let at = text.find('@').expect("the fixture mentions somebody");
    let len = text[at..].find(' ').unwrap_or(text.len() - at);
    ChatDoc::push_message_with_id(
        &mut chat,
        CHANNEL,
        "m_remark",
        NewMessage {
            author: ADA.into(),
            ts: now(),
            text: text.clone(),
            mentions: vec![Mention { user: me.clone(), start: at, len, is_agent: true }],
            reference: Some(ResourceRef::whole(
                "word_processor.section",
                &section,
                "Run sheet",
            )),
            ..Default::default()
        },
    );
    chat.commit();

    let room = Room::open_on(
        chat,
        Some(doc),
        broker_for_machine(
            HANDLE,
            shell::shell::collab_broker::PeerLabel::person("Quinten"),
        ),
    );

    assert_eq!(
        room.acted_on().as_deref(),
        Some(section.as_str()),
        "a remark addressed to this machine's participant must reach the \
         agent running on it and be carried out. Holding: {:?}; every reply \
         in the room: {:?}",
        room.session.apps(),
        ChatDoc::list_messages(&room.chat_doc, CHANNEL)
            .into_iter()
            .map(|m| (m.author, m.text))
            .collect::<Vec<_>>(),
    );
    assert!(
        room.reply_to_from(&me, "m_remark").is_some(),
        "and it must answer under the id it was addressed by. Answers put \
         on the wire: {}",
        room.session.answers_sent(),
    );
    assert!(
        room.reply_to_from("u_claude", "m_remark").is_none(),
        "and never as the person who merely shares its name",
    );
}

/// 2. A message that says nothing to the agent is left alone, however near.
#[test]
fn a_message_that_does_not_mention_the_agent_is_not_acted_on() {
    let doc = draft();
    let mut chat = conversation();
    // Ordinary room chatter, in a channel the agent is in, naming an
    // operation it could perform. Nothing addresses it.
    say(&mut chat, "m_chatter", ADA, "align centre the run sheet please", now());

    let room = Room::open(chat, Some(doc));
    assert_eq!(
        room.acted_on(),
        None,
        "being in the channel is not being spoken to"
    );
    assert!(
        room.replies().is_empty(),
        "and an agent that answers unaddressed chatter is worse than one that misses a mention"
    );
}

/// A compose box being typed into is not a message, however far it has got.
///
/// Worth pinning because a draft here is not private scratch space: the
/// composer diff-splices every keystroke into a per-channel `Text` in the
/// shared document (`ChatDoc::set_draft`), so half a sentence somebody is
/// still writing genuinely does reach this agent's replica. What keeps it
/// from being acted on is that a message does not exist until Send appends
/// one — nothing in the agent's path reads a draft, and this is what says
/// so out loud.
///
/// The text is the one that matters. An instruction sent from a compose
/// panel opens `@Claude make …`, and `Claude: make ` — the same thirteen
/// characters — is exactly what a document paragraph got answered at, with
/// the word `make` quoted back as the whole request. On this path that
/// cannot happen, and a change that made the agent read drafts "so it can
/// see what is coming" would reintroduce it here. Then this fails.
#[test]
fn a_half_typed_compose_box_is_not_a_message() {
    let doc = draft();
    let mut chat = conversation();
    // Typed, not sent: the state the composer is in between the person
    // reaching for `m` and reaching for Send.
    ChatDoc::set_draft(&mut chat, CHANNEL, "@Claude make ");
    chat.commit();

    let room = Room::open(chat, Some(doc));

    // Non-vacuous: the half-written sentence really is in the document that
    // crossed the broker to the agent. Without this the test could pass by
    // the draft never having been shared at all, which would prove nothing
    // about what the agent does with one.
    assert_eq!(
        ChatDoc::draft_text(&room.chat_doc, CHANNEL),
        "@Claude make ",
        "the draft must be in the shared document for this test to mean \
         anything — a draft nobody shared cannot be misread"
    );
    assert_eq!(
        room.acted_on(),
        None,
        "the agent acted on a sentence still being typed into a compose box"
    );
    assert!(
        room.replies().is_empty(),
        "and it must not answer one either — the person has not sent \
         anything yet: {:?}",
        room.replies()
    );
}

/// 3. A carried address is the subject — asserted on the id, not the prose.
#[test]
fn a_carried_resource_address_is_what_the_agent_acts_on() {
    let doc = draft();
    let hs = headings(&doc);
    // The *second* heading, while the body names the first. A consumer that
    // reads the sentence instead of the address gets the wrong one, which is
    // the whole property under test.
    let subject = format!("word-processor.section:{}", hs[1]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_this",
        "@Claude centre this, the one under Run sheet",
        now(),
        Some(ResourceRef::whole("word_processor.section", &subject, "Budget")),
    );

    let room = Room::open(chat, Some(doc));
    assert_eq!(
        room.acted_on().as_deref(),
        Some(subject.as_str()),
        "the address the message carried is the subject, never the one the text describes"
    );
}

/// 4. The document path still works — a paragraph opening with the agent's
///    name and a colon, exactly as before listening existed.
///
/// A regression guard with teeth, because the conversation is now the first
/// thing the agent could look at: a change that made chat shadow the document
/// would leave this the only thing that noticed.
#[test]
fn an_instruction_written_in_the_document_is_still_carried_out() {
    let mut doc = draft();
    {
        let c = qdoc::model::open(&doc, &automerge::ROOT).expect("schema");
        let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
        let last = qdoc::model::materialize(&doc, &c)
            .blocks
            .last()
            .map(|b| b.id.clone())
            .expect("a block");
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let p = dm.insert_block(Some(&last), "paragraph");
        dm.splice_text(&p, 0, 0, "Claude: turn the bullets into a table");
    }
    doc.commit();

    // A conversation is present and says nothing to the agent, so this proves
    // the document is still read rather than merely that chat was empty.
    let mut chat = conversation();
    say(&mut chat, "m_idle", ADA, "morning all", now());

    let room = Room::open(chat, Some(doc));
    assert!(
        room.session.last_route().is_some(),
        "the agent decided something from the document, as it always has"
    );
    assert!(
        room.replies().is_empty(),
        "and answered in the document, not in a channel nobody addressed it from"
    );
}

/// 5a. Near its last action, an unstated subject is the one it just worked on
///     — and with nothing recent, the same words are refused instead.
///
/// The draft has two headings, so "align centre" fits two sections and is
/// ambiguous on its own. That ambiguity is what gives this teeth: a pass means
/// the subject was **borrowed from the agent's own last action**, not guessed,
/// because guessing is precisely what the second half proves it will not do.
#[test]
fn a_request_near_the_agents_last_action_borrows_its_subject() {
    let doc = draft();
    let first = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_first",
        "@Claude align left",
        now(),
        Some(ResourceRef::whole("word_processor.section", &first, "Run sheet")),
    );
    // Names no target at all. Answerable only because the first just landed.
    mention(&mut chat, "m_again", "@Claude align centre", now(), None);

    let room = Room::open(chat, Some(doc));
    let follow_up = room
        .reply_to("m_again")
        .expect("the follow-up was answered at all");
    assert!(
        !follow_up.contains("can't") && !follow_up.contains("could mean"),
        "with the first ask fresh, the follow-up means the section just worked \
         on rather than either of two. Got: {follow_up:?}"
    );
    assert_eq!(
        room.acted_on().as_deref(),
        Some(first.as_str()),
        "and it landed on that resource"
    );

    // The far side: the same words, with nothing recent to borrow from.
    let mut alone = conversation();
    mention(&mut alone, "m_again", "@Claude align centre", now(), None);
    let probe = Room::open(alone, Some(draft()));
    assert_eq!(
        probe.acted_on(),
        None,
        "with nothing recent, an unstated subject must not be guessed at"
    );
    let said = probe.reply_to("m_again").expect("a refusal, not silence");
    assert!(
        said.contains("can't") || said.contains("could mean"),
        "and it has to say so: {said:?}"
    );
}

/// 5b. The boundary itself, on the pure decision, both sides of it.
#[test]
fn recency_is_bounded_and_the_far_side_refuses() {
    use qos_agent::listen::{self, LastAction, Subject, RECENCY_SECS};

    let doc = draft();
    let hs = headings(&doc);
    let first = format!("word-processor.section:{}", hs[0]);
    let mut chat = conversation();
    mention(&mut chat, "m_again", "@Claude centre it", 10_000, None);
    let msgs = ChatDoc::list_messages(&chat, CHANNEL);

    let inside = LastAction::new(&first, 10_000 - RECENCY_SECS + 1);
    let s = listen::next_summons(&msgs, AGENT_ID, AGENT_NAME, Some(&inside), 10_000, &HashSet::new())
        .expect("a summons");
    assert_eq!(
        s.subject.resource(),
        Some(first.as_str()),
        "inside the window the last action is the subject"
    );

    let outside = LastAction::new(&first, 10_000 - RECENCY_SECS - 1);
    let s = listen::next_summons(&msgs, AGENT_ID, AGENT_NAME, Some(&outside), 10_000, &HashSet::new())
        .expect("still addressed, still a summons");
    assert_eq!(
        s.subject,
        Subject::Nothing,
        "one second past the window there is nothing to borrow"
    );
}

/// 6. The same message is not acted on twice, including across a restart.
#[test]
fn the_same_message_is_not_acted_on_twice_across_a_restart() {
    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_once",
        "@Claude align centre",
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let first = Room::open(chat, Some(doc.clone()));
    assert_eq!(first.acted_on().as_deref(), Some(section.as_str()), "acted once");
    let after = first.chat_doc.clone();
    assert_eq!(
        first.replies().len(),
        1,
        "exactly one answer for one question"
    );

    // A wholly new agent process, handed the conversation as it now stands —
    // the agent's own reply included. This is what a restart looks like, and
    // an in-memory set of handled ids would answer "never seen it".
    let second = Room::open(after, Some(doc));
    assert_eq!(
        second.acted_on(),
        None,
        "a restarted agent must not redo work the room already records as done"
    );
    assert_eq!(
        second.replies().len(),
        1,
        "and must not answer the same message a second time"
    );
}

/// 7. Something it cannot do is refused out loud, not swallowed.
#[test]
fn an_instruction_it_cannot_fulfil_is_refused_visibly() {
    let doc = draft();
    let mut chat = conversation();
    mention(&mut chat, "m_no", "@Claude reticulate the splines", now(), None);

    let room = Room::open(chat, Some(doc));
    assert_eq!(room.acted_on(), None, "nothing was performed");
    let said = room
        .reply_to("m_no")
        .expect("a person who mentions the agent must be able to tell it declined");
    assert!(
        said.contains(AGENT_NAME),
        "the refusal is signed, so it reads as the agent's answer: {said:?}"
    );
}

/// 8. The room can read *what* the agent did, not only that it answered.
///
/// The one sentence the agent used to post said "done" and nothing about
/// which operation, and nothing at all until the work had finished. What the
/// room holds now is a record on that same reply: the state it ended in, the
/// app's own word for what it performed, and when. Read here out of the shared
/// document through chat's own reader, which is the only place another
/// machine would ever read it from.
#[test]
fn what_the_agent_did_is_in_the_room_and_not_only_in_its_log() {
    use chat::model::AgentWorkState;

    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_ask",
        "@Claude align centre",
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let room = Room::open(chat, Some(doc));
    assert_eq!(room.acted_on().as_deref(), Some(section.as_str()), "it acted");
    let work = room
        .record_for("m_ask")
        .expect("the reply must carry what the agent did with the message");
    assert!(
        work.finished(),
        "the work is over, so the record must not still read as in flight: {work:?}"
    );
    match &work.state {
        AgentWorkState::Done { did } => assert!(
            !did.trim().is_empty(),
            "a record that says 'done' and not what was done says almost nothing"
        ),
        other => panic!("an applied operation must be recorded as done, not {other:?}"),
    }
    assert!(
        work.since > 0,
        "the record has to say when, or nobody can tell a fresh state from a stale one"
    );
}

/// 9. A refusal is recorded as a refusal, and never left reading as work in
/// progress.
///
/// The failure this pins is the one the whole surface is built to avoid: a
/// strip that goes on saying the agent is working after it has stopped. It is
/// worse than the silence it replaced, because silence at least does not
/// assert anything.
#[test]
fn a_refusal_leaves_a_record_that_says_it_stopped() {
    use chat::model::AgentWorkState;

    let doc = draft();
    let mut chat = conversation();
    mention(&mut chat, "m_no", "@Claude reticulate the splines", now(), None);

    let room = Room::open(chat, Some(doc));
    assert_eq!(room.acted_on(), None, "nothing was performed");
    let work = room
        .record_for("m_no")
        .expect("a refusal has to be legible in the room, not only in the log");
    assert_eq!(
        work.state,
        AgentWorkState::Refused,
        "the agent declined, and the record must say so"
    );
    assert!(work.finished());
    let said = room.reply_to("m_no").expect("and it says why");
    assert!(
        said.contains(AGENT_NAME),
        "the refusal is signed, so it reads as the agent's answer: {said:?}"
    );
}

/// 10. Taking a summons and answering it leave one message, not two.
///
/// The reply goes up when the work starts and is completed in place, so the
/// thread carries one answer per question. A second message would also make
/// `listen::answered` read two records for one summons and put two strips
/// under one sentence.
#[test]
fn taking_a_message_on_and_answering_it_leave_one_reply() {
    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_ask",
        "@Claude align centre",
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let room = Room::open(chat, Some(doc));
    let answers: Vec<_> = room.replies().into_iter().filter(|(p, _)| p == "m_ask").collect();
    assert_eq!(
        answers.len(),
        1,
        "one question, one answer — got {answers:?}"
    );
    assert!(
        !answers[0].1.trim().is_empty(),
        "the reply went up empty and must not have been left that way"
    );
}

/// The trace is a record of dispatches, not a summary of intentions.
///
/// An operation that went through leaves one row naming the resource it was
/// addressed to and saying it was applied. The row is read out of the shared
/// conversation by the peer, which is the only place another machine would
/// ever read it from.
#[test]
fn an_operation_that_was_performed_leaves_a_row_saying_so() {
    use chat::model::AgentStepOutcome;

    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_ask",
        "@Claude align centre",
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let room = Room::open(chat, Some(doc));
    assert_eq!(room.acted_on().as_deref(), Some(section.as_str()), "it acted");
    let work = room.record_for("m_ask").expect("a record reached the room");
    assert_eq!(
        work.trace.len(),
        1,
        "one operation was dispatched, so the room must show exactly one row: {:?}",
        work.trace
    );
    let row = &work.trace[0];
    assert_eq!(
        row.resource, section,
        "the row must name the resource that was actually addressed"
    );
    assert_eq!(
        row.outcome,
        AgentStepOutcome::Applied,
        "the operation changed the document, so the row must say it was applied"
    );
    assert!(
        !row.label.trim().is_empty(),
        "a row with no words for what was done is not worth drawing"
    );
    // The label is the app's own word for the operation, never the address it
    // dispatched by. An address in this field would be machine vocabulary on
    // a line a person reads.
    assert!(
        !row.label.contains("word-processor.section:"),
        "the row shows an address instead of a label: {:?}",
        row.label
    );
}

/// **The anti-fabrication property.** A turn that dispatched nothing shows no
/// rows at all.
///
/// This is the one that makes the trace worth reading. The agent was spoken
/// to, it decided, it answered — and because it never dispatched anything,
/// the room has nothing to show. A trace built from the plan, from the
/// model's answer, or from the sentence the agent wrote would all put a row
/// here, and each of them would be a claim about work that never happened.
#[test]
fn a_turn_that_dispatched_nothing_shows_no_rows() {
    let doc = draft();
    let mut chat = conversation();
    mention(&mut chat, "m_no", "@Claude reticulate the splines", now(), None);

    let room = Room::open(chat, Some(doc));
    assert_eq!(room.acted_on(), None, "nothing was performed");
    let work = room
        .record_for("m_no")
        .expect("a refusal is still recorded in the room");
    assert!(
        work.trace.is_empty(),
        "nothing was dispatched, so the room must show no operations at all — \
         a row here would be an operation the agent never performed: {:?}",
        work.trace
    );
    // And the agent did answer, so this is not "no rows because no turn".
    assert!(room.reply_to("m_no").is_some(), "it answered in the room");
}

/// How the agent decided travels with the record, and says which path
/// decided.
///
/// These sessions run with `model: false`, so every one of them is decided by
/// the agent's own keyword vocabulary. The record has to say that. A route
/// that read `Model` on a turn no model was asked about would be the exact
/// misrepresentation the field exists to prevent — a recording made on the
/// fallback has to stay findable as one afterwards.
#[test]
fn the_record_says_which_path_decided_and_does_not_flatter_it() {
    use chat::model::AgentRoute;

    let doc = draft();
    let section = format!("word-processor.section:{}", headings(&doc)[0]);
    let mut chat = conversation();
    mention(
        &mut chat,
        "m_ask",
        "@Claude align centre",
        now(),
        Some(ResourceRef::whole("word_processor.section", &section, "Run sheet")),
    );

    let room = Room::open(chat, Some(doc));
    let work = room.record_for("m_ask").expect("a record reached the room");
    let reasoning = work
        .reasoning
        .as_ref()
        .expect("a turn that decided something must say how");
    assert_eq!(
        reasoning.route,
        AgentRoute::Floor,
        "no model was asked on this turn, so the record must not claim one chose"
    );
    assert!(
        !reasoning.why.trim().is_empty(),
        "the record says a path decided but not why it had to: {reasoning:?}"
    );
}
