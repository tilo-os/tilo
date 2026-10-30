//! Setup, hosted as an app, from the keyboard and assistive technology:
//! where focus lands, what Tab reaches, what the reader is told.

use super::tests::{
    exported_node_named, focused_name, key_press, onboarding_frame, onboarding_frame_with_presser,
};
use super::*;
use automerge::ReadDoc as _;
use automerge::transaction::Transactable;
use winit::keyboard::ModifiersState;

/// Every stop Tab reaches from where focus starts, by name, once round:
/// the shell's own stops and, inside a hosted app, the app's.
fn tab_stop_names(
    fb: &mut FrameBuilder,
    doc: &mut AutoCommit,
    focus: &mut Option<NodeId>,
) -> Vec<String> {
    fb.reconcile_native_focus(focus);
    let start = focused_name(fb, *focus);
    let mut names: Vec<String> = start.iter().cloned().collect();
    let tab = key_press(Key::Named(NamedKey::Tab));
    // Each control is a stop once; a page holds far fewer than this.
    for _ in 0..64 {
        assert!(fb.dispatch_event(
            &tab,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT,
            focus,
        ));
        let name = focused_name(fb, *focus);
        if name == start {
            return names;
        }
        names.extend(name);
    }
    panic!("Tab never came round to where it started: {names:?}");
}

/// Focus the control the reader is shown named as `matches` asks, as an
/// assistive technology focuses it.
fn focus_control_where(
    fb: &mut FrameBuilder,
    doc: &mut AutoCommit,
    focus: &mut Option<NodeId>,
    matches: impl Fn(&str) -> bool,
) {
    let update = fb.build_a11y_tree_update(*focus, 1.0);
    let (id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.label().is_some_and(&matches))
        .expect("the control reaches the external tree");
    assert!(fb.dispatch_a11y_action(
        *id,
        &ui::a11y::Action::Focus,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
        focus,
    ));
}

/// How far the scroll region of the page setup shows is scrolled.
fn setup_page_scroll_offset(fb: &FrameBuilder) -> Option<f32> {
    fn scroll_offset(widget: &mut dyn Widget) -> Option<f32> {
        if let Some(scroll) = widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<ui::widget::Scrollable>())
        {
            return Some(scroll.offset_y());
        }
        widget.children_mut().into_iter().find_map(scroll_offset)
    }
    fb.node_map.values().copied().find_map(|pointer| {
        // SAFETY: node_map pointers belong to the live tree, which nothing
        // rebuilds while this borrow of the builder lasts.
        let widget = unsafe { &mut *pointer };
        let app = widget
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?;
        scroll_offset(app.component_root_mut()?)
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn native_oobe_accessibility_tree_owns_focus_roles_and_editable_text() {
    fn native_oobe_initializes_focus_without_test_control() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Use my existing Tilo identity")
        );
    }

    fn native_oobe_exports_named_roles_state_and_focus_to_accesskit() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);

        let update = fb.build_a11y_tree_update(focus, 1.0);
        let (existing_id, existing) = exported_node_named(&update, "Use my existing Tilo identity")
            .expect("existing-identity action reaches the external tree");
        let (_, create) = exported_node_named(&update, "Create a new identity")
            .expect("create-identity action reaches the external tree");
        // Identity setup has distinct backup and final hand-off states, so
        // its semantic progress surface contains four stages.
        let (_, progress) = exported_node_named(&update, "Identity setup, step 1 of 4: Choose")
            .expect("the OOBE progress state reaches the external tree");

        // The ways forward are one list of rows that each open a flow.
        assert_eq!(existing.role(), ui::accesskit::Role::ListItem);
        assert_eq!(create.role(), ui::accesskit::Role::ListItem);
        assert_eq!(progress.role(), ui::accesskit::Role::ProgressIndicator);
        assert!(existing.supports_action(ui::accesskit::Action::Focus));
        assert!(existing.supports_action(ui::accesskit::Action::Click));
        assert_eq!(update.focus, existing_id);
        assert_eq!(
            update.tree.as_ref().map(|tree| tree.root),
            Some(ui::accesskit_bridge::ROOT_ID)
        );
    }

    fn native_oobe_routes_external_focus_and_editable_text_to_the_live_widget() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        fb.reconcile_native_focus(&mut focus);

        let intro = fb.build_a11y_tree_update(focus, 1.0);
        let (existing_id, _) = exported_node_named(&intro, "Use my existing Tilo identity")
            .expect("the external tree exposes the Connect-existing action");
        assert!(fb.dispatch_a11y_action(
            existing_id,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();

        let choice = fb.build_a11y_tree_update(focus, 1.0);
        let (restore_id, _) = exported_node_named(&choice, "Restore from a backup")
            .expect("the use-existing choice offers its restore flow");
        assert!(fb.dispatch_a11y_action(
            restore_id,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();

        let connect = fb.build_a11y_tree_update(focus, 1.0);
        let (field_id, field) = exported_node_named(&connect, "Identity backup file")
            .expect("the non-secret identity-path field reaches the external tree");
        assert_eq!(field.role(), ui::accesskit::Role::TextInput);
        assert!(field.supports_action(ui::accesskit::Action::Focus));
        assert!(field.supports_action(ui::accesskit::Action::SetValue));
        assert_eq!(field.value(), Some(""));

        assert!(fb.dispatch_a11y_action(
            field_id,
            &ui::a11y::Action::Focus,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Identity backup file"),
            "an external focus request must update the compositor's real focus cursor"
        );

        let probe = "tilo-atspi-edit-probe";
        assert!(fb.dispatch_a11y_action(
            field_id,
            &ui::a11y::Action::SetValue(probe.into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();

        let updated = fb.build_a11y_tree_update(focus, 1.0);
        let (_, field) = exported_node_named(&updated, "Identity backup file")
            .expect("the editable field remains externally addressable after its update");
        assert_eq!(field.value(), Some(probe));
    }

    fn native_oobe_external_tree_replaces_step_and_updates_control_state() {
        let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        fb.reconcile_native_focus(&mut focus);
        let intro = fb.build_a11y_tree_update(focus, 1.0);

        press(crate::onboarding_view::KEY_BEGIN);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);
        let protect = fb.build_a11y_tree_update(focus, 1.0);

        assert!(
            exported_node_named(&protect, "Use my existing Tilo identity").is_none(),
            "nodes from the replaced step must leave the exported tree"
        );
        assert!(
            exported_node_named(&intro, &crate::onboarding::acknowledgement()).is_none(),
            "the next step must not leak into the initial tree"
        );
        let (ack_id, acknowledgement) =
            exported_node_named(&protect, &crate::onboarding::acknowledgement())
                .expect("the recovery acknowledgement reaches the external tree");
        let (_, create) = exported_node_named(&protect, "Create local identity")
            .expect("the gated create action reaches the external tree");
        assert_eq!(acknowledgement.role(), ui::accesskit::Role::CheckBox);
        assert_eq!(
            acknowledgement.toggled(),
            Some(ui::accesskit::Toggled::False)
        );
        assert!(create.is_disabled());
        assert_eq!(protect.focus, ack_id);

        assert!(fb.dispatch_a11y_action(
            ack_id,
            &ui::a11y::Action::ToggleCheck,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        // The first pump observes the parent flow change; layout rebuilds the
        // composition just as the production frame loop does before its next
        // refresh. The second pump then syncs the newly-created checkbox from
        // the CRDT. Omitting the intervening layout would test two reducer
        // passes against the old tree, which cannot occur in the live loop.
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);
        let acknowledged = fb.build_a11y_tree_update(focus, 1.0);
        let (_, acknowledgement) =
            exported_node_named(&acknowledged, &crate::onboarding::acknowledgement()).unwrap();
        let (_, create) = exported_node_named(&acknowledged, "Create local identity").unwrap();
        assert_eq!(
            acknowledgement.toggled(),
            Some(ui::accesskit::Toggled::True),
            "the rebuilt checkbox must preserve its visible and spoken state"
        );
        assert!(
            !create.is_disabled(),
            "the externally-operated acknowledgement must enable the gated action"
        );
    }

    native_oobe_initializes_focus_without_test_control();
    native_oobe_exports_named_roles_state_and_focus_to_accesskit();
    native_oobe_routes_external_focus_and_editable_text_to_the_live_widget();
    native_oobe_external_tree_replaces_step_and_updates_control_state();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn native_oobe_keyboard_order_activates_and_traverses_enabled_controls() {
    fn native_oobe_enter_activates_the_focused_button() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let tab = key_press(Key::Named(NamedKey::Tab));

        assert!(fb.dispatch_event(
            &tab,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Create a new identity")
        );
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Enter)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            doc.get(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN)
                .expect("read begin action"),
            None,
            "activation is a view action, never a document write"
        );

        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);
        assert!(
            focused_name(&fb, focus).is_some_and(|name| name.contains("I understand")),
            "Enter must advance into the identity-protection step"
        );
    }

    fn native_oobe_backup_tab_order_reaches_fields_acknowledgement_and_continue() {
        let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        for key in [
            crate::onboarding_view::KEY_BEGIN,
            crate::onboarding_view::KEY_UNDERSTOOD,
        ] {
            if key == crate::onboarding_view::KEY_UNDERSTOOD {
                doc.put(&automerge::ROOT, key, true).expect("advance OOBE");
            } else {
                press(key);
            }
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            let _ = fb.paint();
            // The replacement subtree must sync once before a gated control
            // can be activated, matching the production frame loop.
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            let _ = fb.paint();
        }
        // A structural rebuild exposes the acknowledgement control before
        // its document value has been synchronized into the new view.
        doc.put(
            &automerge::ROOT,
            crate::onboarding_view::KEY_UNDERSTOOD,
            true,
        )
        .expect("acknowledge identity protection");
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        press(crate::onboarding_view::KEY_CREATE);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        let names = tab_stop_names(&mut fb, &mut doc, &mut focus);
        assert!(
            names.iter().any(|name| name == "New backup passphrase"),
            "backup passphrase must be keyboard reachable: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|name| name == "Confirm new backup passphrase"),
            "backup confirmation must be keyboard reachable: {names:?}"
        );

        // A screen reader reaches the passphrase as password text it can
        // focus and type into: AccessKit's AT-SPI adapter offers Text and
        // EditableText only on a text input with a text run, so the field
        // carries one that never holds what was typed.
        // Its visible label shares the name, so pick the field by role.
        let passphrase_run = |update: &ui::accesskit::TreeUpdate| {
            let (id, field) = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.label() == Some("New backup passphrase")
                        && node.role() == ui::accesskit::Role::PasswordInput
                })
                .expect("the passphrase reaches AT-SPI as password text");
            assert!(field.supports_action(ui::accesskit::Action::Focus));
            assert!(field.supports_action(ui::accesskit::Action::SetValue));
            assert_eq!(field.value(), None);
            let runs: Vec<_> = update
                .nodes
                .iter()
                .filter(|(node_id, node)| {
                    field.children().contains(node_id)
                        && node.role() == ui::accesskit::Role::TextRun
                })
                .collect();
            let [(_, run)] = runs.as_slice() else {
                panic!("one text run among {:?}", field.children());
            };
            (*id, run.value().map(str::to_owned))
        };
        let (field, typed) = passphrase_run(&fb.build_a11y_tree_update(focus, 1.0));
        assert_eq!(typed.as_deref(), Some(""));
        assert!(fb.dispatch_a11y_action(
            field,
            &ui::a11y::Action::Focus,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert!(fb.dispatch_a11y_action(
            field,
            &ui::a11y::Action::SetValue("tilosecretprobe42".into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        fb.relayout();
        let update = fb.build_a11y_tree_update(focus, 1.0);
        assert_eq!(update.focus, field, "AT-SPI focus lands on the field");
        assert_eq!(passphrase_run(&update).1.as_deref(), Some(""));
        assert!(
            update.nodes.iter().all(|(_, node)| {
                node.value()
                    .is_none_or(|value| !value.contains("tilosecretprobe42"))
                    && node
                        .label()
                        .is_none_or(|label| !label.contains("tilosecretprobe42"))
            }),
            "a typed passphrase never reaches assistive technology"
        );
        assert!(
            names
                .iter()
                .any(|name| name.contains("without a recovery key or backup")),
            "skip acknowledgement must be keyboard reachable: {names:?}"
        );

        focus_control_where(&mut fb, &mut doc, &mut focus, |name| {
            name.contains("without a recovery key or backup")
        });
        let enter = key_press(Key::Named(NamedKey::Enter));
        assert!(fb.dispatch_event(
            &enter,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);
        let tab = key_press(Key::Named(NamedKey::Tab));
        assert!(fb.dispatch_event(
            &tab,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Continue without backup"),
            "acknowledging the risk must make the adjacent continuation action the next keyboard stop"
        );
    }

    fn native_oobe_tab_and_shift_tab_follow_dfs_order() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let tab = key_press(Key::Named(NamedKey::Tab));

        assert!(fb.dispatch_event(
            &tab,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Create a new identity")
        );

        let shift = Modifiers::from(ModifiersState::SHIFT);
        assert!(fb.dispatch_event(
            &tab,
            &shift,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Use my existing Tilo identity")
        );
    }

    fn native_oobe_focus_order_excludes_disabled_controls() {
        let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
        let mut doc = AutoCommit::new();
        press(crate::onboarding_view::KEY_BEGIN);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();

        let mut focus = None;
        let names = tab_stop_names(&mut fb, &mut doc, &mut focus);
        assert!(names.iter().any(|name| name.contains("I understand")));
        assert!(names.iter().any(|name| name == "Back"));
        assert!(
            !names.iter().any(|name| name == "Create"),
            "the gated Create action must not become a Tab stop: {names:?}"
        );
    }

    fn focused_terminal_gets_plain_tab_before_focus_traversal() {
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        let terminal = fb.node_map.iter().find_map(|(id, widget)| {
            // SAFETY: node_map pointers are owned by the live widget tree and
            // remain valid until the next structural rebuild.
            is_terminal(unsafe { &**widget }).then_some(*id)
        });
        let Some(terminal) = terminal else {
            // Environments without a PTY deliberately mount the terminal
            // fallback instead; the routing rule remains covered by OOBE's
            // unclaimed-Tab traversal test above.
            return;
        };
        let mut focus = None;
        fb.set_native_focus(&mut focus, Some(terminal));
        let mut doc = AutoCommit::new();

        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Tab)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(focus, Some(terminal), "terminal Tab must not move focus");
    }

    native_oobe_enter_activates_the_focused_button();
    native_oobe_backup_tab_order_reaches_fields_acknowledgement_and_continue();
    native_oobe_tab_and_shift_tab_follow_dfs_order();
    native_oobe_focus_order_excludes_disabled_controls();
    focused_terminal_gets_plain_tab_before_focus_traversal();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn native_oobe_transitions_and_rebuilds_reseat_focus_and_keep_pointer_state() {
    fn native_oobe_pointer_motion_is_retained_before_modal_dispatch() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let position = Point::new(417.0, 263.0);

        assert!(fb.dispatch_event(
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(position.x, position.y),
            },
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            fb.capture_pointer_position(),
            Some(position),
            "onboarding must retain the motion used to hit-test the following coordinate-free MouseInput"
        );
    }

    fn native_oobe_step_transition_reseats_focus_on_new_first_control() {
        let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        fb.reconcile_native_focus(&mut focus);
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Use my existing Tilo identity")
        );

        press(crate::onboarding_view::KEY_BEGIN);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);

        assert!(
            focused_name(&fb, focus).is_some_and(|name| name.contains("I understand")),
            "focus must move into the replacement OOBE step"
        );
    }

    fn native_oobe_step_transition_keeps_the_new_page_at_its_top() {
        let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        fb.reconcile_native_focus(&mut focus);

        press(crate::onboarding_view::KEY_EXISTING);
        press(crate::onboarding_view::KEY_CHOOSE_RESTORE);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.reconcile_native_focus(&mut focus);

        let scroll_offset = setup_page_scroll_offset(&fb).expect("restore page scroll viewport");
        assert_eq!(
            scroll_offset, 0.0,
            "programmatic first focus must not hide a newly presented page's heading"
        );
    }

    fn native_oobe_incidental_rebuild_preserves_focus_ordinal() {
        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let tab = key_press(Key::Named(NamedKey::Tab));
        assert!(fb.dispatch_event(
            &tab,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Create a new identity")
        );

        fb.rebuild_layout_tree();
        fb.relayout();
        fb.reconcile_native_focus(&mut focus);
        assert_eq!(
            focused_name(&fb, focus).as_deref(),
            Some("Create a new identity"),
            "an unrelated structure refresh must preserve the focused ordinal"
        );
    }

    native_oobe_pointer_motion_is_retained_before_modal_dispatch();
    native_oobe_step_transition_reseats_focus_on_new_first_control();
    native_oobe_step_transition_keeps_the_new_page_at_its_top();
    native_oobe_incidental_rebuild_preserves_focus_ordinal();
}
