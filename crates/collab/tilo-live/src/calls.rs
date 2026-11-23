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
//! owner or a moderator ended the call for, or denied, is not in it. Every
//! member's live session holds the call to it ([`Gate`]): it neither opens nor
//! sends the media of someone the call does not let speak, nor opens anything
//! of it for someone it does not let listen, and the channel lists here leave out
//! whoever it does not let in.

use std::collections::{BTreeMap, BTreeSet};

pub use collab_types::calls::{CallRule, Directions};
use collab_types::presence::{Payload, Presence};

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
            .filter_map(|caller| caller.bridge.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The person each device in the call is, for the devices whose person is known: whose
    /// pictures a device's frames are. A bridge sends no pictures and is nobody's.
    #[must_use]
    pub fn persons(&self) -> BTreeMap<Device, String> {
        self.people_in()
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
                person: caller
                    .user
                    .clone()
                    .or_else(|| persons.get(&device).cloned()),
                camera: caller.camera,
                screen: caller.screen,
                muted: caller.muted,
                deafened: caller.deafened,
                bridge: caller.bridge.clone(),
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

/// What a workspace's calls allow, set against where each of its devices says it is: what a
/// live session opens and sends a call's media by ([`crate::live_session::LiveSession`]).
///
/// A device's call is what its presence last said ([`Self::said`]); a device that said none is in
/// no call, and nothing here judges its media. Each device is the person the workspace's
/// members list names it ([`Self::set`]).
#[derive(Clone, Debug, Default)]
pub struct Gate {
    rules: BTreeMap<String, CallRule>,
    persons: BTreeMap<Device, String>,
    channels: BTreeMap<Device, String>,
}

impl Gate {
    /// Hold the calls to `rules` (by channel) from now on, each device being the person `persons`
    /// names.
    pub fn set(&mut self, rules: BTreeMap<String, CallRule>, persons: BTreeMap<Device, String>) {
        self.rules = rules;
        self.persons = persons;
    }

    /// Take one presence object `device` said (`body` as its presence track carries it; empty
    /// when it left): the call it says it is in, if it says one.
    pub fn said(&mut self, device: Device, body: &[u8]) {
        if body.is_empty() {
            self.channels.remove(&device);
            return;
        }
        let Some(Presence {
            payload: Payload::Availability { call, .. },
            ..
        }) = Presence::decode(body)
        else {
            return;
        };
        match call
            .as_deref()
            .and_then(|call| Call::decode(call, false, false))
        {
            Some(call) => {
                self.channels.insert(device, call.call.channel);
            }
            None => {
                self.channels.remove(&device);
            }
        }
    }

    /// What `device` may do in the call it says it is in: everything outside a call.
    #[must_use]
    pub fn may(&self, device: Device) -> Directions {
        let Some(channel) = self.channels.get(&device) else {
            return Directions::BOTH;
        };
        self.rules.get(channel).map_or(Directions::BOTH, |rule| {
            rule.may(self.persons.get(&device).map(String::as_str))
        })
    }

    /// Whether `own` opens the call media `sender` sent: the sender may speak in its call, and
    /// `own` may listen in its own.
    #[must_use]
    pub fn opens(&self, sender: Device, own: Device) -> bool {
        self.may(sender).publish && self.may(own).subscribe
    }
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

        let in_call = |channel: &str| {
            collab_types::presence::Presence::desktop(
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
        let mut gate = Gate::default();
        gate.set(
            BTreeMap::from([(
                "standup".to_owned(),
                CallRule {
                    denied: BTreeMap::from([("bo".to_owned(), Directions::BOTH)]),
                    ..CallRule::default()
                },
            )]),
            BTreeMap::from([([1; 32], "ada".to_owned()), ([2; 32], "bo".to_owned())]),
        );
        gate.said([1; 32], &in_call("standup"));
        gate.said([2; 32], &in_call("standup"));
        assert!(gate.opens([1; 32], [3; 32]), "Ada is heard");
        assert!(!gate.opens([2; 32], [3; 32]), "Bo is not");
        assert!(!gate.opens([1; 32], [2; 32]), "and hears nothing of it");
        gate.said([2; 32], &in_call("lounge"));
        assert!(gate.opens([2; 32], [1; 32]), "in another call Bo is heard");
        gate.said([2; 32], &[]);
        assert!(gate.opens([2; 32], [1; 32]));
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
