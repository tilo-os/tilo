//! What a run of changes was, said in one line: descriptions of single changes and names over the
//! runs a person would recognise as one piece of work ("Overhauled Q3 financial report").
//! Both land in the document the changes belong to, as history metadata beside them
//! (`ui::history_source::{descriptions, groups}`), so every member reads the same words and no
//! device asks a model again. What a person wrote is never replaced: a name this describer wrote is
//! marked [`ui::history_source::GENERATED_BY`], and only such a name is ever rewritten by it.
//! Nothing here holds a model or reaches the network: [`Describer::next`] hands the host one
//! [`Job`] at a time and [`Describer::apply`] writes its answer, so a session asks through the
//! model gateway off the desk's thread and the portable host asks whatever its page injects.
//! Without a model no job is asked at all, and the history keeps its deterministic digest.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ReadDoc as _};
use ui::history_source::groups::MAX_LABEL_LEN;
use ui::history_source::{GENERATED_BY, descriptions, groups};
use ui::models::{Content, Message, ModelRequest, Role, ToolChoice};
use ui::widget::HistoryEntryId;
use ui::widget::history::HistoryEntry;
use ui::widget::history_group::{GroupingConfig, NamedRange, from_hex16, group_entries, hex16};

/// How many changes one description request asks about: enough to spend a request's latency on, few
/// enough that one failure loses little.
pub const BATCH: usize = 12;

/// Past a few changes a run is a thing someone did rather than a keystroke;
/// naming shorter runs would title the person's every paragraph.
pub const MIN_RUN: usize = 3;

const MAX_DESCRIPTION_TOKENS: u32 = 800;
const MAX_TITLE_TOKENS: u32 = 40;

/// One thing the document asks a model for.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// One line per change, for changes nobody has described.
    Describe(Vec<Fact>),
    /// One name over the run of changes from `start` to `end` .
    Title {
        start: HistoryEntryId,
        end: HistoryEntryId,
        facts: Vec<Fact>,
    },
}

/// What one change looks like to a model: the change itself, never who made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub change: String,
    pub summary: String,
    pub purpose: Option<String>,
    pub item: Option<String>,
    pub wrote: String,
}

impl Job {
    /// What this job asks, as its content: the same work is never asked twice.
    #[must_use]
    pub fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        match self {
            Self::Describe(facts) => {
                hasher.update(b"describe");
                for fact in facts {
                    hasher.update(fact.change.as_bytes());
                }
            }
            Self::Title { start, end, facts } => {
                hasher.update(b"title");
                hasher.update(&start.0);
                hasher.update(&end.0);
                for fact in facts {
                    hasher.update(fact.summary.as_bytes());
                }
            }
        }
        hasher.finalize().to_hex().to_string()
    }

    /// The request this job asks of the model.
    #[must_use]
    pub fn request(&self) -> ModelRequest {
        let (instructions, max_output_tokens) = match self {
            Self::Describe(_) => (
                "For each change in the user message, write one short past-tense sentence of at \
                 most 12 words saying what it did to the document, from its facts only: its \
                 summary, its stated purpose, the item it touched and what it wrote. Never name \
                 a person, never guess intent or outcome, and quote at most a few words of what \
                 it wrote. Return JSON only: \
                 {\"descriptions\":[{\"change\":\"<the change's id>\",\"text\":\"...\"}]}. Leave \
                 out a change whose facts say too little.",
                MAX_DESCRIPTION_TOKENS,
            ),
            Self::Title { .. } => (
                "Name what the person did in the changes in the user message as one thing, in \
                 four words or fewer: a short title a colleague would understand in a history \
                 list, such as \"Overhauled Q3 financial report\". Use only its facts, and never \
                 a name, number, outcome or intent they do not state. Return JSON only: \
                 {\"title\":\"...\"}.",
                MAX_TITLE_TOKENS,
            ),
        };
        let facts = match self {
            Self::Describe(facts) | Self::Title { facts, .. } => facts,
        };
        ModelRequest {
            messages: vec![Message::text(
                Role::User,
                format!("{instructions}\n\n{}", facts_json(facts)),
            )],
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens,
        }
    }
}

/// The changes a job asks about, as the model reads them.
fn facts_json(facts: &[Fact]) -> String {
    let evidence: Vec<serde_json::Value> = facts
        .iter()
        .map(|fact| {
            serde_json::json!({
                "change": fact.change,
                "summary": truncate(&fact.summary, 180),
                "purpose": fact.purpose.as_deref().map(|text| truncate(text, 120)),
                "item": fact.item.as_deref().map(|text| truncate(text, 120)),
                "wrote": truncate(&fact.wrote, 240),
            })
        })
        .collect();
    serde_json::Value::Array(evidence).to_string()
}

fn truncate(value: &str, limit: usize) -> String {
    let truncated = value.chars().count() > limit;
    let mut value: String = value.chars().take(limit).collect();
    if truncated {
        value.push('…');
    }
    value
}

/// The text of a model's answer, from whichever content part carried it.
#[must_use]
pub fn answer_text(response: &ui::models::ModelResponse) -> String {
    response
        .message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Descriptions the document carries, with what a model wrote standing in place of the change's own
/// summary. One history row per change, read exactly as the pane would read it otherwise.
pub fn shown(entries: &mut [HistoryEntry], doc: &AutoCommit) {
    let described = descriptions::read_descriptions(doc);
    for entry in entries {
        if let Some(text) = described.get(&entry.id) {
            entry.summary.clone_from(text);
        }
    }
}

/// What this device has asked for, and what it wrote.
pub struct Describer {
    /// Whether a model may be asked at all: the host's answer to whether one is configured, here
    /// and now. A page or a session without one asks nothing and keeps the deterministic digest.
    enabled: bool,
    /// Job keys already sent, so a failure is not asked again.
    asked: HashSet<String>,
    /// Changes already asked about, in flight or answered.
    describing: HashSet<HistoryEntryId>,
    /// Runs this device already tried to name.
    titled: HashSet<HistoryEntryId>,
}

impl Default for Describer {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Describer {
    /// A describer that asks `enabled` 's model, or, when nothing is configured, none at all.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            asked: HashSet::new(),
            describing: HashSet::new(),
            titled: HashSet::new(),
        }
    }

    /// Whether a model is configured now: the person may configure one while a session runs, and
    /// one may go away.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// What the document asks for that nothing has asked yet: one batch of descriptions, else one
    /// name. `None` when every change is described and every run of this device's own changes is
    /// named, or when there is no model to ask.
    /// Only this device's own changes are named: a title says what the person here did, and another
    /// device names its own work.
    pub fn next(&mut self, doc: &mut AutoCommit) -> Option<Job> {
        if !self.enabled {
            return None;
        }
        let described = descriptions::read_descriptions(doc);
        let entries = ui::HistorySource::new().entries(doc);
        if entries.is_empty() {
            return None;
        }
        if let Some(job) = self.next_descriptions(&entries, &described, doc) {
            return Some(job);
        }
        let named = groups::read_named_ranges(doc, &ui::AuthorDirectory::new());
        self.next_title(doc, &entries, &named)
    }

    /// The newest changes nobody described, up to [`BATCH`] of them.
    fn next_descriptions(
        &mut self,
        entries: &[HistoryEntry],
        described: &HashMap<HistoryEntryId, String>,
        doc: &mut AutoCommit,
    ) -> Option<Job> {
        let mut facts: Vec<(HistoryEntryId, Fact)> = entries
            .iter()
            .rev()
            .filter(|entry| {
                !entry.is_genesis()
                    && entry.redaction.is_none()
                    && !entry.is_effect_receipt()
                    && !described.contains_key(&entry.id)
                    && !self.describing.contains(&entry.id)
            })
            .take(BATCH)
            .map(|entry| (entry.id, fact_of(entry)))
            .collect();
        if facts.is_empty() {
            return None;
        }
        let ids: Vec<HistoryEntryId> = facts.iter().map(|(id, _)| *id).collect();
        let mut wrote = descriptions::change_evidence(doc, &ids);
        for (id, fact) in &mut facts {
            fact.wrote = wrote.remove(id).unwrap_or_default();
        }
        self.describing.extend(ids);
        let job = Job::Describe(facts.into_iter().map(|(_, fact)| fact).collect());
        self.asked.insert(job.key());
        Some(job)
    }

    /// The longest run of this device's own unnamed changes worth naming: the newest one, so what
    /// the person just did is what gets a name.
    fn next_title(
        &mut self,
        doc: &mut AutoCommit,
        entries: &[HistoryEntry],
        named: &[NamedRange],
    ) -> Option<Job> {
        let covered = covered_entries(entries, named);
        let actors = actors_of(doc);
        let mine = doc.get_actor().to_hex_string();
        let run = group_entries(entries, named, &GroupingConfig::default())
            .into_iter()
            .rev()
            .find(|group| {
                group.end.saturating_sub(group.start) >= MIN_RUN
                    && group.children.is_empty()
                    && entries.get(group.start..group.end).is_some_and(|run| {
                        run.iter().all(|entry| {
                            !covered.contains(&entry.id)
                                && entry.redaction.is_none()
                                && actors.get(&entry.id).is_some_and(|actor| *actor == mine)
                        })
                    })
                    && entries
                        .get(group.start)
                        .is_some_and(|first| !self.titled.contains(&first.id))
            })?;
        let run = entries.get(run.start..run.end)?;
        let start = run.first()?.id;
        let end = run.last()?.id;
        self.titled.insert(start);
        let job = Job::Title {
            start,
            end,
            facts: run.iter().map(fact_of).collect(),
        };
        self.asked.insert(job.key());
        Some(job)
    }

    /// Write what a model answered for `job` . `false` when the answer wrote nothing — a refused or
    /// unreadable answer never retries forever.
    pub fn apply(&mut self, doc: &mut AutoCommit, job: &Job, answer: &str) -> bool {
        match job {
            Job::Describe(facts) => {
                let mut wrote = false;
                for (id, text) in parse_descriptions(answer, facts) {
                    wrote |= descriptions::write_description(doc, &id, &text, GENERATED_BY);
                    self.describing.insert(id);
                }
                wrote
            }
            Job::Title { start, end, .. } => {
                self.titled.insert(*start);
                let Some(title) = parse_title(answer) else {
                    return false;
                };
                // A person's name over this range always wins; a range nobody named, or that this
                // describer named, is the model's to name.
                let key = group_key(start);
                let taken = doc.get(automerge::ROOT, &key).ok().flatten().is_some()
                    && !groups::name_is_generated(doc, &key);
                if taken {
                    return false;
                }
                groups::name_range_by(doc, start, Some(end), &title, GENERATED_BY);
                true
            }
        }
    }

    /// Forget `job` : no model could be asked, or the answer never came. Nothing is retried for it
    /// and no description is invented instead.
    pub fn failed(&mut self, job: &Job) {
        self.asked.insert(job.key());
        match job {
            Job::Describe(facts) => {
                for fact in facts {
                    if let Some(id) = id_of(&fact.change) {
                        self.describing.insert(id);
                    }
                }
            }
            Job::Title { start, .. } => {
                self.titled.insert(*start);
            }
        }
    }
}

/// The changes a named range covers, so a run that is already part of one is never named again as a
/// whole.
fn covered_entries(entries: &[HistoryEntry], named: &[NamedRange]) -> HashSet<HistoryEntryId> {
    let index: HashMap<HistoryEntryId, usize> = entries
        .iter()
        .enumerate()
        .map(|(position, entry)| (entry.id, position))
        .collect();
    let mut covered = HashSet::new();
    for range in named {
        let Some(start) = index.get(&range.start).copied() else {
            continue;
        };
        let end = range
            .end
            .and_then(|end| index.get(&end).copied())
            .map_or(entries.len(), |end| end.saturating_add(1));
        for entry in entries.get(start..end).unwrap_or_default() {
            covered.insert(entry.id);
        }
    }
    covered
}

fn group_key(start: &HistoryEntryId) -> String {
    format!("{}{}", ui::history_source::GROUPS_KEY, hex16(start))
}

/// Every change's actor, so a run can be told as this device's own.
fn actors_of(doc: &mut AutoCommit) -> HashMap<HistoryEntryId, String> {
    doc.get_changes(&[])
        .into_iter()
        .map(|change| {
            (
                ui::history_source::entry_id_from_hash(&change.hash().to_string()),
                change.actor_id().to_hex_string(),
            )
        })
        .collect()
}

fn fact_of(entry: &HistoryEntry) -> Fact {
    let context = entry.context.as_ref();
    let item = context.and_then(|context| {
        let item = context.item.as_ref().map(|item| item.label.as_str());
        let resource = context.resource.as_ref().map(|r| r.label.as_str());
        item.or(resource)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
    });
    Fact {
        change: hex16(&entry.id),
        summary: entry.summary.clone(),
        purpose: context.and_then(|context| context.purpose.clone()),
        item,
        wrote: String::new(),
    }
}

/// The id a hex change name stands for, when it is one this code wrote.
fn id_of(change: &str) -> Option<HistoryEntryId> {
    from_hex16(change)
}

/// One line per asked change, from a JSON answer: only the changes asked about, each once, each a
/// sentence History accepts.
fn parse_descriptions(answer: &str, asked: &[Fact]) -> Vec<(HistoryEntryId, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(answer.trim()) else {
        return Vec::new();
    };
    let Some(items) = value
        .get("descriptions")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut described: Vec<(HistoryEntryId, String)> = Vec::new();
    for item in items {
        let (Some(change), Some(text)) = (
            item.get("change").and_then(serde_json::Value::as_str),
            item.get("text").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let text = text.trim();
        let Some(id) = id_of(change) else {
            continue;
        };
        if asked.iter().any(|fact| fact.change == change)
            && !described.iter().any(|(done, _)| *done == id)
            && descriptions::is_acceptable(text)
        {
            described.push((id, text.to_owned()));
        }
    }
    described
}

/// A name, from a JSON answer: one line, within the length a history band has room for.
fn parse_title(answer: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(answer.trim()).ok()?;
    let title = value.get("title")?.as_str()?.trim().trim_end_matches('.');
    let acceptable = !title.is_empty()
        && title.chars().count() <= MAX_LABEL_LEN
        && !title.chars().any(char::is_control);
    acceptable.then(|| title.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable as _};
    use automerge::{ObjType, ROOT};

    /// One sitting, one second per line: inside the grouping's idle gap.
    fn commit_at(doc: &mut AutoCommit, second: i64) {
        doc.commit_with(CommitOptions::default().with_time(second));
    }

    /// The second `offset` seconds into the sitting.
    const fn at(offset: i64) -> i64 {
        1_760_000_000_i64.saturating_add(offset)
    }

    /// A document of `changes` typing commits by this device, all in one sitting, as the Q3 report
    /// is filled in.
    fn typed(changes: usize) -> AutoCommit {
        let mut doc = AutoCommit::new();
        let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        commit_at(&mut doc, at(0));
        for line in 0..changes {
            let end = doc.length(&body);
            doc.splice_text(&body, end, 0, &format!("Line {line} of the report. "))
                .unwrap();
            commit_at(&mut doc, at(i64::try_from(line).unwrap_or(0)));
        }
        doc
    }

    /// What a working model answers a job, in the shape each job asks for.
    fn answer(job: &Job) -> String {
        match job {
            Job::Describe(facts) => serde_json::json!({
                "descriptions": facts
                    .iter()
                    .map(|fact| serde_json::json!({
                        "change": fact.change,
                        "text": format!("Wrote {}", fact.change),
                    }))
                    .collect::<Vec<_>>(),
            })
            .to_string(),
            Job::Title { .. } => serde_json::json!({
                "title": "Overhauled Q3 financial report",
            })
            .to_string(),
        }
    }

    /// Run the describer until it has nothing left to ask, answering every job as a model would.
    /// The jobs it asked, in order.
    fn run(describer: &mut Describer, doc: &mut AutoCommit) -> Vec<Job> {
        let mut asked = Vec::new();
        while let Some(job) = describer.next(doc) {
            assert!(
                asked.len() < 64,
                "the describer asks a bounded number of times"
            );
            describer.apply(doc, &job, &answer(&job));
            doc.commit();
            asked.push(job);
        }
        asked
    }

    #[test]
    fn an_edit_sessions_changes_become_one_titled_group() {
        let mut doc = typed(20);
        let mut describer = Describer::new(true);
        let asked = run(&mut describer, &mut doc);
        assert!(
            matches!(asked.last(), Some(Job::Title { .. })),
            "the run is named: {asked:?}"
        );
        assert_eq!(
            asked
                .iter()
                .filter(|job| matches!(job, Job::Title { .. }))
                .count(),
            1,
            "one run of one sitting is one title"
        );

        let named = groups::read_named_ranges(&doc, &ui::AuthorDirectory::new());
        assert_eq!(named.len(), 1, "one name, over the whole sitting");
        assert_eq!(named[0].label, "Overhauled Q3 financial report");
        assert!(
            named[0].named_by.is_none(),
            "a generated name is nobody's words"
        );

        let entries = ui::HistorySource::new().entries(&mut doc);
        let groups = group_entries(&entries, &named, &GroupingConfig::default());
        assert_eq!(groups.len(), 1, "the sitting reads as one band");
        assert_eq!(
            groups[0].name.as_deref(),
            Some("Overhauled Q3 financial report")
        );
        assert!(
            groups[0].len() >= 20,
            "the band holds the whole sitting: {}",
            groups[0].len()
        );
        assert!(
            groups[0]
                .subtitle_facts(&entries)
                .iter()
                .all(|fact| !fact.contains("named by")),
            "nobody is credited with the model's words"
        );
        let words = descriptions::read_descriptions(&doc);
        assert_eq!(words.len(), entries.len(), "every change is described");
    }

    #[test]
    fn a_persons_rename_survives_regeneration() {
        let mut doc = typed(20);
        let mut describer = Describer::new(true);
        run(&mut describer, &mut doc);
        let generated = groups::read_named_ranges(&doc, &ui::AuthorDirectory::new());
        let range = generated[0].clone();
        let person = doc.get_actor().to_hex_string();

        // The person says what the run was, in their own words.
        groups::name_range_by(
            &mut doc,
            &range.start,
            range.end.as_ref(),
            "Q3 report",
            &person,
        );
        doc.commit();
        let named = groups::read_named_ranges(&doc, &ui::AuthorDirectory::new());
        assert_eq!(named[0].label, "Q3 report");

        // A model naming the same range again is refused, by the answer that arrives after the
        // person's.
        let mut later = Describer::new(true);
        let job = Job::Title {
            start: range.start,
            end: range.end.unwrap(),
            facts: Vec::new(),
        };
        assert!(
            !later.apply(&mut doc, &job, &answer(&job)),
            "a person's name always wins"
        );
        doc.commit();
        assert_eq!(
            groups::read_named_ranges(&doc, &ui::AuthorDirectory::new())[0].label,
            "Q3 report"
        );

        // And a later sitting is named without touching theirs.
        let Some((_, body)) = doc.get(ROOT, "body").unwrap() else {
            panic!("the report has a body");
        };
        for line in 0..20_i64 {
            let length = doc.length(&body);
            doc.splice_text(&body, length, 0, &format!("More line {line}. "))
                .unwrap();
            commit_at(&mut doc, at(3_600_i64.saturating_add(line)));
        }
        let mut after = Describer::new(true);
        run(&mut after, &mut doc);
        let after = groups::read_named_ranges(&doc, &ui::AuthorDirectory::new());
        assert!(
            after.iter().any(|range| range.label == "Q3 report"),
            "the person's name is still there: {after:?}"
        );
    }

    #[test]
    fn without_a_model_nothing_is_asked_and_the_digest_stands() {
        let mut doc = typed(20);
        let mut describer = Describer::new(false);
        assert!(
            describer.next(&mut doc).is_none(),
            "no provider, no request"
        );
        assert!(descriptions::read_descriptions(&doc).is_empty());

        let entries = ui::HistorySource::new().entries(&mut doc);
        let digest = ui::history_source::ActivityDigest::from_entries(
            &entries,
            ui::time::UNIX_EPOCH,
            entries.len(),
        );
        assert!(!digest.briefs.is_empty(), "the digest stands in its place");
        assert!(
            digest.briefs.iter().all(|brief| !brief.title.is_empty()),
            "and it says what happened in its own words"
        );
    }
}
