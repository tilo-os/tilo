//! Native-host adapter. The persistence/decision path is independent of the
//! OS event loop; Windows and macOS use this same admission boundary.
use super::App;
#[cfg(feature = "model")]
use super::{BrokerReply, ResourceInvocation};
use crate::close_admission::{Choice, Command, OPERATION};
use crate::widget::Widget;

// Compile the actual native Quit routing state in portable host tests. The
// AppKit delegate itself still requires macOS qualification.
#[cfg(test)]
#[path = "../../../../../vendor/winit/src/platform_impl/quit_request.rs"]
mod native_quit_request;

#[cfg(feature = "model")]
/// Why the workspace did not take what a window published, in words a person reads.
fn barrier_failure(error: &crate::collaboration::DocumentPublicationError) -> String {
    use crate::collaboration::DocumentPublicationError;
    match error {
        DocumentPublicationError::Refused(reason) | DocumentPublicationError::Failed(reason) => {
            reason.clone()
        }
        DocumentPublicationError::DeadlineElapsed
        | DocumentPublicationError::Unsupported
        | DocumentPublicationError::Unavailable
        | DocumentPublicationError::WorkerStopped => {
            crate::i18n::I18nManager::lookup_plain("close-broker-disconnected")
        }
    }
}

impl App {
    #[cfg(any(test, feature = "native"))]
    pub(super) fn admit_native_close_request(&mut self) -> bool {
        // Native hosts may deliver another window-close or application Quit
        // while a decision is already visible. Coalesce it at the native
        // boundary so explicit API retries can still re-check repaired owners.
        if self.close_admission.dialog.is_some() {
            return false;
        }
        // The window's loop never waits on the disk: the save only starts the store's commits,
        // and later passes hear of them.
        let _started_only = crate::storage::waiting::forbid_on_this_thread();
        self.request_close() && self.admit_close()
    }

    fn invalidate_close_presentation(&mut self) {
        #[cfg(feature = "model")]
        {
            self.actions.action_palette = None;
        }
        self.bindings.clear();
        self.layout.node_map.clear();
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
    }
    /// Refusal keeps every live owner and wake scope available for editing.
    pub fn request_close(&mut self) -> bool {
        if self.close_admission.admitted {
            return true;
        }
        // A window keeps nothing a workspace lacks without the model: it goes at once.
        #[cfg(feature = "model")]
        let ready = self.admit_workspace_close();
        #[cfg(not(feature = "model"))]
        let ready = true;
        ready
    }

    /// [`Self::request_close`] for a window whose records a workspace keeps: it goes once every
    /// change it made is kept, and a refusal keeps it open with its edits.
    #[cfg(feature = "model")]
    fn admit_workspace_close(&mut self) -> bool {
        // Native hosts may deliver another window-close or application Quit
        // while a decision is already visible. Coalesce it here so it cannot
        // replace the session, semantic addresses, focus, or failure detail.
        if self.close_admission.dialog.is_some() || self.close_admission.save_waiting {
            return false;
        }
        let pending = crate::component_lifecycle::unsaved_documents(
            self.root_widget.as_ref(),
            &self.state.doc,
        );
        let remaining: Vec<_> = pending
            .into_iter()
            .filter(|document| !self.close_admission.discarded.contains(document))
            .collect();
        if !remaining.is_empty() {
            self.close_admission.reviewed = remaining;
            self.close_admission.show(
                "close-title",
                "close-unsaved",
                &[Choice::Save, Choice::Discard, Choice::KeepEditing],
                self.input.platform,
            );
            self.invalidate_close_presentation();
            return false;
        }
        // The window takes no more invocations and answers the ones it was handed before it
        // saves: nothing reaches it once it starts to go, and the save holds what they changed.
        if !self.left_invocations() {
            return false;
        }
        if self.close_admission.ready {
            return true;
        }
        // The components commit what they hold, once per close: a write a store refuses keeps
        // the window and its edits, with Retry.
        if !self.close_admission.checkpointed {
            let errors = {
                #[cfg(feature = "native")]
                let _save = super::slow_event::phase("save");
                self.checkpoint_before_close()
            };
            if !errors.is_empty() {
                self.show_save_failed(&errors);
                return false;
            }
            self.close_admission.checkpointed = true;
            // What they committed, and the answers sent as the window let go of its
            // invocations, go out before a barrier the close waits on; what the workspace
            // refused before is sent again.
            self.close_admission.barrier.wanted = true;
            self.resend_refused(true);
        }
        // The window goes once the workspace took every change of it. One it did not take
        // keeps the window and its edits, and says so.
        match self.kept_before_close() {
            crate::close_admission::Keeping::Waiting => return false,
            crate::close_admission::Keeping::NotKept(reason) => {
                self.show_not_kept(&reason);
                return false;
            }
            crate::close_admission::Keeping::Kept => {}
        }
        self.close_admission.dialog = None;
        self.invalidate_close_presentation();
        self.close_admission.ready = true;
        true
    }

    fn show_save_failed(&mut self, errors: &[String]) {
        self.close_admission.show(
            "close-save-failed",
            &errors.join("\n"),
            &[Choice::Retry, Choice::KeepEditing],
            self.input.platform,
        );
        self.invalidate_close_presentation();
    }

    /// The workspace did not take a change of the window: closing now would lose it. The person
    /// tries again, keeps the window, or closes it without the change.
    fn show_not_kept(&mut self, reason: &str) {
        self.close_admission.show(
            "close-save-failed",
            reason,
            &[Choice::Retry, Choice::Discard, Choice::KeepEditing],
            self.input.platform,
        );
        self.close_admission.not_kept = true;
        self.invalidate_close_presentation();
    }

    #[cfg(feature = "model")]
    /// Whether the workspace keeps every change of the window's records, for a close: what the
    /// window published since it checkpointed is answered by a barrier first. A window with
    /// nothing the workspace lacks goes at once, connected or not.
    fn kept_before_close(&mut self) -> crate::close_admission::Keeping {
        use crate::close_admission::Keeping;
        use crate::resource::ResourceSaveStatus;
        if self.close_admission.discard_unkept {
            return Keeping::Kept;
        }
        self.publish_owned_documents();
        if let Err(reason) = self.watch_barrier() {
            return Keeping::NotKept(crate::i18n::I18nManager::format(
                "close-not-kept",
                &[("reason", reason.into())],
            ));
        }
        if self.close_admission.barrier.pending.is_some() {
            return Keeping::Waiting;
        }
        // What the components' checkpoint wrote themselves: a view, a record of a store that
        // writes for itself.
        let settling = crate::component_lifecycle::close_settling(self.root_widget.as_mut());
        self.close_admission.settling = settling.waiting && settling.refused.is_empty();
        if !settling.refused.is_empty() {
            return Keeping::NotKept(crate::i18n::I18nManager::format(
                "close-not-kept",
                &[("reason", settling.refused.join("\n").into())],
            ));
        }
        if settling.waiting {
            return Keeping::Waiting;
        }
        let manager = &self.collaboration.manager;
        let mut unkept = false;
        for entry in self
            .document_routes
            .owners
            .iter()
            .filter(|entry| entry.publishes)
        {
            let Some(owner) = entry.owners.first() else {
                continue;
            };
            let (store, checkout) = owner.store_checkout();
            match store.save_status(&checkout.id) {
                Ok(ResourceSaveStatus::Failed(reason)) => {
                    return Keeping::NotKept(crate::i18n::I18nManager::format(
                        "close-not-kept",
                        &[("reason", reason.into())],
                    ));
                }
                // A route still joining opens, and its changes go out then.
                Ok(ResourceSaveStatus::Saving) if manager.document_route_joining(&entry.route) => {
                    return Keeping::Waiting;
                }
                Ok(ResourceSaveStatus::Saving) => unkept = true,
                Ok(ResourceSaveStatus::Saved | ResourceSaveStatus::MemoryOnly) | Err(_) => {}
            }
        }
        if !unkept {
            return Keeping::Kept;
        }
        Keeping::NotKept(crate::i18n::I18nManager::lookup_plain(
            if manager.is_connected() {
                "close-not-published"
            } else {
                "close-not-connected"
            },
        ))
    }

    #[cfg(feature = "model")]
    /// Send again, whole, each record the person asked to try again
    /// ([`crate::resource::ResourceStore::take_resend`]), and with `refused` every record the
    /// workspace refused: the next pump publishes it, and the barrier after it settles it.
    pub(super) fn resend_refused(&mut self, refused: bool) {
        let barrier = &mut self.close_admission.barrier;
        for entry in self
            .document_routes
            .owners
            .iter_mut()
            .filter(|entry| entry.publishes)
        {
            let asked = entry.owners.first().is_some_and(|owner| {
                let (store, checkout) = owner.store_checkout();
                store.take_resend(&checkout.id)
            });
            if !(asked || (refused && barrier.refused.contains_key(&entry.route))) {
                continue;
            }
            barrier.refused.remove(&entry.route);
            barrier.confirmed.remove(&entry.route);
            entry.sent = crate::state::DocumentFrontier::default();
            entry.needs_snapshot = true;
            barrier.wanted = true;
        }
    }

    #[cfg(feature = "model")]
    /// Take the workspace's answer to the window's barrier, when it came, and ask for
    /// another once a record's route sent changes the workspace has not confirmed, so the save
    /// indicator says Saved only for what the workspace took ([`crate::close_admission::Barrier`]).
    /// Its answer wakes the window from the connection's thread.
    ///
    /// # Errors
    /// The workspace did not take what the window published, said in plain words; each record
    /// the barrier covered shows it failed.
    pub(super) fn watch_barrier(&mut self) -> Result<(), String> {
        use crate::collaboration::DocumentPublicationError;
        let barrier = &mut self.close_admission.barrier;
        if let Some((receipt, _)) = &barrier.pending {
            let answer = match receipt.try_complete() {
                Ok(None) => return Ok(()),
                Ok(Some(())) => Ok(()),
                Err(error) => Err(error),
            };
            let Some((_, records)) = barrier.pending.take() else {
                return Ok(());
            };
            match answer {
                Ok(()) => {
                    for record in records {
                        record.store.note_published(&record.id, &record.heads);
                        barrier.confirmed.insert(record.route, record.heads);
                    }
                }
                Err(error) => {
                    let reason = barrier_failure(&error);
                    for record in records {
                        record.store.note_unpublished(&record.id, reason.clone());
                        barrier.refused.insert(record.route, record.heads);
                    }
                    return Err(reason);
                }
            }
        }
        let live: Vec<&crate::collaboration::RouteSpec> = self
            .document_routes
            .owners
            .iter()
            .map(|entry| &entry.route)
            .collect();
        barrier.confirmed.retain(|route, _| live.contains(&route));
        barrier.refused.retain(|route, _| live.contains(&route));
        let same = |known: Option<&Vec<automerge::ChangeHash>>, heads: &[automerge::ChangeHash]| {
            known.is_some_and(|known| {
                known.len() == heads.len() && heads.iter().all(|head| known.contains(head))
            })
        };
        let records: Vec<crate::close_admission::Confirming> = self
            .document_routes
            .owners
            .iter()
            .filter(|entry| entry.publishes && !entry.opening)
            .filter_map(|entry| {
                let heads = entry.sent.heads();
                if heads.is_empty()
                    || same(barrier.confirmed.get(&entry.route), heads)
                    || same(barrier.refused.get(&entry.route), heads)
                {
                    return None;
                }
                let (store, checkout) = entry.owners.first()?.store_checkout();
                Some(crate::close_admission::Confirming {
                    store: store.clone(),
                    id: checkout.id.clone(),
                    route: entry.route.clone(),
                    heads: heads.to_vec(),
                })
            })
            .collect();
        if (records.is_empty() && !barrier.wanted) || !self.collaboration.manager.is_connected() {
            return Ok(());
        }
        barrier.wanted = false;
        match self
            .collaboration
            .manager
            .begin_document_publication_barrier()
        {
            Ok(receipt) => {
                barrier.pending = Some((receipt, records));
                Ok(())
            }
            // A transport with no way to say when it published (one in this process, a test's)
            // took the changes into its queue, which is all it knows of them.
            Err(DocumentPublicationError::Unsupported) => {
                for record in records {
                    record.store.note_published(&record.id, &record.heads);
                    barrier.confirmed.insert(record.route, record.heads);
                }
                Ok(())
            }
            Err(error) => {
                let reason = barrier_failure(&error);
                for record in records {
                    record.store.note_unpublished(&record.id, reason.clone());
                    barrier.refused.insert(record.route, record.heads);
                }
                Err(reason)
            }
        }
    }

    #[cfg(feature = "model")]
    /// Whether the window has let go of the invocations the broker hands it, so it may close:
    /// it takes no more and it answered every one it was handed; the close then waits until the
    /// answers reached the broker ([`Self::kept_before_close`]). One left unanswered as the window
    /// goes has an outcome nobody knows, and the
    /// broker never starts it again, so whoever asked is told it may or may not have happened.
    /// Each pass the close waits on it takes it a step further ([`Self::service_close_admission`]).
    fn left_invocations(&mut self) -> bool {
        use crate::close_admission::Leaving;
        // The window left takes no more of what the broker hands out, though it answers what
        // it was handed: the process goes on serving its other windows, and once none takes
        // them it lets go of its link, so work that comes from now on goes to a process of
        // its own rather than to this one, which is on its way out.
        if matches!(self.close_admission.leaving, Leaving::Staying) {
            crate::window_link::window_stops_taking_invocations();
        }
        loop {
            let manager = &mut self.collaboration.manager;
            // Without a connection nothing more reaches this window, nor leaves it.
            if !manager.is_connected() {
                self.close_admission.leaving = Leaving::Left;
            }
            match &self.close_admission.leaving {
                Leaving::Staying => {
                    self.close_admission.leaving = match manager.stop_invocations() {
                        Ok(()) => Leaving::Stopping,
                        Err(_) => Leaving::Left,
                    };
                }
                Leaving::Stopping => {
                    // One it was handed may not be done yet: a job it accepted, or an effect
                    // whose edits the broker still admits. The window waits on the passes that
                    // finish it, and its answer goes with the others.
                    if !manager.invocations_stopped()
                        || !self.actions.acting_for.is_empty()
                        || !self.actions.directed_invocations.is_empty()
                    {
                        return false;
                    }
                    // Every invocation it was handed ran: their answers go now, as they would
                    // as the window closed, and the barrier the close waits on covers them.
                    self.send_held_answers(true);
                    self.close_admission.leaving = Leaving::Left;
                }
                Leaving::Left => return true,
            }
        }
    }

    #[cfg(feature = "native")]
    pub(super) fn finish_close_if_ready(&mut self) -> bool {
        // Recheck at the actual event-loop exit boundary: a batch since may have left a document
        // unsaved.
        let _started_only = crate::storage::waiting::forbid_on_this_thread();
        self.close_admission.ready && self.request_close() && self.admit_close()
    }

    /// Close a window [`Self::request_close`] just let go.
    #[cfg(any(test, feature = "native"))]
    fn admit_close(&mut self) -> bool {
        #[cfg(feature = "native")]
        let _close = super::slow_event::phase("close");
        self.close_admission.admitted = true;
        // No document mutation/event may run between checkpoint and finalization.
        // Existing teardown may cancel jobs through its immutable-document poll.
        #[cfg(feature = "model")]
        {
            self.component_close_reports
                .extend(crate::component_lifecycle::finish_admitted_close(
                    self.root_widget.as_mut(),
                ));
        }
        self.close_wake_scope();
        true
    }

    #[cfg(feature = "model")]
    pub(super) fn checkpoint_before_close(&mut self) -> Vec<String> {
        let _entry = self.enter_wake_scope();
        // Sent, not waited for: a closing window asks each pass whether the workspace answered
        // ([`Self::kept_before_close`]).
        let mut errors =
            crate::component_lifecycle::checkpoint_components(self.root_widget.as_mut());
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
        errors.extend(crate::close_admission::retry_reports(
            &mut self.component_close_reports,
        ));
        errors
    }

    #[cfg(feature = "model")]
    pub(super) fn invoke_close_encoded(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        if encoded.type_id != OPERATION {
            return Err("unknown close command".into());
        }
        let command: Command =
            serde_json::from_slice(&encoded.payload).map_err(|e| e.to_string())?;
        if !self.close_admission.validate(&command) {
            return Err("close decision is stale or unavailable".into());
        }
        match command.choice {
            Choice::KeepEditing => self.keep_editing(),
            Choice::Retry => {
                self.close_admission.retire_presentation();
                self.request_close();
            }
            // Closing without the changes the workspace did not take: they are lost, as the
            // person chose.
            Choice::Discard if self.close_admission.not_kept => {
                self.close_admission.discard_unkept = true;
                self.close_admission.retire_presentation();
                self.request_close();
            }
            Choice::Discard => {
                let current = crate::component_lifecycle::unsaved_documents(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                );
                if current != self.close_admission.reviewed {
                    self.close_admission.discarded.clear();
                    self.close_admission.retire_presentation();
                    self.request_close();
                    return Err("document changed since the close question was shown".into());
                }
                self.close_admission.discarded = current;
                self.close_admission.retire_presentation();
                self.request_close();
            }
            Choice::Save => self.start_close_save()?,
        }
        self.invalidate_close_presentation();
        Ok(())
    }

    #[cfg(feature = "model")]
    /// The person keeps editing: the close this window had begun is called off. It takes
    /// invocations again, and a save of its own that later succeeds closes nothing.
    fn keep_editing(&mut self) {
        use crate::close_admission::Leaving;
        self.close_admission.dismiss();
        if matches!(self.close_admission.leaving, Leaving::Staying) {
            return;
        }
        self.close_admission.leaving = Leaving::Staying;
        if let Err(error) = self.collaboration.manager.resume_invocations() {
            tracing::warn!(%error, "the window could not take invocations again");
        }
    }

    #[cfg(feature = "model")]
    fn start_close_save(&mut self) -> Result<(), String> {
        let current: Vec<_> = crate::component_lifecycle::unsaved_documents(
            self.root_widget.as_ref(),
            &self.state.doc,
        )
        .into_iter()
        .filter(|document| !self.close_admission.discarded.contains(document))
        .collect();
        if current != self.close_admission.reviewed {
            self.close_admission.retire_presentation();
            self.request_close();
            return Err("document changed since the close question was shown".into());
        }
        let Some(document) = self.close_admission.reviewed.first().cloned() else {
            return Err("no unsaved document".into());
        };
        self.close_admission.save_waiting = true;
        self.close_admission.show(
            "close-title",
            "close-saving",
            &[Choice::KeepEditing],
            self.input.platform,
        );
        self.invalidate_close_presentation();
        let before: std::collections::HashSet<_> =
            self.actions.local_invocations.keys().copied().collect();
        let broker_request = self.actions.next_broker_request_id;
        let connected = self.collaboration.manager.is_connected();
        let actor = tilo_actions::Actor::Member(self.local_author.clone());
        // Do not supply an invocation or claim broker admission. This is the
        // ordinary local canonical action API, including permissions/effects.
        let result = self.invoke_resource_action(ResourceInvocation::new(&document.save, &actor));
        match result {
            Ok(crate::widget::ResourceOperationStart::Completed(_)) => {
                self.close_admission.save_result = Some(Ok(()));
            }
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.close_admission.save_local = self
                    .actions
                    .local_invocations
                    .iter()
                    .find(|(id, (address, _))| !before.contains(*id) && *address == document.save)
                    .map(|(id, _)| *id);
                if connected && self.actions.next_broker_request_id != broker_request {
                    self.close_admission.save_broker_request = broker_request;
                }
                if self.close_admission.save_local.is_none()
                    && self.close_admission.save_broker_request.is_none()
                {
                    self.close_admission.save_result = Some(Err(
                        "save admission did not retain an observable invocation".into(),
                    ));
                }
            }
            Err(error) => self.close_admission.save_result = Some(Err(error.to_string())),
        }
        self.service_close_admission();
        Ok(())
    }

    #[cfg(feature = "model")]
    pub(super) fn observe_close_save(&mut self, update: &tilo_actions::InvocationUpdate) {
        if !self.close_admission.save_waiting || !update.is_terminal() {
            return;
        }
        self.close_admission.save_result = Some(match update {
            tilo_actions::InvocationUpdate::Succeeded { .. } => Ok(()),
            tilo_actions::InvocationUpdate::Failed { error, .. } => Err(error.message.clone()),
            tilo_actions::InvocationUpdate::Cancelled { .. } => Err(
                crate::i18n::I18nManager::lookup_plain("close-save-cancelled"),
            ),
            _ => return,
        });
    }

    #[cfg(feature = "model")]
    pub(super) fn close_broker_result(
        &mut self,
        request_id: u64,
        result: &Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) -> bool {
        if self.close_admission.save_broker_request != Some(request_id) {
            return false;
        }
        self.close_admission.save_broker_request = None;
        match result {
            Ok((id, update)) => {
                self.close_admission.save_broker_id = Some(*id);
                self.observe_close_save(update);
                self.close_admission.save_poll_at =
                    crate::time::now() + crate::time::Duration::from_millis(100);
            }
            Err(error) => self.close_admission.save_result = Some(Err(error.clone())),
        }
        self.service_close_admission();
        true
    }

    #[cfg(feature = "model")]
    pub(super) fn service_close_admission(&mut self) {
        let _started_only = crate::storage::waiting::forbid_on_this_thread();
        // The workspace's answer to what the window published, which its records' save status
        // says; a refusal shows on the records it covered.
        if let Err(reason) = self.watch_barrier() {
            tracing::warn!(%reason, "the workspace did not take what the window published");
        }
        // A close that waits on letting go of its invocations, or on its save, goes on once it has.
        if !self.close_admission.ready
            && !self.close_admission.admitted
            && !matches!(
                self.close_admission.leaving,
                crate::close_admission::Leaving::Staying
            )
        {
            self.request_close();
        }
        while let Some(intent) =
            self.close_admission.dialog.as_mut().and_then(|dialog| {
                crate::widget::controls::take_control_invocation(dialog.as_mut())
            })
        {
            let result = self
                .invoke_history_address_local(&intent.address, None)
                .unwrap_or_else(|| Err(tilo_actions::ApiError::Unknown(intent.address.clone())));
            if let Some(dialog) = self.close_admission.dialog.as_mut() {
                crate::widget::controls::on_control_invocation_result(
                    dialog.as_mut(),
                    &intent.address,
                    &result,
                );
            }
        }
        if self.close_admission.save_waiting
            && self.close_admission.save_broker_request.is_some()
            && !self.collaboration.manager.is_connected()
        {
            self.close_admission.save_result = Some(Err(crate::i18n::I18nManager::lookup_plain(
                "close-broker-disconnected",
            )));
        }
        if let Some(result) = self.close_admission.save_result.take() {
            self.close_admission.save_waiting = false;
            self.close_admission.save_local = None;
            self.close_admission.save_broker_request = None;
            self.close_admission.save_broker_id = None;
            match result {
                Ok(()) => {
                    self.close_admission.retire_presentation();
                    self.request_close();
                }
                Err(message) => {
                    self.close_admission.show(
                        "close-save-failed",
                        &message,
                        &[Choice::Retry, Choice::KeepEditing],
                        self.input.platform,
                    );
                    self.invalidate_close_presentation();
                }
            }
        }
        if self.close_admission.save_waiting
            && self.close_admission.save_broker_id.is_some()
            && self.close_admission.save_broker_request.is_none()
            && crate::time::now() >= self.close_admission.save_poll_at
        {
            if !self.collaboration.manager.is_connected() {
                self.close_admission.save_result = Some(Err("save broker disconnected".into()));
                return;
            }
            if let (Ok(request_id), Some(id)) = (
                self.take_broker_request_id(),
                self.close_admission.save_broker_id,
            ) {
                self.close_admission.save_broker_request = Some(request_id);
                self.actions
                    .broker_requests
                    .insert(BrokerReply::Update(id), request_id);
                self.collaboration.manager.send(
                    crate::collaboration::CollabRequest::RemoteAction {
                        message: tilo_actions::remote::RemoteActionMessage::observe(id),
                        press: None,
                    },
                );
            }
        }
    }

    pub(super) fn close_handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
    ) -> bool {
        if self.close_admission.dialog.is_none() {
            return false;
        }
        if self.layout.structure_dirty || self.layout.node_map.is_empty() {
            self.rebuild_structure();
        }
        if let winit::event::WindowEvent::ModifiersChanged(modifiers) = event {
            self.input.modifiers = *modifiers;
        }
        let mut handled = self.handle_screen_reader_key(event);
        if !handled
            && let winit::event::WindowEvent::KeyboardInput { event, .. } = event
            && event.state == winit::event::ElementState::Pressed
        {
            use winit::keyboard::{Key, NamedKey};
            match &event.logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.focus_tab(self.input.modifiers.state().shift_key());
                    handled = true;
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                    if let Some(node) = self.focused_node {
                        let _ = self.perform_node_action(node, &crate::a11y::Action::Activate);
                        handled = true;
                    }
                }
                Key::Named(
                    key @ (NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight),
                ) => {
                    let direction = match key {
                        NamedKey::ArrowUp => super::Direction::Up,
                        NamedKey::ArrowDown => super::Direction::Down,
                        NamedKey::ArrowLeft => super::Direction::Left,
                        _ => super::Direction::Right,
                    };
                    self.navigate_focus(direction);
                    handled = true;
                }
                _ => {}
            }
        }
        if !handled
            && let (Some(dialog), Some(root)) =
                (self.close_admission.dialog.as_mut(), self.layout.root_node)
        {
            let mut focus = None;
            dialog.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: root,
                    taffy: &self.layout.taffy,
                },
            );
            if let Some(focus) = focus {
                self.set_focused_node(Some(focus));
            }
        }
        #[cfg(feature = "model")]
        self.service_close_admission();
        true
    }

    pub(super) fn layout_close_dialog(&mut self, logical: (f64, f64)) {
        let Some(root) = self.layout.root_node else {
            return;
        };
        let _ = crate::widget::layout::solve_layout(
            &mut self.layout.taffy,
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(logical.0 as f32),
                height: taffy::AvailableSpace::Definite(logical.1 as f32),
            },
            &mut |known, available, node| {
                if let Some(ptr) = self.layout.node_map.get(&node) {
                    // SAFETY: the exclusive host owns this newly built dialog
                    // map; changing or dismissing it clears the map first.
                    unsafe { (**ptr).measure(known, available, &mut self.text_renderer) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        if let Some(dialog) = self.close_admission.dialog.as_mut() {
            dialog.apply_layout(&self.layout.taffy, root, self.layout.content_origin);
        }
        self.update_focusable_order();
        self.layout.structure_dirty = false;
        self.layout.layout_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct OwnedRoot {
        text: crate::widget::Text,
        fail: std::rc::Rc<std::cell::Cell<bool>>,
        /// Each save, and whether it could wait on storage.
        saves: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
        document: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        save_allowed: bool,
    }
    impl Widget for OwnedRoot {
        crate::provides!(
            dyn crate::widget::resources::PublishesResources,
            dyn crate::component_lifecycle::ComponentLifecycle,
            dyn crate::widget::ResourceOperations
        );

        fn sync(&mut self, doc: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            self.text.sync(doc, path);
        }
        fn build_layout(
            &mut self,
            t: &mut taffy::TaffyTree<()>,
            theme: &crate::Theme,
            map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.text.build_layout(t, theme, map)
        }
        fn apply_layout(
            &mut self,
            t: &taffy::TaffyTree<()>,
            n: taffy::NodeId,
            p: vello::kurbo::Point,
        ) {
            self.text.apply_layout(t, n, p);
        }
        fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text, theme, mut env) = ctx.parts();
            self.text.draw(&mut crate::draw::DrawCtx::new(
                scene,
                transform,
                text,
                theme,
                env.clone(),
            ));
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut crate::widget::EventCtx<'_>,
        ) {
            self.text.handle_event(event, ctx);
        }
        fn name(&self) -> &str {
            "OwnedRoot"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.text.get_rect()
        }
    }

    impl crate::widget::ResourceOperations for OwnedRoot {
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &[crate::widget::resource_actions::OPERATION_TYPE]
        }

        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            _: &mut crate::widget::Edits<'_>,
            _: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            let request = op.request();
            if request.type_id != crate::widget::resource_actions::OPERATION_TYPE {
                return Ok(None);
            }
            if !self.save_allowed {
                return Err("Save dialog cancelled".into());
            }
            self.document.as_ref().unwrap().set(false);
            Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    impl crate::component_lifecycle::ComponentLifecycle for OwnedRoot {
        fn unsaved_documents(
            &self,
            doc: &automerge::AutoCommit,
        ) -> Vec<crate::close_admission::UnsavedDocument> {
            use automerge::ReadDoc;
            if self.document.as_ref().is_some_and(|dirty| dirty.get()) {
                vec![crate::close_admission::UnsavedDocument {
                    save: tilo_actions::ActionAddress::new("test.quit:root", "save"),
                    identity: "draft.md".into(),
                    revision: doc
                        .get(automerge::ROOT, "draft")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_string()
                        .into_bytes(),
                }]
            } else {
                Vec::new()
            }
        }
        fn checkpoint_components(&mut self) -> Vec<String> {
            self.saves
                .borrow_mut()
                .push(!crate::storage::waiting::forbidden());
            if self.fail.get() {
                vec!["fixture owned writer refused".into()]
            } else {
                Vec::new()
            }
        }
    }

    impl crate::widget::resources::PublishesResources for OwnedRoot {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
            vec![crate::widget::resource_actions::resource(
                "test.quit",
                "Draft",
                "save",
                "Save",
            )]
        }
    }

    fn quit_fixture(save_allowed: bool) -> (App, std::rc::Rc<std::cell::Cell<bool>>) {
        use automerge::transaction::Transactable;
        let dirty = std::rc::Rc::new(std::cell::Cell::new(true));
        let root = OwnedRoot {
            text: crate::widget::Text::new("draft"),
            fail: std::rc::Rc::new(std::cell::Cell::new(false)),
            saves: std::rc::Rc::default(),
            document: Some(dirty.clone()),
            save_allowed,
        };
        let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(root));
        app.state
            .doc
            .put(automerge::ROOT, "draft", "pending edit")
            .unwrap();
        let document =
            crate::component_lifecycle::unsaved_documents(app.root_widget.as_ref(), &app.state.doc)
                [0]
            .clone();
        assert!(
            tilo_actions::api::resolve(
                &crate::widget::resources::resources(app.root_widget.as_ref(), &app.state.doc),
                &document.save,
            )
            .is_some(),
            "the fixture's unsaved document must declare its canonical Save address"
        );
        (app, dirty)
    }

    /// The broker's end of a window's invocations: once asked to stop, it hands over the last
    /// one it holds when the test says so. It keeps the wire id of each answer it was sent,
    /// and takes the window back when it is asked to.
    struct Invocations {
        stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handed_over: std::sync::Arc<std::sync::atomic::AtomicBool>,
        answered: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
        resumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::collaboration::CollabTransport for Invocations {
        fn status(&self) -> crate::collaboration::TransportStatus {
            crate::collaboration::TransportStatus::Ready
        }
        fn submit(
            &mut self,
            command: crate::collaboration::TransportCommand,
        ) -> Result<(), crate::collaboration::TransportError> {
            if let crate::collaboration::TransportCommand::Primary(
                crate::collaboration::CollabRequest::UpdateInvocation { id, .. },
            ) = command
            {
                self.answered.lock().unwrap().push(id);
            }
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<crate::collaboration::CollabEvent>) {}
        fn stop_invocations(&mut self) -> Result<(), crate::collaboration::TransportError> {
            self.stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn invocations_stopped(&self) -> bool {
            self.stopping.load(std::sync::atomic::Ordering::SeqCst)
                && self.handed_over.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn resume_invocations(&mut self) -> Result<(), crate::collaboration::TransportError> {
            self.stopping
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.resumed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn take_primary_route_result(
            &mut self,
        ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
            None
        }
        fn take_action_directory_requests(
            &mut self,
            _: &mut Vec<crate::collaboration::ActionDirectoryRequest>,
        ) {
        }
    }

    /// The broker's end of the closing window's invocations, as [`Invocations`]: whether
    /// it was asked to stop, the switch that hands over the last one, and the answers sent.
    type Ends = (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    );

    /// A window whose root keeps each save it is asked for, on a connection whose
    /// invocations end as the test says ([`Invocations`]).
    fn closing_window() -> (App, std::rc::Rc<std::cell::RefCell<Vec<bool>>>, Ends) {
        let saves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let root = OwnedRoot {
            text: crate::widget::Text::new("body"),
            fail: std::rc::Rc::default(),
            saves: saves.clone(),
            document: None,
            save_allowed: false,
        };
        let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(root));
        let ends: Ends = Default::default();
        app.collaboration.manager =
            crate::collaboration::CollaborationManager::with_transport(Box::new(Invocations {
                stopping: ends.0.clone(),
                handed_over: ends.1.clone(),
                answered: ends.2.clone(),
                resumed: std::sync::Arc::default(),
            }));
        assert!(app.collaboration.manager.is_connected());
        (app, saves, ends)
    }

    /// A closing window first takes no more invocations and answers what it was handed; only
    /// then does it save, once, and that save only starts the store's commits, so the window's
    /// loop never waits on the disk.
    #[test]
    fn a_closing_window_stops_its_invocations_before_it_saves_without_waiting() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut app, saves, (stopping, handed_over, _)) = closing_window();

        assert!(!app.admit_native_close_request());
        assert!(
            stopping.load(SeqCst),
            "the close asks for no more invocations"
        );
        assert!(
            saves.borrow().is_empty(),
            "nothing is saved while invocations may still reach the window"
        );
        app.service_close_admission();
        assert!(!app.finish_close_if_ready());
        assert!(saves.borrow().is_empty());

        handed_over.store(true, SeqCst);
        app.service_close_admission();
        assert!(app.finish_close_if_ready());
        assert!(app.close_admission.admitted);
        assert_eq!(
            *saves.borrow(),
            [false],
            "one save, after the last invocation, that never waits on storage"
        );
    }

    /// On the window's loop, which never waits on the disk, a close's save only starts the
    /// store's writes: the window stays until the workspace takes them and goes once it has. A
    /// write the workspace refuses keeps the window and its edits, and Retry saves them again.
    #[test]
    fn a_closing_window_goes_only_once_its_store_confirms_the_save() {
        use crate::application::{LaunchTarget, RestoreInput};
        use crate::application_test_support as probe;
        use crate::storage::waiting::forbid_strictly_on_this_thread;
        use automerge::ReadDoc as _;
        use automerge::transaction::Transactable as _;
        // The loop's passes until the window goes or asks: each takes what the workspace
        // answered meanwhile, and none waits for it.
        let passes = |app: &mut App| {
            let deadline = tilo_test_env::hang_deadline();
            loop {
                {
                    let _loop = forbid_strictly_on_this_thread();
                    app.service_close_admission();
                    if app.finish_close_if_ready() {
                        return true;
                    }
                }
                if app.close_admission.dialog.is_some() {
                    return false;
                }
                assert!(
                    crate::time::now() < deadline,
                    "the close never went or asked"
                );
            }
        };
        for refused in [false, true] {
            let host = probe::ProbeHarness::new();
            let component = probe::runner_probe_definition()
                .instantiate(
                    host.prepare(probe::runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let checkout = component.record().clone();
            host.records
                .with(&checkout.id, |doc| {
                    doc.put(automerge::ROOT, "body", "kept").unwrap();
                    doc.commit();
                })
                .unwrap();
            let mut app = App::from_component(component, probe::runner_inputs()).unwrap();
            host.workspace.refuse.set(refused);
            let mut closed = {
                let _loop = forbid_strictly_on_this_thread();
                app.admit_native_close_request()
            };
            if !closed {
                closed = passes(&mut app);
            }
            if refused {
                assert!(!closed, "a refused write keeps the window");
                assert!(!app.close_admission.admitted);
                assert!(
                    app.close_admission.resources()[0]
                        .actions
                        .iter()
                        .any(|action| action.id == Choice::Retry.id()),
                    "the refusal offers Retry"
                );
                assert_eq!(
                    host.records
                        .read(&checkout.id, |doc| {
                            doc.get(automerge::ROOT, "body")
                                .unwrap()
                                .and_then(|(body, _)| body.into_scalar().ok())
                                .and_then(|body| body.into_string().ok())
                        })
                        .flatten()
                        .as_deref(),
                    Some("kept"),
                    "the edit is still in the window's record"
                );
                host.workspace.refuse.set(false);
                {
                    let _loop = forbid_strictly_on_this_thread();
                    close_choice(&mut app, Choice::Retry);
                }
                closed = passes(&mut app);
            }
            assert!(
                closed,
                "a confirmed write closes the window; it asks {:?}, the record's refusal {:?}",
                app.close_admission
                    .resources()
                    .iter()
                    .flat_map(|resource| resource.actions.iter().map(|action| action.id.clone()))
                    .collect::<Vec<_>>(),
                host.records.durability_error(&checkout.id),
            );
            assert!(app.close_admission.admitted);
            assert_eq!(
                probe::body(&host.records, &checkout),
                "kept",
                "the record the window closed on keeps what it typed"
            );
        }
    }

    /// An invocation the window took before it began to close may not be done as the broker
    /// ends its stream: here an effect whose edit the broker is still admitting. The window
    /// waits for it, answers it, and only then goes. Before, it went with the effect
    /// unanswered, and whoever asked was told its outcome was unknown.
    #[test]
    fn a_closing_window_answers_an_invocation_still_running_before_it_goes() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut app, _, (_, handed_over, answered)) = closing_window();
        let running = tilo_actions::InvocationId::new();
        let actor = tilo_actions::Actor::Member(tilo_actions::MemberId::person("asker"));
        app.actions.follow_ups.insert(
            5,
            (
                running,
                crate::app_core::FollowUpControl::Outer(tilo_actions::ActionAddress::new(
                    "doc", "edit",
                )),
            ),
        );
        app.actions.acting_for.insert(
            running,
            crate::invocation_scope::Continuation {
                invoker: tilo_actions::ActionInvoker::delivered(&actor, None),
                context: None,
                admitted: tilo_actions::ActionKind::Undoable,
                refused: None,
                outstanding: std::collections::HashSet::from([5]),
                deferred: Some((
                    21,
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation: running,
                        output: tilo_actions::ActionOutput::default(),
                    },
                )),
            },
        );

        assert!(!app.admit_native_close_request());
        handed_over.store(true, SeqCst);
        app.service_close_admission();
        assert!(
            !app.finish_close_if_ready(),
            "the window stays while the effect it was handed is not done"
        );
        assert!(answered.lock().unwrap().is_empty());

        // The broker admitted the effect's edit: the effect is done.
        assert!(app.settle_follow_up(
            5,
            &Ok((
                tilo_actions::InvocationId::new(),
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: running,
                    output: tilo_actions::ActionOutput::default(),
                },
            )),
        ));
        app.service_close_admission();
        assert!(app.finish_close_if_ready());
        assert_eq!(
            *answered.lock().unwrap(),
            [21],
            "its answer went before the window"
        );
    }

    /// Keep Editing calls off the close a refused write stopped: the window takes invocations
    /// again, and the save that later succeeds closes nothing.
    #[test]
    fn keep_editing_calls_off_a_close_the_store_refused() {
        use crate::application::{LaunchTarget, RestoreInput};
        use crate::application_test_support as probe;
        use crate::storage::waiting::forbid_strictly_on_this_thread;
        use automerge::transaction::Transactable as _;
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let host = probe::ProbeHarness::new();
        let component = probe::runner_probe_definition()
            .instantiate(
                host.prepare(probe::runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        host.records
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, "body", "kept").unwrap();
                doc.commit();
            })
            .unwrap();
        let mut app = App::from_component(component, probe::runner_inputs()).unwrap();
        host.workspace.refuse.set(true);

        let stopping = std::sync::Arc::new(AtomicBool::new(false));
        let handed_over = std::sync::Arc::new(AtomicBool::new(false));
        let resumed = std::sync::Arc::new(AtomicBool::new(false));
        app.collaboration.manager =
            crate::collaboration::CollaborationManager::with_transport(Box::new(Invocations {
                stopping: stopping.clone(),
                handed_over: handed_over.clone(),
                answered: std::sync::Arc::default(),
                resumed: resumed.clone(),
            }));
        // The broker holds nothing more for this window: stopping ends its invocations.
        handed_over.store(true, SeqCst);

        {
            let _loop = forbid_strictly_on_this_thread();
            assert!(!app.admit_native_close_request());
        }
        assert!(stopping.load(SeqCst), "the close stops the invocations");
        {
            let _loop = forbid_strictly_on_this_thread();
            app.service_close_admission();
            assert!(
                !app.finish_close_if_ready(),
                "a refused write keeps the window"
            );
        }
        assert!(app.close_admission.dialog.is_some(), "the refusal asks");

        {
            let _loop = forbid_strictly_on_this_thread();
            close_choice(&mut app, Choice::KeepEditing);
        }
        assert!(resumed.load(SeqCst), "Keep Editing takes invocations again");
        assert!(
            !app.collaboration.manager.invocations_stopped(),
            "the window answers what reaches it"
        );

        // The workspace takes writes again and the window's own save succeeds: nothing closes.
        host.workspace.refuse.set(false);
        {
            let _loop = forbid_strictly_on_this_thread();
            app.autosave_flush();
        }
        {
            let _loop = forbid_strictly_on_this_thread();
            app.service_close_admission();
            assert!(
                !app.finish_close_if_ready(),
                "a save after Keep Editing closes nothing"
            );
        }
        assert!(!app.close_admission.admitted);
        assert_eq!(
            crate::application_test_support::body(&host.records, &checkout),
            "kept",
            "the later save went through"
        );
    }

    const KEPT_APP: &str = "so.tilo.test-close-kept";

    /// A window that shows a record of its app's workspace and keeps it open, as a view of the
    /// record does: its route publishes what the record changes.
    struct Shows {
        text: crate::widget::Text,
        owner: crate::history_mode::HistoryOwner,
    }

    impl crate::widget::Decorator for Shows {
        type Inner = crate::widget::Text;
        fn inner(&self) -> &crate::widget::Text {
            &self.text
        }
        fn inner_mut(&mut self) -> &mut crate::widget::Text {
            &mut self.text
        }
        fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
            if request.wants::<dyn crate::bound_dispatch::BoundWidget>() {
                request.provide::<dyn crate::bound_dispatch::BoundWidget>(self);
            } else if request.wants::<dyn crate::widget::decorator::Decorated>() {
                request.provide::<dyn crate::widget::decorator::Decorated>(self);
            } else {
                self.text.provide(request);
            }
        }
    }

    impl crate::bound_dispatch::BoundWidget for Shows {
        fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
            None
        }
        fn resource_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
            vec![self.owner.clone()]
        }
    }

    /// The window's connection: it takes every change its routes publish, and answers each
    /// barrier only when the test does, as the workspace does once it took (or
    /// refused) what was sent before the barrier.
    #[derive(Clone, Default)]
    struct Workspace {
        published: std::sync::Arc<std::sync::Mutex<Vec<crate::collaboration::RouteSpec>>>,
        barriers: std::sync::Arc<
            std::sync::Mutex<
                Vec<
                    std::sync::mpsc::SyncSender<
                        Result<(), crate::collaboration::DocumentPublicationError>,
                    >,
                >,
            >,
        >,
    }

    impl Workspace {
        /// Answer every barrier asked for so far with `answer`; how many there were.
        fn answer(
            &self,
            answer: &Result<(), crate::collaboration::DocumentPublicationError>,
        ) -> usize {
            let barriers = std::mem::take(&mut *self.barriers.lock().unwrap());
            for barrier in &barriers {
                barrier.send(answer.clone()).unwrap();
            }
            barriers.len()
        }
    }

    impl crate::collaboration::CollabTransport for Workspace {
        fn status(&self) -> crate::collaboration::TransportStatus {
            crate::collaboration::TransportStatus::Ready
        }
        fn submit(
            &mut self,
            command: crate::collaboration::TransportCommand,
        ) -> Result<(), crate::collaboration::TransportError> {
            if let crate::collaboration::TransportCommand::ToRoute(
                route,
                crate::collaboration::CollabRequest::Document { .. },
            ) = command
            {
                self.published.lock().unwrap().push(route);
            }
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<crate::collaboration::CollabEvent>) {}
        fn take_primary_route_result(
            &mut self,
        ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
            None
        }
        fn take_action_directory_requests(
            &mut self,
            _: &mut Vec<crate::collaboration::ActionDirectoryRequest>,
        ) {
        }
        fn publication_barrier(
            &mut self,
        ) -> Result<
            crate::collaboration::DocumentPublicationReceipt,
            crate::collaboration::DocumentPublicationError,
        > {
            let (answer, receipt) = std::sync::mpsc::sync_channel(1);
            self.barriers.lock().unwrap().push(answer);
            Ok(crate::collaboration::DocumentPublicationReceipt::new(
                receipt,
            ))
        }
        fn document_publishable(&self, _: Option<&str>) -> bool {
            true
        }
    }

    /// A window showing a record the workspace holds, on `manager`: the record, its store and
    /// what keeps its owner alive.
    fn window_on_record(
        manager: crate::collaboration::CollaborationManager,
    ) -> (
        App,
        crate::resource::ResourceStore,
        crate::resource::DocumentCheckout,
        std::rc::Rc<crate::history_mode::owner::ResourceOwnerLifetime>,
    ) {
        let workspace = uuid::Uuid::new_v4();
        let port = std::rc::Rc::new(crate::resource::MemoryWorkspace::default());
        let record = crate::resource::DocumentCheckout::new(
            crate::resource::DocumentId::minted(),
            crate::resource::ResourceType::new("note"),
        );
        {
            let mut owners = crate::resource::ResourceOwners::writing(port.clone());
            let store = owners.open(KEPT_APP, &workspace.to_string()).unwrap();
            store.create_resource(&record).unwrap();
            store.flush().unwrap();
        }
        let mut owners = crate::resource::ResourceOwners::reading(port);
        let store = owners.open(KEPT_APP, &workspace.to_string()).unwrap();
        assert_eq!(
            store.try_open_resource(&record).unwrap(),
            crate::resource::ResourceOpenOutcome::Loaded
        );
        let lifetime = crate::history_mode::owner::ResourceOwnerLifetime::new();
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(Shows {
                text: crate::widget::Text::new("shown"),
                owner: crate::history_mode::HistoryOwner::record(
                    store.clone(),
                    record.clone(),
                    &lifetime,
                    true,
                ),
            }),
        );
        app.collaboration.manager = manager;
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::AcceptedRoute {
            route: crate::collaboration::RouteSpec::document(KEPT_APP, workspace, "shown"),
            epoch: uuid::Uuid::new_v4(),
        });
        app.refresh_document_routes();
        (app, store, record, lifetime)
    }

    fn type_into(
        store: &crate::resource::ResourceStore,
        record: &crate::resource::DocumentCheckout,
        body: &str,
    ) {
        use automerge::transaction::Transactable as _;
        store
            .with(&record.id, |doc| {
                doc.put(automerge::ROOT, "body", body).unwrap();
                doc.commit();
            })
            .unwrap();
    }

    fn status(
        store: &crate::resource::ResourceStore,
        record: &crate::resource::DocumentCheckout,
    ) -> crate::resource::ResourceSaveStatus {
        store.save_status(&record.id).unwrap()
    }

    /// What a person types into a window is saving until the workspace took it, then saved;
    /// a close waits until the workspace took everything the window sent before it closed,
    /// and goes once it has.
    #[test]
    fn a_closing_window_goes_once_the_workspace_took_its_last_change() {
        use crate::resource::ResourceSaveStatus::{Saved, Saving};
        let workspace = Workspace::default();
        let (mut app, store, record, _lifetime) = window_on_record(
            crate::collaboration::CollaborationManager::with_transport(Box::new(workspace.clone())),
        );
        assert_eq!(
            status(&store, &record),
            Saved,
            "the record as the workspace holds it"
        );

        type_into(&store, &record, "typed before closing");
        app.pump_document_routes();
        assert_eq!(
            workspace.published.lock().unwrap().len(),
            1,
            "the route published the change"
        );
        assert_eq!(status(&store, &record), Saving, "sent, not taken yet");

        assert!(!app.request_close(), "the close waits for the workspace");
        app.service_close_admission();
        assert!(!app.close_admission.ready);
        assert_eq!(workspace.answer(&Ok(())), 1);
        app.service_close_admission();
        assert_eq!(status(&store, &record), Saved, "the workspace took it");
        assert!(
            !app.close_admission.ready,
            "the close waits for the barrier asked after it checkpointed"
        );
        assert_eq!(workspace.answer(&Ok(())), 1);
        app.service_close_admission();
        assert!(
            app.finish_close_if_ready(),
            "everything is kept: the window goes"
        );
        assert!(app.close_admission.dialog.is_none());
    }

    /// The workspace refused what the window sent: the window stays with the edit, the record
    /// says the save failed, and the close says so with Retry, Discard and Keep Editing. Keep
    /// Editing calls the close off, and the window takes invocations again; Retry closes once the
    /// workspace took it.
    #[test]
    fn a_close_the_workspace_refused_keeps_the_window_and_its_edit() {
        use crate::collaboration::DocumentPublicationError::Refused;
        use crate::resource::ResourceSaveStatus::{Failed, Saved};
        let workspace = Workspace::default();
        let (mut app, store, record, _lifetime) = window_on_record(
            crate::collaboration::CollaborationManager::with_transport(Box::new(workspace.clone())),
        );
        type_into(&store, &record, "typed before closing");
        app.pump_document_routes();

        assert!(!app.request_close());
        workspace.answer(&Err(Refused("the disk is full".into())));
        app.service_close_admission();
        // The barrier the close asked for after it checkpointed answers too.
        workspace.answer(&Ok(()));
        app.service_close_admission();
        assert!(
            !app.finish_close_if_ready(),
            "a refused change keeps the window"
        );
        assert!(!app.close_admission.admitted);
        assert_eq!(
            status(&store, &record),
            Failed("the disk is full".into()),
            "the record says it is not saved"
        );
        let choices: Vec<String> = app.close_admission.resources()[0]
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect();
        assert_eq!(choices, ["retry", "discard", "keep-editing"]);
        assert_eq!(
            store.read(&record.id, |doc| {
                use automerge::ReadDoc as _;
                doc.get(automerge::ROOT, "body")
                    .unwrap()
                    .and_then(|(value, _)| value.into_scalar().ok())
                    .and_then(|scalar| scalar.into_string().ok())
            }),
            Some(Some("typed before closing".into())),
            "the edit is still in the window"
        );

        // Try again sends the record again; the workspace takes it this time, the record says
        // it is saved, and the close goes.
        let sent = workspace.published.lock().unwrap().len();
        close_choice(&mut app, Choice::Retry);
        let deadline = tilo_test_env::hang_deadline();
        while !app.close_admission.ready {
            assert!(crate::time::now() < deadline, "the close never went");
            app.service_close_admission();
            workspace.answer(&Ok(()));
        }
        assert!(
            workspace.published.lock().unwrap().len() > sent,
            "Try again sent the record again"
        );
        assert_eq!(status(&store, &record), Saved, "the refusal is cleared");
        assert!(app.finish_close_if_ready());
    }

    /// Without a connection the workspace hears nothing more: a window whose record holds
    /// nothing the workspace lacks closes at once, and one holding an edit it never sent asks
    /// before it loses it. Discard closes it without the edit.
    #[test]
    fn a_window_without_a_connection_closes_at_once_unless_it_holds_an_unsent_edit() {
        let (mut kept, _store, _record, _lifetime) =
            window_on_record(crate::collaboration::CollaborationManager::disabled());
        assert!(
            kept.request_close(),
            "nothing to lose: the window goes at once"
        );
        assert!(kept.close_admission.dialog.is_none());

        let (mut app, store, record, _lifetime) =
            window_on_record(crate::collaboration::CollaborationManager::disabled());
        type_into(&store, &record, "typed while offline");
        app.pump_document_routes();
        assert!(!app.request_close(), "the edit never reached the workspace");
        assert!(app.close_admission.dialog.is_some(), "the close asks");
        assert!(app.close_admission.not_kept);
        close_choice(&mut app, Choice::Discard);
        assert!(
            app.finish_close_if_ready(),
            "Discard closes without the edit"
        );
    }

    /// A process with no window ends only once its workspace took what its app wrote last: the
    /// workspace keeps the only copy. It sends the edit, waits for the barrier, and the record
    /// says saved; with no connection it ends at once.
    #[cfg(feature = "native")]
    #[test]
    fn a_process_with_no_window_ends_once_its_workspace_took_its_last_write() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let workspace = Workspace::default();
        let (mut app, store, record, _lifetime) = window_on_record(
            crate::collaboration::CollaborationManager::with_transport(Box::new(workspace.clone())),
        );
        type_into(&store, &record, "written with no window");
        let ended = std::sync::Arc::new(AtomicBool::new(false));
        let answering = {
            let workspace = workspace.clone();
            let ended = std::sync::Arc::clone(&ended);
            std::thread::spawn(move || {
                let mut answered = 0;
                while !ended.load(Ordering::Acquire) {
                    answered += workspace.answer(&Ok(()));
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                answered
            })
        };
        let (_wake, wakes) = std::sync::mpsc::channel();
        app.keep_last_writes(&wakes);
        ended.store(true, Ordering::Release);
        assert!(answering.join().unwrap() > 0, "it asked the workspace");
        assert!(
            !workspace.published.lock().unwrap().is_empty(),
            "the edit went to the workspace"
        );
        assert_eq!(
            status(&store, &record),
            crate::resource::ResourceSaveStatus::Saved,
            "the workspace took it before the process ended"
        );

        let (mut offline, store, record, _lifetime) =
            window_on_record(crate::collaboration::CollaborationManager::disabled());
        type_into(&store, &record, "written with no connection");
        let (_wake, wakes) = std::sync::mpsc::channel();
        offline.keep_last_writes(&wakes);
    }

    fn close_choice(app: &mut App, choice: Choice) {
        invoke_close_choice(app, choice).unwrap();
    }

    fn invoke_close_choice(app: &mut App, choice: Choice) -> Result<(), String> {
        let resources = app.close_admission.resources();
        let action = resources[0]
            .actions
            .iter()
            .find(|action| action.id == choice.id())
            .unwrap();
        let Some(encoded) = action.encode_immediate() else {
            panic!("unit close action")
        };
        app.invoke_close_encoded(&encoded)
    }

    // This is the production portable Quit router and the very same admission
    // method called by AppHandler for its resulting CloseRequested event.
    fn deliver_quit(quit: &mut native_quit_request::QuitRequest<u8>, app: &mut App) -> bool {
        let native_quit_request::QuitDecision::Forward(request) = quit.request(true) else {
            panic!("Quit must reach the registered primary window")
        };
        assert!(matches!(
            quit.request(true),
            native_quit_request::QuitDecision::Cancel
        ));
        assert_eq!(quit.take(request), Some(1));
        app.admit_native_close_request()
    }

    tilo_test_env::scenario! {
        fn close_admission_keeps_its_dialogs_and_the_pending_session;

        phase close_refusal_and_failure_dialogs_persist_until_an_explicit_choice;
        {
            {
                use automerge::{ReadDoc, transaction::Transactable};
                let fail = std::rc::Rc::new(std::cell::Cell::new(true));
                let saves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let root = OwnedRoot {
                    text: crate::widget::Text::new("body"),
                    fail: fail.clone(),
                    saves: saves.clone(),
                    document: None,
                    save_allowed: false,
                };
                let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(root));
                assert!(!app.request_close());
                assert!(!app.close_admission.admitted);
                assert!(app.close_admission.dialog.is_some());
                let resource = app.close_admission.resources()[0].id.clone();
                let dialog = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                let dialog = (dialog as *const dyn Widget).cast::<()>() as usize;
                app.state
                    .doc
                    .put(automerge::ROOT, "edited_after_refusal", true)
                    .unwrap();
                fail.set(false);
                assert!(
                    !app.request_close(),
                    "a duplicate native close cannot replace the failure decision"
                );
                assert_eq!(
                    saves.borrow().len(),
                    1,
                    "duplicate close does not retry persistence"
                );
                assert_eq!(app.close_admission.resources()[0].id, resource);
                let actual = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                assert_eq!((actual as *const dyn Widget).cast::<()>() as usize, dialog);

                close_choice(&mut app, Choice::Retry);
                assert_eq!(
                    saves.borrow().len(),
                    2,
                    "explicit Retry re-evaluates the same checked owner exactly once"
                );
                assert!(
                    app.state
                        .doc
                        .get(automerge::ROOT, "edited_after_refusal")
                        .unwrap()
                        .is_some()
                );
                assert!(
                    !app.close_admission.admitted,
                    "checkpoint does not destroy the live tree"
                );
                assert!(app.close_admission.ready);
            }

            {
                use crate::application::{LaunchTarget, RestoreInput};
                use crate::application_test_support as probe;
                use automerge::transaction::Transactable as _;
                let host = probe::ProbeHarness::new();
                let component = probe::runner_probe_definition()
                    .instantiate(
                        host.prepare(probe::runner_probe_definition(), 11, &LaunchTarget::New)
                            .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                let checkout = component.record().clone();
                host.records
                    .with(&checkout.id, |doc| {
                        doc.put(automerge::ROOT, "body", "kept").unwrap();
                        doc.commit();
                    })
                    .unwrap();
                host.workspace.refuse.set(true);
                let mut app = App::from_component(component, probe::runner_inputs()).unwrap();
                assert!(
                    !app.request_close(),
                    "ordinary unavailable host writer cannot be skipped"
                );
                assert!(!app.close_admission.admitted);
            }

            {
                let calls = std::rc::Rc::new(std::cell::Cell::new(0));
                let observed = calls.clone();
                let resource = crate::widget::resource_actions::resource(
                    "test.close",
                    "Replacement",
                    "save",
                    "Save",
                );
                let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "save");
                let root = crate::widget::resource_actions::immediate(
                    Box::new(crate::widget::Text::new("replacement")),
                    resource,
                    move |_| {
                        observed.set(observed.get() + 1);
                        Ok(tilo_actions::ActionOutput::default())
                    },
                );
                let mut app = App::new_with_controlled_inputs(crate::State::new(), root);
                app.close_admission.reviewed = vec![crate::close_admission::UnsavedDocument {
                    save: address,
                    identity: "old-document.md".into(),
                    revision: vec![1],
                }];
                app.close_admission.show(
                    "close-title",
                    "close-unsaved",
                    &[Choice::Save, Choice::Discard, Choice::KeepEditing],
                    app.input.platform,
                );
                let resources = app.close_admission.resources();
                let action = resources[0]
                    .actions
                    .iter()
                    .find(|action| action.id == "save")
                    .unwrap();
                let Some(encoded) = action.encode_immediate() else {
                    panic!("unit close action")
                };
                assert!(app.invoke_close_encoded(&encoded).is_err());
                assert_eq!(
                    calls.get(),
                    0,
                    "the replacement exposes a real Save action, but was not the reviewed document"
                );
            }

            {
                use automerge::transaction::Transactable;
                let (mut app, dirty) = quit_fixture(false);
                let mut quit = native_quit_request::QuitRequest::new();
                quit.set_target(Some(1));
                assert!(!deliver_quit(&mut quit, &mut app));
                let old_resource = app.close_admission.resources()[0].id.clone();
                let old_reviewed = app.close_admission.reviewed.clone();

                app.state
                    .doc
                    .put(automerge::ROOT, "draft", "newer collaborative revision")
                    .unwrap();
                assert!(!deliver_quit(&mut quit, &mut app));
                assert_eq!(app.close_admission.resources()[0].id, old_resource);
                assert_eq!(app.close_admission.reviewed, old_reviewed);

                assert_eq!(
                    invoke_close_choice(&mut app, Choice::Save),
                    Err("document changed since the close question was shown".into())
                );
                assert_ne!(app.close_admission.resources()[0].id, old_resource);
                assert_ne!(app.close_admission.reviewed, old_reviewed);
                assert!(dirty.get(), "stale Save cannot reach the document action");
            }
        }

        phase native_quit_retains_the_pending_document_session_and_focus;
        {
            {
                use automerge::{ReadDoc, transaction::Transactable};
                let (mut app, dirty) = quit_fixture(false);
                let pending = app.state.doc.pending_ops();
                assert!(pending > 0);
                let mut quit = native_quit_request::QuitRequest::new();
                quit.set_target(Some(1));
                assert!(!deliver_quit(&mut quit, &mut app));
                close_choice(&mut app, Choice::KeepEditing);
                assert!(!app.finish_close_if_ready());
                assert_eq!(
                    app.state.doc.pending_ops(),
                    pending,
                    "cancel must not checkpoint pending edits"
                );
                assert!(dirty.get());
                app.state
                    .doc
                    .put(automerge::ROOT, "draft", "edited after cancelled Quit")
                    .unwrap();
                assert!(
                    !deliver_quit(&mut quit, &mut app),
                    "later Quit must ask again"
                );
                close_choice(&mut app, Choice::Save);
                assert!(
                    !app.finish_close_if_ready(),
                    "cancelled save cannot admit Quit"
                );
                assert!(dirty.get());
                close_choice(&mut app, Choice::KeepEditing);
                assert_eq!(
                    app.state
                        .doc
                        .get(automerge::ROOT, "draft")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("edited after cancelled Quit")
                );
                assert!(!app.close_admission.admitted);
            }

            {
                let (mut app, dirty) = quit_fixture(false);
                let mut quit = native_quit_request::QuitRequest::new();
                quit.set_target(Some(1));
                assert!(!deliver_quit(&mut quit, &mut app));
                app.init_layout();
                app.layout_close_dialog((640.0, 480.0));
                app.refresh_semantic_tree();
                assert!(app.focus_tab(false));

                let resources = app.close_admission.resources();
                let addresses: Vec<_> = resources[0]
                    .actions
                    .iter()
                    .map(|action| (resources[0].id.clone(), action.id.clone()))
                    .collect();
                let reviewed = app.close_admission.reviewed.clone();
                let focused = app.focused_node;
                let dialog = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                let dialog = (dialog as *const dyn Widget).cast::<()>() as usize;

                assert!(!deliver_quit(&mut quit, &mut app));
                let repeated = app.close_admission.resources();
                let repeated_addresses: Vec<_> = repeated[0]
                    .actions
                    .iter()
                    .map(|action| (repeated[0].id.clone(), action.id.clone()))
                    .collect();
                assert_eq!(repeated_addresses, addresses);
                assert_eq!(app.close_admission.reviewed, reviewed);
                assert_eq!(app.focused_node, focused);
                let actual = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                assert_eq!((actual as *const dyn Widget).cast::<()>() as usize, dialog);
                assert!(dirty.get());
            }

            {
                use automerge::transaction::Transactable;
                let (mut app, dirty) = quit_fixture(true);
                let mut quit = native_quit_request::QuitRequest::new();
                quit.set_target(Some(1));
                assert!(!deliver_quit(&mut quit, &mut app));
                close_choice(&mut app, Choice::Save);
                assert!(!dirty.get(), "Save uses the document's declared action");
                assert!(app.finish_close_if_ready());
                assert!(app.close_admission.admitted);

                let (mut app, dirty) = quit_fixture(false);
                assert!(!deliver_quit(&mut quit, &mut app));
                close_choice(&mut app, Choice::Discard);
                app.state
                    .doc
                    .put(automerge::ROOT, "draft", "arrived before exit")
                    .unwrap();
                assert!(
                    !app.finish_close_if_ready(),
                    "discard never covers a newer revision"
                );
                assert!(dirty.get());
                assert!(!app.close_admission.admitted);
                close_choice(&mut app, Choice::Discard);
                assert!(app.finish_close_if_ready());
                assert!(app.close_admission.admitted);
            }

            {
                let mut app = App::new_with_controlled_inputs(
                    crate::State::new(),
                    Box::new(crate::widget::Text::new("body")),
                );
                app.close_admission.show(
                    "close-title",
                    "close-unsaved",
                    &[Choice::Save, Choice::Discard, Choice::KeepEditing],
                    app.input.platform,
                );
                app.init_layout();
                let modal = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                let admitted = (modal as *const dyn Widget).cast::<()>() as usize;
                let root = app.layout.root_node.unwrap();
                assert_eq!(app.layout.node_map[&root].cast::<()>() as usize, admitted);
                // Move the entire initialized App into a different allocation. Compare
                // real tree addresses to the map before dereferencing a single map entry.
                let mut moved = Box::new(app);
                let actual = moved.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
                assert_eq!(
                    (actual as *const dyn Widget).cast::<()>() as usize,
                    admitted
                );
                fn addresses(widget: &dyn Widget, result: &mut Vec<usize>) {
                    result.push((widget as *const dyn Widget).cast::<()>() as usize);
                    for child in widget.children() {
                        addresses(child, result);
                    }
                }
                let mut actual_addresses = Vec::new();
                addresses(actual, &mut actual_addresses);
                assert!(
                    moved
                        .layout
                        .node_map
                        .values()
                        .all(|ptr| actual_addresses.contains(&((*ptr).cast::<()>() as usize)))
                );
                moved.layout_close_dialog((640.0, 480.0));
                assert_eq!(moved.focusable_order.len(), 3);
                moved.refresh_semantic_tree();
                assert!(moved.focus_tab(false));
                assert!(!moved.close_admission.admitted);
            }
        }
    }
}
