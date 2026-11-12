/// Every key a copy button names is a message both shipped languages
/// carry, so no window paints a key.
#[test]
fn the_copy_buttons_name_messages_the_catalogues_carry() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    for locale in ["en-US", "de-DE"] {
        catalog.set_locale(locale);
        for key in [
            "imager-import-action-choose-source",
            "imager-import-action-choose-destination",
            "imager-import-action-clear-selection",
            "imager-import-action-prepare-review",
            "imager-import-action-first-issues",
            "imager-import-action-next-issues",
            "imager-import-loading",
            "installer-details",
            "imager-import-hide-details",
            "imager-import-action-start",
            "imager-import-action-cancel",
            "imager-import-action-retry",
            "imager-import-action-check-identity",
            "imager-import-action-review-destination",
            "imager-import-action-choose-folder-destination",
            "imager-import-action-later-all",
            "imager-import-action-start-reviewed-folders",
            "imager-journey-accounts",
            "imager-journey-store",
        ] {
            assert_ne!(catalog.t(key), key, "{key} is missing from {locale}");
        }
    }
    catalog.set_locale(&previous);
}

/// A German window reads the copy's buttons in German.
#[test]
fn the_copy_buttons_read_in_the_windows_language() {
    use ui::headless::FrameHarness;
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let harness = FrameHarness::new(
        ui::State::new(),
        Box::new(ImportApp::for_shot("import-first-login")),
        (960, 720),
    );
    let labels: Vec<String> = harness
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    catalog.set_locale(&previous);
    for german in [
        "Geprüfte Ordner kopieren",
        "Für alle Ordner später entscheiden",
    ] {
        assert!(
            labels.iter().any(|label| label == german),
            "{german} is missing from {labels:?}"
        );
    }
    assert!(
        labels
            .iter()
            .all(|label| !matches!(label.as_str(), "Copy folders" | "Decide later for all")),
        "the source language is left in the German window: {labels:?}"
    );
}
