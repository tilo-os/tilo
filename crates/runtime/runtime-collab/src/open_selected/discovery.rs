//! Presentation on the existing authenticated Files lane. No discovery value grants access.
use super::{app, digest, id, v2::SelectedAccess};
use serde::{Deserialize, Serialize};
use std::io;

pub const PROTOCOL: &str = "tilo-open-selected-discovery-v1";
pub const MAX_TARGETS: usize = 32;
pub const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationScope {
    pub resource: String,
    pub app_id: String,
    pub access: SelectedAccess,
}
impl InvocationScope {
    pub fn valid(&self) -> bool {
        app(&self.app_id)
            && !self.resource.is_empty()
            && self.resource.len() <= 1024
            && !self.resource.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub invocation: Option<InvocationScope>,
    pub protocol: String,
    pub schema: u32,
    pub request_id: String,
    pub components: Vec<Vec<u8>>,
}
impl Request {
    pub fn valid(&self) -> bool {
        self.invocation.as_ref().is_none_or(InvocationScope::valid)
            && self.protocol == PROTOCOL
            && self.schema == 1
            && id(&self.request_id)
            && !self.components.is_empty()
            && self.components.len() <= 64
            && self.components.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 255
                    && part.as_slice() != b"."
                    && part.as_slice() != b".."
                    && !part.contains(&0)
                    && !part.contains(&b'/')
            })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub app_id: String,
    pub package_sha256: String,
    pub name: String,
    pub is_default: bool,
    pub access: SelectedAccess,
}
impl Target {
    fn valid(&self) -> bool {
        app(&self.app_id)
            && digest(&self.package_sha256)
            && !self.name.is_empty()
            && self.name.len() <= 256
            && !self.name.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub type_label: String,
    pub schema: u32,
    pub request_id: String,
    pub components: Vec<Vec<u8>>,
    pub source_operation: Option<String>,
    pub targets: Vec<Target>,
}
impl Reply {
    pub fn valid(&self) -> bool {
        let request = Request {
            invocation: None,
            protocol: PROTOCOL.into(),
            schema: self.schema,
            request_id: self.request_id.clone(),
            components: self.components.clone(),
        };
        request.valid()
            && self.type_label.len() <= 256
            && !self.type_label.chars().any(char::is_control)
            && (self.targets.is_empty() || !self.type_label.is_empty())
            && self.targets.len() <= MAX_TARGETS
            && self.source_operation.as_ref().is_none_or(|value| id(value))
            && (self.targets.is_empty() || self.source_operation.is_some())
            && self.targets.iter().all(Target::valid)
            && !self.targets.iter().enumerate().any(|(index, target)| {
                self.targets
                    .iter()
                    .take(index)
                    .any(|other| other.app_id == target.app_id)
            })
    }
    pub fn matches(&self, request: &Request) -> bool {
        self.valid()
            && request.valid()
            && self.request_id == request.request_id
            && self.components == request.components
    }
    pub fn unavailable(request: &Request) -> Self {
        Self {
            type_label: String::new(),
            schema: 1,
            request_id: request.request_id.clone(),
            components: request.components.clone(),
            source_operation: None,
            targets: Vec::new(),
        }
    }
}
/// Fixed query on the same live Shell/root bootstrap. An acknowledgement is
/// backend presentation evidence; selection, person, package and policy are
/// independently revalidated when an original action actually opens a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySelectedSupport {
    pub schema: u32,
    pub request_id: String,
    pub app_id: String,
    pub package_sha256: String,
    pub access: SelectedAccess,
}
impl QuerySelectedSupport {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && id(&self.request_id)
            && app(&self.app_id)
            && digest(&self.package_sha256)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum RootCommand {
    QuerySelectedSupport(QuerySelectedSupport),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supported {
    pub schema: u32,
    pub request_id: String,
    pub source_operation: String,
    pub app_id: String,
    pub package_sha256: String,
    pub access: SelectedAccess,
}
impl Supported {
    pub fn matches(&self, request: &QuerySelectedSupport, operation: &str) -> bool {
        request.valid()
            && self.schema == 1
            && id(&self.source_operation)
            && self.source_operation == operation
            && self.request_id == request.request_id
            && self.app_id == request.app_id
            && self.package_sha256 == request.package_sha256
            && self.access == request.access
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RootReply {
    Supported(Supported),
    Unavailable,
}
pub fn is_envelope(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_BYTES
        && serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| {
            value.get("protocol").and_then(serde_json::Value::as_str) == Some(PROTOCOL)
        })
}
/// # Errors
/// Refuses empty, oversized, malformed or invalid discovery requests.
pub fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    decode(bytes, Request::valid)
}
/// # Errors
/// Refuses empty, oversized, malformed or invalid discovery replies.
pub fn decode_reply(bytes: &[u8]) -> io::Result<Reply> {
    decode(bytes, Reply::valid)
}
fn decode<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    valid: impl FnOnce(&T) -> bool,
) -> io::Result<T> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(io::Error::other("invalid selected discovery frame size"));
    }
    let value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if !valid(&value) {
        return Err(io::Error::other("invalid selected discovery frame"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        Request {
            invocation: None,
            protocol: PROTOCOL.into(),
            schema: 1,
            request_id: "0194b042-621e-7417-92ac-72591fcd900d".into(),
            components: vec![b"notes.txt".to_vec()],
        }
    }
    #[test]
    fn unavailable_is_not_support_and_discovery_rejects_other_paths_and_grants() {
        let request = request();
        assert!(Reply::unavailable(&request).matches(&request));
        let mut wrong = request.clone();
        wrong.components = vec![b"..".to_vec()];
        assert!(!wrong.valid());
        wrong = request.clone();
        wrong.components = vec![b"other.txt".to_vec()];
        assert!(!Reply::unavailable(&request).matches(&wrong));
        let mut json = serde_json::to_value(&request).expect("request encoding");
        json["descriptor"] = 55.into();
        assert!(decode_request(&serde_json::to_vec(&json).expect("JSON encoding")).is_err());
        assert!(decode_reply(br#"{"schema":1,"request_id":"0194b042-621e-7417-92ac-72591fcd900d","source_operation":"0194b042-621e-7417-92ac-72591fcd900d"}"#).is_err());
    }
}
