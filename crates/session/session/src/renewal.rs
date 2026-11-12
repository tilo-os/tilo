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
//!
//! A join renews before it presents its certificate
//! ([`before_presenting`]): a device that reissues its own does so there,
//! and a joined device waits for the ask the pass may already have under
//! way, so what a host is shown is never a certificate that ran out.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::platform::fabric::SharedLink;
use tilo_identity_host::provisioning::RENEWAL_CAUSES;
use tilo_user_identity::IdentityPaths;

/// How long to wait after an ask that did not renew before asking again, at most: the causes it can
/// fail on also wake it, but a person's device that is simply not answering must not be asked on
/// every pass.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// One joined device's renewal, as the session's passes drive it.
pub(crate) struct FleetRenewal {
    /// This session's identity tree.
    paths: IdentityPaths,
    /// The directory this session's identity record lives in.
    identity_dir: std::path::PathBuf,
    /// The person's fabric the ask goes over: none while this session has none, and then nothing is
    /// asked.
    fabric: Option<SharedLink>,
    /// When the last ask ended without a renewal: what keeps a failure from being retried on every
    /// pass.
    asked_at: Mutex<Option<Instant>>,
    /// Whether a renewal ask is under way: the worker clears it however it ends, so the next pass
    /// after a failure may ask again.
    asking: std::sync::atomic::AtomicBool,
    /// One ask at a time: a join needing the renewal waits here for the ask already under way
    /// instead of asking over it.
    one_at_a_time: Mutex<()>,
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
    pub(crate) fn for_joined_device(fabric: Option<SharedLink>) -> Option<Self> {
        let identity_dir = runtime_identity::identity_dir_from_env().ok()?;
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        if !tilo_user_identity::persist::joined_device_is_enrolled(&paths).ok()? {
            return None;
        }
        Some(Self::new(identity_dir, paths, fabric))
    }

    /// The renewal of the joined device whose identity lives in `identity_dir` , holding the keys
    /// and certificate at `paths` , asking over `fabric` .
    #[must_use]
    pub(crate) fn new(
        identity_dir: std::path::PathBuf,
        paths: IdentityPaths,
        fabric: Option<SharedLink>,
    ) -> Self {
        Self {
            paths,
            identity_dir,
            fabric,
            asked_at: Mutex::new(None),
            asking: std::sync::atomic::AtomicBool::new(false),
            one_at_a_time: Mutex::new(()),
        }
    }

    /// One pass: when this device joined a person's identity and its certificate is due, ask the
    /// person's devices for a renewal, off the loop. Returns whether an ask started.
    pub(crate) fn follow(self: &Arc<Self>) -> bool {
        use std::sync::atomic::Ordering;
        if self.asking.load(Ordering::Acquire) {
            return false;
        }
        if due(&self.paths).is_none() || self.asked_recently() {
            return false;
        }
        self.asking.store(true, Ordering::Release);
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("tilo-device-renewal".into())
            .spawn(move || {
                this.ask_due();
                this.asking.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start renewing this device's certificate");
            self.asking.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// Ask the person's devices for the renewal now, and wait for it: what a join does before it
    /// presents this device's certificate, so the host is shown the renewal rather than one that
    /// ran out. An ask the pass already has under way is waited for; one that just failed is asked
    /// again only for a certificate that has run out, which cannot wait.
    pub(crate) fn renew_now(&self) {
        let _one = self.lock_ask();
        let Some(held) = due(&self.paths) else {
            return;
        };
        if self.asked_recently() && !held.is_expired_at(tilo_clock::unix_now_ns()) {
            return;
        }
        self.ask_with(&held);
    }

    /// Whether the last ask has just ended without a renewal: what keeps a person's device that is
    /// not answering from being asked again at once.
    fn asked_recently(&self) -> bool {
        self.asked_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|ended| ended.elapsed() < RETRY_AFTER)
    }

    /// The ask one at a time: an ask under way holds this, and another waits for it to finish
    /// rather than asking over it.
    fn lock_ask(&self) -> std::sync::MutexGuard<'_, ()> {
        self.one_at_a_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Ask the person's devices for the renewal of what is due and install the answer, alone.
    fn ask_due(&self) {
        let _one = self.lock_ask();
        // An ask that was waited for has renewed it or failed: either way there is nothing due.
        if let Some(held) = due(&self.paths) {
            self.ask_with(&held);
        }
    }

    /// Ask the person's devices to renew `held` and install the answer, with the ask already held.
    fn ask_with(&self, held: &tilo_user_identity::DeviceCert) {
        let Some(fabric) = &self.fabric else {
            return;
        };
        ask(fabric, &self.identity_dir, &self.paths, held);
        self.asked_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(Instant::now());
    }
}

/// The certificate this device holds that is due for renewal now, when it joined a person's
/// identity and has to ask their devices.
fn due(paths: &IdentityPaths) -> Option<tilo_user_identity::DeviceCert> {
    tilo_identity_host::provisioning::joined_device_renewal_due(paths, tilo_clock::unix_now_ns())
}

/// Renew this device's certificate before it is presented to a host: an
/// enrollment that joins a workspace.
///
/// A device that holds its own root reissues what is due itself
/// ([`tilo_onboarding_host::firstboot::renew_due_device_certificates`]);
/// one that joined a person's identity asks their devices and waits for the
/// renewal. A certificate still outside its validity is said here, so what a
/// host is shown is never one it refuses.
///
/// # Errors
///
/// What the reissue of a certificate held here failed on, or that a
/// certificate outside its validity could not be renewed.
pub(crate) fn before_presenting(renewal: Option<&FleetRenewal>) -> Result<(), String> {
    tilo_onboarding_host::firstboot::renew_due_device_certificates()
        .map_err(|error| error.to_string())?;
    if let Some(renewal) = renewal {
        renewal.renew_now();
    }
    tilo_identity_host::provisioning::device_certificate_clock_refusal(
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
        tilo_clock::unix_now_ns(),
    )
    .map_or(Ok(()), |refusal| {
        Err(format!(
            "this device's certificate is outside its validity and could not be renewed \
             before the join: {refusal}"
        ))
    })
}

/// Ask the person's devices to renew `held` and install the answer, saying what became of it. Runs
/// off the session's loop.
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
                    // The renewal loop and any enrollment waiting on custody may look again now.
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

/// Ask the person's devices to sign a renewal of `held` : an unsigned certificate for the same
/// device and root, issued now, granting the same and expiring as `held` did.
/// A certificate is signed by `threshold` of the identity's `total` devices
/// ([`tilo_identity_host::device_admin::key_holding`]); the fabric answers with this device's share
/// when it holds one, and with the person's other devices' when it does not.
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
    // A renewal keeps the certificate's lifetime shape: one that expires keeps expiring, and one
    // that never did stays that way.
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
