//! What a governor does with its members' writes: take them into its copy,
//! file its verdict on the ones admission refused, and write the copy through
//! to the workspace file.

use super::{GovernedWorkspace, Workspaces, publisher};
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
}

impl Workspaces {
    /// Tell whoever follows the workspace's documents that `message`, a member's
    /// write of `document` of `app`, is in `workspace`'s copy: the write that
    /// starts a document as every write after it, since a follower nobody told
    /// reads it only when it next looks at everything. Sent while the
    /// workspace is still locked, so whoever it wakes reads a copy that holds
    /// the write.
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
        (app, document): (String, String),
        message: &neighbour_wire::NeighbourMsg,
    ) {
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
            let channel = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&entry.bytes)
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
        self.unkept = false;
        let staged = {
            let replica = self.peer.clone();
            let peer = replica.lock();
            self.parts.stage(&peer)
        };
        let kept = staged.and_then(|parts| {
            let prior = std::mem::replace(&mut self.record.peer, parts);
            self.persist().inspect_err(|_| self.record.peer = prior)
        });
        if let Err(error) = kept {
            tracing::warn!(workspace = %self.record.workspace, %error, "the governor's copy of members' writes was not kept");
        }
    }

    /// Take a member's write to a record the governor has into its copy,
    /// in memory: [`Self::keep_followed`] writes it through. What admission
    /// refused, it names with why in [`Followed::refused`], for the governor to
    /// file as its verdict in the workspace document.
    pub(super) fn follow(
        &self,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Followed {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = &message
        else {
            return Followed::default();
        };
        let received = {
            let mut peer = self.peer();
            if !peer.holds_document(app_id, document_id) {
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
        let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(body).ok()?;
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
            return None;
        }
        let state = self.workspace(workspace).await.ok()?;
        state.lock().await.refuses(author, &message)
    }
}
