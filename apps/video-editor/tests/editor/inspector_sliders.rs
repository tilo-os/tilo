//! The inspector, driven as a screen reader drives it: select a clip on the
//! timeline, step its sliders and press its choices, and read the project
//! the host holds afterwards. The project's frame format follows the first
//! clip placed on a blank project unless one was chosen.

use ui::accesskit::{Node, NodeId, Role};
use ui::prelude::*;
use video_editor::model::{MediaItem, MediaKind};
use video_editor::param::{Interp, Param};
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

fn hosting(project: &Project) -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

fn find(app: &mut HeadlessApp<VideoEditor>, pick: impl Fn(&Node) -> bool) -> Option<NodeId> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| pick(node))
        .map(|(id, _)| id)
}

fn act(app: &mut HeadlessApp<VideoEditor>, node: NodeId, action: &ui::a11y::Action) {
    app.harness().perform_a11y_action(node, action).unwrap();
    app.settle();
}

/// Activate the node named `name`.
fn press(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let node =
        find(app, |n| n.label() == Some(name)).unwrap_or_else(|| panic!("no {name} in the tree"));
    act(app, node, &ui::a11y::Action::Activate);
}

/// Select the clip placing `name` on the timeline (not its bin row).
fn select(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let clip = find(app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some(name)
            && n.value().is_some_and(|v| v.contains(" on "))
    })
    .expect("the source is placed");
    act(app, clip, &ui::a11y::Action::Activate);
}

/// Step the slider named `name`, inside the section `section` when given.
fn step(app: &mut HeadlessApp<VideoEditor>, section: Option<&str>, name: &str, up: bool) {
    let nodes = app.harness().a11y_nodes();
    let within: Option<Vec<NodeId>> = section.map(|s| {
        nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Group && n.label() == Some(s))
            .map_or_else(|| panic!("no {s} section"), |(_, n)| n.children().to_vec())
    });
    let slider = nodes
        .iter()
        .find(|(id, n)| {
            n.role() == Role::Slider
                && n.label() == Some(name)
                && within.as_ref().is_none_or(|kids| kids.contains(id))
        })
        .map_or_else(|| panic!("no {name} slider"), |(id, _)| *id);
    let action = if up {
        ui::a11y::Action::Increment
    } else {
        ui::a11y::Action::Decrement
    };
    act(app, slider, &action);
}

fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

fn source(name: &str, width: u32, height: u32, fps: f64) -> MediaItem {
    MediaItem {
        id: uuid::Uuid::new_v4(),
        name: name.into(),
        kind: MediaKind::Video,
        duration: 12.0,
        width,
        height,
        fps,
        path: format!("media/{name}"),
        ..Default::default()
    }
}

/// A blank project with two sources in the bin and nothing placed.
fn blank_bin() -> Project {
    let mut project = Project::new("Blank");
    project.media.push(source("Phone.mp4", 1080, 1920, 24.0));
    project.media.push(source("Camera.mp4", 3840, 2160, 60.0));
    project
}

#[test]
fn the_first_clip_on_a_blank_project_gives_it_its_shape_and_later_ones_do_not() {
    let mut app = hosting(&blank_bin());
    assert_eq!((app.state().width, app.state().height), (1920, 1080));
    // Activating a bin row places the source, as Enter on it does.
    press(&mut app, "Phone.mp4");
    let project = app.state();
    assert_eq!((project.width, project.height), (1080, 1920));
    assert!((project.fps - 24.0).abs() < 1e-9, "and its rate");
    assert!(
        project.format_explicit,
        "the shape is now the project's own"
    );
    let (w, h) = video_editor::export::output_dims(
        &video_editor::export::ExportSettings::default(),
        &project,
    );
    assert!(h > w, "a vertical project exports vertical, got {w}x{h}");

    press(&mut app, "Camera.mp4");
    assert_eq!(
        (app.state().width, app.state().height),
        (1080, 1920),
        "the shape the timeline was cut against survives"
    );
}

#[test]
fn a_format_chosen_in_the_inspector_is_kept_when_a_clip_lands() {
    let mut app = hosting(&blank_bin());
    // The section starts folded: a reader unfolds it through its header.
    press(&mut app, "Project");
    press(&mut app, "9:16, 1080 by 1920");
    press(&mut app, "25 fps");
    press(&mut app, "Camera.mp4");
    let project = app.state();
    assert_eq!((project.width, project.height), (1080, 1920));
    assert!((project.fps - 25.0).abs() < 1e-9);
}

#[test]
fn an_inspector_slider_step_is_one_undoable_edit_of_the_document() {
    let mut app = hosting(&sample_project());
    let id = clip_of(&app.state(), "Cutaway_Market.mp4");
    select(&mut app, "Cutaway_Market.mp4");
    let before = app.state().clip(id).unwrap().filters.brightness;
    let versions = app.versions().len();
    step(&mut app, None, "Brightness", true);
    let after = app.state().clip(id).unwrap().filters.brightness;
    assert!(after > before, "{before} -> {after}");
    assert_eq!(app.versions().len(), versions + 1, "one action");
    assert!(app.undo());
    assert!((app.state().clip(id).unwrap().filters.brightness - before).abs() < 1e-12);
}

#[test]
fn brightening_a_clip_brightens_the_preview_composite() {
    // The time-lapse on V2 is the picture on top at 23 s.
    let mut app = hosting(&sample_project());
    select(&mut app, "Sunset_Timelapse.mp4");
    let luma = |project: &Project| match video_editor::preview::composite(project, 23.0) {
        video_editor::preview::CompositedFrame::Clip { base, .. } => base.luma(),
        video_editor::preview::CompositedFrame::Empty => panic!("a clip is under 23s"),
    };
    let before = luma(&app.state());
    for _ in 0..4 {
        step(&mut app, None, "Brightness", true);
    }
    let after = luma(&app.state());
    assert!(after > before, "{before} -> {after}");
}

#[test]
fn geometry_and_crop_sliders_write_the_transform() {
    let mut app = hosting(&sample_project());
    let id = clip_of(&app.state(), "Cutaway_Market.mp4");
    select(&mut app, "Cutaway_Market.mp4");
    assert!(app.state().clip(id).unwrap().transform.is_identity());
    step(&mut app, None, "Scale X", false);
    step(&mut app, None, "Top", true);
    let t = app.state().clip(id).unwrap().transform;
    assert!(t.scale_x < 1.0, "scale_x is {}", t.scale_x);
    assert!(t.crop_top > 0.0, "crop_top is {}", t.crop_top);
    assert!(t.crop_bottom.abs() < 1e-12, "only the stepped edge moved");
    assert!(t.is_cropped());
}

#[test]
fn the_rate_slider_retimes_the_clip_without_touching_its_source_window() {
    let mut app = hosting(&sample_project());
    let id = clip_of(&app.state(), "Cutaway_Market.mp4");
    select(&mut app, "Cutaway_Market.mp4");
    let before = app.state().clip(id).unwrap().clone();
    step(&mut app, Some("Speed"), "Rate", true);
    let after = app.state().clip(id).unwrap().clone();
    assert!(after.speed.rate() > 1.0, "rate {}", after.speed.rate());
    assert!(
        after.duration() < before.duration(),
        "a faster clip is shorter"
    );
    assert!((after.source_span() - before.source_span()).abs() < 1e-9);
}

#[test]
fn stepping_an_animated_slider_writes_a_key_at_the_playhead() {
    let mut project = sample_project();
    let id = clip_of(&project, "Cutaway_Market.mp4");
    project
        .clip_mut(id)
        .unwrap()
        .set_key_at(Param::Opacity, 23.0, 1.0, Interp::Linear);
    let mut app = hosting(&project);
    select(&mut app, "Cutaway_Market.mp4");
    // Selecting the clip puts the playhead on its head (20 s), off the key.
    step(&mut app, None, "Opacity", false);
    let clip = app.state().clip(id).unwrap().clone();
    let keys = clip.anim.track(Param::Opacity).unwrap().keys.len();
    assert_eq!(keys, 2, "the step added a key rather than a dead constant");
    assert!(clip.param_at(Param::Opacity, 20.0) < clip.param_at(Param::Opacity, 23.0));
}

#[test]
fn an_empty_inspector_says_what_to_do() {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    assert!(
        find(&mut app, |n| n
            .label()
            .is_some_and(|l| l.starts_with("No clip selected"))
            && n.label().is_some_and(|l| l.contains("Click a clip")))
        .is_some()
    );
}

#[test]
fn an_audio_clip_offers_no_key_or_mask() {
    let mut app = hosting(&sample_project());
    select(&mut app, "VO_Take3.wav");
    for section in ["Chroma key", "Mask"] {
        assert!(
            find(&mut app, |n| n.role() == Role::Group
                && n.label() == Some(section))
            .is_none(),
            "an audio clip has no picture to {section}"
        );
    }
    select(&mut app, "Cutaway_Market.mp4");
    for section in ["Chroma key", "Mask"] {
        assert!(
            find(&mut app, |n| n.role() == Role::Group
                && n.label() == Some(section))
            .is_some()
        );
    }
}

#[test]
fn the_masks_rotation_turns_the_mask_not_the_picture() {
    // Both rows read "Rotation"; an action at one must not land on the other.
    let mut app = hosting(&sample_project());
    let id = clip_of(&app.state(), "Cutaway_Market.mp4");
    select(&mut app, "Cutaway_Market.mp4");
    press(&mut app, "Mask shape rect");
    step(&mut app, Some("Mask"), "Rotation", true);
    let clip = app.state().clip(id).unwrap().clone();
    assert!(clip.mask.rotation.abs() > 0.0, "the mask turned");
    assert!(
        clip.transform.rotation.abs() < 1e-12,
        "and the picture did not"
    );
}
