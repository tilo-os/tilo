//! Caller-only palette presentation. Submission transfers inert intent to the
//! host; it never installs a worker route or executes an application effect.
use super::{
    ElementState, FrameHarness, HashMap, InspectionNode, Key, Point, WindowEvent,
    collect_inspection,
};
use crate::action_palette::{ActionPalette, PendingOperation};

#[derive(Default)]
pub(super) struct CallerPalette {
    pub palette: Option<ActionPalette>,
    pub resources: Vec<tilo_actions::Resource>,
    pub pending: Option<PendingOperation>,
    /// Set by a root that answers the command chord itself.
    pub disabled: bool,
}

impl FrameHarness {
    pub(super) fn caller_resources(&self) -> Vec<tilo_actions::Resource> {
        let mut resources = self.caller_palette.resources.clone();
        resources.extend(
            crate::resources::caller_view_resources(self.root.as_ref(), &self.state.doc)
                .into_iter()
                .filter(|resource| {
                    resource
                        .actions
                        .iter()
                        .all(|action| action.kind == tilo_actions::OperationKind::View)
                }),
        );
        resources
    }
    /// Replace the caller's currently authenticated action projection. Resource
    /// addresses must include immutable host/route/catalogue identity.
    /// Whether the command chord opens the caller palette. A root that
    /// answers the chord with its own search (the web desktop) turns it off.
    pub fn set_caller_palette_enabled(&mut self, enabled: bool) {
        self.invalidate_local();
        self.caller_palette.disabled = !enabled;
    }

    pub fn set_caller_actions(&mut self, resources: Vec<tilo_actions::Resource>) {
        self.invalidate_local();
        self.caller_palette.resources = resources;
        let resources = self.caller_resources();
        if let Some(palette) = &mut self.caller_palette.palette {
            palette.refresh_visible_resources(&resources);
        }
        self.refresh_system_search();
        self.relayout();
    }

    /// Transfer one UI intent. The host owns durable invocation admission and
    /// recovery; this frame never acknowledges an effect or mutates its document.
    /// Browser-owned command search opens the same authoritative palette.
    pub fn open_caller_search(&mut self, query: &str) -> Result<(), String> {
        self.invalidate_local();
        if self.global_search_query().is_some()
            || self.global_search_pending()
            || self.root.modal_capture().is_some()
            || self.focus_trap_active
            || self.caller_palette.pending.is_some()
        {
            return Err("Finish the current dialog or command first".into());
        }
        let mut palette = ActionPalette::with_host_text(self.text.font_system.locale().to_owned());
        palette.load_open_resources(&self.caller_resources());
        palette.open();
        palette.set_query(query);
        self.caller_palette.palette = Some(palette);
        self.relayout();
        Ok(())
    }
    /// The caller palette's rows as a person reads them, label and the line
    /// under it, in display order; empty while it is closed.
    #[must_use]
    pub fn caller_palette_rows(&self) -> Vec<(String, Option<String>)> {
        self.caller_palette
            .palette
            .as_ref()
            .map(ActionPalette::result_rows)
            .unwrap_or_default()
    }

    pub fn take_caller_operation(&mut self) -> Option<PendingOperation> {
        self.invalidate_local();
        self.caller_palette.pending.take()
    }

    pub(super) fn caller_palette_event(&mut self, event: &WindowEvent) -> bool {
        if self.caller_palette.palette.is_none() {
            if !self.caller_palette.disabled
                && let WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == ElementState::Pressed
                && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("k"))
                && self.platform.command_modifier_down(self.modifiers.state())
                && !self.modifiers.state().alt_key()
                && self.root.modal_capture().is_none()
                && !self.focus_trap_active
                && self.caller_palette.pending.is_none()
                && !self.global_search_pending()
            {
                let mut palette =
                    ActionPalette::with_host_text(self.text.font_system.locale().to_owned());
                palette.load_open_resources(&self.caller_resources());
                palette.open();
                self.caller_palette.palette = Some(palette);
                self.relayout();
                return true;
            }
            return false;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("k"))
            && self.platform.command_modifier_down(self.modifiers.state())
            && !self.modifiers.state().alt_key()
        {
            self.caller_palette.palette = None;
            self.relayout();
            return true;
        }
        let Some(palette) = self.caller_palette.palette.as_mut() else {
            return false;
        };
        let consumed = palette.handle_host_event(
            event,
            self.input_scale,
            self.modifiers,
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        self.finish_caller_palette();
        if consumed {
            self.relayout();
        }
        consumed
    }

    pub(super) fn finish_caller_palette(&mut self) {
        let local = crate::resources::caller_view_resources(self.root.as_ref(), &self.state.doc);
        let resources = self.caller_resources();
        let mut local_pending = None;
        if let Some(palette) = &mut self.caller_palette.palette {
            if let Some(pending) = palette.take_operation() {
                // Validate again after confirmation and before handing intent to
                // the host. Revoked/replaced addresses never execute locally.
                if pending.has_valid_input()
                    && tilo_actions::door::resolve(&resources, &pending.address).is_some()
                {
                    if pending.kind == tilo_actions::OperationKind::View
                        && tilo_actions::door::resolve(&local, &pending.address).is_some()
                    {
                        local_pending = Some(pending);
                    } else {
                        self.caller_palette.pending = Some(pending);
                    }
                }
            }
            if !palette.is_open() {
                self.caller_palette.palette = None;
            }
        }
        if let Some(pending) = local_pending {
            let encoded = match pending.encode_at_local_owner(&local) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::warn!(%error, "caller local request refused");
                    return;
                }
            };
            if let Err(error) = crate::widget::start_without_document_writes(
                self.root.as_mut(),
                &crate::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    pending.kind,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut crate::widget::Edits::lend(&mut self.state.doc),
                &self.state.root,
            ) {
                tracing::warn!(%error, "caller local request refused");
            }
        }
    }

    pub(super) fn perform_caller_palette_semantic(
        &mut self,
        name: &str,
        target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> bool {
        let Some(snapshot) = self.caller_palette_inspection() else {
            return false;
        };
        let Some(results_owner) = semantic_owner(&snapshot, name, target) else {
            return false;
        };
        let Some(palette) = self.caller_palette.palette.as_mut() else {
            return false;
        };
        palette.perform_host_semantic(
            results_owner,
            target,
            action,
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        )
    }

    pub(super) fn draw_caller_palette(&mut self) {
        let transform = self.capture_draw_transform();
        if let Some(palette) = &mut self.caller_palette.palette {
            let width = (f64::from(self.viewport.0) - 32.0).clamp(1.0, 640.0);
            palette.layout_with_text(
                &self.theme,
                Point::new((f64::from(self.viewport.0) - width) / 2.0, 24.0),
                width,
                &mut self.text,
            );
            palette.draw(
                &mut self.scene,
                transform,
                &mut self.text,
                &self.theme,
                &self.presence,
            );
        }
    }

    pub(super) fn caller_palette_inspection(&self) -> Option<InspectionNode> {
        let palette = self.caller_palette.palette.as_ref()?;
        let mut counts = HashMap::new();
        let (field, results) = palette.presentation_widgets();
        let rect = palette.rect();
        Some(InspectionNode {
            path: "caller-palette".into(),
            widget: "ActionPalette".into(),
            test_id: Some("caller-palette".into()),
            focusable: false,
            rect: [rect.x0, rect.y0, rect.width(), rect.height()],
            child_clip: None,
            layout: None,
            semantics: None,
            children: vec![
                collect_inspection(field, "caller-palette", &mut counts),
                collect_inspection(results, "caller-palette", &mut counts),
            ],
        })
    }
}

// Mirror the browser's unique-test-ID or exact-path owner contract using only
// the current palette tree. Background, invented and ambiguous owners fail closed.
fn semantic_owner(snapshot: &InspectionNode, name: &str, target: Option<&str>) -> Option<bool> {
    fn collect<'a>(
        node: &'a InspectionNode,
        results: bool,
        name: &str,
        found: &mut Vec<(bool, &'a InspectionNode)>,
    ) {
        if node.path == name || node.test_id.as_deref() == Some(name) {
            found.push((results, node));
        }
        for child in &node.children {
            collect(child, results, name, found);
        }
    }
    fn target_count(node: &crate::a11y::AccessNode, target: &str) -> usize {
        usize::from(node.action_target.as_deref() == Some(target))
            + node
                .children
                .iter()
                .map(|child| target_count(child, target))
                .sum::<usize>()
    }
    let mut found = Vec::new();
    for (index, child) in snapshot.children.iter().enumerate() {
        collect(child, index == 1, name, &mut found);
    }
    let [(results, node)] = found.as_slice() else {
        return None;
    };
    let semantics = node.semantics.as_ref()?;
    if target.is_some_and(|target| target_count(semantics, target) != 1) {
        return None;
    }
    Some(*results)
}

#[cfg(test)]
mod tests {
    use super::super::{NamedKey, State, Theme, key_input};
    use super::*;
    fn frame() -> FrameHarness {
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(
                crate::widget::TextInput::local()
                    .with_id("document")
                    .with_initial_value("unchanged"),
            ),
            (640, 480),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("caller".into()),
        );
        frame.set_platform_conventions(crate::platform::PlatformConventions::new(
            crate::platform::Platform::Linux,
        ));
        frame
    }
    fn resources(
        confirmation: tilo_actions::ConfirmationPolicy,
        params: Vec<tilo_actions::ActionParam>,
    ) -> Vec<tilo_actions::Resource> {
        let mut action = tilo_actions::ActionSpec::typed(
            "insert",
            "Insert table",
            "",
            tilo_actions::ExecutionLocus::ServerPreferred,
            params,
            |_| tilo_actions::EncodedAction {
                type_id: "must-never-execute-locally".into(),
                payload: vec![],
            },
        );
        action.kind = tilo_actions::OperationKind::Effect;
        action.contract.confirmation = confirmation;
        vec![
            tilo_actions::Resource::new(
                "certified-host/room/record/revision",
                "document",
                "Writer",
            )
            .with_action(action),
        ]
    }
    fn open(frame: &mut FrameHarness) {
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
        ));
        frame.send_host_event(&key_input(
            &Key::Character("k".into()),
            ElementState::Pressed,
        ));
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
    }
    fn enter(frame: &mut FrameHarness) {
        frame.send_host_event(&key_input(
            &Key::Named(NamedKey::Enter),
            ElementState::Pressed,
        ));
    }
    #[test]
    fn real_palette_transfers_only_one_inert_intent_without_local_execution() {
        let mut frame = frame();
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::None, vec![]));
        let before = frame.state.doc.get_heads();
        open(&mut frame);
        enter(&mut frame);
        let intent = frame
            .take_caller_operation()
            .expect("real keyboard selected action");
        assert_eq!(
            intent.address.resource,
            "certified-host/room/record/revision"
        );
        assert_eq!(intent.kind, tilo_actions::OperationKind::Effect);
        assert!(frame.take_caller_operation().is_none());
        assert_eq!(frame.state.doc.get_heads(), before);
    }
    #[test]
    fn browser_ime_collects_typed_arguments_and_exact_confirmation_then_revocation_refuses() {
        let mut frame = frame();
        let params = vec![tilo_actions::ActionParam::required(
            "rows",
            tilo_actions::ParamType::Number,
            "Rows",
        )];
        frame.set_caller_actions(resources(
            tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "INSERT".into(),
            },
            params,
        ));
        open(&mut frame);
        enter(&mut frame);
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
            "{\"rows\":2}".into(),
        )));
        enter(&mut frame);
        assert!(frame.take_caller_operation().is_none());
        // Cancel has the keyboard first; the name is typed in its field.
        frame.send_host_event(&crate::synthetic_input::key_input(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            winit::event::ElementState::Pressed,
        ));
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
            "INSERT".into(),
        )));
        // Return in the field stays the safe answer: the action's own button,
        // past Cancel, goes ahead.
        enter(&mut frame);
        assert!(frame.take_caller_operation().is_none());
        frame.send_host_event(&crate::synthetic_input::key_input(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            winit::event::ElementState::Pressed,
        ));
        frame.send_host_event(&crate::synthetic_input::key_input(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            winit::event::ElementState::Pressed,
        ));
        enter(&mut frame);
        let intent = frame
            .take_caller_operation()
            .expect("typed and confirmed intent");
        assert_eq!(
            intent.wire_argument().as_deref(),
            Some("\u{1e}tilo-args:{\"rows\":2}")
        );
        assert_eq!(
            intent.confirmation_evidence(),
            &serde_json::json!({"confirmation_phrase":"INSERT"})
        );
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::Confirm, vec![]));
        open(&mut frame);
        enter(&mut frame);
        frame.set_caller_actions(vec![]);
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
            "CONFIRM".into(),
        )));
        enter(&mut frame);
        assert!(frame.take_caller_operation().is_none());
    }
    #[test]
    fn palette_pointer_uses_surface_dpr_without_changing_document_coordinates() {
        let mut frame = frame();
        frame.set_input_scale(2.0).unwrap();
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::None, vec![]));
        open(&mut frame);
        let rect = frame
            .caller_palette
            .palette
            .as_ref()
            .unwrap()
            .presentation_widgets()
            .1
            .get_rect();
        frame.send_host_event(&crate::synthetic_input::cursor_moved(
            rect.center().x * 2.0,
            (rect.y1 - 16.0) * 2.0,
        ));
        frame.send_host_event(&crate::synthetic_input::mouse_press());
        assert_eq!(
            frame.take_caller_operation().unwrap().address.action,
            "insert"
        );
        assert!(frame.inspection_json().unwrap().contains("unchanged"));
    }

    #[test]
    fn command_k_toggles_and_semantic_query_stays_in_the_palette() {
        let mut frame = frame();
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::None, vec![]));
        open(&mut frame);
        let snapshot = frame.caller_palette_inspection().unwrap();
        let field = &snapshot.children[0];
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(field.path.as_str()),
            "the caller palette owns keyboard input outside the document focus tree"
        );
        frame
            .perform_semantic_named(
                &field.path,
                None,
                &crate::a11y::Action::SetValue("Insert table".into()),
            )
            .unwrap();
        assert_eq!(
            frame.caller_palette.palette.as_ref().unwrap().query(),
            "Insert table"
        );
        open(&mut frame);
        assert!(frame.caller_palette.palette.is_none());
        assert!(frame.inspection_json().unwrap().contains("unchanged"));
    }

    #[test]
    fn open_palette_refreshes_visible_availability_without_input_and_keeps_target() {
        let mut frame = frame();
        let original = resources(tilo_actions::ConfirmationPolicy::None, vec![]);
        frame.set_caller_actions(original.clone());
        open(&mut frame);
        assert_eq!(
            frame
                .caller_palette
                .palette
                .as_ref()
                .unwrap()
                .result_labels(),
            // The row reads as the outcome alone: never the document's name
            // joined on with a dash.
            vec!["Insert table"]
        );
        frame.set_caller_actions(vec![]);
        assert!(
            frame
                .caller_palette
                .palette
                .as_ref()
                .unwrap()
                .result_labels()
                .is_empty(),
            "revocation removes rendered rows without a keypress"
        );
        assert!(!frame.inspection_json().unwrap().contains("Insert table"));
        frame.set_caller_actions(original);
        assert!(
            frame.inspection_json().unwrap().contains("Insert table"),
            "restoration is present in real semantics without input"
        );
        enter(&mut frame);
        assert_eq!(
            frame.take_caller_operation().unwrap().address.resource,
            "certified-host/room/record/revision"
        );
    }

    #[test]
    fn real_result_semantics_activate_only_the_current_exact_row() {
        let mut frame = frame();
        let original = resources(tilo_actions::ConfirmationPolicy::None, vec![]);
        frame.set_caller_actions(original.clone());
        open(&mut frame);
        let snapshot = frame.caller_palette_inspection().unwrap();
        let results = &snapshot.children[1];
        let target = results.semantics.as_ref().unwrap().children[0]
            .action_target
            .clone()
            .unwrap();
        // semanticBridge chooses a unique test ID before the inspection path.
        let owner = results.test_id.as_deref().unwrap();
        assert_eq!(owner, "action-palette-results");
        frame.set_caller_actions(vec![]);
        assert!(
            frame
                .perform_semantic_named(owner, Some(&target), &crate::a11y::Action::Activate)
                .is_err()
        );
        frame.set_caller_actions(original);
        frame
            .perform_semantic_named(owner, Some(&target), &crate::a11y::Action::Activate)
            .unwrap();
        assert_eq!(
            frame.take_caller_operation().unwrap().address.action,
            "insert"
        );
    }

    #[test]
    fn palette_semantics_refuse_unpublished_owners_and_field_targets() {
        let mut frame = frame();
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::None, vec![]));
        open(&mut frame);
        let snapshot = frame.caller_palette_inspection().unwrap();
        let field = &snapshot.children[0];
        assert!(
            field.test_id.is_none(),
            "search publishes its exact path owner"
        );
        for owner in [
            "document",
            "caller-palette/not-a-SearchField",
            "caller-palette/SearchField[99]",
        ] {
            assert!(
                frame
                    .perform_semantic_named(
                        owner,
                        None,
                        &crate::a11y::Action::SetValue("wrong".into())
                    )
                    .is_err()
            );
        }
        assert!(
            frame
                .perform_semantic_named(
                    &field.path,
                    Some("unpublished-child"),
                    &crate::a11y::Action::SetValue("wrong".into())
                )
                .is_err()
        );
        assert_eq!(frame.caller_palette.palette.as_ref().unwrap().query(), "");
        frame
            .perform_semantic_named(
                &field.path,
                None,
                &crate::a11y::Action::SetValue("Insert table".into()),
            )
            .unwrap();
        assert_eq!(
            frame.caller_palette.palette.as_ref().unwrap().query(),
            "Insert table"
        );
        assert!(frame.take_caller_operation().is_none());
        assert!(frame.inspection_json().unwrap().contains("Insert table"));
    }

    #[test]
    fn palette_semantic_owner_refuses_ambiguous_ids_and_targets() {
        let mut frame = frame();
        frame.set_caller_actions(resources(tilo_actions::ConfirmationPolicy::None, vec![]));
        open(&mut frame);
        let mut snapshot = frame.caller_palette_inspection().unwrap();
        let results = &snapshot.children[1];
        let owner = results.test_id.clone().unwrap();
        let target = results.semantics.as_ref().unwrap().children[0]
            .action_target
            .clone()
            .unwrap();
        assert_eq!(semantic_owner(&snapshot, &owner, Some(&target)), Some(true));
        snapshot.children[0].test_id = Some(owner.clone());
        assert_eq!(semantic_owner(&snapshot, &owner, Some(&target)), None);
        snapshot.children[0].test_id = None;
        let rows = &mut snapshot.children[1].semantics.as_mut().unwrap().children;
        rows.push(rows[0].clone());
        assert_eq!(semantic_owner(&snapshot, &owner, Some(&target)), None);
    }

    #[test]
    fn invalid_typed_input_does_not_become_a_nullary_effect() {
        let mut frame = frame();
        frame.set_caller_actions(resources(
            tilo_actions::ConfirmationPolicy::None,
            vec![tilo_actions::ActionParam::required(
                "rows",
                tilo_actions::ParamType::Number,
                "Rows",
            )],
        ));
        open(&mut frame);
        enter(&mut frame);
        frame.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
            "invalid".into(),
        )));
        enter(&mut frame);
        assert!(frame.take_caller_operation().is_none());
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("Invalid arguments"),
            "invalid input has a visible semantic diagnostic"
        );
        assert!(
            frame
                .caller_palette
                .palette
                .as_ref()
                .unwrap()
                .in_argument_mode()
        );
    }
}
