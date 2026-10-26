//! Slides' controls live in the window's bar: the slide list's toggle, the
//! deck's title, Editor and Overview, Insert, Present and the properties'
//! toggle, each running the deck's published action for it.

use ui::window_toolbar::{ToolbarEvent, WindowToolbar};

use super::super::bench::{Bench, WINDOW, deck_of};
use super::super::{ElementKind, Layout, Mode, Slide, SlidesApp, SlidesEffect};
use super::{EDITOR, INSERT, OVERVIEW, PRESENT, PROPERTIES, ZOOM, insert_id};

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

/// Press a slot of the window's bar, as the desktop hands the press to the
/// editor, and let what it asked for run.
fn press(bench: &mut Bench, event: ToolbarEvent) {
    bench.app.harness().press_window_slot(event);
    bench.frame();
    bench.frame();
}

fn toolbar(bench: &mut Bench) -> WindowToolbar {
    bench.read(SlidesApp::window_toolbar)
}

fn action_ids(toolbar: &WindowToolbar) -> Vec<&str> {
    toolbar
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect()
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

#[test]
fn the_editor_draws_no_header_row_and_its_controls_are_in_the_window_bar() {
    let mut bench = Bench::sample();
    let title = bench.deck().title;
    let toolbar = toolbar(&mut bench);
    let location = toolbar.location.clone().expect("the deck is the location");
    assert_eq!(location.label, title, "the bar names the deck");
    assert_eq!(
        location.value, title,
        "an edit starts from the deck's title"
    );
    assert!(location.editable, "the deck is renamed in the bar");
    let views = toolbar
        .views
        .clone()
        .expect("Editor and Overview are views");
    let ids: Vec<&str> = views.choices.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(ids, [EDITOR, OVERVIEW]);
    assert_eq!(views.chosen, EDITOR);
    assert_eq!(toolbar.sidebar, Some(true), "the docked slide list is open");
    assert_eq!(action_ids(&toolbar), [INSERT, PRESENT, PROPERTIES, ZOOM]);
    let insert = &toolbar.actions[0];
    let tools: Vec<&str> = insert.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        tools,
        [
            ElementKind::Text,
            ElementKind::Rect,
            ElementKind::Ellipse,
            ElementKind::Line,
            ElementKind::Image,
        ]
        .map(insert_id)
    );

    let (drawn, stray) = bench.read(|app| {
        let mut drawn = Vec::new();
        labels(
            &ui::Widget::accessibility(app).expect("the editor's semantics"),
            &mut drawn,
        );
        let chrome = app.compute_chrome();
        let in_a_panel = |rect: vello::kurbo::Rect| {
            chrome.rail.contains(rect.center()) || chrome.inspector.contains(rect.center())
        };
        let stray: Vec<String> = chrome
            .buttons
            .iter()
            .filter(|button| !in_a_panel(button.rect))
            .map(|button| app.semantic_target(&button.hit))
            .chain(
                chrome
                    .labels
                    .iter()
                    .filter(|label| {
                        !chrome.rail.contains(label.pos) && !chrome.inspector.contains(label.pos)
                    })
                    .map(|label| label.text.clone()),
            )
            .collect();
        (drawn, stray)
    });
    assert!(
        stray.is_empty(),
        "the slide stands alone between the panels, with no header row: {stray:?}"
    );
    for own in [
        "slides.control.present",
        "slides.control.overview",
        "slides.control.toggle-slides",
        "slides.control.toggle-properties",
        "slides.control.more-tools",
        "Start presentation",
        "Show properties",
        "Hide properties",
        "Show slides",
        "Hide slides",
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the editor draws no {own} of its own: {drawn:?}"
        );
    }
}

#[test]
fn the_bar_switches_between_the_editor_and_the_overview_with_the_same_slots() {
    let mut bench = Bench::sample();
    let editor = toolbar(&mut bench);
    press(&mut bench, action(OVERVIEW));
    assert_eq!(bench.read(|app| app.mode), Mode::Overview);
    let overview = toolbar(&mut bench);
    assert_eq!(
        overview.views.as_ref().map(|views| views.chosen.as_str()),
        Some(OVERVIEW)
    );
    assert_eq!(
        overview.sidebar,
        Some(false),
        "the overview shows no slide list"
    );
    assert_eq!(
        action_ids(&overview),
        action_ids(&editor),
        "the bar keeps its slots between the views"
    );
    assert!(
        overview.location.is_some(),
        "the overview still names the deck"
    );

    press(&mut bench, action(EDITOR));
    assert_eq!(bench.read(|app| app.mode), Mode::Edit);

    press(&mut bench, action(OVERVIEW));
    press(&mut bench, ToolbarEvent::ToggleSidebar);
    assert_eq!(
        bench.read(|app| (app.mode, app.rail_open())),
        (Mode::Edit, true),
        "the slide list's toggle returns to the editor with the list shown"
    );

    press(&mut bench, action(OVERVIEW));
    let before = bench.deck().slides[0].elements.len();
    press(&mut bench, action(&insert_id(ElementKind::Rect)));
    assert_eq!(
        bench.read(|app| app.mode),
        Mode::Edit,
        "Insert returns to the editor"
    );
    assert_eq!(bench.deck().slides[0].elements.len(), before + 1);
}

#[test]
fn the_bar_shows_and_hides_the_slide_list_and_the_properties() {
    let mut bench = Bench::sample();
    assert!(bench.read(SlidesApp::rail_open));
    press(&mut bench, ToolbarEvent::ToggleSidebar);
    assert!(
        !bench.read(SlidesApp::rail_open),
        "the toggle hides the list"
    );
    assert_eq!(toolbar(&mut bench).sidebar, Some(false));
    press(&mut bench, ToolbarEvent::ToggleSidebar);
    assert!(bench.read(SlidesApp::rail_open), "and shows it again");

    let open = bench.read(SlidesApp::inspector_open);
    press(&mut bench, action(PROPERTIES));
    assert_eq!(bench.read(SlidesApp::inspector_open), !open);
    let toggle = toolbar(&mut bench)
        .actions
        .into_iter()
        .find(|action| action.id == PROPERTIES)
        .expect("the properties' toggle");
    assert_eq!(toggle.on, !open, "the toggle shows the properties' state");
}

#[test]
fn the_bar_presents_and_declares_nothing_while_presenting() {
    let mut bench = Bench::sample();
    press(&mut bench, action(PRESENT));
    assert_eq!(bench.read(|app| app.mode), Mode::Present);
    assert_eq!(toolbar(&mut bench), WindowToolbar::default());
}

#[test]
fn the_bar_inserts_a_shape_as_one_undo_step() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    let before = bench.deck().slides[0].elements.len();
    press(&mut bench, action(&insert_id(ElementKind::Rect)));
    let deck = bench.deck();
    assert_eq!(deck.slides[0].elements.len(), before + 1);
    let inserted = deck.slides[0].elements.last().expect("the inserted shape");
    assert_eq!(inserted.kind, ElementKind::Rect);
    assert_eq!(
        bench.read(|app| app.sel.clone()).as_deref(),
        Some(inserted.id.as_str()),
        "the inserted shape is selected"
    );
    assert!(bench.undo(), "an insert is one undo step");
    assert_eq!(bench.deck().slides[0].elements.len(), before);
}

#[test]
fn the_location_renames_the_deck_as_one_undo_step() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    press(
        &mut bench,
        ToolbarEvent::Commit {
            text: " Quarterly review ".into(),
        },
    );
    assert_eq!(bench.deck().title, "Quarterly review");
    assert_eq!(
        toolbar(&mut bench)
            .location
            .map(|location| location.label)
            .as_deref(),
        Some("Quarterly review")
    );
    press(&mut bench, ToolbarEvent::Commit { text: "  ".into() });
    assert_eq!(
        bench.deck().title,
        "Quarterly review",
        "a blank title keeps the deck's"
    );
    assert!(bench.undo(), "the rename is one undo step");
    assert_eq!(bench.deck().title, "t");
}

#[test]
fn a_viewer_can_neither_rename_nor_insert_from_the_bar() {
    let mut bench = Bench::open_as(
        &deck_of(vec![Slide::new(Layout::Blank)]),
        WINDOW,
        ui::permissions::Role::Viewer,
    );
    let declared = toolbar(&mut bench);
    assert!(!declared.location.expect("the location").editable);
    let insert = declared
        .actions
        .into_iter()
        .find(|action| action.id == INSERT)
        .expect("the Insert menu");
    assert!(!insert.enabled, "a viewer cannot insert");
    press(
        &mut bench,
        ToolbarEvent::Commit {
            text: "nope".into(),
        },
    );
    assert_eq!(bench.deck().title, "t", "a viewer's rename changes nothing");
}

#[test]
fn the_slide_list_and_the_properties_are_published_actions() {
    let mut bench = Bench::sample();
    bench
        .app
        .effect(&SlidesEffect::ShowSlideList { shown: false })
        .expect("the slide list hides");
    bench.frame();
    assert!(!bench.read(SlidesApp::rail_open));
    bench
        .app
        .effect(&SlidesEffect::ShowSlideList { shown: true })
        .expect("the slide list shows");
    bench.frame();
    assert!(bench.read(SlidesApp::rail_open));

    bench
        .app
        .effect(&SlidesEffect::ShowProperties { shown: false })
        .expect("the properties hide");
    bench.frame();
    assert!(!bench.read(SlidesApp::inspector_open));
    bench
        .app
        .effect(&SlidesEffect::ShowProperties { shown: true })
        .expect("the properties show");
    bench.frame();
    assert!(bench.read(SlidesApp::inspector_open));
}

#[test]
fn a_script_renames_the_deck_through_the_published_title_edit_the_bar_uses() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    bench
        .app
        .call(
            "field-edit",
            serde_json::json!({
                "edit": {"path": [{"field": "title"}], "op": "replace", "text": "Field Notes"}
            }),
        )
        .expect("the deck publishes its title edit");
    bench.frame();
    assert_eq!(bench.deck().title, "Field Notes");
    assert_eq!(
        toolbar(&mut bench)
            .location
            .map(|location| location.label)
            .as_deref(),
        Some("Field Notes"),
        "the bar names the renamed deck"
    );

    press(
        &mut bench,
        ToolbarEvent::Commit {
            text: "Pilot review".into(),
        },
    );
    assert_eq!(bench.deck().title, "Pilot review");
    let history = bench.app.history();
    let renames = bench.app.sentences(history.iter().rev().take(2));
    assert_eq!(
        renames,
        ["Renamed the deck", "Renamed the deck"],
        "the bar renames through the same published title edit"
    );
}
