//! `FutoSwipeModel` — the real FUTO `ExecuTorch` swipe decoder, wired into the
//! OSK's [`SwipeModel`](crate::swipe::SwipeModel) seam via the `swipe`
//! crate (C++/ExecuTorch over FFI).
//!
//! The encoder is universal; we pair it with the `en_qwerty` decoder + the
//! dictionary. The OSK's on-screen letter keys are normalized into the
//! model's [0,1] layout space (both are standard QWERTY, so the bounding-box
//! normalization aligns them).
//!
//! [`FutoSwipeModel::default_load`] returns `None` when the FUTO libs aren't
//! linked (crate built without `QOS_SWIPE_LIB`) or the model assets are
//! absent — the widget then keeps its geometric decoder. So this is a pure
//! upgrade: best-in-class swipe when available, graceful fallback otherwise.

use std::path::{Path, PathBuf};

use swipe::{FutoSwipe, ModelPaths};
use vello::kurbo::{Point, Rect};

use crate::layout::{KeyId, KeyboardLayout};
use crate::swipe::{Candidate, SwipeModel};

/// FUTO model file layout (the HF `futo-org/futo-swipe` tree + the
/// `en_w2.combined` dictionary fetched alongside it).
const ENCODER: &str = "honorable_sturgeon/model_fp32.pte";
const DECODER: &str = "magic_macaw/model_fp32.pte";
const LM_MODEL: &str = "hungry_jellyfish/context_lm.pte";
const LM_VOCAB: &str = "hungry_jellyfish/vocab.txt";
const DICT: &str = "en_w2.combined";

pub struct FutoSwipeModel {
    engine: FutoSwipe,
}

impl FutoSwipeModel {
    /// Load the engine from a directory containing the FUTO model assets.
    /// Returns `None` if the FUTO backend isn't compiled in or the encoder
    /// fails to load.
    pub fn load(models_dir: &Path) -> Option<Self> {
        let d = models_dir;
        let lm_model = d.join(LM_MODEL);
        let lm_vocab = d.join(LM_VOCAB);
        let with_lm = lm_model.exists() && lm_vocab.exists();
        let paths = ModelPaths {
            encoder: d.join(ENCODER),
            decoder: Some(d.join(DECODER)),
            lm_model: with_lm.then(|| lm_model.clone()),
            lm_vocab: with_lm.then(|| lm_vocab.clone()),
            dict_combined: Some(d.join(DICT)),
        };
        FutoSwipe::new(&paths, 100, crate::widget::MAX_SUGGESTIONS as i32)
            .map(|engine| Self { engine })
    }

    /// Resolve the model directory from the environment / known install
    /// locations and load. `None` if nothing is found or FUTO isn't linked.
    pub fn default_load() -> Option<Self> {
        for dir in candidate_dirs() {
            if dir.join(ENCODER).exists()
                && let Some(m) = Self::load(&dir)
            {
                return Some(m);
            }
        }
        None
    }

    /// Next-word prediction via the context LM (empty if no LM loaded).
    pub fn predict_next(&self, context: &[&str], k: usize) -> Vec<Candidate> {
        self.engine
            .predict_next(context, k as i32)
            .into_iter()
            .map(|c| Candidate {
                word: c.word,
                score: c.score,
            })
            .collect()
    }
}

fn candidate_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("QOS_SWIPE_MODELS") {
        v.push(PathBuf::from(p));
    }
    // System install path (shipped in the OS image).
    v.push(PathBuf::from("/usr/share/qos/swipe-models"));
    // Dev: vendored next to the crate.
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models"));
    v
}

impl SwipeModel for FutoSwipeModel {
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
        // Bounding box of the on-screen letter keys → normalize the gesture
        // into the model's [0,1] layout space.
        let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (id, rect) in key_geom {
            if layout.key(*id).is_some_and(super::layout::Key::is_letter) {
                minx = minx.min(rect.x0);
                miny = miny.min(rect.y0);
                maxx = maxx.max(rect.x1);
                maxy = maxy.max(rect.y1);
            }
        }
        if !(maxx > minx && maxy > miny) {
            return Vec::new();
        }
        let (w, h) = (maxx - minx, maxy - miny);
        let mut xs = Vec::with_capacity(path.len());
        let mut ys = Vec::with_capacity(path.len());
        let mut ts = Vec::with_capacity(path.len());
        for (i, p) in path.iter().enumerate() {
            xs.push((((p.x - minx) / w).clamp(0.0, 1.0)) as f32);
            ys.push((((p.y - miny) / h).clamp(0.0, 1.0)) as f32);
            ts.push(i as f32 * 16.0);
        }
        let cands = self.engine.recognize(&xs, &ys, &ts, top_n as i32);
        // The FUTO scores are model log-scores; the OSK only needs ranked
        // order in `0..=1`, so map by rank (best first).
        let n = cands.len().max(1);
        cands
            .into_iter()
            .enumerate()
            .map(|(i, c)| Candidate {
                word: c.word,
                score: 1.0 - (i as f32) / (n as f32),
            })
            .collect()
    }
}

// Self-skips unless the FUTO libs are linked (real `swipe` backend) AND
// the model dir is resolvable (env `QOS_SWIPE_MODELS` or an install path) —
// i.e. `FutoSwipeModel::default_load()` returns `Some`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KeyboardLayout, ShiftState, key_rects};

    fn qwerty_geom() -> (KeyboardLayout, Vec<(KeyId, Rect)>) {
        let layout = KeyboardLayout::qwerty();
        let area = Rect::new(0.0, 0.0, 720.0, 260.0);
        let geom = key_rects(&layout, area, ShiftState::Off);
        (layout, geom)
    }

    fn letter_center(geom: &[(KeyId, Rect)], layout: &KeyboardLayout, c: char) -> Point {
        for (id, rect) in geom {
            if let Some(key) = layout.key(*id)
                && let crate::layout::KeyAction::Char(s) = &key.action
                && s.chars().eq(std::iter::once(c))
            {
                return Point::new(
                    f64::midpoint(rect.x0, rect.x1),
                    f64::midpoint(rect.y0, rect.y1),
                );
            }
        }
        panic!("no key for {c}");
    }

    fn path_through(word: &str, geom: &[(KeyId, Rect)], layout: &KeyboardLayout) -> Vec<Point> {
        let mut pts: Vec<Point> = Vec::new();
        for c in word.chars() {
            let centre = letter_center(geom, layout, c);
            if let Some(prev) = pts.last().copied() {
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
    fn futo_decodes_through_osk_adapter() {
        let Some(model) = FutoSwipeModel::default_load() else {
            eprintln!("FUTO model unavailable (set QOS_SWIPE_MODELS) — skipping");
            return;
        };
        let (layout, geom) = qwerty_geom();
        for word in ["hello", "the", "world"] {
            let path = path_through(word, &geom, &layout);
            let cands = model.decode(&path, &geom, &layout, 8);
            let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
            assert!(
                words.contains(&word),
                "{word}: expected in top-8, got {words:?}"
            );
            eprintln!("osk-adapter {word} -> {words:?}");
        }
    }
}
