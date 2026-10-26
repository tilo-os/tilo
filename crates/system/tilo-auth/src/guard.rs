//! An entry already tried is not tried again (docs/security/sign-in.md §4.2).
//!
//! Someone unsure whether their first attempt registered types the same thing
//! again; that must cost one TPM try, not two. Wrong entries are kept as HMACs
//! under a key drawn when the daemon starts, in its memory only, per person,
//! until that person succeeds or ten minutes pass. This is no security
//! boundary: anything that bypasses the daemon still meets the TPM.

use crate::secret::Secret;
use crate::userkey::hmac_sha256;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const REMEMBERED_FOR: Duration = Duration::from_secs(10 * 60);
/// More distinct wrong entries than anyone types in ten minutes; the oldest
/// go first beyond it, so a flood cannot grow the daemon's memory.
const PER_PERSON: usize = 32;

pub struct Guard {
    key: Zeroizing<[u8; 32]>,
    wrong: HashMap<u32, Vec<([u8; 32], Instant)>>,
}

impl Guard {
    pub fn new() -> Result<Self> {
        let mut key = Zeroizing::new([0_u8; 32]);
        getrandom::fill(key.as_mut()).map_err(|e| anyhow!("reading OS randomness: {e}"))?;
        Ok(Self {
            key,
            wrong: HashMap::new(),
        })
    }

    fn digest(&self, secret: &Secret) -> [u8; 32] {
        *hmac_sha256(self.key.as_slice(), secret.as_bytes())
    }

    pub fn forget_expired(&mut self, now: Instant) {
        self.wrong.retain(|_, entries| {
            entries.retain(|(_, at)| now.saturating_duration_since(*at) < REMEMBERED_FOR);
            !entries.is_empty()
        });
    }

    /// Whether `secret` is one of this person's recent wrong entries.
    pub fn already_wrong(&mut self, uid: u32, secret: &Secret, now: Instant) -> bool {
        self.forget_expired(now);
        let digest = self.digest(secret);
        self.wrong
            .get(&uid)
            .is_some_and(|entries| entries.iter().any(|(d, _)| *d == digest))
    }

    pub fn remember_wrong(&mut self, uid: u32, secret: &Secret, now: Instant) {
        self.forget_expired(now);
        let digest = self.digest(secret);
        let entries = self.wrong.entry(uid).or_default();
        if entries.iter().any(|(d, _)| *d == digest) {
            return;
        }
        if entries.len() >= PER_PERSON {
            entries.remove(0);
        }
        entries.push((digest, now));
    }

    pub fn succeeded(&mut self, uid: u32) {
        self.wrong.remove(&uid);
    }

    /// When the oldest remembered entry is due to be forgotten.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.wrong
            .values()
            .flatten()
            .filter_map(|(_, at)| at.checked_add(REMEMBERED_FOR))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_entry_is_remembered_per_person_until_success_or_ten_minutes() {
        let mut guard = Guard::new().unwrap();
        let start = Instant::now();
        let typo = Secret::new("12345");
        assert!(!guard.already_wrong(1000, &typo, start));
        guard.remember_wrong(1000, &typo, start);
        assert!(guard.already_wrong(1000, &typo, start + Duration::from_secs(1)));
        assert!(!guard.already_wrong(1001, &typo, start));
        assert!(!guard.already_wrong(1000, &Secret::new("123456"), start));

        guard.succeeded(1000);
        assert!(!guard.already_wrong(1000, &typo, start));

        guard.remember_wrong(1000, &typo, start);
        assert!(
            guard.already_wrong(
                1000,
                &typo,
                (start + REMEMBERED_FOR)
                    .checked_sub(Duration::from_secs(1))
                    .unwrap()
            )
        );
        assert!(!guard.already_wrong(1000, &typo, start + REMEMBERED_FOR));
    }

    #[test]
    fn expired_entries_leave_memory_and_the_next_expiry_follows_the_oldest() {
        let mut guard = Guard::new().unwrap();
        let start = Instant::now();
        assert_eq!(guard.next_expiry(), None);
        guard.remember_wrong(1000, &Secret::new("1"), start);
        let later = start + Duration::from_secs(60);
        guard.remember_wrong(1001, &Secret::new("2"), later);
        assert_eq!(guard.next_expiry(), Some(start + REMEMBERED_FOR));

        guard.forget_expired(start + REMEMBERED_FOR);
        assert!(!guard.wrong.contains_key(&1000));
        assert_eq!(guard.next_expiry(), Some(later + REMEMBERED_FOR));
        guard.forget_expired(later + REMEMBERED_FOR);
        assert!(guard.wrong.is_empty());
        assert_eq!(guard.next_expiry(), None);
    }

    #[test]
    fn entries_are_kept_as_keyed_digests_not_text() {
        let a = Guard::new().unwrap();
        let b = Guard::new().unwrap();
        let secret = Secret::new("hunter2");
        assert_ne!(a.digest(&secret), b.digest(&secret));
        assert_ne!(&a.digest(&secret)[..7], secret.as_bytes());
    }

    #[test]
    fn a_flood_of_wrong_entries_keeps_only_the_newest() {
        let mut guard = Guard::new().unwrap();
        let now = Instant::now();
        for i in 0..=PER_PERSON {
            guard.remember_wrong(1, &Secret::new(&i.to_string()), now);
        }
        assert!(!guard.already_wrong(1, &Secret::new("0"), now));
        assert!(guard.already_wrong(1, &Secret::new(&PER_PERSON.to_string()), now));
        assert_eq!(guard.wrong[&1].len(), PER_PERSON);
    }
}
