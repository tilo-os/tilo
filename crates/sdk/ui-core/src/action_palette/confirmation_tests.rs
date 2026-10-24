#[test]
fn confirmation_refreshes_never_turn_the_question_back_into_search() {
    let _settled = crate::motion::testing::settled();
    let mut spec = ActionSpec::immediate(
        "delete-message",
        "Delete message",
        NOTE_TYPE,
        &Note {
            key: "deleted".into(),
            value: "yes".into(),
        },
    )
    .unwrap()
    .with_description("Remove this message for everyone");
    spec.contract.confirmation = ConfirmationPolicy::Confirm;
    spec.contract.risk = tilo_actions::RiskLevel::Destructive;
    let resources = vec![Resource::new("chat", "chat", "Chat").with_action(spec)];
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&resources);
    palette.open();
    palette.activate_selected();
    let original = palette.asking().unwrap().0.clone();
    // These are the refreshes a live frame performs after opening a question.
    palette.load_open_resources(&resources);
    palette.load_bound_resources(Vec::new());
    palette.set_providers(Vec::new());
    palette.set_ambient_context(None);
    palette.advance_search_page();
    assert!(palette.result_labels().is_empty());
    assert_eq!(palette.asking().unwrap().0, &original);

    let mut text = crate::font_catalog::fixture_renderer();
    for theme in [Theme::dark(), Theme::light()] {
        for window in [
            Rect::new(0.0, 0.0, 900.0, 700.0),
            Rect::new(0.0, 0.0, 320.0, 540.0),
        ] {
            let presence = PresenceCtx::default();
            let previous = presence.push_viewport(window);
            let (origin, width) = placement(window);
            palette.layout(&theme, origin, width);
            palette.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &presence,
            );
            let layout = palette
                .confirmation_sheet
                .layout()
                .expect("standard modal geometry");
            assert!(layout.card.width() <= window.width());
            assert!(layout.card.height() <= window.height());
            let node = palette.accessibility().unwrap();
            assert_eq!(node.name.as_deref(), Some("Delete message?"));
            assert_eq!(
                node.children.len(),
                2,
                "only the two answers, no search box or result list"
            );
            assert!(
                node.children
                    .iter()
                    .all(|child| child.role == crate::a11y::Role::Button)
            );
            assert_eq!(node.children[0].name.as_deref(), Some("Delete message"));
            assert_eq!(node.children[1].name.as_deref(), Some("Cancel"));
            assert!(
                node.children[1].state.focused,
                "destructive action defaults to Cancel"
            );
            presence.pop_viewport(previous);
        }
    }
    let mut doc = automerge::AutoCommit::new();
    let enter =
        crate::synthetic_input::key_input(&Key::Named(NamedKey::Enter), ElementState::Pressed);
    assert!(palette.handle_key(
        &enter,
        &Modifiers::default(),
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(!palette.is_open());
    assert!(
        palette.take_operation().is_none(),
        "default Enter must not delete anything"
    );
}

#[test]
fn the_standard_confirmation_button_keeps_exact_evidence_and_cancels_on_escape() {
    let mut spec = ActionSpec::immediate(
        "delete-message",
        "Delete message",
        NOTE_TYPE,
        &Note {
            key: "deleted".into(),
            value: "yes".into(),
        },
    )
    .unwrap();
    spec.contract.confirmation = ConfirmationPolicy::Confirm;
    spec.contract.risk = tilo_actions::RiskLevel::Destructive;
    let resource = Resource::new("chat", "chat", "Chat").with_action(spec);
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&[resource]);
    let mut doc = automerge::AutoCommit::new();
    for confirm in [false, true] {
        palette.open();
        palette.activate_selected();
        if confirm {
            let target = palette.target("submit");
            assert!(palette.perform_semantic_action(
                &target,
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            let pending = palette.take_operation().expect("explicit deletion answer");
            assert_eq!(pending.address.action, "delete-message");
            assert_eq!(
                pending.confirmation_evidence(),
                &serde_json::json!({"confirm": true})
            );
        } else {
            let escape = crate::synthetic_input::key_input(
                &Key::Named(NamedKey::Escape),
                ElementState::Pressed,
            );
            palette.handle_key(
                &escape,
                &Modifiers::default(),
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
            assert!(palette.take_operation().is_none());
        }
    }
}

/// Nobody is asked to type a phrase: the palette asks the action's own
/// question, swallows typing, and one Enter queues the action with the
/// evidence its policy needs, a declared phrase included.
#[test]
fn a_confirmation_asks_the_actions_question_and_one_enter_goes_ahead() {
    let _settled = crate::motion::testing::settled();
    for policy in [
        ConfirmationPolicy::Confirm,
        ConfirmationPolicy::Phrase {
            phrase: "ERASE DISK".into(),
        },
    ] {
        let mut spec = ActionSpec::immediate(
            "delete-message",
            "Delete message",
            NOTE_TYPE,
            &Note {
                key: "deleted".into(),
                value: "yes".into(),
            },
        )
        .unwrap()
        .with_description("Remove this message for everyone");
        spec.contract.confirmation = policy.clone();
        let resource = Resource::new("chat", "chat", "Chat").with_action(spec);
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&[resource]);
        palette.open();
        palette.activate_selected();
        let (_, question) = palette.asking().expect("the palette asks");
        assert_eq!(question, "Delete message?");
        let detail = &palette.confirm.as_ref().unwrap().detail;
        assert_eq!(detail, "Remove this message for everyone.");
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let typed =
            crate::synthetic_input::key_input(&Key::Character("x".into()), ElementState::Pressed);
        assert!(palette.handle_key(
            &typed,
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        ));
        assert_eq!(palette.field.value(), "", "typing is swallowed");
        let enter =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Enter), ElementState::Pressed);
        palette.handle_key(
            &enter,
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        let pending = palette.take_operation().expect("one Enter goes ahead");
        assert_eq!(
            pending.confirmation_evidence(),
            &confirmation_evidence(&policy)
        );
    }
    assert_eq!(
        confirmation_text("Save", None),
        ("Save?".to_owned(), String::new())
    );
    assert_eq!(
        confirmation_text(
            "Clear site data",
            Some("Clear site data Cookies will be removed.")
        ),
        (
            "Clear site data?".to_owned(),
            "Cookies will be removed.".to_owned()
        )
    );
    assert_eq!(
        confirmation_text("Clear site data", Some("Clear site data")),
        ("Clear site data?".to_owned(), String::new())
    );
}
