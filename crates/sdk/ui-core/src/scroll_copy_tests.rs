use super::*;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

/// A widget that is only a box, a child clip and children.
struct Boxed {
    rect: Rect,
    clip: Option<Rect>,
    children: Vec<Boxed>,
}

impl Boxed {
    fn leaf(rect: Rect) -> Self {
        Self {
            rect,
            clip: None,
            children: Vec::new(),
        }
    }
}

impl Widget for Boxed {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "Boxed"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn child_clip_rect(&self) -> Option<Rect> {
        self.clip
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(|child| child as &dyn Widget)
            .collect()
    }
}

/// A 400×300 window: a 40 px toolbar over a list scrolled in a viewport
/// below it, with three rows.
fn window(scroll: f64) -> Boxed {
    let viewport = Rect::new(0.0, 100.0, 400.0, 300.0);
    let rows = (0..3)
        .map(|row| {
            let top = 100.0 + f64::from(row) * 50.0 - scroll;
            Boxed::leaf(Rect::new(0.0, top, 380.0, top + 50.0))
        })
        .collect();
    Boxed {
        rect: Rect::new(0.0, 0.0, 400.0, 300.0),
        clip: None,
        children: vec![
            Boxed::leaf(Rect::new(0.0, 0.0, 400.0, 40.0)),
            Boxed {
                rect: viewport,
                clip: Some(viewport),
                children: rows,
            },
        ],
    }
}

fn scroller(root: &Boxed) -> &Boxed {
    &root.children[1]
}

fn offer_from(root: &Boxed, delta: Vec2, overlays: Vec<Rect>) -> ScrollOffer {
    offer_at(root, delta, overlays, 1.0)
}

/// The scroller's offer, drawn at `scale` device pixels per logical one.
fn offer_at(root: &Boxed, delta: Vec2, overlays: Vec<Rect>, scale: f64) -> ScrollOffer {
    let scroller = scroller(root);
    let offers = accepting(|| {
        assert!(offer(
            scroller,
            scroller.rect,
            delta,
            overlays,
            Affine::scale(scale)
        ));
    });
    offers.into_iter().next().expect("one offer")
}

/// What a host repaints for `planned`, as hosts assemble it: the plan's
/// damage carried by its copies, and what they exposed.
fn presented_damage(planned: &CopyPlan) -> Vec<Rect> {
    let mut damage = planned.damage.clone();
    carry(&planned.copies, &mut damage);
    damage.extend(planned.exposed.iter().copied());
    damage
}

#[test]
fn an_offer_reaches_only_a_host_that_copies() {
    let root = window(0.0);
    assert!(
        !offer(
            scroller(&root),
            scroller(&root).rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::IDENTITY
        ),
        "with no host collecting, the scroller damages its viewport"
    );
    let offers = accepting(|| {
        assert!(offer(
            scroller(&root),
            scroller(&root).rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::IDENTITY
        ));
    });
    assert_eq!(offers.len(), 1);
    assert!(
        !offer(
            scroller(&root),
            scroller(&root).rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::IDENTITY
        ),
        "the collection ends with the host's"
    );
}

#[test]
#[cfg(feature = "native")]
fn moves_inside_an_offering_scroller_by_its_delta_are_the_copys() {
    let before_tree = window(0.0);
    let after_tree = window(10.0);
    let before = placed(&before_tree);
    let after = placed(&after_tree);
    // The trees are different values; give them one identity.
    let after: Vec<Placed> = before
        .iter()
        .zip(&after)
        .map(|(&(id, _), &(_, rect))| (id, rect))
        .collect();
    let offer = ScrollOffer {
        owner: before[2].0.0,
        viewport: scroller(&before_tree).rect,
        delta: Vec2::new(0.0, -10.0),
        overlays: Vec::new(),
        transform: Affine::IDENTITY,
    };
    assert_eq!(
        moves_besides(&before, &after, std::slice::from_ref(&offer)),
        Moves::default()
    );
    // Without the offer, every row moved.
    assert_eq!(moves_besides(&before, &after, &[]).besides.len(), 6);
    // A row that stayed put over the content (a sticky header) is damage
    // where it is and where the copy took its pixels.
    let mut sticky = after.clone();
    sticky[3].1 = before[3].1;
    let reach = before[3]
        .1
        .inflate(EFFECT_REACH, EFFECT_REACH)
        .intersect(scroller(&before_tree).rect);
    assert_eq!(
        moves_besides(&before, &sticky, std::slice::from_ref(&offer)),
        Moves {
            besides: vec![],
            carried: vec![reach],
        }
    );
    // One that moved by anything else is not the copy's.
    let mut resized = after.clone();
    resized[3].1.y1 += 4.0;
    assert_eq!(
        moves_besides(&before, &resized, std::slice::from_ref(&offer)).besides,
        vec![before[3].1, resized[3].1]
    );
    // A row that stayed put clear of the viewport (a pinned title) is.
    let mut pinned = after.clone();
    pinned[5].1 = Rect::new(0.0, 600.0, 380.0, 650.0);
    let mut pinned_before = before.clone();
    pinned_before[5].1 = pinned[5].1;
    assert_eq!(
        moves_besides(&pinned_before, &pinned, std::slice::from_ref(&offer)),
        Moves::default()
    );
    // The scroller itself may not move.
    let mut moved_scroller = after.clone();
    moved_scroller[2].1 = moved_scroller[2].1 + Vec2::new(0.0, 5.0);
    assert_eq!(
        moves_besides(&before, &moved_scroller, std::slice::from_ref(&offer))
            .besides
            .len(),
        2
    );
    // A row that came into the content (a list mounting what the scroll
    // brought near) is damage where it is; one that went, where it was (the
    // copy carries that).
    let came = Rect::new(0.0, 240.0, 380.0, 290.0);
    let mut grown = after.clone();
    grown.push(((std::ptr::from_ref(&came).cast::<()>(), 2), came));
    let gone = after[..5].to_vec();
    assert_eq!(
        moves_besides(&before, &grown, std::slice::from_ref(&offer)),
        Moves {
            besides: vec![],
            carried: vec![came],
        }
    );
    assert_eq!(
        moves_besides(&before, &gone, std::slice::from_ref(&offer)),
        Moves {
            besides: vec![],
            carried: vec![before[5].1],
        }
    );
    // A widget that came outside any scrolled content is not.
    let mut sibling = after.clone();
    sibling.insert(2, ((std::ptr::from_ref(&came).cast::<()>(), 1), came));
    assert_eq!(
        moves_besides(&before, &sibling, &[offer]).besides,
        vec![came]
    );
}

#[test]
fn a_planned_copy_keeps_what_stays_and_damages_what_was_exposed() {
    let root = window(10.0);
    let offer = offer_from(&root, Vec2::new(0.0, -10.0), Vec::new());
    let planned = plan(&[offer], &[&root], 1.0, (400, 300));
    assert_eq!(
        planned.copies,
        vec![DeviceCopy {
            x: 0,
            y: 110,
            width: 400,
            height: 190,
            dx: 0,
            dy: -10,
        }]
    );
    let presented = presented_damage(&planned);
    let covers = |point: Point| presented.iter().any(|rect| rect.contains(point));
    // The strip the scroll exposed at the bottom of the viewport.
    assert!(covers(Point::new(200.0, 295.0)), "{presented:?}");
    // The middle of the viewport is copied, not repainted.
    assert!(!covers(Point::new(200.0, 200.0)), "{presented:?}");
}

#[test]
fn what_reaches_into_the_viewport_repaints_where_it_is_and_where_the_copy_took_it() {
    let mut root = window(0.0);
    // A toolbar tall enough that its effect reach (a shadow) falls 6 px into
    // the viewport.
    root.children[0].rect = Rect::new(0.0, 0.0, 400.0, 90.0);
    let offer = offer_from(&root, Vec2::new(0.0, 10.0), Vec::new());
    let planned = plan(&[offer], &[&root], 1.0, (400, 300));
    assert_eq!(planned.copies.len(), 1);
    let presented = presented_damage(&planned);
    let covers = |point: Point| presented.iter().any(|rect| rect.contains(point));
    // The exposed strip at the top, the shadow where it is, and where the
    // copy took the shadow's old pixels: 10 px further down.
    assert!(covers(Point::new(200.0, 105.0)), "{presented:?}");
    assert!(covers(Point::new(200.0, 115.0)), "{presented:?}");
    assert!(!covers(Point::new(200.0, 150.0)), "{presented:?}");
}

#[test]
fn overlays_repaint_where_they_are_and_where_the_copy_took_them() {
    let root = window(10.0);
    // A horizontal scroll bar's track along the bottom of the viewport.
    let bar = Rect::new(40.0, 260.0, 340.0, 268.0);
    let offer = offer_from(&root, Vec2::new(0.0, -30.0), vec![bar]);
    let planned = plan(&[offer], &[&root], 1.0, (400, 300));
    assert_eq!(planned.copies.len(), 1);
    let presented = presented_damage(&planned);
    let covers = |point: Point| presented.iter().any(|rect| rect.contains(point));
    assert!(covers(Point::new(200.0, 264.0)), "{presented:?}");
    // The copy took the bar's old pixels 30 px up.
    assert!(covers(Point::new(200.0, 234.0)), "{presented:?}");
    assert!(!covers(Point::new(200.0, 220.0)), "{presented:?}");
}

#[test]
fn a_fractional_device_delta_damages_the_viewport() {
    let root = window(10.5);
    let offer = offer_from(&root, Vec2::new(0.0, -10.5), Vec::new());
    let planned = plan(&[offer], &[&root], 1.0, (400, 300));
    assert!(planned.copies.is_empty());
    assert_eq!(planned.damage, vec![Rect::new(0.0, 100.0, 400.0, 300.0)]);
    // At 150 % the same half pixel is three quarters of a device pixel; at
    // 200 % it is one whole one.
    let offer = offer_at(&root, Vec2::new(0.0, -10.5), Vec::new(), 1.5);
    assert!(plan(&[offer], &[&root], 1.5, (600, 450)).copies.is_empty());
    let offer = offer_at(&root, Vec2::new(0.0, -10.5), Vec::new(), 2.0);
    let doubled = plan(&[offer], &[&root], 2.0, (800, 600));
    assert_eq!(doubled.copies.len(), 1);
    assert_eq!(doubled.copies[0].dy, -21);
}

#[test]
fn a_scroller_drawn_under_another_transform_damages_the_window() {
    let root = window(10.0);
    // An ancestor that draws its content moved: the viewport is not where
    // the window shows it.
    let scroller = scroller(&root);
    let offers = accepting(|| {
        assert!(offer(
            scroller,
            scroller.rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::translate((0.0, 30.0))
        ));
    });
    let planned = plan(&offers, &[&root], 1.0, (400, 300));
    assert!(planned.copies.is_empty());
    assert_eq!(planned.damage, vec![Rect::new(0.0, 0.0, 400.0, 300.0)]);
}

#[test]
fn bands_never_read_what_an_earlier_band_wrote() {
    let copy = DeviceCopy {
        x: 0,
        y: 10,
        width: 100,
        height: 300,
        dx: 0,
        dy: -10,
    };
    let up = bands(copy);
    assert_eq!(up.len(), 3);
    assert_eq!(up.iter().map(|band| band.height).sum::<u32>(), 300);
    // Moving up, the top band goes first; moving down, the bottom one.
    assert!(up.windows(2).all(|pair| pair[0].y < pair[1].y));
    let down = bands(DeviceCopy { dy: 10, ..copy });
    assert!(down.windows(2).all(|pair| pair[0].y > pair[1].y));
    // Sideways, in columns, nearest the direction of travel first.
    let left = bands(DeviceCopy {
        width: 300,
        dy: 0,
        dx: -5,
        ..copy
    });
    assert!(left.windows(2).all(|pair| pair[0].x < pair[1].x));
    assert!(left.iter().all(|band| band.height == 300));
}

#[test]
fn an_ancestor_clip_bounds_the_copy() {
    let mut root = window(10.0);
    // A parent that shows only the top half of the viewport.
    let clipped = Rect::new(0.0, 100.0, 400.0, 200.0);
    let list = root.children.remove(1);
    root.children.push(Boxed {
        rect: list.rect,
        clip: Some(clipped),
        children: vec![list],
    });
    let scroller = &root.children[1].children[0];
    let offers = accepting(|| {
        assert!(offer(
            scroller,
            scroller.rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::IDENTITY
        ));
    });
    let planned = plan(&offers, &[&root], 1.0, (400, 300));
    assert_eq!(planned.copies.len(), 1);
    let copy = planned.copies[0];
    assert!(copy.y + copy.height <= 200, "{copy:?}");
    let presented = presented_damage(&planned);
    assert!(
        presented
            .iter()
            .all(|rect| rect.y1 <= 200.0 && rect.y0 >= 100.0),
        "{presented:?}"
    );
}

#[test]
fn a_scroller_the_tree_does_not_hold_damages_its_viewport() {
    let root = window(10.0);
    let stray = window(10.0);
    let offers = accepting(|| {
        assert!(offer(
            scroller(&stray),
            scroller(&stray).rect,
            Vec2::new(0.0, -10.0),
            Vec::new(),
            Affine::IDENTITY
        ));
    });
    let planned = plan(&offers, &[&root], 1.0, (400, 300));
    assert!(planned.copies.is_empty());
    assert_eq!(planned.damage, vec![Rect::new(0.0, 100.0, 400.0, 300.0)]);
}

#[test]
fn pixels_copy_within_an_image_in_either_direction() {
    // A 4×4 image whose every pixel holds its own index.
    let image: Vec<u8> = (0..16_u8).flat_map(|index| [index, 0, 0, 255]).collect();
    let at = |pixels: &[u8], x: usize, y: usize| pixels[(y * 4 + x) * 4];
    let down = DeviceCopy {
        x: 0,
        y: 0,
        width: 4,
        height: 3,
        dx: 0,
        dy: 1,
    };
    let mut pixels = image.clone();
    assert!(copy_pixels(&mut pixels, (4, 4, 16), &[down]));
    assert_eq!(
        (0..4).map(|y| at(&pixels, 1, y)).collect::<Vec<_>>(),
        vec![1, 1, 5, 9]
    );
    let up_left = DeviceCopy {
        x: 1,
        y: 1,
        width: 3,
        height: 3,
        dx: -1,
        dy: -1,
    };
    let mut pixels = image.clone();
    assert!(copy_pixels(&mut pixels, (4, 4, 16), &[up_left]));
    assert_eq!(at(&pixels, 0, 0), 5);
    assert_eq!(at(&pixels, 2, 2), 15);
    let outside = DeviceCopy { dy: 2, ..down };
    let mut pixels = image.clone();
    assert!(!copy_pixels(&mut pixels, (4, 4, 16), &[outside]));
    assert_eq!(pixels, image, "a refused copy changes nothing");
}

/// A copy through the scratch texture moves exactly the pixels it names,
/// in either direction, on the GPU the toolkit renders with.
#[test]
fn a_gpu_copy_moves_pixels_within_the_target() {
    use crate::offscreen::OffscreenRenderer;
    let Ok(mut renderer) = OffscreenRenderer::try_new(64, 64) else {
        panic!("a scroll copy needs the GPU the toolkit renders with (lavapipe in CI)");
    };
    let mut scene = Scene::new();
    let ink = vello::peniko::Color::from_rgba8(200, 30, 30, 255);
    let paper = vello::peniko::Color::from_rgba8(255, 255, 255, 255);
    scene.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        ink,
        None,
        &Rect::new(8.0, 8.0, 24.0, 24.0),
    );
    let _ = renderer.render(&scene, paper);
    let copy = DeviceCopy {
        x: 0,
        y: 0,
        width: 64,
        height: 48,
        dx: 0,
        dy: 16,
    };
    let mut moved = Scene::new();
    moved.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        ink,
        None,
        &Rect::new(8.0, 24.0, 24.0, 40.0),
    );
    // The copy leaves the top 16 rows as they were; repaint them.
    let _ = renderer.render_copies_and_damage_gpu_wait(
        &moved,
        paper,
        &[copy],
        &[Rect::new(0.0, 0.0, 64.0, 16.0)],
    );
    let actual = renderer.readback_current();
    let mut full = OffscreenRenderer::new(64, 64);
    let expected = full.render(&moved, paper);
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
        0
    );
}
