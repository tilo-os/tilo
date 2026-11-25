//! Content a governed workspace's records reference that this host keeps: what
//! its bots fetched from outside (a bridged person's avatar, a file posted
//! elsewhere), the surfaces of the apps it decided changes with, whichever
//! version those changes were written by, which members decide the host's
//! changes against, and the files of a workspace its members wrote and left
//! ([`crate::workspaces::ContentKeeper`]).
//!
//! A member asks the workspace for content its record lacks with the workspace's
//! presence ([`NeighbourMsg::DocumentContentWant`]), as it asks any member.
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
    /// host in `workspace`, when it answers a request this keeper made: such a
    /// piece counts against what the keeper keeps, not the member's general
    /// content budget. `message` comes back when it answers none of them, for
    /// whatever else this host asked. The frame is decoded once, and taken in
    /// one look at what is asked for: a large tree's pieces arrive by the
    /// thousand.
    ///
    /// # Errors
    ///
    /// `message`, handed back, when it answers no request of this keeper's.
    fn take(
        &self,
        workspace: &str,
        from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), NeighbourMsg>;

    /// A link to `workspace`'s devices started: ask again at once for what
    /// is still being gathered there, since what a link that ended carried
    /// of the asks and their answers ended with it.
    fn relinked(&self, workspace: &str) {
        let _ = workspace;
    }

    /// How many blobs of the content the resources of `workspace` that open
    /// here name and this keeps none of yet, and, when `waiting`, one for each
    /// resource still waiting for the keys that open it: none for one that
    /// keeps no resource's content. Content `beside` addresses is not
    /// counted.
    fn lacks<'a>(
        &'a self,
        workspace: &'a str,
        waiting: bool,
        beside: &'a BTreeSet<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + 'a>> {
        let _ = (workspace, waiting, beside);
        Box::pin(std::future::ready(0))
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

    /// The addresses of the content the resources of `workspace` that open
    /// here name and this keeps none of yet.
    fn lacked<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = BTreeSet<String>> + Send + 'a>> {
        let _ = workspace;
        Box::pin(std::future::ready(BTreeSet::new()))
    }

    /// How many blobs of content the resources of `workspace` that open here
    /// name: none for one that keeps no resource's content.
    fn names<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + 'a>> {
        let _ = workspace;
        Box::pin(std::future::ready(0))
    }
}

/// A member's answer to content this host asked a workspace for: a piece of
/// it, or word that the member keeps none of it.
#[derive(Clone, Debug)]
pub(crate) struct ContentAnswer {
    pub(crate) workspace: String,
    pub(crate) from: [u8; 32],
    pub(crate) lacks: bool,
}

/// Where each content answer this host takes in is told, to whoever waits
/// for one ([`Workspaces::content_answers`]).
pub(super) struct ContentAnswers(tokio::sync::broadcast::Sender<ContentAnswer>);

impl Default for ContentAnswers {
    fn default() -> Self {
        Self(tokio::sync::broadcast::channel(64).0)
    }
}

/// The content source a host's workspaces answer from, and the keeper of what
/// members answer the host with, once it has them.
#[derive(Default)]
pub(super) struct ContentSlot {
    pub(super) sources: RwLock<Vec<Arc<dyn WorkspaceContent>>>,
    /// The answers members give the content this host asks for.
    pub(super) answers: ContentAnswers,
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

    fn keepers(&self) -> Vec<Arc<dyn ContentKeeper>> {
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

    /// The workspace `workspace` governed here, spelled as its listings and
    /// files name it; `None` for one not governed here.
    pub async fn workspace_identity(&self, workspace: WorkspaceId) -> Option<String> {
        self.held(workspace).await.map(|_| workspace.to_string())
    }

    /// The workspace governed here under `workspace`: the host keeps each
    /// under its id spelled out, so this is one lookup, never a walk.
    pub(super) async fn held(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Arc<tokio::sync::Mutex<super::GovernedWorkspace>>> {
        let held = self.inner.workspaces.lock().await;
        held.get(&workspace.to_string()).cloned()
    }

    /// The name of the workspace governed here whose channels are
    /// `workspace`; `None` for one not governed here.
    pub async fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        let governed = self.held(workspace).await?;
        let name = governed.lock().await.name();
        Some(name)
    }

    /// The name the workspace governed here under `workspace` shows for the
    /// member whose root is `root`; `None` for one not governed here or a
    /// member who chose no name.
    pub async fn name_of_member(&self, workspace: WorkspaceId, root: [u8; 32]) -> Option<String> {
        let governed = self.held(workspace).await?;
        let governed = governed.lock().await;
        self.member_name_by_root(&governed, root)
    }

    /// The bytes named `hash` this host keeps for the workspace of `workspace`
    /// governed here, checked against the hash: what its bots fetched and
    /// the files it keeps for members. `None` for a workspace not governed here
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

    /// Every record of `workspace` the governor's copy holds (or of `only`
    /// among them), each saved with all of its history: what tells a host
    /// which content the workspace still references, now or in a version it
    /// can go back to. Read from a copy of the governor's state, taken in a
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
    /// The workspace is not governed here.
    pub async fn whole_documents(
        &self,
        workspace: &str,
        only: Option<&BTreeSet<(String, String)>>,
    ) -> Result<WholeDocuments, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let (peer, device) = {
            let governed = state.lock().await;
            let peer = governed.peer().clone();
            (peer, governed.device.clone())
        };
        let routes: Vec<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|route| only.is_none_or(|only| only.contains(route)))
            .collect();
        let mut read = WholeDocuments {
            files_security_basis: routes
                .iter()
                .any(|(app, _)| app == tilo_app_id::apps::FILE_EXPLORER)
                .then(|| files_security_basis(&peer))
                .flatten(),
            ..WholeDocuments::default()
        };
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
        Ok(read)
    }

    /// Publish a replacement Files index from a snapshot, only while its
    /// revision, access policy and encryption epoch still match the copy.
    /// Returns false when a newer state must be scanned again.
    ///
    /// # Errors
    /// The governor cannot read the key or publish the change.
    pub async fn publish_files_migration(
        &self,
        workspace: &str,
        snapshot: &WholeDocuments,
        keys: &key_group::ContentKeys,
        changes: Vec<u8>,
    ) -> Result<bool, CollaborationError> {
        if changes.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "Files migration exceeds publication limit",
            ));
        }
        let app = tilo_app_id::apps::FILE_EXPLORER;
        let index = collab_types::person_document_id(collab_types::FILES_TREE_KIND);
        let Some(revision) = snapshot.revisions.get(&(app.to_owned(), index.clone())) else {
            return Ok(false);
        };
        let Some(basis) = snapshot.files_security_basis.as_ref() else {
            return Ok(false);
        };
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        let device = governed
            .device
            .as_ref()
            .ok_or_else(|| CollaborationError::new("this device has no workspace keys"))?;
        let now = tilo_clock::wall();
        let scoped = governed.peer().requires_document_keys(app, &index, now)?;
        let writing = super::sealed_documents::read_document_keys(
            device,
            workspace,
            (app, &index),
            scoped,
            true,
        )
        .await?;
        if !writing.same_write_key(keys) {
            return Ok(false);
        }
        let written = {
            let shared = governed.peer.shared();
            let mut replica = shared.lock();
            if replica.document_revision(app, &index).as_ref() != Some(revision)
                || files_security_basis(&replica).as_ref() != Some(basis)
            {
                return Ok(false);
            }
            replica
                .workspace()
                .check_workspace_administration(self.device_cert().device_public_key, now)?;
            let mut candidate = replica.clone();
            let signer = &self.inner.host.key;
            let written = super::sealed_documents::publish_changes(
                &mut candidate,
                Some(&writing),
                (app, &index),
                changes,
                signer,
                now,
            )?;
            governed.commit_held(
                &mut replica,
                candidate,
                (signer, now),
                super::Change::Other,
                |entry, _| Ok(entry.collaboration.clone()),
            )?;
            drop(replica);
            written
        };
        self.settle(&mut governed).await?;
        self.carry_written(governed.workspace_id(), &[written])?;
        drop(governed);
        Ok(true)
    }

    /// Run `act` while `workspace` takes no write, if none of the records
    /// `read` read has changed since and none was added: `Ok(Ok(_))` with
    /// what it answered, or `Ok(Err(_))` with the records that changed or
    /// appeared, for the caller to read again. The workspace waits only as
    /// long as `act` takes. Retiring legacy payloads also checks Files access policy.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn while_unwritten<R>(
        &self,
        workspace: &str,
        read: &WholeDocuments,
        guard_files_policy: bool,
        act: impl FnOnce() -> R,
    ) -> Result<Result<R, BTreeSet<(String, String)>>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        // Members' writes are taken into the replica under its own lock: held
        // too, nothing lands while `act` runs.
        let peer = governed.peer();
        if guard_files_policy
            && (read.files_security_basis.is_none()
                || files_security_basis(&peer) != read.files_security_basis)
        {
            return Err(CollaborationError::new(
                "Files access changed during collection",
            ));
        }
        let changed: BTreeSet<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|(app, record)| {
                read.revisions.get(&(app.clone(), record.clone()))
                    != peer.document_revision(app, record).as_ref()
            })
            .collect();
        let answer = if changed.is_empty() {
            Ok(act())
        } else {
            Err(changed)
        };
        drop(peer);
        drop(governed);
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
    /// `workspace` ([`Self::content_for_host`]): one thread takes the answers
    /// in, in the order the link carried them, and the link's reader waits
    /// while that thread is two answers behind. A fast link is held back by
    /// the link's own flow control, and never by a crowd of threads each
    /// waiting on the keeper with one piece.
    pub(super) fn content_intake(
        &self,
        workspace: &str,
    ) -> tokio::sync::mpsc::Sender<([u8; 32], NeighbourMsg)> {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel(2 * neighbour_wire::content::PIECES_PER_ANSWER);
        let workspaces = self.downgrade();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            while let Some((from, message)) = receiver.blocking_recv() {
                // The host is gone: so is what it asked for.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                workspaces.content_message_for_host(&workspace, from, message);
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
                    // waits on a thread of its own, never on the runtime's.
                    let read = tokio::task::spawn_blocking(move || {
                        workspaces.content_answer(&workspace, &asker, &body)
                    })
                    .await;
                    let answers = match read {
                        Ok(answers) => answers,
                        Err(error) => {
                            tracing::warn!(%error, "reading what a member asked for ended early");
                            return;
                        }
                    };
                    let mut room = Some(room);
                    for answer in answers {
                        let message = match NeighbourMsg::decode(&answer) {
                            Ok(message) => message,
                            Err(error) => {
                                tracing::warn!(%error, "a content answer did not encode");
                                continue;
                            }
                        };
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
                });
            }
        });
        sender
    }

    /// [`Self::content_for_host`] for a message already decoded.
    fn content_message_for_host(&self, workspace: &str, from: [u8; 32], message: NeighbourMsg) {
        let lacks = matches!(message, NeighbourMsg::DocumentContentLacks { .. });
        self.keep_content_for_host(workspace, from, message);
        // Told once what it brings is kept.
        let answers = &self.inner.content.answers.0;
        if answers.receiver_count() > 0 {
            drop(answers.send(ContentAnswer {
                workspace: workspace.to_owned(),
                from,
                lacks,
            }));
        }
    }

    /// Hand `message` to whoever asked for what it brings
    /// ([`Self::content_for_host`]).
    fn keep_content_for_host(&self, workspace: &str, from: [u8; 32], message: NeighbourMsg) {
        let mut message = message;
        for keeper in self.inner.content.keepers() {
            match keeper.take(workspace, from, message) {
                Ok(()) => return,
                Err(back) => message = back,
            }
        }
        // The asker of a surface is this host: a piece is kept when it is
        // addressed to the host's own device, whoever among the members
        // answered with it.
        let device = self.device_cert().device_public_key;
        let assembled = self.inner.content.surfaces.lock_recover().assemble(
            &device,
            message,
            tilo_clock::unix_now_ms(),
        );
        if let Some(arrived) = assembled.and_then(neighbour_wire::content::Assembled::verified) {
            crate::surfaces::keep(&arrived.hash, &arrived.bytes);
        }
    }

    /// A link to `workspace`'s devices started: whatever gathers content there
    /// asks again at once for what it still waits on ([`ContentKeeper::relinked`]).
    pub(crate) fn content_relinked(&self, workspace: &str) {
        for keeper in self.inner.content.keepers() {
            keeper.relinked(workspace);
        }
    }

    /// Each answer a member gives the content this host asks for from now on.
    pub(crate) fn content_answers(&self) -> tokio::sync::broadcast::Receiver<ContentAnswer> {
        self.inner.content.answers.0.subscribe()
    }

    /// How many blobs of the content the resources of `workspace` that open
    /// here name this host keeps none of yet, as what keeps them counts, and,
    /// when `waiting`, one for each resource still waiting for its keys.
    /// Content `beside` addresses is not counted.
    pub(crate) async fn content_lacking(
        &self,
        workspace: &str,
        waiting: bool,
        beside: &BTreeSet<String>,
    ) -> usize {
        let mut lacking = 0_usize;
        for keeping in self.inner.content.keepers() {
            lacking = lacking.saturating_add(keeping.lacks(workspace, waiting, beside).await);
        }
        lacking
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

    /// The addresses of the content the resources of `workspace` that open
    /// here name this host keeps none of yet, as what keeps them says.
    pub(crate) async fn content_lacked(&self, workspace: &str) -> BTreeSet<String> {
        let mut lacked = BTreeSet::new();
        for keeping in self.inner.content.keepers() {
            lacked.extend(keeping.lacked(workspace).await);
        }
        lacked
    }

    /// How many blobs of content the resources of `workspace` that open here
    /// name, as what keeps them counts.
    pub(crate) async fn content_named(&self, workspace: &str) -> usize {
        let mut named = 0_usize;
        for keeping in self.inner.content.keepers() {
            named = named.saturating_add(keeping.names(workspace).await);
        }
        named
    }

    /// Ask `workspace`'s members for the app surface `hash` of `app` this host
    /// lacks, unless it asked lately ([`ContentWants::ask`]): the changes held
    /// for it are decided once a member answers with it
    /// ([`crate::surfaces::arrivals`]). The number of devices it reached.
    pub async fn ask_surface(&self, workspace: &str, app: &str, hash: &str) -> usize {
        let device = self.device_cert().device_public_key;
        let asked = self.inner.content.surfaces.lock_recover().ask(
            &device,
            (),
            (app, workspace),
            hash,
            tilo_clock::unix_now_ms(),
        );
        let Some(request) = asked else {
            return 0;
        };
        match self.ask_members(workspace, request).await {
            Ok(reached) => reached,
            Err(error) => {
                tracing::debug!(%workspace, %hash, %error, "could not ask the workspace for a surface");
                0
            }
        }
    }

    /// Publish `body`, a `NeighbourMsg::DocumentContentWant` naming this
    /// host, to `workspace`'s members: the ones keeping the content answer the
    /// host. A member reaches the workspace's other devices only through this
    /// host, and the host asks them the same way they ask it, on the
    /// workspace's own link, which is where their answers come from. The
    /// number of devices that link reaches.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or its workspace not hosted here.
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
        let link = self
            .inner
            .direct
            .link(workspace_id)
            .ok_or("the workspace has no link here")?;
        link.send_content(message).await.map_err(|error| {
            format!("a workspace's link did not carry the host's request: {error}")
        })?;
        Ok(self
            .inner
            .link_remotes
            .lock_recover()
            .get(&workspace_id)
            .map_or(0, BTreeSet::len))
    }

    /// The answer to `body`, a presence the member device `asker` of
    /// `workspace` published, when it asks for content: the pieces of what
    /// this host keeps, or word that it keeps none, as frames for the asker's
    /// device alone ([`neighbour_wire::content::answer`]). Empty for any other
    /// presence, or a request naming another workspace. What it keeps of app
    /// surfaces is the workspace's content store, never a registry.
    ///
    /// An ask answered with word that this device keeps none of the content
    /// is owed: answered again, on the same link, once this device keeps it
    /// ([`Self::content_kept`]), so the asker need not ask again and again.
    pub fn content_answer(&self, workspace: &str, asker: &[u8; 32], body: &[u8]) -> Vec<Vec<u8>> {
        if !matches!(
            NeighbourMsg::decode(body),
            Ok(NeighbourMsg::DocumentContentWant { cluster_id, .. }) if cluster_id == workspace
        ) {
            return Vec::new();
        }
        let answers = self.answer_from_kept(workspace, asker, body);
        if let [only] = answers.as_slice()
            && let Ok(NeighbourMsg::DocumentContentLacks { hash, .. }) = NeighbourMsg::decode(only)
        {
            self.inner.content.owed.lock_recover().owe(
                workspace,
                hash.to_ascii_lowercase(),
                *asker,
                body,
            );
        }
        answers
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

    fn answer_from_kept(&self, workspace: &str, asker: &[u8; 32], body: &[u8]) -> Vec<Vec<u8>> {
        // The answer names this device, the one its link proves: one key signs and answers
        // everywhere.
        let device = self.device_cert().device_public_key;
        neighbour_wire::content::answer(&device, asker, body, |app_id, _, hash| {
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
        })
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
    files_security_basis: Option<FilesSecurityBasis>,
}

// The governance heads include custom-role assignments. The security digest
// also includes certified revocations received outside the governance history.
type FilesSecurityBasis = (Vec<automerge::ChangeHash>, [u8; 32]);

fn files_security_basis(peer: &tilo_peer_state::StatePeer) -> Option<FilesSecurityBasis> {
    Some((
        peer.admitted_workspace()?.clone().get_heads(),
        peer.security_basis()?.ok()?,
    ))
}

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
        postcard::to_allocvec(&NeighbourMsg::DocumentContentWant {
            requester: peer_handle(&MEMBER),
            app_id: "so.tilo.chat".into(),
            cluster_id: workspace.into(),
            hash: hash.into(),
            have: Vec::new(),
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
        let (key, cert) = crate::workspaces::tests::governor();
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
            panic!("one piece: {} frames", answers.len());
        };
        let NeighbourMsg::DocumentContentPiece {
            requester, bytes, ..
        } = postcard::from_bytes(answer).unwrap()
        else {
            panic!("a piece");
        };
        assert_eq!((requester, bytes), (peer_handle(&MEMBER), written.clone()));
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
        let (key, cert) = crate::workspaces::tests::governor();
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
        for frame in &frames {
            let NeighbourMsg::DocumentContentPiece {
                responder,
                requester,
                total,
                offset,
                bytes,
                ..
            } = postcard::from_bytes(frame).unwrap()
            else {
                panic!("a piece");
            };
            assert_eq!(responder, peer_handle(&cert.device_public_key));
            assert_eq!(
                requester,
                peer_handle(&MEMBER),
                "addressed to whoever asked"
            );
            assert_eq!(total, u64::try_from(avatar.len()).unwrap());
            assert_eq!(offset, u64::try_from(gathered.len()).unwrap());
            gathered.extend(bytes);
        }
        assert_eq!(frames.len(), 2);
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
}
