// The palette's confirmation step and the rows it lists, included in
// `action_palette::tests`.

/// Put the keyboard in the field for the name, as a person tabbing to it
/// does.
fn focus_confirmation(palette: &mut ActionPalette, doc: &mut automerge::AutoCommit) {
    let target = palette.target("field");
    Widget::perform_semantic_action(
        palette,
        &target,
        &crate::a11y::Action::Focus,
        &mut crate::widget::Edits::lend(doc),
        &ROOT,
    );
}

/// Press the palette's `part` ("submit", "cancel") as a click or a screen
/// reader does.
fn press(palette: &mut ActionPalette, part: &str, doc: &mut automerge::AutoCommit) {
    let target = palette.target(part);
    Widget::perform_semantic_action(
        palette,
        &target,
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(doc),
        &ROOT,
    );
}

/// Type `text` into the field for the name, the keyboard there.
fn type_confirmation(palette: &mut ActionPalette, text: &str, doc: &mut automerge::AutoCommit) {
    focus_confirmation(palette, doc);
    let target = palette.target("field");
    Widget::perform_semantic_action(
        palette,
        &target,
        &crate::a11y::Action::SetValue(text.into()),
        &mut crate::widget::Edits::lend(doc),
        &ROOT,
    );
}

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

/// A plain question: the palette asks the action's own question with no
/// field at all, swallows typing, and one Enter queues the action.
#[test]
fn a_confirmation_asks_the_actions_question_and_one_enter_goes_ahead() {
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
    let resource = Resource::new("chat", "chat", "Chat").with_action(spec);
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&[resource]);
    palette.open();
    palette.activate_selected();
    let (_, question) = palette.asking().expect("the palette asks");
    assert_eq!(question, "Delete message?");
    let detail = &palette.confirm.as_ref().unwrap().detail;
    assert_eq!(detail, "Remove this message for everyone.");
    assert!(
        !palette.confirm.as_ref().unwrap().typed(),
        "a question has no field to type in"
    );
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
        &serde_json::json!({ "confirm": true })
    );
    assert_eq!(confirm::question("Save", None), "Save?");
    assert_eq!(
        confirm::confirmation_text("Save", None),
        ("Save?".to_owned(), String::new())
    );
    assert_eq!(
        confirm::confirmation_text(
            "Clear site data",
            Some("Clear site data Cookies will be removed.")
        ),
        (
            "Clear site data?".to_owned(),
            "Cookies will be removed.".to_owned()
        )
    );
    assert_eq!(
        confirm::confirmation_text("Clear site data", Some("Clear site data")),
        ("Clear site data?".to_owned(), String::new())
    );
}

/// A confirmation input is a plain text field: no search icon, no dropdown,
/// never wired to search providers.
#[test]
fn a_confirmation_field_never_shows_search_results() {
    let _settled = crate::motion::testing::settled();
    let mut erase = ActionSpec::immediate(
        "erase",
        "Erase drive",
        NOTE_TYPE,
        &Note {
            key: "erased".into(),
            value: "yes".into(),
        },
    )
    .unwrap()
    .destructive();
    erase.contract.confirmation = ConfirmationPolicy::Phrase {
        phrase: "nvme0n1".into(),
    };
    let resources = vec![
        Resource::new("disks", "disks.drive", "nvme0n1")
            .with_action(erase)
            .with_action(
                ActionSpec::immediate(
                    "refresh",
                    "Refresh all drives",
                    NOTE_TYPE,
                    &Note {
                        key: "refreshed".into(),
                        value: "yes".into(),
                    },
                )
                .unwrap(),
            ),
    ];
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&resources);
    palette.set_providers(vec![Box::new(People)]);
    palette.open();
    palette.set_query("Erase");
    palette.activate_selected();
    assert!(palette.asking().is_some(), "erasing a drive asks first");

    // A plain text field, never the search field.
    let field = Widget::accessibility(&palette)
        .expect("the palette asks")
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some(palette.target("field").as_str()))
        .expect("a field for the name");
    assert_eq!(
        field.role,
        crate::a11y::Role::TextInput,
        "a plain text field"
    );
    let mut doc = automerge::AutoCommit::new();
    // Whatever reaches the palette meanwhile (typing, the app's
    // resources, its search sources, another page of results), the card
    // under the field stays the question: no result list opens.
    for typed in ["n", "Alan", "Refresh", "nvme0"] {
        type_confirmation(&mut palette, typed, &mut doc);
        palette.load_open_resources(&resources);
        palette.set_providers(vec![Box::new(People)]);
        palette.load_bound_resources(Vec::new());
        palette.set_ambient_context(None);
        let _ = palette.advance_search_page();
        assert!(
            palette.result_labels().is_empty(),
            "a confirmation field shows no search results: {:?}",
            palette.result_labels()
        );
        assert!(palette.asking().is_some());
    }
    let shown = serde_json::to_string(&Widget::accessibility(&palette)).unwrap();
    assert!(!shown.contains("Refresh all drives"), "{shown}");
    assert!(
        !shown.contains("\"Search\""),
        "no search field is offered: {shown}"
    );

    // The name typed in full lets Erase go ahead.
    type_confirmation(&mut palette, "nvme0n1", &mut doc);
    press(&mut palette, "submit", &mut doc);
    assert_eq!(
        palette
            .take_operation()
            .expect("the typed name goes ahead")
            .confirmation_evidence(),
        &serde_json::json!({ "confirmation_phrase": "nvme0n1" })
    );
}

/// Listed action rows read as outcomes and appear once: no raw ids, no
/// separator glyphs, no implementation suffixes, and window-bar actions are
/// not listed again as app actions.
#[test]
fn listed_action_rows_read_as_outcomes_once_without_ids_glyphs_or_control_verbs() {
    let _settled = crate::motion::testing::settled();
    let new_folder = || {
        ActionSpec::immediate(
            "new-folder",
            "New folder",
            NOTE_TYPE,
            &Note {
                key: "folder".into(),
                value: "new".into(),
            },
        )
        .unwrap()
    };
    let mut focus = new_folder();
    focus.label = "Focus".into();
    let resources = vec![
        Resource::new("feeds.library", "feeds.library", "Feeds")
            .with_action(new_folder())
            .with_child(
                Resource::new("feeds.library/feeds/1", "feed", "Hacker News").with_action(
                    ActionSpec::immediate(
                        "unsubscribe",
                        "Unsubscribe",
                        NOTE_TYPE,
                        &Note {
                            key: "feed".into(),
                            value: "gone".into(),
                        },
                    )
                    .unwrap(),
                ),
            ),
        // A bar button's own verb, as the control projection publishes
        // it: for agents, never a person's row.
        Resource::new("ui:new-folder", "tilo.ui.control", "New folder")
            .with_action(focus.for_audience(tilo_actions::Audience::Agents)),
        // The same action published twice (the bar and the document).
        Resource::new("feeds.library", "feeds.library", "Feeds").with_action(new_folder()),
    ];
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&resources);
    palette.open();
    let rows: Vec<(String, Option<String>)> = palette
        .results
        .groups()
        .iter()
        .flat_map(|group| &group.results)
        .map(|row| (row.title.clone(), row.subtitle.clone()))
        .collect();
    assert_eq!(
        rows.iter()
            .filter(|(title, _)| title == "New folder")
            .count(),
        1,
        "one row however many parts publish it: {rows:?}"
    );
    assert!(
        rows.contains(&("Unsubscribe".to_owned(), Some("Hacker News".to_owned()))),
        "an item's action names the item under it: {rows:?}"
    );
    for (title, subtitle) in &rows {
        let row = tilo_actions::ListedRow {
            title: title.clone(),
            subtitle: subtitle.clone(),
        };
        assert!(
            tilo_actions::listed_row_problems(&row).is_empty(),
            "{row:?} reads with {:?}",
            tilo_actions::listed_row_problems(&row)
        );
    }
}

/// Two open documents offering the same action are two rows, each naming
/// its document under the outcome: neither is hidden as a duplicate, and
/// neither joins the name on with a glyph.
#[test]
fn the_same_action_of_two_documents_is_two_rows_naming_each() {
    let _settled = crate::motion::testing::settled();
    let mark = || {
        ActionSpec::immediate(
            "mark",
            "Mark done",
            NOTE_TYPE,
            &Note {
                key: "marked".into(),
                value: "yes".into(),
            },
        )
        .unwrap()
    };
    let resources = vec![
        Resource::new("a", "thing", "Alpha").with_action(mark()),
        Resource::new("b", "thing", "Beta").with_action(mark()),
    ];
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&resources);
    palette.open();
    let mut rows: Vec<(String, Option<String>)> = palette
        .results
        .groups()
        .iter()
        .flat_map(|group| &group.results)
        .map(|row| (row.title.clone(), row.subtitle.clone()))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("Mark done".to_owned(), Some("Alpha".to_owned())),
            ("Mark done".to_owned(), Some("Beta".to_owned())),
        ]
    );
}

#[test]
fn history_requires_fresh_palette_confirmation_for_ready_work() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut resource = thing_resource();
    resource
        .actions
        .iter_mut()
        .find(|action| action.id == "mark")
        .unwrap()
        .contract
        .confirmation = ConfirmationPolicy::Phrase {
        phrase: "MARK THING".into(),
    };
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(&[resource]);
    palette.open();
    let idx = palette
        .result_labels()
        .iter()
        .position(|label| label.contains("Mark done"))
        .unwrap();
    palette.set_cursor(idx);
    palette.activate_selected();
    let mut doc = automerge::AutoCommit::new();
    // The name goes into the confirmation's own plain field.
    palette.set_query("MARK THING");
    press(&mut palette, "submit", &mut doc);
    assert!(palette.pending_dispatch.is_some());

    palette.suspend_for_history();
    palette.resume_after_history();
    assert!(palette.take_operation().is_none());
    assert!(palette.in_argument_mode());
    let typed = palette
        .confirm
        .as_ref()
        .and_then(ConfirmState::typed_text)
        .expect("the palette asks for the name again")
        .to_owned();
    assert!(typed.is_empty(), "old affirmative is not reused");
    palette.set_query("MARK THING");
    press(&mut palette, "submit", &mut doc);
    assert_eq!(
        palette.take_operation().unwrap().confirmation_evidence(),
        &serde_json::json!({ "confirmation_phrase": "MARK THING" })
    );
}

/// The palette asking to erase a drive named "Samsung T7", which it asks to
/// be typed.
fn asking_to_erase() -> ActionPalette {
    let mut erase = ActionSpec::immediate(
        "erase",
        "Erase drive",
        NOTE_TYPE,
        &Note {
            key: "erased".into(),
            value: "yes".into(),
        },
    )
    .unwrap()
    .destructive()
    .with_description("Erases every file on Samsung T7");
    erase.contract.confirmation = ConfirmationPolicy::Phrase {
        phrase: "Samsung T7".into(),
    };
    let mut palette = ActionPalette::new();
    palette.load_open_resources(&[
        Resource::new("drive:t7", "drive", "Samsung T7").with_action(erase)
    ]);
    palette.open();
    palette.set_query("Erase");
    palette.activate_selected();
    palette
}

/// The palette's part `part` ("submit", "cancel") as a screen reader finds it.
fn part(palette: &ActionPalette, part: &str) -> crate::a11y::AccessNode {
    let target = palette.target(part);
    Widget::accessibility(palette)
        .expect("the palette asks")
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some(target.as_str()))
        .unwrap_or_else(|| panic!("the palette shows {part}"))
}

/// The dialog names the action and what it destroys, says what is lost, and
/// its button is the action's own verb, disabled until the name is typed
/// exactly.
#[test]
fn a_typed_confirmation_names_what_is_lost_and_its_verb_waits_for_the_exact_name() {
    let _settled = crate::motion::testing::settled();
    let mut palette = asking_to_erase();
    let mut doc = automerge::AutoCommit::new();
    let (_, question) = palette.asking().expect("erasing a drive asks first");
    assert_eq!(question, "Erase Samsung T7?");
    let submit = part(&palette, "submit");
    assert_eq!(
        submit.name.as_deref(),
        Some("Erase"),
        "the action's own verb"
    );
    assert!(submit.state.disabled, "nothing typed yet");
    for typed in ["Samsung", "samsung t7", "Samsung T7 2"] {
        type_confirmation(&mut palette, typed, &mut doc);
        assert!(
            part(&palette, "submit").state.disabled,
            "{typed:?} is not the name"
        );
    }
    type_confirmation(&mut palette, "Samsung T7", &mut doc);
    assert!(!part(&palette, "submit").state.disabled, "the exact name");
    type_confirmation(&mut palette, "Samsung T", &mut doc);
    assert!(
        part(&palette, "submit").state.disabled,
        "and not once it changes"
    );
    // The field for the name is labelled; the search field is gone.
    let field = Widget::accessibility(&palette)
        .unwrap()
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some(palette.target("field").as_str()))
        .unwrap();
    assert_eq!(field.name.as_deref(), Some("Type Samsung T7 to confirm"));
    let description = field.description.unwrap_or_default();
    assert!(
        description.contains("Erases every file on Samsung T7."),
        "{description}"
    );
    // It is the shared sheet, the field inside its body, in any window.
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
                .expect("standard modal geometry")
                .clone();
            let field = palette
                .confirm
                .as_ref()
                .and_then(ConfirmState::phrase_field)
                .unwrap()
                .get_rect();
            assert!(
                field.height() > 0.0
                    && layout.body.contains(field.origin())
                    && field.x1 <= layout.body.x1 + 0.5
                    && field.y1 <= layout.body.y1 + 0.5,
                "the field {field:?} stands in the sheet's body {:?}",
                layout.body
            );
            presence.pop_viewport(previous);
        }
    }
}

/// Return stays the safe answer: Enter in the field for the name never
/// erases, not even once the name matches. Only a press on Erase does, and
/// only with the exact name.
#[test]
fn enter_never_erases_only_the_erase_button_does() {
    let _settled = crate::motion::testing::settled();
    let mut palette = asking_to_erase();
    let mut doc = automerge::AutoCommit::new();
    let enter = |palette: &mut ActionPalette, doc: &mut automerge::AutoCommit| {
        palette.handle_key(
            &enter(),
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(doc),
            &ROOT,
        );
    };
    for typed in ["", "Samsung", "Samsung T7"] {
        type_confirmation(&mut palette, typed, &mut doc);
        enter(&mut palette, &mut doc);
        assert!(
            palette.take_operation().is_none(),
            "Enter after {typed:?} erases nothing"
        );
        assert!(
            palette.asking().is_some(),
            "it keeps asking after {typed:?}"
        );
    }
    type_confirmation(&mut palette, "Samsung", &mut doc);
    press(&mut palette, "submit", &mut doc);
    assert!(
        palette.take_operation().is_none(),
        "Erase waits for the whole name"
    );
    assert!(palette.asking().is_some());
    type_confirmation(&mut palette, "Samsung T7", &mut doc);
    press(&mut palette, "submit", &mut doc);
    assert_eq!(
        palette
            .take_operation()
            .expect("Erase with the exact name goes ahead")
            .confirmation_evidence(),
        &serde_json::json!({ "confirmation_phrase": "Samsung T7" })
    );
}

/// Cancel has the keyboard when the dialog opens: Enter straight away
/// cancels, and Tab reaches the field for the name past the disabled verb.
#[test]
fn cancel_has_the_keyboard_when_a_typed_confirmation_opens() {
    let _settled = crate::motion::testing::settled();
    let mut palette = asking_to_erase();
    assert!(part(&palette, "cancel").state.focused, "Cancel first");
    assert!(!part(&palette, "submit").state.focused);
    let mut doc = automerge::AutoCommit::new();
    let key = |palette: &mut ActionPalette, key: Key, doc: &mut automerge::AutoCommit| {
        palette.handle_key(
            &crate::synthetic_input::key_input(&key, ElementState::Pressed),
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(doc),
            &ROOT,
        );
    };
    key(&mut palette, Key::Named(NamedKey::Tab), &mut doc);
    assert!(
        palette
            .confirm
            .as_ref()
            .and_then(ConfirmState::phrase_field)
            .is_some_and(|field| field.is_focused()),
        "Tab passes the disabled verb to the field for the name"
    );
    key(&mut palette, Key::Named(NamedKey::Tab), &mut doc);
    assert!(part(&palette, "cancel").state.focused, "and back to Cancel");
    key(&mut palette, Key::Named(NamedKey::Enter), &mut doc);
    assert!(
        palette.take_operation().is_none(),
        "Enter on Cancel erases nothing"
    );
    assert!(palette.asking().is_none(), "it stops asking");
}
