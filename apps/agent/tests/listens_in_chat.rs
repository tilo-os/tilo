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
//! constructs — connected the way the launcher connects one: an fd in
//! `QOS_SVC_COLLAB_FD`, `QUICK_COLLAB_SOCK` explicitly cleared so the legacy
//! branch cannot be taken, talking to
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
use std::sync::{Mutex, MutexGuard, OnceLock};
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

/// Connecting a peer means writing the process environment, because that is
/// where a launched app reads its fd and its room from. Every test here is
/// serialised behind this so no two are mid-handshake at once.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

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

/// A peer of `app`, connected the way the launcher connects a sandboxed app:
/// a connected fd on the **capnp** socket, named in `QOS_SVC_COLLAB_FD`.
///
/// `QUICK_COLLAB_SOCK` is removed rather than merely left unset. The manager
/// prefers the legacy dialect whenever that variable names a socket it can
/// reach, and a stray one from another test would silently downgrade this peer
/// to the transport these tests exist to avoid.
fn capnp_peer(capnp_sock: &PathBuf, room: &Uuid, app: &str) -> CollaborationManager {
    let fd = wait_for(capnp_sock).into_raw_fd();
    // SAFETY: every test in this file holds `env_lock` across this call, and
    // `CollaborationManager::new` reads these before it returns.
    unsafe {
        std::env::remove_var("QUICK_COLLAB_SOCK");
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_WORKSPACE_ID", room.to_string());
        std::env::set_var("QOS_PACKAGE_ID", app);
    }
    let m = CollaborationManager::new();
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
        let _guard = env_lock();
        let (dir, json, capnp, handle) = broker();
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
