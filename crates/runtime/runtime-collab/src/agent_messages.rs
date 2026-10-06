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
