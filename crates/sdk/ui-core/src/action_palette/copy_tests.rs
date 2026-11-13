// What the palette tells assistive technology it is, in the window's language.

/// The toolkit's own catalogue, as a window of `locale` reads it.
fn speak(locale: &str) {
    let mut catalogues = vec![("en-US", "")];
    if locale != "en-US" {
        catalogues.push((locale, ""));
    }
    crate::i18n::I18nManager::install("en-US", locale, &catalogues)
        .expect("the window's catalogues install");
}

/// The palette's query field, as an assistive client finds it.
fn query(palette: &ActionPalette) -> crate::a11y::AccessNode {
    palette
        .accessibility()
        .expect("an open palette is a dialog")
        .children
        .into_iter()
        .find(|node| node.role == crate::a11y::Role::TextInput)
        .expect("the palette's query is a text field")
}

/// A German screen reader hears the palette, and the step that collects one
/// action's argument, in German rather than in the source catalogue's words.
#[test]
fn the_palette_names_itself_in_the_windows_language() {
    use tilo_actions::{ActionParam, ParamType};

    speak("en-US");
    let mut palette = ActionPalette::new();
    palette.open();
    assert_eq!(
        palette
            .accessibility()
            .expect("an open palette is a dialog")
            .name
            .as_deref(),
        Some("Command palette")
    );
    // The query is the search field it is, and what it covers is what the
    // empty field says, so a screen reader announces each once.
    assert_eq!(query(&palette).name.as_deref(), Some("Search"));
    assert_eq!(
        query(&palette).description.as_deref(),
        Some("Search actions and results")
    );

    speak("de-DE");
    let mut palette = ActionPalette::new();
    palette.open();
    assert_eq!(
        palette
            .accessibility()
            .expect("an open palette is a dialog")
            .name
            .as_deref(),
        Some("Befehlspalette")
    );
    assert_eq!(query(&palette).name.as_deref(), Some("Suche"));
    assert_eq!(
        query(&palette).description.as_deref(),
        Some("Aktionen und Ergebnisse durchsuchen")
    );

    let resources = vec![
        ResourceSpec::new("tl:so.tilo.chat:root", "chat.workspace", "Chat").with_action(an_action(
            "forward",
            "Forward",
            vec![ActionParam::required("to", ParamType::Text, "To")],
        )),
    ];
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("de-DE")
    });
    palette.load_open_resources(&resources);
    palette.open();
    assert!(
        palette.activate_address("tl:so.tilo.chat:root", "forward"),
        "the action asking for its argument opens the step that collects it"
    );
    assert!(palette.in_argument_mode());
    assert_eq!(
        palette
            .accessibility()
            .expect("the argument step is a dialog")
            .name
            .as_deref(),
        Some("Aktionsbestätigung")
    );
    // The step is asked in the action's own words, and the field says them
    // once: its name, not a description echoing it.
    assert_eq!(query(&palette).name.as_deref(), Some("To"));
    assert_eq!(query(&palette).description, None);
}

/// A search over what the host found is named as an app's palette names its
/// own: the field is the search field it is, the words the empty field shows
/// say what it searches, and a step asks in the host's own words — so a screen
/// reader hears the same thing from the desktop's search bar.
#[test]
fn the_hosts_search_names_its_field_as_the_palette_does() {
    let open = |placeholder: &str| {
        let mut palette = ActionPalette::rows(vec![CommandEntry::new("apps", "Mail")]);
        palette.set_query_placeholder(placeholder);
        palette.open();
        palette
    };

    speak("en-US");
    let palette = open("Search apps, windows, documents and actions\u{2026}");
    assert_eq!(query(&palette).name.as_deref(), Some("Search"));
    assert_eq!(
        query(&palette).description.as_deref(),
        Some("Search apps, windows, documents and actions\u{2026}")
    );

    speak("de-DE");
    let palette = open("Apps, Fenster, Dokumente und Aktionen durchsuchen\u{2026}");
    assert_eq!(query(&palette).name.as_deref(), Some("Suche"));
    assert_eq!(
        query(&palette).description.as_deref(),
        Some("Apps, Fenster, Dokumente und Aktionen durchsuchen\u{2026}")
    );

    // A step collecting one row's argument is asked in the row's own words,
    // and its name is all a screen reader has to hear of it.
    let step = ArgumentSpec {
        prompt: "Neuer Name".to_owned(),
        placeholder: "Zum Beispiel: Notizen".to_owned(),
        prefill: None,
        kind: ArgumentKind::Text,
    };
    let mut palette = palette;
    palette.open_argument_step("apps", step);
    assert_eq!(query(&palette).name.as_deref(), Some("Neuer Name"));
    assert_eq!(query(&palette).description, None);
}
