/// Every key a setup button names is a message both shipped languages
/// carry, so no window paints a key.
#[test]
fn the_setup_buttons_name_messages_the_catalogues_carry() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    for locale in ["en-US", "de-DE"] {
        catalog.set_locale(locale);
        for key in [
            "installer-preview-continue",
            "imager-setup-action-start-download",
            "imager-setup-action-pause-download",
            "imager-setup-action-cancel-download",
            "imager-setup-action-resume-download",
            "imager-setup-action-release-download",
            "imager-setup-action-discard-download",
            "imager-setup-internal-install",
            "imager-setup-internal-continue",
            "imager-setup-internal-installing",
            "imager-setup-desktop-install",
            "imager-setup-desktop-cancel",
            "imager-setup-desktop-recheck",
            "installer-desktop-retry",
            "imager-setup-internal-remedy-firmware",
            "imager-setup-internal-remedy-fast-startup",
            "imager-setup-internal-recheck",
            "imager-setup-internal-resume-retry",
            "imager-setup-internal-resume-remove",
            "imager-setup-internal-restart-later",
            "imager-setup-internal-restart-now",
        ] {
            assert_ne!(catalog.t(key), key, "{key} is missing from {locale}");
        }
    }
    catalog.set_locale(&previous);
}

/// A German window reads the review's buttons in German.
#[test]
fn the_setup_buttons_read_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let mut app = supported_fixture();
    review(&mut app, Experience::FullOs);
    let harness = FrameHarness::new(ui::State::new(), Box::new(app), (960, 720));
    let labels: Vec<String> = harness
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    catalog.set_locale(&previous);
    for german in ["Tilo installieren", "Tilo herunterladen"] {
        assert!(
            labels.iter().any(|label| label == german),
            "{german} is missing from {labels:?}"
        );
    }
    assert!(
        labels
            .iter()
            .all(|label| !matches!(label.as_str(), "Install Tilo" | "Download Tilo")),
        "the source language is left in the German window: {labels:?}"
    );
}
