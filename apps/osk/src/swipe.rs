//! Swipe-to-type decoder (W9a).
//!
//! Given a gesture path traced across the keyboard, return ranked candidate
//! words in the keyboard's language. This is the classic
//! **SHARK²/gesture-typing** approach:
//!
//! 1. Map each letter to the centre of its key (from the live
//!    [`crate::layout::key_rects`] geometry — one source of truth with what
//!    is drawn).
//! 2. For each candidate word, build its *ideal* polyline through its
//!    letters' key centres.
//! 3. Resample both the user path and each ideal polyline to a fixed number
//!    of points, then score by mean point-to-point distance (shape match),
//!    blended with the word's dictionary frequency.
//! 4. Prune aggressively first (first/last key proximity, length window) so
//!    only a few hundred candidates are scored — keeps decode < ~30 ms over a
//!    30k-word dictionary.
//!
//! ## Why geometric and not FUTO/ExecuTorch
//! The accessibility design's preferred path is FUTO's `swipe-library` (C++)
//! running an `ExecuTorch` `.pte` transformer. That runtime is a large `CMake`
//! build (`PyTorch` core + XNNPACK + flatbuffers, multi-GB) with no maintained
//! Rust binding, links a GPL library, and needs a model download — not
//! feasible to build cleanly+offline in the current toolchain. So this
//! geometric decoder is the **shipping** path. The [`SwipeModel`] trait is
//! the clean seam: drop in a `FutoSwipeModel` implementing it later and the
//! widget wiring is unchanged.

use std::collections::HashMap;

use vello::kurbo::{Point, Rect};

use crate::dict::{Dictionary, Language, fold_char};
use crate::layout::{KeyAction, KeyId, KeyboardLayout};

/// Number of points each path (user + ideal) is resampled to before scoring.
/// SHARK² uses ~100; a smaller count is plenty for a phone-width keyboard and
/// keeps the inner loop cheap.
const RESAMPLE_N: usize = 32;

/// A decoded swipe candidate: the word plus a score in `0.0..=1.0` (higher is
/// better).
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub word: String,
    pub score: f32,
}

/// The decoder seam. The shipping implementation is [`GeometricSwipeModel`];
/// a future `FutoSwipeModel` (`ExecuTorch`) can implement the same trait and be
/// swapped in with no change to the widget.
pub trait SwipeModel {
    /// Decode a gesture `path` (in the same coordinate space as `key_geom`)
    /// into up to `top_n` ranked candidate words.
    fn decode(
        &self,
        path: &[Point],
        key_geom: &[(KeyId, Rect)],
        layout: &KeyboardLayout,
        top_n: usize,
    ) -> Vec<Candidate>;

    /// The user switched to a layout that types `language`. Decoders with
    /// per-language vocabularies follow it; the default keeps decoding as
    /// before.
    fn set_language(&mut self, _language: Language) {}
}

/// SHARK²-style geometric + dictionary swipe decoder (the shipping path).
pub struct GeometricSwipeModel {
    dict: Dictionary,
    /// The embedded language `dict` holds; `None` for a custom dictionary,
    /// which a language switch leaves alone.
    language: Option<Language>,
}

impl GeometricSwipeModel {
    /// Build a decoder over the embedded dictionary of `language`.
    pub fn for_language(language: Language) -> Self {
        Self {
            dict: Dictionary::for_language(language),
            language: Some(language),
        }
    }

    /// Build a decoder over the embedded English dictionary.
    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

    /// Build a decoder over a custom dictionary (tests / other locales).
    pub fn with_dictionary(dict: Dictionary) -> Self {
        Self {
            dict,
            language: None,
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }

    /// Convenience over the trait method with a fixed top-N.
    pub fn decode_top(
        &self,
        path: &[Point],
        key_geom: &[(KeyId, Rect)],
        layout: &KeyboardLayout,
        top_n: usize,
    ) -> Vec<Candidate> {
        self.decode(path, key_geom, layout, top_n)
    }
}

impl SwipeModel for GeometricSwipeModel {
    fn set_language(&mut self, language: Language) {
        if self.language.is_some_and(|current| current != language) {
            *self = Self::for_language(language);
        }
    }

    fn decode(
        &self,
        path: &[Point],
        key_geom: &[(KeyId, Rect)],
        layout: &KeyboardLayout,
        top_n: usize,
    ) -> Vec<Candidate> {
        if path.len() < 2 || key_geom.is_empty() || top_n == 0 {
            return Vec::new();
        }

        // letter -> key centre, for every single-letter Char key on screen.
        let centres = letter_centres(key_geom, layout);
        if centres.is_empty() {
            return Vec::new();
        }
        // Average key dimension → the scoring distance scale (so a "near miss"
        // is judged relative to key size, not absolute pixels).
        let key_scale = avg_key_size(key_geom).max(1.0);

        let user = resample(path, RESAMPLE_N);
        let user_bbox = bbox_diag(&user).max(1.0);

        // The keys nearest the path's first and last points constrain the
        // candidate's first/last letters — the cheapest, strongest prune.
        // `path.len() >= 2` is checked above, so both ends always exist.
        let (Some(&start), Some(&end)) = (path.first(), path.last()) else {
            return Vec::new();
        };
        let start_letters = nearby_letters(start, &centres, key_scale * 1.6);
        let end_letters = nearby_letters(end, &centres, key_scale * 1.6);

        let path_len = polyline_length(&user);
        // A swipe's polyline is roughly as long as the sum of hops between its
        // letters; use it to bound candidate length and skip wildly off words.
        let approx_letters = (path_len / key_scale).round() as i32;

        let mut scored: Vec<Candidate> = Vec::new();
        for entry in self.dict.words() {
            let word = &entry.word;
            // Accented letters are swiped through their base key (é on e).
            let chars: Vec<char> = word.chars().map(fold_char).collect();
            // length window: a swipe through k keys traces ~k segments.
            let len = chars.len() as i32;
            if len < 1 {
                continue;
            }
            // Single-letter words can't be swiped (no path) — skip; they are
            // produced by tapping, not swiping.
            if len < 2 {
                continue;
            }
            if (len - approx_letters).abs() > approx_letters.max(4) {
                continue;
            }
            // First/last letter must be near the path ends (the classic
            // SHARK² location prune). `len >= 2` was checked above, so both
            // ends always exist.
            let (Some(&first), Some(&last)) = (chars.first(), chars.last()) else {
                continue;
            };
            if !start_letters.contains(&first) {
                continue;
            }
            if !end_letters.contains(&last) {
                continue;
            }
            // Every letter of the candidate must have a key on screen.
            let Some(ideal) = ideal_polyline(&chars, &centres) else {
                continue;
            };
            if ideal.len() < 2 {
                continue;
            }
            let ideal_rs = resample(&ideal, RESAMPLE_N);

            // Shape distance: mean point distance, normalised by the user
            // path's bounding diagonal so longer swipes aren't penalised.
            let shape = mean_point_distance(&user, &ideal_rs) / user_bbox;
            // Map distance → similarity in 0..1 (closer = higher).
            let shape_score = (1.0 - shape).clamp(0.0, 1.0) as f32;

            // Blend with frequency (a strong prior for short ambiguous swipes).
            let freq = self.dict.frequency(word);
            let score = 0.78 * shape_score + 0.22 * freq;

            scored.push(Candidate {
                word: word.clone(),
                score,
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.word.cmp(&b.word))
        });
        scored.truncate(top_n);
        scored
    }
}

// --- geometry helpers -----------------------------------------------------

/// Map each single-letter `Char` key to the centre of its rect.
fn letter_centres(key_geom: &[(KeyId, Rect)], layout: &KeyboardLayout) -> HashMap<char, Point> {
    let mut out = HashMap::new();
    for (id, rect) in key_geom {
        let Some(key) = layout.key(*id) else { continue };
        let KeyAction::Char(s) = &key.action else {
            continue;
        };
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.next())
            && c.is_alphabetic()
        {
            // Lowercase: dictionary words + the candidate polyline are
            // lowercase.
            out.entry(c.to_ascii_lowercase())
                .or_insert_with(|| rect.center());
        }
    }
    out
}

/// Average of key width+height across the geometry (the scoring length unit).
fn avg_key_size(key_geom: &[(KeyId, Rect)]) -> f64 {
    if key_geom.is_empty() {
        return 1.0;
    }
    let sum: f64 = key_geom
        .iter()
        .map(|(_, r)| (r.width() + r.height()) * 0.5)
        .sum();
    sum / key_geom.len() as f64
}

/// Letters whose key centre is within `radius` of `p`.
fn nearby_letters(p: Point, centres: &HashMap<char, Point>, radius: f64) -> Vec<char> {
    let r2 = radius * radius;
    let mut v: Vec<char> = centres
        .iter()
        .filter(|&(_, &c)| dist2(p, c) <= r2)
        .map(|(&ch, _)| ch)
        .collect();
    // Always include the single nearest letter even if just outside the
    // radius — guards against a swipe that started slightly off the key.
    if let Some((&nearest, _)) = centres
        .iter()
        .min_by(|a, b| {
            dist2(p, *a.1)
                .partial_cmp(&dist2(p, *b.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|(ch, _)| !v.contains(ch))
    {
        v.push(nearest);
    }
    v
}

/// The ideal polyline for a word: its letters' key centres in order. Returns
/// `None` if any letter has no key on screen. Consecutive duplicate letters
/// collapse to one point (a swipe doesn't loop on a double letter).
fn ideal_polyline(chars: &[char], centres: &HashMap<char, Point>) -> Option<Vec<Point>> {
    let mut pts: Vec<Point> = Vec::with_capacity(chars.len());
    for &c in chars {
        let p = *centres.get(&c.to_ascii_lowercase())?;
        if pts.last().is_none_or(|&l| l != p) {
            pts.push(p);
        }
    }
    Some(pts)
}

/// Resample a polyline to exactly `n` equally-spaced points along its arc.
fn resample(path: &[Point], n: usize) -> Vec<Point> {
    if path.is_empty() {
        return Vec::new();
    }
    // `path` is non-empty from the check above, so `first()` always hits.
    let Some(&first_pt) = path.first() else {
        return Vec::new();
    };
    if path.len() == 1 || n <= 1 {
        return vec![first_pt; n.max(1)];
    }
    let total = polyline_length(path);
    if total <= f64::EPSILON {
        return vec![first_pt; n];
    }
    let step = total / (n - 1) as f64;
    let mut out = Vec::with_capacity(n);
    out.push(first_pt);
    let mut seg = 0usize;
    let mut seg_consumed = 0.0;
    let mut acc = 0.0;
    for _ in 1..(n - 1) {
        acc += step;
        // Advance along segments until we've covered `acc` arc length.
        // `seg` is bounded below `path.len() - 1` by the loop condition
        // (`seg + 1 >= path.len() - 1` breaks it out first), so both
        // indices are always in range.
        while let (Some(&a), Some(&b)) = (path.get(seg), path.get(seg + 1)) {
            let seg_len = dist(a, b);
            let remaining = seg_len - seg_consumed;
            let need = acc - traversed(path, seg, seg_consumed);
            if need <= remaining || seg + 1 >= path.len() - 1 {
                let t = if seg_len > 0.0 {
                    (seg_consumed + need).min(seg_len) / seg_len
                } else {
                    0.0
                };
                out.push(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
                seg_consumed = (seg_consumed + need).min(seg_len);
                break;
            }
            seg += 1;
            seg_consumed = 0.0;
        }
    }
    // Same non-emptiness invariant as `first_pt` above.
    if let Some(&last_pt) = path.last() {
        out.push(last_pt);
    }
    out
}

/// Arc length already traversed up to `(seg, seg_consumed)`.
fn traversed(path: &[Point], seg: usize, seg_consumed: f64) -> f64 {
    // Same pairs as `for i in 0..seg { path[i], path[i + 1] }`, without
    // indexing: `windows(2)` already yields `(path[i], path[i + 1])`.
    let t: f64 = path
        .windows(2)
        .take(seg)
        .map(|w| match w {
            [a, b] => dist(*a, *b),
            _ => 0.0,
        })
        .sum();
    t + seg_consumed
}

fn polyline_length(path: &[Point]) -> f64 {
    path.windows(2)
        .map(|w| match w {
            [a, b] => dist(*a, *b),
            _ => 0.0,
        })
        .sum()
}

fn bbox_diag(path: &[Point]) -> f64 {
    if path.is_empty() {
        return 0.0;
    }
    let mut min_pt = Point::new(f64::MAX, f64::MAX);
    let mut max_pt = Point::new(f64::MIN, f64::MIN);
    for p in path {
        min_pt.x = min_pt.x.min(p.x);
        min_pt.y = min_pt.y.min(p.y);
        max_pt.x = max_pt.x.max(p.x);
        max_pt.y = max_pt.y.max(p.y);
    }
    dist(min_pt, max_pt)
}

/// Mean Euclidean distance between corresponding points of two equal-length
/// resampled paths.
fn mean_point_distance(a: &[Point], b: &[Point]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::MAX;
    }
    // `zip` already yields exactly `n = a.len().min(b.len())` pairs.
    let sum: f64 = a.iter().zip(b.iter()).map(|(&pa, &pb)| dist(pa, pb)).sum();
    sum / n as f64
}

fn dist(a: Point, b: Point) -> f64 {
    dist2(a, b).sqrt()
}

fn dist2(a: Point, b: Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// Heuristic: does this pointer path look like a swipe (vs. a tap)? A swipe
/// has travelled meaningfully relative to the key size; a tap stays put.
/// `key_scale` is the average key dimension. Returns true when the path's
/// total travel exceeds ~0.9× a key (i.e. it crossed into a neighbour).
pub fn is_swipe(path: &[Point], key_scale: f64) -> bool {
    if path.len() < 3 {
        return false;
    }
    polyline_length(path) > key_scale * 0.9
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KeyboardLayout, ShiftState, key_rects};

    /// Build the QWERTY key geometry the widget would, at a phone-ish size.
    fn qwerty_geom() -> (KeyboardLayout, Vec<(KeyId, Rect)>) {
        let layout = KeyboardLayout::qwerty();
        // Key area only (the widget gives keys the area below the toolbar);
        // exact origin doesn't matter, the decoder is translation-invariant
        // within its own coordinate space.
        let area = Rect::new(0.0, 0.0, 720.0, 260.0);
        let geom = key_rects(&layout, area, ShiftState::Off);
        (layout, geom)
    }

    /// Build a swipe path that visits each letter's key centre in order.
    fn path_through(word: &str, geom: &[(KeyId, Rect)], layout: &KeyboardLayout) -> Vec<Point> {
        let centres = letter_centres(geom, layout);
        let mut pts = Vec::new();
        for c in word.chars() {
            let centre = centres[&c];
            // Push a couple of samples per key + interpolate toward the next,
            // mimicking a real (denser) drag.
            if let Some(prev) = pts.last().copied() {
                let prev: Point = prev;
                for k in 1..=4 {
                    let t = f64::from(k) / 4.0;
                    pts.push(Point::new(
                        prev.x + (centre.x - prev.x) * t,
                        prev.y + (centre.y - prev.y) * t,
                    ));
                }
            } else {
                pts.push(centre);
            }
        }
        pts
    }

    #[test]
    fn swipe_decoder_recovers_common_words() {
        eprintln!("phase decodes_quick");
        {
            let (layout, geom) = qwerty_geom();
            let model = GeometricSwipeModel::english();
            let path = path_through("quick", &geom, &layout);
            let cands = model.decode_top(&path, &geom, &layout, 8);
            assert!(!cands.is_empty(), "swipe produced candidates");
            let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
            assert!(
                words.contains(&"quick"),
                "expected 'quick' in top-8, got {words:?}"
            );
        }

        eprintln!("phase decodes_hello");
        {
            let (layout, geom) = qwerty_geom();
            let model = GeometricSwipeModel::english();
            let path = path_through("hello", &geom, &layout);
            let cands = model.decode_top(&path, &geom, &layout, 8);
            let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
            assert!(
                words.contains(&"hello"),
                "expected 'hello' in top-8, got {words:?}"
            );
        }

        eprintln!("phase decodes_the");
        {
            let (layout, geom) = qwerty_geom();
            let model = GeometricSwipeModel::english();
            let path = path_through("the", &geom, &layout);
            let cands = model.decode_top(&path, &geom, &layout, 8);
            let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
            // "the" is by far the most common word; it should rank at the very top
            // for a path through t-h-e.
            assert_eq!(cands[0].word, "the", "got {words:?}");
        }
    }

    #[test]
    fn swipe_paths_resample_and_distinguish_taps_from_drags() {
        eprintln!("phase empty_or_trivial_path_yields_nothing");
        {
            let (layout, geom) = qwerty_geom();
            let model = GeometricSwipeModel::english();
            assert!(model.decode_top(&[], &geom, &layout, 8).is_empty());
            assert!(
                model
                    .decode_top(&[Point::new(1.0, 1.0)], &geom, &layout, 8)
                    .is_empty()
            );
        }

        eprintln!("phase is_swipe_distinguishes_tap_from_drag");
        {
            let (_layout, geom) = qwerty_geom();
            let scale = avg_key_size(&geom);
            // A tap: a few samples all in the same spot.
            let tap = vec![
                Point::new(100.0, 100.0),
                Point::new(101.0, 100.5),
                Point::new(100.5, 101.0),
            ];
            assert!(!is_swipe(&tap, scale), "tiny jitter is not a swipe");
            // A drag across several keys.
            let drag = vec![
                Point::new(0.0, 100.0),
                Point::new(200.0, 100.0),
                Point::new(400.0, 100.0),
            ];
            assert!(is_swipe(&drag, scale), "long drag is a swipe");
        }

        eprintln!("phase resample_produces_exact_count_and_endpoints");
        {
            let path = vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 10.0),
            ];
            let rs = resample(&path, 16);
            assert_eq!(rs.len(), 16);
            assert_eq!(rs[0], Point::new(0.0, 0.0));
            assert_eq!(*rs.last().unwrap(), Point::new(10.0, 10.0));
        }
    }

    #[test]
    // The budget is a property of the shipping build. A debug build runs this
    // an order of magnitude slower, so the gate there measures the compiler,
    // not the decoder — it only ever caught the machine being busy. Decode
    // correctness is covered by the tests above, in every build.
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget only means something in a release build; run with --release"
    )]
    fn decode_is_fast_enough() {
        // A full decode over the embedded dictionary against the 30 ms budget,
        // with headroom so only a real regression trips it.
        let (layout, geom) = qwerty_geom();
        let model = GeometricSwipeModel::english();
        let path = path_through("keyboard", &geom, &layout);
        let t0 = ui::time::now();
        let _ = model.decode_top(&path, &geom, &layout, 8);
        let dt = t0.elapsed();
        assert!(dt.as_millis() < 200, "decode took {dt:?} (budget headroom)");
    }
}
