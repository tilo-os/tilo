//! The triggers on something new in a source
//! ([`TriggerSpec::SourceChanged`]): each thing found there that was not
//! there before is fired once ([`tilo_automation::source_change`]).
//!
//! The source read here is the mail of the person the program is carried
//! out for, as the holder of their accounts hears it arrive
//! ([`Workspaces::listen_for_mail`]): their inboxes are watched for as long
//! as the program waits on them, and what arrives is read from their own
//! log, never anybody else's. Only what arrived in an account that person
//! named their assistant for is handed over, since what a message says is
//! handed to that assistant. What the log held when the program was given
//! its person is where it starts.
//!
//! A source outside, a feed or a page on the public web, tells nobody of
//! what is new, so it is looked at on a clock, with no model
//! ([`source_change::looked_at`]). The look is the fetch an agent's own read
//! of a page goes through ([`tilo_web_search::fetch`]) and nothing beside
//! it: the same refusal of this server's own network and of every address
//! that is not public, at the lookup the connection uses, the same wait and
//! the same redirects, and less to read. What it read is compared with the
//! last look by [`tilo_automation::source_change`], which also keeps the
//! bounds: how often, how much is handed over, how much longer a source
//! that fails is left alone, and when it counts as gone. What is new goes
//! with the firing as data. An address is never written to the log: it may
//! carry a key of its person's.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Armed, Event, Firer, Firing, SourceEvent};
use tilo_automation::source_change::{self, Failure, Found, Seen};
use tilo_web_search::{FetchError, FetchLimits};

use super::{BotRunner, Ended};
use crate::workspaces::MailListening;

/// The most one look downloads: a fifth of what an agent's read of a page
/// may.
const LOOK_BYTES: usize = 1024 * 1024;
/// The longest a key that names a thing found outside may be, in characters.
const MAX_KEY_CHARS: usize = 512;

/// Whether `trigger` waits on something new in a source: nothing a member
/// writes in the workspace wakes it.
pub(super) const fn waits_on_a_source(trigger: &TriggerSpec) -> bool {
    matches!(trigger, TriggerSpec::SourceChanged { .. })
}

/// Whether `trigger` waits on the mail its person's accounts receive.
fn waits_on_mail(trigger: &TriggerSpec) -> bool {
    matches!(trigger, TriggerSpec::SourceChanged { kind, .. } if kind == source_change::PERSON_MAIL)
}

/// The mail of one person, heard for as long as this is held.
pub(super) struct Mail {
    /// Keeps their inboxes watched.
    _listening: MailListening,
    /// Moves whenever anybody's log of arrivals grew.
    grew: tokio::sync::watch::Receiver<u64>,
    /// Whether their log is looked at whether or not one grew: at first,
    /// and after a look that could not be finished.
    again: bool,
}

impl Mail {
    /// Whether the log may hold something not read yet. Asked before the
    /// log is read, so what arrives while it is read is found by the next
    /// look.
    fn may_hold_more(&mut self) -> bool {
        let grew = self.grew.has_changed().unwrap_or(true);
        if grew {
            self.grew.mark_unchanged();
        }
        std::mem::take(&mut self.again) || grew
    }
}

/// Honest operational status of a source monitor at a given moment.
///
/// Explicitly separates verified "no change since T" from "last successful
/// sync at T; currently stale/unreachable" or backlogged coverage so callers
/// never mistake a broken or stalled monitor for an unchanged source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MonitorStatus {
    /// No successful read has completed yet and no failure has occurred.
    NeverRead,
    /// Reads are succeeding on cadence with zero backlog and no gap in coverage.
    NoChangeSince {
        since_ms: i64,
        last_successful_read_ms: i64,
        covered_through_ms: i64,
    },
    /// The most recent read succeeded, but unconsumed overflow items remain.
    Backlogged {
        last_successful_read_ms: i64,
        covered_through_ms: Option<i64>,
        backlog_count: usize,
    },
    /// No explicit read error occurred, but the monitor has fallen behind its
    /// expected polling cadence.
    Stale {
        last_successful_read_ms: i64,
        covered_through_ms: Option<i64>,
        overdue_by_ms: i64,
    },
    /// One or more consecutive reads failed; coverage stopped advancing at the
    /// last successful sync.
    Unreachable {
        last_successful_read_ms: Option<i64>,
        covered_through_ms: Option<i64>,
        consecutive_failures: u32,
        last_error: String,
    },
}

impl MonitorStatus {
    /// Whether this status truthfully asserts that the source has not changed
    /// since the reported timestamp.
    #[must_use]
    pub const fn claims_no_change(&self) -> bool {
        matches!(self, Self::NoChangeSince { .. })
    }

    /// Whether coverage is degraded (unreachable, stale, or backlogged).
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(
            self,
            Self::Unreachable { .. } | Self::Stale { .. } | Self::Backlogged { .. }
        )
    }

    /// Concise human-readable status distinguishing verified no-change from
    /// stale, backlogged, or unreachable sync state.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::NeverRead => "never read yet".to_owned(),
            Self::NoChangeSince {
                since_ms,
                covered_through_ms,
                ..
            } => format!("no change since {since_ms}; verified through {covered_through_ms}"),
            Self::Backlogged {
                last_successful_read_ms,
                backlog_count,
                ..
            } => format!(
                "last successful sync at {last_successful_read_ms}; {backlog_count} item(s) in backlog"
            ),
            Self::Stale {
                last_successful_read_ms,
                overdue_by_ms,
                ..
            } => format!(
                "last successful sync at {last_successful_read_ms}; currently stale (overdue by {overdue_by_ms}ms)"
            ),
            Self::Unreachable {
                last_successful_read_ms: Some(last_ok),
                consecutive_failures,
                last_error,
                ..
            } => format!(
                "last successful sync at {last_ok}; currently unreachable ({consecutive_failures} consecutive failures: {last_error})"
            ),
            Self::Unreachable {
                last_successful_read_ms: None,
                consecutive_failures,
                last_error,
                ..
            } => format!(
                "never reached; currently unreachable ({consecutive_failures} consecutive failures: {last_error})"
            ),
        }
    }
}

/// Per-trigger monitor coverage and health tracking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorHealth {
    /// Timestamp (ms since epoch) of the most recent successful read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_read_ms: Option<i64>,
    /// Timestamp (ms since epoch) through which coverage is verified without
    /// gaps. Does not advance when reads fail or leave unconsumed backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_through_ms: Option<i64>,
    /// Number of consecutive failed read attempts since the last success.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Most recent error description while failing; cleared on recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Number of unconsumed or overflow items remaining in backlog.
    #[serde(default)]
    pub backlog_count: usize,
    /// Expected polling cadence in milliseconds for staleness detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_interval_ms: Option<i64>,
    /// Timestamp (ms since epoch) of the most recent observed change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_change_ms: Option<i64>,
}

impl MonitorHealth {
    /// Current backlog item count.
    #[must_use]
    pub const fn backlog(&self) -> usize {
        self.backlog_count
    }

    /// Update health after a successful read at `now_ms`.
    pub fn note_success(
        &mut self,
        now_ms: i64,
        changed: bool,
        backlog_count: usize,
        expected_interval_ms: Option<i64>,
    ) {
        self.last_successful_read_ms = Some(now_ms);
        self.consecutive_failures = 0;
        self.last_error = None;
        self.backlog_count = backlog_count;
        if expected_interval_ms.is_some() {
            self.expected_interval_ms = expected_interval_ms;
        }
        if changed {
            self.last_change_ms = Some(now_ms);
        }
        if backlog_count == 0 {
            self.covered_through_ms = Some(now_ms);
        }
    }

    /// Update health after a failed read, preserving the last successful sync
    /// and verified coverage timestamps.
    pub fn note_failure(&mut self, error: impl Into<String>, expected_interval_ms: Option<i64>) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_error = Some(error.into());
        if expected_interval_ms.is_some() {
            self.expected_interval_ms = expected_interval_ms;
        }
    }

    /// Whether the monitor is past twice its expected polling interval at
    /// `now_ms` without a fresh read.
    #[must_use]
    pub fn is_stale_at(&self, now_ms: i64) -> bool {
        let (Some(last_ok), Some(interval)) =
            (self.last_successful_read_ms, self.expected_interval_ms)
        else {
            return false;
        };
        interval > 0 && now_ms.saturating_sub(last_ok) > interval.saturating_mul(2)
    }

    /// Compute the honest monitor status at `now_ms`.
    #[must_use]
    pub fn honest_status(&self, now_ms: i64) -> MonitorStatus {
        if self.consecutive_failures > 0 {
            return MonitorStatus::Unreachable {
                last_successful_read_ms: self.last_successful_read_ms,
                covered_through_ms: self.covered_through_ms,
                consecutive_failures: self.consecutive_failures,
                last_error: self.last_error.clone().unwrap_or_default(),
            };
        }
        let Some(last_ok) = self.last_successful_read_ms else {
            return MonitorStatus::NeverRead;
        };
        if self.is_stale_at(now_ms) {
            let interval = self.expected_interval_ms.unwrap_or(0);
            let overdue_by_ms = now_ms.saturating_sub(last_ok).saturating_sub(interval);
            return MonitorStatus::Stale {
                last_successful_read_ms: last_ok,
                covered_through_ms: self.covered_through_ms,
                overdue_by_ms,
            };
        }
        if self.backlog_count > 0 {
            return MonitorStatus::Backlogged {
                last_successful_read_ms: last_ok,
                covered_through_ms: self.covered_through_ms,
                backlog_count: self.backlog_count,
            };
        }
        let covered_through_ms = self.covered_through_ms.unwrap_or(last_ok);
        let since_ms = self.last_change_ms.unwrap_or(last_ok);
        MonitorStatus::NoChangeSince {
            since_ms,
            last_successful_read_ms: last_ok,
            covered_through_ms,
        }
    }
}

/// What looks at the sources outside for one program: the client every
/// fetch goes out through, made at the first look, together with per-trigger
/// monitor coverage health.
#[derive(Default)]
pub(super) struct Outside {
    client: Option<tilo_net::Client>,
    /// Per-trigger monitor coverage health, keyed by trigger index.
    health: BTreeMap<usize, MonitorHealth>,
    /// Whether a look reaches a server on this machine: a test's own.
    #[cfg(test)]
    fixtures: bool,
}

/// What a look at a source outside read.
enum Read {
    /// A feed's entries, in the feed's own order.
    Entries(Vec<Found>),
    /// A page's text, and what the page is called.
    Text { title: Option<String>, text: String },
}

/// How a fetch that failed is taken: an address the fetch refuses is never
/// looked at again, a server in trouble may answer the next time, and
/// anything else says that what was asked for is not there.
fn failure_of(error: &FetchError) -> Failure {
    match error {
        FetchError::Refused(_) | FetchError::InvalidUrl(_) => Failure::Refused,
        error if error.retryable() => Failure::Passing,
        _ => Failure::Gone,
    }
}

/// `text` without the markup a feed may carry in a title or a summary.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' if inside => {
                inside = false;
                out.push(' ');
            }
            ch if !inside => out.push(ch),
            _ => {}
        }
    }
    tilo_web_search::html::decode_entities(&out)
}

/// The entries of the feed `text` is, read at `at`, each named by the id
/// the feed gives it and carrying what a person reads of it.
fn entries(at: &str, text: &str, now_ms: i64) -> Result<Vec<Found>, String> {
    let feed = feed_rs::parser::parse(text.as_bytes())
        .map_err(|error| format!("what is there does not read as a feed: {error}"))?;
    let fact = |text: &str| {
        json!(source_change::cut(
            &plain(text),
            source_change::MAX_FACT_CHARS
        ))
    };
    Ok(feed
        .entries
        .into_iter()
        .take(source_change::MAX_KEYS)
        .map(|entry| {
            let link = entry
                .links
                .iter()
                .find(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
                .or_else(|| entry.links.first())
                .map(|link| link.href.clone())
                .unwrap_or_default();
            let mut facts = Map::new();
            if let Some(title) = &entry.title {
                facts.insert("title".to_owned(), fact(&title.content));
            }
            if !link.is_empty() {
                facts.insert("link".to_owned(), fact(&link));
            }
            if let Some(when) = entry.published.or(entry.updated) {
                facts.insert("published".to_owned(), json!(when.to_rfc3339()));
            }
            if let Some(author) = entry.authors.first() {
                facts.insert("author".to_owned(), fact(&author.name));
            }
            let summary = entry
                .summary
                .map(|summary| summary.content)
                .or_else(|| entry.content.and_then(|content| content.body));
            if let Some(summary) = summary.filter(|summary| !summary.trim().is_empty()) {
                facts.insert("summary".to_owned(), fact(&summary));
            }
            let key = if entry.id.is_empty() { link } else { entry.id };
            Found {
                key: key.chars().take(MAX_KEY_CHARS).collect(),
                at: at.to_owned(),
                at_ms: now_ms,
                facts,
            }
        })
        .collect())
}

impl Outside {
    /// The client every look goes out through, made at the first one. It dials
    /// every address itself: a proxy this server's environment names would
    /// resolve a name behind the lookup the fetch judges.
    fn client(&mut self) -> Result<tilo_net::Client, (String, Failure)> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = tilo_web_search::direct_client()
            .map_err(|error| (error.to_string(), Failure::Passing))?;
        self.client = Some(client.clone());
        Ok(client)
    }

    /// Look once at the source of `kind` at `at`, through the fetch an
    /// agent's own read of a page goes through, with every limit that has
    /// and less to read. Why it could not be read, and how that is taken,
    /// otherwise.
    async fn look(&mut self, kind: &str, at: &str, now_ms: i64) -> Result<Read, (String, Failure)> {
        let client = self.client()?;
        let limits = FetchLimits {
            max_bytes: LOOK_BYTES,
            ..FetchLimits::default()
        };
        #[cfg(test)]
        let fetched = if self.fixtures {
            tilo_web_search::fetch_fixture(&client, at, &limits).await
        } else {
            tilo_web_search::fetch(&client, at, &limits).await
        };
        #[cfg(not(test))]
        let fetched = tilo_web_search::fetch(&client, at, &limits).await;
        let page = fetched.map_err(|error| (error.to_string(), failure_of(&error)))?;
        let Some(text) = page.text() else {
            return Err((
                format!("what is there is not text ({})", page.content_type),
                Failure::Gone,
            ));
        };
        if kind == source_change::WEB_FEED {
            if page.truncated {
                return Err((
                    format!("the feed is larger than {LOOK_BYTES} bytes, the most one look reads"),
                    Failure::Gone,
                ));
            }
            return entries(at, text, now_ms)
                .map(Read::Entries)
                .map_err(|why| (why, Failure::Passing));
        }
        Ok(Read::Text {
            title: page.title().map(str::to_owned),
            text: text.to_owned(),
        })
    }
}

impl BotRunner {
    /// Have the looks at sources outside reach a server on this machine: a
    /// test's own, which the fetch refuses.
    #[cfg(test)]
    pub(crate) fn reach_fixtures(&mut self) {
        self.outside.fixtures = true;
    }

    /// Keep, where whoever lists the program reads how it last ended, that
    /// a source it waits on is gone and why, for as long as it is: its
    /// person is told once, and reads it there afterwards. It is kept once,
    /// and again only after something else was kept in its place.
    pub(super) fn keep_gone(&mut self) {
        let Some(gone) = self.progress.sources.values().find_map(Seen::gone) else {
            self.gone_said = None;
            return;
        };
        if self.gone_said == Some(gone.since_ms) {
            return;
        }
        let since_ms = gone.since_ms;
        let why = format!("its source could not be read: {}", gone.why);
        self.ran(true, Some((Ended::Failed, why)));
        self.gone_said = Some(since_ms);
    }

    /// The firings of the program's triggers on something new in a source
    /// that is looked at on a clock, for each such source whose look is
    /// due: one for each thing the look found that the last one did not,
    /// and one, once, when the source has come to count as gone. What was
    /// read is kept with the program's progress, so a restart looks no
    /// sooner and fires nothing twice. The next look is what the runner's
    /// idle wait ends at.
    async fn looked_firings(&mut self, now_ms: i64) -> Vec<Firing> {
        let shortcut = Arc::clone(&self.shortcut);
        let installation = self.bot.installation.clone();
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &installation,
            },
            shortcut: &shortcut,
        }];
        let mut firings = Vec::new();
        let mut next: Option<i64> = None;
        for (index, trigger) in shortcut.triggers.iter().enumerate() {
            let TriggerSpec::SourceChanged {
                kind,
                at,
                every_secs,
                filter,
            } = trigger
            else {
                continue;
            };
            if !source_change::looked_at(kind) {
                continue;
            }
            let every = source_change::look_secs(*every_secs);
            let mark = tilo_peer_state::trigger_mark(trigger);
            let seen = self
                .progress
                .sources
                .entry(index)
                .or_insert_with(|| Seen::starting(&mark, now_ms));
            seen.follow(&mark, now_ms);
            let mut found = Vec::new();
            if seen.due(now_ms) {
                let read = self.outside.look(kind, at, now_ms).await;
                let Some(seen) = self.progress.sources.get_mut(&index) else {
                    continue;
                };
                let expected_interval_ms = i64::try_from(every.saturating_mul(1000)).ok();
                match read {
                    Ok(Read::Entries(entries)) => {
                        let news = seen.took(entries, every, now_ms);
                        let changed = !news.new.is_empty() || news.passed_over > 0;
                        self.outside.health.entry(index).or_default().note_success(
                            now_ms,
                            changed,
                            news.passed_over,
                            expected_interval_ms,
                        );
                        found.extend(news.new.into_iter().filter(|one| {
                            tilo_automation::resource_time::matches(filter, &one.facts)
                        }));
                    }
                    Ok(Read::Text { title, text }) => {
                        let added = seen.took_text(at, title.as_deref(), &text, every, now_ms);
                        let changed = added.is_some();
                        self.outside.health.entry(index).or_default().note_success(
                            now_ms,
                            changed,
                            0,
                            expected_interval_ms,
                        );
                        found.extend(added);
                    }
                    Err((why, failure)) => {
                        tracing::info!(%installation, %kind, ?failure, "a look at a source outside did not read it");
                        self.outside
                            .health
                            .entry(index)
                            .or_default()
                            .note_failure(why.clone(), expected_interval_ms);
                        found.extend(seen.failed(at, &why, failure, every, now_ms));
                    }
                }
            }
            let at_ms = self
                .progress
                .sources
                .get(&index)
                .and_then(Seen::next_look_ms);
            if let Some(at_ms) = at_ms {
                next = Some(next.map_or(at_ms, |kept| kept.min(at_ms)));
            }
            for one in found {
                let event = SourceEvent {
                    workspace: self.workspace.clone(),
                    trigger: index,
                    key: one.key.clone(),
                    payload: source_change::told(kind, &one),
                };
                firings.extend(
                    tilo_automation::dispatch::dispatch(&armed, Event::Source(&event)).firings,
                );
            }
        }
        self.next_source_ms = next;
        firings
    }

    /// The firings of the program's triggers on something new in a source:
    /// what arrived in its person's mail, and what a look at a source
    /// outside found.
    pub(super) async fn source_firings(&mut self, now_ms: i64) -> Vec<Firing> {
        let mut firings = self.mail_firings(now_ms).await;
        firings.extend(self.looked_firings(now_ms).await);
        let shortcut = Arc::clone(&self.shortcut);
        self.progress
            .sources
            .retain(|index, _| shortcut.triggers.get(*index).is_some_and(waits_on_a_source));
        self.outside
            .health
            .retain(|index, _| shortcut.triggers.get(*index).is_some_and(waits_on_a_source));
        firings
    }

    /// Coverage health tracked for the source trigger at `trigger_index`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn monitor_health(&self, trigger_index: usize) -> Option<&MonitorHealth> {
        self.outside.health.get(&trigger_index)
    }

    /// Coverage health tracked across every active source trigger.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn all_monitor_health(&self) -> &BTreeMap<usize, MonitorHealth> {
        &self.outside.health
    }

    /// The person the program is carried out for, by their root.
    fn person_root(&self) -> Option<[u8; 32]> {
        self.person.as_deref().and_then(|person| {
            hex::decode(person)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        })
    }

    /// Whether the program waits on its person's mail and hears it.
    pub(super) fn hears_mail(&self) -> bool {
        self.mail.is_some()
    }

    /// Begin to hear the sources the program's triggers wait on, for the
    /// person it is carried out for, and fix where each of those triggers
    /// starts: what its source holds now is not new to it. Done as the
    /// program is given its person, so nothing that comes after is missed;
    /// a trigger that had read its source before carries on from there.
    pub(super) fn hear_sources(&mut self) {
        let shortcut = Arc::clone(&self.shortcut);
        if !shortcut.triggers.iter().any(waits_on_mail) {
            return;
        }
        let Some(person) = self.person_root() else {
            return;
        };
        // Heard first and read second: what arrives between the two comes
        // after the place read.
        self.mail = Some(Mail {
            _listening: self.workspaces.listen_for_mail(person),
            grew: self.workspaces.mail_arrival_changes(),
            again: true,
        });
        let newest = self
            .workspaces
            .mail_arrivals_for(&person, None, u64::MAX)
            .newest;
        let now_ms = self.now_ms();
        for (index, trigger) in shortcut.triggers.iter().enumerate() {
            if !waits_on_mail(trigger) {
                continue;
            }
            let mark = tilo_peer_state::trigger_mark(trigger);
            let seen = self
                .progress
                .sources
                .entry(index)
                .or_insert_with(|| Seen::starting(&mark, now_ms));
            seen.follow(&mark, now_ms);
            if seen.place.is_none() {
                seen.place = Some(newest);
            }
        }
    }

    /// The firings of the program's triggers on something new in its
    /// person's mail, one for each message that arrived since the last
    /// look. What was read is kept with the program's progress, so a
    /// restart fires nothing twice and what came while nothing looked is
    /// fired once. A log that could not be read is looked at again on the
    /// next pass, and nothing of it is passed over meanwhile.
    async fn mail_firings(&mut self, now_ms: i64) -> Vec<Firing> {
        let shortcut = Arc::clone(&self.shortcut);
        let mut firings = Vec::new();
        let (Some(person), true) = (
            self.person_root(),
            self.mail.as_mut().is_some_and(Mail::may_hold_more),
        ) else {
            return firings;
        };
        // What a message says is handed to its person's assistant, so only
        // the accounts they named that assistant for are read.
        let assistant = match super::super::assistant::assistant_of(
            &self.workspaces,
            &self.workspace,
            &person,
        )
        .await
        {
            Ok(assistant) => assistant,
            Err(error) => {
                tracing::warn!(installation = %self.bot.installation, %error, "the assistant a program hands its person's mail to could not be read; it looks again");
                for (index, trigger) in shortcut.triggers.iter().enumerate() {
                    if waits_on_mail(trigger) {
                        self.outside
                            .health
                            .entry(index)
                            .or_default()
                            .note_failure(error.to_string(), None);
                    }
                }
                if let Some(mail) = self.mail.as_mut() {
                    mail.again = true;
                }
                return firings;
            }
        };
        let named = assistant
            .as_ref()
            .map(|assistant| (self.workspace.as_str(), assistant.id.0.as_str()));
        let installation = self.bot.installation.clone();
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &installation,
            },
            shortcut: &shortcut,
        }];
        for (index, trigger) in shortcut.triggers.iter().enumerate() {
            let TriggerSpec::SourceChanged {
                kind, at, filter, ..
            } = trigger
            else {
                continue;
            };
            if kind != source_change::PERSON_MAIL {
                continue;
            }
            let mark = tilo_peer_state::trigger_mark(trigger);
            let seen = self
                .progress
                .sources
                .entry(index)
                .or_insert_with(|| Seen::starting(&mark, now_ms));
            seen.follow(&mark, now_ms);
            // A trigger that never looked takes nothing the log holds now.
            let after = seen.place.unwrap_or(u64::MAX);
            let mut arrived = self.workspaces.mail_arrivals_for(&person, named, after);
            if seen.place.is_some() && arrived.newest < after {
                // The log started over: all it holds came after that.
                arrived = self.workspaces.mail_arrivals_for(&person, named, 0);
            }
            seen.place = Some(arrived.newest);
            let changed = !arrived.arrivals.is_empty();
            self.outside
                .health
                .entry(index)
                .or_default()
                .note_success(now_ms, changed, 0, None);
            for arrival in arrived.arrivals {
                if !(at.is_empty() || arrival.account.eq_ignore_ascii_case(at)) {
                    continue;
                }
                let found = Found {
                    key: arrival.id.clone(),
                    at: arrival.account.clone(),
                    at_ms: arrival.at_ms,
                    facts: arrival.facts(),
                };
                if !tilo_automation::resource_time::matches(filter, &found.facts) {
                    continue;
                }
                let event = SourceEvent {
                    workspace: self.workspace.clone(),
                    trigger: index,
                    key: found.key.clone(),
                    payload: source_change::told(kind, &found),
                };
                firings.extend(
                    tilo_automation::dispatch::dispatch(&armed, Event::Source(&event)).firings,
                );
            }
        }
        firings
    }
}

#[cfg(test)]
mod tests {
    use test_http::{Reply, Responder};

    use super::*;

    /// A look at a source goes to the source itself: a proxy this server's
    /// environment names is sent nothing of it. Such a proxy would resolve a
    /// name itself, behind the lookup the fetch's client judges, and so reach
    /// what that lookup refuses.
    #[tokio::test]
    async fn a_look_at_a_source_sends_nothing_to_a_proxy_the_environment_names() {
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_look_at_a_source_sends_nothing_to_a_proxy_the_environment_names",
        ) {
            return;
        }
        let proxy = Responder::handle(|_| {
            Some(
                Reply::status(200)
                    .header("Content-Type", "text/plain")
                    .body("the proxy's answer"),
            )
        });
        let source = Responder::handle(|_| {
            Some(
                Reply::status(200)
                    .header("Content-Type", "text/plain")
                    .body("the source's own words"),
            )
        });
        let mut environment = tilo_test_env::ScopedEnv::new();
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            environment.set(name, proxy.url());
        }
        // Nothing exempts an address from that proxy, and nothing has a name of it ignored.
        for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
            environment.remove(name);
        }

        let mut outside = Outside {
            client: None,
            health: BTreeMap::new(),
            fixtures: true,
        };
        let read = outside
            .look(
                source_change::WEB_PAGE,
                &format!("{}/page", source.url()),
                0,
            )
            .await;

        let sent = proxy.drain_requests();
        assert!(
            sent.is_empty(),
            "the proxy the environment names was sent {} request(s) of a look",
            sent.len()
        );
        match read {
            Ok(Read::Text { text, .. }) => {
                assert!(text.contains("the source's own words"), "{text}");
            }
            Ok(Read::Entries(_)) => panic!("a page was read as a feed"),
            Err((why, _)) => panic!("the look failed: {why}"),
        }
        assert!(
            !source.drain_requests().is_empty(),
            "the source itself was asked"
        );
    }

    #[test]
    fn monitor_health_distinguishes_verified_no_change_from_stale_or_unreachable() {
        let interval_ms = 30 * 60 * 1000; // 30 minutes
        let t_0900 = 9 * 60 * 60 * 1000;
        let t_0930 = t_0900 + interval_ms;
        let t_1000 = t_0930 + interval_ms;
        let t_1005 = t_1000 + 5 * 60 * 1000;
        let t_1130 = t_1000 + 3 * interval_ms;

        let mut health = MonitorHealth::default();
        assert_eq!(health.honest_status(t_0900), MonitorStatus::NeverRead);
        assert!(!health.honest_status(t_0900).claims_no_change());

        // Initial clean sync at 09:00 with a change observed.
        health.note_success(t_0900, true, 0, Some(interval_ms));
        assert_eq!(health.last_successful_read_ms, Some(t_0900));
        assert_eq!(health.covered_through_ms, Some(t_0900));
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.backlog(), 0);

        // Clean sync at 10:00 with no new changes: truthfully reports no change since 09:00.
        health.note_success(t_1000, false, 0, Some(interval_ms));
        let status_ok = health.honest_status(t_1005);
        assert!(status_ok.claims_no_change());
        assert!(!status_ok.is_degraded());
        assert_eq!(
            status_ok,
            MonitorStatus::NoChangeSince {
                since_ms: t_0900,
                last_successful_read_ms: t_1000,
                covered_through_ms: t_1000,
            }
        );
        assert!(status_ok.summary().contains("no change since"));

        // If reads stop happening past 2x cadence without errors, status is Stale — NOT no change!
        let status_stale = health.honest_status(t_1130);
        assert!(!status_stale.claims_no_change());
        assert!(status_stale.is_degraded());
        assert!(matches!(
            status_stale,
            MonitorStatus::Stale {
                last_successful_read_ms,
                covered_through_ms: Some(cov),
                ..
            } if last_successful_read_ms == t_1000 && cov == t_1000
        ));
        assert!(status_stale.summary().contains("last successful sync at"));
        assert!(status_stale.summary().contains("currently stale"));

        // If reads fail, status is Unreachable and preserves last successful sync time.
        health.note_failure("connection refused", Some(interval_ms));
        health.note_failure("timeout", Some(interval_ms));
        assert_eq!(health.consecutive_failures, 2);
        assert_eq!(health.last_successful_read_ms, Some(t_1000));
        assert_eq!(health.covered_through_ms, Some(t_1000));
        let status_down = health.honest_status(t_1005);
        assert!(!status_down.claims_no_change());
        assert!(status_down.is_degraded());
        assert_eq!(
            status_down,
            MonitorStatus::Unreachable {
                last_successful_read_ms: Some(t_1000),
                covered_through_ms: Some(t_1000),
                consecutive_failures: 2,
                last_error: "timeout".to_owned(),
            }
        );
        assert!(status_down.summary().contains("last successful sync at"));
        assert!(status_down.summary().contains("currently unreachable"));

        // Recovery with backlog advances last_successful_read_ms, clears failures,
        // but does NOT advance covered_through_ms past the backlog gap.
        health.note_success(t_1130, true, 4, Some(interval_ms));
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.last_error, None);
        assert_eq!(health.last_successful_read_ms, Some(t_1130));
        assert_eq!(health.covered_through_ms, Some(t_1000));
        assert_eq!(health.backlog(), 4);
        let status_backlog = health.honest_status(t_1130 + 1_000);
        assert!(!status_backlog.claims_no_change());
        assert!(status_backlog.is_degraded());
        assert!(matches!(
            status_backlog,
            MonitorStatus::Backlogged {
                last_successful_read_ms,
                covered_through_ms: Some(cov),
                backlog_count: 4,
            } if last_successful_read_ms == t_1130 && cov == t_1000
        ));
    }
}
