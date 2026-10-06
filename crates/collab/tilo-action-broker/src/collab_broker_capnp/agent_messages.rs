//! Explicit managed input never grants a Chat replica or generic write access.
use super::{CapnpChannelHandle, Promise, Rc, channel_handle};
use crate::services::NativeAgentMessageSession;
use std::sync::Arc;

impl CapnpChannelHandle {
    fn current_message_process(
        &self,
    ) -> Result<launcher::managed::VerifiedManagedProcess, capnp::Error> {
        let route = self.route();
        let identity = self
            .state
            .revalidate_admission(&route, &self.verified_identity)?;
        let agent = identity.agent_id.ok_or_else(|| {
            capnp::Error::failed("only a managed Agent receives addressed input".into())
        })?;
        self.state
            .managed_agent_facts(self.peer_id, &route.cluster_id, &agent)
            .map_err(capnp::Error::failed)?;
        self.state
            .managed_workers
            .lock()
            .map_err(|_| capnp::Error::failed("managed worker registry unavailable".into()))?
            .get(&self.peer_id)
            .cloned()
            .ok_or_else(|| capnp::Error::failed("managed process custody unavailable".into()))
    }

    pub(super) fn agent_message_session(
        &self,
    ) -> Result<Arc<dyn NativeAgentMessageSession>, capnp::Error> {
        let process = self.current_message_process()?;
        self.state
            .services
            .workspace_messages
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("native Message service unavailable".into()))?
            .open_agent_execution(process)
            .map_err(capnp::Error::failed)
    }
}

pub(super) fn subscribe(
    channel: Rc<CapnpChannelHandle>,
    mut results: channel_handle::SubscribeAgentMessagesResults,
) -> Promise<(), capnp::Error> {
    let session = match channel.agent_message_session() {
        Ok(session) => session,
        Err(error) => return Promise::err(error),
    };
    let client: runtime_collab::collab_capnp::agent_message_stream::Client =
        capnp_rpc::new_client(Stream {
            channel,
            session,
            seen: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        });
    results.get().set_stream(client);
    Promise::ok(())
}

struct Stream {
    channel: Rc<CapnpChannelHandle>,
    session: Arc<dyn NativeAgentMessageSession>,
    seen: tokio::sync::Mutex<std::collections::HashSet<uuid::Uuid>>,
}

impl runtime_collab::collab_capnp::agent_message_stream::Server for Stream {
    fn read(
        self: Rc<Self>,
        _params: runtime_collab::collab_capnp::agent_message_stream::ReadParams,
        mut results: runtime_collab::collab_capnp::agent_message_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            // Serialize pipelined readers and bound remembered IDs to hot custody.
            let mut seen = self.seen.lock().await;
            loop {
                self.channel.current_message_process()?;
                let pending = self.session.pending().map_err(capnp::Error::failed)?;
                if pending.len() > 128 {
                    return Err(capnp::Error::failed(
                        "managed Message inbox exceeded its bound".into(),
                    ));
                }
                seen.retain(|id| pending.contains(id));
                for id in pending {
                    if seen.contains(&id) {
                        continue;
                    }
                    // Old-launch refusals must not block newer valid input or be retargeted.
                    let Ok(input) = self.session.deliver(id) else {
                        continue;
                    };
                    input.validate().map_err(capnp::Error::failed)?;
                    let json = serde_json::to_string(&input).map_err(|_| {
                        capnp::Error::failed("managed Message encoding failed".into())
                    })?;
                    if json.len() > runtime_collab::AgentMessageInput::MAX_JSON_BYTES {
                        return Err(capnp::Error::failed(
                            "managed Message delivery too large".into(),
                        ));
                    }
                    self.channel.current_message_process()?;
                    seen.insert(id);
                    drop(seen);
                    results.get().set_input_json(&json);
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        })
    }
}

pub(super) fn correlation(
    invocation: capnp::text::Reader<'_>,
    digest: &[u8],
) -> Result<(uuid::Uuid, [u8; 32]), capnp::Error> {
    let raw = invocation
        .to_str()
        .map_err(|_| capnp::Error::failed("invalid Message invocation".into()))?;
    let id = uuid::Uuid::parse_str(raw)
        .map_err(|_| capnp::Error::failed("invalid Message invocation".into()))?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| capnp::Error::failed("invalid Message source digest".into()))?;
    if id.is_nil() || id.to_string() != raw || digest == [0; 32] {
        return Err(capnp::Error::failed("invalid Message correlation".into()));
    }
    Ok((id, digest))
}

#[cfg(test)]
mod tests {
    use super::correlation;

    #[test]
    fn correlation_rejects_noncanonical_or_missing_original_identity() {
        let id = "12345678-1234-1234-1234-123456789abc";
        let (parsed, digest) = correlation(id.into(), &[7; 32]).unwrap();
        assert_eq!(parsed.to_string(), id);
        assert_eq!(digest, [7; 32]);
        for invalid in [
            "",
            "00000000-0000-0000-0000-000000000000",
            "12345678-1234-1234-1234-123456789ABC",
            "12345678123412341234123456789abc",
        ] {
            assert!(correlation(invalid.into(), &[7; 32]).is_err());
        }
        assert!(correlation(id.into(), &[0; 32]).is_err());
        assert!(correlation(id.into(), &[7; 31]).is_err());
        assert!(correlation(id.into(), &[7; 33]).is_err());
    }
}
