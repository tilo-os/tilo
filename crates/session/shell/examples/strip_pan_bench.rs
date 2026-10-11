//! Strip pan and strip-to-strip frame cost.
//!
//! Drives the two signature desktop motions through the production frame
//! producer and GPU target, one frame at a time:
//!
//! - `pan`: a continuous three-finger pan of the strip of a one-strip desktop
//!   (`update_active_strip_pan`, total travel growing by a fractional step
//!   per frame, as a touchpad delivers it), back and forth over the strip;
//! - `strip-move`: on a two-strip desktop, focus moving to the next strip and
//!   back (`ChromeState::switch_strip`), every frame until the motion settles.
//!
//! Each frame runs `FrameBuilder::relayout`, `FrameBuilder::paint_with_damage`
//! and `RetainedGpuTarget::render_until` (the DRM direct path's render, with
//! the frame's damage) and records the time of each, and the pixels the frame
//! asks the renderer to rasterise: the area of its damage rects, or the whole
//! output for a dense frame.
//!
//! The windows are the in-process apps the populated fixture supports
//! (Notes, Files, Terminal, Document): vector content the compositor draws
//! itself, the case where a pan costs most.
//!
//! ```text
//! cargo run -p shell --example strip_pan_bench --release -- \
//!     [--out DIR] [--frames N] [--size 1080p|4k] [--windows N] [--mode dense|layered]
//! ```
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
use shell::shell::state::{StripId, WindowGroup, WindowWidth, WorkspaceTree};

/// Frames a pan measures, after the warm-up.
const PAN_FRAMES: usize = 240;
/// Frames that warm the renderer up for the moving scene, not measured.
const WARM_UP_FRAMES: usize = 8;
/// Strip moves measured; each runs until the motion settles.
const STRIP_MOVES: usize = 8;
/// Longest a strip move may animate, in frames.
const MAX_MOVE_FRAMES: usize = 120;
/// Layout pixels a pan travels per frame: about 1,000 px/s at 60 Hz, never
/// a whole pixel, as a touchpad reports it.
const PAN_STEP: f64 = 17.3;
const RENDER_DEADLINE: Duration = Duration::from_secs(10);

struct Args {
    out: Option<PathBuf>,
    frames: usize,
    sizes: Vec<(&'static str, (u32, u32))>,
    windows: Vec<usize>,
    modes: Vec<&'static str>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        out: None,
        frames: PAN_FRAMES,
        sizes: vec![("1080p", (1920, 1080)), ("4k", (3840, 2160))],
        windows: vec![3, 6],
        modes: vec!["dense", "layered"],
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => args.out = Some(it.next().ok_or("--out needs a directory")?.into()),
            "--frames" => {
                args.frames = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--frames needs a count")?;
            }
            "--size" => {
                let size = it.next().ok_or("--size needs 1080p or 4k")?;
                args.sizes.retain(|(name, _)| *name == size);
            }
            "--windows" => {
                let count: usize = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--windows needs a count")?;
                args.windows = vec![count];
            }
            "--mode" => {
                let mode = it.next().ok_or("--mode needs dense or layered")?;
                args.modes.retain(|name| *name == mode);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if args.sizes.is_empty() {
        return Err("--size must be 1080p or 4k".into());
    }
    if args.modes.is_empty() {
        return Err("--mode must be dense or layered".into());
    }
    Ok(args)
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
    fn new(
        size: (u32, u32),
        windows: usize,
        layered: bool,
        second_strip: bool,
    ) -> Result<Self, String> {
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
            FrameBuilder::with_fixture_inputs(tree(windows, second_strip), size, inputs)
                .map_err(|error| format!("fixture: {error}"))?;
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
    dense: usize,
    layered: usize,
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
fn frame(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
    samples: Option<&mut Samples>,
) -> Result<(), String> {
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
    let stats = match cache {
        Some(cache) => target.render_layers_until(cache, paint.revision, deadline, |_, _, _| {}),
        None => target.render_until(
            &paint.scene,
            SceneDamage {
                revision: paint.revision,
                previous_revision: paint.previous_revision,
                regions: paint.damage_regions.as_deref(),
            },
            deadline,
        ),
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
    }
    Ok(())
}

fn settle(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
) -> Result<(), String> {
    for _ in 0..30 {
        frame(fixture, target, size, None)?;
        if !fixture.frame.needs_animation_frame() {
            break;
        }
    }
    Ok(())
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
    let mut samples = Samples::default();
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
        frame(
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
    Ok(samples)
}

fn strip_move(
    fixture: &mut Fixture,
    target: &mut RetainedGpuTarget,
    size: (u32, u32),
) -> Result<Samples, String> {
    settle(fixture, target, size)?;
    let mut samples = Samples::default();
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
            frame(fixture, target, size, Some(&mut samples))?;
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

fn report(label: &str, size: (u32, u32), samples: &Samples) {
    let output = output_pixels(size);
    let pixels_p50 = percentile(&samples.pixels, 500);
    println!(
        "{label}: frames={} dense={} layered={} frame_us[{}] layout_us[{}] scene_us[{}] \
         render_us[{}] pixels[p50={pixels_p50} p99={} output={output} share_p50={}%] \
         composited_p50={} paths_p50={}",
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
    );
}

fn write_samples(dir: &Path, label: &str, samples: &Samples) -> Result<(), String> {
    for (name, values) in [
        ("frame-us", &samples.frame_us),
        ("scene-us", &samples.scene_us),
        ("render-us", &samples.render_us),
        ("pixels", &samples.pixels),
        ("composited", &samples.composited),
    ] {
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
    (size_name, size): (&str, (u32, u32)),
    (windows, mode): (usize, &str),
    args: &Args,
) -> Result<(), String> {
    // One strip pans; a second strip below it would put the docked wheel,
    // which rides the active strip, over the other strip's row, and those
    // pans stay in the scene.
    let mut fixture = Fixture::new(size, windows, mode == "layered", false)?;
    let mut target =
        RetainedGpuTarget::new(gpu, size.0, size.1).map_err(|error| format!("target: {error}"))?;
    let label = format!("pan.{size_name}.w{windows}.{mode}");
    let samples = pan(&mut fixture, &mut target, size, args.frames)?;
    drop(fixture);
    report(&label, size, &samples);
    if let Some(dir) = &args.out {
        write_samples(dir, &label, &samples)?;
    }
    let label = format!("strip-move.{size_name}.w{windows}.{mode}");
    let mut fixture = Fixture::new(size, windows, mode == "layered", true)?;
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
    for &size in &args.sizes {
        for &windows in &args.windows {
            for &mode in &args.modes {
                run_case(gpu, size, (windows, mode), &args)?;
            }
        }
    }
    Ok(())
}
