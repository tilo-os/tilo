//! The host's word for the runs it executes, said and heard outside the live
//! plane ([`tilo_relay_protocol::host_voice`]).
//!
//! The host's session signs each frame with this device's key and hands it
//! to the relay that is the host; a member's session takes a frame only when
//! the host this session is pinned to signed it, and hands it on as the
//! plane's own events would carry it ([`crate::Event::LivePlane`] for a run's
//! presence, [`crate::Event::Live`] for its narration), from the host's device.
//! Readers keep their one rule: what is said for a run is taken only from
//! the device the workspace's governance pins as its host.
//!
//! Nothing said for a run is taken from the plane: a run's word travels here
//! alone, so a host needs no key of the plane to say it, and no member of
//! the plane can say it in the host's place.

use tilo_relay_protocol::host_voice::{HOST_VOICE_VERSION, HostSaid, HostWords};
use tilo_relay_protocol::live::{LiveFrame, OpsFrame};
use tilo_relay_protocol::transport::Envelope;
use tilo_relay_session::RelayClient;

use super::Task;

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
        workspace_id: u64,
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
                self.reports.emit(&crate::Event::LivePlane {
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

/// Whether a presence `body` heard on the plane is said for a run: a run's
/// word travels only as the host's own ([`Task::heard_from_host`]), never on
/// the plane.
pub(super) fn plane_speaks_for_a_run(body: &[u8]) -> bool {
    tilo_live::presence::decode(body).is_some_and(|presence| presence.on_behalf.is_some())
}

/// Whether an opened live `frame` heard on the plane narrates a run: the same
/// rule as [`plane_speaks_for_a_run`], for narration.
pub(super) fn plane_narrates_a_run(frame: &[u8]) -> bool {
    matches!(
        LiveFrame::decode(frame),
        Ok(LiveFrame::Ops(OpsFrame::Semantic { event, .. })) if event.on_behalf.is_some()
    )
}
