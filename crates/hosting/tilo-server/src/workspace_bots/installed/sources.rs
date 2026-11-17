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

use std::sync::Arc;

use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Armed, Event, Firer, Firing, SourceEvent};
use tilo_automation::source_change::{self, Found, Seen};

use super::BotRunner;
use crate::workspaces::MailListening;

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

impl BotRunner {
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

    /// The firings of the program's triggers on something new in a source,
    /// one for each thing found since the last look. What was read is kept
    /// with the program's progress, so a restart fires nothing twice and
    /// what came while nothing looked is fired once. A source that could
    /// not be read is looked at again on the next pass, and nothing of it
    /// is passed over meanwhile.
    pub(super) async fn source_firings(&mut self, now_ms: i64) -> Vec<Firing> {
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
            let TriggerSpec::SourceChanged { kind, at, filter } = trigger else {
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
        self.progress
            .sources
            .retain(|index, _| shortcut.triggers.get(*index).is_some_and(waits_on_a_source));
        firings
    }
}
