//! What a culled frame leaves out, and when it holds.

#![allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "test fixtures: failures are the assertion; regions are whole pixels"
)]

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

use super::{Culling, Keep, bind, keeping, kept, unbind};

const VIEW: Rect = Rect::new(0.0, 0.0, 200.0, 400.0);

#[test]
fn a_scroll_keeps_where_the_copy_lands_in_scene_pixels() {
    let keep = Keep::scrolled(Affine::scale(2.0), VIEW, Vec2::new(0.0, -10.0), &[]).unwrap();
    assert_eq!(keep.region, Rect::new(0.0, 0.0, 400.0, 780.0));
    assert!(keep.moving());
    let still = Keep::scrolled(Affine::IDENTITY, VIEW, Vec2::ZERO, &[]).unwrap();
    assert_eq!(still.region, VIEW);
    assert!(!still.moving());
    assert!(
        Keep::scrolled(Affine::IDENTITY, VIEW, Vec2::new(0.0, -0.5), &[]).is_none(),
        "half a pixel is no copy"
    );
    assert!(
        Keep::scrolled(Affine::IDENTITY, VIEW, Vec2::new(0.0, -400.0), &[]).is_none(),
        "a step past the view keeps nothing"
    );
}

#[test]
fn a_widget_is_left_out_only_wholly_inside_and_clear_of_what_repaints() {
    let mut culling = Culling::default();
    culling.repainted(&[Rect::new(0.0, 100.0, 200.0, 120.0)], true);
    let frame = culling.begin();
    let mut scene = Scene::new();
    let overlay = [Rect::new(190.0, 0.0, 200.0, 400.0)];
    let keep = Keep::scrolled(Affine::IDENTITY, VIEW, Vec2::new(0.0, -20.0), &overlay);
    bind(&scene);
    let mut left_out = Vec::new();
    keeping(&mut scene, keep, |scene| {
        for (index, bounds) in [
            Rect::new(10.0, 10.0, 100.0, 30.0),
            // Into the strip the scroll exposed.
            Rect::new(10.0, 370.0, 100.0, 390.0),
            // Under the bar's lane.
            Rect::new(10.0, 40.0, 195.0, 60.0),
            // Where the last copy's frame repainted.
            Rect::new(10.0, 105.0, 100.0, 115.0),
        ]
        .into_iter()
        .enumerate()
        {
            if kept(scene, Affine::IDENTITY, bounds) {
                left_out.push(index);
            }
        }
    });
    // Into any other scene, nothing is left out.
    let other = Scene::new();
    assert!(!kept(
        &other,
        Affine::IDENTITY,
        Rect::new(10.0, 10.0, 100.0, 30.0)
    ));
    unbind();
    assert_eq!(left_out, [0]);
    assert!(frame.skipped_any());
    assert!(frame.holds(Some(&[Rect::new(0.0, 380.0, 200.0, 400.0)])));
    assert!(
        !frame.holds(Some(&[Rect::new(0.0, 30.5, 200.0, 40.0)])),
        "damage within a pixel of what was left out repaints it"
    );
    assert!(!frame.holds(None), "a whole repaint needs everything");
}

#[test]
fn two_scrolls_at_once_keep_nothing() {
    let _frame = Culling::default().begin();
    let mut scene = Scene::new();
    bind(&scene);
    let outer = Keep::scrolled(Affine::IDENTITY, VIEW, Vec2::new(0.0, -20.0), &[]);
    let inner_view = Rect::new(20.0, 50.0, 180.0, 250.0);
    let inner = Keep::scrolled(Affine::IDENTITY, inner_view, Vec2::new(0.0, -10.0), &[]);
    let still = Keep::scrolled(Affine::IDENTITY, inner_view, Vec2::ZERO, &[]);
    let row = Rect::new(30.0, 100.0, 170.0, 120.0);
    keeping(&mut scene, outer.clone(), |scene| {
        keeping(scene, inner, |scene| {
            assert!(!kept(scene, Affine::IDENTITY, row));
        });
        keeping(scene, still, |scene| {
            assert!(kept(scene, Affine::IDENTITY, row));
        });
        keeping(scene, None, |scene| {
            assert!(!kept(scene, Affine::IDENTITY, row));
        });
    });
    unbind();
}

#[test]
fn a_secret_left_out_still_shows() {
    let mut culling = Culling::default();
    assert!(culling.shows_secret(true, false));
    assert!(
        culling.shows_secret(false, true),
        "left out, it is still on screen"
    );
    assert!(!culling.shows_secret(false, false));
    assert!(!culling.shows_secret(false, true));
}
