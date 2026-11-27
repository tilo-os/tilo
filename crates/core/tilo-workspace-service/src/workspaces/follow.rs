//! What a host does with its members' writes: take them into its copy,
//! file its verdict on the ones admission refused, and write the copy through
//! to the workspace file.

use std::sync::Arc;

use neighbour_wire::NeighbourMsg;

use super::{HostedWorkspace, KEEP_FOLLOWED, WeakWorkspaces, Workspaces, peer_parts};
use ed25519_dalek::SigningKey;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// A member's write to a record of a workspace this device hosts, after the
/// host's copy took it in: what wakes the workspace's installed bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceWrite {
    /// The workspace's UUID.
    pub workspace: String,
    /// The record's app.
    pub app: String,
    /// The record's id on the wire (`""` for the app's primary).
    pub record: String,
    /// The records the same write changed, this one among them: one action writes a message
    /// and the files it carries to documents of their own, and whoever reads the change reads
    /// them together. The write's notices share one list, so a write of many documents names
    /// them once.
    pub together: std::sync::Arc<Vec<String>>,
}

/// What taking a member's write into the host's copy did.
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
    /// [`HostedWorkspace::follow_kept`], keeping this host's check of a compaction the copy
    /// took: one taken behind was checked as it was taken, against every write here it covers,
    /// and no later read can check it against the writes only it holds; any other is checked
    /// against the writes it stands in for now. The stores keep those writes until every device
    /// with the record's keys has checked it, so a copy that took one on a replay and never
    /// checked it would keep them everywhere for good.
    pub(super) async fn follow_kept_checked(
        &self,
        workspace: &mut HostedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Followed {
        let followed = workspace.follow_kept(author, message.clone()).await;
        #[cfg(feature = "device-keys")]
        if let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = &message
        {
            let route = (app_id.as_str(), document_id.as_str());
            let checked = match &followed.taken_behind {
                Some(compaction) => self.keep_checkpoint_check(workspace, route, compaction),
                None if followed.changed && envelope.kind == collab_types::OpKind::Compaction => {
                    self.check_leading_checkpoint(workspace, route).await
                }
                None => Ok(()),
            };
            if let Err(error) = checked {
                tracing::warn!(workspace = %workspace.workspace(), %app_id, %document_id, %error,
                    "this host's check of a compaction it took was not kept");
            }
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
        workspace: &HostedWorkspace,
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
        workspace: &HostedWorkspace,
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
        workspace: &HostedWorkspace,
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

impl HostedWorkspace {
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
        // A device placed for records is in no member list: its certificate alone says what it
        // writes, and the workspace's admission decides its write by that certificate.
        let placed = self.peer().placed_device(&author, now);
        if let Some(placed) = placed {
            let placed = placed.map_err(|error| error.to_string())?;
            return if placed
                .records
                .reaches(&neighbour_wire::app_document_group(app, document))
            {
                Ok(())
            } else {
                Err(tilo_peer_state::MembershipError::OutsideItsRecords.to_string())
            };
        }
        let member = self
            .peer()
            .permissions(author, now)
            .map_err(|error| error.to_string())?;
        let node = collab_types::validator::document_node(&member.policy.workspace, app, document);
        if !member.allows(&node, tilo_action_types::ActionKind::Undoable, now) {
            return Err(format!(
                "{app} record written by a device that may not edit"
            ));
        }
        Ok(())
    }

    /// Carry this host's own changes of the workspace not carried yet, in the
    /// order they were made, each as the frame its device signed. Stops at a
    /// change that could not be carried, with why, for the next settle to
    /// carry again.
    pub(super) fn carry_unsent(
        &mut self,
        records: &crate::workspace_documents::WorkspaceDocuments,
    ) -> Result<(), String> {
        while let Some(frame) = self.unsent.first() {
            let body = postcard::to_stdvec(frame).map_err(|error| error.to_string())?;
            records
                .commit(
                    self.record.workspace_id,
                    &crate::workspace_documents::HostCommit { body },
                )
                .map_err(|error| format!("this host's change was not carried: {error}"))?;
            self.unsent.remove(0);
        }
        Ok(())
    }

    /// Write the host's copy, as following members' writes changed it,
    /// through to the workspace file. A start replays what the server retains,
    /// but that replay alone does not bring a write back: the app surfaces
    /// it is decided against are still on their way then, so it holds the
    /// write and may let it go. A write the host took in and lost that
    /// way left its record with a gap, once a later write that builds on it
    /// was committed, and the workspace no longer opened.
    pub(super) fn keep_followed(&mut self) {
        let Some((files, write)) = self.keep_later() else {
            return;
        };
        if let Err(error) = files.write(&write) {
            self.unsure();
            self.unkept = true;
            tracing::warn!(workspace = %self.workspace(), %error, "the host's copy of members' writes was not kept");
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
                tracing::warn!(workspace = %self.workspace(), %error, "the host's copy of members' writes was not kept");
                None
            }
        }
    }

    /// Take a member's write into the host's copy, in memory:
    /// [`Self::keep_followed`] writes it through. A retained checkpoint can
    /// also restore signed history for a document placed by the store.
    /// Admission refusals name the reason in [`Followed::refused`] so the
    /// host can file its verdict in the workspace document.
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
            // A write this copy holds the record of is taken in. So is one that is the record
            // whole: a sealed checkpoint starts a record, and a replay that skipped it because
            // this copy held none of the record yet would lose the write the server answered it
            // kept, and every write after it, from the workspace's copy for good.
            if !(peer.holds_document(app_id, document_id) || envelope.is_whole_resource()) {
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
            // Asked for with the lock let go: the compaction is taken once they are here.
            Err(super::wanted_keys::Unkept::Keys(wanted)) => {
                wanted.ask_later();
                return Err(BehindRefusal::Keys);
            }
            Err(super::wanted_keys::Unkept::Refused(error)) => {
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

    /// File this device's verdict on the write admission refused as
    /// `refusal` says, in the host's copy, for [`Self::keep_followed`] to write
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
                    "the host did not file a refusal of a record write"
                );
                Followed::default()
            }
        }
    }
}

impl Workspaces {
    /// Replay what the server retains for `workspace` into the host's copy
    /// ([`Self::recall_retained`]), apart from whatever asked for it. The
    /// server's copy is read once every write the host was handed is
    /// durable in it, and before the workspace is locked: their delivery waits
    /// for the host to follow them.
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
    /// within [`KEEP_FOLLOWED`], once for all of them. A workspace this host serves writes its
    /// writes through before it answers them ([`HostedWorkspace::keep_followed`]); this is for a member
    /// copy, whose durable copy is the device serving it, so a part that ends here starts again
    /// from there.
    pub(super) fn keep_soon(&self, workspace: &mut HostedWorkspace) {
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
                tracing::warn!(workspace = %workspace.workspace(), %error, "the host's copy of members' writes was not kept");
            }
        });
    }
}

impl Workspaces {
    /// Serve `workspace`'s replica over the direct link, keeping its history
    /// and its channels' governance at the host's store, and take in what its
    /// members send there.
    ///
    /// What the store kept of the workspace's own state is judged first, as
    /// the link judges it: a change the workspace refuses is dropped before
    /// any device is sent it.
    pub(super) async fn serve_direct(&self, workspace: &HostedWorkspace) -> Result<(), String> {
        let hooks = std::sync::Arc::clone(&self.inner.versions);
        let announcing: std::sync::Arc<tilo_peer_state::direct::Versions> =
            std::sync::Arc::new(move |device, theirs| {
                for hook in hooks.lock_recover().iter() {
                    hook(device, theirs);
                }
            });
        let link = tilo_peer_state::direct::DirectSync::over(
            workspace.peer.clone(),
            self.inner.host.key.clone(),
        )
        .with_store(Arc::clone(&self.inner.store))
        .announcing(announcing);
        link.drop_refused_workspace_changes().await;
        self.inner.direct.serve(link.clone())?;
        self.follow_link(workspace, &link);
        self.keep_pushing(workspace.workspace(), &link);
        Ok(())
    }

    /// Take in what `workspace`'s link admits and answer what its members ask
    /// there: a frame the link admitted is a member's write, followed into the
    /// host's copy ([`Self::absorb`]), and each ask for content is answered
    /// with what this host keeps of it, or word that it keeps none, addressed
    /// to whoever asked. The link carries each ask on to the workspace's other
    /// members itself.
    ///
    /// A reader that falls behind the link's frames takes in the writes it
    /// missed before it reads on ([`tilo_peer_state::direct::Reader`]): a
    /// member is told its write landed once the link admitted it, so the host
    /// following it is not the member's to retry. Only the link ending stops
    /// the reads.
    fn follow_link(&self, workspace: &HostedWorkspace, link: &tilo_peer_state::direct::DirectSync) {
        let name = workspace.workspace().to_owned();
        let workspaces = WeakWorkspaces(Arc::downgrade(&self.inner));
        let link = link.clone();
        let intake = self.content_intake(&name);
        let asks = self.content_asks(&name, &link);
        // Subscribed, where the replica stands, before the task runs:
        // every frame the link admits from here on is this reader's, and what
        // the replica held before is not read again.
        let mut reader = link.reader();
        let (follower, mut asked) = tokio::sync::mpsc::unbounded_channel();
        self.inner
            .followers
            .lock_recover()
            .insert(name.clone(), follower);
        tokio::spawn(async move {
            loop {
                // An ask is answered only while no frame waits: every frame told before it is in.
                let frame = tokio::select! {
                    biased;
                    frame = reader.recv() => frame,
                    Some(ask) = asked.recv() => {
                        let _ = ask.send(());
                        continue;
                    }
                };
                // The link is gone: nothing more comes through it.
                let Some(frame) = frame else {
                    return;
                };
                // The host this link served is gone: so is the answer.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let asking = matches!(frame.message, NeighbourMsg::DocumentContentWants { .. });
                let answering = matches!(
                    frame.message,
                    NeighbourMsg::DocumentContentPieces { .. }
                        | NeighbourMsg::DocumentContentLacks { .. }
                        | NeighbourMsg::DocumentContentRefused { .. }
                        | NeighbourMsg::DocumentContentReleased { .. }
                        | NeighbourMsg::DocumentContentKept { .. }
                );
                // What a member pushes or answers the host with, and what it
                // lets go of, is the host's to keep: a device that keeps
                // its workspace's files reads the pieces here
                // ([`Self::content_for_host`]). Asks and answers are no record
                // writes: they go to the content's own intake, as they came,
                // never through the host's copy.
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
                // The writes that waited in the copy for what this frame brought were taken in
                // with it, and are kept and told as each would have been had it come in order:
                // a record's first write that came before the change it builds on is read, and
                // its content asked for, once it lands, not when its next write comes.
                let released = frame
                    .report
                    .as_ref()
                    .map(tilo_peer_state::AdmissionReport::released)
                    .unwrap_or_default();
                for released in released {
                    if !matches!(released.message, NeighbourMsg::AppDocument { .. }) {
                        continue;
                    }
                    let Ok(body) = postcard::to_allocvec(&released.message) else {
                        continue;
                    };
                    if let Err(error) = workspaces.absorb(&name, released.author, &body).await {
                        tracing::warn!(%name, %error, "the host did not keep a write its link let in");
                    }
                }
            }
        });
    }

    /// Wait until the follower of `workspace`'s link took in every frame the link told it so far:
    /// what this device's copy reads holds them then. At once for a workspace this device serves
    /// no link of.
    pub(super) async fn followed(&self, workspace: &str) {
        let (ask, answer) = tokio::sync::oneshot::channel();
        let asked = self
            .inner
            .followers
            .lock_recover()
            .get(workspace)
            .is_some_and(|follower| follower.send(ask).is_ok());
        if asked {
            let _ = answer.await;
        }
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

    /// What the one admission rule ([`crate::workspace_access::decide_in`])
    /// decides of `remote` linking to the workspace `link` serves, asked of
    /// the workspace's replica, which the link admits into
    /// ([`Self::decision_in`]).
    fn link_decision(
        &self,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
        link: &tilo_peer_state::direct::DirectSync,
    ) -> Result<LinkDecision, tilo_server_protocol::refusal::Refusal> {
        self.decision_in(&link.replica(), remote, offered)
    }

    /// What the one admission rule ([`crate::workspace_access::decide_in`])
    /// decides of `remote` in the workspace `state` is a replica of: the
    /// decision for a subscribe, whether it holds the channels' keys, whether
    /// the device keeps the workspace, and the device as it was decided.
    ///
    /// The device is the one the workspace's roster certifies, one placed for
    /// its records whose certificate the replica took in, a hosted browser
    /// environment's this server placed, or else the certificate it named
    /// itself by (`offered`, on its dial or its action link), once it
    /// verifies as that device's.
    fn decision_in(
        &self,
        state: &tilo_peer_state::direct::Replica,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
    ) -> Result<LinkDecision, tilo_server_protocol::refusal::Refusal> {
        use tilo_server_protocol::refusal::Refusal;

        let workspace_id = state.id().map_err(Refusal::Other)?;
        let host = &self.inner.host;
        let now = tilo_clock::unix_now_ns();
        let certificate = state.read(|peer| {
            // The device a workspace names among its holders is no member and is
            // admitted by the one row its naming writes: the address it signed,
            // carrying its own certificate.
            let keeping = peer
                .governance()
                .keeping_devices()
                .into_iter()
                .find(|keeping| keeping.host.device_public_key == remote);
            peer.certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == remote)
                .or_else(|| keeping.map(|keeping| keeping.host))
                .or_else(|| peer.placed_device_certificate(&remote))
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
            environments: &host.environments,
        };
        let at = tilo_clock::wall();
        let (decision, key_access) = state.read(|facts| {
            let decide = |op| {
                crate::workspace_access::decide_in(
                    &crate::workspace_access::WorkspaceFacts::of_link(facts, workspace_id, at),
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
            peer,
        })
    }

    /// Whether `device` may link to `workspace` now, and what it reaches there
    /// ([`Self::admit_device`]).
    ///
    /// It runs before the link listens, so nothing a device sends is taken
    /// before it is admitted, and it answers the scope the link carries:
    /// everything, or the records a placed device or a hosted browser
    /// environment reaches. A device that keeps no workspace passes no other
    /// device's frames, and one the workspace does not admit for its channels'
    /// keys carries none of their commits. A device of someone the workspace
    /// has in it that its member list does not list yet is admitted to sign its
    /// own row there, and to nothing else
    /// ([`tilo_peer_state::direct::LinkScope::lists_itself`]): no host writes a
    /// device into the list in its place.
    pub(super) fn admit_link(
        &self,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
        link: &tilo_peer_state::direct::DirectSync,
    ) -> Result<tilo_peer_state::direct::LinkScope, tilo_server_protocol::refusal::Refusal> {
        let admitted = self.admit_device(&link.replica(), remote, offered)?;
        if admitted.scope.lists_itself {
            self.hold_for_own_row(admitted.workspace, remote, link);
            return Ok(admitted.scope);
        }
        self.inner
            .link_remotes
            .lock_recover()
            .entry(admitted.workspace)
            .or_default()
            .insert(remote);
        super::presence_changed();
        // A device that reached its workspace is online from here: its
        // person's record of their keys says so, and when it was last.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let workspaces = self.clone();
            let cert = admitted.cert;
            runtime.spawn(async move {
                workspaces.session_opened(&cert).await;
                // This device's own session opened: its record now says where its endpoint
                // answers, so its person's other devices dial it directly when no relay and no
                // discovery find it.
                if cert.device_public_key == workspaces.inner.host.cert.device_public_key
                    && let Err(error) = workspaces.say_endpoint().await
                {
                    tracing::debug!(
                        %error,
                        "the person's record does not say where this device answers"
                    );
                }
            });
        }
        Ok(admitted.scope)
    }

    /// Take in `certificate`, which a device placed for records of `workspace` presented over the
    /// action link it dialled this host on, by the admission its state link is taken in by
    /// ([`Self::admit_device`]): a host that restarted, or a device that asks before its state
    /// link said hello, decides it by the certificate it proves again on each link. Whether the
    /// workspace's copy now decides the device as placed.
    pub(super) async fn admit_placed_device(
        &self,
        workspace: &str,
        certificate: DeviceCert,
    ) -> bool {
        if certificate.capabilities.records.is_none() {
            return false;
        }
        let held = self.inner.workspaces.lock().await.get(workspace).cloned();
        let Some(held) = held else {
            return false;
        };
        let state = held.lock().await.peer.clone();
        let device = certificate.device_public_key;
        match self.admit_device(&state, device, Some(&certificate)) {
            Ok(admitted) => admitted.placed,
            Err(refusal) => {
                tracing::debug!(
                    device = %hex::encode(&device[..4]),
                    %workspace,
                    ?refusal,
                    "a placed device's certificate is refused on its action link"
                );
                false
            }
        }
    }

    /// Whether `remote` may hold a link of `state`'s workspace now, and what it reaches there
    /// ([`Self::decision_in`]). A device placed for records is taken into the workspace's copy as
    /// its hello takes it in, and decided by that certificate from then on.
    fn admit_device(
        &self,
        state: &tilo_peer_state::direct::Replica,
        remote: [u8; 32],
        offered: Option<&DeviceCert>,
    ) -> Result<AdmittedDevice, tilo_server_protocol::refusal::Refusal> {
        use tilo_server_protocol::refusal::Refusal;

        let LinkDecision {
            workspace_id,
            decision,
            key_access,
            peer,
        } = self.decision_in(state, remote, offered)?;
        let host = &self.inner.host;
        // A device placed for records reaches those records and nothing else here, whatever its
        // person's part in the workspace: its certificate says which, the same way on every
        // device that keeps the workspace. A device this host placed with a certificate that
        // names no records (a Linux apps session's) is confined by the register that placed it.
        let workspace_name = workspace_id.to_string();
        let placed =
            tilo_peer_state::direct::LinkScope::of_placed_device(&peer.cert, &workspace_name);
        if peer.cert.capabilities.records.is_some() && placed.is_none() {
            return Err(Refusal::NotAMember(
                "the device was placed for records of another workspace".into(),
            ));
        }
        let reads = crate::environment::environment_of(&host.environments, &remote)
            .and_then(crate::environment::Environment::placed)
            .map(|scope| vec![scope.channel]);
        match decision {
            crate::workspace_access::Decision::Admit => {}
            // A placed device is in no member list, and never signs a row there.
            crate::workspace_access::Decision::Deny(
                crate::workspace_access::DenyReason::Unlisted,
            ) if placed.is_none() => {
                return Ok(AdmittedDevice {
                    workspace: workspace_id,
                    cert: peer.cert,
                    placed: false,
                    scope: tilo_peer_state::direct::LinkScope::listing_itself(),
                });
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
        // A placed device's copy proves it by its hello, which may reach this host after the
        // link's first frames are sent: the host takes in the certificate it admitted the link by
        // first, so the device's own records reach it from the first frame, decided by that
        // certificate as on every device.
        if placed.is_some() {
            let hello = NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(peer.cert.clone()),
                revocations: Vec::new(),
            };
            state
                .write(|replica| {
                    replica.receive_from(
                        tilo_peer_state::TransportOrigin::ServerDelta(remote),
                        hello,
                        tilo_clock::wall(),
                    )
                })
                .map_err(|error| Refusal::NotAMember(error.to_string()))?;
        }
        let is_placed = placed.is_some();
        let scope = placed.unwrap_or(tilo_peer_state::direct::LinkScope {
            reads,
            // Whether the device passes on other devices' frames is the link's to read for
            // each frame, as the workspace names its keeping devices then: a device named
            // after its link came up passes them on from then.
            own_frames_only: false,
            key_access,
            lists_itself: false,
        });
        Ok(AdmittedDevice {
            workspace: workspace_id,
            cert: peer.cert,
            placed: is_placed,
            scope,
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
                        .or_else(|| peer.placed_device_certificate(&remote))
                });
                let certificate =
                    certificate.or_else(|| self.inner.host.environments.certificate(&remote));
                let refusal = match certificate {
                    // A device this host placed and stopped keeps no link here: the register
                    // forgot the certificate it was placed with, and says it stopped.
                    None if crate::environment::environment_of(
                        &self.inner.host.environments,
                        &remote,
                    )
                    .is_some() =>
                    {
                        Some(tilo_server_protocol::refusal::Refusal::NotAMember(
                            "the host stopped the device".into(),
                        ))
                    }
                    // Any other certificate neither the workspace's roster nor the
                    // environments' register keeps any more keeps no link here: nothing a
                    // device without one sends is admitted, and it cannot dial again.
                    None => continue,
                    Some(certificate)
                        if certificate
                            .expires_at_ns
                            .is_some_and(|expiry| expiry <= tilo_clock::unix_now_ns()) =>
                    {
                        Some(tilo_server_protocol::refusal::Refusal::NotAMember(
                            "the device's certificate expired".into(),
                        ))
                    }
                    Some(_) => self.admit_link(remote, None, &link).err(),
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
    peer: crate::auth::AuthenticatedPeer,
}

impl Workspaces {
    /// Replay every record write the device's store keeps for `workspace` into
    /// the host's copy, and keep what it took in. A sealed write the copy
    /// holds and the store lost is carried again, as the bytes its author
    /// signed: a device that links later reads it from the store as from
    /// anyone who kept it.
    pub(super) async fn recall(&self, workspace: &mut HostedWorkspace) -> Result<(), String> {
        let retained = self
            .inner
            .records
            .retained(workspace.workspace_id())
            .await?;
        let kept: std::collections::HashSet<collab_types::Signature> = retained
            .iter()
            .filter_map(|retained| match retained_message(retained, true) {
                Some(neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }) => {
                    Some(envelope.signature)
                }
                _ => None,
            })
            .collect();
        self.recall_retained(workspace, retained).await;
        self.carry_held(workspace, &kept)
    }

    /// Carry each sealed write `workspace`'s copy holds whose signature is not
    /// among `kept`, under its author, unchanged.
    fn carry_held(
        &self,
        workspace: &HostedWorkspace,
        kept: &std::collections::HashSet<collab_types::Signature>,
    ) -> Result<(), String> {
        // Read out under the copy's lock and carried after it is let go: the
        // link keeping a frame reads the same copy.
        let held: Vec<_> = {
            let peer = workspace.peer();
            peer.records()
                .into_iter()
                .flat_map(|(app, record)| {
                    peer.publication()
                        .sealed_history(&app, &record)
                        .into_iter()
                        .filter(|op| !kept.contains(&op.signature))
                        .map(move |op| (app.clone(), record.clone(), op))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        for (app, record, op) in held {
            let author = *op.author.as_bytes();
            let message = neighbour_wire::NeighbourMsg::AppDocument {
                peer: tilo_peer_state::peer_handle(&author),
                app_id: app,
                cluster_id: workspace.workspace().to_owned(),
                document_id: record,
                envelope: Box::new(op),
                readers: None,
            };
            let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
            self.inner.records.carry(
                workspace.workspace_id(),
                author,
                &crate::workspace_documents::HostCommit { body },
            )?;
        }
        Ok(())
    }

    /// Take `retained`, the record writes the device's store keeps for `workspace`, into
    /// the host's copy, and keep what it took in. A write held for an
    /// app surface not here yet may be let go past the held queue's bound,
    /// and so is every later write of its author to its record, which
    /// would otherwise overtake it for good. The store still keeps them
    /// all: they are replayed again, in the order their authors wrote them,
    /// once the surface arrives ([`Self::follow`]) or something they waited
    /// for lands ([`Followed::replay_wanted`]).
    pub(super) async fn recall_retained(
        &self,
        workspace: &mut HostedWorkspace,
        retained: Vec<crate::workspace_documents::RetainedBody>,
    ) {
        let (mut changed, mut again) = (false, false);
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
            // host's own commits and its own writes of records alike. The
            // first are the host's copy already, each written through
            // as it was made. The second (the actions the server carries
            // out for members, its agents' answers) the host took in as
            // they came, in memory, and writes through within
            // `KEEP_FOLLOWED`: a server killed inside that time starts
            // without them, and nobody sends them again. So those are
            // replayed like a member's.
            // Changes of the workspace's own document are taken as the link
            // admits them, and kept then: none waits in the store for this.
            if (own && !written_as_a_record(&message))
                || matches!(message, neighbour_wire::NeighbourMsg::SyncResp { .. })
            {
                continue;
            }
            let followed = self
                .follow_kept_checked(workspace, retained.author, message.clone())
                .await;
            self.replay_resource_write(workspace, retained.author, &message)
                .await;
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
        // What a write let go waited for landed in this replay: the server's
        // copy is read again, with what reached it since this one was read.
        // Each further replay needs something new to land, so it ends.
        if again {
            self.replay_soon(workspace.workspace().to_owned());
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
        state: &tokio::sync::Mutex<HostedWorkspace>,
        retained: &[crate::workspace_documents::RetainedBody],
    ) {
        let mut first: std::collections::BTreeMap<
            (String, String),
            &crate::workspace_documents::RetainedBody,
        > = std::collections::BTreeMap::new();
        for retained in retained {
            // What the server's own device wrote the host holds already, or
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
            // first write was handed to the host: only a sealed one waits
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
                tracing::warn!(%workspace, %error, "the workspace's host did not found a document its members wrote");
            }
        }
    }
}

/// Whether `message`, which the server's device wrote, is a write of a record
/// it made as any member does: a change to a document or a resource store
/// write. Other whole documents under that key are the host's own commits.
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

/// A device [`Workspaces::admit_device`] admitted: the workspace, the certificate it was admitted
/// by, whether it is placed for records, and what its link reaches.
struct AdmittedDevice {
    workspace: tilo_server_protocol::WorkspaceId,
    cert: DeviceCert,
    placed: bool,
    scope: tilo_peer_state::direct::LinkScope,
}
