//! Keeping a joined device's certificate renewed through the person's
//! fleet.
//!
//! A device that joined another person's identity holds no root, so
//! [`tilo_identity_host::provisioning::ensure_collaboration_identity`] has
//! no key to reissue its certificate with and answers `Existing` however
//! due the certificate is. Its renewal has to come from a device of the
//! person that does hold the root, exactly as a new device is certified:
//! this session asks each of the person's devices over the fabric
//! ([`tilo_identity_host::certification_ceremony::Gathering::renewal`]),
//! and the first key holder signs. The certificate that comes back replaces
//! this device's, in its identity tree and in the identity's fleet
//! ([`tilo_identity_host::provisioning::install_renewed_certificate`]), so
//! a host is shown the renewal from then on.
//!
//! The check runs on the session's pass, off the loop, once the fabric is
//! up and the certificate is due. A renewal that fails waits for what it
//! failed on — the fabric coming back, custody becoming available — through
//! [`tilo_identity_host::provisioning::RENEWAL_CAUSES`], and the running
//! renewal loop picks up the new expiry on its own.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_device_fabric::link::SharedLink;
use tilo_identity_host::provisioning::RENEWAL_CAUSES;
use tilo_user_identity::IdentityPaths;

/// How long to wait after an ask that did not renew before asking again, at
/// most: the causes it can fail on also wake it, but a person's device that
/// is simply not answering must not be asked on every pass.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// One joined device's renewal, as the session's passes drive it.
pub(crate) struct FleetRenewal {
    /// This session's identity tree.
    paths: IdentityPaths,
    /// The directory this session's identity record lives in.
    identity_dir: std::path::PathBuf,
    /// When the last ask ended without a renewal: what keeps a failure from
    /// being retried on every pass.
    asked_at: Arc<Mutex<Option<Instant>>>,
    /// Whether a renewal ask is under way: the worker clears it however it
    /// ends, so the next pass after a failure may ask again.
    asking: Arc<std::sync::atomic::AtomicBool>,
}

impl FleetRenewal {
    /// The renewal of this session's own device, when it joined a person's
    /// identity instead of being provisioned with their root.
    ///
    /// `None` for an ordinary first-boot identity, whose own session
    /// reissues its certificate
    /// ([`tilo_identity_host::provisioning::ensure_collaboration_identity`]),
    /// and for a session with no identity at all.
    #[must_use]
    pub(crate) fn for_joined_device() -> Option<Self> {
        let identity_dir = runtime_identity::identity_dir_from_env().ok()?;
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        if !tilo_user_identity::persist::joined_device_is_enrolled(&paths).ok()? {
            return None;
        }
        Some(Self::new(identity_dir, paths))
    }

    /// The renewal of the joined device whose identity lives in
    /// `identity_dir`, holding the keys and certificate at `paths`.
    #[must_use]
    pub(crate) fn new(identity_dir: std::path::PathBuf, paths: IdentityPaths) -> Self {
        Self {
            paths,
            identity_dir,
            asked_at: Arc::new(Mutex::new(None)),
            asking: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// One pass: when this device joined a person's identity and its
    /// certificate is due, ask the person's devices for a renewal, off the
    /// loop. Returns whether an ask started.
    pub(crate) fn follow(&self, fabric: &SharedLink) -> bool {
        use std::sync::atomic::Ordering;
        if self.asking.load(Ordering::Acquire) {
            return false;
        }
        let now = tilo_clock::unix_now_ns();
        let Some(held) =
            tilo_identity_host::provisioning::joined_device_renewal_due(&self.paths, now)
        else {
            return false;
        };
        if self
            .asked_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|ended| ended.elapsed() < RETRY_AFTER)
        {
            return false;
        }
        self.asking.store(true, Ordering::Release);
        let fabric = Arc::clone(fabric);
        let paths = self.paths.clone();
        let identity_dir = self.identity_dir.clone();
        let asking = Arc::clone(&self.asking);
        let asked_at = Arc::clone(&self.asked_at);
        let spawned = std::thread::Builder::new()
            .name("tilo-device-renewal".into())
            .spawn(move || {
                ask(&fabric, &identity_dir, &paths, &held);
                asked_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .replace(Instant::now());
                asking.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start renewing this device's certificate");
            self.asking.store(false, Ordering::Release);
            return false;
        }
        true
    }
}

/// Ask the person's devices to renew `held` and install the answer, saying
/// what became of it. Runs off the session's loop.
fn ask(
    fabric: &SharedLink,
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
    held: &tilo_user_identity::DeviceCert,
) {
    match renew(fabric, identity_dir, held) {
        Ok(renewed) => {
            match tilo_identity_host::provisioning::install_renewed_certificate(
                identity_dir,
                paths,
                held,
                &renewed,
                tilo_clock::unix_now_ns(),
            ) {
                Ok(()) => {
                    tracing::info!(
                        target: tilo_startup_metrics::JOIN_FLOW_TARGET,
                        identity = %identity_dir.display(),
                        device = %hex::encode(renewed.device_public_key),
                        outcome = "renewed",
                        "renewal: this device's certificate was renewed through the person's devices"
                    );
                    // The renewal loop and any enrollment waiting on custody
                    // may look again now.
                    RENEWAL_CAUSES.changed();
                }
                Err(error) => tracing::warn!(
                    target: tilo_startup_metrics::JOIN_FLOW_TARGET,
                    identity = %identity_dir.display(),
                    %error,
                    outcome = "failed",
                    "renewal: the renewal was signed but not written"
                ),
            }
        }
        Err(error) => tracing::info!(
            target: tilo_startup_metrics::JOIN_FLOW_TARGET,
            identity = %identity_dir.display(),
            %error,
            outcome = "failed",
            "renewal: the person's devices did not renew this one's certificate"
        ),
    }
}

/// Ask the person's devices to sign a renewal of `held`: an unsigned
/// certificate for the same device and root, issued now, granting the same
/// and expiring as `held` did.
///
/// A certificate is signed by `threshold` of the identity's `total` devices
/// ([`tilo_identity_host::device_admin::key_holding`]); the fabric answers
/// with this device's share when it holds one, and with the person's other
/// devices' when it does not.
fn renew(
    fabric: &SharedLink,
    identity_dir: &std::path::Path,
    held: &tilo_user_identity::DeviceCert,
) -> Result<tilo_user_identity::DeviceCert, String> {
    use tilo_identity_host::device_admin::{KeyHolding, key_holding};

    let person = tilo_identity_host::device_admin::PersonPaths {
        dir: identity_dir.to_path_buf(),
        root: identity_dir.to_path_buf(),
    };
    // The identity's own record says how many of the person's devices must
    // sign: a joined device's record names the person's whole key as 1 of 1,
    // which is exactly the count a renewal is gathered under.
    let (threshold, total) = match key_holding(&person)? {
        KeyHolding::Whole => (1, 1),
        KeyHolding::Part {
            threshold, total, ..
        } => (threshold, total),
    };
    let now = tilo_clock::unix_now_ns();
    let mut cert = held.clone();
    cert.issued_at_ns = now;
    // A renewal keeps the certificate's lifetime shape: one that expires
    // keeps expiring, and one that never did stays that way.
    cert.expires_at_ns = held
        .expires_at_ns
        .map(|expires| now.saturating_add(expires.saturating_sub(held.issued_at_ns)));
    cert.signature = [0; 64];
    crate::certification::renew(fabric, cert, held.clone(), [0; 32], threshold, total)
}

impl std::fmt::Debug for FleetRenewal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetRenewal")
            .field("identity", &self.identity_dir)
            .finish_non_exhaustive()
    }
}
