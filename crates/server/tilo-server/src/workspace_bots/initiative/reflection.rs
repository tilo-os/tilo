//! Bounded reflection and deterministic-predicate event promotion.
//!
//! Three bounded triggers start a reflection batch ([`ReflectionTrigger`]):
//! changes from the person's own outgoing work, unsure or person-corrected
//! judgments, and a bounded daily review. Every batch is capped in both daily
//! spend and cases inspected ([`ReflectionBounds`]).
//!
//! Authority follows a strict total order ([`Authority`]):
//! `Person > LargeModel > DecisionModel`. Reflection may add new items from
//! clear evidence and report each addition in one line, and must return
//! [`AuthorityOutcome::RequiresPersonApproval`] before changing or removing
//! anything set by the person, or before promoting durable personal state from
//! untrusted inbound content.
//!
//! When the same sender and a concrete [`DeterministicPredicate`] repeatedly
//! yield the same classification and action without contradiction (default ten
//! times), [`PromotionDetector`] emits a zero-token [`PromotionProposal`] that
//! can graduate into an explicit event filter. Semantic agreement alone without
//! a deterministic predicate never promotes to a zero-token filter.

use std::collections::{BTreeMap, BTreeSet};

/// Milliseconds in one UTC day, used to roll daily reflection caps.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Default daily spend cap for reflection batches, in currency micros ($0.25).
pub const DEFAULT_DAILY_BUDGET_MICROS: u64 = 250_000;

/// Default maximum number of cases inspected in a single reflection batch.
pub const DEFAULT_MAX_CASES_PER_BATCH: usize = 20;

/// Default maximum number of reflection batches admitted in one UTC day.
pub const DEFAULT_MAX_DAILY_BATCHES: u32 = 12;

/// Default number of consistent deterministic observations required before
/// proposing graduation to a zero-token event filter.
pub const DEFAULT_PROMOTION_THRESHOLD: usize = 10;

/// Maximum number of sample identifiers retained on a promotion proposal.
const MAX_PROPOSAL_SAMPLE_IDS: usize = 32;

/// The three bounded triggers that may start a reflection batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReflectionTrigger {
    /// The person's own outgoing change or updated fact arrived.
    OnChange,
    /// A judgment landed in the unsure band or the person corrected it.
    OnUnsureOrCorrected,
    /// Bounded periodic daily review of recent unsure or corrected entries.
    ScheduledDaily,
}

/// Hard caps governing how much work reflection may perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectionBounds {
    /// Maximum daily spend allowed across all reflection batches, in micros.
    pub daily_budget_micros: u64,
    /// Maximum number of cases handed to the large model in one batch.
    pub max_cases_per_batch: usize,
    /// Maximum number of reflection batches allowed per UTC day.
    pub max_daily_batches: u32,
}

impl Default for ReflectionBounds {
    fn default() -> Self {
        Self {
            daily_budget_micros: DEFAULT_DAILY_BUDGET_MICROS,
            max_cases_per_batch: DEFAULT_MAX_CASES_PER_BATCH,
            max_daily_batches: DEFAULT_MAX_DAILY_BATCHES,
        }
    }
}

/// One queued case waiting for reflection review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionCase {
    /// Stable identifier used to deduplicate pending cases.
    pub id: String,
    /// Which trigger enqueued this case.
    pub trigger: ReflectionTrigger,
    /// Compact one-line summary of the case.
    pub summary: String,
    /// Timestamp when the case was observed, in ms since the Unix epoch.
    pub at_ms: i64,
}

/// Why a reflection batch was not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferReason {
    /// The daily spend cap for reflection has been reached.
    DailyBudgetExhausted,
    /// The daily batch count cap has been reached.
    DailyBatchCapReached,
    /// No eligible pending cases exist for this trigger.
    NoPendingCases,
    /// The scheduled daily review has already been admitted today.
    ScheduledDailyAlreadyCompleted,
}

/// Outcome of planning a reflection batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchPlan {
    /// Admit a bounded batch of cases for large-model reflection.
    Proceed {
        trigger: ReflectionTrigger,
        cases: Vec<ReflectionCase>,
        /// Number of matching cases left queued because of the per-batch cap.
        remaining_queued: usize,
        /// Remaining daily spend budget before this batch charges its cost.
        remaining_budget_micros: u64,
    },
    /// Do not start a reflection batch right now.
    Deferred(DeferReason),
}

/// Bounded scheduler and daily budget tracker for reflection batches.
#[derive(Clone, Debug, Default)]
pub struct ReflectionScheduler {
    bounds: ReflectionBounds,
    day_index: Option<i64>,
    spent_today_micros: u64,
    batches_today: u32,
    last_daily_review_day: Option<i64>,
    pending: Vec<ReflectionCase>,
}

impl ReflectionScheduler {
    /// Create a scheduler with explicit reflection bounds.
    #[must_use]
    pub fn new(bounds: ReflectionBounds) -> Self {
        Self {
            bounds,
            day_index: None,
            spent_today_micros: 0,
            batches_today: 0,
            last_daily_review_day: None,
            pending: Vec::new(),
        }
    }

    /// Create a scheduler with [`ReflectionBounds::default`].
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ReflectionBounds::default())
    }

    /// Active reflection bounds.
    #[must_use]
    pub const fn bounds(&self) -> ReflectionBounds {
        self.bounds
    }

    /// Queue a case for reflection, ignoring duplicate case identifiers.
    pub fn enqueue(&mut self, case: ReflectionCase) -> bool {
        if case.id.trim().is_empty() || self.pending.iter().any(|queued| queued.id == case.id) {
            return false;
        }
        self.pending.push(case);
        true
    }

    /// Number of cases currently queued across all triggers.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Charge actual spend from a completed reflection call.
    pub fn note_spend(&mut self, spend_micros: u64, now_ms: i64) {
        self.roll_day(now_ms);
        self.spent_today_micros = self.spent_today_micros.saturating_add(spend_micros);
    }

    /// Spend accumulated on the UTC day containing `now_ms`.
    #[must_use]
    pub fn spent_today_micros(&self, now_ms: i64) -> u64 {
        if self.day_index == Some(utc_day(now_ms)) {
            self.spent_today_micros
        } else {
            0
        }
    }

    /// Remaining reflection budget on the UTC day containing `now_ms`.
    #[must_use]
    pub fn remaining_budget_micros(&self, now_ms: i64) -> u64 {
        self.bounds
            .daily_budget_micros
            .saturating_sub(self.spent_today_micros(now_ms))
    }

    /// Plan a bounded reflection batch for `trigger` at `now_ms`.
    pub fn plan_batch(&mut self, trigger: ReflectionTrigger, now_ms: i64) -> BatchPlan {
        self.roll_day(now_ms);
        let today = utc_day(now_ms);

        if self.bounds.daily_budget_micros == 0
            || self.spent_today_micros >= self.bounds.daily_budget_micros
        {
            return BatchPlan::Deferred(DeferReason::DailyBudgetExhausted);
        }
        if self.bounds.max_daily_batches == 0 || self.batches_today >= self.bounds.max_daily_batches
        {
            return BatchPlan::Deferred(DeferReason::DailyBatchCapReached);
        }
        if trigger == ReflectionTrigger::ScheduledDaily && self.last_daily_review_day == Some(today)
        {
            return BatchPlan::Deferred(DeferReason::ScheduledDailyAlreadyCompleted);
        }

        let cap = self.bounds.max_cases_per_batch;
        if cap == 0 {
            return BatchPlan::Deferred(DeferReason::NoPendingCases);
        }

        let mut selected = Vec::new();
        let mut kept = Vec::with_capacity(self.pending.len());
        let mut remaining_matching = 0usize;

        for case in self.pending.drain(..) {
            let eligible = match trigger {
                ReflectionTrigger::ScheduledDaily => true,
                specific => case.trigger == specific,
            };
            if eligible && selected.len() < cap {
                selected.push(case);
            } else {
                if eligible {
                    remaining_matching += 1;
                }
                kept.push(case);
            }
        }
        self.pending = kept;

        if selected.is_empty() {
            return BatchPlan::Deferred(DeferReason::NoPendingCases);
        }

        self.batches_today = self.batches_today.saturating_add(1);
        if trigger == ReflectionTrigger::ScheduledDaily {
            self.last_daily_review_day = Some(today);
        }

        let remaining_budget_micros = self
            .bounds
            .daily_budget_micros
            .saturating_sub(self.spent_today_micros);

        BatchPlan::Proceed {
            trigger,
            cases: selected,
            remaining_queued: remaining_matching,
            remaining_budget_micros,
        }
    }

    fn roll_day(&mut self, now_ms: i64) {
        let day = utc_day(now_ms);
        if self.day_index != Some(day) {
            self.day_index = Some(day);
            self.spent_today_micros = 0;
            self.batches_today = 0;
        }
    }
}

/// Authority hierarchy: `Person > LargeModel > DecisionModel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Authority {
    DecisionModel = 1,
    LargeModel = 2,
    Person = 3,
}

/// Category of kept personal knowledge managed by reflection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeptKind {
    /// Durable personal fact (for example, a raise window or preference).
    PersonalFact,
    /// Durable personal contact group membership.
    ContactGroup,
    /// Per-thread classification or topic label.
    ThreadTopic,
    /// Refined criteria or examples for a decision question.
    QuestionCriteria,
}

impl KeptKind {
    const fn is_durable_personal_state(self) -> bool {
        matches!(self, Self::PersonalFact | Self::ContactGroup)
    }
}

/// Provenance of the evidence behind a proposed kept-state edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvidenceOrigin {
    /// Directly stated or approved by the person.
    PersonExplicit,
    /// Explicit correction entered by the person over an earlier decision.
    PersonCorrection,
    /// Clear, trusted evidence (for example, quote-stripped outgoing messages
    /// authored by the person or verified workspace actions by the person).
    ClearObservedEvidence,
    /// Untrusted external content (inbound email, web page, or third party).
    InboundUntrusted,
}

/// One kept entry with the authority that set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptItem {
    pub key: String,
    pub kind: KeptKind,
    pub value: String,
    pub set_by: Authority,
    pub updated_at_ms: i64,
    pub source_note: String,
}

/// Mutation requested against the kept store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditKind {
    Add { value: String },
    Update { new_value: String },
    Remove,
}

/// One proposed edit submitted by a model or by the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedEdit {
    pub key: String,
    pub kind: KeptKind,
    pub edit: EditKind,
    pub actor: Authority,
    pub origin: EvidenceOrigin,
    pub evidence_note: String,
    pub at_ms: i64,
}

/// Authoritative example saved whenever the person corrects a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionExample {
    pub key: String,
    pub previous_value: Option<String>,
    pub corrected_value: Option<String>,
    pub note: String,
    pub at_ms: i64,
}

/// Result of enforcing authority rules on a [`ProposedEdit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorityOutcome {
    /// Edit was applied immediately and must be reported in this single line.
    Applied { report_line: String },
    /// Edit cannot apply automatically; the person must explicitly approve it.
    RequiresPersonApproval {
        reason: String,
        proposed_summary: String,
    },
    /// Edit is invalid or violates the authority hierarchy outright.
    Rejected { reason: String },
}

/// Kept personal state enforcing `Person > LargeModel > DecisionModel`.
#[derive(Clone, Debug, Default)]
pub struct KeptStore {
    items: BTreeMap<String, KeptItem>,
    corrections: Vec<CorrectionExample>,
}

impl KeptStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a kept item by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&KeptItem> {
        self.items.get(key)
    }

    /// All kept items, ordered by key.
    #[must_use]
    pub fn items(&self) -> &BTreeMap<String, KeptItem> {
        &self.items
    }

    /// Person corrections preserved for future decision prompt refinement.
    #[must_use]
    pub fn corrections(&self) -> &[CorrectionExample] {
        &self.corrections
    }

    /// Evaluate and apply `proposal` strictly under `Person > LargeModel > DecisionModel`.
    pub fn apply_edit(&mut self, proposal: ProposedEdit) -> AuthorityOutcome {
        let key = one_line(&proposal.key);
        if key.is_empty() {
            return AuthorityOutcome::Rejected {
                reason: "kept item key cannot be empty".to_owned(),
            };
        }

        let clean_note = one_line(&proposal.evidence_note);
        let requested_value = match &proposal.edit {
            EditKind::Add { value } | EditKind::Update { new_value: value } => {
                let clean = one_line(value);
                if clean.is_empty() {
                    return AuthorityOutcome::Rejected {
                        reason: "kept item value cannot be empty".to_owned(),
                    };
                }
                Some(clean)
            }
            EditKind::Remove => None,
        };

        // Inbound non-escalation: untrusted inbound text may tag a thread,
        // never add or modify durable contact groups or personal facts without
        // the person's explicit nod.
        if proposal.actor < Authority::Person
            && proposal.origin == EvidenceOrigin::InboundUntrusted
            && proposal.kind.is_durable_personal_state()
        {
            return AuthorityOutcome::RequiresPersonApproval {
                reason: "untrusted inbound content cannot add or modify durable personal state"
                    .to_owned(),
                proposed_summary: summarize_proposal(
                    &key,
                    &proposal.edit,
                    requested_value.as_deref(),
                ),
            };
        }

        if let Some(existing) = self.items.get(&key) {
            // Strict rule: anything set by the person is locked against changes
            // or removals by any model until the person explicitly approves.
            if existing.set_by == Authority::Person && proposal.actor < Authority::Person {
                return AuthorityOutcome::RequiresPersonApproval {
                    reason: format!(
                        "'{key}' was set by the person; approval is required before changing or removing it"
                    ),
                    proposed_summary: summarize_proposal(
                        &key,
                        &proposal.edit,
                        requested_value.as_deref(),
                    ),
                };
            }

            // A lower-authority model cannot overwrite a higher-authority model.
            if proposal.actor < existing.set_by {
                return AuthorityOutcome::Rejected {
                    reason: format!(
                        "{:?} cannot overwrite '{key}' set by {:?}",
                        proposal.actor, existing.set_by
                    ),
                };
            }

            let previous_value = existing.value.clone();
            match proposal.edit {
                EditKind::Remove => {
                    self.items.remove(&key);
                    if proposal.actor == Authority::Person
                        && proposal.origin == EvidenceOrigin::PersonCorrection
                    {
                        self.corrections.push(CorrectionExample {
                            key: key.clone(),
                            previous_value: Some(previous_value),
                            corrected_value: None,
                            note: clean_note.clone(),
                            at_ms: proposal.at_ms,
                        });
                    }
                    return AuthorityOutcome::Applied {
                        report_line: one_line(&format!("Removed kept item '{key}'.")),
                    };
                }
                EditKind::Add { .. } | EditKind::Update { .. } => {
                    let value = requested_value.unwrap_or_default();
                    self.items.insert(
                        key.clone(),
                        KeptItem {
                            key: key.clone(),
                            kind: proposal.kind,
                            value: value.clone(),
                            set_by: proposal.actor,
                            updated_at_ms: proposal.at_ms,
                            source_note: clean_note.clone(),
                        },
                    );
                    if proposal.actor == Authority::Person
                        && proposal.origin == EvidenceOrigin::PersonCorrection
                    {
                        self.corrections.push(CorrectionExample {
                            key: key.clone(),
                            previous_value: Some(previous_value),
                            corrected_value: Some(value.clone()),
                            note: clean_note,
                            at_ms: proposal.at_ms,
                        });
                    }
                    return AuthorityOutcome::Applied {
                        report_line: one_line(&format!("Updated '{key}' to '{value}'.")),
                    };
                }
            }
        }

        // New entry path.
        match proposal.edit {
            EditKind::Remove => AuthorityOutcome::Rejected {
                reason: format!("cannot remove '{key}' because it does not exist"),
            },
            EditKind::Add { .. } | EditKind::Update { .. } => {
                if proposal.actor == Authority::DecisionModel
                    && proposal.kind.is_durable_personal_state()
                {
                    return AuthorityOutcome::RequiresPersonApproval {
                        reason: "the decision model cannot add durable personal state directly"
                            .to_owned(),
                        proposed_summary: summarize_proposal(
                            &key,
                            &proposal.edit,
                            requested_value.as_deref(),
                        ),
                    };
                }

                let value = requested_value.unwrap_or_default();
                self.items.insert(
                    key.clone(),
                    KeptItem {
                        key: key.clone(),
                        kind: proposal.kind,
                        value: value.clone(),
                        set_by: proposal.actor,
                        updated_at_ms: proposal.at_ms,
                        source_note: clean_note.clone(),
                    },
                );
                let suffix = if clean_note.is_empty() {
                    String::new()
                } else {
                    format!(" ({clean_note})")
                };
                AuthorityOutcome::Applied {
                    report_line: one_line(&format!("Added '{key}': {value}{suffix}.")),
                }
            }
        }
    }

    /// Apply a previously held proposal once the person explicitly approves it,
    /// locking the resulting state at [`Authority::Person`].
    pub fn approve_edit(&mut self, mut proposal: ProposedEdit) -> AuthorityOutcome {
        proposal.actor = Authority::Person;
        proposal.origin = EvidenceOrigin::PersonExplicit;
        self.apply_edit(proposal)
    }
}

/// Concrete, code-evaluable predicate that can execute at zero model tokens.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeterministicPredicate {
    /// Exact sender address match (case-insensitive).
    SenderExact { address: String },
    /// Sender domain match (case-insensitive, e.g. `news.example.com`).
    SenderDomain { domain: String },
    /// Exact message header equality (e.g. `List-Id` or `X-GitHub-Reason`).
    HeaderEquals { name: String, value: String },
    /// Exact title prefix match (e.g. `[Weekly Digest]`).
    TitlePrefix { prefix: String },
    /// Exact mail account identifier match.
    AccountEquals { account: String },
    /// Sender belongs to a durable personal contact group.
    ContactGroup { group: String },
    /// Conjunction of deterministic predicates that must all hold.
    All(Vec<DeterministicPredicate>),
}

impl DeterministicPredicate {
    /// Whether this predicate is structurally valid and non-empty.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::SenderExact { address } => !address.trim().is_empty(),
            Self::SenderDomain { domain } => {
                let d = domain.trim().trim_start_matches('@');
                !d.is_empty() && d.contains('.')
            }
            Self::HeaderEquals { name, value } => {
                !name.trim().is_empty() && !value.trim().is_empty()
            }
            Self::TitlePrefix { prefix } => !prefix.trim().is_empty(),
            Self::AccountEquals { account } => !account.trim().is_empty(),
            Self::ContactGroup { group } => !group.trim().is_empty(),
            Self::All(parts) => !parts.is_empty() && parts.iter().all(Self::is_valid),
        }
    }

    /// Normalized form used for deduplication and consistent grouping.
    #[must_use]
    pub fn canonical(&self) -> Self {
        match self {
            Self::SenderExact { address } => Self::SenderExact {
                address: normalize_lower(address),
            },
            Self::SenderDomain { domain } => Self::SenderDomain {
                domain: normalize_lower(domain.trim().trim_start_matches('@')),
            },
            Self::HeaderEquals { name, value } => Self::HeaderEquals {
                name: normalize_lower(name),
                value: one_line(value),
            },
            Self::TitlePrefix { prefix } => Self::TitlePrefix {
                prefix: one_line(prefix),
            },
            Self::AccountEquals { account } => Self::AccountEquals {
                account: normalize_lower(account),
            },
            Self::ContactGroup { group } => Self::ContactGroup {
                group: normalize_lower(group),
            },
            Self::All(parts) => {
                let mut flat = Vec::new();
                for part in parts {
                    match part.canonical() {
                        Self::All(inner) => flat.extend(inner),
                        other => flat.push(other),
                    }
                }
                flat.sort();
                flat.dedup();
                if flat.len() == 1 {
                    flat.pop().unwrap_or_else(|| Self::All(Vec::new()))
                } else {
                    Self::All(flat)
                }
            }
        }
    }

    /// Stable key uniquely identifying this canonical predicate.
    #[must_use]
    pub fn canonical_key(&self) -> String {
        match self.canonical() {
            Self::SenderExact { address } => format!("sender_exact:{address}"),
            Self::SenderDomain { domain } => format!("sender_domain:{domain}"),
            Self::HeaderEquals { name, value } => format!("header:{name}={value}"),
            Self::TitlePrefix { prefix } => format!("title_prefix:{prefix}"),
            Self::AccountEquals { account } => format!("account:{account}"),
            Self::ContactGroup { group } => format!("contact_group:{group}"),
            Self::All(parts) => parts
                .into_iter()
                .map(|part| part.canonical_key())
                .collect::<Vec<_>>()
                .join("&&"),
        }
    }

    /// Concrete key/value filter clauses for zero-token trigger evaluation.
    #[must_use]
    pub fn filter_clauses(&self) -> Vec<(String, String)> {
        let mut clauses = Vec::new();
        self.canonical().collect_clauses(&mut clauses);
        clauses.sort();
        clauses.dedup();
        clauses
    }

    fn collect_clauses(&self, out: &mut Vec<(String, String)>) {
        match self {
            Self::SenderExact { address } => out.push(("from".to_owned(), address.clone())),
            Self::SenderDomain { domain } => out.push(("from_domain".to_owned(), domain.clone())),
            Self::HeaderEquals { name, value } => {
                out.push((format!("header.{name}"), value.clone()));
            }
            Self::TitlePrefix { prefix } => out.push(("title_prefix".to_owned(), prefix.clone())),
            Self::AccountEquals { account } => out.push(("account".to_owned(), account.clone())),
            Self::ContactGroup { group } => out.push(("contact_group".to_owned(), group.clone())),
            Self::All(parts) => {
                for part in parts {
                    part.collect_clauses(out);
                }
            }
        }
    }

    /// Concise human-readable description for a one-line proposal card.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.canonical() {
            Self::SenderExact { address } => format!("from {address}"),
            Self::SenderDomain { domain } => format!("from @{domain}"),
            Self::HeaderEquals { name, value } => format!("header {name} == {value}"),
            Self::TitlePrefix { prefix } => format!("title starts with \"{prefix}\""),
            Self::AccountEquals { account } => format!("account {account}"),
            Self::ContactGroup { group } => format!("contact group \"{group}\""),
            Self::All(parts) => parts
                .into_iter()
                .map(|part| part.describe())
                .collect::<Vec<_>>()
                .join(" and "),
        }
    }
}

/// Whether a logged classification has a deterministic predicate backing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassificationBasis {
    /// Backed by a concrete, zero-token deterministic predicate.
    Deterministic(DeterministicPredicate),
    /// Reached only through semantic model agreement; cannot graduate into a
    /// zero-token event filter.
    SemanticOnly,
}

/// One logged classification observation fed into the promotion detector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassificationSample {
    /// Stable message or item identifier for deduplication.
    pub sample_id: String,
    /// Sender address or identity.
    pub sender: String,
    /// Whether this observation is backed by a deterministic predicate or only
    /// by semantic agreement.
    pub basis: ClassificationBasis,
    /// Assigned classification label (e.g. `newsletter`, `receipt`).
    pub classification: String,
    /// Resulting action kind (e.g. `archive`, `tag_investor`).
    pub action_kind: String,
    /// Observation timestamp in ms since the Unix epoch.
    pub at_ms: i64,
    /// Whether the person later overturned or corrected this classification.
    pub corrected_by_person: bool,
}

/// Concrete zero-token event filter proposal emitted once a pattern repeats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromotionProposal {
    /// Normalized sender identity shared across observations.
    pub sender: String,
    /// Canonical deterministic predicate that justifies zero-token filtering.
    pub predicate: DeterministicPredicate,
    /// Consistent classification label observed across all matching samples.
    pub classification: String,
    /// Consistent action kind observed across all matching samples.
    pub action_kind: String,
    /// Total distinct observations supporting this proposal.
    pub observed_count: usize,
    /// Bounded list of supporting sample identifiers.
    pub sample_ids: Vec<String>,
    /// Concrete key/value filter clauses for zero-token trigger matching.
    pub filter_clauses: Vec<(String, String)>,
    /// One-line summary for the Accept / Pause / Change proposal card.
    pub summary_line: String,
    /// Always `true`: accepted proposals execute at Step 2 with zero tokens.
    pub zero_token: bool,
}

/// Detects recurring deterministic patterns ready to graduate into zero-token
/// explicit events.
#[derive(Clone, Debug)]
pub struct PromotionDetector {
    threshold: usize,
    samples: Vec<ClassificationSample>,
    seen_ids: BTreeSet<String>,
    suppressed_keys: BTreeSet<String>,
}

impl Default for PromotionDetector {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl PromotionDetector {
    /// Create a detector requiring at least `threshold` consistent observations.
    #[must_use]
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold: threshold.max(1),
            samples: Vec::new(),
            seen_ids: BTreeSet::new(),
            suppressed_keys: BTreeSet::new(),
        }
    }

    /// Create a detector with [`DEFAULT_PROMOTION_THRESHOLD`] (10).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_PROMOTION_THRESHOLD)
    }

    /// Minimum number of consistent observations required to emit a proposal.
    #[must_use]
    pub const fn threshold(&self) -> usize {
        self.threshold
    }

    /// Ingest one logged classification observation, deduplicating by `sample_id`.
    pub fn ingest(&mut self, sample: ClassificationSample) -> bool {
        let id = sample.sample_id.trim();
        if id.is_empty() || !self.seen_ids.insert(id.to_owned()) {
            return false;
        }
        self.samples.push(sample);
        true
    }

    /// Suppress future proposals for `(sender, predicate)` once accepted,
    /// proposed, or declined.
    pub fn suppress(&mut self, sender: &str, predicate: &DeterministicPredicate) {
        let key = promotion_group_key(&normalize_lower(sender), &predicate.canonical_key());
        self.suppressed_keys.insert(key);
    }

    /// Scan ingested observations and emit concrete zero-token filter proposals.
    #[must_use]
    pub fn detect_proposals(&self) -> Vec<PromotionProposal> {
        struct Bucket<'a> {
            sender: String,
            predicate: DeterministicPredicate,
            outcomes: BTreeSet<(String, String)>,
            sample_ids: Vec<&'a str>,
            any_corrected: bool,
        }

        let mut buckets: BTreeMap<String, Bucket<'_>> = BTreeMap::new();

        for sample in &self.samples {
            let sender = normalize_lower(&sample.sender);
            if sender.is_empty() {
                continue;
            }
            let ClassificationBasis::Deterministic(predicate) = &sample.basis else {
                // Semantic agreement alone never supplies a zero-token filter.
                continue;
            };
            if !predicate.is_valid() {
                continue;
            }
            let canonical_pred = predicate.canonical();
            let pred_key = canonical_pred.canonical_key();
            let group_key = promotion_group_key(&sender, &pred_key);
            if self.suppressed_keys.contains(&group_key) {
                continue;
            }

            let classification = one_line(&sample.classification);
            let action_kind = one_line(&sample.action_kind);
            if classification.is_empty() || action_kind.is_empty() {
                continue;
            }

            let bucket = buckets.entry(group_key).or_insert_with(|| Bucket {
                sender: sender.clone(),
                predicate: canonical_pred,
                outcomes: BTreeSet::new(),
                sample_ids: Vec::new(),
                any_corrected: false,
            });
            if sample.corrected_by_person {
                bucket.any_corrected = true;
            }
            bucket.outcomes.insert((classification, action_kind));
            bucket.sample_ids.push(sample.sample_id.as_str());
        }

        let mut proposals = Vec::new();
        for bucket in buckets.into_values() {
            if bucket.any_corrected || bucket.outcomes.len() != 1 {
                continue;
            }
            if bucket.sample_ids.len() < self.threshold {
                continue;
            }
            let Some((classification, action_kind)) = bucket.outcomes.into_iter().next() else {
                continue;
            };

            let mut filter_clauses = bucket.predicate.filter_clauses();
            let sender_clause = ("from".to_owned(), bucket.sender.clone());
            if !filter_clauses.contains(&sender_clause) {
                filter_clauses.push(sender_clause);
            }
            filter_clauses.sort();
            filter_clauses.dedup();

            let observed_count = bucket.sample_ids.len();
            let sample_ids: Vec<String> = bucket
                .sample_ids
                .into_iter()
                .take(MAX_PROPOSAL_SAMPLE_IDS)
                .map(str::to_owned)
                .collect();
            let summary_line = one_line(&format!(
                "Zero-token filter for {} ({}): classify as '{}' and {} ({observed_count} consistent matches).",
                bucket.sender,
                bucket.predicate.describe(),
                classification,
                action_kind,
            ));

            proposals.push(PromotionProposal {
                sender: bucket.sender,
                predicate: bucket.predicate,
                classification,
                action_kind,
                observed_count,
                sample_ids,
                filter_clauses,
                summary_line,
                zero_token: true,
            });
        }

        proposals
    }
}

fn promotion_group_key(sender: &str, predicate_key: &str) -> String {
    format!("{sender}\0{predicate_key}")
}

fn utc_day(now_ms: i64) -> i64 {
    now_ms.div_euclid(MS_PER_DAY)
}

fn normalize_lower(text: &str) -> String {
    one_line(text).to_ascii_lowercase()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summarize_proposal(key: &str, edit: &EditKind, value: Option<&str>) -> String {
    match edit {
        EditKind::Add { .. } => one_line(&format!("Add '{key}' = '{}'", value.unwrap_or_default())),
        EditKind::Update { .. } => one_line(&format!(
            "Change '{key}' to '{}'",
            value.unwrap_or_default()
        )),
        EditKind::Remove => one_line(&format!("Remove '{key}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_case(id: &str, trigger: ReflectionTrigger, at_ms: i64) -> ReflectionCase {
        ReflectionCase {
            id: id.to_owned(),
            trigger,
            summary: format!("case {id}"),
            at_ms,
        }
    }

    #[test]
    fn reflection_scheduler_enforces_triggers_batch_cap_and_daily_budget() {
        let bounds = ReflectionBounds {
            daily_budget_micros: 100_000,
            max_cases_per_batch: 3,
            max_daily_batches: 5,
        };
        let mut scheduler = ReflectionScheduler::new(bounds);
        let day1 = 1_700_000_000_000;

        for i in 0..5 {
            assert!(scheduler.enqueue(sample_case(
                &format!("chg-{i}"),
                ReflectionTrigger::OnChange,
                day1 + i,
            )));
        }
        // Duplicate id is ignored.
        assert!(!scheduler.enqueue(sample_case("chg-0", ReflectionTrigger::OnChange, day1)));
        assert!(scheduler.enqueue(sample_case(
            "unsure-1",
            ReflectionTrigger::OnUnsureOrCorrected,
            day1 + 10,
        )));

        // OnChange selects at most max_cases_per_batch (3) and leaves 2 queued.
        match scheduler.plan_batch(ReflectionTrigger::OnChange, day1 + 20) {
            BatchPlan::Proceed {
                trigger,
                cases,
                remaining_queued,
                remaining_budget_micros,
            } => {
                assert_eq!(trigger, ReflectionTrigger::OnChange);
                assert_eq!(cases.len(), 3);
                assert_eq!(remaining_queued, 2);
                assert_eq!(remaining_budget_micros, 100_000);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }

        // OnUnsureOrCorrected selects only its own case.
        match scheduler.plan_batch(ReflectionTrigger::OnUnsureOrCorrected, day1 + 30) {
            BatchPlan::Proceed { cases, .. } => {
                assert_eq!(cases.len(), 1);
                assert_eq!(cases[0].id, "unsure-1");
            }
            other => panic!("expected Proceed, got {other:?}"),
        }

        // ScheduledDaily sweeps remaining cases once per day.
        match scheduler.plan_batch(ReflectionTrigger::ScheduledDaily, day1 + 40) {
            BatchPlan::Proceed { trigger, cases, .. } => {
                assert_eq!(trigger, ReflectionTrigger::ScheduledDaily);
                assert_eq!(cases.len(), 2);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }

        // Second ScheduledDaily on the same day is deferred even if new cases arrive.
        scheduler.enqueue(sample_case(
            "chg-late",
            ReflectionTrigger::OnChange,
            day1 + 50,
        ));
        assert_eq!(
            scheduler.plan_batch(ReflectionTrigger::ScheduledDaily, day1 + 60),
            BatchPlan::Deferred(DeferReason::ScheduledDailyAlreadyCompleted)
        );

        // Exhaust daily budget: further batches on day1 are deferred.
        scheduler.note_spend(100_000, day1 + 70);
        assert_eq!(scheduler.remaining_budget_micros(day1 + 70), 0);
        assert_eq!(
            scheduler.plan_batch(ReflectionTrigger::OnChange, day1 + 80),
            BatchPlan::Deferred(DeferReason::DailyBudgetExhausted)
        );

        // Next UTC day resets budget and scheduled daily marker.
        let day2 = day1 + MS_PER_DAY;
        assert_eq!(scheduler.remaining_budget_micros(day2), 100_000);
        match scheduler.plan_batch(ReflectionTrigger::ScheduledDaily, day2) {
            BatchPlan::Proceed { cases, .. } => {
                assert_eq!(cases.len(), 1);
                assert_eq!(cases[0].id, "chg-late");
            }
            other => panic!("expected Proceed on next day, got {other:?}"),
        }
    }

    #[test]
    fn authority_order_locks_person_items_and_allows_large_model_additions() {
        assert!(Authority::Person > Authority::LargeModel);
        assert!(Authority::LargeModel > Authority::DecisionModel);

        let mut store = KeptStore::new();

        // LargeModel can add a new fact from clear observed evidence and reports in 1 line.
        let add_outcome = store.apply_edit(ProposedEdit {
            key: "raise_window".to_owned(),
            kind: KeptKind::PersonalFact,
            edit: EditKind::Add {
                value: "2027-02-17..2027-05-17".to_owned(),
            },
            actor: Authority::LargeModel,
            origin: EvidenceOrigin::ClearObservedEvidence,
            evidence_note: "quote-stripped sent mail".to_owned(),
            at_ms: 1_000,
        });
        match add_outcome {
            AuthorityOutcome::Applied { report_line } => {
                assert!(!report_line.is_empty());
                assert!(!report_line.contains('\n'));
                assert!(report_line.contains("raise_window"));
            }
            other => panic!("expected Applied, got {other:?}"),
        }

        // DecisionModel cannot overwrite LargeModel.
        let dm_overwrite = store.apply_edit(ProposedEdit {
            key: "raise_window".to_owned(),
            kind: KeptKind::PersonalFact,
            edit: EditKind::Update {
                new_value: "now".to_owned(),
            },
            actor: Authority::DecisionModel,
            origin: EvidenceOrigin::ClearObservedEvidence,
            evidence_note: "guess".to_owned(),
            at_ms: 2_000,
        });
        assert!(matches!(dm_overwrite, AuthorityOutcome::Rejected { .. }));
        assert_eq!(
            store.get("raise_window").map(|i| i.value.as_str()),
            Some("2027-02-17..2027-05-17")
        );

        // Person corrects the entry, locking it at Authority::Person and saving an example.
        let person_edit = store.apply_edit(ProposedEdit {
            key: "raise_window".to_owned(),
            kind: KeptKind::PersonalFact,
            edit: EditKind::Update {
                new_value: "2027-04-01..2027-06-30".to_owned(),
            },
            actor: Authority::Person,
            origin: EvidenceOrigin::PersonCorrection,
            evidence_note: "pushed back one quarter".to_owned(),
            at_ms: 3_000,
        });
        assert!(matches!(person_edit, AuthorityOutcome::Applied { .. }));
        assert_eq!(store.corrections().len(), 1);

        // LargeModel MUST require person approval before changing a person-set item.
        let lm_change = ProposedEdit {
            key: "raise_window".to_owned(),
            kind: KeptKind::PersonalFact,
            edit: EditKind::Update {
                new_value: "2027-01-01..2027-03-01".to_owned(),
            },
            actor: Authority::LargeModel,
            origin: EvidenceOrigin::ClearObservedEvidence,
            evidence_note: "inferred from draft".to_owned(),
            at_ms: 4_000,
        };
        assert!(matches!(
            store.apply_edit(lm_change.clone()),
            AuthorityOutcome::RequiresPersonApproval { .. }
        ));
        assert_eq!(
            store.get("raise_window").map(|i| i.value.as_str()),
            Some("2027-04-01..2027-06-30")
        );

        // LargeModel MUST require person approval before removing a person-set item.
        let lm_remove = ProposedEdit {
            key: "raise_window".to_owned(),
            kind: KeptKind::PersonalFact,
            edit: EditKind::Remove,
            actor: Authority::LargeModel,
            origin: EvidenceOrigin::ClearObservedEvidence,
            evidence_note: "looks outdated".to_owned(),
            at_ms: 5_000,
        };
        assert!(matches!(
            store.apply_edit(lm_remove),
            AuthorityOutcome::RequiresPersonApproval { .. }
        ));
        assert!(store.get("raise_window").is_some());

        // Once the person approves the proposed change, it applies and stays Person-owned.
        assert!(matches!(
            store.approve_edit(lm_change),
            AuthorityOutcome::Applied { .. }
        ));
        assert_eq!(
            store
                .get("raise_window")
                .map(|i| (i.value.as_str(), i.set_by)),
            Some(("2027-01-01..2027-03-01", Authority::Person))
        );
    }

    #[test]
    fn inbound_untrusted_content_cannot_promote_durable_groups_or_facts() {
        let mut store = KeptStore::new();

        // Untrusted inbound mail trying to promote a sender into a durable contact group.
        let group_promo = store.apply_edit(ProposedEdit {
            key: "contact:alice@example.com:group".to_owned(),
            kind: KeptKind::ContactGroup,
            edit: EditKind::Add {
                value: "Invested".to_owned(),
            },
            actor: Authority::LargeModel,
            origin: EvidenceOrigin::InboundUntrusted,
            evidence_note: "inbound email claims to be an investor".to_owned(),
            at_ms: 1_000,
        });
        assert!(matches!(
            group_promo,
            AuthorityOutcome::RequiresPersonApproval { .. }
        ));
        assert!(store.get("contact:alice@example.com:group").is_none());

        // Untrusted inbound mail MAY tag a thread topic, however.
        let thread_tag = store.apply_edit(ProposedEdit {
            key: "thread:42:topic".to_owned(),
            kind: KeptKind::ThreadTopic,
            edit: EditKind::Add {
                value: "fundraising".to_owned(),
            },
            actor: Authority::LargeModel,
            origin: EvidenceOrigin::InboundUntrusted,
            evidence_note: "classified thread topic".to_owned(),
            at_ms: 1_001,
        });
        assert!(matches!(thread_tag, AuthorityOutcome::Applied { .. }));
    }

    #[test]
    fn promotion_detector_requires_deterministic_predicate_not_semantic_agreement() {
        let mut detector = PromotionDetector::with_defaults();
        assert_eq!(detector.threshold(), 10);

        // 15 identical semantic-only classifications from the same sender MUST NOT promote.
        for i in 0..15 {
            assert!(detector.ingest(ClassificationSample {
                sample_id: format!("sem-{i}"),
                sender: "updates@example.com".to_owned(),
                basis: ClassificationBasis::SemanticOnly,
                classification: "newsletter".to_owned(),
                action_kind: "archive".to_owned(),
                at_ms: 1_000 + i,
                corrected_by_person: false,
            }));
        }
        assert!(
            detector.detect_proposals().is_empty(),
            "semantic agreement alone must never emit a zero-token filter proposal"
        );

        // 9 deterministic samples: still below threshold 10.
        let pred = DeterministicPredicate::All(vec![
            DeterministicPredicate::HeaderEquals {
                name: "List-Id".to_owned(),
                value: "<weekly.example.com>".to_owned(),
            },
            DeterministicPredicate::TitlePrefix {
                prefix: "[Digest]".to_owned(),
            },
        ]);
        for i in 0..9 {
            assert!(detector.ingest(ClassificationSample {
                sample_id: format!("det-{i}"),
                sender: "Digest@Example.com".to_owned(),
                basis: ClassificationBasis::Deterministic(pred.clone()),
                classification: "newsletter".to_owned(),
                action_kind: "archive".to_owned(),
                at_ms: 2_000 + i,
                corrected_by_person: false,
            }));
        }
        assert!(detector.detect_proposals().is_empty());

        // Duplicate sample_id does not bump the count to 10.
        assert!(!detector.ingest(ClassificationSample {
            sample_id: "det-0".to_owned(),
            sender: "digest@example.com".to_owned(),
            basis: ClassificationBasis::Deterministic(pred.clone()),
            classification: "newsletter".to_owned(),
            action_kind: "archive".to_owned(),
            at_ms: 2_099,
            corrected_by_person: false,
        }));
        assert!(detector.detect_proposals().is_empty());

        // 10th distinct deterministic sample triggers a concrete zero-token filter proposal.
        assert!(detector.ingest(ClassificationSample {
            sample_id: "det-9".to_owned(),
            sender: "digest@example.com".to_owned(),
            basis: ClassificationBasis::Deterministic(pred.clone()),
            classification: "newsletter".to_owned(),
            action_kind: "archive".to_owned(),
            at_ms: 2_100,
            corrected_by_person: false,
        }));

        let proposals = detector.detect_proposals();
        assert_eq!(proposals.len(), 1);
        let proposal = &proposals[0];
        assert!(proposal.zero_token);
        assert_eq!(proposal.sender, "digest@example.com");
        assert_eq!(proposal.classification, "newsletter");
        assert_eq!(proposal.action_kind, "archive");
        assert_eq!(proposal.observed_count, 10);
        assert!(
            proposal
                .filter_clauses
                .contains(&("from".to_owned(), "digest@example.com".to_owned()))
        );
        assert!(proposal.filter_clauses.contains(&(
            "header.list-id".to_owned(),
            "<weekly.example.com>".to_owned()
        )));
        assert!(
            proposal
                .filter_clauses
                .contains(&("title_prefix".to_owned(), "[Digest]".to_owned()))
        );
        assert!(!proposal.summary_line.contains('\n'));

        // Suppressing the proposed pattern prevents duplicate cards.
        detector.suppress(&proposal.sender, &proposal.predicate);
        assert!(detector.detect_proposals().is_empty());
    }

    #[test]
    fn promotion_detector_rejects_conflicting_or_corrected_predicates() {
        let mut detector = PromotionDetector::new(3);
        let pred = DeterministicPredicate::SenderDomain {
            domain: "alerts.example.com".to_owned(),
        };

        // Conflicting outcomes on the same (sender, predicate) must not promote.
        for i in 0..3 {
            detector.ingest(ClassificationSample {
                sample_id: format!("c-{i}"),
                sender: "bot@alerts.example.com".to_owned(),
                basis: ClassificationBasis::Deterministic(pred.clone()),
                classification: "noise".to_owned(),
                action_kind: "archive".to_owned(),
                at_ms: i,
                corrected_by_person: false,
            });
        }
        detector.ingest(ClassificationSample {
            sample_id: "c-conflict".to_owned(),
            sender: "bot@alerts.example.com".to_owned(),
            basis: ClassificationBasis::Deterministic(pred.clone()),
            classification: "incident".to_owned(),
            action_kind: "notify".to_owned(),
            at_ms: 99,
            corrected_by_person: false,
        });
        assert!(detector.detect_proposals().is_empty());

        // A predicate whose samples include a person correction must not promote.
        let corr_pred = DeterministicPredicate::TitlePrefix {
            prefix: "[Invoice]".to_owned(),
        };
        for i in 0..3 {
            detector.ingest(ClassificationSample {
                sample_id: format!("k-{i}"),
                sender: "billing@vendor.example.com".to_owned(),
                basis: ClassificationBasis::Deterministic(corr_pred.clone()),
                classification: "receipt".to_owned(),
                action_kind: "file".to_owned(),
                at_ms: i,
                corrected_by_person: i == 2,
            });
        }
        assert!(detector.detect_proposals().is_empty());
    }
}
