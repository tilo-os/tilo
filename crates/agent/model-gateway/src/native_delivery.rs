//! Non-serializable evidence minted only by the protected HTTP source.
use agent_provider_config::ProviderProfile;

/// A completed, decoded successful response to one dispatched HTTP request.
/// No wire decoder or public constructor can manufacture this evidence.
pub struct NativeModelSuccess {
    pub(crate) invocation: serde_json::Value,
    pub(crate) profile: ProviderProfile,
    pub(crate) model: String,
    pub(crate) endpoint: String,
    pub(crate) body: Vec<u8>,
    pub(crate) texts: Vec<String>,
}
impl NativeModelSuccess {
    pub fn invocation(&self) -> &serde_json::Value {
        &self.invocation
    }
    pub fn profile(&self) -> &ProviderProfile {
        &self.profile
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Original user frames which survived in the actual outbound HTTP body.
    /// Provider codecs may coalesce frames, but cannot omit one and still prove
    /// its delivery. Unknown encodings conservatively provide no envelope proof.
    pub fn user_texts(&self) -> Vec<&str> {
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&self.body) else {
            return Vec::new();
        };
        let mut sent = Vec::new();
        for field in ["messages", "input", "contents"] {
            for message in body
                .get(field)
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                if message.get("role").and_then(serde_json::Value::as_str) == Some("user") {
                    for field in ["content", "parts"] {
                        if let Some(content) = message.get(field) {
                            text_blocks(content, &mut sent);
                        }
                    }
                }
            }
        }
        self.texts
            .iter()
            .filter(|text| sent.iter().any(|sent| sent.contains(text.as_str())))
            .map(String::as_str)
            .collect()
    }
}

fn text_blocks(value: &serde_json::Value, texts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => texts.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                text_blocks(value, texts);
            }
        }
        serde_json::Value::Object(value) => {
            if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                texts.push(text.into());
            }
        }
        _ => {}
    }
}
