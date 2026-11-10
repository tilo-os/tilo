//! The thread that settles a device's channel groups.
//!
//! [`ChannelKeyAccess::pump`] runs one pass of the channel protocol: it takes
//! what the workspace transport carries, moves every group to the access
//! policy, keeps waiting devices' requests alive, and publishes what the
//! transport did not take. A device runs it on a thread of its own
//! ([`GovernancePump`]), which sleeps between passes until something happens
//! ([`GovernancePark`]): a frame or a poke from the host, or the deadline the
//! pass named for work no message brings (a request's backoff, a channel's
//! founding, a policy expiry).

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[cfg(not(target_family = "wasm"))]
use std::io;
use std::sync::atomic::AtomicBool;
#[cfg(not(target_family = "wasm"))]
use std::sync::atomic::Ordering;

#[cfg(not(target_family = "wasm"))]
use crate::encryption::{ChannelKeyAccess, GovernanceTransport};

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

#[cfg_attr(
    target_family = "wasm",
    allow(dead_code, reason = "a page waits on its own loop, never on a thread")
)]
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
        // The poke is spent: the pass this one waits for does what it asked
        // for, and the next wait sleeps again.
        *pending = false;
    }
}

/// A device's channel service, settling on a thread of its own; stops when
/// dropped.
#[cfg_attr(
    target_family = "wasm",
    allow(dead_code, reason = "a page drives the pump from its own loop")
)]
pub struct GovernancePump {
    park: Arc<GovernancePark>,
    stop: Arc<AtomicBool>,
}

impl Drop for GovernancePump {
    fn drop(&mut self) {
        #[cfg(not(target_family = "wasm"))]
        self.stop.store(true, Ordering::Relaxed);
        self.park.poke();
    }
}

#[cfg_attr(
    target_family = "wasm",
    allow(dead_code, reason = "a page drives the pump from its own loop")
)]
impl GovernancePump {
    /// Run `access`'s governance over `transport` on a thread named `name`,
    /// and `after_pass` after every pass: the host's own work that settles beside
    /// the channels (a session tells the collaboration plane its rosters
    /// there).
    ///
    /// Between passes the thread sleeps until it is poked
    /// ([`GovernancePark::poke`]) or until the deadline the pass named
    /// ([`ChannelKeyAccess::pump_until_due`]) is due, whichever comes first;
    /// with nothing due it waits for a poke alone. `access`'s
    /// [`ChannelKeyAccess::with_wake`] should be `park`'s own poke.
    ///
    /// A host with threads runs this; a host with one thread drives
    /// [`ChannelKeyAccess::pump`] from its own loop and never spawns.
    #[cfg(not(target_family = "wasm"))]
    pub fn spawn(
        name: &str,
        park: Arc<GovernancePark>,
        access: Option<ChannelKeyAccess>,
        transport: Arc<dyn GovernanceTransport + Send + Sync>,
        mut after_pass: impl FnMut() + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let woken = Arc::clone(&park);
        let wake = Arc::clone(&park);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let policy_watch =
                    tilo_identity_host::policy_change::Watch::new(move || wake.poke());
                while !stopped.load(Ordering::Relaxed) {
                    let mut due = access
                        .as_ref()
                        .and_then(|access| access.pump_until_due(&*transport));
                    after_pass();
                    if let Some(access) = &access
                        && !access.arm_policy_watch(&policy_watch)
                    {
                        // A policy change arrived that the arming will not
                        // wake for: settle it now.
                        due = Some(Duration::ZERO);
                    }
                    woken.wait(due);
                }
            })?;
        Ok(Self { park, stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::{
        ChannelAccess, ChannelEncryption, ChannelEncryptionConfig, ChannelsDir,
        InboundChannelGovernance,
    };
    use crate::gates::Gates;
    use crate::testing::{Announcements, Directory, Endpoint, Policy, Workspace, key};
    use channel_crypto::VaultKey;
    use collab_types::ChannelId;
    use neighbour_wire::NeighbourMsg;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use tilo_desktop_model::WorkspaceId;

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
                peers: Arc::new(ChannelsDir::new(dir.join("channels"))),
                vault: VaultKey::new([1; 32]),
                device_key: key(1),
            },
            Box::new(Directory {
                policy: Policy::default(),
            }),
            Arc::new(Gates::default()),
        )
        .unwrap();
        let park = GovernancePark::new();
        let access = ChannelKeyAccess::new(service);
        let pump = GovernancePump::spawn(
            name,
            Arc::clone(&park),
            Some(access.clone()),
            Arc::clone(transport) as Arc<dyn GovernanceTransport + Send + Sync>,
            || {},
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

    /// A poke wakes the pump once: the pass it woke for does its work, and
    /// the pump sleeps again rather than running flat out from then on.
    #[test]
    fn a_poke_is_spent_by_the_pass_it_wakes() {
        let park = GovernancePark::new();
        park.poke();
        let started = Instant::now();
        park.wait(Some(Duration::from_secs(30)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a poke waiting before the sleep returns at once"
        );
        // The poke was spent: the next sleep lasts as long as it was told.
        let started = Instant::now();
        park.wait(Some(Duration::from_millis(60)));
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "a spent poke does not wake the next pass"
        );
        // A poke from elsewhere wakes a pump that is asleep.
        let woken = Arc::clone(&park);
        let poker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            woken.poke();
        });
        let started = Instant::now();
        park.wait(Some(Duration::from_secs(30)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a poke wakes the pass that was asleep"
        );
        poker.join().expect("the poker finishes");
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
        let (_access, park, _pump) = settled("broker-idle", dir.path(), &transport);
        tilo_test_env::wait_until("the pump's first pass", || transport.taken() > 0);
        // Named once it runs: the first pass is proof the thread is there.
        let tid = tilo_test_env::idle::named_thread("broker-idle");
        let before = transport.taken();
        tilo_test_env::idle::assert_stays_asleep(&tid, "channel pump");
        park.poke();
        tilo_test_env::wait_until("the poked pump's pass", || transport.taken() > before);
    }

    /// A device that joined a channel of a workspace whose directory
    /// announces its changes, where nothing expires: a live channel with
    /// nothing due, however long it stays open.
    fn live_member() -> (ChannelEncryption, Endpoint) {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::readers([1, 2]);
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
            std::sync::atomic::AtomicU64::new(1),
        )));
        let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let open = |id: usize, seed: u8, dir: &Path| {
            let service = ChannelEncryption::open(
                ChannelEncryptionConfig {
                    root: dir.join("channels"),
                    peers: Arc::new(ChannelsDir::new(dir.join("channels"))),
                    vault: VaultKey::new([seed; 32]),
                    device_key: key(seed),
                },
                Box::new(Directory {
                    policy: policy.clone(),
                }),
                Arc::new(Gates::default()),
            )
            .unwrap();
            (service, Endpoint::join(&workspace, workspace_id, id))
        };
        let (mut owner, owner_endpoint) = open(0, 1, owner_dir.path());
        let (mut member, member_endpoint) = open(1, 2, member_dir.path());
        let mut now = Instant::now();
        owner.access(workspace_id, channel, now);
        member.access(workspace_id, channel, now);
        for _ in 0..40 {
            now = now.checked_add(Duration::from_millis(250)).unwrap();
            owner.pump(&owner_endpoint, now, 1_000);
            member.pump(&member_endpoint, now, 1_000);
        }
        assert!(
            matches!(
                member.access(workspace_id, channel, now),
                ChannelAccess::Ready(_)
            ),
            "the member joined the channel"
        );
        assert_eq!(
            member.next_pump_deadline(now),
            None,
            "a joined channel whose directory announces has nothing due"
        );
        (member, member_endpoint)
    }

    /// A live channel with nothing due does not wake the pump: it sleeps
    /// until a frame, an announcement or the deadline a later pass names.
    ///
    /// Linux only: it reads the thread's scheduling from `/proc/self/task`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pump_with_a_live_channel_sleeps_until_something_happens() {
        let (member, endpoint) = live_member();
        let park = GovernancePark::new();
        let poke = Arc::clone(&park);
        let access = ChannelKeyAccess::with_wake(member, Arc::new(move || poke.poke()));
        let _pump = GovernancePump::spawn(
            "broker-live",
            park,
            Some(access),
            Arc::new(endpoint) as Arc<dyn GovernanceTransport + Send + Sync>,
            || {},
        )
        .unwrap();
        // Named once it runs, which is before its first pass reads the
        // transport.
        let tid = tilo_test_env::wait_for("the live pump's thread", || {
            tilo_test_env::idle::live_named("broker-live")
        });
        tilo_test_env::wait_until("the pump to settle", || tilo_test_env::idle::blocked(&tid));
        let before = tilo_test_env::idle::asleep_runs(&tid);
        // Longer than the second a live channel used to tick on.
        std::thread::sleep(Duration::from_millis(2_500));
        assert_eq!(
            tilo_test_env::idle::asleep_runs(&tid).saturating_sub(before),
            0,
            "a live channel with nothing due woke the pump"
        );
    }
}
