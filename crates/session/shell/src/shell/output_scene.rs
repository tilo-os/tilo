//! The shell's desktop as each display draws it.
//!
//! The shell lays out in the **logical** desktop — the space outputs tile in,
//! the pointer moves in and clients are configured in (see
//! `compositor::backend::layout`). A display paints its part of that desktop
//! into its own rectangle of the **scene**, the one physical-pixel texture the
//! compositor presents, magnified by its own scale. So chrome is one logical
//! size everywhere and each monitor draws it at its own density: a 32 px top
//! bar is 64 physical rows on a 2x laptop panel and 32 on the 1x monitor
//! beside it.
//!
//! [`OutputGeometry::to_scene`] is that per-display magnification. Painting
//! applies it inside a clip of the display's scene rectangle
//! ([`compose`]); the compositor maps input back through its inverse before
//! the shell hit-tests, so what is drawn under the pointer is what it hits.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

/// One display the shell draws across.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputGeometry {
    /// The compositor's stable id for the display (`OutputId`). It says
    /// nothing about which display is primary: the backend lists that one
    /// first.
    pub id: u32,
    /// The physical monitor behind the output, stable across restarts and
    /// port swaps that renumber [`Self::id`]: its EDID identity, told apart
    /// from an identical monitor by connector, or the connector alone
    /// without a readable EDID. Empty when the backend knows neither.
    pub monitor: String,
    /// Left edge in the logical desktop.
    pub x: f64,
    /// Top edge in the logical desktop.
    pub y: f64,
    /// Logical width.
    pub width: f64,
    /// Logical height.
    pub height: f64,
    /// The display's scale: physical pixels per logical pixel.
    pub scale: f64,
    /// Where the display's pixels sit in the painted scene, in physical
    /// pixels.
    pub scene: Rect,
}

impl OutputGeometry {
    /// A display at scale 1 whose scene rectangle is its logical one — a
    /// single window, a test viewport, or any arrangement where every scale
    /// is 1.
    #[must_use]
    pub fn unscaled(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            id: 0,
            monitor: String::new(),
            x,
            y,
            width,
            height,
            scale: 1.0,
            scene: Rect::new(x, y, x + width, y + height),
        }
    }

    /// The same display under the compositor's id `id`.
    #[must_use]
    pub fn with_id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    /// The display's rectangle in the logical desktop.
    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.x + self.width, self.y + self.height)
    }

    /// Whether `p` (logical) lies on this display.
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.x + self.width && p.y < self.y + self.height
    }

    /// Logical desktop → this display's scene pixels.
    ///
    /// The magnification is taken from the two extents rather than
    /// [`Self::scale`] so the far edges meet exactly, whatever rounding
    /// produced the logical size — the same ratio the compositor's pointer
    /// mapping uses, so painting and hit-testing agree to the pixel.
    #[must_use]
    pub fn to_scene(&self) -> Affine {
        let ratio = |scene: f64, logical: f64| {
            if logical > 0.0 && scene > 0.0 {
                scene / logical
            } else {
                1.0
            }
        };
        let sx = ratio(self.scene.width(), self.width);
        let sy = ratio(self.scene.height(), self.height);
        Affine::translate((self.scene.x0, self.scene.y0))
            .pre_scale_non_uniform(sx, sy)
            .pre_translate(vello::kurbo::Vec2::new(-self.x, -self.y))
    }

    /// Whether this display draws the logical desktop unmagnified at the
    /// same place.
    fn is_identity(&self) -> bool {
        const EPS: f64 = 1e-9;
        (self.scene.x0 - self.x).abs() < EPS
            && (self.scene.y0 - self.y).abs() < EPS
            && (self.scene.width() - self.width).abs() < EPS
            && (self.scene.height() - self.height).abs() < EPS
    }
}

/// Whether the scene is the logical desktop unchanged: every display draws
/// at scale 1 where it sits logically. Painting then skips the per-display
/// passes entirely.
#[must_use]
pub fn is_identity(outputs: &[OutputGeometry]) -> bool {
    outputs.iter().all(OutputGeometry::is_identity)
}

/// The size of the scene the displays paint into.
#[must_use]
pub fn scene_extent(outputs: &[OutputGeometry]) -> (f64, f64) {
    outputs.iter().fold((0.0_f64, 0.0_f64), |(w, h), o| {
        (w.max(o.scene.x1), h.max(o.scene.y1))
    })
}

/// The size of the logical desktop the displays cover.
#[must_use]
pub fn logical_extent(outputs: &[OutputGeometry]) -> (f64, f64) {
    outputs.iter().fold((0.0_f64, 0.0_f64), |(w, h), o| {
        (w.max(o.x + o.width), h.max(o.y + o.height))
    })
}

/// The display a logical point is on: the one containing it, else the
/// nearest (a point in a gap of an L-shaped arrangement still belongs
/// somewhere).
#[must_use]
pub fn output_at(outputs: &[OutputGeometry], p: Point) -> Option<&OutputGeometry> {
    outputs.iter().find(|o| o.contains(p)).or_else(|| {
        outputs.iter().min_by(|a, b| {
            distance_sq(a.rect(), p)
                .partial_cmp(&distance_sq(b.rect(), p))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    })
}

fn distance_sq(r: Rect, p: Point) -> f64 {
    let dx = (r.x0 - p.x).max(0.0).max(p.x - r.x1);
    let dy = (r.y0 - p.y).max(0.0).max(p.y - r.y1);
    dx * dx + dy * dy
}

/// A logical point in scene pixels, through the display it is on.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine * Point is floating-point matrix application; it cannot panic or overflow"
)]
pub fn point_to_scene(outputs: &[OutputGeometry], p: Point) -> Point {
    output_at(outputs, p).map_or(p, |o| o.to_scene() * p)
}

/// The scene pixels a logical rectangle covers: the bounding box of its part
/// on every display, each through that display's magnification. `None` when
/// it lies on no display.
#[must_use]
pub fn rect_to_scene(outputs: &[OutputGeometry], rect: Rect) -> Option<Rect> {
    if outputs.is_empty() {
        return Some(rect);
    }
    outputs
        .iter()
        .filter_map(|o| {
            let part = rect.intersect(o.rect());
            (part.width() > 0.0 && part.height() > 0.0)
                .then(|| o.to_scene().transform_rect_bbox(part))
        })
        .reduce(|a, b| a.union(b))
}

/// Append a scene drawn in logical coordinates to `target`, once per
/// display: magnified by that display's scale and clipped to its scene
/// rectangle.
///
/// A draw in `logical` that opened a layer and never closed it is closed
/// inside each display's pass, so it cannot swallow that display's clip and
/// blank every display after it. The leak is still a bug in that draw: it
/// is logged and, in a debug build, fails an assertion.
pub fn compose(outputs: &[OutputGeometry], logical: &Scene, target: &mut Scene) {
    let leaked = compose_closing_leaks(outputs, logical, target);
    if leaked > 0 {
        tracing::error!(
            leaked,
            "a draw in the desktop scene left {leaked} layer(s) open; closed within each display's pass"
        );
    }
    debug_assert_eq!(
        leaked, 0,
        "a draw in the desktop scene left {leaked} layer(s) open (push_layer without pop_layer)"
    );
}

/// [`compose`] without the report: returns how many layers `logical` left
/// open, each of which every display's pass closed before its own clip. For
/// a scene that may stop inside layers by design (what a floating surface is
/// drawn over, cut where the surface begins) as well as for [`compose`].
#[must_use]
pub fn compose_closing_leaks(
    outputs: &[OutputGeometry],
    logical: &Scene,
    target: &mut Scene,
) -> u32 {
    let leaked = logical.encoding().n_open_clips;
    let append = |target: &mut Scene, transform: Option<Affine>| {
        target.append(logical, transform);
        for _ in 0..leaked {
            target.pop_layer();
        }
    };
    if is_identity(outputs) {
        append(target, None);
        return leaked;
    }
    for o in outputs {
        if o.scene.width() <= 0.0 || o.scene.height() <= 0.0 {
            continue;
        }
        target.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &o.scene,
        );
        append(target, Some(o.to_scene()));
        target.pop_layer();
    }
    leaked
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "every fixture coordinate is exactly representable"
)]
mod tests {
    use super::*;

    /// A 2x 2560x1600 panel at the origin and a 1x 1920x1080 monitor to its
    /// right, 200 logical rows lower — the `mixed-scale-offset` fixture.
    fn mixed() -> Vec<OutputGeometry> {
        vec![
            OutputGeometry {
                id: 0,
                monitor: String::new(),
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                scale: 2.0,
                scene: Rect::new(0.0, 0.0, 2560.0, 1600.0),
            },
            OutputGeometry {
                id: 1,
                monitor: String::new(),
                x: 1280.0,
                y: 200.0,
                width: 1920.0,
                height: 1080.0,
                scale: 1.0,
                scene: Rect::new(2560.0, 200.0, 4480.0, 1280.0),
            },
        ]
    }

    #[test]
    fn each_display_magnifies_by_its_own_scale() {
        let outputs = mixed();
        assert_eq!(
            point_to_scene(&outputs, Point::new(100.0, 40.0)),
            Point::new(200.0, 80.0)
        );
        assert_eq!(
            point_to_scene(&outputs, Point::new(1300.0, 240.0)),
            Point::new(2580.0, 240.0)
        );
    }

    #[test]
    fn a_band_across_both_displays_is_twice_as_tall_on_the_dense_one() {
        let outputs = mixed();
        let band = Rect::new(0.0, 200.0, 3200.0, 232.0);
        let on_panel = outputs[0]
            .to_scene()
            .transform_rect_bbox(band.intersect(outputs[0].rect()));
        let on_monitor = outputs[1]
            .to_scene()
            .transform_rect_bbox(band.intersect(outputs[1].rect()));
        assert_eq!(on_panel.height(), 64.0);
        assert_eq!(on_monitor.height(), 32.0);
    }

    #[test]
    fn a_rect_spanning_displays_maps_to_the_union_of_its_parts() {
        let outputs = mixed();
        let all = Rect::new(0.0, 0.0, 3200.0, 1280.0);
        assert_eq!(
            rect_to_scene(&outputs, all),
            Some(Rect::new(0.0, 0.0, 4480.0, 1600.0))
        );
        assert_eq!(scene_extent(&outputs), (4480.0, 1600.0));
        assert_eq!(logical_extent(&outputs), (3200.0, 1280.0));
    }

    #[test]
    fn unscaled_side_by_side_displays_need_no_transform() {
        let outputs = [
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
            OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 1024.0).with_id(1),
        ];
        assert!(is_identity(&outputs));
        assert!(!is_identity(&mixed()));
    }

    /// The logical desktop of [`mixed`] painted opaque, by a draw that
    /// opens a layer over the panel and never closes it.
    fn leaking_desktop() -> Scene {
        let mut logical = Scene::new();
        logical.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            vello::peniko::Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, 3200.0, 1280.0),
        );
        logical.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            0.5,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 400.0, 300.0),
        );
        logical.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            vello::peniko::Color::BLACK,
            None,
            &Rect::new(0.0, 0.0, 400.0, 300.0),
        );
        logical
    }

    /// The alpha of every pixel of `area` in `scene`, rasterized on the CPU
    /// into a target of `size` scene pixels.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "pixel offsets into a few-megapixel test target cannot overflow usize"
    )]
    fn alphas(scene: &Scene, size: (u32, u32), area: tilo_cpu_raster::Region) -> Vec<u8> {
        use tilo_cpu_raster::{CpuRasterizer, Options, Target};
        let (width, height) = size;
        let stride = width as usize * 4;
        let mut pixels = vec![0u8; stride * height as usize];
        {
            let mut target = Target::new(&mut pixels, width, height, stride).expect("target");
            CpuRasterizer::new(Options::default()).render(
                scene.encoding(),
                &mut target,
                Some(&[area]),
                vello::peniko::Color::TRANSPARENT,
            );
        }
        (area.y..area.y + area.height)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .map(|(x, y)| {
                let at = y as usize * stride + x as usize * 4 + 3;
                pixels.get(at).copied().expect("inside the target")
            })
            .collect()
    }

    /// A draw that leaks a layer on the dense panel no longer swallows the
    /// panel's clip: the 1x monitor after it still draws, and the leak is
    /// counted for the report.
    #[test]
    fn a_layer_a_draw_leaves_open_cannot_blank_the_next_display() {
        let outputs = mixed();
        let logical = leaking_desktop();
        assert_eq!(logical.encoding().n_open_clips, 1, "the fixture leaks");

        let mut scene = Scene::new();
        let leaked = compose_closing_leaks(&outputs, &logical, &mut scene);
        assert_eq!(leaked, 1, "the leak is reported");
        assert_eq!(
            scene.encoding().n_open_clips,
            0,
            "every display's pass closes what it opened"
        );

        let size = (4480, 1600);
        // The monitor drawn after the panel: opaque white across.
        let monitor = alphas(
            &scene,
            size,
            tilo_cpu_raster::Region::new(3500, 700, 16, 16),
        );
        assert!(
            monitor.iter().all(|alpha| *alpha == 255),
            "the 1x monitor after the leak still draws: {monitor:?}"
        );
        // And the panel itself, both inside and outside the leaked layer.
        for (x, y) in [(100, 100), (1500, 1000)] {
            let panel = alphas(&scene, size, tilo_cpu_raster::Region::new(x, y, 8, 8));
            assert!(
                panel.iter().all(|alpha| *alpha == 255),
                "the 2x panel draws at ({x}, {y}): {panel:?}"
            );
        }
    }

    /// Without the per-display close, the same leak blanks the monitor, so
    /// the test above observes the hardening rather than a scene that never
    /// needed it.
    #[test]
    fn without_closing_the_leak_the_next_display_is_blank() {
        let outputs = mixed();
        let logical = leaking_desktop();
        let mut scene = Scene::new();
        for o in &outputs {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &o.scene,
            );
            scene.append(&logical, Some(o.to_scene()));
            scene.pop_layer();
        }
        let monitor = alphas(
            &scene,
            (4480, 1600),
            tilo_cpu_raster::Region::new(3500, 700, 16, 16),
        );
        assert!(
            monitor.iter().all(|alpha| *alpha == 0),
            "a leaked layer swallows the panel's clip and blanks the monitor: {monitor:?}"
        );
    }

    /// Composing at scale 1 everywhere closes the leak too.
    #[test]
    fn an_unscaled_desktop_closes_a_leaked_layer() {
        let outputs = [OutputGeometry::unscaled(0.0, 0.0, 3200.0, 1280.0)];
        let mut scene = Scene::new();
        assert_eq!(
            compose_closing_leaks(&outputs, &leaking_desktop(), &mut scene),
            1
        );
        assert_eq!(scene.encoding().n_open_clips, 0);
    }

    /// A debug build fails on the leak instead of hiding it.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "left 1 layer(s) open")]
    fn a_leaked_layer_fails_a_debug_build() {
        let mut scene = Scene::new();
        compose(&mixed(), &leaking_desktop(), &mut scene);
    }

    #[test]
    fn a_point_in_a_gap_belongs_to_the_nearest_display() {
        let outputs = mixed();
        // Right of the panel, above the monitor's top edge.
        let o = output_at(&outputs, Point::new(1500.0, 50.0)).expect("some display");
        assert_eq!(o.x, 1280.0);
    }
}
