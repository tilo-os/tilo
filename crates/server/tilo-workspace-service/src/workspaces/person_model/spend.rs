//! What a person's agents spent today on this host, counted as each answer
//! arrives.
//!
//! A workspace bounds what an agent may spend in a day. Every model call the
//! host makes for that agent goes through one entry, so the day is counted
//! here, under one lock: of dozens of calls at once each adds what it cost
//! and none is lost, and a call that starts once the limit is reached is
//! refused before its provider is asked. The count is kept in the person's
//! own store, so a host that restarts still knows the day.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

/// The file of a person's store their agents' days are kept in.
const FILE: &str = "model-spend.json";

/// Whose day a call spends: an agent in a workspace, and the limit the workspace
/// set it. A piece of an agent's work that its person bounded on its own (an
/// event, a task) is a spender too, under an id no agent has, so the same
/// call counts on the agent's day and on that work's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spender {
    /// The workspace the agent works in, by its id spelled out.
    pub workspace: String,
    /// The agent's id in the workspace's record of its agents, or the id a
    /// piece of its work keeps its day under (`event:<id>`).
    pub agent: String,
    /// Its name, as its person reads it.
    pub name: String,
    /// The day counted, `YYYY-MM-DD`, as the agent's record counts its days.
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

/// What one agent spent on the last day it was counted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    day: String,
    micros: u64,
}

/// One person's agents, each with its day.
type Days = BTreeMap<String, Counted>;

/// The days of every person this host asked a model for, by their store.
#[derive(Default)]
pub(super) struct Log {
    people: Mutex<BTreeMap<PathBuf, Days>>,
}

impl Log {
    /// What `spender` spent on its day so far, in millionths of the
    /// currency: nothing on a day not counted yet.
    pub(super) fn spent(&self, root: &Path, spender: &Spender) -> u64 {
        self.with_days(root, |days| {
            days.get(&spender.key())
                .filter(|counted| counted.day == spender.day)
                .map_or(0, |counted| counted.micros)
        })
    }

    /// Count `cost` on `spender`'s day and answer the day's total. A later
    /// day starts the count again.
    pub(super) fn charge(&self, root: &Path, spender: &Spender, cost: u64) -> u64 {
        self.with_days(root, |days| {
            let counted = days.entry(spender.key()).or_default();
            if counted.day != spender.day {
                *counted = Counted {
                    day: spender.day.clone(),
                    micros: 0,
                };
            }
            counted.micros = counted.micros.saturating_add(cost);
            let total = counted.micros;
            // Written while no other call counts, so the file never holds an
            // older count than a later one. What an answer cost is counted
            // in any case: a store that cannot be written loses the day only
            // across a restart.
            if cost > 0
                && let Err(error) = keep(root, days)
            {
                tracing::warn!(%error, "a person's agents' spending was counted and not written");
            }
            total
        })
    }

    /// `work` on the days `root` keeps, read from its store the first time,
    /// while no other call counts.
    fn with_days<T>(&self, root: &Path, work: impl FnOnce(&mut Days) -> T) -> T {
        let mut people = self.people.lock_recover();
        let answer = work(
            people
                .entry(root.to_path_buf())
                .or_insert_with(|| kept(root)),
        );
        drop(people);
        answer
    }
}

/// The days `root` keeps, none when it keeps no file or one this build
/// cannot read.
fn kept(root: &Path) -> Days {
    let Ok(bytes) = std::fs::read(root.join(FILE)) else {
        return Days::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "a person's agents' spending could not be read and starts again");
        Days::new()
    })
}

/// Write `days` into `root`, whole or not at all.
fn keep(root: &Path, days: &Days) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(days).map_err(std::io::Error::other)?;
    tilo_files::native::atomic::write(&root.join(FILE), &bytes)
}
