//! Native addressed Message publisher custody. The host signs only requests
//! created from actual admitted private-worker directory/stream registrations.
#[path = "workspace_messages/publication.rs"]
mod publication;
#[path = "workspace_messages/source.rs"]
mod source;
#[path = "workspace_messages/upload.rs"]
mod upload;
pub use publication::{NativeMessageHostAttestations, NativeMessagePublication};
pub(super) use source::MessageSources;

/// Bound retained native Message originals independently of retired host signers.
pub(crate) const MAX_MESSAGE_SOURCES: usize = 32;

pub(super) struct OriginalMessageFacts {
    pub(super) agent: String,
    pub(super) request: ui::invocation_scope::PublishedInvocationRequest,
    pub(super) caller: tilo_user_identity::DeviceCert,
    pub(super) operation: tilo_joined_room::AgentMessageOperationData,
    pub(super) spec: ui::invocation_scope::PublishedInvocationSpec,
}

/// Broker-created installation facts, not a caller-supplied authority stamp.
/// There is deliberately no public constructor or deserializer.
pub struct AgentMessagePublisherInstallation {
    pub(super) data: tilo_joined_room::AgentMessagePublisherData,
}

impl AgentMessagePublisherInstallation {
    /// Inert facts the installed native host attests after checking its owner.
    pub fn data(&self) -> &tilo_joined_room::AgentMessagePublisherData {
        &self.data
    }
}

/// Shared lifetime of the actual stable stream, even when its directory is
/// published later. Closing that stream cannot race a late lease installation.
pub(super) struct MessagePublisherStream {
    pub(super) identity: uuid::Uuid,
    custody: std::sync::Mutex<StreamCustody>,
}

/// Before an actual RPC server owns the stream, every refusal retires its
/// installation. Dropping a receiver is not enough to retire a late directory.
pub(super) struct ProvisionalMessagePublisherStream(Option<std::sync::Arc<MessagePublisherStream>>);

impl ProvisionalMessagePublisherStream {
    pub(super) fn new(stream: Option<std::sync::Arc<MessagePublisherStream>>) -> Self {
        Self(stream)
    }

    pub(super) fn transfer(mut self) -> Option<std::sync::Arc<MessagePublisherStream>> {
        self.0.take()
    }
}

impl Drop for ProvisionalMessagePublisherStream {
    fn drop(&mut self) {
        if let Some(stream) = self.0.as_ref() {
            stream.retire();
        }
    }
}

#[derive(Default)]
struct StreamCustody {
    retired: bool,
    lease: Option<tilo_joined_room::AgentMessagePublisherLease>,
}

impl MessagePublisherStream {
    pub(super) fn lease(&self) -> Result<tilo_joined_room::AgentMessagePublisherLease, String> {
        let custody = self
            .custody
            .lock()
            .map_err(|_| "Message stream unavailable")?;
        if custody.retired {
            return Err("Message publisher stream retired".into());
        }
        custody
            .lease
            .clone()
            .ok_or_else(|| "Message publisher owner custody unavailable".into())
    }

    pub(super) fn new() -> Self {
        Self {
            identity: uuid::Uuid::new_v4(),
            custody: std::sync::Mutex::new(StreamCustody::default()),
        }
    }

    pub(super) fn install(
        &self,
        lease: tilo_joined_room::AgentMessagePublisherLease,
    ) -> Result<(), String> {
        let mut custody = self
            .custody
            .lock()
            .map_err(|_| "Message stream unavailable")?;
        if custody.retired {
            lease.retire();
            return Err("Message publisher stream already retired".into());
        }
        if let Some(previous) = custody.lease.replace(lease) {
            previous.retire();
        }
        drop(custody);
        Ok(())
    }

    pub(super) fn retire(&self) {
        let mut custody = match self.custody.lock() {
            Ok(custody) => custody,
            Err(error) => error.into_inner(),
        };
        custody.retired = true;
        if let Some(lease) = custody.lease.take() {
            lease.retire();
        }
    }
}
