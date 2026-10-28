use super::*;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};
use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill};

fn right_anchored_positioner(size: (i32, i32), anchor: (i32, i32)) -> PositionerState {
    PositionerState {
        rect_size: size.into(),
        anchor_rect: Rectangle::new(anchor.into(), (1, 1).into()),
        anchor_edges: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment::SlideX,
        ..Default::default()
    }
}

fn popup_screen_rect(parent: Rect, geometry: Rectangle<i32, Logical>) -> Rect {
    popup_rect_from_parent(parent, geometry)
}

fn draw_rect(scene: &mut Scene, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
}

fn assert_rect_within_viewport(rect: Rect, viewport: Rect) {
    assert!(
        rect.x0 >= viewport.x0
            && rect.y0 >= viewport.y0
            && rect.x1 <= viewport.x1
            && rect.y1 <= viewport.y1,
        "{rect:?} extends outside {viewport:?}"
    );
}

#[test]
fn popup_constraints_are_parent_relative_and_saturating() {
    // Phase `popup_constraints_are_relative_to_the_parent_not_global_origin`.
    {
        let positioner = PositionerState {
            rect_size: (80, 20).into(),
            anchor_rect: Rectangle::new((95, 10).into(), (1, 1).into()),
            anchor_edges: Anchor::Right,
            gravity: Gravity::Right,
            constraint_adjustment: ConstraintAdjustment::SlideX,
            ..Default::default()
        };
        // The parent begins at x=300 on a 400px output.  The visible target
        // is therefore -300..100 in parent coordinates, not 0..400: using
        // global bounds here would let this menu escape off the output.
        let geometry = constrained_popup_geometry(
            positioner,
            (300, 0),
            Rectangle::new((0, 0).into(), (400, 200).into()),
        );
        assert_eq!(geometry.loc.x, 20);
        assert_eq!(geometry.size.w, 80);
        assert_eq!(geometry.loc.y, 0);
    }
    // Phase `popup_constraints_preserve_requested_size_when_slide_can_fit_it`.
    {
        let positioner = right_anchored_positioner((40, 30), (95, 5));
        let geometry = constrained_popup_geometry(
            positioner,
            (0, 0),
            Rectangle::new((0, 0).into(), (100, 100).into()),
        );
        assert_eq!(geometry.loc.x, 60);
        assert_eq!(geometry.size, (40, 30).into());
    }
    // Phase `popup_scene_rect_saturates_extreme_client_geometry`.
    {
        let parent = Rect::new(10.0, 20.0, 110.0, 120.0);
        let geometry = Rectangle::new((i32::MAX - 4, i32::MAX - 2).into(), (64, 64).into());

        let rect = popup_rect_from_parent(parent, geometry);

        assert_eq!(rect.x1, parent.x0 + f64::from(i32::MAX));
        assert_eq!(rect.y1, parent.y0 + f64::from(i32::MAX));
        assert!(rect.x1.is_finite() && rect.y1.is_finite());
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "The single raster fixture keeps normal and scaled popup geometry in one scene contract; its fixed, viewport-bounded coordinates are explicitly checked before rendering."
)]
fn popup_and_ime_geometry_fixture_renders_normal_and_scaled_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let output = Rectangle::new((0, 0).into(), (800, 500).into());
    let parent = Rect::new(280.0, 130.0, 640.0, 370.0);
    let popup = popup_screen_rect(
        parent,
        constrained_popup_geometry(
            right_anchored_positioner((190, 110), (350, 72)),
            (280, 130),
            output,
        ),
    );
    let nested = popup_screen_rect(
        popup,
        constrained_popup_geometry(
            right_anchored_positioner((130, 76), (185, 54)),
            (
                i32::try_from(popup.x0.round() as i64)?,
                i32::try_from(popup.y0.round() as i64)?,
            ),
            output,
        ),
    );
    let candidate = constrain_transient_rect(
        Rect::new(
            parent.x1 - 18.0,
            parent.y1 - 22.0,
            parent.x1 + 142.0,
            parent.y1 + 30.0,
        ),
        Rect::new(0.0, 0.0, 800.0, 500.0),
    );
    let normal_viewport = Rect::new(0.0, 0.0, 800.0, 500.0);
    assert_rect_within_viewport(popup, normal_viewport);
    assert_rect_within_viewport(nested, normal_viewport);
    assert_rect_within_viewport(candidate, normal_viewport);

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/popup-ime-geometry-fixtures");
    std::fs::create_dir_all(&out)?;
    let mut normal = Scene::new();
    draw_rect(
        &mut normal,
        Rect::new(0.0, 0.0, 800.0, 500.0),
        Color::from_rgb8(0x0b, 0x10, 0x1d),
    );
    draw_rect(&mut normal, parent, Color::from_rgb8(0x45, 0x72, 0xb8));
    draw_rect(&mut normal, popup, Color::from_rgb8(0x36, 0xbc, 0xa6));
    draw_rect(&mut normal, nested, Color::from_rgb8(0xf0, 0xa0, 0x45));
    draw_rect(&mut normal, candidate, Color::from_rgb8(0xdc, 0x66, 0x9c));
    let mut renderer = OffscreenRenderer::new(800, 500);
    renderer.render_png(&normal, Color::BLACK, out.join("normal.png"));

    // The compositor positions in logical space.  Reserve a small work-area
    // inset so an edge-constrained transient is visibly inside the physical
    // framebuffer after the 1.5x logical-to-physical transform, rather than
    // appearing cropped by the raster edge.
    let logical_output = Rectangle::new((16, 12).into(), (568, 336).into());
    let logical_parent = Rect::new(340.0, 86.0, 580.0, 266.0);
    let logical_popup = popup_screen_rect(
        logical_parent,
        constrained_popup_geometry(
            right_anchored_positioner((160, 90), (230, 50)),
            (340, 86),
            logical_output,
        ),
    );
    let logical_nested = popup_screen_rect(
        logical_popup,
        constrained_popup_geometry(
            right_anchored_positioner((110, 64), (155, 36)),
            (
                i32::try_from(logical_popup.x0.round() as i64)?,
                i32::try_from(logical_popup.y0.round() as i64)?,
            ),
            logical_output,
        ),
    );
    let logical_candidate = constrain_transient_rect(
        Rect::new(550.0, 246.0, 670.0, 292.0),
        Rect::new(16.0, 12.0, 584.0, 348.0),
    );
    let scale = 1.5;
    let scale_rect = |rect: Rect| {
        Rect::new(
            rect.x0 * scale,
            rect.y0 * scale,
            rect.x1 * scale,
            rect.y1 * scale,
        )
    };
    let physical_viewport = Rect::new(0.0, 0.0, 900.0, 540.0);
    let physical_work_area = scale_rect(Rect::new(16.0, 12.0, 584.0, 348.0));
    assert_rect_within_viewport(scale_rect(logical_popup), physical_viewport);
    assert_rect_within_viewport(scale_rect(logical_nested), physical_viewport);
    assert_rect_within_viewport(scale_rect(logical_candidate), physical_viewport);
    // Assert the stronger contract too: all constrained surfaces retain
    // the physical 24px right-edge safe margin, not merely a non-clipped
    // endpoint at the framebuffer boundary.
    assert_rect_within_viewport(scale_rect(logical_popup), physical_work_area);
    assert_rect_within_viewport(scale_rect(logical_nested), physical_work_area);
    assert_rect_within_viewport(scale_rect(logical_candidate), physical_work_area);
    let mut scaled = Scene::new();
    draw_rect(
        &mut scaled,
        Rect::new(0.0, 0.0, 900.0, 540.0),
        Color::from_rgb8(0x0b, 0x10, 0x1d),
    );
    draw_rect(
        &mut scaled,
        scale_rect(logical_parent),
        Color::from_rgb8(0x45, 0x72, 0xb8),
    );
    draw_rect(
        &mut scaled,
        scale_rect(logical_popup),
        Color::from_rgb8(0x36, 0xbc, 0xa6),
    );
    draw_rect(
        &mut scaled,
        scale_rect(logical_nested),
        Color::from_rgb8(0xf0, 0xa0, 0x45),
    );
    draw_rect(
        &mut scaled,
        scale_rect(logical_candidate),
        Color::from_rgb8(0xdc, 0x66, 0x9c),
    );
    let mut scaled_renderer = OffscreenRenderer::new(900, 540);
    scaled_renderer.render_png(
        &scaled,
        Color::BLACK,
        out.join("edge-constrained-scale-150.png"),
    );
    Ok(())
}
