//! Which keyed address hash each guest joined from (7 days) and which
//! hashes are cooling down after a removal (24 hours), kept on the
//! persistent volume so a restart lifts neither a cooldown nor an address's
//! guest join windows.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use crate::limits::DAY_MS;
pub const REMEMBER_MS: u64 = 7 * DAY_MS;
pub const COOLDOWN_MS: u64 = DAY_MS;

#[derive(Default, Serialize, Deserialize)]
struct Joined {
    ip_hash: String,
    at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    joins: BTreeMap<String, Joined>,
    cooldowns: BTreeMap<String, u64>,
}

pub struct GuestLedger {
    path: PathBuf,
    stored: Stored,
}

impl GuestLedger {
    pub fn open(path: PathBuf) -> Result<Self> {
        let stored = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("read {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self { path, stored })
    }

    fn prune(&mut self, now_ms: u64) {
        self.stored
            .joins
            .retain(|_, joined| now_ms.saturating_sub(joined.at_ms) < REMEMBER_MS);
        self.stored.cooldowns.retain(|_, until| *until > now_ms);
    }

    /// Replaces the ledger atomically, so a crash mid-write never leaves a
    /// torn file that would refuse the next start.
    fn save(&self) -> Result<()> {
        tilo_files::native::atomic::write(&self.path, &serde_json::to_vec(&self.stored)?)
            .context("save the guest ledger")
    }

    pub fn record_join(&mut self, user_public_key: &str, ip_hash: &str, now_ms: u64) -> Result<()> {
        self.prune(now_ms);
        self.stored.joins.insert(
            user_public_key.to_owned(),
            Joined {
                ip_hash: ip_hash.to_owned(),
                at_ms: now_ms,
            },
        );
        self.save()
    }

    /// A guest was removed or banned: cool down the address hash they
    /// joined from. True when a cooldown was set.
    pub fn removed(&mut self, user_public_key: &str, now_ms: u64) -> Result<bool> {
        self.prune(now_ms);
        let Some(joined) = self.stored.joins.remove(user_public_key) else {
            return Ok(false);
        };
        self.stored
            .cooldowns
            .insert(joined.ip_hash, now_ms.saturating_add(COOLDOWN_MS));
        self.save()?;
        Ok(true)
    }

    /// Every join since `since_ms`, as `(address hash, when)`: what the
    /// per-address join windows held before a restart. A guest who joined
    /// again counts once, at their latest join.
    pub fn joins_since(&self, since_ms: u64) -> impl Iterator<Item = (&str, u64)> {
        self.stored
            .joins
            .values()
            .filter(move |joined| joined.at_ms >= since_ms)
            .map(|joined| (joined.ip_hash.as_str(), joined.at_ms))
    }

    pub fn cooling_down(&self, ip_hash: &str, now_ms: u64) -> bool {
        self.stored
            .cooldowns
            .get(ip_hash)
            .is_some_and(|until| *until > now_ms)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    #[test]
    fn a_removed_guest_cools_down_the_address_they_joined_from_for_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guests.json");
        let mut ledger = GuestLedger::open(path.clone()).unwrap();
        ledger.record_join("aa", "hash-1", 0).unwrap();
        assert!(!ledger.cooling_down("hash-1", 10));
        assert!(ledger.removed("aa", 1_000).unwrap());
        assert!(ledger.cooling_down("hash-1", 1_000 + COOLDOWN_MS - 1));
        assert!(!ledger.cooling_down("hash-1", 1_000 + COOLDOWN_MS));
        // It survives a restart of the service.
        let reopened = GuestLedger::open(path).unwrap();
        assert!(reopened.cooling_down("hash-1", 2_000));
    }

    #[test]
    fn joins_are_forgotten_after_seven_days_and_unknown_removals_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = GuestLedger::open(dir.path().join("guests.json")).unwrap();
        ledger.record_join("aa", "hash-1", 0).unwrap();
        assert!(!ledger.removed("aa", REMEMBER_MS + 1).unwrap());
        assert!(!ledger.removed("never-joined", 5).unwrap());
    }

    #[test]
    fn the_ledger_holds_only_what_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guests.json");
        let mut ledger = GuestLedger::open(path.clone()).unwrap();
        ledger.record_join("aa", "5f2bhash", 0).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("5f2bhash") && !text.contains("203.0.113"));
    }
}
