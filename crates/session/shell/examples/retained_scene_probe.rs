//! Diagnostic-only cold native-4K retained-scene probe.
//!
//! This is intentionally one frame, not an acceptance benchmark. It keeps the
//! first-paint order used by `redraw_bench` and reports dense/versioned bridge
//! behavior without warming a legacy scene first.

use std::time::Instant;

use shell::shell::frame::{FrameBuilder, FramePaint};
use shell::shell::render::paint::{SceneDamage, VelloDrmBridge};

const WIDTH: u32 = 3_840;
const HEIGHT: u32 = 2_160;

#[derive(Clone, Copy, Debug)]
enum Mode {
    LegacyDense,
    LegacyVersioned,
    SegmentedDense,
    SegmentedVersioned,
}

impl Mode {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let Some(mode) = args.next() else {
            return Err("usage: retained_scene_probe <legacy-dense|legacy-versioned|segmented-dense|segmented-versioned>".to_owned());
        };
        if args.next().is_some() {
            return Err("retained_scene_probe accepts exactly one mode".to_owned());
        }
        match mode.as_str() {
            "legacy-dense" => Ok(Self::LegacyDense),
            "legacy-versioned" => Ok(Self::LegacyVersioned),
            "segmented-dense" => Ok(Self::SegmentedDense),
            "segmented-versioned" => Ok(Self::SegmentedVersioned),
            _ => Err(format!("unknown retained_scene_probe mode {mode:?}")),
        }
    }

    fn is_segmented(self) -> bool {
        matches!(self, Self::SegmentedDense | Self::SegmentedVersioned)
    }

    fn is_versioned(self) -> bool {
        matches!(self, Self::LegacyVersioned | Self::SegmentedVersioned)
    }
}

fn main() -> Result<(), String> {
    let mode = Mode::parse()?;
    // Preserve the cold production ordering: construct the frame builder first, then the bridge,
    // then build and render exactly one scene without a preparatory legacy paint.
    let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).map_err(|error| error.to_string())?;
    bridge.set_phase_timing_enabled(true);
    let adapter = bridge.adapter_info().map_err(|error| error.to_string())?;
    println!(
        "retained_scene_probe diagnostic only: mode={mode:?} adapter name={:?} backend={} device_type={} driver={:?}",
        adapter.name, adapter.backend, adapter.device_type, adapter.driver
    );

    let scene_started = Instant::now();
    let frame_paint = if mode.is_segmented() {
        frame.paint_with_damage()
    } else {
        FramePaint::full(frame.paint())
    };
    let scene_elapsed = scene_started.elapsed();
    print_encoding(&frame_paint);

    let width = usize::try_from(WIDTH).map_err(|_| "width does not fit usize".to_owned())?;
    let height = usize::try_from(HEIGHT).map_err(|_| "height does not fit usize".to_owned())?;
    let pitch = width
        .checked_mul(4)
        .ok_or_else(|| "destination pitch overflow".to_owned())?;
    let len = pitch
        .checked_mul(height)
        .ok_or_else(|| "destination allocation overflow".to_owned())?;
    let mut destination = vec![0_u8; len];
    let bridge_started = Instant::now();
    let result = if mode.is_versioned() {
        // `regions: None` deliberately requests a full render. The first frame has no previous
        // revision, so this exercises versioned admission without claiming sparse reuse.
        bridge.paint_damage_into_xrgb8888(
            &frame_paint.scene,
            SceneDamage {
                revision: frame_paint.revision.max(1),
                previous_revision: None,
                regions: None,
            },
            &mut destination,
            pitch,
        )
    } else {
        bridge.paint_into_xrgb8888(&frame_paint.scene, &mut destination, pitch)
    };
    let bridge_elapsed = bridge_started.elapsed();
    println!(
        "cpu_scene_build_us={} bridge_elapsed_us={} result={result:?}",
        scene_elapsed.as_micros(),
        bridge_elapsed.as_micros()
    );
    println!(
        "bridge_attempts total={} sparse={} full={}",
        bridge.last_render_attempts(),
        bridge.last_sparse_render_attempts(),
        bridge.last_full_render_attempts()
    );
    if let Some(phase) = bridge.last_phase_timing() {
        println!(
            "phase_us submit={} completion_map={} settle={} cpu_copy_swizzle={} readback_payload_bytes={} cpu_output_bytes={}",
            phase.submit_us,
            phase.completed_readback_map_us,
            phase.settle_us,
            phase.cpu_copy_swizzle_us,
            phase.readback_bytes,
            phase.cpu_output_bytes,
        );
    }
    println!("diagnostic only: no acceptance gate, no multi-sample claim");
    result.map_err(|error| error.to_string())
}

fn print_encoding(frame: &FramePaint) {
    let encoding = frame.scene.encoding();
    let streams = encoding.stream_offsets();
    let resources = &encoding.resources;
    println!(
        "scene revision={} previous={:?} damage_regions={} streams path_tags={} path_data={} draw_tags={} draw_data={} transforms={} styles={} scalars paths={} segments={} clips={} open_clips={} resources patches={} color_stops={} glyphs={} glyph_runs={} normalized_coords={}",
        frame.revision,
        frame.previous_revision,
        frame.damage_regions.as_ref().map_or(0, Vec::len),
        streams.path_tags,
        streams.path_data,
        streams.draw_tags,
        streams.draw_data,
        streams.transforms,
        streams.styles,
        encoding.n_paths,
        encoding.n_path_segments,
        encoding.n_clips,
        encoding.n_open_clips,
        resources.patches.len(),
        resources.color_stops.len(),
        resources.glyphs.len(),
        resources.glyph_runs.len(),
        resources.normalized_coords.len(),
    );
}
