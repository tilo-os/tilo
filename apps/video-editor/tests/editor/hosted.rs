#![allow(dead_code, reason = "each test module uses a subset of these")]

//! The editor as the host runs it, driven the way a person or a screen
//! reader drives it: keys, and actions on the exported accessibility tree.

use ui::accesskit::{Node, NodeId, Role};
use ui::prelude::*;
use video_editor::resource::load_project;
use video_editor::{Project, VideoEditor};

pub type Editor = HeadlessApp<VideoEditor>;

/// An editor holding `project`, as opening it does.
pub fn editor_with(project: &Project) -> Editor {
    let mut app = Editor::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app
}

/// The demo project: on V1, `Drone_Coastline.mov` over 0–8 s,
/// `Interview_A.mp4` over 8–20 s and `Cutaway_Market.mp4` over 20–26 s; on V2
/// `Logo_Bug.png` and `Sunset_Timelapse.mp4`; narration and a bed on A1/A2.
pub fn sample_editor() -> Editor {
    editor_with(&video_editor::seed::sample_project())
}

/// Every exported node that satisfies `pick`.
pub fn nodes(app: &mut Editor, pick: impl Fn(&Node) -> bool) -> Vec<(NodeId, Node)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, n)| pick(n))
        .collect()
}

/// The one node with `role` named `label`.
pub fn node(app: &mut Editor, role: Role, label: &str) -> Option<(NodeId, Node)> {
    nodes(app, |n| n.role() == role && n.label() == Some(label))
        .into_iter()
        .next()
}

/// Perform `action` on the node with `role` named `label`.
pub fn act(app: &mut Editor, role: Role, label: &str, action: &ui::a11y::Action) {
    let (id, _) = node(app, role, label).unwrap_or_else(|| panic!("no {role:?} named {label:?}"));
    app.harness().perform_a11y_action(id, action).unwrap();
}

/// Press the control with `role` named `label`, as a reader does.
pub fn press(app: &mut Editor, role: Role, label: &str) {
    act(app, role, label, &ui::a11y::Action::Activate);
}

/// The clip placing the source `name` on the timeline, selected alone with
/// the playhead parked at its head — what activating it does.
pub fn select_clip(app: &mut Editor, name: &str) {
    let (id, _) = nodes(app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some(name)
            && n.value().is_some_and(|v| v.contains(" on "))
    })
    .into_iter()
    .next()
    .unwrap_or_else(|| panic!("{name} is not placed on the timeline"));
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Activate)
        .unwrap();
}

/// The id of the first clip placing the source `name`.
pub fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

/// Press `key` with `modifier` held.
pub fn chord(app: &mut Editor, modifier: &str, key: &str) {
    app.harness().set_modifiers(&[modifier.into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

/// Move the playhead to `timecode` through the go-to field.
pub fn go_to(app: &mut Editor, timecode: &str) {
    app.harness().key("g");
    app.harness().type_text(timecode);
    app.harness().key("Enter");
}

/// The playhead as the transport reads it out.
pub fn playhead(app: &mut Editor) -> String {
    node(app, Role::Group, "Transport")
        .and_then(|(_, n)| n.value().map(str::to_string))
        .expect("the transport reads out the playhead")
}
