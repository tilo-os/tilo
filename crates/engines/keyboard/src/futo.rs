//! `FutoSwipeModel` — the real FUTO `ExecuTorch` swipe decoder, wired into the
//! OSK's [`SwipeModel`] boundary via the `swipe`
//! crate (C++/ExecuTorch over FFI).
//!
//! The encoder is universal; we pair it with the `en_qwerty` decoder + the
//! dictionary. The OSK's on-screen letter keys are normalized into the
//! model's `0,1` layout space (both are standard QWERTY, so the bounding-box
//! normalization aligns them).
//!
//! [`FutoSwipeModel::default_load`] returns `None` when the FUTO libs aren't
//! linked (crate built without `TILO_SWIPE_LIB`) or the model assets are
//! absent — the widget then keeps its geometric decoder. So this is a pure
//! upgrade: best-in-class swipe when available, graceful fallback otherwise.

use std::path::{Path, PathBuf};

use swipe::{FutoSwipe, ModelPaths};
use vello::kurbo::{Point, Rect};

use crate::layout::{KeyId, KeyboardLayout};
use crate::num::{f32_of_count, f32_of_f64};
use crate::swipe::{Candidate, SwipeModel};

const ENCODER: &str = "honorable_sturgeon/model_fp32.pte";
const DECODER: &str = "magic_macaw/model_fp32.pte";
const LM_MODEL: &str = "hungry_jellyfish/context_lm.pte";
const LM_VOCAB: &str = "hungry_jellyfish/vocab.txt";
const DICT: &str = "en_w2.combined";

pub struct FutoSwipeModel {
    engine: FutoSwipe,
}

impl FutoSwipeModel {
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
        FutoSwipe::new(&paths, 100, 5).map(|engine| Self { engine })
    }

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

    pub fn predict_next(&self, context: &[&str], k: usize) -> Vec<Candidate> {
        // The decoder names its count as `i32`; asking for more candidates than
        // it can name is answered with all it can.
        let k = i32::try_from(k).unwrap_or(i32::MAX);
        self.engine
            .predict_next(context, k)
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
    if let Some(p) = std::env::var_os("TILO_SWIPE_MODELS") {
        v.push(PathBuf::from(p));
    }
    v.push(PathBuf::from("/usr/share/tilo/swipe-models"));
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
        let mut min_pt = Point::new(f64::MAX, f64::MAX);
        let mut max_pt = Point::new(f64::MIN, f64::MIN);
        for (id, rect) in key_geom {
            if layout.key(*id).is_some_and(super::layout::Key::is_letter) {
                min_pt.x = min_pt.x.min(rect.x0);
                min_pt.y = min_pt.y.min(rect.y0);
                max_pt.x = max_pt.x.max(rect.x1);
                max_pt.y = max_pt.y.max(rect.y1);
            }
        }
        if !(max_pt.x > min_pt.x && max_pt.y > min_pt.y) {
            return Vec::new();
        }
        let (w, h) = (max_pt.x - min_pt.x, max_pt.y - min_pt.y);
        let mut xs = Vec::with_capacity(path.len());
        let mut ys = Vec::with_capacity(path.len());
        let mut ts = Vec::with_capacity(path.len());
        for (i, p) in path.iter().enumerate() {
            xs.push(f32_of_f64(((p.x - min_pt.x) / w).clamp(0.0, 1.0)));
            ys.push(f32_of_f64(((p.y - min_pt.y) / h).clamp(0.0, 1.0)));
            ts.push(f32_of_count(i) * 16.0);
        }
        let top_n = i32::try_from(top_n).unwrap_or(i32::MAX);
        let cands = self.engine.recognize(&xs, &ys, &ts, top_n);
        // The FUTO scores are model log-scores; the OSK only needs ranked
        // order in `0..=1`, so map by rank (best first).
        let n = cands.len().max(1);
        cands
            .into_iter()
            .enumerate()
            .map(|(i, c)| Candidate {
                word: c.word,
                score: 1.0 - f32_of_count(i) / f32_of_count(n),
            })
            .collect()
    }
}
