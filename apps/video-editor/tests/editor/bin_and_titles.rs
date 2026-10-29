//! Sources reach the timeline from the bin, and titles are written in place:
//! T makes a title at the playhead with the caret in its words, Enter breaks
//! a line, Ctrl+Enter accepts and Escape leaves the words as they were.

use crate::hosted;

use hosted::*;
use ui::accesskit::Role;
use video_editor::Project;
use video_editor::model::{MediaItem, MediaKind};

/// Activate the bin row of the source `name`, as double-clicking it does.
fn place_from_bin(app: &mut Editor, name: &str) {
    let (id, _) = nodes(app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some(name)
            && n.value().is_some_and(|v| !v.contains(" on "))
    })
    .into_iter()
    .next()
    .unwrap_or_else(|| panic!("{name} is not in the bin"));
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Activate)
        .unwrap();
}

/// The words, name and placed clips of the title source `name` holds.
fn title(app: &Editor) -> MediaItem {
    app.state()
        .media
        .iter()
        .find(|m| m.kind == MediaKind::Text)
        .cloned()
        .expect("a title source")
}

#[test]
fn activating_a_bin_row_places_one_clip_of_that_source() {
    let mut app = sample_editor();
    let before = app.state();
    let logo = before
        .media
        .iter()
        .find(|m| m.name == "Logo_Bug.png")
        .unwrap()
        .id;
    let placed = before.clips.iter().filter(|c| c.media == logo).count();
    place_from_bin(&mut app, "Logo_Bug.png");
    let after = app.state();
    assert_eq!(after.clips.len(), before.clips.len() + 1);
    assert_eq!(
        after.clips.iter().filter(|c| c.media == logo).count(),
        placed + 1
    );
}

#[test]
fn placing_into_a_project_with_no_tracks_makes_a_matching_track() {
    let mut project = Project::new("Blank");
    project.media.push(MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "Still.png".into(),
        kind: MediaKind::Image,
        duration: 5.0,
        width: 320,
        height: 240,
        ..MediaItem::default()
    });
    let mut app = editor_with(&project);
    place_from_bin(&mut app, "Still.png");
    let project = app.state();
    assert_eq!(project.tracks.len(), 1, "a matching track was made");
    assert_eq!(project.clips.len(), 1, "and the clip placed on it");
}

#[test]
fn t_places_a_title_at_the_playhead_with_its_words_open() {
    let mut app = sample_editor();
    let (media, clips) = (app.state().media.len(), app.state().clips.len());
    go_to(&mut app, "0:12");
    app.harness().key("t");
    app.harness().type_text("SUNSET");
    chord(&mut app, "ctrl", "Enter");

    let project = app.state();
    assert_eq!(project.media.len(), media + 1, "a source was made");
    assert_eq!(project.clips.len(), clips + 1, "and placed");
    let item = title(&app);
    assert_eq!(
        item.text.as_ref().unwrap().content,
        "SUNSET",
        "typed straight in"
    );
    assert_eq!(item.name, "SUNSET", "the bin row follows the words");
    // A title has no bytes, so no peer ever finds it missing.
    assert!(item.path.is_empty());
    let clip = project.clips.iter().find(|c| c.media == item.id).unwrap();
    assert!(
        (clip.start - 12.0).abs() < 0.1,
        "at the playhead: {}",
        clip.start
    );
    let (_, node) = nodes(&mut app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some("SUNSET")
            && n.value().is_some_and(|v| v.contains(" on "))
    })
    .into_iter()
    .next()
    .expect("the title's clip is on the timeline");
    assert!(node.is_selected().unwrap_or_default(), "and selected");
}

#[test]
fn enter_breaks_a_title_line_ctrl_enter_accepts_and_escape_discards() {
    let mut app = editor_with(&Project::new("Blank"));
    app.harness().key("t");
    app.harness().type_text("COASTLINE");
    app.harness().key("Enter");
    app.harness().type_text("episode two");
    chord(&mut app, "ctrl", "Enter");
    let item = title(&app);
    assert_eq!(item.text.unwrap().content, "COASTLINE\nepisode two");
    assert_eq!(
        item.name, "COASTLINE",
        "a one-line list shows the first line"
    );

    // Reopen the words by adding another title, then walk away from them.
    let titles = app.state().media.len();
    app.harness().key("t");
    app.harness().type_text("DISCARDED");
    app.harness().key("Escape");
    let project = app.state();
    assert_eq!(project.media.len(), titles + 1);
    assert!(
        project
            .media
            .iter()
            .filter_map(|m| m.text.as_ref())
            .all(|t| !t.content.contains("DISCARDED")),
        "Escape leaves the words as they were"
    );
}

#[test]
fn the_size_slider_writes_the_title_source() {
    let mut app = editor_with(&Project::new("Blank"));
    app.harness().key("t");
    chord(&mut app, "ctrl", "Enter");
    let before = title(&app).text.unwrap().size;
    act(&mut app, Role::Slider, "Size", &ui::a11y::Action::Increment);
    let after = title(&app).text.unwrap().size;
    assert!(after > before, "{before} -> {after}");
    assert!(after <= 0.4, "and stays in the offered range");
}
