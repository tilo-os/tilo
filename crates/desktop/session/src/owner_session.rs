//! The lock follows the person across their own devices. Each device says, in the record of their
//! devices in their own workspace, whether it is locked, when the person last locked or unlocked it
//! themselves, their last input on it and whether they are using it ([`OwnerSession`]), written by
//! the device's daemon and followed here through it ([`OwnerDevices`]). From those:
//!
//! - Locking a device locks the person's other devices, unless they used one
//!   after that.
//! - Unlocking a device unlocks the others on the same network that locked
//!   only because it did, if they were in use when they locked; one left idle
//!   or away stays locked.
//! - A device does not lock for idleness while the person uses another of
//!   their devices on the same network: its idle time counts from their last
//!   input there.
//!
//! A device says its session only when it changes: a lock, an unlock, the person starting to use it
//! or leaving it for [`USING_FOR_MS`]. Typing writes nothing, so the record moves a few times an hour.
//! Which devices are online and on a common network is live, from the fabric.
//!
//! The idle timer's lock (and the lock before an idle sleep) stays on its
//! device: it neither spreads nor is undone by another device, so a device set
//! never to lock on its own is not locked by another one going idle. A lock
//! followed from another device does not spread further either. A lock a lock
//! client holds is the client's, and is not followed either. Unlocking always
//! takes the person's credential on one of their devices.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use tilo_compositor::Error;
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    DEVICES, Devices, KeyKind, OwnerSession,
};
use tilo_workspace_service::workspaces::person_workspace::wake;

use crate::Session;
use crate::followed_resource::FollowedResource;
use crate::platform::fabric::DeviceId;

/// How long after their last input the person counts as using a device. Their other devices hear
/// of it as they start and as they stop, never as they type.
pub const USING_FOR_MS: u64 = 60_000;

/// How long a device waits before it says something of itself again after its daemon did not
/// take it, the first time: twice as long after each refusal in a row, up to [`SAY_AT_MOST_EVERY`].
const SAY_AGAIN: Duration = Duration::from_secs(5);
const SAY_AT_MOST_EVERY: Duration = Duration::from_secs(300);

/// How the daemon answered what this device last said of its session, as the loop has not heard
/// yet: nothing new, taken, or not taken.
const UNHEARD: u8 = 0;
const TAKEN: u8 = 1;
const MISSED: u8 = 2;

/// What this device waits to say of itself again: a fact by its name, or its session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Saying {
    Fact(String),
    Session,
}

/// When one thing this device says of itself may be said again, and how long it waits after the
/// next refusal.
#[derive(Clone, Copy)]
struct Quiet {
    until: Instant,
    next: Duration,
}

/// When each thing this device says of itself may be said again: one write waiting for the daemon
/// holds back that one, never what else the device has to say.
#[derive(Default)]
struct Quiets(std::collections::BTreeMap<Saying, Quiet>);

impl Quiets {
    /// Whether `saying` waits at `now`: the daemon was asked it a moment ago, or did not take it.
    fn waits(&self, saying: &Saying, now: Instant) -> bool {
        self.0.get(saying).is_some_and(|quiet| now < quiet.until)
    }

    /// Hold `saying` back from `now`: [`SAY_AGAIN`] the first time, twice as long each time in a
    /// row after, up to [`SAY_AT_MOST_EVERY`], so a daemon that keeps refusing it is not asked on a
    /// clock.
    fn hush(&mut self, saying: Saying, now: Instant) {
        let wait = self
            .0
            .get(&saying)
            .map_or(SAY_AGAIN, |quiet| quiet.next)
            .min(SAY_AT_MOST_EVERY);
        if let Some(until) = now.checked_add(wait) {
            self.0.insert(
                saying,
                Quiet {
                    until,
                    next: wait.saturating_mul(2),
                },
            );
        }
    }

    /// `saying` reached the record: the next change of it is said at once.
    fn heard(&mut self, saying: &Saying) {
        self.0.remove(saying);
    }
}

/// What this device does to follow the person's other devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    Lock,
    Unlock,
}

/// This device's side of [`OwnerSession`]: what it says and what it has already followed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionFollow {
    /// When the person last locked or unlocked this device themselves.
    changed_unix_ms: u64,
    /// The latest change on another device this device has acted on, so an old lock elsewhere is
    /// not followed twice.
    acted_unix_ms: u64,
    /// Locked because another device was locked at this time, and whether this device was in use
    /// then.
    followed: Option<Followed>,
    /// Whether the lock this device has, if any, is the person's own here and not the idle timer's
    /// or another device's.
    locked_by_person: bool,
    /// What this device said last, which it does not say again.
    said: Option<OwnerSession>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Followed {
    since_unix_ms: u64,
    in_use: bool,
}

/// `ms` as the record keeps a time.
fn record_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// A time the record keeps, as ms since the epoch.
fn unix_ms(ms: i64) -> u64 {
    u64::try_from(ms).unwrap_or(0)
}

impl SessionFollow {
    /// The person locked or unlocked this device themselves.
    pub fn changed_by_person(&mut self, now_unix_ms: u64) {
        self.changed_unix_ms = now_unix_ms;
        self.followed = None;
        self.locked_by_person = true;
    }

    /// The idle timer locked this device: no other device's unlock undoes it,
    /// and no other device follows it.
    pub fn locked_by_idle(&mut self) {
        self.followed = None;
        self.locked_by_person = false;
    }

    /// Whether the lock this device has is one it followed from another of the person's devices:
    /// that lock is not the person's own here, and the session raises it without counting it again.
    #[must_use]
    pub fn followed_lock(&self) -> bool {
        self.followed.is_some()
    }

    /// What this device says of itself, given its lock, the person's last input and whether they are
    /// using it. A lock that is not the person's own here says no change for the others to follow.
    #[must_use]
    pub fn presence(&self, locked: bool, input_unix_ms: u64, using: bool) -> OwnerSession {
        OwnerSession {
            locked,
            changed_ms: if locked && !self.locked_by_person {
                0
            } else {
                record_ms(self.changed_unix_ms)
            },
            active_ms: record_ms(input_unix_ms),
            using,
        }
    }

    /// What this device has to say now, when it differs from what it said last in anything but the
    /// time of the last input: that moves with every key and is said only with the rest.
    pub fn saying(
        &mut self,
        locked: bool,
        input_unix_ms: u64,
        using: bool,
    ) -> Option<OwnerSession> {
        let now = self.presence(locked, input_unix_ms, using);
        let same = self.said.is_some_and(|said| {
            said.locked == now.locked
                && said.changed_ms == now.changed_ms
                && said.using == now.using
        });
        if same {
            return None;
        }
        self.said = Some(now);
        Some(now)
    }

    /// What this device said did not reach the record: it says it again.
    pub fn unsaid(&mut self) {
        self.said = None;
    }

    /// Follow the person's other devices' sessions, each with whether it shares a network with this
    /// one: lock after a newer lock elsewhere this device was not used since, unlock when a device
    /// nearby is unlocked after the lock it followed and this one was in use when it locked.
    pub fn decide(
        &mut self,
        locked: bool,
        input_unix_ms: u64,
        in_use: bool,
        others: impl IntoIterator<Item = (OwnerSession, bool)>,
    ) -> Option<Follow> {
        let others = others.into_iter().collect::<Vec<_>>();
        let latest = |want_locked: bool, nearby_only: bool| {
            others
                .iter()
                .filter(|(session, nearby)| {
                    session.locked == want_locked && (*nearby || !nearby_only)
                })
                .map(|(session, _)| unix_ms(session.changed_ms))
                .max()
        };
        if !locked {
            let locked_at = latest(true, false)?;
            if locked_at
                <= self
                    .changed_unix_ms
                    .max(self.acted_unix_ms)
                    .max(input_unix_ms)
            {
                return None;
            }
            self.acted_unix_ms = locked_at;
            self.followed = Some(Followed {
                since_unix_ms: locked_at,
                in_use,
            });
            self.locked_by_person = false;
            return Some(Follow::Lock);
        }
        let followed = self.followed?;
        let unlocked_at = latest(false, true)?;
        if unlocked_at <= followed.since_unix_ms {
            return None;
        }
        self.acted_unix_ms = unlocked_at;
        self.followed = None;
        followed.in_use.then_some(Follow::Unlock)
    }
}

/// Another of the person's devices, as this one follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Other {
    /// How their session on it stands, as it said last.
    pub session: OwnerSession,
    /// It is on a session with this device now.
    pub online: bool,
    /// It was last seen on a network this device is on.
    pub nearby: bool,
}

/// The person's latest use of another of their devices that is here, shares a network with this
/// one and is unlocked, at `now_unix_ms`: one they are using is in use now. Idle time here counts
/// from it.
#[must_use]
pub fn latest_use_nearby(now_unix_ms: u64, others: &[Other]) -> Option<u64> {
    others
        .iter()
        .filter(|other| other.online && other.nearby && !other.session.locked)
        .map(|other| {
            if other.session.using {
                now_unix_ms
            } else {
                unix_ms(other.session.active_ms)
            }
        })
        .max()
}

/// The record of the person's devices in their own workspace, followed through the device's daemon:
/// where this device reads how the person's session stands on their others, and says its own.
/// The other devices' sessions as the record last read them, by device, and the reading they are
/// of.
type Others = Option<(u64, Vec<(DeviceId, OwnerSession, Vec<wake::WakeRoute>)>)>;

pub(crate) struct OwnerDevices {
    record: FollowedResource<Devices>,
    cert: DeviceCert,
    /// The other devices' sessions as the record last read, by device, and the reading they are of.
    others: RefCell<Others>,
    /// When each thing this device says of itself may be said again.
    quiet: RefCell<Quiets>,
    /// The daemon did not take what this device last said of its session: said off the loop, so
    /// the loop hears it on its next pass ([`Self::take_missed`]).
    missed: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// What this device is ([`crate::platform::fabric::DeviceAbout`]), postcard in hex, and its
    /// certificate the same way: what it keeps of itself in the record, through its daemon too.
    about: Option<(String, String)>,
}

impl OwnerDevices {
    /// Follow the record of the devices of `cert`'s person, as this device of theirs.
    pub(crate) fn follow(daemon: &DaemonShell, cert: DeviceCert, wake: tilo_service::Wake) -> Self {
        let person = cert.user_public_key;
        let own = tilo_server_protocol::servers::person_workspace_id(&person);
        let member = tilo_member_types::PersonId::from_root(person);
        let record =
            FollowedResource::follow(daemon, (&own, DEVICES.0, DEVICES.1), member.as_str(), wake);
        let about = crate::platform::fabric::about(&cert.device_label);
        let about = postcard::to_allocvec(&about)
            .and_then(|about| Ok((about, postcard::to_allocvec(&cert)?)))
            .map(|(about, certificate)| (hex::encode(about), hex::encode(certificate)))
            .inspect_err(|error| tracing::warn!(%error, "what this device is does not encode"))
            .ok();
        Self {
            record,
            cert,
            others: RefCell::default(),
            quiet: RefCell::default(),
            missed: std::sync::Arc::default(),
            about,
        }
    }

    /// The record followed, which the activity reads its history from too, and this device's key
    /// (hex) in it.
    pub(crate) fn record(&self) -> (FollowedResource<Devices>, String) {
        (
            self.record.clone(),
            hex::encode(self.cert.device_public_key),
        )
    }

    /// This device's certificate, as the record keeps it.
    pub(crate) fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// Who this device is: its person's root and its own key (hex).
    pub(crate) fn who(&self) -> ([u8; 32], String) {
        (
            self.cert.user_public_key,
            hex::encode(self.cert.device_public_key),
        )
    }

    /// The record as it last read.
    pub(crate) fn devices(&self) -> std::rc::Rc<Devices> {
        self.record.get()
    }

    /// A number that moves each time the record does.
    pub(crate) fn revision(&self) -> u64 {
        self.record.revision()
    }

    /// Keep what this device is in the record, once it read the record and found it says something
    /// else: what the person's other devices list it as.
    fn keep_about(&self) {
        if let Some((about, _)) = &self.about {
            self.keep_own(crate::platform::fabric::ABOUT, about);
        }
    }

    /// Keep `value` (hex) as this device's fact `name` in the record, once it read the record and
    /// found it says something else there; a daemon that did not take it is not asked again for a
    /// few seconds.
    pub(crate) fn keep_own(&self, name: &str, value: &str) {
        let Some((_, certificate)) = &self.about else {
            return;
        };
        let saying = Saying::Fact(name.to_owned());
        if self.record.held().is_none() || self.quiet(&saying) {
            return;
        }
        let key = hex::encode(self.cert.device_public_key);
        let kept = self
            .record
            .get()
            .devices
            .get(&tilo_resource::Id::from_raw(key.clone()))
            .is_some_and(|kept| {
                &kept.certificate == certificate
                    && kept.facts.get(name).map(String::as_str) == Some(value)
            });
        if kept {
            self.quiet.borrow_mut().heard(&saying);
            return;
        }
        let keep =
            Devices::KEEP_FACT.with((key, certificate.clone(), name.to_owned(), value.to_owned()));
        // Said off the loop: the follow shows it once the daemon wrote it, and until then, or
        // when the daemon did not take it, it is not said again for a while.
        self.hush(saying);
        let name = name.to_owned();
        self.record.invoke_unwaited(&keep, move |kept| {
            if let Err(error) = kept {
                tracing::warn!(%error, name, "what this device says of itself reaches the person's devices later");
            }
        });
    }

    /// Ask the person's devices to wake the device `name` names, by its key or its label. The ask
    /// goes to the daemon without the session waiting on it; the record says how it went, and the
    /// person hears that as the wake ends ([`crate::wake::Wake::follow`]).
    ///
    /// # Errors
    /// Why it cannot be woken, in the person's words.
    pub(crate) fn wake(&self, name: &str) -> Result<(), String> {
        let (person, local) = self.who();
        let now = i64::try_from(tilo_clock::unix_now_ns()).unwrap_or(i64::MAX);
        let ask = wake::asking(&self.record.get(), &person, &local, name, now)
            .map_err(|refusal| refusal.to_string())?;
        let name = name.to_owned();
        self.record.invoke_unwaited(&ask, move |asked| {
            if let Err(error) = asked {
                tracing::warn!(%error, device = name, "the wake did not reach this device's daemon");
            }
        });
        Ok(())
    }

    /// The interfaces this device keeps in the record, that it can be woken through.
    fn own_routes(&self) -> Vec<wake::WakeRoute> {
        self.record
            .get()
            .devices
            .get(&tilo_resource::Id::from_raw(hex::encode(
                self.cert.device_public_key,
            )))
            .map(wake::routes)
            .unwrap_or_default()
    }

    /// The person's other devices the record keeps, whose certificates their root signed and did
    /// not take back, how their session stands on each and the interfaces each keeps.
    fn others(&self) -> Vec<(DeviceId, OwnerSession, Vec<wake::WakeRoute>)> {
        let revision = self.record.revision();
        if let Some((at, others)) = self.others.borrow().as_ref()
            && *at == revision
        {
            return others.clone();
        }
        let person = self.cert.user_public_key;
        let others: Vec<(DeviceId, OwnerSession, Vec<wake::WakeRoute>)> = self
            .record
            .get()
            .listed(&person)
            .into_iter()
            .filter(|listed| {
                listed.cert.device_public_key != self.cert.device_public_key
                    && listed.device.kind == KeyKind::Device
                    && !listed.device.revoked(&listed.cert)
            })
            .filter_map(|listed| {
                Some((
                    DeviceId::new(listed.key).ok()?,
                    listed.device.session,
                    wake::routes(&listed.device),
                ))
            })
            .collect();
        *self.others.borrow_mut() = Some((revision, others.clone()));
        others
    }

    /// Say `session` as this device's: through its daemon, which writes it into the record, off
    /// the loop. Whether it was said; one the daemon did not take is heard on a later pass
    /// ([`Self::take_missed`]), and the daemon is not asked again for a few seconds.
    fn say(&self, session: OwnerSession) -> bool {
        if self.quiet(&Saying::Session) {
            return false;
        }
        let missed = std::sync::Arc::clone(&self.missed);
        self.record.invoke_unwaited(
            &Devices::saying_session(&self.cert.device_public_key, session),
            move |said| {
                let heard = match said {
                    Ok(()) => TAKEN,
                    Err(error) => {
                        tracing::warn!(%error, "this device's session reaches the person's devices later");
                        MISSED
                    }
                };
                missed.store(heard, std::sync::atomic::Ordering::Relaxed);
            },
        );
        true
    }

    /// Whether the daemon did not take what this device last said of its session: it is said
    /// again once the daemon may be asked again.
    fn take_missed(&self) -> bool {
        match self
            .missed
            .swap(UNHEARD, std::sync::atomic::Ordering::Relaxed)
        {
            MISSED => {
                self.hush(Saying::Session);
                true
            }
            TAKEN => {
                self.quiet.borrow_mut().heard(&Saying::Session);
                false
            }
            _ => false,
        }
    }

    /// Whether `saying` waits now.
    fn quiet(&self, saying: &Saying) -> bool {
        self.quiet.borrow().waits(saying, Instant::now())
    }

    /// Hold `saying` back from now.
    fn hush(&self, saying: Saying) {
        self.quiet.borrow_mut().hush(saying, Instant::now());
    }
}

impl Session {
    /// Follow the person's session on their other devices, every pass: idle time here counts from
    /// their use of one nearby, a lock or unlock there is followed on the pass that brings it, and
    /// this device says its own session to them when it changed.
    pub(crate) fn follow_owner_session(&mut self) -> Result<(), Error> {
        let Some(devices) = self.shared.owner_devices.as_ref() else {
            return Ok(());
        };
        devices.keep_about();
        let now = Instant::now();
        let now_ms = tilo_clock::unix_now_ms();
        let since_input =
            u64::try_from(self.compositor.input_idle().as_millis()).unwrap_or(u64::MAX);
        let input_ms = now_ms.saturating_sub(since_input);
        let using = since_input < USING_FOR_MS;
        let locked = self.compositor.locked();
        let in_use = self.compositor.in_use();
        // Which of the devices are online is live, from the fabric: without it none is, and only a
        // lock is followed. Which are on a network with this one is what each keeps in the record.
        let online = self
            .shared
            .fabric
            .as_ref()
            .map(|fabric| fabric.online_device_ids())
            .unwrap_or_default();
        let own = devices.own_routes();
        let others: Vec<Other> = devices
            .others()
            .into_iter()
            .map(|(id, session, routes)| {
                let online = online.contains(&id);
                Other {
                    session,
                    online,
                    nearby: online && wake::shares_network(&own, &routes),
                }
            })
            .collect();
        // Idle time here counts from the person's last input on a device of theirs nearby: neither
        // locks while they are on the other.
        if let Some(used) = latest_use_nearby(now_ms, &others)
            && let Some(at) = now.checked_sub(Duration::from_millis(now_ms.saturating_sub(used)))
        {
            self.compositor.note_use_elsewhere(at);
        }
        let followed = self.shared.owner_session.decide(
            locked,
            input_ms,
            in_use,
            others.iter().map(|other| (other.session, other.nearby)),
        );
        match followed {
            Some(Follow::Lock) => self.lock_by_person()?,
            // The person unlocked on one of their other devices: this device unlocks the way it
            // does when they type the password here.
            Some(Follow::Unlock) => self.unlock_by_person()?,
            None => {}
        }
        // What this device says of itself is what it is now: a lock or unlock followed this pass is
        // said on the same one.
        let locked = self.compositor.locked();
        if self
            .shared
            .owner_devices
            .as_ref()
            .is_some_and(OwnerDevices::take_missed)
        {
            self.shared.owner_session.unsaid();
        }
        if let Some(said) = self.shared.owner_session.saying(locked, input_ms, using)
            && let Some(devices) = self.shared.owner_devices.as_ref()
            && !devices.say(said)
        {
            self.shared.owner_session.unsaid();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One thing this device says of itself waiting for its daemon holds back that one alone: its
    /// other facts and its session are said at once, and a daemon that keeps refusing one is asked
    /// it ever less often, never on a clock.
    #[test]
    fn a_write_waiting_for_the_daemon_holds_back_only_itself() {
        let now = Instant::now();
        let displays = Saying::Fact("displays".to_owned());
        let mut quiet = Quiets::default();
        quiet.hush(displays.clone(), now);
        assert!(quiet.waits(&displays, now));
        assert!(!quiet.waits(&Saying::Fact("about".to_owned()), now));
        assert!(!quiet.waits(&Saying::Session, now));
        assert!(!quiet.waits(&displays, now + SAY_AGAIN));

        let mut waited = Vec::new();
        for _ in 0..10 {
            quiet.hush(displays.clone(), now);
            let until = (1..=600)
                .map(|seconds| now + Duration::from_secs(seconds))
                .find(|at| !quiet.waits(&displays, *at))
                .expect("it is said again within ten minutes");
            waited.push(until.duration_since(now).as_secs());
        }
        assert_eq!(waited, [10, 20, 40, 80, 160, 300, 300, 300, 300, 300]);

        quiet.heard(&displays);
        quiet.hush(displays.clone(), now);
        assert!(
            !quiet.waits(&displays, now + SAY_AGAIN),
            "a fact heard starts over"
        );
    }

    /// A device on the same network.
    fn session(locked: bool, changed_ms: i64) -> (OwnerSession, bool) {
        let session = OwnerSession {
            locked,
            changed_ms,
            ..OwnerSession::default()
        };
        (session, true)
    }

    tilo_test_env::scenario! {
        fn a_lock_elsewhere_is_followed_only_where_the_person_left_the_device;

        phase a_lock_elsewhere_locks_a_device_not_used_since_once;
        {
            let mut follow = SessionFollow::default();
            let laptop = session(true, 2_000);
            assert_eq!(
                follow.decide(false, 1_000, true, [laptop]),
                Some(Follow::Lock)
            );
            // Unlocked here by the person afterwards: the same lock is not followed again.
            follow.changed_by_person(3_000);
            assert_eq!(follow.decide(false, 1_000, true, [laptop]), None);
        }

        phase a_device_used_after_the_lock_elsewhere_stays_unlocked;
        {
            let mut follow = SessionFollow::default();
            assert_eq!(
                follow.decide(false, 2_500, true, [session(true, 2_000)]),
                None
            );
        }

        phase an_unlock_elsewhere_undoes_only_a_followed_lock_of_a_device_in_use;
        {
            let mut follow = SessionFollow::default();
            assert_eq!(
                follow.decide(false, 1_000, true, [session(true, 2_000)]),
                Some(Follow::Lock)
            );
            assert_eq!(
                follow.decide(true, 1_000, true, [session(true, 2_000)]),
                None
            );
            assert_eq!(
                follow.decide(true, 1_000, true, [session(false, 3_000)]),
                Some(Follow::Unlock)
            );

            // Idle when it followed the lock: the unlock elsewhere leaves it locked.
            let mut idle = SessionFollow::default();
            idle.decide(false, 1_000, false, [session(true, 2_000)]);
            assert_eq!(
                idle.decide(true, 1_000, false, [session(false, 3_000)]),
                None
            );

            // Locked by its own idle timer or by the person here: never unlocked from elsewhere.
            let mut own = SessionFollow::default();
            own.locked_by_idle();
            assert_eq!(own.decide(true, 1_000, true, [session(false, 3_000)]), None);
            own.changed_by_person(1_500);
            assert_eq!(own.decide(true, 1_000, true, [session(false, 3_000)]), None);
        }

        phase an_unlock_on_another_network_leaves_a_followed_lock;
        {
            let mut follow = SessionFollow::default();
            let (away_lock, _) = session(true, 2_000);
            assert_eq!(
                follow.decide(false, 1_000, true, [(away_lock, false)]),
                Some(Follow::Lock)
            );
            let (away_unlock, _) = session(false, 3_000);
            assert_eq!(
                follow.decide(true, 1_000, true, [(away_unlock, false)]),
                None
            );
        }

        phase an_older_lock_elsewhere_does_not_relock_after_an_unlock_was_followed;
        {
            let mut follow = SessionFollow::default();
            let earlier = session(true, 1_500);
            follow.decide(false, 1_000, true, [session(true, 2_000), earlier]);
            assert_eq!(
                follow.decide(true, 1_000, true, [session(false, 3_000), earlier]),
                Some(Follow::Unlock)
            );
            assert_eq!(
                follow.decide(false, 1_000, true, [session(false, 3_000), earlier]),
                None
            );
        }

        phase an_idle_lock_elsewhere_never_locks_a_device_set_never_to_lock;
        {
            // The person unlocks the desktop after last touching the laptop, then leaves both; the
            // desktop's idle timer locks it (or its idle sleep does, which counts as the idle timer's
            // lock).
            let mut desktop = SessionFollow::default();
            desktop.changed_by_person(2_000);
            desktop.locked_by_idle();
            let mut laptop = SessionFollow::default();
            assert_eq!(
                laptop.decide(false, 1_000, false, [published(&desktop, true, 2_000)]),
                None,
                "the desktop's idle lock is not followed"
            );

            // The person's own lock there still locks the laptop.
            desktop.changed_by_person(3_000);
            assert_eq!(
                laptop.decide(false, 1_000, false, [published(&desktop, true, 3_000)]),
                Some(Follow::Lock)
            );
        }

        phase a_followed_lock_does_not_spread_further;
        {
            let mut desktop = SessionFollow::default();
            desktop.changed_by_person(500);
            assert_eq!(
                desktop.decide(false, 400, false, [session(true, 2_000)]),
                Some(Follow::Lock)
            );
            // The tablet, last used before the desktop's last unlock, sees only the desktop: its lock
            // is not the person's, nothing to follow.
            let mut tablet = SessionFollow::default();
            assert_eq!(
                tablet.decide(false, 300, false, [published(&desktop, true, 400)]),
                None
            );
        }

        phase a_followed_lock_is_not_the_person_s_own_here;
        {
            let mut follow = SessionFollow::default();
            assert!(!follow.followed_lock());
            assert_eq!(
                follow.decide(false, 1_000, true, [session(true, 2_000)]),
                Some(Follow::Lock)
            );
            assert!(follow.followed_lock());
            assert_eq!(
                follow.presence(true, 1_000, false).changed_ms,
                0,
                "a followed lock is not said for others to follow"
            );
            follow.changed_by_person(3_000);
            assert!(!follow.followed_lock());
            assert_eq!(follow.presence(true, 1_000, false).changed_ms, 3_000);
        }
    }

    /// What `device` says, as seen on the same network.
    fn published(device: &SessionFollow, locked: bool, input_unix_ms: u64) -> (OwnerSession, bool) {
        (device.presence(locked, input_unix_ms, false), true)
    }

    #[test]
    fn a_device_says_its_session_only_when_it_changed() {
        let mut follow = SessionFollow::default();
        let first = follow
            .saying(false, 20_001, true)
            .expect("the first pass says it");
        assert_eq!(first.active_ms, 20_001);
        assert_eq!(
            follow.saying(false, 29_999, true),
            None,
            "typing on moves no record"
        );
        let idle = follow
            .saying(false, 29_999, false)
            .expect("leaving the device is said");
        assert!(!idle.using);
        assert_eq!(idle.active_ms, 29_999, "with the last input it had");
        follow.changed_by_person(40_000);
        assert!(
            follow.saying(true, 29_999, false).is_some(),
            "the person's lock is said"
        );
        follow.unsaid();
        assert!(
            follow.saying(true, 29_999, false).is_some(),
            "what did not reach the record is said again"
        );
    }

    #[test]
    fn a_device_in_use_nearby_keeps_this_one_in_use_now() {
        let other = |locked, using, online, nearby| Other {
            session: OwnerSession {
                locked,
                changed_ms: 0,
                active_ms: 5_000,
                using,
            },
            online,
            nearby,
        };
        assert_eq!(
            latest_use_nearby(9_000, &[other(false, true, true, true)]),
            Some(9_000)
        );
        assert_eq!(
            latest_use_nearby(9_000, &[other(false, false, true, true)]),
            Some(5_000),
            "a device left idle counts from its last input"
        );
        assert_eq!(
            latest_use_nearby(
                9_000,
                &[
                    other(true, true, true, true),
                    other(false, true, false, true),
                    other(false, true, true, false)
                ]
            ),
            None,
            "a locked device, one offline and one away keep nothing in use"
        );
    }
}
