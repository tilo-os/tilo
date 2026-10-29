//! The render range: Shift+I / Shift+O mark it, Shift+X clears it, a render
//! covers only what is marked, and plain I / O still trim the clip.

use ui::prelude::*;
use video_editor::export::RenderPlan;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

fn editor() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

fn go_to(app: &mut HeadlessApp<VideoEditor>, timecode: &str) {
    app.harness().key("g");
    app.harness().type_text(timecode);
    app.harness().key("Enter");
}

fn shifted(app: &mut HeadlessApp<VideoEditor>, key: &str) {
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

#[test]
fn marking_in_and_out_narrows_the_render_and_shift_x_widens_it_again() {
    let mut app = editor();
    go_to(&mut app, "0:04");
    shifted(&mut app, "I");
    go_to(&mut app, "0:14");
    shifted(&mut app, "O");

    // Read from the document, which a peer and a reopened project see.
    let project = app.state();
    let (from, to) = project.export_range();
    assert!(
        (from - 4.0).abs() < 1e-9 && (to - 14.0).abs() < 1e-9,
        "{from}..{to}"
    );
    let plan = RenderPlan::new(&project, 30.0);
    assert_eq!(plan.total, 300, "ten seconds at 30 fps");
    assert!((plan.time_of(0) - 4.0).abs() < 1e-9);
    assert!((plan.time_of(plan.total - 1) - (14.0 - 1.0 / 30.0)).abs() < 1e-6);

    shifted(&mut app, "X");
    let project = app.state();
    assert!(!project.has_marks());
    let (from, to) = project.export_range();
    assert!(from.abs() < 1e-9 && (to - project.duration()).abs() < 1e-9);
}

#[test]
fn plain_i_still_trims_the_selected_clip_rather_than_marking() {
    let mut app = editor();
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let before = app.state().clip(interview).unwrap().in_point;
    let clip = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| {
            n.label() == Some("Interview_A.mp4") && n.value().is_some_and(|v| v.contains(" on "))
        })
        .unwrap()
        .0;
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
    go_to(&mut app, "0:09");
    app.harness().key("i");
    let project = app.state();
    assert!(project.clip(interview).unwrap().in_point > before + 1e-6);
    assert!(!project.has_marks(), "it did not mark the timeline instead");
}
