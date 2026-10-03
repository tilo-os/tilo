//! Cell grids and the few conversions between continuous canvas positions and
//! cells. Every simulation field is a row-major `Vec` of one value per cell.

/// A float to a cell index, clamped to `0..len`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to the non-negative grid range before the cast"
)]
pub(crate) fn cell(value: f32, len: usize) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let max = len.saturating_sub(1);
    (value as usize).min(max)
}

/// A cell count or index as a float.
#[allow(
    clippy::cast_precision_loss,
    reason = "grid dimensions are far below the 2^24 limit of exact f32 integers"
)]
pub(crate) fn float(value: usize) -> f32 {
    value as f32
}

/// An index as hash input.
pub(crate) fn key(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// A seed-mixing integer as a unit float in `0..1`.
#[allow(
    clippy::cast_precision_loss,
    reason = "only the top 24 bits are kept, which f32 represents exactly"
)]
pub(crate) fn unit(bits: u64) -> f32 {
    ((bits >> 40) as f32) / 16_777_216.0
}

/// An inclusive-exclusive rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Area {
    pub(crate) const EMPTY: Self = Self {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    /// The cells covering `[min, max]` grown by `margin`, inside a
    /// `width` by `height` grid.
    pub(crate) fn around(
        min: (f32, f32),
        max: (f32, f32),
        margin: f32,
        width: usize,
        height: usize,
    ) -> Self {
        if !(min.0.is_finite() && min.1.is_finite() && max.0.is_finite() && max.1.is_finite()) {
            return Self::EMPTY;
        }
        let x0 = cell((min.0 - margin).floor(), width);
        let y0 = cell((min.1 - margin).floor(), height);
        let x1 = cell((max.0 + margin).ceil(), width).saturating_add(1);
        let y1 = cell((max.1 + margin).ceil(), height).saturating_add(1);
        if max.0 + margin < 0.0 || max.1 + margin < 0.0 {
            return Self::EMPTY;
        }
        Self {
            x0,
            y0,
            x1: x1.min(width),
            y1: y1.min(height),
        }
    }

    pub(crate) fn width(self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    pub(crate) fn height(self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    pub(crate) fn is_empty(self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// The layer cell at a row-major `index` into this area's cells.
    pub(crate) fn position(self, index: usize) -> (usize, usize) {
        let span = self.width().max(1);
        let column = index.checked_rem(span).unwrap_or(0);
        let row = index.checked_div(span).unwrap_or(0);
        (self.x0.saturating_add(column), self.y0.saturating_add(row))
    }

    pub(crate) fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// A copy of the cells of `area` from a `width`-wide field, row-major.
pub(crate) fn extract(field: &[f32], width: usize, area: Area) -> Vec<f32> {
    let mut local = Vec::with_capacity(area.width().saturating_mul(area.height()));
    for y in area.y0..area.y1 {
        let start = at(width, area.x0, y);
        let end = at(width, area.x1, y);
        local.extend(field.get(start..end).unwrap_or_default());
    }
    local
}

/// Write a local copy made by [`extract`] back into its field.
pub(crate) fn store(field: &mut [f32], width: usize, area: Area, local: &[f32]) {
    let span = area.width();
    if span == 0 {
        return;
    }
    for (row, values) in (area.y0..area.y1).zip(local.chunks_exact(span)) {
        let start = at(width, area.x0, row);
        let end = at(width, area.x1, row);
        if let Some(slots) = field.get_mut(start..end) {
            slots.copy_from_slice(values);
        }
    }
}

/// The row-major index of a cell.
pub(crate) fn at(width: usize, x: usize, y: usize) -> usize {
    y.saturating_mul(width).saturating_add(x)
}

/// A field value, or `default` outside the grid.
pub(crate) fn read<T: Copy>(field: &[T], width: usize, x: usize, y: usize, default: T) -> T {
    if x >= width {
        return default;
    }
    field.get(at(width, x, y)).copied().unwrap_or(default)
}

/// Box-blur a `width` by `height` grid with the given radius in cells. Each
/// cell averages only the cells inside the grid, so blurring a local copy of
/// a larger field does not drain value at the copy's edge. Linear in the grid
/// size, whatever the radius.
pub(crate) fn box_blur(field: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    if radius == 0 || width == 0 || height == 0 {
        return field.to_vec();
    }
    let mut horizontal = vec![0.0_f32; field.len()];
    for (row, out) in field
        .chunks_exact(width)
        .zip(horizontal.chunks_exact_mut(width))
    {
        blur_line(row.iter().copied(), out.iter_mut(), radius);
    }
    let mut result = vec![0.0_f32; field.len()];
    for x in 0..width {
        let column = (0..height).map(|y| read(&horizontal, width, x, y, 0.0));
        let mut slots: Vec<f32> = vec![0.0; height];
        blur_line(column, slots.iter_mut(), radius);
        for (y, value) in slots.into_iter().enumerate() {
            if let Some(slot) = result.get_mut(at(width, x, y)) {
                *slot = value;
            }
        }
    }
    result
}

/// One pass of a running-sum box blur along a line, averaging the cells of
/// the window that lie on the line.
fn blur_line<'a>(
    line: impl Iterator<Item = f32> + Clone,
    out: impl Iterator<Item = &'a mut f32>,
    radius: usize,
) {
    let values: Vec<f32> = line.collect();
    let last = values.len().saturating_sub(1);
    let mut sum: f32 = values.iter().take(radius).sum();
    for (index, slot) in out.enumerate() {
        if let Some(entering) = values.get(index.saturating_add(radius)) {
            sum += entering;
        }
        let first = index.saturating_sub(radius);
        let end = index.saturating_add(radius).min(last);
        let count = end.saturating_sub(first).saturating_add(1);
        *slot = sum / float(count);
        if index >= radius
            && let Some(leaving) = values.get(index.saturating_sub(radius))
        {
            sum -= leaving;
        }
    }
}

/// Hermite smoothstep of `value` between `edge0` and `edge1`.
pub(crate) fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let span = edge1 - edge0;
    if span.abs() < f32::EPSILON {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / span).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
