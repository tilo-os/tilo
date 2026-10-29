use super::*;
use ui::widget::Scrollable;

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
/// boot (1280×800): its page fits, so Tab has one stop, Continue to desktop,
/// and it is named. (When the page overflowed by a line its unnamed scroll
/// region was a second stop, and Tab from the action landed there.)
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
    lay_out_as_root(&mut view, 1280.0, 800.0, &theme);

    // The shell's one stop is the setup app; Tab then walks the app's own.
    let mut stops = Vec::new();
    keyboard_stops(ceremony(&mut view), &mut stops);
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
    assert_eq!(names, [("Button", Some("Continue to desktop"))]);
}

/// Lay `view` out in a `width`×`height` window, as the shell does.
fn lay_out(view: &mut OnboardingView, width: f32, height: f32) -> Vec<*mut dyn Widget> {
    let theme = Theme::dark();
    let LaidOut {
        nodes: node_map, ..
    } = lay_out_as_root(view, width, height, &theme);
    node_map.into_values().collect()
}

/// How far each scroll region of the laid-out page overflows.
fn overflow(widgets: Vec<*mut dyn Widget>) -> Vec<f32> {
    widgets
        .into_iter()
        .filter_map(|ptr| {
            // SAFETY: the pointers belong to the tree just laid out.
            let widget = unsafe { &mut *ptr };
            widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Scrollable>())
                .map(|scroll| scroll.max_offset_y())
        })
        .collect()
}

/// The steps of a first setup whose copy is short fit their panel without
/// scrolling, at the gates' 1280×800 and at 800×600: a scroll region on a
/// page that has one action to take is a stop that scrolls nothing. The
/// backup step is a form whose fields bring themselves into view, and its
/// page may scroll.
#[test]
fn the_short_setup_steps_fit_without_scrolling() {
    for (width, height) in [(1280.0_f32, 800.0_f32), (800.0, 600.0)] {
        let (mut view, _) = view();
        let mut doc = AutoCommit::new();
        let fits = |view: &mut OnboardingView, step: &str| {
            let overflow = overflow(lay_out(view, width, height));
            assert!(
                overflow.iter().all(|&by| by == 0.0),
                "{step} at {width}x{height} overflows its panel by {overflow:?}"
            );
        };
        fits(&mut view, "the introduction");
        press(&view, &mut doc, KEY_BEGIN);
        view.sync(&doc, &automerge::ROOT);
        fits(&mut view, "protecting the identity");
        press(&view, &mut doc, KEY_UNDERSTOOD);
        view.sync(&doc, &automerge::ROOT);
        press(&view, &mut doc, KEY_CREATE);
        view.sync(&doc, &automerge::ROOT);
        sync_journaled(&mut view, &doc);
        press(&view, &mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
        view.sync(&doc, &automerge::ROOT);
        press(&view, &mut doc, KEY_BACKUP_CONTINUE);
        sync_journaled(&mut view, &doc);
        assert!(
            view.identity_ready_page,
            "the walk ends on the ready screen"
        );
        fits(&mut view, "the ready screen");
    }
}
