//! Finding the room's records by what they hold.
//!
//! A planner is shown the records it has open and the room's others by
//! title. A task that says "the Q3 partner review" without its title, or
//! names a record among stale copies and decoys, needs the records' text:
//! [`SEARCH_ROOM`] ranks every record the agent may open by the request's
//! words (BM25 over each record's plain text, its title weighted more, a
//! phrase found whole counted more again; the most recently changed first
//! among equals) and answers with the best, a snippet or two of each, and
//! the id that opens it in a plan.
//!
//! The text is read app-agnostically from the record's Automerge document
//! ([`plain_text`]): its text objects and the strings that read as words,
//! never the app's code, at most [`MOST_TEXT_BYTES`] of it. The participant
//! reads it once per change of the record ([`crate::PlanContext::searchable`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use automerge::{ObjId, ObjType, ReadDoc, ScalarValueRef, ValueRef};
use tilo_agent::surface;
use tilo_browser_peer::DocumentRoute;

/// The planning tool that searches the room's records.
pub const SEARCH_ROOM: &str = "search_room";

/// The most bytes of one record's text kept for search: its start and its
/// end (where a thread's latest messages are) when it holds more.
pub const MOST_TEXT_BYTES: usize = 20_000;

/// Of [`MOST_TEXT_BYTES`], how many are the record's end when it is cut.
const TAIL_BYTES: usize = 6_000;

/// The most bytes of text gathered from one document before it is cut.
const MOST_GATHERED_BYTES: usize = 256 * 1024;

/// The most values of one document read for its text.
const MOST_VISITED: usize = 200_000;

/// How deep into nested objects the text is read.
const MOST_DEPTH: usize = 48;

/// The most records a plan searches: those open, then those people have
/// open, then the latest changed.
pub const MOST_SEARCHED: usize = 256;

/// The most records one search answers with.
pub const MOST_HITS: usize = 8;

/// The most snippets shown per record, and the most characters of each.
const MOST_SNIPPETS: usize = 2;
const MOST_SNIPPET_CHARS: usize = 200;

/// BM25's parameters, and how much more a word of a record's title counts
/// than one of its body.
const K1: f64 = 1.2;
const B: f64 = 0.75;
const TITLE_WEIGHT: f64 = 2.5;

/// Keys whose string values are an app's bookkeeping (ids, kinds, colours),
/// never what a person wrote.
const BOOKKEEPING_KEYS: &[&str] = &[
    "id", "ids", "type", "kind", "color", "colour", "fill", "stroke", "style", "font", "mime",
    "hash", "align", "schema", "version", "icon", "theme", "cursor", "anchor",
];

/// Words that say nothing about which record is meant.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "the", "this", "that", "to", "with",
];

/// One record of the room a search ranks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchDoc {
    /// The record.
    pub route: DocumentRoute,
    /// What its app calls it, when it names it.
    pub title: Option<String>,
    /// Its plain text ([`plain_text`]).
    pub text: Arc<str>,
    /// When it last changed, as a count that only grows (higher is later);
    /// `None` when not known.
    pub changed: Option<u64>,
    /// It is open, with its operations listed in this plan.
    pub open: bool,
}

/// The plain text of `doc`: its text objects and its strings that read as
/// words, in document order, one per line. Values under bookkeeping keys
/// (`id`, `type`, `color`…), ids, hashes and inlined data are left out. At
/// most [`MOST_TEXT_BYTES`]: its start and end when it holds more.
#[must_use]
pub fn plain_text(doc: &impl ReadDoc) -> String {
    let mut walk = Walk {
        text: String::new(),
        budget: MOST_VISITED,
    };
    walk.object(doc, &automerge::ROOT, ObjType::Map, 0);
    cut(walk.text)
}

struct Walk {
    text: String,
    budget: usize,
}

impl Walk {
    fn full(&self) -> bool {
        self.budget == 0 || self.text.len() >= MOST_GATHERED_BYTES
    }

    fn push(&mut self, piece: &str) {
        let piece = piece.replace('\u{fffc}', "\n");
        let piece = piece.trim();
        if piece.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(piece);
    }

    fn object(&mut self, doc: &impl ReadDoc, object: &ObjId, kind: ObjType, depth: usize) {
        if self.full() || depth > MOST_DEPTH {
            return;
        }
        match kind {
            ObjType::Map | ObjType::Table => {
                for item in doc.map_range(object, ..) {
                    if self.full() {
                        return;
                    }
                    self.budget = self.budget.saturating_sub(1);
                    let bookkeeping = BOOKKEEPING_KEYS
                        .iter()
                        .any(|key| item.key.eq_ignore_ascii_case(key));
                    match item.value {
                        ValueRef::Object(kind) => {
                            self.object(doc, &item.id(), kind, depth.saturating_add(1));
                        }
                        ValueRef::Scalar(ScalarValueRef::Str(text))
                            if !bookkeeping && wordy(&text) =>
                        {
                            self.push(&text);
                        }
                        ValueRef::Scalar(_) => {}
                    }
                }
            }
            ObjType::List => {
                for item in doc.list_range(object, ..) {
                    if self.full() {
                        return;
                    }
                    self.budget = self.budget.saturating_sub(1);
                    match item.value {
                        ValueRef::Object(kind) => {
                            self.object(doc, &item.id(), kind, depth.saturating_add(1));
                        }
                        ValueRef::Scalar(ScalarValueRef::Str(text)) if wordy(&text) => {
                            self.push(&text);
                        }
                        ValueRef::Scalar(_) => {}
                    }
                }
            }
            ObjType::Text => {
                self.budget = self.budget.saturating_sub(1);
                if let Ok(text) = doc.text(object) {
                    self.push(&text);
                }
            }
        }
    }
}

/// Whether a string value reads as words rather than an id, a hash, a
/// colour or inlined data.
fn wordy(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty()
        || text.starts_with("data:")
        || text.starts_with("tilo.content:")
        || !text.chars().any(char::is_alphanumeric)
    {
        return false;
    }
    if text.contains(char::is_whitespace) {
        return true;
    }
    // One word: short, and not an id (`uuid:…`, hex, `#a0b0c0`).
    let id_like = text.contains(':')
        || text.starts_with('#')
        || (text.len() >= 8
            && text.chars().any(|c| c.is_ascii_digit())
            && text
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_'));
    text.len() <= 40 && !id_like
}

/// `text` within [`MOST_TEXT_BYTES`]: whole, or its start and its end.
fn cut(text: String) -> String {
    if text.len() <= MOST_TEXT_BYTES {
        return text;
    }
    let head = floor_boundary(&text, MOST_TEXT_BYTES.saturating_sub(TAIL_BYTES));
    let tail = ceil_boundary(&text, text.len().saturating_sub(TAIL_BYTES));
    format!(
        "{}\n…\n{}",
        text.get(..head).unwrap_or_default(),
        text.get(tail..).unwrap_or_default()
    )
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at = at.saturating_sub(1);
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    while at < text.len() && !text.is_char_boundary(at) {
        at = at.saturating_add(1);
    }
    at.min(text.len())
}

/// A word as searched: lower case, a plural's `s` and a final `e`
/// dropped; a stopword as it is.
fn term(word: &str) -> String {
    let mut word = word.to_lowercase();
    if STOPWORDS.contains(&word.as_str()) {
        return word;
    }
    let long = |word: &str, more: usize| word.chars().count() > more;
    if long(&word, 4) && word.ends_with("ies") {
        word.truncate(word.len().saturating_sub(3));
        word.push('y');
        return word;
    }
    if long(&word, 3) && word.ends_with('s') && !word.ends_with("ss") {
        word.pop();
    }
    // "launches" and "launch", "caches" and "cache" alike.
    if long(&word, 3) && word.ends_with('e') {
        word.pop();
    }
    word
}

/// Terms, each with where its word starts and ends.
type Terms = Vec<(usize, usize, String)>;

/// The terms of `text`, each with where its word starts and ends.
fn terms(text: &str) -> Terms {
    let mut found = Vec::new();
    let mut start = None;
    for (at, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                if let Some(word) = text.get(from..at) {
                    found.push((from, at, term(word)));
                }
                start = None;
            }
            _ => {}
        }
    }
    found
}

/// The terms a query searches for, in order, without stopwords.
fn query_terms(query: &str) -> Vec<String> {
    terms(query)
        .into_iter()
        .map(|(_, _, term)| term)
        .filter(|term| !STOPWORDS.contains(&term.as_str()))
        .collect()
}

/// Whether `app` (an id, its last segment, or its name in words) names
/// `app_id`.
fn names_app(app: &str, app_id: &str) -> bool {
    let wanted = app.trim().to_lowercase().replace([' ', '_'], "-");
    let own = app_id.to_lowercase();
    !wanted.is_empty()
        && (own == wanted
            || own.rsplit('.').next() == Some(wanted.as_str())
            || own
                .rsplit('.')
                .next()
                .is_some_and(|last| last.contains(&wanted)))
}

/// One record a search found.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Where it is in the searched records.
    pub index: usize,
    /// How well it matches.
    pub score: f64,
    /// Up to two passages that hold the query's words.
    pub snippets: Vec<String>,
}

/// The records of `docs` (only `app`'s, when given) that best match
/// `query`, best first, at most `most`: each record's text and title
/// scored by BM25 over the query's terms, the title's words weighted
/// `TITLE_WEIGHT` times, a record that holds the query's words together
/// as a phrase raised, and one holding fewer of them lowered. Among equal
/// scores the most recently changed comes first.
#[must_use]
pub fn search(docs: &[SearchDoc], query: &str, app: Option<&str>, most: usize) -> Vec<Hit> {
    let wanted: Vec<String> = query_terms(query);
    let distinct: BTreeSet<&str> = wanted.iter().map(String::as_str).collect();
    if distinct.is_empty() {
        return Vec::new();
    }
    let candidates: Vec<(usize, &SearchDoc)> = docs
        .iter()
        .enumerate()
        .filter(|(_, doc)| app.is_none_or(|app| names_app(app, &doc.route.app_id)))
        .collect();
    let indexed: Vec<(Terms, Vec<String>)> = candidates
        .iter()
        .map(|(_, doc)| {
            let title = doc
                .title
                .as_deref()
                .map(|title| terms(title).into_iter().map(|(_, _, term)| term).collect())
                .unwrap_or_default();
            (terms(&doc.text), title)
        })
        .collect();
    let count = float(indexed.len().max(1));
    let average = indexed
        .iter()
        .map(|(body, _)| float(body.len()))
        .sum::<f64>()
        / count;
    let idf: BTreeMap<&str, f64> = distinct
        .iter()
        .map(|term| {
            let holding = indexed
                .iter()
                .filter(|(body, title)| {
                    title.iter().any(|t| t == term) || body.iter().any(|(_, _, t)| t == term)
                })
                .count();
            let holding = float(holding);
            (
                *term,
                (1.0 + (count - holding + 0.5) / (holding + 0.5)).ln(),
            )
        })
        .collect();
    let mut hits: Vec<(Hit, Option<u64>)> = Vec::new();
    for ((index, doc), (body, title)) in candidates.iter().zip(&indexed) {
        let length = float(body.len());
        let mut score = 0.0;
        let mut matched = 0_usize;
        for term in &distinct {
            let in_body = body.iter().filter(|(_, _, t)| t == term).count();
            let in_title = title.iter().filter(|t| t == term).count();
            if in_body == 0 && in_title == 0 {
                continue;
            }
            let (in_body, in_title) = (float(in_body), float(in_title));
            matched = matched.saturating_add(1);
            let weight = idf.get(term).copied().unwrap_or_default();
            let body_part =
                in_body * (K1 + 1.0) / (in_body + K1 * (1.0 - B + B * length / average.max(1.0)));
            let title_part = in_title * (K1 + 1.0) / (in_title + K1);
            score += weight * (body_part + TITLE_WEIGHT * title_part);
        }
        if matched == 0 {
            continue;
        }
        if wanted.len() > 1 {
            let whole: f64 = idf.values().sum();
            if holds_phrase(title.iter().map(String::as_str), &wanted) {
                score += whole;
            } else if holds_phrase(body.iter().map(|(_, _, t)| t.as_str()), &wanted) {
                score += whole / 2.0;
            }
        }
        score *= 0.5 + 0.5 * float(matched) / float(distinct.len());
        hits.push((
            Hit {
                index: *index,
                score,
                snippets: Vec::new(),
            },
            doc.changed,
        ));
    }
    hits.sort_by(|(a, a_changed), (b, b_changed)| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b_changed.cmp(a_changed))
            .then_with(|| a.index.cmp(&b.index))
    });
    hits.truncate(most);
    hits.into_iter()
        .map(|(mut hit, _)| {
            if let Some(doc) = docs.get(hit.index) {
                hit.snippets = snippets(&doc.text, &distinct);
            }
            hit
        })
        .collect()
}

/// `count` as a float, for scoring.
fn float(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Whether `terms` holds `phrase` as consecutive terms.
fn holds_phrase<'a>(terms: impl Iterator<Item = &'a str>, phrase: &[String]) -> bool {
    let terms: Vec<&str> = terms.collect();
    !phrase.is_empty()
        && terms
            .windows(phrase.len())
            .any(|window| window.iter().zip(phrase).all(|(a, b)| *a == b.as_str()))
}

/// Up to [`MOST_SNIPPETS`] passages of `text` of at most
/// [`MOST_SNIPPET_CHARS`] each, those holding the most of `wanted` first,
/// each on one line, cut at words.
fn snippets(text: &str, wanted: &BTreeSet<&str>) -> Vec<String> {
    let found: Vec<(usize, usize, String)> = terms(text)
        .into_iter()
        .filter(|(_, _, term)| wanted.contains(term.as_str()))
        .collect();
    // A window starting a little before each match, scored by how many of
    // the wanted terms it holds, then how many matches.
    let before = MOST_SNIPPET_CHARS / 4;
    let mut windows: Vec<(usize, usize, usize, usize)> = found
        .iter()
        .map(|(start, _, _)| {
            let from = floor_boundary(text, start.saturating_sub(before));
            let to = floor_boundary(text, from.saturating_add(MOST_SNIPPET_CHARS * 2));
            let inside: Vec<&String> = found
                .iter()
                .filter(|(at, end, _)| *at >= from && *end <= to)
                .map(|(_, _, term)| term)
                .collect();
            let distinct = inside.iter().collect::<BTreeSet<_>>().len();
            (distinct, inside.len(), from, to)
        })
        .collect();
    windows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    let mut chosen: Vec<(usize, usize)> = Vec::new();
    for (_, _, from, to) in windows {
        if chosen.len() >= MOST_SNIPPETS {
            break;
        }
        if chosen.iter().any(|(f, t)| from < *t && *f < to) {
            continue;
        }
        chosen.push((from, to));
    }
    if chosen.is_empty() {
        // Found by its title alone: how the record starts.
        chosen.push((0, floor_boundary(text, MOST_SNIPPET_CHARS * 2)));
    }
    chosen.sort_unstable();
    chosen
        .into_iter()
        .filter_map(|(from, to)| passage(text, from, to))
        .collect()
}

/// `text[from..to]` on one line, cut at words to [`MOST_SNIPPET_CHARS`],
/// marked where it starts or ends inside the text. `None` when empty.
fn passage(text: &str, from: usize, to: usize) -> Option<String> {
    let slice = text.get(from..to)?;
    // Start at a word: drop a partial first word unless the text starts here.
    let slice = if from > 0 {
        slice
            .find(char::is_whitespace)
            .and_then(|at| slice.get(at..))
            .unwrap_or(slice)
    } else {
        slice
    };
    let flat = slice.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let lead = if from > 0 { "…" } else { "" };
    let budget = MOST_SNIPPET_CHARS.saturating_sub(2);
    let mut kept = String::new();
    for word in flat.split(' ') {
        let next = kept
            .chars()
            .count()
            .saturating_add(word.chars().count())
            .saturating_add(1);
        if next > budget {
            break;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(word);
    }
    if kept.is_empty() {
        kept = flat.chars().take(budget).collect();
    }
    let ends_inside = to < text.len() || kept.len() < flat.len();
    Some(format!(
        "{lead}{kept}{}",
        if ends_inside { "…" } else { "" }
    ))
}

/// The id a plan names to open the record at `route`.
#[must_use]
pub fn open_id(route: &DocumentRoute) -> String {
    surface::encode_address(&surface::open_record_address(&route.app_id, &route.record))
}

/// What a record is shown as: its title, or how its text starts.
fn label(doc: &SearchDoc) -> String {
    let named = doc
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| {
            doc.text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
        })
        .unwrap_or("untitled");
    let mut label: String = named.chars().take(80).collect();
    if named.chars().count() > 80 {
        label.push('…');
    }
    label
}

/// The answer to one [`SEARCH_ROOM`] call over `docs`: the best records
/// for its `query` (of its `app`, when given), each with its app, title,
/// how recently it changed, its snippets and the id that opens it, which
/// is revealed so a plan may name it.
pub fn answer(
    input: &serde_json::Value,
    docs: &[SearchDoc],
    revealed: &mut tilo_agent::tools::Revealed,
) -> String {
    let Some(query) = input
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
    else {
        return format!("{SEARCH_ROOM} needs a `query`.");
    };
    let app = input
        .get("app")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|app| !app.is_empty());
    if let Some(app) = app
        && !docs.iter().any(|doc| names_app(app, &doc.route.app_id))
    {
        let apps: BTreeSet<&str> = docs.iter().map(|doc| doc.route.app_id.as_str()).collect();
        return format!(
            "No record of {app} in this room. Apps with records: {}.",
            apps.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    let hits = search(docs, query, app, MOST_HITS);
    if hits.is_empty() {
        return format!(
            "No record of the {} searched holds the words of {query:?}.",
            docs.len()
        );
    }
    // Recency: 1 for the record that changed last.
    let mut stamps: Vec<u64> = docs.iter().filter_map(|doc| doc.changed).collect();
    stamps.sort_unstable_by(|a, b| b.cmp(a));
    let mut text = format!(
        "{} of {} records match {query:?}, best first (recency 1 = changed last). To work in \
         one, plan its open id as a step and the rest in `then`.\n",
        hits.len(),
        docs.len()
    );
    for (rank, hit) in hits.iter().enumerate() {
        let Some(doc) = docs.get(hit.index) else {
            continue;
        };
        let _ = write!(
            text,
            "{}. {} \"{}\" (record {}",
            rank.saturating_add(1),
            doc.route.app_id,
            label(doc),
            doc.route.record
        );
        if let Some(changed) = doc.changed
            && let Some(at) = stamps.iter().position(|stamp| *stamp == changed)
        {
            let _ = write!(text, ", recency {}", at.saturating_add(1));
        }
        if doc.open {
            text.push_str("): open now, its operations are listed\n");
        } else {
            let id = open_id(&doc.route);
            let _ = writeln!(text, "): open with {id}");
            revealed.reveal(id);
        }
        for snippet in &hit.snippets {
            let _ = writeln!(text, "   {snippet}");
        }
    }
    text
}

/// The [`SEARCH_ROOM`] tool definition. Short: every planning request
/// carries it.
#[must_use]
pub fn definition() -> tilo_model_gateway::host_backend::protocol::ToolDefinition {
    tilo_model_gateway::host_backend::protocol::ToolDefinition {
        name: SEARCH_ROOM.into(),
        description: "Search the room's documents (all apps, open or not) by their text and \
            titles. Answers the best matches with snippets and the id that opens each."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Words to find."},
                "app": {"type": "string", "description": "Only this app's documents."}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: "room".into(),
            record: record.into(),
        }
    }

    fn doc(app: &str, record: &str, title: &str, text: &str, changed: u64) -> SearchDoc {
        SearchDoc {
            route: route(app, record),
            title: (!title.is_empty()).then(|| title.to_owned()),
            text: text.into(),
            changed: Some(changed),
            open: false,
        }
    }

    /// The room of a "deck for the Q3 partner review" task: the brief it
    /// means, a stale copy, decoys sharing some of its words, and others.
    fn room() -> Vec<SearchDoc> {
        let filler = "Weekly sync notes. Action items assigned to the team. ".repeat(20);
        vec![
            doc(
                "so.tilo.word-processor",
                "uuid:brief",
                "Partner brief",
                &format!(
                    "{filler}\nAgenda for the Q3 partner review with Northwind: revenue \
                     share, joint roadmap and the renewal terms.\n{filler}"
                ),
                10,
            ),
            doc(
                "so.tilo.word-processor",
                "uuid:stale",
                "Partner brief (old)",
                "Q2 partner review agenda: superseded, see the current brief.",
                3,
            ),
            doc(
                "so.tilo.notes",
                "uuid:decoy",
                "Review checklist",
                "Code review checklist: tests pass, naming, partner APIs untouched.",
                12,
            ),
            doc(
                "so.tilo.sheets",
                "uuid:numbers",
                "",
                "Q3 revenue by region",
                8,
            ),
            doc(
                "so.tilo.notes",
                "uuid:other",
                "Groceries",
                "Milk, eggs.",
                14,
            ),
        ]
    }

    #[test]
    fn the_record_holding_the_phrase_ranks_first_and_unrelated_ones_not_at_all() {
        let docs = room();
        let hits = search(&docs, "Q3 partner review", None, MOST_HITS);
        let order: Vec<&str> = hits
            .iter()
            .map(|hit| docs[hit.index].route.record.as_str())
            .collect();
        assert_eq!(order.first(), Some(&"uuid:brief"), "{order:?}");
        assert!(!order.contains(&"uuid:other"), "{order:?}");
        assert!(
            order.iter().position(|r| *r == "uuid:brief")
                < order.iter().position(|r| *r == "uuid:stale"),
            "{order:?}"
        );
        // A stopword-only query finds nothing; an app narrows.
        assert!(search(&docs, "the of", None, MOST_HITS).is_empty());
        let notes = search(&docs, "review", Some("notes"), MOST_HITS);
        assert_eq!(notes.len(), 1);
        assert_eq!(docs[notes[0].index].route.record, "uuid:decoy");
    }

    #[test]
    fn a_title_counts_more_and_recency_breaks_ties() {
        let docs = vec![
            doc("so.tilo.notes", "uuid:body", "Misc", "budget figures", 1),
            doc("so.tilo.notes", "uuid:title", "Budget", "figures", 1),
            doc("so.tilo.notes", "uuid:older", "Plan", "launch plan", 2),
            doc("so.tilo.notes", "uuid:newer", "Plan", "launch plan", 5),
        ];
        let hits = search(&docs, "budget", None, MOST_HITS);
        assert_eq!(docs[hits[0].index].route.record, "uuid:title");
        let hits = search(&docs, "launch", None, MOST_HITS);
        assert_eq!(docs[hits[0].index].route.record, "uuid:newer");
        assert_eq!(docs[hits[1].index].route.record, "uuid:older");
        // Plurals find the singular.
        assert_eq!(search(&docs, "launches", None, MOST_HITS).len(), 2);
        assert_eq!(term("caches"), term("cache"));
        assert_eq!(term("Policies"), term("policy"));
        assert_eq!(term("this"), "this");
    }

    #[test]
    fn snippets_show_the_matching_passage_within_bounds() {
        let docs = room();
        let hits = search(&docs, "Q3 partner review", None, 1);
        let snippets = &hits[0].snippets;
        assert!(!snippets.is_empty() && snippets.len() <= MOST_SNIPPETS);
        assert!(
            snippets[0].contains("Q3 partner review with Northwind"),
            "{snippets:?}"
        );
        for snippet in snippets {
            assert!(snippet.chars().count() <= MOST_SNIPPET_CHARS, "{snippet}");
            assert!(!snippet.contains('\n'), "{snippet}");
        }
        // Found by title alone: the record's start.
        let docs = vec![doc(
            "so.tilo.notes",
            "uuid:t",
            "Roadmap",
            "First line here.",
            1,
        )];
        let hits = search(&docs, "roadmap", None, 1);
        assert_eq!(hits[0].snippets, vec!["First line here.".to_owned()]);
    }

    #[test]
    fn an_answer_names_and_reveals_the_open_ids_of_its_records() {
        let mut docs = room();
        docs[3].open = true;
        let mut revealed = tilo_agent::tools::Revealed::new();
        let text = answer(
            &serde_json::json!({"query": "Q3 partner review"}),
            &docs,
            &mut revealed,
        );
        let brief = open_id(&docs[0].route);
        assert_eq!(
            brief, "record.so.tilo.word-processor.uuid:brief#open",
            "the id the catalogue offers"
        );
        assert!(text.contains(&brief), "{text}");
        assert!(text.contains("\"Partner brief\""), "{text}");
        assert!(text.contains("recency 3"), "{text}");
        assert!(revealed.contains(&brief));
        assert!(
            text.contains("open now, its operations are listed"),
            "{text}"
        );
        assert!(!revealed.contains(&open_id(&docs[3].route)));
        let first = text.find("uuid:brief").unwrap();
        let stale = text.find("uuid:stale").unwrap();
        assert!(first < stale, "{text}");

        assert!(answer(&serde_json::json!({}), &docs, &mut revealed).contains("needs a `query`"));
        let none = answer(&serde_json::json!({"query": "zebra"}), &docs, &mut revealed);
        assert!(none.starts_with("No record"), "{none}");
        let other = answer(
            &serde_json::json!({"query": "review", "app": "so.tilo.mail"}),
            &docs,
            &mut revealed,
        );
        assert!(other.contains("No record of so.tilo.mail"), "{other}");
    }

    #[test]
    fn plain_text_reads_words_and_leaves_bookkeeping_out() {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "id", "uuid:1234-5678").unwrap();
        doc.put(automerge::ROOT, "title", "Quarterly review")
            .unwrap();
        doc.put(automerge::ROOT, "color", "#ff00aa").unwrap();
        doc.put(automerge::ROOT, "picture", "data:image/png;base64,AAAA")
            .unwrap();
        doc.put(automerge::ROOT, "hash", "deadbeef00112233")
            .unwrap();
        doc.put(automerge::ROOT, "count", 42_i64).unwrap();
        let body = doc
            .put_object(automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        doc.splice_text(&body, 0, 0, "Agenda\u{fffc}Partner terms")
            .unwrap();
        let messages = doc
            .put_object(automerge::ROOT, "messages", ObjType::List)
            .unwrap();
        let message = doc.insert_object(&messages, 0, ObjType::Map).unwrap();
        doc.put(&message, "from", "Ana").unwrap();
        doc.put(&message, "kind", "chat").unwrap();
        doc.put(&message, "token", "a3f9c2d18e7b6a5c4d3e2f1a0b9c8d7e")
            .unwrap();
        let text = plain_text(&doc);
        for kept in ["Quarterly review", "Agenda\nPartner terms", "Ana"] {
            assert!(text.contains(kept), "{kept} in {text:?}");
        }
        for left in [
            "uuid:", "#ff00aa", "data:", "deadbeef", "chat", "42", "a3f9c2",
        ] {
            assert!(!text.contains(left), "{left} in {text:?}");
        }
    }

    #[test]
    fn plain_text_is_bounded_keeping_start_and_end() {
        let mut doc = automerge::AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "messages", ObjType::List)
            .unwrap();
        for at in 0..3_000_usize {
            doc.insert(&list, at, format!("message number {at} says hello"))
                .unwrap();
        }
        let text = plain_text(&doc);
        assert!(text.len() <= MOST_TEXT_BYTES + 8, "{}", text.len());
        assert!(text.starts_with("message number 0 says"));
        assert!(
            text.ends_with("message number 2999 says hello"),
            "the latest kept"
        );
    }
}
