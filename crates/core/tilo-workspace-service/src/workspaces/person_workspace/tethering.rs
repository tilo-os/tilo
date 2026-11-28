//! One of the person's phones sharing its connection with another of their devices for a while,
//! through the record of their devices ([`Tether`]).
//!
//! The device that needs a connection asks a phone of theirs ([`asking`]), for a while at most
//! [`MAX_SHARE_MS`]. The phone asks its own person; on their yes it starts a hotspot under a fresh
//! passphrase, which it shares with the person's devices as every saved network's is shared
//! (sealed to each of their devices, never in the clear), and says it shares it. The device that
//! asked joins it with that passphrase and says whether it joined. Either device ends it, and the
//! phone ends it once its time runs out; a share nobody answered in time, or whose other device
//! left, is given up. Each device reads its part of each share from the record ([`turns`]), does
//! it with its own radio, and writes how it went ([`finished`]).
//!
//! The person consents on both devices: they ask on the one that needs a connection, and answer on
//! the phone. A local hotspot the person turns on themselves is the network settings' own switch,
//! not this.

use std::collections::BTreeSet;

use tilo_resource::{Id, Invoke};

use super::devices::{Devices, Tether, TetherStage};

/// The longest a phone shares its connection for one ask.
pub const MAX_SHARE_MS: i64 = 2 * 60 * 60 * 1_000;
/// How long a share is asked for when the person does not say: one hour.
pub const SHARE_MS: i64 = 60 * 60 * 1_000;
/// How long the phone's person has to answer before the ask is given up.
pub const ASK_WAIT_MS: i64 = 60_000;
/// How long the device that asked has to join a hotspot the phone started before the share is
/// given up: a hotspot takes its while to come up, and a join its own.
pub const JOIN_WAIT_MS: i64 = 120_000;
/// The longest a hotspot's network name may be: what Wi-Fi carries.
pub const MAX_NETWORK: usize = 32;

impl TetherStage {
    /// Whether a share at this stage ended: it was refused, stopped, given up or failed.
    #[must_use]
    pub fn ended(self) -> bool {
        !matches!(self, Self::Asked | Self::Shared | Self::Joined)
    }
}

impl Tether {
    /// Whether the share is under way at `now_ms`: it did not end, and its time did not run out.
    #[must_use]
    pub fn under_way(&self, now_ms: i64) -> bool {
        !self.stage.ended() && now_ms < self.until_ms
    }
}

/// Why a phone cannot be asked to share its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TetherRefusal {
    /// The phone is not one of the person's devices in the record.
    UnknownDevice,
    /// The device would ask itself.
    SameDevice,
    /// The device uses a phone's connection already.
    Sharing,
}

impl std::fmt::Display for TetherRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownDevice => "that is not one of your devices",
            Self::SameDevice => "a device cannot share its connection with itself",
            Self::Sharing => "this device uses a phone's connection already",
        })
    }
}

/// The ask that the device `local` makes of the phone `phone` at `now_ms`, to share its
/// connection for `for_ms`.
///
/// # Errors
/// Why `record` says it cannot be asked.
pub fn asking(
    record: &Devices,
    local: &str,
    phone: &str,
    now_ms: i64,
    for_ms: i64,
) -> Result<Invoke<Devices>, TetherRefusal> {
    if phone == local {
        return Err(TetherRefusal::SameDevice);
    }
    if !record.devices.contains(&Id::from_raw(phone.to_owned())) {
        return Err(TetherRefusal::UnknownDevice);
    }
    if record
        .tethers
        .get(&Id::from_raw(local.to_owned()))
        .is_some_and(|kept| kept.under_way(now_ms))
    {
        return Err(TetherRefusal::Sharing);
    }
    Ok(Devices::ASK_TETHER.with((
        local.to_owned(),
        phone.to_owned(),
        now_ms,
        now_ms.saturating_add(for_ms.clamp(1, MAX_SHARE_MS)),
    )))
}

/// The action that ends the share `kept` with `device` at `now_ms`, as either device stops it.
#[must_use]
pub fn ending(device: &str, kept: &Tether, now_ms: i64) -> Invoke<Devices> {
    Devices::END_TETHER.with((device.to_owned(), kept.asked_ms, true, now_ms))
}

/// What a device does with its own radio, or asks its person, for one share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// On the phone: ask its person whether to share its connection.
    Ask,
    /// On the device that asked: join the hotspot the phone shares.
    Join,
    /// On the phone: stop the hotspot it started.
    StopSharing,
    /// On the device that asked: leave the hotspot it joined.
    Leave,
}

/// One piece of work a device does for the share with `device` asked at `asked_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The device the phone shares with, by key.
    pub device: String,
    /// The phone, by key.
    pub phone: String,
    pub asked_ms: i64,
    /// The hotspot, once the phone shares one.
    pub network: String,
    pub step: Step,
}

/// What one device took up of the shares in the record, so it does each piece of work once.
#[derive(Debug, Default)]
pub struct Part {
    /// The work it began and the shares it wrote an end of, by device, ask and step.
    begun: BTreeSet<(String, i64, Option<Step>)>,
    /// The shares whose hotspot this phone started.
    started: BTreeSet<(String, i64)>,
    /// The shares whose hotspot this device joined.
    joined: BTreeSet<(String, i64)>,
}

/// What the device `own` does for the shares `record` holds at `now_ms`: the writes it makes (the
/// shares it gives up, because nobody answered in time or the other device left, which `online`
/// answers by key, and the ones whose time ran out) and the work it begins.
pub fn turns(
    record: &Devices,
    own: &str,
    online: impl Fn(&str) -> bool,
    now_ms: i64,
    part: &mut Part,
) -> (Vec<Invoke<Devices>>, Vec<Turn>) {
    let mut writes = Vec::new();
    let mut work = Vec::new();
    for (device, kept) in record.tethers.iter() {
        let device = device.as_str();
        let asking = device == own;
        if !asking && kept.phone != own {
            continue;
        }
        let other = if asking { kept.phone.as_str() } else { device };
        let key = (device.to_owned(), kept.asked_ms);
        if !kept.stage.ended() {
            let late = match kept.stage {
                TetherStage::Asked => now_ms >= kept.asked_ms.saturating_add(ASK_WAIT_MS),
                TetherStage::Shared => now_ms >= kept.at_ms.saturating_add(JOIN_WAIT_MS),
                _ => false,
            };
            let ran_out = now_ms >= kept.until_ms;
            if (late || ran_out || !online(other))
                && part.begun.insert((key.0.clone(), key.1, None))
            {
                writes.push(Devices::END_TETHER.with((
                    key.0.clone(),
                    kept.asked_ms,
                    ran_out && !late,
                    now_ms,
                )));
            }
            if late || ran_out || !online(other) {
                // What it began is undone on the pass that reads the end.
                continue;
            }
        }
        let step = match (kept.stage, asking) {
            (TetherStage::Asked, false) => Some(Step::Ask),
            (TetherStage::Shared, true) => Some(Step::Join),
            (stage, false) if stage.ended() && part.started.contains(&key) => {
                Some(Step::StopSharing)
            }
            (stage, true) if stage.ended() && part.joined.contains(&key) => Some(Step::Leave),
            _ => None,
        };
        if let Some(step) = step
            && part.begun.insert((key.0.clone(), key.1, Some(step)))
        {
            work.push(Turn {
                device: key.0,
                phone: kept.phone.clone(),
                asked_ms: key.1,
                network: kept.network.clone(),
                step,
            });
        }
    }
    (writes, work)
}

/// How a piece of a share's work went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The phone's person said no.
    Declined,
    /// The phone's person said yes and its hotspot `network` is on.
    Started { network: String },
    /// The phone's person said yes, and its hotspot did not start.
    NotStarted,
    /// The device joined the hotspot, or did not.
    Joined(bool),
    /// The hotspot stopped, or the device left it.
    Stopped,
}

/// What the device writes once `turn` is done with `outcome`, at `now_ms`.
pub fn finished(
    turn: &Turn,
    outcome: &Outcome,
    part: &mut Part,
    now_ms: i64,
) -> Option<Invoke<Devices>> {
    let key = (turn.device.clone(), turn.asked_ms);
    let answer = |network: &str, declined: bool| {
        Devices::ANSWER_TETHER.with((
            turn.device.clone(),
            turn.asked_ms,
            network.to_owned(),
            declined,
            now_ms,
        ))
    };
    match outcome {
        Outcome::Declined => Some(answer("", true)),
        Outcome::NotStarted => Some(answer("", false)),
        Outcome::Started { network } => {
            part.started.insert(key);
            Some(answer(network, false))
        }
        Outcome::Joined(joined) => {
            if *joined {
                part.joined.insert(key);
            }
            Some(Devices::TETHER_JOINED.with((turn.device.clone(), turn.asked_ms, *joined, now_ms)))
        }
        Outcome::Stopped => {
            part.started.remove(&key);
            part.joined.remove(&key);
            None
        }
    }
}

/// The hotspot name a phone called `label` shares under: its name, printable and short enough for
/// Wi-Fi to carry.
#[must_use]
pub fn network_name(label: &str) -> String {
    let mut name: String = label
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned();
    if name.is_empty() {
        "Tilo".clone_into(&mut name);
    }
    while name.len() > MAX_NETWORK {
        name.pop();
    }
    name
}

#[cfg(test)]
mod tests {
    use tilo_resource::{AppNamed, Replica};

    use super::*;

    const ASKED: i64 = 1_700_000_000_000;

    fn laptop() -> String {
        "22".repeat(32)
    }

    fn phone() -> String {
        "33".repeat(32)
    }

    /// A record that keeps the laptop and the phone.
    fn record() -> Replica<Devices, AppNamed> {
        let mut record = Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for key in [laptop(), phone()] {
            record
                .apply(Devices::KEEP_FACT.with((
                    key,
                    "00".to_owned(),
                    "seen".to_owned(),
                    "00".to_owned(),
                )))
                .expect("the record keeps the device");
        }
        record
    }

    fn state(record: &Replica<Devices, AppNamed>) -> Devices {
        record.state().expect("the record reads")
    }

    fn kept(record: &Replica<Devices, AppNamed>) -> Tether {
        state(record)
            .tethers
            .get(&Id::from_raw(laptop()))
            .cloned()
            .expect("the share is kept")
    }

    fn steps(turns: &[Turn]) -> Vec<Step> {
        turns.iter().map(|turn| turn.step).collect()
    }

    /// The laptop asks, the phone's person says yes and its hotspot comes up, the laptop joins
    /// it, and once the share's time runs out the phone stops its hotspot and the laptop leaves:
    /// each device does its part once, and nothing else of the other's.
    #[test]
    fn a_shared_connection_is_joined_and_taken_down_when_its_time_runs_out() {
        let (laptop, phone) = (laptop(), phone());
        let mut record = record();
        let here = |_: &str| true;
        let (mut on_laptop, mut on_phone) = (Part::default(), Part::default());
        record
            .apply(asking(&state(&record), &laptop, &phone, ASKED, SHARE_MS).unwrap())
            .unwrap();
        assert_eq!(
            asking(&state(&record), &laptop, &phone, ASKED + 1, SHARE_MS).err(),
            Some(TetherRefusal::Sharing),
            "one share at a time"
        );
        assert!(
            turns(&state(&record), &laptop, here, ASKED + 1, &mut on_laptop)
                .1
                .is_empty(),
            "the laptop waits for the phone's person"
        );
        let (_, ask) = turns(&state(&record), &phone, here, ASKED + 1, &mut on_phone);
        assert_eq!(steps(&ask), [Step::Ask]);
        assert!(
            turns(&state(&record), &phone, here, ASKED + 2, &mut on_phone)
                .1
                .is_empty(),
            "the phone asks its person once"
        );
        let started = Outcome::Started {
            network: network_name("Ada's phone"),
        };
        record
            .apply(finished(&ask[0], &started, &mut on_phone, ASKED + 3).unwrap())
            .unwrap();
        assert_eq!(kept(&record).stage, TetherStage::Shared);
        assert_eq!(kept(&record).network, "Ada's phone");

        let (_, join) = turns(&state(&record), &laptop, here, ASKED + 4, &mut on_laptop);
        assert_eq!(steps(&join), [Step::Join]);
        assert_eq!(join[0].network, "Ada's phone");
        record
            .apply(finished(&join[0], &Outcome::Joined(true), &mut on_laptop, ASKED + 5).unwrap())
            .unwrap();
        assert_eq!(kept(&record).stage, TetherStage::Joined);

        let late = ASKED + SHARE_MS;
        let (writes, work) = turns(&state(&record), &phone, here, late, &mut on_phone);
        assert!(
            work.is_empty(),
            "the end is written before the hotspot stops"
        );
        assert_eq!(writes.len(), 1);
        for write in writes {
            record.apply(write).unwrap();
        }
        assert_eq!(kept(&record).stage, TetherStage::Ended);
        let (_, stop) = turns(&state(&record), &phone, here, late, &mut on_phone);
        assert_eq!(steps(&stop), [Step::StopSharing]);
        let (_, leave) = turns(&state(&record), &laptop, here, late, &mut on_laptop);
        assert_eq!(steps(&leave), [Step::Leave]);
        assert!(
            turns(&state(&record), &phone, here, late + 1, &mut on_phone)
                .1
                .is_empty(),
            "once"
        );
        // The laptop may ask again.
        assert!(asking(&state(&record), &laptop, &phone, late + 1, SHARE_MS).is_ok());
    }

    /// A phone whose person said no starts nothing, an ask nobody answers is given up, and a
    /// share whose other device left is given up and its hotspot taken down.
    #[test]
    fn a_declined_unanswered_or_abandoned_share_leaves_no_hotspot_up() {
        let (laptop, phone) = (laptop(), phone());
        let here = |_: &str| true;

        let mut declined = record();
        declined
            .apply(asking(&state(&declined), &laptop, &phone, ASKED, SHARE_MS).unwrap())
            .unwrap();
        let mut on_phone = Part::default();
        let (_, ask) = turns(&state(&declined), &phone, here, ASKED + 1, &mut on_phone);
        declined
            .apply(finished(&ask[0], &Outcome::Declined, &mut on_phone, ASKED + 2).unwrap())
            .unwrap();
        assert_eq!(kept(&declined).stage, TetherStage::Declined);
        assert!(
            turns(&state(&declined), &phone, here, ASKED + 3, &mut on_phone)
                .1
                .is_empty(),
            "no hotspot to stop"
        );

        let mut unanswered = record();
        unanswered
            .apply(asking(&state(&unanswered), &laptop, &phone, ASKED, SHARE_MS).unwrap())
            .unwrap();
        let (writes, _) = turns(
            &state(&unanswered),
            &laptop,
            here,
            ASKED + ASK_WAIT_MS,
            &mut Part::default(),
        );
        for write in writes {
            unanswered.apply(write).unwrap();
        }
        assert_eq!(kept(&unanswered).stage, TetherStage::GivenUp);

        let mut left = record();
        left.apply(asking(&state(&left), &laptop, &phone, ASKED, SHARE_MS).unwrap())
            .unwrap();
        let mut on_phone = Part::default();
        let (_, ask) = turns(&state(&left), &phone, here, ASKED + 1, &mut on_phone);
        let started = Outcome::Started {
            network: "Phone".to_owned(),
        };
        left.apply(finished(&ask[0], &started, &mut on_phone, ASKED + 2).unwrap())
            .unwrap();
        let gone = |key: &str| key != laptop;
        let (writes, work) = turns(&state(&left), &phone, gone, ASKED + 3, &mut on_phone);
        assert!(work.is_empty());
        for write in writes {
            left.apply(write).unwrap();
        }
        assert_eq!(kept(&left).stage, TetherStage::GivenUp);
        let (_, stop) = turns(&state(&left), &phone, gone, ASKED + 4, &mut on_phone);
        assert_eq!(steps(&stop), [Step::StopSharing]);
    }

    /// The record refuses a share with no phone of the person's, with itself, past two hours, or
    /// an answer that is not the phone's.
    #[test]
    fn the_record_keeps_only_shares_between_two_of_the_persons_devices() {
        let (laptop, phone) = (laptop(), phone());
        let mut record = record();
        let stranger = "44".repeat(32);
        assert_eq!(
            asking(&state(&record), &laptop, &stranger, ASKED, SHARE_MS).err(),
            Some(TetherRefusal::UnknownDevice)
        );
        assert_eq!(
            asking(&state(&record), &laptop, &laptop, ASKED, SHARE_MS).err(),
            Some(TetherRefusal::SameDevice)
        );
        assert!(
            record
                .apply(Devices::ASK_TETHER.with((
                    laptop.clone(),
                    phone.clone(),
                    ASKED,
                    ASKED + MAX_SHARE_MS + 1,
                )))
                .is_err()
        );
        record
            .apply(asking(&state(&record), &laptop, &phone, ASKED, SHARE_MS).unwrap())
            .unwrap();
        assert!(
            record
                .apply(Devices::ANSWER_TETHER.with((
                    laptop.clone(),
                    ASKED,
                    "Tilo".to_owned(),
                    true,
                    ASKED + 1,
                )))
                .is_err(),
            "a phone that declines shares no hotspot"
        );
        assert!(
            record
                .apply(Devices::ANSWER_TETHER.with((
                    laptop,
                    ASKED,
                    "x".repeat(MAX_NETWORK + 1),
                    false,
                    ASKED + 1,
                )))
                .is_err(),
            "a hotspot is named in at most 32 bytes"
        );
        assert_eq!(network_name("\u{7}  "), "Tilo");
        assert_eq!(network_name(&"x".repeat(40)).len(), MAX_NETWORK);
    }
}
