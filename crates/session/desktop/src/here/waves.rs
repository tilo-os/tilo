//! Waves: "hello", from one person here to another, through the room's
//! live presence.
//!
//! A wave rides the waving page's presence frame as `{seq, target}`, the
//! target being the waved-at member's live publisher. Restating the frame
//! restates the same wave, so a receiver shows it once per new `seq`, and
//! only the person it is for (any of their devices) shows it. The first
//! frame seen from someone sets where their count stands without showing
//! anything: a wave that was already there when this page arrived, or
//! reloaded, is not news.
use std::collections::HashMap;

use super::{HereState, LiveMember};

/// How long a wave at someone stays the last one: the panel shows it sent,
/// and waving at them again waits.
pub const WAVE_AGAIN_MS: u64 = 60_000;

/// A wave as a member's presence carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Wave {
    /// Per-publisher counter; a new value is a new wave.
    pub seq: u32,
    /// Whom it is for: their live publisher.
    pub target: String,
}

/// Someone waved at this person: who, as everyone here sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveFrom {
    /// Their live publisher, as their face and row are keyed.
    pub id: String,
    /// Their name here.
    pub name: String,
}

/// The waves this page sent and heard.
#[derive(Debug, Default)]
pub(super) struct Waves {
    /// The last wave counter seen from each publisher.
    seen: HashMap<String, u32>,
    /// Waves at this person not taken yet.
    heard: Vec<WaveFrom>,
    /// This page's latest wave, which its presence carries.
    sent: Option<Wave>,
    /// When this page last waved at whom (Unix milliseconds).
    waved_at: HashMap<String, u64>,
}

impl HereState {
    /// Note the waves `members` carry, keeping the ones at this person.
    pub(super) fn hear_waves(&mut self, members: &[LiveMember]) {
        let mut heard = Vec::new();
        for member in members {
            let Some(wave) = &member.wave else {
                continue;
            };
            let previous = self.waves.seen.insert(member.publisher.clone(), wave.seq);
            // First sight sets the count; the same count is a restatement.
            if previous.is_none_or(|seq| seq == wave.seq)
                || member.kind == "agent"
                || !self.is_own_person(&wave.target)
            {
                continue;
            }
            heard.push(WaveFrom {
                id: member.publisher.clone(),
                name: self
                    .person(&member.publisher)
                    .map_or_else(|| member.label.trim().to_owned(), |(name, _)| name),
            });
        }
        self.waves.heard.extend(heard);
    }

    /// Wave at `target` (a member's live publisher) at `now_ms`: this
    /// page's presence carries it next. `false` when they were waved at
    /// within [`WAVE_AGAIN_MS`], or are this person.
    pub fn wave_at(&mut self, target: &str, now_ms: u64) -> bool {
        if target.is_empty() || self.is_own_person(target) || self.waved_recently(target, now_ms) {
            return false;
        }
        let seq = self
            .waves
            .sent
            .as_ref()
            .map_or(1, |wave| wave.seq.wrapping_add(1));
        self.waves.sent = Some(Wave {
            seq,
            target: target.to_owned(),
        });
        self.waves.waved_at.insert(target.to_owned(), now_ms);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Whether this page waved at `target` within [`WAVE_AGAIN_MS`].
    pub fn waved_recently(&self, target: &str, now_ms: u64) -> bool {
        self.waves
            .waved_at
            .get(target)
            .is_some_and(|at| now_ms.saturating_sub(*at) < WAVE_AGAIN_MS)
    }

    /// Everyone this page waved at within [`WAVE_AGAIN_MS`].
    pub fn waved(&self, now_ms: u64) -> Vec<String> {
        self.waves
            .waved_at
            .keys()
            .filter(|target| self.waved_recently(target, now_ms))
            .cloned()
            .collect()
    }

    /// This page's latest wave, for its presence.
    pub fn sent_wave(&self) -> Option<&Wave> {
        self.waves.sent.as_ref()
    }

    /// The waves at this person heard since the last call.
    pub fn take_waves(&mut self) -> Vec<WaveFrom> {
        std::mem::take(&mut self.waves.heard)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HostLine, LiveMember};
    use super::*;

    fn member(publisher: &str, label: &str, wave: Option<(u32, &str)>) -> LiveMember {
        LiveMember {
            publisher: publisher.into(),
            kind: "person".into(),
            label: label.into(),
            wave: wave.map(|(seq, target)| Wave {
                seq,
                target: target.into(),
            }),
            ..LiveMember::default()
        }
    }

    fn here() -> HereState {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "Kim", None)),
            Some(vec![member("ada", "Ada", Some((4, "ben")))]),
        );
        here
    }

    /// A wave already there when the page arrived is not news; a new one
    /// at this person shows once, however often the frame is restated, and
    /// a wave at someone else shows nothing here.
    #[test]
    fn a_new_wave_at_this_person_shows_once() {
        let mut here = here();
        assert!(here.take_waves().is_empty(), "already there on arrival");
        here.set_members(
            Some(member("me", "Kim", None)),
            Some(vec![member("ada", "Ada", Some((5, "me")))]),
        );
        assert_eq!(
            here.take_waves(),
            [WaveFrom {
                id: "ada".into(),
                name: "Ada".into()
            }]
        );
        let mut restated = member("ada", "Ada", Some((5, "me")));
        restated.place = Some("2".into());
        here.set_members(Some(member("me", "Kim", None)), Some(vec![restated]));
        assert!(here.take_waves().is_empty(), "the same wave, restated");
        here.set_members(
            Some(member("me", "Kim", None)),
            Some(vec![member("ada", "Ada", Some((6, "ben")))]),
        );
        assert!(here.take_waves().is_empty(), "for someone else");
    }

    /// A wave at another device of this person reaches them here too.
    #[test]
    fn a_wave_at_another_device_of_this_person_reaches_them() {
        let mut here = HereState::default();
        let mut own = member("me", "Kim", None);
        own.user = Some("kim-root".into());
        here.set_members(
            Some(own.clone()),
            Some(vec![member("ada", "Ada", Some((1, "x")))]),
        );
        here.set_lines(
            vec![HostLine {
                device: "me-phone".into(),
                user: "kim-root".into(),
                ..HostLine::default()
            }],
            0,
        );
        here.set_members(
            Some(own),
            Some(vec![member("ada", "Ada", Some((2, "me-phone")))]),
        );
        assert_eq!(here.take_waves().len(), 1);
    }

    /// Waving puts a new wave on this page's presence, and the same person
    /// is waved at again only after a while.
    #[test]
    fn waving_counts_up_and_waits_before_the_same_person_again() {
        let mut here = here();
        assert!(here.wave_at("ada", 1_000));
        assert_eq!(
            here.sent_wave(),
            Some(&Wave {
                seq: 1,
                target: "ada".into()
            })
        );
        assert!(!here.wave_at("ada", 2_000), "not again so soon");
        assert_eq!(here.waved(2_000), ["ada".to_owned()]);
        assert!(here.wave_at("ada", 1_000 + WAVE_AGAIN_MS));
        assert_eq!(here.sent_wave().map(|wave| wave.seq), Some(2));
        assert!(!here.wave_at("me", 0), "nobody waves at themselves");
    }
}
