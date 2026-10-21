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
//!    of points, then form a coarse shortlist by location and word frequency.
//! 4. Rerank its first 64 words with band-limited dynamic alignment, endpoint
//!    location and the same dictionary prior. The alignment tolerates uneven
//!    progress along curved traces without learned weights.
//! 5. Prune aggressively first (first/last key proximity, length window) so
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
use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout};

/// Number of points each path (user + ideal) is resampled to before scoring.
/// SHARK² uses ~100; a smaller count is plenty for a phone-width keyboard and
/// keeps the inner loop cheap.
const RESAMPLE_N: usize = 32;

#[cfg(test)]
thread_local! {
    /// Dictionary words whose shape a decode on this thread scored.
    static SHAPES_SCORED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

/// Offline geometric + dictionary swipe decoder with bounded elastic reranking.
pub struct GeometricSwipeModel {
    dict: Dictionary,
    /// Folded first/last-letter buckets avoid scanning the full vocabulary
    /// for every gesture. The same location prune selects these candidates.
    endpoints: HashMap<(char, char), Vec<usize>>,
    /// The embedded language `dict` holds; `None` for a custom dictionary,
    /// which a language switch leaves alone.
    language: Option<Language>,
    /// Frequency prior in hundredths; `None` reproduces the original matcher.
    elastic: Option<u8>,
}

impl GeometricSwipeModel {
    /// Build a decoder over the embedded dictionary of `language`.
    pub fn for_language(language: Language) -> Self {
        let mut model = Self::with_dictionary(Dictionary::for_language(language));
        model.language = Some(language);
        model
    }

    /// Build a decoder over the embedded English dictionary.
    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

    /// Build a decoder over a custom dictionary (tests / other locales).
    pub fn with_dictionary(dict: Dictionary) -> Self {
        let endpoints = index_endpoints(&dict);
        Self {
            dict,
            endpoints,
            language: None,
            elastic: Some(100),
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }

    /// Rerank the coarse shortlist with bounded elastic path alignment.
    /// This keeps inference offline and uses the same deployment dictionary.
    #[must_use]
    pub fn with_elastic_alignment(mut self) -> Self {
        self.elastic = Some(100);
        self
    }

    /// Keep the original scoring rule for reproducible baseline comparisons.
    /// User-facing keyboards use bounded elastic alignment by default.
    #[must_use]
    pub fn with_classic_alignment(mut self) -> Self {
        self.elastic = None;
        self
    }

    /// Select the bounded lexical-prior weight for repeatable qualification.
    /// The weight is expressed in hundredths; no text data is learned here.
    #[must_use]
    pub fn with_elastic_prior(mut self, hundredths: u8) -> Self {
        self.elastic = Some(hundredths);
        self
    }

    fn endpoint_candidates(&self, starts: &[char], ends: &[char]) -> Vec<usize> {
        let mut indices = Vec::new();
        for first in starts {
            for last in ends {
                if let Some(bucket) = self.endpoints.get(&(*first, *last)) {
                    indices.extend_from_slice(bucket);
                }
            }
        }
        indices
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
    fn rerank_elastic(
        &self,
        scored: &mut Vec<Candidate>,
        user: &[Point],
        centres: &HashMap<char, Point>,
        key_scale: f64,
    ) {
        let Some(prior_weight) = self.elastic else {
            return;
        };
        scored.truncate(64);
        for candidate in scored.iter_mut() {
            let chars: Vec<_> = candidate.word.chars().map(fold_char).collect();
            if let Some(ideal) = ideal_polyline(&chars, centres) {
                let ideal = resample(&ideal, RESAMPLE_N);
                let distance = elastic_distance(user, &ideal) / (key_scale * key_scale);
                let ends = user
                    .first()
                    .zip(ideal.first())
                    .map_or(0.0, |(&a, &b)| dist2(a, b))
                    + user
                        .last()
                        .zip(ideal.last())
                        .map_or(0.0, |(&a, &b)| dist2(a, b));
                let prior = 1.0 - f64::from(self.dict.frequency(&candidate.word));
                let cost = distance
                    + 0.15 * ends / (key_scale * key_scale)
                    + f64::from(prior_weight) / 100.0 * prior;
                candidate.score = (-cost).exp() as f32;
            }
        }
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.word.cmp(&b.word))
        });
    }
}

impl SwipeModel for GeometricSwipeModel {
    fn set_language(&mut self, language: Language) {
        if self.language.is_some_and(|current| current != language) {
            let elastic = self.elastic;
            *self = Self::for_language(language);
            self.elastic = elastic;
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
        let key_scale = letter_key_scale(key_geom, layout).unwrap_or(1.0).max(1.0);

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
        for index in self.endpoint_candidates(&start_letters, &end_letters) {
            let Some(entry) = self.dict.words().get(index) else {
                continue;
            };
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
            #[cfg(test)]
            SHAPES_SCORED.with(|count| count.set(count.get() + 1));

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
        if self.elastic.is_some() {
            self.rerank_elastic(&mut scored, &user, &centres, key_scale);
        }
        scored.truncate(top_n);
        scored
    }
}

/// Dynamic alignment accommodates curved traces and uneven progress along
/// an ideal path. A fixed band bounds both deformation and computation.
fn elastic_distance(user: &[Point], ideal: &[Point]) -> f64 {
    const BAND: usize = 6;
    let mut previous = [f64::INFINITY; RESAMPLE_N + 1];
    previous[0] = 0.0;
    for (i, &a) in user.iter().enumerate() {
        let mut current = [f64::INFINITY; RESAMPLE_N + 1];
        let lower = i.saturating_sub(BAND);
        let upper = (i + BAND + 1).min(ideal.len());
        for (j, &b) in ideal.iter().enumerate().take(upper).skip(lower) {
            if let (Some(&diagonal), Some(&vertical), Some(horizontal), Some(cell)) = (
                previous.get(j),
                previous.get(j + 1),
                current.get(j).copied(),
                current.get_mut(j + 1),
            ) {
                *cell = dist2(a, b) + diagonal.min(vertical).min(horizontal);
            }
        }
        previous = current;
    }
    previous.get(ideal.len()).copied().unwrap_or(f64::INFINITY) / RESAMPLE_N as f64
}

fn index_endpoints(dict: &Dictionary) -> HashMap<(char, char), Vec<usize>> {
    let mut index: HashMap<(char, char), Vec<usize>> = HashMap::new();
    for (position, entry) in dict.words().iter().enumerate() {
        let mut letters = entry.word.chars().map(fold_char);
        if let (Some(first), Some(last)) = (letters.next(), letters.next_back()) {
            index.entry((first, last)).or_default().push(position);
        }
    }
    index
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

/// Letter-key width and height define the scoring unit. Wide editing keys
/// must not change recognition of a path through otherwise identical letters.
pub(crate) fn letter_key_scale(key_geom: &[(KeyId, Rect)], layout: &KeyboardLayout) -> Option<f64> {
    let mut count = 0;
    let sum: f64 = key_geom
        .iter()
        .filter(|(id, _)| layout.key(*id).is_some_and(Key::is_letter))
        .map(|(_, rect)| {
            count += 1;
            (rect.width() + rect.height()) * 0.5
        })
        .sum();
    (count != 0).then(|| sum / f64::from(count))
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
    if path.len() < 2 {
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
    fn editing_key_width_does_not_change_letter_path_recognition() {
        for (layout, model, word) in [
            (
                KeyboardLayout::qwerty(),
                GeometricSwipeModel::english(),
                "quick",
            ),
            (
                KeyboardLayout::azerty(),
                GeometricSwipeModel::for_language(Language::French),
                "été",
            ),
        ] {
            let geometry = key_rects(&layout, Rect::new(0.0, 0.0, 720.0, 260.0), ShiftState::Off);
            let path = path_through(
                &word.chars().map(fold_char).collect::<String>(),
                &geometry,
                &layout,
            );
            let letters: Vec<_> = geometry
                .iter()
                .copied()
                .filter(|(id, _)| layout.key(*id).is_some_and(Key::is_letter))
                .collect();
            let mut wide = geometry.clone();
            for (id, rect) in &mut wide {
                if layout.key(*id).is_some_and(|key| !key.is_letter()) {
                    *rect = Rect::new(rect.x0, rect.y0, rect.x1 + 10_000.0, rect.y1);
                }
            }
            let model = model.with_elastic_alignment();
            let expected = model.decode_top(&path, &letters, &layout, 8);
            assert!(
                expected.iter().any(|candidate| candidate.word == word),
                "missing {word}"
            );
            for actual in [&geometry, &wide] {
                let actual = model.decode_top(&path, actual, &layout, 8);
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(&expected) {
                    assert_eq!(actual.word, expected.word);
                    assert_eq!(actual.score, expected.score);
                }
            }
        }
    }

    #[test]
    fn elastic_recognition_follows_scaled_translated_geometry_and_language() {
        let (layout, geometry) = qwerty_geom();
        let model = GeometricSwipeModel::english().with_elastic_alignment();
        let path = path_through("quick", &geometry, &layout);
        let expected = model.decode_top(&path, &geometry, &layout, 8);
        assert_eq!(
            expected.first().map(|candidate| candidate.word.as_str()),
            Some("quick")
        );
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let transform =
                vello::kurbo::Affine::translate((71.0, 43.0)) * vello::kurbo::Affine::scale(scale);
            let geometry: Vec<_> = geometry
                .iter()
                .map(|(id, rect)| (*id, transform.transform_rect_bbox(*rect)))
                .collect();
            let path: Vec<_> = path.iter().map(|point| transform * *point).collect();
            let actual = model.decode_top(&path, &geometry, &layout, 8);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.word, expected.word);
                assert!((actual.score - expected.score).abs() < 0.000_01);
            }
        }
        let mut model = model;
        model.set_language(Language::French);
        let layout = KeyboardLayout::azerty();
        let geometry = key_rects(&layout, Rect::new(0.0, 0.0, 390.0, 220.0), ShiftState::Off);
        let path = path_through("bonjour", &geometry, &layout);
        assert_eq!(
            model
                .decode_top(&path, &geometry, &layout, 8)
                .first()
                .map(|candidate| candidate.word.as_str()),
            Some("bonjour")
        );
    }

    #[test]
    fn indexed_swipes_follow_language_switches_and_folded_accents() {
        let layout = KeyboardLayout::azerty();
        let geom = key_rects(&layout, Rect::new(0.0, 0.0, 390.0, 224.0), ShiftState::Off);
        let mut model = GeometricSwipeModel::english();
        model.set_language(Language::French);
        let path = path_through("bonjour", &geom, &layout);
        let candidates = model.decode_top(&path, &geom, &layout, 8);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.word == "bonjour")
        );
        let model =
            GeometricSwipeModel::with_dictionary(Dictionary::from_str("été\t8000\nétude\t7000\n"));
        let path = path_through("ete", &geom, &layout);
        assert_eq!(model.decode_top(&path, &geom, &layout, 1)[0].word, "été");
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
            let (layout, geom) = qwerty_geom();
            let scale = letter_key_scale(&geom, &layout).expect("QWERTY has letter keys");
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
    fn a_decode_scores_only_words_its_endpoints_allow() {
        let (layout, geom) = qwerty_geom();
        let model = GeometricSwipeModel::english();
        let path = path_through("keyboard", &geom, &layout);
        SHAPES_SCORED.with(|count| count.set(0));
        let top = model.decode_top(&path, &geom, &layout, 8);
        let scored = SHAPES_SCORED.with(std::cell::Cell::get);
        assert_eq!(top.first().map(|c| c.word.as_str()), Some("keyboard"));
        let words = model.dictionary().len();
        assert!(
            scored * 4 < words,
            "the endpoint index prunes the dictionary: {scored} of {words} words scored"
        );
    }

    #[test]
    #[ignore = "measurement; run with --release --ignored --nocapture"]
    fn decode_time() {
        // The budget is 30 ms per decode in a release build.
        let (layout, geom) = qwerty_geom();
        let model = GeometricSwipeModel::english();
        let path = path_through("keyboard", &geom, &layout);
        let t0 = ui::time::now();
        let _ = model.decode_top(&path, &geom, &layout, 8);
        println!("swipe decode of \"keyboard\": {:?}", t0.elapsed());
    }
}
