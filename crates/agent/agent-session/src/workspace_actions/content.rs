//! A workspace's content on its host: the store its records' files live in.
//!
//! A caller of the host's actions over a session (an agent's MCP session
//! through the person's own device) puts a file's bytes here, step by step
//! ([`ContentPutStep`]), and names the content `tilo.content:<hash>` in an
//! action. The confined app worker that runs the action is lent the same
//! store, so an app reads what was put (PDF Viewer's `import-pdf-content`,
//! Writer's `read-content`) and keeps what it makes (Writer's
//! `export-content`) exactly as it does on a device. The host answers the
//! workspace's members from it too, so a page that opens the record later
//! reads the same bytes.
//!
//! The same callers read those bytes back out, a piece at a time
//! ([`super::RemoteActionMessage::ContentGetRequest`]): only this
//! workspace's store answers, so a reference another workspace holds reads
//! as content this one does not have.
//!
//! Who may: the host's own person and their agents, and any member of the
//! workspace, or an agent of theirs, as their member access there is now: one who
//! may change its records (an editor, an agent of one) puts files, and one
//! who may read it gets them. A viewer only reads.
//!
//! Once a workspace's store is found, its steps are served on threads of their own
//! ([`ContentSteps`]), the caller's access checked there before a byte of the
//! step is taken: an upload waits neither for the step before it nor for the
//! workspace's actions, so what bounds it is the link or the disk. Only the
//! step that finds the store is the service's thread's ([`Job::Content`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::sync::{Arc, Condvar, Mutex};
use tilo_actions::remote::{
    ActionCallError, CONTENT_GETS_UNAVAILABLE, Caller, ContentGetAnswer, ContentPiece, ContentPut,
    ContentPutStep, ContentPuts, MAX_CONTENT_FRAME_BYTES, Pending, RemoteActionMessage, pending,
};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;

use super::{Job, Shared, Worker, WorkspaceActions};
use crate::worker::ConfinedReplica;
use tilo_server_protocol::WorkspaceId;
use ui::permissions::policy::ActionKind;

/// The most bytes one upload into a workspace's store takes. A file goes up
/// in pieces whatever its size; this bounds one file so a runaway agent
/// cannot fill a host's disk with a single upload. The bytes wait on disk,
/// never in memory ([`StorePut`]), so a file of hundreds of megabytes takes
/// no more of the host's memory than a small one.
pub const MAX_PUT_BYTES: u64 = 1024 * 1024 * 1024;

/// One piece of `content` read out of `store`: `length` bytes from `offset`,
/// or one piece's worth when `length` is zero.
///
/// # Errors
///
/// A reference that is not one, content this store does not hold, an offset
/// past its end, or a piece longer than a frame carries.
fn piece(
    store: &ContentStore,
    content: &str,
    offset: u64,
    length: u64,
) -> Result<ContentPiece, ActionCallError> {
    let length = if length == 0 {
        MAX_CONTENT_FRAME_BYTES as u64
    } else {
        length
    };
    if length > MAX_CONTENT_FRAME_BYTES as u64 {
        return Err(ActionCallError::InvalidRequest {
            reason: format!("a piece carries at most {MAX_CONTENT_FRAME_BYTES} bytes"),
        });
    }
    let hash =
        tilo_actions::content_hash(content).ok_or_else(|| ActionCallError::InvalidRequest {
            reason: format!("{content} is not a tilo.content:<hash> reference"),
        })?;
    // Only the piece is read, sealed content opened from the chunk it
    // begins in: a caller downloading a file of hundreds of megabytes asks
    // for it a piece at a time, and reading the whole file for each piece
    // would read it hundreds of times.
    let (size, bytes) =
        store
            .read_range(hash, offset, length)
            .map_err(|_| ActionCallError::InvalidRequest {
                reason: format!("this workspace's store holds no {content}"),
            })?;
    if offset > size {
        return Err(ActionCallError::InvalidRequest {
            reason: format!("the content is {size} bytes: an offset of {offset} is past its end"),
        });
    }
    Ok(ContentPiece {
        size,
        offset,
        bytes,
    })
}

/// Puts into one workspace's store.
struct StorePuts(ContentStore);

impl ContentPuts for StorePuts {
    fn put_many(
        &self,
        _caller: &Caller,
        files: &[Vec<u8>],
    ) -> Result<Vec<String>, ActionCallError> {
        for bytes in files {
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PUT_BYTES {
                return Err(ActionCallError::InvalidRequest {
                    reason: format!(
                        "one file is at most {}: split the archive, or keep the piece the workspace needs",
                        ui::format::memory(MAX_PUT_BYTES)
                    ),
                });
            }
        }
        let resource = |hash: &str| {
            tilo_actions::content_resource(hash).ok_or_else(|| ActionCallError::Refused {
                reason: "the store named no content".into(),
            })
        };
        // A store that seals writes one item at a time; one that does not
        // keeps the batch in one write (one index update, one flush).
        if !self.0.seals() {
            let items: Vec<&[u8]> = files.iter().map(Vec::as_slice).collect();
            return self
                .0
                .put_public_many(&items)
                .into_iter()
                .map(|kept| {
                    let hash = kept.map_err(|error| ActionCallError::Refused {
                        reason: format!("the workspace could not keep the file: {error}"),
                    })?;
                    resource(&hash)
                })
                .collect();
        }
        let mut contents = Vec::with_capacity(files.len());
        for bytes in files {
            let hash = self
                .0
                .put(bytes)
                .map_err(|error| ActionCallError::Refused {
                    reason: format!("the workspace could not keep the file: {error}"),
                })?;
            contents.push(resource(&hash)?);
        }
        Ok(contents)
    }

    fn have(&self, _caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        Ok(hashes.iter().map(|hash| self.0.has(hash)).collect())
    }

    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        if size > MAX_PUT_BYTES {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "one file is at most {}: split the archive, or keep the piece the workspace needs",
                    ui::format::memory(MAX_PUT_BYTES)
                ),
            });
        }
        let spool = tempfile::tempfile().map_err(|error| ActionCallError::Unavailable {
            reason: format!("the host has nowhere to keep an upload: {error}"),
        })?;
        Ok(Box::new(StorePut {
            store: self.0.clone(),
            size,
            spool,
            written: 0,
        }))
    }
}

/// One upload: its bytes wait in a file of the host's until the last piece
/// arrives, so a file of hundreds of megabytes costs no memory. Dropped
/// unfinished, the file goes with it and nothing is stored.
struct StorePut {
    store: ContentStore,
    size: u64,
    spool: std::fs::File,
    written: u64,
}

impl ContentPut for StorePut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.written {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.written, self.written
                ),
            });
        }
        let after = self
            .written
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if after > self.size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload began with {} bytes", self.size),
            });
        }
        self.spool
            .write_all(chunk)
            .map_err(|error| ActionCallError::Refused {
                reason: format!("the host could not keep the upload's bytes: {error}"),
            })?;
        self.written = after;
        Ok(after)
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionCallError> {
        if self.written != self.size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the upload has {} of {} bytes: append the rest at offset {}",
                    self.written, self.size, self.written
                ),
            });
        }
        let kept = |error: String| ActionCallError::Refused {
            reason: format!("the workspace could not keep the file: {error}"),
        };
        // What the bytes are decides the media type the store keeps them
        // under: it is read from their first kilobyte, never from a name.
        let mut head = vec![0_u8; tilo_content_types::HEAD_BYTES];
        self.spool
            .seek(SeekFrom::Start(0))
            .map_err(|error| kept(error.to_string()))?;
        let read =
            read_up_to(&mut self.spool, &mut head).map_err(|error| kept(error.to_string()))?;
        head.truncate(read);
        let mime = tilo_content_types::sniff(&head).mime;
        self.spool
            .seek(SeekFrom::Start(0))
            .map_err(|error| kept(error.to_string()))?;
        let stored = self
            .store
            .put_reader(&mut self.spool, &mime)
            .map_err(|error| kept(error.to_string()))?;
        tilo_actions::content_resource(&stored.hash).ok_or_else(|| ActionCallError::Refused {
            reason: "the store named no content".into(),
        })
    }
}

/// Fill `into` from `file` until it is full or the file ends: how many bytes
/// were read.
fn read_up_to(file: &mut std::fs::File, into: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while let Some(unfilled) = into.get_mut(read..) {
        if unfilled.is_empty() {
            break;
        }
        match file.read(unfilled) {
            Ok(0) => break,
            Ok(bytes) => read = read.saturating_add(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}

/// A step of a caller's content, served off the service's thread.
struct Step {
    workspace: WorkspaceId,
    /// The upload the step belongs to, where it belongs to one: the steps of
    /// one upload keep the order they arrived in, which the offsets an upload
    /// appends at need.
    upload: Option<u64>,
    work: Box<dyn FnOnce() + Send>,
}

/// How many content steps are served at once across the service.
const THREADS: usize = 8;
/// How many content steps one workspace serves at once: enough that a client's
/// calls in flight do not wait on each other, small enough that one
/// workspace's burst leaves threads to the others, and that writes still
/// reach the disk the disk's own way.
const PER_WORKSPACE: usize = 4;
/// How long a thread waits for a step before it ends, so a service that
/// uploaded once keeps no thread for the rest of its life.
const IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// The threads content steps are served on, beside the service's own thread:
/// what was put into a workspace waits for neither the step before it nor the
/// workspace's actions, so what bounds an upload is the link or the disk.
///
/// A step is served once it has been checked against the caller's access there, so
/// no byte of one the workspace does not admit is taken. The steps of one
/// upload keep their order, and one workspace takes [`PER_WORKSPACE`] of them,
/// which leaves the other workspaces' steps threads of their own.
#[derive(Default)]
pub(super) struct ContentSteps {
    running: Mutex<Running>,
    /// Signals a thread that a step waits for it.
    ready: Condvar,
}

#[derive(Default)]
struct Running {
    /// The steps a thread may take now, oldest first.
    ready: VecDeque<Step>,
    /// The steps waiting for a place in their workspace, or for the step before
    /// them of the same upload, by workspace.
    parked: HashMap<WorkspaceId, VecDeque<Step>>,
    /// How many steps each workspace serves now.
    workspaces: HashMap<WorkspaceId, usize>,
    /// The uploads whose step is served now.
    uploads: HashSet<u64>,
    /// The threads started, idle or busy.
    threads: usize,
}

impl Running {
    /// Whether `upload`'s step in `workspace_id` may be served now.
    fn admits(&self, workspace_id: WorkspaceId, upload: Option<u64>) -> bool {
        let steps = self.workspaces.get(&workspace_id).copied().unwrap_or(0);
        steps < PER_WORKSPACE && upload.is_none_or(|upload| !self.uploads.contains(&upload))
    }

    /// Take `step` as running.
    fn hand(&mut self, step: Step) {
        let served = self.workspaces.entry(step.workspace).or_default();
        *served = served.saturating_add(1);
        if let Some(upload) = step.upload {
            self.uploads.insert(upload);
        }
        self.ready.push_back(step);
    }

    /// Let go of the step that ran for `workspace_id` and `upload`, and take
    /// in the steps it held back.
    fn finished(&mut self, workspace_id: WorkspaceId, upload: Option<u64>) {
        if let Some(steps) = self.workspaces.get_mut(&workspace_id) {
            *steps = steps.saturating_sub(1);
            if *steps == 0 {
                self.workspaces.remove(&workspace_id);
            }
        }
        if let Some(upload) = upload {
            self.uploads.remove(&upload);
        }
        let Some(mut parked) = self.parked.remove(&workspace_id) else {
            return;
        };
        while let Some(step) = parked.pop_front() {
            if !self.admits(step.workspace, step.upload) {
                parked.push_front(step);
                break;
            }
            self.hand(step);
        }
        if !parked.is_empty() {
            self.parked.insert(workspace_id, parked);
        }
    }

    /// Start as many threads as there are steps for, up to [`THREADS`].
    fn staff(&mut self, pool: &Arc<ContentSteps>) {
        while self.threads < THREADS && self.threads < self.ready.len() {
            self.threads = self.threads.saturating_add(1);
            let pool = Arc::clone(pool);
            let spawned = std::thread::Builder::new()
                .name("workspace-content".into())
                .spawn(move || pool.serve());
            if spawned.is_err() {
                self.threads = self.threads.saturating_sub(1);
                return;
            }
        }
    }
}

impl ContentSteps {
    /// Serve `work` for `workspace_id`, in the order its `upload`'s steps arrived
    /// where it names one, once a thread of `workspace_id`'s is free.
    pub(super) fn step(
        self: &Arc<Self>,
        workspace_id: WorkspaceId,
        upload: Option<u64>,
        work: impl FnOnce() + Send + 'static,
    ) {
        let step = Step {
            workspace: workspace_id,
            upload,
            work: Box::new(work),
        };
        let mut running = self.running.lock_recover();
        if running.admits(workspace_id, upload) {
            running.hand(step);
            running.staff(self);
        } else {
            running
                .parked
                .entry(workspace_id)
                .or_default()
                .push_back(step);
        }
        drop(running);
        self.ready.notify_all();
    }

    /// Take steps while there are any, and end once none comes for [`IDLE`].
    fn serve(self: &Arc<Self>) {
        loop {
            let mut running = self.running.lock_recover();
            while running.ready.is_empty() {
                let (held, waited) = self
                    .ready
                    .wait_timeout(running, IDLE)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                running = held;
                if running.ready.is_empty() && waited.timed_out() {
                    running.threads = running.threads.saturating_sub(1);
                    return;
                }
            }
            let Some(step) = running.ready.pop_front() else {
                return;
            };
            drop(running);
            (step.work)();
            self.running
                .lock_recover()
                .finished(step.workspace, step.upload);
            self.ready.notify_all();
        }
    }
}

impl WorkspaceActions {
    /// Answer `step` of an upload `caller` asks of the host in
    /// `workspace_id`. The host's own person and their agents put files; a
    /// member, or an agent of theirs, puts them where their access lets
    /// them change the workspace's records, which is read before a byte of
    /// the step is taken. Its later steps go to the upload they began.
    ///
    /// Once the workspace's store is found the step is served beside the
    /// service's thread ([`ContentSteps`]); the first step, which finds the
    /// store, is the service thread's.
    pub(super) fn content_put(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        correlation: u64,
        step: ContentPutStep,
    ) -> Pending<RemoteActionMessage> {
        let respond = move |result| RemoteActionMessage::ContentPutResponse {
            correlation,
            result,
        };
        let check = put_check(&step).filter(|_| !self.authorized(caller));
        if check.is_some() && !super::speaks_for_a_member(caller) {
            return Pending::ready(respond(Err(ActionCallError::NotPermitted)));
        }
        if let Some(store) = self.known_store(workspace_id) {
            let (done, answer) = pending();
            let caller = caller.clone();
            let shared = Arc::clone(&self.shared);
            self.shared
                .steps
                .step(workspace_id, upload_of(&step), move || {
                    done.complete(put_into(
                        &shared,
                        workspace_id,
                        &store,
                        &caller,
                        check,
                        step,
                    ));
                });
            return answer.map(respond);
        }
        let (done, answer) = pending();
        let job = Job::Content {
            workspace_id,
            caller: caller.clone(),
            step,
            done,
        };
        if let Err(Job::Content { done, .. }) = self.submit(job) {
            done.complete(Err(ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(respond)
    }

    /// Answer `caller`'s request for a piece of `content` from the content
    /// store of the host's workspace `workspace_id`: the host's own person
    /// and their agents, and anyone whose member access there lets them read it,
    /// which is read before a byte of it is read out.
    pub(super) fn content_get(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        correlation: u64,
        (content, offset, length): (String, u64, u64),
    ) -> Pending<RemoteActionMessage> {
        let respond = move |result| RemoteActionMessage::ContentGetResponse {
            correlation,
            result,
        };
        let check = (!self.authorized(caller)).then_some(ActionKind::Read);
        if check.is_some() && !super::speaks_for_a_member(caller) {
            return Pending::ready(respond(Err(ActionCallError::NotPermitted)));
        }
        if let Some(store) = self.known_store(workspace_id) {
            let (done, answer) = pending();
            let caller = caller.clone();
            let shared = Arc::clone(&self.shared);
            self.shared.steps.step(workspace_id, None, move || {
                done.complete(get_from(
                    &shared,
                    workspace_id,
                    &store,
                    &caller,
                    check,
                    (&content, offset, length),
                ));
            });
            return answer.map(respond);
        }
        let (done, answer) = pending();
        let job = Job::ContentGet {
            workspace_id,
            caller: caller.clone(),
            content,
            offset,
            length,
            done,
        };
        if let Err(Job::ContentGet { done, .. }) = self.submit(job) {
            done.complete(Err(ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(respond)
    }

    /// Answer `caller`'s request for a link to content of the host's
    /// workspace `workspace_id`: anyone who may read it, as their access
    /// there is read before the link is made.
    pub(super) fn content_link(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        correlation: u64,
        link: tilo_actions::remote::ContentLinkAsk,
    ) -> Pending<RemoteActionMessage> {
        let respond = move |result| RemoteActionMessage::ContentLinkResponse {
            correlation,
            result,
        };
        let check = (!self.authorized(caller)).then_some(ActionKind::Read);
        if check.is_some() && !super::speaks_for_a_member(caller) {
            return Pending::ready(respond(Err(ActionCallError::NotPermitted)));
        }
        if let Err(reason) = link.check() {
            return Pending::ready(respond(Err(ActionCallError::InvalidRequest { reason })));
        }
        if let Some(store) = self.known_store(workspace_id) {
            let (done, answer) = pending();
            let caller = caller.clone();
            let shared = Arc::clone(&self.shared);
            self.shared.steps.step(workspace_id, None, move || {
                done.complete(link_into(
                    &shared,
                    workspace_id,
                    &store,
                    &caller,
                    check,
                    &link,
                ));
            });
            return answer.map(respond);
        }
        let (done, answer) = pending();
        let job = Job::ContentLink {
            workspace_id,
            caller: caller.clone(),
            link,
            done,
        };
        if let Err(Job::ContentLink { done, .. }) = self.submit(job) {
            done.complete(Err(ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(respond)
    }

    /// `workspace_id`'s store, once the service's thread found it.
    fn known_store(&self, workspace_id: WorkspaceId) -> Option<ContentStore> {
        self.shared
            .stores
            .lock_recover()
            .get(&workspace_id)
            .cloned()
    }
}

/// What a put step needs the caller to be allowed across the workspace, when
/// it changes the workspace or asks what it holds: the steps of an upload
/// already begun were checked when it began.
fn put_check(step: &ContentPutStep) -> Option<ActionKind> {
    match step {
        ContentPutStep::Begin { .. } | ContentPutStep::Many { .. } => Some(ActionKind::Undoable),
        ContentPutStep::Have { .. } => Some(ActionKind::Read),
        ContentPutStep::Append { .. }
        | ContentPutStep::Finish { .. }
        | ContentPutStep::Progress { .. } => None,
    }
}

/// The upload `step` belongs to, where it belongs to one.
fn upload_of(step: &ContentPutStep) -> Option<u64> {
    match step {
        ContentPutStep::Append { upload, .. }
        | ContentPutStep::Finish { upload }
        | ContentPutStep::Progress { upload } => Some(*upload),
        ContentPutStep::Begin { .. }
        | ContentPutStep::Many { .. }
        | ContentPutStep::Have { .. } => None,
    }
}

/// [`Worker::content_put`]'s step, on a content thread: the caller's access
/// read before a byte of the step is taken.
fn put_into(
    shared: &Shared,
    workspace_id: WorkspaceId,
    store: &ContentStore,
    caller: &Caller,
    check: Option<ActionKind>,
    step: ContentPutStep,
) -> Result<tilo_actions::remote::ContentPutAnswer, ActionCallError> {
    admitted(shared, workspace_id, caller, check)?;
    shared.puts.step(&StorePuts(store.clone()), caller, step)
}

/// [`Worker::content_get`]'s piece, on a content thread.
fn get_from(
    shared: &Shared,
    workspace_id: WorkspaceId,
    store: &ContentStore,
    caller: &Caller,
    check: Option<ActionKind>,
    (content, offset, length): (&str, u64, u64),
) -> Result<ContentGetAnswer, ActionCallError> {
    admitted(shared, workspace_id, caller, check)?;
    ContentGetAnswer::new(piece(store, content, offset, length)?)
}

/// [`Worker::content_link`]'s link, on a content thread.
fn link_into(
    shared: &Shared,
    workspace_id: WorkspaceId,
    store: &ContentStore,
    caller: &Caller,
    check: Option<ActionKind>,
    link: &tilo_actions::remote::ContentLinkAsk,
) -> Result<tilo_actions::remote::ContentLink, ActionCallError> {
    admitted(shared, workspace_id, caller, check)?;
    let unavailable = || ActionCallError::Unavailable {
        reason: tilo_actions::remote::CONTENT_LINKS_UNAVAILABLE.into(),
    };
    shared
        .link
        .get()
        .and_then(|host| host.content_link(workspace_id, store.clone(), link))
        .ok_or_else(unavailable)
}

/// What a step the workspace does not admit is answered.
fn admitted(
    shared: &Shared,
    workspace_id: WorkspaceId,
    caller: &Caller,
    check: Option<ActionKind>,
) -> Result<(), ActionCallError> {
    match check {
        Some(kind) if !shared.may_across(workspace_id, caller, kind) => {
            Err(ActionCallError::NotPermitted)
        }
        _ => Ok(()),
    }
}

impl Worker {
    /// `workspace_id`'s content store on this host, found once.
    pub(super) fn workspace_content(&self, workspace_id: WorkspaceId) -> Option<ContentStore> {
        if let Some(store) = self.shared.stores.lock_recover().get(&workspace_id) {
            return Some(store.clone());
        }
        let store = self.shared.link.get()?.workspace_content(workspace_id)?;
        self.shared
            .stores
            .lock_recover()
            .insert(workspace_id, store.clone());
        Some(store)
    }

    /// Answer `step` of `caller`'s upload into `workspace_id`'s store: one
    /// begins only where they may change the workspace's records. The step a
    /// workspace's first request carries, which finds its store.
    pub(super) fn content_put(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        step: ContentPutStep,
    ) -> Result<tilo_actions::remote::ContentPutAnswer, ActionCallError> {
        admitted(&self.shared, workspace_id, caller, put_check(&step))?;
        let store =
            self.workspace_content(workspace_id)
                .ok_or_else(|| ActionCallError::Unavailable {
                    reason: tilo_actions::remote::CONTENT_PUTS_UNAVAILABLE.into(),
                })?;
        self.shared.puts.step(&StorePuts(store), caller, step)
    }

    /// The piece of `content` `caller` asked of `workspace_id`'s store,
    /// where they may read the workspace.
    pub(super) fn content_get(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        content: &str,
        (offset, length): (u64, u64),
    ) -> Result<ContentGetAnswer, ActionCallError> {
        admitted(&self.shared, workspace_id, caller, Some(ActionKind::Read))?;
        let store =
            self.workspace_content(workspace_id)
                .ok_or_else(|| ActionCallError::Unavailable {
                    reason: CONTENT_GETS_UNAVAILABLE.into(),
                })?;
        ContentGetAnswer::new(piece(&store, content, offset, length)?)
    }

    /// A link to `link`'s files in `workspace_id`'s store, where `caller`
    /// may read the workspace.
    pub(super) fn content_link(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        link: &tilo_actions::remote::ContentLinkAsk,
    ) -> Result<tilo_actions::remote::ContentLink, ActionCallError> {
        admitted(&self.shared, workspace_id, caller, Some(ActionKind::Read))?;
        let unavailable = || ActionCallError::Unavailable {
            reason: tilo_actions::remote::CONTENT_LINKS_UNAVAILABLE.into(),
        };
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(unavailable)?;
        self.shared
            .link
            .get()
            .and_then(|host| host.content_link(workspace_id, store, link))
            .ok_or_else(unavailable)
    }

    /// `replica`, a record of the workspace `workspace` names, lent the workspace's members when
    /// this host joined it and its content store when this host keeps one.
    pub(super) fn lend(&self, replica: ConfinedReplica, workspace: &str) -> ConfinedReplica {
        let id = WorkspaceId::parse(workspace);
        let replica = match id.and_then(|id| self.workspaces.get(&id)) {
            Some(held) => replica.knowing(std::sync::Arc::clone(held.peer.engine())),
            None => replica,
        };
        match id.and_then(|id| self.workspace_content(id)) {
            Some(store) => replica.lending(store),
            None => replica,
        }
    }
}

/// Shared between the service's handle and its thread.
pub(super) type Stores = std::sync::Mutex<std::collections::HashMap<WorkspaceId, ContentStore>>;

#[cfg(test)]
#[path = "content_upload_tests.rs"]
mod upload_tests;
