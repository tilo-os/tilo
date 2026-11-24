//! Moving one of the person's Bluetooth accessories from one device of theirs to another, through
//! the record of their devices ([`AccessoryMove`]).
//!
//! A bond belongs to the accessory, not to a device: moving it means the device that has it lets
//! go, and the device it moves to connects it with the bond it already holds. Nothing is copied.
//! The device that has the accessory asks the move ([`asking`]); the one it moves to says whether
//! it holds the bond; the first says it let go; the other says it connected it. Each device reads
//! its part of each move from the record ([`turns`]), does it with its own radio, and writes how
//! it went ([`finished`]).
//!
//! A move that does not finish (the other device leaves, a step fails, or [`MOVE_WAIT_NS`] runs
//! out) puts the accessory back on the device it came from, so it is never left connected to
//! neither: the device that let go connects it again, and the one that connected it after the move
//! was given up lets go again.

use std::collections::BTreeSet;

use tilo_resource::{Id, Invoke};

use super::devices::{AccessoryMove, Devices, MoveStage};

/// How long one move may take before either device gives it up and the accessory goes back where
/// it was.
pub const MOVE_WAIT_NS: i64 = 75_000_000_000;

/// The longest name of an accessory, in bytes.
pub const MAX_NAME: usize = 128;

impl MoveStage {
    /// Whether a move at this stage ended: it moved, or stayed or went back where it was.
    #[must_use]
    pub fn ended(self) -> bool {
        !matches!(self, Self::Asked | Self::Ready | Self::Released)
    }
}

impl AccessoryMove {
    /// Whether the move is still under way at `now_ns`: it did not end, and its time did not run
    /// out.
    #[must_use]
    pub fn under_way(&self, now_ns: i64) -> bool {
        !self.stage.ended() && now_ns.saturating_sub(self.asked_ns) < MOVE_WAIT_NS
    }

    /// Whether a move given up was given up because it took too long.
    #[must_use]
    pub fn ran_out(&self) -> bool {
        self.at_ns.saturating_sub(self.asked_ns) >= MOVE_WAIT_NS
    }
}

/// Why an accessory cannot be moved to the device asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRefusal {
    /// The device it would move to is not one of the person's in the record.
    UnknownDevice,
    /// It would move to the device that has it.
    SameDevice,
    /// It is moving already.
    Moving,
}

impl std::fmt::Display for MoveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownDevice => "that is not one of your devices",
            Self::SameDevice => "the accessory is on that device already",
            Self::Moving => "that accessory is moving already",
        })
    }
}

/// The move of the accessory at `address`, called `name`, from `local` (the device that has it)
/// to `to`, as asked at `now_ns`.
///
/// # Errors
/// Why `record` says it cannot move there.
pub fn asking(
    record: &Devices,
    local: &str,
    address: &str,
    name: &str,
    to: &str,
    now_ns: i64,
) -> Result<Invoke<Devices>, MoveRefusal> {
    if to == local {
        return Err(MoveRefusal::SameDevice);
    }
    if !record.devices.contains(&Id::from_raw(to.to_owned())) {
        return Err(MoveRefusal::UnknownDevice);
    }
    if record
        .moves
        .get(&Id::from_raw(address.to_ascii_uppercase()))
        .is_some_and(|kept| kept.under_way(now_ns))
    {
        return Err(MoveRefusal::Moving);
    }
    Ok(Devices::MOVE_ACCESSORY.with((
        address.to_ascii_uppercase(),
        name.to_owned(),
        local.to_owned(),
        to.to_owned(),
        now_ns,
    )))
}

/// What a device does with its own radio for one move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Radio {
    /// On the device it moves to: check that it holds the accessory's bond.
    Inspect,
    /// On the device that has it: let go of it, keeping its bond.
    Release,
    /// On the device it moves to: connect it, with the bond it holds.
    Take,
    /// On the device it came from: connect it again, because the move did not finish after it
    /// let go.
    GiveBack,
    /// On the device it was to move to: let go of it again, because the move was given up after it
    /// connected it.
    LetGo,
}

/// One piece of radio work a device does for the move of `address` asked at `asked_ns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub address: String,
    pub asked_ns: i64,
    pub radio: Radio,
}

/// What one device took up of the moves in the record, so it does each piece of work once.
#[derive(Debug, Default)]
pub struct Part {
    /// The work it began, and the moves it gave up, by address and when each was asked.
    begun: BTreeSet<(String, i64, Option<Radio>)>,
    /// The moves whose accessory it let go of.
    released: BTreeSet<(String, i64)>,
    /// The moves whose accessory it connected.
    connected: BTreeSet<(String, i64)>,
}

/// What the device `own` does for the moves `record` holds at `now_ns`: the writes it makes (the
/// moves it gives up, because the other device left, which `online` answers by key, or because
/// they ran out of time) and the radio work it begins.
pub fn turns(
    record: &Devices,
    own: &str,
    online: impl Fn(&str) -> bool,
    now_ns: i64,
    part: &mut Part,
) -> (Vec<Invoke<Devices>>, Vec<Turn>) {
    let mut writes = Vec::new();
    let mut work = Vec::new();
    for (address, kept) in record.moves.iter() {
        let origin = kept.from == own;
        if !origin && kept.to != own {
            continue;
        }
        let address = address.as_str();
        let other = if origin { &kept.to } else { &kept.from };
        if !kept.stage.ended() && (!kept.under_way(now_ns) || !online(other)) {
            if part.begun.insert((address.to_owned(), kept.asked_ns, None)) {
                writes.push(Devices::GIVE_UP_MOVE.with((
                    address.to_owned(),
                    kept.asked_ns,
                    now_ns,
                )));
            }
            continue;
        }
        let key = (address.to_owned(), kept.asked_ns);
        let radio = match (kept.stage, origin) {
            (MoveStage::Asked, false) => Some(Radio::Inspect),
            (MoveStage::Ready, true) => Some(Radio::Release),
            (MoveStage::Released, false) => Some(Radio::Take),
            (MoveStage::NotConnected | MoveStage::GivenUp, true)
                if kept.released_ns > 0 || part.released.contains(&key) =>
            {
                Some(Radio::GiveBack)
            }
            (MoveStage::GivenUp, false) if part.connected.contains(&key) => Some(Radio::LetGo),
            _ => None,
        };
        if let Some(radio) = radio
            && part.begun.insert((key.0.clone(), key.1, Some(radio)))
        {
            work.push(Turn {
                address: key.0,
                asked_ns: key.1,
                radio,
            });
        }
    }
    (writes, work)
}

/// What the device writes once `turn` is done, at `now_ns`, given whether its radio `worked`.
pub fn finished(
    turn: &Turn,
    worked: bool,
    part: &mut Part,
    now_ns: i64,
) -> Option<Invoke<Devices>> {
    let address = turn.address.clone();
    let key = (address.clone(), turn.asked_ns);
    match turn.radio {
        Radio::Inspect => {
            Some(Devices::ACCESSORY_READY.with((address, turn.asked_ns, worked, now_ns)))
        }
        Radio::Release => {
            if worked {
                part.released.insert(key);
            }
            Some(Devices::ACCESSORY_RELEASED.with((address, turn.asked_ns, worked, now_ns)))
        }
        Radio::Take => {
            if worked {
                part.connected.insert(key);
            }
            Some(Devices::ACCESSORY_TAKEN.with((address, turn.asked_ns, worked, now_ns)))
        }
        Radio::GiveBack | Radio::LetGo => None,
    }
}

#[cfg(test)]
mod tests {
    use tilo_resource::Replica;

    use super::*;

    const SPEAKER: &str = "AA:BB:CC:DD:EE:01";
    const ASKED: i64 = 1_700_000_000_000_000_000;

    fn desk() -> String {
        "11".repeat(32)
    }

    fn laptop() -> String {
        "22".repeat(32)
    }

    /// A record that keeps the desktop and the laptop.
    fn record() -> Replica<Devices> {
        let mut record = Replica::<Devices>::new();
        for key in [desk(), laptop()] {
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

    fn state(record: &Replica<Devices>) -> Devices {
        record.state().expect("the record reads")
    }

    fn work(turns: &[Turn]) -> Vec<Radio> {
        turns.iter().map(|turn| turn.radio).collect()
    }

    #[test]
    fn a_move_that_runs_out_after_letting_go_gives_the_accessory_back() {
        let (desk, laptop) = (desk(), laptop());
        let mut record = record();
        let ask = asking(
            &state(&record),
            &desk,
            "aa:bb:cc:dd:ee:01",
            "Speaker",
            &laptop,
            ASKED,
        )
        .expect("the speaker can move to the laptop");
        record.apply(ask).expect("the move is asked");
        assert_eq!(
            asking(
                &state(&record),
                &desk,
                SPEAKER,
                "Speaker",
                &laptop,
                ASKED + 1
            )
            .err(),
            Some(MoveRefusal::Moving)
        );
        let (mut on_desk, mut on_laptop) = (Part::default(), Part::default());
        let here = |_: &str| true;

        let (_, inspect) = turns(&state(&record), &laptop, here, ASKED + 1, &mut on_laptop);
        assert_eq!(work(&inspect), [Radio::Inspect]);
        record
            .apply(finished(&inspect[0], true, &mut on_laptop, ASKED + 2).unwrap())
            .unwrap();
        let (_, release) = turns(&state(&record), &desk, here, ASKED + 3, &mut on_desk);
        assert_eq!(work(&release), [Radio::Release]);
        // The desktop let go, and the laptop never says how its connecting went.
        record
            .apply(finished(&release[0], true, &mut on_desk, ASKED + 4).unwrap())
            .unwrap();

        let late = ASKED + MOVE_WAIT_NS;
        let (writes, back) = turns(&state(&record), &desk, here, late, &mut on_desk);
        assert!(back.is_empty(), "the desktop gives the move up first");
        for write in writes {
            record.apply(write).unwrap();
        }
        let kept = state(&record)
            .moves
            .get(&Id::from_raw(SPEAKER.to_owned()))
            .cloned()
            .unwrap();
        assert_eq!(kept.stage, MoveStage::GivenUp);
        assert!(kept.ran_out());
        let (_, back) = turns(&state(&record), &desk, here, late, &mut on_desk);
        assert_eq!(work(&back), [Radio::GiveBack]);
        assert!(
            turns(&state(&record), &desk, here, late, &mut on_desk)
                .1
                .is_empty(),
            "once"
        );
        // The laptop, which connected nothing, has nothing to let go of.
        assert!(
            turns(&state(&record), &laptop, here, late, &mut on_laptop)
                .1
                .is_empty()
        );
        // The accessory can be asked to move again.
        assert!(
            asking(
                &state(&record),
                &desk,
                SPEAKER,
                "Speaker",
                &laptop,
                late + 1
            )
            .is_ok()
        );
    }

    #[test]
    fn a_move_whose_other_device_left_is_given_up() {
        let (desk, laptop) = (desk(), laptop());
        let mut record = record();
        let ask = asking(&state(&record), &desk, SPEAKER, "Speaker", &laptop, ASKED).unwrap();
        record.apply(ask).unwrap();
        let (writes, work) = turns(
            &state(&record),
            &desk,
            |key| key != laptop,
            ASKED + 1,
            &mut Part::default(),
        );
        assert!(work.is_empty());
        assert_eq!(writes.len(), 1);
        for write in writes {
            record.apply(write).unwrap();
        }
        let kept = state(&record)
            .moves
            .get(&Id::from_raw(SPEAKER.to_owned()))
            .cloned()
            .unwrap();
        assert_eq!(kept.stage, MoveStage::GivenUp);
        assert!(!kept.ran_out());
    }
}
