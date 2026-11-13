fn speak(locale: &str) {
    let mut catalogues = vec![("en-US", "")];
    if locale != "en-US" {
        catalogues.push((locale, ""));
    }
    crate::i18n::I18nManager::install("en-US", locale, &catalogues)
        .expect("the window's catalogues install");
}

fn query(palette: &ActionPalette) -> crate::a11y::AccessNode {
    palette
        .accessibility()
        .expect("an open palette is a dialog")
        .children
        .into_iter()
        .find(|node| node.role == crate::a11y::Role::TextInput)
        .expect("the palette's query is a text field")
}

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
    assert_eq!(query(&palette).name.as_deref(), Some("To"));
    assert_eq!(query(&palette).description, None);
}

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
