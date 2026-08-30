//! End-to-end test of the real FUTO swipe engine. Runs only when the crate
//! is built with the FUTO libs (`cfg(futo)`, i.e. `QOS_SWIPE_LIB` set) AND
//! the model dir is provided via `QOS_SWIPE_MODELS` (the HF `futo-org/futo-swipe`
//! clone, plus `en_w2.combined`). Otherwise it self-skips.

#![cfg(futo)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use swipe::{FutoSwipe, ModelPaths};

// default_en_qwerty() normalized key centres (mirrors layout.hpp).
fn key_center(c: char) -> (f32, f32) {
    let cx = [
        0.100, 0.600, 0.400, 0.300, 0.250, 0.400, 0.500, 0.600, 0.750, 0.700, 0.800, 0.900, 0.800,
        0.700, 0.850, 0.950, 0.050, 0.350, 0.200, 0.450, 0.650, 0.500, 0.150, 0.300, 0.550, 0.200,
    ];
    let cy = [
        0.500, 0.833, 0.833, 0.500, 0.167, 0.500, 0.500, 0.500, 0.167, 0.500, 0.500, 0.500, 0.833,
        0.833, 0.167, 0.167, 0.167, 0.167, 0.500, 0.167, 0.167, 0.833, 0.167, 0.833, 0.167, 0.833,
    ];
    let i = (c as u8 - b'a') as usize;
    (cx[i], cy[i])
}

/// Build a sampled swipe path through the given word's letters.
fn swipe_path(word: &str) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let centers: Vec<(f32, f32)> = word.chars().map(key_center).collect();
    let (mut xs, mut ys, mut ts) = (Vec::new(), Vec::new(), Vec::new());
    let per_seg = 12;
    let mut tms = 0.0f32;
    for w in centers.windows(2) {
        let (a, b) = (w[0], w[1]);
        for s in 0..per_seg {
            let f = s as f32 / per_seg as f32;
            xs.push(a.0 + (b.0 - a.0) * f);
            ys.push(a.1 + (b.1 - a.1) * f);
            ts.push(tms);
            tms += 16.0;
        }
    }
    // include the final point
    if let Some(&last) = centers.last().as_ref() {
        xs.push(last.0);
        ys.push(last.1);
        ts.push(tms);
    }
    (xs, ys, ts)
}

fn models() -> Option<PathBuf> {
    std::env::var_os("QOS_SWIPE_MODELS").map(PathBuf::from)
}

fn engine() -> Option<FutoSwipe> {
    let m = models()?;
    let paths = ModelPaths {
        encoder: m.join("honorable_sturgeon/model_fp32.pte"),
        decoder: Some(m.join("magic_macaw/model_fp32.pte")),
        lm_model: None,
        lm_vocab: None,
        dict_combined: Some(m.join("en_w2.combined")),
    };
    FutoSwipe::new(&paths, 100, 8)
}

#[test]
fn recognizes_common_words() {
    let Some(eng) = engine() else {
        eprintln!("QOS_SWIPE_MODELS unset or engine load failed — skipping");
        return;
    };
    assert!(eng.has_decoder(), "decoder should be loaded");

    for word in ["hello", "the", "world", "swipe"] {
        let (x, y, t) = swipe_path(word);
        let cands = eng.recognize(&x, &y, &t, 8);
        assert!(!cands.is_empty(), "{word}: no candidates");
        let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
        assert!(
            words.iter().any(|w| *w == word),
            "{word}: expected in top-8, got {words:?}"
        );
        eprintln!("{word} -> {words:?}");
    }
}
