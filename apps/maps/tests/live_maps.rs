//! Live-verify lane for the maps raster basemap.
//!
//! These tests exercise the REAL end-to-end tile path — the LIVE `MapsApp`
//! (network on, real background worker), the production
//! `draw_tiles → request → worker fetch → PNG decode → drain` plumbing, and a
//! GPU rasterise of the result — the exact path that hermetic unit tests (which
//! hit a local mock) and `--shots` (which renders offline SAMPLE data) never
//! touch. That gap is why the "tiles never load" bug shipped: every test was
//! green while the real network→decode→render path was never run.
//!
//! The network-dependent cases are explicitly ignored so hermetic CI reports
//! them as not executed instead of counting an early return as a passing test.
//! They still compile with every build and are runnable as one opt-in lane:
//!
//!   tilo-build test -p maps --test `live_maps` -- \
//!     --ignored --nocapture --test-threads 1
//!
//! Set `MAPS_LIVE_OUT=/some/dir` to control where the inspection PNGs are
//! written (default: `apps/maps/live-out/`).

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use ui::time::Duration;

use maps::MapsApp;
mod livetest;

use livetest::{Frame, Harness, pump_frames_until, render_live_to_png};
use vello::kurbo::Rect;

/// Live viewport. Width < 720 puts the side panel into its overlay drawer,
/// which starts CLOSED for the live `new()` app — so the whole frame is map,
/// with only the floating chrome (search bar top, controls right, attribution
/// bottom-right) over it. That gives a large, clean central tile region.
const VP: (u32, u32) = (700, 780);

/// Central map region, inset well clear of every piece of floating chrome:
/// search bar (top), zoom/locate/download controls (right edge), scale bar +
/// map attribution (bottom-right). Everything sampled here is pure basemap.
fn map_region() -> Rect {
    let (w, h) = VP;
    Rect::new(30.0, 110.0, f64::from(w) - 90.0, f64::from(h) - 90.0)
}

fn unique_cache() -> PathBuf {
    // Only needs to be distinct per run; a pre-epoch clock (never in practice)
    // just falls back to 0 rather than panicking the test setup.
    let n = ui::time::unix_now_ns();
    std::env::temp_dir().join(format!("tilo-maps-live-{}-{}", std::process::id(), n))
}

fn out_dir() -> PathBuf {
    let dir = std::env::var("MAPS_LIVE_OUT")
        .unwrap_or_else(|_| format!("{}/live-out", env!("CARGO_MANIFEST_DIR")));
    let p = PathBuf::from(dir);
    std::fs::create_dir_all(&p).ok();
    p
}

/// Every `.png` the worker persisted under `dir`, as bytes — exactly what the
/// production fetch path accepted from the tile server.
fn cached_pngs(dir: &std::path::Path) -> Vec<Vec<u8>> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "png") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files
        .iter()
        .map(|p| std::fs::read(p).expect("a cached tile file to read"))
        .collect()
}

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Fail unless the tiles the worker accepted are real map imagery. A provider
/// refusing service answers every coordinate with one placeholder image (CARTO's
/// "API KEY REQUIRED" tile is byte-identical for every z/x/y), which decodes
/// fine and paints plenty of colours — so decoding alone cannot tell. Distinct
/// coordinates across a dense city centre must be distinct images.
fn assert_real_imagery(tiles: &[Vec<u8>]) {
    assert!(
        tiles.len() >= 8,
        "expected a viewport of cached tiles, found {}",
        tiles.len()
    );
    for bytes in tiles {
        assert!(bytes.starts_with(PNG_MAGIC), "a cached tile is not a PNG");
        let tile = ui::png::decode_rgba8(bytes).expect("a cached tile decodes");
        assert_eq!(
            (tile.width, tile.height),
            (256, 256),
            "a cached tile is not a slippy-map tile"
        );
    }
    let distinct: std::collections::HashSet<&[u8]> = tiles.iter().map(Vec::as_slice).collect();
    assert_eq!(
        distinct.len(),
        tiles.len(),
        "the server answered different city-centre tiles with identical images — a \
         placeholder, not map imagery ({} distinct of {})",
        distinct.len(),
        tiles.len()
    );
}

/// (1) FULL-PLUMBING integration test — the thing that was missing.
///
/// Drives the LIVE app's real frame cycle until tiles arrive, then asserts the
/// production request→worker→fetch→DECODE→drain path actually delivered decoded
/// imagery into memory, recorded no failures, and wrote real PNGs to disk.
/// No GPU required — this is pure network + decode + drain.
#[test]
#[ignore = "requires live tile services; run the explicit maps live lane"]
fn live_tiles_fetch_decode_and_cache() {
    let cache = unique_cache();
    let mut app = Harness::new(MapsApp::new_with_cache_dir(cache.clone()), VP);

    // Pump the real frame cycle until the first tile is decoded & resident, or
    // 15s elapse. Each frame's draw issues real requests; the next frame's sync
    // drains decoded results. 150ms between frames lets the worker make headway.
    let stats = pump_frames_until(
        &mut app,
        Duration::from_secs(15),
        Duration::from_millis(150),
        |a| a.tiles_mem_len() > 0,
    );

    // Give the worker a little longer to fill more of the viewport so the count
    // is representative (still bounded).
    let _ = pump_frames_until(
        &mut app,
        Duration::from_secs(6),
        Duration::from_millis(120),
        |a| a.tiles_mem_len() >= 8 || !a.tiles_has_inflight(),
    );

    let mem = app.tiles_mem_len();
    let tiles = cached_pngs(app.tiles_cache_dir());
    let png_count = tiles.len();
    eprintln!(
        "live plumbing: stopped={} frames={} elapsed={:?} mem_tiles={} disk_pngs={} failure={:?}",
        stats.stopped,
        stats.frames,
        stats.elapsed,
        mem,
        png_count,
        app.tiles_failure()
    );

    assert!(
        stats.stopped,
        "no tile ever decoded into memory within 15s — the real fetch→decode→drain path is broken \
         (mem={mem}, failures={}, inflight={})",
        app.tiles_has_failures(),
        app.tiles_has_inflight()
    );
    assert!(
        mem > 0,
        "tiles.mem_len() must be > 0 after live load; got {mem}"
    );
    assert_eq!(
        app.tiles_failure(),
        None,
        "the negative-cache must be empty — a real fetch failed (Rejected: an error status or a \
         non-PNG answer; Unreachable: egress blocked)"
    );
    assert!(
        png_count > 0,
        "the worker must persist real PNGs to the on-disk cache; found {png_count}"
    );
    assert_real_imagery(&tiles);

    std::fs::remove_dir_all(&cache).ok();
}

/// Opt-in live GPU lane: real tiles render, and a mid-zoom reuses tiles without
/// blanking.
#[test]
#[ignore = "requires live tile services and a GPU; run the explicit maps live lane"]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn live_render_shows_real_tiles_and_mid_zoom_never_blanks() {
    // Phase `live_render_shows_real_tiles`.
    // (2) LIVE-RENDER screenshot e2e — inspect the pixels.
    //
    // Renders the LIVE, network-on map to PNG both BEFORE tiles load (placeholder)
    // and AFTER, then proves the map area shows real, varied tile imagery: it is
    // not uniform, it is far more colour-diverse than the placeholder, and it
    // substantially CHANGED once imagery arrived. Requires a headless GPU
    // (lavapipe is fine).
    {
        let cache = unique_cache();
        let mut app = Harness::new(MapsApp::new_with_cache_dir(cache.clone()), VP);
        let out = out_dir();
        let before_png = out.join("maps-live-before.png");
        let after_png = out.join("maps-live-after.png");

        // Frame 0: no tiles resident yet → the honest placeholder (grid substrate).
        let before = render_live_to_png(&mut app, &before_png);
        assert_eq!(
            app.tiles_mem_len(),
            0,
            "the first frame should predate any decoded tile"
        );

        // Pump until enough tiles cover the region, or 25s elapse.
        let stats = pump_frames_until(
            &mut app,
            Duration::from_secs(25),
            Duration::from_millis(150),
            |a| a.tiles_mem_len() >= 8 && !a.tiles_has_inflight(),
        );
        // A few settle frames so late arrivals fold in.
        let _ = pump_frames_until(
            &mut app,
            Duration::from_secs(3),
            Duration::from_millis(120),
            |a| !a.tiles_has_inflight() && a.tiles_mem_len() >= 8,
        );

        let after = render_live_to_png(&mut app, &after_png);

        let region = map_region();
        let before_colors = before.distinct_colors(region, 4);
        let after_colors = after.distinct_colors(region, 4);
        let changed = after.changed_fraction(&before, region, 4, 16);
        eprintln!(
            "live render: mem_tiles={} failures={} frames={} elapsed={:?}\n  \
             before_colors={before_colors} after_colors={after_colors} changed_fraction={:.2}\n  \
             before={} after={}",
            app.tiles_mem_len(),
            app.tiles_has_failures(),
            stats.frames,
            stats.elapsed,
            changed,
            before_png.display(),
            after_png.display(),
        );

        // The data-path outcome must be sound.
        assert!(
            app.tiles_mem_len() > 0,
            "no decoded tiles resident to render"
        );
        assert_eq!(
            app.tiles_failure(),
            None,
            "tile fetch failed — imagery refused or unreachable"
        );
        // The painted tiles are the server's real imagery, not one placeholder
        // repeated: pixel variety alone passed CARTO's "API KEY REQUIRED" tile.
        assert_real_imagery(&cached_pngs(app.tiles_cache_dir()));

        // Pixel outcome: real imagery replaced the loading substrate.
        //
        // The harness renders the dark theme, so this is the OSM Standard
        // raster re-toned dark: deep land, roads, parks, water and light labels.
        // It quantises (5 bits/channel, to ignore AA noise) to many distinct
        // colours; the FLOOR here stays well above what a flat fill or the
        // loading substrate reaches. `changed_fraction` shows most sampled map
        // pixels changing from the loading substrate to the map.
        assert!(
            !after.is_uniform(region),
            "map area is a uniform fill — no imagery rendered"
        );
        assert!(
            after_colors >= 16,
            "map area colour-diversity too low ({after_colors}); expected real, varied tile imagery \
             (before={before_colors})"
        );
        assert!(
            changed > 0.5,
            "less than half the map area changed after tiles loaded ({changed:.2}) — \
             imagery did not replace the placeholder"
        );

        std::fs::remove_dir_all(&cache).ok();
    }

    // Phase `live_mid_zoom_reuses_tiles_and_never_blanks`.
    // Zoom must never blank. After z14 imagery is resident, jumping to a mid-zoom
    // where the target-level tiles are NOT fetched yet must still paint imagery —
    // the resident tiles reused *scaled* (lower-z ancestors on a zoom-in,
    // higher-z descendants on a zoom-out) — rather than a flat land fill. Renders
    // one zoom-out and one zoom-in mid-frame and asserts each shows varied imagery,
    // exercising the real fetch→decode→draw path (only the base tiles ever load).
    {
        let cache = unique_cache();
        let mut app = Harness::new(MapsApp::new_with_cache_dir(cache.clone()), VP);
        let out = out_dir();
        let region = map_region();
        let base_zoom = app.zoom_for_verify();

        // Load the base-zoom imagery.
        let _ = pump_frames_until(
            &mut app,
            Duration::from_secs(25),
            Duration::from_millis(150),
            |a| a.tiles_mem_len() >= 8 && !a.tiles_has_inflight(),
        );
        assert!(app.tiles_mem_len() > 0, "no base imagery loaded to reuse");
        assert_eq!(
            app.tiles_failure(),
            None,
            "base tile fetch failed — imagery refused or unreachable"
        );
        assert_real_imagery(&cached_pngs(app.tiles_cache_dir()));

        // Zoom OUT one integer level: the target tiles aren't loaded, so the frame
        // must reuse the resident tiles as scaled descendants — never blank.
        app.set_zoom_for_verify(base_zoom - 1.0);
        let out_png = out.join("maps-mid-zoom-out.png");
        let f_out = render_live_to_png(&mut app, &out_png);
        let out_colors = f_out.distinct_colors(region, 4);
        eprintln!(
            "mid zoom-out: zoom={:.2} colors={out_colors} mem={} -> {}",
            app.zoom_for_verify(),
            app.tiles_mem_len(),
            out_png.display()
        );
        assert!(
            !f_out.is_uniform(region),
            "zoom-out mid-frame is a uniform fill — the blank bug"
        );
        assert!(
            out_colors > 12,
            "zoom-out mid-frame shows too little imagery ({out_colors}) — tiles not reused scaled"
        );

        // Re-settle at the base zoom, then zoom IN one integer level: reuse the
        // resident tiles as scaled ancestors — again, never blank.
        app.set_zoom_for_verify(base_zoom);
        let _ = pump_frames_until(
            &mut app,
            Duration::from_secs(15),
            Duration::from_millis(150),
            |a| a.tiles_mem_len() >= 8 && !a.tiles_has_inflight(),
        );
        app.set_zoom_for_verify(base_zoom + 1.0);
        let in_png = out.join("maps-mid-zoom-in.png");
        let f_in = render_live_to_png(&mut app, &in_png);
        let in_colors = f_in.distinct_colors(region, 4);
        eprintln!(
            "mid zoom-in: zoom={:.2} colors={in_colors} mem={} -> {}",
            app.zoom_for_verify(),
            app.tiles_mem_len(),
            in_png.display()
        );
        assert!(
            !f_in.is_uniform(region),
            "zoom-in mid-frame is a uniform fill — the blank bug"
        );
        assert!(
            in_colors > 12,
            "zoom-in mid-frame shows too little imagery ({in_colors}) — tiles not reused scaled"
        );

        std::fs::remove_dir_all(&cache).ok();
    }
}

/// A trivially-runnable sanity assert on the pixel helpers themselves, so the
/// content assertions are trusted (a uniform frame reads as uniform / low
/// diversity). Runs hermetically — no gate, no network, no GPU.
#[test]
fn frame_pixel_helpers_detect_uniform_vs_varied() {
    let (w, h) = (32u32, 32u32);
    let uniform = Frame {
        rgba: vec![20u8; (w * h * 4) as usize],
        width: w,
        height: h,
    };
    let region = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    assert!(uniform.is_uniform(region));
    assert_eq!(uniform.distinct_colors(region, 1), 1);

    // A coarse 8px-block checker (survives the step-4 sampler): not uniform,
    // > 1 distinct colour.
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if (x / 8 + y / 8) % 2 == 0 { 200 } else { 40 };
            data[i] = c;
            data[i + 1] = c;
            data[i + 2] = c;
            data[i + 3] = 255;
        }
    }
    let varied = Frame {
        rgba: data,
        width: w,
        height: h,
    };
    assert!(!varied.is_uniform(region));
    assert!(varied.distinct_colors(region, 1) >= 2);
    // Fully different from the uniform frame.
    assert!(varied.changed_fraction(&uniform, region, 1, 16) > 0.4);
}
