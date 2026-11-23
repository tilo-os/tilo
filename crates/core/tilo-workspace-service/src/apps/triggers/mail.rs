//! The triggers on mail that arrives in a person's own accounts
//! ([`source_change::PERSON_MAIL`]): each message that arrived since the last look fires once.
//!
//! The mail read here is that of the shortcut's saver, as the host hears it arrive in the accounts
//! [`tilo.accounts`](person_accounts::PROVIDER) keeps for them ([`Workspaces::listen_for_mail`]):
//! their inboxes are watched for as long as a trigger waits on them. That person's accounts need
//! not be the workspace's mailbox; mail the mailbox takes in is a change of its records, which a
//! trigger on that change waits on instead.
//!
//! A firing holds what the arrival log keeps of the message, its envelope and the start of its
//! words, which its trigger's filter compares and its run is told as data; nothing more of it is
//! copied anywhere. It names the message itself as what the run is made from (`$from`, its app,
//! kind and resource): the host runs each step made from it only for a caller who may read it,
//! and the whole message is read from the account by whoever may.
//!
//! What each trigger read up to is kept in the workspace ([`super::keep_progress`]): a restart
//! fires nothing twice, and what arrived while nothing looked fires once. The arrival log is read
//! for its person alone, and only where they keep an assistant in the workspace, since what a
//! message says is handed to that assistant.
//!
//! [`Workspaces::listen_for_mail`]: crate::workspaces::Workspaces::listen_for_mail

use std::collections::{BTreeMap, BTreeSet};

use tilo_actions::params::FROM_ARGUMENT;
use tilo_actions::remote::person_accounts;
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, SourceEvent};
use tilo_automation::source_change::{self, Found, Seen};

use super::{Armed, Runner, dispatched, keep_progress, kept_progress, person_key};
use crate::workspaces::MailListening;

/// What the runner hears of the mail its triggers wait on.
#[derive(Default)]
pub(super) struct Heard {
    /// Keeps the inboxes of each workspace a trigger waits on watched.
    listening: BTreeMap<String, MailListening>,
    /// Moves whenever any workspace's arrival log grew; made at the first trigger on mail.
    grew: Option<tokio::sync::watch::Receiver<u64>>,
    /// What each trigger on mail read up to, by workspace, shortcut and trigger.
    seen: BTreeMap<(String, String, usize), Seen>,
}

/// Whether `trigger` waits on mail arriving in its person's accounts.
fn waits_on_mail(trigger: &TriggerSpec) -> bool {
    matches!(trigger, TriggerSpec::SourceChanged { kind, .. } if kind == source_change::PERSON_MAIL)
}

/// The arrival as what a trigger on mail compares and a firing holds: what the arrival log keeps
/// of the message.
fn found(arrival: &person_accounts::MailArrival) -> Found {
    Found {
        key: arrival.id.clone(),
        at: arrival.account.clone(),
        at_ms: arrival.at_ms,
        facts: arrival.facts(),
    }
}

impl Runner {
    /// Fire the triggers on mail arriving in their saver's accounts, one firing for each message
    /// that arrived since the trigger last read the log. The log is read when it grew, when a
    /// trigger is first seen, and after a read that could not be finished.
    pub(super) async fn mail(&mut self, now_ms: i64) {
        let waiting: BTreeSet<(String, String, usize)> = self
            .armed
            .iter()
            .flat_map(|armed| {
                armed
                    .shortcut
                    .triggers
                    .iter()
                    .enumerate()
                    .filter(|(_, trigger)| waits_on_mail(trigger))
                    .map(|(index, _)| (armed.workspace.clone(), armed.shortcut.id.clone(), index))
            })
            .collect();
        let workspaces: BTreeSet<&String> =
            waiting.iter().map(|(workspace, ..)| workspace).collect();
        self.heard
            .listening
            .retain(|workspace, _| workspaces.contains(workspace));
        self.heard.seen.retain(|key, _| waiting.contains(key));
        if waiting.is_empty() {
            return;
        }
        // Heard first and read second: what arrives between the two comes after the place read.
        for workspace in workspaces {
            if !self.heard.listening.contains_key(workspace) {
                let listening = self.workspaces.listen_for_mail(workspace);
                self.heard.listening.insert(workspace.clone(), listening);
            }
        }
        let grew = match self.heard.grew.as_mut() {
            Some(grew) => {
                let changed = grew.has_changed().unwrap_or(true);
                grew.mark_unchanged();
                changed
            }
            None => {
                self.heard.grew = Some(self.workspaces.mail_arrival_changes());
                true
            }
        };
        let mut firings = Vec::new();
        let mut unread = false;
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::SourceChanged {
                    kind, at, filter, ..
                } = trigger
                else {
                    continue;
                };
                if kind != source_change::PERSON_MAIL {
                    continue;
                }
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                // A trigger first seen here, or another in its place, carries on from what the
                // workspace kept of it; one that never read takes nothing the log holds now.
                let first = self
                    .heard
                    .seen
                    .get(&key)
                    .is_none_or(|seen| seen.mark != mark);
                if first {
                    let kept = kept_progress::<Seen>(&self.workspaces, armed, index, &mark)
                        .await
                        .filter(|seen| seen.mark == mark);
                    self.heard.seen.insert(
                        key.clone(),
                        kept.unwrap_or_else(|| Seen::starting(&mark, now_ms)),
                    );
                } else if !grew {
                    continue;
                }
                let place = self.heard.seen.get(&key).and_then(|seen| seen.place);
                let Some(read) = arrivals(&self.workspaces, armed, place).await else {
                    // Read again at the next pass, whether or not the log grew.
                    unread = true;
                    continue;
                };
                let Some(seen) = self.heard.seen.get_mut(&key) else {
                    continue;
                };
                let before = seen.clone();
                let started = seen.place.is_none();
                seen.place = Some(read.newest);
                if *seen != before {
                    let seen = seen.clone();
                    keep_progress(&self.workspaces, armed, index, &mark, &seen).await;
                }
                if started {
                    continue;
                }
                for arrival in read.arrivals {
                    if !(at.is_empty() || arrival.account.eq_ignore_ascii_case(at)) {
                        continue;
                    }
                    let found = found(&arrival);
                    if !tilo_automation::resource_time::matches(filter, &found.facts) {
                        continue;
                    }
                    let mut payload = source_change::told(kind, &found);
                    payload.insert(FROM_ARGUMENT.to_owned(), arrival.made_from().to_json());
                    let event = SourceEvent {
                        workspace: armed.workspace.clone(),
                        trigger: index,
                        key: found.key,
                        payload,
                    };
                    firings.extend(dispatched(
                        std::slice::from_ref(armed),
                        Event::Source(&event),
                        now_ms,
                    ));
                }
            }
        }
        if unread {
            self.heard.grew = None;
        }
        self.offer(firings);
    }
}

/// What arrived for `armed`'s saver after `place` of the log, or `None` when it could not be read
/// now and is read again at the next pass. A log that started over is read from its start.
async fn arrivals(
    workspaces: &crate::workspaces::Workspaces,
    armed: &Armed,
    place: Option<u64>,
) -> Option<person_accounts::MailArrivals> {
    let after = place.unwrap_or(u64::MAX);
    let nothing = || person_accounts::MailArrivals {
        arrivals: Vec::new(),
        newest: workspaces.mail_arrivals(&armed.workspace, u64::MAX).newest,
    };
    // A shortcut carried out for nobody has nobody's mail.
    let (Some(person), Some(workspace)) = (
        person_key(&armed.saver),
        collab_types::WorkspaceId::parse(&armed.workspace),
    ) else {
        return Some(nothing());
    };
    // What a message says is handed to its person's assistant: where they keep none, the log is
    // passed over.
    match crate::agents::assistant::assistant_of(workspaces, &armed.workspace, &person).await {
        Ok(Some(_)) => {}
        Ok(None) => return Some(nothing()),
        Err(error) => {
            tracing::warn!(shortcut = %armed.shortcut.id, %error, "the assistant mail is handed to could not be read; it is read again");
            return None;
        }
    }
    let mut read = workspaces
        .mail_arrivals_for(workspace, &person, after)
        .await;
    if place.is_some() && read.newest < after {
        read = workspaces.mail_arrivals_for(workspace, &person, 0).await;
    }
    Some(read)
}
