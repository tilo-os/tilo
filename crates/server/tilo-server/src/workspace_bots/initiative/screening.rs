//! Concise workspace change lines and low-threshold wake screening for the
//! look-round (Part 4 Change 4).
//!
//! Instead of waking a full large-model turn on every workspace edit, this
//! module:
//! 1. Filters out housekeeping, assistant-authored, and self-authored edits
//!    (the person's own edits belong to the verb counter, not to the
//!    look-round).
//! 2. Formats the remaining document changes into compact one-line summaries
//!    (`who`, `verb`, `app`, `item`, `before`, `after`) bounded to roughly
//!    1,000 tokens.
//! 3. Immediately bypasses screening for deterministic triggers (due
//!    reminders, due scheduled events, and waiting questions) so an outage or
//!    classifier miss never drops a commitment the person already set.
//! 4. Builds a typed `YesNo` screening question with a low-threshold wake
//!    policy biased toward waking rather than missing something important.
//! 5. Treats `Unknown`, invalid probabilities, and outages explicitly by
//!    falling back to waking (never coercing silence to `No`).
//! 6. Emits structured drop entries for the daily backstop review whenever a
//!    look is cleanly suppressed.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

/// Stable question identifier for the look-round wake screening prompt.
pub const WAKE_QUESTION_ID: &str = "look_worth_waking";

/// Default upper bound on formatted change lines in one screening batch.
pub const DEFAULT_MAX_LINES: usize = 24;

/// Default character budget for the screening material (~900 tokens at ~4
/// chars/token, leaving ~100 tokens for the question and metadata state).
pub const DEFAULT_MAX_MATERIAL_CHARS: usize = 3_600;

/// Per-field character caps so one noisy edit never crowds out the batch.
const MAX_WHO_CHARS: usize = 48;
const MAX_VERB_CHARS: usize = 48;
const MAX_APP_CHARS: usize = 48;
const MAX_ITEM_CHARS: usize = 80;
const MAX_VALUE_CHARS: usize = 96;

/// Default low-threshold policy: only a confident low score (`<= 0.25`) drops
/// a look; anything at or above `0.45` wakes immediately, and the uncertain
/// middle band also wakes by default so important news is not missed.
pub const DEFAULT_NO_MAX: f32 = 0.25;
pub const DEFAULT_YES_MIN: f32 = 0.45;

/// Default maximum number of summary lines kept inside one drop entry.
pub const MAX_DROP_SUMMARY_LINES: usize = 5;

/// Default maximum number of drop entries retained for the daily backstop.
pub const DEFAULT_MAX_DROP_LOG_ENTRIES: usize = 128;

/// Who originated a workspace document change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeOrigin {
    /// Another person in the workspace.
    OtherPerson,
    /// The look-round's own owner editing directly (routed to activity offers).
    OwnerSelf,
    /// The owner's assistant or the look-round itself.
    OwnAssistant,
    /// Another agent, bot, or automation.
    OtherAgent,
    /// Unattributed software change.
    Software,
}

/// One typed workspace document change before formatting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeLine {
    /// Human-readable actor label (for example `"Alice"` or `"a bot"`).
    pub who: String,
    /// The action or edit verb (for example `"set-status"` or `"comment"`).
    pub verb: String,
    /// App identifier or short name (for example `"so.tilo.tasks"`).
    pub app: String,
    /// Concise item label (never full body text).
    pub item: String,
    /// Optional compact value before the change.
    pub before: Option<String>,
    /// Optional compact value after the change.
    pub after: Option<String>,
    /// Who originated the change, used to exclude self and assistant edits.
    pub origin: ChangeOrigin,
    /// Housekeeping metadata changes (such as read markers) are ignored.
    pub housekeeping: bool,
}

impl ChangeLine {
    /// Construct a non-housekeeping change line from another person.
    #[must_use]
    pub fn new(
        who: impl Into<String>,
        verb: impl Into<String>,
        app: impl Into<String>,
        item: impl Into<String>,
        before: Option<String>,
        after: Option<String>,
    ) -> Self {
        Self {
            who: who.into(),
            verb: verb.into(),
            app: app.into(),
            item: item.into(),
            before,
            after,
            origin: ChangeOrigin::OtherPerson,
            housekeeping: false,
        }
    }

    /// Attach the origin classification to this change.
    #[must_use]
    pub const fn with_origin(mut self, origin: ChangeOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Mark whether this change is housekeeping noise.
    #[must_use]
    pub const fn with_housekeeping(mut self, housekeeping: bool) -> Self {
        self.housekeeping = housekeeping;
        self
    }

    /// Whether the look-round should evaluate this change.
    ///
    /// Housekeeping, the assistant's own edits, and the owner's direct edits
    /// are excluded here (owner edits feed the separate verb counter).
    #[must_use]
    pub const fn is_relevant_for_look(&self) -> bool {
        !self.housekeeping
            && !matches!(
                self.origin,
                ChangeOrigin::OwnerSelf | ChangeOrigin::OwnAssistant
            )
    }

    /// Render this change into one compact typed line suitable for a fast
    /// decision model.
    #[must_use]
    pub fn format_compact(&self) -> String {
        let who = compact_field(&self.who, MAX_WHO_CHARS, "someone");
        let verb = compact_field(&self.verb, MAX_VERB_CHARS, "changed");
        let app = compact_field(&self.app, MAX_APP_CHARS, "app");
        let item = compact_field(&self.item, MAX_ITEM_CHARS, "item");

        let before = self
            .before
            .as_deref()
            .map(|value| compact_optional(value, MAX_VALUE_CHARS))
            .filter(|value| !value.is_empty());
        let after = self
            .after
            .as_deref()
            .map(|value| compact_optional(value, MAX_VALUE_CHARS))
            .filter(|value| !value.is_empty());

        let mut line = format!("[{app}] {who} {verb} \"{item}\"");
        match (before, after) {
            (Some(b), Some(a)) if b != a => {
                let _ = write!(line, " ({b} -> {a})");
            }
            (None, Some(a)) => {
                let _ = write!(line, " (-> {a})");
            }
            (Some(b), None) => {
                let _ = write!(line, " (was {b})");
            }
            _ => {}
        }
        line
    }
}

/// A task approaching its deadline included in a look-round batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueTaskLine {
    pub id: String,
    pub title: String,
    pub due_ms: i64,
    pub overdue: bool,
}

impl DueTaskLine {
    /// Render one compact due-task line.
    #[must_use]
    pub fn format_compact(&self) -> String {
        let title = compact_field(&self.title, MAX_ITEM_CHARS, "untitled task");
        let status = if self.overdue { "overdue" } else { "due soon" };
        format!("[tasks] {status}: \"{title}\"")
    }
}

/// A deterministic signal that bypasses model screening and always wakes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BypassSignal {
    /// A reminder the person explicitly asked for is now due.
    DueReminder { id: String, label: String },
    /// A scheduled or subscribed event already matched its trigger.
    ScheduledEvent { id: String, name: String },
    /// A waiting question needs attention or follow-through.
    WaitingQuestion { id: String, summary: String },
}

/// Everything gathered by one look-round pass before screening.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookBatch {
    pub workspace: String,
    pub at_ms: i64,
    pub changes: Vec<ChangeLine>,
    pub incoming_mail_count: u64,
    pub mail_snippets: Vec<String>,
    pub due_tasks: Vec<DueTaskLine>,
    pub bypass_signals: Vec<BypassSignal>,
    /// Optional concise active-situation context passed in `state`.
    pub active_context: Option<String>,
}

/// Output of formatting a [`LookBatch`] into a bounded prompt payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedChanges {
    pub lines: Vec<String>,
    pub included_changes: usize,
    pub omitted_changes: usize,
    pub material_text: String,
}

/// Format workspace changes, arrivals, and due tasks into a bounded text
/// payload targeting ~1,000 tokens for a fast decision model.
#[must_use]
pub fn format_look_material(
    batch: &LookBatch,
    max_lines: usize,
    max_chars: usize,
) -> FormattedChanges {
    let relevant: Vec<String> = batch
        .changes
        .iter()
        .filter(|change| change.is_relevant_for_look())
        .map(ChangeLine::format_compact)
        .collect();

    let mut lines = Vec::new();
    let mut text = String::new();
    let mut included_changes = 0usize;

    // Reserve headroom for a trailing truncation note if needed.
    let truncation_reserve = 48usize;
    let body_budget = max_chars.saturating_sub(truncation_reserve);

    let push_line = |target_text: &mut String, target_lines: &mut Vec<String>, line: String| {
        let extra = line
            .chars()
            .count()
            .saturating_add(usize::from(!target_text.is_empty()));
        if target_text.chars().count().saturating_add(extra) > body_budget {
            return false;
        }
        if !target_text.is_empty() {
            target_text.push('\n');
        }
        target_text.push_str(&line);
        target_lines.push(line);
        true
    };

    for line in relevant.iter().take(max_lines) {
        if push_line(&mut text, &mut lines, line.clone()) {
            included_changes = included_changes.saturating_add(1);
        } else {
            break;
        }
    }

    let omitted_changes = relevant.len().saturating_sub(included_changes);
    if omitted_changes > 0 {
        let note = format!("(+{omitted_changes} more changes omitted)");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&note);
        lines.push(note);
    }

    if batch.incoming_mail_count > 0 {
        let mail_line = if batch.mail_snippets.is_empty() {
            format!("[mail] {} new arrival(s)", batch.incoming_mail_count)
        } else {
            let preview = batch
                .mail_snippets
                .iter()
                .take(3)
                .map(|snippet| compact_field(snippet, MAX_ITEM_CHARS, "message"))
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "[mail] {} new arrival(s): {preview}",
                batch.incoming_mail_count
            )
        };
        let _ = push_line(&mut text, &mut lines, mail_line);
    }

    for task in batch.due_tasks.iter().take(5) {
        if !push_line(&mut text, &mut lines, task.format_compact()) {
            break;
        }
    }

    FormattedChanges {
        lines,
        included_changes,
        omitted_changes,
        material_text: text,
    }
}

/// Independent yes/no question sent to a fast decision model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YesNoQuestion {
    pub id: String,
    pub question: String,
}

/// Ready-to-send decision request for one look-round screening check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreeningRequest {
    pub material_text: String,
    pub state: BTreeMap<String, String>,
    pub question: YesNoQuestion,
    pub relevant_change_count: usize,
    pub omitted_change_count: usize,
    pub summary_lines: Vec<String>,
}

/// Outcome of the zero-token pre-check before calling any decision model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreCheck {
    /// Deterministic trigger fired: bypass the decision model and wake now.
    Bypass(Vec<BypassSignal>),
    /// Nothing relevant changed, arrived, or became due: stay quiet at zero cost.
    NothingNew,
    /// Uncertain workspace activity: send this compact request to the decision model.
    Screen(ScreeningRequest),
}

/// Execute the zero-token pre-check and build the `YesNo` screening request
/// when model judgment is actually needed.
#[must_use]
pub fn prepare_screening(batch: &LookBatch) -> PreCheck {
    prepare_screening_with_limits(batch, DEFAULT_MAX_LINES, DEFAULT_MAX_MATERIAL_CHARS)
}

/// Same as [`prepare_screening`] with explicit line and character limits.
#[must_use]
pub fn prepare_screening_with_limits(
    batch: &LookBatch,
    max_lines: usize,
    max_chars: usize,
) -> PreCheck {
    if !batch.bypass_signals.is_empty() {
        return PreCheck::Bypass(batch.bypass_signals.clone());
    }

    let relevant_change_count = batch
        .changes
        .iter()
        .filter(|change| change.is_relevant_for_look())
        .count();

    if relevant_change_count == 0 && batch.incoming_mail_count == 0 && batch.due_tasks.is_empty() {
        return PreCheck::NothingNew;
    }

    let formatted = format_look_material(batch, max_lines, max_chars);

    let mut state = BTreeMap::new();
    if !batch.workspace.trim().is_empty() {
        state.insert(
            "workspace".to_owned(),
            compact_field(&batch.workspace, 64, "workspace"),
        );
    }
    state.insert("change_count".to_owned(), relevant_change_count.to_string());
    state.insert(
        "incoming_mail".to_owned(),
        batch.incoming_mail_count.to_string(),
    );
    state.insert("due_tasks".to_owned(), batch.due_tasks.len().to_string());
    if let Some(context) = &batch.active_context {
        let clean = compact_optional(context, 240);
        if !clean.is_empty() {
            state.insert("active_context".to_owned(), clean);
        }
    }

    let summary_lines = formatted
        .lines
        .iter()
        .take(MAX_DROP_SUMMARY_LINES)
        .cloned()
        .collect();

    PreCheck::Screen(ScreeningRequest {
        material_text: formatted.material_text,
        state,
        question: YesNoQuestion {
            id: WAKE_QUESTION_ID.to_owned(),
            question: "Could any of these workspace changes, arrivals, or due tasks need the \
                       assistant to act, prepare a draft, or tell the person? Answer yes when \
                       in doubt; answer no only when the activity is clearly routine noise."
                .to_owned(),
        },
        relevant_change_count,
        omitted_change_count: formatted.omitted_changes,
        summary_lines,
    })
}

/// Why a decision model returned an unknown answer rather than a probability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnknownCause {
    MissingFacts,
    Abstained,
    OmittedByEndpoint,
    InvalidProbability,
}

/// Raw outcome from calling the decision model (or failing to reach it).
#[derive(Clone, Debug, PartialEq)]
pub enum ScreeningOutcome {
    /// Calibrated probability in `[0.0, 1.0]`.
    YesNo { probability: f32 },
    /// Explicit unknown answer (never coerced to `No`).
    Unknown { cause: UnknownCause },
    /// Decision model timed out, was unavailable, or returned a transport error.
    Outage { detail: String },
}

/// Low-threshold wake policy for look-round screening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WakePolicy {
    /// Maximum probability (inclusive) that cleanly drops the look.
    no_max: f32,
    /// Minimum probability (inclusive) treated as a clear wake.
    yes_min: f32,
    /// Whether the middle band `(no_max, yes_min)` wakes the large model.
    /// Defaults to `true` so the policy errs on the side of waking.
    wake_on_unsure: bool,
}

impl Default for WakePolicy {
    fn default() -> Self {
        Self {
            no_max: DEFAULT_NO_MAX,
            yes_min: DEFAULT_YES_MIN,
            wake_on_unsure: true,
        }
    }
}

impl WakePolicy {
    /// Construct a validated wake policy (`0.0 <= no_max < yes_min <= 1.0`).
    ///
    /// # Errors
    /// Returns a description when either threshold is non-finite, outside
    /// `[0.0, 1.0]`, or `no_max >= yes_min`.
    pub fn new(no_max: f32, yes_min: f32, wake_on_unsure: bool) -> Result<Self, String> {
        if !valid_probability(no_max) || !valid_probability(yes_min) {
            return Err("thresholds must be finite values within [0.0, 1.0]".to_owned());
        }
        if no_max >= yes_min {
            return Err("no_max must be strictly less than yes_min".to_owned());
        }
        Ok(Self {
            no_max,
            yes_min,
            wake_on_unsure,
        })
    }

    #[must_use]
    pub const fn no_max(self) -> f32 {
        self.no_max
    }

    #[must_use]
    pub const fn yes_min(self) -> f32 {
        self.yes_min
    }

    #[must_use]
    pub const fn wake_on_unsure(self) -> bool {
        self.wake_on_unsure
    }
}

/// Why the look-round decided to wake the large model.
#[derive(Clone, Debug, PartialEq)]
pub enum WakeReason {
    /// Deterministic reminder, event, or waiting question bypassed screening.
    Bypass(Vec<BypassSignal>),
    /// Decision model probability met or exceeded `yes_min`.
    ModelYes { probability: f32 },
    /// Probability landed in the uncertain middle band; low-threshold policy
    /// wakes rather than risking a missed item.
    ModelUnsure { probability: f32 },
    /// Decision model returned `Unknown` or an invalid probability: never
    /// coerce silence to `No`.
    FallbackUnknown { cause: UnknownCause },
    /// Decision model was unreachable or timed out: fail open and wake.
    FallbackOutage { detail: String },
}

/// Structured entry kept for the daily backstop review whenever a look-round
/// is suppressed.
#[derive(Clone, Debug, PartialEq)]
pub struct DropEntry {
    pub at_ms: i64,
    pub workspace: String,
    pub probability: f32,
    pub threshold_no_max: f32,
    pub change_count: usize,
    pub incoming_mail: u64,
    pub due_tasks: usize,
    pub summary_lines: Vec<String>,
    pub reason: String,
}

/// Final decision produced for one look-round pass.
#[derive(Clone, Debug, PartialEq)]
pub enum WakeDecision {
    /// Wake the large model for an isolated turn.
    Wake { reason: WakeReason },
    /// Suppress the large model and keep this structured entry for the daily
    /// backstop turn.
    Drop(DropEntry),
    /// Nothing relevant occurred; zero tokens spent and nothing to log.
    QuietNoChanges,
}

/// Combine a [`LookBatch`], a [`WakePolicy`], and an optional
/// [`ScreeningOutcome`] into the final [`WakeDecision`].
///
/// Invariants:
/// - Bypass signals always wake, even during an outage or low model score.
/// - `Unknown`, invalid probabilities (`NaN`, `< 0.0`, `> 1.0`), missing
///   outcomes, and outages always fall back to waking — silence is never `No`.
/// - Only an explicit finite probability `<= policy.no_max()` (or an unsure
///   score when `wake_on_unsure` is explicitly disabled) produces `Drop`.
#[must_use]
pub fn evaluate_wake(
    batch: &LookBatch,
    policy: WakePolicy,
    outcome: Option<&ScreeningOutcome>,
) -> WakeDecision {
    match prepare_screening(batch) {
        PreCheck::Bypass(signals) => WakeDecision::Wake {
            reason: WakeReason::Bypass(signals),
        },
        PreCheck::NothingNew => WakeDecision::QuietNoChanges,
        PreCheck::Screen(request) => evaluate_screened(batch, &request, policy, outcome),
    }
}

/// Evaluate a [`ScreeningRequest`] against `policy` and `outcome`.
#[must_use]
pub fn evaluate_screened(
    batch: &LookBatch,
    request: &ScreeningRequest,
    policy: WakePolicy,
    outcome: Option<&ScreeningOutcome>,
) -> WakeDecision {
    if !batch.bypass_signals.is_empty() {
        return WakeDecision::Wake {
            reason: WakeReason::Bypass(batch.bypass_signals.clone()),
        };
    }

    let Some(outcome) = outcome else {
        return WakeDecision::Wake {
            reason: WakeReason::FallbackUnknown {
                cause: UnknownCause::OmittedByEndpoint,
            },
        };
    };

    match outcome {
        ScreeningOutcome::Unknown { cause } => WakeDecision::Wake {
            reason: WakeReason::FallbackUnknown { cause: *cause },
        },
        ScreeningOutcome::Outage { detail } => WakeDecision::Wake {
            reason: WakeReason::FallbackOutage {
                detail: detail.clone(),
            },
        },
        ScreeningOutcome::YesNo { probability } => {
            let p = *probability;
            if !valid_probability(p) {
                return WakeDecision::Wake {
                    reason: WakeReason::FallbackUnknown {
                        cause: UnknownCause::InvalidProbability,
                    },
                };
            }
            if p >= policy.yes_min {
                WakeDecision::Wake {
                    reason: WakeReason::ModelYes { probability: p },
                }
            } else if p <= policy.no_max {
                WakeDecision::Drop(build_drop_entry(
                    batch,
                    request,
                    p,
                    policy.no_max,
                    format!("below wake threshold ({p:.2} <= {:.2})", policy.no_max),
                ))
            } else if policy.wake_on_unsure {
                WakeDecision::Wake {
                    reason: WakeReason::ModelUnsure { probability: p },
                }
            } else {
                WakeDecision::Drop(build_drop_entry(
                    batch,
                    request,
                    p,
                    policy.no_max,
                    format!(
                        "unsure band ({p:.2} in ({:.2}, {:.2})) with wake_on_unsure disabled",
                        policy.no_max, policy.yes_min
                    ),
                ))
            }
        }
    }
}

fn build_drop_entry(
    batch: &LookBatch,
    request: &ScreeningRequest,
    probability: f32,
    threshold_no_max: f32,
    reason: String,
) -> DropEntry {
    DropEntry {
        at_ms: batch.at_ms,
        workspace: batch.workspace.clone(),
        probability,
        threshold_no_max,
        change_count: request.relevant_change_count,
        incoming_mail: batch.incoming_mail_count,
        due_tasks: batch.due_tasks.len(),
        summary_lines: request.summary_lines.clone(),
        reason,
    }
}

/// Bounded drop log retained for the daily backstop review and false-negative
/// auditing.
#[derive(Clone, Debug, PartialEq)]
pub struct DropLog {
    max_entries: usize,
    entries: VecDeque<DropEntry>,
}

impl Default for DropLog {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MAX_DROP_LOG_ENTRIES)
    }
}

impl DropLog {
    #[must_use]
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            entries: VecDeque::new(),
        }
    }

    /// Append one dropped look entry, evicting the oldest entry if full.
    pub fn push(&mut self, entry: DropEntry) {
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over retained entries from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &DropEntry> {
        self.entries.iter()
    }

    /// Drain all entries at or after `since_ms` for the daily backstop review.
    pub fn drain_since(&mut self, since_ms: i64) -> Vec<DropEntry> {
        let mut kept = VecDeque::new();
        let mut drained = Vec::new();
        while let Some(entry) = self.entries.pop_front() {
            if entry.at_ms >= since_ms {
                drained.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        self.entries = kept;
        drained
    }

    /// Render a compact digest of dropped looks for the daily backstop review,
    /// capped at `max_chars`.
    #[must_use]
    pub fn format_backstop_digest(&self, max_chars: usize) -> String {
        if self.entries.is_empty() || max_chars == 0 {
            return String::new();
        }
        let mut out = String::new();
        let mut omitted = 0usize;
        for entry in &self.entries {
            let preview = entry
                .summary_lines
                .first()
                .map_or("no summary", String::as_str);
            let line = format!(
                "- [{}] p={:.2} changes={} mail={} due={}: {}",
                entry.workspace,
                entry.probability,
                entry.change_count,
                entry.incoming_mail,
                entry.due_tasks,
                preview
            );
            let extra = line
                .chars()
                .count()
                .saturating_add(usize::from(!out.is_empty()));
            if out.chars().count().saturating_add(extra) > max_chars {
                omitted = omitted.saturating_add(1);
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&line);
        }
        if omitted > 0 {
            let tail = format!("\n(+{omitted} earlier suppressed looks omitted)");
            if out.chars().count().saturating_add(tail.chars().count()) <= max_chars {
                out.push_str(&tail);
            }
        }
        out
    }
}

fn valid_probability(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn compact_field(raw: &str, max_chars: usize, fallback: &str) -> String {
    let clean = compact_optional(raw, max_chars);
    if clean.is_empty() {
        fallback.to_owned()
    } else {
        clean
    }
}

fn compact_optional(raw: &str, max_chars: usize) -> String {
    let mut normalized = String::new();
    for part in raw.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(part);
    }
    truncate_chars(&normalized, max_chars)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 1 {
        return text.chars().take(max_chars).collect();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_change(who: &str, verb: &str, item: &str) -> ChangeLine {
        ChangeLine::new(
            who,
            verb,
            "so.tilo.tasks",
            item,
            Some("open".to_owned()),
            Some("blocked".to_owned()),
        )
    }

    #[test]
    fn formats_typed_change_line_with_before_and_after_delta() {
        let change = ChangeLine::new(
            "Alice",
            "set-status",
            "so.tilo.tasks",
            "Ship investor memo\nby Friday",
            Some("in-progress".to_owned()),
            Some("blocked".to_owned()),
        );
        assert_eq!(
            change.format_compact(),
            "[so.tilo.tasks] Alice set-status \"Ship investor memo by Friday\" (in-progress -> blocked)"
        );
    }

    #[test]
    fn omits_unchanged_before_after_and_handles_one_sided_values() {
        let unchanged = ChangeLine::new(
            "Bob",
            "touch",
            "so.tilo.notes",
            "Meeting notes",
            Some("draft".to_owned()),
            Some("draft".to_owned()),
        );
        assert_eq!(
            unchanged.format_compact(),
            "[so.tilo.notes] Bob touch \"Meeting notes\""
        );

        let added = ChangeLine::new(
            "Bob",
            "assign",
            "so.tilo.tasks",
            "Review contract",
            None,
            Some("Xaver".to_owned()),
        );
        assert_eq!(
            added.format_compact(),
            "[so.tilo.tasks] Bob assign \"Review contract\" (-> Xaver)"
        );

        let cleared = ChangeLine::new(
            "Bob",
            "clear-due",
            "so.tilo.tasks",
            "Review contract",
            Some("2026-12-01".to_owned()),
            None,
        );
        assert_eq!(
            cleared.format_compact(),
            "[so.tilo.tasks] Bob clear-due \"Review contract\" (was 2026-12-01)"
        );
    }

    #[test]
    fn filters_out_self_assistant_and_housekeeping_edits() {
        let batch = LookBatch {
            workspace: "acme".to_owned(),
            at_ms: 1_000,
            changes: vec![
                sample_change("Owner", "edit", "My draft").with_origin(ChangeOrigin::OwnerSelf),
                sample_change("Assistant", "comment", "Own note")
                    .with_origin(ChangeOrigin::OwnAssistant),
                sample_change("Alice", "mark-read", "Thread").with_housekeeping(true),
            ],
            ..LookBatch::default()
        };

        assert_eq!(prepare_screening(&batch), PreCheck::NothingNew);
        assert_eq!(
            evaluate_wake(&batch, WakePolicy::default(), None),
            WakeDecision::QuietNoChanges
        );
    }

    #[test]
    fn bounds_large_change_batches_within_token_budget() {
        let mut batch = LookBatch {
            workspace: "acme".to_owned(),
            at_ms: 2_000,
            ..LookBatch::default()
        };
        for idx in 0..80 {
            batch.changes.push(sample_change(
                "Teammate",
                "update-cell",
                &format!("Quarterly revenue forecast row #{idx} with detailed commentary"),
            ));
        }

        let formatted = format_look_material(&batch, DEFAULT_MAX_LINES, DEFAULT_MAX_MATERIAL_CHARS);
        assert!(formatted.material_text.chars().count() <= DEFAULT_MAX_MATERIAL_CHARS);
        assert!(formatted.included_changes <= DEFAULT_MAX_LINES);
        assert_eq!(formatted.included_changes + formatted.omitted_changes, 80);
        assert!(
            formatted.material_text.contains("more changes omitted"),
            "expected truncation summary in: {}",
            formatted.material_text
        );
    }

    #[test]
    fn bypasses_model_for_reminders_events_and_waiting_questions() {
        let batch = LookBatch {
            workspace: "personal".to_owned(),
            at_ms: 3_000,
            bypass_signals: vec![
                BypassSignal::DueReminder {
                    id: "rem-1".to_owned(),
                    label: "Call Jonas at 14:00".to_owned(),
                },
                BypassSignal::ScheduledEvent {
                    id: "evt-1".to_owned(),
                    name: "Investor brief".to_owned(),
                },
                BypassSignal::WaitingQuestion {
                    id: "q-1".to_owned(),
                    summary: "Approve Q3 wire?".to_owned(),
                },
            ],
            ..LookBatch::default()
        };

        assert!(matches!(prepare_screening(&batch), PreCheck::Bypass(ref s) if s.len() == 3));

        // Even a confident No or an outage must never block a bypass signal.
        let low = ScreeningOutcome::YesNo { probability: 0.01 };
        let decision = evaluate_wake(&batch, WakePolicy::default(), Some(&low));
        assert!(matches!(
            decision,
            WakeDecision::Wake {
                reason: WakeReason::Bypass(ref signals)
            } if signals.len() == 3
        ));

        let outage = ScreeningOutcome::Outage {
            detail: "connection refused".to_owned(),
        };
        let decision_outage = evaluate_wake(&batch, WakePolicy::default(), Some(&outage));
        assert!(matches!(
            decision_outage,
            WakeDecision::Wake {
                reason: WakeReason::Bypass(_)
            }
        ));
    }

    #[test]
    fn builds_yes_no_screening_request_with_context_state() {
        let batch = LookBatch {
            workspace: "fundraise".to_owned(),
            at_ms: 4_000,
            changes: vec![sample_change(
                "Elena",
                "assign",
                "Investor packet checklist",
            )],
            incoming_mail_count: 2,
            mail_snippets: vec!["Jonas: Follow-up on Series A".to_owned()],
            due_tasks: vec![DueTaskLine {
                id: "t-1".to_owned(),
                title: "Send updated deck".to_owned(),
                due_ms: 5_000,
                overdue: false,
            }],
            bypass_signals: Vec::new(),
            active_context: Some("Raising Series A in Q1".to_owned()),
        };

        let PreCheck::Screen(req) = prepare_screening(&batch) else {
            panic!("expected screening request");
        };
        assert_eq!(req.question.id, WAKE_QUESTION_ID);
        assert!(req.question.question.contains("Answer yes when in doubt"));
        assert_eq!(
            req.state.get("workspace").map(String::as_str),
            Some("fundraise")
        );
        assert_eq!(req.state.get("change_count").map(String::as_str), Some("1"));
        assert_eq!(
            req.state.get("incoming_mail").map(String::as_str),
            Some("2")
        );
        assert_eq!(req.state.get("due_tasks").map(String::as_str), Some("1"));
        assert_eq!(
            req.state.get("active_context").map(String::as_str),
            Some("Raising Series A in Q1")
        );
        assert!(req.material_text.contains("[mail] 2 new arrival(s)"));
        assert!(req.material_text.contains("[tasks] due soon"));
    }

    #[test]
    fn low_threshold_policy_wakes_on_yes_and_unsure_and_drops_only_confident_no() {
        let batch = LookBatch {
            workspace: "acme".to_owned(),
            at_ms: 5_000,
            changes: vec![sample_change("Sam", "edit", "Roadmap")],
            ..LookBatch::default()
        };
        let policy = WakePolicy::default();

        // Confident No drops with a structured entry.
        let low = ScreeningOutcome::YesNo { probability: 0.12 };
        match evaluate_wake(&batch, policy, Some(&low)) {
            WakeDecision::Drop(entry) => {
                assert_eq!(entry.workspace, "acme");
                assert_eq!(entry.at_ms, 5_000);
                assert_eq!(entry.change_count, 1);
                assert!((entry.probability - 0.12).abs() < f32::EPSILON);
                assert!(!entry.summary_lines.is_empty());
            }
            other => panic!("expected Drop, got {other:?}"),
        }

        // Middle band wakes under the default low-threshold policy.
        let mid = ScreeningOutcome::YesNo { probability: 0.35 };
        assert!(matches!(
            evaluate_wake(&batch, policy, Some(&mid)),
            WakeDecision::Wake {
                reason: WakeReason::ModelUnsure { .. }
            }
        ));

        // Above yes_min wakes immediately.
        let high = ScreeningOutcome::YesNo { probability: 0.50 };
        assert!(matches!(
            evaluate_wake(&batch, policy, Some(&high)),
            WakeDecision::Wake {
                reason: WakeReason::ModelYes { .. }
            }
        ));
    }

    #[test]
    fn never_coerces_unknown_invalid_missing_or_outage_to_no() {
        let batch = LookBatch {
            workspace: "acme".to_owned(),
            at_ms: 6_000,
            changes: vec![sample_change("Sam", "comment", "Incident doc")],
            ..LookBatch::default()
        };
        let policy = WakePolicy::default();

        // Missing response.
        assert!(matches!(
            evaluate_wake(&batch, policy, None),
            WakeDecision::Wake {
                reason: WakeReason::FallbackUnknown {
                    cause: UnknownCause::OmittedByEndpoint
                }
            }
        ));

        // Explicit Unknown.
        let unknown = ScreeningOutcome::Unknown {
            cause: UnknownCause::Abstained,
        };
        assert!(matches!(
            evaluate_wake(&batch, policy, Some(&unknown)),
            WakeDecision::Wake {
                reason: WakeReason::FallbackUnknown {
                    cause: UnknownCause::Abstained
                }
            }
        ));

        // Outage.
        let outage = ScreeningOutcome::Outage {
            detail: "timeout after 3s".to_owned(),
        };
        assert!(matches!(
            evaluate_wake(&batch, policy, Some(&outage)),
            WakeDecision::Wake {
                reason: WakeReason::FallbackOutage { .. }
            }
        ));

        // Non-finite and out-of-range probabilities.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.2] {
            let outcome = ScreeningOutcome::YesNo { probability: bad };
            assert!(matches!(
                evaluate_wake(&batch, policy, Some(&outcome)),
                WakeDecision::Wake {
                    reason: WakeReason::FallbackUnknown {
                        cause: UnknownCause::InvalidProbability
                    }
                }
            ));
        }
    }

    #[test]
    fn rejects_invalid_wake_policy_thresholds() {
        assert!(WakePolicy::new(0.5, 0.5, true).is_err());
        assert!(WakePolicy::new(0.6, 0.4, true).is_err());
        assert!(WakePolicy::new(-0.1, 0.5, true).is_err());
        assert!(WakePolicy::new(0.1, 1.1, true).is_err());
        assert!(WakePolicy::new(f32::NAN, 0.5, true).is_err());
        assert!(WakePolicy::new(0.2, 0.4, true).is_ok());
    }

    #[test]
    fn drop_log_caps_entries_drains_for_backstop_and_formats_digest() {
        let mut log = DropLog::with_capacity(2);
        for (idx, at_ms) in [(1, 100), (2, 200), (3, 300)] {
            log.push(DropEntry {
                at_ms,
                workspace: format!("ws-{idx}"),
                probability: 0.08,
                threshold_no_max: DEFAULT_NO_MAX,
                change_count: idx,
                incoming_mail: 0,
                due_tasks: 0,
                summary_lines: vec![format!("[notes] Bob edited Doc {idx}")],
                reason: "below threshold".to_owned(),
            });
        }

        // Oldest entry evicted when exceeding capacity 2.
        assert_eq!(log.len(), 2);
        let digest = log.format_backstop_digest(500);
        assert!(digest.contains("ws-2"));
        assert!(digest.contains("ws-3"));
        assert!(!digest.contains("ws-1"));

        let drained = log.drain_since(250);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].workspace, "ws-3");
        assert_eq!(log.len(), 1);
    }
}
