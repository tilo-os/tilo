//! Which of an app's own live invocations its next writes belong to.
//!
//! An agent has no filesystem access of its own: it invokes an app's action,
//! and the app writes through the workspace mount, so the version is signed
//! for the device's person. Before writing for an invocation, the app names
//! the paths it is about to write and the invocation they are for, over the
//! same lane the local clipboard uses. The host resolves the invocation
//! against its own custody of that app's live invocations and takes the actor
//! from there; nothing in this envelope names who acted. When that actor is an
//! agent acting for the session's person, the filesystem records it beside
//! those versions. It grants nothing, and a refusal never stops the write.

use serde::{Deserialize, Serialize};

/// Purpose-separated envelope identifier.
pub const PROTOCOL: &str = "tilo-write-attribution-v1";
/// Most paths one request names.
pub const MAX_PATHS: usize = 64;
/// Longest path a request names, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// Largest encoded request.
pub const MAX_WIRE_BYTES: usize = 64 * 1024;

/// The absolute paths an app is about to write for the invocation it sends
/// this under. A folder covers what is under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: String,
    pub paths: Vec<String>,
}

impl Request {
    /// A request naming `paths`.
    #[must_use]
    pub fn new(paths: Vec<String>) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            paths,
        }
    }

    /// Validate the bounded envelope before giving it to the host.
    ///
    /// # Errors
    /// Refuses another protocol, no or too many paths, and any path that is
    /// not a clean absolute path below the root.
    pub fn validate(&self) -> Result<(), String> {
        let clean = |path: &String| {
            path.len() > 1
                && path.len() <= MAX_PATH_BYTES
                && path.starts_with('/')
                && !path.contains('\0')
                && path
                    .split('/')
                    .skip(1)
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        };
        if self.protocol != PROTOCOL
            || self.paths.is_empty()
            || self.paths.len() > MAX_PATHS
            || !self.paths.iter().all(clean)
        {
            return Err("invalid write attribution envelope".into());
        }
        Ok(())
    }
}

/// Whether `bytes` is a write attribution envelope rather than another
/// command of the same lane.
#[must_use]
pub fn is_envelope(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_WIRE_BYTES {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| {
        value.get("protocol").and_then(serde_json::Value::as_str) == Some(PROTOCOL)
    })
}

/// Decode and validate an envelope.
///
/// # Errors
/// Refuses oversized, malformed or invalid envelopes.
pub fn decode(bytes: &[u8]) -> Result<Request, String> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err("write attribution exceeds its bound".into());
    }
    let request: Request = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    request.validate()?;
    Ok(request)
}

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Name the paths this app is about to write for its own live invocation
    /// `parent`. The host resolves `parent` against its custody of this
    /// app's invocations, never against anything sent here, and records the
    /// agent behind those writes when an agent acting for the session's
    /// person asked for them.
    ///
    /// # Errors
    /// Refuses invalid or oversized envelopes, and the host's refusal: no
    /// such live invocation, or one nobody records an agent for.
    pub async fn attribute_writes(
        &self,
        command: &Request,
        parent: [u8; 16],
    ) -> Result<(), runtime_portal::NotAvailable> {
        let failed =
            |message: String| runtime_portal::NotAvailable::Capnp(capnp::Error::failed(message));
        command.validate().map_err(failed)?;
        if parent == [0; 16] {
            return Err(failed("write attribution requires its invocation".into()));
        }
        let bytes = serde_json::to_vec(command).map_err(|e| failed(e.to_string()))?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(failed("write attribution exceeds its bound".into()));
        }
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        request.get().set_on_behalf_of(&parent);
        request.send().promise.await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_name_clean_absolute_paths_and_nobody() {
        let request = Request::new(vec!["/var/workspace/Reports".into()]);
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(is_envelope(&bytes));
        assert_eq!(decode(&bytes), Ok(request));
        // The lane's other commands are not attribution envelopes.
        assert!(!is_envelope(br#"{"Snapshot":null}"#));
        assert!(!is_envelope(b"\"Snapshot\""));

        for paths in [
            vec![],
            vec!["/".to_string()],
            vec!["relative".to_string()],
            vec!["/a/../b".to_string()],
            vec!["/a//b".to_string()],
            vec!["/a\0b".to_string()],
            vec!["/a".to_string(); MAX_PATHS + 1],
        ] {
            assert!(Request::new(paths.clone()).validate().is_err(), "{paths:?}");
        }
        // An envelope cannot carry who acted: that is the host's to know.
        let forged =
            br#"{"protocol":"tilo-write-attribution-v1","paths":["/a"],"actor":"agent:x"}"#;
        assert!(is_envelope(forged));
        assert!(decode(forged).is_err());
    }
}
