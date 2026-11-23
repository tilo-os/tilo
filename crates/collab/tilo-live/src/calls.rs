//! Calls in a workspace, as its presence says them.
//!
//! Being in a call is presence: joining says the channel in this device's
//! presence ([`crate::presence::Own::call`]), leaving stops saying it, and
//! what a device shows in it (its camera, a shared screen, whether its
//! microphone is off or it hears nothing) rides beside the channel. Who is in a
//! channel's call is what the devices in it say of themselves, so every device
//! of the workspace (a desktop, a page) reads the same channels from its
//! presence board, and no server keeps a call. What a call carries is the
//! workspace's live session ([`crate::live_session`]): its voice and pictures
//! are tracks sealed under the channel's keys, which only the workspace's
//! readers open.
//!
//! What a call allows is the workspace's ([`collab_types::calls`]): someone the
//! owner or a moderator ended the call for, or denied, is not in it. A call's
//! media names its call ([`crate::live::call_track`]), and every member's live
//! session holds it to the call ([`call`]): it opens the media only of the devices
//! the call lets speak, and sends none where it may not speak. The relay holds
//! every call track to the same rule ([`crate::relay::SessionAuthorization::call`]),
//! and the channel lists here leave out whoever the call does not let in.

use std::collections::{BTreeMap, BTreeSet};

pub use collab_types::calls::{CallRule, Directions};

use collab_types::presence::{Bridged, Payload, Presence};

use crate::presence::{Call, Device, LiveCall};

/// One device in a call, as its presence says it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "camera, screen, muted and deafened are each their own flag a person shows in a call"
)]
pub struct Caller {
    /// The device.
    pub device: Device,
    /// The person the device is (their root key as hex), once a certificate or the workspace's
    /// members list named them.
    pub person: Option<String>,
    /// Its camera is on.
    pub camera: bool,
    /// It shares its screen.
    pub screen: bool,
    /// Its microphone is off.
    pub muted: bool,
    /// It hears nothing of the call.
    pub deafened: bool,
    /// The service this device bridges the call to (`discord`), when it is a bridge rather than a
    /// person: where the call is heard besides, never someone in it.
    pub bridge: Option<String>,
    /// Who this caller is on another service, when a bridge carries them here: a person in the
    /// call of their own, reached through `service`, never as the bridge's person.
    pub bridged: Option<Bridged>,
}

/// One person a bridge carries into a call: a member of it of their own, shown
/// by the name they have there and marked as reached through its service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgedPerson {
    /// The workspace member the participant is: the member the account is
    /// linked to, else the participant's own stand-in publisher.
    pub person: String,
    /// The service it is reached through (`discord`).
    pub service: String,
    /// The account there whose voice this carries.
    pub account: String,
    /// The name they are shown by.
    pub name: String,
    /// BLAKE3 hex of their picture in the workspace's content store, when it is held.
    pub avatar: Option<String>,
}

/// One voice channel's call: who is in it, and what each of them shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    /// The channel's id: the whole of a call's identity.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when nobody named it.
    pub name: String,
    /// What the call allows, as the workspace keeps it.
    pub rule: CallRule,
    /// The devices in the call, in the order they arrived, this one among them while it is in it.
    pub callers: Vec<Caller>,
}

impl Channel {
    /// The people in the call, each once, sorted: a person on two devices is in it once.
    #[must_use]
    pub fn people(&self) -> Vec<String> {
        self.people_where(|_| true)
    }

    /// Those of [`Self::people`] with a camera on, on any of their devices.
    #[must_use]
    pub fn cameras(&self) -> Vec<String> {
        self.people_where(|caller| caller.camera)
    }

    /// Those of [`Self::people`] sharing a screen, on any of their devices.
    #[must_use]
    pub fn screens(&self) -> Vec<String> {
        self.people_where(|caller| caller.screen)
    }

    /// Those of [`Self::people`] muted on every device they are in the call on: someone heard on
    /// one device is not muted.
    #[must_use]
    pub fn muted(&self) -> Vec<String> {
        self.people_on_every(|caller| caller.muted)
    }

    /// Those of [`Self::people`] deafened on every device they are in the call on; each of them
    /// is muted too.
    #[must_use]
    pub fn deafened(&self) -> Vec<String> {
        self.people_on_every(|caller| caller.deafened)
    }

    /// The services bridges carry the call to, each once, sorted.
    #[must_use]
    pub fn bridges(&self) -> Vec<String> {
        self.callers
            .iter()
            .filter_map(|caller| {
                caller.bridge.clone().or_else(|| {
                    caller
                        .bridged
                        .as_ref()
                        .map(|bridged| bridged.service.clone())
                })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The people a bridge carries into the call from another service, each
    /// once and sorted by their member id: who a call view shows by their name
    /// there, marked as reached through its service.
    #[must_use]
    pub fn bridged(&self) -> Vec<BridgedPerson> {
        let mut people: Vec<BridgedPerson> = self
            .callers
            .iter()
            .filter_map(|caller| {
                let bridged = caller.bridged.as_ref()?;
                Some(BridgedPerson {
                    person: caller
                        .person
                        .clone()
                        .unwrap_or_else(|| hex::encode(caller.device)),
                    service: bridged.service.clone(),
                    account: bridged.account.clone(),
                    name: bridged.name.clone(),
                    avatar: bridged.avatar.clone(),
                })
            })
            .collect();
        people.sort_by(|left, right| left.person.cmp(&right.person));
        people.dedup_by(|left, right| left.person == right.person);
        people
    }

    /// The person each device in the call is, for the devices whose person is known: whose
    /// pictures a device's frames are. A bridge, and a person it carries with no device of
    /// their own here, sends no pictures and is nobody's.
    #[must_use]
    pub fn persons(&self) -> BTreeMap<Device, String> {
        self.people_in()
            .filter(|caller| caller.bridged.is_none())
            .filter_map(|caller| Some((caller.device, caller.person.clone()?)))
            .collect()
    }

    /// The callers who are someone in the call: everyone but a bridge.
    fn people_in(&self) -> impl Iterator<Item = &Caller> {
        self.callers.iter().filter(|caller| caller.bridge.is_none())
    }

    fn people_where(&self, keep: impl Fn(&Caller) -> bool) -> Vec<String> {
        self.people_in()
            .filter(|caller| keep(caller))
            .filter_map(|caller| caller.person.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn people_on_every(&self, flag: impl Fn(&Caller) -> bool) -> Vec<String> {
        let mut flagged: BTreeMap<&str, bool> = BTreeMap::new();
        for caller in self.people_in() {
            if let Some(person) = caller.person.as_deref() {
                let every = flagged.entry(person).or_insert(true);
                *every &= flag(caller);
            }
        }
        flagged
            .into_iter()
            .filter(|(_, every)| *every)
            .map(|(person, _)| person.to_owned())
            .collect()
    }
}

/// This device in a call, which its own board never hears: what it said of itself.
#[derive(Clone, Copy, Debug)]
pub struct Own<'a> {
    /// This device.
    pub device: Device,
    /// The person it is, when it knows.
    pub person: Option<&'a str>,
    /// The call it said it is in.
    pub call: &'a Call,
}

/// The calls of a workspace, one per channel anyone is in, sorted by channel: what `heard`, its
/// board's calls ([`crate::presence::Board::calls`]), say of the others, and what `own` says of this
/// device. `persons` names the person of a device whose presence named none yet (from the
/// workspace's members); a device no one names is in the call unnamed. `rules` are what the
/// workspace's calls allow, by channel: a call lists nobody it lets do nothing (an ended call
/// nobody), and someone it does not let speak as muted.
#[must_use]
pub fn channels(
    heard: &[LiveCall],
    own: Option<Own<'_>>,
    persons: &BTreeMap<Device, String>,
    rules: &BTreeMap<String, CallRule>,
) -> Vec<Channel> {
    let mut channels: BTreeMap<String, Channel> = BTreeMap::new();
    for call in heard {
        let channel = channels
            .entry(call.channel.clone())
            .or_insert_with(|| Channel {
                channel: call.channel.clone(),
                ..Channel::default()
            });
        if channel.name.trim().is_empty() {
            channel.name.clone_from(&call.name);
        }
        for caller in &call.people {
            let Some(device) = device_of(&caller.id) else {
                continue;
            };
            channel.callers.push(Caller {
                device,
                // A bridged person with no member to be is their own stand-in
                // publisher: in the call by that id, never as the bridge's person.
                person: caller.user.clone().or_else(|| {
                    caller
                        .bridged
                        .as_ref()
                        .map(|_| hex::encode(device))
                        .or_else(|| persons.get(&device).cloned())
                }),
                camera: caller.camera,
                screen: caller.screen,
                muted: caller.muted,
                deafened: caller.deafened,
                bridge: caller.bridge.clone(),
                bridged: caller.bridged.clone(),
            });
        }
    }
    if let Some(own) = own {
        let channel = channels
            .entry(own.call.call.channel.clone())
            .or_insert_with(|| Channel {
                channel: own.call.call.channel.clone(),
                ..Channel::default()
            });
        if channel.name.trim().is_empty() {
            channel.name.clone_from(&own.call.call.name);
        }
        channel.callers.retain(|caller| caller.device != own.device);
        channel.callers.push(Caller {
            device: own.device,
            person: own
                .person
                .map(str::to_owned)
                .or_else(|| persons.get(&own.device).cloned()),
            camera: own.call.camera,
            screen: own.call.screen,
            muted: own.call.muted || own.call.deafened,
            deafened: own.call.deafened,
            bridge: own.call.call.bridge.clone(),
            bridged: None,
        });
    }
    for channel in channels.values_mut() {
        channel.rule = rules.get(&channel.channel).cloned().unwrap_or_default();
        let rule = &channel.rule;
        channel
            .callers
            .retain(|caller| rule.may(caller.person.as_deref()).any());
        for caller in &mut channel.callers {
            let may = rule.may(caller.person.as_deref());
            caller.muted |= !may.publish;
            caller.deafened |= !may.subscribe;
        }
    }
    channels.into_values().collect()
}

/// The devices of `devices` the call in `channel` lets speak, each the person `persons` names:
/// the only ones whose media of it a live session opens. A channel the workspace keeps no rule
/// for lets everyone speak.
#[must_use]
pub fn speakers(
    channel: &str,
    rules: &BTreeMap<String, CallRule>,
    persons: &BTreeMap<Device, String>,
    devices: impl IntoIterator<Item = Device>,
) -> crate::roster::Roster {
    crate::roster::Roster::new(
        devices
            .into_iter()
            .filter(|device| may(channel, rules, persons, *device).publish),
    )
}

/// What the call in `channel` lets `device` do, as the person `persons` names it.
#[must_use]
pub fn may(
    channel: &str,
    rules: &BTreeMap<String, CallRule>,
    persons: &BTreeMap<Device, String>,
    device: Device,
) -> Directions {
    rules.get(channel).map_or(Directions::BOTH, |rule| {
        rule.may(persons.get(&device).map(String::as_str))
    })
}

/// The channel whose call one presence object says its device is in (`body`, as its presence
/// track carries it; empty when the device left): `Some(None)` for no call, and `None` for a body
/// that says nothing of one.
#[must_use]
pub fn said_call(body: &[u8]) -> Option<Option<String>> {
    if body.is_empty() {
        return Some(None);
    }
    let Some(Presence {
        payload: Payload::Availability { call, .. },
        ..
    }) = Presence::decode(body)
    else {
        return None;
    };
    Some(
        call.as_deref()
            .and_then(|call| Call::decode(call, false, false))
            .map(|call| call.call.channel),
    )
}

/// The device a caller's publisher names, as presence writes it (hex).
fn device_of(id: &str) -> Option<Device> {
    let mut device = [0; 32];
    hex::decode_to_slice(id, &mut device).ok()?;
    Some(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::LiveCaller;
    use collab_types::presence::CallPresence;

    fn caller(device: u8, person: Option<&str>) -> LiveCaller {
        LiveCaller {
            id: hex::encode([device; 32]),
            label: String::new(),
            user: person.map(str::to_owned),
            camera: false,
            screen: false,
            muted: false,
            deafened: false,
            bridge: None,
            bridged: None,
        }
    }

    fn call(channel: &str, people: Vec<LiveCaller>) -> LiveCall {
        LiveCall {
            workspace: "w".into(),
            channel: channel.into(),
            name: String::new(),
            people,
        }
    }

    /// A person is in a channel once whatever devices they are in it on, muted only when every
    /// one of those devices is, and the devices the workspace names stand in for a certificate
    /// not heard yet.
    #[test]
    fn a_channel_counts_people_not_devices() {
        let laptop = LiveCaller {
            muted: true,
            camera: true,
            ..caller(1, Some("ada"))
        };
        let phone = caller(2, Some("ada"));
        let page = LiveCaller {
            muted: true,
            deafened: true,
            ..caller(3, None)
        };
        let discord = LiveCaller {
            bridge: Some("discord".into()),
            ..caller(4, None)
        };
        let persons = BTreeMap::from([([3; 32], "bo".to_owned())]);
        let channels = channels(
            &[call("voice", vec![laptop, phone, page, discord])],
            None,
            &persons,
            &BTreeMap::new(),
        );
        let [voice] = channels.as_slice() else {
            panic!("one channel: {channels:?}");
        };
        assert_eq!(voice.people(), ["ada", "bo"]);
        assert_eq!(voice.cameras(), ["ada"]);
        assert_eq!(
            voice.muted(),
            ["bo"],
            "heard on her phone, Ada is not muted"
        );
        assert_eq!(voice.deafened(), ["bo"]);
        assert_eq!(voice.persons().len(), 3, "a bridge is nobody's tile");
        assert_eq!(voice.bridges(), ["discord"]);
    }

    /// A person a bridge carries is a participant of the call of their own,
    /// under their name there; the bridge is no person of it, so the person
    /// whose device it is is not in the call because of it; and a call that
    /// denies the bridged person carries them no more.
    #[test]
    fn a_bridged_person_is_a_participant_of_their_own() {
        use collab_types::presence::Bridged;
        let guest = LiveCaller {
            bridged: Some(Bridged {
                service: "discord".into(),
                account: "7001".into(),
                name: "Dana".into(),
                avatar: Some("ab".repeat(32)),
                linked: None,
            }),
            label: "Dana".into(),
            ..caller(5, None)
        };
        let owner = LiveCaller {
            bridge: Some("discord".into()),
            user: Some("cc".repeat(32)),
            label: "Discord".into(),
            ..caller(6, None)
        };
        let heard = [call("voice", vec![guest, owner])];
        let listed = channels(&heard, None, &BTreeMap::new(), &BTreeMap::new());
        let [voice] = listed.as_slice() else {
            panic!("one channel: {listed:?}");
        };
        let id = hex::encode([5; 32]);
        assert_eq!(
            voice.people(),
            vec![id.clone()],
            "the bridged person is in it"
        );
        assert_eq!(voice.bridges(), ["discord"]);
        assert!(!voice.people().contains(&"cc".repeat(32)));
        let carried = voice.bridged();
        let [person] = carried.as_slice() else {
            panic!("one bridged person: {carried:?}");
        };
        assert_eq!(person.person, id);
        assert_eq!(person.name, "Dana");
        assert_eq!(person.service, "discord");
        assert_eq!(person.account, "7001");
        assert!(
            !voice.persons().contains_key(&[6; 32]),
            "a bridge is nobody's tile"
        );
        assert!(
            !voice.persons().contains_key(&[5; 32]),
            "a carried person has no device here to send pictures"
        );
        let owner_person = "cc".repeat(32);
        assert!(
            !heard[0].joined_by(Some(owner_person.as_str())),
            "the bridge's person is not in the call because of it"
        );

        let rules = BTreeMap::from([(
            "voice".to_owned(),
            CallRule {
                denied: BTreeMap::from([(id, Directions::BOTH)]),
                ..CallRule::default()
            },
        )]);
        let listed = channels(&heard, None, &BTreeMap::new(), &rules);
        assert!(
            listed[0].people().is_empty(),
            "a denied bridged person is carried no more"
        );
    }

    /// This device's own call is in the list as it said it, once, though its board never hears
    /// it.
    #[test]
    fn this_devices_call_is_its_own_word() {
        let own = Call {
            call: CallPresence {
                channel: "standup".into(),
                name: "Standup".into(),
                bridge: None,
            },
            camera: false,
            screen: true,
            muted: false,
            deafened: true,
        };
        let channels = channels(
            &[call(
                "standup",
                vec![caller(9, Some("ada")), caller(1, None)],
            )],
            Some(Own {
                device: [9; 32],
                person: Some("ada"),
                call: &own,
            }),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let [standup] = channels.as_slice() else {
            panic!("one channel: {channels:?}");
        };
        assert_eq!(standup.name, "Standup");
        assert_eq!(standup.callers.len(), 2);
        assert_eq!(standup.screens(), ["ada"]);
        assert_eq!(standup.muted(), ["ada"], "deafened is muted too");
    }

    /// A call lists nobody once it was ended and leaves out whom it denies; a live session opens
    /// nothing a denied device sends in it, and nothing of it for a device it denies, while a
    /// device in another call is not judged by it.
    #[test]
    fn a_moderated_call_carries_nobody_it_refuses() {
        use collab_types::calls::CallChange;
        let heard = [call(
            "standup",
            vec![caller(1, Some("ada")), caller(2, Some("bo"))],
        )];
        let bo_out = CallRule::default()
            .changed(&CallChange::Deny {
                person: [0xb0; 32],
                denied: Directions::BOTH,
            })
            .unwrap();
        let mut rules = BTreeMap::new();
        rules.insert(
            "standup".to_owned(),
            CallRule {
                denied: BTreeMap::from([("bo".to_owned(), Directions::BOTH)]),
                ..bo_out
            },
        );
        let listed = channels(&heard, None, &BTreeMap::new(), &rules);
        assert_eq!(listed[0].people(), ["ada"]);
        rules.insert(
            "standup".to_owned(),
            CallRule::default().changed(&CallChange::End).unwrap(),
        );
        let listed = channels(&heard, None, &BTreeMap::new(), &rules);
        assert!(listed[0].callers.is_empty(), "an ended call carries nobody");

        let persons = BTreeMap::from([([1; 32], "ada".to_owned()), ([2; 32], "bo".to_owned())]);
        let rules = BTreeMap::from([(
            "standup".to_owned(),
            CallRule {
                denied: BTreeMap::from([("bo".to_owned(), Directions::BOTH)]),
                ..CallRule::default()
            },
        )]);
        let devices = [[1; 32], [2; 32], [3; 32]];
        let standup = speakers("standup", &rules, &persons, devices);
        assert!(standup.admits([1; 32]), "Ada speaks in it");
        assert!(!standup.admits([2; 32]), "Bo does not");
        assert!(!may("standup", &rules, &persons, [2; 32]).publish);
        assert!(
            standup.admits([3; 32]),
            "a device the members list does not name speaks as anyone does"
        );
        let lounge = speakers("lounge", &rules, &persons, devices);
        assert!(lounge.admits([2; 32]), "in another call Bo speaks");

        let in_call = |channel: &str| {
            Presence::desktop(
                "w",
                Payload::Availability {
                    state: "available".into(),
                    call: Some(channel.to_owned()),
                    camera: false,
                    screen: false,
                },
            )
            .encode()
            .unwrap()
        };
        assert_eq!(
            said_call(&in_call("standup")),
            Some(Some("standup".to_owned()))
        );
        assert_eq!(
            said_call(&[]),
            Some(None),
            "a device that left is in no call"
        );
    }

    /// The flags a call carries ride in its one text and read back; a text without them is a
    /// device that is heard and hears.
    #[test]
    fn a_calls_text_carries_what_the_device_shows() {
        let said = Call {
            call: CallPresence {
                channel: "c_voice".into(),
                name: String::new(),
                bridge: None,
            },
            camera: true,
            screen: false,
            muted: true,
            deafened: false,
        };
        assert_eq!(
            Call::decode(&said.encode(), true, false),
            Some(said.clone())
        );
        let heard = Call {
            muted: false,
            ..said.clone()
        };
        assert_eq!(heard.encode(), "c_voice");
        // A bridge's service and the flags ride on lines of their own.
        let bridged = Call {
            call: CallPresence {
                channel: "c_voice".into(),
                name: "voice".into(),
                bridge: Some("discord".into()),
            },
            deafened: true,
            ..said
        };
        assert_eq!(Call::decode(&bridged.encode(), true, false), Some(bridged));
        assert_eq!(Call::decode("c_voice", true, false), Some(heard));
        let deaf = Call {
            deafened: true,
            ..said
        };
        assert!(
            Call::decode(&deaf.encode(), true, false)
                .is_some_and(|call| call.muted && call.deafened)
        );
        assert_eq!(Call::decode("", false, false), None);
    }
}
