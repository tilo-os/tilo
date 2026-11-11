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
}
