//! Content a hosted workspace's records reference that this host keeps: what
//! its bots fetched from outside (a bridged person's avatar, a file posted
//! elsewhere), the surfaces of the apps it decided changes with, whichever
//! version those changes were written by, which members decide the host's
//! changes against, and the files of a workspace its members wrote and left
//! ([`crate::workspaces::ContentKeeper`]).
//!
//! A member asks the workspace for content its record lacks with the workspace's
//! presence ([`NeighbourMsg::DocumentContentWants`]), as it asks any member.
//! The host answers what that member reads — a record's content through a resource of the
//! workspace that names it and that they read, an app's own code through its releases as every
//! member does — to that member's device alone, in the same bounded pieces a member answers
//! with; the member keeps the content once every piece arrived and the bytes hash to what it
//! asked.
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

use super::readings::Reading;
use super::{CollaborationError, Workspaces};
use tilo_action_types::ActionKind;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_peer_state::StatePeer;
use tilo_resource_store::content::may_read_content;
use tilo_resource_store::doc_of;
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
    ) -> Result<(), Box<NeighbourMsg>> {
        let _ = (workspace, from);
        Err(Box::new(message))
    }

    /// Take `message` when it answers a want this keeper made: what this device asked a member
    /// for, by the content's route and hash. Every keeper is asked this before any is handed a
    /// push ([`Self::take`]): an answer and a push are the same message, and only the keeper
    /// that made the want knows it is one.
    ///
    /// # Errors
    ///
    /// `message`, handed back, when it answers none of this keeper's wants.
    fn answer(
        &self,
        workspace: &str,
        from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>> {
        let _ = (workspace, from);
        Err(Box::new(message))
    }

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
    /// The shelf this device keeps its workspaces' content on, once its record content is
    /// served from it ([`crate::content::routes_content`]): what its own runs read a
    /// workspace's content from ([`Workspaces::workspace_content`]).
    #[cfg(feature = "content")]
    pub(super) shelf: std::sync::OnceLock<Arc<dyn crate::content::ContentShelf>>,
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

/// Who asks for content on a workspace's link, as the workspace's governance has them now.
struct Asking {
    /// The person the device is, or one a certificate placed it for, as a reader set names them
    /// (their key, hex).
    person: String,
    /// What the workspace is read through without its lock.
    reading: Reading,
}

impl Workspaces {
    /// Keep `kept` as the shelf this device keeps its workspaces' content on: what a run here
    /// reads a workspace's content from. The first shelf stays.
    #[cfg(feature = "content")]
    pub(crate) fn keep_shelf(&self, kept: Arc<dyn crate::content::ContentShelf>) {
        let _ = self.inner.content.shelf.set(kept);
    }

    /// The bytes of the content `hash` names in `workspace`, as an app's own code running on
    /// this device reads a file its workspace holds: what was put into the workspace, and a file
    /// of its tree as the file, read where the workspace's files are read
    /// ([`crate::files::OpenedFiles`]). Only `workspace`'s content answers.
    ///
    /// # Errors
    /// Why there are no bytes: this device keeps no content of its workspaces, the workspace
    /// holds none of `hash`, or its file does not open here.
    pub fn workspace_content(&self, workspace: &str, hash: &str) -> Result<Vec<u8>, String> {
        #[cfg(feature = "files")]
        {
            let kept = self
                .inner
                .content
                .shelf
                .get()
                .ok_or("this device keeps no content of its workspaces")?;
            let id = WorkspaceId::parse(workspace)
                .ok_or_else(|| format!("{workspace:?} names no workspace"))?;
            crate::files::OpenedFiles::new(self.clone(), Arc::clone(kept)).content(id, hash)
        }
        #[cfg(not(feature = "files"))]
        {
            let _ = (workspace, hash);
            Err("this device keeps no content of its workspaces".to_owned())
        }
    }

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
    pub fn kept_record_bytes(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
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
    /// [`WholeDocuments::is_whole`].
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
            let keepers = self.inner.content.keepers();
            for keeper in &keepers {
                match keeper.answer(workspace, from, message) {
                    Ok(()) => return,
                    Err(back) => message = *back,
                }
            }
            for keeper in &keepers {
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

    /// Who `asker`, the device asking for content on `workspace`'s link, asks as now: the person
    /// the workspace's governance names it, a member's own or the one a certificate placed it
    /// for. `None` for a device it knows no member by, which reads nothing by any address.
    fn asking(&self, workspace: &str, asker: &[u8; 32]) -> Option<Asking> {
        let reading = self.inner.readings.held(workspace)?;
        let now = tilo_clock::wall();
        let peer = reading.replica.read(StatePeer::clone);
        let person = match peer.placed_device(asker, now) {
            // A device placed for some of the workspace's records asks as its certificate's
            // person; the records it reaches are checked for each resource that names content.
            Some(placed) => hex::encode(placed.ok()?.person),
            // A member's device asks as its own person.
            None => peer.permissions(*asker, now).ok()?.person_id()?.to_owned(),
        };
        Some(Asking { person, reading })
    }

    /// Whether `asking` reads the content at `address` in the workspace: a resource of the
    /// workspace names it, their person is one of its readers, and the policy's read of that
    /// resource allows them ([`may_read_content`]). A device placed for some records reads what
    /// those reach alone.
    fn reads_content(&self, asking: &Asking, asker: &[u8; 32], address: &str) -> bool {
        let now = tilo_clock::wall();
        let peer = asking.reading.replica.read(StatePeer::clone);
        may_read_content(
            address,
            |address| {
                self.naming_content_in(&asking.reading, &asking.person, address)
                    .into_iter()
                    .map(|named| (named.app, named.kind, named.id))
                    .collect()
            },
            |app, kind, id| {
                // The record the resource is in is one the asker reaches; the read of the
                // resource itself is the policy's, as every read of one is.
                let Ok(access) = peer.read_access(*asker, app, doc_of(id), now) else {
                    return false;
                };
                let node = tilo_actions::resource_node(&access.policy.node(), app, kind, id);
                access.allows(&node, ActionKind::Read, now)
            },
        )
    }

    /// Whether `asking` reads the content at `hash`, asked as `app_id`'s. A file's bytes, its
    /// manifest and the chunks it names, are named by the workspace's files tree rather than by a
    /// resource's fields: whoever reads the tree reads them, as every read of the workspace's
    /// files is decided (the tree's own records, read in `workspace`), and a device that does not
    /// reads none of them. Any other content is read through a resource that names it
    /// ([`Self::reads_content`]).
    fn reads(
        &self,
        workspace: &str,
        asking: &Asking,
        asker: &[u8; 32],
        app_id: &str,
        hash: &str,
    ) -> bool {
        #[cfg(feature = "files")]
        if app_id == fs_protocol::tree::FILES_APP {
            let now = tilo_clock::wall();
            let peer = asking.reading.replica.read(StatePeer::clone);
            let tree = fs_protocol::tree::document_id();
            let records = collab_types::document_node(workspace, app_id, &tree);
            return peer
                .read_access(*asker, app_id, &tree, now)
                .is_ok_and(|access| access.allows(&records, ActionKind::Read, now));
        }
        #[cfg(not(feature = "files"))]
        let _ = workspace;
        tilo_actions::content_resource(hash)
            .is_some_and(|address| self.reads_content(asking, asker, &address))
    }

    /// Answer `body`, a presence the member device `asker` of `workspace`
    /// published, when it asks for content: the pieces of what this host
    /// keeps, many to a frame, and word of each it keeps none of, as frames
    /// for the asker's device alone, each handed to `emit` once made
    /// ([`neighbour_wire::content::answer_each`]). Nothing for any other
    /// presence, or a request naming another workspace, or a device the
    /// workspace knows no member by.
    ///
    /// What it answers is what `asker` reads. An app's own code — the package a member
    /// uploaded and the surface a change named — is the workspace's, read by every member that
    /// runs the app. A record's content is read by whoever reads a resource of the workspace
    /// that names its address ([`may_read_content`]); content no resource they read names is
    /// answered as content this device keeps none of, exactly as a hash nothing names is, so
    /// an answer says nothing of whether the workspace keeps it.
    ///
    /// Content asked for that this device keeps none of is owed: answered
    /// again, on the same link, once this device keeps it
    /// ([`Self::content_kept`]), so the asker need not ask again and again.
    /// Content the asker does not read is owed none of.
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
        let asking = self.asking(workspace, asker);
        let mut ahead = ReadAhead::new(wants);
        let mut lacked = Vec::new();
        neighbour_wire::content::answer_each(
            &device,
            asker,
            &request,
            |app_id, _, hash| {
                let asking = asking.as_ref()?;
                // An app's own code is the workspace's, named by its releases rather than a
                // resource's fields: every member that runs the app reads it.
                if let Some(code) = self.app_code(workspace, app_id, hash) {
                    return Some(code);
                }
                if !self.reads(workspace, asking, asker, app_id, hash) {
                    return None;
                }
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
        self.app_code(workspace, app_id, hash).or_else(|| {
            self.inner
                .content
                .get()
                .iter()
                .find_map(|source| source.read(workspace, app_id, hash))
        })
    }

    /// The bytes of an app's own code this device keeps for `workspace`: the package file a
    /// member uploaded, and the surface of a version the workspace decided a change with
    /// ([`crate::surfaces`]), which a record started by an older version of the app reads its
    /// writes with. Both are the workspace's, named by its releases rather than by a resource's
    /// fields: every member that runs the app reads them
    /// ([`Self::content_answer_each`]).
    fn app_code(&self, workspace: &str, app_id: &str, hash: &str) -> Option<Vec<u8>> {
        self.app_file(workspace, hash)
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
    /// workspace names that the host keeps for it.
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
        self.revisions.extend(newer.revisions);
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::workspaces::fixtures::{
        TestHost, admit_device, answer_grant, declare_naming_kind, hosting_device, names_content,
        test_host, write_store,
    };
    use collab_types::EVERY_MEMBER;
    use tilo_action_policy::{Answer, Grant, Grantee, TreePath};
    use tilo_action_types::KindSet;
    use tilo_peer_state::peer_handle;
    use tilo_user_identity::DeviceCert;

    /// The app whose records name content in these tests.
    const APP: &str = "test.content";

    /// A workspace hosted for the owner's device, holding the records `records` write and serving
    /// every content it is given.
    struct Fixture {
        server: TestHost,
        workspaces: Workspaces,
        workspace: String,
        /// The owner's device certificate: the host's own, which asks as the owner.
        cert: DeviceCert,
        /// The kind of the record that names content ([`declare_naming_kind`]).
        kind: String,
        /// The owner's person, as a reader set names them.
        person: String,
        _dir: tempfile::TempDir,
    }

    /// A content source that serves `bytes` in `workspace` for [`APP`], and none other.
    struct One {
        workspace: String,
        bytes: Vec<u8>,
    }

    impl WorkspaceContent for One {
        fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
            (workspace == self.workspace
                && app == APP
                && hash == blake3::hash(&self.bytes).to_hex().as_str())
            .then(|| self.bytes.clone())
        }
    }

    /// A workspace hosted for the owner's device, holding the records `records` makes for the
    /// owner's person.
    async fn fixture(
        records: impl FnOnce(&str, &str) -> Vec<collab_types::store::wire::Command>,
    ) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let kind = declare_naming_kind(&workspaces, APP);
        let workspace = workspaces.create("Content").await.unwrap().workspace;
        let person = hex::encode(cert.user_public_key);
        for record in records(&person, &kind) {
            write_store(&workspaces, &workspace, APP, record)
                .await
                .expect("the host keeps its record");
        }
        Fixture {
            server,
            workspaces,
            workspace,
            cert,
            kind,
            person,
            _dir: dir,
        }
    }

    /// `workspace`'s ask of `hash` as the device `asker` makes it.
    fn want(workspace: &str, asker: &[u8; 32], hash: &str) -> Vec<u8> {
        postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
            requester: peer_handle(asker),
            cluster_id: workspace.into(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: APP.into(),
                hash: hash.into(),
                have: Vec::new(),
            }],
        })
        .unwrap()
    }

    /// What `frames`, the answer to an ask, carried: every piece's bytes, in order, and how many
    /// pieces they crossed in.
    fn gathered(frames: &[Vec<u8>]) -> (Vec<u8>, usize) {
        let mut bytes = Vec::new();
        let mut pieces: usize = 0;
        for frame in frames {
            match postcard::from_bytes(frame).expect("an answer frame decodes") {
                NeighbourMsg::DocumentContentPieces {
                    pieces: carried, ..
                } => {
                    for piece in carried {
                        assert_eq!(piece.offset, u64::try_from(bytes.len()).unwrap());
                        bytes.extend(piece.bytes);
                        pieces = pieces.saturating_add(1);
                    }
                }
                other => panic!("an answer of pieces, not {other:?}"),
            }
        }
        (bytes, pieces)
    }

    /// What `frames`, the answer to an ask for `hash`, says of it: who answered and who was
    /// answered, of what app in what workspace, and the hash it answers about. `None` unless the
    /// answer is exactly one frame of `hash` as content not kept.
    fn lacks_of(frames: &[Vec<u8>]) -> Option<(u64, u64, String, String, String)> {
        let [frame] = frames else {
            return None;
        };
        match postcard::from_bytes(frame).ok()? {
            NeighbourMsg::DocumentContentLacks {
                responder,
                requester,
                app_id,
                cluster_id,
                hash,
            } => Some((responder, requester, app_id, cluster_id, hash)),
            _ => None,
        }
    }

    /// Content is answered to the device that reads a resource of the workspace naming it, and to
    /// nobody else: a member the resource's readers exclude and one the app's rules refuse are
    /// answered exactly as for content nothing names, which says nothing of whether the host keeps
    /// it.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one host, a member of its own and the answers it serves and refuses"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the server stays live until its consuming shutdown"
    )]
    async fn content_is_answered_only_through_a_resource_its_asker_reads() {
        let shared = vec![1_u8; neighbour_wire::content::PIECE_BYTES + 10];
        let owners = vec![2_u8; 32];
        let secret = vec![3_u8; 32];
        let unnamed = vec![4_u8; 32];
        let hash = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
        let fixture = fixture(|person, kind| {
            vec![
                names_content(kind, "shared", "blob", &hash(&shared), EVERY_MEMBER, person),
                // Sealed to the owner: the closest narrowing this tree has to a scoped device,
                // since scoped roles are not here yet.
                names_content(kind, "owners", "blob", &hash(&owners), person, person),
                names_content(kind, "secret", "blob", &hash(&secret), EVERY_MEMBER, person),
            ]
        })
        .await;
        // Every content is kept and served, so what answers is the read and never what is kept.
        for bytes in [&shared, &owners, &secret, &unnamed] {
            fixture.workspaces.serve_content(Arc::new(One {
                workspace: fixture.workspace.clone(),
                bytes: bytes.clone(),
            }));
        }
        let owner = fixture.cert.device_public_key;
        let member = admit_device(&fixture.workspaces, &fixture.workspace, &fixture.cert, 7).await;
        let editor = member.1.device_public_key;
        let editor_person = hex::encode(member.1.user_public_key);
        // The editor reads the workspace, and this one record of it by the workspace's rules
        // not at all.
        answer_grant(
            &fixture.workspaces,
            &fixture.workspace,
            Grant::new(
                "editor-reads-no-secret-content",
                Grantee::person(editor_person),
                TreePath::workspace(&fixture.workspace)
                    .app(APP)
                    .kind(fixture.kind.clone())
                    .resource("secret"),
                KindSet::READ,
                Answer::Deny,
                Grantee::person(fixture.person.clone()),
            ),
        )
        .await
        .expect("the owner refuses the editor the secret record");

        // The owner reads what a resource of theirs names, the one sealed to them too, and gets
        // it whole, however many pieces it crosses in.
        let frames = fixture.workspaces.content_answer(
            &fixture.workspace,
            &owner,
            &want(&fixture.workspace, &owner, &hash(&shared)),
        );
        assert_eq!(
            gathered(&frames),
            (shared.clone(), 2),
            "the owner reads the shared file, in its two pieces"
        );
        assert_eq!(
            gathered(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &owner,
                &want(&fixture.workspace, &owner, &hash(&owners))
            ))
            .0,
            owners,
            "the owner reads the file sealed to them"
        );

        // A member who reads the resource reads the content.
        assert_eq!(
            gathered(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &editor,
                &want(&fixture.workspace, &editor, &hash(&shared))
            ))
            .0,
            shared,
            "the editor reads the shared file"
        );

        // Content nothing names is answered as content not kept, whatever the device reads.
        let unknown = fixture.workspaces.content_answer(
            &fixture.workspace,
            &editor,
            &want(&fixture.workspace, &editor, &hash(&unnamed)),
        );
        let unknown_answer = lacks_of(&unknown).expect("content nothing names is not kept");
        assert_eq!(unknown_answer.4, hash(&unnamed));
        let unknown_shape = (
            unknown_answer.0,
            unknown_answer.1,
            unknown_answer.2.clone(),
            unknown_answer.3.clone(),
        );

        // What the editor may not read is answered exactly as content nothing names: the file
        // sealed from them, and the one the workspace's rules refuse them.
        for (address, why) in [
            (hash(&owners), "outside the resource's readers"),
            (hash(&secret), "refused by the workspace's rules"),
        ] {
            let refused = fixture.workspaces.content_answer(
                &fixture.workspace,
                &editor,
                &want(&fixture.workspace, &editor, &address),
            );
            let answered = lacks_of(&refused)
                .unwrap_or_else(|| panic!("the editor reads nothing of the file {why}"));
            assert_eq!(
                answered.4, address,
                "{why}: the answer names what was asked"
            );
            assert_eq!(
                (answered.0, answered.1, answered.2, answered.3),
                unknown_shape,
                "{why}: answered as content nothing names"
            );
        }

        // A device the workspace knows no member by reads none of it.
        let stranger = [9_u8; 32];
        assert!(
            lacks_of(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &stranger,
                &want(&fixture.workspace, &stranger, &hash(&shared))
            ))
            .is_some(),
            "a stranger's device is answered as content not kept"
        );
        fixture.server.shutdown().await;
    }

    /// A file of the workspace's tree is read by whoever reads the workspace's files: its bytes
    /// are named by the tree, not by a resource's fields. A device the workspace knows no member
    /// by, and a member the workspace refuses its files, read none of them, answered as content
    /// not kept.
    #[cfg(feature = "files")]
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the server stays live until its consuming shutdown"
    )]
    async fn a_files_bytes_are_answered_to_whoever_reads_the_workspaces_files() {
        /// The bytes of one file of the workspace, kept as Files keeps them.
        struct File {
            workspace: String,
            bytes: Vec<u8>,
        }
        impl WorkspaceContent for File {
            fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
                (workspace == self.workspace
                    && app == fs_protocol::tree::FILES_APP
                    && hash == blake3::hash(&self.bytes).to_hex().as_str())
                .then(|| self.bytes.clone())
            }
        }
        let want = |workspace: &str, asker: &[u8; 32], hash: &str| {
            postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
                requester: peer_handle(asker),
                cluster_id: workspace.into(),
                wants: vec![neighbour_wire::content::Wanted {
                    app_id: fs_protocol::tree::FILES_APP.into(),
                    hash: hash.into(),
                    have: Vec::new(),
                }],
            })
            .unwrap()
        };
        let bytes = vec![5_u8; 64];
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let fixture = fixture(|_, _| Vec::new()).await;
        fixture.workspaces.serve_content(Arc::new(File {
            workspace: fixture.workspace.clone(),
            bytes: bytes.clone(),
        }));
        let reader = admit_device(&fixture.workspaces, &fixture.workspace, &fixture.cert, 7).await;
        let reader = reader.1.device_public_key;
        assert_eq!(
            gathered(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &reader,
                &want(&fixture.workspace, &reader, &hash)
            ))
            .0,
            bytes,
            "a member who reads the workspace's files reads the file"
        );

        let stranger = [9_u8; 32];
        assert!(
            lacks_of(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &stranger,
                &want(&fixture.workspace, &stranger, &hash)
            ))
            .is_some(),
            "a stranger's device is answered as content not kept"
        );

        let refused = admit_device(&fixture.workspaces, &fixture.workspace, &fixture.cert, 8).await;
        answer_grant(
            &fixture.workspaces,
            &fixture.workspace,
            Grant::new(
                "refused-reads-no-files",
                Grantee::person(hex::encode(refused.1.user_public_key)),
                TreePath::workspace(&fixture.workspace).app(fs_protocol::tree::FILES_APP),
                KindSet::READ,
                Answer::Deny,
                Grantee::person(fixture.person.clone()),
            ),
        )
        .await
        .expect("the owner refuses the member the workspace's files");
        let refused = refused.1.device_public_key;
        assert!(
            lacks_of(&fixture.workspaces.content_answer(
                &fixture.workspace,
                &refused,
                &want(&fixture.workspace, &refused, &hash)
            ))
            .is_some(),
            "a member refused the workspace's files reads none of them"
        );
        fixture.server.shutdown().await;
    }

    /// An ask answered with word that the content is not kept here is answered again once it
    /// is, by the device that wrote it: the asker need not ask again and again meanwhile. One
    /// that fetched the content from elsewhere leaves that to whoever it came from. What the
    /// asker does not read is owed none of it.
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the server stays live until its consuming shutdown"
    )]
    async fn content_a_device_said_it_lacks_follows_once_it_keeps_it() {
        let (written, fetched) = (vec![5_u8; 100], vec![6_u8; 100]);
        let unnamed = vec![7_u8; 100];
        let hash = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
        let fixture = fixture(|person, kind| {
            vec![
                names_content(
                    kind,
                    "written",
                    "blob",
                    &hash(&written),
                    EVERY_MEMBER,
                    person,
                ),
                names_content(
                    kind,
                    "fetched",
                    "blob",
                    &hash(&fetched),
                    EVERY_MEMBER,
                    person,
                ),
            ]
        })
        .await;
        let owner = fixture.cert.device_public_key;
        for bytes in [&written, &fetched, &unnamed] {
            let ask = want(&fixture.workspace, &owner, &hash(bytes));
            assert!(
                lacks_of(
                    &fixture
                        .workspaces
                        .content_answer(&fixture.workspace, &owner, &ask)
                )
                .is_some()
            );
        }
        assert!(
            lacks_of(
                &fixture
                    .workspaces
                    .owed_answers(&fixture.workspace, &hash(&written))
            )
            .is_some(),
            "still kept nowhere: still owed"
        );
        assert!(
            fixture
                .workspaces
                .owed_answers(&fixture.workspace, &hash(&unnamed))
                .is_empty(),
            "content nothing the asker reads names is owed none of"
        );
        for bytes in [&written, &fetched] {
            fixture.workspaces.serve_content(Arc::new(One {
                workspace: fixture.workspace.clone(),
                bytes: bytes.clone(),
            }));
        }
        fixture
            .workspaces
            .content_fetched(&fixture.workspace, &hash(&fetched));
        assert!(
            fixture
                .workspaces
                .owed_answers(&fixture.workspace, &hash(&fetched))
                .is_empty()
        );
        let answers = fixture
            .workspaces
            .owed_answers(&fixture.workspace, &hash(&written));
        let [answer] = answers.as_slice() else {
            panic!("one answer: {} frames", answers.len());
        };
        let NeighbourMsg::DocumentContentPieces {
            requester, pieces, ..
        } = postcard::from_bytes(answer).unwrap()
        else {
            panic!("an answer of pieces");
        };
        assert_eq!(requester, peer_handle(&owner));
        let [piece] = pieces.as_slice() else {
            panic!("one piece: {} pieces", pieces.len());
        };
        assert_eq!(piece.hash, hash(&written));
        assert_eq!(piece.bytes, written);
        assert!(
            fixture
                .workspaces
                .owed_answers(&fixture.workspace, &hash(&written))
                .is_empty(),
            "answered once"
        );
        fixture.server.shutdown().await;
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
