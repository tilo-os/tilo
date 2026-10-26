//! Native-host adapter. The persistence/decision path is independent of the
//! OS event loop; Windows and macOS use this same admission boundary.
use super::{App, BrokerReply, ResourceInvocation};
use crate::close_admission::{Choice, Command, OPERATION};
use crate::widget::Widget;

// Compile the actual native Quit routing state in portable host tests. The
// AppKit delegate itself still requires macOS qualification.
#[cfg(test)]
#[path = "../../../../../vendor/winit/src/platform_impl/quit_request.rs"]
mod native_quit_request;

impl App {
    #[cfg(any(test, feature = "native"))]
    pub(super) fn admit_native_close_request(&mut self) -> bool {
        // Native hosts may deliver another window-close or application Quit
        // while a decision is already visible. Coalesce it at the native
        // boundary so explicit API retries can still re-check repaired owners.
        if self.close_admission.dialog.is_some() {
            return false;
        }
        self.request_close() && self.finish_close_if_ready()
    }

    fn invalidate_close_presentation(&mut self) {
        self.actions.action_palette = None;
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
        // Native hosts may deliver another window-close or application Quit
        // while a decision is already visible. Coalesce it here so it cannot
        // replace the session, semantic addresses, focus, or failure detail.
        if self.close_admission.dialog.is_some() {
            return false;
        }
        self.close_admission.ready = false;
        if self.close_admission.save_waiting {
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
        let errors = self.checkpoint_before_close();
        if !errors.is_empty() {
            self.close_admission.show(
                "close-save-failed",
                &errors.join("\n"),
                &[Choice::Retry, Choice::KeepEditing],
                self.input.platform,
            );
            self.invalidate_close_presentation();
            return false;
        }
        self.close_admission.dialog = None;
        self.invalidate_close_presentation();
        self.close_admission.ready = true;
        true
    }

    #[cfg(feature = "native")]
    pub(super) fn finish_close_if_ready(&mut self) -> bool {
        if !self.close_admission.ready || !self.request_close() {
            return false;
        }
        // Recheck at the actual event-loop exit boundary: asynchronous saves
        // may have completed earlier in a batch which also delivered edits.
        self.close_admission.admitted = true;
        // No document mutation/event may run between checkpoint and finalization.
        // Existing teardown may cancel jobs through its immutable-document poll.
        self.component_close_reports
            .extend(crate::component_lifecycle::finish_admitted_close(
                self.root_widget.as_mut(),
            ));
        self.close_wake_scope();
        true
    }

    pub(super) fn checkpoint_before_close(&mut self) -> Vec<String> {
        let _entry = self.enter_wake_scope();
        let mut errors = Vec::new();
        if let Some(records) = self.persistence.records.as_mut() {
            if let Err(error) = records.try_flush(&mut self.state.doc) {
                errors.push(error);
            }
            if let (Some(saver), Some(index)) = (
                self.persistence.autosave.as_mut(),
                self.persistence.room_index.as_mut(),
            ) {
                saver.request_checkpoint(index.document_mut());
                if let Err(error) = saver.try_flush() {
                    errors.push(error);
                }
            }
        } else if let Some(saver) = self.persistence.autosave.as_mut() {
            saver.request_checkpoint(&mut self.state.doc);
            if let Err(error) = saver.try_flush() {
                errors.push(error);
            }
        }
        errors.extend(crate::component_lifecycle::checkpoint_components(
            self.root_widget.as_mut(),
        ));
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
        errors.extend(crate::close_admission::retry_reports(
            &mut self.component_close_reports,
        ));
        errors
    }

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
            Choice::KeepEditing => self.close_admission.dismiss(),
            Choice::Retry => {
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
        let actor = self.collaboration.local_author.0.clone();
        // Do not supply an invocation or claim broker admission. This is the
        // ordinary local canonical action door, including permissions/effects.
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

    pub(super) fn service_close_admission(&mut self) {
        while let Some(intent) =
            self.close_admission.dialog.as_mut().and_then(|dialog| {
                crate::widget::controls::take_control_invocation(dialog.as_mut())
            })
        {
            let result = self
                .invoke_history_address_local(&intent.address, None)
                .unwrap_or_else(|| Err(tilo_actions::DoorError::Unknown(intent.address.clone())));
            if let Some(dialog) = self.close_admission.dialog.as_mut() {
                crate::widget::controls::on_control_invocation_result(
                    dialog.as_mut(),
                    &intent.address,
                    &result,
                );
            }
        }
        while let Some(command) = self.close_admission.take_command() {
            // Pointer/keyboard callbacks enter the same declared command as
            // accessibility and local automation; stale sessions are refused.
            if self.close_admission.validate(&command) {
                let address = tilo_actions::ActionAddress::new(
                    format!("tilo.host.close:{}", command.session),
                    command.choice.id(),
                );
                let _ = self.invoke_history_address_local(&address, None);
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
                        let _ = self.dispatch_node(node, |widget, doc, path| {
                            widget.perform_action(
                                &crate::a11y::Action::Activate,
                                &[],
                                &mut crate::widget::Edits::lend(doc),
                                path,
                            )
                        });
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
        self.service_close_admission();
        true
    }

    #[cfg(feature = "native")]
    pub(super) fn layout_close_dialog(&mut self, logical: (f64, f64)) {
        let Some(root) = self.layout.root_node else {
            return;
        };
        let _ = self.layout.taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(logical.0 as f32),
                height: taffy::AvailableSpace::Definite(logical.1 as f32),
            },
            |known, available, node, _, _| {
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
        flushes: std::rc::Rc<std::cell::Cell<usize>>,
        document: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        save_allowed: bool,
    }
    impl Widget for OwnedRoot {
        crate::provides!(
            dyn crate::resources::PublishesResources,
            dyn crate::component_lifecycle::ComponentLifecycle
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
        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut crate::TextRenderer,
            theme: &crate::Theme,
            presence: &crate::PresenceCtx,
        ) {
            self.text.draw(scene, transform, text, theme, presence);
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
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &[crate::widget::resource_actions::OPERATION_TYPE]
        }
        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            _: &mut crate::widget::Edits<'_>,
            _: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            let Some(request) = op.unscoped() else {
                return Ok(None);
            };
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
            self.flushes.set(self.flushes.get() + 1);
            if self.fail.get() {
                vec!["fixture owned writer refused".into()]
            } else {
                Vec::new()
            }
        }
    }

    impl crate::resources::PublishesResources for OwnedRoot {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            vec![crate::widget::resource_actions::resource(
                "test.quit",
                "Draft",
                "save",
                "Save",
            )]
        }
    }
    #[test]
    fn close_refusal_and_failure_dialogs_persist_until_an_explicit_choice() {
        // Case `repeated_close_preserves_failure_dialog_until_explicit_retry`.
        {
            use automerge::{ReadDoc, transaction::Transactable};
            let fail = std::rc::Rc::new(std::cell::Cell::new(true));
            let flushes = std::rc::Rc::new(std::cell::Cell::new(0));
            let root = OwnedRoot {
                text: crate::widget::Text::new("body"),
                fail: fail.clone(),
                flushes: flushes.clone(),
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
                flushes.get(),
                1,
                "duplicate close does not retry persistence"
            );
            assert_eq!(app.close_admission.resources()[0].id, resource);
            let actual = app.close_admission.dialog.as_deref().unwrap() as &dyn Widget;
            assert_eq!((actual as *const dyn Widget).cast::<()>() as usize, dialog);

            close_choice(&mut app, Choice::Retry);
            assert_eq!(
                flushes.get(),
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

        // Case `disabled_host_writer_still_refuses_close`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Text::new("body")),
            )
            .with_autosave(crate::persist::DocWriter::disabled(
                "required writer unavailable",
            ));
            assert!(
                !app.request_close(),
                "ordinary unavailable host writer cannot be skipped"
            );
            assert!(!app.close_admission.admitted);
        }

        // Case `stale_save_button_does_not_save_the_replacement_document`.
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

        // Case `stale_revision_retires_old_session_only_after_explicit_choice`.
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

    fn quit_fixture(save_allowed: bool) -> (App, std::rc::Rc<std::cell::Cell<bool>>) {
        use automerge::transaction::Transactable;
        let dirty = std::rc::Rc::new(std::cell::Cell::new(true));
        let root = OwnedRoot {
            text: crate::widget::Text::new("draft"),
            fail: std::rc::Rc::new(std::cell::Cell::new(false)),
            flushes: std::rc::Rc::new(std::cell::Cell::new(0)),
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
            tilo_actions::door::resolve(
                &crate::resources::resources(app.root_widget.as_ref(), &app.state.doc),
                &document.save,
            )
            .is_some(),
            "the fixture's unsaved document must declare its canonical Save address"
        );
        (app, dirty)
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

    #[test]
    fn native_quit_retains_the_pending_document_session_and_focus() {
        // Case `native_quit_keep_editing_and_cancelled_save_retain_pending_document`.
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

        // Case `repeated_native_quit_preserves_unsaved_session_addresses_and_focus`.
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

        // Case `native_quit_save_and_discard_share_final_revision_check`.
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

        // Case `modal_control_addresses_survive_moving_the_initialized_app`.
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
