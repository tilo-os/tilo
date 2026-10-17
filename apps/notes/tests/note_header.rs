//! The note header (tags, where the note sits, when it was written, and the
//! notes that link to it) is made of toolkit widgets under the title: it keeps
//! one left edge with the title, clears the first block at any text scale and
//! width, and its buttons activate by pointer.

use notes::MainPanel;
use notes::main_panel::{BacklinkChip, TagChip};
use ui::headless::{FrameOptions, Framed};
use ui::theme::Theme;
use uuid::Uuid;
use vello::kurbo::Rect;

struct Mounted {
    panel: Framed<MainPanel>,
    tags: Vec<Uuid>,
    sources: Vec<Uuid>,
}

fn mounted(width: u32, scale: f32, tags: &[&str], backlinks: &[(&str, u32)]) -> Mounted {
    let mut panel = MainPanel::new();
    let mut host = ui::framework::testing::TestApp::<notes::NotesShell>::new().unwrap();
    let actions = host
        .surface_mut::<notes::NotesApp>()
        .main_panel_mut()
        .actions()
        .expect("the window binds its header");
    panel.attach_actions_for_test(actions);
    panel.load_page(
        Uuid::new_v4(),
        "Launch checklist",
        "What has to land before we ship.\n",
    );
    panel.set_parent_path("Projects".into());
    let tag_ids: Vec<Uuid> = tags.iter().map(|_| Uuid::new_v4()).collect();
    panel.set_tags(
        tags.iter()
            .zip(&tag_ids)
            .map(|(name, id)| TagChip {
                id: *id,
                name: (*name).into(),
            })
            .collect(),
    );
    let source_ids: Vec<Uuid> = backlinks.iter().map(|_| Uuid::new_v4()).collect();
    panel.set_backlinks(
        backlinks
            .iter()
            .zip(&source_ids)
            .map(|((title, references), page)| BacklinkChip {
                page: *page,
                title: (*title).into(),
                references: *references,
            })
            .collect(),
    );
    let framed = Framed::with_options(
        ui::State::new(),
        panel,
        (width, 800),
        FrameOptions {
            theme: Some(Theme::dark().with_text_scale(scale)),
            text: Some(crate::component_support::renderer()),
            ..FrameOptions::default()
        },
    );
    Mounted {
        panel: framed,
        tags: tag_ids,
        sources: source_ids,
    }
}

fn rect_of(panel: &MainPanel, id: &str) -> Rect {
    panel
        .header_rects_for_test()
        .into_iter()
        .find_map(|(found, rect)| (found == id).then_some(rect))
        .unwrap_or_else(|| panic!("no header widget {id}"))
}

fn settle(m: &mut Mounted, width: u32) {
    for _ in 0..2 {
        Framed::harness_mut(&mut m.panel)
            .relayout_at((width, 800))
            .unwrap();
    }
}

#[test]
fn the_header_is_one_group_on_the_content_gutter_and_clears_the_body() {
    for (width, scale) in [(1000, 1.0), (1000, 2.0), (420, 2.0)] {
        let mut m = mounted(
            width,
            scale,
            &["Release planning", "Customer research", "Accessibility"],
            &[("Roadmap", 1), ("Long-running launch notes", 2)],
        );
        settle(&mut m, width);
        let first_tag_id = format!("notes.tag-chip:{}", m.tags[0]);
        let (panel, _) = Framed::split(&mut m.panel);
        let theme = Theme::dark().with_text_scale(scale);
        let gutter = f64::from(theme.tokens.spacing.rhythm.group);
        let handles = panel.editor().handle_gutter(&theme);
        assert!(handles > 0.0, "a note's blocks have a handle gutter");
        let title = panel.title_rect_for_test();
        let edge = title.x0;
        let area = ui::widget::Widget::get_rect(panel);
        // The text column is centred: whatever it stands in from the handle
        // gutter on the left, it stands in from the gutter on the right.
        let side = edge - area.x0 - (gutter + handles);
        assert!(
            side >= -1e-6 && (area.x1 - gutter - title.x1 - side).abs() < 1.0,
            "title on the text edge, outside the handle gutter, in a centred column, \
             at {width}x{scale}: {title:?} in {area:?}"
        );
        if width == 1000 && scale < 1.5 {
            assert!(
                side > 0.0,
                "a wide panel reads in a column narrower than the panel"
            );
        }
        let widgets = panel.header_rects_for_test();
        assert!(
            widgets.len() >= 6,
            "tags, captions and links all exist: {widgets:?}"
        );
        let mut lowest: f64 = 0.0;
        for (id, rect) in &widgets {
            if id.starts_with("notes.tag-chip:") || id.starts_with("notes.backlink-chip:") {
                assert!(rect.x1 <= title.x1 + 1e-6, "{id} stays inside the gutter");
                assert!(
                    rect.y0 >= title.y1 - 1e-6,
                    "{id} sits under the title {title:?}: {rect:?}"
                );
            }
            if id.starts_with("notes.caption:") || id == "notes.linked-from" {
                assert!((rect.x0 - edge).abs() < 1e-6, "{id} on the gutter");
            }
            lowest = lowest.max(rect.y1);
        }
        let first_tag = rect_of(panel, &first_tag_id);
        assert!(
            (first_tag.x0 - edge).abs() < 1e-6,
            "the first tag on the gutter"
        );
        assert!(
            panel.body_top_for_test() >= lowest,
            "the body starts under the header at {width}x{scale}: {} vs {lowest}",
            panel.body_top_for_test()
        );
    }
}

#[test]
fn long_tags_wrap_inside_the_gutter() {
    let mut m = mounted(
        420,
        2.0,
        &["Release planning", "Customer research", "Accessibility"],
        &[],
    );
    settle(&mut m, 420);
    let ids: Vec<String> = m
        .tags
        .iter()
        .map(|id| format!("notes.tag-chip:{id}"))
        .collect();
    let (panel, _) = Framed::split(&mut m.panel);
    let rects: Vec<Rect> = ids.iter().map(|id| rect_of(panel, id)).collect();
    assert!(
        rects[1].y0 > rects[0].y0 || rects[2].y0 > rects[0].y0,
        "tags wrap onto another line rather than leave the panel: {rects:?}"
    );
}

#[test]
fn a_note_without_metadata_keeps_its_body_gap_under_the_title() {
    let mut panel = MainPanel::new();
    panel.load_page(Uuid::new_v4(), "Quiet note", "Just text.\n");
    let mut framed = Framed::with_options(
        ui::State::new(),
        panel,
        (900, 800),
        FrameOptions {
            text: Some(crate::component_support::renderer()),
            ..FrameOptions::default()
        },
    );
    Framed::harness_mut(&mut framed)
        .relayout_at((900, 800))
        .unwrap();
    let (panel, _) = Framed::split(&mut framed);
    assert!(panel.header_rects_for_test().is_empty());
    assert!(
        panel.body_top_for_test() > panel.title_rect_for_test().y1,
        "the body starts under the title"
    );
}

/// Press the widget `id` and return the resource and action the press
/// invoked. The events go to the panel directly so the invocation is left
/// for this test to read instead of being dispatched by the harness.
fn press(m: &mut Mounted, id: &str) -> Option<(String, String)> {
    use winit::event::{ElementState, MouseButton};
    let at = rect_of(Framed::split(&mut m.panel).0, id).center();
    let harness = Framed::harness_mut(&mut m.panel);
    harness.dispatch_to_root(&ui::synthetic_input::cursor_moved(at.x, at.y));
    for state in [ElementState::Pressed, ElementState::Released] {
        harness.dispatch_to_root(&ui::synthetic_input::mouse_input(state, MouseButton::Left));
    }
    let panel: &mut dyn ui::widget::Widget = Framed::split(&mut m.panel).0;
    ui::widget::controls::take_control_invocation(panel)
        .map(|invocation| (invocation.address.resource, invocation.address.action))
}

#[test]
fn the_buttons_open_their_tag_and_note() {
    let mut m = mounted(1000, 1.0, &["Release"], &[("Roadmap", 1)]);
    settle(&mut m, 1000);
    let tag_id = format!("notes.tag-chip:{}", m.tags[0]);
    let link_id = format!("notes.backlink-chip:{}", m.sources[0]);
    assert_eq!(
        press(&mut m, &tag_id).map(|(_, action)| action),
        Some("open-tag".into()),
        "a press opens the tag it names"
    );
    assert_eq!(
        press(&mut m, &link_id).map(|(_, action)| action),
        Some("open-note".into()),
        "a press opens the note that links here"
    );
}
