//! One agent session: join the room, hold the replica, act on instructions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, DocumentMutation, PresencePayload,
    RouteSpec,
};
use ui::permissions::PermissionContext;
use ui::presence::{PathSegment, PresenceKey};
use uuid::Uuid;
use word_processor::actions::{self as wp, RegionKind, WordProcessorAction};

use crate::apply::Editor;
use crate::model::{Chooser, Route};
use crate::plan::{self, Block, ConfirmationEvidence, Instruction, Plan};
use crate::surface::{self, AppContract, View};
use crate::wire::Link;

#[derive(Clone, Debug)]
pub struct Config {
    /// Explicit typed Cap'n Proto endpoint for headless diagnostics outside
    /// the launcher. Normal sessions leave this `None` and inherit the fd.
    pub endpoint: Option<PathBuf>,
    /// Certified device credentials for an explicit endpoint. Launcher-bound
    /// sessions leave this `None` and use the inherited identity service.
    pub endpoint_identity: Option<CertifiedEndpointIdentity>,
    /// The app whose document this is — its manifest package id, which is
    /// what a launched copy of the app announces.
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
    /// How long to wait for somebody in the room to answer with the
    /// document before giving up and saying which address went
    /// unanswered. `Duration::ZERO` waits forever — right for an agent
    /// left running against a room whose document nobody has opened yet,
    /// wrong for a scripted run, where it turns a mismatched route into
    /// a hang that names no cause.
    pub await_document: Duration,
    /// How long a paragraph's text must stop changing before it is read as
    /// a finished request. `Duration::ZERO` acts on the first sight of it —
    /// right for a page seeded before the agent joins, wrong for one a
    /// person is actively typing into, where every keystroke would else
    /// race the agent's own read.
    pub settle: Duration,
    /// The directory the agent is browsing, for an app whose subject is
    /// the filesystem rather than a document.
    pub directory: Option<PathBuf>,
    /// What the agent may enumerate and perform. A denied operation is
    /// absent from what it discovers, never merely refused — see
    /// [`crate::surface::offers`].
    pub permissions: PermissionContext,
}

pub struct Session {
    cfg: Config,
    link: Link,
    editor: Option<Editor>,
    /// Every other application's document that has turned up in this room,
    /// keyed by namespace. The room is not one document — a peer that
    /// announced no app id reaches every subscriber — so what an app other
    /// than [`Self::editor`]'s exports has to live somewhere before
    /// [`Self::offers`] can survey it or [`Self::perform`] can act on it.
    peers: BTreeMap<&'static str, Editor>,
    /// Instruction blocks already acted on, so a slow peer re-sending the
    /// same state does not make the agent do the work twice.
    handled: HashSet<String>,
    /// A paragraph currently addressed to the agent, and the text it held
    /// the last time this was checked — the [`Config::settle`] debounce.
    /// Cleared once the paragraph is handled or stops naming the agent.
    watching: HashMap<String, (String, Instant)>,
    /// Addresses already performed in the plan currently running.
    ///
    /// A stalled clause can be asked again and come back as a whole plan of
    /// its own, and that plan can re-name a step this turn already
    /// dispatched — nothing upstream of dispatch is what stops that
    /// happening twice. Cleared at the start of every [`Session::run_plan`],
    /// never across turns: a later turn re-performing the same address is
    /// an ordinary repeat instruction, not re-entrancy.
    dispatched_this_turn: HashSet<String>,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    /// When the agent last asked the room for the document. It asks again
    /// until it has one: an app that was still opening its file when the
    /// agent arrived never saw the first ask, and the agent would then wait
    /// for a reply that is never coming. On a loaded machine that is the
    /// difference between the beat happening and the beat hanging.
    asked_at: Instant,
    /// How the last instruction was decided. The record that keeps a run on
    /// the fallback from being indistinguishable from a live one afterwards.
    last_route: Option<Route>,
    /// Complete, permission-filtered live action directory supplied by the
    /// broker. This is the planner's application catalogue; local document
    /// contracts remain only for the agent's own editorial state/tools.
    broker_surfaces: Vec<ui::collaboration::BrokerActionSurface>,
    next_broker_request: u64,
}

/// How often the agent asks again while it still has no document.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// Ask the room for the document: peers already in it answer an `Arrived`
/// with their full state. Safe to repeat — a second copy of a document the
/// agent already holds merges to the same thing.
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
    ///
    /// # Errors
    ///
    /// When the connection to the compositor's collaboration broker cannot
    /// be made, or the initial route join or presence announce fails to
    /// send.
    pub fn connect(cfg: Config) -> Result<Self> {
        let mut link = match &cfg.endpoint {
            Some(path) => Link::connect_endpoint(
                path,
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.name.clone()),
                cfg.endpoint_identity.clone(),
            )?,
            None => Link::connect(
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.name.clone()),
            )?,
        };
        // Discovery is room-wide, but writes never are. This subscription
        // lets the agent learn other applications' exported operations while
        // every mutation below still names its owning app route explicitly.
        link.join_route(RouteSpec::cluster(cfg.cluster));
        // The late-join handshake: every peer in the room answers an
        // `Arrived` with its full document.
        ask_for_document(&mut link)?;
        tracing::info!(
            transport = if cfg.endpoint.is_some() { "typed-endpoint" } else { "QOS_SVC_COLLAB_FD" },
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
            handled: HashSet::new(),
            watching: HashMap::new(),
            dispatched_this_turn: HashSet::new(),
            chooser,
            last_route: None,
            asked_at: Instant::now(),
            broker_surfaces: Vec::new(),
            next_broker_request: 1,
        })
    }

    /// How the last instruction was decided, for a test or an operator
    /// checking after the fact whether a run was live.
    ///
    /// `None` after a request that read as several things to do: no single
    /// model call decided a sequence, so there is no one [`Route`] to name
    /// without claiming a call happened that did not.
    pub fn last_route(&self) -> Option<&Route> {
        self.last_route.as_ref()
    }

    /// Run until the broker goes away — or, with `once`, until the first
    /// instruction has been carried out.
    ///
    /// If nobody ever answers with a document, give up after
    /// `await_document` and say which address went unanswered. Waiting
    /// forever was the previous behaviour and it is the wrong one: the
    /// commonest cause is a route that does not match, the broker drops
    /// a mismatched frame silently by design, and the symptom is an
    /// agent sitting at 0% forever while every log line says it joined
    /// fine. A harness then reports "the agent did not finish", which
    /// names the one component that was working.
    ///
    /// # Errors
    ///
    /// When no participant answers with the document within
    /// `await_document` (see [`Self::unanswered`] for the message), or when
    /// a turn's own I/O — receiving from the broker, decoding a snapshot,
    /// applying an edit — fails.
    pub fn run(&mut self) -> Result<()> {
        let joined = Instant::now();
        loop {
            if self.step(Duration::from_millis(100))? && self.cfg.once {
                return Ok(());
            }
            if self.editor.is_none()
                && !self.cfg.await_document.is_zero()
                && joined.elapsed() >= self.cfg.await_document
            {
                anyhow::bail!("{}", self.unanswered());
            }
        }
    }

    /// Why no document arrived, in terms of the thing that decides it.
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
    ///
    /// # Errors
    ///
    /// Whenever a turn's own I/O fails — see [`Self::run`].
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
        // Still empty-handed: ask again. An app that was still opening its
        // file when the agent arrived never saw the first ask, and nothing
        // else would ever prompt a reply.
        if self.editor.is_none() && self.asked_at.elapsed() >= REASK_EVERY {
            ask_for_document(&mut self.link)?;
            self.asked_at = Instant::now();
            tracing::debug!("asked the room for the document again");
        }
        match self.link.recv(wait)? {
            Some(CollabEvent::Document { envelope, .. }) => {
                changed = match DocumentMutation::decode(&envelope.payload)? {
                    DocumentMutation::Snapshot(data) => self.absorb_snapshot(&data)?,
                    DocumentMutation::Incremental(data) => {
                        self.merge_incremental_anywhere(&data)?
                    }
                };
            }
            Some(CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            }) => {
                // A participant arriving after us deserves what we hold —
                // every document, not only the one we were invited to.
                if let Some(ed) = self.editor.as_mut() {
                    let full = ed.full();
                    self.link
                        .send(&CollabRequest::DocumentSnapshot { data: full })?;
                }
                for peer in self.peers.values_mut() {
                    let full = peer.full();
                    self.link.send_to_route(
                        RouteSpec::app(peer.contract().app_id, self.cfg.cluster),
                        &CollabRequest::DocumentSnapshot { data: full },
                    )?;
                }
                tracing::debug!(%user_id, peers = self.peers.len(), "sent our documents to a new participant");
            }
            Some(CollabEvent::ActionCatalogue { surfaces }) => {
                self.broker_surfaces = surfaces;
                tracing::debug!(
                    actions = self
                        .broker_surfaces
                        .iter()
                        .map(|s| s.actions.len())
                        .sum::<usize>(),
                    "updated broker action catalogue"
                );
            }
            Some(_) | None => {}
        }
        if !changed {
            return Ok(false);
        }
        self.act()
    }

    /// Take a peer's whole-document frame, if it is ours to take.
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
    fn absorb_snapshot(&mut self, data: &[u8]) -> Result<bool> {
        let Some(ed) = self.editor.as_mut() else {
            if !crate::apply::looks_like_a_document(data) {
                // Not a page this agent authors into — but it may still be
                // another application's room state, which belongs in
                // `peers` rather than being dropped outright.
                return self.absorb_peer(data);
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
            .is_ok_and(|s| plan::find_addressed(&plan::blocks(&s), &self.cfg.name).is_some());
        if !ours_asks && Editor::is_addressed_to(data, &self.cfg.name) {
            ed.replace_with(data)?;
            tracing::info!(
                bytes = data.len(),
                "took a different document: this is the one addressed to {}",
                self.cfg.name
            );
            return Ok(true);
        }
        // Not the page we were invited to and not addressed to us — but
        // the room holds more than the document we were invited to, and an
        // instruction may name an operation that belongs to whatever else
        // is here.
        self.absorb_peer(data)
    }

    /// Take a peer's whole-document frame as a *second* application's
    /// state — one this session was not invited to, but whose operations
    /// [`Self::offers`] can still discover and [`Self::perform`] can still
    /// dispatch, once its own contract recognises it.
    fn absorb_peer(&mut self, data: &[u8]) -> Result<bool> {
        let Some(contract) = crate::apply::attribute(data) else {
            tracing::debug!(
                bytes = data.len(),
                "ignored a peer's state: no app here claims it"
            );
            return Ok(false);
        };
        if let Some(peer) = self.peers.get_mut(contract.namespace) {
            return peer.merge_full_any(data);
        }
        let peer = Editor::peer_from_snapshot(contract, data, &self.cfg.name)?;
        tracing::info!(
            app = contract.namespace,
            bytes = data.len(),
            "a second application's document joined the room"
        );
        self.peers.insert(contract.namespace, peer);
        Ok(true)
    }

    /// Apply a peer's incremental change to whichever replica it belongs
    /// to — the primary document first, since that is the common case,
    /// then each held peer. A delta belonging to a document this session
    /// holds neither of is silently not this agent's business.
    fn merge_incremental_anywhere(&mut self, data: &[u8]) -> Result<bool> {
        if let Some(ed) = self.editor.as_mut()
            && ed.merge_incremental(data)?
        {
            return Ok(true);
        }
        for peer in self.peers.values_mut() {
            if peer.merge_incremental(data)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every document this session holds, primary first, as the pairs
    /// [`surface::offers`] and [`surface::facts`] are built from.
    fn held(&self) -> Vec<(&'static AppContract, &automerge::AutoCommit)> {
        let mut out = Vec::new();
        if let Some(ed) = self.editor.as_ref() {
            out.push((ed.contract(), &ed.doc));
        }
        for peer in self.peers.values() {
            out.push((peer.contract(), &peer.doc));
        }
        out
    }

    /// Prefer broker-routable offers, while retaining the known-document
    /// catalogue until the asynchronous broker snapshot has advertised the
    /// same address. This prevents an instruction arriving with the document
    /// from being permanently refused merely because its catalogue event is
    /// one frame behind it.
    fn offers(
        &self,
        held: &[(&'static AppContract, &automerge::AutoCommit)],
        view: &View,
    ) -> Vec<surface::Offer> {
        let mut offers = surface::broker_offers(&self.broker_surfaces);
        for local in surface::offers(held, view, &self.cfg.permissions) {
            if !offers
                .iter()
                .any(|broker| broker.address() == local.address())
            {
                offers.push(local);
            }
        }
        offers
    }

    /// Where the agent is looking: `focus` on a document, and the
    /// directory it was configured to browse, when it has one.
    fn view(&self, focus: Option<String>) -> View {
        View {
            focus_block: focus.clone(),
            focus_offset: 0,
            target_blocks: focus.into_iter().collect(),
            subject: None,
            directory: self.cfg.directory.clone(),
        }
    }

    /// Perform the discovered operation `id` names, against whichever
    /// document holds it. `Ok(Err(_))` is a refusal in words a person
    /// reads; `Err(_)` is this session's own I/O failing.
    ///
    /// Resolved against a tree rebuilt from the document **right now**,
    /// through [`Editor::invoke`] — never against the survey `id` was
    /// chosen from. An operation that has gone away since is a refusal
    /// here, not a substitution.
    fn perform(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<(), String>> {
        let offers = surface::broker_offers(&self.broker_surfaces);
        if let Some(offer) = surface::find(&offers, id).cloned() {
            return self.perform_broker(id, &offer, arguments, confirmation);
        }
        self.perform_local(id, arguments, confirmation)
    }

    fn perform_broker(
        &mut self,
        id: &str,
        offer: &surface::Offer,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<(), String>> {
        let Some(peer_id) = offer.broker_peer_id else {
            return Ok(Err(
                "that operation is local-only and cannot be brokered".to_string()
            ));
        };
        let descriptor = offer.descriptor.clone();
        if !confirmation.satisfies(&descriptor.contract.confirmation) {
            return Ok(Err("that action requires explicit confirmation".to_string()));
        }
        if let Err(error) = qos_actions::ActionArgs::bind_json(&descriptor.params, arguments) {
            return Ok(Err(format!("I couldn't do that - {error}")));
        }
        let confirmation_json = match confirmation {
            ConfirmationEvidence::None => serde_json::json!({}),
            ConfirmationEvidence::Confirm => serde_json::json!({"confirm": true}),
            ConfirmationEvidence::Phrase(phrase) => {
                serde_json::json!({"confirmation_phrase": phrase})
            }
        };
        let request_id = self.next_request_id();
        if descriptor.resource_kind == surface::APPLICATION_KIND
            && descriptor.address.action == surface::OPEN_ACTION
        {
            self.link.send(&CollabRequest::LaunchPublishedApplication {
                request_id,
                app_id: offer.app.clone(),
            })?;
        } else {
            self.link.send(&CollabRequest::InvokePublishedAction {
                request_id,
                peer_id,
                resource: descriptor.address.resource.clone(),
                action: descriptor.address.action,
                arguments: arguments.clone(),
                confirmation: confirmation_json,
            })?;
        }
        let (invocation_id, mut update) = match self.wait_for_broker_result(request_id)? {
            Ok(receipt) => receipt,
            Err(error) => return Ok(Err(format!("I couldn't do that - {error}"))),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !update.is_terminal() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            let request_id = self.next_request_id();
            self.link.send(&CollabRequest::PublishedInvocationStatus {
                request_id,
                id: invocation_id,
            })?;
            match self.wait_for_broker_result(request_id)? {
                Ok((_, latest)) => update = latest,
                Err(error) => return Ok(Err(format!("I couldn't observe that action - {error}"))),
            }
        }
        if !update.is_terminal() {
            return Ok(Err(format!(
                "action {invocation_id} was admitted but did not reach a terminal state in time"
            )));
        }
        match update {
            qos_actions::InvocationUpdate::Failed { error, .. } => {
                Ok(Err(format!("I couldn't do that - {}", error.message)))
            }
            qos_actions::InvocationUpdate::Cancelled { message, .. } => {
                Ok(Err(message.unwrap_or_else(|| {
                    "that action was cancelled".to_string()
                })))
            }
            qos_actions::InvocationUpdate::Succeeded { output, .. } => {
                tracing::info!(%id, invocation_id, ?output, "broker action completed");
                std::thread::sleep(self.cfg.pace);
                Ok(Ok(()))
            }
            _ => Ok(Err(
                "the broker returned an invalid nonterminal result".to_string()
            )),
        }
    }

    fn perform_local(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmation: &ConfirmationEvidence,
    ) -> Result<Result<(), String>> {
        let Some(address) = surface::decode_address(id) else {
            return Ok(Err("that no longer names anything I can find".to_string()));
        };
        let namespace = surface::app_of(&address.resource).to_string();
        let view = self.view(None);
        let registry = surface::registry();
        let is_primary = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.contract().namespace == namespace);
        let held = if is_primary {
            self.editor.as_mut()
        } else {
            self.peers.get_mut(namespace.as_str())
        };
        let Some(editor) = held else {
            return Ok(Err("that isn't open here, so I can't act on it".to_string()));
        };
        let target_app_id = editor.contract().app_id;
        let live = editor.resources(&view);
        let Some(descriptor) = qos_actions::door::survey(&live)
            .into_iter()
            .find(|descriptor| descriptor.address == address)
        else {
            return Ok(Err("that no longer names anything I can find".to_string()));
        };
        if !surface::permitted_descriptor(&self.cfg.permissions, &descriptor) {
            return Ok(Err("that no longer names anything I can find".to_string()));
        }
        if !confirmation.satisfies(&descriptor.contract.confirmation) {
            return Ok(Err("that action requires explicit confirmation".to_string()));
        }
        let invoked = if confirmation.is_confirmed() {
            editor.invoke_json_confirmed(&view, &address, arguments, &registry)
        } else {
            editor.invoke_json(&view, &address, arguments, &registry)
        };
        match invoked {
            Ok(Some(delta)) => {
                let request = CollabRequest::DocumentIncremental { data: delta };
                if is_primary {
                    self.link.send(&request)?;
                } else {
                    self.link
                        .send_to_route(RouteSpec::app(target_app_id, self.cfg.cluster), &request)?;
                }
                tracing::info!(%id, "performed a discovered operation");
                std::thread::sleep(self.cfg.pace);
                Ok(Ok(()))
            }
            Ok(None) => {
                tracing::warn!(%id, "the operation changed nothing");
                Ok(Err("that didn't change anything".to_string()))
            }
            Err(error) => {
                tracing::warn!(%id, "refused: {error}");
                Ok(Err(format!("I couldn't do that - {error}")))
            }
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_broker_request;
        self.next_broker_request = self.next_broker_request.saturating_add(1);
        id
    }

    fn wait_for_broker_result(
        &mut self,
        request_id: u64,
    ) -> Result<Result<(u64, qos_actions::InvocationUpdate), String>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut deferred = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.link.defer(deferred);
                return Ok(Err(
                    "the broker did not answer the action request".to_string()
                ));
            }
            match self
                .link
                .recv((deadline - now).min(Duration::from_millis(100)))?
            {
                Some(CollabEvent::PublishedInvocationResult {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return Ok(result);
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.broker_surfaces = surfaces;
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
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
        let Some(instruction_block) = blocks.get(at) else {
            return Ok(false);
        };
        let ins_id = instruction_block.id.clone();
        if self.handled.contains(&ins_id) {
            return Ok(false);
        }
        if !Self::settled(
            self.watching.get(&ins_id),
            &instruction_block.text,
            Instant::now(),
            self.cfg.settle,
        ) {
            self.watching
                .insert(ins_id, (instruction_block.text.clone(), Instant::now()));
            return Ok(false);
        }
        self.watching.remove(&ins_id);
        self.handled.insert(ins_id.clone());

        let plan = self.decide(&ins_id, &asked, &blocks);
        self.run_plan(&ins_id, &plan)
    }

    /// Whether a paragraph's text has stopped changing for long enough to
    /// be read as a finished request, rather than a line still being
    /// typed.
    ///
    /// A pure function of what was seen last time and what is seen now, so
    /// the debounce is checkable against an injected clock rather than a
    /// real wait — the same reason [`crate::model::while_busy`] takes its
    /// clock as an argument. `watched` is `None` the first time this text
    /// is seen at all, which is never settled: there is nothing yet to
    /// compare it against.
    fn settled(
        watched: Option<&(String, Instant)>,
        text: &str,
        now: Instant,
        settle: Duration,
    ) -> bool {
        if settle.is_zero() {
            return true;
        }
        match watched {
            Some((seen, since)) if seen == text => now.saturating_duration_since(*since) >= settle,
            _ => false,
        }
    }

    /// Decide what `asked` asks for.
    ///
    /// A request written as one thing goes through the model exactly as
    /// before — the model answers one catalogue id for one instruction, and
    /// is not yet asked about a sequence. When that lands on nothing in the
    /// fixed catalogue, or the request is written as several things
    /// ([`plan::clauses`] finds an ordering word outside any quotation),
    /// the deterministic floor gets a turn against whatever the room's
    /// applications currently export — the model has no say in either of
    /// those, only in the single catalogue request that never needed it.
    fn decide(&mut self, ins_id: &str, asked: &str, blocks: &[Block]) -> Plan {
        let held = self.held();
        let view = self.view(Some(ins_id.to_string()));
        let catalogue = surface::Catalogue {
            offers: self.offers(&held, &view),
            facts: surface::facts(&held, &view),
            held: held
                .iter()
                .map(|(contract, _)| contract.namespace)
                .collect(),
        };
        let offers = &catalogue.offers;
        let decision = self.chooser.choose_actions(asked, blocks, &catalogue, 8);
        let (plan, route) = match decision {
            Ok(answer) => {
                let mut steps = Vec::new();
                let mut invalid = None;
                for (id, arguments) in answer.actions.iter().zip(&answer.arguments) {
                    match Instruction::from_answer(id, asked, Some(arguments), offers) {
                        Ok(instruction) => steps.push(plan::Step::bound(instruction, offers)),
                        Err(error) => {
                            invalid = Some(format!("{error:?}"));
                            break;
                        }
                    }
                }
                if let Some(error) = invalid {
                    (
                        Plan::from_floor(asked, offers),
                        Route::Keywords(crate::model::Fallback::OutOfCatalogue(error)),
                    )
                } else {
                    if let Some(then) = answer.then {
                        steps.push(plan::Step::own(Instruction::Unknown(then)));
                    }
                    (Plan { steps }, Route::Model { why: answer.why })
                }
            }
            Err(route) => (Plan::from_floor(asked, offers), route),
        };
        tracing::info!(
            block = %ins_id,
            asked = %asked,
            steps = plan.len(),
            offers = offers.len(),
            decided_by = route.label(),
            live = route.is_live(),
            "{}",
            route.detail()
        );
        self.last_route = Some(route);
        plan
    }

    /// Carry out `plan`'s steps in order, against the instruction paragraph
    /// `ins_id`.
    ///
    /// A plan that cannot be finished is refused whole, before anything is
    /// touched — [`Plan::unanswered`] is the check, and the paragraph is
    /// left holding the refusal rather than a half-finished document.
    /// Otherwise each step reads a fresh snapshot, because the step before
    /// it may have changed the document; the instruction paragraph itself
    /// is removed once, at the end, so a step that needs to see it (`table`
    /// reads the bullets *above* it) can still do so wherever it falls in
    /// the sequence.
    fn run_plan(&mut self, ins_id: &str, plan: &Plan) -> Result<bool> {
        self.dispatched_this_turn.clear();
        let blocks = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
        if let Some(what) = plan.unanswered() {
            tracing::warn!(%what, steps = plan.len(), "refused a sequence whole; nothing was touched");
            self.splice_reply(
                &blocks,
                ins_id,
                &format!(
                    "{}: I can't \"{what}\", so I haven't done any of it.",
                    self.cfg.name
                ),
            )?;
            return Ok(true);
        }
        for step in &plan.steps {
            let blocks = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
            let Some(at) = blocks.iter().position(|b| b.id == ins_id) else {
                // A step earlier in this very plan already answered the
                // instruction and took the paragraph with it.
                break;
            };
            let applied = match &step.instruction {
                Instruction::Table => self.make_table(&blocks, at)?,
                Instruction::Caption => self.caption_image(&blocks, at)?,
                Instruction::Heading => {
                    self.make_heading(&blocks, at)?;
                    true
                }
                // A surveyed operation, named by address. Guarded by
                // `dispatched_this_turn`: a clause asked again mid-plan can
                // come back naming a step this turn already performed, and
                // without the guard "put it in the open document" placed
                // the same image twice.
                Instruction::Perform {
                    id,
                    arguments,
                    confirmation,
                } => {
                    if self.dispatched_this_turn.contains(id) {
                        true
                    } else {
                        let id = id.clone();
                        match self.perform(&id, arguments, confirmation)? {
                            Ok(()) => {
                                self.dispatched_this_turn.insert(id);
                                true
                            }
                            Err(why) => {
                                let blocks = plan::blocks(
                                    &self.editor.as_ref().context("no document")?.snapshot()?,
                                );
                                self.splice_reply(
                                    &blocks,
                                    ins_id,
                                    &format!("{}: {why}", self.cfg.name),
                                )?;
                                false
                            }
                        }
                    }
                }
                Instruction::Unknown(what) => {
                    self.splice_reply(
                        &blocks,
                        ins_id,
                        &format!("{}: I can't \"{what}\".", self.cfg.name),
                    )?;
                    false
                }
            };
            if !applied {
                return Ok(true);
            }
        }
        // Every step ran clean, so the instruction is answered in full and
        // the paragraph that asked for it goes away. Idempotent against a
        // step that already removed it (a plan of one, as before).
        if plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?)
            .iter()
            .any(|b| b.id == ins_id)
        {
            self.edit(ins_id, &wp::remove_blocks_action(vec![ins_id.to_string()]))?;
        }
        Ok(true)
    }

    /// Replace the instruction paragraph's current text with `reply` — an
    /// answer left for somebody to read, not a further action.
    fn splice_reply(&mut self, blocks: &[Block], ins_id: &str, reply: &str) -> Result<()> {
        let n = blocks
            .iter()
            .find(|b| b.id == ins_id)
            .map_or(0, |b| b.text.chars().count());
        self.edit(
            ins_id,
            &wp::splice_text_action(RegionKind::Body, ins_id, 0, n, reply),
        )
    }

    /// Bullets above the instruction become a table. Returns whether it did
    /// — `false` when there were none to work with, in which case a reply
    /// saying so was written in place of doing anything.
    fn make_table(&mut self, blocks: &[Block], at: usize) -> Result<bool> {
        let bullets = plan::bullets_above(blocks, at);
        if bullets.is_empty() {
            let reply = format!(
                "{}: I looked for bullets above this line and found none.",
                self.cfg.name
            );
            let Some(instruction_block) = blocks.get(at) else {
                return Ok(false);
            };
            self.splice_reply(blocks, &instruction_block.id, &reply)?;
            return Ok(false);
        }
        let lines: Vec<&str> = bullets
            .iter()
            .filter_map(|&i| blocks.get(i).map(|b| b.text.as_str()))
            .collect();
        let rows = plan::table_cells(&lines);
        let cols = rows.first().map_or(1, Vec::len).max(1);
        let Some(last_bullet_block) = bullets.last().and_then(|&i| blocks.get(i)) else {
            // `bullets` was already shown non-empty above.
            return Ok(false);
        };
        let last_bullet = last_bullet_block.id.clone();
        let before: HashSet<String> = plan::table_ids(blocks).into_iter().collect();

        self.edit(
            &last_bullet,
            &wp::insert_table_action(&last_bullet, rows.len(), cols),
        )?;

        // The table's cells were minted inside the action; read them back.
        let fresh = plan::blocks(&self.editor.as_ref().context("no document")?.snapshot()?);
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
            self.edit(
                &cell_id,
                &wp::splice_text_action(RegionKind::Body, &cell_id, 0, 0, &text),
            )?;
        }
        let gone: Vec<String> = bullets
            .iter()
            .filter_map(|&i| blocks.get(i).map(|b| b.id.clone()))
            .collect();
        self.edit(&last_bullet, &wp::remove_blocks_action(gone))?;
        Ok(true)
    }

    /// A caption paragraph under the first image. Returns whether there was
    /// one — `false` when there was nothing to caption, in which case a
    /// reply saying so was written in place of doing anything.
    fn caption_image(&mut self, blocks: &[Block], at: usize) -> Result<bool> {
        let Some(img) = plan::first_image(blocks) else {
            let reply = format!(
                "{}: there is no image in this document to caption.",
                self.cfg.name
            );
            let Some(instruction_block) = blocks.get(at) else {
                return Ok(false);
            };
            self.splice_reply(blocks, &instruction_block.id, &reply)?;
            return Ok(false);
        };
        let Some(img_block) = blocks.get(img) else {
            return Ok(false);
        };
        let img_id = img_block.id.clone();
        self.edit(&img_id, &wp::insert_paragraph_action(&img_id, "Figure 1"))?;
        Ok(true)
    }

    /// A structural pass over a rough draft: the first line becomes the
    /// title, each section label under it becomes a heading. One
    /// `SetParagraphStyle` per level, so the sections land together rather
    /// than one flicker at a time. An empty draft — nothing to find a
    /// title in — leaves the document untouched; this step always counts
    /// as done either way.
    fn make_heading(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let found = plan::structure(blocks, at);
        let Some(title_id) = found.title else {
            return Ok(());
        };
        self.edit(
            &title_id,
            &wp::set_paragraph_style_action(vec![title_id.clone()], "heading", Some(1), None),
        )?;
        if let Some(anchor) = found.sections.first().cloned() {
            self.edit(
                &anchor,
                &wp::set_paragraph_style_action(found.sections.clone(), "heading", Some(2), None),
            )?;
        }
        tracing::info!(sections = found.sections.len(), "made the draft a document");
        Ok(())
    }

    /// Show the caret on `focus`, apply `action`, broadcast the change, and
    /// pause so the room can see it happen.
    fn edit(&mut self, focus: &str, action: &WordProcessorAction) -> Result<()> {
        let caret_pos = self
            .editor
            .as_ref()
            .and_then(|ed| ed.snapshot().ok())
            .and_then(|s| {
                s.blocks
                    .iter()
                    .find(|b| b.id == focus)
                    .map(|b| b.text.len())
            })
            .unwrap_or(0);
        self.link.send(&CollabRequest::Presence {
            key: block_key(focus),
            payload: PresencePayload::Caret { pos: caret_pos },
        })?;
        let delta = self
            .editor
            .as_mut()
            .context("no document yet")?
            .apply(action)?;
        if let Some(data) = delta {
            self.link
                .send(&CollabRequest::DocumentIncremental { data })?;
            tracing::info!(block = %focus, "applied {}", describe(action));
        } else {
            tracing::warn!(block = %focus, "{} changed nothing", describe(action));
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }
}

fn describe(action: &WordProcessorAction) -> String {
    match action {
        WordProcessorAction::SpliceText { ins, del, .. } => {
            format!("splice text (+{} -{del})", ins.chars().count())
        }
        WordProcessorAction::InsertTable { rows, cols, .. } => {
            format!("insert table {rows}x{cols}")
        }
        WordProcessorAction::InsertParagraph { .. } => "insert paragraph".to_string(),
        WordProcessorAction::RemoveBlocks { blocks } => format!("remove {} block(s)", blocks.len()),
        WordProcessorAction::SetParagraphStyle { kind, .. } => format!("set style {kind}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_settle_never_waits() {
        let now = Instant::now();
        assert!(Session::settled(None, "Scribe: table", now, Duration::ZERO));
        assert!(Session::settled(
            Some(&("Scribe: table".to_string(), now)),
            "Scribe: table",
            now,
            Duration::ZERO
        ));
    }

    #[test]
    fn the_first_sight_of_a_line_is_never_settled() {
        let now = Instant::now();
        assert!(!Session::settled(
            None,
            "Scribe: table",
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn a_line_that_changed_since_it_was_last_seen_is_not_settled() {
        let now = Instant::now();
        let watched = ("Scribe: tab".to_string(), now);
        assert!(!Session::settled(
            Some(&watched),
            "Scribe: table",
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn unchanged_text_settles_once_the_wait_has_elapsed_and_not_before() {
        let settle = Duration::from_millis(500);
        let seen = Instant::now();
        let watched = ("Scribe: table".to_string(), seen);
        assert!(
            !Session::settled(
                Some(&watched),
                "Scribe: table",
                seen + Duration::from_millis(499),
                settle
            ),
            "one millisecond short of the wait must not settle"
        );
        assert!(Session::settled(
            Some(&watched),
            "Scribe: table",
            seen + Duration::from_millis(500),
            settle
        ));
        assert!(Session::settled(
            Some(&watched),
            "Scribe: table",
            seen + Duration::from_secs(5),
            settle
        ));
    }
}
