//! Portable capture lifetime and native authenticated process custody.
//! Constructors certify live kernel peers; serialized caller facts alone are
//! never authority to project a source or signal a process.
#[cfg_attr(not(target_os = "linux"), path = "capture/host_owner/unsupported.rs")]
pub mod host_owner;
pub mod host_session;
mod lifecycle;
pub mod original_caller;
pub mod process;
#[cfg(target_os = "linux")]
pub mod runtime;

/// Home capture protocol binding mounted roots to the sealed descriptor table.
pub const WORKER_PROTOCOL: u32 = 5;
/// Capture protocol additionally supporting authenticated Applications exports.
pub const APPLICATIONS_PROTOCOL: u32 = 6;
#[cfg(target_os = "linux")]
mod projection;
mod projection_receipt;
pub use projection_receipt::ProjectionReceipt;
#[cfg_attr(not(target_os = "linux"), path = "capture/transport/unsupported.rs")]
pub mod transport;
pub use lifecycle::Capture;

/// Native capture support is absent, rather than failed or partially started.
#[derive(Debug)]
pub enum Error {
    Unsupported,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Home capture is unsupported on this platform"),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unsupported => None,
            Self::Io(error) => Some(error),
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Unsupported => Self::new(std::io::ErrorKind::Unsupported, Error::Unsupported),
            Error::Io(error) => error,
        }
    }
}

/// Platform support only; packaged provider qualification is a separate check.
///
/// # Errors
/// Returns [`Error::Unsupported`] on platforms other than Linux.
pub fn supported() -> Result<(), Error> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(Error::Unsupported)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Intended,
    Ready,
    Releasing,
    Released,
}

/// Bounded public status contains no host path, snapshot handle or user data.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub operation: String,
    pub state: State,
    pub expires_unix: Option<u64>,
}

fn failure(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_support_is_typed_and_preserves_io_kind() {
        if cfg!(target_os = "linux") {
            supported().unwrap();
        } else {
            assert!(matches!(supported(), Err(Error::Unsupported)));
        }
        let error = std::io::Error::from(Error::Unsupported);
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn status_wire_shape_remains_bounded_and_path_free() {
        let status = Status {
            operation: "0f273186-e49a-4f14-8507-e13c6c16925a".into(),
            state: State::Ready,
            expires_unix: Some(123),
        };
        let bytes = serde_json::to_vec(&status).unwrap();
        assert_eq!(
            bytes,
            br#"{"operation":"0f273186-e49a-4f14-8507-e13c6c16925a","state":"ready","expires_unix":123}"#
        );
        assert_eq!(serde_json::from_slice::<Status>(&bytes).unwrap(), status);
        assert!(
            serde_json::from_str::<Status>(
                r#"{"operation":"x","state":"ready","expires_unix":null,"snapshot":"/private"}"#
            )
            .is_err()
        );
    }
}
