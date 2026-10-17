//! Read-only custody for completing original-file preparation on boot media.
//!
//! The same API is available on every target. Unsupported hosts cannot create a
//! grant; Linux admission still belongs to the authenticated native helper.
use crate::imager_model::TargetDrive;
use std::{io, sync::atomic::AtomicBool};

/// Refuse unsupported hosts before requesting credentials or starting a job.
pub fn supported() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(unsupported())
    }
}

/// A selected-medium grant, never constructible from a path or serialized data.
/// Dropping it retains the native helper's existing retirement semantics.
pub struct BootMediaGrant {
    #[cfg(target_os = "linux")]
    native: crate::setup::read_media::ReadOnlyMediaGrant,
    #[cfg(not(target_os = "linux"))]
    _unavailable: std::convert::Infallible,
}

/// Admit the exact selected boot medium through the native read-only helper.
pub fn admit_boot(target: &TargetDrive, cancel: &AtomicBool) -> io::Result<BootMediaGrant> {
    #[cfg(target_os = "linux")]
    {
        Ok(BootMediaGrant {
            native: crate::setup::read_media::admit_boot(target, cancel)?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (target, cancel);
        Err(unsupported())
    }
}

impl BootMediaGrant {
    pub fn complete_original(
        &self,
        secret: zeroize::Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native.complete_original(secret, cancel)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (secret, cancel);
            Err(unsupported())
        }
    }

    /// Adopt the completed generation through a fresh authenticated grant.
    pub fn extract_completed(
        &self,
        secret: zeroize::Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native.extract_completed(secret, cancel)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (secret, cancel);
            Err(unsupported())
        }
    }
}

/// Whether the native registry still owns a retired helper for this key.
pub fn retirement_pending(key: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::setup::read_media::retirement_pending(key)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = key;
        false
    }
}

/// Whether every retired helper for this key has exited.
pub fn retirement_ready(key: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::setup::read_media::retirement_ready(key)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = key;
        true
    }
}

/// Finish retained helper cleanup off the UI thread before admitting another job.
pub fn finish_retirement(key: &str) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        crate::setup::read_media::finish_retirement(key)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = key;
        Err(unsupported())
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Completing original-file preparation requires Linux",
    )
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn boot_media_is_unavailable() {
        assert_eq!(supported().unwrap_err().kind(), io::ErrorKind::Unsupported);
        // Exercise admission itself, not only its frontend availability check.
        let target = TargetDrive {
            name: "unavailable".into(),
            model: String::new(),
            stable_id: None,
            size: 0,
            kind: crate::imager_model::TargetKind::Other,
            tran: None,
            removable: true,
            is_system: false,
            partitions: Vec::new(),
        };
        assert!(matches!(
            admit_boot(&target, &AtomicBool::new(false)),
            Err(error) if error.kind() == io::ErrorKind::Unsupported
        ));
    }
}
