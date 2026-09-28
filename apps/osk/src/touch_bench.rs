//! Simulated typing benchmark for tap resolution.
//!
//! Types the phrases in `data/typing_phrases.txt` on a phone-sized QWERTY
//! with noisy touch points and counts how often each tap lands on the letter
//! that was meant, with and without context-aware resolution.
//!
//! Touch model: each tap lands at the intended key's centre plus a Gaussian
//! offset in key units, with a small downward bias (thumbs tend to land
//! below the visual centre of a key) and a spread that varies per run from
//! careful (0.15 of a key) to hurried (0.30). A tap is first hit-tested
//! against the drawn keys, exactly as the widget does; only a tap that lands
//! on a letter key goes through [`resolve_with`].
//!
//! Two failure kinds are reported:
//! * **error** — the tap typed a letter other than the one meant;
//! * **stolen** — the tap landed inside the meant key's drawn rectangle and
//!   still typed something else. These are the misses a user notices and
//!   cannot explain, so tuning keeps them near zero.
//!
//! `cargo test -p osk --lib touch_bench -- --ignored --nocapture` prints the
//! full parameter sweep.

use std::collections::HashMap;

use vello::kurbo::{Point, Rect};

use super::{NextKeyDistribution, ResolveParams, TouchKey, TouchModel, resolve_with};
use crate::layout::{KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::predict::Predictor;

const PHRASES: &str = include_str!("../data/typing_phrases.txt");

/// Downward bias of the touch point, in key heights.
const BIAS_Y: f64 = 0.08;

/// Deterministic xorshift64* with Box–Muller normals, so every run of the
/// benchmark sees the same taps.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        ((v >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let u1 = self.next_f64();
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

struct Keyboard {
    all: Vec<(KeyId, Rect)>,
    letters: Vec<TouchKey>,
}

fn phone_keyboard() -> Keyboard {
    let layout = KeyboardLayout::qwerty();
    // A 390 pt wide phone with the widget's default 56 pt rows.
    let all = key_rects(&layout, Rect::new(0.0, 0.0, 390.0, 224.0), ShiftState::Off);
    let letters = all
        .iter()
        .filter_map(|&(id, rect)| {
            let key = layout.key(id).filter(|k| k.is_letter())?;
            Some(TouchKey {
                id,
                ch: key.label.chars().next()?,
                rect,
            })
        })
        .collect();
    Keyboard { all, letters }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Score {
    pub taps: usize,
    pub errors: usize,
    pub stolen: usize,
}

impl Score {
    pub fn error_rate(self) -> f64 {
        self.errors as f64 / self.taps.max(1) as f64
    }
    pub fn stolen_rate(self) -> f64 {
        self.stolen as f64 / self.taps.max(1) as f64
    }
}

/// Type every phrase `reps` times at touch spread `spread` (key units).
/// `params: None` types with the plain rectangle hit test.
fn run(
    kb: &Keyboard,
    predictor: &Predictor,
    cache: &mut HashMap<(Option<String>, String), NextKeyDistribution>,
    spread: f64,
    params: Option<ResolveParams>,
    reps: usize,
    seed: u64,
) -> Score {
    let mut rng = Rng(seed);
    let mut score = Score::default();
    let model = TouchModel::new();
    for _ in 0..reps {
        for phrase in PHRASES.lines() {
            let mut prev: Option<String> = None;
            for word in phrase.split_whitespace() {
                let mut typed = String::new();
                for ch in word.chars() {
                    let Some(target) = kb.letters.iter().find(|k| k.ch == ch) else {
                        continue;
                    };
                    let (w, h) = (target.rect.width(), target.rect.height());
                    let c = target.rect.center();
                    let touch = Point::new(
                        c.x + rng.normal() * spread * w,
                        c.y + (BIAS_Y + rng.normal() * spread) * h,
                    );
                    let hit = kb.all.iter().find(|(_, r)| r.contains(touch)).map(|p| p.0);
                    let got =
                        match (hit, params) {
                            (Some(id), Some(params)) if kb.letters.iter().any(|k| k.id == id) => {
                                let p = cache.entry((prev.clone(), typed.clone())).or_insert_with(
                                    || predictor.next_key_distribution(prev.as_deref(), &typed),
                                );
                                resolve_with(touch, &kb.letters, p, &model, params)
                            }
                            (hit, _) => hit,
                        };
                    let got_ch = got.and_then(|id| kb.letters.iter().find(|k| k.id == id));
                    score.taps += 1;
                    if got != Some(target.id) {
                        score.errors += 1;
                        if target.rect.contains(touch) {
                            score.stolen += 1;
                        }
                    }
                    // Later letters are predicted from what was really typed.
                    if let Some(k) = got_ch {
                        typed.push(k.ch);
                    }
                }
                prev = Some(typed);
            }
        }
    }
    score
}

const SPREADS: [f64; 4] = [0.15, 0.20, 0.25, 0.30];

fn sweep_point(
    kb: &Keyboard,
    predictor: &Predictor,
    cache: &mut HashMap<(Option<String>, String), NextKeyDistribution>,
    params: Option<ResolveParams>,
    reps: usize,
) -> Vec<Score> {
    SPREADS
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            run(
                kb,
                predictor,
                cache,
                s,
                params,
                reps,
                0x9e37_79b9 + i as u64,
            )
        })
        .collect()
}

#[test]
fn context_resolution_types_fewer_wrong_letters_and_steals_almost_none() {
    let kb = phone_keyboard();
    let predictor = Predictor::english();
    let mut cache = HashMap::new();
    let plain = sweep_point(&kb, &predictor, &mut cache, None, 1);
    let tuned = sweep_point(&kb, &predictor, &mut cache, Some(ResolveParams::DEFAULT), 1);
    for ((spread, plain), tuned) in SPREADS.iter().zip(&plain).zip(&tuned) {
        eprintln!(
            "spread {spread:.2}: plain {:.2}% errors, resolved {:.2}% errors ({:.2}% stolen)",
            plain.error_rate() * 100.0,
            tuned.error_rate() * 100.0,
            tuned.stolen_rate() * 100.0,
        );
        assert!(
            (tuned.errors as f64) < 0.8 * plain.errors as f64,
            "resolution must clearly beat the plain hit test at spread {spread}"
        );
        assert!(
            tuned.stolen_rate() < 0.012,
            "resolution stole {:.2}% of well-aimed taps at spread {spread}",
            tuned.stolen_rate() * 100.0
        );
    }
}

#[test]
#[ignore = "parameter sweep; run explicitly with --ignored --nocapture"]
fn sweep_resolution_parameters() {
    let kb = phone_keyboard();
    let predictor = Predictor::english();
    let mut cache = HashMap::new();
    let reps = 3;
    let fmt = |scores: &[Score]| {
        scores
            .iter()
            .map(|s| {
                format!(
                    "{:5.2}/{:4.2}",
                    s.error_rate() * 100.0,
                    s.stolen_rate() * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let plain = sweep_point(&kb, &predictor, &mut cache, None, reps);
    eprintln!("error%/stolen% at spreads {SPREADS:?}");
    eprintln!("plain hit test                 {}", fmt(&plain));
    let mut best: Option<(f64, ResolveParams)> = None;
    for core in [0.5, 0.6, 0.7] {
        for sigma in [0.20, 0.25, 0.30, 0.35, 0.45] {
            for smoothing in [0.15, 0.25, 0.40, 0.55, 0.70, 0.85] {
                let params = ResolveParams {
                    sigma,
                    core,
                    smoothing,
                };
                let scores = sweep_point(&kb, &predictor, &mut cache, Some(params), reps);
                // A stolen tap counts twice: it is an error the user can see
                // they did not make.
                let cost = scores
                    .iter()
                    .map(|s| s.error_rate() + s.stolen_rate())
                    .sum::<f64>()
                    / scores.len() as f64;
                eprintln!(
                    "core {core:.2} sigma {sigma:.2} smooth {smoothing:.2}  {}  cost {:.2}",
                    fmt(&scores),
                    cost * 100.0
                );
                // The core zone is pinned at 60%; other sizes are for reference.
                if (core - 0.6).abs() < 1e-9 && best.is_none_or(|(c, _)| cost < c) {
                    best = Some((cost, params));
                }
            }
        }
    }
    eprintln!("best at core 0.6: {best:?}");
}
