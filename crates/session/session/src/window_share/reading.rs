//! What the shares' resource reads: the shares as they stand, and how far the live plane they
//! travel came on this device.
//!
//! A shared window's pictures cross the workspace's live plane, and a plane that is not open says
//! nothing by itself: a share started under it captures nothing, and a window watched under it
//! receives nothing. So the reading names each workspace's plane (`live`: open, or what it waits
//! for and which device it waits to reach), and every window shared or watched in a workspace whose
//! plane is not open says what it waits for on its own row. A row that waits for nothing is one
//! whose pictures can flow. An open plane also says what crossed it: the epoch it seals under, how
//! many objects it opened, the ones it could not open and why, and which of this device's windows
//! another device asks it for. Two devices in one plane that hear nothing of each other read there
//! whether they seal under keys the other lacks. A device in a plane on keys its host lent it
//! (`lent_keys`) sends what it shares and opens nothing: a window it watches says so.
//!
//! A relay tells the device it turns away nothing but that. So a plane that was refused says which
//! epoch's keys this device presented (`presented`), and the plane of the device that relays the
//! workspace lists the endpoints it turned away and has not let in since, each with why
//! (`turned_away`): the two readings side by side say what the refused device alone cannot.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_desktop_model::TabId;
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_live::plane::Plane;

use super::WindowShares;
use crate::live::{Refusal, Relayer, Standing, TurnedAway};
use crate::sync::Client;

impl WindowShares {
    /// Read how far each workspace's live plane came, and the planes that opened, once either
    /// moved: nothing is read on a pass that has nothing new to find.
    pub(super) fn follow_live(&mut self, sync: &Client) {
        let seen = sync.live_revision();
        if seen == self.live_seen {
            return;
        }
        self.live_seen = seen;
        self.live = sync.live_standings();
        self.turned = sync.live_turned_away();
        // Held weakly: a plane that ended is its session's to drop.
        self.planes = crate::calls::planes_of(sync)
            .into_iter()
            .map(|(workspace, plane)| (workspace, Arc::downgrade(&plane)))
            .collect();
        self.changed();
    }

    /// Note which shared windows a member's desk is sent pictures of now.
    pub(super) fn follow_streams(&mut self) {
        let streaming: BTreeSet<TabId> = self
            .hosted
            .iter()
            .filter(|(_, hosted)| hosted.shown)
            .map(|(tab, _)| *tab)
            .collect();
        if streaming != self.streaming {
            self.streaming = streaming;
            self.changed();
        }
    }

    /// What the pictures of a window shared or watched in `workspace` wait for; nothing while the
    /// workspace's live plane is open on this device.
    fn waiting_for(&self, workspace: &str) -> String {
        waiting_for(self.live.get(workspace))
    }

    /// What the pictures of a window watched in `workspace` wait for: what its plane waits for,
    /// and in a plane this device is in on lent keys, that it opens nothing another device sends.
    fn watching_waits_for(&self, workspace: &str) -> String {
        match self.live.get(workspace) {
            Some(Standing::Open { lent: true, .. }) => t("session-window-share-live-lent"),
            standing => waiting_for(standing),
        }
    }

    /// The windows this session shares, as the reading says them: the ones it captures, and the
    /// ones whose page it publishes.
    fn sharing_rows(&self) -> Vec<Value> {
        let sharing: Vec<Value> = self
            .hosted
            .iter()
            .map(|(tab, hosted)| {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": hosted.open || hosted.controlled.contains(&peer),
                        })
                    })
                    .collect();
                json!({
                    "tab": tab.to_string(),
                    "window": hosted.window,
                    "app_id": hosted.app_id,
                    // Kept as it is: the views that read this know the key.
                    "room": hosted.workspace,
                    "input_allowed": viewers.iter().any(|viewer| {
                        viewer["can_control"].as_bool().unwrap_or(false)
                    }),
                    "viewers": viewers,
                    // Whether a member's desk asks the plane for the window now, so it is
                    // captured and sent.
                    "streaming": hosted.shown,
                    "waiting_for": self.waiting_for(&hosted.workspace),
                })
            })
            .collect();
        {
            let mut sharing = sharing;
            for (tab, page) in &self.pages {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": false,
                        })
                    })
                    .collect();
                sharing.push(json!({
                    "tab": tab.to_string(),
                    "window": page.window,
                    "app_id": page.app_id,
                    "room": page.workspace,
                    "input_allowed": false,
                    "viewers": viewers,
                    "streaming": !page.announced.is_empty(),
                    "waiting_for": self.waiting_for(&page.workspace),
                }));
            }
            sharing
        }
    }

    /// The resource's reading now.
    pub(crate) fn reading(&self) -> Value {
        let sharing = self.sharing_rows();
        let starting: Vec<Value> = self
            .starting
            .iter()
            .map(|start| {
                json!({
                    "tab": start.tab.to_string(),
                    "window": start.window,
                    "app_id": start.app_id,
                    "room": start.workspace,
                    "waiting_for": start.waiting_for,
                })
            })
            .collect();
        let watching: Vec<Value> = self
            .watching
            .iter()
            .map(|((workspace, tab), watched)| {
                json!({
                    "tab": tab.to_string(),
                    "room": workspace,
                    "host": watched.host.to_string(),
                    // The tab of this desk that pictures it, which is the sharer's own where this
                    // desk holds no view of its own of the window.
                    "shown_in": self.pictured_in(*tab).to_string(),
                    "waiting_for": self.watching_waits_for(workspace),
                })
            })
            .collect();
        let received: Vec<Value> = self
            .received
            .iter()
            .map(|((from, surface), stream)| {
                json!({
                    "tab": stream.tab.to_string(),
                    "surface": surface,
                    "from": from,
                    "app_id": stream.start.app_id,
                    "width": stream.start.width,
                    "height": stream.start.height,
                    "codec": stream.start.codec,
                    "pictured": stream.shown,
                })
            })
            .collect();
        let live: Vec<Value> = self
            .live
            .iter()
            .map(|(workspace, standing)| {
                let plane = self
                    .planes
                    .get(workspace)
                    .and_then(std::sync::Weak::upgrade);
                let turned = self.turned.get(workspace).map_or(&[][..], Vec::as_slice);
                live_row(workspace, standing, plane.as_deref(), turned)
            })
            .collect();
        json!({
            "sharing": sharing,
            "starting": starting,
            "watching": watching,
            "received": received,
            "live": live,
        })
    }
}

/// The word the reading says a plane's standing by: what a tool branches on, the same in every
/// language.
const fn state_of(standing: Option<&Standing>) -> &'static str {
    match standing {
        Some(Standing::Open { .. }) => "open",
        Some(Standing::WaitingForKeys) => "waiting-for-keys",
        Some(Standing::Keyless) => "no-keys",
        Some(Standing::Unread) => "membership-unread",
        Some(Standing::NoRelay) => "no-relay",
        Some(Standing::Unreached { .. }) => "relay-unreached",
        Some(Standing::Refused { .. }) => "refused",
        None => "absent",
    }
}

/// What a relay is called where a person reads it: the name its person gave it, and for a device
/// they never named the start of its key, which is what tells it apart.
fn called(relay: &Relayer) -> String {
    if relay.name.is_empty() {
        hex::encode(relay.device).chars().take(8).collect()
    } else {
        relay.name.clone()
    }
}

/// What a workspace whose plane stands at `standing` waits for, in the person's words; nothing
/// while it is open. `None` is a workspace this session runs no plane for.
fn waiting_for(standing: Option<&Standing>) -> String {
    match standing {
        Some(Standing::Open { .. }) => String::new(),
        Some(Standing::WaitingForKeys) => t("session-window-share-live-keys"),
        Some(Standing::Keyless) => t("session-window-share-live-keyless"),
        Some(Standing::Unread) => t("session-window-share-live-unread"),
        Some(Standing::NoRelay) => t("session-window-share-live-no-relay"),
        Some(Standing::Unreached { relay }) => t_args(
            "session-window-share-live-unreached",
            &[("device", called(relay).into())],
        ),
        Some(Standing::Refused { relay, .. }) => t_args(
            "session-window-share-live-refused",
            &[("device", called(relay).into())],
        ),
        None => t("session-window-share-live-absent"),
    }
}

/// One workspace's plane as the reading says it: its state, the device that relays it or that it
/// waits to reach, and what it waits for; and of `plane`, the one open here, the epoch it seals
/// under, whether it does so on keys its host lent it, how many objects of other devices it
/// opened, the ones it could not open by reason, and the tabs of this device's windows another
/// device asks it for. A plane its relay refused says the epoch whose keys this device presented,
/// and `turned` are the endpoints this device's own relay turned away from the workspace's plane,
/// each with why.
fn live_row(
    workspace: &str,
    standing: &Standing,
    plane: Option<&Plane>,
    turned: &[TurnedAway],
) -> Value {
    let relay = match standing {
        Standing::Open { relay, .. }
        | Standing::Unreached { relay }
        | Standing::Refused { relay, .. } => Some(relay),
        _ => None,
    };
    let epoch = match standing {
        Standing::Open { epoch, .. } => Some(*epoch),
        _ => None,
    };
    let presented = match standing {
        Standing::Refused { epoch, .. } => Some(*epoch),
        _ => None,
    };
    let turned_away: Vec<Value> = turned.iter().map(turned_row).collect();
    let asked_for: Vec<String> = plane
        .map(Plane::wanted_windows)
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();
    json!({
        "room": workspace,
        "state": state_of(Some(standing)),
        "relay": relay.map(|relay| hex::encode(relay.device)).unwrap_or_default(),
        "relay_name": relay.map(|relay| relay.name.clone()).unwrap_or_default(),
        "relay_here": relay.is_some_and(|relay| relay.here),
        "waiting_for": waiting_for(Some(standing)),
        "epoch": epoch,
        "lent_keys": matches!(standing, Standing::Open { lent: true, .. }),
        "presented": presented,
        "turned_away": turned_away,
        "heard": plane.map_or(0, Plane::heard_total),
        "refused": plane.map(Plane::refusals).unwrap_or_default(),
        "asked_for": asked_for,
    })
}

/// One endpoint this device's relay turned away, as the reading says it: the endpoint's key, the
/// name of the device when the workspace holds it and its person named it, why in the word a tool
/// branches on, how often it asked, and for a device whose keys are behind the epoch it presented
/// and the one the relay is at.
fn turned_row(turned: &TurnedAway) -> Value {
    let (presented, newest) = match turned.why {
        Refusal::KeysBehind { presented, newest } => (Some(presented), Some(newest)),
        _ => (None, None),
    };
    json!({
        "device": hex::encode(turned.peer),
        "name": turned.name,
        "why": turned.why.word(),
        "presented": presented,
        "newest": newest,
        "times": turned.times,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

    fn relay(name: &str, here: bool) -> Relayer {
        Relayer {
            device: [0xab; 32],
            name: name.to_owned(),
            here,
        }
    }

    /// An open plane waits for nothing, and says which device carries it.
    #[test]
    fn an_open_plane_waits_for_nothing_and_names_its_relay() {
        let row = live_row(
            WORKSPACE,
            &Standing::Open {
                relay: relay("Ada's desktop", true),
                epoch: 3,
                lent: false,
            },
            None,
            &[],
        );
        assert_eq!(row["epoch"], json!(3));
        assert_eq!(row["lent_keys"], json!(false));
        assert_eq!(row["presented"], Value::Null);
        assert_eq!(row["turned_away"], json!([]));
        assert_eq!(row["room"], json!(WORKSPACE));
        assert_eq!(row["state"], json!("open"));
        assert_eq!(row["waiting_for"], json!(""));
        assert_eq!(row["relay"], json!(hex::encode([0xab; 32])));
        assert_eq!(row["relay_name"], json!("Ada's desktop"));
        assert_eq!(row["relay_here"], json!(true));
    }

    /// A device in a plane on keys its host lent it says so: it sends what it shares and opens
    /// nothing, so that it hears nobody there is no fault, and what it shares waits for nothing.
    #[test]
    fn a_plane_on_lent_keys_says_so() {
        let lent = Standing::Open {
            relay: relay("Ada's desktop", false),
            epoch: 4,
            lent: true,
        };
        let row = live_row(WORKSPACE, &lent, None, &[]);
        assert_eq!(row["state"], json!("open"));
        assert_eq!(row["lent_keys"], json!(true));
        assert_eq!(row["waiting_for"], json!(""));
    }

    /// A plane that waits for the device relaying the workspace names that device, by the name its
    /// person gave it or, for one they never named, by the start of its key; one that was refused
    /// says so in other words than one that merely waits.
    #[test]
    fn a_plane_that_did_not_reach_its_relay_names_the_device_it_waits_for() {
        let named = Standing::Unreached {
            relay: relay("Ada's desktop", false),
        };
        let row = live_row(WORKSPACE, &named, None, &[]);
        assert_eq!(
            row["epoch"],
            Value::Null,
            "a plane that is not open seals nothing"
        );
        assert_eq!(row["state"], json!("relay-unreached"));
        assert_eq!(row["relay_here"], json!(false));
        let waits = row["waiting_for"].as_str().expect("what it waits for");
        assert!(waits.contains("Ada's desktop"), "{waits}");

        let unnamed = waiting_for(Some(&Standing::Unreached {
            relay: relay("", false),
        }));
        assert!(unnamed.contains("abababab"), "{unnamed}");

        let refused = Standing::Refused {
            relay: relay("Ada's desktop", false),
            epoch: 3,
        };
        let row = live_row(WORKSPACE, &refused, None, &[]);
        assert_eq!(row["state"], json!("refused"));
        assert_eq!(
            row["presented"],
            json!(3),
            "a refused plane says whose epoch's keys it presented"
        );
        assert_eq!(row["epoch"], Value::Null);
        let refusal = row["waiting_for"].as_str().expect("what it waits for");
        assert!(refusal.contains("Ada's desktop"), "{refusal}");
        assert_ne!(refusal, waits);
    }

    /// The device that relays a workspace says which endpoints it turned away from its plane and
    /// why: a device whose keys are behind with both epochs, any other by its reason alone.
    #[test]
    fn a_relay_says_which_devices_it_turned_away_and_why() {
        let open = Standing::Open {
            relay: relay("Ada's desktop", true),
            epoch: 4,
            lent: false,
        };
        let turned = [
            TurnedAway {
                peer: [0x0f; 32],
                name: "Ada's laptop".to_owned(),
                why: Refusal::KeysBehind {
                    presented: 3,
                    newest: 4,
                },
                times: 7,
            },
            TurnedAway {
                peer: [0x19; 32],
                name: String::new(),
                why: Refusal::NotAMember,
                times: 1,
            },
        ];
        let row = live_row(WORKSPACE, &open, None, &turned);
        assert_eq!(
            row["turned_away"],
            json!([
                {
                    "device": hex::encode([0x0f; 32]),
                    "name": "Ada's laptop",
                    "why": "keys-behind",
                    "presented": 3,
                    "newest": 4,
                    "times": 7,
                },
                {
                    "device": hex::encode([0x19; 32]),
                    "name": "",
                    "why": "not-a-member",
                    "presented": null,
                    "newest": null,
                    "times": 1,
                },
            ])
        );
    }

    /// Every standing but an open one says something, each its own, and a workspace no plane runs
    /// for says so too: a row never reads as flowing by saying nothing.
    #[test]
    fn every_plane_that_is_not_open_says_what_it_waits_for() {
        let said: Vec<(&str, String)> = [
            Some(Standing::WaitingForKeys),
            Some(Standing::Keyless),
            Some(Standing::Unread),
            Some(Standing::NoRelay),
            None,
        ]
        .iter()
        .map(|standing| (state_of(standing.as_ref()), waiting_for(standing.as_ref())))
        .collect();
        let states: BTreeSet<&str> = said.iter().map(|(state, _)| *state).collect();
        let words: BTreeSet<&str> = said.iter().map(|(_, words)| words.as_str()).collect();
        assert_eq!(states.len(), said.len(), "{said:?}");
        assert_eq!(words.len(), said.len(), "{said:?}");
        assert!(
            said.iter()
                .all(|(_, words)| !words.trim().is_empty()
                    && !words.starts_with("session-window-share")),
            "{said:?}"
        );
    }
}
