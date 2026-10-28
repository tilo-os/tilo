//! Private RPCs use native custody and the governor's signed service pin, never gossip.
use super::{RoomId, ShellNetwork};
use std::sync::Arc;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_relay_protocol::rooms::private::{PrivateOperation, PrivateReply};
use tilo_relay_session::rooms::private::PrivateClient;
use tilo_user_identity::DeviceCert;

// Deliberately not Debug: addresses and principals belong to the private route.
pub(crate) struct PrivateChatRoute {
    network: Arc<ShellNetwork>,
    room: RoomId,
    caller: DeviceCert,
    governor: DeviceCert,
    relay: iroh::EndpointAddr,
}

impl ShellNetwork {
    /// None means this actual device is the governor, not merely a replica.
    pub(crate) fn private_chat_route(
        self: &Arc<Self>,
        room: RoomId,
        caller: &DeviceCert,
    ) -> Result<Option<PrivateChatRoute>, String> {
        let owner = self.native_owner()?;
        self.check_native_custody(&owner, room)?;
        owner.with_current(room, |view| {
            let identity = self
                .host_identity
                .as_ref()
                .ok_or("native identity unavailable")?;
            let (certificate, _) = identity.current_native_material(view.workspace())?;
            if caller != &certificate || view.device() != &certificate {
                return Err("private caller differs from actual native custody".into());
            }
            if view.governor() == &certificate {
                return Ok(None);
            }
            let (hint, _) =
                IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint()?)
                    .map_err(|_| "private governor transport hint invalid")?;
            if hint.room != room.to_string()
                || &hint.governing_authority != view.governor()
                || &hint.host_device_certificate != view.governor()
            {
                return Err("private transport differs from current governor".into());
            }
            if self
                .private_chat_endpoint
                .as_ref()
                .is_none_or(|endpoint| endpoint.id().as_bytes() != &certificate.device_public_key)
            {
                return Err("private endpoint differs from actual native device".into());
            }
            Ok(Some(PrivateChatRoute {
                network: self.clone(),
                room,
                caller: certificate,
                governor: view.governor().clone(),
                relay: hint.endpoint_addr,
            }))
        })
    }
}

impl PrivateChatRoute {
    pub(crate) fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        self.network
            .runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| "private network runtime unavailable".into())
    }

    fn check_current(&self) -> Result<(), String> {
        let fresh = self
            .network
            .private_chat_route(self.room, &self.caller)?
            .ok_or("private governor changed")?;
        if fresh.governor != self.governor || fresh.relay != self.relay {
            return Err("private governor transport changed".into());
        }
        drop(fresh);
        Ok(())
    }

    pub(crate) async fn send(&self, operation: PrivateOperation) -> Result<PrivateReply, String> {
        self.check_current()?;
        let endpoint = self
            .network
            .private_chat_endpoint
            .as_ref()
            .ok_or("private native endpoint unavailable")?;
        let client = PrivateClient::new(endpoint.clone(), self.caller.clone());
        let reply = client
            .send(self.relay.clone(), self.room.to_string(), operation)
            .await?;
        self.check_current()?;
        match reply {
            PrivateReply::Refused => Err("private governor refused request".into()),
            reply => Ok(reply),
        }
    }
}
