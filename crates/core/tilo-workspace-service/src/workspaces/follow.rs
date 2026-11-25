//! What a governor does with its members' writes: take them into its copy,
//! file its verdict on the ones admission refused, and write the copy through
//! to the workspace file.

use std::sync::Arc;

use neighbour_wire::NeighbourMsg;

use super::{GovernedWorkspace, KEEP_FOLLOWED, WeakWorkspaces, Workspaces, peer_parts};
use ed25519_dalek::SigningKey;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// A member's write to a record of a governed workspace, after the governor's
/// copy took it in: what wakes the workspace's installed bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceWrite {
    /// The workspace's UUID.
    pub workspace: String,
    /// The record's app.
    pub app: String,
    /// The record's id on the wire (`""` for the app's primary).
    pub record: String,
}

/// What taking a member's write into the governor's copy did.
#[derive(Clone, Debug, Default)]
pub(super) struct Followed {
    /// The copy changed.
    pub(super) changed: bool,
    /// A write the copy let go earlier waited for something that has landed
    /// since: replaying what the server retains brings it in.
    pub(super) replay_wanted: bool,
    /// Admission refused the write, and why: the author is owed a verdict.
    pub(super) refused: Option<tilo_peer_state::AdmissionError>,
    /// The write is a sealed compaction this copy refused because it is behind in it, and why
    /// it could not take it yet ([`BehindRefusal`]).
    pub(super) behind: Option<BehindRefusal>,
    /// A sealed compaction this copy was behind in and took as the record's base, checked
    /// against every write the copy holds that it covers: the check this host keeps of it.
    pub(super) taken_behind: Option<collab_types::SignedOp>,
}

/// What taking a refused compaction came to, when it could not be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BehindRefusal {
    /// This host holds no keys of the record yet: the compaction waits for them.
    Keys,
    /// Taking the compaction would drop a change this copy holds: it is not the base of what
    /// this copy holds, and stays refused.
    Lacks,
}

impl Workspaces {
    /// [`GovernedWorkspace::follow_kept`], keeping this host's check of a compaction the copy
    /// took behind: it was checked as it was taken, against every write here it covers, and no
    /// later read can check it against the writes only it holds.
    pub(super) async fn follow_kept_checked(
        &self,
        workspace: &mut GovernedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Followed {
        let followed = workspace.follow_kept(author, message.clone()).await;
        #[cfg(feature = "device-keys")]
        if let (
            Some(compaction),
            neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            },
        ) = (&followed.taken_behind, &message)
            && let Err(error) = self
                .keep_checkpoint_check(workspace, (app_id, document_id), compaction)
                .await
        {
            tracing::warn!(workspace = %workspace.workspace(), %app_id, %document_id, %error,
                "this host's check of a compaction it took was not kept");
        }
        #[cfg(not(feature = "device-keys"))]
        let _ = &followed.taken_behind;
        followed
    }
}

impl Workspaces {
    /// Hand `message`, `author`'s write the workspace took in, to the apps of
    /// this device that follow it: the write that starts a document as every
    /// write after it. Sent while the workspace is still locked, so whoever it
    /// wakes reads a copy that holds the write. Another device's write is also
    /// handed on whole ([`Workspaces::admitted_writes`]), as `history` the
    /// workspace already held or as a write made just now.
    ///
    /// Whoever follows the workspace's records through
    /// [`Workspaces::document_writes`] is told by [`Self::wrote`], the one
    /// notice an admitted frame and a local write raise: telling it here as
    /// well woke those watchers twice for one write.
    #[expect(
        clippy::unused_async,
        reason = "one signature with the callers that send the write while the workspace is locked"
    )]
    pub(super) async fn tell_followers(
        &self,
        workspace: &GovernedWorkspace,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
        history: bool,
    ) {
        self.hand_on(workspace, author, message, history);
    }

    /// Tell whoever follows `workspace`'s records through
    /// [`Workspaces::document_writes`] that `message` is in its copy: the one
    /// notice an admitted frame and a local write raise, so it fires once.
    pub(super) fn wrote_frame(&self, workspace: &str, message: &neighbour_wire::NeighbourMsg) {
        if let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = message
        {
            self.wrote(workspace, app_id, document_id);
        }
    }

    /// Hand `message`, `author`'s write `workspace` took in, to the apps of
    /// this device that follow it: not a write of this device's own, which its
    /// apps wrote, and not one whose author the workspace grants nothing now.
    /// `history` says it is a write the workspace already held, which this
    /// device catches up on, rather than one made just now.
    pub(super) fn hand_on(
        &self,
        workspace: &GovernedWorkspace,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
        history: bool,
    ) {
        if self.member_device_key() == Some(author) {
            return;
        }
        self.hand_on_any(workspace, author, message, history);
    }

    /// Hand `message`, `author`'s write `workspace` took in, to the apps of
    /// this device that follow it, whoever wrote it: a write of this device's
    /// own that its copy lacked, one it made before it lost what it held (a
    /// restart on an empty disk), is the apps' to take in as anyone's is.
    pub(super) fn hand_on_any(
        &self,
        workspace: &GovernedWorkspace,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
        history: bool,
    ) {
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. })
            || self.inner.member.writes.receiver_count() == 0
        {
            return;
        }
        let Ok(access) = workspace.peer().permissions(author, tilo_clock::wall()) else {
            return;
        };
        let _ = self.inner.member.writes.send(super::AdmittedWrite {
            workspace: workspace.workspace_id(),
            author,
            message: Arc::new(message.clone()),
            access,
            history,
        });
    }
}

impl GovernedWorkspace {
    /// Whether `author` may start `document` of `app` in this workspace with a
    /// write of its own: a device of an editor may, and no other.
    ///
    /// # Errors
    ///
    /// The device is not admitted here, or may not edit the document.
    pub(super) fn may_start(
        &self,
        author: [u8; 32],
        (app, document): (&str, &str),
        now: web_time::SystemTime,
    ) -> Result<(), String> {
        let member = self
            .peer()
            .permissions(author, now)
            .map_err(|error| error.to_string())?;
        let node = collab_types::validator::document_node(&member.policy.workspace, app, document);
        if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, now) {
            return Err(format!(
                "{app} record written by a device that may not edit"
            ));
        }
        if self.moderation_history_pending(app, document) {
            return Err("the workspace's moderation history is still arriving".into());
        }
        Ok(())
    }

    /// Keep and carry the governance this workspace published that the device
    /// store does not hold yet, in order; `published` moves past each. Stops
    /// at a change that could not be kept, with why, for the next settle to
    /// publish again.
    pub(super) fn publish(
        &mut self,
        records: &crate::workspace_documents::WorkspaceDocuments,
    ) -> Result<(), String> {
        while let Some(entry) = self.record.outgoing.get(self.record.published) {
            records
                .commit(
                    self.record.workspace_id,
                    &crate::workspace_documents::HostCommit {
                        body: entry.bytes.to_vec(),
                    },
                )
                .map_err(|error| {
                    format!(
                        "governance publication {} of {} not carried: {error}",
                        self.record.published,
                        self.record.outgoing.len()
                    )
                })?;
            self.record.published = self.record.published.saturating_add(1);
        }
        Ok(())
    }

    /// Write the governor's copy, as following members' writes changed it,
    /// through to the workspace file. A start replays what the server retains,
    /// but that replay alone does not bring a write back: the app surfaces
    /// it is decided against are still on their way then, so it holds the
    /// write and may let it go. A write the governor took in and lost that
    /// way left its record with a gap, once a later write that builds on it
    /// was committed, and the workspace no longer opened.
    pub(super) fn keep_followed(&mut self) {
        let Some((files, write)) = self.keep_later() else {
            return;
        };
        if let Err(error) = files.write(&write) {
            self.unsure();
            self.unkept = true;
            tracing::warn!(workspace = %self.workspace(), %error, "the governor's copy of members' writes was not kept");
        }
    }

    /// What [`Self::keep_followed`] writes, named now, and the files it is
    /// written to: written after the workspace is let go, as a large document
    /// can take the disk seconds, and every read of the workspace and every
    /// member's write would wait for them.
    fn keep_later(&mut self) -> Option<(peer_parts::PartFiles, peer_parts::FileWrite)> {
        self.unkept = false;
        let named = {
            let peer = self.peer.lock();
            self.parts.name(&peer)
        };
        let kept = named.and_then(|parts| {
            let prior = std::mem::replace(&mut self.record.peer, parts);
            self.file_write().inspect_err(|_| self.record.peer = prior)
        });
        match kept {
            Ok(write) => Some((self.parts.files(), write)),
            Err(error) => {
                tracing::warn!(workspace = %self.workspace(), %error, "the governor's copy of members' writes was not kept");
                None
            }
        }
    }

    /// Take a member's write into the governor's copy, in memory:
    /// [`Self::keep_followed`] writes it through. A retained checkpoint can
    /// also restore signed history for a document placed by the store.
    /// Admission refusals name the reason in [`Followed::refused`] so the
    /// governor can file its verdict in the workspace document.
    pub(super) fn follow(
        &self,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Followed {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = &message
        else {
            return Followed::default();
        };
        let received = {
            let mut peer = self.peer();
            if !(peer.holds_document(app_id, document_id)
                || (collab_types::is_store_document(document_id) && envelope.is_whole_resource()))
            {
                return Followed::default();
            }
            peer.receive_from(
                tilo_peer_state::TransportOrigin::RetainedServerDelta(author),
                message,
                tilo_clock::wall(),
            )
        };
        match received {
            Ok(outcome) => Followed {
                changed: outcome.changed || outcome.metadata_changed,
                replay_wanted: outcome.replay_wanted,
                refused: None,
                behind: None,
                taken_behind: None,
            },
            // This device could not decide the write: no verdict of its says
            // anything about it, and the write stays in the server's ownership
            // for the next replay.
            Err(error) if error.is_device_failure() => {
                tracing::warn!(
                    workspace = %self.record.workspace,
                    %error,
                    "this device could not decide a member's write; nothing was refused"
                );
                Followed::default()
            }
            Err(error) => Followed {
                refused: Some(error),
                ..Followed::default()
            },
        }
    }

    /// [`Self::follow`] for `message`, with a sealed compaction the copy is behind in taken as the
    /// record's base under the record's keys
    /// ([`tilo_peer_state::PeerPublicationMut::take_compaction_behind`]): a replica that holds
    /// only part of what a compaction folds cannot tell it from one that leaves writes out, but
    /// a key holder can, judging every write it holds that the compaction covers. The writes the
    /// copy lacks are nowhere but inside the compaction then, so refusing it for good would lose
    /// them.
    ///
    /// This covers a write that arrives live as well as one a load replays. A compaction this
    /// host holds no keys of yet is reported as [`BehindRefusal::Keys`]: it is the retained
    /// store's, which keeps sealed frames this host cannot read, and a replay takes it once the
    /// keys are here. One that does not hold every change this copy holds is refused as it
    /// stands: taking it would drop a write here.
    pub(super) async fn follow_kept(
        &self,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Followed {
        let followed = self.follow(author, message.clone());
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = &message
        else {
            return followed;
        };
        if followed.refused.is_none() || envelope.kind != collab_types::OpKind::Compaction {
            return followed;
        }
        match self
            .take_compaction_behind((app_id, document_id), envelope.as_ref())
            .await
        {
            Ok(true) => Followed {
                changed: true,
                taken_behind: Some(envelope.as_ref().clone()),
                ..Followed::default()
            },
            Ok(false) => followed,
            Err(behind) => {
                if behind == BehindRefusal::Keys {
                    tracing::debug!(
                        workspace = %self.workspace(),
                        app = %app_id,
                        document = %document_id,
                        "a compaction this copy is behind in waits for the record's keys, kept"
                    );
                }
                Followed {
                    behind: Some(behind),
                    ..followed
                }
            }
        }
    }

    /// Whether this copy can take `compaction`, the sealed compaction of `route` it refused, as
    /// the record's base under the record's keys.
    ///
    /// # Errors
    /// [`BehindRefusal::Keys`] while this host holds no keys of the record, and
    /// [`BehindRefusal::Lacks`] when taking the compaction would drop a write this copy holds.
    async fn take_compaction_behind(
        &self,
        (app, document): (&str, &str),
        compaction: &collab_types::SignedOp,
    ) -> Result<bool, BehindRefusal> {
        // The copy holds it already: a compaction its author sends again, or one a
        // replay brings back, is not behind it.
        if self
            .peer()
            .publication()
            .sealed_history(app, document)
            .iter()
            .any(|write| write.signature == compaction.signature)
        {
            return Ok(true);
        }
        // Its author may write it and it is whole: that needs no key, and a frame that is not
        // is refused however unreadable its content is.
        self.peer()
            .publication()
            .check_sealed_compaction((app, document), compaction, tilo_clock::wall())
            .map_err(|error| {
                tracing::debug!(workspace = %self.workspace(), app, document, %error,
                    "a refused compaction is no write its author may make here");
                BehindRefusal::Lacks
            })?;
        let keys = match self.document_keys(app, document).await {
            Ok(Some(keys)) => keys,
            Ok(None) => return Err(BehindRefusal::Keys),
            Err(error) => {
                tracing::debug!(workspace = %self.workspace(), %error, "a kept compaction waits for the record's keys");
                return Err(BehindRefusal::Keys);
            }
        };
        self.peer()
            .publication_mut()
            .take_compaction_behind(
                (app, document),
                compaction.clone(),
                &mut keys.open(),
                tilo_clock::wall(),
            )
            .map(|taken| taken.is_some())
            .map_err(|error| {
                tracing::debug!(workspace = %self.workspace(), app, document, %error,
                    "a kept compaction does not hold every change the copy holds");
                BehindRefusal::Lacks
            })
    }

    /// The verdict taking `message`, `author`'s write, into the governor's
    /// copy would give, without keeping it: why the workspace refuses the
    /// write, or `None` when it takes it. A test asks it of a write the
    /// workspace's link would have admitted, so nothing is decided for a
    /// change that never reaches a copy. The tests that ask are the sealed
    /// documents', which are built with the agents.
    #[cfg(all(test, feature = "agents"))]
    pub(super) fn refuses(
        &self,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) -> Option<String> {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return None;
        };
        // A write too large for one frame travels in segments, and is decided
        // a segment at a time: no segment is a record write on its own, so
        // the workspace has no verdict to give on one. Its last segment is the
        // whole write, and that is refused or taken as this one would be.
        if matches!(
            collab_types::DocumentFrame::decode(&envelope.payload),
            Ok(collab_types::DocumentFrame::Segment(_))
        ) {
            return None;
        }
        let mut decided = {
            let peer = self.peer();
            if !peer.holds_document(app_id, document_id) {
                return None;
            }
            peer.clone()
        };
        let error = decided
            .receive_from(
                tilo_peer_state::TransportOrigin::RetainedServerDelta(author),
                message.clone(),
                tilo_clock::wall(),
            )
            .err()?;
        if error.is_device_failure() {
            tracing::warn!(%error, "this device could not decide a member's write; nothing is refused");
            return None;
        }
        Some(error.to_string())
    }

    /// File this device's verdict on the write admission refused as
    /// `refusal` says, in the governor's copy, for [`Self::keep_followed`] to write
    /// through: the write's author reads why it is not here.
    pub(super) fn refused(
        &self,
        caller: &DeviceCert,
        key: &SigningKey,
        message: &neighbour_wire::NeighbourMsg,
        refusal: &tilo_peer_state::AdmissionError,
    ) -> Followed {
        let filed = self.peer().workspace_mut().refuse_write(
            caller,
            message,
            refusal,
            key,
            tilo_clock::wall(),
        );
        match filed {
            Ok(filed) => Followed {
                changed: filed,
                ..Followed::default()
            },
            Err(error) => {
                tracing::warn!(
                    workspace = %self.record.workspace,
                    %refusal,
                    %error,
                    "the governor did not file a refusal of a record write"
                );
                Followed::default()
            }
        }
    }
}

impl Workspaces {
    /// Why taking `body`, `author`'s write, into `workspace` would refuse it;
    /// `None` when it takes the write. A test asks it of a write the
    /// workspace's link would have admitted.
    ///
    /// A sealed compaction the copy is behind in is not refused: it is taken
    /// as the record's base under the record's keys, and one whose keys are
    /// not here yet is taken once they are. The tests that ask are the sealed
    /// documents', which are built with the agents.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) async fn refuses_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Option<String> {
        // This device's own writes reach its copy as its own: nothing a
        // member's write is decided by decides them.
        if author == self.inner.host.cert.device_public_key {
            return None;
        }
        // A body that is not a message is reported where the governor takes
        // it in; and one that is no record write is decided elsewhere.
        let message = neighbour_wire::NeighbourMsg::decode(body).ok()?;
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
            return None;
        }
        let state = self.workspace(workspace).await.ok()?;
        let mut governed = state.lock().await;
        let refusal = governed.refuses(author, &message)?;
        let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = &message else {
            return Some(refusal);
        };
        if envelope.kind != collab_types::OpKind::Compaction {
            return Some(refusal);
        }
        let followed = self
            .follow_kept_checked(&mut governed, author, message.clone())
            .await;
        // A compaction this copy cannot open yet is kept, not refused: the
        // link keeps it like any sealed frame it cannot read, and a replay
        // takes it once the keys are here.
        if followed.behind == Some(BehindRefusal::Keys) {
            drop(governed);
            return None;
        }
        if let Some(refused) = followed.refused {
            drop(governed);
            return Some(refused.to_string());
        }
        // The change is this copy's now: what it took is kept, and a verdict
        // filed against it before is cleared.
        if followed.changed {
            self.landed(&governed, &message);
            self.keep_soon(&mut governed);
        }
        drop(governed);
        None
    }

    /// Replay what the server retains for `workspace` into the governor's copy
    /// ([`Self::recall_retained`]), apart from whatever asked for it. The
    /// server's copy is read once every write the governor was handed is
    /// durable in it, and before the workspace is locked: their delivery waits
    /// for the governor to follow them.
    pub(super) fn replay_soon(&self, workspace: String) {
        let workspaces = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let Some(inner) = workspaces.upgrade() else {
                return;
            };
            (Self { inner }).replay(&workspace).await;
        });
    }

    /// Write the members' writes `workspace` took in through to its workspace file
    /// within [`KEEP_FOLLOWED`], once for all of them: a write each made a
    /// busy chat queue behind its own workspace file.
    pub(super) fn keep_soon(&self, workspace: &mut GovernedWorkspace) {
        workspace.unkept = true;
        if std::mem::replace(&mut workspace.keeping, true) {
            return;
        }
        let workspaces = Arc::downgrade(&self.inner);
        let id = workspace.workspace().to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(KEEP_FOLLOWED).await;
            let Some(inner) = workspaces.upgrade() else {
                return;
            };
            let Ok(workspace) = (Self { inner }).workspace(&id).await else {
                return;
            };
            let kept = {
                let mut workspace = workspace.lock().await;
                workspace.keeping = false;
                if !workspace.unkept {
                    return;
                }
                workspace.keep_later()
            };
            let Some((files, write)) = kept else {
                return;
            };
            let written = tokio::task::spawn_blocking(move || files.write(&write))
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
            if let Err(error) = written {
                let mut workspace = workspace.lock().await;
                workspace.unsure();
                workspace.unkept = true;
                tracing::warn!(workspace = %workspace.workspace(), %error, "the governor's copy of members' writes was not kept");
            }
        });
    }
}

impl Workspaces {
    /// Serve `workspace`'s replica over the direct link, keeping its history
    /// and its channels' governance at the host's store, and take in what its
    /// members send there.
    pub(super) fn serve_direct(&self, workspace: &GovernedWorkspace) -> Result<(), String> {
        let link = tilo_peer_state::direct::DirectSync::over(
            workspace.peer.clone(),
            self.inner.host.key.clone(),
        )
        .with_store(Arc::clone(&self.inner.store));
        #[cfg(test)]
        let link = match self
            .inner
            .link_heard_capacity
            .load(std::sync::atomic::Ordering::Acquire)
        {
            0 => link,
            capacity => link.with_heard_capacity(capacity),
        };
        self.inner.direct.serve(link.clone())?;
        self.follow_link(workspace, &link);
        Ok(())
    }

    /// Give the links of the workspaces served from now on workspace for only
    /// `capacity` frames heard and not read yet: what a test that makes the
    /// host's reader of a link fall behind sets.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) fn bound_link_readers(&self, capacity: usize) {
        self.inner
            .link_heard_capacity
            .store(capacity, std::sync::atomic::Ordering::Release);
    }

    /// Take in what `workspace`'s link admits and answer what its members ask
    /// there: a frame the link admitted is a member's write, followed into the
    /// governor's copy ([`Self::absorb`]), and each ask for content is answered with what this host keeps of it, or word
    /// that it keeps none, addressed to whoever asked. The link carries each
    /// ask on to the workspace's other members itself.
    ///
    /// A reader that falls behind the link's frames takes in the writes it
    /// missed before it reads on ([`tilo_peer_state::direct::Reader`]): a
    /// member is told its write landed once the link admitted it, so the host
    /// following it is not the member's to retry. Only the link ending stops
    /// the reads.
    fn follow_link(
        &self,
        workspace: &GovernedWorkspace,
        link: &tilo_peer_state::direct::DirectSync,
    ) {
        let name = workspace.workspace().to_owned();
        let workspaces = WeakWorkspaces(Arc::downgrade(&self.inner));
        let link = link.clone();
        let intake = self.content_intake(&name);
        let asks = self.content_asks(&name, &link);
        // Subscribed, where the replica stands, before the task runs:
        // every frame the link admits from here on is this reader's, and what
        // the replica held before is not read again.
        let mut reader = link.reader();
        // A link that starts carrying content: what this host gathers there is
        // asked for again at once, not after a silence.
        let mut linked = link.content_linked();
        let relinking = WeakWorkspaces(Arc::downgrade(&self.inner));
        let relinked = name.clone();
        tokio::spawn(async move {
            while linked.changed().await.is_ok() {
                let Some(workspaces) = relinking.upgrade() else {
                    return;
                };
                workspaces.content_relinked(&relinked);
            }
        });
        tokio::spawn(async move {
            // The link is gone: nothing more comes through it.
            while let Some(frame) = reader.recv().await {
                // The host this link served is gone: so is the answer.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let asking = matches!(frame.message, NeighbourMsg::DocumentContentWant { .. });
                let answering = matches!(
                    frame.message,
                    NeighbourMsg::DocumentContentPiece { .. }
                        | NeighbourMsg::DocumentContentLacks { .. }
                        | NeighbourMsg::DocumentContentRefused { .. }
                );
                // What a member answers the host with answers a request the
                // host made, and is the host's to keep: a device that keeps
                // its workspace's files reads the pieces here
                // ([`Self::content_for_host`]). Asks and answers are no record
                // writes: they go to the content's own intake, as they came,
                // never through the governor's copy.
                if answering {
                    // The intake's thread is gone only with the host.
                    if intake.send((frame.author, frame.message)).await.is_err() {
                        return;
                    }
                    continue;
                }
                let Ok(body) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                if asking {
                    // The answering is gone only with the host.
                    if asks.send((frame.author, body)).await.is_err() {
                        return;
                    }
                    continue;
                }
                if let Err(error) = workspaces.absorb(&name, frame.author, &body).await {
                    tracing::warn!(%name, %error, "the host did not keep what its link admitted");
                }
            }
        });
    }
}

impl Workspaces {
    /// What admits the device at the other end of every link to the workspaces
    /// this host serves: [`Serving::admitting`](tilo_peer_state::direct::Serving::admitting).
    ///
    /// The host builds its [`Serving`](tilo_peer_state::direct::Serving) before
    /// it exists to be asked, so the slot is filled with what holds it the
    /// moment it is.
    pub(super) fn admitting(
        slot: &Arc<std::sync::OnceLock<WeakWorkspaces>>,
    ) -> Arc<tilo_peer_state::direct::Admitting> {
        let slot = Arc::clone(slot);
        Arc::new(move |remote, certificate, link| {
            let Some(workspaces) = slot.get().and_then(WeakWorkspaces::upgrade) else {
                return Err(tilo_server_protocol::refusal::Refusal::NotAdmitted);
            };
            workspaces.admit_link(remote, certificate, link)
        })
    }

    /// What the one admission rule ([`crate::workspace_access::decide`])
    /// decides of `remote` linking to the workspace `link` serves, asked of
    /// the workspace's replica, which the link admits into: the decision for
    /// a subscribe, whether it holds the channels' keys, whether the device
    /// keeps the workspace, and the device as it was decided.
    ///
    /// The device is the one the workspace's roster certifies, a hosted
    /// browser environment's this server placed, or, for a device the
    /// workspace does not list yet, the certificate it named itself by on
    /// its dial (`offered`), once it verifies as that device's.
    fn link_decision(
        &self,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
        link: &tilo_peer_state::direct::DirectSync,
    ) -> Result<LinkDecision, tilo_server_protocol::refusal::Refusal> {
        use tilo_server_protocol::refusal::Refusal;

        let state = link.replica();
        let workspace_id = state.id().map_err(Refusal::Other)?;
        let host = &self.inner.host;
        let now = tilo_clock::unix_now_ns();
        let (certificate, keeps_workspace) = state.read(|peer| {
            // The device a workspace names among its holders is no member and is
            // admitted by the one row its naming writes: the address it signed,
            // carrying its own certificate.
            let keeping = peer
                .governance()
                .keeping_devices()
                .into_iter()
                .find(|keeping| keeping.host.device_public_key == remote);
            let certificate = peer
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == remote)
                .or_else(|| keeping.clone().map(|keeping| keeping.host));
            (certificate, keeping.is_some())
        });
        // A hosted browser environment's device is the server's own: the
        // register holds the certificate it was placed with, which the
        // workspace's roster need not.
        let certificate = certificate
            .or_else(|| host.environments.certificate(&remote))
            .or_else(|| {
                offered
                    .filter(|offered| {
                        offered.device_public_key == remote
                            && tilo_user_identity::verify_device_cert_at(offered, now).is_ok()
                    })
                    .cloned()
            });
        // A device neither holds nor proves is no member's and reaches
        // nothing: its refusal is the link's, not a frame's.
        let Some(certificate) = certificate else {
            return Err(Refusal::NotAMember(
                "this workspace does not hold the device's certificate".into(),
            ));
        };
        let peer = crate::auth::AuthenticatedPeer {
            user_public_key: certificate.user_public_key,
            device_public_key: remote,
            cert: certificate,
        };
        let trust = crate::workspace_access::ServerTrust {
            revocations: &host.revocations,
            policy: &host.trust_policy,
            blocked: &host.blocked,
            own_user: &host.cert.user_public_key,
            agents: &host.agents,
            environments: &host.environments,
        };
        let at = tilo_clock::wall();
        let (decision, key_access) = state.read(|facts| {
            let decide = |op| {
                crate::workspace_access::decide_in(
                    crate::workspace_access::WorkspaceFacts::of_link(facts, workspace_id, at),
                    &trust,
                    &peer,
                    op,
                    now,
                )
            };
            (
                decide(crate::workspace_access::WorkspaceOperation::Subscribe),
                // What a link may publish of the workspace's channels is its
                // own admission for their keys, asked the same way: a device
                // the workspace lets in for less carries no commit of a
                // channel's group.
                decide(crate::workspace_access::WorkspaceOperation::KeyAccess).admitted(),
            )
        });
        Ok(LinkDecision {
            workspace_id,
            decision,
            key_access,
            keeps_workspace,
            peer,
        })
    }

    /// Whether `device` may link to `workspace` now, and what it reaches there
    /// ([`Self::link_decision`]).
    ///
    /// It runs before the link listens, so nothing a device sends is taken
    /// before it is admitted, and it answers the scope the link carries:
    /// everything, or the one record a hosted browser environment reaches.
    /// A device that keeps no workspace passes no other device's frames, and
    /// one the workspace does not admit for its channels' keys carries none of
    /// their commits. A device of someone the workspace has in it that its
    /// member list does not list yet is admitted to sign its own row there,
    /// and to nothing else ([`tilo_peer_state::direct::LinkScope::lists_itself`]): no host writes a device
    /// into the list in its place.
    pub(super) fn admit_link(
        &self,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
        link: &tilo_peer_state::direct::DirectSync,
    ) -> Result<tilo_peer_state::direct::LinkScope, tilo_server_protocol::refusal::Refusal> {
        let LinkDecision {
            workspace_id,
            decision,
            key_access,
            keeps_workspace,
            peer,
        } = self.link_decision(remote, offered, link)?;
        let host = &self.inner.host;
        // A hosted browser environment's device reaches its one record and
        // nothing else here, whatever its person's part in the workspace.
        let reads = crate::environment::environment_of(&host.environments, &remote)
            .and_then(crate::environment::Environment::placed)
            .map(|scope| scope.channel);
        match decision {
            crate::workspace_access::Decision::Admit => {}
            crate::workspace_access::Decision::Deny(
                crate::workspace_access::DenyReason::Unlisted,
            ) => {
                self.hold_for_own_row(workspace_id, remote, link);
                return Ok(tilo_peer_state::direct::LinkScope::listing_itself());
            }
            crate::workspace_access::Decision::Deny(reason) => {
                // A device the person's root retired is told with the entry that retires it, as a
                // refused hello is: its session reads the removal and stops, rather than as a
                // member the workspace took out.
                return Err(match host.revocations.read() {
                    Ok(revocations) => reason.refusal_for(&peer, &revocations),
                    Err(_) => reason.refusal(),
                });
            }
        }
        self.inner
            .link_remotes
            .lock_recover()
            .entry(workspace_id)
            .or_default()
            .insert(remote);
        super::presence_changed();
        // A device that reached its workspace is online from here: its
        // person's record of their keys says so, and when it was last.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let workspaces = self.clone();
            let cert = peer.cert.clone();
            runtime.spawn(async move { workspaces.session_opened(&cert).await });
        }
        Ok(tilo_peer_state::direct::LinkScope {
            reads,
            own_frames_only: !keeps_workspace,
            key_access,
            lists_itself: false,
        })
    }

    /// Hold `remote`'s link to `workspace_id` as one its device signs its own
    /// row on, and watch the workspace's replica until that row is in: then
    /// what admission reads changed, and [`Self::reenforce_links`] ends the
    /// link with [`Refusal::Listed`](tilo_server_protocol::refusal::Refusal::Listed),
    /// so the device dials again as the member device it now is.
    fn hold_for_own_row(
        &self,
        workspace_id: WorkspaceId,
        remote: [u8; 32],
        link: &tilo_peer_state::direct::DirectSync,
    ) {
        self.inner
            .link_remotes
            .lock_recover()
            .entry(workspace_id)
            .or_default()
            .insert(remote);
        if !self
            .inner
            .listing_themselves
            .lock_recover()
            .insert((workspace_id, remote))
        {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let workspaces = self.downgrade();
        let link = link.clone();
        runtime.spawn(async move {
            let mut changes = link.changes();
            loop {
                let Some(held) = workspaces.upgrade() else {
                    return;
                };
                if !held
                    .inner
                    .listing_themselves
                    .lock_recover()
                    .contains(&(workspace_id, remote))
                {
                    return;
                }
                let listed = held
                    .link_decision(remote, None, &link)
                    .is_ok_and(|decided| {
                        decided.decision == crate::workspace_access::Decision::Admit
                    });
                drop(held);
                if listed {
                    crate::authority_epoch::bump();
                    return;
                }
                if changes.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// Re-check every link this host admitted each time a change to what
    /// admission reads is announced ([`crate::authority_epoch`]): a device
    /// the workspace no longer admits has its own links ended, told why, while
    /// every other device's links keep running and are sent what it writes. A
    /// device whose access is back has the links that were ended for it opened
    /// again, and one that signed its own row on a link that carried only that
    /// has that link ended, told it is listed now, so it dials again.
    pub(super) fn watch_link_access(&self) {
        let workspaces = self.downgrade();
        tokio::spawn(async move {
            let mut epoch = crate::authority_epoch::subscribe();
            while epoch.changed().await.is_ok() {
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                workspaces.reenforce_links();
            }
        });
    }

    /// [`Self::watch_link_access`]'s one pass.
    fn reenforce_links(&self) {
        let served: Vec<WorkspaceId> = self
            .inner
            .link_remotes
            .lock_recover()
            .keys()
            .copied()
            .collect();
        for workspace_id in served {
            let Some(link) = self.inner.direct.link(workspace_id) else {
                self.inner.link_remotes.lock_recover().remove(&workspace_id);
                continue;
            };
            let remotes: Vec<[u8; 32]> = self
                .inner
                .link_remotes
                .lock_recover()
                .get(&workspace_id)
                .map(|remotes| remotes.iter().copied().collect())
                .unwrap_or_default();
            // Its own admission re-records each device that still reaches the
            // workspace; this pass's answer replaces the set, and a device it
            // refuses is recorded again so a later widening can open it.
            self.inner.link_remotes.lock_recover().remove(&workspace_id);
            for remote in remotes {
                if self
                    .inner
                    .listing_themselves
                    .lock_recover()
                    .contains(&(workspace_id, remote))
                {
                    self.reenforce_listing_itself(workspace_id, remote, &link);
                    continue;
                }
                let certificate = link.replica().read(|peer| {
                    peer.certificates()
                        .into_iter()
                        .find(|certificate| certificate.device_public_key == remote)
                });
                // A certificate neither the workspace's roster nor the
                // environments' register holds any more holds no link here:
                // nothing a device without one sends is admitted, and it
                // cannot dial again.
                let certificate =
                    certificate.or_else(|| self.inner.host.environments.certificate(&remote));
                let Some(certificate) = certificate else {
                    continue;
                };
                let refusal = if certificate
                    .expires_at_ns
                    .is_some_and(|expiry| expiry <= tilo_clock::unix_now_ns())
                {
                    Some(tilo_server_protocol::refusal::Refusal::NotAMember(
                        "the device's certificate expired".into(),
                    ))
                } else {
                    self.admit_link(remote, None, &link).err()
                };
                match refusal {
                    None => {
                        // It reaches the workspace again: a link it was kept
                        // out of is open to it once more.
                        if link.ended_link(remote).is_some() {
                            link.reopen_link(remote);
                        }
                    }
                    Some(refusal) => {
                        tracing::info!(
                            workspace = %workspace_id,
                            remote = %hex::encode(remote),
                            %refusal,
                            "a workspace's link no longer admits a device it held; its links end"
                        );
                        link.end_link(remote, refusal);
                        self.inner
                            .link_remotes
                            .lock_recover()
                            .entry(workspace_id)
                            .or_default()
                            .insert(remote);
                    }
                }
            }
        }
        // A link this pass ended or opened again moves who is present.
        super::presence_changed();
    }

    /// [`Self::reenforce_links`] for a link its device signs its own row on:
    /// once the row is in, the link ends, told it is listed now, and the
    /// device's next dial is admitted as a member device's; until then it
    /// stays as it is.
    fn reenforce_listing_itself(
        &self,
        workspace_id: WorkspaceId,
        remote: [u8; 32],
        link: &tilo_peer_state::direct::DirectSync,
    ) {
        let listed = self
            .link_decision(remote, None, link)
            .is_ok_and(|decided| decided.decision == crate::workspace_access::Decision::Admit);
        if !listed {
            self.inner
                .link_remotes
                .lock_recover()
                .entry(workspace_id)
                .or_default()
                .insert(remote);
            return;
        }
        self.inner
            .listing_themselves
            .lock_recover()
            .remove(&(workspace_id, remote));
        tracing::info!(
            workspace = %workspace_id,
            remote = %hex::encode(remote),
            "a device signed its own row onto the member list; its link ends so it dials again"
        );
        link.end_link(remote, tilo_server_protocol::refusal::Refusal::Listed);
        // Ended for the link that ran, not for the device: its next dial is
        // a member device's.
        link.reopen_link(remote);
    }
}

/// What [`Workspaces::link_decision`] decided of a link's device.
struct LinkDecision {
    workspace_id: WorkspaceId,
    decision: crate::workspace_access::Decision,
    key_access: bool,
    keeps_workspace: bool,
    peer: crate::auth::AuthenticatedPeer,
}

impl Workspaces {
    /// Replay every record write the device's store keeps for `workspace` into
    /// the governor's copy, and keep what it took in.
    pub(super) async fn recall(&self, workspace: &mut GovernedWorkspace) -> Result<(), String> {
        let retained = self
            .inner
            .records
            .retained(workspace.workspace_id())
            .await?;
        self.recall_retained(workspace, retained).await;
        Ok(())
    }

    /// Take `retained`, the record writes the device's store keeps for `workspace`, into
    /// the governor's copy, and keep what it took in. A write held for an
    /// app surface not here yet may be let go past the held queue's bound,
    /// and so is every later write of its author to its record, which
    /// would otherwise overtake it for good. The store still keeps them
    /// all: they are replayed again, in the order their authors wrote them,
    /// once the surface arrives ([`Self::follow`]) or something they waited
    /// for lands ([`Followed::replay_wanted`]).
    pub(super) async fn recall_retained(
        &self,
        workspace: &mut GovernedWorkspace,
        retained: Vec<crate::workspace_documents::RetainedBody>,
    ) {
        let (mut changed, mut again) = (false, false);
        let mut windows = Vec::new();
        // The documents this replay changed: whoever follows them is told, as a write taken in
        // as it is made tells them, since a replay runs again whenever something a write waited
        // for lands, long after those followers first read the copy.
        let mut written = std::collections::BTreeSet::new();
        for retained in retained {
            let own = retained.author == self.inner.host.cert.device_public_key;
            let Some(message) = retained_message(&retained, own) else {
                continue;
            };
            // What the server's device wrote is kept under its key, the
            // governor's own commits and its own writes of records alike. The
            // first are the governor's copy already, each written through
            // as it was made. The second (the actions the server carries
            // out for members, its agents' answers) the governor took in as
            // they came, in memory, and writes through within
            // `KEEP_FOLLOWED`: a server killed inside that time starts
            // without them, and nobody sends them again. So those are
            // replayed like a member's.
            if own && !written_as_a_record(&message) {
                continue;
            }
            let followed = self
                .follow_kept_checked(workspace, retained.author, message.clone())
                .await;
            self.replay_resource_write(workspace, retained.author, &message)
                .await;
            if let neighbour_wire::NeighbourMsg::SyncResp {
                workspace: named,
                changes,
            } = &message
            {
                if named == workspace.workspace()
                    && !super::unbuildable_windows::holds_all(
                        workspace.peer().admitted_workspace(),
                        changes,
                    )
                {
                    windows.push((retained.author, message));
                }
                continue;
            }
            let Followed {
                changed: took,
                replay_wanted,
                refused,
                behind,
                ..
            } = followed;
            changed |= took;
            again |= replay_wanted;
            if took
                && let neighbour_wire::NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    ..
                } = &message
            {
                written.insert((app_id.clone(), document_id.clone()));
            }
            // A compaction whose record keys are not here yet is kept by the store it came
            // from: the next replay, once they are, takes it in.
            if let Some(reason) =
                refused.filter(|_| behind != Some(super::follow::BehindRefusal::Keys))
            {
                let filed = workspace.refused(
                    &self.inner.host.cert,
                    &self.inner.host.key,
                    &message,
                    &reason,
                );
                changed |= filed.changed;
            } else if took && self.landed(workspace, &message) {
                changed = true;
            }
        }
        if changed {
            workspace.keep_followed();
        }
        for (app, record) in written {
            self.wrote(workspace.workspace(), &app, &record);
        }
        self.recall_windows(workspace, windows).await;
        // What a write let go waited for landed in this replay: the server's
        // copy is read again, with what reached it since this one was read.
        // Each further replay needs something new to land, so it ends.
        if again {
            self.replay_soon(workspace.workspace().to_owned());
        }
    }

    /// Take `windows`, members' changes to the workspace's own state the
    /// server retains and the governor's copy lacks, into it, in the order
    /// they were retained: a member's change the link admitted that the
    /// server stopped before committing. The member's device holds it as the
    /// workspace's, and builds every later change on it, never sending it
    /// again; without it each of those is refused for history the workspace
    /// lost, for as long as the member keeps publishing.
    async fn recall_windows(
        &self,
        workspace: &mut GovernedWorkspace,
        windows: Vec<([u8; 32], NeighbourMsg)>,
    ) {
        let mut taken = 0_usize;
        for (author, message) in windows {
            match self.take_topology(workspace, author, message).await {
                Ok(true) => taken = taken.saturating_add(1),
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(workspace = %workspace.workspace(), %error, "a retained change of a member's was not taken in");
                }
            }
        }
        if taken > 0 {
            tracing::info!(workspace = %workspace.workspace(), taken, "the governor took in members' changes it kept but had not committed");
        }
    }

    /// Found every document of `workspace` that `retained` holds a write of
    /// and this server holds none of, from the document's first retained write
    /// ([`Self::absorb`]): the copy of a document the server was admitted to a
    /// channel of after its members started writing it has to start
    /// somewhere, and its later writes build on the first. A document found
    /// first from a later write would refuse that one for the changes it
    /// lacks, and never be founded.
    #[cfg(any(feature = "agents", feature = "device-keys"))]
    pub(super) async fn found_retained(
        &self,
        workspace: &str,
        state: &tokio::sync::Mutex<GovernedWorkspace>,
        retained: &[crate::workspace_documents::RetainedBody],
    ) {
        let mut first: std::collections::BTreeMap<
            (String, String),
            &crate::workspace_documents::RetainedBody,
        > = std::collections::BTreeMap::new();
        for retained in retained {
            // What the server's own device wrote the governor holds already, or
            // took in as it came ([`Self::recall_retained`]).
            let server = &self.inner.host;
            if retained.author == server.cert.device_public_key {
                continue;
            }
            let Ok(neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                envelope,
                ..
            }) = neighbour_wire::NeighbourMsg::decode(&retained.body)
            else {
                continue;
            };
            // A document the server may read in the clear was founded when its
            // first write was handed to the governor: only a sealed one waits
            // for the group to admit this server's device.
            if envelope.encryption.is_none() {
                continue;
            }
            first.entry((app_id, document_id)).or_insert(retained);
        }
        if first.is_empty() {
            return;
        }
        let missing: Vec<_> = {
            let workspace = state.lock().await;
            first
                .into_iter()
                .filter(|((app, document), _)| !workspace.peer().holds_document(app, document))
                .map(|(_, retained)| retained)
                .collect()
        };
        for retained in missing {
            if let Err(error) = self
                .absorb(workspace, retained.author, &retained.body)
                .await
            {
                tracing::warn!(%workspace, %error, "governed workspace did not found a document its members wrote");
            }
        }
    }
}

/// Whether `message`, which the server's device wrote, is a write of a record
/// it made as any member does: a change to a document or a resource store
/// write. Other whole documents under that key are the governor's own commits.
fn written_as_a_record(message: &neighbour_wire::NeighbourMsg) -> bool {
    matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { document_id, envelope, .. }
            if !envelope.is_whole_resource() || collab_types::is_store_document(document_id)
    )
}

/// The write `retained` holds, decoded; `None` for one that is not a write to
/// replay. Not all the server's device retains under its key (`own`) is a
/// write to a document; anyone else's that does not decode is reported.
fn retained_message(
    retained: &crate::workspace_documents::RetainedBody,
    own: bool,
) -> Option<neighbour_wire::NeighbourMsg> {
    if own {
        return tilo_failure::ignored!(
            "not all the server's device retains under its key is a write to a document",
            neighbour_wire::NeighbourMsg::decode(&retained.body).ok()
        );
    }
    match neighbour_wire::NeighbourMsg::decode(&retained.body) {
        Ok(message) => Some(message),
        Err(error) => {
            tracing::warn!(
                author = %hex::encode(retained.author),
                %error,
                "a retained write does not decode; it is not replayed"
            );
            None
        }
    }
}
