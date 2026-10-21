// The palette's confirmation step and the rows it lists, included in
// `action_palette::tests`.

/// Type `text` into the confirmation's field as assistive technology
/// sets a value.
fn type_confirmation(palette: &mut ActionPalette, text: &str, doc: &mut automerge::AutoCommit) {
    let target = palette.target("field");
    Widget::perform_semantic_action(
        palette,
        &target,
        &crate::a11y::Action::SetValue(text.into()),
        &mut crate::widget::Edits::lend(doc),
        &ROOT,
    );
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
    assert_eq!(
        detail,
        "Remove this message for everyone. Press Enter or Confirm to go ahead."
    );
    assert!(
        matches!(
            palette.confirm.as_ref().unwrap().head,
            ConfirmHead::Question(_)
        ),
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
    assert_eq!(
        confirmation_text("Save", None, None),
        ("Save?".to_owned(), CONFIRMATION_HINT.to_owned())
    );
}

// USER REQUIREMENT (2026-10-21): "A confirmation input is a plain text
// field: no search icon, no dropdown, never wired to search providers."
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
    let (head, _) = palette.presentation_widgets();
    assert_eq!(head.name(), "TextInput", "a plain text field");
    assert_ne!(head.name(), "SearchField", "never the search field");
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

    // The name typed in full goes ahead.
    type_confirmation(&mut palette, "nvme0n1", &mut doc);
    palette.handle_key(
        &enter(),
        &Modifiers::default(),
        &mut crate::widget::Edits::lend(&mut doc),
        &ROOT,
    );
    assert_eq!(
        palette
            .take_operation()
            .expect("the typed name goes ahead")
            .confirmation_evidence(),
        &serde_json::json!({ "confirmation_phrase": "nvme0n1" })
    );
}

// USER REQUIREMENT (2026-10-21): "never show raw ids", "never use
// separator glyphs (\"·\") or implementation suffixes like \"— Focus\" or
// \"— History\"", "duplicates (History twice; Feeds' window-bar actions
// listed again as app actions) appear once."
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
        for text in std::iter::once(title).chain(subtitle) {
            assert!(
                !text.contains('\u{b7}') && !text.contains(" \u{2014} "),
                "no joined pair: {text}"
            );
            assert!(!text.contains("Focus"), "no control verb: {text}");
            assert!(
                !text
                    .split_whitespace()
                    .any(|word| word.len() >= 6 && word.chars().all(|c| c.is_ascii_digit())),
                "no raw id: {text}"
            );
        }
    }
}
