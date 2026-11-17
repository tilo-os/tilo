//! The host's word for the runs it executes, said and heard outside the live
//! live session ([`tilo_server_protocol::host_voice`]).
//!
//! The host's session signs each frame with this device's key and hands it
//! to the relay that is the host; a member's session takes a frame only when
//! the host this session is pinned to signed it, and hands it on as the
//! live session's own events would carry it ([`crate::Event::LiveSession`] for a run's
//! presence, [`crate::Event::Live`] for its narration), from the host's device.
//! A run is keyed by the device that speaks for it, so a member of the live session
//! speaking for the agents it admitted (on the live session, as its own presence
//! is) never speaks for the host's runs.
//!
//! The host's narration of a run travels here alone: no member of the live session
//! narrates in the host's place.

use tilo_server_protocol::host_voice::{HOST_VOICE_VERSION, HostSaid, HostWords};
use tilo_server_protocol::live::{LiveFrame, OpsFrame};
use tilo_server_protocol::transport::Envelope;
use tilo_server_session::RelayClient;

use super::Task;
use tilo_server_protocol::WorkspaceId;

impl Task {
    /// Say `words` for a run this host executes, in `workspace_id`, signed
    /// as this device. Dropped while disconnected, and when it is no run's
    /// word: a run's word is current, and the next one says where it is.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the session task is Send but not Sync: a shared borrow held across the signer's await would make its future not Send"
    )]
    pub(super) async fn say_for_run(
        &mut self,
        workspace_id: WorkspaceId,
        words: HostWords,
        client: Option<&RelayClient>,
    ) {
        let Some(client) = client else {
            return;
        };
        let said = HostSaid {
            version: HOST_VOICE_VERSION,
            workspace_id,
            host: self.config.cert.device_public_key,
            words,
        };
        let Ok((said, signed)) = said.signing_bytes() else {
            return;
        };
        let Ok(signature) = self.signer.sign(signed).await else {
            return;
        };
        let _ = client.send(Envelope::HostSaid {
            workspace_id,
            said,
            signature,
        });
    }

    /// Take the host's word for one of its runs ([`Envelope::HostSaid`]),
    /// when the host this session is pinned to signed it for its workspace:
    /// a run's presence or the narration of its action, handed on as from
    /// the host's device.
    pub(super) fn heard_from_host(&self, envelope: &Envelope) {
        let Envelope::HostSaid {
            workspace_id,
            said,
            signature,
        } = envelope
        else {
            return;
        };
        let workspace_id = *workspace_id;
        let host = self.config.host_cert.device_public_key;
        let Ok(said) = HostSaid::open(said, signature, workspace_id, &host) else {
            return;
        };
        match said.words {
            HostWords::Presence(presence) => {
                let Ok(body) = presence.encode() else {
                    return;
                };
                self.reports.emit(&crate::Event::LiveSession {
                    ws: workspace_id,
                    from: host,
                    track: tilo_live::live::Track::Presence,
                    sequence: 0,
                    body,
                });
            }
            HostWords::Narration { record, event } => {
                let Ok(frame) = LiveFrame::Ops(OpsFrame::Semantic { record, event }).encode()
                else {
                    return;
                };
                self.reports.emit(&crate::Event::Live {
                    ws: workspace_id,
                    from: host,
                    frame,
                });
            }
        }
    }
}

/// Whether an opened live `frame` heard on the live session narrates a run: a run's
/// narration travels only as the host's own ([`Task::heard_from_host`]),
/// never on the live session.
pub(super) fn live_session_narrates_a_run(frame: &[u8]) -> bool {
    matches!(
        LiveFrame::decode(frame),
        Ok(LiveFrame::Ops(OpsFrame::Semantic { event, .. })) if event.on_behalf.is_some()
    )
}
