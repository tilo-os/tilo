//! CPU cost of the interactions a person feels in Tube on a feed of real-size
//! thumbnails: the frames while thumbnails stream in, scrolling, resizing and
//! opening a page. Budgets apply to optimised builds only.
//!
//! `cargo test --profile thin-app -p tube --test perf_budget -- --nocapture`
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "timing statistics over bounded sample counts: microsecond casts, percentile indices and medians of a few hundred samples"
)]
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
    eprintln!(
        "perf tube cold start (build + first frame): {:?}",
        t.elapsed()
    );
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
        h.resize((1300 + (i * 7) % 170, 800 + (i * 5) % 110))
            .unwrap();
        resize.push(t.elapsed().as_micros() as u64);
    }
    let p50 = stat("tube resize step (feed)", &mut resize);
    budget("tube resize step", p50, 8_000);
}

/// Tube as every host mounts it (the framework app, not the bare widget),
/// on the sample catalogue at the Yoga's logical size.
fn hosted() -> ui::framework::testing::TestApp<tube::TubeShell> {
    let mut app = ui::framework::testing::TestApp::<tube::TubeShell>::new().unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.with_surface::<TubeApp, _>(|surface| {
        surface.stage_model_for_test(tube::sample::model());
    })
    .expect("the canonical Tube surface is mounted");
    app.settle();
    app.harness().relayout();
    app
}

fn visible_card(app: &mut ui::framework::testing::TestApp<tube::TubeShell>, skip: usize) -> String {
    let targets = app.harness().targets();
    let mut cards: Vec<_> = targets
        .iter()
        .filter(|(name, rect)| {
            name.starts_with("video_card_")
                && rect.y0 >= 0.0
                && rect.y1 <= f64::from(VIEWPORT.1)
                && rect.x1 <= f64::from(VIEWPORT.0)
        })
        .map(|(name, _)| name.clone())
        .collect();
    cards.sort();
    assert!(!cards.is_empty(), "Home shows cards");
    cards[skip % cards.len()].clone()
}

fn click_back(app: &mut ui::framework::testing::TestApp<tube::TubeShell>) {
    assert!(
        ui::window_toolbar::declared().is_some_and(|bar| !bar.leading.is_empty()),
        "the watch page offers Back in the window bar"
    );
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Back);
    app.settle();
}

/// The frames a window runs while nothing of the app's moves, and which of
/// them recomposed the layout tree. A rebuild is what `rebuilt_frames` counts
/// and what the perf budgets forbid, so a run over several frames reads the
/// frames rather than counting from a base: a rebuild in one of them cannot
/// be told from a quiet frame in another that way.
fn frames_while_quiet(
    app: &mut ui::framework::testing::TestApp<tube::TubeShell>,
    frames: usize,
) -> (usize, Vec<u64>) {
    app.harness().begin_profiling();
    let mut polled = 0;
    for _ in 0..frames {
        std::thread::sleep(std::time::Duration::from_millis(20));
        polled += usize::from(app.harness().poll_frame().unwrap());
    }
    let rebuilt: Vec<u64> = app
        .harness()
        .take_profile_samples()
        .into_iter()
        .filter(|frame| frame.build_layout_us > 0)
        .map(|frame| frame.frame)
        .collect();
    (polled, rebuilt)
}

fn route(app: &mut ui::framework::testing::TestApp<tube::TubeShell>) -> tube::model::Route {
    app.with_surface::<TubeApp, _>(|tube| tube.route_for_test().clone())
        .expect("the window shows Tube")
}

/// Opening the window: the framework app's construction and first frame.
/// The media engine is the costly part a window does not need until
/// something plays (its first construction starts `GStreamer`: 0.8 s on the
/// builder), so a window that has only browsed has none.
#[test]
fn hosted_window_opens_without_starting_the_media_engine() {
    let t = Instant::now();
    let mut app = hosted();
    eprintln!(
        "perf tube window open (hosted, first frame): {:?}",
        t.elapsed()
    );
    let card = visible_card(&mut app, 0);
    app.harness().click_named(&card).unwrap();
    app.settle();
    click_back(&mut app);
    let started = app
        .with_surface::<TubeApp, _>(|tube| tube.media_engine_started_for_test())
        .unwrap();
    assert!(
        !started,
        "opening Tube, a video page and going back built a media engine with nothing to play"
    );
}

/// Opening a video from Home and going back: the click a person makes most.
#[test]
fn hosted_open_a_video_and_go_back() {
    let mut app = hosted();
    let (mut open, mut back) = (Vec::new(), Vec::new());
    for i in 0..24 {
        let card = visible_card(&mut app, i);
        let t = Instant::now();
        app.harness().click_named(&card).unwrap();
        app.settle();
        open.push(t.elapsed().as_micros() as u64);
        assert!(
            matches!(route(&mut app), tube::model::Route::Watch(_)),
            "clicking {card} opens its watch page"
        );
        let t = Instant::now();
        click_back(&mut app);
        back.push(t.elapsed().as_micros() as u64);
        assert_eq!(
            route(&mut app),
            tube::model::Route::Home,
            "Back returns Home"
        );
    }
    let p50 = stat("tube open a video (hosted)", &mut open);
    budget("tube open a video", p50, 16_000);
    let p50 = stat("tube back to Home (hosted)", &mut back);
    budget("tube back to Home", p50, 16_000);
}

/// Moving the pointer over the Home grid: every motion is a hover change.
#[test]
fn hosted_pointer_over_the_feed() {
    let mut app = hosted();
    let mut motion = Vec::new();
    for i in 0..200u32 {
        let x = 300.0 + f64::from((i * 37) % 1100);
        let y = 120.0 + f64::from((i * 53) % 760);
        let t = Instant::now();
        app.harness().send(&ui::synthetic_input::cursor_moved(x, y));
        motion.push(t.elapsed().as_micros() as u64);
    }
    let p50 = stat("tube pointer motion over the feed (hosted)", &mut motion);
    budget("tube pointer motion", p50, 2_000);
}

/// Results the network answers while a person looks at Home, each folded
/// in the frame after it lands: a video's dislikes (shown only on its own
/// page, so Home has nothing to redraw) and a crowdsourced title (shown on
/// its card, so Home is rebuilt).
#[test]
fn hosted_frames_while_results_land() {
    let mut app = hosted();
    let results = app
        .with_surface::<TubeApp, _>(TubeApp::script_worker_for_test)
        .expect("the canonical Tube surface is mounted");
    let ids: Vec<String> = tube::sample::model()
        .videos
        .iter()
        .map(|video| video.id.clone())
        .collect();
    let (mut unseen, mut seen) = (Vec::new(), Vec::new());
    for (round, id) in ids.iter().cycle().take(120).enumerate() {
        results
            .send(tube::api::Update::Dislikes {
                video: id.clone(),
                count: round as u64,
                likes: None,
            })
            .unwrap();
        let t = Instant::now();
        app.harness().relayout();
        unseen.push(t.elapsed().as_micros() as u64);
        results
            .send(tube::api::Update::DeArrow {
                video: id.clone(),
                title: Some(format!("A clearer title, take {round}")),
                thumbnail_url: None,
            })
            .unwrap();
        let t = Instant::now();
        app.harness().relayout();
        seen.push(t.elapsed().as_micros() as u64);
    }
    let p50 = stat(
        "tube frame folding a result Home does not show (hosted)",
        &mut unseen,
    );
    budget("tube frame folding a result Home does not show", p50, 2_000);
    let p50 = stat("tube frame folding a result Home shows (hosted)", &mut seen);
    budget("tube frame folding a result Home shows", p50, 6_000);
}

/// A playing video on its watch page: each new frame is a picture, the same
/// size as the last, so it repaints and lays nothing out.
#[test]
fn hosted_playing_video_frames_lay_nothing_out() {
    let mut app = hosted();
    let card = visible_card(&mut app, 0);
    app.harness().click_named(&card).unwrap();
    app.settle();
    app.with_surface::<TubeApp, _>(TubeApp::play_synthesized_for_test)
        .expect("the window shows Tube");
    // Let the first frames land and the page settle around the player.
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.harness().poll_frame().unwrap();
    }
    let (polled, rebuilt) = frames_while_quiet(&mut app, 30);
    assert!(polled > 0, "a playing video asks for frames");
    assert!(
        rebuilt.is_empty(),
        "video frames {} recomposed the layout tree",
        rebuilt
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    // A painted frame measures what it draws and marks no leaf to measure:
    // a picture the size of the last one changes no size in the tree.
    let (measured, leaves) = painted_while_playing(&mut app, 30);
    assert_eq!(measured, 0, "no video frame re-measured the layout");
    assert_eq!(leaves, 0, "no leaf was marked to measure");
}

/// The frames a playing video painted: how many, and how many of them
/// re-measured and marked leaves to measure. Measured after a settle, so the
/// replayed input of an earlier frame does not land in them.
fn painted_while_playing(
    app: &mut ui::framework::testing::TestApp<tube::TubeShell>,
    polls: usize,
) -> (usize, usize) {
    app.settle();
    let (mut painted, mut measured, mut leaves) = (0, 0, 0);
    for _ in 0..polls {
        std::thread::sleep(std::time::Duration::from_millis(20));
        if app.harness().poll_frame().unwrap() {
            painted += 1;
            let stats = app.harness().last_relayout();
            measured += usize::from(stats.measured);
            leaves += stats.marked_leaves;
        }
    }
    assert!(painted > 0, "a playing video paints");
    (measured, leaves)
}
