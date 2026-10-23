//! Root controller workspace creation; no source or service authority is minted here.
use std::{fs::File, io};

#[cfg(target_os = "linux")]
mod native;

/// Transport used by an already authenticated retained-source service.
#[cfg(unix)]
pub type ControlStream = std::os::unix::net::UnixStream;

/// Opaque transport on hosts without native retained-source channels.
/// There is deliberately no caller-selected transport constructor.
#[cfg(not(unix))]
pub struct ControlStream {
    _private: (),
}

/// Retain the checked, root-controlled worker directory ancestry.
pub fn working_parent() -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        native::working_parent()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(super::Unsupported.into())
    }
}

/// Create private controller state beneath its already checked worker parent.
/// The caller must retain `working_parent()` throughout this workspace's life.
pub fn temporary_directory(prefix: &str) -> io::Result<tempfile::TempDir> {
    #[cfg(target_os = "linux")]
    {
        native::temporary_directory(prefix)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = prefix;
        Err(super::Unsupported.into())
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[cfg(not(unix))]
    #[test]
    fn unsupported_deadline_never_runs_the_exchange() {
        use super::super::deadline::{ProofInterrupt, with_proof_deadline_at};
        use std::{sync::atomic::AtomicBool, time::Instant};

        let stream = ControlStream { _private: () };
        let observation = ProofInterrupt::observe(&stream).err().unwrap();
        let exchange = with_proof_deadline_at(
            &stream,
            &AtomicBool::new(false),
            Instant::now(),
            || -> io::Result<()> { panic!("unsupported exchange must not run") },
        )
        .unwrap_err();
        for error in [observation, exchange] {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(error.get_ref().unwrap().is::<super::super::Unsupported>());
        }
    }

    #[test]
    fn unsupported_workspace_refuses_before_creating_state() {
        for error in [
            working_parent().unwrap_err(),
            temporary_directory("retained-worker-").unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(error.get_ref().unwrap().is::<super::super::Unsupported>());
        }
    }
}
