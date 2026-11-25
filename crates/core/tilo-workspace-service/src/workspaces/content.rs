//! Content a hosted workspace's records reference that this host keeps: what
//! its bots fetched from outside (a bridged person's avatar, a file posted
//! elsewhere), the surfaces of the apps it decided changes with, whichever
//! version those changes were written by, which members decide the host's
//! changes against, and the files of a workspace its members wrote and left
//! ([`crate::workspaces::ContentKeeper`]).
//!
//! A member asks the workspace for content its record lacks with the workspace's
//! presence ([`NeighbourMsg::DocumentContentWants`]), as it asks any member.
//! The host answers from everything it keeps, to that member's device alone, in
//! the same bounded pieces a member answers with; the member keeps the
//! content once every piece arrived and the bytes hash to what it asked.
//!
//! The host asks too: it keeps a copy of the files members post
//! ([`ContentKeeper`]), so they stay in the workspace after the member who
//! posted them leaves, and of the app surfaces a change it must decide names
//! and it lacks ([`super::surfaces`]). It asks the workspace on its own
//! presence, and the pieces members answer it with come to it, not to another
//! device.
//!
//! Several keepers keep for a host at once (its bots' files, its workspaces'
//! files), each of them saying which of the pieces members answer the host
//! with are its own; and several sources serve, each answering with the
//! content it keeps and leaving the rest to the others.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock, Weak};

use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::ContentWants;

use super::{CollaborationError, Workspaces};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::WorkspaceId;

/// Where a host's content comes from.
pub trait WorkspaceContent: Send + Sync {
    /// The bytes named `hash` (lowercase BLAKE3 hex) that a record of `app`
    /// in `workspace` may reference, when this host keeps them.
    fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>>;
}

/// Content a host stopped keeping because a limit said so: what the member
/// who posted it reads, naming the limit and who can raise it. The record
/// itself is untouched: it syncs between the members' devices as any does.
///
/// A refusal is a record of the workspace, not a host's memory
/// ([`collab_types::CONTENT_REFUSAL_KIND`]): the host writes it as its own
/// record and reads it back from there, so a host that restarts refuses the
/// same content no second time. Its fields are the record's.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentRefusal {
    /// The address of the record whose content it is, `app/document`.
    pub record: String,
    /// The content's address, lowercase BLAKE3 hex.
    pub hash: String,
    /// How many bytes it is.
    pub bytes: u64,
    /// The limit it went over, and what that limit allowed.
    pub limit: tilo_action_policy::ContentLimit,
    pub allowed: u64,
    /// The member who posted it, by person id: what a reader compares theirs
    /// with to decide the refusal is theirs.
    pub member: String,
    /// When it was refused, milliseconds since the Unix epoch.
    pub at: u64,
    /// Who can raise the limit.
    pub raise: tilo_action_policy::Raises,
}

impl ContentRefusal {
    /// The address of `app`'s `record`: what a refusal names the record whose
    /// content it is by.
    #[must_use]
    pub fn address(app: &str, record: &str) -> String {
        collab_types::app_document_id(app, record)
    }

    /// The app of the record whose content it is.
    #[must_use]
    pub fn app(&self) -> &str {
        self.record.split_once('/').map_or("", |(app, _)| app)
    }

    /// The record itself, of its app: the address less the app.
    #[must_use]
    pub fn named(&self) -> &str {
        self.record.split_once('/').map_or("", |(_, named)| named)
    }
}

/// Where content members answer the host with goes.
pub trait ContentKeeper: Send + Sync {
    /// Take `message`, a content frame the member device `from` sent this
    /// host in `workspace` unasked: a piece of content it pushes, or word that
    /// it lets go of content it pushed before. `message` comes back when this
    /// keeper takes none of it. The frame is decoded once: a large tree's
    /// pieces arrive by the thousand.
    ///
    /// # Errors
    ///
    /// `message`, handed back, when this keeper takes none of it.
    fn take(
        &self,
        workspace: &str,
        from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>>;

    /// What `workspace` keeps here that its members pushed: how many contents
    /// a member holds (`held`), and how many are on their way (`going`).
    fn held(&self, workspace: &str) -> ContentHeld {
        let _ = workspace;
        ContentHeld::default()
    }

    /// The content this keeps of `workspace` for the members who pushed it,
    /// each with its app: what this device hands a server taking the
    /// workspace over.
    fn holdings(&self, workspace: &str) -> Vec<(String, String)> {
        let _ = workspace;
        Vec::new()
    }

    /// The content of `workspace` this keeper stopped keeping because a limit
    /// said so: what the member who posted it reads, naming the limit and who
    /// can raise it.
    fn refused<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<ContentRefusal>> + Send + 'a>> {
        let _ = workspace;
        Box::pin(std::future::ready(Vec::new()))
    }
}

/// What a host keeps of a workspace's pushed content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ContentHeld {
    /// Contents a member holds here.
    pub held: usize,
    /// Contents a member is pushing here and the host does not keep whole yet.
    pub going: usize,
}

/// The content source a host's workspaces answer from, and the keeper of what
/// members answer the host with, once it has them.
#[derive(Default)]
pub(super) struct ContentSlot {
    pub(super) sources: RwLock<Vec<Arc<dyn WorkspaceContent>>>,
    /// Held weakly: a keeper keeps the workspaces that answer members' pieces
    /// (a host's files do), so a strong handle here would keep the keeper and
    /// the workspaces alive — and the host's endpoint with them — for as long
    /// as the process runs.
    pub(super) keepers: RwLock<Vec<Weak<dyn ContentKeeper>>>,
    /// The app surfaces this host asked the workspace for, by hash
    /// ([`Workspaces::ask_surface`]).
    pub(super) surfaces: Mutex<ContentWants<()>>,
    /// The surfaces the host lacks and asks for until they arrive, by hash:
    /// the workspace and app each is of.
    pub(super) missing: Mutex<HashMap<[u8; 32], (String, String)>>,
    /// The asks this device answered with word that it keeps none of the
    /// content ([`Workspaces::content_answer`]): answered again once it keeps
    /// it ([`Workspaces::content_kept`]).
    pub(super) owed: Mutex<OwedAsks>,
    /// The content this device pushed to its workspaces' hosts.
    pub(super) pushes: super::pushes::Pushes,
}

/// Asks owed, at most this many contents at once; past it, the oldest go.
/// A tree's whole content can be asked for at once, before the device that
/// wrote it keeps the bytes where it serves them from.
const MAX_OWED: usize = 16_384;

/// The asks this device owes an answer, by workspace and content hash: each
/// asking device, with the ask it sent.
#[derive(Default)]
pub(super) struct OwedAsks {
    asks: BTreeMap<(String, String), (u64, Askers)>,
    made: u64,
}

/// Each asking device, by key, with the ask it sent.
type Askers = BTreeMap<[u8; 32], Vec<u8>>;

impl OwedAsks {
    fn owe(&mut self, workspace: &str, hash: String, asker: [u8; 32], ask: &[u8]) {
        let key = (workspace.to_owned(), hash);
        if !self.asks.contains_key(&key)
            && self.asks.len() >= MAX_OWED
            && let Some(oldest) = self
                .asks
                .iter()
                .min_by_key(|(_, (order, _))| *order)
                .map(|(key, _)| key.clone())
        {
            self.asks.remove(&oldest);
        }
        self.made = self.made.wrapping_add(1);
        let made = self.made;
        self.asks
            .entry(key)
            .or_insert_with(|| (made, BTreeMap::new()))
            .1
            .insert(asker, ask.to_vec());
    }

    fn take(&mut self, workspace: &str, hash: &str) -> BTreeMap<[u8; 32], Vec<u8>> {
        self.asks
            .remove(&(workspace.to_owned(), hash.to_ascii_lowercase()))
            .map(|(_, asks)| asks)
            .unwrap_or_default()
    }
}

impl ContentSlot {
    fn get(&self) -> Vec<Arc<dyn WorkspaceContent>> {
        self.sources.read_recover().clone()
    }

    pub(super) fn keepers(&self) -> Vec<Arc<dyn ContentKeeper>> {
        self.keepers
            .read_recover()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl Workspaces {
    /// Answer members' requests for content from `content`, beside whatever
    /// else this host serves.
    pub fn serve_content(&self, content: Arc<dyn WorkspaceContent>) {
        let mut sources = self.inner.content.sources.write_recover();
        if !sources.iter().any(|served| Arc::ptr_eq(served, &content)) {
            sources.push(content);
        }
    }

    /// `workspace`'s name as this host's listings and files spell it; `None`
    /// for one not hosted here.
    pub async fn workspace_identity(&self, workspace: WorkspaceId) -> Option<String> {
        self.held(workspace).await.map(|_| workspace.to_string())
    }

    /// The workspace hosted here under `workspace`: the host keeps each
    /// under its id spelled out, so this is one lookup, never a walk.
    pub(super) async fn held(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Arc<tokio::sync::Mutex<super::HostedWorkspace>>> {
        let held = self.inner.workspaces.lock().await;
        held.get(&workspace.to_string()).cloned()
    }

    /// The name of the workspace hosted here whose channels are
    /// `workspace`; `None` for one not hosted here.
    pub async fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        let hosted = self.held(workspace).await?;
        let name = hosted.lock().await.name();
        Some(name)
    }

    /// The name the workspace hosted here under `workspace` shows for the
    /// member whose root is `root`; `None` for one not hosted here or a
    /// member who chose no name.
    pub async fn name_of_member(&self, workspace: WorkspaceId, root: [u8; 32]) -> Option<String> {
        let hosted = self.held(workspace).await?;
        let hosted = hosted.lock().await;
        self.member_name_by_root(&hosted, root)
    }

    /// The bytes named `hash` this host keeps for the workspace of `workspace`
    /// hosted here, checked against the hash: what its bots fetched and
    /// the files it keeps for members. `None` for a workspace not hosted here
    /// or content it does not keep.
    pub async fn kept_content(&self, workspace: WorkspaceId, hash: &str) -> Option<Vec<u8>> {
        self.kept_record_content(workspace, "", hash).await
    }

    /// The bytes named `hash` this host keeps for the records of `app` in the workspace of
    /// `workspace`, checked against the hash: what a member reading one of them is answered.
    pub async fn kept_record_content(
        &self,
        workspace: WorkspaceId,
        app: &str,
        hash: &str,
    ) -> Option<Vec<u8>> {
        let identity = self.workspace_identity(workspace).await?;
        let sources = self.inner.content.get();
        sources.iter().find_map(|source| {
            source
                .read(&identity, app, hash)
                .filter(|bytes| tilo_content::verify(hash, bytes))
        })
    }

    /// The bytes `hash` names that this host keeps for the records of `app`
    /// in `workspace`, checked against the hash: what a record reading one of
    /// them is answered, without the workspace being looked up first.
    pub(crate) fn kept_record_bytes(
        &self,
        workspace: &str,
        app: &str,
        hash: &str,
    ) -> Option<Vec<u8>> {
        let sources = self.inner.content.get();
        sources.iter().find_map(|source| {
            source
                .read(workspace, app, hash)
                .filter(|bytes| tilo_content::verify(hash, bytes))
        })
    }

    /// Every record of `workspace` the host's copy keeps (or of `only`
    /// among them), each saved with all of its history: what tells a host
    /// which content the workspace still references, now or in a version it
    /// can go back to. Read from a copy of the host's state, taken in a
    /// moment: the workspace takes writes while its records are read.
    /// A record that cannot be read whole (sealed for a channel this host has
    /// no keys of, or with sealed writes that do not open or merge yet) is
    /// logged with why and listed in [`WholeDocuments::unread`], and the rest
    /// are read all the same: a caller that must see every reference checks
    /// [`WholeDocuments::is_whole`]. A person's own record sealed for their
    /// devices alone is listed in [`WholeDocuments::private`] instead.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn whole_documents(
        &self,
        workspace: &str,
        only: Option<&BTreeSet<(String, String)>>,
    ) -> Result<WholeDocuments, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let (peer, device) = {
            let hosted = state.lock().await;
            let peer = hosted.peer().clone();
            (peer, hosted.device.clone())
        };
        let routes: Vec<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|route| only.is_none_or(|only| only.contains(route)))
            .collect();
        let mut read = WholeDocuments::default();
        for (app, record) in routes {
            let document = super::sealed_documents::whole_document(
                &peer,
                device.as_ref(),
                workspace,
                (&app, &record),
            )
            .await;
            let document = document.and_then(|document| {
                peer.document_revision(&app, &record)
                    .map(|revision| (document, revision))
                    .ok_or_else(|| "it has no revision yet".to_owned())
            });
            match document {
                Ok((document, revision)) => {
                    read.revisions
                        .insert((app.clone(), record.clone()), revision);
                    read.documents.push(((app, record), document));
                }
                // A person's private record sealed for their devices alone
                // (their drafts) is not the host's to read: it is named apart,
                // not as unread.
                Err(why)
                    if tilo_action_policy::roles::is_private_record(&app, &record)
                        && peer.publication().is_sealed_document(&app, &record) =>
                {
                    tracing::debug!(workspace, %app, %record, %why, "a person's own record is not the host's to read");
                    read.private.push((app, record));
                }
                Err(why) => {
                    tracing::warn!(
                        workspace,
                        %app,
                        %record,
                        %why,
                        "a record of the workspace does not read whole"
                    );
                    read.unread.push(((app, record), why));
                }
            }
        }
        // The records the workspace's store keeps, each with all of its history.
        let mut hosted = state.lock().await;
        for (route, _, document) in self.stored_documents(&mut hosted, only, true) {
            match document {
                Ok((revision, saved)) => {
                    read.revisions.insert(route.clone(), revision);
                    read.documents.push((route, saved));
                }
                Err(why) => read.unread.push((route, why)),
            }
        }
        Ok(read)
    }

    /// Every record `hosted`'s store keeps (or of `only` among them), by its app and id, with
    /// its kind, the revision it is at and, when `saved`, the record saved with all of its
    /// history.
    pub(super) fn stored_documents(
        &self,
        hosted: &mut super::HostedWorkspace,
        only: Option<&BTreeSet<(String, String)>>,
        saved: bool,
    ) -> Vec<StoredDocument> {
        let apps: BTreeMap<String, String> = self
            .inner
            .host
            .surfaces
            .kinds()
            .into_iter()
            .map(|(app, kind)| (kind, app))
            .collect();
        let mut store = self.store_of(hosted);
        // Every record the store holds, whether or not an app this device knows declares its
        // kind: a record of an app not installed here names content all the same.
        let index = store.index();
        let held: BTreeSet<(String, String)> = index
            .documents()
            .flat_map(|doc| index.held(doc))
            .map(|(kind, id)| (kind.to_owned(), id.to_owned()))
            .collect();
        let mut documents = Vec::new();
        for (kind, id) in held {
            let app = apps
                .get(&kind)
                .cloned()
                .or_else(|| collab_types::app_surface::app_of_kind(&kind))
                .unwrap_or_default();
            let route = (app, id);
            if only.is_some_and(|only| !only.contains(&route)) {
                continue;
            }
            let document = match store.content_document(&kind, &route.1) {
                Ok(Some(document)) => {
                    let bytes = if saved { document.save() } else { Vec::new() };
                    Ok((super::reads::record_revision(&document.get_heads()), bytes))
                }
                Ok(None) => continue,
                Err(error) => Err(error.to_string()),
            };
            documents.push((route, kind, document));
        }
        drop(store);
        documents
    }

    /// Run `act` while `workspace` takes no write, if none of the records
    /// `read` read has changed since and none was added: `Ok(Ok(_))` with
    /// what it answered, or `Ok(Err(_))` with the records that changed or
    /// appeared, for the caller to read again. The workspace waits only as
    /// long as `act` takes.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn while_unwritten<R>(
        &self,
        workspace: &str,
        read: &WholeDocuments,
        act: impl FnOnce() -> R,
    ) -> Result<Result<R, BTreeSet<(String, String)>>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let mut hosted = state.lock().await;
        // A record of the store that changed since it was read.
        let stored: Vec<(String, String)> = self
            .stored_documents(&mut hosted, None, false)
            .into_iter()
            .filter(|(route, _, document)| {
                document.as_ref().ok().map(|(revision, _)| revision) != read.revisions.get(route)
            })
            .map(|(route, _, _)| route)
            .collect();
        // Members' writes are taken into the replica under its own lock: held
        // too, nothing lands while `act` runs.
        let peer = hosted.peer();
        let changed: BTreeSet<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|(app, record)| {
                read.revisions.get(&(app.clone(), record.clone()))
                    != peer.document_revision(app, record).as_ref()
            })
            .chain(stored)
            .collect();
        let answer = if changed.is_empty() {
            Ok(act())
        } else {
            Err(changed)
        };
        drop(peer);
        drop(hosted);
        Ok(answer)
    }

    /// Hand the pieces of content members send this host to `keeper`, beside
    /// whatever else this host keeps. The keeper is not held: whoever runs it
    /// owns it, and its pieces stop coming here when it goes.
    pub fn keep_content(&self, keeper: &Arc<dyn ContentKeeper>) {
        let mut keepers = self.inner.content.keepers.write_recover();
        keepers.retain(|kept| kept.strong_count() > 0);
        let keeper = Arc::downgrade(keeper);
        if !keepers.iter().any(|kept| Weak::ptr_eq(kept, &keeper)) {
            keepers.push(keeper);
        }
    }

    /// `frame`, a piece of content the member device `from` sent this host
    /// in `workspace`: to the keeper that asked for it, or, when it is a
    /// surface the host asked for, into the content store the host decides
    /// with. Dropped when the host expects neither.
    pub fn content_for_host(&self, workspace: &str, from: [u8; 32], frame: &[u8]) {
        match NeighbourMsg::decode(frame) {
            Ok(message) => self.content_message_for_host(workspace, from, message),
            Err(error) => {
                tracing::warn!(workspace, from = %hex::encode(from), %error, "a content frame from a member did not decode");
            }
        }
    }

    /// One link's intake of what members answer this host's asks with in
    /// `workspace` ([`Self::content_for_host`]): the answers are taken in one
    /// run at a time, in the order the link carried them, each run on a
    /// blocking thread with every answer that came while the run before it was
    /// taken in, and the link's reader waits while the intake is two answers
    /// behind. A fast link is held back by the link's own flow control, and
    /// never by a crowd of threads each waiting on the keeper with one piece.
    ///
    /// No thread is held while no answer waits: a blocking thread kept for the
    /// link's life is work the runtime never sees end, and a runtime whose
    /// clock is paused would never move it on.
    pub(super) fn content_intake(
        &self,
        workspace: &str,
    ) -> tokio::sync::mpsc::Sender<([u8; 32], NeighbourMsg)> {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel(2 * neighbour_wire::content::PIECES_PER_ANSWER);
        let workspaces = self.downgrade();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            while let Some(first) = receiver.recv().await {
                let mut run = vec![first];
                while let Ok(next) = receiver.try_recv() {
                    run.push(next);
                }
                // The host is gone: so is what it asked for.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let workspace = workspace.clone();
                let taken = tokio::task::spawn_blocking(move || {
                    for (from, message) in run {
                        workspaces.content_message_for_host(&workspace, from, message);
                    }
                })
                .await;
                match taken.map_err(tokio::task::JoinError::try_into_panic) {
                    Ok(()) => {}
                    Err(Ok(panic)) => std::panic::resume_unwind(panic),
                    // The runtime is shutting down, and the link with it.
                    Err(Err(_)) => return,
                }
            }
        });
        sender
    }

    /// One link's answers to what members ask this device for in `workspace`
    /// ([`Self::content_answer`]): each ask is read once the link to its asker
    /// has room for its answer, so the link decides how many are read at once
    /// ([`tilo_peer_state::direct::DirectSync::content_room`]). One by one,
    /// every ask waited for each read before it, and so did its asker.
    pub(super) fn content_asks(
        &self,
        workspace: &str,
        link: &tilo_peer_state::direct::DirectSync,
    ) -> tokio::sync::mpsc::Sender<([u8; 32], Vec<u8>)> {
        // The reader hands over one ask at a time: an ask waiting for room
        // holds the reader, and so the link's other frames, no longer than that.
        let (sender, mut asks) = tokio::sync::mpsc::channel::<([u8; 32], Vec<u8>)>(1);
        let workspaces = self.downgrade();
        let (workspace, link) = (workspace.to_owned(), link.clone());
        tokio::spawn(async move {
            while let Some((asker, body)) = asks.recv().await {
                let Some(room) = link.content_room(asker).await else {
                    continue;
                };
                // The host is gone: so is what it was asked.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let (workspace, link) = (workspace.clone(), link.clone());
                tokio::spawn(async move {
                    // A mounted workspace's file bytes are read from its Files daemon: the read
                    // waits on a thread of its own, never on the runtime's. Each frame goes out
                    // once made, so the first of a large answer travels while the rest is read.
                    let (frames, mut made) = tokio::sync::mpsc::channel::<NeighbourMsg>(2);
                    let read = tokio::task::spawn_blocking(move || {
                        workspaces.content_answer_each(&workspace, &asker, &body, |message| {
                            // The asker's link ended: the rest of the answer has nowhere to go.
                            let _ = frames.blocking_send(message);
                        });
                    });
                    let mut room = Some(room);
                    while let Some(message) = made.recv().await {
                        // The first answer goes in the room the ask took; the
                        // rest wait while the asker's link is full.
                        let sent = match room.take() {
                            Some(room) => link.send_content_in(room, message).await,
                            None => link.send_content(message).await,
                        };
                        if let Err(error) = sent {
                            tracing::debug!(%error, "a content answer did not reach its asker");
                        }
                    }
                    drop(room);
                    if let Err(error) = read.await {
                        tracing::warn!(%error, "reading what a member asked for ended early");
                    }
                });
            }
        });
        sender
    }

    /// [`Self::content_for_host`] for a message already decoded.
    fn content_message_for_host(&self, workspace: &str, from: [u8; 32], message: NeighbourMsg) {
        if let NeighbourMsg::DocumentContentKept {
            requester,
            cluster_id,
            hash,
            ..
        } = &message
        {
            if *requester == tilo_peer_state::peer_handle(&self.device_cert().device_public_key)
                && cluster_id == workspace
            {
                self.content_pushed(workspace, hash);
            }
            return;
        }
        if let NeighbourMsg::DocumentContentRefused {
            requester,
            cluster_id,
            hash,
            ..
        } = &message
            && *requester == tilo_peer_state::peer_handle(&self.device_cert().device_public_key)
            && cluster_id == workspace
            && self.push_refused(workspace, hash)
        {
            return;
        }
        self.keep_content_for_host(workspace, from, message);
    }

    /// Hand `message` to whoever asked for what it brings
    /// ([`Self::content_for_host`]).
    fn keep_content_for_host(&self, workspace: &str, from: [u8; 32], message: NeighbourMsg) {
        // The asker of a surface is this host: a piece is kept when it is
        // addressed to the host's own device, whoever among the members
        // answered with it. Everything else addressed to it was pushed here.
        let device = self.device_cert().device_public_key;
        let mut surfaces = self.inner.content.surfaces.lock_recover();
        if !surfaces.expects_in(&device, &message) && !surfaces.awaits_in(&device, &message) {
            drop(surfaces);
            let mut message = message;
            for keeper in self.inner.content.keepers() {
                match keeper.take(workspace, from, message) {
                    Ok(()) => return,
                    Err(back) => message = *back,
                }
            }
            return;
        }
        let assembled = surfaces.assemble(&device, message, tilo_clock::unix_now_ms());
        drop(surfaces);
        for arrived in assembled
            .into_iter()
            .filter_map(neighbour_wire::content::Assembled::verified)
        {
            crate::surfaces::keep(&arrived.hash, &arrived.bytes);
        }
    }

    /// The content of `workspace` this host stopped keeping because a limit
    /// said so: each refusal names the limit, what it allowed, and who can
    /// raise it, so the member who posted it reads why.
    pub async fn content_refused(&self, workspace: &str) -> Vec<ContentRefusal> {
        let mut refused = Vec::new();
        for keeping in self.inner.content.keepers() {
            refused.extend(keeping.refused(workspace).await);
        }
        refused
    }

    /// What `workspace` serves as its content root (`workspace.content`) to `reader`: the owners'
    /// limits on what members post there — 0 where nobody set one — and the refusals `reader`
    /// reads, their own, the workspace's owners' and the host's person's. The device hands this
    /// to each of its windows' processes, which serve it as the Workspace package's root.
    pub async fn content_reading(&self, workspace: &str, reader: [u8; 32]) -> serde_json::Value {
        let policy = self.content_policy(workspace).await.unwrap_or_default();
        // A limit raised since refuses nothing: its record stays until the content it refused is
        // kept, and stands for no reader meanwhile, as much as the owners' policy alone can say
        // (a member's share, their hour and the host's budget are about content together, which
        // the host that keeps it weighs against its totals).
        let limits = tilo_action_policy::KeptLimits::of(&policy, None);
        let refusals: Vec<serde_json::Value> = self
            .refusals_for(workspace, reader)
            .await
            .iter()
            .filter(|refusal| limits.refuses(refusal.limit, refusal.bytes))
            .filter_map(|refusal| serde_json::to_value(refusal.read()).ok())
            .collect();
        serde_json::json!({
            "file_bytes": policy.file_bytes.unwrap_or(0),
            "member_bytes": policy.member_bytes.unwrap_or(0),
            "hourly_bytes": policy.hourly_bytes.unwrap_or(0),
            "refusals": refusals,
            // This device's person reads the refusals a host keeps too: the windows that serve
            // this reading are theirs, and the reader filter above already kept only theirs.
            "host_person": hex::encode(self.device_person()),
        })
    }

    /// What `workspace` keeps here that its members pushed, as what keeps it
    /// counts.
    #[must_use]
    pub fn content_held(&self, workspace: &str) -> ContentHeld {
        self.inner
            .content
            .keepers()
            .iter()
            .map(|keeping| keeping.held(workspace))
            .fold(ContentHeld::default(), |all, one| ContentHeld {
                held: all.held.saturating_add(one.held),
                going: all.going.saturating_add(one.going),
            })
    }

    /// Ask `workspace`'s members for the app surface `hash` of `app` this host
    /// lacks, unless it asked lately ([`ContentWants::ask`]): the changes held
    /// for it are decided once a member answers with it
    /// ([`crate::surfaces::arrivals`]). The number of devices it reached.
    pub async fn ask_surface(&self, workspace: &str, app: &str, hash: &str) -> usize {
        let device = self.device_cert().device_public_key;
        let asked = self.inner.content.surfaces.lock_recover().ask(
            (),
            (app, workspace),
            hash,
            tilo_clock::unix_now_ms(),
        );
        let Some(wanted) = asked else {
            return 0;
        };
        let request = neighbour_wire::content::asks(&device, workspace, [wanted]).remove(0);
        match self.ask_members(workspace, request).await {
            Ok(reached) => reached,
            Err(error) => {
                tracing::debug!(%workspace, %hash, %error, "could not ask the workspace for a surface");
                0
            }
        }
    }

    /// Publish `body`, a `NeighbourMsg::DocumentContentWants` naming this
    /// host, to `workspace`'s members: the ones keeping the content answer the
    /// host. A member reaches the workspace's other devices only through this
    /// host, and the host asks them the same way they ask it, on the
    /// workspace's own link, which is where their answers come from. The
    /// number of devices that link reaches.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here, or it has no link here.
    pub async fn ask_members(&self, workspace: &str, body: Vec<u8>) -> Result<usize, String> {
        let workspace_id = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?
            .lock()
            .await
            .workspace_id();
        let message = tilo_failure::ignored!(
            "the body is the host's own request, which it just encoded",
            NeighbourMsg::decode(&body)
        )
        .map_err(|error| error.to_string())?;
        self.tell_members(workspace_id, message).await?;
        Ok(self
            .inner
            .link_remotes
            .lock_recover()
            .get(&workspace_id)
            .map_or(0, BTreeSet::len))
    }

    /// Send `message`, a content frame of this host's, on `workspace`'s own
    /// link, to the members it is addressed to, waiting for room.
    ///
    /// # Errors
    ///
    /// No link serves the workspace here, or it did not carry the frame.
    pub async fn tell_members(
        &self,
        workspace: WorkspaceId,
        message: NeighbourMsg,
    ) -> Result<(), String> {
        let link = self
            .inner
            .direct
            .link(workspace)
            .ok_or("the workspace has no link here")?;
        link.send_content(message)
            .await
            .map_err(|error| format!("a workspace's link did not carry the host's frame: {error}"))
    }

    /// The answer to `body`, a presence the member device `asker` of
    /// `workspace` published, when it asks for content
    /// ([`Self::content_answer_each`]), gathered.
    pub fn content_answer(&self, workspace: &str, asker: &[u8; 32], body: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        self.content_answer_each(workspace, asker, body, |message| {
            frames.push(message.frame());
        });
        frames
    }

    /// Answer `body`, a presence the member device `asker` of `workspace`
    /// published, when it asks for content: the pieces of what this host
    /// keeps, many to a frame, and word of each it keeps none of, as frames
    /// for the asker's device alone, each handed to `emit` once made
    /// ([`neighbour_wire::content::answer_each`]). Nothing for any other
    /// presence, or a request naming another workspace. What it keeps of app
    /// surfaces is the workspace's content store, never a registry.
    ///
    /// Content asked for that this device keeps none of is owed: answered
    /// again, on the same link, once this device keeps it
    /// ([`Self::content_kept`]), so the asker need not ask again and again.
    pub fn content_answer_each(
        &self,
        workspace: &str,
        asker: &[u8; 32],
        body: &[u8],
        emit: impl FnMut(NeighbourMsg),
    ) {
        let request = match NeighbourMsg::decode(body) {
            Ok(request @ NeighbourMsg::DocumentContentWants { .. }) => request,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%workspace, %error, "an ask for content did not decode");
                return;
            }
        };
        if !matches!(&request, NeighbourMsg::DocumentContentWants { cluster_id, .. } if cluster_id == workspace)
        {
            return;
        }
        // The answer names this device, the one its link proves: one key signs and answers
        // everywhere.
        let device = self.device_cert().device_public_key;
        let NeighbourMsg::DocumentContentWants { wants, .. } = &request else {
            return;
        };
        let mut ahead = ReadAhead::new(wants);
        let mut lacked = Vec::new();
        neighbour_wire::content::answer_each(
            &device,
            asker,
            &request,
            |app_id, _, hash| {
                let kept = ahead.read(app_id, hash, |app_id, hash| {
                    self.read_kept(workspace, app_id, hash)
                });
                if kept.is_none() {
                    lacked.push(neighbour_wire::content::Wanted {
                        app_id: app_id.to_owned(),
                        hash: hash.to_owned(),
                        have: Vec::new(),
                    });
                }
                kept
            },
            emit,
        );
        if lacked.is_empty() {
            return;
        }
        let mut owed = self.inner.content.owed.lock_recover();
        for wanted in lacked {
            let hash = wanted.hash.to_ascii_lowercase();
            // Owed as an ask of its own, so it is answered alone once kept.
            for ask in neighbour_wire::content::asks(asker, workspace, [wanted]) {
                owed.owe(workspace, hash.clone(), *asker, &ask);
            }
        }
    }

    /// This device keeps `hash` for `workspace` now: answer the asks it said
    /// it lacks it to, on the link they came by. A member copy calls this for
    /// content written here; a host for what it keeps for its members, whose
    /// asks it answers.
    pub fn content_kept(&self, workspace: &str, hash: &str) {
        let answers = self.owed_answers(workspace, hash);
        if answers.is_empty() {
            return;
        }
        let Some(link) = WorkspaceId::parse(workspace)
            .and_then(|id| self.member_link(id).or_else(|| self.inner.direct.link(id)))
        else {
            return;
        };
        for answer in answers {
            let Ok(message) = tilo_failure::ignored!(
                "a content answer this device made itself does not decode",
                NeighbourMsg::decode(&answer)
            ) else {
                continue;
            };
            if let Err(error) = link.send(message) {
                tracing::debug!(%workspace, %error, "content owed to a device did not reach it");
            }
        }
    }

    /// The answers to the asks for `hash` this device owes in `workspace`, taken: owed again
    /// when it still keeps none.
    fn owed_answers(&self, workspace: &str, hash: &str) -> Vec<Vec<u8>> {
        let owed = self.inner.content.owed.lock_recover().take(workspace, hash);
        owed.into_iter()
            .flat_map(|(asker, ask)| self.content_answer(workspace, &asker, &ask))
            .collect()
    }

    /// This device keeps `hash` for `workspace` now, fetched from another
    /// device: the devices it owed it to are answered by whoever had it,
    /// not sent the bytes a second time from here.
    pub fn content_fetched(&self, workspace: &str, hash: &str) {
        self.inner.content.owed.lock_recover().take(workspace, hash);
    }

    /// The bytes of content `hash` of `app_id` this device keeps for `workspace`.
    pub(super) fn read_kept(&self, workspace: &str, app_id: &str, hash: &str) -> Option<Vec<u8>> {
        self.app_file(workspace, hash)
            .or_else(|| {
                self.inner
                    .content
                    .get()
                    .iter()
                    .find_map(|source| source.read(workspace, app_id, hash))
            })
            // A record started by an older version of an app reads its writes
            // with that version's surface, which the workspace's content store
            // keeps ([`crate::surfaces`]).
            .or_else(|| crate::surfaces::kept(app_id, hash))
    }
}

/// Contents an answer reads at once, on threads of their own: a member's
/// files are read from its Files daemon, and one read after another left the
/// first frame of an ask for many small files waiting on every read before it.
const READS_AHEAD: usize = 16;

/// What an answer to an ask reads ahead of the frame it is filling: each
/// content the answer comes to that it has not read yet is read with the
/// [`READS_AHEAD`] after it, at once, in the order the ask names them.
struct ReadAhead {
    /// What the ask names, by app and lowercase hash, in its order.
    named: Vec<(String, String)>,
    /// Where in `named` the next read ahead starts.
    next: usize,
    /// What was read and not yet taken: `None` for content kept nowhere here.
    read: HashMap<String, Option<Vec<u8>>>,
}

impl ReadAhead {
    fn new(wants: &[neighbour_wire::content::Wanted]) -> Self {
        Self {
            named: wants
                .iter()
                .map(|wanted| (wanted.app_id.clone(), wanted.hash.to_ascii_lowercase()))
                .collect(),
            next: 0,
            read: HashMap::new(),
        }
    }

    /// The bytes of `app_id`'s `hash`, `None` when they are kept nowhere here,
    /// read by `read` (given an app and hash) with the contents named after
    /// it; read alone when the ask names it nowhere ahead, as a second naming
    /// of content taken already.
    fn read(
        &mut self,
        app_id: &str,
        hash: &str,
        read: impl Fn(&str, &str) -> Option<Vec<u8>> + Sync,
    ) -> Option<Vec<u8>> {
        if !self.read.contains_key(hash)
            && let Some(at) = self
                .named
                .get(self.next..)
                .and_then(|rest| rest.iter().position(|(_, named)| named == hash))
        {
            let start = self.next.saturating_add(at);
            let end = start.saturating_add(READS_AHEAD).min(self.named.len());
            let batch = self.named.get(start..end).unwrap_or_default();
            let read = &read;
            let kept: Vec<(String, Option<Vec<u8>>)> = std::thread::scope(|scope| {
                #[expect(
                    clippy::needless_collect,
                    reason = "every read starts before the first is joined; joining as they \
                              start would read them one after another"
                )]
                let reading: Vec<_> = batch
                    .iter()
                    .map(|(app_id, hash)| scope.spawn(move || (hash.clone(), read(app_id, hash))))
                    .collect();
                reading
                    .into_iter()
                    .map(|reading| {
                        reading
                            .join()
                            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                    })
                    .collect()
            });
            self.next = end;
            self.read.extend(kept);
        }
        self.read.remove(hash).unwrap_or_else(|| read(app_id, hash))
    }
}

/// Records of a workspace read whole ([`Workspaces::whole_documents`]), with
/// the revision each was read at, and those that did not read whole.
#[derive(Default)]
pub struct WholeDocuments {
    /// Each record, by its app and id, saved with all of its history.
    pub documents: Vec<((String, String), Vec<u8>)>,
    /// Each record that did not read whole, by its app and id, with why.
    pub unread: Vec<((String, String), String)>,
    /// People's own records sealed for their devices alone, by app and id:
    /// not the host's to read, and no part of what it reads whole.
    pub private: Vec<(String, String)>,
    revisions: BTreeMap<(String, String), [u8; 32]>,
}

/// A record of a workspace's store, by its app and id, with its kind, and the revision it is at
/// with the record saved, or why it does not read ([`Workspaces::stored_documents`]).
pub(super) type StoredDocument = (
    (String, String),
    String,
    Result<([u8; 32], Vec<u8>), String>,
);

impl WholeDocuments {
    /// Every record the host is to read read whole: what they name is all the
    /// workspace names that the host keeps for it. A person's own sealed
    /// record is theirs, not one the host reads.
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.unread.is_empty()
    }

    /// Take in `newer`, records read again: their revisions are the ones
    /// read last, and each read again is unread only if it is unread now.
    pub fn extend(&mut self, newer: Self) {
        self.unread.retain(|(route, _)| {
            !newer.revisions.contains_key(route)
                && !newer.unread.iter().any(|(again, _)| again == route)
        });
        self.documents.extend(newer.documents);
        self.unread.extend(newer.unread);
        self.private
            .retain(|route| !newer.revisions.contains_key(route));
        for route in newer.private {
            if !self.private.contains(&route) {
                self.private.push(route);
            }
        }
        self.revisions.extend(newer.revisions);
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use tilo_peer_state::peer_handle;

    struct One(Vec<u8>);

    impl WorkspaceContent for One {
        fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
            (workspace == WORKSPACE
                && app == "so.tilo.chat"
                && hash == blake3::hash(&self.0).to_hex().as_str())
            .then(|| self.0.clone())
        }
    }

    const WORKSPACE: &str = "2f10f463-8f4c-43a3-b9dd-18df422eb0bd";

    /// The member device that asks.
    const MEMBER: [u8; 32] = [9; 32];

    fn want(workspace: &str, hash: &str) -> Vec<u8> {
        postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
            requester: peer_handle(&MEMBER),
            cluster_id: workspace.into(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: "so.tilo.chat".into(),
                hash: hash.into(),
                have: Vec::new(),
            }],
        })
        .unwrap()
    }

    /// An ask answered with word that the content is not kept here is answered again once it
    /// is, by the device that wrote it: the asker need not ask again and again meanwhile. One
    /// that fetched the content from elsewhere leaves that to whoever it came from.
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the server stays live until its consuming shutdown"
    )]
    async fn content_a_device_said_it_lacks_follows_once_it_keeps_it() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::workspaces::tests::hosting_device();
        let server = crate::workspaces::tests::test_host(scratch.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let (written, fetched) = (vec![3_u8; 100], vec![4_u8; 100]);
        let hash = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
        for bytes in [&written, &fetched] {
            let ask = want(WORKSPACE, &hash(bytes));
            assert!(lacks(&workspaces.content_answer(WORKSPACE, &MEMBER, &ask)).is_some());
        }
        assert!(
            lacks(&workspaces.owed_answers(WORKSPACE, &hash(&written))).is_some(),
            "still kept nowhere: still owed"
        );
        workspaces.serve_content(Arc::new(One(written.clone())));
        workspaces.serve_content(Arc::new(One(fetched.clone())));
        workspaces.content_fetched(WORKSPACE, &hash(&fetched));
        assert!(
            workspaces
                .owed_answers(WORKSPACE, &hash(&fetched))
                .is_empty()
        );
        let answers = workspaces.owed_answers(WORKSPACE, &hash(&written));
        let [answer] = answers.as_slice() else {
            panic!("one answer: {} frames", answers.len());
        };
        let NeighbourMsg::DocumentContentPieces {
            requester, pieces, ..
        } = postcard::from_bytes(answer).unwrap()
        else {
            panic!("an answer of pieces");
        };
        assert_eq!(requester, peer_handle(&MEMBER));
        let [piece] = pieces.as_slice() else {
            panic!("one piece: {} pieces", pieces.len());
        };
        assert_eq!(piece.hash, hash(&written));
        assert_eq!(piece.bytes, written);
        assert!(
            workspaces
                .owed_answers(WORKSPACE, &hash(&written))
                .is_empty(),
            "answered once"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the server stays live until its consuming shutdown"
    )]
    async fn a_member_asking_for_content_gets_it_in_pieces_or_word_that_it_is_not_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::workspaces::tests::hosting_device();
        let server = crate::workspaces::tests::test_host(scratch.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let avatar = vec![7_u8; neighbour_wire::content::PIECE_BYTES + 10];
        let hash = blake3::hash(&avatar).to_hex().to_string();
        assert_eq!(
            lacks(&workspaces.content_answer(WORKSPACE, &MEMBER, &want(WORKSPACE, &hash))),
            Some((peer_handle(&cert.device_public_key), peer_handle(&MEMBER))),
            "a host that serves no content keeps none"
        );
        workspaces.serve_content(Arc::new(One(avatar.clone())));

        let frames = workspaces.content_answer(WORKSPACE, &MEMBER, &want(WORKSPACE, &hash));
        let mut gathered = Vec::new();
        let mut counted = 0;
        for frame in &frames {
            let NeighbourMsg::DocumentContentPieces {
                responder,
                requester,
                pieces,
                ..
            } = postcard::from_bytes(frame).unwrap()
            else {
                panic!("an answer of pieces");
            };
            assert_eq!(responder, peer_handle(&cert.device_public_key));
            assert_eq!(
                requester,
                peer_handle(&MEMBER),
                "addressed to whoever asked"
            );
            for piece in pieces {
                assert_eq!(piece.hash, hash);
                assert_eq!(piece.total, u64::try_from(avatar.len()).unwrap());
                assert_eq!(piece.offset, u64::try_from(gathered.len()).unwrap());
                gathered.extend(piece.bytes);
                counted += 1;
            }
        }
        assert_eq!(counted, 2, "the avatar crossed in its two pieces");
        assert_eq!(gathered, avatar);

        let other = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
        assert!(
            workspaces
                .content_answer(other, &MEMBER, &want(WORKSPACE, &hash))
                .is_empty(),
            "another workspace's request"
        );
        assert_eq!(
            lacks(&workspaces.content_answer(
                WORKSPACE,
                &MEMBER,
                &want(WORKSPACE, &"0".repeat(64))
            )),
            Some((peer_handle(&cert.device_public_key), peer_handle(&MEMBER))),
            "not kept: the member is told so"
        );
        assert!(
            workspaces
                .content_answer(WORKSPACE, &MEMBER, b"not a request")
                .is_empty()
        );
    }

    /// Who said, to whom, that it keeps none of what was asked: the one
    /// frame `frames` holds when it is such word.
    fn lacks(frames: &[Vec<u8>]) -> Option<(u64, u64)> {
        let [frame] = frames else {
            return None;
        };
        match postcard::from_bytes(frame).ok()? {
            NeighbourMsg::DocumentContentLacks {
                responder,
                requester,
                ..
            } => Some((responder, requester)),
            _ => None,
        }
    }

    /// An answer reads what an ask names a batch at a time, in the order it comes to them, each
    /// naming once: content named twice is read for each, and content kept nowhere reads as
    /// none, without holding up what is kept.
    #[test]
    fn an_answer_reads_ahead_what_the_ask_names_each_once() {
        let names: Vec<String> = (0..40).map(|at| format!("{at:064x}")).collect();
        let mut wanted: Vec<neighbour_wire::content::Wanted> = names
            .iter()
            .map(|hash| neighbour_wire::content::Wanted {
                app_id: "so.tilo.files".into(),
                hash: hash.clone(),
                have: Vec::new(),
            })
            .collect();
        wanted.push(wanted[3].clone());
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let read = |_: &str, hash: &str| {
            reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (hash != names[7]).then(|| hash.as_bytes().to_vec())
        };
        let mut ahead = ReadAhead::new(&wanted);
        for (at, hash) in names.iter().enumerate() {
            let kept = ahead.read("so.tilo.files", hash, read);
            if at == 7 {
                assert_eq!(kept, None, "content kept nowhere reads as none");
            } else {
                assert_eq!(kept, Some(hash.as_bytes().to_vec()), "content {at}");
            }
        }
        assert_eq!(
            ahead.read("so.tilo.files", &names[3], read),
            Some(names[3].as_bytes().to_vec()),
            "its second naming was read ahead with the last batch"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::Relaxed),
            wanted.len(),
            "each naming read once, {READS_AHEAD} at a time"
        );
        let unnamed = format!("{:064x}", 99);
        assert_eq!(
            ahead.read("so.tilo.files", &unnamed, read),
            Some(unnamed.as_bytes().to_vec()),
            "content the ask never named is read alone"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::Relaxed),
            wanted.len() + 1,
            "and only it"
        );
    }
}
