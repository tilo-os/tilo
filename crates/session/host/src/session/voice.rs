//! The page's call participation: joining, leaving and what it shows, told to
//! the host that governs the call. The voice and the pictures themselves are
//! tracks of the page's own plane ([`super::plane`]).

use super::*;

impl PeerHandle {
    /// Ask the host for the calls of `workspace_id`; the answer arrives as
    /// [`VoiceEvent::Calls`](crate::voice::VoiceEvent::Calls).
    pub fn voice_calls(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceCalls(workspace_id))
    }

    /// Start, end or moderate a call of `workspace_id`; the answer arrives
    /// as [`VoiceEvent::CallResult`](crate::voice::VoiceEvent::CallResult)
    /// for `request`.
    pub fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
    ) -> Result<(), String> {
        self.send(Command::VoiceCall(workspace_id, request, operation))
    }

    /// Take part in `call` of `workspace_id` until [`Self::voice_leave`]:
    /// the session keeps the participation alive across reconnects and
    /// hands on its grants ([`crate::voice`]).
    pub fn voice_join(&self, workspace_id: u64, call: [u8; 32]) -> Result<(), String> {
        self.send(Command::VoiceJoin(workspace_id, call))
    }

    /// Leave the call of `workspace_id`.
    pub fn voice_leave(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceLeave(workspace_id))
    }

    /// Show `showing` besides this session's voice in the call of
    /// `workspace_id` (its camera, a shared screen).
    pub fn voice_show(
        &self,
        workspace_id: u64,
        showing: tilo_relay_protocol::voice::Showing,
    ) -> Result<(), String> {
        self.send(Command::VoiceShow(workspace_id, showing))
    }
}

impl Task {
    pub(super) fn voice_calls(&self, workspace_id: u64, client: Option<&RelayClient>) -> bool {
        if let Some(client) = client {
            client.send(Envelope::VoiceCalls { workspace_id }).is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::Calls {
                workspace_id,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    pub(super) fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
        client: Option<&RelayClient>,
    ) -> bool {
        if let Some(client) = client {
            client
                .send(Envelope::VoiceCall {
                    workspace_id,
                    request,
                    operation,
                })
                .is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::CallResult {
                workspace_id,
                request,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    /// Offline, the next connection sends the join.
    pub(super) fn voice_join(
        &mut self,
        workspace_id: u64,
        call: [u8; 32],
        client: Option<&RelayClient>,
    ) -> bool {
        let out = self.voice.join(workspace_id, call);
        client.is_none_or(|client| {
            out.into_iter()
                .all(|envelope| client.send(envelope).is_ok())
        })
    }

    /// Offline, the next connection's heartbeat says it.
    pub(super) fn voice_show(
        &mut self,
        workspace_id: u64,
        showing: tilo_relay_protocol::voice::Showing,
        client: Option<&RelayClient>,
    ) -> bool {
        match (client, self.voice.show(workspace_id, showing)) {
            (Some(client), Some(show)) => client.send(show).is_ok(),
            _ => true,
        }
    }

    pub(super) fn voice_leave(&mut self, workspace_id: u64, client: Option<&RelayClient>) -> bool {
        let out = self.voice.leave(workspace_id);
        match (client, out) {
            (Some(client), Some(leave)) => client.send(leave).is_ok(),
            _ => true,
        }
    }
}

impl PeerHandle {
    /// Publish `pictures` this page encoded on the plane of `workspace_id`:
    /// one layer of its camera or screen each, read by every device that
    /// shows that layer.
    pub fn publish_call_pictures(
        &self,
        workspace_id: u64,
        pictures: Vec<super::CallPicture>,
    ) -> Result<(), String> {
        self.send(Command::CallPictures(workspace_id, pictures))
    }

    /// Show the devices' layers of `shown` on the plane of `workspace_id`,
    /// and stop reading those left out: their owners encode only these.
    pub fn show_call_pictures(
        &self,
        workspace_id: u64,
        shown: Vec<super::CallShown>,
    ) -> Result<(), String> {
        self.send(Command::CallShowing(workspace_id, shown))
    }

    /// Show the pages of `shown` 's windows on the plane of `workspace_id`, and
    /// stop reading the devices left out: an owner publishes a window's page only
    /// while a member asks for that window's track.
    pub fn show_pages(
        &self,
        workspace_id: u64,
        shown: Vec<super::PageShown>,
    ) -> Result<(), String> {
        self.send(Command::ShowPages(workspace_id, shown))
    }

    /// Ask `device` for `source` afresh in the plane of `workspace_id`: its
    /// next picture starts a group this page's decoder can begin at.
    pub fn restart_call_picture(
        &self,
        workspace_id: u64,
        device: [u8; 32],
        source: tilo_live::video::Source,
    ) -> Result<(), String> {
        self.send(Command::CallPictureRestart(workspace_id, device, source))
    }
}
