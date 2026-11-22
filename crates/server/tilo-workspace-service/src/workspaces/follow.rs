//! What a governor does with its members' writes: take them into its copy,
//! file its verdict on the ones admission refused, and write the copy through
//! to the workspace file.

use std::sync::Arc;

use super::{GovernedWorkspace, KEEP_FOLLOWED, Workspaces, peer_parts, publisher};
use ed25519_dalek::SigningKey;
use tilo_peer_state::TransportOrigin;
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
    /// What the write carried that the record's repair could take in
    /// ([`super::repaired_documents::Carried`]): the changes it carried, when the
    /// record's kept changes could wait for one of them. Nothing when the
    /// record's archive says nothing it keeps waits for a write like this, so
    /// reading the record for it would be work for nothing.
    #[cfg(feature = "agents")]
    pub carried: super::repaired_documents::Carried,
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
    pub(super) refused: Option<String>,
    /// The write is a sealed compaction this copy refused because it is behind in it, and why
    /// it could not take it yet ([`BehindRefusal`]).
    pub(super) behind: Option<BehindRefusal>,
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
    /// Tell whoever follows the workspace's documents that `message`, `author`'s
    /// write of `document` of `app`, is in `workspace`'s copy: the write that
    /// starts a document as every write after it, since a follower nobody told
    /// reads it only when it next looks at everything. Sent while the
    /// workspace is still locked, so whoever it wakes reads a copy that holds
    /// the write. Another device's write is also handed on whole
    /// ([`Workspaces::admitted_writes`]), for this device's apps to show, as
    /// `history` the workspace already held or as a write made just now.
    #[cfg_attr(
        not(feature = "agents"),
        expect(
            clippy::unused_async,
            reason = "one signature with the build that reads what a write carried"
        )
    )]
    pub(super) async fn tell_followers(
        &self,
        workspace: &GovernedWorkspace,
        author: [u8; 32],
        (app, document): (String, String),
        message: &neighbour_wire::NeighbourMsg,
        history: bool,
    ) {
        self.hand_on(workspace, author, message, history);
        // What the write carried, read here where it is at hand, so a document
        // whose repair keeps changes is read only for a write that could
        // release one of them.
        #[cfg(feature = "agents")]
        let carried = {
            let name = workspace.workspace().to_owned();
            self.carried_into(workspace, (&name, &app, &document), message)
                .await
        };
        #[cfg(not(feature = "agents"))]
        let _ = message;
        let _ = self.inner.document_writes.send(ResourceWrite {
            workspace: workspace.workspace().to_owned(),
            app,
            record: document,
            #[cfg(feature = "agents")]
            carried,
        });
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

    /// The governance this workspace published that the server has not retained,
    /// in order, each with its place reserved in `records`; `published`
    /// moves past them. Stops before a change that does not read, with why.
    pub(super) fn unpublished(
        &mut self,
        records: &crate::workspace_documents::WorkspaceDocuments,
    ) -> (Vec<publisher::Publication>, Result<(), String>) {
        let mut publications = Vec::new();
        while let Some(entry) = self.record.outgoing.get(self.record.published) {
            let channel = neighbour_wire::NeighbourMsg::decode(&entry.bytes)
                .map_err(|error| error.to_string())
                .and_then(|message| {
                    neighbour_wire::retention(&message, entry.checkpoint)
                        .map(|(channel, _)| channel)
                        .ok_or_else(|| {
                            "workspace governance message has no retention channel".into()
                        })
                });
            let channel = match channel {
                Ok(channel) => channel,
                Err(error) => return (publications, Err(error)),
            };
            publications.push(publisher::Publication::new(
                records,
                self.record.workspace_id,
                Some(self.record.published),
                crate::workspace_documents::HostCommit {
                    body: entry.bytes.to_vec(),
                    channel,
                    checkpoint: entry.checkpoint,
                },
            ));
            self.record.published = self.record.published.saturating_add(1);
        }
        (publications, Ok(()))
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
                refused: Some(error.to_string()),
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

    /// The verdict [`Self::follow`] would give `message`, `author`'s write,
    /// without taking it in: why the workspace refuses the write, or `None`
    /// when it takes it. The server asks before it keeps the write, so a
    /// refused change is never stored and never served to a reader, and a
    /// writer numbering its next change the refused one's sequence again
    /// never meets it. `None` for a record the governor's copy does not hold:
    /// that write founds one, which is no refusal.
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
                TransportOrigin::RetainedServerDelta(author),
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

    /// File this device's verdict on the write admission refused because
    /// `reason`, in the governor's copy, for [`Self::keep_followed`] to write
    /// through: the write's author reads why it is not here.
    pub(super) fn refused(
        &self,
        caller: &DeviceCert,
        key: &SigningKey,
        message: &neighbour_wire::NeighbourMsg,
        reason: &str,
    ) -> Followed {
        let filed = self.peer().workspace_mut().refuse_write(
            caller,
            message,
            reason,
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
                    %reason,
                    %error,
                    "the governor did not file a refusal of a record write"
                );
                Followed::default()
            }
        }
    }
}

impl Workspaces {
    /// Why this workspace refuses `body`, `author`'s write, decided before the
    /// server keeps or forwards it; `None` when it takes the write, when this
    /// server does not govern the workspace, or when the write founds the
    /// record rather than changing one. The verdict is the one the governor
    /// would give the write ([`GovernedWorkspace::refuses`]), so the change is
    /// never stored for a later reader to meet, and a writer numbering its
    /// next change the refused one's sequence again writes on cleanly.
    ///
    /// A sealed compaction the copy is behind in is not refused: it is taken as
    /// the record's base under the record's keys. One whose keys are not here yet is kept, as
    /// any sealed frame the store cannot read is, and a replay takes it once they are
    /// ([`GovernedWorkspace::follow_kept`]). The writes it covers are nowhere but inside it, and
    /// a change refused here is never kept.
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
        let followed = governed.follow_kept(author, message.clone()).await;
        // A compaction this copy cannot open yet is kept, not refused: it is a sealed frame the
        // retained store keeps like any it cannot read, and the replay takes it once the keys
        // are here.
        if followed.behind == Some(BehindRefusal::Keys) {
            drop(governed);
            return None;
        }
        if let Some(refused) = followed.refused {
            drop(governed);
            return Some(refused);
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
}

impl Workspaces {
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
