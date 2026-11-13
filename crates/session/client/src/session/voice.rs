//! The page's call participation: joining, leaving and what it shows, told to
//! the host that governs the call. The voice and the pictures themselves are
//! tracks of the page's own plane ([`super::plane`]).

use super::{Envelope, RelayClient, Task};

impl Task {
    pub(super) fn voice_calls(&self, workspace_id: u64, client: Option<&RelayClient>) -> bool {
        if let Some(client) = client {
            client.send(Envelope::VoiceCalls { workspace_id }).is_ok()
        } else {
            self.reports
                .emit(&crate::Event::Voice(crate::VoiceEvent::Calls {
                    ws: workspace_id,
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
            self.reports
                .emit(&crate::Event::Voice(crate::VoiceEvent::CallResult {
                    ws: workspace_id,
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
