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
use crate::model::{Chooser, Decision, Route};
use crate::plan::{self, Block, Instruction};
use crate::surface::{self, AppContract, Offer, View};
use crate::wire::Link;

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
}

/// How often the agent asks again while it is still empty-handed.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// The slowest the agent falls back to once it holds a page but has
/// still found nothing in the room addressed to it.
const REASK_CEILING: Duration = Duration::from_secs(8);

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
            chooser,
            last_route: None,
            asked_at: Instant::now(),
            reask_every: REASK_EVERY,
            addressed_seen: false,
            missing_ancestors: 0,
        })
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

    /// Everything the room exports right now that the agent may perform.
    ///
    /// Asked, never remembered. Each app builds its own tree from the
    /// document as it stands and the door surveys it, so what comes back is
    /// whatever is open at this moment — and an operation the agent holds no
    /// capability for is absent rather than marked unavailable.
    pub fn offers(&self, view: &View) -> Vec<Offer> {
        surface::offers(&self.held(), view, &self.cfg.permissions)
    }

    /// The editor holding the document `namespace` authored.
    fn editor_for(&mut self, namespace: &str) -> Option<&mut Editor> {
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.contract().namespace == namespace)
        {
            return self.editor.as_mut();
        }
        self.peers.get_mut(namespace)
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

    /// One turn: absorb whatever the room sent, then act if the document
    /// asks. Returns whether an instruction was carried out.
    fn step(&mut self, wait: Duration) -> Result<bool> {
        let mut changed = false;
        // Ask again until something in this room is addressed to us.
        //
        // Holding a document is not the same as holding the page that
        // asks for something. The app that answers first may have been
        // opening its file when the agent arrived, or — as in the demo —
        // be holding a replica missing a change it will never be sent,
        // so its page ends above the instruction. Nobody sends a whole
        // document unprompted, so asking again is the only thing in this
        // loop that can change the answer.
        if !self.addressed_seen && self.asked_at.elapsed() >= self.reask_every {
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
            Some(_) | None => {}
        }
        if !changed {
            return Ok(false);
        }
        self.act()
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
                            Editor::from_snapshot_for(contract, data, &self.cfg.name)?;
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

    /// Look for an instruction and carry it out.
    fn act(&mut self) -> Result<bool> {
        let Some(ed) = self.editor.as_ref() else {
            return Ok(false);
        };
        let blocks = plan::blocks(&ed.snapshot()?);
        // Who was addressed is decided here, from the document. What to do
        // about it is the only thing a model gets a say in.
        let Some((at, asked)) = plan::find_addressed(&blocks, &self.cfg.name) else {
            return Ok(false);
        };
        // Something in this room is for us; stop knocking on doors.
        self.addressed_seen = true;
        let ins_id = blocks[at].id.clone();
        if self.handled.contains(&ins_id) {
            return Ok(false);
        }
        self.handled.insert(ins_id.clone());

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

        let Decision { instruction, route } = self.chooser.choose(&asked, &blocks, &offers);
        tracing::info!(
            block = %ins_id,
            asked = %asked,
            decided_by = route.label(),
            live = route.is_live(),
            ?instruction,
            "{}",
            route.detail()
        );
        self.last_route = Some(route);

        match instruction {
            Instruction::Perform { id, argument } => {
                self.perform(&id, argument.as_deref(), &view, &blocks, at)?
            }
            Instruction::Table => self.make_table(&blocks, at)?,
            Instruction::Caption => self.caption_image(&blocks, at)?,
            Instruction::Heading => self.make_heading(&blocks, at)?,
            Instruction::RunSheet(draft) => self.write_run_sheet(&blocks, at, &draft)?,
            Instruction::Unknown(what) => {
                // Say what could have been asked for instead of naming a
                // fixed vocabulary, because there is no longer a fixed
                // vocabulary to name - and an agent that answers "I can do
                // these four things" while holding a hundred addresses is
                // wrong about itself.
                let reply = format!(
                    "{}: I can't \"{what}\" here. {}",
                    self.cfg.name,
                    describe_offers(&offers)
                );
                self.reply_at(&blocks, at, &reply)?;
            }
        }
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
    fn perform(
        &mut self,
        id: &str,
        argument: Option<&str>,
        view: &View,
        blocks: &[Block],
        at: usize,
    ) -> Result<()> {
        let name = self.cfg.name.clone();
        let Some(address) = surface::decode_address(id) else {
            let reply = format!("{name}: {id:?} is not an address I can act on.");
            return self.reply_at(blocks, at, &reply);
        };
        // Asked again, at dispatch. The survey filtered on it too, but a
        // survey is a snapshot: a capability that lapsed between the offer
        // and now must stop the operation, and a gate that only runs when
        // the list is built is advisory.
        if !surface::permitted(&self.cfg.permissions, &address) {
            let reply = format!(
                "{name}: I don't have permission to {} here.",
                address.action
            );
            tracing::warn!(%id, "refused: no capability for this operation");
            return self.reply_at(blocks, at, &reply);
        }
        let namespace = surface::app_of(&address.resource).to_string();
        // The caret goes on the line that asked, so the room can see which
        // participant is working even for an operation that touches another
        // app's state entirely.
        let focus = blocks[at].id.clone();
        self.show_caret(&focus)?;

        let outcome = {
            let registry = &self.registry;
            self.editor_for(&namespace)
                .map(|ed| ed.invoke(view, &address, argument, registry))
        };
        let Some(outcome) = outcome else {
            let reply = format!(
                "{name}: nothing from {namespace:?} is open in this room, so I can't act on it."
            );
            tracing::warn!(%id, %namespace, "refused: that app's state is not here");
            return self.reply_at(blocks, at, &reply);
        };
        match outcome {
            Ok(Some(delta)) => {
                self.link.send(&CollabRequest::SyncIncremental { data: delta })?;
                tracing::info!(%id, "performed an operation the room exports");
            }
            Ok(None) => {
                tracing::warn!(%id, "the operation changed nothing");
            }
            Err(e) => {
                let reply = format!("{name}: I couldn't do that - {e}.");
                tracing::warn!(%id, code = e.code(), "refused: {e}");
                return self.reply_at(blocks, at, &reply);
            }
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }

    /// Answer in the document, over the line that asked.
    fn reply_at(&mut self, blocks: &[Block], at: usize, reply: &str) -> Result<()> {
        let ins = &blocks[at];
        let n = ins.text.chars().count();
        self.edit(
            &ins.id,
            wp::splice_text_action(RegionKind::Body, &ins.id, 0, n, reply),
        )
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
/// Capped, because a room can export a great many operations and a
/// paragraph is not a catalogue; the count is what makes the truncation
/// honest rather than a shrug.
fn describe_offers(offers: &[Offer]) -> String {
    if offers.is_empty() {
        return "Nothing in this room is exporting anything I may act on.".to_string();
    }
    let shown: Vec<String> = offers
        .iter()
        .take(6)
        .map(|o| format!("{} ({})", o.descriptor.action_label, o.id()))
        .collect();
    let more = offers.len().saturating_sub(shown.len());
    let mut s = format!("I can: {}", shown.join("; "));
    if more > 0 {
        s.push_str(&format!("; and {more} more"));
    }
    s.push('.');
    s
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
