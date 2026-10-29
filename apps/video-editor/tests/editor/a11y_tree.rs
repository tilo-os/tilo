//! What a screen reader finds in the editor and what it can operate: the
//! exported tree and the routed actions, the same bridge AT-SPI2 reaches.

use ui::a11y::Action;
use ui::accesskit::{Node, NodeId, Role};
use ui::prelude::*;
use video_editor::model::MaskShape;
use video_editor::param::{Interp, Param};
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

fn editor_with(project: &Project) -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app
}

fn editor() -> HeadlessApp<VideoEditor> {
    editor_with(&sample_project())
}

fn node(app: &mut HeadlessApp<VideoEditor>, name: &str) -> Option<(NodeId, Node)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some(name))
}

fn value_of(app: &mut HeadlessApp<VideoEditor>, name: &str) -> String {
    node(app, name)
        .and_then(|(_, n)| n.value().map(ToString::to_string))
        .unwrap_or_else(|| panic!("{name} reports a value"))
}

/// Select the placed clip of the source `name`, as a reader activating its
/// node on the timeline does (the bin row of the same source is not it).
fn activate_clip(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let (id, _, _) = clips(app)
        .into_iter()
        .find(|(_, n, _)| n == name)
        .unwrap_or_else(|| panic!("{name} is placed"));
    app.harness()
        .perform_a11y_action(id, &Action::Activate)
        .unwrap();
}

fn activate(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let (id, _) = node(app, name).unwrap_or_else(|| panic!("{name} is in the tree"));
    app.harness()
        .perform_a11y_action(id, &Action::Activate)
        .unwrap();
}

/// Every placed clip node: `(id, name, "start to end, length long, on track")`.
fn clips(app: &mut HeadlessApp<VideoEditor>) -> Vec<(NodeId, String, String)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(id, n)| {
            let value = n.value()?.to_string();
            (n.role() == Role::ListItem && value.contains(" long, on "))
                .then(|| (id, n.label().unwrap_or_default().to_string(), value))
        })
        .collect()
}

/// The id of the clip placing the source `name`.
fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

fn focused_label(app: &mut HeadlessApp<VideoEditor>) -> Option<String> {
    app.harness()
        .a11y_focus()
        .and_then(|(_, n)| n.label().map(ToString::to_string))
}

#[test]
fn panes_transport_clips_and_track_toggles_reach_a_reader() {
    let mut app = editor();
    let nodes = app.harness().a11y_nodes();
    let labelled = |name: &str| nodes.iter().find(|(_, n)| n.label() == Some(name));
    for pane in ["Media bin", "Transport", "Timeline", "Inspector"] {
        assert!(labelled(pane).is_some(), "the {pane} pane is in the tree");
    }
    for button in [
        "Jump to start",
        "Step back one frame",
        "Play",
        "Step forward one frame",
        "Jump to end",
    ] {
        assert_eq!(labelled(button).map(|(_, n)| n.role()), Some(Role::Button));
    }
    for track in ["V1", "V2", "A1", "A2"] {
        for verb in ["Mute", "Solo", "Lock"] {
            let name = format!("{verb} {track}");
            let toggle = labelled(&name).map(|(_, n)| n);
            assert!(
                toggle.is_some_and(|n| n.toggled().is_some()),
                "{name} is a real toggle"
            );
        }
    }
    let clip = nodes
        .iter()
        .find(|(_, n)| {
            n.label() == Some("Interview_A.mp4") && n.value().is_some_and(|v| v.contains(" on V"))
        })
        .map(|(_, n)| n)
        .expect("a placed clip is in the tree");
    assert!(clip.value().unwrap().contains(" to "), "it states its span");
    assert!(
        clip.position_in_set().is_some(),
        "and its place in its track"
    );
}

#[test]
fn a_reader_plays_the_video_and_the_button_renames_itself() {
    let mut app = editor();
    activate(&mut app, "Play");
    assert!(node(&mut app, "Pause").is_some());
    assert!(node(&mut app, "Play").is_none());
}

#[test]
fn activating_a_clip_focuses_it_and_parks_the_playhead_on_it() {
    let mut app = editor();
    activate_clip(&mut app, "Cutaway_Market.mp4");
    assert_eq!(
        focused_label(&mut app).as_deref(),
        Some("Cutaway_Market.mp4")
    );
    assert_eq!(
        value_of(&mut app, "Transport"),
        video_editor::timecode::timecode(20.0, 30.0)
    );
}

#[test]
fn an_id_held_across_a_delete_still_operates_the_clip_it_named() {
    let mut app = editor();
    let before = clips(&mut app);
    let on_v1: Vec<_> = before
        .iter()
        .filter(|(_, _, v)| v.ends_with(" on V1"))
        .collect();
    assert!(on_v1.len() >= 2, "{before:?}");
    let (earlier, _, _) = on_v1[0].clone();
    let (held, held_name, held_value) = on_v1.last().copied().cloned().unwrap();

    app.harness()
        .perform_a11y_action(earlier, &Action::Activate)
        .unwrap();
    app.harness().key("Delete");
    let after = clips(&mut app);
    assert_eq!(after.len(), before.len() - 1, "the delete happened");
    assert!(after.iter().all(|(id, _, _)| *id != earlier));

    // Deleting the earlier clip renumbers the later ones; the held id must
    // still name the clip it named, not the one that slid into its place.
    app.harness()
        .perform_a11y_action(held, &Action::Activate)
        .unwrap();
    let (_, focused) = app.harness().a11y_focus().unwrap();
    assert_eq!(focused.label(), Some(held_name.as_str()));
    assert_eq!(focused.value(), Some(held_value.as_str()));
}

#[test]
fn a_stale_clip_id_operates_nothing() {
    let mut app = editor();
    let (real, _, _) = clips(&mut app).remove(0);
    let stale = NodeId(real.0 ^ 0x9e37_79b9);
    assert!(
        app.harness()
            .perform_a11y_action(stale, &Action::Activate)
            .is_err()
    );
}

#[test]
fn folding_a_section_hides_its_controls_but_states_what_is_inside() {
    let mut app = editor();
    activate_clip(&mut app, "Drone_Coastline.mov");
    assert!(node(&mut app, "Rotation").is_some());
    activate(&mut app, "Transform");
    assert!(
        node(&mut app, "Rotation").is_none(),
        "folding hides the controls"
    );
    let (_, header) = node(&mut app, "Transform").unwrap();
    assert_eq!(header.is_expanded(), Some(false));
    let summary = header.value().unwrap_or_default();
    assert!(
        summary.contains("scale") && summary.contains("rotation"),
        "{summary:?}"
    );
    // The fold belongs to the panel, not the clip.
    activate_clip(&mut app, "Interview_A.mp4");
    assert_eq!(
        node(&mut app, "Transform").unwrap().1.is_expanded(),
        Some(false)
    );
}

#[test]
fn folded_headers_report_the_value_a_keyframe_is_playing() {
    // Every constant at rest and one key on each: only the curve can be the
    // source of the reported numbers.
    let mut project = sample_project();
    let sunset = project
        .media
        .iter()
        .find(|m| m.name == "Sunset_Timelapse.mp4")
        .unwrap()
        .id;
    let clip = project
        .clips
        .iter_mut()
        .find(|c| c.media == sunset)
        .unwrap();
    let at = clip.start + 1.0;
    clip.key.enabled = true;
    clip.key.tolerance = 0.0;
    clip.mask.shape = MaskShape::Ellipse;
    clip.mask.invert = true;
    for (param, value) in [
        (Param::ScaleX, 2.5),
        (Param::ScaleY, 2.5),
        (Param::KeyTolerance, 0.4),
        (Param::KeySoftness, 0.2),
        (Param::MaskW, 1.2),
        (Param::MaskH, 0.3),
    ] {
        clip.set_key_at(param, at, value, Interp::Linear);
    }
    let mut app = editor_with(&project);
    activate_clip(&mut app, "Sunset_Timelapse.mp4");
    app.harness().key("g");
    app.harness().type_text("0:22");
    app.harness().key("Enter");
    for section in ["Transform", "Chroma key", "Mask"] {
        activate(&mut app, section);
    }
    let transform = value_of(&mut app, "Transform");
    assert!(transform.contains("2.50"), "{transform:?}");
    let key = value_of(&mut app, "Chroma key");
    assert!(key.contains("40%") && key.contains("20%"), "{key:?}");
    let mask = value_of(&mut app, "Mask");
    assert!(
        mask.contains("ellipse") && mask.contains("120%") && mask.contains("30%"),
        "{mask:?}"
    );
    assert!(
        mask.contains("outside"),
        "an inverted mask says so: {mask:?}"
    );
}

#[test]
fn folded_speed_and_a_key_that_keys_nothing_say_so() {
    let mut project = sample_project();
    project.clips[0].key.enabled = true;
    project.clips[0].key.tolerance = 0.0;
    project.clips[0].set_speed(2.0, true);
    let mut app = editor_with(&project);
    activate_clip(&mut app, "Drone_Coastline.mov");
    activate(&mut app, "Chroma key");
    activate(&mut app, "Speed");
    let key = value_of(&mut app, "Chroma key");
    assert!(key.contains("keys nothing"), "on is not off: {key:?}");
    let speed = value_of(&mut app, "Speed");
    assert!(speed.contains('2') && speed.contains("rev"), "{speed:?}");
}

#[test]
fn the_folded_markers_header_counts_project_and_clip_markers() {
    let mut project = sample_project();
    project.add_marker(12.0, "act two", 0);
    project.clips[0].add_marker(1.0, "on the clip", 2);
    let total = project.all_markers().len();
    let mut app = editor_with(&project);
    activate_clip(&mut app, "Drone_Coastline.mov");
    activate(&mut app, "Markers");
    assert_eq!(value_of(&mut app, "Markers"), format!("{total} markers"));
}

#[test]
fn a_reader_adjusts_a_slider_and_the_document_follows() {
    let mut app = editor();
    activate_clip(&mut app, "Interview_A.mp4");
    let rotation = value_of(&mut app, "Rotation");
    let (id, _) = node(&mut app, "Rotation").unwrap();
    app.harness()
        .perform_a11y_action(id, &Action::Increment)
        .unwrap();
    assert_ne!(value_of(&mut app, "Rotation"), rotation);

    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let before = app.state().clip(interview).unwrap().opacity;
    let (id, _) = node(&mut app, "Opacity").unwrap();
    app.harness()
        .perform_a11y_action(id, &Action::Decrement)
        .unwrap();
    let after = app.state().clip(interview).unwrap().opacity;
    assert!(after < before - 1e-6, "{before} -> {after}");
}

#[test]
fn an_audio_clip_offers_its_volume_where_a_video_clip_offers_opacity() {
    let mut app = editor();
    activate_clip(&mut app, "VO_Take3.wav");
    assert!(node(&mut app, "Volume").is_some());
    assert!(
        node(&mut app, "Opacity").is_none(),
        "no slider is drawn for it"
    );
}

#[test]
fn switching_the_key_on_and_choosing_a_mask_reveal_their_sliders() {
    let mut app = editor();
    activate_clip(&mut app, "Interview_A.mp4");
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    assert!(node(&mut app, "Tolerance").is_none(), "the key starts off");
    assert!(node(&mut app, "Width").is_none(), "no mask shape yet");

    activate(&mut app, "Switch chroma key on");
    assert!(app.state().clip(interview).unwrap().key.enabled);
    assert!(node(&mut app, "Tolerance").is_some());
    assert!(node(&mut app, "Switch chroma key off").is_some());

    activate(&mut app, "Mask shape ellipse");
    assert_eq!(
        app.state().clip(interview).unwrap().mask.shape,
        MaskShape::Ellipse
    );
    assert!(node(&mut app, "Width").is_some());
}

#[test]
fn every_transition_can_be_chosen_by_a_reader() {
    let mut app = editor();
    activate_clip(&mut app, "Interview_A.mp4");
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let mut chosen = Vec::new();
    for name in [
        "Transition cut",
        "Transition dissolve",
        "Transition fade",
        "Transition dip",
        "Transition wipe",
    ] {
        activate(&mut app, name);
        let (_, node) = node(&mut app, name).unwrap();
        assert_eq!(
            node.toggled(),
            Some(ui::accesskit::Toggled::True),
            "{name} is in force"
        );
        chosen.push(app.state().clip(interview).unwrap().transition);
    }
    chosen.dedup();
    assert_eq!(chosen.len(), video_editor::model::Transition::ALL.len());
}
