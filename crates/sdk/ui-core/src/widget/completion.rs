//! What a completion drawer offers and in which order: the sources a field completes from
//! (`ui.Completion(trigger: "@", from: …)`), and the rows a typed token shows from them.
//!
//! A field's view gives each trigger its source once ([`CompletionSource`]): the candidates in
//! priority tiers (this conversation, then the channel, then the workspace), sources that
//! answer later under their own header ([`SlowTier`]), and whether a missing one may be
//! created. Everything per keystroke happens here, in the UI, against what the source gave:
//! the token after the trigger is the query, and [`rows`] orders the candidates for it.
//!
//! - **No letters yet:** each tier under its header, up to its limit.
//! - **While typing:** one list without headers. A name or handle starting with the query
//!   comes first, then a word in the name starting with it, then the query anywhere in the name
//!   or handle, then close fuzzy matches (the query's letters in order from a word start).
//!   Within each, the tiers' order, then the source's own. Accents and case fold as system
//!   search folds them.
//! - **Slow sources** answer below the local rows under their own header, with progress
//!   while they are on their way, and never reorder what is shown.
//! - **No match:** one row saying so, and "Create …" where the source allows it.
//!
//! A pick ([`Pick`]) is what the field writes in place of the token: the candidate's inline
//! text and, where the field keeps rich text, the reference it names.

use std::ops::Range;

use crate::icons::Icon;
use crate::widget::autocomplete::{RowRole, Suggestion, SuggestionPerson};

/// The most rows offered for one token, whatever the source holds.
pub const MAX_ROWS: usize = 50;

/// One thing a completion offers: how it reads in the drawer and what a pick writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    /// What identifies it across tiers and keystrokes: its reference, or its text.
    pub key: String,
    /// What a pick writes in place of the token: the inline view's text (`@Ada Lovelace`,
    /// `#general`, an emoji).
    pub insert: String,
    /// The reference a pick names, for a field that keeps rich text (the `mention` mark's
    /// value). `None` writes plain text.
    pub reference: Option<String>,
    /// The name matched and shown, its matched letters bold.
    pub name: String,
    /// A second name it is found by, shown dim after the name: a handle, a shortcode.
    pub handle: Option<String>,
    /// A line about it, shown dim after the name: a channel's topic, a command's description.
    pub aside: Option<String>,
    /// A label on the row's trailing side: "in this channel", a role, an unread count.
    pub label: Option<String>,
    pub icon: Option<Icon>,
    /// The participant it names, shown as their avatar.
    pub person: Option<SuggestionPerson>,
    /// The command it runs, for a `/` source.
    pub command: Option<Command>,
}

impl Candidate {
    /// A candidate named `name` that writes `insert` and names `reference`.
    #[must_use]
    pub fn reference(
        name: impl Into<String>,
        insert: impl Into<String>,
        reference: impl Into<String>,
    ) -> Self {
        let reference = reference.into();
        Self {
            key: reference.clone(),
            insert: insert.into(),
            reference: Some(reference),
            name: name.into(),
            ..Self::default()
        }
    }

    /// A candidate that writes `insert` as plain text, shown as `name`.
    #[must_use]
    pub fn text(name: impl Into<String>, insert: impl Into<String>) -> Self {
        let insert = insert.into();
        Self {
            key: insert.clone(),
            insert,
            name: name.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_handle(mut self, handle: impl Into<String>) -> Self {
        self.handle = Some(handle.into());
        self
    }

    #[must_use]
    pub fn with_aside(mut self, aside: impl Into<String>) -> Self {
        self.aside = Some(aside.into());
        self
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_person(mut self, person: SuggestionPerson) -> Self {
        self.person = Some(person);
        self
    }

    #[must_use]
    pub fn with_command(mut self, command: Command) -> Self {
        self.command = Some(command);
        self
    }
}

/// A command a `/` source offers: an action, with the parameters its signature asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// What it is typed as, without the `/`.
    pub name: String,
    /// The app it belongs to, for grouping when many apps offer commands.
    pub app: Option<String>,
    /// Its parameters, in the signature's order.
    pub params: Vec<Param>,
    /// Running it removes something; it shows what it will do before Enter runs it.
    pub destructive: bool,
}

impl Command {
    /// The parameter hints a row shows (`person duration message`).
    #[must_use]
    pub fn hints(&self) -> String {
        self.params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One parameter of a command, as its inline field asks for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Param {
    /// The signature's name for it.
    pub name: String,
    /// What its field is labelled.
    pub label: String,
    pub kind: ParamKind,
    /// Enter waits until it is filled.
    pub required: bool,
    /// What it holds before anything is typed.
    pub default: Option<String>,
}

/// What a parameter's field takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ParamKind {
    #[default]
    Text,
    /// A whole number.
    Number,
    /// A yes or no.
    Flag,
    /// How long, or until when: `2h`, `30m`, `tomorrow 9:00`.
    Duration,
    /// Someone, picked from the people drawer (the field's `@` source).
    Person,
    /// One of these.
    Choice(Vec<String>),
}

/// One tier of a source: candidates of one priority, under a header shown before anything
/// is typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tier {
    /// What the tier is called ("In this conversation", "Members").
    pub header: Option<String>,
    /// The most rows it shows before anything is typed.
    pub limit: Option<usize>,
    pub candidates: Vec<Candidate>,
}

impl Tier {
    #[must_use]
    pub fn new(candidates: Vec<Candidate>) -> Self {
        Self {
            header: None,
            limit: None,
            candidates,
        }
    }

    #[must_use]
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// A source that answers later (people outside the workspace, a directory): its rows come
/// under its own header below the local ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlowTier {
    pub header: String,
    /// What it answered; `None` while it is on its way.
    pub candidates: Option<Vec<Candidate>>,
}

/// What one trigger completes from, as a field's view gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSource {
    /// The character that opens the drawer.
    pub trigger: char,
    /// The candidates, in priority tiers.
    pub tiers: Vec<Tier>,
    /// Sources that answer later.
    pub slow: Vec<SlowTier>,
    /// What creating a missing one is called ("Create channel"), when the person may.
    pub create: Option<String>,
}

impl CompletionSource {
    #[must_use]
    pub fn new(trigger: char, tiers: Vec<Tier>) -> Self {
        Self {
            trigger,
            tiers,
            slow: Vec::new(),
            create: None,
        }
    }

    #[must_use]
    pub fn with_slow(mut self, slow: SlowTier) -> Self {
        self.slow.push(slow);
        self
    }

    #[must_use]
    pub fn with_create(mut self, create: impl Into<String>) -> Self {
        self.create = Some(create.into());
        self
    }

    /// Whether picking from it runs a command rather than writing text.
    #[must_use]
    pub fn is_commands(&self) -> bool {
        self.tiers
            .iter()
            .flat_map(|tier| &tier.candidates)
            .any(|candidate| candidate.command.is_some())
    }
}

/// What a pick writes in place of the token, or the command it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Write the candidate (its inline text, and its reference where the field keeps rich
    /// text), then a space.
    Insert(Candidate),
    /// Start the command: the field becomes its chip and parameter fields.
    Command(Candidate),
    /// Create one named by the query, as the source offered.
    Create { trigger: char, query: String },
}

/// One row of the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A tier's header; `busy` while a slow source is on its way.
    Header { title: String, busy: bool },
    /// A candidate, with the byte ranges of its name the query matched.
    Item {
        candidate: Candidate,
        matched: Vec<Range<usize>>,
    },
    /// Nothing matches the query.
    NoMatch(String),
    /// Create one named by the query.
    Create { label: String, query: String },
}

impl Entry {
    /// Whether the row can be selected and picked.
    #[must_use]
    pub fn is_pickable(&self) -> bool {
        matches!(self, Self::Item { .. } | Self::Create { .. })
    }

    /// What identifies the row across keystrokes.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Item { candidate, .. } => Some(&candidate.key),
            Self::Create { .. } => Some("\u{0}create"),
            Self::Header { .. } | Self::NoMatch(_) => None,
        }
    }

    /// The row as the drawer's list paints it.
    #[must_use]
    pub fn suggestion(&self) -> Suggestion {
        match self {
            Self::Header { title, busy } => {
                let mut row = Suggestion::labelled("", title.clone());
                row.role = RowRole::Header { busy: *busy };
                row
            }
            Self::Item { candidate, matched } => {
                let mut row =
                    Suggestion::labelled(candidate.insert.clone(), candidate.name.clone());
                row.matched.clone_from(matched);
                row.aside = match (&candidate.handle, &candidate.aside, &candidate.command) {
                    (_, _, Some(command)) if !command.params.is_empty() => {
                        Some(candidate.aside.as_ref().map_or_else(
                            || command.hints(),
                            |aside| format!("{aside}  {}", command.hints()),
                        ))
                    }
                    (Some(handle), Some(aside), _) => Some(format!("{handle}  {aside}")),
                    (Some(text), None, _) | (None, Some(text), _) => Some(text.clone()),
                    (None, None, _) => None,
                };
                row.detail.clone_from(&candidate.label);
                row.icon = candidate.icon;
                row.person.clone_from(&candidate.person);
                row
            }
            Self::NoMatch(query) => {
                let mut args = fluent::FluentArgs::new();
                args.set("query", query.clone());
                let mut row = Suggestion::labelled(
                    "",
                    crate::i18n::I18nManager::lookup("completion-no-match", Some(&args)),
                );
                row.role = RowRole::Notice;
                row
            }
            Self::Create { label, query } => {
                Suggestion::labelled(query.clone(), format!("{label} \u{201c}{query}\u{201d}"))
                    .with_icon(Icon::Plus)
            }
        }
    }
}

/// Text folded as system search folds it, remembering where each folded byte came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folded {
    text: String,
    /// The original byte offset of each folded byte, and the original's length at the end.
    origin: Vec<usize>,
}

impl Folded {
    #[must_use]
    pub fn new(raw: &str) -> Self {
        let mut text = String::with_capacity(raw.len());
        let mut origin = Vec::with_capacity(raw.len().saturating_add(1));
        for (offset, scalar) in raw.char_indices() {
            crate::search::fold_char(scalar, &mut |part| {
                text.push(part);
                origin.resize(text.len(), offset);
            });
        }
        origin.push(raw.len());
        Self { text, origin }
    }

    /// The original byte range of the folded range `range`.
    fn raw(&self, range: Range<usize>) -> Range<usize> {
        let start = self.origin.get(range.start).copied().unwrap_or(0);
        // The end is the start of the folded byte after the range, so a scalar folded to
        // several keeps its whole original.
        let end = self
            .origin
            .get(range.end)
            .copied()
            .unwrap_or_else(|| self.origin.last().copied().unwrap_or(0));
        start..end.max(start)
    }

    /// The folded byte offsets where a word starts.
    fn word_starts(&self) -> impl Iterator<Item = usize> + '_ {
        let mut previous: Option<char> = None;
        self.text.char_indices().filter_map(move |(at, scalar)| {
            let starts = scalar.is_alphanumeric() && previous.is_none_or(|p| !p.is_alphanumeric());
            previous = Some(scalar);
            starts.then_some(at)
        })
    }
}

/// How well a candidate matches the query, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchClass {
    /// The name or the handle starts with the query.
    Prefix,
    /// A later word of the name starts with it.
    Word,
    /// The name or the handle holds it.
    Substring,
    /// The query's letters in order, from a word start of the name.
    Fuzzy,
}

/// A candidate with its name and handle folded once, when its source arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub candidate: Candidate,
    name: Folded,
    handle: Option<Folded>,
}

impl Prepared {
    #[must_use]
    pub fn new(candidate: Candidate) -> Self {
        let name = Folded::new(&candidate.name);
        let handle = candidate.handle.as_deref().map(Folded::new);
        Self {
            candidate,
            name,
            handle,
        }
    }

    /// How it matches the folded `query`, with the byte ranges of its name matched.
    #[must_use]
    pub fn matches(&self, query: &str) -> Option<(MatchClass, Vec<Range<usize>>)> {
        if query.is_empty() {
            return Some((MatchClass::Prefix, Vec::new()));
        }
        let name = &self.name.text;
        let handle = self.handle.as_ref().map(|handle| handle.text.as_str());
        let whole = |at: usize| vec![self.name.raw(at..at.saturating_add(query.len()))];
        if name.starts_with(query) {
            return Some((MatchClass::Prefix, whole(0)));
        }
        if handle.is_some_and(|handle| handle.starts_with(query)) {
            return Some((MatchClass::Prefix, Vec::new()));
        }
        if let Some(at) = self
            .name
            .word_starts()
            .find(|&at| name.get(at..).is_some_and(|rest| rest.starts_with(query)))
        {
            return Some((MatchClass::Word, whole(at)));
        }
        if let Some(at) = name.find(query) {
            return Some((MatchClass::Substring, whole(at)));
        }
        if handle.is_some_and(|handle| handle.contains(query)) {
            return Some((MatchClass::Substring, Vec::new()));
        }
        self.fuzzy(query)
            .map(|matched| (MatchClass::Fuzzy, matched))
    }

    /// The query's letters in order through the name, the first at a word start; at least
    /// two letters, so one stray letter never matches everything.
    fn fuzzy(&self, query: &str) -> Option<Vec<Range<usize>>> {
        let wanted: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
        if wanted.len() < 2 {
            return None;
        }
        let name = &self.name.text;
        self.name.word_starts().find_map(|start| {
            let mut matched = Vec::with_capacity(wanted.len());
            let mut letters = wanted.iter().peekable();
            for (offset, scalar) in name.get(start..)?.char_indices() {
                let Some(&&next) = letters.peek() else {
                    break;
                };
                if scalar == next {
                    let at = start.saturating_add(offset);
                    matched.push(self.name.raw(at..at.saturating_add(scalar.len_utf8())));
                    letters.next();
                } else if matched.is_empty() {
                    return None;
                }
            }
            letters.peek().is_none().then_some(matched)
        })
    }
}

/// A source with its candidates folded once: what the drawer filters per keystroke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSource {
    pub source: CompletionSource,
    tiers: Vec<Vec<Prepared>>,
    slow: Vec<Option<Vec<Prepared>>>,
}

impl PreparedSource {
    /// `source`, folded. A candidate in two tiers stays only in the first.
    #[must_use]
    pub fn new(source: CompletionSource) -> Self {
        let mut seen = std::collections::HashSet::new();
        let tiers = source
            .tiers
            .iter()
            .map(|tier| {
                tier.candidates
                    .iter()
                    .filter(|candidate| seen.insert(candidate.key.clone()))
                    .cloned()
                    .map(Prepared::new)
                    .collect()
            })
            .collect();
        let slow = source
            .slow
            .iter()
            .map(|slow| {
                slow.candidates.as_ref().map(|candidates| {
                    candidates
                        .iter()
                        .filter(|candidate| !seen.contains(&candidate.key))
                        .cloned()
                        .map(Prepared::new)
                        .collect()
                })
            })
            .collect();
        Self {
            source,
            tiers,
            slow,
        }
    }

    pub fn trigger(&self) -> char {
        self.source.trigger
    }
}

/// The drawer's rows for `query` typed after the trigger of `source` (see the module docs).
#[must_use]
pub fn rows(source: &PreparedSource, query: &str) -> Vec<Entry> {
    let folded = crate::search::fold_search_text(query.trim_start());
    let folded = folded.trim_end();
    let mut out = Vec::new();
    if folded.is_empty() {
        for (tier, prepared) in source.source.tiers.iter().zip(&source.tiers) {
            if prepared.is_empty() {
                continue;
            }
            if let Some(header) = &tier.header {
                out.push(Entry::Header {
                    title: header.clone(),
                    busy: false,
                });
            }
            let limit = tier.limit.unwrap_or(usize::MAX);
            out.extend(prepared.iter().take(limit).map(|p| Entry::Item {
                candidate: p.candidate.clone(),
                matched: Vec::new(),
            }));
        }
    } else {
        let mut found: Vec<(MatchClass, usize, usize, &Prepared, Vec<Range<usize>>)> = Vec::new();
        for (tier, prepared) in source.tiers.iter().enumerate() {
            for (order, candidate) in prepared.iter().enumerate() {
                if let Some((class, matched)) = candidate.matches(folded) {
                    found.push((class, tier, order, candidate, matched));
                }
            }
        }
        found.sort_by_key(|&(class, tier, order, ..)| (class, tier, order));
        out.extend(
            found
                .into_iter()
                .map(|(.., candidate, matched)| Entry::Item {
                    candidate: candidate.candidate.clone(),
                    matched,
                }),
        );
    }
    out.truncate(MAX_ROWS);
    let local = out.iter().filter(|entry| entry.is_pickable()).count();
    let mut slow_rows = 0usize;
    for (slow, prepared) in source.source.slow.iter().zip(&source.slow) {
        let Some(prepared) = prepared else {
            out.push(Entry::Header {
                title: slow.header.clone(),
                busy: true,
            });
            continue;
        };
        let mut found: Vec<(MatchClass, usize, &Prepared, Vec<Range<usize>>)> = prepared
            .iter()
            .enumerate()
            .filter_map(|(order, p)| p.matches(folded).map(|(c, m)| (c, order, p, m)))
            .collect();
        if found.is_empty() {
            continue;
        }
        found.sort_by_key(|&(class, order, ..)| (class, order));
        out.push(Entry::Header {
            title: slow.header.clone(),
            busy: false,
        });
        slow_rows = slow_rows.saturating_add(found.len());
        out.extend(found.into_iter().map(|(.., p, matched)| Entry::Item {
            candidate: p.candidate.clone(),
            matched,
        }));
    }
    if local == 0 && slow_rows == 0 && !folded.is_empty() {
        let pending = source.slow.iter().any(Option::is_none);
        if !pending {
            out.insert(0, Entry::NoMatch(query.trim().to_owned()));
        }
        if let Some(label) = &source.source.create {
            out.push(Entry::Create {
                label: label.clone(),
                query: query.trim().to_owned(),
            });
        }
    }
    out
}

/// `next` with the row `kept` (the one the person selected) back at `place` where it still
/// matches: a still-matching selected row keeps its place while the rest narrow around it.
/// Returns the rows and where the selection is now.
#[must_use]
pub fn keep_place(
    mut next: Vec<Entry>,
    kept: Option<(&str, usize)>,
) -> (Vec<Entry>, Option<usize>) {
    let first = next.iter().position(Entry::is_pickable);
    let Some((key, place)) = kept else {
        return (next, first);
    };
    let Some(now) = next.iter().position(|entry| entry.key() == Some(key)) else {
        return (next, first);
    };
    // Only within the local rows: a row never moves across a header.
    let headers_between = next
        .get(now.min(place)..=now.max(place))
        .is_none_or(|span| span.iter().any(|entry| !entry.is_pickable()));
    if place >= next.len() || headers_between {
        return (next, Some(now));
    }
    let row = next.remove(now);
    next.insert(place, row);
    (next, Some(place))
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
