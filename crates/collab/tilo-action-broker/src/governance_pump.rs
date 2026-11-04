//! The thread that settles a device's channel groups.
//!
//! [`ChannelKeyAccess::pump`] runs one pass of the channel protocol: it takes
//! what the workspace transport carries, moves every group to the access
//! policy, keeps waiting devices' requests alive, and publishes what the
//! transport did not take. A device runs it on a thread of its own
//! ([`GovernancePump`]), which sleeps between passes until something happens
//! ([`GovernancePark`]).

use std::io;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::channel_encryption::{ChannelKeyAccess, GovernanceTransport};

/// How long a live channel's governance is pumped: reconciliation and
/// requests run on deadlines, and a pump that sleeps between them would hold
/// a request the policy answers for longer than the workspace's members
/// expect. With no channel live the pump parks until [`GovernancePark::poke`].
const LIVE_INTERVAL: Duration = Duration::from_secs(1);

/// Parks a governance pump between passes: it waits until poked, or for the
/// deadline the last pass named.
///
/// A poke that arrives before the pump waits is kept, so governance queued
/// just before the pump parks is not left behind.
#[derive(Default)]
pub struct GovernancePark {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl GovernancePark {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Wake the pump: a frame arrived, an endpoint opened a channel, the
    /// workspace's governance moved, or the service stops.
    pub fn poke(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = true;
        drop(pending);
        self.ready.notify_one();
    }

    /// Sleep until poked, or for at most `timeout` when there is one.
    fn wait(&self, timeout: Option<Duration>) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut pending = match timeout {
            Some(timeout) => {
                self.ready
                    .wait_timeout_while(pending, timeout, |pending| !*pending)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0
            }
            None => self
                .ready
                .wait_while(pending, |pending| !*pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        };
        *pending = false;
    }
}

/// A device's channel service, settling on a thread of its own; stops when
/// dropped.
pub struct GovernancePump {
    park: Arc<GovernancePark>,
    stop: Arc<AtomicBool>,
}

impl Drop for GovernancePump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.park.poke();
    }
}

impl GovernancePump {
    /// Run `access`'s governance over `transport` on a thread named `name`:
    /// once per [`LIVE_INTERVAL`] while a channel is live, otherwise only when
    /// `park` is poked. The service's [`ChannelKeyAccess::with_wake`] should
    /// be `park`'s own poke.
    ///
    /// # Errors
    ///
    /// The thread could not start.
    pub fn spawn(
        name: &str,
        park: Arc<GovernancePark>,
        access: ChannelKeyAccess,
        transport: Arc<dyn GovernanceTransport + Send + Sync>,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let woken = Arc::clone(&park);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    access.pump(&*transport);
                    let timeout = access.has_live_channels().then_some(LIVE_INTERVAL);
                    woken.wait(timeout);
                }
            })?;
        Ok(Self { park, stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_encryption::testing::{Directory, Policy};
    use crate::channel_encryption::{ChannelEncryption, ChannelEncryptionConfig, InboundChannelGovernance};
    use crate::collab_core::BrokerChannels;
    use channel_crypto::VaultKey;
    use ed25519_dalek::SigningKey;
    use neighbour_wire::NeighbourMsg;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    /// A transport that counts the passes its pump makes, and carries nothing.
    #[derive(Default)]
    struct Passes(AtomicUsize);

    impl Passes {
        fn taken(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl GovernanceTransport for Passes {
        fn publish(&self, _message: NeighbourMsg) -> bool {
            false
        }

        fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        }
    }

    /// This device's channel service, its state under `dir`, settling on the
    /// thread `name`; `transport` is what its passes travel over.
    fn settled(
        name: &str,
        dir: &Path,
        transport: &Arc<Passes>,
    ) -> (ChannelKeyAccess, Arc<GovernancePark>, GovernancePump) {
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: dir.join("channels"),
                vault: VaultKey::new([1; 32]),
                device_key: SigningKey::from_bytes(&[1; 32]),
            },
            Box::new(Directory {
                policy: Policy::default(),
            }),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .unwrap();
        let park = GovernancePark::new();
        let access = ChannelKeyAccess::new(service);
        let pump = GovernancePump::spawn(
            name,
            Arc::clone(&park),
            access.clone(),
            Arc::clone(transport) as Arc<dyn GovernanceTransport + Send + Sync>,
        )
        .unwrap();
        (access, park, pump)
    }

    #[test]
    fn a_poke_that_arrives_before_the_pump_parks_is_kept() {
        let park = GovernancePark::new();
        // Governance queued just before the pump parks: a park that dropped
        // the poke would leave it waiting for good.
        park.poke();
        let (woken, woke) = std::sync::mpsc::channel();
        std::thread::spawn({
            let park = Arc::clone(&park);
            move || {
                park.wait(None);
                let _ = woken.send(());
            }
        });
        tilo_test_env::wait_until("the kept poke wakes the park", || woke.try_recv().is_ok());
    }

    #[test]
    fn a_parked_pump_takes_the_governance_a_poke_brings() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Passes::default());
        let (_access, park, _pump) = settled("broker-pumped", dir.path(), &transport);
        tilo_test_env::wait_until("the pump's first pass", || transport.taken() > 0);
        let before = transport.taken();
        // Parked on its own, until governance arrives.
        park.poke();
        tilo_test_env::wait_until("the poked pump's pass", || transport.taken() > before);
    }

    /// Linux only: it reads the thread's scheduling from `/proc/self/task`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pump_with_no_live_channel_is_never_scheduled() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Passes::default());
        let (access, park, _pump) = settled("broker-idle", dir.path(), &transport);
        let tid = tilo_test_env::idle::named_thread("broker-idle");
        assert!(!access.has_live_channels());
        tilo_test_env::wait_until("the pump's first pass", || transport.taken() > 0);
        let before = transport.taken();
        tilo_test_env::idle::assert_stays_asleep(&tid, "channel pump");
        park.poke();
        tilo_test_env::wait_until("the poked pump's pass", || transport.taken() > before);
    }
}
