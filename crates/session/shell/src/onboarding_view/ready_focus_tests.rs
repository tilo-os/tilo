/// Every keyboard stop of `widget`'s subtree, in the order Tab visits
/// them, each with what a screen reader says for it.
fn keyboard_stops(widget: &dyn Widget, stops: &mut Vec<(String, Option<String>)>) {
    if widget.is_focusable() && widget.is_enabled() {
        stops.push((
            widget.name().to_owned(),
            widget.accessibility().and_then(|node| node.name),
        ));
    }
    for child in widget.children() {
        keyboard_stops(child, stops);
    }
}

/// The ready screen at the size of the virtual machines the release gates
/// boot (1280×800), where its page overflows: Tab visits the page's scroll
/// region and Continue to desktop, and each is named, so the region is not
/// announced as an unnamed control (which is what Tab from the action
/// reached before the region had a name).
#[test]
fn the_ready_screen_keyboard_stops_are_named() {
    let (mut view, _) = view();
    let mut doc = AutoCommit::new();
    for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
        press(&view, &mut doc, key);
        view.sync(&doc, &automerge::ROOT);
    }
    press(&view, &mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
    view.sync(&doc, &automerge::ROOT);
    press(&view, &mut doc, KEY_BACKUP_CONTINUE);
    sync_journaled(&mut view, &doc);
    if view.codec_page_visible() {
        press(&view, &mut doc, KEY_CODECS_SKIP);
        sync_journaled(&mut view, &doc);
    }
    assert!(
        view.identity_ready_page,
        "the walk ends on the ready screen"
    );

    let theme = Theme::dark();
    let mut text = ui::text_render::TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = view.build_layout(&mut taffy, &theme, &mut node_map);
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(1280.0),
                height: taffy::AvailableSpace::Definite(800.0),
            },
            |known, available, node_id, _, _| {
                measure_map
                    .get(&node_id)
                    .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                        (*ptr).measure(known, available, &mut text)
                    })
            },
        )
        .expect("ready layout");
    view.apply_layout(&taffy, node, Point::ZERO);

    let mut stops = Vec::new();
    keyboard_stops(&view, &mut stops);
    assert!(
        stops
            .iter()
            .all(|(_, name)| name.as_deref().is_some_and(|name| !name.is_empty())),
        "every keyboard stop is named: {stops:?}"
    );
    let names: Vec<(&str, Option<&str>)> = stops
        .iter()
        .map(|(widget, name)| (widget.as_str(), name.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            ("Scrollable", Some("Ready")),
            ("Button", Some("Continue to desktop"))
        ],
        "the page's scroll region is named after its step"
    );
}
