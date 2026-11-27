//! Choosing another tab of a window cross-fades its content: the content the tab showed stays
//! drawn under the one that comes, fading to nothing over the shared motion, and is let go once
//! the swap has played. A second choice mid-fade goes from the content the person saw, never from
//! a ghost behind it. Reduced motion swaps in one frame.
//!
//! What a person sees is what is read: the shell's own frame, replayed on the CPU, at the frames
//! between the choice of one tab and the next.

#![cfg(target_os = "linux")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test fixtures: a frame's pixels are an array the display's size indexes"
)]

use tilolang_runtime::{ResourceRef, Value};
use tilolang_tilo::host::Host;

use ui::time::{Duration, Instant};

use crate::shell_staged::{SHELL, StagedWindow, ada};

/// The scenario staged here: a window of three notes, the first in front.
const SCENARIO: &str = "a window of several places";
/// The strip that window is, as the scenario names it: the row's strip named "Notes".
const STRIP: &str = "s-r-Main-Notes";

const WINDOW: (u32, u32) = (1280, 800);
const FRAME: Duration = Duration::from_millis(16);
/// How far above the rest a pixel has to change to count as another colour.
const SHOWN: u8 = 8;
/// Below this, the window's own bar is not drawn, so what stands there is the tab's content.
const CONTENT_TOP: u32 = 110;

fn staged(reduced: bool) -> StagedWindow {
    let mut theme = tilo_ui_base::Theme::dark();
    theme.reduce_motion = reduced;
    StagedWindow::staged(SCENARIO, WINDOW, theme)
}

/// The window as it is drawn now, replayed on the CPU: its pixels, `WINDOW` wide.
fn shown_pixels(window: &StagedWindow) -> Vec<u8> {
    let mut raster = ui::retained_raster::RetainedRaster::new(true);
    assert!(
        raster.on_cpu(),
        "the CPU canvas is the one this test draws with"
    );
    let frame = ui::retained_raster::FrameContent {
        scene: window.harness.scene(),
        width: WINDOW.0,
        height: WINDOW.1,
        base_color: ui::vello::peniko::Color::TRANSPARENT,
        damage_regions: &[],
        scroll_copies: &[],
        full_redraw: true,
        backdrop: &tilo_ui_base::part_paint::backdrop::BackdropFrame::default(),
    };
    raster
        .rasterize_to_cpu(frame)
        .expect("the CPU canvas replays the shell's frame");
    raster
        .cpu_frame()
        .expect("the pixels the CPU canvas drew")
        .pixels
        .to_vec()
}

/// A pixel counts as changed when any of its channels moved by more than [`SHOWN`].
fn moved(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).any(|channel| a[channel].abs_diff(b[channel]) > SHOWN)
}

/// The pixels of the content, under the window's bar, where `from` and `to` differ: the content
/// one tab shows that the other does not.
fn changed_content(from: &[u8], to: &[u8]) -> Vec<usize> {
    let width = usize::try_from(WINDOW.0).unwrap_or(1);
    let mut out = Vec::new();
    for at in 0..from.len() / 4 {
        let y = u32::try_from(at / width).unwrap_or(0);
        if y < CONTENT_TOP {
            continue;
        }
        let mut a = [0_u8; 4];
        let mut b = [0_u8; 4];
        a.copy_from_slice(&from[at * 4..at * 4 + 4]);
        b.copy_from_slice(&to[at * 4..at * 4 + 4]);
        if moved(a, b) {
            out.push(at);
        }
    }
    out
}

/// How many of `pixels` are drawn as neither `from`'s nor `to`'s colour: the content between two
/// tabs, only drawn while one fades into the other.
fn blended(frame: &[u8], from: &[u8], to: &[u8], pixels: &[usize]) -> usize {
    let pixel = |whole: &[u8], at: usize| {
        let mut rgba = [0_u8; 4];
        rgba.copy_from_slice(&whole[at * 4..at * 4 + 4]);
        rgba
    };
    pixels
        .iter()
        .filter(|at| {
            let now = pixel(frame, **at);
            moved(now, pixel(from, **at)) && moved(now, pixel(to, **at))
        })
        .count()
}

/// The names the window's tabs carry, in front-to-back order: the first window's tabs are the
/// strip this test chooses in.
fn tab_names(window: &StagedWindow) -> Vec<String> {
    let tree = window.instance.root_tree().expect("the window's tree");
    tree.of_kind("ui.TabItem")
        .into_iter()
        .filter_map(|tab| match tab.props.get("name") {
            Some(Value::Text(name)) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Show `place` in the strip, as the session writes it when the person chooses that tab.
fn show_place(window: &StagedWindow, place: &str) {
    let host: Host = window.instance.host().expect("the shell's host");
    let package = host.package(SHELL).expect("the shell's package");
    let meta = &package.meta;
    let ty = meta
        .types
        .iter()
        .find(|declared| declared.name == "LayoutStrip")
        .expect("the layout's strip type")
        .id
        .clone();
    let strip = host
        .store()
        .resources_of(&ty)
        .expect("the layout's strips")
        .into_iter()
        .map(|id| ResourceRef::parse_id(&id, ty.clone()).expect("a strip"))
        .find(|at| {
            host.runtime()
                .read(at, "key")
                .is_ok_and(|key| key == Value::text(STRIP))
        })
        .unwrap_or_else(|| panic!("the scenario holds the strip {STRIP:?}"));
    host.store()
        .set_register(
            &strip,
            &crate::fixtures::field_id(meta, "LayoutStrip", "active"),
            tilolang_runtime::store::Scalar::Text(place.to_owned()),
            "show",
            &ada(),
        )
        .expect("the person's choice reaches the layout");
    let read = host.runtime().read(
        &strip,
        &crate::fixtures::field_id(meta, "LayoutStrip", "active"),
    );
    assert_eq!(
        read.as_ref().ok(),
        Some(&Value::text(place)),
        "the layout holds the tab chosen: {read:?} of {strip:?}"
    );
}

/// The person chooses the tab at `at`: they press the pill, which a change of the layout follows.
/// The write reaches the shell's views over the frames after it, as the session's does.
fn choose(window: &mut StagedWindow, at: Instant, place: &str) {
    ui::time::testing::with_monotonic_time(at, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        show_place(window, place);
    });
    // The session wrote the layout: the window reads again what it showed of it.
    window.instance.store_moved();
    for _ in 0..4 {
        window.frame_at(at);
    }
}

/// Frames of `window` at the live clock until its swap has landed, the clock let to pass as a
/// window's does: the window drawn as a person sees it once the motion is over.
fn settle(window: &mut StagedWindow) {
    std::thread::sleep(std::time::Duration::from_millis(220));
    for _ in 0..4 {
        window.frame_at(ui::time::now());
    }
}

/// How far apart the samples of a fade stand: fine enough to stand in its middle, and to cover
/// the frames the choice takes to reach the view and the motion that follows.
const SAMPLE: Duration = Duration::from_millis(4);

/// Frames of `window` from `start` through the swap, each with the instant it was read at.
fn sampled(window: &mut StagedWindow, start: Instant) -> Vec<(Instant, Vec<u8>)> {
    let mut frames = Vec::new();
    for step in 1..=70 {
        let at = start + SAMPLE * step;
        window.frame_at(at);
        frames.push((at, shown_pixels(window)));
    }
    frames
}

#[test]
fn choosing_another_tab_draws_the_content_it_showed_out_over_the_motion() {
    let start = ui::time::now();
    let mut window = staged(false);
    let names = tab_names(&window);
    assert!(names.len() >= 3, "the window shows its tabs: {names:?}");
    let before = shown_pixels(&window);
    choose(&mut window, start, "p-1");
    let shows_chosen = |window: &StagedWindow| {
        window
            .instance
            .root_tree()
            .expect("the window's tree")
            .of_kind("ui.Swapped")
            .into_iter()
            .any(|node| node.props.get("shown") == Some(&Value::text("p-1")))
    };
    assert!(shows_chosen(&window), "the view shows the tab chosen");

    // The frames between, close enough together to stand in the middle of the fade: what stood
    // where the two tabs' content differs is drawn partly in one colour and partly in the other,
    // and by the end it is the colour that came.
    let played = start + Duration::from_secs(1);
    let held_on = sampled(&mut window, start);
    window.frame_at(played);
    let after = shown_pixels(&window);
    let content = changed_content(&before, &after);
    assert!(
        content.len() > 1_000,
        "the two tabs show different content: {} pixels",
        content.len()
    );
    let held_for = |frame: &[u8]| {
        content
            .iter()
            .filter(|at| {
                let mut was = [0_u8; 4];
                let mut now = [0_u8; 4];
                was.copy_from_slice(&before[**at * 4..**at * 4 + 4]);
                now.copy_from_slice(&frame[**at * 4..**at * 4 + 4]);
                !moved(was, now)
            })
            .count()
    };
    let held: Vec<usize> = held_on.iter().map(|(_, frame)| held_for(frame)).collect();
    let most = held.iter().copied().max().unwrap_or(0);
    assert!(
        most > content.len() / 6,
        "the content it showed is drawn under the one that comes while it fades: {held:?} of {}",
        content.len()
    );
    let least = held.iter().copied().min().unwrap_or(usize::MAX);
    assert!(
        least < content.len() / 20,
        "the content that went fades away over the frames: {held:?} of {}",
        content.len()
    );
    assert_eq!(
        held_for(&after),
        0,
        "the content that went has faded to nothing"
    );
    assert!(shows_chosen(&window));
}

#[test]
fn a_second_choice_mid_fade_goes_from_the_content_the_person_saw() {
    let start = ui::time::now();
    let mut window = staged(false);
    let before = shown_pixels(&window);
    choose(&mut window, start, "p-1");
    window.frame_at(start + FRAME * 2);
    // While the first swap plays, the person chooses a third: the ghost of the first cannot
    // stand behind the one drawn in front.
    choose(&mut window, start + FRAME * 3, "p-2");
    let played = start + Duration::from_secs(1);
    window.frame_at(played);
    let after = shown_pixels(&window);
    let content = changed_content(&before, &after);
    assert!(
        content.len() > 1_000,
        "the third tab shows content of its own: {} pixels",
        content.len()
    );
    assert!(
        window
            .instance
            .root_tree()
            .expect("the window's tree")
            .of_kind("ui.Swapped")
            .into_iter()
            .any(|node| node.props.get("shown") == Some(&Value::text("p-2"))),
        "the newest tab stands"
    );
    // The window comes to rest on the newest tab alone: nothing of the tabs it passed through is
    // drawn any more, and no ghost keeps asking for frames.
    let still = start + Duration::from_secs(2);
    window.frame_at(still);
    assert_eq!(
        changed_content(&after, &shown_pixels(&window)).len(),
        0,
        "the newest tab stands, and the swap has played"
    );
}

/// How much of the content `from` showed still stands in `frame`, over `pixels`: 1 while it is
/// whole, 0 once only `to` is drawn, about half in the middle of a fade. Each pixel's colour is
/// read as how far it has gone from `to` back toward `from`, so ink of the content that came,
/// drawn past the old colour, never counts as the old one fading.
fn old_strength(frame: &[u8], from: &[u8], to: &[u8], pixels: &[usize]) -> f64 {
    let pixel = |whole: &[u8], at: usize| {
        let mut rgba = [0_u8; 4];
        rgba.copy_from_slice(&whole[at * 4..at * 4 + 4]);
        rgba
    };
    let mut sum = 0.0;
    let mut counted = 0_u32;
    for at in pixels {
        let (was, now, came) = (pixel(from, *at), pixel(frame, *at), pixel(to, *at));
        let mut span = 0.0;
        let mut kept = 0.0;
        for channel in 0..3 {
            let whole = f64::from(was[channel]) - f64::from(came[channel]);
            span += whole * whole;
            kept += whole * (f64::from(now[channel]) - f64::from(came[channel]));
        }
        if span <= 0.0 {
            continue;
        }
        sum += (kept / span).clamp(0.0, 1.0);
        counted = counted.saturating_add(1);
    }
    if counted == 0 {
        0.0
    } else {
        sum / f64::from(counted)
    }
}

/// The display the staged shell draws on: a scenario names the windows it holds, not the screen
/// their row is measured on, so a test that reads geometry stages it as a part is told of its
/// output.
fn stage_screen(window: &StagedWindow) {
    let host = window.instance.host().expect("the shell's host");
    crate::fixtures::set_reading(
        &host,
        "session:screen",
        &serde_json::json!({
            "output": 0,
            "connector": "HEADLESS-1",
            "name": "Built-in",
            "x": 0,
            "y": 0,
            "width": WINDOW.0,
            "height": WINDOW.1,
            "scale": 1.0,
            "primary": true
        }),
    );
}

/// How wide the window stands at the row `y`: the longest run of pixels of a colour other than
/// the one most of the row shows, which is the wallpaper around it.
fn window_width(frame: &[u8], y: u32) -> usize {
    let width = usize::try_from(WINDOW.0).unwrap_or(1);
    let row = |x: usize| {
        let at = (usize::try_from(y).unwrap_or(0) * width + x) * 4;
        [frame[at], frame[at + 1], frame[at + 2]]
    };
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for x in 0..width {
        *counts.entry(row(x)).or_default() += 1;
    }
    let ground = counts
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(colour, _)| *colour);
    let mut longest = 0;
    let mut run = 0;
    for x in 0..width {
        if Some(row(x)) != ground {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// An 8-bit sRGB channel as light: the renderer mixes premultiplied colour.
fn light(byte: u8) -> f64 {
    let value = f64::from(byte) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The share `t` of the way from `before` to `after` the `frame` stands at, read off the pixels
/// where the two differ, and the furthest any body pixel stands from the linear mix of the two at
/// that share: a true cross-fade mixes the two pages, whatever translucency their grounds have.
fn mix_of(
    frame: &[u8],
    before: &[u8],
    after: &[u8],
    content: &[usize],
) -> (
    f64,
    f64,
    (u32, u32),
    usize,
    (u32, u32, u32, u32),
    Vec<(u32, u32, [u8; 3], [u8; 3], [u8; 3])>,
) {
    let mut num = 0.0;
    let mut den = 0.0;
    for at in content {
        for channel in 0..3 {
            let was = light(before[at * 4 + channel]);
            let came = light(after[at * 4 + channel]);
            let now = light(frame[at * 4 + channel]);
            num += (came - was) * (now - was);
            den += (came - was) * (came - was);
        }
    }
    let t = if den > 0.0 {
        (num / den).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let width = usize::try_from(WINDOW.0).unwrap_or(1);
    let mut worst: f64 = 0.0;
    let mut worst_at = (0_u32, 0_u32);
    let mut over = 0_usize;
    let mut box_ = (u32::MAX, u32::MAX, 0_u32, 0_u32);
    let mut samples: Vec<(u32, u32, [u8; 3], [u8; 3], [u8; 3])> = Vec::new();
    for at in 0..frame.len() / 4 {
        let y = u32::try_from(at / width).unwrap_or(0);
        if y < CONTENT_TOP {
            continue;
        }
        for channel in 0..3 {
            let was = light(before[at * 4 + channel]);
            let came = light(after[at * 4 + channel]);
            let now = light(frame[at * 4 + channel]);
            let off = (now - (was + t * (came - was))).abs();
            if off > 0.02 {
                over = over.saturating_add(1);
                let x = u32::try_from(at % width).unwrap_or(0);
                let _ = &mut box_;
                box_ = (box_.0.min(x), box_.1.min(y), box_.2.max(x), box_.3.max(y));
                if samples.len() < 8 {
                    let mut rgb =
                        |whole: &[u8]| [whole[at * 4], whole[at * 4 + 1], whole[at * 4 + 2]];
                    samples.push((x, y, rgb(before), rgb(frame), rgb(after)));
                }
            }
            if off > worst {
                worst = off;
                worst_at = (u32::try_from(at % width).unwrap_or(0), y);
            }
        }
    }
    (t, worst, worst_at, over, box_, samples)
}

#[test]
fn the_midpoint_of_a_tab_swap_in_both_themes() {
    let start = ui::time::now();
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    for (name, theme) in [
        ("fade-dark", tilo_ui_base::Theme::dark()),
        ("fade-light", tilo_ui_base::Theme::light()),
    ] {
        // One window, taken through a swap and back: the two tabs' content settled, and then the
        // frames of the way back, where the one the person leaves fades out under the one they
        // choose.
        let mut window = StagedWindow::staged(SCENARIO, WINDOW, theme);
        stage_screen(&window);
        for _ in 0..4 {
            window.frame_at(ui::time::now());
        }
        let first = shown_pixels(&window);
        choose(&mut window, start, "p-1");
        settle(&mut window);
        let second = shown_pixels(&window);
        let content = changed_content(&first, &second);
        assert!(
            content.len() > 1_000,
            "the two tabs show different content: {} pixels",
            content.len()
        );
        let wide = window_width(&second, CONTENT_TOP + 40);
        assert!(
            wide > 300,
            "the window stands full, not a sliver: {wide} pixels wide"
        );

        // The frames of the way back, read as they are drawn: the one nearest halfway across the
        // fade has the content the window showed drawn about half as strongly as it stood, under
        // the one that comes, and that frame is the capture. The choice runs one frame only, so
        // the frames that follow are the fade and not a motion spent before they begin.
        let back = ui::time::now();
        ui::time::testing::with_monotonic_time(back, || {
            tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
            show_place(&window, "p-0");
        });
        window.instance.store_moved();
        window.frame_at(back);
        let strength = |frame: &[u8]| old_strength(frame, &second, &first, &content);
        let path = out.join(format!("{name}.png"));
        let mut best: Option<(f64, Vec<u8>)> = None;
        let mut seen = Vec::new();
        for _ in 0..60 {
            window.frame_at(ui::time::now());
            let frame = shown_pixels(&window);
            let there = strength(&frame);
            seen.push((there * 100.0).round() / 100.0);
            if best
                .as_ref()
                .is_none_or(|(closest, _)| (there - 0.5).abs() < (closest - 0.5).abs())
            {
                // The frame this draw left is the scene, so it is the capture.
                window.harness.render_png(&path);
                best = Some((there, frame));
            }
            if there < 0.2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(6));
        }
        let (there, capture) = best.expect("frames of the way back");
        assert!(
            (0.2..=0.8).contains(&there),
            "the capture stands mid-fade, the old content about half drawn: {there:.2}, of {seen:?}"
        );
        let (mix, worst, worst_at, over, box_, samples) =
            mix_of(&capture, &second, &first, &content);
        eprintln!(
            "mix {mix:.3} over {over} bbox {box_:?} worst {worst:.3} at {worst_at:?} samples \
             {samples:?}"
        );
        assert!(
            (0.2..=0.8).contains(&mix),
            "the capture stands at the middle of the fade: {mix:.2} of the way from one tab to \\
             the other"
        );
        assert!(
            over <= 256,
            "the body is the linear mix of the two tabs at {mix:.2}: {over} pixels of it are \
             not, the furthest {worst:.4} away at {worst_at:?}"
        );
    }
    for name in ["fade-dark", "fade-light"] {
        let path = out.join(format!("{name}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the capture is written: {}",
            path.display()
        );
    }
}

/// Where the captures are written: the directory the check names (`TILO_SHELL_TAB_SHOTS`), or
/// else beside the test's built binary, as the other shots are, since a source tree read from a
/// test may be read-only.
fn captures() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("TILO_SHELL_TAB_SHOTS") {
        return std::path::PathBuf::from(dir);
    }
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("shell-tab-fade-shot")
}

#[test]
fn reduced_motion_swaps_the_tabs_in_one_frame() {
    let start = ui::time::now();
    let mut window = staged(true);
    let before = shown_pixels(&window);
    choose(&mut window, start, "p-1");
    // The frames after the layout has settled on the tab chosen: the frame the new content
    // finds its place in is not a fade, and nothing between the tabs is drawn after it.
    let mut frames = Vec::new();
    for step in 7..12 {
        window.frame_at(start + FRAME * step);
        frames.push(shown_pixels(&window));
    }
    let played = start + Duration::from_secs(1);
    window.frame_at(played);
    let after = shown_pixels(&window);
    let content = changed_content(&before, &after);
    assert!(
        content.len() > 1_000,
        "the tab that came shows its content: {} pixels",
        content.len()
    );
    // Every frame since the choice is one colour or the other: nothing draws between them.
    for (at, frame) in frames.iter().enumerate() {
        let between = blended(frame, &before, &after, &content);
        assert!(
            between <= content.len() / 100,
            "reduced motion draws nothing between the two tabs, at step {}: {between} of {}",
            at + 7,
            content.len()
        );
    }
}
