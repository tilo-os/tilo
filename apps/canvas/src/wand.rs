//! The magic wand's eye: which part of a picture is like the colour it
//! starts from, as a coverage of each pixel.
//!
//! Colours are compared in Oklab, where equal steps look equally different,
//! and through a light blur, so a textured fill (the ground's tooth, a dry
//! brush) reads as the one colour it is. Spreading from the start also
//! stops at an edge: a step in colour much sharper than the paint's own
//! texture around the start. So a smooth gradient is followed only as far
//! as the tolerance reaches, and a crisp outline stops the wand even where
//! the colours either side of it are alike.
//!
//! The answer is a coverage, not yes or no: the pixels found are whole, and
//! each pixel along their edge is covered as far as its colour lies between
//! the found colour and the one beyond, so an anti-aliased edge is traced
//! where it really runs, between pixels.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "pixel indices are bounded by the picture's size, checked once at the start; the arithmetic is float colour maths"
)]

/// How far apart, in Oklab, colours a tolerance of 1 still finds alike.
const REACH: f32 = 0.8;
/// The faintest step in colour that is an edge, in Oklab.
const EDGE_FLOOR: f32 = 0.006;
/// How much sharper than the texture around the start a step must be to be
/// an edge.
const EDGE_OVER_TEXTURE: f32 = 3.0; // a ratio of colour steps, not on-screen spacing

/// A picture, row by row, four bytes a pixel.
pub struct Picture<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
}

/// What the wand looks for.
pub struct Wand<'a> {
    /// The pixel it starts from.
    pub seed: Option<(usize, usize)>,
    /// Or the colour it looks for, `[r, g, b]`.
    pub colour: Option<[u8; 3]>,
    /// `0..=1`.
    pub tolerance: f64,
    /// Like colour anywhere, touching the start or not.
    pub everywhere: bool,
    /// The pixels it may find, row by row; all when absent. Its edge is
    /// traced a pixel past them, to be cut exactly where they end.
    pub allowed: Option<&'a [bool]>,
    /// The picture's pixels per artwork unit: how far the paint's texture
    /// is blurred away.
    pub pixels_per_unit: f64,
}

/// A colour in Oklab and its alpha.
type Lab = [f32; 4];

/// How much of each pixel of `picture` is like the colour `wand` looks
/// for, `0..=1`, row by row.
pub fn coverage(picture: &Picture, wand: &Wand) -> Vec<f32> {
    let (width, height) = (picture.width, picture.height);
    let count = width * height;
    if count == 0 || picture.rgba.len() < count * 4 {
        return Vec::new();
    }
    let linear = srgb_to_linear();
    let lab: Vec<Lab> = picture
        .rgba
        .chunks_exact(4)
        .take(count)
        .map(|pixel| oklab(&linear, [pixel[0], pixel[1], pixel[2], pixel[3]]))
        .collect();
    let radius = blur_radius(wand.pixels_per_unit);
    let soft = box_blur(&lab, (width, height), radius);
    let target = match (wand.colour, wand.seed) {
        (Some([r, g, b]), _) => oklab(&linear, [r, g, b, 255]),
        (None, Some((x, y))) if x < width && y < height => soft[y * width + x],
        _ => return vec![0.0; count],
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a tolerance of 0 to 1 fits an f32"
    )]
    let reach = wand.tolerance.clamp(0.0, 1.0) as f32 * REACH;
    let near: Vec<f32> = soft.iter().map(|colour| apart(colour, &target)).collect();
    let allowed = |index: usize| {
        wand.allowed
            .is_none_or(|allowed| allowed.get(index).copied().unwrap_or(false))
    };
    let mut found = vec![false; count];
    match wand.seed.filter(|_| !wand.everywhere) {
        Some((x, y)) if x < width && y < height => {
            let edges = edge_strength(&soft, (width, height), radius);
            let limit = edge_limit(&edges, &near, reach, (x, y), (width, height), radius);
            // A start on an edge itself is followed by colour alone.
            let stop_at_edges = edges[y * width + x] <= limit;
            flood(&mut found, (width, height), (x, y), |index| {
                allowed(index) && near[index] <= reach && (!stop_at_edges || edges[index] <= limit)
            });
            if stop_at_edges {
                // An edge is seen through the blur a little before it is
                // reached: take the pixels up to it that are still like the
                // found one beside them, not yet across the step.
                let step = limit * 0.5;
                let alike = |index: usize, beside: usize| {
                    allowed(index)
                        && apart(&lab[index], &target) <= reach
                        && apart(&lab[index], &lab[beside]) <= step
                };
                for _ in 0..=radius {
                    grow(&mut found, (width, height), alike);
                }
            }
        }
        _ => {
            for (index, cell) in found.iter_mut().enumerate() {
                *cell = allowed(index) && near[index] <= reach;
            }
        }
    }
    let mut field = vec![0.0; count];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            // A pixel it may not find is still covered as its colour says:
            // the area runs on past the limit, for the caller to cut it
            // exactly there rather than half a pixel short.
            field[index] = if found[index] {
                1.0
            } else {
                edge_cover(&found, &lab, (width, height), (x, y), (&target, reach))
            };
        }
    }
    field
}

/// How much of a pixel beside the found ones is covered: where its colour
/// lies between theirs and that of the paint beyond it, so a pixel half
/// across an anti-aliased edge is half covered. Where no edge runs, as in
/// a gradient, it is covered as far as its colour is still within `reach`
/// of the `target` the wand looks for.
fn edge_cover(
    found: &[bool],
    lab: &[Lab],
    (width, height): (usize, usize),
    (x, y): (usize, usize),
    (target, reach): (&Lab, f32),
) -> f32 {
    let own = lab[y * width + x];
    let mut inner = [0.0f32; 4];
    let mut count = 0.0f32;
    for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
        for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
            let index = ny * width + nx;
            if found[index] {
                for (sum, channel) in inner.iter_mut().zip(lab[index]) {
                    *sum += channel;
                }
                count += 1.0;
            }
        }
    }
    if count == 0.0 {
        return 0.0;
    }
    let inner = inner.map(|sum| sum / count);
    // The paint beyond: the colour least like the found one, two pixels
    // around, past the edge's own blend.
    let mut outer = own;
    let mut farthest = apart(&own, &inner);
    for ny in y.saturating_sub(2)..=(y + 2).min(height - 1) {
        for nx in x.saturating_sub(2)..=(x + 2).min(width - 1) {
            let index = ny * width + nx;
            let away = apart(&lab[index], &inner);
            if !found[index] && away > farthest {
                farthest = away;
                outer = lab[index];
            }
        }
    }
    if farthest < EDGE_FLOOR {
        // No edge: the tolerance's own line, between the found colour and
        // this one.
        let (inside, here) = (apart(&inner, target), apart(&own, target));
        let change = (here - inside).abs().max(1e-6);
        return (0.5 + (reach - here) / (2.0 * change)).clamp(0.0, 1.0);
    }
    let across: [f32; 4] = std::array::from_fn(|channel| outer[channel] - inner[channel]);
    let length = across.iter().map(|value| value * value).sum::<f32>();
    let along = (0..4)
        .map(|channel| (own[channel] - inner[channel]) * across[channel])
        .sum::<f32>()
        / length;
    (1.0 - along).clamp(0.0, 1.0)
}

/// The blur that hides the paint's texture: about half a unit, at least a
/// pixel and at most three.
fn blur_radius(pixels_per_unit: f64) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=3 first"
    )]
    let radius = (pixels_per_unit * 0.6).round().clamp(1.0, 3.0) as usize;
    radius
}

/// How far apart two colours look.
fn apart(a: &Lab, b: &Lab) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f32>()
        .sqrt()
}

fn srgb_to_linear() -> [f32; 256] {
    let mut table = [0.0; 256];
    for (byte, slot) in table.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "a byte is exact in an f32")]
        let value = byte as f32 / 255.0;
        *slot = if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        };
    }
    table
}

/// An sRGB pixel in Oklab, with its alpha.
fn oklab(linear: &[f32; 256], [red, green, blue, alpha]: [u8; 4]) -> Lab {
    let (red, green, blue) = (
        linear[usize::from(red)],
        linear[usize::from(green)],
        linear[usize::from(blue)],
    );
    let long = (0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_995 * blue).cbrt();
    let medium = (0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue).cbrt();
    let short = (0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue).cbrt();
    [
        0.210_454_26 * long + 0.793_617_8 * medium - 0.004_072_047 * short,
        1.977_998_5 * long - 2.428_592_2 * medium + 0.450_593_7 * short,
        0.025_904_037 * long + 0.782_771_77 * medium - 0.808_675_77 * short,
        f32::from(alpha) / 255.0,
    ]
}

/// `colours` blurred by a box `radius` pixels either side, edges held.
fn box_blur(colours: &[Lab], (width, height): (usize, usize), radius: usize) -> Vec<Lab> {
    if radius == 0 {
        return colours.to_vec();
    }
    let along =
        |colours: &[Lab], length: usize, lines: usize, at: &dyn Fn(usize, usize) -> usize| {
            let mut out = vec![[0.0; 4]; colours.len()];
            #[allow(
                clippy::cast_precision_loss,
                reason = "a blur a few pixels wide is exact in an f32"
            )]
            let span = (2 * radius + 1) as f32;
            for line in 0..lines {
                let pick = |step: isize| {
                    let step = usize::try_from(step.max(0)).unwrap_or(0).min(length - 1);
                    colours[at(line, step)]
                };
                let mut sum = [0.0f32; 4];
                let reach = isize::try_from(radius).unwrap_or(0);
                for step in -reach..=reach {
                    let colour = pick(step);
                    for (total, channel) in sum.iter_mut().zip(colour) {
                        *total += channel;
                    }
                }
                for step in 0..length {
                    let index = at(line, step);
                    out[index] = sum.map(|total| total / span);
                    let signed = isize::try_from(step).unwrap_or(0);
                    let (leaving, coming) = (pick(signed - reach), pick(signed + reach + 1));
                    for ((total, gone), new) in sum.iter_mut().zip(leaving).zip(coming) {
                        *total += new - gone;
                    }
                }
            }
            out
        };
    let rows = along(colours, width, height, &|row, column| row * width + column);
    along(&rows, height, width, &|column, row| row * width + column)
}

/// How sharp a step in colour each pixel sits on, as the size of the step
/// (a Sobel gradient undone of the blur that spread it).
fn edge_strength(soft: &[Lab], (width, height): (usize, usize), radius: usize) -> Vec<f32> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a blur a few pixels wide is exact in an f32"
    )]
    let spread = (2 * radius + 1) as f32 / 8.0;
    let mut edges = vec![0.0; soft.len()];
    for y in 0..height {
        let (up, down) = (y.saturating_sub(1), (y + 1).min(height - 1));
        for x in 0..width {
            let (left, right) = (x.saturating_sub(1), (x + 1).min(width - 1));
            let at = |x: usize, y: usize| &soft[y * width + x];
            let mut sum = 0.0;
            for channel in 0..4 {
                let c = |x: usize, y: usize| at(x, y)[channel];
                let gx = c(right, up) + 2.0 * c(right, y) + c(right, down)
                    - c(left, up)
                    - 2.0 * c(left, y)
                    - c(left, down);
                let gy = c(left, down) + 2.0 * c(x, down) + c(right, down)
                    - c(left, up)
                    - 2.0 * c(x, up)
                    - c(right, up);
                sum += gx * gx + gy * gy;
            }
            edges[y * width + x] = sum.sqrt() * spread;
        }
    }
    edges
}

/// How sharp a step must be to stop the wand: well above the texture of
/// the paint like the start, around it.
fn edge_limit(
    edges: &[f32],
    near: &[f32],
    reach: f32,
    (x, y): (usize, usize),
    (width, height): (usize, usize),
    radius: usize,
) -> f32 {
    let around = (radius * 6).max(8);
    let mut texture: Vec<f32> = Vec::new();
    for ny in y.saturating_sub(around)..=(y + around).min(height - 1) {
        for nx in x.saturating_sub(around)..=(x + around).min(width - 1) {
            let index = ny * width + nx;
            if near[index] <= reach {
                texture.push(edges[index]);
            }
        }
    }
    if texture.is_empty() {
        return EDGE_FLOOR;
    }
    let at = (texture.len() * 4 / 5).min(texture.len() - 1);
    let (_, typical, _) = texture.select_nth_unstable_by(at, f32::total_cmp);
    (*typical * EDGE_OVER_TEXTURE).max(EDGE_FLOOR)
}

/// Add to `found` every cell that touches it, side by side, and that
/// `alike` says is like the found cell it touches.
fn grow(found: &mut [bool], (width, height): (usize, usize), alike: impl Fn(usize, usize) -> bool) {
    let before = found.to_vec();
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if before[index] {
                continue;
            }
            let beside = [
                (x > 0).then(|| index - 1),
                (x + 1 < width).then(|| index + 1),
                (y > 0).then(|| index - width),
                (y + 1 < height).then(|| index + width),
            ];
            if beside
                .into_iter()
                .flatten()
                .any(|other| before[other] && alike(index, other))
            {
                found[index] = true;
            }
        }
    }
}

/// Set every cell reachable from `start` through cells `inside` says are
/// in, one row's run at a time.
pub fn flood(
    cells: &mut [bool],
    (width, height): (usize, usize),
    start: (usize, usize),
    inside: impl Fn(usize) -> bool,
) {
    let mut seen = vec![false; cells.len()];
    let mut stack = vec![start];
    while let Some((x, y)) = stack.pop() {
        let at = y * width + x;
        if seen.get(at).copied().unwrap_or(true) || !inside(at) {
            continue;
        }
        // The run of the row through (x, y).
        let mut left = x;
        while left > 0 && !seen[at - (x - left) - 1] && inside(at - (x - left) - 1) {
            left -= 1;
        }
        let mut right = x;
        while right + 1 < width && !seen[at + (right - x) + 1] && inside(at + (right - x) + 1) {
            right += 1;
        }
        for column in left..=right {
            let cell = y * width + column;
            seen[cell] = true;
            cells[cell] = true;
            if y > 0 {
                stack.push((column, y - 1));
            }
            if y + 1 < height {
                stack.push((column, y + 1));
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "test pictures are small and their colours lie in 0..=255"
)]
mod tests {
    use super::*;
    use crate::region::{Coverage, Region};
    use ui::vello::kurbo::{Affine, Point};

    /// A picture `width` by `height` painted by `paint(x, y)`.
    fn picture(width: usize, height: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = paint(x, y);
                rgba.extend([r, g, b, 255]);
            }
        }
        rgba
    }

    fn found(rgba: &[u8], (width, height): (usize, usize), wand: &Wand) -> Region {
        let picture = Picture {
            rgba,
            width,
            height,
        };
        let field = coverage(&picture, wand);
        Coverage {
            width,
            height,
            cells: &field,
        }
        .traced(Affine::IDENTITY)
    }

    fn at(seed: (usize, usize)) -> Wand<'static> {
        Wand {
            seed: Some(seed),
            colour: None,
            tolerance: 0.15,
            everywhere: false,
            allowed: None,
            pixels_per_unit: 1.0,
        }
    }

    /// A cheap repeatable noise, `0..1`.
    fn noise(x: usize, y: usize) -> f64 {
        let n = (x as u64).wrapping_mul(73_856_093) ^ (y as u64).wrapping_mul(19_349_663);
        let n = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40;
        (n % 1000) as f64 / 1000.0
    }

    #[test]
    fn a_flood_stays_inside_its_colour() {
        // Two columns of in-cells joined at the bottom, and an island.
        let (width, height) = (7, 5);
        let inside = |index: usize| {
            let (x, y) = (index % width, index / width);
            x == 1 || x == 3 || y == 4 || (x == 6 && y == 0)
        };
        let mut cells = vec![false; width * height];
        flood(&mut cells, (width, height), (1, 0), inside);
        assert!(cells[3], "the other column joins through the bottom row");
        assert!(cells[4 * width + 6], "the bottom row is reached");
        assert!(!cells[6], "the island is not");
        assert!(!cells[width], "nor a cell between the columns");
    }

    #[test]
    fn a_gradient_does_not_carry_the_wand_over_a_crisp_edge() {
        // A sky fading slowly from blue to a paler blue, over a hill whose
        // green-blue is within the tolerance of the sky above it but meets
        // it in a crisp line.
        let (width, height) = (120, 100);
        let rgba = picture(width, height, |_, y| {
            if y < 60 {
                let t = y as f64 / 60.0;
                [(90.0 + 30.0 * t) as u8, (140.0 + 20.0 * t) as u8, 210]
            } else {
                [112, 168, 196]
            }
        });
        let region = found(&rgba, (width, height), &at((60, 10)));
        assert!(
            region.contains(Point::new(60.0, 50.0)),
            "the sky fills down to the hill"
        );
        assert!(
            !region.contains(Point::new(60.0, 75.0)),
            "the hill is not the sky"
        );
        let bounds = region.bounds().unwrap_or_default();
        assert!(
            (bounds.y1 - 60.0).abs() < 1.5,
            "stops at the hill's line: {bounds:?}"
        );
    }

    #[test]
    fn a_faint_but_crisp_edge_stops_the_wand() {
        // A misty hill against the pale foot of a sky, a step a person
        // barely sees but does see, its slanting edge anti-aliased. From
        // the hill the wand takes the hill, not the sky.
        let (width, height) = (160, 120);
        let sky = |y: f64| {
            let t = y / 140.0;
            [58.0 + 106.0 * t, 111.0 + 87.0 * t, 196.0 + 32.0 * t]
        };
        let rgba = picture(width, height, |x, y| {
            let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
            let hill = (y - (60.0 + 0.2 * x) + 0.5).clamp(0.0, 1.0);
            let above = sky(y);
            let below = [147.0, 185.0, 214.0];
            let mix = |channel: usize| {
                (above[channel] * (1.0 - hill) + below[channel] * hill).round() as u8
            };
            [mix(0), mix(1), mix(2)]
        });
        let region = found(&rgba, (width, height), &at((80, 110)));
        assert!(region.contains(Point::new(80.0, 100.0)));
        assert!(!region.contains(Point::new(80.0, 60.0)), "the sky is left");
        assert!(!region.contains(Point::new(150.0, 20.0)), "the sky is left");
    }

    #[test]
    fn a_gradient_is_followed_only_as_far_as_the_tolerance() {
        // Black to white across 200 pixels: the wand at the dark end takes
        // the dark part, not the whole ramp.
        let (width, height) = (200, 20);
        let rgba = picture(width, height, |x, _| {
            let v = (x as f64 / 199.0 * 255.0) as u8;
            [v, v, v]
        });
        let region = found(&rgba, (width, height), &at((5, 10)));
        let bounds = region.bounds().unwrap_or_default();
        assert!(bounds.x1 > 10.0 && bounds.x1 < 120.0, "{bounds:?}");
    }

    #[test]
    fn a_textured_fill_is_found_whole() {
        // A red disc painted with a rough grain on a light ground.
        let (width, height) = (100, 100);
        let rgba = picture(width, height, |x, y| {
            let inside = (x as f64 + 0.5 - 50.0).hypot(y as f64 + 0.5 - 50.0) <= 35.0;
            let grain = (noise(x, y) - 0.5) * 70.0;
            if inside {
                [
                    (190.0 + grain) as u8,
                    (50.0 + grain * 0.6) as u8,
                    (40.0 + grain * 0.5) as u8,
                ]
            } else {
                [244, 239, 227]
            }
        });
        let region = found(&rgba, (width, height), &at((50, 50)));
        let disc = std::f64::consts::PI * 35.0 * 35.0;
        assert!(
            (region.area() - disc).abs() / disc < 0.05,
            "{} against {disc}",
            region.area()
        );
        assert!(!region.contains(Point::new(5.0, 5.0)));
    }

    #[test]
    fn an_anti_aliased_edge_is_traced_between_pixels() {
        // A dark half-plane whose edge runs at x = 30.3, anti-aliased.
        let (width, height) = (60, 40);
        let edge = 30.3;
        let rgba = picture(width, height, |x, _| {
            let cover = (edge - x as f64).clamp(0.0, 1.0);
            let v = (20.0 * cover + 230.0 * (1.0 - cover)).round() as u8;
            [v, v, v]
        });
        let region = found(&rgba, (width, height), &at((10, 20)));
        let bounds = region.bounds().unwrap_or_default();
        assert!(
            (bounds.x1 - edge).abs() < 0.15,
            "edge at {} for {edge}",
            bounds.x1
        );
    }

    #[test]
    fn the_wand_stays_in_what_it_may_find() {
        // Two blue squares joined by a blue band; only the left square and
        // the band are allowed (what is selected, when the wand subtracts).
        let (width, height) = (90, 40);
        let blue = |x: usize, y: usize| !(30..60).contains(&x) || (15..25).contains(&y);
        let rgba = picture(width, height, |x, y| {
            if blue(x, y) {
                [40, 80, 200]
            } else {
                [250, 250, 250]
            }
        });
        let allowed: Vec<bool> = (0..width * height)
            .map(|index| index % width < 30)
            .collect();
        let wand = Wand {
            allowed: Some(&allowed),
            ..at((10, 10))
        };
        let region = found(&rgba, (width, height), &wand);
        assert!(region.contains(Point::new(10.0, 30.0)));
        assert!(
            !region.contains(Point::new(75.0, 10.0)),
            "the right square is left alone"
        );
    }
}
