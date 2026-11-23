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

use std::sync::Arc;

use serde_json::{Map, json};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Armed, Event, Firer, Firing, SourceEvent};
use tilo_automation::source_change::{self, Failure, Found, Seen};
use tilo_web_search::{FetchError, FetchLimits};

use super::{Ended, ProgramRunner};
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

/// What looks at the sources outside for one program: the client every
/// fetch goes out through, made at the first look.
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

impl ProgramRunner {
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
        let installation = self.program.id.clone();
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
                match read {
                    Ok(Read::Entries(entries)) => {
                        let new = seen.took(entries, every, now_ms).new;
                        found.extend(new.into_iter().filter(|one| {
                            tilo_automation::resource_time::matches(filter, &one.facts)
                        }));
                    }
                    Ok(Read::Text { title, text }) => {
                        found.extend(seen.took_text(at, title.as_deref(), &text, every, now_ms));
                    }
                    Err((why, failure)) => {
                        tracing::info!(%installation, %kind, ?failure, "a look at a source outside did not read it");
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
        firings
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
        if self.person_root().is_none() {
            return;
        }
        // Heard first and read second: what arrives between the two comes
        // after the place read.
        self.mail = Some(Mail {
            _listening: self.workspaces.listen_for_mail(&self.workspace),
            grew: self.workspaces.mail_arrival_changes(),
            again: true,
        });
        let newest = self
            .workspaces
            .mail_arrivals(&self.workspace, u64::MAX)
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
        // the mail the workspace's policy lets that person read is read, and
        // only where they have an assistant to hand it to.
        let assistant = match tilo_workspace_service::agents::assistant::assistant_of(
            &self.workspaces,
            &self.workspace,
            &person,
        )
        .await
        {
            Ok(assistant) => assistant,
            Err(error) => {
                tracing::warn!(installation = %self.program.id, %error, "the assistant a program hands its person's mail to could not be read; it looks again");
                if let Some(mail) = self.mail.as_mut() {
                    mail.again = true;
                }
                return firings;
            }
        };
        let Some(workspace) = collab_types::WorkspaceId::parse(&self.workspace) else {
            return firings;
        };
        let hands_over = assistant.is_some();
        let installation = self.program.id.clone();
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
            let mut arrived = self
                .workspaces
                .mail_arrivals_for(workspace, &person, after)
                .await;
            if seen.place.is_some() && arrived.newest < after {
                // The log started over: all it holds came after that.
                arrived = self
                    .workspaces
                    .mail_arrivals_for(workspace, &person, 0)
                    .await;
            }
            if !hands_over {
                arrived.arrivals.clear();
            }
            seen.place = Some(arrived.newest);
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
}
