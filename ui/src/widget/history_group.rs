//! Turning a flood of individual changes into a small number of things a
//! person can name, collapse and restore to. History surfaces show
//! **groups**, expanding to individual changes on demand.
//!
//! A boundary forms when: the author changed; the clock jumped more than
//! [`GroupingConfig::idle_gap`] (an unstamped change is not a gap —
//! splitting on it would tear a session differently per peer); the op
//! family changed (matching the timeline's filter chips); or the group hit
//! [`GroupingConfig::max_changes`]. All are checked against data every
//! change already carries, so grouping is a pure function of the change
//! graph and every peer computes the same groups. Not used: "a burst of
//! edits to the same region" — its boundaries would move as the document
//! changes, so two peers could group it differently.
//!
//! A [`NamedRange`] is a user's name over a run of changes, authoritative
//! and never split or extended automatically. Named ranges nest one level
//! ([`MAX_GROUP_DEPTH`]); see [`group_entries`] for concurrent naming.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use collab_types::OpClass;

use crate::widget::history::{HistoryAuthor, HistoryEntry, HistoryEntryId};

/// Five minutes: long enough to survive reading a paragraph before typing
/// the next one, short enough that a coffee break shows up as a break.
pub const DEFAULT_IDLE_GAP: Duration = Duration::from_mins(5);

/// Ceiling on how many changes one automatic group may hold.
pub const DEFAULT_MAX_GROUP: usize = 200;

/// Two levels is what a reader can hold; three is a tree of edit history
/// nobody asked for.
pub const MAX_GROUP_DEPTH: usize = 2;

/// A user's name over a run of history, as stored in the document.
///
/// `end` is `None` while the group is still **open**: someone said what they
/// were about to do and is still doing it, so the range absorbs every change
/// after `start` until it is closed.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedRange {
    /// Identity of the naming record in the document. A rename addresses this.
    pub id: String,
    pub label: String,
    pub start: HistoryEntryId,
    pub end: Option<HistoryEntryId>,
    /// Who wrote the name — not necessarily who authored the changes.
    pub named_by: Option<HistoryAuthor>,
    pub created: SystemTime,
}

/// What a host must write when a user names, renames, or un-names a run of
/// changes. Both history surfaces emit this rather than writing to the
/// document directly — neither widget has one, only the host does.
///
/// An empty `label` is a request to **remove** the name: the host maps that
/// onto `ui::history_source::groups::clear_name`.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupNameRequest {
    /// The naming record being replaced, when one already exists.
    pub record_id: Option<String>,
    pub start: HistoryEntryId,
    pub end: HistoryEntryId,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupingConfig {
    pub idle_gap: Duration,
    pub max_changes: usize,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        Self {
            idle_gap: DEFAULT_IDLE_GAP,
            max_changes: DEFAULT_MAX_GROUP,
        }
    }
}

/// Grouping never mixes two families, so a filter chip can never empty a
/// group while leaving its header claiming rows that are no longer shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpFamily {
    Edit,
    Comment,
    Governance,
    Deletion,
}

impl OpFamily {
    pub fn of(op: OpClass) -> Self {
        match op {
            OpClass::Splice | OpClass::Move | OpClass::EditMeta => OpFamily::Edit,
            OpClass::Comment => OpFamily::Comment,
            OpClass::Govern | OpClass::Compaction | OpClass::Redact => OpFamily::Governance,
            OpClass::Tombstone | OpClass::Restore => OpFamily::Deletion,
        }
    }

    fn plural(self) -> &'static str {
        match self {
            OpFamily::Edit => "edits",
            OpFamily::Comment => "comments",
            OpFamily::Governance => "permission changes",
            OpFamily::Deletion => "deletions",
        }
    }
}

/// One collapsible band in a history surface.
///
/// `start..end` are indices into the entry slice the group was built from, in
/// the producer's causal order. `children` are named sub-ranges; entries inside
/// `start..end` that no child covers are the group's own rows.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryGroup {
    /// Derived from the group's first change, which does not move as the
    /// group grows, so expand/collapse survives a refresh.
    pub key: String,
    pub name: Option<String>,
    /// Record id of the naming, so a rename can address it.
    pub record_id: Option<String>,
    pub named_by: Option<HistoryAuthor>,
    /// True while the range is still absorbing new changes.
    pub open: bool,
    pub start: usize,
    pub end: usize,
    /// At most [`MAX_GROUP_DEPTH`] deep in total.
    pub children: Vec<HistoryGroup>,
    /// 0 for a top-level group.
    pub depth: usize,
    /// First-seen order.
    pub authors: Vec<HistoryAuthor>,
    /// First-seen order.
    pub families: Vec<OpFamily>,
    /// Both are `UNIX_EPOCH` when no change in the range carried a clock.
    pub first_time: SystemTime,
    pub last_time: SystemTime,
    /// `None` when at least one change in the range didn't report a count —
    /// a partial total would read as a complete one.
    pub text_delta: Option<(usize, usize)>,
    /// The last one is the restore target: "restore to the end of this
    /// version".
    pub first_entry: HistoryEntryId,
    pub last_entry: HistoryEntryId,
}

impl HistoryGroup {
    /// Number of changes in the range, including those inside children.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// True when this band is a single unnamed change — the surfaces render
    /// those as a plain row rather than as a group with one thing in it.
    pub fn is_bare_change(&self) -> bool {
        self.name.is_none() && self.len() == 1 && self.children.is_empty()
    }

    /// The line a reader scans: the user's name, or an honest description of
    /// what the range contains.
    ///
    /// The derived form never claims more than it counted. It says how many
    /// changes and, when the range spans a minute or more, over how long.
    pub fn title(&self, entries: &[HistoryEntry]) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        if self.len() == 1 {
            return entries
                .get(self.start)
                .map(|e| e.summary.clone())
                .unwrap_or_default();
        }
        let noun = match self.families.as_slice() {
            [only] => only.plural(),
            _ => "changes",
        };
        let mut s = format!("{} {noun}", self.len());
        if let Some(span) = self.span_label() {
            s.push_str(" over ");
            s.push_str(&span);
        }
        s
    }

    /// The dim second line: who, and how much moved.
    pub fn subtitle(&self, entries: &[HistoryEntry]) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.name.is_some() {
            let noun = if self.len() == 1 { "change" } else { "changes" };
            let mut count = format!("{} {noun}", self.len());
            if let Some(span) = self.span_label() {
                count.push_str(" over ");
                count.push_str(&span);
            }
            parts.push(count);
        }
        let who = self.authors_label();
        if !who.is_empty() {
            parts.push(who);
        }
        if let Some((ins, del)) = self.text_delta
            && (ins > 0 || del > 0)
        {
            // Same spelling as a row's own summary: a group that wrote its
            // counts with a different minus sign reads as a different unit.
            parts.push(format!("+{ins} -{del} chars"));
        } else if self.name.is_none()
            && self.len() > 1
            && let Some(first) = entries.get(self.start)
        {
            // No character counts to aggregate (a run of governance acts, say):
            // name the first thing that happened rather than padding the row.
            parts.push(format!("from \u{201c}{}\u{201d}", first.summary));
        }
        if let Some(by) = &self.named_by
            && self.name.is_some()
        {
            parts.push(format!("named by {}", by.name));
        }
        parts.join(" \u{00b7} ")
    }

    /// "Alice", "Alice and Bob", "Alice and 4 others".
    pub fn authors_label(&self) -> String {
        match self.authors.as_slice() {
            [] => String::new(),
            [a] => a.name.clone(),
            [a, b] => format!("{} and {}", a.name, b.name),
            [a, ..] => format!(
                "{} and {} others",
                a.name,
                self.authors.len().saturating_sub(1)
            ),
        }
    }

    /// How long the range took, in the coarsest unit that is still true, or
    /// `None` when it is under a minute or no change carried a clock.
    pub fn span_label(&self) -> Option<String> {
        let span = self.last_time.duration_since(self.first_time).ok()?;
        let secs = span.as_secs();
        if secs < 60 {
            return None;
        }
        let mins = secs / 60;
        if mins < 60 {
            return Some(format!("{mins} min"));
        }
        let hours = mins / 60;
        if hours < 24 {
            return Some(format!("{hours} h"));
        }
        Some(format!("{} d", hours / 24))
    }

    /// Index ranges of this group's own rows — everything in `start..end` that
    /// no child covers. In ascending order.
    pub fn direct_spans(&self) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut cursor = self.start;
        for c in &self.children {
            if c.start > cursor {
                out.push(cursor..c.start);
            }
            cursor = c.end.max(cursor);
        }
        if cursor < self.end {
            out.push(cursor..self.end);
        }
        out
    }

    /// Every group in this subtree, self first, then children. Used by the
    /// surfaces to flatten a forest into a row list.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a HistoryGroup>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

/// A resolved named range, before it is placed in the forest.
#[derive(Clone, Debug)]
struct Placed {
    id: String,
    label: String,
    named_by: Option<HistoryAuthor>,
    start: usize,
    end: usize,
    open: bool,
    children: Vec<Placed>,
}

/// Group `entries` (oldest first, in the producer's causal order).
///
/// # Concurrent naming
///
/// Two people naming ranges at the same time resolves here, at read time,
/// identically on every peer — the inputs are the document and nothing else.
///
/// * **The same range, twice.** Both writes land on the same document key
///   (derived from the range's first change), so automerge's map-conflict
///   rule picks one deterministically; the other stays recoverable in the
///   change graph.
/// * **One range inside another.** The inner one becomes a child of the
///   outer, up to [`MAX_GROUP_DEPTH`]. Deeper than that, it is not rendered
///   as a group but reappears once an enclosing name is removed.
/// * **Ranges that partly overlap.** The earlier-starting range keeps its
///   whole span; the later one is clipped to what's left — clipping rather
///   than dropping means neither user loses their name to a race.
///
/// A named range whose first change is not in `entries` is skipped: the
/// surface cannot honestly show a band whose contents it does not have.
pub fn group_entries(
    entries: &[HistoryEntry],
    named: &[NamedRange],
    cfg: &GroupingConfig,
) -> Vec<HistoryGroup> {
    if entries.is_empty() {
        return Vec::new();
    }
    let index: HashMap<HistoryEntryId, usize> =
        entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();

    // Resolve names to index ranges, dropping the ones this surface cannot
    // place.
    let mut resolved: Vec<Placed> = Vec::new();
    for r in named {
        let Some(&start) = index.get(&r.start) else {
            continue;
        };
        let (end, open) = match r.end {
            None => (entries.len(), true),
            // An end anchor we do not hold means the range runs past the window
            // we loaded. Clamp rather than guess; it is not open, it is longer
            // than what is on screen.
            Some(e) => (
                index.get(&e).map_or(entries.len(), |i| i.saturating_add(1)),
                false,
            ),
        };
        if end <= start {
            continue;
        }
        resolved.push(Placed {
            id: r.id.clone(),
            label: r.label.clone(),
            named_by: r.named_by.clone(),
            start,
            end,
            open,
            children: Vec::new(),
        });
    }
    // Outermost first at a given start, then by record id so two peers order
    // identically.
    resolved.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.id.cmp(&b.id))
    });

    let mut forest: Vec<Placed> = Vec::new();
    for r in resolved {
        insert_placed(&mut forest, r, 1);
    }

    build_level(entries, &forest, 0, entries.len(), 0, cfg)
}

/// Insert `r` into `nodes` (siblings sorted by start), nesting or clipping as
/// documented on [`group_entries`]. `depth` is the depth `r` would occupy.
fn insert_placed(nodes: &mut Vec<Placed>, mut r: Placed, depth: usize) -> bool {
    if let Some(last) = nodes.last_mut()
        && r.start < last.end
    {
        if r.end <= last.end {
            // Contained: nest it, or refuse if that would be too deep.
            if depth < MAX_GROUP_DEPTH {
                return insert_placed(&mut last.children, r, depth.saturating_add(1));
            }
            return false;
        }
        // Partial overlap: the earlier range keeps what it has.
        r.start = last.end;
        if r.start >= r.end {
            return false;
        }
    }
    nodes.push(r);
    true
}

/// Build the groups covering `lo..hi`: the named ranges in `named`, and derived
/// groups over whatever is between them.
fn build_level(
    entries: &[HistoryEntry],
    named: &[Placed],
    lo: usize,
    hi: usize,
    depth: usize,
    cfg: &GroupingConfig,
) -> Vec<HistoryGroup> {
    let mut out: Vec<HistoryGroup> = Vec::new();
    let mut cursor = lo;
    for n in named {
        if n.start >= hi {
            break;
        }
        if n.start > cursor {
            out.extend(derived_groups(entries, cursor, n.start, depth, cfg));
        }
        if let Some(g) = named_group(entries, n, depth) {
            out.push(g);
        }
        cursor = n.end.max(cursor);
    }
    if cursor < hi {
        out.extend(derived_groups(entries, cursor, hi, depth, cfg));
    }
    out
}

/// Turn a placed named range into a band, recursing into the names inside it
/// until [`MAX_GROUP_DEPTH`] levels have been rendered. Inside a named range
/// only *named* sub-ranges become bands — the rest are the group's own rows,
/// since chopping a named version into machine-derived sub-sessions would
/// bury the name someone chose under boundaries they didn't draw. The depth
/// cut also lives in [`insert_placed`]; both must read `MAX_GROUP_DEPTH`.
fn named_group(entries: &[HistoryEntry], n: &Placed, depth: usize) -> Option<HistoryGroup> {
    let next_depth = depth.saturating_add(1);
    let children: Vec<HistoryGroup> = if next_depth < MAX_GROUP_DEPTH {
        n.children
            .iter()
            .filter_map(|c| named_group(entries, c, next_depth))
            .collect()
    } else {
        Vec::new()
    };
    finish_group(entries, n.start, n.end, depth, Some(n), children)
}

/// Chop `lo..hi` at the automatic boundaries.
fn derived_groups(
    entries: &[HistoryEntry],
    lo: usize,
    hi: usize,
    depth: usize,
    cfg: &GroupingConfig,
) -> Vec<HistoryGroup> {
    let mut out = Vec::new();
    let mut start = lo;
    let Some(window) = entries.get(lo..hi) else {
        return out;
    };
    for (offset, pair) in window.windows(2).enumerate() {
        let [prev, cur] = pair else { continue };
        // `offset` is the index of `prev` within `window`, so `cur` sits one
        // past it, at absolute index `lo + offset + 1`.
        let i = lo.saturating_add(offset).saturating_add(1);
        let author_changed = author_key(prev) != author_key(cur);
        let family_changed = OpFamily::of(prev.op_class) != OpFamily::of(cur.op_class);
        // No recorded clock is not evidence of a gap — splitting on it would
        // tear a session differently per peer. A clock running backwards
        // (skewed peer, merged late) likewise reads as no gap, not a
        // negative one.
        let timed =
            prev.timestamp != SystemTime::UNIX_EPOCH && cur.timestamp != SystemTime::UNIX_EPOCH;
        let gap = cur
            .timestamp
            .duration_since(prev.timestamp)
            .unwrap_or_default();
        let idle = timed && gap > cfg.idle_gap;
        let full = i.saturating_sub(start) >= cfg.max_changes;
        if author_changed || family_changed || idle || full {
            if let Some(g) = finish_group(entries, start, i, depth, None, Vec::new()) {
                out.push(g);
            }
            start = i;
        }
    }
    if start < hi
        && let Some(g) = finish_group(entries, start, hi, depth, None, Vec::new())
    {
        out.push(g);
    }
    out
}

/// The identity two consecutive changes are compared on. The short identity
/// hash is included so two different people who chose the same display name are
/// still two groups.
fn author_key(e: &HistoryEntry) -> (&str, &str) {
    (e.author.name.as_str(), e.author.identity_short.as_str())
}

/// Builds the group covering `start..end`, or `None` when that is not a
/// valid, non-empty range into `entries`. Every call site derives `start` and
/// `end` from indices already found in `entries`, so `None` is a defensive
/// fallback rather than a path callers are expected to hit.
fn finish_group(
    entries: &[HistoryEntry],
    start: usize,
    end: usize,
    depth: usize,
    named: Option<&Placed>,
    children: Vec<HistoryGroup>,
) -> Option<HistoryGroup> {
    let slice = entries.get(start..end)?;
    let first = slice.first()?;
    let last = slice.last()?;
    let mut authors: Vec<HistoryAuthor> = Vec::new();
    let mut families: Vec<OpFamily> = Vec::new();
    let mut first_time = SystemTime::UNIX_EPOCH;
    let mut last_time = SystemTime::UNIX_EPOCH;
    let mut seen_time = false;
    let mut ins = 0usize;
    let mut del = 0usize;
    let mut all_counted = true;
    for e in slice {
        if !authors
            .iter()
            .any(|a| a.name == e.author.name && a.identity_short == e.author.identity_short)
        {
            authors.push(e.author.clone());
        }
        let f = OpFamily::of(e.op_class);
        if !families.contains(&f) {
            families.push(f);
        }
        // A change with no recorded clock (`UNIX_EPOCH`) must not drag the span
        // back to 1970 — it is unknown, not ancient.
        if e.timestamp != SystemTime::UNIX_EPOCH {
            if !seen_time || e.timestamp < first_time {
                first_time = e.timestamp;
            }
            if !seen_time || e.timestamp > last_time {
                last_time = e.timestamp;
            }
            seen_time = true;
        }
        match e.text_delta {
            Some((a, b)) => {
                ins = ins.saturating_add(a);
                del = del.saturating_add(b);
            }
            None => all_counted = false,
        }
    }
    let key = match named {
        Some(n) => format!("n:{}", n.id),
        None => format!("d:{}", hex16(&first.id)),
    };
    Some(HistoryGroup {
        key,
        name: named.map(|n| n.label.clone()),
        record_id: named.map(|n| n.id.clone()),
        named_by: named.and_then(|n| n.named_by.clone()),
        open: named.is_some_and(|n| n.open),
        start,
        end,
        children,
        depth,
        authors,
        families,
        first_time,
        last_time,
        text_delta: if all_counted { Some((ins, del)) } else { None },
        first_entry: first.id,
        last_entry: last.id,
    })
}

/// Lower-case hex of an entry id. Also the document key a name is stored under,
/// which is why it lives here rather than in the persistence layer.
pub fn hex16(id: &HistoryEntryId) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(32);
    for b in id.as_bytes() {
        // `write!` to a `String` never fails.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse the hex produced by [`hex16`] back into an id. `None` on anything that
/// is not exactly 32 hex characters, so a corrupted document key is skipped
/// rather than silently resolving to a wrong change.
pub fn from_hex16(s: &str) -> Option<HistoryEntryId> {
    if s.len() != 32 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 16];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let [hi_b, lo_b] = pair else {
            return None;
        };
        let hi = (*hi_b as char).to_digit(16)?;
        let lo = (*lo_b as char).to_digit(16)?;
        // Both digits are 0..=15, so the combined value is 0..=255 and always
        // fits in a `u8`; `try_from` is the honest spelling of that fact.
        *slot = u8::try_from(hi.saturating_mul(16).saturating_add(lo)).ok()?;
    }
    Some(HistoryEntryId(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::ArtifactKind;
    use std::time::Duration;

    fn entry(id: u8, author: &str, secs: u64, op: OpClass) -> HistoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new(author, 120.0, "aaaaaaaa"),
            op_class: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: format!("op {id}"),
            text_delta: Some((3, 0)),
            diff_payload: None,
            redaction: None,
        }
    }

    fn ids(g: &HistoryGroup) -> (usize, usize) {
        (g.start, g.end)
    }

    #[test]
    fn a_typing_run_by_one_person_is_one_group() {
        // Ten changes a second apart by one author: exactly the shape a
        // debounced editor produces, and exactly what must not be ten rows.
        let entries: Vec<HistoryEntry> = (0..10)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        assert_eq!(
            groups.len(),
            1,
            "got {:?}",
            groups.iter().map(ids).collect::<Vec<_>>()
        );
        assert_eq!(groups[0].len(), 10);
        assert_eq!(groups[0].title(&entries), "10 edits");
        assert_eq!(groups[0].subtitle(&entries), "Alice \u{00b7} +30 -0 chars");
    }

    #[test]
    fn a_five_minute_gap_ends_the_session() {
        let mut entries: Vec<HistoryEntry> = (0..3)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        // Six minutes later, same person, same kind of change.
        entries
            .extend((3..6).map(|i| entry(i, "Alice", 1000 + 360 + u64::from(i), OpClass::Splice)));
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        assert_eq!(groups.len(), 2);
        assert_eq!(ids(&groups[0]), (0, 3));
        assert_eq!(ids(&groups[1]), (3, 6));
        // The second group spans six minutes from the first change of the day,
        // but only its own three seconds — the label must describe the group,
        // not the document.
        assert_eq!(
            groups[1].span_label(),
            None,
            "three seconds is not 'over 6 min'"
        );
    }

    #[test]
    fn a_different_author_always_starts_a_new_group() {
        // One second apart, same op class — only the author differs. The
        // grouping must still split, or the header's attribution is a lie.
        let entries = vec![
            entry(0, "Alice", 1000, OpClass::Splice),
            entry(1, "Alice", 1001, OpClass::Splice),
            entry(2, "Bob", 1002, OpClass::Splice),
            entry(3, "Alice", 1003, OpClass::Splice),
        ];
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].authors_label(), "Alice");
        assert_eq!(groups[1].authors_label(), "Bob");
        assert!(
            groups[1].is_bare_change(),
            "one change, unnamed: renders as a row"
        );
    }

    #[test]
    fn a_governance_act_is_not_swallowed_by_a_typing_run() {
        let entries = vec![
            entry(0, "Alice", 1000, OpClass::Splice),
            entry(1, "Alice", 1001, OpClass::Govern),
            entry(2, "Alice", 1002, OpClass::Splice),
        ];
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        assert_eq!(groups.len(), 3, "the grant is its own thing");
        assert_eq!(groups[1].families, vec![OpFamily::Governance]);
    }

    #[test]
    fn a_group_never_grows_past_the_cap() {
        let entries: Vec<HistoryEntry> = (0u32..250)
            .map(|i| {
                let id = u8::try_from(i % 251).unwrap_or(0);
                entry(id, "Alice", 1000 + u64::from(i), OpClass::Splice)
            })
            .collect();
        let cfg = GroupingConfig {
            max_changes: 100,
            ..Default::default()
        };
        let groups = group_entries(&entries, &[], &cfg);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| g.len() <= 100));
        assert_eq!(groups.iter().map(HistoryGroup::len).sum::<usize>(), 250);
    }

    #[test]
    fn a_name_overrides_the_automatic_boundaries_in_both_directions() {
        // Six changes that would automatically split three ways (author changes
        // twice). One name over all six must produce one group.
        let entries = vec![
            entry(0, "Alice", 1000, OpClass::Splice),
            entry(1, "Bob", 1001, OpClass::Splice),
            entry(2, "Alice", 1002, OpClass::Splice),
            entry(3, "Alice", 1003, OpClass::Splice),
            entry(4, "Bob", 1004, OpClass::Splice),
            entry(5, "Alice", 1005, OpClass::Splice),
        ];
        let auto = group_entries(&entries, &[], &GroupingConfig::default());
        assert!(auto.len() > 1, "the fixture must split without a name");

        let named = vec![NamedRange {
            id: "r1".into(),
            label: "Intro rewrite".into(),
            start: entries[0].id,
            end: Some(entries[5].id),
            named_by: Some(HistoryAuthor::new("Alice", 120.0, "aaaaaaaa")),
            created: SystemTime::UNIX_EPOCH,
        }];
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name.as_deref(), Some("Intro rewrite"));
        assert_eq!(groups[0].len(), 6);
        assert_eq!(groups[0].authors_label(), "Alice and Bob");
    }

    #[test]
    fn an_open_group_absorbs_everything_after_its_start() {
        let entries: Vec<HistoryEntry> = (0..5)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i) * 1000, OpClass::Splice))
            .collect();
        let named = vec![NamedRange {
            id: "r1".into(),
            label: "Working".into(),
            start: entries[1].id,
            end: None,
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }];
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        // Change 0 is outside; 1..5 are inside the open group even though the
        // gaps between them are far past the idle threshold.
        assert_eq!(groups.len(), 2);
        assert!(groups[0].is_bare_change());
        assert!(groups[1].open);
        assert_eq!(ids(&groups[1]), (1, 5));
    }

    #[test]
    fn a_name_inside_a_name_nests_one_level() {
        let entries: Vec<HistoryEntry> = (0..8)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        let named = vec![
            NamedRange {
                id: "outer".into(),
                label: "Q3 release".into(),
                start: entries[0].id,
                end: Some(entries[7].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
            NamedRange {
                id: "inner".into(),
                label: "Intro rewrite".into(),
                start: entries[2].id,
                end: Some(entries[4].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
        ];
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        assert_eq!(groups.len(), 1);
        let outer = &groups[0];
        assert_eq!(outer.name.as_deref(), Some("Q3 release"));
        assert_eq!(outer.children.len(), 1);
        assert_eq!(outer.children[0].name.as_deref(), Some("Intro rewrite"));
        assert_eq!(outer.children[0].depth, 1);
        // The parent's own rows are what the child does not cover.
        assert_eq!(outer.direct_spans(), vec![0..2, 5..8]);
    }

    #[test]
    fn a_third_level_of_naming_is_not_rendered_as_a_group() {
        let entries: Vec<HistoryEntry> = (0..8)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        let named = vec![
            NamedRange {
                id: "a".into(),
                label: "outer".into(),
                start: entries[0].id,
                end: Some(entries[7].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
            NamedRange {
                id: "b".into(),
                label: "middle".into(),
                start: entries[1].id,
                end: Some(entries[6].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
            NamedRange {
                id: "c".into(),
                label: "inner".into(),
                start: entries[2].id,
                end: Some(entries[5].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
        ];
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        let mut flat: Vec<&HistoryGroup> = Vec::new();
        for g in &groups {
            g.walk(&mut flat);
        }
        let labels: Vec<&str> = flat.iter().filter_map(|g| g.name.as_deref()).collect();
        assert_eq!(
            labels,
            vec!["outer", "middle"],
            "the third level is not shown"
        );
        assert!(flat.iter().all(|g| g.depth < MAX_GROUP_DEPTH));
    }

    #[test]
    fn two_people_naming_overlapping_runs_converge_on_one_layout() {
        // Alice names 0..5, Bob names 3..8 — a genuine concurrent case. The
        // earlier start keeps its range; the later one is clipped to the tail.
        // Both peers see the same thing whichever order the names merged in.
        let entries: Vec<HistoryEntry> = (0..9)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        let alice = NamedRange {
            id: "alice".into(),
            label: "Intro".into(),
            start: entries[0].id,
            end: Some(entries[4].id),
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        };
        let bob = NamedRange {
            id: "bob".into(),
            label: "Body".into(),
            start: entries[3].id,
            end: Some(entries[7].id),
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        };
        let one = group_entries(
            &entries,
            &[alice.clone(), bob.clone()],
            &GroupingConfig::default(),
        );
        let other = group_entries(&entries, &[bob, alice], &GroupingConfig::default());
        let shape = |gs: &[HistoryGroup]| -> Vec<(Option<String>, usize, usize)> {
            gs.iter()
                .map(|g| (g.name.clone(), g.start, g.end))
                .collect()
        };
        assert_eq!(
            shape(&one),
            shape(&other),
            "merge order must not change the layout"
        );
        assert_eq!(
            shape(&one),
            vec![
                (Some("Intro".to_string()), 0, 5),
                (Some("Body".to_string()), 5, 8),
                (None, 8, 9),
            ]
        );
    }

    #[test]
    fn a_name_whose_first_change_is_missing_is_skipped_not_guessed() {
        let entries: Vec<HistoryEntry> = (0..3)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        let named = vec![NamedRange {
            id: "ghost".into(),
            label: "From another document".into(),
            start: HistoryEntryId([0x5a; 16]),
            end: None,
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }];
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        assert_eq!(groups.len(), 1);
        assert!(
            groups[0].name.is_none(),
            "no band is invented for a range we cannot place"
        );
    }

    #[test]
    fn an_unclocked_change_does_not_drag_the_span_back_to_1970() {
        let mut entries: Vec<HistoryEntry> = (0..3)
            .map(|i| entry(i, "Alice", 100_000 + u64::from(i) * 120, OpClass::Splice))
            .collect();
        entries[1].timestamp = SystemTime::UNIX_EPOCH;
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        let g = groups.last().unwrap();
        assert_eq!(
            g.span_label().as_deref(),
            Some("4 min"),
            "the span is over the changes that actually carried a clock"
        );
    }

    #[test]
    fn a_partial_character_count_is_not_reported_as_a_total() {
        let mut entries: Vec<HistoryEntry> = (0..3)
            .map(|i| entry(i, "Alice", 1000 + u64::from(i), OpClass::Splice))
            .collect();
        entries[1].text_delta = None;
        let groups = group_entries(&entries, &[], &GroupingConfig::default());
        assert_eq!(groups[0].text_delta, None);
        assert!(
            !groups[0].subtitle(&entries).contains("chars"),
            "got {:?}",
            groups[0].subtitle(&entries)
        );
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        let id = HistoryEntryId([0xab; 16]);
        assert_eq!(from_hex16(&hex16(&id)), Some(id));
        assert_eq!(from_hex16("nope"), None);
        assert_eq!(from_hex16(&"z".repeat(32)), None);
    }
}
