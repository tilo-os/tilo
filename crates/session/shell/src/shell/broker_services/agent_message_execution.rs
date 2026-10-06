//! Private native Message execution. Guests receive explicit text and inert
//! correlation only; host signing keys and complete Chat documents stay here.
use super::NativeOwnerCommitService;
use std::sync::Arc;
use tilo_action_broker::services::NativeAgentMessageSession;
use tilo_desktop_model::RoomId;
use tilo_joined_room::AgentMessageHostService;

struct Session {
    service: AgentMessageHostService,
    room: RoomId,
}

pub(super) fn install(
    owner: &NativeOwnerCommitService,
    process: launcher::managed::VerifiedManagedProcess,
) -> Result<Arc<dyn NativeAgentMessageSession>, String> {
    let facts = process.facts()?;
    let room = RoomId::parse(&facts.binding.room).ok_or("invalid managed Message room")?;
    let native = owner
        .owner
        .as_ref()
        .ok_or("native Message owner unavailable")?;
    if let Some(network) = &owner.network {
        network.check_native_custody(native, room)?;
        if network.governed_elsewhere(room) {
            return Err("Message execution requires the governing native owner".into());
        }
    }
    let (_, attestor) = tilo_identity_host::broker::trusted_current_device(&room.to_string())
        .map_err(|_| "native Message host custody unavailable")?;
    let service = AgentMessageHostService::from_connected_process(
        native.clone(),
        attestor,
        process,
        std::time::SystemTime::now(),
    )?;
    Ok(Arc::new(Session { service, room }))
}

impl Session {
    fn check_source(&self, invocation: uuid::Uuid, digest: [u8; 32]) -> Result<(), String> {
        let input = self
            .service
            .deliver(invocation, std::time::SystemTime::now())?;
        if input.source_digest != digest {
            return Err("Message request differs from its original delivery".into());
        }
        Ok(())
    }
}

impl NativeAgentMessageSession for Session {
    fn pending(&self) -> Result<Vec<uuid::Uuid>, String> {
        self.service
            .pending_invocations(std::time::SystemTime::now())
    }

    fn deliver(&self, invocation: uuid::Uuid) -> Result<runtime_collab::AgentMessageInput, String> {
        let input = self
            .service
            .deliver(invocation, std::time::SystemTime::now())?;
        let delivery = runtime_collab::AgentMessageInput {
            invocation: input.invocation.to_string(),
            room: input.room.to_string(),
            agent: input.agent,
            channel: input.channel,
            request_row: input.request_row,
            text: input.text,
            source_digest: input.source_digest,
            launch: input.launch,
            prepared_reply: input
                .prepared_reply
                .map(|(id, text)| (id.to_string(), text)),
        };
        delivery.validate()?;
        Ok(delivery)
    }

    fn accept(&self, invocation: uuid::Uuid, digest: [u8; 32]) -> Result<(), String> {
        self.check_source(invocation, digest)?;
        self.service
            .accept(invocation, std::time::SystemTime::now())
    }

    fn prepare_reply(
        &self,
        invocation: uuid::Uuid,
        digest: [u8; 32],
        text: &str,
    ) -> Result<(), String> {
        self.check_source(invocation, digest)?;
        self.service
            .prepare_reply(invocation, text, std::time::SystemTime::now())
            .map(|_| ())
    }

    fn commit_reply(&self, invocation: uuid::Uuid, digest: [u8; 32]) -> Result<(), String> {
        let (key, _) = tilo_identity_host::broker::trusted_current_device(&self.room.to_string())
            .map_err(|_| "native Message reply signing custody unavailable")?;
        self.service
            .commit_reply(invocation, digest, &key, std::time::SystemTime::now())
            .map(|_| ())
    }
}
