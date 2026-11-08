//! What a governor does with its members' writes: take them into its copy,
//! file its verdict on the ones admission refused, and write the copy through
//! to the workspace file.

use super::{GovernedWorkspace, publisher};
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// What taking a member's write into the governor's copy did.
#[derive(Clone, Debug, Default)]
pub(super) struct Followed {
    /// The copy changed.
    pub(super) changed: bool,
    /// A write the copy let go earlier waited for something that has landed
    /// since: replaying what the relay retains brings it in.
    pub(super) replay_wanted: bool,
    /// Admission refused the write, and why: the author is owed a verdict.
    pub(super) refused: Option<String>,
}

impl GovernedWorkspace {
    /// The governance this workspace published that the relay has not retained,
    /// in order, each with its place reserved in `records`; `published`
    /// moves past them. Stops before a change that does not read, with why.
    pub(super) fn unpublished(
        &mut self,
        records: &crate::workspace_records::WorkspaceRecords,
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
                crate::workspace_records::HostCommit {
                    body: entry.bytes.clone(),
                    channel,
                    checkpoint: entry.checkpoint,
                },
            ));
            self.record.published = self.record.published.saturating_add(1);
        }
        (publications, Ok(()))
    }

    /// Write the governor's copy, as following members' writes changed it,
    /// through to the workspace file. A start replays what the relay retains,
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
            app_id, record_id, ..
        } = &message
        else {
            return Followed::default();
        };
        let received = {
            let mut peer = self.peer();
            if !peer.holds_document(app_id, record_id) {
                return Followed::default();
            }
            peer.receive_from(
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
                message,
                SystemTime::now(),
            )
        };
        match received {
            Ok(outcome) => Followed {
                changed: outcome.changed || outcome.metadata_changed,
                replay_wanted: outcome.replay_wanted,
                refused: None,
            },
            Err(error) => Followed {
                refused: Some(error),
                ..Followed::default()
            },
        }
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
            SystemTime::now(),
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
