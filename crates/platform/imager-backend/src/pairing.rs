//! Existing-device enrollment actor. Unsupported hosts never launch a helper.
use crate::prepared_identity::PreparedIdentity;
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod native;

#[derive(Default)]
pub struct Pairing {
    #[cfg(target_os = "linux")]
    native: native::Pairing,
}

impl Pairing {
    pub fn supported() -> bool {
        cfg!(target_os = "linux")
    }
    pub fn resume(prepared: Arc<PreparedIdentity>) -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        return Ok(Self {
            native: native::Pairing::resume(prepared)?,
        });
        #[cfg(not(target_os = "linux"))]
        {
            let _ = prepared;
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
    pub fn busy(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.native.busy();
        #[cfg(not(target_os = "linux"))]
        false
    }
    pub fn ready(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.native.ready();
        #[cfg(not(target_os = "linux"))]
        false
    }
    pub fn locked_label(&self) -> bool {
        self.busy() || self.ready()
    }
    pub fn started(&self) -> bool {
        self.busy() || self.ready()
    }
    pub fn prepared(&self) -> Option<Arc<PreparedIdentity>> {
        #[cfg(target_os = "linux")]
        return self.native.prepared();
        #[cfg(not(target_os = "linux"))]
        None
    }
    pub fn start(
        &mut self,
        label: &str,
        requester_pid: u32,
        requester_start_time: u64,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        return self
            .native
            .start(label, requester_pid, requester_start_time);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (label, requester_pid, requester_start_time);
            Err("installer-pairing-helper-unavailable")
        }
    }
    pub fn cancel(&mut self) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        return self.native.cancel();
        #[cfg(not(target_os = "linux"))]
        Err("installer-pairing-not-running")
    }
    pub fn poll(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        return self.native.poll();
        #[cfg(not(target_os = "linux"))]
        false
    }
    pub fn code(&self) -> Option<&str> {
        #[cfg(target_os = "linux")]
        return self.native.code();
        #[cfg(not(target_os = "linux"))]
        None
    }
    pub fn state(&self) -> &'static str {
        #[cfg(target_os = "linux")]
        return self.native.state();
        #[cfg(not(target_os = "linux"))]
        "unsupported"
    }
    pub fn projection(&self) -> serde_json::Value {
        #[cfg(target_os = "linux")]
        return self.native.projection();
        #[cfg(not(target_os = "linux"))]
        serde_json::json!({"state":"unsupported", "verification_code":null, "device_label":null, "public_key":null})
    }
    #[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
    pub fn fixture_operation_directory(&self) -> std::path::PathBuf {
        self.native.fixture_operation_directory()
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unsupported_pairing_never_launches_or_adopts_custody() {
        let mut pairing = Pairing::default();
        assert!(!Pairing::supported());
        assert_eq!(
            pairing.start("Future computer", 1, 1),
            Err("installer-pairing-helper-unavailable")
        );
        assert!(!pairing.poll());
        assert!(!pairing.busy());
        assert!(!pairing.ready());
        assert!(pairing.prepared().is_none());
        assert_eq!(pairing.projection()["state"], "unsupported");
        assert_eq!(pairing.cancel(), Err("installer-pairing-not-running"));
    }
}

#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub fn certified_fixture() -> (tempfile::TempDir, Pairing) {
    let (directory, native) = native::certified_fixture();
    (directory, Pairing { native })
}
#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub fn failed_fixture() -> (tempfile::TempDir, Pairing) {
    let (directory, native) = native::failed_fixture();
    (directory, Pairing { native })
}

#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub fn waiting_fixture() -> (tempfile::TempDir, Pairing) {
    let (directory, native) = native::waiting_fixture();
    (directory, Pairing { native })
}
