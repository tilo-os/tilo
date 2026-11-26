//! The kill switch of the person's `NetworkManager` VPN profiles, in the
//! session (docs/device-vpn.md, "Profile kill switch").
//!
//! The person turns it on per profile in Settings
//! ([`settings::PROFILE_KILL_SWITCHES`]). This follows those choices and
//! `NetworkManager`'s active VPNs ([`tilo_nm::NmClient::watch_vpns`]),
//! decides with [`crate::profile::Intent`] which profiles are set to
//! connect and not connected, and asks the root helper to hold or lift the
//! block. A profile is no longer set to connect once the person turns it off:
//! `NetworkManager` says it went down on purpose, or Settings asks
//! ([`settings::PROFILE_RELEASE`]) for one that is not up.
//!
//! Which profiles are set to connect is kept in the settings
//! ([`settings::PROFILE_INTENT`]) before the helper is asked, and adopted
//! when the session starts: after a crash the block holds while the next
//! shell starts and is taken over, with the servers looked up before, and
//! only what the person turned off meanwhile is lifted.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use crate::profile::{Activity, Blocking, Intent, Switch, block_for};
use crate::protocol::{self, MAX_PROFILE_SERVERS, ProfileBlock, Request, Response};
use settings_store::device_vpn::{
    self as settings, ProfileBlockStatus, ProfileIntent, ProfileKillSwitch, ProfileRelease,
};
use tilo_nm::{ActiveState, VpnReading, VpnWatch};
use tokio::sync::watch;

const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// A block the helper could not hold is asked for again after this long,
/// doubling up to [`RETRY_MOST`], while it is still wanted.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MOST: Duration = Duration::from_secs(60);

/// How the helper answered the last ask, as far as the retry is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answered {
    /// The helper took the request, or the block it keeps is the one asked
    /// for: nothing more to ask.
    Held,
    /// The helper could not be asked, or failed: the block is still wanted,
    /// so it is asked for again after a while.
    Again,
    /// The helper refused: the answer is final for this session. A person it
    /// will not serve (a session that runs inside another desktop, whose
    /// helper belongs to the host) will not serve it on a retry either, and
    /// asking every few seconds only fills the log.
    Refused,
}
/// What the follower runs against: the helper it asks, and where
/// `NetworkManager`'s VPNs come from.
#[derive(Clone, Debug)]
pub struct Sources {
    pub helper: std::path::PathBuf,
    pub vpns: Vpns,
}

impl Sources {
    /// The machine's own helper and `NetworkManager` — for a session that owns
    /// the machine. A session that runs inside another desktop has neither
    /// ([`tilo_machine`]), and [`Vpns::None`] keeps the kill switch off
    /// for it.
    #[must_use]
    pub fn system() -> Self {
        if !tilo_machine::owns_the_machine() {
            return Self::hosted();
        }
        Self {
            helper: std::path::PathBuf::from(protocol::SOCKET_PATH),
            vpns: Vpns::System,
        }
    }

    /// No machine of this session's own: the machine's helper is never asked,
    /// and no `NetworkManager` is followed.
    #[must_use]
    pub fn hosted() -> Self {
        Self {
            helper: std::path::PathBuf::new(),
            vpns: Vpns::None,
        }
    }

    /// Whether this session reaches the machine's helper at all.
    #[must_use]
    pub fn reaches_the_helper(&self) -> bool {
        !matches!(self.vpns, Vpns::None)
    }
}

#[derive(Clone, Debug, Default)]
pub enum Vpns {
    /// The machine's `NetworkManager`, followed once some profile has its
    /// kill switch on.
    #[default]
    System,
    Fed(VpnWatch),
    /// None: this session runs inside another desktop, whose `NetworkManager`
    /// is not its to follow. Nothing blocks, and the machine's helper is never
    /// asked.
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Choices {
    switches: BTreeMap<String, ProfileKillSwitch>,
    release: Option<ProfileRelease>,
}

impl Choices {
    fn read(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            switches: settings::PROFILE_KILL_SWITCHES.from_section(section),
            release: settings::PROFILE_RELEASE.from_section(section),
        }
    }
}

/// Follow the person's VPN profiles and their kill switches from now on,
/// telling `publish` what blocks whenever that changes.
pub fn start(publish: impl Fn(ProfileBlockStatus) + Send + 'static) {
    start_with(publish, Sources::system());
}

/// [`start`], against `sources` instead of the machine's own helper and
/// `NetworkManager`.
pub fn start_with(publish: impl Fn(ProfileBlockStatus) + Send + 'static, sources: Sources) {
    let section = vpn_section();
    let kept = settings::PROFILE_INTENT.from_section(&section);
    let (sender, choices) = watch::channel(Choices::read(&section));
    // The choices are sent on whenever the settings change; nothing wakes between changes.
    let listener: std::sync::Arc<settings_store::Listener> = std::sync::Arc::new(move || {
        let choices = Choices::read(&vpn_section());
        sender.send_if_modified(|current| {
            let changed = *current != choices;
            if changed {
                *current = choices;
            }
            changed
        });
    });
    if let Some(source) = settings_store::source() {
        source.changes().listen(&listener);
    }
    let started = std::thread::Builder::new()
        .name("tilo-vpn-kill-switch".into())
        .spawn(move || {
            // Followed for as long as the kill switch runs.
            let _listener = listener;
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run(choices, kept, publish, sources)),
                Err(error) => {
                    tracing::warn!(%error, "could not start the VPN kill switch");
                }
            }
        });
    if let Err(error) = started {
        tracing::warn!(%error, "could not start the VPN kill switch");
    }
}

/// The VPN section of the settings as they are now.
fn vpn_section() -> serde_json::Map<String, serde_json::Value> {
    match settings_store::load().remove(settings::SECTION) {
        Some(serde_json::Value::Object(section)) => section,
        _ => serde_json::Map::new(),
    }
}

/// `NetworkManager`'s active VPNs, followed; `None` when it cannot be.
async fn watch_vpns() -> Option<VpnWatch> {
    let watched = async { tilo_nm::NmClient::system().await?.watch_vpns().await }.await;
    match watched {
        Ok(watch) => Some(watch),
        Err(error) => {
            tracing::warn!(%error, "could not follow NetworkManager's VPNs");
            None
        }
    }
}

/// Resolves when the followed reading changes; never while nothing is
/// followed. A follower that stopped is let go.
async fn vpns_change(vpns: &mut Option<VpnWatch>) {
    let Some(watch) = vpns.as_mut() else {
        return std::future::pending().await;
    };
    if watch.changed().await.is_none() {
        *vpns = None;
    }
}

fn keep(intent: &ProfileIntent) -> bool {
    let value = match serde_json::to_value(intent) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "could not keep the VPN profiles' intent");
            return false;
        }
    };
    let mut values = serde_json::Map::new();
    values.insert(settings::PROFILE_INTENT.name().to_owned(), value);
    match settings_store::update_section(settings::SECTION, values) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "could not keep the VPN profiles' intent");
            false
        }
    }
}

async fn run(
    mut choices: watch::Receiver<Choices>,
    kept: ProfileIntent,
    publish: impl Fn(ProfileBlockStatus),
    sources: Sources,
) {
    let mut follow = Follow::new(kept);
    let (mut vpns, follow_system) = match sources.vpns {
        Vpns::System => (None, true),
        Vpns::Fed(watch) => (Some(watch), false),
        // A session that runs inside another desktop has no machine of its
        // own: it neither follows that desktop's `NetworkManager` nor asks its
        // helper, whose routes belong to the person sitting at the machine.
        Vpns::None => return,
    };
    let mut retry: Option<Duration> = None;
    loop {
        let current = choices.borrow_and_update().clone();
        if follow_system && vpns.is_none() && !current.switches.is_empty() {
            vpns = watch_vpns().await;
        }
        let reading = vpns.as_ref().map(VpnWatch::current).unwrap_or_default();
        let before = follow.status.clone();
        let answered = follow.step(&current, &reading, &sources.helper, keep).await;
        if follow.status != before {
            publish(follow.status.clone());
        }
        retry = match answered {
            Answered::Held | Answered::Refused => None,
            Answered::Again => {
                Some(retry.map_or(RETRY_FIRST, |delay| delay.saturating_mul(2).min(RETRY_MOST)))
            }
        };
        tokio::select! {
            changed = choices.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = vpns_change(&mut vpns) => {}
            () = tokio::time::sleep(retry.unwrap_or(RETRY_MOST)), if retry.is_some() => {}
        }
    }
}

const fn activity(state: ActiveState) -> Option<Activity> {
    match state {
        ActiveState::Activating => Some(Activity::Activating),
        ActiveState::Activated => Some(Activity::Activated),
        ActiveState::Deactivating => Some(Activity::Deactivating),
        ActiveState::Deactivated | ActiveState::Unknown => None,
    }
}

#[derive(Debug)]
enum Applied {
    Unknown,
    Agreed(Option<ProfileBlock>),
    /// The helper refused this session: it will not change the machine's
    /// routes for a desktop it does not serve, and asking again will not
    /// change its answer.
    Refused,
}

struct Follow {
    intent: Intent,
    release_seen: Option<u64>,
    kept: ProfileIntent,
    ended_seen: BTreeMap<String, u64>,
    servers: BTreeMap<String, Vec<IpAddr>>,
    activities: BTreeMap<String, Activity>,
    applied: Applied,
    status: ProfileBlockStatus,
}

impl Follow {
    fn new(kept: ProfileIntent) -> Self {
        Self {
            intent: Intent::adopt(kept.connecting.keys().cloned()),
            release_seen: kept.release_seen,
            ended_seen: BTreeMap::new(),
            servers: kept
                .connecting
                .iter()
                .filter(|(_, servers)| !servers.is_empty())
                .map(|(profile, servers)| (profile.clone(), servers.clone()))
                .collect(),
            kept,
            activities: BTreeMap::new(),
            applied: Applied::Unknown,
            status: ProfileBlockStatus::default(),
        }
    }

    fn released(&mut self, choices: &Choices, reading: &VpnReading) -> Vec<String> {
        let mut released = Vec::new();
        if let Some(release) = &choices.release
            && self.release_seen != Some(release.id)
        {
            self.release_seen = Some(release.id);
            released.push(release.profile.clone());
        }
        for (profile, count) in &reading.ended_on_purpose {
            let seen = self.ended_seen.entry(profile.clone()).or_default();
            if *count != *seen {
                *seen = *count;
                released.push(profile.clone());
            }
        }
        released
    }

    /// The intent to keep: the profiles set to connect with their servers,
    /// and the last release acted on.
    fn intent(&self) -> ProfileIntent {
        ProfileIntent {
            connecting: self
                .intent
                .wanted()
                .map(|profile| {
                    let servers = self.servers.get(profile).cloned().unwrap_or_default();
                    (profile.clone(), servers)
                })
                .collect(),
            release_seen: self.release_seen,
        }
    }

    /// Follow one change, keeping the intent with `keep` before the helper
    /// is asked; whether the helper holds what is wanted.
    async fn step(
        &mut self,
        choices: &Choices,
        reading: &VpnReading,
        helper: &Path,
        keep: impl FnOnce(&ProfileIntent) -> bool,
    ) -> Answered {
        let released = self.released(choices, reading);
        let switches: BTreeMap<String, Switch> = choices
            .switches
            .iter()
            .map(|(profile, switch)| {
                (
                    profile.clone(),
                    Switch {
                        allow_local: switch.allow_local,
                    },
                )
            })
            .collect();
        let active: BTreeMap<String, Activity> = reading
            .active
            .iter()
            .filter_map(|vpn| Some((vpn.uuid.clone(), activity(vpn.state)?)))
            .collect();
        let blocking = self.intent.decide(&switches, &active, &released);
        self.look_up_servers(&switches, &active, blocking.as_ref())
            .await;
        self.activities = active;
        let intent = self.intent();
        if intent != self.kept && keep(&intent) {
            self.kept = intent;
        }
        let block = blocking
            .as_ref()
            .map(|blocking| block_for(blocking, &self.servers));
        let answered = self.apply(block, helper).await;
        let held = matches!(answered, Answered::Held);
        let status = ProfileBlockStatus {
            blocking: match (&blocking, held) {
                (Some(blocking), true) => blocking.profiles.clone(),
                _ => Vec::new(),
            },
            // A refusal is not a fault this session can fix: the machine's
            // helper serves its own desktop alone, and this one says so once
            // rather than reporting a failure it can never clear.
            failed: blocking.is_some() && answered == Answered::Again,
        };
        self.status = status;
        answered
    }

    async fn look_up_servers(
        &mut self,
        switches: &BTreeMap<String, Switch>,
        active: &BTreeMap<String, Activity>,
        blocking: Option<&Blocking>,
    ) {
        for profile in self.to_look_up(switches, active, blocking) {
            let found = servers_of(&profile).await;
            if !found.is_empty() {
                self.servers.insert(profile, found);
            }
        }
        self.servers
            .retain(|profile, _| switches.contains_key(profile));
    }

    /// The profiles whose servers to look up now, each once, in order: a switch turned on
    /// that starts connecting, one set to connect and connected without servers, and one
    /// that must block and has none yet. Looking up before a drop lets the block go up with
    /// its servers, instead of traffic leaving outside the tunnel while a lookup runs.
    fn to_look_up(
        &self,
        switches: &BTreeMap<String, Switch>,
        active: &BTreeMap<String, Activity>,
        blocking: Option<&Blocking>,
    ) -> Vec<String> {
        let starting = active.iter().filter(|(profile, activity)| {
            switches.contains_key(*profile)
                && **activity == Activity::Activating
                && self.activities.get(*profile) != Some(&Activity::Activating)
        });
        let mut wanted: Vec<String> = starting.map(|(profile, _)| profile.clone()).collect();
        let unknown = |profile: &&String| !self.servers.contains_key(*profile);
        wanted.extend(
            self.intent
                .wanted()
                .filter(|profile| active.get(*profile) == Some(&Activity::Activated))
                .filter(unknown)
                .cloned(),
        );
        if let Some(blocking) = blocking {
            wanted.extend(blocking.profiles.iter().filter(unknown).cloned());
        }
        wanted.sort_unstable();
        wanted.dedup();
        wanted
    }

    async fn apply(&mut self, block: Option<ProfileBlock>, helper: &Path) -> Answered {
        if matches!(&self.applied, Applied::Agreed(agreed) if *agreed == block) {
            return Answered::Held;
        }
        // A block the helper refused once is not asked for again: the refusal
        // is the machine's helper saying this session is not the desktop it
        // serves, and nothing this session does changes that answer
        // ([`Answered::Refused`]).
        if matches!(&self.applied, Applied::Refused) {
            return Answered::Refused;
        }
        let installed = helper.exists();
        if block.is_none() && !installed {
            self.applied = Applied::Agreed(None);
            return Answered::Held;
        }
        let request = match &block {
            Some(block) => Request::HoldProfileBlock {
                block: block.clone(),
            },
            None => Request::ReleaseProfileBlock,
        };
        let helper = helper.to_owned();
        let answer =
            tokio::task::spawn_blocking(move || protocol::call(&helper, &request, HELPER_TIMEOUT))
                .await;
        match answer {
            Ok(Ok((Response::Done, _))) => {
                self.applied = Applied::Agreed(block);
                Answered::Held
            }
            // A refusal is final: this session asks once, says so once, and
            // leaves the machine's helper to its own desktop.
            Ok(Ok((Response::Refused { reason }, _))) => {
                tracing::warn!(
                    %reason,
                    "the helper refused this session the VPN kill switch: the machine's helper serves its own desktop alone"
                );
                self.applied = Applied::Refused;
                Answered::Refused
            }
            Ok(Ok((answer, _))) => {
                tracing::warn!(
                    ?answer,
                    "the helper did not change the VPN kill switch's block"
                );
                Answered::Again
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not ask the helper for the VPN kill switch's block");
                Answered::Again
            }
            Err(error) => {
                tracing::warn!(%error, "the VPN kill switch's request did not finish");
                Answered::Again
            }
        }
    }
}

/// The addresses of `profile`'s servers: its addresses as given, its names
/// looked up.
async fn servers_of(profile: &str) -> Vec<IpAddr> {
    let uuid = profile.to_owned();
    let hosts =
        match tokio::task::spawn_blocking(move || os_actions::network_settings::vpn_servers(&uuid))
            .await
        {
            Ok(Ok(hosts)) => hosts,
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not read a VPN profile's servers");
                return Vec::new();
            }
            Err(_) => return Vec::new(),
        };
    let mut found = Vec::new();
    for host in hosts {
        if let Ok(address) = host.parse::<IpAddr>() {
            found.push(address);
        } else if let Ok(addresses) = tokio::net::lookup_host((host.as_str(), 0)).await {
            found.extend(addresses.map(|address| address.ip()));
        }
    }
    found.sort_unstable();
    found.dedup();
    found.truncate(MAX_PROFILE_SERVERS);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_nm::ActiveVpn;

    fn choices(release: Option<u64>) -> Choices {
        Choices {
            switches: BTreeMap::from([("work".to_owned(), ProfileKillSwitch::default())]),
            release: release.map(|id| ProfileRelease {
                id,
                profile: "work".into(),
            }),
        }
    }

    fn reading(state: Option<ActiveState>, ended: u64) -> VpnReading {
        VpnReading {
            known: true,
            active: state
                .map(|state| {
                    vec![ActiveVpn {
                        uuid: "work".into(),
                        state,
                    }]
                })
                .unwrap_or_default(),
            ended_on_purpose: if ended == 0 {
                BTreeMap::new()
            } else {
                BTreeMap::from([("work".to_owned(), ended)])
            },
        }
    }

    fn kept(release_seen: Option<u64>) -> ProfileIntent {
        ProfileIntent {
            connecting: BTreeMap::from([(
                "work".to_owned(),
                vec!["198.51.100.7".parse().unwrap()],
            )]),
            release_seen,
        }
    }

    fn switches() -> BTreeMap<String, Switch> {
        BTreeMap::from([("work".to_owned(), Switch::default())])
    }

    tilo_test_env::scenario! {
        fn the_profile_block_follows_networkmanager_and_the_persons_choices;

        phase a_release_already_acted_on_is_not_acted_on_again;
        {
            let mut follow = Follow::new(kept(Some(4)));
            assert!(
                follow
                    .released(&choices(Some(4)), &reading(None, 0))
                    .is_empty()
            );
            assert_eq!(
                follow.released(&choices(Some(5)), &reading(None, 0)),
                ["work"]
            );
            assert!(
                follow
                    .released(&choices(Some(5)), &reading(None, 0))
                    .is_empty()
            );
        }

        phase each_time_networkmanager_says_a_profile_went_down_on_purpose_releases_it_once;
        {
            let mut follow = Follow::new(ProfileIntent::default());
            assert!(
                follow
                    .released(&choices(None), &reading(None, 0))
                    .is_empty()
            );
            assert_eq!(follow.released(&choices(None), &reading(None, 1)), ["work"]);
            assert!(
                follow
                    .released(&choices(None), &reading(None, 1))
                    .is_empty()
            );
            assert_eq!(follow.released(&choices(None), &reading(None, 2)), ["work"]);
        }

        phase a_new_shell_takes_over_the_block_of_a_profile_still_set_to_connect;
        // A shell starting after one crashed blocks at once for a profile still
        // set to connect, with the servers looked up before, and keeps that.
        {
            let mut follow = Follow::new(kept(Some(4)));
            let released = follow.released(&choices(Some(4)), &reading(None, 0));
            let blocking = follow
                .intent
                .decide(&switches(), &BTreeMap::new(), &released)
                .expect("still set to connect and down: it blocks");
            assert_eq!(
                block_for(&blocking, &follow.servers),
                ProfileBlock {
                    servers: vec!["198.51.100.7".parse().unwrap()],
                    allow_local: false,
                },
                "its server stays reachable, so the tunnel can come back"
            );
            assert_eq!(follow.intent(), kept(Some(4)));
            assert_eq!(follow.intent(), follow.kept, "nothing new to keep");
        }

        phase a_new_shell_lifts_what_was_turned_off_while_it_was_away;
        // A profile the person turned off while no shell ran is lifted, and
        // that is kept.
        {
            let mut follow = Follow::new(kept(Some(4)));
            let released = follow.released(&choices(Some(5)), &reading(None, 0));
            assert_eq!(released, ["work"]);
            assert_eq!(
                follow
                    .intent
                    .decide(&switches(), &BTreeMap::new(), &released),
                None
            );
            assert_eq!(
                follow.intent(),
                ProfileIntent {
                    connecting: BTreeMap::new(),
                    release_seen: Some(5),
                }
            );
        }

        phase a_profile_switched_on_while_connected_has_its_servers_looked_up_before_it_drops;
        // A profile whose kill switch is turned on while it is connected has its servers looked
        // up then, so the block goes up at once with them rather than waiting on a lookup while
        // traffic leaves outside the tunnel. Once known, they are not looked up again.
        {
            let mut follow = Follow::new(ProfileIntent::default());
            let connected = BTreeMap::from([("work".to_owned(), Activity::Activated)]);
            let blocking = follow.intent.decide(&switches(), &connected, &[]);
            assert_eq!(blocking, None, "connected: nothing blocks");
            assert_eq!(
                follow.to_look_up(&switches(), &connected, blocking.as_ref()),
                ["work"],
                "looked up while the tunnel is up"
            );
            follow
                .servers
                .insert("work".into(), vec!["198.51.100.7".parse().unwrap()]);
            assert!(
                follow
                    .to_look_up(&switches(), &connected, blocking.as_ref())
                    .is_empty(),
                "known servers are not looked up again"
            );
            let dropped = follow.intent.decide(&switches(), &BTreeMap::new(), &[]);
            assert!(dropped.is_some(), "dropped: it blocks");
            assert!(
                follow
                    .to_look_up(&switches(), &BTreeMap::new(), dropped.as_ref())
                    .is_empty(),
                "the drop blocks at once, without a lookup first"
            );
        }

        phase only_profiles_set_to_connect_are_looked_up;
        // A connected profile without its kill switch, or not set to connect,
        // is never looked up.
        {
            let follow = Follow::new(ProfileIntent::default());
            let connected = BTreeMap::from([("work".to_owned(), Activity::Activated)]);
            assert!(
                follow
                    .to_look_up(&BTreeMap::new(), &connected, None)
                    .is_empty()
            );
            assert!(
                follow.to_look_up(&switches(), &connected, None).is_empty(),
                "not yet decided as set to connect"
            );
        }

        phase networkmanagers_states_map_to_the_decisions_activities;
        {
            assert_eq!(
                activity(ActiveState::Activating),
                Some(Activity::Activating)
            );
            assert_eq!(activity(ActiveState::Activated), Some(Activity::Activated));
            assert_eq!(
                activity(ActiveState::Deactivating),
                Some(Activity::Deactivating)
            );
            assert_eq!(activity(ActiveState::Deactivated), None);
            assert_eq!(activity(ActiveState::Unknown), None);
        }
    }

    /// A helper that refuses is asked once, not every few seconds: the refusal
    /// is the machine's helper saying this session is not the desktop it
    /// serves, and nothing the session does changes that answer.
    #[test]
    fn a_refused_block_is_not_asked_for_again() {
        let dir = tempfile::tempdir().expect("a directory");
        let socket = dir.path().join("helper.sock");
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&asked);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the helper");
        std::thread::spawn(move || {
            use std::io::Write as _;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut line = String::new();
                if std::io::BufRead::read_line(
                    &mut std::io::BufReader::new(stream.try_clone().expect("a stream")),
                    &mut line,
                )
                .is_err()
                {
                    continue;
                }
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let refused = serde_json::to_vec(&Response::Refused {
                    reason: "only the desktop of the person at this computer may change its routes"
                        .into(),
                })
                .expect("an answer");
                let _ = stream.write_all(&refused);
                let _ = stream.write_all(b"\n");
            }
        });

        let block = ProfileBlock {
            servers: vec!["198.51.100.7".parse().expect("an address")],
            allow_local: false,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let mut follow = Follow::new(ProfileIntent::default());
        let answers: Vec<Answered> = runtime.block_on(async {
            let mut answers = Vec::new();
            for _ in 0..3 {
                answers.push(follow.apply(Some(block.clone()), &socket).await);
            }
            answers
        });
        assert_eq!(
            answers,
            [Answered::Refused, Answered::Refused, Answered::Refused],
            "the refusal is final for the session"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the helper is asked once, not on every retry"
        );
        assert!(
            !follow.status.failed,
            "a refusal is not a fault this session can clear: {:?}",
            follow.status
        );
    }
}
