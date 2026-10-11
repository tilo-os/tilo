//! Strip pan and strip-to-strip frame cost.
//!
//! Drives the two signature desktop motions through the production frame
//! producer and GPU target, one frame at a time:
//!
//! - `pan`: a continuous three-finger pan of the top strip
//!   (`update_active_strip_pan`, total travel growing by a fractional step
//!   per frame, as a touchpad delivers it), back and forth over the strip,
//!   on a one-strip desktop or with a second strip below it (the docked
//!   wheel, which rides the top strip, then reaches over the second), on a
//!   display at 100 %, 150 % or 200 % scale;
//! - `strip-move`: on a two-strip desktop, focus moving to the next strip and
//!   back (`ChromeState::switch_strip`), every frame until the motion settles.
//!
//! Each frame runs `FrameBuilder::relayout`, `FrameBuilder::paint_with_damage`
//! and `RetainedGpuTarget::render_until` (the DRM direct path's render, with
//! the frame's damage) and records the time of each, and the pixels the frame
//! asks the renderer to rasterise: the area of its damage rects, or the whole
//! output for a dense frame.
//!
//! Every motion starts from a desktop at rest: two frames in a row that
//! change nothing, with nothing animating, each kept rather than rendered as
//! the DRM presenter keeps it. Once at rest the layered presenter warms its
//! strip layers as the session does when the desktop rests after a frame
//! that changed it (`FrameBuilder::layers_at_rest`,
//! `RetainedGpuTarget::warm_layers`); that idle work is reported apart
//! (`warm`, `rewarm`: its time and raster pixels), never as a frame. A pan
//! also reports, apart from its samples, its first frame from rest (`start`)
//! and the first frame of a second pan once the desktop came to rest again
//! (`restart`), each with its scene and render time and render attempts.
//!
//! The windows are the in-process apps the populated fixture supports
//! (Notes, Files, Terminal, Document): vector content the compositor draws
//! itself, the case where a pan costs most.
//!
//! ```text
//! cargo run -p shell --example strip_pan_bench --release -- \
//!     [--out DIR] [--frames N] [--size 1080p|4k] [--windows N] [--mode dense|layered] \
//!     [--scale 1|1.5|2] [--strips 1|2]
//! ```
//!
//! Without a selection the cases are 1080p and 4K, three and six windows, at
//! 100 % on one strip (pans and strip moves), then 4K with six windows at
//! 150 % and 200 % on one strip (pans and strip moves) and at 100 % and 200 %
//! on two strips (pans).
//! A selection flag runs every combination of the values it names with the
//! defaults of the others. `--size` names the panel in device pixels; at a
//! scale the desktop is laid out at the panel's size divided by it.
//!
//! Two presenters are measured on the same scenes:
//!
//! - `dense`: the strip drawn in screen space, every frame rendered from the
//!   scene with its damage (the presenter before strip layers);
//! - `layered`: the DRM GPU presenter, which gets a panning strip as layers
//!   (`FrameBuilder::set_strip_pan_presenter`) and composites the pan from
//!   them (`RetainedGpuTarget::render_layers_until`).
//!
//! On a runner without a GPU, Mesa's lavapipe renders: the render time is
//! then the CPU's rasterisation and is only comparable with itself.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pollster::FutureExt as _;
use shell::compositor::backend::drm::paint::{RetainedGpuTarget, SceneDamage};
use shell::compositor::frame::{FrameBuilder, FramePaint};
use shell::shell::output_scene::OutputGeometry;
use shell::shell::state::{StripId, WindowGroup, WindowWidth, WorkspaceTree};
use vello::kurbo::Rect;

/// Frames a pan measures, after the warm-up.
const PAN_FRAMES: usize = 240;
/// Frames that warm the renderer up for the moving scene, not measured.
const WARM_UP_FRAMES: usize = 8;
/// Strip moves measured; each runs until the motion settles.
const STRIP_MOVES: usize = 8;
/// Longest a strip move may animate, in frames.
const MAX_MOVE_FRAMES: usize = 120;
/// Most frames the desktop may take to come to rest before a motion.
const SETTLE_FRAMES: usize = 60;
/// Frames in a row that change nothing, with nothing animating, that make
/// the desktop at rest.
const QUIET_FRAMES: usize = 2;
/// Layout pixels a pan travels per frame: about 1,000 px/s at 60 Hz, never
/// a whole pixel, as a touchpad reports it.
const PAN_STEP: f64 = 17.3;
const RENDER_DEADLINE: Duration = Duration::from_secs(10);

struct Args {
    out: Option<PathBuf>,
    frames: usize,
    cases: Vec<Case>,
    modes: Vec<&'static str>,
}

/// One desktop pans are measured on.
#[derive(Clone, Copy)]
struct Case {
    size_name: &'static str,
    /// The panel, in device pixels.
    size: (u32, u32),
    windows: usize,
    /// Device pixels per layout pixel.
    scale: f64,
    /// Strips on the desktop; the top one pans.
    strips: usize,
    /// Also measure strip-to-strip moves.
    strip_moves: bool,
}

impl Case {
    /// `pan.4k.w6.layered` at 100 % on one strip, the label these records
    /// had before scales and strips were measured;
    /// `pan.4k.s200.strips2.w6.layered` otherwise.
    fn label(&self, motion: &str, mode: &str) -> String {
        let mut parts = vec![motion.to_owned(), self.size_name.to_owned()];
        if (self.scale - 1.0).abs() > f64::EPSILON {
            parts.push(format!("s{}", (self.scale * 100.0).round()));
        }
        if self.strips > 1 {
            parts.push(format!("strips{}", self.strips));
        }
        parts.push(format!("w{}", self.windows));
        parts.push(mode.to_owned());
        parts.join(".")
    }
}

const FOUR_K: (&str, (u32, u32)) = ("4k", (3840, 2160));
const SIZES: [(&str, (u32, u32)); 2] = [("1080p", (1920, 1080)), FOUR_K];

/// Every combination of the selected values, the defaults standing in for
/// what was not selected.
fn selected_cases(
    sizes: Option<&[(&'static str, (u32, u32))]>,
    windows: Option<&[usize]>,
    scales: Option<&[f64]>,
    strips: Option<&[usize]>,
) -> Vec<Case> {
    let mut cases = Vec::new();
    for &(size_name, size) in sizes.unwrap_or(&SIZES) {
        for &windows in windows.unwrap_or(&[3, 6]) {
            for &scale in scales.unwrap_or(&[1.0]) {
                for &strips in strips.unwrap_or(&[1]) {
                    cases.push(Case {
                        size_name,
                        size,
                        windows,
                        scale,
                        strips,
                        strip_moves: strips == 1,
                    });
                }
            }
        }
    }
    cases
}

/// 1080p and 4K with three and six windows at 100 % on one strip (pans and
/// strip moves), then 4K with six windows at 150 % and 200 % on one strip
/// (pans and strip moves) and at 100 % and 200 % on two strips (pans).
fn default_cases() -> Vec<Case> {
    let mut cases = selected_cases(None, None, None, None);
    let (size_name, size) = FOUR_K;
    for (scale, strips) in [(1.5, 1), (2.0, 1), (1.0, 2), (2.0, 2)] {
        cases.push(Case {
            size_name,
            size,
            windows: 6,
            scale,
            strips,
            // Strip moves need a second strip of their own: measured from
            // the one-strip cases.
            strip_moves: strips == 1,
        });
    }
    cases
}

fn parse_args() -> Result<Args, String> {
    let mut out = None;
    let mut frames = PAN_FRAMES;
    let mut sizes: Option<Vec<(&'static str, (u32, u32))>> = None;
    let mut windows: Option<Vec<usize>> = None;
    let mut scales: Option<Vec<f64>> = None;
    let mut strips: Option<Vec<usize>> = None;
    let mut modes = vec!["dense", "layered"];
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = Some(it.next().ok_or("--out needs a directory")?.into()),
            "--frames" => {
                frames = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--frames needs a count")?;
            }
            "--size" => {
                let size = it.next().ok_or("--size needs 1080p or 4k")?;
                let found = SIZES
                    .into_iter()
                    .find(|(name, _)| *name == size)
                    .ok_or("--size must be 1080p or 4k")?;
                sizes.get_or_insert_with(Vec::new).push(found);
            }
            "--windows" => {
                let count: usize = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--windows needs a count")?;
                windows.get_or_insert_with(Vec::new).push(count);
            }
            "--scale" => {
                let scale: f64 = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|scale: &f64| (1.0..=3.0).contains(scale))
                    .ok_or("--scale needs a factor from 1 to 3")?;
                scales.get_or_insert_with(Vec::new).push(scale);
            }
            "--strips" => {
                let count: usize = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|count| (1..=2).contains(count))
                    .ok_or("--strips needs 1 or 2")?;
                strips.get_or_insert_with(Vec::new).push(count);
            }
            "--mode" => {
                let mode = it.next().ok_or("--mode needs dense or layered")?;
                modes.retain(|name| *name == mode);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if modes.is_empty() {
        return Err("--mode must be dense or layered".into());
    }
    let cases = if sizes.is_some() || windows.is_some() || scales.is_some() || strips.is_some() {
        selected_cases(
            sizes.as_deref(),
            windows.as_deref(),
            scales.as_deref(),
            strips.as_deref(),
        )
    } else {
        default_cases()
    };
    Ok(Args {
        out,
        frames,
        cases,
        modes,
    })
}

/// The demo desktop with `windows` window groups on its first strip, each
/// showing another app, and with `second_strip` a strip of two groups below
/// it.
fn tree(windows: usize, second_strip: bool) -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let Some(workspace) = tree.desktop.active_workspace_mut() else {
        return tree;
    };
    let Some(first) = workspace.strips.first_mut() else {
        return tree;
    };
    let Some(template) = first.window_groups.first().cloned() else {
        return tree;
    };
    let tabs = template.tabs.len().max(1);
    let mut ids = 0x5ea1_0000_u64..;
    let mut group = |index: usize| -> WindowGroup {
        let mut copy = template.clone();
        copy.id = ids.next().unwrap_or_default();
        for tab in &mut copy.tabs {
            tab.id = ids.next().unwrap_or_default();
        }
        let shown = index.checked_rem(tabs).unwrap_or(0);
        copy.active_tab = copy.tabs.get(shown).map_or(0, |tab| tab.id);
        copy.width = if index.is_multiple_of(2) {
            WindowWidth::Wide
        } else {
            WindowWidth::Normal
        };
        copy
    };
    first.window_groups = (0..windows).map(&mut group).collect();
    if !second_strip {
        tree.seed_doc_from_cache();
        return tree;
    }
    let mut second = first.clone();
    second.id = StripId(0x5ea1_ffff);
    second.name = "Second".into();
    second.window_groups = (1..3).map(&mut group).collect();
    workspace.strips.push(second);
    tree.seed_doc_from_cache();
    tree
}

struct Fixture {
    frame: FrameBuilder,
    /// Never read: the fixture's Files directory, kept until the frame drops.
    _parent: tempfile::TempDir,
}

impl Fixture {
    fn new(case: &Case, layered: bool, second_strip: bool) -> Result<Self, String> {
        let parent = tempfile::tempdir().map_err(|error| format!("fixture parent: {error}"))?;
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog)
            .map_err(|error| format!("fixture fonts: {error}"))?;
        let inputs = shell::shell::fixture_inputs::ShellFixtureInputs::new(
            parent.path(),
            renderer,
            ui::theme::Theme::dark(),
        )
        .map_err(|error| format!("fixture files: {error}"))?;
        let mut frame =
            FrameBuilder::with_fixture_inputs(tree(case.windows, second_strip), case.size, inputs)
                .map_err(|error| format!("fixture: {error}"))?;
        if (case.scale - 1.0).abs() > f64::EPSILON {
            // One panel at `scale`: the desktop is laid out at the panel's
            // size divided by it and magnified into the panel's pixels.
            let (width, height) = (f64::from(case.size.0), f64::from(case.size.1));
            frame.set_output_geometry(vec![OutputGeometry {
                id: 0,
                monitor: String::new(),
                x: 0.0,
                y: 0.0,
                width: (width / case.scale).round(),
                height: (height / case.scale).round(),
                scale: case.scale,
                scene: Rect::new(0.0, 0.0, width, height),
            }]);
        }
        frame.set_strip_pan_presenter(false, layered);
        Ok(Self {
            frame,
            _parent: parent,
        })
    }
}

#[derive(Default)]
struct Samples {
    layout_us: Vec<u64>,
    scene_us: Vec<u64>,
    render_us: Vec<u64>,
    frame_us: Vec<u64>,
    pixels: Vec<u64>,
    composited: Vec<u64>,
    paths: Vec<u64>,
    /// Vello submissions per frame, sparse and dense (bump-budget retries
    /// included).
    attempts: Vec<(usize, usize)>,
    dense: usize,
    layered: usize,
    /// The idle work before the motion: the presenter warming its layers
    /// once the desktop rested. Time and raster pixels.
    warm: Option<(u64, u64)>,
    /// The same before the second pan.
    rewarm: Option<(u64, u64)>,
    /// A pan's first frame from rest, before the warm-up.
    start: Option<One>,
    /// The first frame of a second pan, once the desktop came to rest after
    /// the first.
    restart: Option<One>,
}

/// One frame measured on its own.
#[derive(Clone, Copy)]
struct One {
    frame_us: u64,
    scene_us: u64,
    render_us: u64,
    pixels: u64,
    composited: u64,
    attempts: (usize, usize),
}

impl One {
    /// The only frame `samples` holds.
    fn of(samples: &Samples) -> Option<Self> {
        Some(Self {
            frame_us: *samples.frame_us.first()?,
            scene_us: *samples.scene_us.first()?,
            render_us: *samples.render_us.first()?,
            pixels: *samples.pixels.first()?,
            composited: *samples.composited.first()?,
            attempts: *samples.attempts.first()?,
        })
    }

    /// ` name[frame_us=… …]`.
    fn report(name: &str, one: Option<Self>) -> String {
        one.map_or_else(String::new, |one| {
            format!(
                " {name}[frame_us={} scene_us={} render_us={} pixels={} composited={} \
                 attempts={}/{}]",
                one.frame_us,
                one.scene_us,
                one.render_us,
                one.pixels,
                one.composited,
                one.attempts.0,
                one.attempts.1
            )
        })
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn output_pixels(size: (u32, u32)) -> u64 {
    u64::from(size.0).saturating_mul(u64::from(size.1))
}

/// The area of the frame's damage rects, overlap counted twice (which only
/// overstates the cost), or the whole output for a dense frame.
fn damage_pixels(paint: &FramePaint, size: (u32, u32)) -> u64 {
    let full = output_pixels(size);
    paint.damage_regions.as_ref().map_or(full, |regions| {
        regions
            .iter()
            .map(|rect| u64::from(rect.width).saturating_mul(u64::from(rect.height)))
            .fold(0_u64, u64::saturating_add)
            .min(full)
    })
}

/// Lay out, produce and render one frame; record it when `samples` is given.
/// `true` when the frame changed nothing on screen.
fn frame(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
    samples: Option<&mut Samples>,
) -> Result<bool, String> {
    let started = Instant::now();
    fixture.frame.relayout();
    let laid_out = Instant::now();
    let paint = fixture.frame.paint_with_damage();
    let produced = Instant::now();
    let deadline = produced
        .checked_add(RENDER_DEADLINE)
        .ok_or("render deadline overflows")?;
    let cache = paint
        .cache
        .as_ref()
        .filter(|cache| target.layers_eligible(cache));
    let damage = SceneDamage {
        revision: paint.revision,
        previous_revision: paint.previous_revision,
        regions: paint.damage_regions.as_deref(),
    };
    // A frame that changes nothing is kept, not rendered, as the DRM
    // presenter keeps it: motions are measured from a desktop at rest the
    // way a session reaches it.
    if cache.is_none() && samples.is_none() && target.keep_unchanged(damage) {
        return Ok(true);
    }
    let stats = match cache {
        Some(cache) => target.render_layers_until(cache, paint.revision, deadline, |_, _, _| {}),
        None => target.render_until(&paint.scene, damage, deadline),
    }
    .map_err(|error| format!("render: {error}"))?;
    let rendered = Instant::now();
    if let Some(samples) = samples {
        samples
            .layout_us
            .push(micros(laid_out.saturating_duration_since(started)));
        samples
            .scene_us
            .push(micros(produced.saturating_duration_since(laid_out)));
        samples
            .render_us
            .push(micros(rendered.saturating_duration_since(produced)));
        samples
            .frame_us
            .push(micros(rendered.saturating_duration_since(started)));
        let layer_work = cache.and_then(|_| target.last_layer_raster_pixels());
        if let Some((raster, composited)) = layer_work {
            samples.layered = samples.layered.saturating_add(1);
            samples.pixels.push(raster);
            samples.composited.push(composited);
        } else {
            let dense = stats.full_attempts > 0 || !stats.damage_admitted;
            samples.dense = samples.dense.saturating_add(usize::from(dense));
            samples.pixels.push(if dense {
                output_pixels(size)
            } else {
                damage_pixels(&paint, size)
            });
            samples.composited.push(0);
        }
        samples
            .paths
            .push(u64::from(paint.scene.encoding().n_paths));
        samples
            .attempts
            .push((stats.sparse_attempts, stats.full_attempts));
    }
    Ok(paint.damage_regions.as_ref().is_some_and(Vec::is_empty))
}

/// The idle work the session does once the desktop rests after a frame that
/// changed it: the producer draws the desktop as layers and the presenter
/// warms its layer textures from them. Its time and raster pixels; `None`
/// when there was nothing to warm.
fn warm(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
) -> Result<Option<(u64, u64)>, String> {
    if !fixture.frame.layers_stale() {
        return Ok(None);
    }
    let started = Instant::now();
    let Some(layers) = fixture.frame.layers_at_rest() else {
        return Ok(None);
    };
    let deadline = started
        .checked_add(RENDER_DEADLINE)
        .ok_or("render deadline overflows")?;
    let pixels = target
        .warm_layers(&layers, deadline, &|| true)
        .map_err(|error| format!("warm: {error}"))?;
    Ok(pixels.map(|pixels| (micros(started.elapsed()), pixels)))
}

/// Draw frames until the desktop is at rest: [`QUIET_FRAMES`] frames in a
/// row that change nothing, with nothing animating. Nothing animating alone
/// is not rest: the in-process apps paint their first content a frame or two
/// after the desktop's first, so the next frame would repaint the whole
/// output whatever the motion measured after it.
fn settle(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
) -> Result<(), String> {
    let mut quiet = 0_usize;
    for _ in 0..SETTLE_FRAMES {
        let unchanged = frame(fixture, target, size, None)?;
        quiet = if unchanged && !fixture.frame.needs_animation_frame() {
            quiet.saturating_add(1)
        } else {
            0
        };
        if quiet >= QUIET_FRAMES {
            return Ok(());
        }
    }
    Err(format!(
        "the desktop did not come to rest in {SETTLE_FRAMES} frames"
    ))
}

fn active_strip_max_scroll(frame: &FrameBuilder) -> f64 {
    let active = frame.chrome().active_strip_id();
    frame
        .host()
        .strip_scroll_offsets()
        .into_iter()
        .find(|(id, _, _)| active.is_some_and(|strip| strip.0 == *id))
        .map_or(0.0, |(_, _, max)| max)
}

fn pan(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
    frames: usize,
) -> Result<Samples, String> {
    settle(fixture, target, size)?;
    let max = active_strip_max_scroll(&fixture.frame);
    if max < PAN_STEP * 4.0 {
        return Err(format!("the strip does not pan: max scroll {max}"));
    }
    let mut samples = Samples {
        warm: warm(fixture, target)?,
        ..Samples::default()
    };
    let mut travel = 0.0;
    let mut direction = 1.0;
    fixture.frame.host_mut().update_active_strip_pan(0.0, true);
    for index in 0..frames.saturating_add(WARM_UP_FRAMES) {
        travel += direction * PAN_STEP;
        if travel >= max || travel <= 0.0 {
            direction = -direction;
            travel = travel.clamp(0.0, max);
        }
        fixture
            .frame
            .host_mut()
            .update_active_strip_pan(travel, false);
        if index == 0 {
            samples.start = single_frame(fixture, target, size)?;
            continue;
        }
        let _ = frame(
            fixture,
            target,
            size,
            (index >= WARM_UP_FRAMES).then_some(&mut samples),
        )?;
    }
    fixture
        .frame
        .host_mut()
        .end_active_strip_pan(Instant::now());
    settle(fixture, target, size)?;
    samples.rewarm = warm(fixture, target)?;
    fixture.frame.host_mut().update_active_strip_pan(0.0, true);
    fixture
        .frame
        .host_mut()
        .update_active_strip_pan(PAN_STEP, false);
    samples.restart = single_frame(fixture, target, size)?;
    fixture
        .frame
        .host_mut()
        .end_active_strip_pan(Instant::now());
    Ok(samples)
}

/// One frame, measured on its own.
fn single_frame(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
) -> Result<Option<One>, String> {
    let mut one = Samples::default();
    let _ = frame(fixture, target, size, Some(&mut one))?;
    Ok(One::of(&one))
}

fn strip_move(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
) -> Result<Samples, String> {
    settle(fixture, target, size)?;
    let mut samples = Samples {
        warm: warm(fixture, target)?,
        ..Samples::default()
    };
    for index in 0..STRIP_MOVES {
        let direction = if index.is_multiple_of(2) { 1 } else { -1 };
        let mut moved = false;
        fixture
            .frame
            .with_chrome(|chrome| moved = chrome.switch_strip(direction));
        if !moved {
            return Err("the fixture has no second strip to move to".into());
        }
        for _ in 0..MAX_MOVE_FRAMES {
            let _ = frame(fixture, target, size, Some(&mut samples))?;
            if !fixture.frame.needs_animation_frame() {
                break;
            }
        }
    }
    Ok(samples)
}

/// Nearest-rank percentile, `permille` of the way up the sorted samples.
fn percentile(values: &[u64], permille: usize) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = sorted
        .len()
        .saturating_mul(permille)
        .div_ceil(1000)
        .clamp(1, sorted.len().max(1));
    sorted.get(rank.saturating_sub(1)).copied().unwrap_or(0)
}

/// `part` as a percentage of `whole`, to one decimal.
fn percent(part: u64, whole: u64) -> String {
    let tenths = part.saturating_mul(1000).checked_div(whole).unwrap_or(0);
    format!(
        "{}.{}",
        tenths.checked_div(10).unwrap_or(0),
        tenths.checked_rem(10).unwrap_or(0)
    )
}

fn summary(values: &[u64]) -> String {
    format!(
        "p50={} p99={} max={}",
        percentile(values, 500),
        percentile(values, 990),
        values.iter().max().copied().unwrap_or(0)
    )
}

/// ` name[us=… pixels=…]` for idle work, if any was done.
fn idle_work(name: &str, measured: Option<(u64, u64)>) -> String {
    measured.map_or_else(String::new, |(us, pixels)| {
        format!(" {name}[us={us} pixels={pixels}]")
    })
}

fn report(label: &str, size: (u32, u32), samples: &Samples) {
    let output = output_pixels(size);
    let pixels_p50 = percentile(&samples.pixels, 500);
    let first = samples.attempts.first().copied().unwrap_or_default();
    println!(
        "{label}: frames={} dense={} layered={} frame_us[{}] layout_us[{}] scene_us[{}] \
         render_us[{}] pixels[p50={pixels_p50} p99={} output={output} share_p50={}%] \
         composited_p50={} paths_p50={} first[frame_us={} pixels={} attempts={}/{}]{}{}{}{}",
        samples.frame_us.len(),
        samples.dense,
        samples.layered,
        summary(&samples.frame_us),
        summary(&samples.layout_us),
        summary(&samples.scene_us),
        summary(&samples.render_us),
        percentile(&samples.pixels, 990),
        percent(pixels_p50, output),
        percentile(&samples.composited, 500),
        percentile(&samples.paths, 500),
        samples.frame_us.first().copied().unwrap_or(0),
        samples.pixels.first().copied().unwrap_or(0),
        first.0,
        first.1,
        idle_work("warm", samples.warm),
        One::report("start", samples.start),
        idle_work("rewarm", samples.rewarm),
        One::report("restart", samples.restart),
    );
}

fn write_samples(dir: &Path, label: &str, samples: &Samples) -> Result<(), String> {
    // A pan's first frames from rest, the first pan's and the second's.
    let starts: Vec<One> = samples.start.into_iter().chain(samples.restart).collect();
    let start_us: Vec<u64> = starts.iter().map(|one| one.frame_us).collect();
    let start_pixels: Vec<u64> = starts.iter().map(|one| one.pixels).collect();
    for (name, values) in [
        ("frame-us", &samples.frame_us),
        ("scene-us", &samples.scene_us),
        ("render-us", &samples.render_us),
        ("pixels", &samples.pixels),
        ("composited", &samples.composited),
        ("start-us", &start_us),
        ("start-pixels", &start_pixels),
    ] {
        if values.is_empty() {
            continue;
        }
        let mut text = values
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        std::fs::write(dir.join(format!("{label}.{name}.txt")), text)
            .map_err(|error| format!("write samples: {error}"))?;
    }
    Ok(())
}

fn run_case(
    gpu: &ui::gpu::DeviceHandle,
    case: &Case,
    mode: &str,
    args: &Args,
) -> Result<(), String> {
    let size = case.size;
    let mut fixture = Fixture::new(case, mode == "layered", case.strips > 1)?;
    let mut target =
        RetainedGpuTarget::new(gpu, size.0, size.1).map_err(|error| format!("target: {error}"))?;
    let label = case.label("pan", mode);
    let samples = pan(&mut fixture, &mut target, size, args.frames)?;
    drop(fixture);
    report(&label, size, &samples);
    if let Some(dir) = &args.out {
        write_samples(dir, &label, &samples)?;
    }
    if !case.strip_moves {
        return Ok(());
    }
    let label = case.label("strip-move", mode);
    let mut fixture = Fixture::new(case, mode == "layered", true)?;
    let samples = strip_move(&mut fixture, &mut target, size)?;
    drop(fixture);
    report(&label, size, &samples);
    if let Some(dir) = &args.out {
        write_samples(dir, &label, &samples)?;
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    if let Some(dir) = &args.out {
        std::fs::create_dir_all(dir).map_err(|error| format!("--out: {error}"))?;
    }
    let mut context = ui::gpu::RenderContext::new();
    let device = context
        .device(None)
        .block_on()
        .ok_or("no wgpu adapter (install vulkan/lavapipe drivers)")?;
    let gpu = context
        .devices
        .get(device)
        .ok_or("wgpu device id out of range")?;
    let adapter = gpu.adapter.get_info();
    println!(
        "adapter: name={:?} backend={:?} driver={:?}",
        adapter.name, adapter.backend, adapter.driver
    );
    for case in &args.cases {
        for &mode in &args.modes {
            run_case(gpu, case, mode, &args)?;
        }
    }
    Ok(())
}
