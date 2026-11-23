//! The triggers on something new in a source outside the workspace
//! ([`TriggerSpec::SourceChanged`]): each thing found there that was not there before fires once
//! ([`tilo_automation::source_change`]).
//!
//! A source outside, a feed or a page on the public web, tells nobody of what is new, so it is
//! looked at on a clock, with no model ([`source_change::looked_at`]). The look is the fetch an
//! agent's own read of a page goes through ([`tilo_web_search::fetch`]) and nothing beside it:
//! the same refusal of this machine's own network and of every address that is not public, at
//! the lookup the connection uses, the same wait and the same redirects, and less to read. What
//! it read is compared with the last look by [`tilo_automation::source_change`], which also
//! keeps the bounds: how often, how much is handed over, how much longer a source that fails is
//! left alone, and when it counts as gone. What is new goes with the firing as data. An address
//! is never written to the log: it may carry a key of its person's.
//!
//! What the looks took in is kept in the workspace ([`super::keep_progress`]): a restart looks
//! no sooner and hands nothing over twice, and the device that looks next carries on from it.
//!
//! A source that arrived in the workspace's mail is another matter: a message that arrives is
//! reported as a change of the workspace's records, and a shortcut waiting on it fires there.

use std::collections::BTreeSet;

use serde_json::{Map, json};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, SourceEvent};
use tilo_automation::source_change::{self, Failure, Found, Seen};
use tilo_web_search::{FetchError, FetchLimits};

use super::{Runner, dispatched, keep_progress, kept_progress};

/// The most one look downloads: a fifth of what an agent's read of a page may.
const LOOK_BYTES: usize = 1024 * 1024;
/// The longest a key that names a thing found outside may be, in characters.
const MAX_KEY_CHARS: usize = 512;

/// What looks at the sources outside for this device's shortcuts: the client every fetch goes
/// out through, made at the first look.
#[derive(Default)]
pub(super) struct Outside {
    client: Option<tilo_net::Client>,
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

/// How a fetch that failed is taken: an address the fetch refuses is never looked at again, a
/// server in trouble may answer the next time, and anything else says that what was asked for is
/// not there.
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

/// The entries of the feed `text` is, read at `at`, each named by the id the feed gives it and
/// carrying what a person reads of it.
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
    /// The client every look goes out through, made at the first one. It dials every address
    /// itself: a proxy this machine's environment names would resolve a name behind the lookup
    /// the fetch judges.
    fn client(&mut self) -> Result<tilo_net::Client, (String, Failure)> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = tilo_web_search::direct_client()
            .map_err(|error| (error.to_string(), Failure::Passing))?;
        self.client = Some(client.clone());
        Ok(client)
    }

    /// Look once at the source of `kind` at `at`, through the fetch an agent's own read of a
    /// page goes through, with every limit that has and less to read. Why it could not be read,
    /// and how that is taken, otherwise.
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

impl Runner {
    /// Have the looks at sources outside reach a server on this machine: a test's own, which the
    /// fetch refuses.
    #[cfg(test)]
    pub(super) fn reach_fixtures(&mut self) {
        self.outside.fixtures = true;
    }

    /// Fire the triggers on something new in a source that is looked at on a clock, for each
    /// source whose look is due: one firing for each thing the look found that the last one did
    /// not, and one, once, when the source has come to count as gone.
    pub(super) async fn outside_changes(&mut self, now_ms: i64) {
        let mut firings = Vec::new();
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::SourceChanged {
                    kind,
                    at,
                    every_secs,
                    filter,
                } = trigger
                else {
                    continue;
                };
                // A source that tells nobody of what is new is looked at here; the mail of the
                // workspace's accounts is a change of its records instead.
                if !source_change::looked_at(kind) {
                    continue;
                }
                let every = source_change::look_secs(*every_secs);
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                // A trigger first seen here, or another trigger in this one's place, carries on
                // from what the workspace kept of it, and starts over where nothing was.
                if self.looks.get(&key).is_none_or(|seen| seen.mark != mark) {
                    let kept = kept_progress::<Seen>(&self.workspaces, armed, index, &mark)
                        .await
                        .filter(|seen| seen.mark == mark);
                    self.looks.insert(
                        key.clone(),
                        kept.unwrap_or_else(|| Seen::starting(&mark, now_ms)),
                    );
                }
                let due = self.looks.get_mut(&key).is_some_and(|seen| {
                    seen.follow(&mark, now_ms);
                    seen.due(now_ms)
                });
                if !due {
                    continue;
                }
                let read = self.outside.look(kind, at, now_ms).await;
                let Some(seen) = self.looks.get_mut(&key) else {
                    continue;
                };
                let before = seen.clone();
                let mut found = Vec::new();
                match read {
                    Ok(Read::Entries(entries)) => {
                        found.extend(seen.took(entries, every, now_ms).new.into_iter().filter(
                            |one| tilo_automation::resource_time::matches(filter, &one.facts),
                        ));
                    }
                    Ok(Read::Text { title, text }) => {
                        found.extend(seen.took_text(at, title.as_deref(), &text, every, now_ms));
                    }
                    Err((why, failure)) => {
                        tracing::info!(%kind, ?failure, shortcut = %armed.shortcut.id, "a look at a source outside did not read it");
                        found.extend(seen.failed(at, &why, failure, every, now_ms));
                    }
                }
                // What the look took in is kept in the workspace, so nothing found is handed over
                // twice and a restart looks no sooner.
                if *seen != before {
                    let seen = seen.clone();
                    keep_progress(&self.workspaces, armed, index, &mark, &seen).await;
                }
                for one in found {
                    let event = SourceEvent {
                        workspace: armed.workspace.clone(),
                        trigger: index,
                        key: one.key.clone(),
                        payload: source_change::told(kind, &one),
                    };
                    firings.extend(dispatched(
                        std::slice::from_ref(armed),
                        Event::Source(&event),
                        now_ms,
                    ));
                }
            }
        }
        let watched: BTreeSet<(String, String, usize)> = self
            .armed
            .iter()
            .flat_map(|armed| {
                armed
                    .shortcut
                    .triggers
                    .iter()
                    .enumerate()
                    .filter(|(_, trigger)| {
                        matches!(trigger, TriggerSpec::SourceChanged { kind, .. } if source_change::looked_at(kind))
                    })
                    .map(|(index, _)| (armed.workspace.clone(), armed.shortcut.id.clone(), index))
            })
            .collect();
        self.looks.retain(|key, _| watched.contains(key));
        self.offer(firings);
    }
}
