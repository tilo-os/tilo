//! Route-owned observations of synchronous local input. Snapshot identities and
//! child aliases are frozen before execution; draining never queries a directory.
use super::WindowEvent;
use super::{App, action_directory_snapshot};
use crate::local_document_action::{InputRoute, Queue};
use std::{collections::HashMap, rc::Rc};

/// The window's resource for changes that could not be saved to the history.
pub(super) const UNSAVED_RESOURCE: &str = "tilo.host.unsaved-changes";
/// Its one operation: Retry.
pub(super) const UNSAVED_OPERATION: &str = "tilo.host.unsaved-changes.retry";
/// What the window says while its edits land where no report is admitted.
const UNRECORDED: &str = "history-local-action-unrecorded";

#[derive(Default)]
pub(super) struct LocalInputCapture {
    queues: HashMap<crate::collaboration::RouteSpec, Queue>,
    pending: HashMap<u64, PendingCapture>,
    transport_connected: bool,
    // One bounded, inert copy of the last authenticated pre-input surface.
    offline_routes: Vec<InputRoute>,
    // Edits made under input no authenticated publication observed. Only a
    // recorded edit sets it; scrolling, selecting or hovering never does.
    unsigned_edits: crate::local_document_action::UnsignedEdits,
    // Edits made under input that a publication saw but no published
    // action route could carry. Only a recorded edit sets it: a wheel that
    // pans, a hover or a selection is no unconfirmed action.
    unrouted_edits: crate::local_document_action::UnsignedEdits,
    /// What the person is told, once a change definitively could not be
    /// saved to their history. Nothing is said while it is still on its way.
    pub(super) failure: Option<String>,
    /// Routes whose connection the broker admits no local reports on (it
    /// is no worker there: no launcher vouched for this app, or the route
    /// is another app's). Edits on them still land and are saved here, and
    /// the person is told their history does not record them, until the
    /// connection is replaced.
    reports_nothing: std::collections::HashSet<crate::collaboration::RouteSpec>,
    /// Edits made under input whose every route reports nothing. Only a
    /// recorded edit sets it.
    unrecorded_edits: crate::local_document_action::UnsignedEdits,
    /// Changes the broker refused, kept for the person's Retry.
    refused: Vec<(
        crate::collaboration::RouteSpec,
        crate::local_document_action::CapturedAction,
    )>,
}

struct PendingCapture {
    capture: crate::local_document_action::CapturedAction,
    route: crate::collaboration::RouteSpec,
    _lease: crate::local_document_action::Lease,
    /// The published surface a capture made under a stale one is reported
    /// against, held until its outcome is known.
    _pin: Option<Rc<action_directory_snapshot::LocalDocumentDirectoryPin>>,
    retry_at: crate::time::Instant,
    retry_delay: crate::time::Duration,
}

impl LocalInputCapture {
    /// Keep `capture` until its report's outcome is known, under a request
    /// id of its own, reported from `now`.
    fn hold(
        &mut self,
        route: crate::collaboration::RouteSpec,
        capture: crate::local_document_action::CapturedAction,
        lease: crate::local_document_action::Lease,
        now: crate::time::Instant,
    ) {
        let mut request_id = capture.invocation.as_u64_pair().0;
        while self.pending.contains_key(&request_id) {
            request_id = request_id.wrapping_add(1);
        }
        self.pending.insert(
            request_id,
            PendingCapture {
                capture,
                route,
                _lease: lease,
                _pin: None,
                retry_at: now,
                retry_delay: crate::time::Duration::from_secs(1),
            },
        );
    }

    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(super) fn retry_now(&mut self) {
        for pending in self.pending.values_mut() {
            pending.retry_at = crate::time::now();
        }
    }

    #[cfg(test)]
    pub(super) fn ownership_size(&self) -> usize {
        self.pending
            .values()
            .map(|pending| retained_size(&pending.capture))
            .sum()
    }

    #[cfg(test)]
    pub(super) fn route_count(&self) -> usize {
        self.queues.len()
    }
}

#[cfg(test)]
fn retained_size(capture: &crate::local_document_action::CapturedAction) -> usize {
    // Include allocations duplicated in the typed and published forms.
    capture.encoded.payload.len()
        + capture.encoded.type_id.len()
        + capture.revision.len()
        + serde_json::to_vec(&capture.context).map_or(0, |bytes| bytes.len())
        + serde_json::to_vec(&capture.ownership).map_or(0, |bytes| bytes.len())
        + capture.published.as_ref().map_or(0, |published| {
            published.resource.len()
                + published.action.len()
                + serde_json::to_vec(&published.arguments).map_or(0, |bytes| bytes.len())
        })
        + (capture.original_heads.len() + capture.mutation_hashes.len()) * 32
}

impl App {
    pub(super) fn begin_local_input_event(
        &mut self,
        event: &WindowEvent,
    ) -> crate::local_document_action::Scope {
        let input = match event {
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::Ime(winit::event::Ime::Commit(_)) => true,
            WindowEvent::CursorMoved { .. } => self.input.pointer.buttons != 0,
            _ => false,
        };
        if input {
            self.begin_local_input_capture()
        } else {
            crate::local_document_action::suspend()
        }
    }

    /// An edit went into history no authenticated publication observed: no
    /// action reports it. The change is the person's, saved here, and there
    /// is nothing to tell them.
    fn note_local_input_unsigned(&self) {
        tracing::debug!("an edit no published action reports was saved");
    }

    pub(super) fn begin_local_input_capture(&mut self) -> crate::local_document_action::Scope {
        // Input that arrives while this thread's input is already observed is
        // that observation's: it already carries the routes the person's edits
        // are reported on, and the app must not replace them with its own.
        if crate::local_document_action::input_is_active() {
            return crate::local_document_action::keep();
        }
        if !self.collaboration.manager.is_connected() {
            let routes = self.local_input_capture.offline_routes.clone();
            if routes.is_empty() && self.actions.action_directory_snapshot.is_none() {
                // No authenticated publication: preserve editing, but do not
                // invent a caller, route, revision or admission. Input is not
                // history: only an edit it records makes history unsigned,
                // learnt when the input drains.
                return crate::local_document_action::enter_unsigned_input(
                    self.local_input_capture.unsigned_edits.clone(),
                );
            }
            for route in &routes {
                if let Some(pin) = route.pin.as_ref().and_then(|pin| {
                    pin.clone()
                        .downcast::<action_directory_snapshot::LocalDocumentDirectoryPin>()
                        .ok()
                }) {
                    if !self
                        .actions
                        .local_document_directory_pins
                        .iter()
                        .any(|old| old.upgrade().is_some_and(|old| Rc::ptr_eq(&old, &pin)))
                    {
                        self.actions
                            .local_document_directory_pins
                            .push(Rc::downgrade(&pin));
                    }
                }
            }
            if !routes.is_empty() {
                return crate::local_document_action::enter_input(routes);
            }
        }
        // Publication runs outside the input scope: refreshing controls or
        // draining a model wake must not acquire the person's input identity.
        let background = crate::local_document_action::suspend();
        let mut serviced = 0;
        if self.collaboration.manager.is_connected() {
            // Controls left by an earlier model pass are not this input's
            // actions. Service them before freezing the input's snapshot.
            serviced = self.dispatch_control_invocations();
        }
        // An input that arrives while the frame after an earlier one has
        // still to show and publish what that one changed (a key's release
        // after its press, the next key typed in the same wake) acts on what
        // the window shows: the surface on the route. Building the surface
        // again here would only be paid for again by that frame.
        let owed = self.actions.action_surface_owed && serviced == 0;
        let (revision, snapshot, pinned) = if let Some((revision, snapshot)) =
            owed.then(|| self.published_action_directory()).flatten()
        {
            let pinned = self.pin_local_document_action_directory(&revision, snapshot.clone());
            (Some(revision), snapshot, Some(pinned))
        } else {
            self.publish_action_surface();
            let Some(snapshot) = self.actions.action_directory_snapshot.clone() else {
                drop(background);
                return crate::local_document_action::enter_unsigned_input(
                    self.local_input_capture.unsigned_edits.clone(),
                );
            };
            let revision = self
                .actions
                .action_directory_stamp
                .as_ref()
                .map(|(_, revision)| revision.clone());
            let pinned = revision
                .as_deref()
                .map(|revision| self.try_pin_local_document_action_directory(revision));
            (revision, snapshot, pinned)
        };
        let mut routes: Vec<InputRoute> = Vec::new();
        let mut reports_nothing = false;
        let revision_pending = !matches!(
            pinned,
            Some(Ok(_) | Err(action_directory_snapshot::PinRefused::Full))
        );
        let pin = pinned.and_then(Result::ok);
        if let Some(revision) = revision.as_ref() {
            let mut aliases: HashMap<
                Option<uuid::Uuid>,
                (crate::collaboration::RouteSpec, HashMap<String, String>),
            > = HashMap::new();
            for address in snapshot.actions.keys() {
                match self.actions.bound_publication.target(address) {
                    Ok(Some(target)) => {
                        if let Some(route) = target.binding.broker_route() {
                            let entry = aliases
                                .entry(Some(target.binding.lifetime_id))
                                .or_insert_with(|| (route, HashMap::new()));
                            entry.1.insert(
                                target.address.address.resource.clone(),
                                address.resource.clone(),
                            );
                            entry
                                .1
                                .insert(address.resource.clone(), address.resource.clone());
                        }
                    }
                    Ok(None) => {
                        if let Some(observation) = self
                            .collaboration
                            .manager
                            .primary_route()
                            .or(self.actions.last_published_bound_route.as_ref())
                        {
                            aliases
                                .entry(None)
                                .or_insert_with(|| (observation.route().clone(), HashMap::new()))
                                .1
                                .insert(address.resource.clone(), address.resource.clone());
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "local input publication alias unavailable")
                    }
                }
            }
            for (owner, (route, aliases)) in aliases {
                if self.local_input_capture.reports_nothing.contains(&route) {
                    reports_nothing = true;
                    continue;
                }
                let queue = self
                    .local_input_capture
                    .queues
                    .entry(route.clone())
                    .or_default()
                    .clone();
                routes.push(InputRoute {
                    ownership: self.collaboration.manager.connection_snapshot(&route),
                    owner,
                    queue,
                    revision: if pin.is_some() {
                        revision.clone()
                    } else {
                        String::new()
                    },
                    revision_pending: pin.is_none() && revision_pending,
                    aliases: Rc::new(aliases),
                    pin: pin.clone().map(|pin| pin as Rc<dyn std::any::Any>),
                });
            }
        }
        if routes.is_empty() && reports_nothing {
            // The connection can carry no report: the edit is the person's,
            // saved here, and the history will not show it.
            drop(background);
            return crate::local_document_action::enter_unsigned_input(
                self.local_input_capture.unrecorded_edits.clone(),
            );
        }
        if routes.is_empty() {
            // Nothing can carry this input's actions; whether that matters
            // is learnt when it records an edit, as the input drains.
            drop(background);
            return crate::local_document_action::enter_unsigned_input(
                self.local_input_capture.unrouted_edits.clone(),
            );
        }
        // Retain inert snapshot copies, not registered leases. Ordinary online
        // reconciliation can still release all in-flight directory pins.
        self.local_input_capture.offline_routes = routes
            .iter()
            .cloned()
            .map(|mut route| {
                route.pin = pin
                    .as_ref()
                    .map(|pin| Rc::new((**pin).clone()) as Rc<dyn std::any::Any>);
                route
            })
            .collect();
        // Drop the suspension before installing a new input scope so nesting
        // restores the outer host rather than erasing the new scope.
        drop(background);
        crate::local_document_action::enter_input(routes)
    }

    pub(super) fn drain_local_input_capture(&mut self) {
        if self.local_input_capture.unsigned_edits.take() {
            self.note_local_input_unsigned();
        }
        if self.local_input_capture.unrecorded_edits.take() {
            self.local_input_unrecorded();
        }
        if self.local_input_capture.unrouted_edits.take() {
            self.local_input_capture_failed("local input has no published action route");
        }
        let mut failures = Vec::new();
        let now = crate::time::now();
        let queues: Vec<_> = self
            .local_input_capture
            .queues
            .iter()
            .map(|(route, queue)| (route.clone(), queue.clone()))
            .collect();
        for (route, queue) in queues {
            failures.extend(queue.take_refusals());
            for (capture, mut lease) in queue.drain_leased() {
                if let Some(pin) = self.report_pin(&capture) {
                    lease.narrow_pin(pin);
                }
                // Transfer ownership before enqueueing: send failure must not
                // discard an action already applied to the local document.
                self.local_input_capture
                    .hold(route.clone(), capture, lease, now);
            }
        }
        let connected = self.collaboration.manager.is_connected();
        if connected && !self.local_input_capture.transport_connected {
            for pending in self.local_input_capture.pending.values_mut() {
                pending.retry_at = now;
            }
            // A new connection may be one the broker admits reports on.
            self.local_input_capture.reports_nothing.clear();
            let unrecorded = crate::i18n::I18nManager::lookup_plain(UNRECORDED);
            if self.local_input_capture.failure.as_deref() == Some(unrecorded.as_str()) {
                self.local_input_capture.failure = None;
                self.needs_sync = true;
            }
        }
        self.local_input_capture.transport_connected = connected;
        let mut rejected = Vec::new();
        let mut reports_nothing = Vec::new();
        if connected {
            self.fill_pending_revisions(now);
            // Every report due goes out: the host answers each on a later
            // turn, so one held back here waits a turn more while typing
            // fills the queue. Retrying reports is not dispatch: the original
            // UUID, snapshot and mutations are reused, and the document
            // action is never executed again.
            for (request_id, pending) in self
                .local_input_capture
                .pending
                .iter_mut()
                // A change made under a surface that moved waits, quietly,
                // for one it can be reported against.
                .filter(|(_, pending)| {
                    pending.retry_at <= now && !pending.capture.revision.is_empty()
                })
            {
                // A capture without provenance, or made on another
                // connection, is never relabelled with this one's actor.
                if pending.capture.ownership.is_none()
                    || pending.capture.ownership
                        != self
                            .collaboration
                            .manager
                            .connection_snapshot(&pending.route)
                {
                    pending.retry_at = now + crate::time::Duration::from_secs(30);
                    // Never reported on another connection's authority: this
                    // change cannot be saved to the history from here.
                    failures.push(
                        "local action recovery requires its original authenticated connection"
                            .into(),
                    );
                    continue;
                }
                let result = self
                    .collaboration
                    .manager
                    .report_local_document_action_to_route(
                        pending.route.clone(),
                        *request_id,
                        pending.capture.clone(),
                    );
                // The transport owns a four-second RPC deadline. Do not
                // enqueue another report behind an ordinary in-flight call.
                let wait = if result.is_ok() {
                    pending.retry_delay.max(crate::time::Duration::from_secs(5))
                } else {
                    pending.retry_delay
                };
                pending.retry_at = now + wait;
                pending.retry_delay =
                    (pending.retry_delay * 2).min(crate::time::Duration::from_secs(30));
                if let Err(error) = result {
                    if is_connection_without_reports(&error) {
                        reports_nothing.push(*request_id);
                    } else if matches!(error, runtime_collab::LocalDocumentActionError::Rejected(_))
                    {
                        rejected.push(*request_id);
                        failures.push(error.to_string());
                    } else {
                        // Reported again, paced: still on its way.
                        tracing::debug!(%error, "a change is still on its way to the history");
                    }
                }
            }
        }
        for request_id in rejected {
            self.retire_rejected_local_capture(request_id);
        }
        for request_id in reports_nothing {
            self.retire_capture_on_connection_without_reports(request_id);
        }
        // Never retire a route's capacity accounting while replies are in
        // flight, including unknown outcomes that still own snapshot leases.
        self.local_input_capture.queues.retain(|route, queue| {
            queue.has_outstanding()
                || self
                    .collaboration
                    .manager
                    .primary_route()
                    .is_some_and(|w| w.route() == route)
                || self
                    .document_routes
                    .owners
                    .iter()
                    .any(|owner| &owner.route == route)
        });
        for error in failures {
            self.local_input_capture_failed(&error);
        }
    }

    /// Give each change made under a surface that moved under its input the
    /// revision of the surface published since, pinned until its outcome is
    /// known, so it is reported against that one. A change whose action that
    /// surface no longer offers is refused by the broker, and the person is
    /// told then; until a surface can be pinned, nothing is said.
    fn fill_pending_revisions(&mut self, now: crate::time::Instant) {
        if !self
            .local_input_capture
            .pending
            .values()
            .any(|pending| pending.capture.revision.is_empty())
        {
            return;
        }
        let background = crate::local_document_action::suspend();
        self.publish_action_surface();
        drop(background);
        let Some(revision) = self
            .actions
            .action_directory_stamp
            .as_ref()
            .map(|(_, revision)| revision.clone())
        else {
            return;
        };
        let Ok(pin) = self.try_pin_local_document_action_directory(&revision) else {
            return;
        };
        let waiting: Vec<u64> = self
            .local_input_capture
            .pending
            .iter()
            .filter(|(_, pending)| pending.capture.revision.is_empty())
            .map(|(request_id, _)| *request_id)
            .collect();
        for request_id in waiting {
            let Some(pending) = self.local_input_capture.pending.get_mut(&request_id) else {
                continue;
            };
            if let Err(error) = pending.capture.report_against(&revision) {
                // It can never be reported: drop its ownership.
                self.local_input_capture.pending.remove(&request_id);
                self.local_input_capture_failed(&error);
                continue;
            }
            pending._pin = Some(Rc::clone(&pin));
            // Reported in this same pump.
            pending.retry_at = now;
        }
    }

    pub(super) fn local_input_capture_deadline(&self) -> Option<crate::time::Instant> {
        self.collaboration
            .manager
            .is_connected()
            .then(|| {
                self.local_input_capture
                    .pending
                    .values()
                    .map(|pending| pending.retry_at)
                    .min()
            })
            .flatten()
    }

    /// The broker admits no report on this capture's connection, and never
    /// will: its edit is kept, unreported, as one no publication observed,
    /// and later edits on the route are not reported either. The person has
    /// nothing to retry, so nothing is said.
    fn retire_capture_on_connection_without_reports(&mut self, request_id: u64) {
        let Some(pending) = self.local_input_capture.pending.remove(&request_id) else {
            return;
        };
        self.local_input_capture
            .reports_nothing
            .insert(pending.route);
        self.local_input_unrecorded();
    }

    /// An edit landed on a connection whose reports the broker refuses: it
    /// is saved here, and the person is told their history will not show
    /// it. Retrying would be refused the same way, so none is offered.
    fn local_input_unrecorded(&mut self) {
        tracing::warn!("an edit was saved that this window's connection cannot record in history");
        if self.local_input_capture.failure.is_none() {
            self.local_input_capture.failure =
                Some(crate::i18n::I18nManager::lookup_plain(UNRECORDED));
            self.needs_sync = true;
        }
    }

    fn retire_rejected_local_capture(&mut self, request_id: u64) {
        if let Some(pending) = self.local_input_capture.pending.remove(&request_id) {
            if let Some(published) = pending.capture.published.as_ref() {
                tracing::warn!(
                    resource = %published.resource,
                    action = %published.action,
                    "the broker refused a change's report"
                );
            }
            // The person may retry it ([`Self::retry_unsaved_changes`]).
            let mut capture = pending.capture;
            capture.revision.clear();
            let refused = &mut self.local_input_capture.refused;
            if refused.len() >= 128 {
                refused.remove(0);
                tracing::warn!(
                    "the oldest change the broker refused is no longer offered to retry"
                );
            }
            refused.push((pending.route, capture));
        }
    }

    /// A change definitively could not be saved to the person's history:
    /// say so in plain words. A change still on its way says nothing.
    pub(super) fn local_input_capture_failed(&mut self, error: &str) {
        tracing::warn!(%error, "a change could not be saved to the history");
        self.local_input_capture.failure = Some(crate::i18n::I18nManager::lookup_plain(
            "history-local-action-not-saved",
        ));
        self.needs_sync = true;
    }

    /// The person's Retry, while changes the broker refused wait for it: a
    /// resource of the window's own, reached by the History bar's status,
    /// the palette, menus and assistive technology alike.
    pub(super) fn unsaved_changes_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        if !self.has_unsaved_changes_to_retry() {
            return Vec::new();
        }
        let mut resource = tilo_actions::ResourceSpec::new(
            UNSAVED_RESOURCE,
            UNSAVED_RESOURCE,
            crate::i18n::I18nManager::lookup_plain("history-local-action-not-saved"),
        );
        match tilo_actions::ActionSpec::operation(
            "retry",
            crate::i18n::I18nManager::lookup_plain("history-local-action-retry"),
            tilo_actions::OperationKind::Effect,
            UNSAVED_OPERATION,
            &(),
        ) {
            Ok(action) => resource.actions.push(action),
            Err(error) => tracing::warn!(%error, "no Retry for unsaved changes"),
        }
        vec![resource]
    }

    /// What the window's header says about its changes: the sync status, and
    /// Retry beside it while it says changes could not be saved and the
    /// person can retry them.
    pub(super) fn window_status(&self) -> Option<crate::widget::WindowStatus> {
        let status = crate::widget::WindowStatus::says(self.document_sync_status()?);
        if self.local_input_capture.failure.is_none() {
            return Some(status);
        }
        let retry = self
            .unsaved_changes_resources()
            .first()
            .and_then(|resource| tilo_actions::ActionBinding::unit_for(resource, "retry").ok());
        Some(match retry {
            Some(binding) => status.with_action(crate::widget::StatusAction {
                label: crate::i18n::I18nManager::lookup_plain("history-local-action-retry"),
                binding,
            }),
            None => status,
        })
    }

    /// Whether the person can retry saving changes the broker refused.
    pub(super) fn has_unsaved_changes_to_retry(&self) -> bool {
        !self.local_input_capture.refused.is_empty()
    }

    /// The person's Retry: report the refused changes again, each against
    /// the surface published now, and stop saying they were not saved. A
    /// change the broker refuses again says so again.
    ///
    /// # Errors
    ///
    /// A change whose report no longer fits the pending budget.
    pub(super) fn retry_unsaved_changes(&mut self) -> Result<(), String> {
        let refused = std::mem::take(&mut self.local_input_capture.refused);
        self.local_input_capture.failure = None;
        self.needs_sync = true;
        // Each is reported once it has the surface it is reported against
        // (`fill_pending_revisions`).
        for (route, capture) in refused {
            let history = &mut self.local_input_capture;
            let lease = history
                .queues
                .entry(route.clone())
                .or_default()
                .restore_lease(&capture, None)?;
            history.hold(route, capture, lease, crate::time::now());
        }
        Ok(())
    }

    pub(super) fn observe_local_input_capture(
        &mut self,
        request_id: u64,
        result: Result<
            runtime_collab::PublishedInvocationObservation,
            runtime_collab::LocalDocumentActionError,
        >,
    ) {
        let Some(pending) = self.local_input_capture.pending.get(&request_id) else {
            return;
        };
        let invocation = pending.capture.invocation;
        use runtime_collab::LocalDocumentActionError::{Rejected, Uncertain};
        let result = result.and_then(|observation| {
            if observation.invocation != *invocation.as_bytes() {
                return Err(Uncertain(
                    "local action observation invocation mismatch".into(),
                ));
            }
            let result = observation
                .result
                .ok_or_else(|| Uncertain("local action observation has no outcome".into()))?;
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(&result.update_json)
                .map_err(|error| Uncertain(error.to_string()))?;
            if update.invocation().0 != invocation {
                return Err(Uncertain("local action update invocation mismatch".into()));
            }
            if result.uncertain {
                return Err(Uncertain("local action admission was not confirmed".into()));
            }
            // A known terminal refusal need not carry an admission. Release
            // ownership before the next input needs the frozen directory's pin.
            if matches!(
                update,
                tilo_actions::InvocationUpdate::Failed { .. }
                    | tilo_actions::InvocationUpdate::Cancelled { .. }
            ) {
                return Err(Rejected("local action observation was refused".into()));
            }
            // Admission is private status only, never completion or a signed
            // document-publication permit.
            result
                .admission_json
                .ok_or_else(|| Uncertain("local action admission was not confirmed".into()))
        });
        match result {
            // The host's journal holds the report from here: nothing of it
            // stays with the window.
            Ok(_admission) => {
                self.local_input_capture.pending.remove(&request_id);
            }
            Err(error @ Rejected(_)) if is_connection_without_reports(&error) => {
                self.retire_capture_on_connection_without_reports(request_id);
            }
            Err(Rejected(error)) => {
                self.retire_rejected_local_capture(request_id);
                self.local_input_capture_failed(&error);
            }
            // Unknown or malformed replies keep ownership until a bounded,
            // backoff-paced exact-UUID report reconciles their outcome.
            Err(Uncertain(error)) => {
                tracing::debug!(%error, "a change is still on its way to the history");
            }
        }
    }
}

/// Whether the broker refused a report because its connection admits none.
fn is_connection_without_reports(error: &runtime_collab::LocalDocumentActionError) -> bool {
    matches!(
        error,
        runtime_collab::LocalDocumentActionError::Rejected(reason)
            if reason == runtime_collab::LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport, RouteSpec};
    use std::cell::RefCell;

    struct Peer(Rc<RefCell<Vec<CollabRequest>>>);
    impl PeerTransport for Peer {
        fn connection_snapshot(
            &self,
            route: &RouteSpec,
        ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
            Some(ownership(route))
        }
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }
        fn send_to_route(&mut self, _: RouteSpec, request: CollabRequest) -> Result<(), String> {
            self.0.borrow_mut().push(request);
            Ok(())
        }
    }
    fn ownership(route: &RouteSpec) -> runtime_collab::committed_document::ConnectionSnapshot {
        runtime_collab::committed_document::ConnectionSnapshot {
            root: [1; 32],
            device: [2; 32],
            agent: None,
            app_id: route.app_id.clone(),
            workspace: route.cluster_id.to_string(),
            document: route.document.clone().unwrap_or_default(),
        }
    }
    fn input(queue: &Queue, route: &RouteSpec) -> crate::local_document_action::Scope {
        crate::local_document_action::enter_input(vec![InputRoute {
            ownership: Some(ownership(route)),
            owner: None,
            queue: queue.clone(),
            revision: "snapshot".into(),
            revision_pending: false,
            aliases: Rc::new([("body".into(), "body".into())].into()),
            pin: None,
        }])
    }
    #[derive(Debug, serde::Serialize)]
    struct Noop;
    impl tilo_actions::Action for Noop {
        fn apply(&self, _: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            Ok(())
        }
        fn locus(&self) -> tilo_actions::ExecutionLocus {
            tilo_actions::ExecutionLocus::ClientLocal
        }
    }

    #[derive(Debug, serde::Serialize)]
    struct Put(i64);
    impl tilo_actions::Action for Put {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, "edited", self.0)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    struct CommitThenRefuse;
    impl tilo_actions::Action for CommitThenRefuse {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, "committed", "survives")?;
            doc.commit();
            Err(tilo_actions::ActionError::Refused(
                "fixture refusal after commit".into(),
            ))
        }
    }

    #[derive(Debug, serde::Serialize)]
    struct WriteThenRefuse;
    impl tilo_actions::Action for WriteThenRefuse {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, "pending", "discarded")?;
            Err(tilo_actions::ActionError::Refused(
                "fixture refusal before commit".into(),
            ))
        }
    }

    tilo_test_env::scenario! {
        fn local_input_capture_signs_only_what_it_recorded;

        phase only_a_recorded_edit_makes_unmanaged_history_unsigned;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let mut doc = automerge::AutoCommit::new();
            let mut host = crate::local_dispatch::LocalDispatch::new();
            // Input that edits nothing, even one running a no-op action.
            for _ in 0..3 {
                let input = app.begin_local_input_capture();
                host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
                drop(input);
                app.needs_sync = false;
                app.drain_local_input_capture();
                assert!(app.local_input_capture.failure.is_none());
                assert!(app.document_sync_status().is_none());
                assert!(
                    !app.needs_sync,
                    "input that edits nothing asks for no frame"
                );
            }
            // An edit outside input is not this person's unsigned history.
            host.try_dispatch(&mut doc, "put", &Put(1)).unwrap();
            app.drain_local_input_capture();
            assert!(app.local_input_capture.failure.is_none());
            let input = app.begin_local_input_capture();
            host.try_dispatch(&mut doc, "put", &Put(2)).unwrap();
            drop(input);
            app.drain_local_input_capture();
            assert_eq!(
                app.document_sync_status(),
                None,
                "an edit no action reports is the person's, saved here: nothing to say"
            );
        }

        phase a_refused_action_that_committed_an_edit_is_unsigned_history;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let mut doc = automerge::AutoCommit::new();
            let mut host = crate::local_dispatch::LocalDispatch::new();
            let input = app.begin_local_input_capture();
            assert!(
                host.try_dispatch(&mut doc, "commit-then-refuse", &CommitThenRefuse)
                    .is_err()
            );
            drop(input);
            assert!(
                automerge::ReadDoc::get(&doc, &automerge::ROOT, "committed")
                    .unwrap()
                    .is_some(),
                "the action's committed edit survives its refusal"
            );
            app.drain_local_input_capture();
            assert_eq!(
                app.document_sync_status(),
                None,
                "a surviving unmanaged edit is the person's, saved here: nothing to say"
            );
        }

        phase a_refused_action_without_a_commit_leaves_history_silent;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let mut doc = automerge::AutoCommit::new();
            let mut host = crate::local_dispatch::LocalDispatch::new();
            let input = app.begin_local_input_capture();
            assert!(
                host.try_dispatch(&mut doc, "write-then-refuse", &WriteThenRefuse)
                    .is_err()
            );
            drop(input);
            assert!(
                automerge::ReadDoc::get(&doc, &automerge::ROOT, "pending")
                    .unwrap()
                    .is_none(),
                "the executor rolled back the uncommitted write"
            );
            app.needs_sync = false;
            app.drain_local_input_capture();
            assert!(app.local_input_capture.failure.is_none());
            assert!(app.document_sync_status().is_none());
            assert!(
                !app.needs_sync,
                "a refused action that left no edit asks for no frame"
            );
        }

        phase legacy_capture_without_ownership_is_not_relabelled_by_a_live_connection;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let requests = Rc::new(RefCell::new(Vec::new()));
            app.collaboration.manager =
                crate::collaboration::CollaborationManager::with_peer_transport(Peer(requests.clone()));
            let route = RouteSpec::app("notes", uuid::Uuid::new_v4());
            let queue = Queue::new();
            app.local_input_capture.queues.insert(route, queue.clone());
            let mut doc = automerge::AutoCommit::new();
            {
                // The compatibility queue has no authenticated host provenance.
                let _scope = queue.enter("snapshot".into());
                let _invocation = crate::local_document_action::with_invocation(
                    crate::local_document_action::PublishedInvocation {
                        resource: "body".into(),
                        action: "read".into(),
                        arguments: serde_json::Map::new(),
                    },
                );
                crate::local_dispatch::LocalDispatch::new()
                    .try_dispatch(&mut doc, "noop", &Noop)
                    .unwrap();
            }
            app.drain_local_input_capture();
            assert!(requests.borrow().is_empty());
            assert_eq!(app.local_input_capture.pending_count(), 1);
            assert!(queue.has_outstanding());
            assert!(app.local_input_capture.failure.is_some());
        }

        phase retired_route_is_released_after_uncertain_capture_is_reconciled;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let requests = Rc::new(RefCell::new(Vec::new()));
            app.collaboration.manager =
                crate::collaboration::CollaborationManager::with_peer_transport(Peer(requests.clone()));
            let queue = Queue::new();
            let route = RouteSpec::app("retired", uuid::Uuid::new_v4());
            app.local_input_capture
                .queues
                .insert(route.clone(), queue.clone());
            let mut doc = automerge::AutoCommit::new();
            let mut host = crate::local_dispatch::LocalDispatch::new();
            {
                let _scope = input(&queue, &route);
                let _invocation = crate::local_document_action::with_invocation(
                    crate::local_document_action::PublishedInvocation {
                        resource: "body".into(),
                        action: "read".into(),
                        arguments: serde_json::Map::new(),
                    },
                );
                host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
            }
            app.drain_local_input_capture();
            let request_id = requests
                .borrow()
                .iter()
                .find_map(|request| match request {
                    CollabRequest::ReportLocalDocumentAction { request_id, .. } => Some(*request_id),
                    _ => None,
                })
                .unwrap();
            app.observe_local_input_capture(
                request_id,
                Err(runtime_collab::LocalDocumentActionError::Uncertain(
                    "reply lost".into(),
                )),
            );
            app.drain_local_input_capture();
            assert!(queue.has_outstanding());
            assert_eq!(app.local_input_capture.route_count(), 1);
            app.observe_local_input_capture(
                request_id,
                Err(runtime_collab::LocalDocumentActionError::Rejected(
                    "worker retired".into(),
                )),
            );
            app.drain_local_input_capture();
            assert!(!queue.has_outstanding());
            assert_eq!(app.local_input_capture.route_count(), 0);
            assert_eq!(app.local_input_capture.pending_count(), 0);
        }

        phase noops_are_submitted_and_authenticated_admissions_settle_them;
        {
            let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
            let requests = Rc::new(RefCell::new(Vec::new()));
            app.collaboration.manager =
                crate::collaboration::CollaborationManager::with_peer_transport(Peer(requests.clone()));
            let route = RouteSpec::app("test", uuid::Uuid::new_v4());
            let queue = Queue::new();
            app.local_input_capture
                .queues
                .insert(route.clone(), queue.clone());
            let mut doc = automerge::AutoCommit::new();
            let mut host = crate::local_dispatch::LocalDispatch::new();
            for _ in 0..130 {
                {
                    let _scope = input(&queue, &route);
                    let _invocation = crate::local_document_action::with_invocation(
                        crate::local_document_action::PublishedInvocation {
                            resource: "body".into(),
                            action: "read".into(),
                            arguments: serde_json::Map::new(),
                        },
                    );
                    host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
                }
                app.drain_local_input_capture();
                let (request_id, invocation) = requests
                    .borrow()
                    .iter()
                    .rev()
                    .find_map(|request| match request {
                        CollabRequest::ReportLocalDocumentAction { request_id, report } => {
                            assert!(report.mutation_hashes.is_empty());
                            Some((*request_id, report.invocation))
                        }
                        _ => None,
                    })
                    .expect("read/no-op must be submitted");
                app.observe_local_input_capture(
                    request_id,
                    Ok(runtime_collab::PublishedInvocationObservation {
                        host: [1; 16],
                        invocation: *invocation.as_bytes(),
                        result: Some(runtime_collab::PublishedInvocationResult {
                            id: request_id,
                            update_json: serde_json::to_string(
                                &tilo_actions::InvocationUpdate::Accepted {
                                    invocation: tilo_actions::InvocationId(invocation),
                                },
                            )
                            .unwrap(),
                            admission_json: Some("{\"admitted\":true}".into()),
                            terminal_json: None,
                            uncertain: false,
                            cancellation_requested: false,
                        }),
                    }),
                );
                assert!(!queue.has_outstanding());
            }
            assert_eq!(app.local_input_capture.pending_count(), 0);
        }
    }
}
