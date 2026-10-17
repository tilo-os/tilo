//! Closed, portable launch scope. Descriptive labels never select credentials,
//! source paths, installed certificates or canonical signed packages.
use std::io;

pub const MAX_REQUEST: usize = 4096;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    schema: u32,
    operation_id: String,
    app_id: String,
    package_sha256: String,
    room: String,
    resource_record: String,
    context_sha256: String,
}

/// Bounded, validated declaration only, never a person/source admission.
/// The only constructor decodes the closed wire schema on every target.
#[derive(Clone, Debug)]
pub struct LaunchRequest(WireRequest);
impl LaunchRequest {
    /// Decode a bounded request using the closed launch schema.
    ///
    /// # Errors
    /// Returns an error for empty or oversized input, invalid JSON, unknown or
    /// duplicate fields, unsupported schema, or invalid identifiers and labels.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let refused = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Installed retained launch refused",
            )
        };
        if bytes.is_empty() || bytes.len() > MAX_REQUEST {
            return Err(refused());
        }
        let value: WireRequest = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if value.schema != 1
            || !uuid(&value.operation_id)
            || !uuid(&value.room)
            || !label(&value.app_id)
            || !label(&value.resource_record)
            || !digest(&value.package_sha256)
            || !digest(&value.context_sha256)
        {
            return Err(refused());
        }
        Ok(Self(value))
    }
    pub fn operation_id(&self) -> &str {
        &self.0.operation_id
    }
    pub fn app_id(&self) -> &str {
        &self.0.app_id
    }
    pub fn package_sha256(&self) -> &str {
        &self.0.package_sha256
    }
    pub fn room(&self) -> &str {
        &self.0.room
    }
    pub fn resource_record(&self) -> &str {
        &self.0.resource_record
    }
    pub fn context_sha256(&self) -> &str {
        &self.0.context_sha256
    }
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> serde_json::Value {
        serde_json::json!({"schema":1,"operation_id":"11111111-1111-4111-8111-111111111111",
            "app_id":"so.tilo.files","package_sha256":"a".repeat(64),
            "room":"22222222-2222-4222-8222-222222222222","resource_record":"home",
            "context_sha256":"b".repeat(64)})
    }
    #[test]
    fn scope_is_portable_closed_and_bounded() {
        let bytes = serde_json::to_vec(&request()).unwrap();
        let scope = LaunchRequest::decode(&bytes).unwrap();
        assert_eq!(scope.operation_id(), "11111111-1111-4111-8111-111111111111");
        assert_eq!(scope.app_id(), "so.tilo.files");
        assert_eq!(scope.resource_record(), "home");
        for field in [
            "source",
            "argv",
            "target_uid",
            "target_pid",
            "groups",
            "certificate",
        ] {
            let mut value = request();
            value[field] = serde_json::json!("untrusted");
            assert!(LaunchRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        assert!(LaunchRequest::decode(&[]).is_err());
        assert!(LaunchRequest::decode(&vec![b' '; MAX_REQUEST + 1]).is_err());
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            LaunchRequest::decode(
                format!("{},\"schema\":1}}", json.trim_end_matches('}')).as_bytes()
            )
            .is_err()
        );
    }
}
