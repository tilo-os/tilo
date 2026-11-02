// A result and the actions on it are rows of one list. The result names itself once; its
// actions read as what each does, never as a second copy of the result's name; and the band
// under the list shows the result, never an echo of the row under it.

/// A provider whose one hit is `title`, found by any query containing "note": what an app's
/// own search lists when a person finds a message or a note.
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

/// What the app publishes: its root, holding the message a person found. The message is an
/// item of the root (`chat.workspace/messages/…`), so its actions read with the item's name
/// under them, as Chat's do (`tilo_actions::listed_row`).
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
    // The result's own row names it, with the note's kind under it.
    assert!(
        rows.iter()
            .any(|(title, subtitle)| title == "The onboarding notes"
                && subtitle.as_deref() == Some("Message")),
        "the result reads as itself: {rows:?}"
    );
    // Its actions read as what they do: no action row says the result's name again.
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
    // The hit's preview is its own title: the band would say it a second time.
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

    // A preview that says more than the title (the body of a message, its sender) stays.
    let rich = SearchResult::new("Notes", "The onboarding notes", "note:1")
        .with_preview("Morning! The new onboarding build is on the test machines.");
    assert!(
        rich.preview.is_some(),
        "a body the title does not say is kept"
    );

    // A headed preview keeps its heading even where the body matches.
    let headed = SearchResult::new("Feeds", "Rust Blog", "feed:1")
        .with_preview(crate::search::SearchPreview::titled("Rust Blog").body("Rust Blog"));
    assert!(headed.preview.is_some(), "a heading is the preview's own");
}

/// A list is told apart from one the card would cut: as many rows as
/// the cap holds stand inside it, under their group headers, and one
/// more does not.
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
