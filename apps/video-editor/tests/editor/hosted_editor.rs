#![allow(dead_code, reason = "each test module uses a subset of these")]

//! The editor as the host runs it, driven the way a person or a screen
//! reader drives it: keys, the go-to field and the accessibility tree.

use ui::accesskit::{Node, NodeId, Role};
use ui::prelude::*;
use video_editor::model::{ClipId, TrackKind};
use video_editor::resource::load_project;
use video_editor::{Project, VideoEditor};

pub type Editor = HeadlessApp<VideoEditor>;

/// The editor holding `project`, with the keyboard on its surface.
pub fn open(project: &Project) -> Editor {
    // No test may put a real file chooser on the screen.
    ui::host::install_default(ui::host::HostServices::recording().0);
    let mut app = Editor::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

/// The first exported node matching `want`.
pub fn find(app: &mut Editor, want: impl Fn(&Node) -> bool) -> Option<(NodeId, Node)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| want(n))
}

/// The node named `name`, if the tree has one.
pub fn named(app: &mut Editor, name: &str) -> Option<Node> {
    find(app, |n| n.label() == Some(name)).map(|(_, n)| n)
}

/// What the node named `name` says beyond its name.
pub fn value(app: &mut Editor, name: &str) -> String {
    named(app, name)
        .and_then(|n| n.value().map(str::to_string))
        .unwrap_or_else(|| panic!("no node named {name:?} with a value"))
}

/// Perform `action` on the node named `name`.
pub fn act(app: &mut Editor, name: &str, action: &ui::a11y::Action) -> Result<(), String> {
    let id = app
        .harness()
        .a11y_node_named(name)
        .ok_or_else(|| format!("no node named {name:?}"))?;
    app.harness().perform_a11y_action(id, action)
}

/// Press the control named `name` as a screen reader does.
pub fn press(app: &mut Editor, name: &str) {
    act(app, name, &ui::a11y::Action::Activate).unwrap();
}

/// Select the clip placing `name` on track `track` alone, by activating its
/// timeline node (which also parks the playhead at its head).
pub fn select_on(app: &mut Editor, name: &str, track: &str) {
    let suffix = format!(" on {track}");
    let (id, _) = find(app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some(name)
            && n.value().is_some_and(|v| v.ends_with(&suffix))
    })
    .unwrap_or_else(|| panic!("{name} is not placed on {track}"));
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Activate)
        .unwrap();
}

/// Move the playhead to `timecode` through the go-to field.
pub fn go_to(app: &mut Editor, timecode: &str) {
    app.harness().key("g");
    app.harness().type_text(timecode);
    app.harness().key("Enter");
}

pub fn with_modifier(app: &mut Editor, modifier: &str, key: &str) {
    app.harness().set_modifiers(&[modifier.into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

/// Where the playhead is, as the transport reads it out.
pub fn playhead(app: &mut Editor) -> String {
    value(app, "Transport")
}

/// The clip placing the source `name` on a track of `kind`.
pub fn clip_of(project: &Project, name: &str, kind: TrackKind) -> ClipId {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project
        .clips
        .iter()
        .find(|c| c.media == media && project.clip_kind(c.id) == kind)
        .unwrap_or_else(|| panic!("{name} has no {kind:?} clip"))
        .id
}

/// The name of the track `clip` sits on.
pub fn track_name(project: &Project, clip: ClipId) -> String {
    let track = project.clip(clip).unwrap().track;
    project.track(track).unwrap().name.clone()
}
