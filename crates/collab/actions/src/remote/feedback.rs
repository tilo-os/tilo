//! Read-only feedback admission and canonical acknowledgment on the existing
//! authenticated action transport. These values never grant membership.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDestination {
    pub room: String,
    pub provider: String,
    pub channel: String,
}

/// No submission asks only to inspect existing destination admission. A
/// submission asks for its durable receipt; neither operation publishes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackQuery {
    pub destination: FeedbackDestination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<serde_json::Value>,
    /// A report's review prompt, for the owner's review model to look over
    /// ([`FeedbackAnswer::reviewer`]): exactly the feedback review contract
    /// over the report's model view, which shows personal values only as
    /// placeholders. The owner refuses any other prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
}

/// Current policy returned by the authenticated owner for this exact caller.
/// A consumer must match both identities to its retained certificates. Reading
/// this JSON from a config file does not authenticate it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeedbackAdmission {
    pub destination: FeedbackDestination,
    pub owner_user: String,
    pub owner_device: String,
    pub reporter_user: String,
    pub reporter_device: String,
    pub standing: serde_json::Value,
    /// The room's current sandbox capabilities.
    pub sandbox: serde_json::Value,
    pub unix_time: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAnswer {
    pub admission: FeedbackAdmission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<serde_json::Value>,
    /// The model the owner looks over its reporters' reviews with, as a
    /// review names it ("Anthropic claude-haiku-5-5"); `None` when it has
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// The review model's answer to [`FeedbackQuery::review`], holding only
    /// quotes from the texts asked about; `None` when nothing was asked or
    /// the model gave no usable answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{ActionHostError, RemoteActionMessage, refuse};
    #[test]
    fn feedback_admission_is_an_owner_request_and_generic_hosts_refuse_it() {
        let message = RemoteActionMessage::FeedbackRequest {
            correlation: 7,
            query: FeedbackQuery {
                destination: FeedbackDestination {
                    room: "existing-room".into(),
                    provider: "record:so.tilo.chat:".into(),
                    channel: "existing-channel".into(),
                },
                submission: None,
                review: None,
            },
        };
        assert!(message.is_request());
        let response = refuse(&message, ActionHostError::NotPermitted).unwrap();
        assert!(!response.is_request());
        assert!(matches!(
            response,
            RemoteActionMessage::FeedbackResponse {
                correlation: 7,
                result: Err(ActionHostError::NotPermitted)
            }
        ));
        let untrusted = serde_json::json!({"destination":{"room":"existing-room","provider":"record:so.tilo.chat:","channel":"existing-channel"},"standing":{"person":"owner"}});
        assert!(serde_json::from_value::<FeedbackQuery>(untrusted).is_err());
    }
}
