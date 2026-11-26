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
//! the clean boundary: drop in a `FutoSwipeModel` implementing it later and the
//! widget wiring is unchanged.

use std::collections::HashMap;

use vello::kurbo::{Point, Rect};

use crate::dict::{Dictionary, Language, fold_char};
use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout};
use crate::num::{f32_of_f64, f64_of_count};

/// Number of points each path (user + ideal) is resampled to before scoring.
/// SHARK² uses ~100; a smaller count is plenty for a phone-width keyboard and
/// keeps the inner loop cheap.
const RESAMPLE_N: usize = 32;

#[cfg(test)]
thread_local! {
    static SHAPES_SCORED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub word: String,
    pub score: f32,
}

pub trait SwipeModel {
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
    endpoints: HashMap<(char, char), Vec<usize>>,
    language: Option<Language>,
    elastic: Option<u8>,
}

impl GeometricSwipeModel {
    pub fn for_language(language: Language) -> Self {
        let mut model = Self::with_dictionary(Dictionary::for_language(language));
        model.language = Some(language);
        model
    }

    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

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
                candidate.score = f32_of_f64((-cost).exp());
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

        let centres = letter_centres(key_geom, layout);
        if centres.is_empty() {
            return Vec::new();
        }
        // Average key dimension → the scoring distance scale (so a "near miss"
        // is judged relative to key size, not absolute pixels).
        let key_scale = letter_key_scale(key_geom, layout).unwrap_or(1.0).max(1.0);

        let user = resample(path, RESAMPLE_N);
        let user_bbox = bbox_diag(&user).max(1.0);

        let (Some(&start), Some(&end)) = (path.first(), path.last()) else {
            return Vec::new();
        };
        let start_letters = nearby_letters(start, &centres, key_scale * 1.6);
        let end_letters = nearby_letters(end, &centres, key_scale * 1.6);

        let path_len = polyline_length(&user);
        // A swipe's polyline is roughly as long as the sum of hops between its
        // letters; use it to bound candidate length and skip wildly off words.
        let approx_letters = (path_len / key_scale).round();

        let mut scored: Vec<Candidate> = Vec::new();
        for index in self.endpoint_candidates(&start_letters, &end_letters) {
            let Some(entry) = self.dict.words().get(index) else {
                continue;
            };
            let word = &entry.word;
            let chars: Vec<char> = word.chars().map(fold_char).collect();
            let letters = f64_of_count(chars.len());
            if letters < 1.0 {
                continue;
            }
            // Single-letter words can't be swiped (no path) — skip; they are
            // produced by tapping, not swiping.
            if letters < 2.0 {
                continue;
            }
            if (letters - approx_letters).abs() > approx_letters.max(4.0) {
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
            SHAPES_SCORED.with(|count| count.set(count.get().saturating_add(1)));

            let shape = mean_point_distance(&user, &ideal_rs) / user_bbox;
            let shape_score = f32_of_f64((1.0 - shape).clamp(0.0, 1.0));

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
        let upper = i.saturating_add(BAND).saturating_add(1).min(ideal.len());
        for (j, &b) in ideal.iter().enumerate().take(upper).skip(lower) {
            if let (Some(&diagonal), Some(&vertical), Some(horizontal), Some(cell)) = (
                previous.get(j),
                previous.get(j.saturating_add(1)),
                current.get(j).copied(),
                current.get_mut(j.saturating_add(1)),
            ) {
                *cell = dist2(a, b) + diagonal.min(vertical).min(horizontal);
            }
        }
        previous = current;
    }
    previous.get(ideal.len()).copied().unwrap_or(f64::INFINITY) / f64_of_count(RESAMPLE_N)
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
            out.entry(c.to_ascii_lowercase())
                .or_insert_with(|| rect.center());
        }
    }
    out
}

/// Letter-key width and height define the scoring unit. Wide editing keys
/// must not change recognition of a path through otherwise identical letters.
pub fn letter_key_scale(key_geom: &[(KeyId, Rect)], layout: &KeyboardLayout) -> Option<f64> {
    let mut count = 0usize;
    let sum: f64 = key_geom
        .iter()
        .filter(|(id, _)| layout.key(*id).is_some_and(Key::is_letter))
        .map(|(_, rect)| {
            count = count.saturating_add(1);
            (rect.width() + rect.height()) * 0.5
        })
        .sum();
    (count != 0).then(|| sum / f64_of_count(count))
}

fn nearby_letters(p: Point, centres: &HashMap<char, Point>, radius: f64) -> Vec<char> {
    let r2 = radius * radius;
    let mut v: Vec<char> = centres
        .iter()
        .filter(|&(_, &c)| dist2(p, c) <= r2)
        .map(|(&ch, _)| ch)
        .collect();
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

fn resample(path: &[Point], n: usize) -> Vec<Point> {
    if path.is_empty() {
        return Vec::new();
    }
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
    let step = total / f64_of_count(n.saturating_sub(1));
    let mut out = Vec::with_capacity(n);
    out.push(first_pt);
    let mut seg = 0usize;
    let mut seg_consumed = 0.0;
    let mut acc = 0.0;
    for _ in 1..n.saturating_sub(1) {
        acc += step;
        // Advance along segments until we've covered `acc` arc length.
        // `seg` is bounded below `path.len() - 1` by the loop condition
        // (`seg + 1 >= path.len() - 1` breaks it out first), so both
        // indices are always in range.
        while let (Some(&a), Some(&b)) = (path.get(seg), path.get(seg.saturating_add(1))) {
            let seg_len = dist(a, b);
            let remaining = seg_len - seg_consumed;
            let need = acc - traversed(path, seg, seg_consumed);
            if need <= remaining || seg.saturating_add(1) >= path.len().saturating_sub(1) {
                let t = if seg_len > 0.0 {
                    (seg_consumed + need).min(seg_len) / seg_len
                } else {
                    0.0
                };
                out.push(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
                seg_consumed = (seg_consumed + need).min(seg_len);
                break;
            }
            seg = seg.saturating_add(1);
            seg_consumed = 0.0;
        }
    }
    // Same non-emptiness invariant as `first_pt` above.
    if let Some(&last_pt) = path.last() {
        out.push(last_pt);
    }
    out
}

fn traversed(path: &[Point], seg: usize, seg_consumed: f64) -> f64 {
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

fn mean_point_distance(a: &[Point], b: &[Point]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::MAX;
    }
    let sum: f64 = a.iter().zip(b.iter()).map(|(&pa, &pb)| dist(pa, pb)).sum();
    sum / f64_of_count(n)
}

fn dist(a: Point, b: Point) -> f64 {
    dist2(a, b).sqrt()
}

fn dist2(a: Point, b: Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

pub fn is_swipe(path: &[Point], key_scale: f64) -> bool {
    if path.len() < 2 {
        return false;
    }
    polyline_length(path) > key_scale * 0.9
}
