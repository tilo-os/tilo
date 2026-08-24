//! One agent session: join the room, hold the replica, act on instructions.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use qos_actions::ActionRegistry;
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use ui::permissions::PermissionContext;
use ui::presence::{PathSegment, PresenceKey};
use uuid::Uuid;
use word_processor::actions::{self as wp, RegionKind, WordProcessorAction};

use crate::apply::Editor;
use crate::listen::{self, LastAction, Summons};
use crate::model::{Chooser, Decision, Route};
use crate::plan::{self, Block, Instruction, Plan};
use crate::surface::{self, AppContract, Offer, View};
use crate::wire::Link;

/// Where an answer goes.
///
/// The agent answers wherever it was spoken to, and both places are
/// answerable: over the paragraph that asked, or as a reply in the channel
/// the message came from. Naming this rather than threading a
/// `(blocks, index)` pair through every refusal is what let the chat path
/// reuse the dispatch path whole — the checks an operation has to pass are
/// the same checks whether the request arrived in a document or a channel,
/// and having one copy of them is the point.
#[derive(Clone, Debug)]
enum Answer {
    /// Rewrite the paragraph that asked, in the word-processor document.
    Block { id: String, chars: usize },
    /// Post a threaded reply under the message that asked.
    ///
    /// The reply is also the record that the message has been dealt with —
    /// see [`crate::listen::answered`] — so this is not decoration that can
    /// be skipped when there is nothing interesting to say.
    Message { channel: String, parent: String },
}

impl Answer {
    /// The block the agent's caret goes on while it works, when the request
    /// came from a document. A request from a channel has no caret: there is
    /// no text position in a conversation for one to mean anything at.
    fn caret(&self) -> Option<&str> {
        match self {
            Answer::Block { id, .. } => Some(id.as_str()),
            Answer::Message { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The broker socket; `None` resolves the session default.
    pub socket: Option<PathBuf>,
    /// The app whose document this is — its manifest package id, which is
    /// what a launched copy of the app announces.
    ///
    /// **Empty joins the room rather than one app.** The broker's fan-out
    /// narrows to an app only when both ends name one, so a connection
    /// announcing nothing is cluster-scoped and is handed every app's
    /// state. That is what lets the agent enumerate the room instead of a
    /// single document — and it is how it came to be holding the file
    /// manager's state back when holding it was an accident.
    pub app_id: String,
    /// The room (workspace) the document lives in.
    pub cluster: Uuid,
    /// The record the app announced for the document (`file:<path>`), or
    /// `None` for an untitled document.
    pub record: Option<String>,
    /// The agent's name: the caret pill, the history author, and the word
    /// instructions start with.
    pub name: String,
    /// Pause between edits, so people can watch the caret work.
    pub pace: Duration,
    /// Stop after the first instruction has been carried out.
    pub once: bool,
    /// Let a model choose which exported action answers an instruction.
    /// False pins the run to the keyword vocabulary.
    pub model: bool,
    /// How long that call may take before the keyword vocabulary decides
    /// instead. Short on purpose: the work must not wait on the network.
    pub model_timeout: Duration,
    /// The directory the agent is browsing, for the apps whose subject is
    /// the filesystem rather than a document. Its own view state — the
    /// equivalent of the caret it puts on a block — and the only thing it
    /// contributes to those apps' resource trees.
    pub directory: Option<PathBuf>,
    /// What the agent is allowed to do, and to which app.
    ///
    /// A capability set, evaluated by the one permission model qOS has
    /// (`ui::permissions`). It gates the surface twice: an operation the
    /// agent may not perform never becomes an offer, and the same question
    /// is asked again at dispatch, because a survey is a snapshot and
    /// acting on a stale one is exactly the failure a gate exists to
    /// prevent.
    ///
    /// Fail-closed: with no capability the agent enumerates nothing and
    /// performs nothing. It holds no role — an agent is not a person — so
    /// everything it can do is something a person explicitly granted.
    pub permissions: PermissionContext,
    /// How long an addressed line must stop changing before it is read as
    /// a request.
    ///
    /// A person writing `Claude: make this a run sheet` types a colon
    /// several seconds before they type the request, and the agent is
    /// handed a delta per keystroke. Without this it reads the line the
    /// instant the colon lands, finds nothing after the name, and answers
    /// `I can't "" here` -- over the top of a sentence still being written,
    /// which then carries on arriving after the reply. Worse, the line is
    /// marked handled by that reading, so the instruction the person
    /// finishes typing is never looked at again.
    ///
    /// So a line is a request only once it has stopped moving. Long enough
    /// to outlast the pauses inside real typing, short enough that nobody
    /// waits on it.
    pub settle: Duration,
    /// How long to wait for the room to produce something addressed to
    /// this agent before giving up and saying what it did see.
    ///
    /// `Duration::ZERO` waits forever — right for an agent left running
    /// against a room nobody has written an instruction in yet, wrong
    /// for a scripted run, where it turns every way of finding no work
    /// into a hang that names no cause.
    ///
    /// It covers *nothing to do*, not merely *nothing received*.
    /// Bounding only the empty-handed case is what let the demo's agent
    /// sit for two minutes: a peer answered with a page that stopped
    /// above the instruction, and from that moment no deadline applied
    /// to it at all.
    pub await_document: Duration,
}

pub struct Session {
    cfg: Config,
    link: Link,
    /// The document instructions are read from — the one addressed to the
    /// agent. Its app is whichever one claimed the first snapshot that
    /// looked like a page.
    editor: Option<Editor>,
    /// Every *other* app's document the agent is holding, by namespace.
    ///
    /// A room is not one document, and until now everything that was not
    /// the page was discarded. Keeping them is what makes the surface the
    /// room's rather than one app's: an instruction written on a page can
    /// name a file in the folder next to it, because the folder is here
    /// too.
    peers: BTreeMap<&'static str, Editor>,
    /// Decoders for every app the agent speaks to. The door decodes through
    /// this before dispatching, so an action whose type no app registered is
    /// refused rather than run on trust.
    registry: ActionRegistry,
    /// Instruction blocks already acted on, so a slow peer re-sending the
    /// same state does not make the agent do the work twice.
    handled: HashSet<String>,
    /// The addressed line the agent is watching settle: its block, the text
    /// last seen there, and when that text stopped changing. `None` when
    /// nothing is ripening, which is also what keeps the loop quiet.
    pending: Option<(String, String, Instant)>,
    /// Which block each other participant's caret was last seen in.
    ///
    /// A timer asks whether the text has stopped moving, which is a proxy
    /// for the question that actually matters -- whether the person is
    /// still writing this line. Presence answers that one directly, and
    /// it does not get slower because the machine is busy.
    ///
    /// Not used the obvious way round. A caret sitting in the block is
    /// true of somebody mid-sentence *and* of somebody who has finished
    /// and is looking at what they wrote, so it cannot be what forbids
    /// acting -- a person who never presses Enter would then never be
    /// answered at all, which is worse than the fault this fixes. What a
    /// caret is unambiguous about is the *other* direction: once it moves
    /// out of the block, that block is finished. Enter is the usual way
    /// that happens, and it is how both demo scripts end an instruction.
    ///
    /// Keyed by the participant id the broker stamps on presence, holding
    /// the block id from [`block_key`]. An entry disappears when that
    /// participant publishes [`PresencePayload::Left`].
    carets: BTreeMap<String, String>,
    /// Every block a caret has ever been seen in.
    ///
    /// What turns "nobody is in this line" into "somebody has been in this
    /// line and left", which are the two halves of [`Written::Unknown`] and
    /// [`Written::Committed`] and mean opposite things. Without it a writer
    /// who publishes no caret at all — a script, a peer on an older build —
    /// is indistinguishable from one who just finished, and every line they
    /// touch would be read the instant it appeared.
    ///
    /// It also survives the one frame that would otherwise lose the fact:
    /// [`PresencePayload::Left`] says a participant is gone and not where
    /// they went, so the block they were in is remembered here rather than
    /// inferred from an entry that has just been dropped.
    visited: HashSet<String>,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    /// When the agent last asked the room for the document. It asks again
    /// until something that comes back is addressed to it: an app that
    /// was still opening its file when the agent arrived never saw the
    /// first ask, and an app whose own copy stops short will never send
    /// a whole one. On a loaded machine that is the difference between
    /// the beat happening and the beat hanging.
    asked_at: Instant,
    /// How long to leave before asking again. Constant while the agent
    /// holds nothing at all — the room may simply not be up yet, and a
    /// steady beat is what gets the first answer — then doubling up to
    /// [`REASK_CEILING`], so an agent holding a quiet page is not
    /// knocking every three quarters of a second forever.
    reask_every: Duration,
    /// Whether anything in this room has ever been addressed to the
    /// agent. Until something is, holding *a* document proves nothing:
    /// the first peer to answer may be an app still opening its file, or
    /// one whose own replica is missing a change nobody can send it.
    addressed_seen: bool,
    /// Changes the replica is holding but cannot apply, because whatever
    /// they were built on never arrived. Non-zero means the page that
    /// materialises here is provably not the whole page — Automerge
    /// queues a change with unmet dependencies rather than applying it,
    /// so the document stops at the gap however much came after it.
    missing_ancestors: usize,
    /// How the last instruction was decided. The record that keeps a run on
    /// the fallback from being indistinguishable from a live one afterwards.
    last_route: Option<Route>,
    /// How many answers the agent has actually put on the wire.
    ///
    /// Distinct from how many it composed: the gap between the two is an
    /// agent that thinks it replied and a person who saw nothing.
    answers_sent: usize,
    /// What the agent last acted on, and when.
    ///
    /// The one piece of state proximity needs: it is what lets "do that
    /// again" mean something for a short while and nothing afterwards. Set
    /// only when an operation actually succeeded — an attempt that was
    /// refused did not touch anything, so nothing about it should become the
    /// implied subject of the next request.
    last_action: Option<LastAction>,
    /// When something last arrived from the room.
    ///
    /// A room arrives one document at a time, and there is no moment at
    /// which it announces that it is complete. So "nothing here does what
    /// was asked" is a claim about a snapshot, and the snapshot the agent
    /// happens to hold a quarter of a second after joining is not the room.
    last_arrival: Instant,
    /// An addressed line whose refusal is being held back, and when the
    /// holding began. `None` when nothing is being held.
    ///
    /// The line is deliberately *not* marked handled while it sits here: a
    /// request the agent has decided not to answer yet is a request it has
    /// not answered, and consuming it would make the wait permanent.
    ///
    /// The instant is what makes the wait mean something. Holding a refusal
    /// back is only useful if the agent does something with the time, and
    /// what it does is ask the room again — so the clock starts at the ask,
    /// and the refusal is only said once *that* has gone unanswered.
    withheld: Option<(String, Instant)>,
    /// A session's answer to an [`CollabRequest::OpenApp`] it would not carry
    /// out: the application, and why.
    ///
    /// Held rather than acted on where it arrives, because it arrives in the
    /// middle of absorbing the room and the only place it means anything is
    /// the step that asked. Cleared before each ask, so a refusal from an
    /// earlier one can never answer a later one.
    refused_open: Option<(String, String)>,
    /// Whether the last turn carried something out.
    ///
    /// The agent answers one request per turn, deliberately: a run that fans
    /// out over everything outstanding is a run nobody can follow. But a turn
    /// otherwise begins only when something *arrives*, and after acting there
    /// is nothing left to arrive — the agent has just written the last change
    /// in the room and is waiting to be woken by an event it is itself the
    /// cause of.
    ///
    /// So having acted is a reason to look again. It is self-limiting: the
    /// turn that finds nothing more to do sets this false and the loop goes
    /// quiet, which is why "one per turn" survives intact and "the second
    /// request in a conversation" stops depending on somebody else typing.
    acted: bool,
}

/// Whether this instruction is one of the document compositions — the ones
/// defined relative to the line that asked for them.
fn is_recipe(i: &Instruction) -> bool {
    matches!(
        i,
        Instruction::Table
            | Instruction::Caption
            | Instruction::Heading
            | Instruction::RunSheet(_)
    )
}

/// What running a plan came to.
///
/// Only two answers matter to a caller: everything asked for was done, or
/// something was said instead. Both count as having dealt with the request —
/// the distinction is whether there is anything left to add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ran {
    /// Every step was carried out.
    All,
    /// An answer was written instead: a refusal, whole or partial.
    Said,
}

/// What a refusal says when some of the request already happened.
///
/// The whole reason this is composed rather than written at the point of
/// failure: "I couldn't do that" is a true sentence and a misleading one when
/// the document in front of the reader has already changed. They need to know
/// which half stands, and they need it in the same breath — a person who reads
/// only "I couldn't" and then finds an edit will not trust the next answer
/// either.
///
/// Nothing is undone to make the sentence simpler. Reverting a change the
/// agent made in somebody else's document, unasked, is a bigger decision than
/// the one that failed, and it would take with it whatever anybody else typed
/// in between.
fn refusal(name: &str, done: &[String], step: Option<&str>, why: &str) -> String {
    // Nothing happened, so there is nothing to distinguish: the sentence is
    // the same one a request that only ever asked for one thing gets.
    if done.is_empty() {
        return format!("{name}: {why}.");
    }
    let could_not = match step {
        Some(label) => format!("Then {label} — I couldn't: {why}"),
        None => format!("Then I couldn't go on: {why}"),
    };
    format!(
        "{name}: {} — done. {could_not}. What is done stands; the rest did not \
         happen.",
        in_sentence(done)
    )
}

/// A list of things, in the words a person would join them with.
fn in_sentence(items: &[String]) -> String {
    match items.split_last() {
        None => "nothing".to_string(),
        Some((last, [])) => last.clone(),
        Some((last, head)) => format!("{}, then {last}", head.join(", then ")),
    }
}

/// The agent's clock, in the epoch seconds a chat message is timestamped in.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// How often the agent asks again while it is still empty-handed.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// How long an application the agent asked this session to open has to turn
/// up in the room before the agent says it did not.
///
/// It bounds a wait that has a much better reason than most: the agent is not
/// wondering whether an app is here, it has just asked for one, and something
/// is starting a process on its behalf. So this is not the patience a survey
/// needs — it is the length of a cold start, and it is long enough for one on
/// a machine that is busy. Shorter would produce the worst answer available:
/// an agent that gave up on a window that then opened.
///
/// It is bounded at all because the alternative is a run that hangs on an app
/// that will never come, reported to whoever asked as silence. The refusal
/// written when it expires names the application and the wait, so a person
/// reading it knows which of the two happened.
const OPENING: Duration = Duration::from_secs(30);

/// How often the agent asks the room for its documents while an application
/// it opened is starting.
///
/// An app that has just come up announces itself and then waits to be asked;
/// nobody volunteers a whole document. Paced rather than continuous because
/// the answer is a full snapshot per app in the room, and asking at the rate
/// this loop turns would ask a hundred times for one arrival.
const ASK_WHILE_OPENING: Duration = Duration::from_millis(400);

/// The slowest the agent falls back to once it holds a page but has
/// still found nothing in the room addressed to it.
const REASK_CEILING: Duration = Duration::from_secs(8);

/// How long the room must have been quiet before "nothing here does that" is
/// something the agent may say.
///
/// A refusal is a claim about the room, and the room arrives one document at
/// a time with nothing marking the end. An agent that answers the instant it
/// cannot match is answering about whatever happened to have landed — which
/// is how a request naming the file manager came out as "I can't do that
/// here" from an agent that was holding only the page, and would have been
/// holding the folder a fifth of a second later.
///
/// So the shape is the same as [`Config::settle`], and for the same reason: a
/// line is read once it stops changing, and a refusal is said once the room
/// stops arriving. It is not the same duration and not the same setting,
/// because they are answers to different questions — one is about how fast
/// somebody types, this one is about how fast a room comes up.
///
/// It costs nothing when the agent *can* answer: a request that resolves is
/// carried out immediately, and only a refusal waits.
const QUIET_BEFORE_REFUSING: Duration = Duration::from_millis(600);

/// Ask the room for the document: peers already in it answer an `Arrived`
/// with their full state. Safe to repeat — a second copy of a document the
/// agent already holds merges to the same thing, and a copy from a peer
/// with more history than this replica fills in what is missing.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::Presence {
        key: PresenceKey::Workspace {
            id: "word-processor".to_string(),
        },
        payload: PresencePayload::Arrived,
    })
}

/// How long a line somebody is demonstrably still writing may go without
/// changing before it is read anyway.
///
/// The backstop, not the mechanism. It exists for one case: a person who
/// types an instruction, leaves the caret sitting in it, and never commits
/// the line. They have to be answered eventually, so this bounds the wait
/// — and because only a line nobody has finished ever reaches it, it is
/// off the path an ordinary instruction takes. One that ends the way
/// instructions end, with Enter, is acted on the moment the caret leaves
/// and waits for nothing at all.
///
/// Long, therefore, on purpose. What it has to outlast is not the gaps
/// inside typing — the film types at about a seventh of a second a
/// character, so those are nowhere near [`Config::settle`] — but a
/// delivery stall on a machine with three builds running, which is what
/// beat the settle window and produced the refusal this replaces.
const STILL_WRITING_GRACE: Duration = Duration::from_secs(4);

/// Which block a presence frame is about, for the frames that are about a
/// block at all.
///
/// The inverse of [`block_key`], and deliberately written next to it: the
/// word processor publishes a caret under exactly this shape
/// (`block_presence_key`, in its widget), so a key without it is presence
/// about something else — a pointer, a viewport, a page — and says nothing
/// about who is writing what.
fn block_of(key: &PresenceKey) -> Option<&str> {
    match key {
        PresenceKey::Widget { path } => match path.as_slice() {
            [PathSegment::Field(kind), PathSegment::Field(id)] if kind == "wp_block" => {
                Some(id.as_str())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether an addressed line has been written, or is still being written.
///
/// The question the settle timer was standing in for. A timer can only ask
/// whether the text has stopped moving, and a line stops moving for two
/// unrelated reasons: the writer finished, or the writer paused and the
/// delta carrying their next word has not arrived yet. Under load the
/// second is ordinary, and answering it is what wrote a refusal over the
/// top of a sentence still being typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    /// The writer left the block. Enter breaking the paragraph, a click
    /// elsewhere, an arrow out of it — whatever they did, this line is what
    /// they meant to leave behind, so there is nothing left to wait for.
    Committed,
    /// Somebody is demonstrably still in the middle of this line: their
    /// caret is in it, or it breaks off mid-construction. Either way the
    /// sentence in front of the agent is not the one being asked.
    StillWriting,
    /// Nothing says either way. No caret has ever been seen here, and the
    /// line reads as whole. Its having stopped is all there is to go on, so
    /// that is what decides — the behaviour a scripted writer has always
    /// had, unchanged.
    Unknown,
}

/// The rule behind [`Written`], from the three facts it turns on: whether a
/// caret is in the block now, whether one ever was, and what the line says
/// about itself.
///
/// Split out from [`Session::written`] so the whole truth table can be read
/// — and tested — without a room, a broker and a document to stand it up in.
/// The session calls exactly this; there is no second copy of the rule.
fn written_from(
    someone_is_here: bool,
    ever_visited: bool,
    behind_a_writer: bool,
    line: &str,
) -> Written {
    if someone_is_here {
        return Written::StillWriting;
    }
    // A caret that has left settles it outright, and it outranks everything
    // below: the writer moved on, so what is there is what they meant,
    // whatever shape it is left in.
    //
    // Except when this replica is provably behind the person who left. The
    // toolkit publishes a caret while dispatching the keystroke and the text
    // on the idle tick after it (`broadcast_focus_presence` against
    // `broadcast_doc_delta` in `ui::app`), so under load a burst of keys can
    // put a caret on the wire ahead of the words it moved over. Acting then
    // would answer a sentence missing its last word — the same fault as
    // before, arrived at from the opposite direction.
    //
    // What gives it away is where the caret went: a block this replica has
    // never heard of cannot have reached it yet, so its author is ahead of
    // us and their leaving proves nothing yet. Waiting costs only as long as
    // the delta that creates that block, which had to arrive regardless.
    if ever_visited {
        return if behind_a_writer {
            Written::StillWriting
        } else {
            Written::Committed
        };
    }
    // Nobody's caret has ever been here, so all that is left is what the line
    // says about itself. One thing it can say for certain: a line ending in a
    // space is not a finished sentence. That space is the gap before the next
    // word — it is precisely what `Claude: make ` is, and reading it as a
    // request is how the agent came to answer the word `make` while `this a
    // run sheet` was still on its way.
    //
    // Not a guess about grammar, and not a claim to recognise a finished
    // sentence: it recognises one specific thing that is never the end of
    // one. A person who has finished typing an instruction has not left a
    // space hanging off it — and the rare one who has is still answered, by
    // [`STILL_WRITING_GRACE`].
    if line != line.trim_end() {
        return Written::StillWriting;
    }
    Written::Unknown
}

/// How the word processor keys a caret: the block's own presence key.
fn block_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
}

impl Session {
    /// Connect, announce the route, and ask the room for the document.
    pub fn connect(cfg: Config) -> Result<Self> {
        let path = cfg
            .socket
            .clone()
            .unwrap_or_else(crate::wire::default_socket_path);
        let mut link = Link::connect(&path)?;
        // The name here is self-declared, and a self-declared name is
        // spoofable — which is why a *peer's* name is resolved through the
        // room document, under the authority that admitted it, rather than
        // taken from what the peer says it is called. The agent should end
        // up resolving the same way. What keeps this honest meanwhile is
        // that the agent is admitted by a capability its inviter granted,
        // so nothing false is being asserted about who is in the room; the
        // name is a label on a participant that was already let in.
        link.send(&CollabRequest::Hello {
            app_id: cfg.app_id.clone(),
            cluster_id: cfg.cluster,
            record: cfg.record.clone(),
            handle: Some(cfg.name.clone()),
        })?;
        // The late-join handshake: every peer in the room answers an
        // `Arrived` with its full document.
        ask_for_document(&mut link)?;
        tracing::info!(
            socket = %path.display(),
            app = %cfg.app_id,
            room = %cfg.cluster,
            record = ?cfg.record,
            name = %cfg.name,
            "joined the room; waiting for the document"
        );
        let chooser = Chooser::new(cfg.model, cfg.model_timeout);
        // Said once, at the top of the run: an operator should know which
        // path a recording is going to take before the beat, not after.
        tracing::info!(
            live = chooser.is_live(),
            model = crate::model::MODEL,
            timeout_ms = cfg.model_timeout.as_millis() as u64,
            "action choice: {}",
            if chooser.is_live() {
                "a model chooses from the app's exported actions"
            } else {
                "no model available; the keyword vocabulary chooses"
            }
        );
        Ok(Self {
            cfg,
            link,
            editor: None,
            peers: BTreeMap::new(),
            registry: surface::registry(),
            handled: HashSet::new(),
            pending: None,
            carets: BTreeMap::new(),
            visited: HashSet::new(),
            chooser,
            last_route: None,
            last_action: None,
            // Joining is itself the start of the room arriving: peers answer
            // the handshake with their documents, and until that has gone
            // quiet the agent has not seen the room.
            last_arrival: Instant::now(),
            withheld: None,
            refused_open: None,
            acted: false,
            answers_sent: 0,
            asked_at: Instant::now(),
            reask_every: REASK_EVERY,
            addressed_seen: false,
            missing_ancestors: 0,
        })
    }

    /// How many answers this agent has put on the wire.
    pub fn answers_sent(&self) -> usize {
        self.answers_sent
    }

    /// What the agent last acted on, and when.
    ///
    /// The resource half of the address that actually reached dispatch — so a
    /// caller checking after the fact can tell *which thing* was operated on,
    /// rather than inferring it from whatever the reply happens to say.
    pub fn last_action(&self) -> Option<&LastAction> {
        self.last_action.as_ref()
    }

    /// How the last instruction was decided, for a test or an operator
    /// checking after the fact whether a run was live.
    pub fn last_route(&self) -> Option<&Route> {
        self.last_route.as_ref()
    }

    /// Where the agent is looking: the block it has its caret on, and the
    /// directory it is browsing. Nothing here is a screen position.
    pub fn view(&self, focus_block: Option<String>, focus_offset: usize) -> View {
        View {
            target_blocks: focus_block.iter().cloned().collect(),
            focus_block,
            focus_offset,
            directory: self.cfg.directory.clone(),
        }
    }

    /// Every document the agent is holding, with the app that authored it.
    fn held(&self) -> Vec<(&'static AppContract, &automerge::AutoCommit)> {
        self.editor
            .iter()
            .map(|e| (e.contract(), &e.doc))
            .chain(self.peers.values().map(|e| (e.contract(), &e.doc)))
            .collect()
    }

    /// Which apps' state the agent is currently holding, by namespace.
    ///
    /// Exposed because "the agent did nothing" has several causes that look
    /// identical from outside, and "it is not holding that app yet" is one of
    /// them. A caller that can see this can tell waiting from refusing.
    pub fn apps(&self) -> Vec<&'static str> {
        self.held().iter().map(|(c, _)| c.namespace).collect()
    }

    /// The applications the agent could have brought into this room, if it
    /// had been given leave to.
    ///
    /// Said out loud in a refusal, and that is a deliberate exception to the
    /// rule the rest of the surface follows. An operation the agent may not
    /// perform is *absent* — never listed and never mentioned — because the
    /// audience for a list of operations includes a model choosing from it,
    /// and because what an app exports is worth not leaking.
    ///
    /// A refusal is a different audience: the person who asked. "I can't do
    /// that here" is true and useless when the reason is a missing grant — it
    /// sends them looking for a better sentence instead of for the permission
    /// — and this desktop's whole position on agents is that the grant is the
    /// interesting part. Nothing is leaked by saying it either: applications
    /// are not secret, the agent is describing its own capability set to its
    /// own principal, and *what those applications can do* stays exactly as
    /// invisible as before.
    fn not_permitted_to_open(&self) -> Vec<&'static str> {
        surface::CONTRACTS
            .iter()
            .filter(|c| !self.holds(c.namespace))
            .filter(|c| {
                !surface::permitted(
                    &self.cfg.permissions,
                    &surface::open_address(c.namespace),
                )
            })
            .map(|c| c.title)
            .collect()
    }

    /// That list, narrowed to the applications `asked` actually named, as a
    /// clause to hang off the end of a refusal.
    ///
    /// Narrowed, because the unnarrowed version is noise: a request that has
    /// nothing to do with applications would be answered with a list of every
    /// one the agent may not open, on every refusal. What earns a mention is
    /// the request having named it — matched on the app's own title the same
    /// way [`surface::from_verbs`] matches an operation's own label, so this
    /// says "you asked for the photo editor" rather than guessing at what
    /// somebody might have meant.
    ///
    /// Empty when the request named none, which is the common case and reads
    /// exactly as it did before.
    fn could_have_opened(&self, asked: &str) -> String {
        let said = asked.to_lowercase();
        let named: Vec<&'static str> = self
            .not_permitted_to_open()
            .into_iter()
            .filter(|title| {
                // Every significant word of the app's name, so "photo editor"
                // is named by "open the photo editor" and not by "photograph".
                title
                    .split_whitespace()
                    .filter(|w| w.chars().count() >= 4)
                    .all(|w| said.contains(w))
            })
            .collect();
        match named.split_last() {
            None => String::new(),
            Some((last, [])) => format!(
                " {last} isn't open here and I haven't been given leave to \
                 open it."
            ),
            Some((last, head)) => format!(
                " {} and {last} aren't open here and I haven't been given \
                 leave to open them.",
                head.join(", "),
            ),
        }
    }

    /// Whether the agent is holding the state of the app `namespace` names.
    fn holds(&self, namespace: &str) -> bool {
        self.held().iter().any(|(c, _)| c.namespace == namespace)
    }

    /// Everything the room exports right now that the agent may perform.
    ///
    /// Asked, never remembered. Each app builds its own tree from the
    /// document as it stands and the door surveys it, so what comes back is
    /// whatever is open at this moment — and an operation the agent holds no
    /// capability for is absent rather than marked unavailable.
    pub fn offers(&self, view: &View) -> Vec<Offer> {
        surface::offers(&self.held(), view, &self.cfg.permissions)
    }


    /// Changes this replica holds but cannot apply for want of what they
    /// were built on. Non-zero means what the agent can read is short of
    /// what the peer that sent it can read.
    pub fn missing_ancestors(&self) -> usize {
        self.missing_ancestors
    }

    /// Run until the broker goes away — or, with `once`, until the first
    /// instruction has been carried out.
    ///
    /// If nothing addressed to the agent turns up, give up after
    /// `await_document` and say which of the three things went wrong.
    /// Waiting forever was the previous behaviour and it is the wrong
    /// one: every way of finding no work looks identical from outside —
    /// a process that connected, announced, logged that it joined, and
    /// then went quiet. A harness then reports "the agent did not
    /// finish", which names the one component that was working.
    pub fn run(&mut self) -> Result<()> {
        let joined = Instant::now();
        loop {
            if self.step(Duration::from_millis(100))? && self.cfg.once {
                return Ok(());
            }
            if !self.addressed_seen
                && !self.cfg.await_document.is_zero()
                && joined.elapsed() >= self.cfg.await_document
            {
                anyhow::bail!("{}", self.nothing_to_do());
            }
        }
    }

    /// Why there was no work, in terms of the thing that decides it.
    ///
    /// Three different faults present identically as silence and each
    /// sends the reader somewhere else, so they are separated here
    /// rather than left to whoever opens the log.
    fn nothing_to_do(&self) -> String {
        if self.editor.is_none() {
            return self.unanswered();
        }
        let waited = self.cfg.await_document.as_secs();
        let name = &self.cfg.name;
        if self.missing_ancestors > 0 {
            return format!(
                "a document arrived, but nothing in it is addressed to \
                 {name:?} after {waited}s — and it is not the whole \
                 document. It is built on {} ancestor change(s) that were \
                 never sent here, so everything made after them is held \
                 unapplied and the page stops at the gap. Nobody in the \
                 room answered with a copy that has them, which points \
                 upstream of this agent: a change too large to cross \
                 severs every edit made after it, because `shell`'s gossip \
                 publish refuses an app snapshot over \
                 MAX_APP_SNAPSHOT_BYTES rather than truncating it.",
                self.missing_ancestors,
            );
        }
        format!(
            "the whole document arrived, but nothing in it is addressed to \
             {name:?} after {waited}s. An instruction is a paragraph whose \
             first word is this agent's name and a colon — `{name}: turn \
             the bullets into a table`. Check --name against what the line \
             in the document actually says."
        )
    }

    /// Why no document arrived at all, in terms of the thing that
    /// decides it.
    ///
    /// The broker's fan-out matches `(app, room, record)` and the record
    /// is the innermost test — it fails closed both ways, and "no
    /// record" is a value like any other rather than a wildcard. So an
    /// agent told nothing about the record does not join an untitled
    /// conversation *and* a saved one: it joins the untitled one only,
    /// and a word processor with a file open is announcing
    /// `file:<path>` and cannot hear it.
    fn unanswered(&self) -> String {
        let waited = self.cfg.await_document.as_secs();
        let addressed = match self.cfg.record.as_deref() {
            Some(r) => format!("record {r:?}"),
            None => "no record (an untitled document)".to_string(),
        };
        format!(
            "no participant answered with a document in {waited}s. Announced \
             app={:?} room={} and {addressed}. The broker matches all three \
             and drops what does not match, so if the app has a file open \
             its record is `file:<path>` and this agent must be given the \
             same one with --record.",
            self.cfg.app_id, self.cfg.cluster,
        )
    }

    /// Run until `deadline`, then return whether an instruction was
    /// carried out. For tests.
    pub fn run_until(&mut self, deadline: Instant) -> Result<bool> {
        let mut acted = false;
        while Instant::now() < deadline {
            acted |= self.step(Duration::from_millis(50))?;
            if acted && self.cfg.once {
                break;
            }
        }
        Ok(acted)
    }

    /// Take whatever the room sent, and fold it into what the agent holds.
    ///
    /// Separated from [`Self::step`] because absorbing and acting are not the
    /// same turn. A step that has just dispatched an *open* has to keep
    /// listening while the app it asked for starts — that app's state arrives
    /// on this socket like everything else — and it has to do that without
    /// re-entering the half of a turn that reads instructions, or the room
    /// would be answered twice from inside answering it once.
    ///
    /// Returns whether the agent's replicas moved.
    fn absorb_next(&mut self, wait: Duration) -> Result<bool> {
        let mut changed = false;
        match self.link.recv(wait)? {
            Some(CollabEvent::CRDTUpdate { data }) => {
                changed = self.absorb_snapshot(&data)?;
            }
            Some(CollabEvent::CRDTIncremental { data }) => {
                if let Some(ed) = self.editor.as_mut() {
                    changed = ed.merge_incremental(&data)?;
                }
                if changed {
                    self.note_what_is_missing();
                } else {
                    // Not the page — try the room's other documents. A
                    // delta belonging to none of them refers to objects
                    // nobody here has seen, which is not this agent's
                    // business and must not end its run.
                    for ed in self.peers.values_mut() {
                        if ed.merge_incremental(&data)? {
                            changed = true;
                            break;
                        }
                    }
                }
            }
            Some(CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            }) => {
                // A participant arriving after us deserves what we hold.
                if let Some(ed) = self.editor.as_mut() {
                    let full = ed.full();
                    self.link.send(&CollabRequest::SyncCRDT { data: full })?;
                    tracing::debug!(%user_id, "sent our document to a new participant");
                }
            }
            // Where everyone else is writing. Kept because it answers the
            // question a settle timer can only guess at — see [`Written`].
            //
            // Only frames keyed to a block count. A pointer or a viewport
            // says where somebody is looking, which is not the same as
            // where they are typing, and reading one as a caret would move
            // a writer out of their own sentence.
            Some(CollabEvent::RemotePresenceUpdate { user_id, key, payload })
                // Never the agent's own. It puts a caret on the very line
                // it is reading, to make the wait legible, and taking that
                // back as evidence would have it conclude that somebody is
                // still writing the sentence it is holding — waiting on
                // itself, on every instruction. The compositor's broker
                // does not echo a sender its own frames; the toolkit's
                // in-process relay does, and both have to be safe here.
                if user_id != self.cfg.name =>
            {
                match payload {
                    PresencePayload::Caret { .. }
                    | PresencePayload::Selection { .. }
                    | PresencePayload::Typing { .. } => {
                        if let Some(block) = block_of(&key) {
                            self.visited.insert(block.to_string());
                            self.carets.insert(user_id, block.to_string());
                        }
                    }
                    // Gone from wherever they were. Recorded as an absence
                    // rather than ignored: a writer who left is a line
                    // finished, which is the whole point of watching.
                    PresencePayload::Left => {
                        self.carets.remove(&user_id);
                    }
                    _ => {}
                }
            }
            // Not news for the room and not a change to anything: the
            // answer to a request this agent made, kept for the step that
            // made it. See [`Self::open_app`].
            Some(CollabEvent::AppOpenRefused { app_id, why }) => {
                self.refused_open = Some((app_id, why));
            }
            Some(_) | None => {}
        }
        if changed {
            self.last_arrival = Instant::now();
        }
        Ok(changed)
    }

    /// One turn: absorb whatever the room sent, then act if the document
    /// asks. Returns whether an instruction was carried out.
    fn step(&mut self, wait: Duration) -> Result<bool> {
        // Ask again until something in this room is addressed to us — and
        // again while something addressed to us is one the agent cannot yet
        // answer.
        //
        // Holding a document is not the same as holding the page that
        // asks for something. The app that answers first may have been
        // opening its file when the agent arrived, or — as in the demo —
        // be holding a replica missing a change it will never be sent,
        // so its page ends above the instruction. Nobody sends a whole
        // document unprompted, so asking again is the only thing in this
        // loop that can change the answer.
        //
        // The second half of that condition is what makes waiting on a room
        // that is still arriving anything other than waiting: an instruction
        // naming an app whose state has not come would otherwise sit out its
        // patience against a room nobody has asked anything of, and then be
        // refused for exactly the reason it was refused before.
        if (!self.addressed_seen || self.withheld.is_some())
            && self.asked_at.elapsed() >= self.reask_every
        {
            ask_for_document(&mut self.link)?;
            self.asked_at = Instant::now();
            self.reask_every = if self.editor.is_some() {
                (self.reask_every * 2).min(REASK_CEILING)
            } else {
                REASK_EVERY
            };
            tracing::debug!(
                every_ms = self.reask_every.as_millis() as u64,
                "asked the room for the document again"
            );
        }
        let changed = self.absorb_next(wait)?;
        // A line stops changing precisely by nothing further arriving, so
        // waiting for the next delta to re-read it would wait forever. The
        // same is true of a room going quiet, which is what a withheld
        // refusal is waiting on. While either is ripening the turn runs
        // anyway; `recv` has already bounded how often that is.
        if !changed && self.pending.is_none() && self.withheld.is_none() && !self.acted {
            return Ok(false);
        }
        self.acted = self.act()?;
        Ok(self.acted)
    }

    /// Record — and say, when it changes — whether the replica can see
    /// its own history.
    ///
    /// Automerge will not apply a change whose ancestors it lacks; it
    /// queues it. So a replica that missed one change in the middle
    /// materialises as the page *before* that change, however many later
    /// changes it is carrying, and nothing about reading it says so.
    /// That is what the demo's agent read as "the document", and there
    /// was no line anywhere saying the page it held was short.
    fn note_what_is_missing(&mut self) {
        let was = self.missing_ancestors;
        let now = match self.editor.as_mut() {
            Some(ed) => ed.missing_ancestors(),
            None => return,
        };
        self.missing_ancestors = now;
        if now > 0 && now != was {
            tracing::warn!(
                missing = now,
                "this is not the whole document: it is built on change(s) \
                 that were never sent here, so everything after them is \
                 held unapplied and the page stops at the gap — still \
                 asking the room for a copy that has them"
            );
        } else if now == 0 && was > 0 {
            tracing::info!("the missing history arrived; the page is whole again");
        }
    }

    /// Take a peer's whole-document frame, if it is ours to take, and
    /// note afterwards whether what we now hold is whole.
    fn absorb_snapshot(&mut self, data: &[u8]) -> Result<bool> {
        let took = self.take_snapshot(data)?;
        if took {
            self.note_what_is_missing();
        }
        Ok(took)
    }

    /// The taking half of [`Self::absorb_snapshot`].
    ///
    /// A room is not one document. Every participant answers an arrival
    /// with its own state, and the broker hands a frame to any
    /// subscriber whose address it matches — which, for a participant
    /// that announced no app id, is everyone in the room. So the agent
    /// is offered the file manager's state alongside the page it was
    /// invited to, and nothing in the frame says which is which.
    ///
    /// Given a `--record` the address settles it and only one arrives.
    /// Without one, this does what it can from the content: take the
    /// first thing that is a document at all, and if a later one turns
    /// out to be a *different* document — established by its refusing to
    /// merge — prefer whichever of the two is actually asking us for
    /// something.
    ///
    /// Returns whether the agent's replica moved.
    fn take_snapshot(&mut self, data: &[u8]) -> Result<bool> {
        // A document some *other* app in the room authored. Not the page
        // instructions are written on, and not a stray frame either: it is
        // part of the surface the agent can be asked to act on, so it is
        // kept rather than dropped.
        if let Some(contract) = crate::apply::attribute(data) {
            if !crate::apply::looks_like_a_document(data) {
                match self.peers.get_mut(contract.namespace) {
                    Some(ed) => {
                        let took = ed.merge_full_any(data)?;
                        if took {
                            tracing::debug!(
                                app = contract.namespace,
                                bytes = data.len(),
                                "merged another app's state in this room"
                            );
                        }
                        return Ok(took);
                    }
                    None => {
                        let ed =
                            Editor::peer_from_snapshot(contract, data, &self.cfg.name)?;
                        self.peers.insert(contract.namespace, ed);
                        tracing::info!(
                            app = contract.namespace,
                            bytes = data.len(),
                            "holding another app's state in this room"
                        );
                        return Ok(true);
                    }
                }
            }
        }
        let Some(ed) = self.editor.as_mut() else {
            if !crate::apply::looks_like_a_document(data) {
                tracing::debug!(
                    bytes = data.len(),
                    "ignored a peer's state: it is not a document"
                );
                return Ok(false);
            }
            self.editor = Some(Editor::from_snapshot(data, &self.cfg.name)?);
            tracing::info!(bytes = data.len(), "received the document");
            return Ok(true);
        };
        if ed.merge_full(data)? {
            return Ok(true);
        }
        // It did not merge, so it is not this document. Worth having only
        // if it is the one addressed to us and what we hold is not.
        let ours_asks = ed
            .snapshot()
            .map(|s| plan::find_addressed(&plan::blocks(&s), &self.cfg.name).is_some())
            .unwrap_or(false);
        if !ours_asks && Editor::is_addressed_to(data, &self.cfg.name) {
            ed.replace_with(data)?;
            tracing::info!(
                bytes = data.len(),
                "took a different document: this is the one addressed to {}",
                self.cfg.name
            );
            return Ok(true);
        }
        tracing::debug!(
            bytes = data.len(),
            "ignored another document in this room"
        );
        Ok(false)
    }

    /// Look for something addressed to the agent and carry it out.
    ///
    /// Two places, in that order. The document is first because an
    /// instruction written into a page is unambiguous about which page it
    /// means, and because it is what the agent has always answered. The
    /// conversation is second and is the one people actually reach for.
    ///
    /// One instruction per turn either way: a run that fans out over
    /// everything outstanding is a run nobody can follow, and the ordering
    /// people expect is the order they said things in.
    fn act(&mut self) -> Result<bool> {
        if self.act_in_document()? {
            return Ok(true);
        }
        self.act_in_chat()
    }

    /// The agent's own participant id, as the room's roster gives it.
    ///
    /// A mention carries the id the room resolved at send, so "am I the one
    /// mentioned" is only answerable against that same roster. Matching on
    /// the display name here is not a second guess at who was meant — it is
    /// how the agent finds *itself* in a list it did not write, and the id it
    /// finds is then the only thing compared.
    ///
    /// Empty when the room has no agent by this name, which is the honest
    /// answer: nobody can have mentioned a participant the room does not
    /// have, and [`listen::addressed`] treats an empty id as matching
    /// nothing rather than as matching everything.
    fn me_in_chat(&self) -> String {
        self.peers
            .get("chat")
            .map(|ed| chat::doc::ChatDoc::list_users(&ed.doc))
            .unwrap_or_default()
            .into_iter()
            .find(|u| u.is_agent && u.name.eq_ignore_ascii_case(&self.cfg.name))
            .map(|u| u.id)
            .unwrap_or_default()
    }

    /// Every message in the room's conversation, across all its channels.
    fn conversation(&self) -> Vec<chat::model::Message> {
        let Some(ed) = self.peers.get("chat") else {
            return Vec::new();
        };
        chat::doc::ChatDoc::list_channels(&ed.doc)
            .into_iter()
            .flat_map(|c| chat::doc::ChatDoc::list_messages(&ed.doc, &c.id))
            .collect()
    }

    /// Look for something said to the agent in the room's conversation.
    ///
    /// The bar for acting is unchanged from the document path: something has
    /// to be **addressed** to the agent — mentioned, or named. Being near its
    /// work is not an address and never becomes one; what nearness buys is
    /// the *subject* of a request that did not state one, and only for as
    /// long as [`listen::RECENCY_SECS`].
    fn act_in_chat(&mut self) -> Result<bool> {
        if !self.peers.contains_key("chat") {
            return Ok(false);
        }
        let messages = self.conversation();
        let me = self.me_in_chat();
        // What the room already has an answer to, read out of the room. The
        // in-memory set is only a same-turn guard on top of it — it is the
        // document that has to be right after a restart.
        let mut done = listen::answered(&messages, &me);
        done.extend(self.handled.iter().cloned());
        let Some(summons) = listen::next_summons(
            &messages,
            &me,
            &self.cfg.name,
            self.last_action.as_ref(),
            now_secs(),
            &done,
        ) else {
            return Ok(false);
        };
        // A room arrives one document at a time. A summons about an app whose
        // state has not got here yet is one the agent can neither answer nor
        // honestly refuse — it does not know yet, and "I can't act on that"
        // would be a claim about timing dressed up as a claim about the
        // request. Leave it unconsumed and look again next turn.
        //
        // Nothing is lost by waiting: the agent keeps asking the room until
        // something is addressed to it, and `await_document` is what bounds
        // the wait and names the cause if the state never comes.
        if let Some(resource) = summons.subject.resource() {
            let namespace = surface::app_of(resource);
            if !self.holds(namespace) {
                tracing::debug!(
                    %namespace,
                    message = %summons.message,
                    "addressed, but that app's state is not here yet — waiting"
                );
                return Ok(false);
            }
        }
        self.addressed_seen = true;
        self.handled.insert(summons.message.clone());
        tracing::info!(
            message = %summons.message,
            channel = %summons.channel,
            from = %summons.author,
            addressed_by = summons.address.label(),
            subject = summons.subject.label(),
            about = ?summons.subject.resource(),
            asked = %summons.asked,
            "spoken to in the room"
        );
        self.carry_out(summons)
    }

    /// Decide what a summons asks for and do it, answering in its channel.
    fn carry_out(&mut self, summons: Summons) -> Result<bool> {
        let view = self.view(None, 0);
        let offers = self.offers(&view);
        // The subject narrows *what may be chosen*, never what the choice
        // then means. An address the message carried is the resource the
        // agent may act on, so only that resource's operations are offered —
        // which is how "crop this" reaches the right square without anyone
        // parsing "this", and how a request about one thing cannot come out
        // as an operation on another.
        let scoped: Vec<Offer> = match summons.subject.resource() {
            Some(resource) => offers
                .iter()
                .filter(|o| o.address().resource == resource)
                .cloned()
                .collect(),
            None => offers.clone(),
        };
        let to = Answer::Message {
            channel: summons.channel.clone(),
            parent: summons.message.clone(),
        };
        // A subject that exports nothing the agent may perform is refused
        // outright rather than widened back to the room. Falling back to
        // every offer here would answer a request about one resource with an
        // operation on whichever other one happened to match the words.
        if scoped.is_empty() && summons.subject.resource().is_some() {
            let what = summons.subject.resource().unwrap_or_default();
            let reply = format!(
                "{}: I can't act on {what:?} — nothing in this room exports \
                 anything I may do to it.",
                self.cfg.name
            );
            tracing::warn!(%what, "refused: the subject exports nothing available");
            self.reply(&to, &reply)?;
            return Ok(true);
        }
        let Decision { mut plan, route } = self.chooser.choose(&summons.asked, &[], &scoped);
        // A recipe rewrites the run of blocks around the line that asked for
        // it, so it only means anything when the request *is* a line in the
        // document. A request that carried its own subject has already said
        // what it is about, and a recipe is not a reading of it — most often
        // it is a word in the prose colliding with a recipe's keyword, which
        // is how "align centre the Run sheet section" came out as a table.
        //
        // So when the subject is known, decide again from that subject's own
        // operations. Nothing is widened: the candidates are the same scoped
        // set, and an answer that still does not fit is refused below.
        if summons.subject.resource().is_some() && plan.steps.iter().any(|s| is_recipe(&s.instruction))
        {
            tracing::debug!(
                asked = %summons.asked,
                "a recipe cannot answer a request that named its own subject; \
                 deciding again from that subject's operations"
            );
            plan = Plan::from_operations(&summons.asked, &scoped);
        }
        tracing::info!(
            message = %summons.message,
            decided_by = route.label(),
            live = route.is_live(),
            steps = plan.len(),
            ?plan,
            "{}",
            route.detail()
        );
        self.last_route = Some(route);
        // A message is not a line in a document, so there is no place in a
        // page for a composition to act on — `run_plan` says so rather than
        // choosing one. Everything else takes exactly the path a request
        // written into the document takes.
        let single = !plan.is_sequence();
        let only = match plan.steps.first().map(|s| &s.instruction) {
            Some(Instruction::Perform { id, .. }) if single => Some(id.clone()),
            _ => None,
        };
        let ran = self.run_plan(plan, None, &to, &scoped)?;
        // A refusal answers for itself; a success has to be said out loud too.
        // Not politeness: this reply is what records the message as dealt
        // with, so an agent that worked silently would do the same work again
        // after a restart. A sequence has already said so in `run_plan`.
        if let (Some(id), Ran::All) = (only, ran) {
            // Said in the app's words, not the address it was dispatched
            // by: this line is read by a person.
            let did = surface::label_of(&self.offers(&view), &id)
                .map(|l| l.to_lowercase())
                .unwrap_or_else(|| "that".to_string());
            let reply = format!("{}: done — {did}.", self.cfg.name);
            self.reply(&to, &reply)?;
        }
        Ok(true)
    }

    /// Whether `block`, reading `line`, has been written or is still being
    /// written. See [`Written`].
    ///
    /// Two kinds of evidence, and the order between them is the whole rule.
    /// A caret that has left settles it outright — the writer moved on, so
    /// whatever is there is what they meant, whatever it looks like. Only
    /// when nobody has committed does the *shape* of the line get a say.
    fn written(&self, block: &str, line: &str, page: &[Block]) -> Written {
        written_from(
            self.carets.values().any(|b| b == block),
            self.visited.contains(block),
            // Somebody's caret is in a block this replica does not have, so
            // their document is ahead of ours and nothing they have done is
            // safe to read as finished yet.
            self.carets
                .values()
                .any(|b| !page.iter().any(|blk| &blk.id == b)),
            line,
        )
    }

    /// Look for an instruction written into the document and carry it out.
    fn act_in_document(&mut self) -> Result<bool> {
        let Some(ed) = self.editor.as_ref() else {
            return Ok(false);
        };
        let blocks = plan::blocks(&ed.snapshot()?);
        // Who was addressed is decided here, from the document. What to do
        // about it is the only thing a model gets a say in.
        let Some((at, asked)) = plan::find_addressed(&blocks, &self.cfg.name) else {
            return Ok(false);
        };
        // Something in this room is for us; stop knocking on doors. True
        // of a line still being typed as much as a finished one -- being
        // named is being addressed, whatever comes after the colon.
        self.addressed_seen = true;
        let ins_id = blocks[at].id.clone();
        if self.handled.contains(&ins_id) {
            return Ok(false);
        }
        // A name and a colon with nothing after it is not a request. It is
        // what every request looks like on its way to being typed, and
        // answering it would put an `I can't ""` over the top of a
        // sentence the person is still writing.
        //
        // Nothing is consumed here: the block stays unhandled, so the
        // instruction they go on to finish is still there to be read.
        if asked.trim().is_empty() {
            self.pending = None;
            return Ok(false);
        }
        // And a line that is still moving is still being written. Watched
        // by its whole text rather than by the part after the name, so a
        // trailing word or a correction counts as movement.
        let line = blocks[at].text.clone();
        let settled_at = match self.pending.as_ref() {
            Some((id, seen, since)) if *id == ins_id && *seen == line => *since,
            _ => {
                let now = Instant::now();
                tracing::debug!(block = %ins_id, "an addressed line is still being written");
                self.pending = Some((ins_id.clone(), line, now));
                now
            }
        };
        // And whether it has stopped moving is a proxy for whether anyone is
        // still writing it, which is the thing actually being asked. Where
        // the room answers that outright, the timer does not get a vote.
        let written = self.written(&ins_id, &blocks[at].text, &blocks);
        let wait_for = match written {
            // Nothing to wait for: the writer has left this line behind.
            // This is the ordinary path — an instruction ends with Enter,
            // which breaks the paragraph and takes the caret with it — and
            // it is *faster* than waiting out the settle window, which is
            // what used to happen to every instruction including the
            // finished ones.
            Written::Committed => Duration::ZERO,
            // Somebody is mid-sentence. The timer is now only a backstop
            // against a line nobody ever finishes.
            Written::StillWriting => STILL_WRITING_GRACE,
            // Nothing said either way; the line stopping is the only
            // evidence there is, exactly as before.
            Written::Unknown => self.cfg.settle,
        };
        if settled_at.elapsed() < wait_for {
            tracing::trace!(
                block = %ins_id,
                ?written,
                waited_ms = settled_at.elapsed().as_millis() as u64,
                "not yet a finished request"
            );
            return Ok(false);
        }
        // `pending` is deliberately still set here. It is cleared where the
        // instruction is consumed, so a turn that decides not to answer yet
        // does not make the line look freshly typed and start it ripening
        // again from nothing.
        //
        // A request already found unanswerable is not decided again until the
        // room has both stopped arriving and had its chance to answer the ask
        // that the holding sent. Deciding costs a call, and re-deciding every
        // turn against a surface that has not moved would spend a dozen of
        // them to reach the same answer.
        if let Some((held, since)) = self.withheld.as_ref() {
            if held == &ins_id
                && (self.last_arrival.elapsed() < QUIET_BEFORE_REFUSING
                    || since.elapsed() < QUIET_BEFORE_REFUSING)
            {
                return Ok(false);
            }
        }

        // Put the caret on the line before deciding what it asks for.
        //
        // Deciding is a call that takes seconds, and until now nothing in
        // the room changed while it ran: the agent joined, the line was
        // written, and then a silence indistinguishable from an agent that
        // is not working. Its caret arriving on the line it is reading is
        // what makes that wait legible as work.
        //
        // Presence, not an action, and deliberately so — where the agent
        // is looking is view state. It leaves nothing in the document,
        // nothing in the history, and nothing to undo.
        self.link.send(&CollabRequest::Presence {
            key: block_key(&ins_id),
            payload: PresencePayload::Caret {
                pos: blocks[at].text.chars().count(),
            },
        })?;

        // What the room exports right now, from the caret the agent just
        // put down. This is the offer a model chooses from and the floor
        // matches against - asked afresh, never a list kept here.
        let view = self.view(Some(ins_id.clone()), blocks[at].text.chars().count());
        let offers = self.offers(&view);
        tracing::info!(
            block = %ins_id,
            operations = offers.len(),
            apps = self.held().len(),
            "surveyed the room"
        );

        let Decision { plan, route } = self.chooser.choose(&asked, &blocks, &offers);
        tracing::info!(
            block = %ins_id,
            asked = %asked,
            decided_by = route.label(),
            live = route.is_live(),
            steps = plan.len(),
            ?plan,
            "{}",
            route.detail()
        );
        self.last_route = Some(route);

        // Nothing here answers it — but "here" is a room that may still be
        // arriving, and a refusal is a claim about the room. So it is held
        // back until the room has been quiet, and the agent keeps asking
        // meanwhile. Nothing is consumed: if the missing app never comes, the
        // very same refusal is written a moment later, naming the same cause.
        //
        // The alternative is what this replaces, and it was not a race the
        // agent lost occasionally: it was the agent answering a question
        // about a room it had not finished being shown. An instruction naming
        // the file manager, read while only the page had landed, refuses —
        // and with `--once` that refusal is the whole run.
        if let Some(part) = plan.unanswered() {
            let waited = matches!(self.withheld.as_ref(), Some((h, since))
                if h == &ins_id && since.elapsed() >= QUIET_BEFORE_REFUSING);
            if !waited {
                tracing::debug!(
                    block = %ins_id,
                    part = %part,
                    apps = self.held().len(),
                    "nothing here answers that yet — asking the room again \
                     rather than refusing"
                );
                // The ask is the whole point of waiting, and it goes out
                // *now* rather than on the next tick of the re-ask cadence.
                // That cadence backs off to seconds once a page is held, so
                // leaving it to schedule this meant the patience expired
                // before the agent had asked anybody anything — a wait that
                // reached the same answer for the same reason, slower.
                ask_for_document(&mut self.link)?;
                self.asked_at = Instant::now();
                self.reask_every = REASK_EVERY;
                self.withheld = Some((ins_id.clone(), Instant::now()));
                return Ok(false);
            }
            tracing::info!(
                block = %ins_id,
                part = %part,
                apps = self.held().len(),
                "asked the room again and nothing more came; refusing now"
            );
        }
        self.withheld = None;
        self.pending = None;
        self.handled.insert(ins_id.clone());

        let to = Answer::Block {
            id: blocks[at].id.clone(),
            chars: blocks[at].text.chars().count(),
        };
        self.run_plan(plan, Some(at), &to, &offers)?;
        Ok(true)
    }

    /// Perform one operation the room exports, by address.
    ///
    /// Three things have to hold before anything happens, and they are
    /// checked here rather than trusted from the survey the answer came
    /// from: the address has to parse, the agent has to hold a capability
    /// for the app it names, and the document that app authored has to be
    /// one the agent is actually holding. The fourth - that the address
    /// still resolves to a live operation - belongs to
    /// [`Editor::invoke`](crate::apply::Editor::invoke), which rebuilds the
    /// tree before resolving against it.
    ///
    /// Every refusal is written into the document. A run that could not do
    /// what it was asked must not be indistinguishable from one that did.
    /// The refusal is returned rather than written, and that is what makes a
    /// sequence sayable. What else has to be said alongside "I could not do
    /// this" depends on what the caller already did — a first step that
    /// stands, a second that did not happen — and only the caller knows that.
    fn perform(
        &mut self,
        id: &str,
        argument: Option<&str>,
        view: &View,
        caret: Option<&str>,
    ) -> Result<Result<(), String>> {
        // The app's own words for this operation, for anything said out
        // loud. Taken from the same fresh survey the dispatch resolves
        // against, so the sentence and the action cannot disagree.
        let known = self.offers(view);
        let what = surface::label_of(&known, id).map(|l| l.to_lowercase());
        let Some(address) = surface::decode_address(id) else {
            tracing::warn!(%id, "refused: not an address");
            return Ok(Err("I couldn't work out what to act on there".to_string()));
        };
        // Asked again, at dispatch. The survey filtered on it too, but a
        // survey is a snapshot: a capability that lapsed between the offer
        // and now must stop the operation, and a gate that only runs when
        // the list is built is advisory.
        if !surface::permitted(&self.cfg.permissions, &address) {
            tracing::warn!(%id, "refused: no capability for this operation");
            return Ok(Err(match &what {
                Some(w) => format!("I'm not allowed to {w} here"),
                None => "I'm not allowed to do that here".to_string(),
            }));
        }
        // Opening is dispatched from here rather than through the door,
        // because there is no replica for the door to resolve against: the app
        // this names is not in the room, which is the entire reason it was
        // chosen. Everything up to this line is identical to every other
        // operation — the same fresh survey, the same address, the same
        // capability question asked in the same place — and that is the claim
        // being made. Opening an application is an operation like any other;
        // what differs is only where it lands.
        if let Some(app) = surface::opens(&address) {
            let app = app.to_string();
            return self.open_app(&app, what.as_deref());
        }
        let namespace = surface::app_of(&address.resource).to_string();
        // The caret goes on the line that asked, so the room can see which
        // participant is working even for an operation that touches another
        // app's state entirely. A request made in a channel has no line to
        // put it on, and inventing one would put the agent's caret somewhere
        // nobody asked it to be.
        if let Some(focus) = caret.map(str::to_string) {
            self.show_caret(&focus)?;
        }

        // Field borrows, not a `&mut self` helper: the registry and the
        // replicas are disjoint parts of this session and the door needs both
        // at once.
        let outcome = {
            let registry = &self.registry;
            let held = if self
                .editor
                .as_ref()
                .is_some_and(|e| e.contract().namespace == namespace)
            {
                self.editor.as_mut()
            } else {
                self.peers.get_mut(namespace.as_str())
            };
            held.map(|ed| ed.invoke(view, &address, argument, registry))
        };
        let Some(outcome) = outcome else {
            tracing::warn!(%id, %namespace, "refused: that app's state is not here");
            return Ok(Err(match &what {
                Some(w) => format!("I can't {w} — that isn't open here"),
                None => "that isn't open here, so I can't act on it".to_string(),
            }));
        };
        match outcome {
            Ok(Some(delta)) => {
                self.link.send(&CollabRequest::SyncIncremental { data: delta })?;
                // Only now — the operation went through and this resource is
                // what the agent is demonstrably working on. Recording it
                // before the outcome is known would make a refusal set the
                // subject of the next request to something never touched.
                self.last_action =
                    Some(LastAction::new(address.resource.clone(), now_secs()));
                tracing::info!(%id, "performed an operation the room exports");
            }
            Ok(None) => {
                tracing::warn!(%id, "the operation changed nothing");
            }
            Err(e) => {
                tracing::warn!(%id, code = e.code(), "refused: {e}");
                return Ok(Err(format!("I couldn't do that - {e}")));
            }
        }
        std::thread::sleep(self.cfg.pace);
        Ok(Ok(()))
    }

    /// Ask this session to open an application, and wait for it to arrive.
    ///
    /// **Opened means its state is in the room.** Not that a frame was sent,
    /// and not that a process started. The step after this one names the app's
    /// own operations, and those exist only once its resource tree is here to
    /// be surveyed — so an open that returned at the moment of asking would
    /// hand the next step a room that does not contain what it was promised,
    /// and the sequence would refuse for a reason that was about timing.
    ///
    /// That is the same failure `act_in_chat` already declines to make when it
    /// leaves a summons unconsumed rather than answer a question about an app
    /// whose state has not landed. This is the stronger form of it: that path
    /// waits because it cannot tell "not here yet" from "not here", and this
    /// one waits because it just asked for the app itself.
    ///
    /// Three ways out, and each says something different:
    ///
    /// * the app's state arrives — done, and the survey the next step takes
    ///   has its operations in it;
    /// * the session refuses — said in the session's own words, because the
    ///   reason is the session's (see [`CollabEvent::AppOpenRefused`]);
    /// * [`OPENING`] passes — said as what it is, a wait that ran out, naming
    ///   the application and the length of it.
    fn open_app(&mut self, namespace: &str, what: Option<&str>) -> Result<Result<(), String>> {
        let Some(contract) = surface::for_namespace(namespace) else {
            // Unreachable through an offer, which is built from the contracts.
            // Reachable from an address a model composed, which is exactly the
            // input this whole path treats as untrusted.
            tracing::warn!(%namespace, "refused: no application by that name");
            return Ok(Err(
                "I don't know an application by that name".to_string()
            ));
        };
        // Already here. Not an error, and deliberately not a second window:
        // what was asked for is that the app be open, and it is. The survey
        // stops offering this the moment its state lands, so the only way to
        // reach this line is a plan made before it did — which is precisely
        // the race that would otherwise put two copies of an app on somebody's
        // screen.
        if self.holds(namespace) {
            tracing::info!(app = %contract.app_id, "already open in this room");
            return Ok(Ok(()));
        }
        // A refusal to an earlier ask must never answer this one.
        self.refused_open = None;
        self.link.send(&CollabRequest::OpenApp {
            app_id: contract.app_id.to_string(),
        })?;
        tracing::info!(
            app = %contract.app_id,
            "asked this session to open an application, and waiting for it"
        );
        let started = Instant::now();
        let mut asked = Instant::now() - ASK_WHILE_OPENING;
        while started.elapsed() < OPENING {
            self.absorb_next(Duration::from_millis(50))?;
            if let Some((app, why)) = self.refused_open.take() {
                // Only this one. A refusal naming something else belongs to
                // nobody here and must not end this step.
                if app != contract.app_id {
                    continue;
                }
                tracing::warn!(app = %app, "this session refused to open it: {why}");
                return Ok(Err(match what {
                    Some(w) => format!("I can't {w} — {why}"),
                    None => format!("I can't open {} — {why}", contract.title),
                }));
            }
            if self.holds(namespace) {
                tracing::info!(
                    app = %contract.app_id,
                    waited_ms = started.elapsed().as_millis() as u64,
                    "it opened, and its state is here"
                );
                std::thread::sleep(self.cfg.pace);
                return Ok(Ok(()));
            }
            // An app that has just started announces itself and then waits to
            // be asked. Nobody sends a whole document unprompted, so this is
            // the only thing in the loop that can change the answer — the same
            // reason `step` asks again while it is empty-handed.
            if asked.elapsed() >= ASK_WHILE_OPENING {
                ask_for_document(&mut self.link)?;
                asked = Instant::now();
            }
        }
        tracing::warn!(
            app = %contract.app_id,
            "asked for it and nothing arrived within {}s",
            OPENING.as_secs()
        );
        Ok(Err(format!(
            "I asked to open {} and nothing came back within {}s, so I stopped \
             rather than carry on as though it had opened",
            contract.title,
            OPENING.as_secs(),
        )))
    }

    /// What the app `namespace` names now denotes, as something another
    /// operation could be handed.
    ///
    /// Asked of the app's own contract, after the step that changed it. A
    /// step that produced nothing transferable answers `None`, and a step
    /// bound to it is then refused rather than performed with something
    /// invented.
    fn product(&self, namespace: &str, view: &View) -> Option<surface::Product> {
        let (contract, doc) = self
            .held()
            .into_iter()
            .find(|(c, _)| c.namespace == namespace)?;
        (contract.product)(view, doc)
    }

    /// Carry out an instruction's steps, in order, and stop where it cannot.
    ///
    /// Three rules, and each is there because its opposite is a way of
    /// misleading somebody about their own document:
    ///
    /// * **A sequence that cannot be finished is not started.** Every step is
    ///   resolved before the first is dispatched, and a part of the request
    ///   the room cannot answer refuses the whole thing while the document is
    ///   still untouched. Working through the steps and stopping at the one
    ///   that was never going to work leaves changes nobody asked for on their
    ///   own.
    /// * **A step that fails mid-sequence names what did happen.** Once the
    ///   first change has landed it cannot be taken back by refusing — undoing
    ///   somebody else's document is not the agent's to decide — so the answer
    ///   says both halves: what stands, and what did not happen.
    /// * **Every step is checked again at dispatch.** The survey is retaken
    ///   between steps, because the first step changed a document and what the
    ///   room exports is a function of what its documents say.
    ///
    /// `scope` is the survey the request was decided against, and is used only
    /// for the sentence that lists what could have been asked for instead — a
    /// request that named its own subject must not be answered with a list of
    /// everything else in the room.
    fn run_plan(
        &mut self,
        plan: Plan,
        at: Option<usize>,
        to: &Answer,
        scope: &[Offer],
    ) -> Result<Ran> {
        let name = self.cfg.name.clone();
        // Nothing is dispatched yet, so a part that answers to nothing costs
        // only a sentence. Said before the loop and not inside it.
        if plan.is_sequence() {
            if let Some(part) = plan.unanswered() {
                let reply = format!(
                    "{name}: I can't \"{part}\", so I haven't done any of it. {}",
                    describe_offers(scope)
                );
                tracing::warn!(%part, steps = plan.len(), "refused a sequence whole");
                self.reply(to, &reply)?;
                return Ok(Ran::Said);
            }
        }
        let sequence = plan.is_sequence();
        // What the last step left behind, and what the room's own words for
        // the steps already carried out are. Both exist only for the sentence
        // at the end and for binding the next argument; neither is a decision.
        let mut carried: Option<surface::Product> = None;
        let mut done: Vec<String> = Vec::new();

        let total = plan.steps.len();
        for (n, step) in plan.steps.into_iter().enumerate() {
            // Said out loud, per step, because a sequence is the one thing the
            // agent does that a person cannot see the whole of in one place:
            // the work crosses applications, and what ties two windows
            // changing to one instruction is the boundary between the steps.
            // Anything drawing that crossing — a hand-off, a picture
            // travelling from one window to the other — needs to know where
            // one step ended and the next began, and guessing it from the
            // document deltas would be reconstructing what is known here.
            tracing::info!(step = n + 1, of = total, ?step, "step begins");
            // Retaken per step: the step before this one changed a document,
            // and every app builds its resource tree out of what its document
            // says. An address resolved against the survey the plan was made
            // from would be resolved against a page that no longer exists.
            let focus = to.caret().map(str::to_string);
            let view = self.view(focus.clone(), 0);
            let offers = self.offers(&view);

            match step.instruction {
                Instruction::Perform { id, argument } => {
                    let label = surface::label_of(&offers, &id).map(|l| l.to_lowercase());
                    // The binding. An operation that collects content is
                    // handed what the previous step actually produced, and
                    // nothing else is ever put there: not a value from the
                    // request, not one from a model, and not a second reading
                    // of the other app's document.
                    let bound;
                    let argument = if step.carries {
                        let Some(product) = carried.as_ref() else {
                            let reply = refusal(
                                &name,
                                &done,
                                label.as_deref(),
                                "nothing before it produced anything I could place",
                            );
                            tracing::warn!(%id, "refused: the step it carries from produced nothing");
                            self.reply(to, &reply)?;
                            return Ok(Ran::Said);
                        };
                        bound = product.argument().to_string();
                        Some(bound.as_str())
                    } else {
                        argument.as_deref()
                    };
                    match self.perform(&id, argument, &view, focus.as_deref())? {
                        Err(why) => {
                            let reply = refusal(&name, &done, label.as_deref(), &why);
                            self.reply(to, &reply)?;
                            return Ok(Ran::Said);
                        }
                        Ok(()) => {
                            done.push(
                                label.unwrap_or_else(|| "what you asked for".to_string()),
                            );
                            // Only ever from the app this step touched: what
                            // is carried forward is the outcome of the step,
                            // not the state of whichever app happens to hold
                            // something transferable.
                            let namespace = surface::decode_address(&id)
                                .map(|a| surface::app_of(&a.resource).to_string());
                            carried = namespace
                                .clone()
                                .and_then(|ns| self.product(&ns, &self.view(focus.clone(), 0)));
                            tracing::info!(
                                step = n + 1,
                                of = total,
                                app = ?namespace,
                                produced = carried.is_some(),
                                "step ends"
                            );
                        }
                    }
                }
                // Say what could have been asked for instead of naming a fixed
                // vocabulary, because there is no longer a fixed vocabulary to
                // name — an agent that answers "I can do these four things"
                // while holding a hundred addresses is wrong about itself.
                Instruction::Unknown(what) => {
                    let reply = format!(
                        "{name}: I can't \"{what}\" here. {}{}",
                        describe_offers(scope),
                        self.could_have_opened(&what),
                    );
                    self.reply(to, &reply)?;
                    return Ok(Ran::Said);
                }
                // A composition is defined relative to the line that asked for
                // it, so the page is re-read here rather than carried in: an
                // earlier step may have moved everything under it. A request
                // made in a channel is not a line in a document at all, and
                // saying so beats picking a place in the page to rewrite.
                other => {
                    let Some((blocks, at)) = self.page_at(at) else {
                        let reply = format!(
                            "{name}: that one rewrites the text around the line that asks \
                             for it, so ask me for it in the document itself."
                        );
                        self.reply(to, &reply)?;
                        return Ok(Ran::Said);
                    };
                    match other {
                        Instruction::Table => self.make_table(&blocks, at)?,
                        Instruction::Caption => self.caption_image(&blocks, at)?,
                        Instruction::Heading => self.make_heading(&blocks, at)?,
                        Instruction::RunSheet(draft) => {
                            self.write_run_sheet(&blocks, at, &draft)?
                        }
                        Instruction::Perform { .. } | Instruction::Unknown(_) => {
                            unreachable!("matched above")
                        }
                    }
                    done.push("that".to_string());
                    // A composition rewrites text; it leaves nothing another
                    // operation could be handed, and saying so explicitly is
                    // what stops a later step being given the step before
                    // last's result as though it were this one's.
                    carried = None;
                }
            }
        }
        // Only a sequence says so out loud. A single operation is its own
        // answer — the change is on the page — and writing "done" over the
        // line that asked would be noise. Several changes across several
        // applications are nowhere visible together, so there is nothing to
        // read unless it is said.
        if sequence {
            let reply = format!("{name}: done — {}.", in_sentence(&done));
            self.reply(to, &reply)?;
        }
        Ok(Ran::All)
    }

    /// The document and the index of the line that asked, read again now.
    fn page_at(&self, at: Option<usize>) -> Option<(Vec<Block>, usize)> {
        let at = at?;
        let blocks = plan::blocks(&self.editor.as_ref()?.snapshot().ok()?);
        if at >= blocks.len() {
            return None;
        }
        Some((blocks, at))
    }

    /// Answer wherever the request came from.
    ///
    /// Every path that declines routes through here, and that is deliberate:
    /// silence and refusal look identical from outside, and the whole reason
    /// this agent writes its refusals down is that a person who asked it for
    /// something must be able to tell which one happened.
    fn reply(&mut self, to: &Answer, text: &str) -> Result<()> {
        match to {
            Answer::Block { id, chars } => self.edit(
                id,
                wp::splice_text_action(RegionKind::Body, id, 0, *chars, text),
            ),
            Answer::Message { channel, parent } => self.say(channel, Some(parent), text),
        }
    }

    /// Post a message into a channel, as the agent.
    ///
    /// Threaded under `parent` when there is one, which is what makes the
    /// answer findable next to the question *and* what records the question
    /// as dealt with: [`crate::listen::answered`] reads exactly these back.
    /// So an agent that has replied cannot be made to act on the same message
    /// again by a restart, a reload, or a peer re-sending the channel.
    fn say(&mut self, channel: &str, parent: Option<&str>, text: &str) -> Result<()> {
        let me = self.me_in_chat();
        let action = chat::actions::send_message_action(
            channel,
            &format!("m_{}", Uuid::new_v4().simple()),
            &me,
            now_secs(),
            text,
            parent.map(str::to_string),
            None,
            Vec::new(),
        );
        let delta = self
            .peers
            .get_mut("chat")
            .context("no conversation in this room to answer in")?
            .apply_foreign(&action)?;
        match delta {
            Some(data) => {
                self.link.send(&CollabRequest::SyncIncremental { data })?;
                self.answers_sent += 1;
                tracing::info!(%channel, ?parent, "answered in the room");
            }
            // Loud, because this is the shape of the worst bug in this file:
            // an agent that believes it answered, holding the answer, while
            // the person who asked sees nothing at all.
            None => tracing::warn!(
                %channel, ?parent,
                "wrote an answer that produced no change to send — nobody will see it"
            ),
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }


    /// Put the agent's caret on a block, so the room can see it working.
    fn show_caret(&mut self, block: &str) -> Result<()> {
        let caret_pos = self
            .editor
            .as_ref()
            .and_then(|ed| ed.snapshot().ok())
            .and_then(|s| s.blocks.iter().find(|b| b.id == block).map(|b| b.text.len()))
            .unwrap_or(0);
        self.link.send(&CollabRequest::Presence {
            key: block_key(block),
            payload: PresencePayload::Caret { pos: caret_pos },
        })
    }

    /// Bullets above the instruction become a table; the bullets and the
    /// instruction go away.
    fn make_table(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let bullets = plan::bullets_above(blocks, at);
        let ins_id = blocks[at].id.clone();
        if bullets.is_empty() {
            let reply = format!("{}: I looked for bullets above this line and found none.", self.cfg.name);
            let n = blocks[at].text.chars().count();
            return self.edit(&ins_id, wp::splice_text_action(RegionKind::Body, &ins_id, 0, n, &reply));
        }
        let lines: Vec<&str> = bullets.iter().map(|&i| blocks[i].text.as_str()).collect();
        let rows = plan::table_cells(&lines);
        let cols = rows.first().map(Vec::len).unwrap_or(1).max(1);
        let last_bullet = blocks[*bullets.last().expect("non-empty")].id.clone();
        let before: HashSet<String> = plan::table_ids(blocks).into_iter().collect();

        self.edit(&last_bullet, wp::insert_table_action(&last_bullet, rows.len(), cols))?;

        // The table's cells were minted inside the action; read them back.
        let fresh = plan::blocks(&self.editor.as_ref().expect("editor").snapshot()?);
        let table_id = plan::table_ids(&fresh)
            .into_iter()
            .find(|t| !before.contains(t))
            .context("the inserted table's cells were not found")?;
        let cells = plan::cells_of(&fresh, &table_id);
        for (row, col, cell_id) in cells {
            let text = rows
                .get(row as usize)
                .and_then(|r| r.get(col as usize))
                .cloned()
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            self.edit(&cell_id, wp::splice_text_action(RegionKind::Body, &cell_id, 0, 0, &text))?;
        }
        let mut gone: Vec<String> = bullets.iter().map(|&i| blocks[i].id.clone()).collect();
        gone.push(ins_id);
        self.edit(&last_bullet, wp::remove_blocks_action(gone))?;
        Ok(())
    }

    /// Write out a table the model drafted, then whatever it wanted to say
    /// underneath it; the notes it was drawn from and the instruction go
    /// away.
    ///
    /// The division of labour is the whole point of this path. The model
    /// supplied cell text and one line of prose — nothing else, because
    /// nothing else fits through [`plan::TableDraft`]. Which blocks the
    /// table replaces, where it goes, what is removed, and which actions
    /// carry that out are decided here, from the document, and dispatched
    /// through the word processor's exported actions exactly as the
    /// toolbar would. So the model's reading of the notes is visible in
    /// the result while its reach over the document stays nil.
    fn write_run_sheet(
        &mut self,
        blocks: &[Block],
        at: usize,
        draft: &plan::TableDraft,
    ) -> Result<()> {
        let ins_id = blocks[at].id.clone();
        let grid = draft.grid();
        let cols = draft.headers.len();
        let before: HashSet<String> = plan::table_ids(blocks).into_iter().collect();

        // Anchored on the instruction, so the table lands where the line
        // that asked for it was, with or without bullets above it.
        self.edit(
            &ins_id,
            wp::insert_table_action(&ins_id, grid.len(), cols),
        )?;

        let fresh = plan::blocks(&self.editor.as_ref().expect("editor").snapshot()?);
        let table_id = plan::table_ids(&fresh)
            .into_iter()
            .find(|t| !before.contains(t))
            .context("the inserted table's cells were not found")?;
        let cells = plan::cells_of(&fresh, &table_id);
        let last_cell = cells
            .last()
            .map(|c| c.2.clone())
            .context("the inserted table has no cells")?;
        for (row, col, cell_id) in cells {
            let text = grid
                .get(row as usize)
                .and_then(|r| r.get(col as usize))
                .cloned()
                .unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            self.edit(
                &cell_id,
                wp::splice_text_action(RegionKind::Body, &cell_id, 0, 0, &text),
            )?;
        }

        // A table is a flat run of cell blocks, so a paragraph anchored on
        // the last one lands after the table rather than inside it — and
        // it lands last, which is also the order it should be read in.
        if let Some(note) = draft.note_text() {
            self.edit(&last_cell, wp::insert_paragraph_action(&last_cell, note))?;
        }

        let mut gone: Vec<String> = plan::bullets_above(blocks, at)
            .iter()
            .map(|&i| blocks[i].id.clone())
            .collect();
        gone.push(ins_id);
        self.edit(&last_cell, wp::remove_blocks_action(gone))?;
        tracing::info!(
            rows = draft.rows.len(),
            cols,
            noted = draft.note_text().is_some(),
            "wrote a run sheet the model drafted"
        );
        Ok(())
    }

    /// A caption paragraph under the first image; the instruction goes away.
    fn caption_image(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let ins_id = blocks[at].id.clone();
        let Some(img) = plan::first_image(blocks) else {
            let reply = format!("{}: there is no image in this document to caption.", self.cfg.name);
            let n = blocks[at].text.chars().count();
            return self.edit(&ins_id, wp::splice_text_action(RegionKind::Body, &ins_id, 0, n, &reply));
        };
        let img_id = blocks[img].id.clone();
        let figure = plan::first_image(blocks).map(|_| 1).unwrap_or(1);
        self.edit(&img_id, wp::insert_paragraph_action(&img_id, &format!("Figure {figure}")))?;
        self.edit(&img_id, wp::remove_blocks_action(vec![ins_id]))?;
        Ok(())
    }

    /// A structural pass over a rough draft: the first line becomes the
    /// title, each section label under it becomes a heading, and the
    /// instruction goes away. One `SetParagraphStyle` per level, so the
    /// sections land together rather than one flicker at a time.
    fn make_heading(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let ins_id = blocks[at].id.clone();
        let found = plan::structure(blocks, at);
        let Some(title_id) = found.title else {
            return self.edit(&ins_id, wp::remove_blocks_action(vec![ins_id.clone()]));
        };
        self.edit(
            &title_id,
            wp::set_paragraph_style_action(vec![title_id.clone()], "heading", Some(1), None),
        )?;
        if !found.sections.is_empty() {
            let anchor = found.sections[0].clone();
            self.edit(
                &anchor,
                wp::set_paragraph_style_action(found.sections.clone(), "heading", Some(2), None),
            )?;
        }
        tracing::info!(
            sections = found.sections.len(),
            "made the draft a document"
        );
        self.edit(&title_id, wp::remove_blocks_action(vec![ins_id]))?;
        Ok(())
    }

    /// Show the caret on `focus`, apply `action`, broadcast the change, and
    /// pause so the room can see it happen.
    fn edit(&mut self, focus: &str, action: WordProcessorAction) -> Result<()> {
        self.show_caret(focus)?;
        let delta = self
            .editor
            .as_mut()
            .context("no document yet")?
            .apply(&action)?;
        if let Some(data) = delta {
            self.link.send(&CollabRequest::SyncIncremental { data })?;
            tracing::info!(block = %focus, "applied {}", describe(&action));
        } else {
            tracing::warn!(block = %focus, "{} changed nothing", describe(&action));
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }
}

/// What the agent could have been asked for, in the apps' own words.
///
/// Labels, never addresses. This sentence goes into a document somebody is
/// reading: `word-processor.document#insert-table` is how the agent
/// dispatches, and printing it makes a working agent look broken. Two or
/// three, because a person skims a refusal — six operations recited inline
/// is a paragraph that teaches nothing.
fn describe_offers(offers: &[Offer]) -> String {
    let words = surface::in_words(offers, 3);
    if words.is_empty() {
        return "Nothing open here is something I can act on.".to_string();
    }
    format!("I can {words}.")
}

fn describe(action: &WordProcessorAction) -> String {
    match action {
        WordProcessorAction::SpliceText { ins, del, .. } => format!("splice text (+{} -{del})", ins.chars().count()),
        WordProcessorAction::InsertTable { rows, cols, .. } => format!("insert table {rows}x{cols}"),
        WordProcessorAction::InsertParagraph { .. } => "insert paragraph".to_string(),
        WordProcessorAction::RemoveBlocks { blocks } => format!("remove {} block(s)", blocks.len()),
        WordProcessorAction::SetParagraphStyle { kind, .. } => format!("set style {kind}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Written, block_key, block_of, written_from};

    /// The rule the settle window used to stand in for, case by case.
    ///
    /// A table because the cases are what matter, and each one is a different
    /// person: somebody mid-sentence, somebody who pressed Enter, somebody
    /// who typed the whole thing before the agent ever arrived.
    #[test]
    fn who_is_still_writing_this_line() {
        let whole = "Claude: make this a run sheet";
        // Their caret is in it. Whatever the text looks like, they are here.
        assert_eq!(written_from(true, true, false, whole), Written::StillWriting);
        // They left it. That is the commit, and it outranks every other
        // signal — including a trailing space, which is now merely how they
        // happened to leave it.
        assert_eq!(written_from(false, true, false, whole), Written::Committed);
        assert_eq!(
            written_from(false, true, false, "Claude: make "),
            Written::Committed
        );
        // They left it, but they are somewhere this replica has not caught
        // up to. Their caret outran their text, so their leaving says
        // nothing yet.
        assert_eq!(written_from(false, true, true, whole), Written::StillWriting);
        // No caret has ever been here — a script, or a peer that publishes
        // none. A whole-looking line is judged exactly as it always was.
        assert_eq!(written_from(false, false, false, whole), Written::Unknown);
        // The one the live run produced: thirteen characters ending in the
        // space before the next word. Nobody has to be watching for that to
        // be unfinished.
        assert_eq!(
            written_from(false, false, false, "Claude: make "),
            Written::StillWriting
        );
        assert_eq!("Claude: make ".len(), 13, "the signature of the fault");
    }

    /// A block caret key round-trips. The two halves are separate functions
    /// — the agent writes one, the word processor writes the other — and a
    /// drift between them would be silent: the agent would simply never see
    /// anybody typing, and fall back to the timer for ever.
    #[test]
    fn a_block_caret_key_round_trips() {
        assert_eq!(block_of(&block_key("b7")), Some("b7"));
        // Presence about something that is not a block says nothing about
        // who is writing, and must not be read as a caret.
        assert_eq!(
            block_of(&PresenceKey::Workspace {
                id: "word-processor".into()
            }),
            None
        );
    }

    use ui::presence::PresenceKey;
}
