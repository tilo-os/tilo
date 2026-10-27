#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel counts over a bounded fixture canvas"
)]

//! Other people's pointers a host placed over its surface are drawn by the
//! frame itself: the shell's arrow in the person's colour with their name
//! tag, laid out so the whole name stays on screen at the surface's edge.

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::permissions::UserId;
use tilo_ui::presence::{PlacedPointer, PresenceCtx};
use tilo_ui::theme::Theme;
use tilo_ui::widget::Spacer;

const SIZE: (u32, u32) = (420, 200);
const PINK: (u8, u8, u8) = (0xee, 0x83, 0xaf);

/// A frame showing Ada's pointer at `at`, and how many of its pixels wear
/// her colour.
fn frame_with_pointer_at(at: (f64, f64)) -> (FrameHarness, usize) {
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(Spacer::new()), SIZE, Theme::dark());
    let mut presence = PresenceCtx::empty();
    let ada = UserId("ada-device".into());
    presence.set_display_name(ada.clone(), "Eager Dolphin");
    presence.set_color_override(ada.clone(), "#ee83af");
    let cursor = presence.placed_cursor(PlacedPointer {
        user: ada,
        at,
        name: None,
        agent: false,
        activity: None,
        chat: None,
        reaction: None,
    });
    assert!(harness.set_placed_pointers(vec![cursor]));
    harness.relayout();
    let rgba = harness.render_rgba();
    let near = |p: &[u8]| {
        p[0].abs_diff(PINK.0) < 24 && p[1].abs_diff(PINK.1) < 24 && p[2].abs_diff(PINK.2) < 24
    };
    let worn = rgba.chunks_exact(4).filter(|p| near(p)).count();
    (harness, worn)
}

#[test]
fn a_placed_pointer_is_drawn_whole_in_its_colour_even_at_the_edge() {
    let (harness, middle) = frame_with_pointer_at((120.0, 80.0));
    assert_eq!(harness.placed_pointers().len(), 1);
    assert!(
        middle > 300,
        "the arrow and the name tag are drawn in the person's colour ({middle} px)"
    );
    // Right at the edge the tag moves inside rather than running off it:
    // as much of it is drawn as in the middle of the surface.
    let (_, edge) = frame_with_pointer_at((f64::from(SIZE.0) - 4.0, 80.0));
    assert!(
        edge * 10 >= middle * 9,
        "the name tag stays whole on screen at the edge ({edge} of {middle} px)"
    );
}

#[test]
fn pointers_outside_the_surface_or_taken_away_are_not_drawn() {
    let (mut harness, _) = frame_with_pointer_at((120.0, 80.0));
    assert!(
        harness.set_placed_pointers(Vec::new()),
        "taking them down is a change"
    );
    assert!(harness.placed_pointers().is_empty());
    assert!(
        !harness.set_placed_pointers(Vec::new()),
        "nothing shown, nothing to redraw"
    );
    let presence = PresenceCtx::empty();
    let outside = presence.placed_cursor(PlacedPointer {
        user: UserId("far".into()),
        at: (f64::from(SIZE.0) + 50.0, 10.0),
        name: None,
        agent: false,
        activity: None,
        chat: None,
        reaction: None,
    });
    harness.set_placed_pointers(vec![outside]);
    assert!(harness.placed_pointers().is_empty());
}

/// A window showing Ada's caret, labelled with her name, at (100, 80).
struct AdasCaret;

impl tilo_ui::Widget for AdasCaret {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui::Widget>,
    ) -> taffy::NodeId {
        use tilo_ui::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut tilo_ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        presence.draw_peer_caret_labeled(
            scene,
            theme,
            text_renderer,
            transform,
            &UserId("ada-device".into()),
            "Eager Dolphin",
            vello::kurbo::Rect::new(100.0, 70.0, 102.0, 90.0),
        );
    }

    fn name(&self) -> &'static str {
        "AdasCaret"
    }
}

/// The pointers as the frame shows them over Ada's caret, with hers at `at`.
fn pointers_over_adas_caret(at: (f64, f64)) -> Vec<tilo_ui::presence::PeerCursor> {
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(AdasCaret), SIZE, Theme::dark());
    let presence = PresenceCtx::empty();
    let cursor = presence.placed_cursor(PlacedPointer {
        user: UserId("ada-device".into()),
        at,
        name: Some("Eager Dolphin".into()),
        agent: false,
        activity: None,
        chat: None,
        reaction: None,
    });
    harness.set_placed_pointers(vec![cursor]);
    harness.relayout();
    harness.render_rgba();
    harness.drawn_placed_pointers()
}

#[test]
fn a_pointer_whose_tag_meets_its_persons_caret_tag_shows_one_name_tag() {
    // Her caret's tag sits above the caret (y 70..90), from x 100: a pointer
    // just above-left of the caret puts its tag right over the caret's.
    let beside = pointers_over_adas_caret((110.0, 40.0));
    assert_eq!(beside.len(), 1);
    assert!(
        beside[0].name_opacity <= 0.0,
        "the caret's tag names her; the pointer whose tag meets it does not again"
    );
    // Right below the caret its tag hangs clear of the caret's tag above.
    let below = pointers_over_adas_caret((120.0, 95.0));
    assert!(
        below[0].name_opacity > 0.0,
        "a pointer whose tag stays clear of her caret's keeps its own name"
    );
    let away = pointers_over_adas_caret((300.0, 150.0));
    assert!(
        away[0].name_opacity > 0.0,
        "a pointer away from her caret keeps its own name"
    );
}
