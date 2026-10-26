//! The frame builder's scene arithmetic: damage regions without the ones
//! another contains, magnifying a scene drawn in logical coordinates into
//! scene pixels, whole-pixel pans and edges, and the timing report of a slow
//! scene build.

use super::{Affine, FineDamageRect, Rect, Scene};

/// `regions` without any region another one contains (the first of two
/// equal ones stays): the same pixels, rasterised once. A changed card's
/// old and new bounds and the blurred surface on it often nest.
pub(super) fn without_contained_regions(regions: &[FineDamageRect]) -> Vec<FineDamageRect> {
    let rect = |region: &FineDamageRect| {
        Rect::new(
            f64::from(region.x),
            f64::from(region.y),
            f64::from(region.x) + f64::from(region.width),
            f64::from(region.y) + f64::from(region.height),
        )
    };
    let contains = |outer: &FineDamageRect, inner: &FineDamageRect| {
        rect(outer).union(rect(inner)) == rect(outer)
    };
    regions
        .iter()
        .enumerate()
        .filter(|&(index, region)| {
            !regions.iter().enumerate().any(|(other, outer)| {
                other != index && contains(outer, region) && (outer != region || other < index)
            })
        })
        .map(|(_, region)| *region)
        .collect()
}

/// `scene`, drawn in logical coordinates, magnified by `to_scene` into
/// scene pixels without clipping it to the display: a strip's canvas runs
/// past the display's right edge, and the pan brings that part in.
/// [`magnified`] for a scene drawn with damage parts, which keep marking
/// what they marked, their bounds magnified with it.
pub(super) fn magnified_with_parts(
    scene: Scene,
    parts: Option<ui::scene_parts::SectionParts>,
    to_scene: Affine,
) -> (Scene, Option<ui::scene_parts::SectionParts>) {
    if to_scene == Affine::IDENTITY {
        return (scene, parts);
    }
    let Some(parts) = parts else {
        return (magnified(scene, to_scene), None);
    };
    let mut magnified = Scene::new();
    let ((), parts) = ui::scene_parts::record(&mut magnified, |magnified| {
        ui::scene_parts::append(magnified, &scene, Some(&parts), Some(to_scene));
    });
    (magnified, parts)
}

fn magnified(scene: Scene, to_scene: Affine) -> Scene {
    if to_scene == Affine::IDENTITY {
        return scene;
    }
    let mut magnified = Scene::new();
    magnified.append(&scene, Some(to_scene));
    magnified
}

/// One device pixel in layout pixels on a display at `scale` device pixels
/// per layout pixel: two thirds at 150 %, four fifths at 125 %, a half at
/// 200 %. `None` for a scale that is not finite and positive.
pub(super) fn pan_step(scale: f64) -> Option<f64> {
    (scale.is_finite() && scale > 0.0).then_some(1.0 / scale)
}

/// `value` is within a thousandth of a whole number.
pub(super) fn nearly_whole(value: f64) -> bool {
    value.is_finite() && (value - value.round()).abs() < 1e-3
}

/// A non-negative scene coordinate as whole pixels, rounded down.
pub(super) fn scene_px(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let value = value.min(f64::from(u32::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scene pixel edges are finite, non-negative, and clamped to u32"
    )]
    {
        value as u32
    }
}

/// Phase timings of one retained scene build, reported only when the build
/// is slow. A typical build is under a millisecond; the main-thread freezes
/// seen when a window opens or closes (`SLOW-ITER post_pump_paint` of
/// 240-490 ms) were not attributable to a phase, and this names it in the
/// session log (`SLOW-SCENE`).
pub(super) struct ScenePhases {
    started: std::time::Instant,
    last: std::time::Instant,
    spans: Vec<(&'static str, std::time::Duration)>,
}

impl ScenePhases {
    /// A scene build slower than this is reported: one frame at 60 Hz, past
    /// which the build alone misses the frame.
    const REPORT_AFTER: std::time::Duration = std::time::Duration::from_millis(16);
    /// At most one report this often, so a machine slow at every frame
    /// still logs a sample of them rather than every one.
    const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

    pub(super) fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: now,
            spans: Vec::with_capacity(8),
        }
    }

    pub(super) fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        let took = now.duration_since(self.last);
        crate::compositor::metrics::profile::record_phase(phase, took, None);
        self.spans.push((phase, took));
        self.last = now;
    }

    pub(super) fn report(&self) {
        thread_local! {
            static LAST_REPORT: std::cell::Cell<Option<std::time::Instant>> =
                const { std::cell::Cell::new(None) };
        }
        let sections = crate::shell::overlay_host::take_section_spans();
        let total = self.started.elapsed();
        if total < Self::REPORT_AFTER
            || LAST_REPORT
                .with(std::cell::Cell::get)
                .is_some_and(|last| last.elapsed() < Self::REPORT_EVERY)
        {
            return;
        }
        LAST_REPORT.with(|last| last.set(Some(std::time::Instant::now())));
        // The section draws are inside `scene/draw-sections`; named apart,
        // the line says which part of the desktop the time went to.
        let split = self
            .spans
            .iter()
            .chain(sections.iter())
            .map(|(phase, took)| format!("{phase}={took:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("SLOW-SCENE total={total:?} {split}");
    }
}
