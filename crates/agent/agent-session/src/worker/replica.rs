//! A record held in a confined app worker, as the supervisor uses it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_actions::ActionOutput;
use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_automerge_isolate::{Applied, Deferred, Dependencies};
use tilo_content::ContentStore;
use tilo_server_protocol::live::LiveAnchor;
use ui::resource::DocumentId;

use super::process::{Lease, Stopped};
use super::proto::{MAX_CONTENT_ITEM_BYTES, Mode, Reply, Request};
use crate::apps::{AppCatalogue, AppError, ReplicaActor, Seed};
use tilo_lock::MutexRecover as _;

/// How often a worker that died is replaced, from what the supervisor holds
/// of its record, before the record is given up on: a document that crashes
/// every worker must not keep starting new ones.
const MAX_REVIVALS: u32 = 2;

/// The most pieces of content the record refers to that are put into its
/// worker before one effect runs.
const REFERENCED_CONTENT_ITEMS: usize = 256;

/// The most bytes of such content put into the worker before one effect
/// runs; the piece that crosses it is the last.
const REFERENCED_CONTENT_BYTES: usize = 256 * 1024 * 1024;

/// The most values of a record's document read looking for the content it
/// refers to.
const MOST_SCANNED_VALUES: usize = 1 << 20;

/// The most values of sealed content a held document's worker is told the stored
/// form of: what a document keeps of the content it names, remembered so a
/// second operation over the same content costs no read of it.
const REMEMBERED_STORED_FORMS: usize = 256;

/// One record of one app, held as a replica of its resource in a confined
/// app worker ([`crate::worker`]): [`crate::Replica`]'s operations, with the
/// app's code in the worker and everything this process signs admitted here
/// first.
///
/// The supervisor keeps its own copy of the record (the *mirror*), built
/// only through `tilo-automerge-isolate` from the same changes the worker
/// receives. Every change the worker reports is admitted to the mirror the
/// same way before it is handed back: a whole document must load, and an
/// increment must apply on top of what the record holds, with nothing
/// missing. A worker that died is replaced from the mirror.
///
/// Lent the workspace's content store ([`Self::lending`]), the replica carries
/// content between the workspace and the worker around each operation: what the
/// operation names, and before an effect what the record refers to, goes
/// in before it runs; what the app stored comes out after, hashed again
/// here before the workspace keeps it.
/// Who a workspace's members are, by what it calls each, as its host reads them now.
type Members = std::sync::Arc<dyn Fn() -> Vec<(tilo_actions::MemberId, String)> + Send + Sync>;

pub struct ConfinedReplica {
    /// What its worker does with it: hold the document, or also show it as a page.
    mode: Mode,
    app_id: String,
    workspace: String,
    member: tilo_actions::MemberId,
    /// JSON [`ReplicaActor`], sent again to a revived worker.
    actor: Mutex<String>,
    record: DocumentId,
    content: Option<ContentStore>,
    /// Who the workspace's members are, read again for each operation.
    members: Option<Members>,
    /// Who the workspace's member list calls its people and agents, read again for each
    /// operation: the same names the person's page is told.
    naming: Option<Members>,
    inner: Mutex<Inner>,
}

struct Inner {
    /// The record's slot in a pooled worker, while it holds the record.
    process: Option<Lease>,
    revivals: u32,
    mirror: AutoCommit,
    /// The record is new here: the whole document is still to be sent.
    fresh: bool,
    /// An operation ran since the last take; the worker may hold changes.
    dirty: bool,
    /// This member's changes taken and put back ([`ConfinedReplica::put_back`]):
    /// the next take returns them first.
    kept: Vec<u8>,
    /// The records the app keeps beside this one that are held with it
    /// (the parts of its chain), each mirrored as this one is. What the
    /// worker writes to them is checked as it is taken (its framing, and that
    /// it builds on what the part holds) and applied, in an isolate and then
    /// here, only once the mirror is read: an action that writes a part is
    /// followed by the next one, rarely by a read of the part here.
    beside: BTreeMap<DocumentId, Deferred>,
    /// An operation ran since the last take of the records beside.
    beside_dirty: bool,
    /// Changes to the documents beside, admitted to their mirrors by a take that failed on a
    /// later one: the next take returns them first. The mirrors and the worker hold them, so
    /// whatever is written there next builds on them: dropped, they were lost, and every later
    /// change to those documents was refused as building on what the workspace never got.
    beside_kept: Vec<(DocumentId, DocumentMutation)>,
    /// Content the worker holds, put again into a worker that replaces it.
    held: BTreeSet<String>,
    /// The record's store asks its workspace's hosts for what it lacks
    /// ([`ConfinedReplica::ask_workspace_for_missing`]), and the records they
    /// answered they hold none of: told again to a worker that replaces it.
    asks_workspace: Option<BTreeMap<String, ui::resource::WorkspaceAnswer>>,
    /// What the worker was told of the workspace unasked ([`ConfinedReplica::workspace_answered`]
    /// for a store that asks nothing): told again to a worker that replaces it, since what
    /// the app keeps for its whole workspace opens only on that word.
    told: BTreeMap<String, ui::resource::WorkspaceAnswer>,
    /// The documents the store asked its workspace for
    /// ([`ConfinedReplica::wanted_resources`]): the app's own that it opens from the workspace,
    /// and holds beside its own once one is handed over.
    asked: BTreeSet<DocumentId>,
    /// The action last found by its address (both as JSON: the address, and the worker's
    /// answer), while nothing asked of the worker since could change that answer
    /// ([`Request::moves_nothing`]). One request of the service asks for the same address
    /// several times over (whether the document offers it, what it is, whether it is an
    /// effect, what its change is called), and each answer costs the worker a look at what
    /// the document publishes, which grows with what it holds.
    found: Option<(String, String)>,
    /// The stored form of sealed content the workspace keeps, by reference: what
    /// an operation names it by when it takes the content by reference
    /// ([`ConfinedReplica::stored_form`]).
    forms: HashMap<String, String>,
    /// The content the document (`None`) and each part held beside it name, with the heads
    /// they were read at ([`referenced_content`]): a document is read again only once it
    /// moved. Reading every value of a chain's parts cost a second for a tracker of a few
    /// thousand issues, before every action, and more with every issue it held.
    named: BTreeMap<Option<DocumentId>, (Vec<automerge::ChangeHash>, BTreeSet<String>)>,
    /// The parts held beside that took changes from the workspace since they were last read
    /// for the content they name. A part's own changes, which the worker wrote, wait
    /// unapplied in its mirror and name only content the worker made and holds; a part is
    /// applied to be read for content only once others wrote to it.
    others: BTreeSet<DocumentId>,
}

/// Whether `document` is one the store asked its workspace for, which this replica may hold
/// beside its own: a document the view opened from the workspace (the folder a Files window
/// shows), or one the workspace answered it holds none of, which the view founded.
fn beside_asked(inner: &Inner, document: &DocumentId) -> bool {
    inner.asked.contains(document)
        || inner
            .asks_workspace
            .as_ref()
            .is_some_and(|answers| answers.contains_key(document.as_str()))
}

impl Inner {
    /// Stop the worker holding the record: it answered in a way no worker
    /// should. Every record in it is given to a worker again.
    fn stop(&mut self) {
        if let Some(mut lease) = self.process.take() {
            lease.stop();
        }
    }
}

/// A worker's reply to a request that only says it was carried out.
fn done(reply: Result<Reply, AppError>) -> Result<(), AppError> {
    match reply? {
        Reply::Done => Ok(()),
        Reply::Failed(reason) => Err(worker_error(reason)),
        _ => Err(worker_error("the worker answered out of turn")),
    }
}

fn worker_error(reason: impl Into<String>) -> AppError {
    AppError::Confinement(reason.into())
}

/// The changes `document` carries past `held`, as one run of change chunks: what a
/// worker holding `held` lacks of it, so a whole document crosses as what it
/// adds. `None` when `held` holds nothing, or holds heads `document` no longer
/// carries (a folded or replaced history), and the whole document must cross.
fn changes_since(document: &mut AutoCommit, held: &mut AutoCommit) -> Option<Vec<u8>> {
    let heads = held.get_heads();
    if heads.is_empty() {
        return None;
    }
    if heads.iter().any(|head| !document.has_change(head)) {
        return None;
    }
    let mut bytes = Vec::new();
    for change in document.get_changes(&heads) {
        bytes.extend_from_slice(change.raw_bytes());
    }
    Some(bytes)
}

impl ConfinedReplica {
    /// Hold a record of `app_id` in `workspace` as `member`, in a new
    /// confined worker: [`crate::Replica::open`].
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        workspace: &str,
        member: &tilo_actions::MemberId,
        actor: &ReplicaActor,
        seed: Seed,
        asks_workspace: bool,
    ) -> Result<Self, AppError> {
        Self::open_in(
            Mode::Replica,
            catalogue,
            (app_id, workspace),
            member,
            actor,
            (seed, asks_workspace),
        )
    }

    fn open_in(
        mode: Mode,
        catalogue: &AppCatalogue,
        (app_id, workspace): (&str, &str),
        member: &tilo_actions::MemberId,
        actor: &ReplicaActor,
        (seed, asks_workspace): (Seed, bool),
    ) -> Result<Self, AppError> {
        let actor =
            serde_json::to_string(actor).map_err(|error| worker_error(error.to_string()))?;
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let (mirror, seed, fresh) = match seed {
            Seed::New => (AutoCommit::new(), None, true),
            // No changes asks the worker for a new record under that id.
            Seed::NewAt(record) => (
                AutoCommit::new(),
                Some((record.as_str().to_owned(), Vec::new())),
                true,
            ),
            Seed::Existing { record, changes } => {
                let mut mirror = AutoCommit::new();
                admit_history(&mut mirror, &changes)?;
                (mirror, Some((record.as_str().to_owned(), changes)), false)
            }
        };
        let expected = seed.as_ref().map(|(record, _)| record.clone());
        let mut process = Lease::new(mode, workspace);
        let reply = process
            .call(&Request::Open {
                app_id: definition.app_id().to_owned(),
                workspace: workspace.to_owned(),
                member: member.clone(),
                actor: actor.clone(),
                seed,
                asks_workspace,
            })
            .map_err(|stopped| worker_error(stopped.to_string()))?;
        let record = match reply {
            Reply::Opened { record } => super::serve::parse_document(&record)
                .filter(|record| expected.as_deref().is_none_or(|id| id == record.as_str()))
                .ok_or_else(|| worker_error("the worker named another record"))?,
            Reply::Failed(reason) => return Err(open_error(&reason, app_id)),
            _ => return Err(worker_error("the worker answered out of turn")),
        };
        let replica = Self {
            mode,
            app_id: definition.app_id().to_owned(),
            workspace: workspace.to_owned(),
            member: member.clone(),
            actor: Mutex::new(actor),
            record,
            content: None,
            members: None,
            naming: None,
            inner: Mutex::new(Inner {
                process: Some(process),
                revivals: 0,
                mirror,
                fresh,
                // An existing record's worker may have brought it up to
                // the app's storage on open: asked once, so the migration
                // reaches the mirror and the workspace.
                dirty: true,
                beside: BTreeMap::new(),
                beside_dirty: false,
                beside_kept: Vec::new(),
                kept: Vec::new(),
                held: BTreeSet::new(),
                asks_workspace: asks_workspace.then(BTreeMap::new),
                told: BTreeMap::new(),
                asked: BTreeSet::new(),
                found: None,
                forms: HashMap::new(),
                named: BTreeMap::new(),
                others: BTreeSet::new(),
            }),
        };
        if fresh {
            // The genesis is admitted now, so the mirror holds the record
            // before anything else is asked of it.
            let mut inner = replica.lock();
            replica.collect(&mut inner)?;
            drop(inner);
        }
        Ok(replica)
    }

    /// Lend the record the workspace's content store: content an operation names
    /// (and, before an effect, content the record refers to) is put into the
    /// worker from it, and content the app stores in the worker is kept in
    /// it, so an export or a look stays in the workspace.
    #[must_use]
    pub fn lending(mut self, content: ContentStore) -> Self {
        self.content = Some(content);
        self
    }

    /// Lend the record the workspace's members, as `engine` holds the workspace's governance: what
    /// an operation binds a typed `@Name` to and names an author by.
    #[must_use]
    pub fn knowing(mut self, engine: std::sync::Arc<tilo_session_client::engine::Engine>) -> Self {
        let workspace = self.workspace.clone();
        self.members = Some(std::sync::Arc::new(move || {
            engine
                .people(&workspace)
                .map(|people| people.into_iter().map(|row| (row.id, row.name)).collect())
                .unwrap_or_default()
        }));
        self
    }

    /// Lend the replica the names the workspace's member list gives its people and agents,
    /// beside those its governance knows ([`Self::knowing`]): what a view this worker renders for
    /// an agent shows its rows' authors by, as the person's page does.
    #[must_use]
    pub fn naming(
        mut self,
        names: std::sync::Arc<dyn Fn() -> Vec<(tilo_actions::MemberId, String)> + Send + Sync>,
    ) -> Self {
        self.naming = Some(names);
        self
    }

    /// What each of the workspace's members is called here: its governance's names, then the
    /// workspace's member list's.
    fn member_names(&self) -> Vec<(tilo_actions::MemberId, String)> {
        let mut names = self.members.as_ref().map(|read| read()).unwrap_or_default();
        if let Some(extra) = &self.naming {
            names.extend(extra());
        }
        names
    }

    /// The members a worker is told of before it performs an action `invoker` asks for: the
    /// workspace's member list's names, and the acting agent's own where the list names it not
    /// — the name its admission gave it, which the person's page is told for the same change.
    /// A name the list holds wins: it is the one every surface shows.
    fn members_for(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
    ) -> Vec<(tilo_actions::MemberId, String)> {
        let mut names = self.member_names();
        let admitted = invoker.and_then(|invoker| {
            Some((invoker.acting.agent_id()?, invoker.name.as_deref()?.trim()))
        });
        if let Some((agent, name)) = admitted
            && !name.is_empty()
            && !names
                .iter()
                .any(|(member, known)| member.id() == agent && !known.trim().is_empty())
        {
            names.push((
                tilo_actions::MemberId::agent(agent.to_owned()),
                name.to_owned(),
            ));
        }
        names
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock_recover()
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Whether an agent holds the record ([`ReplicaActor::Agent`]): only an
    /// agent's renders go to the host's render service.
    pub(crate) fn is_agents(&self) -> bool {
        serde_json::from_str::<ReplicaActor>(&self.actor.lock_recover())
            .is_ok_and(|actor| matches!(actor, ReplicaActor::Agent { .. }))
    }

    /// Collaboration route of this record: [`crate::Replica::wire_document`].
    #[must_use]
    pub fn wire_document(&self) -> &str {
        self.record.route()
    }

    #[must_use]
    pub const fn document_id(&self) -> &DocumentId {
        &self.record
    }

    /// The worker's process id, while one runs.
    #[must_use]
    pub fn worker_id(&self) -> Option<u32> {
        self.lock().process.as_ref().and_then(Lease::worker_id)
    }

    /// One request to the worker, giving the record to a worker again first
    /// when the one holding it stopped. A failed call stops the worker; the
    /// next call gives the record to another.
    fn call(&self, inner: &mut Inner, request: &Request) -> Result<Reply, AppError> {
        // The same address asked for again with nothing between that moves what the
        // document offers: the worker's last answer stands, and the worker is not asked.
        if let Request::Resolve { address } = request
            && let Some((known, answer)) = &inner.found
            && known == address
        {
            return Ok(Reply::Descriptors(answer.clone()));
        }
        // What the operation this request is part of left to collect is
        // still to be collected from the worker the record is given to.
        let pending = (inner.dirty, inner.beside_dirty);
        if inner.process.is_none() {
            self.revive(inner, true)?;
            (inner.dirty, inner.beside_dirty) = pending;
        }
        let began = std::time::Instant::now();
        let mut answer = Self::send(inner, request);
        // Another slot's request stopped the worker this record was in:
        // nothing of this request was sent, and the record is given again
        // from the mirror, which is no fault of the record's.
        if matches!(answer, Err(Stopped::Lost)) {
            inner.process = None;
            self.revive(inner, false)?;
            (inner.dirty, inner.beside_dirty) = pending;
            answer = Self::send(inner, request);
        }
        // The worker was still working on the request past the caller's bound:
        // only its slot is let go, and the document is held again from the
        // mirror at once, which holds nothing of that request. The caller
        // learns which request it was; the document answers the next as usual.
        if matches!(answer, Err(Stopped::Slow)) {
            inner.stop();
            self.revive(inner, false)?;
            (inner.dirty, inner.beside_dirty) = pending;
            return Err(AppError::Reopened {
                request: request.kind().to_owned(),
            });
        }
        // A request that reads the record's view may lay it out, and the view
        // may write as it takes up what changed (a file it now names opened
        // for the first time): what it wrote is collected with the next take.
        if answer.is_ok()
            && matches!(
                request,
                Request::Survey
                    | Request::SurveyRows
                    | Request::SurveyPage { .. }
                    | Request::Resolve { .. }
                    | Request::Wanted
            )
        {
            inner.dirty = true;
            inner.beside_dirty = true;
        }
        match (request, &answer) {
            (Request::Resolve { address }, Ok(Reply::Descriptors(text))) => {
                inner.found = Some((address.clone(), text.clone()));
            }
            _ if request.moves_nothing() => {}
            _ => inner.found = None,
        }
        // What each thing asked of a worker took, beside the service's own line for the
        // whole request: where a request spends its time.
        tracing::debug!(
            app = %self.app_id,
            kind = request.kind(),
            took_us = began.elapsed().as_micros(),
            "app worker: a request answered"
        );
        answer.map_err(|stopped| {
            inner.stop();
            tracing::warn!(app = %self.app_id, record = %self.record.as_str(), reason = %stopped,
                "app worker: a request failed; the worker was stopped");
            worker_error(stopped.to_string())
        })
    }

    fn send(inner: &mut Inner, request: &Request) -> Result<Reply, Stopped> {
        inner
            .process
            .as_mut()
            .ok_or_else(|| Stopped::Failed("no app worker is running".into()))?
            .call(request)
    }

    /// Give the record to a worker again from what the mirror holds of it.
    /// `counted` when the record's own request stopped the last one: a record
    /// that keeps stopping its workers is given up on.
    fn revive(&self, inner: &mut Inner, counted: bool) -> Result<(), AppError> {
        // Another worker answers from here on: it is asked again.
        inner.found = None;
        if counted {
            if inner.revivals >= MAX_REVIVALS {
                return Err(worker_error(
                    "the record's app worker kept stopping; the record is no longer held",
                ));
            }
            inner.revivals = inner.revivals.saturating_add(1);
        }
        let mut process = Lease::new(self.mode, &self.workspace);
        let worker = |stopped: Stopped| worker_error(stopped.to_string());
        let actor = self.actor.lock_recover().clone();
        match process
            .call(&Request::Open {
                app_id: self.app_id.clone(),
                workspace: self.workspace.clone(),
                member: self.member.clone(),
                actor,
                seed: Some((self.record.as_str().to_owned(), vec![inner.mirror.save()])),
                asks_workspace: inner.asks_workspace.is_some(),
            })
            .map_err(worker)?
        {
            Reply::Opened { record } if record == self.record.as_str() => {
                // The records held beside it go back to the new worker as
                // the mirrors hold them.
                for (record, mirror) in &mut inner.beside {
                    let bytes = mirror
                        .settled()
                        .map_err(|error| AppError::Merge(error.to_string()))?
                        .save();
                    match process
                        .call(&Request::ApplyDocument {
                            record: record.as_str().to_owned(),
                            snapshot: true,
                            bytes,
                        })
                        .map_err(worker)?
                    {
                        Reply::Done => {}
                        Reply::Failed(reason) => return Err(worker_error(reason)),
                        _ => return Err(worker_error("the worker answered out of turn")),
                    }
                }
                // The content the last worker held went with it.
                let held = std::mem::take(&mut inner.held);
                if let Some(content) = &self.content {
                    for hash in held {
                        if put_content(&mut process, content, &hash)?.is_some() {
                            inner.held.insert(hash);
                        }
                    }
                }
                // So do what the workspace was asked and answered.
                if let Some(answered) = &inner.asks_workspace {
                    let told = std::iter::once(Request::AskWorkspace).chain(answered.iter().map(
                        |(record, answer)| Request::Answered {
                            record: record.clone(),
                            answer: *answer,
                        },
                    ));
                    for request in told {
                        match process.call(&request).map_err(worker)? {
                            Reply::Done => {}
                            Reply::Failed(reason) => return Err(worker_error(reason)),
                            _ => return Err(worker_error("the worker answered out of turn")),
                        }
                    }
                } else {
                    for (id, answer) in &inner.told {
                        let again = Request::Answered {
                            record: id.clone(),
                            answer: *answer,
                        };
                        match process.call(&again).map_err(worker)? {
                            Reply::Done => {}
                            Reply::Failed(reason) => return Err(worker_error(reason)),
                            _ => return Err(worker_error("the worker answered out of turn")),
                        }
                    }
                }
                inner.process = Some(process);
                inner.dirty = false;
                inner.beside_dirty = false;
                Ok(())
            }
            Reply::Failed(reason) => Err(worker_error(reason)),
            _ => Err(worker_error("the worker named another record")),
        }
    }

    /// Take the record's store to come from its workspace's hosts:
    /// [`crate::Replica::ask_workspace_for_missing`].
    ///
    /// # Errors
    ///
    /// The worker could not be told.
    pub fn ask_workspace_for_missing(&self) -> Result<(), AppError> {
        let mut inner = self.lock();
        if inner.asks_workspace.is_some() {
            return Ok(());
        }
        inner.asks_workspace = Some(BTreeMap::new());
        let reply = self.call(&mut inner, &Request::AskWorkspace);
        drop(inner);
        done(reply)
    }

    /// What the held document's store asks its workspace for:
    /// [`crate::Replica::wanted_resources`]. Empty when the worker cannot answer. What it
    /// names is kept: one the workspace hands over is held beside this one
    /// ([`Self::apply_document`]), as a chain's part is.
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<DocumentId> {
        let mut inner = self.lock();
        let reply = self.call(&mut inner, &Request::Wanted);
        let wanted: Vec<DocumentId> = match reply {
            Ok(Reply::Wanted(asked)) => asked
                .iter()
                .filter_map(|document| super::serve::parse_document(document))
                .collect(),
            _ => Vec::new(),
        };
        inner.asked.extend(wanted.iter().cloned());
        drop(inner);
        wanted
    }

    /// The workspace's hosts hold none of `record`:
    /// [`crate::Replica::workspace_answered`].
    ///
    /// # Errors
    ///
    /// The worker could not be told.
    pub fn workspace_answered(
        &self,
        record: &DocumentId,
        answer: ui::resource::WorkspaceAnswer,
    ) -> Result<(), AppError> {
        let mut inner = self.lock();
        let id = record.as_str().to_owned();
        match &mut inner.asks_workspace {
            Some(answered) => answered.insert(id, answer),
            None => inner.told.insert(id, answer),
        };
        let reply = self.call(
            &mut inner,
            &Request::Answered {
                record: record.as_str().to_owned(),
                answer,
            },
        );
        drop(inner);
        done(reply)
    }

    /// Where the agent's last operation left it: [`crate::Replica::presence`].
    #[must_use]
    pub fn presence(&self) -> Option<(LiveAnchor, LiveAnchor)> {
        let reply = self.call(&mut self.lock(), &Request::Presence).ok()?;
        match reply {
            Reply::Presence(Some(text)) => serde_json::from_str(&text).ok(),
            _ => None,
        }
    }

    /// Every document operation the member may perform now:
    /// [`crate::Replica::survey`]. Empty when the worker cannot answer.
    #[must_use]
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        let mut inner = self.lock();
        self.supply_before_rendering(&mut inner);
        let reply = self.call(&mut inner, &Request::Survey);
        drop(inner);
        match reply {
            Ok(Reply::Descriptors(text)) => {
                crate::render::offered(serde_json::from_str(&text).unwrap_or_default())
            }
            _ => Vec::new(),
        }
    }

    /// Every operation the record publishes now, each in the row of the
    /// resource holding it: [`crate::Replica::survey_rows`].
    #[must_use]
    pub fn survey_rows(&self) -> Vec<tilo_actions::remote::CatalogueRow> {
        let mut inner = self.lock();
        self.supply_before_rendering(&mut inner);
        let reply = self.call(&mut inner, &Request::SurveyRows);
        drop(inner);
        match reply {
            Ok(Reply::Page { rows, .. }) => serde_json::from_str(&rows).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// One page of [`Self::survey_rows`] as `query` lists it: how many
    /// operations it admits in all, and those from the `offset`th on, at most
    /// `take`, each row where it stands. Only the page crosses from the
    /// worker: a record holding thousands of items costs a page what a record
    /// of ten does.
    pub fn survey_page(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        window: (usize, usize),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>) {
        let (total, rows, _) = self.survey_page_arriving(query, window);
        (total, rows)
    }

    /// [`Self::survey_page`], and whether the page reads parts of a chain still on their way
    /// to the worker ([`crate::Replica::survey_page_arriving`]).
    pub fn survey_page_arriving(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        (offset, take): (usize, usize),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>, bool) {
        let Ok(query) = serde_json::to_string(query) else {
            return (0, Vec::new(), false);
        };
        let mut inner = self.lock();
        self.supply_before_rendering(&mut inner);
        let reply = self.call(
            &mut inner,
            &Request::SurveyPage {
                query,
                can_render: crate::render::can_render(),
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
                take: u64::try_from(take).unwrap_or(u64::MAX),
            },
        );
        drop(inner);
        match reply {
            Ok(Reply::Page {
                total,
                rows,
                arriving,
            }) => (
                usize::try_from(total).unwrap_or(usize::MAX),
                serde_json::from_str(&rows).unwrap_or_default(),
                arriving,
            ),
            _ => (0, Vec::new(), false),
        }
    }

    /// The keys `mutation`, changes this replica took from its worker and
    /// admitted into the document `id`, writes there: read from the changes' own
    /// operations where it holds them, so the admission that checked
    /// them names what they touched and no worker is asked. `None` for changes
    /// it does not hold.
    pub fn written_keys(
        &self,
        id: &DocumentId,
        mutation: &DocumentMutation,
    ) -> Option<Vec<Vec<String>>> {
        let mut inner = self.lock();
        let written = if id == &self.record {
            own_keys_written(&mut inner.mirror, mutation)
        } else {
            beside_keys_written(inner.beside.get_mut(id)?, mutation)
        };
        drop(inner);
        written
    }

    /// The heads the document `id` stands at here, sorted: this one, or one held
    /// beside it. What a merge that follows it takes only the changes after.
    pub fn heads_of(&self, id: &DocumentId) -> Option<Vec<[u8; 32]>> {
        let mut inner = self.lock();
        let heads = if id == &self.record {
            inner.mirror.get_heads()
        } else {
            inner.beside.get_mut(id)?.heads()
        };
        let mut heads: Vec<[u8; 32]> = heads.into_iter().map(|head| head.0).collect();
        heads.sort_unstable();
        Some(heads)
    }

    /// The one operation `address` names, as [`Self::survey`] lists it, or
    /// `None`: what a host that already knows what it runs asks its record
    /// for, rather than the whole listing.
    #[must_use]
    pub fn descriptor(&self, address: &ActionAddress) -> Option<ActionDescriptor> {
        let address = serde_json::to_string(address).ok()?;
        let mut inner = self.lock();
        self.supply_before_rendering(&mut inner);
        let answered = self.call(&mut inner, &Request::Resolve { address });
        drop(inner);
        match answered {
            Ok(Reply::Descriptors(text)) => crate::render::offered(
                serde_json::from_str::<Vec<ActionDescriptor>>(&text).unwrap_or_default(),
            )
            .into_iter()
            .next(),
            _ => None,
        }
    }

    /// Perform one published operation through the resource's own API:
    /// [`crate::Replica::invoke`].
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_with(None, None, address, arguments, confirm)
    }

    /// [`Self::invoke`] showing the appearance the API admitted:
    /// [`crate::Replica::invoke_appearing`].
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_appearing(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
        appearance: Option<&tilo_actions::Appearance>,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_within(None, None, appearance, address, arguments, confirm)
    }

    /// [`Self::invoke`] as `invoker` within `context`:
    /// [`crate::Replica::invoke_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_with(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
        context: Option<&tilo_actions::ActionContext>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_within(invoker, context, None, address, arguments, confirm)
    }

    /// [`Self::invoke_with`] showing `appearance`, which only an API that
    /// admitted it passes.
    pub fn invoke_within(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
        context: Option<&tilo_actions::ActionContext>,
        appearance: Option<&tilo_actions::Appearance>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        // A render is made in the host's render service, never the worker.
        if let Some(rendered) =
            crate::render::route(self, self.content.as_ref(), address, arguments)
        {
            return rendered;
        }
        let unavailable = |reason: String| ApiError::Unavailable {
            code: "app_worker".into(),
            reason,
        };
        let mut inner = self.lock();
        inner.dirty = true;
        inner.beside_dirty = true;
        let mut given = Vec::new();
        if let Some(content) = &self.content {
            // Content the workspace keeps, sealed, is given as the value the
            // document keeps it by: the operation takes it by reference, and
            // none of its bytes is handed over for a reference alone. What
            // the app actually reads still arrives the way it asks for it.
            let (by_reference, supplied) = Self::by_reference(&mut inner, content, arguments);
            given = by_reference;
            self.supply(&mut inner, content, &supplied)
                .map_err(|error| unavailable(error.to_string()))?;
            // What the record refers to is the effect's best effort: the
            // operation still runs, and asks for what it lacks, without it.
            if let Err(error) = self.supply_referenced(&mut inner, content, address) {
                tracing::warn!(app = %self.app_id, %error,
                    "app worker: the content the record refers to was not put");
            }
        }
        let request = Request::Invoke {
            address: serde_json::to_string(address).map_err(|e| unavailable(e.to_string()))?,
            arguments: serde_json::to_string(arguments).map_err(|e| unavailable(e.to_string()))?,
            confirm,
            invoker: invoker
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
            context: context
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
            appearance: appearance
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
            members: self.members_for(invoker),
            given: given.clone(),
        };
        let mut reply = self.call(&mut inner, &request);
        if let Some(content) = &self.content {
            let wanted = self.gather(&mut inner, content);
            let failed = !matches!(reply, Ok(Reply::Output(_)));
            let mut missing: Vec<String> = wanted
                .into_iter()
                .filter(|hash| content.has(hash))
                .collect();
            // The operation read content it was given by reference: the
            // bytes it asked for, and those of what it named that it lacked,
            // are put in and it is made once more with them there.
            if !missing.is_empty() {
                missing.extend(
                    given
                        .iter()
                        .map(|(reference, _)| reference.clone())
                        .filter(|reference| !inner.held.contains(reference)),
                );
            }
            if failed && !missing.is_empty() {
                for hash in &missing {
                    inner.held.remove(hash);
                }
                if self.supply(&mut inner, content, &missing).is_ok() {
                    reply = self.call(&mut inner, &request);
                    self.gather(&mut inner, content);
                }
            }
        }
        drop(inner);
        match reply {
            Ok(Reply::Output(text)) => serde_json::from_str(&text)
                .map_err(|error| ApiError::Failed(format!("the output did not decode: {error}"))),
            Ok(Reply::Refused(error)) => Err(error.into_api(address)),
            Ok(Reply::Failed(reason)) => Err(unavailable(reason)),
            Ok(_) => Err(unavailable("the worker answered out of turn".into())),
            Err(error) => Err(unavailable(error.to_string())),
        }
    }

    /// What the arguments name, split by how it reaches the worker: the
    /// sealed content the workspace keeps already, each with the stored form
    /// the document keeps it by, and the references that still have to be
    /// put in. An operation that only names content costs nothing for it.
    fn by_reference(
        inner: &mut Inner,
        content: &ContentStore,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> (Vec<(String, String)>, Vec<String>) {
        let mut given = Vec::new();
        let mut supplied = Vec::new();
        for reference in named_content(arguments) {
            let sealed = tilo_content::is_sealed(&reference) && content.has(&reference);
            match sealed
                .then(|| Self::stored_form(inner, content, &reference))
                .flatten()
            {
                Some(stored) => given.push((reference, stored)),
                None => supplied.push(reference),
            }
        }
        (given, supplied)
    }

    /// The value the sealed content `reference` names as a document keeps it
    /// (`blake3:<hash>:<len>:<reference>`): read from the workspace's store
    /// once, and remembered while this replica holds its worker.
    fn stored_form(inner: &mut Inner, content: &ContentStore, reference: &str) -> Option<String> {
        if let Some(stored) = inner.forms.get(reference) {
            return Some(stored.clone());
        }
        let bytes = content.read(reference)?;
        let stored = format!(
            "blake3:{}:{}:{reference}",
            blake3::hash(&bytes).to_hex(),
            bytes.len()
        );
        if inner.forms.len() < REMEMBERED_STORED_FORMS {
            inner.forms.insert(reference.to_owned(), stored.clone());
        }
        Some(stored)
    }

    /// Put the content at `hashes` the workspace has and the worker does not yet
    /// hold into the worker.
    fn supply(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        hashes: &[String],
    ) -> Result<(), AppError> {
        for hash in hashes {
            if inner.held.contains(hash) || !content.has(hash) {
                continue;
            }
            self.put_held(inner, content, hash)?;
        }
        Ok(())
    }

    /// Put the content at `hash` into the worker, starting one if none
    /// runs, and note that it holds it. Its size, when it was put.
    fn put_held(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        hash: &str,
    ) -> Result<Option<usize>, AppError> {
        if inner.process.is_none() {
            // A new worker is given everything held; this one is next.
            self.revive(inner, true)?;
        }
        let process = inner
            .process
            .as_mut()
            .ok_or_else(|| worker_error("no app worker is running"))?;
        match put_content(process, content, hash) {
            Ok(Some(size)) => {
                inner.held.insert(hash.to_owned());
                Ok(Some(size))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                inner.stop();
                Err(error)
            }
        }
    }

    /// Put into the worker the content the document refers to, before its view
    /// renders: a view reads what its document names (the PDF a PDF document
    /// pins), and the operation that named it may have taken it by reference
    /// alone, so no byte of it crossed to the worker. Best effort, as an
    /// effect's referenced content is: a view that lacks what it reads says so
    /// itself.
    fn supply_before_rendering(&self, inner: &mut Inner) {
        let Some(content) = &self.content else {
            return;
        };
        if let Err(error) = self.supply_held_content(inner, content) {
            tracing::warn!(app = %self.app_id, %error,
                "app worker: the content a view reads was not put");
        }
    }

    /// Before an effect runs, put into the worker the content the document
    /// refers to that the workspace has and the worker does not hold: what an
    /// effect reads of it (an export's pictures) may have been placed by
    /// another member, in another worker. A view's own read is supplied by
    /// [`Self::supply_before_rendering`], before it renders.
    /// Document operations take nothing more than they name. Bounded by
    /// [`REFERENCED_CONTENT_ITEMS`] and [`REFERENCED_CONTENT_BYTES`]; what
    /// is left out is logged.
    fn supply_referenced(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        address: &ActionAddress,
    ) -> Result<(), AppError> {
        // Only an effect reads the content its record refers to, and reading
        // every value the record holds for it costs what the record holds: a
        // document operation, nearly every write, reads none of it.
        if !self.is_effect(inner, address)? {
            return Ok(());
        }
        self.supply_held_content(inner, content)
    }

    /// Put into the worker the content the held document refers to that
    /// the workspace has and the worker does not hold: what an effect reads of
    /// the document — a message's parts, an export's pictures — which another
    /// member may have placed, in another worker. Bounded as
    /// [`Self::supply_referenced`]; what is left out is logged.
    ///
    /// A caller of the workspace's actions reads a document within their own
    /// bounds, in a replica of their own that already told the worker
    /// [`Self::supply_referenced`] would ask it: they say what the operation is.
    pub fn put_held_content(&self) -> Result<(), AppError> {
        let Some(content) = &self.content else {
            return Ok(());
        };
        let mut inner = self.lock();
        self.supply_held_content(&mut inner, content)
    }

    fn supply_held_content(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
    ) -> Result<(), AppError> {
        // The document and each of its parts: what an app keeps in a chain is
        // held beside its own document, and its content is named there.
        let Inner {
            mirror,
            beside,
            named: read,
            others,
            ..
        } = &mut *inner;
        read.retain(|document, _| document.as_ref().is_none_or(|id| beside.contains_key(id)));
        let mut named = named_in(read, None, mirror).clone();
        for (id, part) in beside.iter_mut() {
            // What the worker wrote to a part and that waits unapplied names content the worker
            // made itself: it holds that content, and a new worker is given it again
            // ([`Self::revive`]) or asks for it ([`Self::gather`]). The part is applied, to be
            // read whole, only for what others wrote to it.
            let doc = if others.contains(id) {
                let settled = part
                    .settled()
                    .map_err(|error| AppError::Merge(error.to_string()))?;
                others.remove(id);
                &*settled
            } else {
                part.unsettled().0
            };
            named.extend(named_in(read, Some(id.clone()), doc).iter().cloned());
        }
        let missing: Vec<String> = named
            .into_iter()
            .filter(|hash| !inner.held.contains(hash) && content.has(hash))
            .collect();
        let (mut items, mut bytes) = (0usize, 0usize);
        for (at, hash) in missing.iter().enumerate() {
            if items >= REFERENCED_CONTENT_ITEMS || bytes >= REFERENCED_CONTENT_BYTES {
                tracing::warn!(app = %self.app_id, record = %self.record.as_str(),
                    skipped = missing.len().saturating_sub(at),
                    "app worker: the record refers to more content than an effect is given");
                break;
            }
            if let Some(size) = self.put_held(inner, content, hash)? {
                items = items.saturating_add(1);
                bytes = bytes.saturating_add(size);
            }
        }
        Ok(())
    }

    /// Whether `address` names one of the record's effects or views now,
    /// rather than a change of a document ([`ActionDescriptor::changes_only_a_document`]).
    fn is_effect(&self, inner: &mut Inner, address: &ActionAddress) -> Result<bool, AppError> {
        let address_json =
            serde_json::to_string(address).map_err(|error| worker_error(error.to_string()))?;
        // The one action, resolved by its address, never the whole listing.
        match self.call(
            inner,
            &Request::Resolve {
                address: address_json,
            },
        )? {
            Reply::Descriptors(text) => {
                let offered: Vec<ActionDescriptor> =
                    serde_json::from_str(&text).map_err(|error| worker_error(error.to_string()))?;
                Ok(offered.iter().any(|descriptor| {
                    descriptor.address == *address && !descriptor.changes_only_a_document()
                }))
            }
            Reply::Failed(reason) => Err(worker_error(reason)),
            _ => {
                inner.stop();
                Err(worker_error("the worker answered out of turn"))
            }
        }
    }

    /// Keep what the app stored in the workspace's store under the hash its
    /// bytes have here, and answer the hashes it asked for and lacked.
    /// Content the workspace does not keep is reported and left in the worker.
    fn gather(&self, inner: &mut Inner, content: &ContentStore) -> Vec<String> {
        let mut wanted = Vec::new();
        while inner.process.is_some() {
            let (offered, asked) = match self.call(inner, &Request::TakeContent) {
                Ok(Reply::Content { offered, wanted }) => (offered, wanted),
                Ok(Reply::Failed(reason)) => {
                    tracing::warn!(app = %self.app_id, %reason,
                        "app worker: its content was not taken");
                    break;
                }
                Ok(_) => {
                    inner.stop();
                    break;
                }
                Err(_) => break,
            };
            wanted.extend(
                asked
                    .into_iter()
                    .filter(|hash| tilo_content::valid_hash(hash))
                    .map(|hash| hash.to_ascii_lowercase()),
            );
            if offered.is_empty() {
                break;
            }
            // Offered as the worker keeps them (sealed, or public): kept
            // under the address the worker's references name.
            for bytes in offered {
                match content.put_public(&bytes) {
                    Ok(hash) => {
                        inner.held.insert(hash);
                    }
                    Err(error) => {
                        tracing::warn!(app = %self.app_id, %error,
                            "app worker: stored content was not kept");
                    }
                }
            }
        }
        wanted
    }

    /// Read the record as the supervisor holds it: everything taken from the
    /// worker and everything applied, as admitted here.
    pub fn read<R>(&self, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        Some(read(&self.lock().mirror))
    }

    /// [`Self::read`] through the mirror's history: its heads and changes,
    /// which a reader of events after a cursor needs.
    pub fn read_history<R>(&self, read: impl FnOnce(&automerge::Automerge) -> R) -> R {
        read(self.lock().mirror.document())
    }

    /// This member's changes since the last call, or `None`:
    /// [`crate::Replica::take_mutation`]. The first call on a new record
    /// returns the whole document.
    ///
    /// # Errors
    ///
    /// The worker's changes were not admitted (they would not load or apply
    /// on top of the record), or the worker could not be asked. Nothing is
    /// returned to sign then.
    pub fn take_mutation(&self) -> Result<Option<DocumentMutation>, AppError> {
        let mut inner = self.lock();
        let own = self.collect(&mut inner)?;
        let mut kept = std::mem::take(&mut inner.kept);
        let whole = std::mem::take(&mut inner.fresh).then(|| inner.mirror.save());
        drop(inner);
        let own = match own {
            Some(own) if kept.is_empty() => Some(own),
            Some(own) => {
                kept.extend(own);
                Some(kept)
            }
            None => (!kept.is_empty()).then_some(kept),
        };
        Ok(whole
            .map(DocumentMutation::Snapshot)
            .or_else(|| own.map(DocumentMutation::Incremental)))
    }

    /// Give back `mutation`, taken by [`Self::take_mutation`] but not
    /// written: the next take returns it again, before anything since. What
    /// fails to be written alongside it (the records beside) must not take
    /// it with it.
    pub fn put_back(&self, mutation: DocumentMutation) {
        let mut inner = self.lock();
        match mutation {
            // The whole record is in the mirror: it goes whole again.
            DocumentMutation::Snapshot(_) => {
                inner.fresh = true;
                inner.kept.clear();
            }
            DocumentMutation::Incremental(bytes) => {
                if !inner.fresh {
                    let later = std::mem::replace(&mut inner.kept, bytes);
                    inner.kept.extend(later);
                }
            }
        }
    }

    /// Ask the worker for its changes and admit them to the mirror.
    fn collect(&self, inner: &mut Inner) -> Result<Option<Vec<u8>>, AppError> {
        if !std::mem::take(&mut inner.dirty) {
            return Ok(None);
        }
        match self.call(inner, &Request::TakeMutation)? {
            Reply::Mutation(None) => Ok(None),
            Reply::Mutation(Some((true, bytes))) if inner.fresh => {
                inner.mirror = tilo_automerge_isolate::load(&bytes)
                    .map_err(|error| AppError::Merge(error.to_string()))?;
                Ok(None)
            }
            Reply::Mutation(Some((false, bytes))) => {
                // What must be in the record is the worker's own change, not
                // every change of the record: another member's change
                // that arrived before what it builds on waits in the mirror's
                // queue, and is no reason to refuse this one. When the bytes
                // name no single change of their own — a document, a bundle —
                // only the whole record can be asked, as before.
                let own = tilo_automerge_isolate::change_hashes(&bytes);
                let dependencies = match &own {
                    Some(_) => Dependencies::RetainOrphans,
                    None => Dependencies::RequireComplete,
                };
                match tilo_automerge_isolate::apply(&mut inner.mirror, &bytes, dependencies) {
                    Ok(Applied::Changed(_)) if holds(&inner.mirror, own.as_deref()) => {
                        Ok(Some(bytes))
                    }
                    Ok(_) => {
                        inner.stop();
                        Err(AppError::Merge(
                            "the worker's change builds on nothing the record holds".into(),
                        ))
                    }
                    Err(error) => {
                        inner.stop();
                        Err(AppError::Merge(error.to_string()))
                    }
                }
            }
            Reply::Mutation(Some((true, _))) => {
                inner.stop();
                Err(worker_error(
                    "the worker replaced a record it did not create",
                ))
            }
            Reply::Failed(reason) => {
                // The worker kept its changes: they are asked for again.
                inner.dirty = true;
                Err(worker_error(reason))
            }
            _ => {
                inner.stop();
                Err(worker_error("the worker answered out of turn"))
            }
        }
    }

    /// Merge another member's durable change: [`crate::Replica::apply`].
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when the change is not admitted or the
    /// record refuses it, and [`AppError::Confinement`] when the worker
    /// could not be asked.
    pub fn apply(&self, mutation: &DocumentMutation) -> Result<(), AppError> {
        let (snapshot, bytes) = match mutation {
            DocumentMutation::Snapshot(bytes) => (true, bytes),
            DocumentMutation::Incremental(bytes) => (false, bytes),
        };
        let mut inner = self.lock();
        // What the mirror holds already (this replica's own write, coming back
        // from the workspace) is no change: neither merged again nor sent the
        // worker.
        if !snapshot && holds_all(&inner.mirror, bytes) {
            return Ok(());
        }
        // Admitted here before the worker sees it. A whole document may be
        // another record's, which the record refuses below, so it is merged
        // into a copy kept only if the record takes it. An increment that
        // builds on nothing here only waits in the mirror's queue: the
        // mirror then holds more than the worker, never less, so the
        // worker's own changes still apply on top of it.
        let mut candidate = if snapshot {
            let mut candidate = inner.mirror.clone();
            admit_incoming(&mut candidate, bytes)?;
            Some(candidate)
        } else {
            admit_incoming(&mut inner.mirror, bytes)?;
            None
        };
        // The worker holds this record too, and a whole document is sent to
        // it as the changes it lacks: what crossed for a note added to a
        // notebook of thousands is that note, never the notebook.
        let (snapshot, bytes) = match &mut candidate {
            Some(candidate) => match changes_since(candidate, &mut inner.mirror) {
                Some(changes) => (false, changes),
                None => (snapshot, bytes.clone()),
            },
            None => (snapshot, bytes.clone()),
        };
        let reply = self.call(&mut inner, &Request::Apply { snapshot, bytes });
        let result = match reply {
            Ok(Reply::Done) => {
                if let Some(candidate) = candidate {
                    inner.mirror = candidate;
                }
                Ok(())
            }
            Ok(Reply::Failed(reason)) => Err(AppError::Merge(reason)),
            Ok(_) => {
                inner.stop();
                Err(worker_error("the worker answered out of turn"))
            }
            Err(error) => Err(error),
        };
        drop(inner);
        result
    }

    /// Merge another member's durable change into `record`: this
    /// one ([`Self::apply`]), or one its app keeps beside it, a part of its
    /// chain ([`crate::Replica::apply_document`]), mirrored here as this one
    /// is. A change to a part not held yet waits for its whole document.
    ///
    /// # Errors
    ///
    /// As [`Self::apply`]; a record that is no part of a chain is refused.
    pub fn apply_document(
        &self,
        document: &DocumentId,
        mutation: &DocumentMutation,
    ) -> Result<(), AppError> {
        if document == &self.record {
            return self.apply(mutation);
        }
        let (snapshot, bytes) = match mutation {
            DocumentMutation::Snapshot(bytes) => (true, bytes),
            DocumentMutation::Incremental(bytes) => (false, bytes),
        };
        let mut inner = self.lock();
        if document.chain_part().is_none()
            && !self.keeps_for_workspace(document)
            && !beside_asked(&inner, document)
        {
            return Err(AppError::Merge(format!(
                "{} is held on its own, not beside {}",
                document.as_str(),
                self.record.as_str()
            )));
        }
        // Admitted here before the worker sees it, as the held record's own
        // changes are ([`Self::apply`]): a whole document into a copy of the
        // part's mirror, kept once the worker took it; an increment into the
        // mirror itself, which then holds more than the worker, never less.
        // A part's mirror grows with every post: copying it for each one
        // made a post cost what the part already holds.
        let candidate = match inner.beside.get_mut(document) {
            Some(mirror) if !snapshot => {
                // What the part holds already (this replica's own write
                // coming back from the workspace) is no change to it.
                if tilo_automerge_isolate::change_hashes(bytes)
                    .is_some_and(|carried| carried.iter().all(|hash| mirror.holds(hash)))
                {
                    return Ok(());
                }
                admit_beside_incoming(mirror, bytes)?;
                None
            }
            Some(mirror) => {
                let mut candidate = mirror.clone();
                admit_beside_incoming(&mut candidate, bytes)?;
                Some(candidate)
            }
            None if snapshot => {
                let mut candidate = Deferred::default();
                admit_beside_incoming(&mut candidate, bytes)?;
                Some(candidate)
            }
            None => return Ok(()),
        };
        inner.others.insert(document.clone());
        let reply = self.call(
            &mut inner,
            &Request::ApplyDocument {
                record: document.as_str().to_owned(),
                snapshot,
                bytes: bytes.clone(),
            },
        );
        let result = match reply {
            Ok(Reply::Done) => {
                if let Some(candidate) = candidate {
                    inner.beside.insert(document.clone(), candidate);
                }
                // A write that waited for this part (a post made while the
                // chain's newest part was on its way) is applied as it lands.
                inner.beside_dirty = true;
                Ok(())
            }
            Ok(Reply::Failed(reason)) => Err(AppError::Merge(reason)),
            Ok(_) => {
                inner.stop();
                Err(worker_error("the worker answered out of turn"))
            }
            Err(error) => Err(error),
        };
        drop(inner);
        result
    }

    /// This member's changes to the documents held beside this one since
    /// the last call, by record, admitted here first:
    /// [`crate::Replica::take_document_mutations`]. A part the worker started
    /// comes as the changes it made and whole, which founds it here.
    ///
    /// # Errors
    ///
    /// As [`Self::take_mutation`].
    pub fn take_document_mutations(&self) -> Result<Vec<(DocumentId, DocumentMutation)>, AppError> {
        let mut inner = self.lock();
        let mut out = std::mem::take(&mut inner.beside_kept);
        if !std::mem::take(&mut inner.beside_dirty) {
            return Ok(out);
        }
        let taken = match self.call(&mut inner, &Request::TakeDocumentMutations) {
            Ok(Reply::DocumentMutations(taken)) => taken,
            Ok(Reply::Failed(reason)) => {
                // The worker kept its changes: they are asked for again.
                inner.beside_dirty = true;
                inner.beside_kept = out;
                return Err(worker_error(reason));
            }
            Ok(_) => {
                inner.stop();
                inner.beside_kept = out;
                return Err(worker_error("the worker answered out of turn"));
            }
            Err(error) => {
                inner.beside_kept = out;
                return Err(error);
            }
        };
        for taken in taken {
            match self.admit_beside(&mut inner, &taken.record, taken.changes, taken.whole) {
                Ok((document, mutation)) => joined(&mut out, document, mutation),
                Err(error) => {
                    // A worker that sends what is not admitted is not asked
                    // again. What was admitted before it is in the mirrors, and goes out
                    // with the next take.
                    inner.stop();
                    inner.beside_kept = out;
                    return Err(error);
                }
            }
        }
        drop(inner);
        Ok(out)
    }

    /// Admit one change the worker made to a document held beside this one
    /// to its mirror: a part it started, as the changes it made and the part
    /// whole; a part held here, as changes on top of it.
    fn admit_beside(
        &self,
        inner: &mut Inner,
        record: &str,
        changes: Vec<u8>,
        whole: Option<Vec<u8>>,
    ) -> Result<(DocumentId, DocumentMutation), AppError> {
        let parsed = DocumentId::parse(record);
        // Whether the store asked its workspace for it, read before its mirror is borrowed.
        let asked = parsed
            .as_ref()
            .is_some_and(|document| beside_asked(inner, document));
        let beside = &mut inner.beside;
        let document = parsed
            .filter(|document| {
                (document.chain_part().is_some()
                    || self.keeps_for_workspace(document)
                    || beside.contains_key(document)
                    || asked)
                    && document != &self.record
            })
            .ok_or_else(|| worker_error("the worker named one it does not hold beside"))?;
        match (whole, beside.get_mut(&document)) {
            // A part the worker started: its changes were made on the empty document, so
            // they wait here unapplied, applied in an isolate first only once the part is
            // read. What founds it at a peer is the part whole.
            (Some(whole), None) => {
                let mut mirror = Deferred::default();
                match mirror.admit_unapplied(&changes) {
                    Ok(Applied::Changed(_)) => {
                        beside.insert(document.clone(), mirror);
                        Ok((document, DocumentMutation::Snapshot(whole)))
                    }
                    Ok(Applied::MissingDependencies) => Err(AppError::Merge(
                        "the worker's changes to a part it started build on nothing it made".into(),
                    )),
                    Err(error) => Err(AppError::Merge(error.to_string())),
                }
            }
            // The worker handed the part over again, after a take that failed on a later one:
            // what is new of its changes waits as any change does, and the whole still founds
            // it where the first take did not reach.
            (Some(whole), Some(mirror)) => {
                let held = tilo_automerge_isolate::change_hashes(&changes)
                    .is_some_and(|carried| carried.iter().all(|hash| mirror.holds(hash)));
                if !held {
                    match mirror.admit_unapplied(&changes) {
                        Ok(Applied::Changed(_)) => {}
                        Ok(Applied::MissingDependencies) => {
                            return Err(AppError::Merge(
                                "the worker's changes to a part it started build on nothing it \
                                 made"
                                    .into(),
                            ));
                        }
                        Err(error) => return Err(AppError::Merge(error.to_string())),
                    }
                }
                Ok((document, DocumentMutation::Snapshot(whole)))
            }
            // The worker made this change on the state the mirror holds: it waits there
            // unapplied, and is applied, in an isolate first, only once the mirror is read.
            (None, Some(mirror)) => match mirror.admit_unapplied(&changes) {
                Ok(Applied::Changed(_)) => Ok((document, DocumentMutation::Incremental(changes))),
                Ok(Applied::MissingDependencies) => Err(AppError::Merge(
                    "the worker's change builds on nothing the part holds".into(),
                )),
                Err(error) => Err(AppError::Merge(error.to_string())),
            },
            (None, None) => Err(worker_error(
                "the worker changed a part it did not start, and gave no whole for it",
            )),
        }
    }

    /// Whether `id` is what this app keeps for its whole workspace beside its own document
    /// ([`collab_types::WORKSPACE_DOCUMENTS`]): held with it and mirrored here as a chain's
    /// parts are, since the app's actions reach it through the same view.
    fn keeps_for_workspace(&self, id: &DocumentId) -> bool {
        collab_types::workspace_document(&self.app_id, id.as_str()).is_some()
    }

    /// The documents held beside this one ([`Self::apply_document`]), by id.
    #[must_use]
    pub fn resources_beside(&self) -> Vec<DocumentId> {
        self.lock().beside.keys().cloned().collect()
    }

    /// Read the document `id` as the supervisor holds it: this one, or one held
    /// beside it. `None` for one not held here.
    pub fn read_resource<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Option<R> {
        let mut inner = self.lock();
        let read = if id == &self.record {
            Some(read(&inner.mirror))
        } else {
            let mirror = inner.beside.get_mut(id)?.settled().ok()?;
            Some(read(mirror))
        };
        drop(inner);
        read
    }

    /// [`Self::read_resource`] through the document's history.
    pub fn read_document_history<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&automerge::Automerge) -> R,
    ) -> Option<R> {
        let mut inner = self.lock();
        let read = if id == &self.record {
            Some(read(inner.mirror.document()))
        } else {
            let mirror = inner.beside.get_mut(id)?.settled().ok()?;
            Some(read(mirror.document()))
        };
        drop(inner);
        read
    }
}

/// `mutation` of `document` after what `out` holds: a change to a document `out` already changes
/// joins that change, so each document is written once, its changes in the order they were made.
fn joined(
    out: &mut Vec<(DocumentId, DocumentMutation)>,
    document: DocumentId,
    mutation: DocumentMutation,
) {
    if let DocumentMutation::Incremental(bytes) = &mutation
        && let Some((_, DocumentMutation::Incremental(earlier))) =
            out.iter_mut().find(|(held, _)| held == &document)
    {
        earlier.extend_from_slice(bytes);
        return;
    }
    out.push((document, mutation));
}

/// [`ConfinedReplica::written_keys`] of the held document's own `mirror`.
fn own_keys_written(doc: &mut AutoCommit, mutation: &DocumentMutation) -> Option<Vec<Vec<String>>> {
    let changes: Vec<automerge::Change> = match mutation {
        // The changes as they came, each one the document holds.
        DocumentMutation::Incremental(bytes) => {
            let changes = tilo_automerge_isolate::changes(bytes)?;
            let held = doc.committed_document();
            changes
                .iter()
                .all(|change| held.get_change_meta_by_hash(&change.hash()).is_some())
                .then_some(changes)?
        }
        // A record it started whole is every change it holds.
        DocumentMutation::Snapshot(_) => doc.get_changes(&[]),
    };
    ui::framework::keys_written(doc, &changes)
}

/// [`ConfinedReplica::written_keys`] of a part's `mirror`: read against what
/// the changes build on, so what waits to be applied is applied only for a
/// change that writes to what another waiting change made.
fn beside_keys_written(
    mirror: &mut Deferred,
    mutation: &DocumentMutation,
) -> Option<Vec<Vec<String>>> {
    match mutation {
        DocumentMutation::Incremental(bytes) => {
            // The changes as their admission parsed them, when they wait there unapplied:
            // parsing a list of a few hundred issues again cost a tenth of a second.
            let hashes = tilo_automerge_isolate::change_hashes(bytes)?;
            let changes: Vec<automerge::Change> = match mirror.admitted(&hashes) {
                Some(admitted) => admitted.into_iter().cloned().collect(),
                None => tilo_automerge_isolate::changes(bytes)?,
            };
            if !changes.iter().all(|change| mirror.holds(&change.hash())) {
                return None;
            }
            ui::framework::keys_written(mirror.unsettled().0, &changes)
                .or_else(|| ui::framework::keys_written(mirror.settled().ok()?, &changes))
        }
        // A part it started is every change it holds; a part the worker started and this
        // replica has not read yet keeps those changes parsed while they wait, so what they
        // wrote is read without applying the part here.
        DocumentMutation::Snapshot(_) => {
            let pending = mirror.pending_changes();
            if !pending.is_empty() {
                let (doc, _) = mirror.unsettled();
                if let Some(written) = ui::framework::keys_written(doc, pending.iter().copied()) {
                    return Some(written);
                }
            }
            let doc = mirror.settled().ok()?;
            let changes = doc.get_changes(&[]);
            ui::framework::keys_written(doc, &changes)
        }
    }
}

/// Admit untrusted `bytes` (a document, changes, or both) to `doc` through
/// the isolate, queueing changes whose dependencies have not arrived, as
/// `load_incoming` does.
fn admit_incoming(doc: &mut AutoCommit, bytes: &[u8]) -> Result<(), AppError> {
    if bytes.is_empty() {
        return Ok(());
    }
    tilo_automerge_isolate::apply(doc, bytes, Dependencies::RetainOrphans)
        .map(|_| ())
        .map_err(|error| AppError::Merge(error.to_string()))
}

/// [`admit_incoming`] to a part's mirror, where changes it can wait for
/// until the mirror is read wait.
fn admit_beside_incoming(mirror: &mut Deferred, bytes: &[u8]) -> Result<(), AppError> {
    if bytes.is_empty() {
        return Ok(());
    }
    mirror
        .admit(bytes, Dependencies::RetainOrphans)
        .map(|_| ())
        .map_err(|error| AppError::Merge(error.to_string()))
}

/// Whether `doc` holds every change `own` names: a change of the worker's
/// that waits in `doc`'s queue for a dependency it has not got is not held,
/// and is never published as if it were whole. Changes the bytes did not name
/// cannot be asked after, and are the whole record's business instead.
fn holds(doc: &AutoCommit, own: Option<&[automerge::ChangeHash]>) -> bool {
    own.is_none_or(|own| own.iter().all(|hash| doc.has_change(hash)))
}

/// Whether `doc` holds every change the increment `bytes` carries, read
/// from the changes' headers alone: then merging it brings nothing new.
fn holds_all(doc: &AutoCommit, bytes: &[u8]) -> bool {
    tilo_automerge_isolate::change_hashes(bytes).is_some_and(|carried| holds(doc, Some(&carried)))
}

/// Most bytes of a record's history admitted in one request: well under the
/// isolate's input ceiling, so a long history is admitted in parts rather
/// than refused for its length.
const HISTORY_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Admit a record's history, in order, in batches of whole changes.
fn admit_history(doc: &mut AutoCommit, changes: &[Vec<u8>]) -> Result<(), AppError> {
    let mut batch = Vec::new();
    for change in changes {
        if !batch.is_empty() && batch.len().saturating_add(change.len()) > HISTORY_BATCH_BYTES {
            admit_incoming(doc, &std::mem::take(&mut batch))?;
        }
        batch.extend_from_slice(change);
    }
    admit_incoming(doc, &batch)
}

/// Put the content at `hash` from `content` into the worker, answering its
/// size. `None` when it is not there to put, or too large to cross.
fn put_content(
    process: &mut Lease,
    content: &ContentStore,
    hash: &str,
) -> Result<Option<usize>, AppError> {
    let Some(bytes) = content.read(hash) else {
        return Ok(None);
    };
    if bytes.len() > MAX_CONTENT_ITEM_BYTES {
        tracing::warn!(%hash, "app worker: content is too large to hand over");
        return Ok(None);
    }
    let size = bytes.len();
    match process
        .call(&Request::PutContent {
            reference: hash.to_owned(),
            bytes,
        })
        .map_err(|stopped| worker_error(stopped.to_string()))?
    {
        Reply::Stored(stored) if stored == hash => Ok(Some(size)),
        Reply::Stored(_) => Err(worker_error("the worker kept content under another hash")),
        Reply::Failed(reason) => {
            tracing::warn!(%hash, %reason, "app worker: content was not kept");
            Ok(None)
        }
        _ => Err(worker_error("the worker answered out of turn")),
    }
}

/// The content `text` names: itself, when it is a bare hash or a kept
/// value's stored form, and every content reference inside it — with the key
/// that opens it, when it is sealed.
fn content_in(text: &str, named: &mut BTreeSet<String>) {
    if tilo_content::valid_hash(text) {
        named.insert(text.to_ascii_lowercase());
    }
    // A kept value's stored form: `blake3:<hash>:<len>[:<reference>]`.
    if let Some(rest) = text.trim().strip_prefix("blake3:") {
        let mut parts = rest.splitn(3, ':');
        let hash = parts.next().unwrap_or_default();
        let kept = parts.next();
        let reference = parts.next();
        if kept.is_some() && tilo_content::valid_hash(hash) {
            let named_by = reference
                .map(str::trim)
                .filter(|reference| tilo_content::valid_reference(reference))
                .map_or_else(|| hash.to_ascii_lowercase(), reference_of);
            named.insert(named_by);
        }
    }
    for after in text.split(tilo_actions::CONTENT_RESOURCE_KIND).skip(1) {
        if let Some(reference) = after
            .strip_prefix(':')
            .and_then(|rest| reference_at(rest.get(..64)?, rest.get(64..)))
        {
            named.insert(reference);
        }
    }
}

/// The reference `reference` names, its address lower case and the key that
/// opens it as it is: the key is a case-sensitive ciphertext.
fn reference_of(reference: &str) -> String {
    match reference.split_once('.') {
        Some((address, key)) => format!("{}.{key}", address.to_ascii_lowercase()),
        None => reference.to_ascii_lowercase(),
    }
}

/// The reference an address `hash` and what follows it (`key`'s own text,
/// when the reference carries a key) name, for a reference `hash` is a valid
/// address of.
fn reference_at(hash: &str, after: Option<&str>) -> Option<String> {
    if !tilo_content::valid_hash(hash) {
        return None;
    }
    let key = after.and_then(|tail| tail.strip_prefix('.'));
    let sealed = key
        .and_then(|key| key.get(..43))
        .map(|key| format!("{}.{key}", hash.to_ascii_lowercase()))
        .filter(|reference| tilo_content::valid_reference(reference));
    Some(sealed.unwrap_or_else(|| hash.to_ascii_lowercase()))
}

/// The content a record's document refers to: every key, string and text
/// in it that is a hash or holds a content address, as [`content_in`]
/// reads one. At most [`MOST_SCANNED_VALUES`] values are read.
fn referenced_content(doc: &AutoCommit) -> BTreeSet<String> {
    use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValueRef, ValueRef};
    fn visit(
        value: ValueRef<'_>,
        id: ObjId,
        named: &mut BTreeSet<String>,
        objects: &mut Vec<(ObjId, ObjType)>,
    ) {
        match value {
            ValueRef::Object(kind) => objects.push((id, kind)),
            ValueRef::Scalar(ScalarValueRef::Str(text)) => content_in(&text, named),
            ValueRef::Scalar(_) => {}
        }
    }
    let _profile = ui::tilo_resource::profiling::Phase::start("app_worker.content_named");
    let mut named = BTreeSet::new();
    let mut objects = vec![(automerge::ROOT, ObjType::Map)];
    let mut budget = MOST_SCANNED_VALUES;
    while let Some((object, kind)) = objects.pop() {
        match kind {
            ObjType::Map | ObjType::Table => {
                for item in doc.map_range(&object, ..) {
                    if budget == 0 {
                        break;
                    }
                    budget = budget.saturating_sub(1);
                    content_in(&item.key, &mut named);
                    let id = item.id();
                    visit(item.value, id, &mut named, &mut objects);
                }
            }
            ObjType::List => {
                for item in doc.list_range(&object, ..) {
                    if budget == 0 {
                        break;
                    }
                    budget = budget.saturating_sub(1);
                    let id = item.id();
                    visit(item.value, id, &mut named, &mut objects);
                }
            }
            ObjType::Text => {
                budget = budget.saturating_sub(1);
                if let Ok(text) = doc.text(&object) {
                    content_in(&text, &mut named);
                }
            }
        }
        if budget == 0 {
            tracing::warn!("app worker: the record is too large to read whole for its content");
            break;
        }
    }
    named
}

/// The content `doc` names ([`referenced_content`]), read again only when its heads moved
/// since it was read for `document` in `read`.
fn named_in<'a>(
    read: &'a mut BTreeMap<Option<DocumentId>, (Vec<automerge::ChangeHash>, BTreeSet<String>)>,
    document: Option<DocumentId>,
    doc: &AutoCommit,
) -> &'a BTreeSet<String> {
    use automerge::transaction::Transactable as _;
    let heads = doc.committed_heads();
    // No committed heads say nothing of what a transaction still open holds, so such a document
    // is read every time. One that holds nothing at all, as a part the worker started stands
    // while its own changes wait unapplied beside it, names nothing and is not read again.
    let unknown = heads.is_empty() && !(doc.is_empty() && doc.pending_ops() == 0);
    let entry = read.entry(document).or_default();
    if entry.0 != heads || unknown {
        *entry = (heads, referenced_content(doc));
    }
    &entry.1
}

/// The content an operation's arguments name: every string that is a
/// content address or a bare hash, and every address inside a longer one.
fn named_content(arguments: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    fn visit(value: &serde_json::Value, named: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(text) => content_in(text, named),
            serde_json::Value::Array(items) => {
                for item in items {
                    visit(item, named);
                }
            }
            serde_json::Value::Object(fields) => {
                for field in fields.values() {
                    visit(field, named);
                }
            }
            _ => {}
        }
    }
    let mut named = BTreeSet::new();
    for value in arguments.values() {
        visit(value, &mut named);
    }
    named.into_iter().collect()
}

/// What a worker's refusal to open says, as [`crate::Replica::open`] says it.
fn open_error(reason: &str, app_id: &str) -> AppError {
    if reason.starts_with("no app ") {
        AppError::UnknownApp(app_id.to_owned())
    } else {
        AppError::Launch(reason.to_owned())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use automerge::transaction::Transactable as _;

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// A request the app is still working on past the bound a caller waits is
    /// answered as the document opened again, naming the request; the worker
    /// goes on, and the next request is applied as usual.
    #[test]
    fn a_request_past_the_bound_reopens_the_document_and_the_next_applies() {
        let replica = begun();
        let worker = replica.worker_id();
        let mut inner = replica.lock();
        // Only the request past the bound waits less: opening the record
        // starts the app, which takes as long as it takes on a loaded machine.
        inner
            .process
            .as_mut()
            .expect("the begun record is held in a worker")
            .wait_for(std::time::Duration::from_millis(500));
        let answer = replica.call(&mut inner, &Request::Stall { millis: 5_000 });
        drop(inner);
        assert!(
            matches!(&answer, Err(AppError::Reopened { request }) if request == "stall"),
            "{answer:?}"
        );
        assert_eq!(replica.worker_id(), worker, "the worker was not stopped");
        let mut arguments = serde_json::Map::new();
        arguments.insert("name".into(), "Made after a slow request".into());
        replica
            .invoke(
                &ActionAddress::new("sheets.workbook", "new-sheet"),
                &arguments,
                false,
            )
            .expect("the reopened document takes the next act");
        assert!(
            matches!(
                replica.take_mutation(),
                Ok(Some(DocumentMutation::Incremental(_)))
            ),
            "what the next act wrote is published"
        );
    }

    /// A replica of a new workbook, at the change that began it.
    fn begun() -> ConfinedReplica {
        let replica = ConfinedReplica::open(
            &AppCatalogue::native(),
            "so.tilo.sheets",
            WORKSPACE,
            &tilo_actions::MemberId::agent("maker"),
            &ReplicaActor::Agent {
                person: tilo_actions::MemberId::person("maker"),
                name: String::new(),
            },
            Seed::New,
            false,
        )
        .unwrap();
        assert!(
            matches!(
                replica.take_mutation().unwrap(),
                Some(DocumentMutation::Snapshot(_))
            ),
            "a new record's genesis is what it first publishes"
        );
        replica
    }

    /// Every line the code under test writes to the log.
    #[derive(Clone, Default)]
    struct Lines(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock_recover().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
        type Writer = Self;

        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    /// How often `during` had a worker find an action by its address, counted by the line
    /// each request to a worker writes.
    fn asked_to_find(during: impl FnOnce()) -> usize {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(lines.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, during);
        let written = String::from_utf8_lossy(&lines.0.lock_recover()).into_owned();
        written
            .lines()
            .filter(|line| {
                line.contains("app worker: a request answered") && line.contains("kind=\"resolve\"")
            })
            .count()
    }

    #[test]
    fn an_action_found_again_with_nothing_between_asks_its_worker_once() {
        let replica = begun();
        let address = replica
            .survey()
            .into_iter()
            .next()
            .expect("a workbook offers its agent an action")
            .address;
        // As one write of the service asks: whether the document offers the action, what it
        // is, and once more after what it wrote was collected.
        let mut answers = Vec::new();
        let asked = asked_to_find(|| {
            answers.push(replica.descriptor(&address).map(|found| found.address));
            answers.push(replica.descriptor(&address).map(|found| found.address));
            replica.take_mutation().unwrap();
            answers.push(replica.descriptor(&address).map(|found| found.address));
        });
        assert_eq!(
            answers,
            vec![Some(address.clone()); 3],
            "each time it is the action asked for"
        );
        assert_eq!(
            asked, 1,
            "nothing moved between: the worker is asked once, not for every asking"
        );
        // Another member's change may change what the document offers: the worker is asked
        // again, once.
        let (first, _) = another_members_two_changes(&replica);
        let asked = asked_to_find(|| {
            replica
                .apply(&DocumentMutation::Incremental(first))
                .unwrap();
            assert!(replica.descriptor(&address).is_some());
            assert!(replica.descriptor(&address).is_some());
        });
        assert_eq!(asked, 1, "after a merge the worker is asked again, once");
    }

    /// Two changes of another member's on `replica`'s record, the second
    /// building on the first, each on its own: what arrives out of order.
    fn another_members_two_changes(replica: &ConfinedReplica) -> (Vec<u8>, Vec<u8>) {
        let saved = replica.lock().mirror.save();
        let mut doc = tilo_automerge_isolate::load(&saved).unwrap();
        doc.set_actor(automerge::ActorId::random());
        let began = doc.get_heads();
        doc.put(automerge::ROOT, "theirs", "first").unwrap();
        let first: Vec<u8> = doc
            .get_changes(&began)
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        let after_first = doc.get_heads();
        doc.put(automerge::ROOT, "theirs", "second").unwrap();
        let second: Vec<u8> = doc
            .get_changes(&after_first)
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        (first, second)
    }

    /// Whether `replica`'s record holds every change `hashes` names.
    fn recorded(replica: &ConfinedReplica, hashes: &[automerge::ChangeHash]) -> bool {
        let inner = replica.lock();
        holds(&inner.mirror, Some(hashes))
    }

    /// A change of the worker's own that waits for a dependency is not
    /// published as if it were whole, and one of another member's that
    /// waits is no reason to refuse it.
    #[test]
    fn the_workers_change_is_published_though_anothers_waits_for_what_it_builds_on() {
        let replica = begun();
        let (first, second) = another_members_two_changes(&replica);
        // The second change arrives first: it builds on the first, which the
        // record does not hold yet, so it waits in the record's queue.
        replica
            .apply(&DocumentMutation::Incremental(second.clone()))
            .unwrap();
        let waiting = tilo_automerge_isolate::change_hashes(&second).unwrap();
        assert!(
            !recorded(&replica, &waiting),
            "the change that arrived first waits, and is not held"
        );

        // The app makes a change of its own while that one waits.
        let mut arguments = serde_json::Map::new();
        arguments.insert("name".into(), "Made while another waited".into());
        replica
            .invoke(
                &ActionAddress::new("sheets.workbook", "new-sheet"),
                &arguments,
                false,
            )
            .expect("the app adds a sheet");

        let taken = replica
            .take_mutation()
            .expect("a change of the app's own is published though another's waits")
            .expect("the app changed the record");
        let DocumentMutation::Incremental(bytes) = taken else {
            panic!("an existing record publishes its changes, not the whole of it");
        };
        let own = tilo_automerge_isolate::change_hashes(&bytes).unwrap();
        assert!(
            recorded(&replica, &own),
            "what was published is in the record"
        );
        // And what waited still waits: it was never taken as whole.
        assert!(
            !recorded(&replica, &waiting),
            "a change waiting for what it builds on is not published as whole"
        );

        // The change it builds on arrives: both are the record's now.
        replica
            .apply(&DocumentMutation::Incremental(first))
            .unwrap();
        assert!(
            recorded(&replica, &waiting),
            "the change applies once what it builds on arrives"
        );
    }

    /// A view this worker renders is lent the names the workspace's member list gives its
    /// people and agents, beside those its governance knows: the same names the person's own
    /// page is told, so a row's author and writer chip read alike in both.
    #[test]
    fn a_view_the_worker_renders_is_lent_the_workspace_member_lists_names() {
        let ada = tilo_actions::MemberId::person("ada");
        let stack = tilo_actions::MemberId::agent("6f1d3a521c7e4b8e");
        let listed = {
            let (ada, stack) = (ada.clone(), stack.clone());
            move || {
                vec![
                    (ada.clone(), "Ada Stack".to_owned()),
                    (stack.clone(), "Local stack".to_owned()),
                ]
            }
        };
        let replica = begun().naming(std::sync::Arc::new(listed));
        let names = replica.member_names();
        assert!(
            names.contains(&(ada, "Ada Stack".to_owned())),
            "the person is lent the name the page shows: {names:?}"
        );
        assert!(
            names.contains(&(stack, "Local stack".to_owned())),
            "and so is the agent whose session wrote for them: {names:?}"
        );
    }

    /// What a list of issues written into a part writes is read from its changes as their
    /// admission parsed them: reading it costs less than parsing them. Parsing every change
    /// again cost each list of 200 issues more than a tenth of a second on a hosted workspace.
    #[test]
    fn what_a_list_writes_is_read_without_parsing_it_again() {
        use automerge::ReadDoc as _;
        use std::time::{Duration, Instant};

        const PARA: &str = "The page should load in a second whatever the tracker holds, and a \
            click should answer within a frame. This paragraph stands in for what a real issue says. ";
        let list = |doc: &mut AutoCommit, first: usize| {
            let issues = match doc.get(automerge::ROOT, "issues").unwrap() {
                Some((_, id)) => id,
                None => doc
                    .put_object(automerge::ROOT, "issues", automerge::ObjType::Map)
                    .unwrap(),
            };
            for at in first..first + 200 {
                let issue = doc
                    .put_object(&issues, format!("issue-{at}"), automerge::ObjType::Map)
                    .unwrap();
                doc.put(&issue, "title", format!("ENG-{at} Fix the list"))
                    .unwrap();
                let body = doc
                    .put_object(&issue, "body", automerge::ObjType::Text)
                    .unwrap();
                doc.splice_text(&body, 0, 0, &PARA.repeat(2 + at % 7))
                    .unwrap();
                doc.commit();
            }
        };
        let mut doc = AutoCommit::new();
        list(&mut doc, 0);
        let whole = doc.save();
        let heads = doc.get_heads();
        list(&mut doc, 200);
        let bytes = doc.save_after(&heads);
        let mutation = DocumentMutation::Incremental(bytes.clone());

        let mut mirror = Deferred::from(AutoCommit::load(&whole).unwrap());
        assert!(matches!(
            mirror.admit_unapplied(&bytes),
            Ok(Applied::Changed(true))
        ));
        let parsed = tilo_automerge_isolate::changes(&bytes).unwrap();
        let written =
            beside_keys_written(&mut mirror, &mutation).expect("it reads what they write");
        assert_eq!(
            Some(written),
            ui::framework::keys_written(mirror.unsettled().0, &parsed),
            "the same keys as the changes parsed anew"
        );

        // The fastest of a few tries: what the work costs, not what the machine did meanwhile.
        let fastest = |work: &mut dyn FnMut() -> Duration| {
            (0..5).map(|_| work()).min().unwrap_or(Duration::ZERO)
        };
        let parsing = fastest(&mut || {
            let began = Instant::now();
            std::hint::black_box(tilo_automerge_isolate::changes(&bytes));
            began.elapsed()
        });
        let reading = fastest(&mut || {
            let began = Instant::now();
            std::hint::black_box(beside_keys_written(&mut mirror, &mutation));
            began.elapsed()
        });
        eprintln!("parsing the list took {parsing:?}, reading what it writes {reading:?}");
        assert!(
            reading <= parsing,
            "reading what a list writes took {reading:?}, parsing it {parsing:?}"
        );
    }
}
