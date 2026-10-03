//! Anti-aliased coverage of mark outlines over the few cells they touch.

use crate::grid::{Area, at, box_blur, cell, float};

/// Coverage `0..=1` over `area` of a layer grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Coverage {
    pub area: Area,
    pub values: Vec<f32>,
}

impl Coverage {
    /// Coverage at a layer cell; zero outside the area.
    pub(crate) fn get(&self, x: usize, y: usize) -> f32 {
        if x < self.area.x0 || y < self.area.y0 || x >= self.area.x1 || y >= self.area.y1 {
            return 0.0;
        }
        let local = at(
            self.area.width(),
            x.saturating_sub(self.area.x0),
            y.saturating_sub(self.area.y0),
        );
        self.values.get(local).copied().unwrap_or(0.0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.area.is_empty() || self.values.iter().all(|value| *value <= 0.0)
    }
}

/// Fill closed `contours`, in cell coordinates, inside a `width` by `height`
/// grid. Overlapping contours follow the non-zero rule, so a contour wound
/// the other way cuts a hole only with `even_odd`.
pub(crate) fn fill(
    contours: &[Vec<(f32, f32)>],
    width: usize,
    height: usize,
    margin: f32,
    even_odd: bool,
) -> Coverage {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &(x, y) in contours.iter().flatten() {
        if x.is_finite() && y.is_finite() {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
    }
    let area = Area::around(min, max, margin.max(1.0), width, height);
    let empty = Coverage {
        area,
        values: vec![0.0; area.width().saturating_mul(area.height())],
    };
    if area.is_empty() {
        return empty;
    }
    let mut builder = tiny_skia::PathBuilder::new();
    let ox = float(area.x0);
    let oy = float(area.y0);
    for contour in contours {
        let mut points = contour
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite());
        let Some(&(x, y)) = points.next() else {
            continue;
        };
        builder.move_to(x - ox, y - oy);
        for &(x, y) in points {
            builder.line_to(x - ox, y - oy);
        }
        builder.close();
    }
    let Some(path) = builder.finish() else {
        return empty;
    };
    let (Ok(mask_width), Ok(mask_height)) =
        (u32::try_from(area.width()), u32::try_from(area.height()))
    else {
        return empty;
    };
    let Some(mut mask) = tiny_skia::Mask::new(mask_width, mask_height) else {
        return empty;
    };
    let rule = if even_odd {
        tiny_skia::FillRule::EvenOdd
    } else {
        tiny_skia::FillRule::Winding
    };
    mask.fill_path(&path, rule, true, tiny_skia::Transform::identity());
    Coverage {
        area,
        values: mask
            .data()
            .iter()
            .map(|value| f32::from(*value) / 255.0)
            .collect(),
    }
}

/// The footprint of a round brush dragged along `samples`: the union of its
/// tip at each sample, width following pressure and taper. Unlike one
/// outline around the whole stroke it cannot fold over itself where a wide
/// brush turns tightly.
/// `taper_floor` is how wide the brush is where it lands and lifts, as a
/// share of its full width: low for a pointed brush, near one for a loaded
/// wash brush that lands flat.
pub(crate) fn brush_footprint(
    samples: &[crate::brush::Sample],
    size: f32,
    taper_floor: f32,
    grid: (usize, usize),
    margin: f32,
) -> Coverage {
    let (width, height) = grid;
    let total = crate::brush::length(samples);
    let floor = taper_floor.clamp(0.0, 1.0);
    let radius = |sample: &crate::brush::Sample| {
        // `taper` eases from 0.35 to 1; stretch that onto floor..=1.
        let eased = (crate::brush::taper(sample.distance, total, size) - 0.35) / 0.65;
        (size * 0.5 * (0.4 + 0.6 * sample.pressure) * (floor + (1.0 - floor) * eased)).max(0.5)
    };
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for sample in samples {
        let r = radius(sample);
        min = (min.0.min(sample.x - r), min.1.min(sample.y - r));
        max = (max.0.max(sample.x + r), max.1.max(sample.y + r));
    }
    let area = Area::around(min, max, margin.max(1.0), width, height);
    let empty = Coverage {
        area,
        values: vec![0.0; area.width().saturating_mul(area.height())],
    };
    if area.is_empty() {
        return empty;
    }
    let (ox, oy) = (float(area.x0), float(area.y0));
    let mut builder = tiny_skia::PathBuilder::new();
    let mut last: Option<(f32, f32)> = None;
    for sample in samples {
        let r = radius(sample);
        // A dab every third of a radius covers the stroke without gaps.
        if let Some((x, y)) = last
            && (sample.x - x).hypot(sample.y - y) < r / 3.0
        {
            continue;
        }
        builder.push_circle(sample.x - ox, sample.y - oy, r);
        last = Some((sample.x, sample.y));
    }
    if let Some(end) = samples.last() {
        builder.push_circle(end.x - ox, end.y - oy, radius(end));
    }
    let (Some(path), Ok(mask_width), Ok(mask_height)) = (
        builder.finish(),
        u32::try_from(area.width()),
        u32::try_from(area.height()),
    ) else {
        return empty;
    };
    let Some(mut mask) = tiny_skia::Mask::new(mask_width, mask_height) else {
        return empty;
    };
    mask.fill_path(
        &path,
        tiny_skia::FillRule::Winding,
        true,
        tiny_skia::Transform::identity(),
    );
    Coverage {
        area,
        values: mask
            .data()
            .iter()
            .map(|value| f32::from(*value) / 255.0)
            .collect(),
    }
}

/// How much of the paint an eraser takes off each cell, `0..=1`: its
/// footprint, softened by its hardness, cut to its clip and scaled by its
/// strength.
pub(crate) fn erase_coverage(erase: &crate::engine::Erase, grid: (usize, usize)) -> Coverage {
    let samples = crate::brush::smooth(&erase.points, 0.5);
    let size = erase.size.max(0.5);
    let softness = 1.0 - erase.hardness.clamp(0.0, 1.0);
    // A soft eraser fades over the outer half of its radius: the footprint
    // shrinks by a quarter of that span and a box blur of the same reach
    // ramps from the half radius out to the full one.
    let reach = softness * size * 0.125;
    let radius = cell(reach, 256);
    let mut coverage = brush_footprint(&samples, size - reach * 2.0, 1.0, grid, reach + 2.0);
    let area = coverage.area;
    if radius > 0 {
        coverage.values = box_blur(&coverage.values, area.width(), area.height(), radius);
    }
    let clip = (!erase.clip.is_empty()).then(|| fill(&erase.clip, grid.0, grid.1, 1.0, false));
    let strength = erase.strength.clamp(0.0, 1.0);
    for (local, value) in coverage.values.iter_mut().enumerate() {
        let (x, y) = area.position(local);
        let inside = clip.as_ref().map_or(1.0, |clip| clip.get(x, y));
        *value = (*value * inside * strength).clamp(0.0, 1.0);
    }
    coverage
}

/// Scale a `width`-wide layer field by `1 - coverage` where the coverage
/// lies: what an eraser leaves of it.
pub(crate) fn lift(field: &mut [f32], width: usize, coverage: &Coverage) {
    let area = coverage.area;
    for (local, taken) in coverage.values.iter().enumerate() {
        let (x, y) = area.position(local);
        if let Some(value) = field.get_mut(at(width, x, y)) {
            *value *= 1.0 - taken;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_covers_its_cells_and_nothing_else() {
        let square = vec![(10.0, 10.0), (20.0, 10.0), (20.0, 20.0), (10.0, 20.0)];
        let coverage = fill(&[square], 64, 64, 1.0, false);
        assert!((coverage.get(15, 15) - 1.0).abs() < 1e-6);
        assert!(coverage.get(5, 5).abs() < 1e-6);
        assert!(coverage.get(25, 15).abs() < 1e-6);
        let total: f32 = coverage.values.iter().sum();
        assert!((total - 100.0).abs() < 1.0, "area {total}");

        let outside = vec![(-50.0, -50.0), (-40.0, -50.0), (-40.0, -40.0)];
        assert!(fill(&[outside], 64, 64, 1.0, false).is_empty());
    }
}
