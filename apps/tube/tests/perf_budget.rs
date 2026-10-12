//! CPU cost of the interactions a person feels in Tube on a feed of real-size
//! thumbnails: the frames while thumbnails stream in, scrolling, resizing and
//! opening a page. Budgets apply to optimised builds only.
//!
//! `cargo test --profile thin-app -p tube --test perf_budget -- --nocapture`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::panic,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::sync::Arc;
use std::time::Instant;

use tube::TubeApp;
use ui::headless::Framed;
use ui::theme::Theme;

const VIEWPORT: (u32, u32) = (1472, 920);

fn thumbnail() -> Vec<u8> {
    let (w, h) = (640u32, 360u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
            let [r, g, b, _] = v.to_le_bytes();
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    ui::png::encode_rgba8(w, h, &rgba).unwrap()
}

fn stat(label: &str, samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "perf {label}: n={} p50={}us p99={}us max={}us",
        samples.len(),
        at(0.5),
        at(0.99),
        samples[samples.len() - 1]
    );
    at(0.5)
}

fn budget(label: &str, value: u64, limit: u64) {
    if !cfg!(debug_assertions) {
        assert!(value <= limit, "{label}: {value}us exceeds {limit}us");
    }
}

/// A Home feed whose every card has its own thumbnail URL.
fn feed() -> (Framed<TubeApp>, ui::ImageStore) {
    let bytes = thumbnail();
    let fetch: ui::Fetcher = Arc::new(move |_url: &str| Some(bytes.clone()));
    let store = ui::ImageStore::new(&fetch);
    let mut model = tube::sample::model();
    model.route = tube::model::Route::Home;
    for (index, video) in model.videos.iter_mut().enumerate() {
        video.thumbnail_url = format!("http://test.local/thumb-{index}.png");
    }
    let app = TubeApp::new()
        .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
        .with_model_for_test(model)
        .with_image_store(store.clone());
    (
        Framed::new(ui::State::new(), app, VIEWPORT, Theme::dark()),
        store,
    )
}

#[test]
fn feed_with_thumbnails() {
    let t = Instant::now();
    let (mut app, store) = feed();
    eprintln!("perf tube cold start (build + first frame): {:?}", t.elapsed());
    // Frames while thumbnails decode off-thread and fold in.
    let mut loading = Vec::new();
    for _ in 0..400 {
        let t = Instant::now();
        Framed::harness_mut(&mut app).relayout();
        loading.push(t.elapsed().as_micros() as u64);
        if store.pending() == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let max_loading = *loading.iter().max().unwrap();
    stat("tube frame while thumbnails load", &mut loading);
    budget("tube frame while thumbnails load (max)", max_loading, 8_000);

    let h = Framed::harness_mut(&mut app);
    h.click_at(700.0, 500.0);
    let mut scroll = Vec::new();
    for i in 0..80 {
        let t = Instant::now();
        h.scroll(0.0, if i < 40 { -1.0 } else { 1.0 });
        scroll.push(t.elapsed().as_micros() as u64);
    }
    let p50 = stat("tube scroll step (feed)", &mut scroll);
    budget("tube scroll step", p50, 4_000);

    for i in 0..6 {
        h.resize((1472 - i, 920 - i)).unwrap();
    }
    let mut resize = Vec::new();
    for i in 0..60u32 {
        let t = Instant::now();
        h.resize((1300 + (i * 7) % 170, 800 + (i * 5) % 110)).unwrap();
        resize.push(t.elapsed().as_micros() as u64);
    }
    let p50 = stat("tube resize step (feed)", &mut resize);
    budget("tube resize step", p50, 8_000);
}
