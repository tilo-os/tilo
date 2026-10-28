//! A device certificate refused only because of the wall clock's reading,
//! and waiting for the wall clock to reach an instant or to change.
//!
//! A certificate that verifies but is "not yet valid" or "expired" says
//! nothing about the identity it certifies: the same bytes verify again once
//! the clock reads a time inside their validity interval. A machine whose
//! real-time clock came up weeks behind (a PMIC counter that restarted, a
//! drained clock battery) refuses its own certificate until network time
//! steps the clock. Callers tell that apart from a broken or missing
//! identity with [`CertificateClockError`], and wait for the clock with
//! [`wait_for_wall_clock`] instead of exiting or retrying on a timer.

use std::io;

use crate::cert::{DeviceCert, DeviceCertError};

/// A certificate whose signature verified and which was refused only
/// because the wall clock read a time outside its validity interval.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct CertificateClockError {
    /// [`DeviceCertError::NotYetValid`] or [`DeviceCertError::Expired`].
    pub reason: DeviceCertError,
    /// The certificate's issuance instant, Unix nanoseconds.
    pub valid_from_ns: u64,
    /// The certificate's expiry, Unix nanoseconds, when it has one.
    pub valid_until_ns: Option<u64>,
    /// What the wall clock read when the certificate was refused.
    pub clock_ns: u64,
}

impl CertificateClockError {
    /// The clock refusal `error` stands for, when `error` is what
    /// [`crate::verify_device_cert_at`] returned for `cert` at `clock_ns`
    /// and the refusal was about time alone.
    #[must_use]
    pub fn of(cert: &DeviceCert, clock_ns: u64, error: &DeviceCertError) -> Option<Self> {
        matches!(
            error,
            DeviceCertError::NotYetValid | DeviceCertError::Expired
        )
        .then(|| Self {
            reason: error.clone(),
            valid_from_ns: cert.issued_at_ns,
            valid_until_ns: cert.expires_at_ns,
            clock_ns,
        })
    }

    /// The instant the certificate becomes valid if the clock only runs
    /// forward: its issuance when the clock is behind it. An expired
    /// certificate has none; only a clock moved back can admit it again.
    #[must_use]
    pub fn valid_at_ns(&self) -> Option<u64> {
        (self.reason == DeviceCertError::NotYetValid).then_some(self.valid_from_ns)
    }

    /// This refusal as an [`io::Error`] that [`Self::in_io`] recognises
    /// through any caller that passes the error on unchanged.
    #[must_use]
    pub fn into_io(self) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, self)
    }

    /// The clock refusal carried by `error`, when it is one.
    #[must_use]
    pub fn in_io(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref::<Self>()
    }
}

/// What ended a [`wait_for_wall_clock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallClockWait {
    /// The wall clock reached the instant.
    Reached,
    /// The wall clock was set or stepped (or the machine resumed) before
    /// it reached the instant. Whatever depended on its reading may have
    /// changed either way.
    Changed,
}

/// Block until the wall clock reaches `unix_ns` (Unix nanoseconds), or
/// until it changes discontinuously, whichever comes first. Returns at once
/// when the clock already reads `unix_ns` or later.
///
/// On Linux this is one absolute `CLOCK_REALTIME` timerfd armed with
/// `TFD_TIMER_CANCEL_ON_SET`: the kernel ends the wait when the instant
/// arrives or when the clock is set, so nothing polls and no step is
/// missed, however far the clock jumps.
///
/// # Errors
///
/// Returns the system error when the timer cannot be created, armed or
/// read.
#[cfg(target_os = "linux")]
pub fn wait_for_wall_clock(unix_ns: u64) -> io::Result<WallClockWait> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    // SAFETY: a plain syscall; the descriptor is checked and owned once.
    let raw = unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let timer = unsafe { OwnedFd::from_raw_fd(raw) };
    // Zero would disarm the timer: an instant at the epoch fires at its
    // first nanosecond instead.
    let at = unix_ns.max(1);
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: libc::time_t::try_from(at / 1_000_000_000).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::try_from(at % 1_000_000_000).unwrap_or(0),
        },
    };
    // SAFETY: `timer` is a live timerfd and `spec` a valid value.
    let armed = unsafe {
        libc::timerfd_settime(
            timer.as_raw_fd(),
            libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            &raw const spec,
            std::ptr::null_mut(),
        )
    };
    if armed != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut expirations = [0u8; 8];
    loop {
        // SAFETY: `expirations` is a writable buffer of the length passed.
        let read = unsafe {
            libc::read(
                timer.as_raw_fd(),
                expirations.as_mut_ptr().cast::<libc::c_void>(),
                expirations.len(),
            )
        };
        if read == 8 {
            return Ok(WallClockWait::Reached);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ECANCELED) => return Ok(WallClockWait::Changed),
            Some(libc::EINTR) => {}
            _ => return Err(error),
        }
    }
}

/// [`wait_for_wall_clock`] where the kernel cannot report clock changes:
/// one sleep until the instant, which a clock step does not shorten.
///
/// # Errors
///
/// Never; the signature matches the Linux implementation.
#[cfg(all(not(target_os = "linux"), not(target_arch = "wasm32")))]
pub fn wait_for_wall_clock(unix_ns: u64) -> io::Result<WallClockWait> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    if let Some(remaining) = unix_ns.checked_sub(now).filter(|ns| *ns > 0) {
        std::thread::sleep(std::time::Duration::from_nanos(remaining));
    }
    Ok(WallClockWait::Reached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{DeviceCapabilities, sign_device_cert, verify_device_cert_at};

    fn cert(issued_at_ns: u64, expires_at_ns: Option<u64>) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    #[test]
    fn a_certificate_refused_for_the_clock_alone_is_told_apart_through_io_errors() {
        let cert = cert(1_000, Some(2_000));

        let behind = verify_device_cert_at(&cert, 500).unwrap_err();
        let refusal = CertificateClockError::of(&cert, 500, &behind).expect("a clock refusal");
        assert_eq!(refusal.valid_at_ns(), Some(1_000));
        let carried = refusal.clone().into_io();
        assert_eq!(carried.kind(), io::ErrorKind::InvalidData);
        assert_eq!(CertificateClockError::in_io(&carried), Some(&refusal));
        assert_eq!(carried.to_string(), "device cert is not yet valid");

        let ahead = verify_device_cert_at(&cert, 2_000).unwrap_err();
        let expired = CertificateClockError::of(&cert, 2_000, &ahead).expect("a clock refusal");
        assert_eq!(
            expired.valid_at_ns(),
            None,
            "the clock running on never admits it"
        );

        assert_eq!(
            CertificateClockError::of(&cert, 500, &DeviceCertError::BadSignature),
            None
        );
        let other = io::Error::new(
            io::ErrorKind::InvalidData,
            "device-cert signature did not verify",
        );
        assert!(CertificateClockError::in_io(&other).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn waiting_for_the_wall_clock_ends_when_it_reaches_the_instant() {
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| u64::try_from(since.as_nanos()).unwrap())
                .unwrap()
        };
        assert_eq!(wait_for_wall_clock(1).unwrap(), WallClockWait::Reached);
        let soon = now() + 50_000_000;
        assert_eq!(wait_for_wall_clock(soon).unwrap(), WallClockWait::Reached);
        assert!(now() >= soon);
    }
}
