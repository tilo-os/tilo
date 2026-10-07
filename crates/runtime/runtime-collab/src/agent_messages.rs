//! Bounded explicit input on an authenticated managed Agent connection.
//! These descriptive fields grant neither Chat history nor write authority.

/// One owner-retained addressed message. Only the explicit submitted text is
/// delivered, not a Chat replica or conversation history. Prepared output is
/// immutable recovery data, not a request to generate another model answer.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageInput {
    pub invocation: String,
    pub room: String,
    pub agent: String,
    pub channel: String,
    pub request_row: String,
    pub text: String,
    pub source_digest: [u8; 32],
    pub launch: String,
    pub prepared_reply: Option<(String, String)>,
}

impl std::fmt::Debug for AgentMessageInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AgentMessageInput(..)")
    }
}

impl AgentMessageInput {
    /// Maximum escaped JSON delivery, including a retained bounded reply.
    pub const MAX_JSON_BYTES: usize = 640 * 1024;

    /// Validate descriptive identity and input bounds. Passing this check
    /// does not supply execution authority; the host rechecks its real process
    /// and original receipt at acceptance and reply publication.
    ///
    /// # Errors
    /// Invalid identity, correlation or oversized input/recovery output.
    pub fn validate(&self) -> Result<(), String> {
        if [&self.invocation, &self.room, &self.launch]
            .into_iter()
            .any(|id| !canonical_uuid(id))
            || self.agent.is_empty()
            || self.agent.len() > 128
            || self.channel.is_empty()
            || self.channel.len() > 128
            || self.request_row != format!("m_{}", self.invocation.replace('-', ""))
            || self.source_digest == [0; 32]
            || self.text.trim().is_empty()
            || self.text.len() > 32_000
            || self.text.chars().count() > 8_000
            || self.prepared_reply.as_ref().is_some_and(|(id, text)| {
                !canonical_uuid(id) || text.trim().is_empty() || text.len() > 64 * 1024
            })
        {
            return Err("invalid managed Agent Message input".into());
        }
        Ok(())
    }
}

fn canonical_uuid(value: &str) -> bool {
    super::valid_uuid(value)
        && value != "00000000-0000-0000-0000-000000000000"
        && !value.bytes().any(|byte| byte.is_ascii_uppercase())
}

/// Owner-observed state for one exact source and actual executing launch.
/// It grants no new effect and carries no Chat history or process authority.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AgentMessageObservation {
    Bound,
    Accepted,
    Prepared { reply: String, text: String },
    Committed { reply: String },
    NeedsAttention,
}

impl std::fmt::Debug for AgentMessageObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentMessageObservation(..)")
    }
}

impl AgentMessageObservation {
    /// # Errors
    /// Invalid receipt identity or oversized immutable prepared output.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Prepared { reply, text }
                if !canonical_uuid(reply) || text.trim().is_empty() || text.len() > 64 * 1024 =>
            {
                Err("invalid Message observation".into())
            }
            Self::Committed { reply } if !canonical_uuid(reply) => {
                Err("invalid Message observation".into())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(feature = "client")]
#[derive(Clone)]
pub struct AgentMessageStream {
    inner: std::rc::Rc<crate::collab_capnp::agent_message_stream::Client>,
}

#[cfg(feature = "client")]
impl AgentMessageStream {
    /// Read one explicit input from this managed Agent's private stream.
    ///
    /// # Errors
    /// Refuses malformed, oversized input or unavailable original custody.
    pub async fn read(&self) -> Result<AgentMessageInput, runtime_portal::NotAvailable> {
        let response = self.inner.read_request().send().promise.await?;
        let json =
            response.get()?.get_input_json()?.to_str().map_err(|_| {
                capnp::Error::failed("invalid managed Message input encoding".into())
            })?;
        if json.len() > AgentMessageInput::MAX_JSON_BYTES {
            return Err(capnp::Error::failed("managed Message input too large".into()).into());
        }
        let input: AgentMessageInput = serde_json::from_str(json)
            .map_err(|_| capnp::Error::failed("invalid managed Message input".into()))?;
        input.validate().map_err(capnp::Error::failed)?;
        Ok(input)
    }
}

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Read-only exact original recovery, never a retry or signing door.
    ///
    /// # Errors
    /// Unknown custody, unavailable owner, unsupported or malformed response.
    pub async fn observe_agent_message(
        &self,
        invocation: &str,
        digest: [u8; 32],
    ) -> Result<AgentMessageObservation, runtime_portal::NotAvailable> {
        validate_request(invocation, digest)?;
        let mut request = self.inner.observe_agent_message_request();
        request.get().set_invocation(invocation);
        request.get().set_source_digest(&digest);
        let response = request.send().promise.await?;
        let reader = response.get()?;
        let json = reader
            .get_observation_json()?
            .to_str()
            .map_err(|_| capnp::Error::failed("invalid Message observation encoding".into()))?;
        if json.len() > AgentMessageInput::MAX_JSON_BYTES {
            return Err(capnp::Error::failed("Message observation too large".into()).into());
        }
        let observation: AgentMessageObservation = serde_json::from_str(json)
            .map_err(|_| capnp::Error::failed("invalid Message observation".into()))?;
        observation.validate().map_err(capnp::Error::failed)?;
        Ok(observation)
    }
    /// Subscribe to only explicitly addressed input for this actual Agent.
    ///
    /// # Errors
    /// Missing managed/native custody or unsupported protocol never falls back.
    pub async fn subscribe_agent_messages(
        &self,
    ) -> Result<AgentMessageStream, runtime_portal::NotAvailable> {
        let response = self
            .inner
            .subscribe_agent_messages_request()
            .send()
            .promise
            .await?;
        Ok(AgentMessageStream {
            inner: std::rc::Rc::new(response.get()?.get_stream()?),
        })
    }

    /// Acknowledge exactly the original explicit input, not a Chat replica.
    ///
    /// # Errors
    /// Refuses changed correlation, source, process or unavailable service.
    pub async fn accept_agent_message(
        &self,
        invocation: &str,
        digest: [u8; 32],
    ) -> Result<(), runtime_portal::NotAvailable> {
        validate_request(invocation, digest)?;
        let mut request = self.inner.accept_agent_message_request();
        request.get().set_invocation(invocation);
        request.get().set_source_digest(&digest);
        request.send().promise.await?;
        Ok(())
    }

    /// Retain immutable output. Keep this acknowledgement separate from Commit
    /// so a lost Commit acknowledgement can observe the original terminal receipt.
    ///
    /// # Errors
    /// Invalid text/correlation, changed custody, or uncertain publication.
    pub async fn prepare_agent_message_reply(
        &self,
        invocation: &str,
        digest: [u8; 32],
        text: &str,
    ) -> Result<(), runtime_portal::NotAvailable> {
        validate_request(invocation, digest)?;
        if text.trim().is_empty() || text.len() > 64 * 1024 {
            return Err(capnp::Error::failed("invalid managed Message reply".into()).into());
        }
        let mut prepare = self.inner.prepare_agent_message_reply_request();
        prepare.get().set_invocation(invocation);
        prepare.get().set_source_digest(&digest);
        prepare.get().set_text(text);
        prepare.send().promise.await?;
        Ok(())
    }

    /// Commit the originally prepared reply, or observe its historical receipt.
    /// This never replaces output or appends a second reply on exact replay.
    ///
    /// # Errors
    /// Refuses changed source or execution custody and unprepared output.
    pub async fn commit_agent_message_reply(
        &self,
        invocation: &str,
        digest: [u8; 32],
    ) -> Result<(), runtime_portal::NotAvailable> {
        validate_request(invocation, digest)?;
        let mut commit = self.inner.commit_agent_message_reply_request();
        commit.get().set_invocation(invocation);
        commit.get().set_source_digest(&digest);
        commit.send().promise.await?;
        Ok(())
    }
}

#[cfg(feature = "client")]
fn validate_request(
    invocation: &str,
    digest: [u8; 32],
) -> Result<(), runtime_portal::NotAvailable> {
    if !canonical_uuid(invocation) || digest == [0; 32] {
        return Err(capnp::Error::failed("invalid managed Message correlation".into()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AgentMessageInput {
        AgentMessageInput {
            invocation: "12345678-1234-1234-1234-123456789abc".into(),
            room: "22345678-1234-1234-1234-123456789abc".into(),
            agent: "scribe".into(),
            channel: "agent-conversation".into(),
            request_row: "m_12345678123412341234123456789abc".into(),
            text: "Explicit input".into(),
            source_digest: [7; 32],
            launch: "32345678-1234-1234-1234-123456789abc".into(),
            prepared_reply: None,
        }
    }

    #[test]
    fn explicit_input_and_prepared_output_round_trip_without_debug_disclosure() {
        let mut original = input();
        original.prepared_reply = Some((
            "42345678-1234-1234-1234-123456789abc".into(),
            "Retained output".into(),
        ));
        original.validate().unwrap();
        let encoded = serde_json::to_vec(&original).unwrap();
        assert!(encoded.len() < AgentMessageInput::MAX_JSON_BYTES);
        let decoded: AgentMessageInput = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(format!("{decoded:?}"), "AgentMessageInput(..)");
    }

    #[test]
    fn observations_are_bounded_redacted_and_strictly_typed() {
        let prepared = AgentMessageObservation::Prepared {
            reply: input().invocation,
            text: "Private immutable output".into(),
        };
        prepared.validate().unwrap();
        assert_eq!(
            serde_json::from_str::<AgentMessageObservation>(
                &serde_json::to_string(&prepared).unwrap()
            )
            .unwrap(),
            prepared
        );
        assert_eq!(format!("{prepared:?}"), "AgentMessageObservation(..)");
        for invalid in [
            AgentMessageObservation::Prepared {
                reply: input().invocation,
                text: "x".repeat(64 * 1024 + 1),
            },
            AgentMessageObservation::Committed {
                reply: "00000000-0000-0000-0000-000000000000".into(),
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        assert!(
            serde_json::from_str::<AgentMessageObservation>(
                r#"{"Accepted":{"history":["not allowed"]}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_correlation_and_oversized_input_are_refused() {
        let original = input();
        for kind in 0..7 {
            let mut invalid = original.clone();
            match kind {
                0 => invalid.invocation = "00000000-0000-0000-0000-000000000000".into(),
                1 => invalid.request_row = "another-parent".into(),
                2 => invalid.source_digest = [0; 32],
                3 => invalid.text = "x".repeat(8001),
                4 => invalid.text = "\u{1f600}".repeat(8001),
                5 => invalid.prepared_reply = Some((original.launch.clone(), " ".into())),
                _ => {
                    invalid.prepared_reply =
                        Some((original.launch.clone(), "x".repeat(64 * 1024 + 1)));
                }
            }
            assert!(invalid.validate().is_err(), "invalid input case {kind}");
        }
        let mut extra = serde_json::to_value(&original).unwrap();
        extra["history"] = serde_json::json!(["Not an allowed input field"]);
        assert!(serde_json::from_value::<AgentMessageInput>(extra).is_err());
    }
}
