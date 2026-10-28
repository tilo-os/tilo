//! Tap resolution by the likely next key.
//!
//! A tap near the border between two letters is ambiguous: the pointer landed
//! in one key's rectangle, but the user may well have aimed at its neighbour.
//! Rather than resizing keys (which would make the keyboard shift under the
//! user's fingers), the tap is resolved to
//!
//! ```text
//! argmax_key  P(touch | key) · P(key | context)
//! ```
//!
//! * `P(touch | key)` is a 2D Gaussian around the key's centre with a standard
//!   deviation of [`SIGMA_FRACTION`] of the key's size on each axis, shifted by
//!   a per-key [`KeyOffset`] learned from where this user actually hits it.
//! * `P(key | context)` is the [`NextKeyDistribution`] — how likely each letter
//!   is to come next given the word so far and the previous word (built by
//!   [`crate::predict::Predictor::next_key_distribution`]).
//!
//! A tap inside the inner [`CORE_FRACTION`] of a key always returns that key,
//! so a deliberate, well-aimed tap is never overridden by the language model.
//! Everything here is pure: no widget, no I/O.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Model};
use vello::kurbo::{Point, Rect};

use crate::layout::KeyId;

/// Standard deviation of the touch distribution around a key centre, as a
/// fraction of the key's width (x) and height (y). Tuned with the simulated
/// typing benchmark (`touch_bench`): a tighter spread lets the language model
/// decide only genuinely ambiguous taps, which cut wrong letters by a third to
/// three quarters (depending on how sloppily the benchmark types) while
/// keeping taps stolen from inside the meant key below one in a hundred.
pub const SIGMA_FRACTION: f64 = 0.25;

/// Fraction of a key's width and height (centred) inside which a tap always
/// selects that key, regardless of context.
pub const CORE_FRACTION: f64 = 0.6;

/// Share of uniform probability mixed into the context distribution so an
/// unlikely letter stays reachable outside its core zone.
pub const UNIFORM_SMOOTHING: f64 = 0.25;

/// The tunable shape of [`resolve`]. [`ResolveParams::DEFAULT`] is what the
/// keyboard ships; other values exist for the typing benchmark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolveParams {
    /// See [`SIGMA_FRACTION`].
    pub sigma: f64,
    /// See [`CORE_FRACTION`].
    pub core: f64,
    /// See [`UNIFORM_SMOOTHING`].
    pub smoothing: f64,
}

impl ResolveParams {
    pub const DEFAULT: Self = Self {
        sigma: SIGMA_FRACTION,
        core: CORE_FRACTION,
        smoothing: UNIFORM_SMOOTHING,
    };
}

impl Default for ResolveParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Learned offsets are stored in key-size units and clamped to this magnitude
/// so a run of sloppy taps can never move a key's centre past its own edge.
const MAX_OFFSET: f32 = 0.35;

/// Learning rate floor for the running offset average: later samples keep
/// nudging the offset instead of freezing it after many taps.
const MIN_LEARNING_RATE: f32 = 0.05;

/// Bound on the number of per-key offsets retained.
pub(crate) const MAX_LEARNED_KEYS: usize = 64;

/// One letter key the resolver may choose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchKey {
    pub id: KeyId,
    /// The lower-case letter the key types.
    pub ch: char,
    /// The key's on-screen rectangle.
    pub rect: Rect,
}

/// How likely each letter is to be typed next. Letters absent from the map
/// have zero context probability (before smoothing). An empty distribution is
/// uniform.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NextKeyDistribution {
    probs: HashMap<char, f64>,
}

impl NextKeyDistribution {
    /// Every letter equally likely.
    pub fn uniform() -> Self {
        Self::default()
    }

    /// Build from unnormalised per-letter masses. Non-positive and non-finite
    /// masses are dropped; the rest are normalised to sum to one.
    pub fn from_weights(weights: impl IntoIterator<Item = (char, f64)>) -> Self {
        let mut probs: HashMap<char, f64> = HashMap::new();
        for (ch, w) in weights {
            if w.is_finite() && w > 0.0 {
                *probs.entry(ch).or_insert(0.0) += w;
            }
        }
        let total: f64 = probs.values().sum();
        if total > 0.0 {
            for p in probs.values_mut() {
                *p /= total;
            }
        }
        Self { probs }
    }

    /// Mix two distributions: `(1 - other_share) · self + other_share · other`.
    /// An empty side contributes nothing, so blending with an empty
    /// distribution returns the other unchanged.
    #[must_use]
    pub fn blend(&self, other: &Self, other_share: f64) -> Self {
        if other.probs.is_empty() {
            return self.clone();
        }
        if self.probs.is_empty() {
            return other.clone();
        }
        let share = other_share.clamp(0.0, 1.0);
        let mut probs = HashMap::new();
        for (&ch, &p) in &self.probs {
            *probs.entry(ch).or_insert(0.0) += (1.0 - share) * p;
        }
        for (&ch, &p) in &other.probs {
            *probs.entry(ch).or_insert(0.0) += share * p;
        }
        Self { probs }
    }

    /// Whether the distribution carries no context (uniform).
    pub fn is_uniform(&self) -> bool {
        self.probs.is_empty()
    }

    /// Context probability of `ch` (0 for an unseen letter; not smoothed).
    pub fn prob(&self, ch: char) -> f64 {
        self.probs.get(&ch).copied().unwrap_or(0.0)
    }
}

/// Where this user tends to hit one key, in key-size units from its centre
/// (`dx = 0.1` means a tenth of a key width to the right).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Model, JsonSchema)]
pub struct KeyOffset {
    pub dx: f32,
    pub dy: f32,
    pub samples: u32,
}

/// Per-user touch adaptation: one learned [`KeyOffset`] per letter. Plain
/// data so it can be persisted or synced privately between a user's devices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TouchModel {
    offsets: BTreeMap<char, KeyOffset>,
}

impl TouchModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The learned offset for `ch`, if any.
    pub fn offset(&self, ch: char) -> Option<KeyOffset> {
        self.offsets.get(&ch).copied()
    }

    /// Number of keys with a learned offset.
    pub fn learned_keys(&self) -> usize {
        self.offsets.len()
    }

    /// Every learned offset, by letter.
    pub fn offsets(&self) -> &BTreeMap<char, KeyOffset> {
        &self.offsets
    }

    /// Take `offsets` as where this user tends to hit each letter.
    pub fn adopt(&mut self, offsets: &BTreeMap<char, KeyOffset>) {
        self.offsets = offsets
            .iter()
            .take(MAX_LEARNED_KEYS)
            .map(|(letter, offset)| (*letter, *offset))
            .collect();
    }

    /// Forget every learned offset.
    pub fn clear(&mut self) {
        self.offsets.clear();
    }

    /// Record that the user meant `ch` (drawn at `rect`) when touching
    /// `touch`, moving that key's expected centre towards where they hit it.
    pub fn learn(&mut self, ch: char, touch: Point, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        if !self.offsets.contains_key(&ch) && self.offsets.len() >= MAX_LEARNED_KEYS {
            return;
        }
        let c = rect.center();
        let lim = f64::from(MAX_OFFSET) * 2.0;
        let ox = ((touch.x - c.x) / rect.width()).clamp(-lim, lim) as f32;
        let oy = ((touch.y - c.y) / rect.height()).clamp(-lim, lim) as f32;
        let entry = self.offsets.entry(ch).or_default();
        entry.samples = entry.samples.saturating_add(1);
        let rate = (1.0 / entry.samples as f32).max(MIN_LEARNING_RATE);
        entry.dx = (entry.dx + rate * (ox - entry.dx)).clamp(-MAX_OFFSET, MAX_OFFSET);
        entry.dy = (entry.dy + rate * (oy - entry.dy)).clamp(-MAX_OFFSET, MAX_OFFSET);
    }

    /// Log-likelihood (up to a shared constant) of `touch` given key `k`.
    fn log_touch_likelihood(&self, touch: Point, k: &TouchKey, sigma: f64) -> f64 {
        let w = k.rect.width();
        let h = k.rect.height();
        let off = self.offset(k.ch).unwrap_or_default();
        let c = k.rect.center();
        let cx = c.x + f64::from(off.dx) * w;
        let cy = c.y + f64::from(off.dy) * h;
        let zx = (touch.x - cx) / (sigma * w);
        let zy = (touch.y - cy) / (sigma * h);
        -0.5 * (zx * zx + zy * zy)
    }
}

/// The inner `fraction` of `rect`.
pub fn core_zone(rect: Rect, fraction: f64) -> Rect {
    let mx = rect.width() * (1.0 - fraction) * 0.5;
    let my = rect.height() * (1.0 - fraction) * 0.5;
    Rect::new(rect.x0 + mx, rect.y0 + my, rect.x1 - mx, rect.y1 - my)
}

/// Resolve a tap at `touch` to the most likely intended key among `keys`.
/// Returns `None` only when `keys` holds no key with a positive size.
pub fn resolve(
    touch: Point,
    keys: &[TouchKey],
    p_next: &NextKeyDistribution,
    model: &TouchModel,
) -> Option<KeyId> {
    resolve_with(touch, keys, p_next, model, ResolveParams::DEFAULT)
}

/// [`resolve`] with explicit [`ResolveParams`].
pub fn resolve_with(
    touch: Point,
    keys: &[TouchKey],
    p_next: &NextKeyDistribution,
    model: &TouchModel,
    params: ResolveParams,
) -> Option<KeyId> {
    let sized = || {
        keys.iter()
            .filter(|k| k.rect.width() > 0.0 && k.rect.height() > 0.0)
    };
    if let Some(k) = sized().find(|k| core_zone(k.rect, params.core).contains(touch)) {
        return Some(k.id);
    }
    let uniform = if keys.is_empty() {
        0.0
    } else {
        1.0 / keys.len() as f64
    };
    let prior = |ch: char| {
        if p_next.is_uniform() {
            uniform
        } else {
            (1.0 - params.smoothing) * p_next.prob(ch) + params.smoothing * uniform
        }
    };
    sized()
        .map(|k| {
            let score = model.log_touch_likelihood(touch, k, params.sigma) + prior(k.ch).ln();
            (k.id, score)
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id)
}

/// One sampled cell of the effective hit map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitCell {
    pub rect: Rect,
    /// The key a tap at the cell's centre resolves to.
    pub key: KeyId,
    /// The letter key whose drawn rectangle holds the cell's centre.
    pub drawn: KeyId,
}

/// Sample the effective hit areas over `area` on a `step`-sized grid: what a
/// tap at each cell's centre resolves to in this context. Drives the debug
/// overlay that shows how context grows and shrinks the invisible targets.
pub fn hit_map(
    area: Rect,
    step: f64,
    keys: &[TouchKey],
    p_next: &NextKeyDistribution,
    model: &TouchModel,
    params: ResolveParams,
) -> Vec<HitCell> {
    let mut cells = Vec::new();
    if step <= 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return cells;
    }
    let columns = (area.width() / step).ceil() as usize;
    let rows = (area.height() / step).ceil() as usize;
    for row in 0..rows {
        let y = area.y0 + row as f64 * step;
        for column in 0..columns {
            let x = area.x0 + column as f64 * step;
            let rect = Rect::new(x, y, (x + step).min(area.x1), (y + step).min(area.y1));
            let c = rect.center();
            // Resolution only ever runs for a tap that landed on a letter.
            let drawn = keys.iter().find(|k| k.rect.contains(c)).map(|k| k.id);
            if let Some(drawn) = drawn
                && let Some(key) = resolve_with(c, keys, p_next, model, params)
            {
                cells.push(HitCell { rect, key, drawn });
            }
        }
    }
    cells
}

/// Remembers the [`NextKeyDistribution`] for one typing context so it is
/// computed once per keystroke rather than once per candidate or pointer move.
#[derive(Clone, Debug, Default)]
pub struct NextKeyCache {
    context: Option<(Option<String>, String)>,
    dist: NextKeyDistribution,
}

impl NextKeyCache {
    /// The distribution for (`prev_word`, `prefix`), computing it with
    /// `compute` only when the context changed since the last call.
    pub fn get(
        &mut self,
        prev_word: Option<&str>,
        prefix: &str,
        compute: impl FnOnce() -> NextKeyDistribution,
    ) -> &NextKeyDistribution {
        let fresh = self
            .context
            .as_ref()
            .is_some_and(|(p, w)| p.as_deref() == prev_word && w == prefix);
        if !fresh {
            self.dist = compute();
            self.context = Some((prev_word.map(str::to_string), prefix.to_string()));
        }
        &self.dist
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KeyboardLayout, ShiftState, key_rects};
    use crate::predict::Predictor;

    fn qwerty_keys() -> Vec<TouchKey> {
        let layout = KeyboardLayout::qwerty();
        key_rects(&layout, Rect::new(0.0, 0.0, 1000.0, 240.0), ShiftState::Off)
            .into_iter()
            .filter_map(|(id, rect)| {
                let key = layout.key(id)?;
                if !key.is_letter() {
                    return None;
                }
                Some(TouchKey {
                    id,
                    ch: key.label.chars().next()?,
                    rect,
                })
            })
            .collect()
    }

    fn key(keys: &[TouchKey], ch: char) -> TouchKey {
        *keys.iter().find(|k| k.ch == ch).unwrap()
    }

    fn resolved_char(
        keys: &[TouchKey],
        touch: Point,
        p: &NextKeyDistribution,
        m: &TouchModel,
    ) -> char {
        let id = resolve(touch, keys, p, m).unwrap();
        keys.iter().find(|k| k.id == id).unwrap().ch
    }

    #[test]
    fn key_centre_resolves_to_that_key() {
        let keys = qwerty_keys();
        let p = NextKeyDistribution::from_weights([('e', 1.0)]);
        for k in &keys {
            assert_eq!(
                resolved_char(&keys, k.rect.center(), &p, &TouchModel::new()),
                k.ch
            );
        }
    }

    #[test]
    fn e_r_border_after_th_resolves_to_e() {
        let keys = qwerty_keys();
        let predictor = Predictor::english();
        let p = predictor.next_key_distribution(None, "th");
        assert!(
            p.prob('e') > p.prob('r'),
            "lexicon should favour 'e' after 'th'"
        );
        let e = key(&keys, 'e');
        let r = key(&keys, 'r');
        let touch = Point::new(r.rect.x0 + 1.0, e.rect.center().y);
        assert!(r.rect.contains(touch));
        assert_eq!(resolved_char(&keys, touch, &p, &TouchModel::new()), 'e');
    }

    #[test]
    fn uniform_context_resolves_to_nearest_key() {
        let keys = qwerty_keys();
        let e = key(&keys, 'e');
        let r = key(&keys, 'r');
        let y = e.rect.center().y;
        let uniform = NextKeyDistribution::uniform();
        let m = TouchModel::new();
        assert_eq!(
            resolved_char(&keys, Point::new(r.rect.x0 + 1.0, y), &uniform, &m),
            'r'
        );
        assert_eq!(
            resolved_char(&keys, Point::new(e.rect.x1 - 1.0, y), &uniform, &m),
            'e'
        );
    }

    #[test]
    fn core_zone_always_wins() {
        let keys = qwerty_keys();
        let r = key(&keys, 'r');
        // Context says only 'e' can follow, but the tap is well inside 'r'.
        let only_e = NextKeyDistribution::from_weights([('e', 1.0)]);
        let core = core_zone(r.rect, CORE_FRACTION);
        let touch = Point::new(core.x0 + 0.5, core.center().y);
        assert_eq!(
            resolved_char(&keys, touch, &only_e, &TouchModel::new()),
            'r'
        );
    }

    #[test]
    fn core_zone_wins_for_every_key_against_every_other_letter() {
        let keys = qwerty_keys();
        let m = TouchModel::new();
        for k in &keys {
            let core = core_zone(k.rect, CORE_FRACTION);
            for other in keys.iter().filter(|o| o.id != k.id) {
                let p = NextKeyDistribution::from_weights([(other.ch, 1.0)]);
                for (fx, fy) in [
                    (0.01, 0.5),
                    (0.99, 0.5),
                    (0.5, 0.01),
                    (0.5, 0.99),
                    (0.01, 0.01),
                ] {
                    let touch =
                        Point::new(core.x0 + core.width() * fx, core.y0 + core.height() * fy);
                    assert_eq!(resolved_char(&keys, touch, &p, &m), k.ch);
                }
            }
        }
    }

    #[test]
    fn learned_offset_shifts_decision() {
        let keys = qwerty_keys();
        let e = key(&keys, 'e');
        let r = key(&keys, 'r');
        let touch = Point::new(r.rect.x0 + 2.0, e.rect.center().y);
        let uniform = NextKeyDistribution::uniform();
        let mut m = TouchModel::new();
        assert_eq!(resolved_char(&keys, touch, &uniform, &m), 'r');
        for _ in 0..20 {
            m.learn('e', Point::new(e.rect.x1 - 2.0, e.rect.center().y), e.rect);
        }
        assert!(m.offset('e').unwrap().dx > 0.2);
        assert_eq!(resolved_char(&keys, touch, &uniform, &m), 'e');
    }

    #[test]
    fn learned_offsets_are_bounded_and_serialisable() {
        let keys = qwerty_keys();
        let e = key(&keys, 'e');
        let mut m = TouchModel::new();
        for _ in 0..50 {
            m.learn(
                'e',
                Point::new(e.rect.x1 + 500.0, e.rect.y0 - 500.0),
                e.rect,
            );
        }
        let off = m.offset('e').unwrap();
        assert!(off.dx <= MAX_OFFSET && off.dy >= -MAX_OFFSET);
        let json = serde_json::to_string(&m).unwrap();
        let back: TouchModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        m.clear();
        assert_eq!(m.learned_keys(), 0);
    }

    #[test]
    fn cache_recomputes_only_on_context_change() {
        let mut cache = NextKeyCache::default();
        let mut calls = 0;
        for _ in 0..3 {
            cache.get(Some("of"), "th", || {
                calls += 1;
                NextKeyDistribution::uniform()
            });
        }
        cache.get(Some("of"), "the", || {
            calls += 1;
            NextKeyDistribution::uniform()
        });
        assert_eq!(calls, 2);
    }
}

#[cfg(test)]
#[path = "touch_bench.rs"]
mod touch_bench;
