//! Anti-aliased coverage of mark outlines over the few cells they touch.

use crate::grid::{Area, at, float};

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
