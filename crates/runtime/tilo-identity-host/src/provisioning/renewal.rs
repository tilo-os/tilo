//! Keeping this device's expiring certificates renewed while it runs: when
//! one is due, when it was renewed, and why a renewal failed. A device that
//! slept past the time a renewal was due renews when it wakes and its
//! network comes back, never when it is refused at a host.

use std::io;
use std::path::Path;

use tilo_user_identity::{DeviceCert, IdentityPaths, persist};

/// What a renewal can fail on changing: this device's identity custody
/// becoming available, the network coming back, a relay or host becoming
/// reachable again, room to write coming back. A renewal that failed waits
/// for one of them, not for a clock ([`keep_device_certificates_renewed`]).
pub struct RenewalCauses {
    changes: std::sync::Mutex<u64>,
    changed: std::sync::Condvar,
}

impl RenewalCauses {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            changes: std::sync::Mutex::new(0),
            changed: std::sync::Condvar::new(),
        }
    }

    /// Note that one of them changed.
    pub fn changed(&self) {
        let mut changes = self
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *changes = changes.wrapping_add(1);
        drop(changes);
        self.changed.notify_all();
    }

    /// The changes so far: [`Self::wait_past`] waits for one after them.
    #[must_use]
    pub fn seen(&self) -> u64 {
        *self
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for a change after `seen`, or until `timeout` passes when there
    /// is one. Answers whether there was a change. A change noted between
    /// [`Self::seen`] and this wait ends it at once.
    pub fn wait_past(&self, seen: u64, timeout: Option<std::time::Duration>) -> bool {
        let deadline = timeout.map(|timeout| std::time::Instant::now().checked_add(timeout));
        let mut changes = self
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *changes == seen {
            changes = match deadline {
                None | Some(None) => self
                    .changed
                    .wait(changes)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Some(Some(deadline)) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return false;
                    }
                    self.changed
                        .wait_timeout(changes, left)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0
                }
            };
        }
        drop(changes);
        true
    }
}

impl Default for RenewalCauses {
    fn default() -> Self {
        Self::new()
    }
}

/// This device's: the session notes changes here, and its renewal loop
/// waits on them.
pub static RENEWAL_CAUSES: RenewalCauses = RenewalCauses::new();

/// The time and the changes a renewal loop runs by
/// ([`keep_device_certificates_renewed`]).
pub trait RenewalClock {
    /// Now, in nanoseconds since the Unix epoch.
    fn now_ns(&self) -> u64;
    /// The changes so far to what a renewal can fail on
    /// ([`RenewalCauses::seen`]).
    fn changes(&self) -> u64;
    /// Wait `duration`, or as long as it takes with `None`, unless one of
    /// those changes after `seen` first. Answers whether to go on.
    fn wait(&self, duration: Option<std::time::Duration>, seen: u64) -> bool;
}

/// Keep this device's expiring certificates for the identity in
/// `identity_dir` renewed while the device runs: wait until the next is due
/// ([`next_device_certificate_renewal`]), renew it through `renew` (the
/// provisioning the session started with, which reissues what is due), and
/// wait for the next. Returns once none of them expires, or when `clock`
/// says to stop.
///
/// A renewal that fails is tried again when something it can fail on
/// changes ([`RenewalCauses`]), never on a timer. Any such change also ends
/// the wait for the next renewal early, to look at the wall clock again: a
/// device that slept past the time a renewal was due renews when it wakes
/// and its network comes back.
pub fn keep_device_certificates_renewed(
    identity_dir: &Path,
    paths: &IdentityPaths,
    clock: &dyn RenewalClock,
    mut renew: impl FnMut() -> io::Result<()>,
) {
    use tilo_startup_metrics::JOIN_FLOW_TARGET;
    loop {
        let Some(due) = next_device_certificate_renewal(identity_dir, paths) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                identity = %identity_dir.display(),
                "renewal: no certificate of this device expires, so none is renewed"
            );
            return;
        };
        let seen = clock.changes();
        let now = clock.now_ns();
        let wait = if due > now {
            tracing::debug!(
                target: JOIN_FLOW_TARGET,
                identity = %identity_dir.display(),
                due_ns = due,
                now_ns = now,
                in_secs = (due.saturating_sub(now)) / 1_000_000_000,
                "renewal: the next certificate is not due yet"
            );
            Some(std::time::Duration::from_nanos(due.saturating_sub(now)))
        } else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                identity = %identity_dir.display(),
                due_ns = due,
                now_ns = now,
                "renewal: a certificate is due; renewing it"
            );
            match renew() {
                Ok(())
                    if next_device_certificate_renewal(identity_dir, paths)
                        .is_some_and(|next| next <= now) =>
                {
                    tracing::warn!("a renewed device certificate is due again at once");
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        identity = %identity_dir.display(),
                        outcome = "failed",
                        "renewal: the certificate is due again at once after being renewed"
                    );
                    None
                }
                Ok(()) => {
                    tracing::info!("renewed this device's certificate before it expires");
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        identity = %identity_dir.display(),
                        outcome = "renewed",
                        "renewal: the device's certificate was renewed"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "could not renew this device's certificate; trying again when what it failed on changes"
                    );
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        identity = %identity_dir.display(),
                        %error,
                        outcome = "failed",
                        "renewal: the device's certificate could not be renewed"
                    );
                    None
                }
            }
        };
        if !clock.wait(wait, seen) {
            return;
        }
    }
}

/// When `cert` is due for renewal: halfway through its lifetime. `None` for
/// a certificate that never expires.
#[must_use]
pub fn renewal_due_at(cert: &DeviceCert) -> Option<u64> {
    let expires = cert.expires_at_ns?;
    let lifetime = expires.saturating_sub(cert.issued_at_ns);
    Some(cert.issued_at_ns.saturating_add(lifetime / 2))
}

/// When the next of this device's certificates for the identity in
/// `identity_dir` is due for renewal ([`renewal_due_at`]): the device's own
/// certificate and the one keyed by the identity's root. `None` when none
/// of them expires, or none can be read.
#[must_use]
pub fn next_device_certificate_renewal(identity_dir: &Path, paths: &IdentityPaths) -> Option<u64> {
    let mut certificates = vec![paths.device_cert.clone()];
    if let Some(record) = runtime_identity::resolve_active_identity(identity_dir)
        && let Ok(identity) = runtime_identity::load_from_path(record)
    {
        certificates.push(paths.device_cert_for_user(&identity.group_public_key()));
    }
    certificates
        .iter()
        .filter_map(|path| persist::read_device_cert(path).ok())
        .filter_map(|cert| renewal_due_at(&cert))
        .min()
}

/// The certificate a joined device holds: its own, when it has no local
/// root and the certificate is due for renewal at `now_ns`.
///
/// A device that joined another person's identity holds no root, so nothing
/// here can reissue its certificate ([`ensure_collaboration_identity`]
/// answers `Existing`). Its renewal has to be signed by a device of the
/// person that holds the root
/// ([`crate::certification_ceremony::Gathering::renewal`]), which a session
/// asks for over the person's fabric and installs with
/// [`install_renewed_certificate`]. This answers `Some(held)` when such a
/// renewal is due, so a caller knows to ask.
///
/// Whether this device holds the person's root is
/// [`persist::joined_device_is_enrolled`]'s question, not a file's presence:
/// a primary device keeps its root in its identity record as well as beside
/// it, and reissues its own certificate however due it is.
///
/// [`ensure_collaboration_identity`]: crate::provisioning::ensure_collaboration_identity
#[must_use]
pub fn joined_device_renewal_due(paths: &IdentityPaths, now_ns: u64) -> Option<DeviceCert> {
    if !persist::joined_device_is_enrolled(paths).ok()? {
        return None;
    }
    let held = persist::read_device_cert(&paths.device_cert).ok()?;
    let due = renewal_due_at(&held)?;
    (now_ns >= due).then_some(held)
}

/// Write `renewed`, a certificate the person's root signed for this device,
/// over the one it replaces: the system-facing certificate, this identity's
/// own copy, and the identity's fleet.
///
/// The person's own root never comes here. [`persist::install_device_cert`]
/// refuses a certificate that is not this device's key, does not verify,
/// grants device admission, or belongs to another identity; a renewal adds
/// none of that. The fleet is the one a joined device's identity directory
/// keeps ([`crate::device_fleet::DeviceFleetStore`]), so the certificate a
/// host is shown is the one this fleet holds.
pub fn install_renewed_certificate(
    identity_dir: &Path,
    paths: &IdentityPaths,
    held: &DeviceCert,
    renewed: &DeviceCert,
    now_ns: u64,
) -> io::Result<()> {
    if !renewed.renews(held) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the renewed certificate is not the same device under the same root, later",
        ));
    }
    persist::install_device_cert(paths, renewed, now_ns)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    crate::device_fleet::DeviceFleetStore::at(identity_dir, paths.clone())
        .remember_certificate(&hex::encode(renewed.user_public_key), renewed.clone())
        .map(drop)
}

#[cfg(test)]
#[path = "renewal_tests.rs"]
pub(crate) mod renewal_tests;
