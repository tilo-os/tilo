//! The kill switch of the person's `NetworkManager` VPN profiles: when to
//! block (docs/device-vpn.md, "Profile kill switch").
//!
//! A profile is set to connect from when `NetworkManager` starts bringing it
//! up until the person turns it off on purpose. While a profile with its kill
//! switch on is set to connect and is not connected, nothing leaves the
//! computer but through a tunnel, to its servers and for address
//! configuration ([`ProfileBlock`]). [`Intent`] decides that from what the
//! person chose and what `NetworkManager` reports; it does no I/O, so the
//! session and the tests drive the same decisions.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use crate::net;
use crate::protocol::{MAX_PROFILE_SERVERS, ProfileBlock};

/// A profile's kill switch, while it is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Switch {
    /// Let the local network through while the block holds.
    pub allow_local: bool,
}

/// Where `NetworkManager` has a profile, while it has it at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Activating,
    Activated,
    Deactivating,
}

/// The profiles whose block must hold now, and whether all of them let the
/// local network through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocking {
    /// Their ids, in order.
    pub profiles: Vec<String>,
    /// The local network stays reachable only if every blocking profile
    /// allows it.
    pub allow_local: bool,
}

/// Which profiles are set to connect, remembered between readings.
#[derive(Clone, Debug, Default)]
pub struct Intent {
    wanted: BTreeSet<String>,
}

impl Intent {
    /// Take in the person's `switches`, the profiles `NetworkManager` has
    /// now (`active`), and the profiles turned off on purpose since the last
    /// call (`released`), and say what must block.
    ///
    /// - A profile turned off on purpose is no longer set to connect.
    /// - A profile with its switch on is set to connect once it is
    ///   activating or activated.
    /// - A profile whose switch is off, or that is gone, is forgotten.
    pub fn decide(
        &mut self,
        switches: &BTreeMap<String, Switch>,
        active: &BTreeMap<String, Activity>,
        released: &[String],
    ) -> Option<Blocking> {
        for profile in released {
            self.wanted.remove(profile);
        }
        for (profile, activity) in active {
            if switches.contains_key(profile)
                && matches!(activity, Activity::Activating | Activity::Activated)
            {
                self.wanted.insert(profile.clone());
            }
        }
        self.wanted.retain(|profile| switches.contains_key(profile));
        let profiles: Vec<String> = self
            .wanted
            .iter()
            .filter(|profile| active.get(*profile) != Some(&Activity::Activated))
            .cloned()
            .collect();
        if profiles.is_empty() {
            return None;
        }
        let allow_local = profiles.iter().all(|profile| {
            switches
                .get(profile)
                .is_some_and(|switch| switch.allow_local)
        });
        Some(Blocking {
            profiles,
            allow_local,
        })
    }

    /// Whether `profile` is set to connect.
    #[must_use]
    pub fn wants(&self, profile: &str) -> bool {
        self.wanted.contains(profile)
    }
}

/// The block for `blocking`, letting through the servers of each blocking
/// profile as `servers` has them: addresses a block can carry, each once, at
/// most [`MAX_PROFILE_SERVERS`].
#[must_use]
pub fn block_for(blocking: &Blocking, servers: &BTreeMap<String, Vec<IpAddr>>) -> ProfileBlock {
    let mut through: Vec<IpAddr> = blocking
        .profiles
        .iter()
        .filter_map(|profile| servers.get(profile))
        .flatten()
        .copied()
        .filter(|address| net::is_server_address(*address))
        .collect();
    through.sort_unstable();
    through.dedup();
    through.truncate(MAX_PROFILE_SERVERS);
    ProfileBlock {
        servers: through,
        allow_local: blocking.allow_local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(allow_local: bool) -> BTreeMap<String, Switch> {
        BTreeMap::from([("work".to_owned(), Switch { allow_local })])
    }

    fn now(activity: Option<Activity>) -> BTreeMap<String, Activity> {
        activity
            .map(|activity| BTreeMap::from([("work".to_owned(), activity)]))
            .unwrap_or_default()
    }

    fn blocking(allow_local: bool) -> Blocking {
        Blocking {
            profiles: vec!["work".into()],
            allow_local,
        }
    }

    #[test]
    fn a_profile_blocks_while_set_to_connect_and_not_connected() {
        let mut intent = Intent::default();
        let switches = on(false);
        assert_eq!(
            intent.decide(&switches, &now(None), &[]),
            None,
            "never asked for"
        );
        assert_eq!(
            intent.decide(&switches, &now(Some(Activity::Activating)), &[]),
            Some(blocking(false))
        );
        assert_eq!(
            intent.decide(&switches, &now(Some(Activity::Activated)), &[]),
            None,
            "connected"
        );
        assert_eq!(
            intent.decide(&switches, &now(Some(Activity::Deactivating)), &[]),
            Some(blocking(false)),
            "dropping"
        );
        assert_eq!(
            intent.decide(&switches, &now(None), &[]),
            Some(blocking(false)),
            "dropped, not on purpose"
        );
        assert!(intent.wants("work"));
        assert_eq!(
            intent.decide(&switches, &now(None), &["work".into()]),
            None,
            "turned off on purpose"
        );
        assert!(!intent.wants("work"));
        assert_eq!(intent.decide(&switches, &now(None), &[]), None);
    }

    #[test]
    fn a_profile_without_its_switch_never_blocks() {
        let mut intent = Intent::default();
        let off = BTreeMap::new();
        assert_eq!(
            intent.decide(&off, &now(Some(Activity::Activating)), &[]),
            None
        );
        assert_eq!(intent.decide(&off, &now(None), &[]), None);
        // Turning the switch off while blocking lifts the block; turning it
        // on again does not bring back a profile no longer set to connect.
        let switches = on(false);
        intent.decide(&switches, &now(Some(Activity::Activated)), &[]);
        assert_eq!(
            intent.decide(&switches, &now(None), &[]),
            Some(blocking(false))
        );
        assert_eq!(intent.decide(&off, &now(None), &[]), None);
        assert_eq!(intent.decide(&switches, &now(None), &[]), None);
    }

    #[test]
    fn turning_off_a_connected_profile_on_purpose_never_blocks() {
        let mut intent = Intent::default();
        let switches = on(true);
        intent.decide(&switches, &now(Some(Activity::Activated)), &[]);
        // The person's request arrives while it is still up, and its going
        // down follows.
        assert_eq!(
            intent.decide(
                &switches,
                &now(Some(Activity::Deactivating)),
                &["work".into()]
            ),
            None
        );
        assert_eq!(intent.decide(&switches, &now(None), &[]), None);
    }

    #[test]
    fn the_local_network_stays_reachable_only_if_every_blocking_profile_allows_it() {
        let mut intent = Intent::default();
        let mut switches = on(true);
        switches.insert("home".into(), Switch { allow_local: false });
        let active = BTreeMap::from([
            ("work".to_owned(), Activity::Activating),
            ("home".to_owned(), Activity::Activated),
        ]);
        assert_eq!(intent.decide(&switches, &active, &[]), Some(blocking(true)));
        let active = BTreeMap::from([("work".to_owned(), Activity::Activating)]);
        assert_eq!(
            intent.decide(&switches, &active, &[]),
            Some(Blocking {
                profiles: vec!["home".into(), "work".into()],
                allow_local: false,
            })
        );
    }

    #[test]
    fn the_block_lets_through_each_servers_address_once() {
        let servers = BTreeMap::from([
            (
                "work".to_owned(),
                vec![
                    "198.51.100.7".parse().unwrap(),
                    "198.51.100.7".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ],
            ),
            ("other".to_owned(), vec!["203.0.113.1".parse().unwrap()]),
        ]);
        let block = block_for(&blocking(false), &servers);
        assert_eq!(
            block,
            ProfileBlock {
                servers: vec!["198.51.100.7".parse().unwrap()],
                allow_local: false,
            }
        );
        assert_eq!(block.check(), Ok(()));
        // Servers not looked up yet: the block holds all the same.
        assert!(
            block_for(&blocking(true), &BTreeMap::new())
                .servers
                .is_empty()
        );
    }
}
