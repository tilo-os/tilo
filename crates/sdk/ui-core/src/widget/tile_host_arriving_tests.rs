use super::tests::{paint, starting_host};
use super::*;
use crate::arriving::{Source, Step};
use crate::time::testing::with_monotonic_time;
use crate::widget::SkeletonShape;
use vello::kurbo::Rect;

#[test]
fn an_app_whose_code_arrives_goes_on_with_the_arrival_its_window_showed() {
    let theme = Theme::default();
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut waiting = with_monotonic_time(t0, || {
        TileHost::new(TileContent::Arriving).arriving_from(Source::host("tilo.coffee"))
    });
    waiting.rect = rect;
    assert!(
        with_monotonic_time(t0, || waiting.is_arriving()),
        "the window reserves the app's place at once"
    );
    assert!(!waiting.uses_host_record());
    with_monotonic_time(at(400), || {
        let _ = paint(&mut waiting, &theme);
        assert_eq!(waiting.arrival_step(at(400)), Some(Step::Skeleton));
    });

    // The code is in: the window's host is built again, around the app.
    let (mut arrived, drawn) = with_monotonic_time(at(500), || starting_host(true));
    with_monotonic_time(at(500), || {
        arrived.continue_arrival(&waiting);
        let _ = paint(&mut arrived, &theme);
        assert!(drawn.get(), "the arrived app draws at once");
        assert!(
            arrived.needs_animation_frame(),
            "and cross-fades in over the skeleton instead of cutting to it"
        );
    });

    // A place nobody saw anything in has nothing to fade: an app whose
    // code was here inside the frame budget shows at once.
    let t1 = crate::time::now();
    let unseen = with_monotonic_time(t1, || TileHost::new(TileContent::Arriving));
    let soon = t1 + Duration::from_millis(5);
    let (mut quick, _) = with_monotonic_time(soon, || starting_host(true));
    with_monotonic_time(soon, || {
        quick.continue_arrival(&unseen);
        let _ = paint(&mut quick, &theme);
        assert!(!quick.needs_animation_frame());
        assert_eq!(quick.arrival_step(soon), None);
    });
}

/// An app on its way that declares `shape` for its placeholder, or none.
struct Declaring(Option<SkeletonShape>);

impl Widget for Declaring {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
    fn name(&self) -> &'static str {
        "Declaring"
    }
    fn presented(&self) -> bool {
        false
    }
    fn arrival_shape(&self) -> SkeletonShape {
        self.0.unwrap_or(SkeletonShape::Ground)
    }
}

impl crate::widget::app_widget::AppWidget for Declaring {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Declaring"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("D")
    }
}

#[test]
fn a_window_that_declares_no_shape_paints_no_skeleton_bars() {
    let theme = Theme::default();
    for shape in [None, Some(SkeletonShape::Page)] {
        let t0 = crate::time::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut host = with_monotonic_time(t0, || {
            TileHost::new(TileContent::App(Box::new(Declaring(shape))))
                .arriving_from(Source::host("tilo.coffee"))
        });
        host.rect = Rect::new(50.0, 50.0, 1250.0, 850.0);
        let painted = |host: &mut TileHost, ms: u64| {
            with_monotonic_time(at(ms), || {
                let scene = paint(host, &theme);
                (host.arrival_step(at(ms)), scene.encoding().n_paths)
            })
        };
        let (ground_step, ground) = painted(&mut host, 50);
        let (skeleton_step, skeleton) = painted(&mut host, 400);
        assert_eq!(ground_step, Some(Step::Ground));
        assert_eq!(skeleton_step, Some(Step::Skeleton));
        if shape.is_none() {
            assert_eq!(
                skeleton, ground,
                "a window with no declared shape stays its ground: nothing guesses its content"
            );
        } else {
            assert!(
                skeleton > ground,
                "a declared page stands in as its bars: {ground} -> {skeleton}"
            );
        }
    }
}
