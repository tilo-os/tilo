use super::*;
use crate::presentation::MAX_KEYBOARD_CONTENT_WIDTH;
use automerge::AutoCommit;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn contextual_tools_retire_semantic_targets_and_cannot_use_custom_callbacks() {
    let invoked = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&invoked);
    let mut osk = OskWidget::new();
    osk.set_context_handler(Box::new(move |generation, slot| {
        received.borrow_mut().push((generation, slot));
        Ok(())
    }));
    osk.on_toolbar = Some(Box::new(|_| {
        panic!("context must use its authenticated route")
    }));
    let item = |generation| {
        OskToolbarItem::new(
            "bold",
            None,
            "Bold",
            OskToolbarAction::Contextual {
                generation,
                slot: 7,
            },
        )
    };
    osk.set_context_toolbar(10, vec![item(10)]);
    lay_out(&mut osk, 390.0, 312.0);
    let target = "osk:context:10:7:Bold";
    let activate = |osk: &mut OskWidget, target: &str| {
        osk.perform_semantic_action(
            target,
            &AccessibilityAction::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        )
    };
    assert!(activate(&mut osk, target));
    assert_eq!(*invoked.borrow(), vec![(10, 7)]);
    assert!(!activate(&mut osk, "osk:tool:0:Bold"));
    osk.set_context_toolbar(11, vec![item(11)]);
    assert!(!activate(&mut osk, target));
    osk.context_result(10, 7, false, "stale failure".into());
    assert!(osk.context_error.is_none());
    osk.context_result(11, 7, false, "Permission denied".into());
    assert_eq!(osk.context_error.as_deref(), Some("Permission denied"));
    assert!(!osk.activate_suggestion(0));
    osk.set_purpose(InputPurpose::Password);
    assert!(osk.context_generation.is_none());
    osk.set_context_toolbar(12, vec![item(12)]);
    assert!(osk.context_generation.is_none());
    assert!(!activate(&mut osk, "osk:context:12:7:Bold"));
    assert_eq!(*invoked.borrow(), vec![(10, 7)]);
}

#[test]
fn a_tall_phone_surface_keeps_normal_keyboard_rows_docked_and_touchable() {
    let mut osk = OskWidget::new().with_surface_height();
    lay_out(&mut osk, 390.0, 844.0);
    let height = f64::from(keyboard_height(
        osk.current_layout().row_count(),
        &osk.spacing,
        &osk.metrics,
    ));
    assert!((osk.content_rect.height() - height).abs() < 0.01);
    assert!((osk.content_rect.y1 - 844.0).abs() < 0.01);
    let (_, q) = key_rect(&osk, "q");
    assert!(q.height() <= f64::from(osk.metrics.header_height));
    assert!(!osk.key_area.contains(Point::new(195.0, 100.0)));
    finger(&mut osk, 1, TouchPhase::Started, q.center());
    finger(&mut osk, 1, TouchPhase::Ended, q.center());
    assert_eq!(osk.pending_text(), "q");
    lay_out(&mut osk, 844.0, 220.0);
    assert!(osk.content_rect.y0 >= 0.0);
    assert!(osk.content_rect.y1 <= 220.0);
}

fn lay_out(osk: &mut OskWidget, w: f32, h: f32) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(w, h),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    taffy
        .compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
        )
        .unwrap();
    osk.apply_layout(&taffy, child, Point::ORIGIN);
}

#[test]
fn key_taps_edit_pending_text_and_fire_callbacks() {
    eprintln!("phase tapping_letter_appends_to_pending_text");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("h"));
        assert!(osk.activate_label("i"));
        assert_eq!(osk.pending_text(), "hi");
    }

    eprintln!("phase tapping_letter_emits_char_action_and_fires_callback");
    {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut osk = {
            let seen = seen.clone();
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .on_output(move |o| seen.borrow_mut().push(o))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("a");
        let log = osk.take_key_actions();
        assert_eq!(log, vec![KeyAction::Char("a".into())]);
        assert_eq!(*seen.borrow(), vec![pre("a")]);
    }

    eprintln!("phase backspace_removes_last_char");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("Backspace");
        assert_eq!(osk.pending_text(), "h");
    }

    eprintln!("phase take_output_drains");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("space");
        assert_eq!(osk.take_output(), vec![pre("h"), commit("h ")]);
        assert!(osk.take_output().is_empty(), "drained");
        assert_eq!(osk.pending_text(), "h ");
    }
}

fn pre(s: &str) -> OskOutput {
    OskOutput::SetPreedit(s.into())
}

fn commit(s: &str) -> OskOutput {
    OskOutput::Commit(s.into())
}

#[test]
fn typed_word_composes_as_preedit_and_commits_at_space() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 800.0, 360.0);
    osk.activate_label("h");
    osk.activate_label("i");
    osk.activate_label("space");
    osk.activate_label("m");
    assert_eq!(
        osk.take_output(),
        vec![pre("h"), pre("hi"), commit("hi "), pre("m")]
    );
    assert_eq!(osk.pending_text(), "hi m");
}

fn type_word(osk: &mut OskWidget, word: &str) {
    for c in word.chars() {
        osk.apply_action(KeyAction::Char(c.to_string()));
    }
}

#[test]
fn space_autocorrects_a_non_word_and_offers_it_as_typed() {
    let mut osk = OskWidget::new();
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "the ");
    assert_eq!(osk.suggestions()[0].word, "\u{201c}teh\u{201d}");
    type_word(&mut osk, "tge");
    osk.apply_action(KeyAction::Char(",".into()));
    assert_eq!(osk.pending_text(), "the the,");
}

#[test]
fn backspace_after_a_correction_reverts_it_and_keeps_the_word() {
    let mut osk = OskWidget::new();
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    osk.take_output();
    osk.apply_action(KeyAction::Backspace);
    assert_eq!(
        osk.take_output(),
        vec![
            OskOutput::key(Symbol::BackSpace),
            OskOutput::key(Symbol::BackSpace),
            OskOutput::key(Symbol::BackSpace),
            OskOutput::key(Symbol::BackSpace),
            commit("teh "),
        ]
    );
    assert_eq!(osk.pending_text(), "teh ");
    // Kept as typed: the same word is no longer corrected.
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "teh teh ");
    osk.apply_action(KeyAction::Backspace);
    assert_eq!(osk.pending_text(), "teh teh");
}

#[test]
fn as_typed_chip_reverts_a_correction() {
    let mut osk = OskWidget::new();
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert!(osk.activate_suggestion(0));
    assert_eq!(osk.pending_text(), "teh ");
}

#[test]
fn words_the_user_has_typed_are_never_corrected() {
    let mut predictor = Predictor::english();
    predictor.learn_transition(None, "teh");
    let mut osk = OskWidget::new().with_predictor(predictor);
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "teh ");
}

#[test]
fn password_fields_are_never_autocorrected() {
    let mut osk = OskWidget::new().with_purpose(InputPurpose::Password);
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "teh ");
}

#[test]
fn sensitive_text_fields_are_never_autocorrected() {
    let mut osk = OskWidget::new();
    osk.set_content_hints(content_hint::SENSITIVE_DATA);
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "teh ");
}

#[test]
fn punctuation_commits_the_word_with_it() {
    let mut osk = OskWidget::new();
    osk.apply_action(KeyAction::Char("o".into()));
    osk.apply_action(KeyAction::Char("k".into()));
    osk.apply_action(KeyAction::Char(",".into()));
    osk.apply_action(KeyAction::Char(".".into()));
    assert_eq!(
        osk.take_output(),
        vec![pre("o"), pre("ok"), commit("ok,"), commit(".")]
    );
}

#[test]
fn enter_commits_the_word_before_the_return_key() {
    let mut osk = OskWidget::new();
    osk.apply_action(KeyAction::Char("g".into()));
    osk.apply_action(KeyAction::Char("o".into()));
    osk.apply_action(KeyAction::Enter);
    osk.apply_action(KeyAction::Enter);
    assert_eq!(
        osk.take_output(),
        vec![
            pre("g"),
            pre("go"),
            commit("go"),
            OskOutput::key(Symbol::Return),
            OskOutput::key(Symbol::Return),
        ]
    );
    assert_eq!(osk.pending_text(), "go\n\n");
}

#[test]
fn backspace_edits_the_preedit_then_becomes_a_key() {
    let mut osk = OskWidget::new();
    osk.apply_action(KeyAction::Char("a".into()));
    osk.apply_action(KeyAction::Space);
    osk.apply_action(KeyAction::Char("n".into()));
    osk.apply_action(KeyAction::Char("o".into()));
    osk.take_output();
    osk.apply_action(KeyAction::Backspace);
    osk.apply_action(KeyAction::Backspace);
    // Nothing composing: the next Backspace deletes committed text, which
    // only a key press can do.
    osk.apply_action(KeyAction::Backspace);
    osk.apply_action(KeyAction::Char("t".into()));
    assert_eq!(
        osk.take_output(),
        vec![
            pre("n"),
            pre(""),
            OskOutput::key(Symbol::BackSpace),
            pre("t"),
        ]
    );
    assert_eq!(osk.pending_text(), "at");
}

#[test]
fn shift_uppercases_next_letter_then_resets() {
    let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
    lay_out(&mut osk, 800.0, 360.0);
    assert_eq!(osk.shift_state(), ShiftState::Off);
    osk.activate_label("Shift");
    assert_eq!(osk.shift_state(), ShiftState::Shift);
    osk.activate_label("a"); // -> "A", one-shot shift resets
    osk.activate_label("b"); // -> "b"
    assert_eq!(osk.pending_text(), "Ab");
    assert_eq!(osk.shift_state(), ShiftState::Off);
}

#[test]
fn caps_lock_is_sticky() {
    let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
    lay_out(&mut osk, 800.0, 360.0);
    osk.activate_label("Shift"); // Off -> Shift
    osk.activate_label("Shift"); // Shift -> CapsLock
    assert_eq!(osk.shift_state(), ShiftState::CapsLock);
    osk.activate_label("a");
    osk.activate_label("b");
    assert_eq!(osk.pending_text(), "AB");
    assert_eq!(osk.shift_state(), ShiftState::CapsLock, "caps stays on");
}

#[test]
fn symbols_toggle_switches_layer() {
    let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
    lay_out(&mut osk, 800.0, 360.0);
    assert_eq!(osk.active_layout_name(), "qwerty");
    assert!(!osk.showing_symbols());
    osk.activate_label("?123");
    assert!(osk.showing_symbols());
    assert_eq!(osk.active_layout_name(), "symbols");
    // Re-layout (host would after structure_dirty) and type a digit.
    lay_out(&mut osk, 800.0, 360.0);
    assert!(osk.activate_label("1"));
    assert_eq!(osk.pending_text(), "1");
}

#[test]
fn toggle_raises_structure_dirty() {
    let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
    lay_out(&mut osk, 800.0, 360.0);
    osk.activate_label("?123");
    assert!(osk.take_structure_dirty());
    assert!(!osk.take_structure_dirty(), "flag is rising-edge");
}

#[test]
fn accessibility_operates_each_painted_key_and_announces_disabled_suggestions() {
    eprintln!("phase accessibility_exposes_and_operates_each_painted_key");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        let semantics = osk.accessibility().expect("visible keyboard semantics");
        let q = semantics
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("q"))
            .expect("q key semantic node");
        assert_eq!(q.role, Role::Button);
        assert!(q.bounds.is_some());
        let target = q.action_target.as_deref().expect("stable action target");
        assert!(
            ui::widget::controls::control_action_bindings(&osk)
                .iter()
                .any(|binding| {
                    binding.control_id == target
                        && matches!(
                            binding.kind,
                            ui::widget::ControlBindingKind::SemanticAction(_)
                        )
                })
        );
        assert!(osk.perform_semantic_action(
            target,
            &AccessibilityAction::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(osk.pending_text(), "q");
    }

    eprintln!("phase disabled_suggestions_are_announced_and_not_materialised");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        osk.set_suggestions_enabled(false);
        lay_out(&mut osk, 720.0, 360.0);
        assert!(osk.suggestions().is_empty());
        let node = osk.accessibility().expect("visible semantics");
        assert!(
            node.description
                .as_deref()
                .is_some_and(|text| text.contains("turned off"))
        );
    }
}

#[test]
fn shift_caps_and_symbol_layers_toggle_and_mark_structure_dirty() {
    eprintln!("phase shift_uppercases_next_letter_then_resets");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.shift_state(), ShiftState::Off);
        osk.activate_label("Shift");
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        osk.activate_label("a"); // -> "A", one-shot shift resets
        osk.activate_label("b"); // -> "b"
        assert_eq!(osk.pending_text(), "Ab");
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }

    eprintln!("phase caps_lock_is_sticky");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("Shift"); // Off -> Shift
        osk.activate_label("Shift"); // Shift -> CapsLock
        assert_eq!(osk.shift_state(), ShiftState::CapsLock);
        osk.activate_label("a");
        osk.activate_label("b");
        assert_eq!(osk.pending_text(), "AB");
        assert_eq!(osk.shift_state(), ShiftState::CapsLock, "caps stays on");
    }

    eprintln!("phase symbols_toggle_switches_layer");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        assert!(!osk.showing_symbols());
        osk.activate_label("?123");
        assert!(osk.showing_symbols());
        assert_eq!(osk.active_layout_name(), "symbols");
        // Re-layout (host would after structure_dirty) and type a digit.
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("1"));
        assert_eq!(osk.pending_text(), "1");
    }

    eprintln!("phase toggle_raises_structure_dirty");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("?123");
        assert!(osk.take_structure_dirty());
        assert!(!osk.take_structure_dirty(), "flag is rising-edge");
    }
}

#[test]
fn pointer_and_toolbar_taps_route_to_their_targets() {
    eprintln!("phase pointer_tap_routes_to_key_under_cursor");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);

        // Pick the rect of the 'q' key (row 0, col 0) and aim at its centre.
        let target = KeyId::new(0, 0);
        let rect = osk
            .key_geometry()
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, r)| *r)
            .expect("q key laid out");
        let c = rect.center();

        let device = winit::event::DeviceId::dummy();
        let dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
            ui::testing::dispatch(osk, &ev);
        };

        dispatch(
            &mut osk,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (c.x, c.y).into(),
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert_eq!(osk.pending_text(), "q", "tap over 'q' typed q");
    }

    eprintln!("phase toolbar_tap_emits_action");
    {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut osk = {
            let seen = seen.clone();
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .on_toolbar(move |a| seen.borrow_mut().push(a))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_tool(0); // default[0] == Paste
        assert_eq!(osk.take_toolbar_actions(), vec![OskToolbarAction::Paste]);
        assert_eq!(*seen.borrow(), vec![OskToolbarAction::Paste]);
    }

    eprintln!("phase layout_action_switches_active_letter_layout");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.apply_action(KeyAction::Layout("azerty".into()));
        assert_eq!(osk.active_layout_name(), "azerty");
        lay_out(&mut osk, 800.0, 360.0);
        // AZERTY row 0 starts with 'a'.
        assert!(osk.activate_label("a"));
        assert_eq!(osk.pending_text(), "a");
    }

    eprintln!("phase letter_layout_selects_the_word_language");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        assert_eq!(osk.language(), Language::English);
        lay_out(&mut osk, 800.0, 360.0);
        osk.apply_action(KeyAction::Layout("azerty".into()));
        assert_eq!(osk.language(), Language::French);
        lay_out(&mut osk, 800.0, 360.0);
        for letter in ["t", "r", "e", "s"] {
            assert!(osk.activate_label(letter));
        }
        let words: Vec<&str> = osk.suggestions().iter().map(|s| s.word.as_str()).collect();
        assert!(words.contains(&"très"), "French completes tres: {words:?}");
        osk.apply_action(KeyAction::Layout("qwerty".into()));
        assert_eq!(osk.language(), Language::English);
    }

    eprintln!("phase hidden_keyboard_ignores_pointer_events");
    {
        // A hidden keyboard must not consume taps meant for the app behind it:
        // `handle_event` returns early and `handled` stays false.
        let mut osk =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        lay_out(&mut osk, 800.0, 360.0);

        let device = winit::event::DeviceId::dummy();
        // Aim where the 'q' key would be on a visible keyboard.
        let handled = ui::testing::dispatch(
            &mut osk,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert!(!handled, "a hidden keyboard does not consume pointer input");
        assert_eq!(osk.pending_text(), "");
    }
}

#[test]
fn configured_layout_order_controls_the_actual_language_key() {
    let mut keyboard = OskWidget::new();
    keyboard.present_preferences(&crate::resource::OskPreferences {
        layout: "qwerty".into(),
        layouts: vec!["qwerty".into()],
        ..Default::default()
    });
    let action = keyboard
        .current_layout()
        .rows
        .iter()
        .flatten()
        .find_map(|key| matches!(key.action, KeyAction::Layout(_)).then_some(key.action.clone()))
        .unwrap();
    assert_eq!(action, KeyAction::Layout("qwerty".into()));
    keyboard.apply_action(action);
    assert_eq!(keyboard.preferred_text_layout(), "qwerty");
    assert!(keyboard.take_preference_edits().is_empty());
    keyboard.apply_action(KeyAction::SymbolsToggle);
    assert_eq!(keyboard.active_layout_name(), "symbols");
    keyboard.apply_action(KeyAction::Layout("qwerty".into()));
    assert_eq!(keyboard.active_layout_name(), "qwerty");
    keyboard.present_preferences(&crate::resource::OskPreferences {
        layout: "qwerty".into(),
        layouts: vec!["azerty".into(), "qwerty".into()],
        ..Default::default()
    });
    let action = keyboard
        .current_layout()
        .rows
        .iter()
        .flatten()
        .find_map(|key| matches!(key.action, KeyAction::Layout(_)).then_some(key.action.clone()))
        .unwrap();
    keyboard.apply_action(action);
    assert_eq!(keyboard.preferred_text_layout(), "azerty");
}

#[test]
fn purpose_changes_swap_layouts_and_clear_stale_state() {
    eprintln!("phase set_purpose_swaps_active_layout_and_marks_dirty");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        osk.set_purpose(InputPurpose::Telephone);
        assert_eq!(osk.purpose(), InputPurpose::Telephone);
        assert_eq!(osk.active_layout_name(), "phone");
        assert!(osk.take_structure_dirty(), "purpose change relays out");
    }

    eprintln!("phase switching_purpose_then_back_registers_layouts_on_demand");
    {
        // Tilo only preloads qwerty/azerty/symbols; the specialised layouts must
        // be registered the first time a purpose needs them.
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        osk.set_purpose(InputPurpose::Email);
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "email");
        assert!(osk.activate_label("@"));
        osk.set_purpose(InputPurpose::Text);
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
    }

    eprintln!("phase purpose_change_clears_stale_shift");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("Shift");
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        // Moving to a number pad must drop the now-meaningless shift state.
        osk.set_purpose(InputPurpose::Number);
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }
}

#[test]
fn initial_purpose_selects_matching_layout_and_names_round_trip() {
    eprintln!("phase with_purpose_starts_on_the_matching_layout");
    {
        let osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
            .with_purpose(InputPurpose::Pin);
        assert_eq!(osk.purpose(), InputPurpose::Pin);
        assert_eq!(osk.active_layout_name(), "pin");
    }

    eprintln!("phase numeric_purpose_types_digits_and_has_no_letters");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        osk.set_purpose(InputPurpose::Number);
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("1"));
        assert!(osk.activate_label("2"));
        assert!(osk.activate_label("3"));
        assert_eq!(osk.pending_text(), "123");
        // There is no Shift key to tap on a number pad.
        assert!(!osk.activate_label("Shift"), "no Shift on the number pad");
    }

    eprintln!("phase input_purpose_name_round_trips");
    {
        use InputPurpose::*;
        for p in [
            Text, Number, Decimal, Telephone, Url, Email, Pin, Password, Date, Time,
        ] {
            let name = input_purpose_name(p);
            assert_eq!(
                input_purpose_from_name(name),
                Some(p),
                "{p:?} should round-trip through its name {name:?}"
            );
        }
    }
}

#[test]
fn offscreen_render_and_geometry_follow_layout_and_suggestions() {
    eprintln!("phase numeric_layout_renders_offscreen");
    {
        // A non-letter layout must lay out and draw without panicking even
        // though it has no Shift / symbols affordances.
        use ui::offscreen::OffscreenRenderer;
        use vello::peniko::Color;

        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
            .with_purpose(InputPurpose::Telephone);
        lay_out(&mut osk, 720.0, 360.0);
        assert!(!osk.key_geometry().is_empty(), "dialpad laid out keys");

        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("osk_dialpad.png");
        let mut renderer = OffscreenRenderer::new(720, 360);
        renderer.render_png(&scene, Color::BLACK, &out);
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.len() > 1000, "rendered a non-trivial PNG");
    }

    eprintln!("phase offscreen_render_includes_suggestion_bar");
    {
        // A full offscreen frame with a populated suggestion bar must produce
        // non-empty pixels (the bar + chips draw).
        use ui::offscreen::OffscreenRenderer;
        use vello::peniko::Color;

        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path));
        assert!(!osk.suggestions().is_empty());
        lay_out(&mut osk, 720.0, 360.0);

        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("osk_suggestion_bar.png");
        let mut renderer = OffscreenRenderer::new(720, 360);
        renderer.render_png(&scene, Color::BLACK, &out);
        let bytes = std::fs::read(&out).unwrap();
        assert!(
            bytes.len() > 1000,
            "rendered a non-trivial PNG ({} bytes)",
            bytes.len()
        );
    }

    eprintln!("phase suggestion_bar_geometry_matches_suggestion_count");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        // Cold start: the bar shows common-word predictions, so geometry is
        // populated and chips fit within the suggestion band.
        let path = path_through(&osk, "hello");
        osk.swipe(&path);
        // Re-layout so suggestion_rects is rebuilt from the new suggestions.
        lay_out(&mut osk, 720.0, 360.0);
        let n = osk.suggestions().len();
        assert!(n > 0);
        assert_eq!(osk.suggestion_geometry().len(), n, "one rect per chip");
        let geom = osk.suggestion_geometry();
        let leftmost = geom.iter().map(|(_, r)| r.x0).fold(f64::MAX, f64::min);
        let rightmost = geom.iter().map(|(_, r)| r.x1).fold(f64::MIN, f64::max);
        let gutter = keyboard_gutter(&osk.spacing);
        assert!((leftmost - gutter).abs() < 1.0, "chips start at the gutter");
        assert!(
            (rightmost - (720.0 - gutter)).abs() < 1.0,
            "chips fill the width inside the gutter"
        );
    }

    eprintln!("phase responsive_content_caps_on_desktop_but_keeps_tablet_width");
    {
        let mut desktop = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut desktop, 1_920.0, 480.0);
        assert_eq!(desktop.content_rect.width(), MAX_KEYBOARD_CONTENT_WIDTH);
        assert!(desktop.content_rect.x0 > 0.0, "desktop keyboard is centred");

        let mut tablet = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut tablet, 768.0, 420.0);
        let gutter = keyboard_gutter(&tablet.spacing);
        assert_eq!(tablet.content_rect.width(), 768.0 - 2.0 * gutter);
        assert_eq!(tablet.content_rect.x0, gutter);
    }
}

// --- swipe + prediction + suggestion bar -----------------------

/// Build a dense pointer path through the live key centres of `word`,
/// using the widget's own laid-out geometry (the same source of truth the
/// decoder consumes).
fn path_through(osk: &OskWidget, word: &str) -> Vec<Point> {
    // letter -> key centre from the current frame's geometry.
    let layout = osk.current_layout();
    let mut centres: HashMap<char, Point> = HashMap::new();
    for (id, r) in osk.key_geometry() {
        let Some(k) = layout.key(*id) else { continue };
        let KeyAction::Char(s) = &k.action else {
            continue;
        };
        if let Some(c) = s.chars().next()
            && c.is_alphabetic()
            && s.chars().count() == 1
        {
            centres
                .entry(c.to_ascii_lowercase())
                .or_insert_with(|| r.center());
        }
    }
    let mut pts = Vec::new();
    for c in word.chars() {
        let centre = centres[&c];
        if let Some(prev) = pts.last().copied() {
            let prev: Point = prev;
            for k in 1..=5 {
                let t = f64::from(k) / 5.0;
                pts.push(Point::new(
                    prev.x + (centre.x - prev.x) * t,
                    prev.y + (centre.y - prev.y) * t,
                ));
            }
        } else {
            pts.push(centre);
        }
    }
    pts
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn swipes_and_suggestion_chips_edit_the_current_word() {
    eprintln!("phase swipe_inserts_top_candidate_and_fills_suggestion_bar");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path), "swipe decoded a word");
        assert_eq!(osk.pending_text(), "quick");
        assert_eq!(osk.take_output(), vec![pre("quick")]);
        let words: Vec<&str> = osk.suggestions().iter().map(|s| s.word.as_str()).collect();
        assert!(!words.is_empty(), "suggestion bar populated");
        assert!(
            words.contains(&"quick"),
            "candidates include quick: {words:?}"
        );
    }

    eprintln!("phase tapping_suggestion_chip_replaces_current_word");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        // Type a prefix that yields completions, e.g. "th" -> the/this/that...
        osk.activate_label("t");
        osk.activate_label("h");
        assert_eq!(osk.current_word(), "th");
        let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
        assert!(!chips.is_empty(), "completions shown for 'th': {chips:?}");
        // Tap the top chip; it replaces the in-progress word with the full
        // word and a space, committed in one step.
        let top = chips[0].clone();
        osk.take_output();
        assert!(osk.activate_suggestion(0));
        assert_eq!(osk.take_output(), vec![commit(&format!("{top} "))]);
        assert_eq!(osk.pending_text(), format!("{top} "));
        assert_eq!(osk.current_word(), "", "word committed after chip tap");
    }

    eprintln!("phase next_word_chip_inserts_word_and_space");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        // Commit "of" so the bar fills with next-word predictions ("the" tops).
        osk.activate_label("o");
        osk.activate_label("f");
        osk.activate_label("space");
        let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
        assert!(
            chips.contains(&"the".to_string()),
            "next-word after 'of': {chips:?}"
        );
        // Find and tap the "the" chip; it inserts "the ".
        let idx = chips.iter().position(|w| w == "the").unwrap();
        assert!(osk.activate_suggestion(idx));
        assert_eq!(osk.pending_text(), "of the ");
    }

    eprintln!("phase short_tap_is_not_treated_as_a_swipe");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);

        let target = KeyId::new(0, 0); // 'q'
        let rect = osk
            .key_geometry()
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, r)| *r)
            .unwrap();
        let c = rect.center();

        let device = winit::event::DeviceId::dummy();
        let dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
            ui::testing::dispatch(osk, &ev);
        };

        // Press, a tiny jitter (1px), release — all on 'q'. This must type
        // exactly "q", NOT decode a swipe.
        dispatch(
            &mut osk,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (c.x, c.y).into(),
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (c.x + 1.0, c.y + 0.5).into(),
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert_eq!(
            osk.pending_text(),
            "q",
            "short tap typed 'q', not a swiped word"
        );
    }

    eprintln!("phase pointer_swipe_decodes_through_keys");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "the");
        let start = *path.first().unwrap();

        let device = winit::event::DeviceId::dummy();
        let dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
            ui::testing::dispatch(osk, &ev);
        };

        // Hover the first key, press, drag along the whole path, release.
        dispatch(
            &mut osk,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (start.x, start.y).into(),
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        for p in &path {
            dispatch(
                &mut osk,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (p.x, p.y).into(),
                },
            );
        }
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert_eq!(
            osk.pending_text(),
            "the",
            "pointer swipe through t-h-e typed 'the'"
        );
    }
}

#[test]
fn chip_corrects_a_swiped_word_in_place() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let path = path_through(&osk, "quick");
    assert!(osk.swipe(&path));
    let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
    assert!(chips.len() > 1, "swipe offers alternatives: {chips:?}");
    osk.take_output();
    assert!(osk.activate_suggestion(1));
    assert_eq!(osk.take_output(), vec![pre(&chips[1])]);
    assert_eq!(osk.pending_text(), chips[1]);
    assert_eq!(osk.suggestions().len(), chips.len());
}

#[test]
fn next_swipe_commits_the_previous_word_with_a_space() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let first = path_through(&osk, "quick");
    let second = path_through(&osk, "the");
    assert!(osk.swipe(&first));
    let swiped = osk.current_word().to_string();
    assert!(osk.swipe(&second));
    let out = osk.take_output();
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[1], commit(&format!("{swiped} ")));
    assert!(matches!(&out[2], OskOutput::SetPreedit(w) if !w.is_empty()));
}

#[test]
fn typing_after_a_swipe_starts_a_new_word() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let path = path_through(&osk, "quick");
    assert!(osk.swipe(&path));
    let swiped = osk.current_word().to_string();
    osk.take_output();
    osk.apply_action(KeyAction::Char("a".into()));
    assert_eq!(
        osk.take_output(),
        vec![commit(&format!("{swiped} ")), pre("a")]
    );
}

#[test]
fn backspace_removes_a_swiped_word_whole() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let path = path_through(&osk, "quick");
    assert!(osk.swipe(&path));
    osk.take_output();
    osk.apply_action(KeyAction::Backspace);
    assert_eq!(osk.take_output(), vec![pre("")]);
    assert_eq!(osk.pending_text(), "");
}

#[test]
fn swipe_replaces_a_partly_typed_word() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.apply_action(KeyAction::Char("x".into()));
    let path = path_through(&osk, "quick");
    assert!(osk.swipe(&path));
    let out = osk.take_output();
    assert_eq!(out[0], pre("x"));
    assert_eq!(out.len(), 2, "no commit of the replaced prefix: {out:?}");
    assert_eq!(osk.pending_text(), osk.current_word());
}

#[test]
fn password_field_never_composes_suggests_or_learns() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.set_purpose(InputPurpose::Password);
    let before = osk.predictor.learned_context_count();
    for c in ["h", "u", "n", "t", "e", "r"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    osk.apply_action(KeyAction::Space);
    for c in ["t", "w", "o"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    osk.apply_action(KeyAction::Enter);
    assert_eq!(osk.predictor.learned_context_count(), before);
    assert!(osk.suggestions().is_empty());
    assert_eq!(osk.current_word(), "");
    assert!(
        osk.take_output()
            .iter()
            .all(|o| !matches!(o, OskOutput::SetPreedit(_))),
        "a secret is never shown as preedit"
    );
    let path = path_through(&osk, "quick");
    assert!(!osk.swipe(&path), "no swiping into a secret field");
}

#[test]
fn sensitive_hint_is_secret_in_any_purpose() {
    let mut osk = OskWidget::new();
    osk.apply_action(KeyAction::Char("a".into()));
    osk.set_content_hints(content_hint::SENSITIVE_DATA);
    assert!(osk.is_secret_field());
    assert_eq!(osk.current_word(), "");
    assert_eq!(osk.take_output().last(), Some(&pre("")));
    assert!(osk.suggestions().is_empty());
    let before = osk.predictor.learned_context_count();
    osk.apply_action(KeyAction::Char("b".into()));
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.predictor.learned_context_count(), before);
}

#[test]
fn blur_clears_typed_text_traces() {
    let mut osk = OskWidget::new();
    osk.set_purpose(InputPurpose::Password);
    osk.apply_action(KeyAction::Char("s".into()));
    osk.follow_input_method(field_changed());
    assert_eq!(osk.pending_text(), "");
    assert!(osk.take_output().is_empty());
    assert!(osk.take_key_actions().is_empty());
}

#[test]
fn auto_capitalization_shifts_at_sentence_starts() {
    let mut osk = OskWidget::new();
    osk.follow_input_method(ImTransition {
        hints_changed: Some(content_hint::AUTO_CAPITALIZATION),
        ..ImTransition::default()
    });
    assert_eq!(osk.shift_state(), ShiftState::Shift);
    lay_out(&mut osk, 800.0, 360.0);
    for label in ["h", "i", ".", "space", "o", "k", "space"] {
        osk.activate_label(label);
    }
    assert_eq!(osk.pending_text(), "Hi. Ok ");
    assert_eq!(osk.shift_state(), ShiftState::Off);

    osk.set_content_hints(content_hint::AUTO_CAPITALIZATION | content_hint::LOWERCASE);
    osk.follow_input_method(field_changed());
    assert_eq!(osk.shift_state(), ShiftState::Off);
}

#[test]
fn field_change_drops_the_composition() {
    let mut osk = OskWidget::new();
    osk.apply_action(KeyAction::Char("h".into()));
    osk.follow_input_method(field_changed());
    assert_eq!(osk.current_word(), "");
    osk.apply_action(KeyAction::Backspace);
    assert_eq!(
        osk.take_output().last(),
        Some(&OskOutput::key(Symbol::BackSpace))
    );
}

#[test]
fn visibility_defaults_toggle_and_collapse_height() {
    eprintln!("phase keyboard_is_visible_by_default");
    {
        assert!(
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).is_visible()
        );
    }

    eprintln!("phase hidden_builder_starts_invisible");
    {
        assert!(
            !OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .hidden()
                .is_visible()
        );
    }

    eprintln!("phase hidden_keyboard_takes_zero_height");
    {
        // A visible keyboard occupies its intrinsic height; when the compositor
        // has hidden it the widget requests no height and paints nothing.
        let mut visible = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut visible, 720.0, 360.0);
        assert!(visible.get_rect().height() > 0.0);

        let mut hidden =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        lay_out(&mut hidden, 720.0, 360.0);
        assert_eq!(hidden.get_rect().height(), 0.0);
    }

    eprintln!("phase set_visible_toggles_and_marks_structure_dirty");
    {
        let mut osk =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        assert!(!osk.is_visible());
        osk.set_visible(true);
        assert!(osk.is_visible());
        assert!(osk.take_structure_dirty(), "showing relays out");
        // Idempotent: setting the same state again is a no-op (no relayout).
        osk.set_visible(true);
        assert!(!osk.take_structure_dirty(), "no-op when unchanged");
        osk.set_visible(false);
        assert!(!osk.is_visible());
        assert!(osk.take_structure_dirty(), "hiding relays out");
    }
}

#[test]
fn the_input_method_drives_visibility_and_purpose_without_blocking() {
    eprintln!("phase keyboard_app_id_is_the_well_known_id");
    {
        // The compositor recognises the keyboard by this exact app-id to keep
        // it out of the tile layout and own its visibility. If this drifts, the
        // keyboard would be treated as an ordinary window — so pin the value.
        assert_eq!(crate::KEYBOARD_APP_ID, "so.tilo.osk");
    }

    eprintln!("phase input_method_focus_drives_visibility");
    {
        let mut osk =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        let shown = |visible| ImTransition {
            visibility_changed: Some(visible),
            ..ImTransition::default()
        };

        osk.follow_input_method(shown(true));
        assert!(osk.is_visible());

        osk.follow_input_method(shown(false));
        assert!(!osk.is_visible());
    }

    eprintln!("phase input_method_purpose_switches_layout");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        let purpose = |purpose| ImTransition {
            purpose_changed: Some(purpose),
            ..ImTransition::default()
        };

        osk.follow_input_method(purpose(InputPurpose::Telephone));
        assert_eq!(osk.purpose(), InputPurpose::Telephone);
        assert_eq!(osk.active_layout_name(), "phone");

        osk.follow_input_method(purpose(InputPurpose::Email));
        assert_eq!(osk.purpose(), InputPurpose::Email);
        assert_eq!(osk.active_layout_name(), "email");
    }

    eprintln!("phase an_unchanged_focus_changes_nothing");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        osk.apply_action(KeyAction::Char("h".into()));
        osk.follow_input_method(ImTransition::default());
        assert_eq!(osk.purpose(), InputPurpose::Text);
        assert!(osk.is_visible());
        assert_eq!(osk.current_word(), "h");
    }

    eprintln!("phase bounded_diagnostic_buffers_report_overflow_without_blocking_delivery");
    {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        osk.apply_action(KeyAction::Char("x".repeat(MAX_PENDING_TEXT_BYTES + 11)));
        assert_eq!(osk.pending_text().len(), MAX_PENDING_TEXT_BYTES);
        assert_eq!(osk.pending_text_dropped_bytes(), 11);
        assert_eq!(
            osk.take_output().len(),
            1,
            "an overlong word is committed, not composed"
        );
        for _ in 0..(MAX_OUTPUT_ENTRIES + 3) {
            osk.apply_action(KeyAction::Enter);
        }
        assert_eq!(osk.output.len(), MAX_OUTPUT_ENTRIES);
        assert_eq!(osk.dropped_outputs(), 3);

        for _ in 0..(MAX_KEY_LOG_ENTRIES + 5) {
            osk.apply_action(KeyAction::Shift);
        }
        assert_eq!(osk.key_log.len(), MAX_KEY_LOG_ENTRIES);
        assert!(osk.dropped_key_actions() >= 6);
    }
}

fn field_changed() -> ImTransition {
    ImTransition {
        field_changed: true,
        ..ImTransition::default()
    }
}

fn semantic(osk: &mut OskWidget, target: &str, action: &AccessibilityAction) -> bool {
    osk.perform_semantic_action(
        target,
        action,
        &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    )
}

fn activate(osk: &mut OskWidget, target: &str) -> bool {
    semantic(osk, target, &AccessibilityAction::Activate)
}

fn open_panel(osk: &mut OskWidget) {
    lay_out(osk, 720.0, 360.0);
    assert!(
        osk.activate_label("Emoji"),
        "the emoji key is on the letters"
    );
    assert_eq!(osk.emoji_panel().mode(), PanelMode::Browse);
}

fn named_nodes(osk: &OskWidget) -> Vec<(String, String)> {
    osk.accessibility()
        .into_iter()
        .flat_map(|n| n.children)
        .flat_map(|n| std::iter::once(n.clone()).chain(n.children))
        .filter_map(|n| Some((n.name?, n.action_target?)))
        .collect()
}

#[test]
fn accessible_password_backspace_edits_committed_text() {
    let mut osk = OskWidget::new().with_purpose(InputPurpose::Password);
    lay_out(&mut osk, 720.0, 360.0);
    let names = named_nodes(&osk);
    let (_, letter) = names
        .iter()
        .find(|(name, _)| name == "a")
        .expect("letter key");
    let (_, backspace) = names
        .iter()
        .find(|(name, _)| name == "Backspace")
        .expect("editing key");
    assert!(activate(&mut osk, letter));
    assert_eq!(osk.take_output(), vec![commit("a")]);
    assert_eq!(osk.pending_text(), "a");
    assert!(activate(&mut osk, backspace));
    assert_eq!(osk.take_output(), vec![OskOutput::key(Symbol::BackSpace)]);
    assert_eq!(osk.pending_text(), "");
    assert!(
        osk.current_word().is_empty(),
        "secret text is never composed"
    );
}

#[test]
fn emoji_key_opens_the_panel_in_place_of_the_keys() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    assert!(osk.key_geometry().is_empty(), "the grid replaces the keys");
    assert!(
        osk.suggestion_geometry().is_empty(),
        "the search field replaces the strip"
    );
    assert!(
        !osk.claims_raw_touch(osk.rect.center()),
        "the grid takes taps and drags"
    );
    let names = named_nodes(&osk);
    assert!(names.iter().any(|(n, _)| n == &i18n::t("osk-emoji-search")));
    assert!(names.iter().any(|(_, t)| t == "osk:emoji-group:0"));
    assert!(activate(&mut osk, "osk:emoji-letters"));
    assert_eq!(osk.emoji_panel().mode(), PanelMode::Closed);
    assert!(!osk.key_geometry().is_empty(), "the keys are back");
}

#[test]
fn emoji_key_is_named_for_assistive_tech() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let names = named_nodes(&osk);
    assert!(
        names
            .iter()
            .any(|(n, t)| n == &i18n::t("osk-emoji-key") && t.starts_with("osk:key:"))
    );
}

#[test]
fn picking_an_emoji_commits_it_after_the_composing_word() {
    let mut osk = OskWidget::new();
    type_word(&mut osk, "hi");
    let _ = osk.take_output();
    open_panel(&mut osk);
    let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
    assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
    assert_eq!(osk.take_output(), vec![commit("hi"), commit(&first)]);
    assert_eq!(osk.emoji_recents(), [first]);
    assert_eq!(
        osk.emoji_panel().mode(),
        PanelMode::Browse,
        "the panel stays for more"
    );
}

#[test]
fn recents_lead_the_grid() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    assert!(activate(&mut osk, "osk:emoji-group:3"));
    let second = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[1].clone();
    assert!(activate(&mut osk, &format!("osk:emoji:{second}")));
    let names = named_nodes(&osk);
    let first_emoji = names
        .iter()
        .find(|(_, t)| t.starts_with("osk:emoji:"))
        .map(|(_, t)| t.clone());
    assert_eq!(first_emoji, Some(format!("osk:emoji:{second}")));
}

#[test]
fn search_is_typed_with_the_keys_and_enter_picks_the_best_match() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    assert!(activate(&mut osk, "osk:emoji-search"));
    assert_eq!(osk.emoji_panel().mode(), PanelMode::Search);
    assert!(!osk.key_geometry().is_empty(), "the keys type the query");
    for c in ["p", "i", "z", "z", "a"] {
        assert!(osk.activate_label(c));
    }
    assert_eq!(osk.emoji_panel().query(), "pizza");
    assert!(
        osk.take_output().is_empty(),
        "the query never reaches the field"
    );
    assert!(named_nodes(&osk).iter().any(|(_, t)| t == "osk:emoji:🍕"));
    assert!(osk.activate_label("Backspace"));
    assert_eq!(osk.emoji_panel().query(), "pizz");
    assert!(osk.activate_label("a"));
    assert!(osk.activate_label("Return"));
    assert_eq!(osk.take_output(), vec![commit("🍕")]);
    assert!(activate(&mut osk, "osk:emoji-end-search"));
    assert_eq!(osk.emoji_panel().mode(), PanelMode::Browse);
    assert_eq!(osk.emoji_panel().query(), "");
}

#[test]
fn long_press_offers_skin_tones_and_commits_the_toned_form() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    // Thumbs up is in People & Body; search puts it on screen.
    assert!(semantic(
        &mut osk,
        "osk:emoji-search",
        &AccessibilityAction::SetValue("thumbs up".into()),
    ));
    assert!(!semantic(
        &mut osk,
        "osk:emoji:🍕",
        &AccessibilityAction::ShowContextMenu
    ));
    assert!(semantic(
        &mut osk,
        "osk:emoji:👍",
        &AccessibilityAction::ShowContextMenu
    ));
    assert_eq!(osk.emoji_panel().tones_for(), Some("👍"));
    let toned = emoji::Catalogue::english()
        .with_skin_tone("👍", Some(emoji::SkinTone::MediumDark))
        .unwrap()
        .to_owned();
    assert!(activate(&mut osk, &format!("osk:emoji-tone:{toned}")));
    assert_eq!(osk.take_output(), vec![commit(&toned)]);
    assert_eq!(osk.emoji_panel().tones_for(), None);
    assert_eq!(osk.emoji_recents(), [toned]);
}

#[test]
fn emoji_without_tones_open_no_tone_row() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
    assert!(!semantic(
        &mut osk,
        &format!("osk:emoji:{first}"),
        &AccessibilityAction::ShowContextMenu,
    ));
}

#[test]
fn a_word_that_names_an_emoji_offers_it_and_the_chip_commits_it() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    type_word(&mut osk, "pizza");
    let chip = osk
        .suggestions()
        .iter()
        .position(|s| s.emoji && s.word == "🍕")
        .expect("an emoji chip for pizza");
    assert!(osk.suggestions().len() <= MAX_SUGGESTIONS);
    let _ = osk.take_output();
    assert!(osk.activate_suggestion(chip));
    assert_eq!(osk.take_output(), vec![commit("pizza "), commit("🍕 ")]);
    assert_eq!(osk.pending_text(), "pizza 🍕 ");
    assert_eq!(osk.emoji_recents(), ["🍕".to_owned()]);
}

#[test]
fn emoji_chips_are_named_for_the_emoji() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    type_word(&mut osk, "pizza");
    lay_out(&mut osk, 720.0, 360.0);
    let expected = i18n::t_args("osk-emoji-insert", &[("name", "pizza".into())]);
    assert!(named_nodes(&osk).iter().any(|(n, _)| n == &expected));
}

#[test]
fn password_fields_get_no_emoji_suggestions_or_recents() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.set_purpose(InputPurpose::Password);
    type_word(&mut osk, "pizza");
    assert!(osk.suggestions().iter().all(|s| !s.emoji));
    osk.set_purpose(InputPurpose::Text);
    osk.set_content_hints(content_hint::SENSITIVE_DATA);
    type_word(&mut osk, "pizza");
    assert!(osk.suggestions().is_empty());
    open_panel(&mut osk);
    let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
    assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
    assert!(
        osk.emoji_recents().is_empty(),
        "a secret field leaves no recents"
    );
}

#[test]
fn recents_follow_the_personalization_preference() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
    assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
    assert_eq!(osk.emoji_recents().len(), 1);
    osk.clear_personalization();
    assert!(osk.emoji_recents().is_empty());
    osk.set_personalization_enabled(false);
    assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
    assert!(osk.emoji_recents().is_empty());
}

#[test]
fn picked_emoji_are_saved_as_preferences_and_presented_back() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
    assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
    let edits = osk.take_preference_edits();
    assert_eq!(edits, [SetPreference::EmojiRecents(vec![first.clone()])]);
    assert!(osk.take_preference_edits().is_empty(), "taken once");

    let mut restarted = OskWidget::new();
    restarted.present_preferences(&crate::resource::OskPreferences {
        emoji_recents: vec![first.clone()],
        ..crate::resource::OskPreferences::default()
    });
    assert_eq!(restarted.emoji_recents(), [first]);
    assert!(
        restarted.take_preference_edits().is_empty(),
        "presenting saved preferences saves nothing"
    );
}

#[test]
fn every_panel_face_paints() {
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    let mut paint = |osk: &mut OskWidget| {
        let mut scene = Scene::new();
        osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(!scene.encoding().is_empty());
    };
    paint(&mut osk);
    assert!(semantic(
        &mut osk,
        "osk:emoji-search",
        &AccessibilityAction::SetValue("thumbs".into()),
    ));
    paint(&mut osk);
    assert!(semantic(
        &mut osk,
        "osk:emoji:👍",
        &AccessibilityAction::ShowContextMenu,
    ));
    paint(&mut osk);
}

#[test]
fn a_field_change_closes_the_panel() {
    let mut osk = OskWidget::new();
    open_panel(&mut osk);
    osk.follow_input_method(field_changed());
    assert_eq!(osk.emoji_panel().mode(), PanelMode::Closed);
}

fn finger(osk: &mut OskWidget, id: u64, phase: TouchPhase, at: Point) {
    let touch = Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(at.x, at.y),
        force: None,
        id,
    };
    let mut handled = false;
    osk.handle_touch(&touch, 1.0, &mut handled);
}

fn key_rect(osk: &OskWidget, label: &str) -> (KeyId, Rect) {
    let layout = osk.current_layout();
    osk.key_geom
        .iter()
        .find(|(id, _)| layout.key(*id).is_some_and(|k| k.label == label))
        .copied()
        .unwrap()
}

fn touch_swipe(osk: &mut OskWidget, id: u64, word: &str) {
    let path = path_through(osk, word);
    finger(osk, id, TouchPhase::Started, path[0]);
    for &point in path.iter().skip(1).take(path.len().saturating_sub(2)) {
        finger(osk, id, TouchPhase::Moved, point);
    }
    // End owns the final endpoint, rather than repeating a Moved sample.
    finger(osk, id, TouchPhase::Ended, *path.last().unwrap());
}

#[test]
fn swipe_correction_preserves_the_phrase_and_the_next_gesture() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    touch_swipe(&mut osk, 1, "the");
    assert_eq!(osk.current_word(), "the");
    touch_swipe(&mut osk, 2, "quick");
    assert_eq!(osk.current_word(), "quick");
    assert_eq!(osk.pending_text(), "the quick");
    let (index, candidate) = osk
        .suggestions
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate.word != "quick")
        .expect("alternate candidate");
    let correction = candidate.word.clone();
    let target = format!("osk:suggestion:{index}:{correction}");
    assert!(semantic(&mut osk, &target, &AccessibilityAction::Activate));
    // Swipe corrections remain preedit so another candidate can still
    // replace them. The next gesture commits exactly one separating space.
    assert_eq!(osk.pending_text(), format!("the {correction}"));
    assert_eq!(osk.current_word(), correction);
    assert!(osk.word_from_swipe);
    osk.take_output();
    touch_swipe(&mut osk, 3, "brown");
    assert_eq!(
        osk.take_output(),
        vec![commit(&format!("{correction} ")), pre(osk.current_word()),]
    );
    assert!(!osk.current_word().is_empty());
    assert_eq!(
        osk.pending_text(),
        format!("the {correction} {}", osk.current_word())
    );
    assert!(
        osk.active_swipe_path().is_empty(),
        "release clears the trail"
    );
}

#[test]
fn swipe_typing_obeys_sentence_capitalization_and_caps_lock() {
    let mut osk = OskWidget::new();
    osk.set_content_hints(content_hint::AUTO_CAPITALIZATION);
    lay_out(&mut osk, 720.0, 360.0);
    touch_swipe(&mut osk, 1, "the");
    assert_eq!(osk.current_word(), "The");
    assert_eq!(osk.shift_state(), ShiftState::Off);
    assert!(
        osk.suggestions
            .iter()
            .all(|s| s.emoji || s.word.chars().next().unwrap().is_uppercase())
    );
    touch_swipe(&mut osk, 2, "quick");
    assert_eq!(osk.pending_text(), "The quick");
    osk.apply_action(KeyAction::Char(".".into()));
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.shift_state(), ShiftState::Shift);
    touch_swipe(&mut osk, 3, "the");
    assert_eq!(osk.current_word(), "The");
    osk.apply_action(KeyAction::Shift);
    osk.apply_action(KeyAction::Shift);
    assert_eq!(osk.shift_state(), ShiftState::CapsLock);
    touch_swipe(&mut osk, 4, "quick");
    assert_eq!(osk.current_word(), "QUICK");
    assert_eq!(osk.shift_state(), ShiftState::CapsLock);
    assert_eq!(case_swipe_word("été", ShiftState::Shift), "Été");
}

#[test]
fn gesture_feedback_is_transient_solo_and_never_secret() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let path = path_through(&osk, "quick");
    finger(&mut osk, 1, TouchPhase::Started, path[0]);
    for &point in path.iter().skip(1) {
        finger(&mut osk, 1, TouchPhase::Moved, point);
    }
    assert_eq!(osk.active_swipe_path(), path);
    let last = *path.last().unwrap();
    osk.reset_composition();
    assert!(osk.active_swipe_path().is_empty());
    finger(&mut osk, 1, TouchPhase::Ended, last);
    assert!(osk.pending_text().is_empty());
    osk.set_purpose(InputPurpose::Password);
    finger(&mut osk, 2, TouchPhase::Started, path[0]);
    for &point in path.iter().skip(1) {
        finger(&mut osk, 2, TouchPhase::Moved, point);
    }
    assert!(
        osk.active_swipe_path().is_empty(),
        "secret contacts have no trace"
    );
    finger(&mut osk, 2, TouchPhase::Cancelled, last);
    assert!(osk.take_output().is_empty());
}

#[test]
fn secret_touch_typing_never_adapts_the_touch_model() {
    for (purpose, hints) in [
        (InputPurpose::Password, 0),
        (InputPurpose::Text, content_hint::SENSITIVE_DATA),
        (InputPurpose::Text, content_hint::HIDDEN_TEXT),
    ] {
        let mut osk = OskWidget::new();
        osk.set_purpose(purpose);
        osk.set_content_hints(hints);
        lay_out(&mut osk, 720.0, 360.0);
        let (_, e) = key_rect(&osk, "e");
        let before = osk.touch_model.clone();
        finger(&mut osk, 1, TouchPhase::Started, e.center());
        finger(&mut osk, 1, TouchPhase::Ended, e.center());
        assert_eq!(osk.take_output(), vec![commit("e")]);
        assert_eq!(osk.touch_model, before, "secret taps must never be learned");
        assert!(
            osk.take_lesson().is_none(),
            "secret taps must not stage persisted learning"
        );
        assert!(osk.suggestions().is_empty());
    }
}

#[test]
fn verbatim_fields_ignore_personal_and_language_touch_bias() {
    for purpose in [
        InputPurpose::Email,
        InputPurpose::Url,
        InputPurpose::Password,
    ] {
        let mut osk = OskWidget::new();
        osk.set_purpose(purpose);
        lay_out(&mut osk, 720.0, 360.0);
        let (e_id, e) = key_rect(&osk, "e");
        let (_, r) = key_rect(&osk, "r");
        for _ in 0..20 {
            osk.touch_model
                .learn('r', Point::new(r.x0 + 1.0, r.center().y), r);
        }
        let touch = Point::new(e.x1 - 1.0, e.center().y);
        assert_eq!(osk.resolve_letter_tap(e_id, Some(touch)), e_id);
    }
}

#[test]
fn the_terminal_row_speaks_its_latches_and_retires_on_field_change() {
    let mut osk = OskWidget::new();
    osk.set_purpose(crate::imclient::purpose_from_wire(
        crate::imclient::content_purpose::TERMINAL,
    ));
    lay_out(&mut osk, 390.0, 360.0);
    let ids: Vec<&str> = osk.toolbar.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "esc", "tab", "ctrl", "alt", "left", "down", "up", "right", "dismiss"
        ]
    );
    let ctrl_name = |osk: &OskWidget| {
        let index = osk
            .toolbar
            .iter()
            .position(|item| item.id == "ctrl")
            .unwrap();
        let target = osk.toolbar[index].semantic_target(index);
        osk.accessibility()
            .unwrap()
            .children
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some(target.as_str()))
            .unwrap()
            .name
            .unwrap()
    };
    let target = {
        let index = osk
            .toolbar
            .iter()
            .position(|item| item.id == "ctrl")
            .unwrap();
        osk.toolbar[index].semantic_target(index)
    };
    assert_eq!(ctrl_name(&osk), "Ctrl");
    assert!(semantic(&mut osk, &target, &AccessibilityAction::Activate));
    assert_eq!(ctrl_name(&osk), "Ctrl, held for the next key");
    assert!(semantic(&mut osk, &target, &AccessibilityAction::Activate));
    assert_eq!(ctrl_name(&osk), "Ctrl, locked");
    assert!(
        osk.take_output().is_empty(),
        "a modifier alone sends nothing"
    );

    osk.set_purpose(InputPurpose::Password);
    lay_out(&mut osk, 390.0, 360.0);
    assert_eq!(osk.modifier_latches(), ModifierLatches::default());
    assert!(!osk.toolbar.iter().any(OskToolbarItem::is_key));
    osk.activate_label("c");
    assert!(
        osk.take_output()
            .iter()
            .all(|output| !matches!(output, OskOutput::Key(_))),
        "a lock does not outlive the terminal field"
    );
}

#[test]
fn semantic_toolbar_edits_the_focused_field_and_hides_without_stale_contacts() {
    let (services, _) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    ui::clipboard_copy("literal clipboard");
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.activate_label("q");
    osk.take_output();
    let tool = |osk: &mut OskWidget, name: &str| {
        let target = osk
            .accessibility()
            .unwrap()
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some(name))
            .unwrap()
            .action_target
            .unwrap();
        assert!(semantic(osk, &target, &AccessibilityAction::Activate));
    };
    tool(&mut osk, "Move caret left");
    assert_eq!(
        osk.take_output(),
        vec![commit("q"), OskOutput::key(Symbol::Left)]
    );
    tool(&mut osk, "Move caret right");
    assert_eq!(osk.take_output(), vec![OskOutput::key(Symbol::Right)]);
    osk.take_lesson();
    tool(&mut osk, "Paste");
    assert_eq!(osk.take_output(), vec![commit("literal clipboard")]);
    assert!(
        osk.take_lesson().is_none(),
        "paste must not train personal predictions"
    );
    let (_, e) = key_rect(&osk, "e");
    finger(&mut osk, 1, TouchPhase::Started, e.center());
    tool(&mut osk, "Hide keyboard");
    assert!(!osk.visible);
    finger(&mut osk, 1, TouchPhase::Ended, e.center());
    assert!(osk.take_output().is_empty());
}

#[test]
fn editing_keys_have_spoken_names_and_shift_announces_its_state() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let names = |osk: &OskWidget| {
        osk.accessibility()
            .unwrap()
            .children
            .iter()
            .map(|node| node.name.clone())
            .collect::<Vec<_>>()
    };
    let spoken = names(&osk);
    for name in [
        "Backspace",
        "Enter",
        "Space",
        "Shift, off",
        "Switch to symbols",
        "Move caret left",
        "Move caret right",
    ] {
        assert!(
            spoken.iter().any(|spoken| spoken.as_deref() == Some(name)),
            "missing spoken control: {name}"
        );
    }
    let shift_target = osk
        .accessibility()
        .unwrap()
        .children
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Shift, off"))
        .unwrap()
        .action_target
        .unwrap();
    assert!(semantic(
        &mut osk,
        &shift_target,
        &AccessibilityAction::Activate
    ));
    assert!(
        names(&osk)
            .iter()
            .any(|name| name.as_deref() == Some("Shift, next letter uppercase"))
    );
    assert!(semantic(
        &mut osk,
        &shift_target,
        &AccessibilityAction::Activate
    ));
    assert!(
        names(&osk)
            .iter()
            .any(|name| name.as_deref() == Some("Caps lock, on"))
    );
}

#[test]
fn hiding_cancels_a_held_backspace_before_its_deadline() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.apply_action(KeyAction::Char("keep".into()));
    let backspace = osk
        .key_geom
        .iter()
        .find(|(id, _)| {
            osk.current_layout()
                .key(*id)
                .is_some_and(|key| key.action == KeyAction::Backspace)
        })
        .unwrap()
        .1
        .center();
    finger(&mut osk, 1, TouchPhase::Started, backspace);
    let deadline = osk.next_wake_deadline().expect("repeat armed");
    osk.set_visible(false);
    assert_eq!(osk.next_wake_deadline(), None);
    osk.poll_hold(deadline);
    osk.set_visible(true);
    finger(&mut osk, 1, TouchPhase::Ended, backspace);
    assert_eq!(osk.pending_text(), "keep");
}

#[test]
fn field_change_cancels_a_pointer_swipe_and_release() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (id, rect) = key_rect(&osk, "e");
    osk.pressed_key = Some(id);
    osk.hovered_key = Some(id);
    osk.pointer = Some(rect.center());
    osk.gesture_path = Some(vec![rect.center()]);
    osk.begin_hold(id, None, Instant::now());
    osk.reset_composition();
    osk.pointer_released(&mut false);
    assert!(osk.pending_text().is_empty());
    assert!(osk.take_output().is_empty());
    assert_eq!(osk.next_wake_deadline(), None);
}

#[test]
fn lifting_selects_the_accent_at_the_final_contact_position() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (_, e) = key_rect(&osk, "e");
    finger(&mut osk, 1, TouchPhase::Started, e.center());
    osk.poll_hold(osk.next_wake_deadline().unwrap());
    let accent = osk.alternates.as_ref().unwrap().1.cells[1].center();
    // No intervening Moved event: the Ended event owns the last sample.
    finger(&mut osk, 1, TouchPhase::Ended, accent);
    assert_eq!(osk.pending_text(), "é");
}

#[test]
fn long_press_offers_accents_and_slide_picks_one() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (_, e) = key_rect(&osk, "e");
    finger(&mut osk, 1, TouchPhase::Started, e.center());
    let deadline = osk
        .next_wake_deadline()
        .expect("a letter with accents arms a long press");
    osk.poll_hold(deadline.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(osk.alternates.is_none(), "nothing before the delay");
    osk.poll_hold(deadline);
    let cells = osk.alternates.as_ref().unwrap().1.cells.clone();
    assert_eq!(osk.alternates.as_ref().unwrap().1.chosen(), Some("e"));
    assert!(
        cells
            .iter()
            .all(|c| c.y1 <= e.y0 && osk.content_rect.contains(c.origin()))
    );
    finger(&mut osk, 1, TouchPhase::Moved, cells[1].center());
    finger(&mut osk, 1, TouchPhase::Ended, cells[1].center());
    assert_eq!(osk.pending_text(), "é");
    assert!(osk.alternates.is_none());
}

fn long_press_options(osk: &mut OskWidget, label: &str) -> Vec<String> {
    let (id, _) = key_rect(osk, label);
    let now = Instant::now();
    osk.begin_hold(id, None, now);
    osk.poll_hold(now + LONG_PRESS);
    osk.alternates.take().unwrap().1.options
}

#[test]
fn french_elisions_compose_as_prefix_plus_word() {
    let mut osk = OskWidget::new().with_active_layout("azerty");
    assert_eq!(osk.language(), Language::French);
    for c in ["l", "'", "h", "o", "m"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    assert_eq!(osk.current_word(), "l'hom");
    let words: Vec<&str> = osk.suggestions().iter().map(|s| s.word.as_str()).collect();
    assert_eq!(words.first(), Some(&"l'homme"), "{words:?}");
    for c in ["m", "r"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    osk.apply_action(KeyAction::Space);
    assert_eq!(
        osk.pending_text(),
        "l'homme ",
        "only the word after the elision is corrected"
    );
    // The word after the elision is the next word's context.
    assert_eq!(osk.prev_word.as_deref(), Some("homme"));
    // A typographic apostrophe composes the same way.
    for c in ["q", "u", "\u{2019}", "i"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    assert_eq!(osk.current_word(), "qu'i");
    // An apostrophe that starts no known word still ends the word.
    let mut osk = OskWidget::new().with_active_layout("azerty");
    for c in ["x", "y", "'"] {
        osk.apply_action(KeyAction::Char(c.into()));
    }
    assert_eq!(osk.current_word(), "");
}

#[test]
fn french_accents_come_in_the_order_french_uses_them() {
    let mut osk = OskWidget::new().with_active_layout("azerty");
    lay_out(&mut osk, 390.0, 312.0);
    assert_eq!(osk.language(), Language::French);
    for (key, leading) in [
        ("e", &["e", "é", "è", "ê", "ë"][..]),
        ("a", &["a", "à", "â"][..]),
        ("u", &["u", "ù", "û"][..]),
        ("o", &["o", "ô", "œ"][..]),
        ("c", &["c", "ç"][..]),
    ] {
        let options = long_press_options(&mut osk, key);
        assert_eq!(&options[..leading.len()], leading, "{options:?}");
    }
    let all: Vec<String> = ["e", "a", "c", "u", "o", "i", "y"]
        .into_iter()
        .flat_map(|k| long_press_options(&mut osk, k))
        .collect();
    for form in [
        "é", "è", "ê", "ë", "à", "â", "ç", "ù", "û", "ü", "ô", "œ", "î", "ï", "ÿ",
    ] {
        assert!(all.iter().any(|f| f == form), "{form} missing");
    }
}

#[test]
fn the_plain_letter_sits_above_the_finger_even_at_the_edges() {
    let mut osk = OskWidget::new().with_active_layout("azerty");
    lay_out(&mut osk, 390.0, 312.0);
    for label in ["a", "e", "o", "p", "m", "q"] {
        let (id, rect) = key_rect(&osk, label);
        let now = Instant::now();
        osk.begin_hold(id, None, now);
        osk.poll_hold(now + LONG_PRESS);
        let Some((_, mut popup)) = osk.alternates.take() else {
            continue;
        };
        let first = popup.cells[0];
        assert!(
            (first.center().x - rect.center().x).abs() < 1.0,
            "{label}: the letter is not above the key"
        );
        assert!(
            popup
                .cells
                .iter()
                .all(|c| c.x0 >= osk.content_rect.x0 - 0.5 && c.x1 <= osk.content_rect.x1 + 0.5),
            "{label}: row spills out"
        );
        popup.track(rect.center().x + rect.width() * 0.3);
        assert_eq!(popup.chosen(), Some(label));
    }
}

#[test]
fn shifted_long_press_offers_capitals() {
    let mut osk = OskWidget::new().with_active_layout("azerty");
    lay_out(&mut osk, 390.0, 312.0);
    osk.apply_action(KeyAction::Shift);
    let e = long_press_options(&mut osk, "e");
    assert_eq!(&e[..3], ["E", "É", "È"], "{e:?}");
    // A form whose capital is two letters (ß → SS) stays as it is.
    let mut s = OskWidget::new();
    lay_out(&mut s, 390.0, 312.0);
    s.apply_action(KeyAction::Shift);
    assert!(long_press_options(&mut s, "s").contains(&"ß".to_string()));
}

#[test]
fn a_long_press_released_without_sliding_types_the_letter_once() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (_, e) = key_rect(&osk, "e");
    finger(&mut osk, 1, TouchPhase::Started, e.center());
    osk.poll_hold(Instant::now() + LONG_PRESS);
    finger(&mut osk, 1, TouchPhase::Ended, e.center());
    assert_eq!(osk.pending_text(), "e");
}

#[test]
fn a_quick_tap_opens_nothing_and_types_the_letter() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (_, e) = key_rect(&osk, "e");
    finger(&mut osk, 1, TouchPhase::Started, e.center());
    finger(&mut osk, 1, TouchPhase::Ended, e.center());
    assert!(osk.hold.is_none() && osk.next_wake_deadline().is_none());
    assert_eq!(osk.pending_text(), "e");
}

#[test]
fn held_backspace_repeats_and_its_release_adds_nothing() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    osk.apply_action(KeyAction::Char(".".into()));
    type_word(&mut osk, "hello");
    let (_, bs) = key_rect(&osk, "Backspace");
    finger(&mut osk, 1, TouchPhase::Started, bs.center());
    let mut now = Instant::now() + REPEAT_DELAY;
    for _ in 0..3 {
        osk.poll_hold(now);
        now += REPEAT_INTERVAL;
    }
    finger(&mut osk, 1, TouchPhase::Ended, bs.center());
    assert_eq!(osk.pending_text(), ".he");
}

#[test]
fn dragging_the_space_bar_moves_the_caret_instead_of_typing() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    type_word(&mut osk, "hello");
    osk.take_output();
    let (_, space) = key_rect(&osk, "space");
    let c = space.center();
    let step = osk.key_scale() * CURSOR_STEP;
    finger(&mut osk, 1, TouchPhase::Started, c);
    finger(&mut osk, 1, TouchPhase::Moved, Point::new(c.x - 3.0, c.y));
    assert!(!osk.cursor_drag_active());
    // Just past the threshold, so rounding in the key's centre cannot
    // leave it short.
    let start = c.x - osk.key_scale() * CURSOR_START - 1.0;
    finger(&mut osk, 1, TouchPhase::Moved, Point::new(start, c.y));
    finger(
        &mut osk,
        1,
        TouchPhase::Moved,
        Point::new(start - 3.5 * step, c.y),
    );
    finger(
        &mut osk,
        1,
        TouchPhase::Ended,
        Point::new(start - 3.5 * step, c.y),
    );
    assert_eq!(
        osk.take_output(),
        vec![
            commit("hello"),
            OskOutput::key(Symbol::Left),
            OskOutput::key(Symbol::Left),
            OskOutput::key(Symbol::Left),
        ],
        "the word commits as typed, the caret steps left, no space"
    );
}

#[test]
fn a_touch_down_previews_its_character_but_never_in_a_password() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    let (g, rect) = key_rect(&osk, "g");
    finger(&mut osk, 1, TouchPhase::Started, rect.center());
    assert_eq!(osk.preview_key(&[g]), Some(g));
    let bubble = crate::presentation::preview_rect(rect, osk.content_rect);
    assert!(bubble.y1 <= rect.y0 && bubble.width() > rect.width() * 0.8);
    let (space, _) = key_rect(&osk, "space");
    assert_eq!(osk.preview_key(&[space]), None, "only characters preview");
    osk.set_purpose(InputPurpose::Password);
    assert_eq!(osk.preview_key(&[g]), None);
}

#[test]
fn previews_stay_inside_the_keyboard_at_its_edges() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 390.0, 312.0);
    for label in ["q", "p", "a", "l", "z", "m"] {
        let (_, rect) = key_rect(&osk, label);
        let bubble = crate::presentation::preview_rect(rect, osk.content_rect);
        assert!(
            bubble.x0 >= osk.content_rect.x0 && bubble.x1 <= osk.content_rect.x1,
            "{label} preview spills out: {bubble:?}"
        );
        assert!(bubble.y0 >= osk.content_rect.y0);
    }
}

#[test]
fn hit_areas_move_with_context_but_the_drawn_keys_never_do() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 390.0, 312.0);
    let before = osk.key_geometry().to_vec();
    let start = osk.hit_cells(2.0);
    type_word(&mut osk, "th");
    lay_out(&mut osk, 390.0, 312.0);
    assert_eq!(
        osk.key_geometry(),
        before.as_slice(),
        "context never moves a key"
    );
    let th = osk.hit_cells(2.0);
    let (e, _) = key_rect(&osk, "e");
    let (r, rr) = key_rect(&osk, "r");
    let e_after_th = th.iter().filter(|c| c.key == e).count();
    let e_at_start = start.iter().filter(|c| c.key == e).count();
    assert!(e_after_th > e_at_start, "'e' grows after 'th'");
    assert!(
        th.iter().any(|c| c.drawn == r && c.key == e),
        "the r/e border goes to 'e' after 'th'"
    );
    let core = crate::touch_model::core_zone(rr, crate::touch_model::CORE_FRACTION);
    assert!(
        th.iter()
            .filter(|c| core.contains(c.rect.center()))
            .all(|c| c.key == r),
        "r's core zone stays r's"
    );
}

/// Parking puts the keys away and keeps the strip that brings them back,
/// and telling the window to shrink is what gives the screen back: the
/// keys are not painted over a panel that is now the app's.
#[test]
fn a_parked_keyboard_is_one_strip_and_asks_for_a_strip_of_a_surface() {
    let mut osk = OskWidget::new();
    lay_out(&mut osk, 720.0, 360.0);
    assert!(osk.keys_up());
    assert!(
        osk.key_geometry()
            .iter()
            .any(|(_, rect)| rect.height() > 0.0)
    );
    assert_eq!(
        osk.preferred_window_size(),
        None,
        "a keyboard with its keys up fills the panel it was given"
    );

    // The toolbar offers the way to park.
    let park = osk
        .toolbar_items()
        .iter()
        .position(|item| item.action == OskToolbarAction::Park)
        .expect("the toolbar can put the keys away");
    osk.activate_tool(park);
    assert!(osk.is_parked());
    assert!(!osk.keys_up());
    lay_out(&mut osk, 720.0, 96.0);
    assert!(
        osk.key_geometry()
            .iter()
            .all(|(_, rect)| rect.height() <= 0.0),
        "no key is left to tap"
    );
    let (width, height) = osk
        .preferred_window_size()
        .expect("a parked keyboard says how small its surface may be");
    assert_eq!(width, 720.0);
    assert!(
        height < 96.0,
        "the strip is one toolbar row: {height} of a 96-pixel panel"
    );

    // The strip's own item brings them back.
    let unpark = osk
        .toolbar_items()
        .iter()
        .position(|item| item.action == OskToolbarAction::Unpark)
        .expect("the strip says how to come back");
    osk.activate_tool(unpark);
    assert!(!osk.is_parked());
    lay_out(&mut osk, 720.0, 360.0);
    assert_eq!(osk.preferred_window_size(), None);
    assert!(
        osk.key_geometry()
            .iter()
            .any(|(_, rect)| rect.height() > 0.0)
    );
}
