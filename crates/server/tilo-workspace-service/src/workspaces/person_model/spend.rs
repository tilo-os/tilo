//! What a person's agents spent today on this server, reserved before each
//! model call and reconciled as each answer arrives.
//!
//! A keeper bounds what an agent may spend in a day. Every model call this
//! server makes for that agent goes through one entry under one lock: before a
//! provider is asked, the call reserves budget atomically across every spender
//! it counts against so concurrent calls cannot all pass the pre-call check at
//! once and blow past a hard daily cap. On completion, the reservation is
//! reconciled against the actual cost; on error, timeout, or cancellation, the
//! guard releases the reservation automatically on drop.
//!
//! If the persisted spending file cannot be read or parsed, capped calls fail
//! closed rather than silently resetting the day's spend to zero, and the
//! unreadable file is never overwritten.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

/// The file of a person's store their agents' days are kept in.
const FILE: &str = "model-spend.json";

/// Whose day a call spends: an agent in a workspace, and the limit its keeper
/// set it. A piece of an agent's work that its person bounded on its own (an
/// event, a task) is a spender too, under an id no agent has, so the same
/// call counts on the agent's day and on that work's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spender {
    /// The workspace the agent works in, by its id spelled out.
    pub workspace: String,
    /// The agent's id in the workspace's agent list, or the id a piece of its
    /// work keeps its day under (`event:<id>`).
    pub agent: String,
    /// Its name, as its person reads it.
    pub name: String,
    /// The day counted, `YYYY-MM-DD`, as the agent counts its days.
    pub day: String,
    /// The most it may spend that day, in millionths of the currency; none
    /// spends unmetered, and is still counted.
    pub limit_micros: Option<u64>,
}

impl Spender {
    fn key(&self) -> String {
        format!("{}/{}", self.workspace, self.agent)
    }
}

/// Why a person's spending history could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpendError {
    /// The spending file existed on disk (or failed to open) and could not be
    /// read or parsed.
    Unreadable,
}

impl std::fmt::Display for SpendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => f.write_str("spending file is unreadable"),
        }
    }
}

impl std::error::Error for SpendError {}

/// Why a pre-call budget reservation was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReserveRefusal {
    /// Committed spend (settled plus in-flight reservations) has reached or
    /// would exceed the spender's daily limit.
    DayLimitReached {
        agent: String,
        limit_micros: u64,
        spent_micros: u64,
    },
    /// The spending file could not be read, so capped calls fail closed rather
    /// than resetting today's spend to zero.
    Unreadable,
}

/// How much budget a call asks to reserve while in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReserveEstimate {
    /// An explicit upper bound set by the caller: refused if it exceeds the
    /// remaining cap.
    Explicit(u64),
    /// An estimated upper bound from the model's price table (`None` when the
    /// model's pricing is unknown).
    Estimated(Option<u64>),
}

const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// What one agent spent on the last day it was counted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    day: String,
    micros: u64,
    /// Calls answered on this day by a model whose price is unknown, tracked
    /// explicitly so unpriced calls are never disguised as zero-cost calls.
    #[serde(default, skip_serializing_if = "is_zero")]
    unknown_calls: u64,
}

/// One person's agents, each with its day.
type Days = BTreeMap<String, Counted>;

/// Active in-flight commitments for one `(person_root, spender_key, day)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InFlight {
    reserved_micros: u64,
    active_calls: usize,
}

#[derive(Default)]
struct State {
    people: BTreeMap<PathBuf, Days>,
    in_flight: BTreeMap<(PathBuf, String, String), InFlight>,
}

/// The days of every person this server asked a model for, by their store,
/// together with active in-flight budget reservations.
#[derive(Default)]
pub(super) struct Log {
    state: Mutex<State>,
}

impl Log {
    /// What `spender` spent on its day so far, in millionths of the currency,
    /// or [`SpendError::Unreadable`] if the spending file is corrupt or
    /// unreadable.
    pub(super) fn try_spent(&self, root: &Path, spender: &Spender) -> Result<u64, SpendError> {
        let mut state = self.state.lock_recover();
        let days = load_days(&mut state.people, root)?;
        Ok(days
            .get(&spender.key())
            .filter(|counted| counted.day == spender.day)
            .map_or(0, |counted| counted.micros))
    }

    /// What `spender` spent on its day so far, failing closed when the
    /// spending file is unreadable by returning at least its limit (or
    /// `u64::MAX` if unmetered).
    pub(super) fn spent(&self, root: &Path, spender: &Spender) -> u64 {
        match self.try_spent(root, spender) {
            Ok(micros) => micros,
            Err(SpendError::Unreadable) => spender.limit_micros.unwrap_or(u64::MAX),
        }
    }

    /// How many calls with unknown pricing `spender` made on its day so far.
    pub(super) fn unknown_calls(&self, root: &Path, spender: &Spender) -> Result<u64, SpendError> {
        let mut state = self.state.lock_recover();
        let days = load_days(&mut state.people, root)?;
        Ok(days
            .get(&spender.key())
            .filter(|counted| counted.day == spender.day)
            .map_or(0, |counted| counted.unknown_calls))
    }

    /// Currently reserved in-flight budget for `spender` on its day.
    pub(super) fn reserved(&self, root: &Path, spender: &Spender) -> u64 {
        let state = self.state.lock_recover();
        state
            .in_flight
            .get(&(root.to_path_buf(), spender.key(), spender.day.clone()))
            .map_or(0, |flight| flight.reserved_micros)
    }

    /// Fast pre-check before reading settings/vault: refuses immediately if
    /// any capped spender has already exhausted its committed daily cap or if
    /// the spending file is unreadable under a hard cap.
    pub(super) fn check_available(
        &self,
        root: &Path,
        spenders: &[Spender],
    ) -> Result<(), ReserveRefusal> {
        if spenders.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock_recover();
        let State { people, in_flight } = &mut *state;
        let days = match load_days(people, root) {
            Ok(days) => days,
            Err(SpendError::Unreadable) => {
                if spenders
                    .iter()
                    .any(|spender| spender.limit_micros.is_some())
                {
                    return Err(ReserveRefusal::Unreadable);
                }
                return Ok(());
            }
        };
        for spender in spenders {
            let Some(limit_micros) = spender.limit_micros else {
                continue;
            };
            let key = spender.key();
            let spent = days
                .get(&key)
                .filter(|counted| counted.day == spender.day)
                .map_or(0, |counted| counted.micros);
            let reserved = in_flight
                .get(&(root.to_path_buf(), key, spender.day.clone()))
                .map_or(0, |flight| flight.reserved_micros);
            let committed = spent.saturating_add(reserved);
            if committed >= limit_micros {
                return Err(ReserveRefusal::DayLimitReached {
                    agent: spender.name.clone(),
                    limit_micros,
                    spent_micros: committed,
                });
            }
        }
        Ok(())
    }

    /// Atomically admit and reserve budget for a call across all `spenders`.
    ///
    /// Either every spender's cap is satisfied and all holds are committed
    /// together, or none are modified and a [`ReserveRefusal`] is returned.
    /// The returned [`Reservation`] releases its holds automatically on drop
    /// if the call fails, times out, or is cancelled before reconciling.
    pub(super) fn reserve(
        self: &Arc<Self>,
        root: PathBuf,
        spenders: Vec<Spender>,
        estimate: ReserveEstimate,
    ) -> Result<Reservation, ReserveRefusal> {
        if spenders.is_empty() {
            return Ok(Reservation {
                log: Arc::clone(self),
                root,
                entries: Vec::new(),
                unreadable: false,
                settled: true,
            });
        }
        let mut state = self.state.lock_recover();
        let State { people, in_flight } = &mut *state;
        let days = match load_days(people, &root) {
            Ok(days) => days,
            Err(SpendError::Unreadable) => {
                if spenders
                    .iter()
                    .any(|spender| spender.limit_micros.is_some())
                {
                    return Err(ReserveRefusal::Unreadable);
                }
                return Ok(Reservation {
                    log: Arc::clone(self),
                    root,
                    entries: Vec::new(),
                    unreadable: true,
                    settled: true,
                });
            }
        };

        // Phase 1: check every spender before mutating any in-flight state.
        let mut planned: BTreeMap<(String, String), InFlight> = BTreeMap::new();
        let mut entries = Vec::with_capacity(spenders.len());
        for spender in spenders {
            let key = spender.key();
            let spent = days
                .get(&key)
                .filter(|counted| counted.day == spender.day)
                .map_or(0, |counted| counted.micros);
            let existing = in_flight
                .get(&(root.clone(), key.clone(), spender.day.clone()))
                .copied()
                .unwrap_or_default();
            let extra = planned
                .get(&(key.clone(), spender.day.clone()))
                .copied()
                .unwrap_or_default();
            let reserved = existing
                .reserved_micros
                .saturating_add(extra.reserved_micros);
            let active_calls = existing.active_calls.saturating_add(extra.active_calls);
            let committed = spent.saturating_add(reserved);

            let hold_micros = if let Some(limit_micros) = spender.limit_micros {
                if committed >= limit_micros {
                    return Err(ReserveRefusal::DayLimitReached {
                        agent: spender.name,
                        limit_micros,
                        spent_micros: committed,
                    });
                }
                let remaining = limit_micros.saturating_sub(committed);
                match estimate {
                    ReserveEstimate::Explicit(est) => {
                        if est > remaining {
                            return Err(ReserveRefusal::DayLimitReached {
                                agent: spender.name,
                                limit_micros,
                                spent_micros: committed,
                            });
                        }
                        est
                    }
                    ReserveEstimate::Estimated(Some(est @ 1..)) => {
                        if est <= remaining {
                            est
                        } else if active_calls > 0 {
                            return Err(ReserveRefusal::DayLimitReached {
                                agent: spender.name,
                                limit_micros,
                                spent_micros: committed,
                            });
                        } else {
                            // No sibling is in flight yet: commit the entire
                            // remaining balance so concurrent siblings cannot
                            // sneak in beside this call.
                            remaining
                        }
                    }
                    ReserveEstimate::Estimated(None | Some(0)) => {
                        // Without a positive model-price estimate, hold the
                        // remaining balance while in flight so concurrent
                        // calls cannot all pass admission simultaneously.
                        remaining
                    }
                }
            } else {
                0
            };

            if hold_micros > 0 {
                let pending = planned.entry((key, spender.day.clone())).or_default();
                pending.reserved_micros = pending.reserved_micros.saturating_add(hold_micros);
                pending.active_calls = pending.active_calls.saturating_add(1);
            }
            entries.push(ReservedEntry {
                spender,
                hold_micros,
            });
        }

        // Phase 2: commit every hold atomically under the same lock.
        for entry in &entries {
            if entry.hold_micros > 0 {
                let flight = in_flight
                    .entry((root.clone(), entry.spender.key(), entry.spender.day.clone()))
                    .or_default();
                flight.reserved_micros = flight.reserved_micros.saturating_add(entry.hold_micros);
                flight.active_calls = flight.active_calls.saturating_add(1);
            }
        }

        Ok(Reservation {
            log: Arc::clone(self),
            root,
            entries,
            unreadable: false,
            settled: false,
        })
    }
}

struct ReservedEntry {
    spender: Spender,
    hold_micros: u64,
}

/// An active in-flight budget reservation across a call's spenders.
///
/// Reconcile with [`Self::reconcile`] when the provider answers, or let it
/// drop (or call [`Self::release`]) to free every held reservation on error,
/// timeout, or cancellation.
pub(super) struct Reservation {
    log: Arc<Log>,
    root: PathBuf,
    entries: Vec<ReservedEntry>,
    unreadable: bool,
    settled: bool,
}

impl Reservation {
    /// Release all held reservations and charge `actual_cost` (plus mark an
    /// unknown-price call when `cost_unknown` is true) atomically.
    ///
    /// Returns the first spender's updated day total, or `None` when no
    /// spender was attached or the spending file was unreadable.
    pub(super) fn reconcile(mut self, actual_cost: u64, cost_unknown: bool) -> Option<u64> {
        self.settled = true;
        if self.unreadable || self.entries.is_empty() {
            return None;
        }
        let mut state = self.log.state.lock_recover();
        let State { people, in_flight } = &mut *state;
        release_entries(in_flight, &self.root, &self.entries);
        let days = match load_days(people, &self.root) {
            Ok(days) => days,
            Err(SpendError::Unreadable) => return None,
        };
        let mut first_total = None;
        for entry in &self.entries {
            let counted = days.entry(entry.spender.key()).or_default();
            if counted.day != entry.spender.day {
                *counted = Counted {
                    day: entry.spender.day.clone(),
                    micros: 0,
                    unknown_calls: 0,
                };
            }
            counted.micros = counted.micros.saturating_add(actual_cost);
            if cost_unknown {
                counted.unknown_calls = counted.unknown_calls.saturating_add(1);
            }
            first_total.get_or_insert(counted.micros);
        }
        if (actual_cost > 0 || cost_unknown)
            && let Err(error) = keep(&self.root, days)
        {
            tracing::warn!(%error, "a person's agents' spending was counted and not written");
        }
        first_total
    }

    /// Release all held reservations without charging spend (used on provider
    /// error, timeout, or cancellation).
    pub(super) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        if self.entries.iter().all(|entry| entry.hold_micros == 0) {
            return;
        }
        let mut state = self.log.state.lock_recover();
        release_entries(&mut state.in_flight, &self.root, &self.entries);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.release_inner();
    }
}

fn release_entries(
    in_flight: &mut BTreeMap<(PathBuf, String, String), InFlight>,
    root: &Path,
    entries: &[ReservedEntry],
) {
    for entry in entries {
        if entry.hold_micros == 0 {
            continue;
        }
        let key = (
            root.to_path_buf(),
            entry.spender.key(),
            entry.spender.day.clone(),
        );
        if let Some(flight) = in_flight.get_mut(&key) {
            flight.reserved_micros = flight.reserved_micros.saturating_sub(entry.hold_micros);
            flight.active_calls = flight.active_calls.saturating_sub(1);
            if flight.active_calls == 0 && flight.reserved_micros == 0 {
                in_flight.remove(&key);
            }
        }
    }
}

/// Load `root`'s days into `people` if not already loaded.
///
/// Only `ErrorKind::NotFound` starts a fresh empty day map. Any I/O or JSON
/// parse failure returns [`SpendError::Unreadable`] without inserting an empty
/// map into `people`, so capped calls fail closed and repairing the file on
/// disk recovers cleanly without a restart.
fn load_days<'a>(
    people: &'a mut BTreeMap<PathBuf, Days>,
    root: &Path,
) -> Result<&'a mut Days, SpendError> {
    if !people.contains_key(root) {
        let days = read_kept(root)?;
        people.insert(root.to_path_buf(), days);
    }
    people.get_mut(root).ok_or(SpendError::Unreadable)
}

/// Read the days `root` keeps on disk.
///
/// Returns an empty map only when no spending file exists yet (`NotFound`).
/// Any read failure or corrupt JSON returns [`SpendError::Unreadable`].
fn read_kept(root: &Path) -> Result<Days, SpendError> {
    let bytes = match std::fs::read(root.join(FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Days::new()),
        Err(error) => {
            tracing::warn!(%error, "a person's agents' spending file could not be read");
            return Err(SpendError::Unreadable);
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        tracing::warn!(%error, "a person's agents' spending file could not be parsed");
        SpendError::Unreadable
    })
}

/// Write `days` into `root`, whole or not at all.
fn keep(root: &Path, days: &Days) -> io::Result<()> {
    let bytes = serde_json::to_vec(days).map_err(io::Error::other)?;
    tilo_files::native::atomic::write(&root.join(FILE), &bytes)
}
