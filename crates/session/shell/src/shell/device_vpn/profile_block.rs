//! The kill switch of the person's `NetworkManager` VPN profiles, in the
//! session (docs/device-vpn.md, "Profile kill switch").
//!
//! The person turns it on per profile in Settings
//! ([`settings::PROFILE_KILL_SWITCHES`]). This follows those choices and
//! `NetworkManager`'s active VPNs ([`tilo_nm::NmClient::watch_vpns`]),
//! decides with [`device_vpn::profile::Intent`] which profiles are set to
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
//!
//! Nothing runs, and `NetworkManager` is not followed, until some profile
//! has its kill switch on. What blocks is published with the device VPN's
//! status ([`super::set_profile_block`]) for Settings.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use device_vpn::profile::{Activity, Blocking, Intent, Switch, block_for};
use device_vpn::protocol::{self, MAX_PROFILE_SERVERS, ProfileBlock, Request, Response};
use settings_store::device_vpn::{
    self as settings, ProfileBlockStatus, ProfileIntent, ProfileKillSwitch, ProfileRelease,
};
use settings_store::live::LiveSettings;
use tilo_nm::{ActiveState, VpnReading, VpnWatch};
use tokio::sync::watch;

/// How long a request to the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// A block the helper could not hold is asked for again after this long,
/// doubling up to [`RETRY_MOST`], while it is still wanted.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MOST: Duration = Duration::from_secs(60);

/// What the person chose, as the settings say.
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

/// Follow the person's VPN profiles and their kill switches from now on.
pub fn start(live: &LiveSettings) {
    let section = live.section(settings::SECTION);
    let kept = settings::PROFILE_INTENT.from_section(&section);
    let (sender, choices) = watch::channel(Choices::read(&section));
    let settings = live.clone();
    let following = std::thread::Builder::new()
        .name("tilo-vpn-kill-switch-settings".into())
        .spawn(move || follow_settings(&settings, &sender));
    if let Err(error) = following {
        tracing::warn!(%error, "could not follow the VPN kill switch settings");
        return;
    }
    let started = std::thread::Builder::new()
        .name("tilo-vpn-kill-switch".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run(choices, kept)),
                Err(error) => {
                    tracing::warn!(%error, "could not start the VPN kill switch");
                }
            }
        });
    if let Err(error) = started {
        tracing::warn!(%error, "could not start the VPN kill switch");
    }
}

/// Send the choices on whenever the settings document changes; nothing
/// wakes between changes.
fn follow_settings(live: &LiveSettings, sender: &watch::Sender<Choices>) {
    let mut seen = live.snapshot().generation;
    loop {
        let snapshot = live.wait_for_next(seen);
        seen = snapshot.generation;
        let choices = Choices::read(&snapshot.section(settings::SECTION));
        sender.send_if_modified(|current| {
            let changed = *current != choices;
            if changed {
                *current = choices;
            }
            changed
        });
        if sender.is_closed() {
            return;
        }
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

/// Keep `intent` in the settings; whether it was written.
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

async fn run(mut choices: watch::Receiver<Choices>, kept: ProfileIntent) {
    let mut follow = Follow::new(kept);
    let mut vpns: Option<VpnWatch> = None;
    let mut retry: Option<Duration> = None;
    loop {
        let current = choices.borrow_and_update().clone();
        if vpns.is_none() && !current.switches.is_empty() {
            vpns = watch_vpns().await;
        }
        let reading = vpns.as_ref().map(VpnWatch::current).unwrap_or_default();
        let held = follow.step(&current, &reading, keep).await;
        retry = if held {
            None
        } else {
            Some(retry.map_or(RETRY_FIRST, |delay| delay.saturating_mul(2).min(RETRY_MOST)))
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

/// What a profile's activity is, as the decisions name it.
const fn activity(state: ActiveState) -> Option<Activity> {
    match state {
        ActiveState::Activating => Some(Activity::Activating),
        ActiveState::Activated => Some(Activity::Activated),
        ActiveState::Deactivating => Some(Activity::Deactivating),
        ActiveState::Deactivated | ActiveState::Unknown => None,
    }
}

/// What the helper holds, as far as the follower knows.
#[derive(Debug)]
enum Applied {
    /// Not asked yet: a block an earlier shell left may be up.
    Unknown,
    /// The helper agreed to hold this block, or none.
    Agreed(Option<ProfileBlock>),
}

/// What the follower keeps between steps.
struct Follow {
    intent: Intent,
    /// The last release request acted on.
    release_seen: Option<u64>,
    /// The intent as last kept in the settings.
    kept: ProfileIntent,
    /// Per profile, how often `NetworkManager` said it went down on purpose.
    ended_seen: BTreeMap<String, u64>,
    /// Each profile's servers, from the last lookup that found any.
    servers: BTreeMap<String, Vec<IpAddr>>,
    /// Each profile's activity at the last step.
    activities: BTreeMap<String, Activity>,
    /// What the helper was last told and agreed to.
    applied: Applied,
    status: ProfileBlockStatus,
}

impl Follow {
    /// Take over the intent `kept` from before this session.
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

    /// The profiles turned off on purpose since the last step.
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
        keep: impl FnOnce(&ProfileIntent) -> bool,
    ) -> bool {
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
        let held = self.apply(block).await;
        let status = ProfileBlockStatus {
            blocking: match (&blocking, held) {
                (Some(blocking), true) => blocking.profiles.clone(),
                _ => Vec::new(),
            },
            failed: blocking.is_some() && !held,
        };
        if status != self.status {
            self.status = status.clone();
            super::set_profile_block(status);
        }
        held
    }

    /// Look up the servers of each profile with its switch on that starts
    /// connecting (the network is open then), and of each that must block
    /// and has none yet.
    async fn look_up_servers(
        &mut self,
        switches: &BTreeMap<String, Switch>,
        active: &BTreeMap<String, Activity>,
        blocking: Option<&Blocking>,
    ) {
        let starting = active.iter().filter(|(profile, activity)| {
            switches.contains_key(*profile)
                && **activity == Activity::Activating
                && self.activities.get(*profile) != Some(&Activity::Activating)
        });
        let mut wanted: Vec<String> = starting.map(|(profile, _)| profile.clone()).collect();
        if let Some(blocking) = blocking {
            wanted.extend(
                blocking
                    .profiles
                    .iter()
                    .filter(|profile| !self.servers.contains_key(*profile))
                    .cloned(),
            );
        }
        wanted.sort_unstable();
        wanted.dedup();
        for profile in wanted {
            let found = servers_of(&profile).await;
            if !found.is_empty() {
                self.servers.insert(profile, found);
            }
        }
        self.servers
            .retain(|profile, _| switches.contains_key(profile));
    }

    /// Have the helper hold `block` (or nothing); whether it does.
    async fn apply(&mut self, block: Option<ProfileBlock>) -> bool {
        if matches!(&self.applied, Applied::Agreed(agreed) if *agreed == block) {
            return true;
        }
        let installed = Path::new(protocol::SOCKET_PATH).exists();
        // Nothing to lift on a device without the helper.
        if block.is_none() && !installed {
            self.applied = Applied::Agreed(None);
            return true;
        }
        let request = match &block {
            Some(block) => Request::HoldProfileBlock {
                block: block.clone(),
            },
            None => Request::ReleaseProfileBlock,
        };
        let answer = tokio::task::spawn_blocking(move || {
            protocol::call(Path::new(protocol::SOCKET_PATH), &request, HELPER_TIMEOUT)
        })
        .await;
        match answer {
            Ok(Ok((Response::Done, _))) => {
                self.applied = Applied::Agreed(block);
                true
            }
            Ok(Ok((answer, _))) => {
                tracing::warn!(
                    ?answer,
                    "the helper did not change the VPN kill switch's block"
                );
                false
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not ask the helper for the VPN kill switch's block");
                false
            }
            Err(error) => {
                tracing::warn!(%error, "the VPN kill switch's request did not finish");
                false
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

    #[test]
    fn a_release_already_acted_on_is_not_acted_on_again() {
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

    #[test]
    fn each_time_networkmanager_says_a_profile_went_down_on_purpose_releases_it_once() {
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

    /// A shell starting after one crashed blocks at once for a profile still
    /// set to connect, with the servers looked up before, and keeps that.
    #[test]
    fn a_new_shell_takes_over_the_block_of_a_profile_still_set_to_connect() {
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

    /// A profile the person turned off while no shell ran is lifted, and
    /// that is kept.
    #[test]
    fn a_new_shell_lifts_what_was_turned_off_while_it_was_away() {
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

    #[test]
    fn networkmanagers_states_map_to_the_decisions_activities() {
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
