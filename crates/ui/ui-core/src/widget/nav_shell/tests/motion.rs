//! The shell moves what a person moved and lands what anything else moved: a click or a tap
//! slides, a key, an agent or a still theme lands at once.

use super::*;
use crate::tokens::MotionSource;

fn still() -> Theme {
    let mut theme = Theme::dark();
    theme.reduce_motion = true;
    theme
}

/// Whether the content of a narrow mail window is still sliding one frame after `push` brought
/// it to the front. A push that landed owes at most the frames that lay the landed pose out and
/// drop what it hides, and asks for nothing after them.
fn slides_after_push(push: impl FnOnce(&mut NavShell)) -> bool {
    let mut h = harness(mail_shell(), 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    push(&mut h.root_mut::<NavShell>());
    h.relayout_at((560, 800)).expect("the push lays out");
    let shell = h.root::<NavShell>();
    assert_eq!(shell.front_pane(), NavPane::Content);
    let moving = shell.inputs.borrow().push.is_moving(crate::time::now());
    if moving {
        assert!(shell.needs_animation_frame(), "a slide asks for its frames");
    } else {
        for _ in 0..3 {
            if !h.root::<NavShell>().needs_animation_frame() {
                break;
            }
            h.relayout_at((560, 800)).expect("a landing frame lays out");
        }
        assert!(
            !h.root::<NavShell>().needs_animation_frame(),
            "a push that landed asks for no frames beyond its landing's"
        );
    }
    moving
}

/// A push follows the input that asked for it: a click or a tap slides the content in; a key,
/// an agent's action or a show nobody caused lands it. A host that knows the origin says so, and
/// that wins over whatever this thread last noted.
#[test]
fn a_push_slides_for_a_click_or_a_tap_and_lands_for_anything_else() {
    assert!(
        !slides_after_push(|shell| shell.show(NavPane::Content)),
        "a show nobody caused lands at once"
    );
    for (source, slides) in [
        (MotionSource::Pointer, true),
        (MotionSource::Gesture, true),
        (MotionSource::Keyboard, false),
        (MotionSource::Action, false),
    ] {
        let moving = slides_after_push(|shell| {
            crate::motion::note_source(source);
            shell.show(NavPane::Content);
        });
        assert_eq!(moving, slides, "a push after {source:?}");
    }
    assert!(
        !slides_after_push(|shell| {
            crate::motion::note_source(MotionSource::Pointer);
            shell.show_from(NavPane::Content, MotionSource::Action);
        }),
        "an agent's open lands at once right after a click elsewhere"
    );
    assert!(
        slides_after_push(|shell| shell.show_from(NavPane::Content, MotionSource::Pointer)),
        "a person's click slides even when nothing was noted"
    );
}

/// Hiding the sidebar slides it away; a person who asked for less motion sees it gone on the
/// next frame. A resize that switches the layout stands in its new place at once under reduced
/// motion, nothing sliding or fading in.
#[test]
fn reduced_motion_lands_the_sidebar_and_a_switched_layout_at_once() {
    for (theme, slides) in [(Theme::dark(), true), (still(), false)] {
        let mut h = harness(mail_shell(), 1280, 800, theme);
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>().toggle_sidebar();
        h.relayout_at((1280, 800)).expect("layout");
        if slides {
            assert!(
                h.root::<NavShell>().needs_animation_frame(),
                "hiding the sidebar animates"
            );
        }
        h.relayout_at((1280, 800)).expect("layout");
        assert_eq!(
            geometry(&h).sidebar_column == Rect::ZERO,
            !slides,
            "the sidebar is still on its way out a frame later unless motion is reduced"
        );
    }

    let shell = NavShell::new()
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0);
    let mut h = harness(shell, 1280, 700, still());
    settle(&mut h, 1280, 700);
    let point = geometry(&h).content_column.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    h.relayout_at((400, 700)).expect("narrow");
    let shell = h.root::<NavShell>();
    assert_eq!(
        shell.front_pane(),
        NavPane::Content,
        "the pressed page stays"
    );
    assert_eq!(shell.geometry().content_column.x0, 0.0, "no slide");
    assert!(
        !shell.switch_columns.contains(&true),
        "and no column it has to fade in"
    );
    assert_eq!(shell.switch_fade.progress(crate::time::now()), 1.0);
}
