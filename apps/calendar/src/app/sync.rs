//! Reconciling the calendar with its `CalDAV` servers, and reading the
//! document that reconciliation runs on.
//!
//! A pull is only folded into the document it was made against: the state
//! that last read whole is what a server's answer is compared with, so a
//! document that does not read refuses the reconciliation instead of losing
//! the identities, sync tokens and queued uploads it still holds.

use std::collections::HashSet;

use automerge::AutoCommit;
use uuid::Uuid;

use super::{CalendarApp, status};
use crate::caldav::{self, SyncOutcome};
use crate::model::{Calendar, Event, Id};

impl CalendarApp {
    /// Kick off a `CalDAV` pull from the configured calendars. The live pull
    /// touches the network, so it runs on a worker thread and its result is
    /// drained in [`Self::poll_sync`]; the UI stays responsive throughout. The
    /// hermetic demo/test build stays fully synchronous (it synthesises each
    /// calendar's events through the real [`ics::parse`] path — no network, no
    /// thread — so `--shots` and tests are deterministic).
    #[allow(
        clippy::too_many_lines,
        reason = "sync orchestration keeps worker admission and failure contracts together"
    )]
    pub(super) fn start_sync(&mut self, doc: &mut ui::widget::Edits<'_>) -> Result<(), String> {
        if self.syncing {
            return Ok(());
        }
        let inputs: Vec<_> = self
            .accounts
            .accounts
            .iter()
            .map(|a| (a.config.clone(), a.calendars.clone()))
            .collect();
        let sync_account_ids: Vec<String> = self
            .accounts
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect();

        if self.sample_accounts {
            let (result, outcome) = caldav::sample_pull_all(&inputs, self.today);
            return self.apply_sync_result(result, outcome, doc);
        }

        // No account configured: answer instantly rather than spinning up a
        // worker that would only return Offline.
        if self.accounts.is_empty() {
            self.show_status_with(
                "No calendar accounts yet. Add one in Settings.".into(),
                &[status::StatusAction::OpenSettings],
            );
            return Ok(());
        }

        // A live pull as a task on the app runtime; the result lands via
        // `poll_sync`. Uploads and deletes are drained as they are queued
        // (`crate::outbox`); an item still waiting for its upload is never
        // overwritten or removed by what the server sends meanwhile.
        let runtime = match ui::rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                self.status.show(format!("Sync unavailable: {error}"));
                self.dirty = true;
                return Ok(());
            }
        };
        let Some(current) = self.read_document(doc.doc()) else {
            // A pull from a calendar read as empty would drop the protection of
            // every edit still waiting for its upload, and start from no sync
            // token. Ask for a read that succeeds first.
            return Err(Self::unreadable_refusal(self.model_read.failure()));
        };
        let protected_hrefs: Vec<(String, String)> = crate::outbox::pending_copies(&current)
            .into_iter()
            .map(|copy| (copy.calendar.clone(), copy.href.clone()))
            .collect();
        let sync_tokens: Vec<(String, String)> = current
            .sync_tokens
            .iter()
            .map(|(calendar, token)| (calendar.clone(), token.clone()))
            .collect();
        // The worker's reply wakes the app (`waking_channel`): nothing polls
        // for it frame by frame while a slow server keeps the request open.
        let (tx, rx) = ui::wake::waking_channel();
        let today = self.today;
        for id in &sync_account_ids {
            if let Err(error) = accounts::AccountSyncReporter::new(id.clone()).started() {
                tracing::warn!(%error, account = id, "calendar: could not persist sync start");
            }
        }
        drop(runtime.spawn(async move {
            let mut result = caldav::SyncResult::default();
            let mut synced_calendars = 0;
            let mut first_failure = None;
            for (id, input) in sync_account_ids.iter().zip(inputs.iter()) {
                let (mut account_result, account_outcome) = caldav::sync_all_with_checkpoints(
                    std::slice::from_ref(input),
                    today,
                    &sync_tokens,
                )
                .await;
                let reporter = accounts::AccountSyncReporter::new(id.clone());
                let persisted = match &account_outcome {
                    caldav::SyncOutcome::Synced { .. } => reporter.succeeded(),
                    caldav::SyncOutcome::Offline => reporter.offline(),
                    caldav::SyncOutcome::Failed(reason)
                        if reason.contains("401") || reason.contains("403") =>
                    {
                        reporter.needs_authentication()
                    }
                    caldav::SyncOutcome::Failed(_) => reporter.failed(),
                };
                if let Err(error) = persisted {
                    tracing::warn!(%error, account = id, "calendar: could not persist sync result");
                }
                result.events.append(&mut account_result.events);
                result.tasks.append(&mut account_result.tasks);
                result.resources.append(&mut account_result.resources);
                result.deletions.append(&mut account_result.deletions);
                result.checkpoints.append(&mut account_result.checkpoints);
                match account_outcome {
                    caldav::SyncOutcome::Synced { calendars, .. } => {
                        synced_calendars += calendars;
                    }
                    caldav::SyncOutcome::Offline => {}
                    caldav::SyncOutcome::Failed(reason) => {
                        first_failure.get_or_insert(reason);
                    }
                }
            }
            let outcome = if let Some(reason) = first_failure {
                caldav::SyncOutcome::Failed(reason)
            } else if synced_calendars > 0 {
                caldav::SyncOutcome::Synced {
                    calendars: synced_calendars,
                    events: result.events.len(),
                }
            } else {
                caldav::SyncOutcome::Offline
            };
            // Do not overwrite a locally edited object still waiting for its
            // upload (or whose conditional upload conflicted). It stays
            // queued; the next upload can be resolved without destroying
            // either copy.
            result.resources.retain(|resource| {
                !protected_hrefs.iter().any(|(calendar_id, href)| {
                    calendar_id == &resource.calendar_id
                        && caldav::hrefs_equal(href, &resource.href)
                })
            });
            // A failed/conflicted local edit wins neither side automatically.
            // Suppress an incremental tombstone for it and treat it as seen in
            // a full fallback, leaving the local copy and outbox intact for an
            // explicit retry/resolution.
            result.deletions.retain(|deletion| {
                !protected_hrefs.iter().any(|(calendar_id, href)| {
                    calendar_id == &deletion.calendar_id
                        && caldav::hrefs_equal(href, &deletion.href)
                })
            });
            for checkpoint in &mut result.checkpoints {
                if checkpoint.full {
                    checkpoint.seen_hrefs.extend(
                        protected_hrefs
                            .iter()
                            .filter(|(calendar_id, _)| calendar_id == &checkpoint.calendar_id)
                            .map(|(_, href)| href.clone()),
                    );
                }
            }
            let _ = tx.send((result, outcome));
        }));
        self.sync_rx = Some(rx);
        self.syncing = true;
        self.status.show_progress("Syncing…".into());
        self.dirty = true;
        Ok(())
    }

    /// Drain a finished background sync, if any. Called every `sync` pass; a
    /// no-op until the worker delivers. Keeping this off the click path is why
    /// a slow or unreachable server never freezes the window.
    pub(super) fn poll_sync(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(rx) = self.sync_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((result, outcome)) => {
                self.sync_rx = None;
                self.syncing = false;
                let failure = match &outcome {
                    SyncOutcome::Failed(reason) => Some(reason.clone()),
                    SyncOutcome::Offline => Some("calendar service is offline".to_string()),
                    SyncOutcome::Synced { .. } => None,
                };
                let reconciled = self.apply_sync_result(result, outcome, doc);
                if let Some(reply) = self.sync_reply.take() {
                    // A pull that could not be reconciled answers with why, not
                    // with the server's outcome: the document holds neither.
                    let answered = reconciled.and_then(|()| {
                        failure.map_or_else(|| Ok(tilo_actions::ActionOutput::default()), Err)
                    });
                    reply.finish(answered);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Worker panicked or dropped the sender without a reply.
                self.sync_rx = None;
                self.syncing = false;
                self.show_status_with("Sync failed".into(), &[status::StatusAction::SyncAgain]);
                if let Some(reply) = self.sync_reply.take() {
                    reply.finish(Err(
                        "calendar synchronization stopped without a result".into()
                    ));
                }
                self.dirty = true;
            }
        }
    }

    /// Atomically reconcile server resources by stable collection href. A
    /// title/date is presentation, never identity; edits update the same local
    /// UUID across pulls and server deletions remove it without resurrection.
    #[allow(
        clippy::too_many_lines,
        reason = "remote reconciliation is deliberately kept in one transactional merge routine"
    )]
    pub(super) fn apply_sync_result(
        &mut self,
        result: caldav::SyncResult,
        outcome: SyncOutcome,
        doc: &mut ui::widget::Edits<'_>,
    ) -> Result<(), String> {
        // Reconciling against a calendar read as empty would give every item
        // the server holds a new identity, forgetting which of them this
        // document already knows. Refuse instead: the answer says nothing was
        // reconciled and the pull is not repeated from a made-up state.
        let Some(current) = self.read_document(doc.doc()) else {
            let refusal = Self::unreadable_refusal(self.model_read.failure());
            self.status.show(refusal.clone());
            self.dirty = true;
            return Err(refusal);
        };
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut removed = 0usize;
        // Where each item a server holds lives there, as the calendar knows
        // it and as this pull adds to it.
        let mut known: Vec<(Id, String, String)> = current
            .remote
            .iter()
            .filter_map(|(id, copy)| {
                Some((id.parse().ok()?, copy.calendar.clone(), copy.href.clone()))
            })
            .collect();
        let find = |known: &[(Id, String, String)], calendar: &str, href: &str| {
            known
                .iter()
                .find(|(_, held_calendar, held_href)| {
                    held_calendar == calendar && caldav::hrefs_equal(held_href, href)
                })
                .map(|(id, _, _)| *id)
        };

        // Sample results carry no server address of their own.
        for mut e in result.events {
            if self
                .model
                .events
                .iter()
                .any(|x| x.title == e.title && x.start == e.start)
            {
                continue;
            }
            e.id = Uuid::new_v4();
            let at = crate::resource::RemoteAt {
                calendar: e.calendar_id.clone().unwrap_or_else(|| "sample".into()),
                href: String::new(),
                etag: None,
            };
            self.adopt_event(doc, &e, at);
            self.model.events.push(e);
            added += 1;
        }
        for resource in result.resources {
            for (index, mut event) in resource.events.into_iter().enumerate() {
                // Current parser represents recurrence as one event. If a
                // server returns multiple components in one object, retain
                // stable identities for all by an internal component suffix.
                let binding_href = if index == 0 {
                    resource.href.clone()
                } else {
                    format!("{}#tilo-component-{index}", resource.href)
                };
                let id = find(&known, &resource.calendar_id, &binding_href);
                // An item deleted here keeps what its server held until the
                // server deleted it too: a late pull still carrying it does
                // not bring it back.
                if id.is_some_and(|id| current.event(id).is_none()) {
                    continue;
                }
                let id = id.unwrap_or_else(Uuid::new_v4);
                event.id = id;
                event.calendar_id = Some(resource.calendar_id.clone());
                if let Some(slot) = self.model.events.iter_mut().find(|item| item.id == id) {
                    *slot = event.clone();
                    updated += 1;
                } else {
                    self.model.events.push(event.clone());
                    added += 1;
                }
                known.push((id, resource.calendar_id.clone(), binding_href.clone()));
                self.adopt_event(
                    doc,
                    &event,
                    crate::resource::RemoteAt {
                        calendar: resource.calendar_id.clone(),
                        href: binding_href,
                        etag: resource.etag.clone(),
                    },
                );
            }
            for (index, mut task) in resource.tasks.into_iter().enumerate() {
                let binding_href = format!("{}#tilo-task-{index}", resource.href);
                let id = find(&known, &resource.calendar_id, &binding_href);
                if id.is_some_and(|id| current.task(id).is_none()) {
                    continue;
                }
                let id = id.unwrap_or_else(Uuid::new_v4);
                task.id = id;
                if let Some(slot) = self.model.tasks.iter_mut().find(|item| item.id == id) {
                    *slot = task.clone();
                    updated += 1;
                } else {
                    self.model.tasks.push(task.clone());
                    added += 1;
                }
                known.push((id, resource.calendar_id.clone(), binding_href.clone()));
                self.submit(
                    doc,
                    Calendar::ADOPT_REMOTE_TASK.with(crate::resource::calendar::AdoptRemoteTask {
                        task: crate::actions::TaskData::from_task(&task),
                        at: crate::resource::RemoteAt {
                            calendar: resource.calendar_id.clone(),
                            href: binding_href,
                            etag: resource.etag.clone(),
                        },
                    }),
                );
            }
        }
        let held_in = |calendar: &str| -> Vec<(Id, String)> {
            current
                .remote
                .iter()
                .filter(|(_, copy)| copy.calendar == calendar && !copy.href.is_empty())
                .filter_map(|(id, copy)| Some((id.parse().ok()?, copy.href.clone())))
                .collect()
        };
        for deletion in result.deletions {
            let victims: Vec<Id> = held_in(&deletion.calendar_id)
                .into_iter()
                .filter(|(_, href)| {
                    caldav::hrefs_equal(href.split("#tilo-").next().unwrap_or(href), &deletion.href)
                })
                .map(|(id, _)| id)
                .collect();
            for id in victims {
                self.drop_remote(doc, id);
                removed += 1;
            }
        }
        // A successful full query is authoritative for that collection. This
        // is also the invalid-token recovery path.
        for checkpoint in &result.checkpoints {
            if checkpoint.full {
                let seen: HashSet<&str> =
                    checkpoint.seen_hrefs.iter().map(String::as_str).collect();
                for (id, href) in held_in(&checkpoint.calendar_id) {
                    let base = href.split("#tilo-").next().unwrap_or(&href);
                    if !seen
                        .iter()
                        .any(|seen_href| caldav::hrefs_equal(base, seen_href))
                    {
                        self.drop_remote(doc, id);
                        removed += 1;
                    }
                }
            }
        }
        // Commit markers are deliberately last: a crash before here repeats a
        // response idempotently; a crash after here sees fully reconciled data.
        for checkpoint in result.checkpoints {
            self.submit(
                doc,
                Calendar::PULLED.with(crate::resource::calendar::Pulled {
                    calendar: checkpoint.calendar_id.clone(),
                    token: checkpoint.token.clone(),
                }),
            );
        }
        self.model.events.settle();
        let actions = status::after_sync(&outcome, self.accounts.is_empty());
        let text = match outcome {
            SyncOutcome::Synced { calendars, .. } => {
                format!(
                    "Synced {calendars} calendars. {added} new, {updated} updated, {removed} removed."
                )
            }
            SyncOutcome::Offline => {
                if self.accounts.is_empty() {
                    "No calendar accounts yet. Add one in Settings.".into()
                } else {
                    "Couldn't reach the calendar server".into()
                }
            }
            SyncOutcome::Failed(reason) => {
                format!("Sync failed: {reason}. Check the account in Settings, then sync again.")
            }
        };
        self.show_status_with(text, actions);
        Ok(())
    }

    /// Write `event` as its server holds it, at `at`.
    fn adopt_event(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        event: &Event,
        at: crate::resource::RemoteAt,
    ) {
        self.submit(
            doc,
            Calendar::ADOPT_REMOTE_EVENT.with(crate::resource::calendar::AdoptRemoteEvent {
                event: crate::actions::EventData::from_event(event),
                at,
            }),
        );
    }

    /// Drop the item `id` its server no longer holds.
    fn drop_remote(&mut self, doc: &mut ui::widget::Edits<'_>, id: Id) {
        self.submit(
            doc,
            Calendar::DROP_REMOTE_ITEM.with(crate::resource::calendar::DropRemoteItem { item: id }),
        );
        self.model.events.retain(|event| event.id != id);
        self.model.tasks.retain(|task| task.id != id);
    }

    /// What `state` holds, or `None` while it does not read as a calendar.
    /// A failed read is not an empty calendar: `model` keeps the state that
    /// last read whole, and the person is told why the newest read did not.
    fn read_document(&mut self, state: &AutoCommit) -> Option<Calendar> {
        let calendar = self.model_read.adopt(Calendar::read_whole(state));
        if calendar.is_none() {
            self.report_unreadable();
        }
        calendar
    }

    /// Read `state` into the model, or leave the calendar that last read
    /// whole in place.
    pub(super) fn adopt_model(&mut self, state: &AutoCommit) {
        if let Some(calendar) = self.read_document(state) {
            self.model = calendar;
        }
    }

    /// Say why the document does not read, once per distinct failure.
    fn report_unreadable(&mut self) {
        let Some(reason) = self.model_read.take_report() else {
            return;
        };
        self.status.show(format!(
            "This calendar can't be read: {reason}. Nothing was changed."
        ));
        self.dirty = true;
    }

    /// What a synchronization that cannot read its document answers: nothing
    /// was reconciled, so a caller must not be told it was.
    fn unreadable_refusal(failure: Option<&ui::tilo_resource::ModelError>) -> String {
        let reason = failure.map(ToString::to_string).unwrap_or_default();
        format!("This calendar can't be read: {reason}. Nothing was changed.")
    }
}
