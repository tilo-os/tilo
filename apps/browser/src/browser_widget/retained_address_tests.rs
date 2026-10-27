// Included in the BrowserWidget production-path test module.
#[test]
fn hosted_address_edits_retain_the_page_and_its_viewport() {
    struct Hosted;
    impl Drop for Hosted {
        fn drop(&mut self) {
            ui::window_toolbar::set_hosted(false);
            let _ = ui::window_toolbar::take_events();
        }
    }
    ui::window_toolbar::set_hosted(true);
    let _hosted = Hosted;
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let (state, root) = synced(&mut widget);
    layout_at(&mut widget, 900.0, 700.0);
    let resize_count = || {
        engine
            .commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::Resize { .. }))
            .count()
    };
    let initial_resizes = resize_count();

    for text in ["r", "ru", "rus", "rust"] {
        ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Edit { text: text.into() });
        widget.sync(&state, &root);
        assert_eq!(widget.bar_query.as_deref(), Some(text));
        layout_at(&mut widget, 900.0, 700.0);
        assert_eq!(
            resize_count(),
            initial_resizes,
            "an address edit must not replace the retained WebView"
        );
    }
    // Finding in the page through the window bar's search keeps the
    // retained WebView too: the bar hosts the query, the page's engine finds.
    let mut driver = ui::widget::SearchDriver::default();
    driver.open(ui::widget::InPlace::Document);
    driver.drive(
        &mut widget,
        "rust",
        ui::widget::FindOptions::default(),
        None,
        false,
    );
    layout_at(&mut widget, 900.0, 700.0);
    assert!(
        engine
            .commands()
            .iter()
            .any(|command| matches!(command, WebViewCommand::Find { text, .. } if text == "rust")),
        "the page's engine finds"
    );
    assert_eq!(
        resize_count(),
        initial_resizes,
        "finding must not replace the retained WebView"
    );
}
