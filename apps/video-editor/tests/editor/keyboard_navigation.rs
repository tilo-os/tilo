//! Getting around the editor without a mouse: Tab between panes, arrows
//! inside one, Enter on what the cursor is on, the shortcut sheet and the
//! go-to field — read back through the exported tree, as a reader hears it.

use std::collections::HashMap;

use ui::accesskit::{NodeId, Role};
use ui::prelude::*;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::timecode::timecode;
use video_editor::{Project, VideoEditor};

const PANES: [&str; 4] = ["Media bin", "Transport", "Timeline", "Inspector"];

/// The demo project with the editor holding the keyboard.
fn editor() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

fn press(app: &mut HeadlessApp<VideoEditor>, keys: &[&str]) {
    for key in keys {
        app.harness().key(key);
    }
}

fn with_modifier(app: &mut HeadlessApp<VideoEditor>, modifier: &str, key: &str) {
    app.harness().set_modifiers(&[modifier.into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

/// The focused node's label and the pane it sits in.
fn focus(app: &mut HeadlessApp<VideoEditor>) -> (Option<String>, Option<String>) {
    let nodes = app.harness().a11y_nodes();
    let Some((focused, node)) = app.harness().a11y_focus() else {
        return (None, None);
    };
    let parent: HashMap<NodeId, NodeId> = nodes
        .iter()
        .flat_map(|(id, n)| n.children().iter().map(move |c| (*c, *id)))
        .collect();
    let label = |id: NodeId| {
        nodes
            .iter()
            .find(|(n, _)| *n == id)
            .and_then(|(_, n)| n.label().map(ToString::to_string))
    };
    let mut at = Some(focused);
    let mut pane = None;
    while let Some(id) = at {
        if let Some(l) = label(id).filter(|l| PANES.contains(&l.as_str())) {
            pane = Some(l);
            break;
        }
        at = parent.get(&id).copied();
    }
    (node.label().map(ToString::to_string), pane)
}

fn pane(app: &mut HeadlessApp<VideoEditor>) -> String {
    focus(app).1.expect("the cursor is in a pane")
}

fn focused(app: &mut HeadlessApp<VideoEditor>) -> String {
    focus(app).0.expect("something is focused")
}

fn playhead(app: &mut HeadlessApp<VideoEditor>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some("Transport"))
        .and_then(|(_, n)| n.value().map(ToString::to_string))
        .unwrap()
}

fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

fn select(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let clip = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| {
            n.role() == Role::ListItem
                && n.label() == Some(name)
                && n.value().is_some_and(|v| v.contains(" on "))
        })
        .map(|(id, _)| id)
        .unwrap();
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
}

#[test]
fn tab_walks_every_pane_and_shift_tab_walks_back() {
    let mut app = editor();
    assert_eq!(pane(&mut app), "Timeline");
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.harness().key("Tab");
        seen.push(pane(&mut app));
    }
    assert_eq!(seen, ["Inspector", "Media bin", "Transport", "Timeline"]);
    with_modifier(&mut app, "shift", "Tab");
    assert_eq!(pane(&mut app), "Transport");
}

/// Put the keyboard cursor on the control `name`, as a reader focusing it does.
fn focus_on(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let id = app.harness().a11y_node_named(name).unwrap();
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Focus)
        .unwrap();
}

#[test]
fn arrows_walk_the_bin_without_wrapping_and_the_transport_sideways() {
    let mut app = editor();
    focus_on(&mut app, "Ambient_Pad.wav");
    assert_eq!(pane(&mut app), "Media bin");
    press(&mut app, &["ArrowUp"; 8]);
    let first = focused(&mut app);
    app.harness().key("ArrowDown");
    assert_ne!(focused(&mut app), first, "Down moves to the next row");
    press(&mut app, &["ArrowUp", "ArrowUp"]);
    assert_eq!(focused(&mut app), first, "the first row is the first row");

    focus_on(&mut app, "Jump to start");
    app.harness().key("ArrowRight");
    assert_eq!(focused(&mut app), "Step back one frame");
}

#[test]
fn on_the_timeline_arrows_nudge_and_walk_clips_across_tracks() {
    let mut app = editor();
    select(&mut app, "Interview_A.mp4");
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let before = app.state().clip(interview).unwrap().start;
    app.harness().key("ArrowRight");
    assert!(
        app.state().clip(interview).unwrap().start > before,
        "Right still nudges the selection"
    );

    select(&mut app, "Drone_Coastline.mov");
    app.harness().key("ArrowDown");
    let below = focused(&mut app);
    assert_ne!(below, "Drone_Coastline.mov", "Down reaches another track");
    app.harness().key("ArrowUp");
    let back = focused(&mut app);
    let project = app.state();
    let track_of = |name: &str| {
        let id = clip_of(&project, name);
        project.clip(id).unwrap().track
    };
    assert_eq!(
        track_of(&back),
        track_of("Drone_Coastline.mov"),
        "Up comes back"
    );
}

#[test]
fn enter_activates_the_button_the_cursor_is_on() {
    let mut app = editor();
    focus_on(&mut app, "Play");
    assert_eq!(focused(&mut app), "Play");
    app.harness().key("Enter");
    assert!(
        app.harness().a11y_node_named("Pause").is_some(),
        "Enter on Play starts playback"
    );
}

#[test]
fn the_shortcut_sheet_lists_every_binding_and_closes_on_escape() {
    let mut app = editor();
    app.harness().key("?");
    let nodes = app.harness().a11y_nodes();
    let sheet = nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Dialog && n.label() == Some("Keyboard shortcuts"))
        .map(|(_, n)| n.clone())
        .expect("? opens the sheet");
    let rows: Vec<(String, String)> = sheet
        .children()
        .iter()
        .filter_map(|id| nodes.iter().find(|(n, _)| n == id))
        .filter(|(_, n)| n.role() == Role::Label)
        .map(|(_, n)| {
            (
                ui::spell_shortcut(n.value().unwrap_or_default()),
                n.label().unwrap_or_default().to_string(),
            )
        })
        .collect();
    for combo in [
        "Space",
        "J / K / L",
        "I / O",
        "S",
        "M",
        "T",
        ", / .",
        "Z",
        "+ / -",
        "B",
        "Ctrl+I",
        "Ctrl+D",
        "Ctrl+A",
        "Ctrl+Z",
        "Tab / Shift+Tab",
        "G",
        "?",
        "Shift+I / O",
        "Shift+X",
    ] {
        let spelled = ui::spell_shortcut(combo);
        assert!(
            rows.iter().any(|(k, _)| *k == spelled),
            "{spelled}: {rows:?}"
        );
    }
    for redo in ["Ctrl+Shift+Z", "Ctrl+Y"] {
        let spelled = ui::spell_shortcut(redo);
        assert!(
            rows.iter().any(|(k, what)| *k == spelled && what == "Redo"),
            "{rows:?}"
        );
    }
    assert!(
        rows.iter()
            .any(|(k, what)| k == "C" && what.to_lowercase().contains("clip")),
        "C walks the clipping warnings: {rows:?}"
    );
    assert!(app.harness().a11y_node_named("Close shortcuts").is_some());

    app.harness().key("Escape");
    assert!(
        app.harness()
            .a11y_node_named("Keyboard shortcuts")
            .is_none()
    );
}

#[test]
fn the_go_to_field_jumps_explains_and_owns_the_keyboard() {
    let mut app = editor();
    app.harness().key("g");
    app.harness().type_text("1:30");
    app.harness().key("Enter");
    assert_eq!(playhead(&mut app), timecode(90.0, 30.0));

    // Unreadable: the playhead stays and the field stays open for a fix.
    app.harness().key("g");
    app.harness().type_text(":");
    app.harness().key("Enter");
    assert_eq!(playhead(&mut app), timecode(90.0, 30.0));
    app.harness().key("Backspace");
    app.harness().type_text("0:05");
    app.harness().key("Enter");
    assert_eq!(playhead(&mut app), timecode(5.0, 30.0));

    // While it is open, letters are not edit shortcuts.
    let (clips, markers) = (app.state().clips.len(), app.state().markers.len());
    app.harness().key("g");
    press(&mut app, &["s", "m", "t"]);
    app.harness().key("Escape");
    assert_eq!(app.state().clips.len(), clips, "no clip was split");
    assert_eq!(app.state().markers.len(), markers, "no marker was dropped");
}
