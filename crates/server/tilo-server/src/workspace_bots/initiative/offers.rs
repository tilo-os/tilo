//! Attributable personal verb counter and reversible activity offers
//! (Part 4 Change 6).
//!
//! Tracks short bursts of repetitive personal actions strictly by structural
//! tuple `(app, verb, target_kind)` — never by document text, message bodies,
//! or item content — and enforces five invariants:
//! 1. **Attribution:** only direct actions by the owning person are counted.
//!    Assistant-authored actions, automation actions, other workspace members,
//!    and unattributable external state changes (such as IMAP flag flips) are
//!    ignored.
//! 2. **Privacy by construction:** observations carry only `(app, verb,
//!    target_kind)` and an optional deduplication key — no content field
//!    exists anywhere in this module.
//! 3. **At most one active offer per kind:** raising an offer exposes the
//!    structured choices `Take over / Not now / Always`. Continued verbs of
//!    the same kind refresh the existing active offer rather than stacking
//!    duplicates.
//! 4. **Withdraw when the person stops (never on a UI timer):** an active
//!    offer stays visible while the person keeps performing the verb, and is
//!    withdrawn cleanly — without counting as a refusal — once the person
//!    stops or leaves the workflow.
//! 5. **Quiet after two refusals, reversible via "ask again":** choosing
//!    `Not now` twice silences that kind until the person explicitly re-enables
//!    it through `ask_again`.

use std::collections::{BTreeMap, HashSet, VecDeque};

/// Default sliding burst window (3 minutes).
pub const DEFAULT_BURST_WINDOW_MS: i64 = 3 * 60 * 1_000;

/// Default count of identical attributable verbs in one burst window before
/// raising an offer.
pub const DEFAULT_BURST_THRESHOLD: u32 = 5;

/// Number of explicit `Not now` refusals after which an offer kind is muted.
pub const QUIET_AFTER_REFUSALS: u32 = 2;

/// Default maximum deduplication keys retained across sources.
pub const DEFAULT_MAX_DEDUP_IDS: usize = 512;

/// Structural identity of a personal app verb — strictly `(app, verb,
/// target_kind)`, never item content or titles.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VerbKey {
    pub app: String,
    pub verb: String,
    pub target_kind: String,
}

impl VerbKey {
    /// Construct a normalized [`VerbKey`]. Returns `None` if any component is
    /// blank after trimming.
    #[must_use]
    pub fn new(
        app: impl AsRef<str>,
        verb: impl AsRef<str>,
        target_kind: impl AsRef<str>,
    ) -> Option<Self> {
        let app = normalize_token(app.as_ref())?;
        let verb = normalize_token(verb.as_ref())?;
        let target_kind = normalize_token(target_kind.as_ref())?;
        Some(Self {
            app,
            verb,
            target_kind,
        })
    }

    /// Canonical kind string (`app::verb::target_kind`) used as the offer kind
    /// identifier and for muting / un-muting.
    #[must_use]
    pub fn kind_id(&self) -> String {
        format!("{}::{}::{}", self.app, self.verb, self.target_kind)
    }

    /// Calm, content-free template line describing the repeated verb.
    #[must_use]
    pub fn offer_prompt(&self, observed_count: u32) -> String {
        format!(
            "You performed {} on {} {} times in {}. Want me to take over?",
            self.verb, self.target_kind, observed_count, self.app
        )
    }
}

/// Who originated an observed verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerbAttribution {
    /// Directly invoked by a person on their own device.
    DirectPerson { person_id: String },
    /// Authored by the person's own assistant (never counted as a human habit).
    AssistantAuthored { agent_id: String },
    /// Authored by an automation or Shortcut.
    AutomationAuthored { automation_id: String },
    /// External mutation whose actor cannot be proven (for example an IMAP
    /// flag flip, mail filter rule, or delegate write).
    UnattributableExternal { source: String },
    /// Directly invoked by a different member of a shared workspace.
    OtherPerson { person_id: String },
}

/// One structural verb observation fed into [`OfferTracker`].
///
/// By construction this type carries no message text, document body, or item
/// labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerbObservation {
    pub at_ms: i64,
    pub attribution: VerbAttribution,
    pub key: VerbKey,
    /// Optional change hash or external event identifier so duplicate
    /// observations across sources are counted once.
    pub dedup_id: Option<String>,
}

/// The three structured choices shown on every activity offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OfferChoice {
    /// Let the assistant finish this burst once.
    TakeOver,
    /// Decline this offer (2 refusals silence the kind).
    NotNow,
    /// Always handle this kind going forward (bounded by existing grants).
    Always,
}

/// An active, currently displayed activity offer for one kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveOffer {
    pub id: String,
    pub kind: String,
    pub key: VerbKey,
    pub observed_count: u32,
    pub raised_at_ms: i64,
    pub last_verb_at_ms: i64,
    pub choices: [OfferChoice; 3],
    pub prompt_line: String,
}

/// Summary of an offer withdrawn because the person stopped acting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawnOffer {
    pub id: String,
    pub kind: String,
    pub withdrawn_at_ms: i64,
    pub reason: WithdrawalReason,
}

/// Why an active offer was withdrawn without counting as a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WithdrawalReason {
    /// Caller reported that the person stopped or switched workflows.
    PersonStopped,
    /// Caller reported that the person left the app where the burst happened.
    LeftApp,
    /// No matching verb occurred within the burst window, so the burst ended.
    BurstEnded,
}

/// Why an observation was ignored without incrementing any counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IgnoreReason {
    AssistantAuthored,
    AutomationAuthored,
    UnattributableExternal,
    OtherPerson,
    DuplicateObservation,
}

/// Result of feeding one [`VerbObservation`] into [`OfferTracker`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Observation did not qualify (unattributable, PA-authored, or duplicate).
    Ignored(IgnoreReason),
    /// Counted toward the burst window, but still below threshold.
    Counted { kind: String, observed_count: u32 },
    /// Threshold met for the first time in this burst: raise a new offer.
    Raised(ActiveOffer),
    /// An offer for this kind is already active: refreshed count and timestamp.
    UpdatedActive(ActiveOffer),
    /// Kind was silenced after repeated refusals: no offer raised.
    Muted { kind: String },
    /// Person already chose `Always` for this kind: no offer prompt needed.
    AlreadyAlways { kind: String, observed_count: u32 },
}

/// Outcome of the person choosing `Take over`, `Not now`, or `Always`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferResolution {
    pub offer_id: String,
    pub kind: String,
    pub choice: OfferChoice,
    pub refusals: u32,
    pub muted: bool,
    pub always: bool,
}

/// One muted offer kind surfaced in Supervision beside an "ask again" control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutedKind {
    pub kind: String,
    pub key: VerbKey,
    pub refusals: u32,
    pub muted_at_ms: Option<i64>,
}

/// Configuration for [`OfferTracker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferConfig {
    pub burst_window_ms: i64,
    pub burst_threshold: u32,
    pub quiet_after_refusals: u32,
    pub max_dedup_ids: usize,
}

impl Default for OfferConfig {
    fn default() -> Self {
        Self {
            burst_window_ms: DEFAULT_BURST_WINDOW_MS,
            burst_threshold: DEFAULT_BURST_THRESHOLD,
            quiet_after_refusals: QUIET_AFTER_REFUSALS,
            max_dedup_ids: DEFAULT_MAX_DEDUP_IDS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KindPreference {
    key: VerbKey,
    refusals: u32,
    always: bool,
    muted_at_ms: Option<i64>,
}

impl KindPreference {
    const fn new(key: VerbKey) -> Self {
        Self {
            key,
            refusals: 0,
            always: false,
            muted_at_ms: None,
        }
    }

    const fn is_muted(&self, quiet_after: u32) -> bool {
        self.refusals >= quiet_after
    }
}

/// Pure, in-memory state machine for attributable verb counting and reversible
/// activity offers.
#[derive(Clone, Debug)]
pub struct OfferTracker {
    owner_person_id: String,
    config: OfferConfig,
    next_offer_seq: u64,
    /// Recent timestamps per kind within the sliding burst window.
    bursts: BTreeMap<String, VecDeque<i64>>,
    /// At most one active offer per kind.
    active: BTreeMap<String, ActiveOffer>,
    /// Per-kind refusal count and `Always` preference.
    preferences: BTreeMap<String, KindPreference>,
    /// Bounded FIFO set of seen deduplication IDs across sources.
    dedup_order: VecDeque<String>,
    dedup_set: HashSet<String>,
}

impl OfferTracker {
    /// Create a new tracker scoped strictly to `owner_person_id`.
    #[must_use]
    pub fn new(owner_person_id: impl Into<String>, config: OfferConfig) -> Self {
        let sanitized = OfferConfig {
            burst_window_ms: config.burst_window_ms.max(1),
            burst_threshold: config.burst_threshold.max(1),
            quiet_after_refusals: config.quiet_after_refusals.max(1),
            max_dedup_ids: config.max_dedup_ids.max(1),
        };
        Self {
            owner_person_id: owner_person_id.into(),
            config: sanitized,
            next_offer_seq: 1,
            bursts: BTreeMap::new(),
            active: BTreeMap::new(),
            preferences: BTreeMap::new(),
            dedup_order: VecDeque::new(),
            dedup_set: HashSet::new(),
        }
    }

    /// Observe one personal app verb and decide whether to raise or update an
    /// offer.
    pub fn observe(&mut self, observation: VerbObservation) -> ObserveOutcome {
        match &observation.attribution {
            VerbAttribution::DirectPerson { person_id } if person_id == &self.owner_person_id => {}
            VerbAttribution::DirectPerson { .. } | VerbAttribution::OtherPerson { .. } => {
                return ObserveOutcome::Ignored(IgnoreReason::OtherPerson);
            }
            VerbAttribution::AssistantAuthored { .. } => {
                return ObserveOutcome::Ignored(IgnoreReason::AssistantAuthored);
            }
            VerbAttribution::AutomationAuthored { .. } => {
                return ObserveOutcome::Ignored(IgnoreReason::AutomationAuthored);
            }
            VerbAttribution::UnattributableExternal { .. } => {
                return ObserveOutcome::Ignored(IgnoreReason::UnattributableExternal);
            }
        }

        if let Some(dedup_id) = observation.dedup_id.as_deref()
            && !dedup_id.is_empty()
            && !self.remember_dedup(dedup_id)
        {
            return ObserveOutcome::Ignored(IgnoreReason::DuplicateObservation);
        }

        let kind = observation.key.kind_id();
        let pref = self
            .preferences
            .entry(kind.clone())
            .or_insert_with(|| KindPreference::new(observation.key.clone()));

        if pref.is_muted(self.config.quiet_after_refusals) {
            return ObserveOutcome::Muted { kind };
        }

        let window_start = observation
            .at_ms
            .saturating_sub(self.config.burst_window_ms);
        let stamps = self.bursts.entry(kind.clone()).or_default();
        while stamps.front().is_some_and(|&ts| ts < window_start) {
            stamps.pop_front();
        }
        stamps.push_back(observation.at_ms);
        let observed_count = u32::try_from(stamps.len()).unwrap_or(u32::MAX);

        if pref.always {
            return ObserveOutcome::AlreadyAlways {
                kind,
                observed_count,
            };
        }

        if let Some(existing) = self.active.get_mut(&kind) {
            existing.observed_count = observed_count;
            existing.last_verb_at_ms = observation.at_ms;
            existing.prompt_line = existing.key.offer_prompt(observed_count);
            return ObserveOutcome::UpdatedActive(existing.clone());
        }

        if observed_count < self.config.burst_threshold {
            return ObserveOutcome::Counted {
                kind,
                observed_count,
            };
        }

        let id = format!("offer-{}", self.next_offer_seq);
        self.next_offer_seq = self.next_offer_seq.saturating_add(1);

        let offer = ActiveOffer {
            id,
            kind: kind.clone(),
            prompt_line: observation.key.offer_prompt(observed_count),
            key: observation.key,
            observed_count,
            raised_at_ms: observation.at_ms,
            last_verb_at_ms: observation.at_ms,
            choices: [
                OfferChoice::TakeOver,
                OfferChoice::NotNow,
                OfferChoice::Always,
            ],
        };
        self.active.insert(kind, offer.clone());
        ObserveOutcome::Raised(offer)
    }

    /// Apply the person's choice (`Take over`, `Not now`, or `Always`) to an
    /// active offer.
    ///
    /// # Errors
    /// Returns an error if no active offer matches `offer_id`.
    pub fn respond(
        &mut self,
        offer_id: &str,
        choice: OfferChoice,
        now_ms: i64,
    ) -> Result<OfferResolution, String> {
        let Some(kind) = self
            .active
            .iter()
            .find_map(|(k, offer)| (offer.id == offer_id).then(|| k.clone()))
        else {
            return Err(format!("no active offer `{offer_id}`"));
        };

        let offer = self
            .active
            .remove(&kind)
            .ok_or_else(|| format!("no active offer `{offer_id}`"))?;
        self.bursts.remove(&kind);

        let pref = self
            .preferences
            .entry(kind.clone())
            .or_insert_with(|| KindPreference::new(offer.key));

        match choice {
            OfferChoice::TakeOver => {
                pref.refusals = 0;
                pref.muted_at_ms = None;
            }
            OfferChoice::NotNow => {
                pref.refusals = pref.refusals.saturating_add(1);
                if pref.is_muted(self.config.quiet_after_refusals) {
                    pref.always = false;
                    pref.muted_at_ms = Some(now_ms);
                }
            }
            OfferChoice::Always => {
                pref.always = true;
                pref.refusals = 0;
                pref.muted_at_ms = None;
            }
        }

        let muted = pref.is_muted(self.config.quiet_after_refusals);
        Ok(OfferResolution {
            offer_id: offer.id,
            kind,
            choice,
            refusals: pref.refusals,
            muted,
            always: pref.always,
        })
    }

    /// Withdraw the active offer for `kind` because the person stopped doing
    /// that task. Never increments the refusal count.
    pub fn note_person_stopped(&mut self, kind: &str, at_ms: i64) -> Option<WithdrawnOffer> {
        self.bursts.remove(kind);
        let offer = self.active.remove(kind)?;
        Some(WithdrawnOffer {
            id: offer.id,
            kind: offer.kind,
            withdrawn_at_ms: at_ms,
            reason: WithdrawalReason::PersonStopped,
        })
    }

    /// Withdraw every active offer belonging to `app` when the person leaves
    /// that app. Never increments refusal counts.
    pub fn note_app_left(&mut self, app: &str, at_ms: i64) -> Vec<WithdrawnOffer> {
        let Some(target_app) = normalize_token(app) else {
            return Vec::new();
        };
        let matching_kinds: Vec<String> = self
            .active
            .iter()
            .filter(|&(_, offer)| offer.key.app == target_app)
            .map(|(kind, _)| kind.clone())
            .collect();

        let mut withdrawn = Vec::with_capacity(matching_kinds.len());
        for kind in matching_kinds {
            self.bursts.remove(&kind);
            if let Some(offer) = self.active.remove(&kind) {
                withdrawn.push(WithdrawnOffer {
                    id: offer.id,
                    kind: offer.kind,
                    withdrawn_at_ms: at_ms,
                    reason: WithdrawalReason::LeftApp,
                });
            }
        }
        withdrawn
    }

    /// Reconcile active offers against ongoing verb activity at `now_ms`.
    ///
    /// Notice there is **no** display-duration TTL: an offer stays active as
    /// long as the person keeps performing matching verbs within
    /// `burst_window_ms` of the last verb. Only once the burst itself has
    /// stopped (`now_ms - last_verb_at_ms > burst_window_ms`) is the offer
    /// withdrawn cleanly without counting as a refusal.
    pub fn sync_burst_activity(&mut self, now_ms: i64) -> Vec<WithdrawnOffer> {
        let cutoff = now_ms.saturating_sub(self.config.burst_window_ms);

        // Prune stale burst timestamps.
        self.bursts.retain(|_, stamps| {
            while stamps.front().is_some_and(|&ts| ts < cutoff) {
                stamps.pop_front();
            }
            !stamps.is_empty()
        });

        let stopped_kinds: Vec<String> = self
            .active
            .iter()
            .filter(|&(_, offer)| offer.last_verb_at_ms < cutoff)
            .map(|(kind, _)| kind.clone())
            .collect();

        let mut withdrawn = Vec::with_capacity(stopped_kinds.len());
        for kind in stopped_kinds {
            self.bursts.remove(&kind);
            if let Some(offer) = self.active.remove(&kind) {
                withdrawn.push(WithdrawnOffer {
                    id: offer.id,
                    kind: offer.kind,
                    withdrawn_at_ms: now_ms,
                    reason: WithdrawalReason::BurstEnded,
                });
            }
        }
        withdrawn
    }

    /// Reversibly un-mute `kind` ("ask again"): clears its refusal count so
    /// future bursts can raise offers again. Returns `true` if `kind` had any
    /// refusals cleared.
    pub fn ask_again(&mut self, kind: &str) -> bool {
        let Some(pref) = self.preferences.get_mut(kind) else {
            return false;
        };
        let had_refusals = pref.refusals > 0 || pref.muted_at_ms.is_some();
        pref.refusals = 0;
        pref.muted_at_ms = None;
        self.bursts.remove(kind);
        had_refusals
    }

    /// Revert an `Always` choice for `kind` so the assistant asks first again.
    pub fn ask_first(&mut self, kind: &str) -> bool {
        let Some(pref) = self.preferences.get_mut(kind) else {
            return false;
        };
        let was_always = pref.always;
        pref.always = false;
        self.bursts.remove(kind);
        was_always
    }

    /// Whether `kind` is currently muted after repeated refusals.
    #[must_use]
    pub fn is_muted(&self, kind: &str) -> bool {
        self.preferences
            .get(kind)
            .is_some_and(|pref| pref.is_muted(self.config.quiet_after_refusals))
    }

    /// Whether the person chose `Always` for `kind`.
    #[must_use]
    pub fn is_always(&self, kind: &str) -> bool {
        self.preferences.get(kind).is_some_and(|pref| pref.always)
    }

    /// Refusal count currently held for `kind`.
    #[must_use]
    pub fn refusal_count(&self, kind: &str) -> u32 {
        self.preferences.get(kind).map_or(0, |pref| pref.refusals)
    }

    /// All currently muted kinds, ordered stably for Supervision's "ask again"
    /// controls.
    #[must_use]
    pub fn muted_kinds(&self) -> Vec<MutedKind> {
        self.preferences
            .iter()
            .filter(|(_, pref)| pref.is_muted(self.config.quiet_after_refusals))
            .map(|(kind, pref)| MutedKind {
                kind: kind.clone(),
                key: pref.key.clone(),
                refusals: pref.refusals,
                muted_at_ms: pref.muted_at_ms,
            })
            .collect()
    }

    /// Currently active offers across all kinds.
    #[must_use]
    pub fn active_offers(&self) -> Vec<&ActiveOffer> {
        self.active.values().collect()
    }

    /// Active offer for `key`, if one is currently raised.
    #[must_use]
    pub fn active_offer_for(&self, key: &VerbKey) -> Option<&ActiveOffer> {
        self.active.get(&key.kind_id())
    }

    fn remember_dedup(&mut self, id: &str) -> bool {
        if self.dedup_set.contains(id) {
            return false;
        }
        while self.dedup_order.len() >= self.config.max_dedup_ids {
            if let Some(evicted) = self.dedup_order.pop_front() {
                self.dedup_set.remove(&evicted);
            }
        }
        let owned = id.to_owned();
        self.dedup_order.push_back(owned.clone());
        self.dedup_set.insert(owned);
        true
    }
}

fn normalize_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail_archive_key() -> VerbKey {
        VerbKey::new("so.tilo.mail", "archive", "message").expect("valid key")
    }

    fn obs(at_ms: i64, attribution: VerbAttribution, key: VerbKey) -> VerbObservation {
        VerbObservation {
            at_ms,
            attribution,
            key,
            dedup_id: None,
        }
    }

    fn owner_obs(at_ms: i64, key: VerbKey) -> VerbObservation {
        obs(
            at_ms,
            VerbAttribution::DirectPerson {
                person_id: "xaver".to_owned(),
            },
            key,
        )
    }

    #[test]
    fn ignores_unattributable_imap_flags_assistant_automation_and_other_people() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 2,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();

        assert_eq!(
            tracker.observe(obs(
                1_000,
                VerbAttribution::UnattributableExternal {
                    source: "imap-flag-flip".to_owned()
                },
                key.clone()
            )),
            ObserveOutcome::Ignored(IgnoreReason::UnattributableExternal)
        );
        assert_eq!(
            tracker.observe(obs(
                2_000,
                VerbAttribution::AssistantAuthored {
                    agent_id: "pa-1".to_owned()
                },
                key.clone()
            )),
            ObserveOutcome::Ignored(IgnoreReason::AssistantAuthored)
        );
        assert_eq!(
            tracker.observe(obs(
                3_000,
                VerbAttribution::AutomationAuthored {
                    automation_id: "auto-1".to_owned()
                },
                key.clone()
            )),
            ObserveOutcome::Ignored(IgnoreReason::AutomationAuthored)
        );
        assert_eq!(
            tracker.observe(obs(
                4_000,
                VerbAttribution::OtherPerson {
                    person_id: "colleague".to_owned()
                },
                key.clone()
            )),
            ObserveOutcome::Ignored(IgnoreReason::OtherPerson)
        );
        assert_eq!(
            tracker.observe(obs(
                5_000,
                VerbAttribution::DirectPerson {
                    person_id: "colleague".to_owned()
                },
                key
            )),
            ObserveOutcome::Ignored(IgnoreReason::OtherPerson)
        );
        assert!(tracker.active_offers().is_empty());
    }

    #[test]
    fn deduplicates_observations_across_sources() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 2,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();

        let first = VerbObservation {
            at_ms: 1_000,
            attribution: VerbAttribution::DirectPerson {
                person_id: "xaver".to_owned(),
            },
            key: key.clone(),
            dedup_id: Some("change-abc".to_owned()),
        };
        let dup = VerbObservation {
            at_ms: 1_500,
            ..first.clone()
        };

        assert_eq!(
            tracker.observe(first),
            ObserveOutcome::Counted {
                kind: key.kind_id(),
                observed_count: 1,
            }
        );
        assert_eq!(
            tracker.observe(dup),
            ObserveOutcome::Ignored(IgnoreReason::DuplicateObservation)
        );
        assert!(tracker.active_offers().is_empty());
    }

    #[test]
    fn raises_one_structured_offer_per_kind_past_threshold() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 3,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();

        assert!(matches!(
            tracker.observe(owner_obs(1_000, key.clone())),
            ObserveOutcome::Counted {
                observed_count: 1,
                ..
            }
        ));
        assert!(matches!(
            tracker.observe(owner_obs(2_000, key.clone())),
            ObserveOutcome::Counted {
                observed_count: 2,
                ..
            }
        ));

        let raised = match tracker.observe(owner_obs(3_000, key.clone())) {
            ObserveOutcome::Raised(offer) => offer,
            other => panic!("expected Raised, got {other:?}"),
        };
        assert_eq!(
            raised.choices,
            [
                OfferChoice::TakeOver,
                OfferChoice::NotNow,
                OfferChoice::Always,
            ]
        );
        assert_eq!(raised.observed_count, 3);
        assert_eq!(tracker.active_offers().len(), 1);

        // Additional verbs refresh the existing offer; never create a second one.
        let updated = match tracker.observe(owner_obs(4_000, key)) {
            ObserveOutcome::UpdatedActive(offer) => offer,
            other => panic!("expected UpdatedActive, got {other:?}"),
        };
        assert_eq!(updated.id, raised.id);
        assert_eq!(updated.observed_count, 4);
        assert_eq!(tracker.active_offers().len(), 1);
    }

    #[test]
    fn stays_active_while_person_continues_and_withdraws_when_person_stops() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_window_ms: 60_000,
                burst_threshold: 2,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();

        tracker.observe(owner_obs(0, key.clone()));
        let raised = match tracker.observe(owner_obs(10_000, key.clone())) {
            ObserveOutcome::Raised(offer) => offer,
            other => panic!("expected Raised, got {other:?}"),
        };

        // Even 10 minutes later, as long as the person keeps doing the verb
        // every 30s, the offer is NEVER expired on a UI display timer.
        for step in 1..=20 {
            let t = 10_000 + step * 30_000;
            let outcome = tracker.observe(owner_obs(t, key.clone()));
            assert!(matches!(outcome, ObserveOutcome::UpdatedActive(_)));
            assert!(tracker.sync_burst_activity(t + 1_000).is_empty());
        }
        assert_eq!(
            tracker.active_offer_for(&key).map(|o| o.id.as_str()),
            Some(raised.id.as_str())
        );

        // Once the person stops and the burst window elapses with no matching
        // verbs, the offer withdraws cleanly without incrementing refusals.
        let withdrawn = tracker.sync_burst_activity(10_000 + 20 * 30_000 + 61_000);
        assert_eq!(withdrawn.len(), 1);
        assert_eq!(withdrawn[0].id, raised.id);
        assert_eq!(withdrawn[0].reason, WithdrawalReason::BurstEnded);
        assert!(tracker.active_offers().is_empty());
        assert_eq!(tracker.refusal_count(&key.kind_id()), 0);
        assert!(!tracker.is_muted(&key.kind_id()));
    }

    #[test]
    fn explicit_stop_and_app_leave_withdraw_without_counting_refusal() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 2,
                ..OfferConfig::default()
            },
        );
        let mail_key = mail_archive_key();
        let sheet_key = VerbKey::new("so.tilo.sheets", "set-cell", "row").unwrap();

        tracker.observe(owner_obs(1_000, mail_key.clone()));
        tracker.observe(owner_obs(2_000, mail_key.clone()));
        tracker.observe(owner_obs(3_000, sheet_key.clone()));
        tracker.observe(owner_obs(4_000, sheet_key.clone()));
        assert_eq!(tracker.active_offers().len(), 2);

        let stopped = tracker
            .note_person_stopped(&mail_key.kind_id(), 5_000)
            .expect("mail offer withdrawn");
        assert_eq!(stopped.reason, WithdrawalReason::PersonStopped);
        assert_eq!(tracker.refusal_count(&mail_key.kind_id()), 0);

        let left = tracker.note_app_left("so.tilo.sheets", 6_000);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].reason, WithdrawalReason::LeftApp);
        assert_eq!(tracker.refusal_count(&sheet_key.kind_id()), 0);
        assert!(tracker.active_offers().is_empty());
    }

    #[test]
    fn silences_after_two_refusals_and_supports_reversible_ask_again() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 2,
                quiet_after_refusals: 2,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();
        let kind = key.kind_id();

        // First burst -> Not now (1 refusal, not yet muted).
        tracker.observe(owner_obs(1_000, key.clone()));
        let first = match tracker.observe(owner_obs(2_000, key.clone())) {
            ObserveOutcome::Raised(offer) => offer,
            other => panic!("expected Raised, got {other:?}"),
        };
        let res1 = tracker
            .respond(&first.id, OfferChoice::NotNow, 2_500)
            .unwrap();
        assert_eq!(res1.refusals, 1);
        assert!(!res1.muted);

        // Second burst -> Not now (2 refusals -> muted).
        tracker.observe(owner_obs(10_000, key.clone()));
        let second = match tracker.observe(owner_obs(11_000, key.clone())) {
            ObserveOutcome::Raised(offer) => offer,
            other => panic!("expected Raised, got {other:?}"),
        };
        let res2 = tracker
            .respond(&second.id, OfferChoice::NotNow, 11_500)
            .unwrap();
        assert_eq!(res2.refusals, 2);
        assert!(res2.muted);
        assert!(tracker.is_muted(&kind));
        assert_eq!(tracker.muted_kinds().len(), 1);
        assert_eq!(tracker.muted_kinds()[0].kind, kind);

        // Third burst is suppressed while muted.
        assert_eq!(
            tracker.observe(owner_obs(20_000, key.clone())),
            ObserveOutcome::Muted { kind: kind.clone() }
        );
        assert!(tracker.active_offers().is_empty());

        // Reversible "ask again" un-mutes the kind.
        assert!(tracker.ask_again(&kind));
        assert!(!tracker.is_muted(&kind));
        assert!(tracker.muted_kinds().is_empty());

        tracker.observe(owner_obs(30_000, key.clone()));
        assert!(matches!(
            tracker.observe(owner_obs(31_000, key)),
            ObserveOutcome::Raised(_)
        ));
    }

    #[test]
    fn always_choice_suppresses_future_prompts_until_ask_first() {
        let mut tracker = OfferTracker::new(
            "xaver",
            OfferConfig {
                burst_threshold: 2,
                ..OfferConfig::default()
            },
        );
        let key = mail_archive_key();
        let kind = key.kind_id();

        tracker.observe(owner_obs(1_000, key.clone()));
        let offer = match tracker.observe(owner_obs(2_000, key.clone())) {
            ObserveOutcome::Raised(offer) => offer,
            other => panic!("expected Raised, got {other:?}"),
        };
        let res = tracker
            .respond(&offer.id, OfferChoice::Always, 2_500)
            .unwrap();
        assert!(res.always);
        assert!(tracker.is_always(&kind));

        assert!(matches!(
            tracker.observe(owner_obs(10_000, key.clone())),
            ObserveOutcome::AlreadyAlways { .. }
        ));

        // Person can reverse Always back to asking first.
        assert!(tracker.ask_first(&kind));
        assert!(!tracker.is_always(&kind));
        tracker.observe(owner_obs(20_000, key.clone()));
        assert!(matches!(
            tracker.observe(owner_obs(21_000, key)),
            ObserveOutcome::Raised(_)
        ));
    }
}
