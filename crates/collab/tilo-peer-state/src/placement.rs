//! Choosing which of a workspace's other devices runs a request this device
//! does not run itself.
//!
//! A request this device can serve locally is served locally; otherwise the
//! devices that may run work for the person answer with an offer each, and
//! the requester picks one. The choice is a routing preference, never
//! authority: the device that is picked still decides the request through its
//! own dispatcher, against the caller it authenticated.
//!
//! Ranking, best first: the operator [`priority`](State::priority) (higher
//! wins, as a person's preference precedes load balancing), then power
//! (external, charging, unknown, battery), then load as a fraction of
//! capacity, then the measured round trip, then the device key so equal
//! candidates always resolve the same way. A device that would rather not
//! says why instead ([`Decline`]), and the requester keeps what it knows
//! about every device it asked.

use serde::{Deserialize, Serialize};

/// How a device is powered, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    /// Stable external power.
    External,
    Charging,
    /// No reliable observation.
    Unknown,
    /// Running from a battery above the critical threshold.
    Battery,
}

/// Why a device will not run the request it was offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decline {
    /// The device that asked. It is never its own remote worker: work with
    /// no other willing device runs where it was asked, or not at all.
    Requester,
    /// Not connected, so the offer never came back.
    Offline,
    /// A store keeps ciphertext and holds no keys: it never runs work.
    StoreOnly,
    /// It does not hold the workspace the request names.
    WorkspaceNotHeld,
    /// It cannot run the app the request's action belongs to.
    AppNotInstalled,
    /// Its person asked the device to avoid discretionary work.
    DataSaving,
    /// Its battery is critically low.
    CriticalBattery,
}

impl Decline {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Requester => "requester",
            Self::Offline => "offline",
            Self::StoreOnly => "store_only",
            Self::WorkspaceNotHeld => "workspace_not_held",
            Self::AppNotInstalled => "app_not_installed",
            Self::DataSaving => "data_saving",
            Self::CriticalBattery => "critical_battery",
        }
    }
}

/// What a device says of itself when it is offered work: the answer to one
/// offer, from its own state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact the device reports, not a mode"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Whether this is the device that asked.
    pub requester: bool,
    /// Whether it is connected right now.
    pub online: bool,
    /// Whether it holds the workspace's keys and state.
    pub holds_workspace: bool,
    /// Whether it can run the request's app.
    pub app_installed: bool,
    /// Whether it is a store of the workspace rather than a participant.
    pub store_role: bool,
    /// Its person asked it to avoid discretionary work.
    pub data_saving: bool,
    /// Bumped-out-of-shape low battery: not while there is anything else.
    pub critical_battery: bool,
    /// Operator preference; higher wins.
    pub priority: i32,
    pub power: Power,
    /// Work already in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip to the requester; lower wins.
    pub latency_ms: u32,
}

impl State {
    /// What this device ranks by, or why it declines.
    ///
    /// # Errors
    ///
    /// The first rule it fails, in the order a person would state them.
    pub fn verdict(&self) -> Result<Rank, Decline> {
        let decline = if self.requester {
            Decline::Requester
        } else if !self.online {
            Decline::Offline
        } else if self.store_role {
            Decline::StoreOnly
        } else if !self.holds_workspace {
            Decline::WorkspaceNotHeld
        } else if !self.app_installed {
            Decline::AppNotInstalled
        } else if self.data_saving {
            Decline::DataSaving
        } else if self.critical_battery {
            Decline::CriticalBattery
        } else {
            return Ok(Rank {
                priority: self.priority,
                power: self.power,
                load_permille: self.load_permille,
                latency_ms: self.latency_ms,
            });
        };
        Err(decline)
    }
}

/// What a willing device was ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    /// Operator preference; higher wins.
    pub priority: i32,
    /// Power standing; better wins.
    pub power: Power,
    /// Work in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip; lower wins.
    pub latency_ms: u32,
}

impl Rank {
    /// Sort key: smaller is better.
    fn key(&self, device: [u8; 32]) -> (std::cmp::Reverse<i32>, Power, u32, u32, [u8; 32]) {
        (
            std::cmp::Reverse(self.priority),
            self.power,
            self.load_permille,
            self.latency_ms,
            device,
        )
    }
}

/// One device that was asked, and what the requester made of its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The device's public key.
    pub device: [u8; 32],
    /// Its rank when it offered, or why it declined.
    pub verdict: Result<Rank, Decline>,
}

/// The device a request goes to, and what it won on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Choice {
    /// The willing device that won, if any did.
    pub worker: Option<[u8; 32]>,
    /// What it won on.
    pub rank: Option<Rank>,
    /// Every device that was asked, willing ones best first, then the ones
    /// that declined by device key.
    pub considered: Vec<Candidate>,
}

impl Choice {
    /// Whether nothing offered, so the request runs where it was asked or
    /// not at all.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.worker.is_none()
    }
}

impl std::fmt::Display for Choice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some((worker, rank)) = self.worker.zip(self.rank) else {
            formatter.write_str("no device offered to run it")?;
            for candidate in &self.considered {
                if let Err(reason) = candidate.verdict {
                    write!(
                        formatter,
                        "; {}: {}",
                        short(&candidate.device),
                        reason.token()
                    )?;
                }
            }
            return Ok(());
        };
        let offered = self
            .considered
            .iter()
            .filter(|candidate| candidate.verdict.is_ok())
            .count();
        write!(
            formatter,
            "{} (priority {}, power {:?}, load {}‰, latency {} ms) of {offered} that offered among {} asked",
            short(&worker),
            rank.priority,
            rank.power,
            rank.load_permille,
            rank.latency_ms,
            self.considered.len(),
        )
    }
}

/// The first four bytes of a device key, in hex, for log lines.
fn short(device: &[u8; 32]) -> String {
    hex::encode(device.get(..4).unwrap_or_default())
}

/// Pick the device a request goes to among the ones that were asked.
#[must_use]
pub fn pick(answers: impl IntoIterator<Item = ([u8; 32], State)>) -> Choice {
    let mut considered = answers
        .into_iter()
        .map(|(device, state)| Candidate {
            device,
            verdict: state.verdict(),
        })
        .collect::<Vec<_>>();
    considered.sort_by(|left, right| match (&left.verdict, &right.verdict) {
        (Ok(a), Ok(b)) => a.key(left.device).cmp(&b.key(right.device)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.device.cmp(&right.device),
    });
    let won = considered
        .first()
        .and_then(|best| best.verdict.ok().map(|rank| (best.device, rank)));
    Choice {
        worker: won.map(|(device, _)| device),
        rank: won.map(|(_, rank)| rank),
        considered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUESTER: [u8; 32] = [9; 32];

    fn worker() -> State {
        State {
            requester: false,
            online: true,
            holds_workspace: true,
            app_installed: true,
            store_role: false,
            data_saving: false,
            critical_battery: false,
            priority: 0,
            power: Power::External,
            load_permille: 0,
            latency_ms: 20,
        }
    }

    fn chosen(answers: &[(u8, State)]) -> Option<[u8; 32]> {
        pick(answers.iter().map(|(seed, state)| ([*seed; 32], *state))).worker
    }

    /// A device with no power left, or one whose person asked it to save
    /// energy, is passed over whatever its rank would have been.
    #[test]
    fn a_worker_that_would_rather_not_is_never_chosen() {
        let mut drained = worker();
        drained.critical_battery = true;
        drained.priority = 100;
        let mut saving = worker();
        saving.data_saving = true;
        saving.priority = 100;
        let mut desktop = worker();
        desktop.load_permille = 750;
        desktop.latency_ms = 400;

        assert_eq!(chosen(&[(1, drained), (2, saving)]), None);
        assert_eq!(
            chosen(&[(1, drained), (2, saving), (3, desktop)]),
            Some([3; 32])
        );

        let asked = pick([([1; 32], drained), ([2; 32], saving)]);
        assert_eq!(
            asked
                .considered
                .iter()
                .map(|candidate| (candidate.device[0], candidate.verdict))
                .collect::<Vec<_>>(),
            vec![
                (1, Err(Decline::CriticalBattery)),
                (2, Err(Decline::DataSaving)),
            ]
        );
        assert!(asked.is_local());
        assert_eq!(
            asked.to_string(),
            "no device offered to run it; 01010101: critical_battery; 02020202: data_saving"
        );
    }

    /// The cheapest healthy device wins: power, then load, then latency, and
    /// a person's preference before all of them.
    #[test]
    fn the_lowest_cost_healthy_device_is_chosen() {
        let bust = {
            let mut bust = worker();
            bust.load_permille = 750;
            bust
        };
        let idle_far = {
            let mut far = worker();
            far.latency_ms = 90;
            far
        };
        let idle_near = worker();
        let on_battery = {
            let mut battery = worker();
            battery.power = Power::Battery;
            battery.latency_ms = 1;
            battery
        };
        let choice = pick([
            ([1; 32], bust),
            ([2; 32], idle_far),
            ([3; 32], idle_near),
            ([4; 32], on_battery),
        ]);
        assert_eq!(choice.worker, Some([3; 32]));
        assert_eq!(
            choice
                .considered
                .iter()
                .map(|candidate| candidate.device[0])
                .collect::<Vec<_>>(),
            vec![3, 2, 1, 4],
            "power, then load, then latency"
        );
        assert_eq!(
            choice.to_string(),
            "03030303 (priority 0, power External, load 0‰, latency 20 ms) of 4 that offered among 4 asked"
        );

        let mut preferred = worker();
        preferred.priority = 1;
        preferred.power = Power::Charging;
        preferred.load_permille = 500;
        assert_eq!(
            chosen(&[(3, worker()), (5, preferred)]),
            Some([5; 32]),
            "a person's preference comes before everything else"
        );
    }

    /// Equal devices resolve the same way whatever order they answered in.
    #[test]
    fn equal_workers_resolve_the_same_way_whatever_their_order() {
        assert_eq!(chosen(&[(4, worker()), (2, worker())]), Some([2; 32]));
        assert_eq!(chosen(&[(2, worker()), (4, worker())]), Some([2; 32]));
    }

    /// Each rule passes a device over with its own reason, in the order a
    /// person would state them.
    #[test]
    fn each_rule_passes_a_device_over_with_its_reason() {
        type Change = fn(&mut State);
        let cases: [(Change, Decline); 6] = [
            (|w| w.requester = true, Decline::Requester),
            (|w| w.online = false, Decline::Offline),
            (|w| w.store_role = true, Decline::StoreOnly),
            (|w| w.holds_workspace = false, Decline::WorkspaceNotHeld),
            (|w| w.app_installed = false, Decline::AppNotInstalled),
            (|w| w.data_saving = true, Decline::DataSaving),
        ];
        for (change, reason) in cases {
            let mut state = worker();
            change(&mut state);
            assert_eq!(state.verdict(), Err(reason), "{reason:?}");
        }
        let mut asked = worker();
        asked.requester = true;
        assert!(
            pick([(REQUESTER, asked)]).is_local(),
            "the device that asked never offers to itself"
        );
    }

    /// Nothing asked means nothing offers, and the request stays where it
    /// was asked.
    #[test]
    fn nothing_asked_says_so() {
        let choice = pick([]);
        assert!(choice.is_local());
        assert!(choice.considered.is_empty());
        assert_eq!(choice.to_string(), "no device offered to run it");
    }
}
