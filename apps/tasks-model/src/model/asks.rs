//! What the people of a workspace ask its agents on the tracker: the issues they gave an agent.
//!
//! An agent works inside a turn a person caused ([`tilo_action_policy::turn`]). On the tracker the
//! cause is a person giving an issue to the agent (its `assign` action), a person writing in the
//! thread of an issue the agent has, the issue being told to go on (`resume`), or its wait running
//! out; the issue's thread is the conversation, and the agent's answer is its own message there
//! after the ask. Nothing is kept beside the issue: whatever works as the agent reads the same asks
//! from it, also after a restart.
//!
//! An issue that waits asks nothing until the wait ends, and one whose work stopped asks nothing
//! until somebody says something new: what was asked before the wait began or before the stop is
//! not asked a second time.

use std::collections::{BTreeMap, BTreeSet};

use tilo_resource::{Id, MemberId};

use super::{Issue, MessageKind, Status, WaitKind, Work};

/// What asked an agent to work on an issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asked {
    /// A person gave it the issue.
    Assigned,
    /// A person wrote in the issue's thread.
    Message,
    /// The issue was told to go on: [`IssueAsk::words`] says by what.
    Resumed,
    /// The latest moment its wait ends has come.
    WaitEnded,
    /// Every piece it waited for is through: [`IssueAsk::words`] says how they ended.
    PiecesEnded,
}

/// A person's ask of the agent an issue is assigned to that the agent has not answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueAsk {
    /// The issue.
    pub issue: Id,
    /// The agent asked, by its bare id.
    pub agent: String,
    /// The person who asked: the agent works for them.
    pub person: MemberId,
    /// What asked: the id of the person's message in the thread, `assigned-<ms>` for the
    /// assignment itself, `resumed-<ms>` for an issue told to go on, or `waited-<ms>` for a wait
    /// that ran out.
    pub cause: String,
    /// The person's words in the thread, when a message asked; what let it go on, when it was
    /// resumed; empty otherwise.
    pub words: String,
    /// What kind of ask it is.
    pub asked: Asked,
}

/// Every open issue of `issues` whose agent has a person's ask it has not answered: the newest
/// ask on each. No wait has ended: an issue that waits asks only once it is told to go on or a
/// person writes in it ([`agent_asks_at`] reads the clock too).
#[must_use]
pub fn agent_asks<'a>(issues: impl IntoIterator<Item = (&'a Id, &'a Issue)>) -> Vec<IssueAsk> {
    agent_asks_at(issues, i64::MIN)
}

/// [`agent_asks`] at `now_ms`: an issue whose wait was to end by then asks its agent again, and
/// so does one that waits for its pieces once every one of them is through. Of the pieces of an
/// issue that bounds how many are in work at once, only so many are begun: the others ask
/// when those in work are through, in the tracker's order.
#[must_use]
pub fn agent_asks_at<'a>(
    issues: impl IntoIterator<Item = (&'a Id, &'a Issue)>,
    now_ms: i64,
) -> Vec<IssueAsk> {
    let issues: Vec<(&Id, &Issue)> = issues.into_iter().collect();
    let pieces = Pieces::of(&issues);
    let asks = issues
        .iter()
        .filter_map(|(id, issue)| ask_on(id, issue, now_ms, &pieces))
        .collect();
    pieces.in_turn(asks)
}

/// Whether work on a piece is over for now: it is closed, ready for review, or stopped.
fn through(piece: &Issue) -> bool {
    piece.status.closed()
        || piece.status == Status::InReview
        || piece
            .work
            .as_ref()
            .is_some_and(|work| work.stopped.is_some())
}

/// Whether work on a piece began and is not through.
fn in_work(piece: &Issue) -> bool {
    !through(piece) && piece.work.as_ref().is_some_and(|work| work.sessions > 0)
}

/// The pieces of every issue, by the issue's id: the issues under it, in the tracker's order,
/// and how many of them the issue lets be in work at once.
struct Pieces<'a> {
    under: BTreeMap<&'a str, Vec<(&'a Id, &'a Issue)>>,
    at_once: BTreeMap<&'a str, u64>,
}

impl<'a> Pieces<'a> {
    fn of(issues: &[(&'a Id, &'a Issue)]) -> Self {
        let mut under: BTreeMap<&str, Vec<(&Id, &Issue)>> = BTreeMap::new();
        let mut at_once = BTreeMap::new();
        for &(id, issue) in issues {
            if let Some(parent) = &issue.parent {
                under.entry(parent.as_str()).or_default().push((id, issue));
            }
            if let Some(bound) = issue.work.as_ref().and_then(|work| work.pieces_at_once) {
                at_once.insert(id.as_str(), bound);
            }
        }
        for pieces in under.values_mut() {
            pieces.sort_by(|(a_id, a), (b_id, b)| {
                (&a.rank, a.number, a_id.as_str()).cmp(&(&b.rank, b.number, b_id.as_str()))
            });
        }
        Self { under, at_once }
    }

    /// How the pieces under `issue` ended, in words, once every one of them is through; none
    /// while one is still to be worked on.
    fn ended(&self, issue: &Id) -> Option<String> {
        let pieces = self
            .under
            .get(issue.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !pieces.iter().all(|(_, piece)| through(piece)) {
            return None;
        }
        let count =
            |fits: fn(&Issue) -> bool| pieces.iter().filter(|(_, piece)| fits(piece)).count();
        let cancelled = count(|piece| piece.status == Status::Cancelled);
        let done = count(|piece| piece.status == Status::Done);
        let ready = count(|piece| piece.status == Status::InReview);
        let stopped = count(|piece| !piece.status.closed() && piece.status != Status::InReview);
        Some(format!(
            "{} pieces: {ready} ready for review, {done} done, {stopped} stopped, {cancelled} \
             cancelled",
            pieces.len()
        ))
    }

    /// What asks an issue that waits for its pieces, once every one of them is through: the
    /// ask's own id, and how they ended. None without a clock to read.
    fn asks(&self, issue: &Id, work: Option<&Work>, now_ms: i64) -> Option<(String, String)> {
        let work = work.filter(|work| work.waiting() && now_ms != i64::MIN)?;
        if !work.waits.iter().any(|wait| wait.kind == WaitKind::Pieces) {
            return None;
        }
        let ended = self.ended(issue)?;
        Some((
            format!("pieces-{}", work.since_ms.unwrap_or_default()),
            ended,
        ))
    }

    /// `asks` without those that would begin a piece beyond what its issue lets be in work at
    /// once: a piece that began keeps its asks, and the others are begun in the tracker's order
    /// as places come free.
    fn in_turn(&self, asks: Vec<IssueAsk>) -> Vec<IssueAsk> {
        let mut held: BTreeSet<&str> = BTreeSet::new();
        for (issue, bound) in &self.at_once {
            let Some(pieces) = self.under.get(issue) else {
                continue;
            };
            let going = pieces.iter().filter(|(_, piece)| in_work(piece)).count();
            let mut places = usize::try_from(*bound)
                .unwrap_or(usize::MAX)
                .saturating_sub(going);
            for (id, piece) in pieces {
                if in_work(piece) || through(piece) || !asks.iter().any(|ask| &ask.issue == *id) {
                    continue;
                }
                if places == 0 {
                    held.insert(id.as_str());
                } else {
                    places = places.saturating_sub(1);
                }
            }
        }
        asks.into_iter()
            .filter(|ask| !held.contains(ask.issue.as_str()))
            .collect()
    }
}

fn is_person(member: Option<&MemberId>) -> bool {
    member.is_some_and(MemberId::is_person)
}

/// The ask on `issue` its agent has not answered at `now_ms`, if any.
fn ask_on(id: &Id, issue: &Issue, now_ms: i64, pieces: &Pieces<'_>) -> Option<IssueAsk> {
    if issue.status.closed() {
        return None;
    }
    let agent = issue.assigned_agent()?.strip_prefix("agent:")?.to_owned();
    let by_agent = MemberId::agent(agent.clone());
    // Whoever gave it the issue: the person its agent works on it for.
    let given = issue
        .assigned_by
        .clone()
        .filter(|by| is_person(Some(by)) && issue.assigned_ms > 0)?;
    // The newest ask: the assignment, or what came after it.
    let mut ask = (
        issue.assigned_ms,
        given.clone(),
        format!("assigned-{}", issue.assigned_ms),
        String::new(),
        Asked::Assigned,
    );
    for (message_id, message) in issue.comments.iter() {
        let Some(author) = message
            .author
            .clone()
            .filter(|author| is_person(Some(author)))
        else {
            continue;
        };
        if message.kind != MessageKind::Comment {
            continue;
        }
        // Only what was said once the agent had the issue asks it.
        if message.at_ms >= ask.0 {
            ask = (
                message.at_ms,
                author,
                message_id.to_string(),
                message.text.clone(),
                Asked::Message,
            );
        }
    }
    let work = issue.work.as_ref();
    if let Some(resumed) = work.and_then(|work| work.resumed.as_ref())
        && resumed.at_ms >= ask.0
    {
        ask = (
            resumed.at_ms,
            given.clone(),
            format!("resumed-{}", resumed.at_ms),
            resumed.cause.clone(),
            Asked::Resumed,
        );
    }
    if let Some(until_ms) = work.and_then(|work| work.until_ms)
        && until_ms <= now_ms
        && until_ms >= ask.0
    {
        ask = (
            until_ms,
            given.clone(),
            format!("waited-{until_ms}"),
            String::new(),
            Asked::WaitEnded,
        );
    }
    // An issue that waits for its pieces is asked again once every one of them is through.
    if let Some((cause, ended)) = pieces.asks(id, work, now_ms) {
        ask = (now_ms, given, cause, ended, Asked::PiecesEnded);
    }
    let (since, person, cause, words, asked) = ask;
    if let Some(work) = work {
        // While it waits, only what came since the wait began asks it.
        if let (Some(until_ms), Some(began_ms)) = (work.until_ms, work.since_ms)
            && until_ms > now_ms
            && since < began_ms
        {
            return None;
        }
        // Once its work stopped, only what came since asks it.
        if work
            .stopped
            .as_ref()
            .is_some_and(|stopped| since < stopped.at_ms)
        {
            return None;
        }
    }
    let answered = issue
        .comments
        .iter()
        .any(|(_, message)| message.author.as_ref() == Some(&by_agent) && message.at_ms >= since);
    (!answered).then(|| IssueAsk {
        issue: id.clone(),
        agent,
        person,
        cause,
        words,
        asked,
    })
}

/// The open issues of `issues` that wait for `person`'s automation `automation`: each is with an
/// agent, was given to it by `person`, waits, and names the automation in one of the things it
/// waits for ([`WaitFor::event`](super::WaitFor)). That automation going on tells them to go on
/// (`resume`), and tells no other: an automation of one person never ends the wait of an issue
/// given for another, whatever id that wait names.
#[must_use]
pub fn waiting_for<'a>(
    issues: impl IntoIterator<Item = (&'a Id, &'a Issue)>,
    person: &str,
    automation: &str,
) -> Vec<Id> {
    issues
        .into_iter()
        .filter(|(_, issue)| issue.waits_for_automation(person, automation))
        .map(|(id, _)| id.clone())
        .collect()
}

impl Issue {
    /// Whether this issue currently waits for this automation of the person who gave it to its
    /// agent. Check this on the current issue before an automation tells it to go on.
    #[must_use]
    pub fn waits_for_automation(&self, person: &str, automation: &str) -> bool {
        !automation.is_empty()
            && !self.status.closed()
            && self.assigned_agent().is_some()
            && self.assigned_by.as_ref().and_then(MemberId::person_id) == Some(person)
            && self.work.as_ref().is_some_and(|work| {
                work.waiting()
                    && work
                        .waits
                        .iter()
                        .any(|wait| wait.event.as_deref() == Some(automation))
            })
    }
}
