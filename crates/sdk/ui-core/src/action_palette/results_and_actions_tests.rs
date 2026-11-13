// A result and the actions on it are rows of one list. The result names itself once; its
// actions read as what each does, never as a second copy of the result's name; and the band
// under the list shows the result, never an echo of the row under it.

struct OneHit(&'static str);

impl SearchProvider for OneHit {
    fn name(&self) -> &'static str {
        "Notes"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        if crate::search::fold_search_text(query).contains("note") {
            vec![
                SearchResult::new("Notes", self.0, "ignore")
                    .with_subtitle("Message")
                    .with_preview(self.0),
            ]
        } else {
            Vec::new()
        }
    }
}

fn published_tree(actions: Vec<(&'static str, &'static str)>) -> Vec<ResourceSpec> {
    let mut message = ResourceSpec::new(
        "tl:so.tilo.chat:root/messages/m1",
        "chat.workspace.messages",
        "The onboarding notes",
    );
    for (id, label) in actions {
        message = message.with_action(
            ActionSpec::operation(
                id,
                label,
                tilo_actions::OperationKind::View,
                "test.note",
                &(),
            )
            .unwrap(),
        );
    }
    vec![ResourceSpec::new("tl:so.tilo.chat:root", "chat.workspace", "Chat").with_child(message)]
}

fn search_palette(resources: &[ResourceSpec], provider: Box<dyn SearchProvider>) -> ActionPalette {
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(resources);
    palette.set_providers(vec![provider]);
    palette.open();
    palette.set_query("note");
    while palette.advance_search_page().pending {}
    palette
}

#[test]
fn an_action_does_not_repeat_the_result_it_acts_on() {
    let palette = search_palette(
        &published_tree(vec![("open", "Open"), ("edit", "Edit"), ("pin", "Pin")]),
        Box::new(OneHit("The onboarding notes")),
    );
    let rows = palette.result_rows();
    assert!(
        rows.iter()
            .any(|(title, subtitle)| title == "The onboarding notes"
                && subtitle.as_deref() == Some("Message")),
        "the result reads as itself: {rows:?}"
    );
    let actions: Vec<&(String, Option<String>)> = rows
        .iter()
        .filter(|(title, _)| matches!(title.as_str(), "Open" | "Edit" | "Pin"))
        .collect();
    assert_eq!(actions.len(), 3, "one row per action: {rows:?}");
    for (title, subtitle) in actions {
        assert_eq!(
            subtitle.as_deref(),
            None,
            "{title:?} repeats the result's name under it"
        );
    }
}

#[test]
fn a_preview_that_only_echoes_the_row_is_not_drawn() {
    let mut echoed = SearchResult::new("Notes", "The onboarding notes", "note:1")
        .with_preview("The onboarding notes");
    assert!(
        echoed.preview.is_none(),
        "an echo of the title previews nothing"
    );
    echoed.preview = Some(crate::search::SearchPreview::from("The onboarding notes"));
    assert!(
        crate::search::SearchPreview::from("The onboarding notes").restates("The onboarding notes")
    );

    let rich = SearchResult::new("Notes", "The onboarding notes", "note:1")
        .with_preview("Morning! The new onboarding build is on the test machines.");
    assert!(
        rich.preview.is_some(),
        "a body the title does not say is kept"
    );

    let headed = SearchResult::new("Feeds", "Rust Blog", "feed:1")
        .with_preview(crate::search::SearchPreview::titled("Rust Blog").body("Rust Blog"));
    assert!(headed.preview.is_some(), "a heading is the preview's own");
}

#[test]
fn a_list_that_fits_the_card_is_told_apart_from_one_that_does_not() {
    let mut palette = ActionPalette::rows(Vec::new());
    palette.set_max_list_height(420.0);
    let groups = 3;
    let capacity = palette.entry_row_capacity_for(groups, false);
    let rows = |count: usize| -> Vec<CommandEntry> {
        (0..count)
            .map(|index| CommandEntry::new(format!("r{index}"), format!("Row {index}")))
            .collect()
    };
    assert!(
        palette.entries_fit(&rows(capacity), groups),
        "{capacity} rows stand under {groups} headers inside the cap"
    );
    assert!(
        !palette.entries_fit(&rows(capacity + 1), groups),
        "one row more would be cut at the card's edge"
    );
}

fn message_with(action: ActionSpec) -> ResourceSpec {
    ResourceSpec::new(
        "tl:so.tilo.chat:root/messages/m1",
        "chat.workspace.messages",
        "The onboarding notes",
    )
    .with_action(action)
}

fn an_action(
    id: &str,
    label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> tilo_actions::ActionSpec {
    ActionSpec::typed(
        id,
        label,
        label,
        ExecutionLocus::ClientLocal,
        params,
        |_args| EncodedAction {
            type_id: "test.note".into(),
            payload: Vec::new(),
        },
    )
}

/// What a row cannot take, the palette does not offer: a value picked where it
/// lives, and an action nobody can run now. What it does offer keeps the
/// picture and the chord the toolkit knows it by.
#[test]
fn the_palette_offers_only_what_a_row_can_take_now_with_its_own_picture_and_key() {
    use tilo_actions::{ActionParam, ParamType, Pick};

    let mut message = message_with(an_action(
        "assign",
        "Assign to…",
        vec![ActionParam::required("who", ParamType::Object, "Who").picking(Pick::Grantee)],
    ));
    message = message.with_action(an_action(
        "remove",
        "Remove",
        vec![
            ActionParam::required("message", ParamType::Object, "Message").picking(
                Pick::Resource {
                    kind: "chat.workspace.messages".into(),
                },
            ),
        ],
    ));
    message = message.with_action(
        an_action("snooze", "Snooze", Vec::new())
            .unavailable("nothing_selected", "Choose a message first"),
    );
    message = message.with_action(an_action("undo", "Undo", Vec::new()));
    let resources = vec![
        ResourceSpec::new("tl:so.tilo.chat:root", "chat.workspace", "Chat").with_child(message),
    ];

    let palette = search_palette(&resources, Box::new(OneHit("The onboarding notes")));
    let rows = palette.result_rows();
    let labels: Vec<&str> = rows.iter().map(|(title, _)| title.as_str()).collect();
    for row in ["Assign to…", "Remove", "Snooze"] {
        assert!(
            !labels.contains(&row),
            "a row is listed only when a person can take it now: {rows:?}"
        );
    }
    assert!(labels.contains(&"Undo"), "{rows:?}");

    let rows = palette.result_hits();
    let undo = rows
        .iter()
        .find(|row| row.title == "Undo")
        .expect("Undo is a row");
    assert_eq!(
        undo.icon,
        Some(crate::icons::Icon::Undo2),
        "a host command is drawn as the toolkit draws it"
    );
    let platform = crate::platform::PlatformConventions::default();
    let Some(crate::search::SearchRow::Command(command)) = &undo.row else {
        panic!("a command row is painted from its command: {undo:?}");
    };
    assert_eq!(
        command.keycaps,
        Some(
            crate::platform::HostShortcut::Undo
                .shortcut(platform)
                .keycap_labels(platform)
        ),
        "and shows the chord the press means, from the one shortcut table"
    );
}

#[test]
fn a_hit_that_runs_an_action_is_its_row() {
    let resources = vec![
        ResourceSpec::new("tl:so.tilo.chat:root", "chat.workspace", "Chat")
            .with_action(
                ActionSpec::operation(
                    "open",
                    "Open",
                    tilo_actions::OperationKind::View,
                    "test.note",
                    &(),
                )
                .unwrap(),
            )
            .with_action(an_action("pin", "Pin", Vec::new())),
    ];
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(&resources);
    palette.set_providers(vec![Box::new(crate::search::OpenResourceSearch::new(
        &resources,
    ))]);
    palette.open();
    while palette.advance_search_page().pending {}

    let rows = palette.result_rows();
    let labels: Vec<&str> = rows.iter().map(|(title, _)| title.as_str()).collect();
    assert_eq!(
        labels.iter().filter(|label| **label == "Chat").count(),
        1,
        "the app's own row names it once: {rows:?}"
    );
    assert!(
        !labels.contains(&"Open"),
        "its row opens it, so no second row does: {rows:?}"
    );
    assert!(labels.contains(&"Pin"), "{rows:?}");
}

#[test]
fn an_action_no_row_lists_is_still_asked_for_by_address() {
    use tilo_actions::{ActionParam, ParamType};

    let resources = vec![
        ResourceSpec::new("tl:so.tilo.chat:root", "chat.workspace", "Chat").with_action(an_action(
            "forward",
            "Forward",
            vec![
                ActionParam::required("to", ParamType::Text, "To"),
                ActionParam::required("subject", ParamType::Text, "Subject"),
            ],
        )),
    ];
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(&resources);
    palette.open();
    while palette.advance_search_page().pending {}

    let rows = palette.result_rows();
    assert!(
        !rows.iter().any(|(title, _)| title == "Forward"),
        "a row cannot take two arguments, so none lists it: {rows:?}"
    );
    assert!(
        palette.activate_address("tl:so.tilo.chat:root", "forward"),
        "the app's own control still asks for it"
    );
    assert!(
        palette.in_argument_mode(),
        "and the field asks the arguments it takes"
    );
}
