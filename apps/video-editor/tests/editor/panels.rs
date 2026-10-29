//! The editor's panels as a window changes size: a fresh editor opens empty,
//! a narrow window's bin is a modal drawer, and a dimmed editor behind a
//! sheet takes no clicks.

use std::collections::HashMap;

use ui::accesskit::{NodeId, Role};
use ui::prelude::*;
use video_editor::VideoEditor;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;

fn editor(size: (u32, u32)) -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.harness().resize(size).unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app.settle();
    app
}

/// The pane the keyboard cursor is in, as a reader finds it.
fn pane(app: &mut HeadlessApp<VideoEditor>) -> String {
    let nodes = app.harness().a11y_nodes();
    let (mut at, _) = app.harness().a11y_focus().expect("something is focused");
    let parent: HashMap<NodeId, NodeId> = nodes
        .iter()
        .flat_map(|(id, n)| n.children().iter().map(move |c| (*c, *id)))
        .collect();
    loop {
        if let Some(label) = nodes
            .iter()
            .find(|(id, _)| *id == at)
            .and_then(|(_, n)| n.label())
            .filter(|l| ["Media bin", "Transport", "Timeline", "Inspector"].contains(l))
        {
            return label.to_string();
        }
        at = *parent.get(&at).expect("the focus sits inside a pane");
    }
}

fn playhead(app: &mut HeadlessApp<VideoEditor>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some("Transport"))
        .and_then(|(_, n)| n.value().map(ToString::to_string))
        .unwrap()
}

#[test]
fn a_fresh_editor_opens_blank_and_says_how_to_start() {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    let project = app.state();
    assert!(
        project.clips.is_empty() && project.media.is_empty(),
        "nothing fabricated"
    );
    let said: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, n)| n.role() == Role::Label)
        .filter_map(|(_, n)| n.label().map(ToString::to_string))
        .collect();
    assert!(
        said.iter().any(|s| s.starts_with("No media yet")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|s| s.starts_with("Timeline is empty")),
        "{said:?}"
    );
}

/// Select the drone clip and park the playhead inside it, so S has
/// something to split.
fn ready_to_split(app: &mut HeadlessApp<VideoEditor>) {
    let clip = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| {
            n.label() == Some("Drone_Coastline.mov")
                && n.value().is_some_and(|v| v.contains(" on "))
        })
        .unwrap()
        .0;
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
    app.harness().key("g");
    app.harness().type_text("0:04");
    app.harness().key("Enter");
}

#[test]
fn a_narrow_windows_bin_is_a_modal_drawer_that_returns_focus() {
    let mut app = editor((720, 820));
    ready_to_split(&mut app);
    let clips = app.state().clips.len();

    app.harness().key("b");
    assert_eq!(
        pane(&mut app),
        "Media bin",
        "opening moves focus into the bin"
    );
    app.harness().key("s");
    assert_eq!(app.state().clips.len(), clips, "timeline edits are refused");

    app.harness().key("b");
    assert_eq!(pane(&mut app), "Timeline", "closing returns focus");
    app.harness().key("s");
    assert_eq!(
        app.state().clips.len(),
        clips + 1,
        "and the timeline edits again"
    );
}

#[test]
fn a_press_on_the_dimmed_editor_closes_the_drawer_and_does_nothing_else() {
    let mut app = editor((720, 820));
    let before = playhead(&mut app);
    app.harness().key("b");
    // Near the right edge, low down: covered ruler and lanes, never the drawer.
    app.harness().click_at(700.0, 600.0);
    assert_eq!(playhead(&mut app), before, "the covered timeline is inert");
    assert_eq!(
        pane(&mut app),
        "Timeline",
        "the drawer closed and focus came back"
    );
}

#[test]
fn widening_past_the_breakpoint_docks_the_bin_again() {
    let mut app = editor((720, 820));
    ready_to_split(&mut app);
    let clips = app.state().clips.len();
    app.harness().key("b");
    app.harness().resize((1280, 800)).unwrap();
    app.settle();
    app.harness().key("s");
    assert_eq!(
        app.state().clips.len(),
        clips + 1,
        "a docked bin traps nothing"
    );
}

#[test]
fn the_shortcut_sheet_and_go_to_field_swallow_clicks_on_the_editor() {
    // A press and drag across the lanes, where clips sit.
    let drag = |app: &mut HeadlessApp<VideoEditor>| {
        app.harness().drag_at((400.0, 660.0), (700.0, 660.0));
    };
    let mut bare = editor((1280, 800));
    let before = bare.state().clips;
    drag(&mut bare);
    assert_ne!(
        bare.state().clips,
        before,
        "with nothing open, the drag edits"
    );

    for open in ["?", "g"] {
        let mut app = editor((1280, 800));
        let before = app.state().clips;
        app.harness().key(open);
        drag(&mut app);
        app.harness().key("Escape");
        assert_eq!(
            app.state().clips,
            before,
            "{open}: the drag reached the timeline through the scrim"
        );
    }
}
