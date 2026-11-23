//! Choosing which of a workspace's other devices runs a request this device
//! does not run itself.
//!
//! A request this device can serve locally is served locally; otherwise the
//! devices that may run work for the person answer with an offer each, and
//! the requester picks one. The choice is a routing preference, never
//! authority: the device that is picked still decides the request through its
//! own dispatcher, against the caller it authenticated.
//!
//! Ranking, best first: power (external, charging, unknown, battery), then
//! load as a fraction of capacity, then the measured round trip, then the
//! device key so equal candidates always resolve the same way. A device that
//! does not take the work says why instead ([`Decline`]), and the requester
//! keeps what it knows about every device it asked.
//!
//! Before rank decides, the job's inputs do: a device is picked only when
//! moving what it does not already hold there beats running the job where it
//! was asked ([`transfer`]), the rule compute placement used to weigh on the
//! server. Measured rank order decides between the devices that pass, so the
//! cheapest transfer is not a preference of its own.

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
    /// Running from a battery.
    Battery,
}

/// Why a device does not take a piece of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decline {
    /// Not connected, so it never answered.
    Offline,
    /// It cannot run the app the work's action belongs to.
    AppNotInstalled,
    /// Its person's work setting does not take this work.
    NotTakingWork,
    /// Its setting takes work only while charging, and it runs on battery.
    OnBattery,
    /// Its setting takes work only with more of the machine free than it has.
    LowResources,
    /// It is running as many jobs as it takes on at once.
    AtCapacity,
}

impl Decline {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::AppNotInstalled => "app_not_installed",
            Self::NotTakingWork => "not_taking_work",
            Self::OnBattery => "on_battery",
            Self::LowResources => "low_resources",
            Self::AtCapacity => "at_capacity",
        }
    }
}

/// What a willing device is ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    /// Power level; better wins.
    pub power: Power,
    /// Work already in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip to the requester; lower wins. The requester sets
    /// it: it is the round trip it saw, not the device's own figure.
    pub latency_ms: u32,
}

/// One device's answer: what it is ranked by, or why it does not take the
/// work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// What it said of the work.
    pub verdict: Result<Rank, Decline>,
    /// The digest of each of the job's inputs it already holds, so moving
    /// them is no part of the choice ([`Work::inputs`]).
    pub holds: Vec<[u8; 32]>,
}

impl Answer {
    /// A device that takes the work, ranked by what it reported.
    #[must_use]
    pub fn taking(rank: Rank, holds: Vec<[u8; 32]>) -> Self {
        Self {
            verdict: Ok(rank),
            holds,
        }
    }

    /// A device that does not take the work, `decline` being why.
    #[must_use]
    pub fn declining(decline: Decline) -> Self {
        Self {
            verdict: Err(decline),
            holds: Vec::new(),
        }
    }

    /// What a device that never answered is taken to have said: it is not
    /// there, and it holds none of the job's inputs.
    #[must_use]
    pub fn away() -> Self {
        Self::declining(Decline::Offline)
    }
}

/// What a piece of work would read, and how long the device that asked last took
/// for it: the cost of moving the work weighed against the cost of running
/// it where it was asked.
///
/// An answering device says which of these it already holds, so an input it
/// has is not weighed as moving; the device that asked names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    /// The distinct inputs, by digest, each with its bytes; their bytes are
    /// part of `input_bytes`.
    pub inputs: Vec<([u8; 32], u64)>,
    /// Every input's bytes, whether or not it was named above.
    pub input_bytes: u64,
    /// How long the device that asked last took for this action over
    /// inputs like these, which moving it has to beat; `None` before it
    /// has run it.
    pub local_wall_ms: Option<u64>,
}

impl Work {
    /// What runs where it was asked: reading nothing, never offloaded.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

/// How fast a byte moves between two devices while nothing has measured
/// their link: what a pair with no transfer history is taken to manage.
pub const ASSUMED_BYTES_PER_SECOND: u64 = 8 * 1024 * 1024;

/// What moving a job's inputs to one device costs, given the round trip
/// measured to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Transfer {
    /// The bytes that device does not already hold.
    pub moving_bytes: u64,
    /// How long moving them should take, round trip included.
    pub moving_ms: u64,
}

/// What moving `work`'s inputs to the device that answered `answer` costs,
/// `latency_ms` being the round trip measured to it.
///
/// An input the device already holds does not move; an input the job did not
/// name is weighed as `input_bytes` alone, which every device is taken to
/// receive.
#[must_use]
pub fn transfer(answer: &Answer, work: &Work, latency_ms: u32) -> Transfer {
    let listed = work
        .inputs
        .iter()
        .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes));
    let mut moving_bytes = work.input_bytes.saturating_sub(listed);
    for (digest, bytes) in &work.inputs {
        if !answer.holds.contains(digest) {
            moving_bytes = moving_bytes.saturating_add(*bytes);
        }
    }
    let moving_ms = moving_bytes
        .saturating_mul(1_000)
        .div_ceil(ASSUMED_BYTES_PER_SECOND)
        .saturating_add(u64::from(latency_ms));
    Transfer {
        moving_bytes,
        moving_ms,
    }
}

/// One device that was asked, and what the requester made of its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The device's public key.
    pub device: [u8; 32],
    /// What it answered: what it is ranked by, or why it declines.
    pub verdict: Result<Rank, Decline>,
    /// What moving the job's inputs to it costs, for a device that offered.
    pub transfer: Option<Transfer>,
    /// Whether that beats the run where the job was asked. A device is
    /// picked only when it does.
    pub worthwhile: bool,
}

/// The device a request goes to, and what it won on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Choice {
    /// The willing device that won, if any did.
    pub worker: Option<[u8; 32]>,
    /// What it won on.
    pub rank: Option<Rank>,
    /// What moving the job's inputs there costs.
    pub transfer: Option<Transfer>,
    /// Every device that was asked: willing ones that beat the run here best
    /// first, then willing ones that do not, then the ones that declined, by
    /// device key.
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
                match (candidate.verdict, candidate.transfer) {
                    (Err(reason), _) => {
                        write!(
                            formatter,
                            "; {}: {}",
                            short(&candidate.device),
                            reason.token()
                        )?;
                    }
                    (Ok(_), Some(transfer)) => write!(
                        formatter,
                        "; {}: moving {} bytes takes {} ms, no faster than the run here",
                        short(&candidate.device),
                        transfer.moving_bytes,
                        transfer.moving_ms
                    )?,
                    (Ok(_), None) => {
                        write!(formatter, "; {}: not weighed", short(&candidate.device))?;
                    }
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
            "{} (power {:?}, load {}‰, latency {} ms) of {offered} that offered among {} asked",
            short(&worker),
            rank.power,
            rank.load_permille,
            rank.latency_ms,
            self.considered.len(),
        )?;
        if let Some(transfer) = self.transfer {
            write!(
                formatter,
                ", moving {} bytes in {} ms",
                transfer.moving_bytes, transfer.moving_ms
            )?;
        }
        Ok(())
    }
}

/// The first four bytes of a device key, in hex, for log lines.
fn short(device: &[u8; 32]) -> String {
    hex::encode(device.get(..4).unwrap_or_default())
}

/// Sort key: smaller is better.
fn key(rank: &Rank, device: [u8; 32]) -> (Power, u32, u32, [u8; 32]) {
    (rank.power, rank.load_permille, rank.latency_ms, device)
}

/// Whether moving `work`'s inputs to a willing device that answered
/// `answer` is quicker than running it where it was asked. A job never run
/// there has no run to beat, so nothing is quicker than it.
fn worthwhile(answer: &Answer, rank: &Rank, work: &Work) -> (Transfer, bool) {
    let transfer = transfer(answer, work, rank.latency_ms);
    let faster = work
        .local_wall_ms
        .is_some_and(|local_ms| transfer.moving_ms < local_ms);
    (transfer, faster)
}

/// Pick the device a request goes to among the ones that were asked, as
/// `work` weighs them.
#[must_use]
pub fn pick(answers: impl IntoIterator<Item = ([u8; 32], Answer)>, work: &Work) -> Choice {
    let mut considered = answers
        .into_iter()
        .map(|(device, answer)| {
            // A device that does not take the work is weighed by its reason alone
            // ([`Candidate::verdict`]).
            let ranked = answer
                .verdict
                .as_ref()
                .ok()
                .map(|rank| worthwhile(&answer, rank, work));
            Candidate {
                device,
                verdict: answer.verdict,
                transfer: ranked.map(|(transfer, _)| transfer),
                worthwhile: ranked.is_some_and(|(_, faster)| faster),
            }
        })
        .collect::<Vec<_>>();
    considered.sort_by(|left, right| match (&left.verdict, &right.verdict) {
        (Ok(a), Ok(b)) => left
            .worthwhile
            .cmp(&right.worthwhile)
            .reverse()
            .then_with(|| key(a, left.device).cmp(&key(b, right.device))),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.device.cmp(&right.device),
    });
    let won = considered.first().filter(|best| best.worthwhile);
    Choice {
        worker: won.map(|best| best.device),
        rank: won.and_then(|best| best.verdict.ok()),
        transfer: won.and_then(|best| best.transfer),
        considered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUESTER: [u8; 32] = [9; 32];

    fn rank() -> Rank {
        Rank {
            power: Power::External,
            load_permille: 0,
            latency_ms: 20,
        }
    }

    fn answer() -> Answer {
        Answer::taking(rank(), Vec::new())
    }

    fn holding(digests: Vec<[u8; 32]>) -> Answer {
        Answer::taking(rank(), digests)
    }

    fn with_rank(change: impl FnOnce(&mut Rank)) -> Answer {
        let mut rank = rank();
        change(&mut rank);
        Answer::taking(rank, Vec::new())
    }

    /// A job with one named input and a local run to beat.
    fn work() -> Work {
        Work {
            inputs: vec![([3; 32], 1_000)],
            input_bytes: 1_000,
            local_wall_ms: Some(10_000),
        }
    }

    fn chosen(answers: &[(u8, Answer)]) -> Option<[u8; 32]> {
        pick(
            answers
                .iter()
                .map(|(seed, answer)| ([*seed; 32], answer.clone())),
            &work(),
        )
        .worker
    }

    tilo_test_env::scenario! {
        fn a_run_is_placed_on_the_cheapest_device_that_takes_it;

        phase a_device_that_declines_is_never_chosen;
        // A device that says it does not take the work is passed over whatever
        // its rank would have been, and the reason it gave is kept.
        {
            let on_battery = Answer::declining(Decline::OnBattery);
            let busy = Answer::declining(Decline::LowResources);
            let desktop = with_rank(|rank| {
                rank.load_permille = 750;
                rank.latency_ms = 400;
            });

            assert_eq!(
                chosen(&[(1, on_battery.clone()), (2, busy.clone())]),
                None,
                "nobody who was asked takes the work"
            );
            assert_eq!(
                chosen(&[(1, on_battery.clone()), (2, busy.clone()), (3, desktop)]),
                Some([3; 32])
            );

            let asked = pick([([1; 32], on_battery), ([2; 32], busy)], &work());
            assert_eq!(
                asked
                    .considered
                    .iter()
                    .map(|candidate| (candidate.device[0], candidate.verdict))
                    .collect::<Vec<_>>(),
                vec![
                    (1, Err(Decline::OnBattery)),
                    (2, Err(Decline::LowResources)),
                ]
            );
            assert!(asked.is_local());
            assert_eq!(
                asked.to_string(),
                "no device offered to run it; 01010101: on_battery; 02020202: low_resources"
            );
        }

        phase the_lowest_cost_healthy_device_is_chosen;
        // The cheapest willing device wins: power, then load, then latency.
        {
            let bust = with_rank(|rank| rank.load_permille = 750);
            let idle_far = with_rank(|rank| rank.latency_ms = 90);
            let idle_near = answer();
            let on_battery = with_rank(|rank| {
                rank.power = Power::Battery;
                rank.latency_ms = 1;
            });
            let choice = pick(
                [
                    ([1; 32], bust),
                    ([2; 32], idle_far),
                    ([3; 32], idle_near),
                    ([4; 32], on_battery),
                ],
                &work(),
            );
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
                "03030303 (power External, load 0‰, latency 20 ms) of 4 that offered among 4 asked, moving 1000 bytes in 21 ms"
            );
        }

        phase equal_workers_resolve_the_same_way_whatever_their_order;
        // Equal devices resolve the same way whatever order they answered in.
        {
            assert_eq!(chosen(&[(4, answer()), (2, answer())]), Some([2; 32]));
            assert_eq!(chosen(&[(2, answer()), (4, answer())]), Some([2; 32]));
        }

        phase an_input_a_device_already_holds_is_not_weighed_as_moving;
        // An input a device already holds does not move to it, so holding the
        // job's inputs makes it cheaper than one that must receive them even
        // where rank would have said otherwise.
        {
            let mut heavy = work();
            heavy.inputs = vec![([3; 32], 8_000_000)];
            heavy.input_bytes = 8_000_000;
            heavy.local_wall_ms = Some(500);

            let mut far = holding(vec![[3; 32]]);
            if let Ok(rank) = &mut far.verdict {
                rank.latency_ms = 400;
            }
            let mut near = answer();
            if let Ok(rank) = &mut near.verdict {
                rank.latency_ms = 1;
            }
            let choice = pick([([1; 32], far), ([2; 32], near)], &heavy);
            assert_eq!(
                choice.worker,
                Some([1; 32]),
                "the device that already holds the input passes the weighing, and rank no longer decides"
            );
            assert_eq!(
                choice.transfer,
                Some(Transfer {
                    moving_bytes: 0,
                    moving_ms: 400
                })
            );
            assert!(
                !choice.considered[1].worthwhile,
                "the device that would receive all of it cannot beat the run here"
            );
        }

        phase moving_inputs_slower_than_the_run_leaves_it_where_it_was_asked;
        // A job whose inputs would take longer to move than the run took where
        // it was asked stays there, and says so.
        {
            let mut heavy = work();
            heavy.inputs = vec![([3; 32], 400 * 1024 * 1024)];
            heavy.input_bytes = 400 * 1024 * 1024;
            heavy.local_wall_ms = Some(1_000);
            let choice = pick([([1; 32], answer())], &heavy);
            assert!(choice.is_local());
            assert!(
                !choice.considered[0].worthwhile,
                "a willing device that would receive more than the run took is not picked"
            );
            assert!(
                choice.to_string().contains("no faster than the run here"),
                "{}",
                choice
            );

            let mut never_run = work();
            never_run.local_wall_ms = None;
            assert!(
                pick([([1; 32], answer())], &never_run).is_local(),
                "a job never run where it was asked has no run to beat"
            );
        }

        phase a_device_that_did_not_answer_is_offline;
        // Every device that was asked with no answer to give is recorded as
        // what a device that is away is taken to have said.
        {
            let choice = pick([(REQUESTER, Answer::away())], &work());
            assert_eq!(
                choice.considered[0].verdict,
                Err(Decline::Offline),
                "a device that never answered is not there"
            );
            assert!(choice.is_local());
        }

        phase nothing_asked_says_so;
        // Nothing asked means nothing offers, and the request stays where it
        // was asked.
        {
            let choice = pick([], &work());
            assert!(choice.is_local());
            assert!(choice.considered.is_empty());
            assert_eq!(choice.to_string(), "no device offered to run it");
        }
    }
}
