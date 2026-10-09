//! Matched production-backend benchmark; integer geometry keeps the dense oracle exact.
use shell::compositor::backend::{Backend, OutputId, headless::HeadlessBackend};
use shell::compositor::frame::FramePaint;
use std::time::{Duration, Instant};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

fn process_cpu() -> Result<Duration, String> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the process clock is valid and time points to writable storage.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &raw mut time) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(Duration::new(
        u64::try_from(time.tv_sec).map_err(|e| e.to_string())?,
        u32::try_from(time.tv_nsec).map_err(|e| e.to_string())?,
    ))
}

fn scene(color: Color, changed: bool) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color,
        None,
        &Rect::new(0.0, 0.0, 1600.0, 900.0),
    );
    for row in 0u32..12 {
        for column in 0u32..20 {
            let x = column.saturating_mul(80);
            let y = row.saturating_mul(72);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(if changed { 40 } else { 180 }, 80, 120),
                None,
                &Rect::new(
                    f64::from(x.saturating_add(8)),
                    f64::from(y.saturating_add(8)),
                    f64::from(x.saturating_add(64)),
                    f64::from(y.saturating_add(56)),
                ),
            );
        }
    }
    scene
}

fn main() -> Result<(), String> {
    let began = Instant::now();
    for (name, theme) in [
        ("dark", ui::theme::Theme::dark()),
        ("light", ui::theme::Theme::light()),
        ("high-contrast", ui::theme::Theme::high_contrast()),
    ] {
        let mut baseline = HeadlessBackend::new(1600, 900).map_err(|e| e.to_string())?;
        let mut candidate = HeadlessBackend::new(1600, 900).map_err(|e| e.to_string())?;
        baseline.set_watched(true);
        candidate.set_watched(true);
        for sample in 0u32..24 {
            if began.elapsed() > Duration::from_secs(30) {
                return Err("bounded probe exceeded 30s".into());
            }
            let changed = sample.is_multiple_of(6);
            let changed_color = (6..12).contains(&sample) || (18..24).contains(&sample);
            let mut paint =
                FramePaint::full(scene(theme.tokens.colors.surfaces.base, changed_color));
            paint.revision = u64::from(sample).saturating_add(1);
            paint.previous_revision = (sample != 0).then_some(u64::from(sample));
            paint.damage_regions = (!changed).then(Vec::new);
            let mut dense_wall_ns = 0;
            let mut dense_cpu_ns = 0;
            let mut retained_wall_ns = 0;
            let mut retained_cpu_ns = 0;
            for retain in [!sample.is_multiple_of(2), sample.is_multiple_of(2)] {
                let wall = Instant::now();
                let cpu = process_cpu()?;
                if retain {
                    candidate.submit_frame_with_damage(&paint, OutputId(0));
                    retained_cpu_ns = process_cpu()?.saturating_sub(cpu).as_nanos();
                    retained_wall_ns = wall.elapsed().as_nanos();
                } else {
                    baseline.submit_frame(&paint.scene, OutputId(0));
                    dense_cpu_ns = process_cpu()?.saturating_sub(cpu).as_nanos();
                    dense_wall_ns = wall.elapsed().as_nanos();
                }
            }
            if baseline.last_frame() != candidate.last_frame() {
                return Err(format!(
                    "strict dense pixel oracle failed: {name} sample {sample}"
                ));
            }
            println!(
                "{}",
                serde_json::json!({"theme":name,"sample":sample,"changed":changed,"qualifying":matches!(sample, 1 | 7 | 13 | 19),
                "dense_wall_ns":dense_wall_ns,"dense_cpu_ns":dense_cpu_ns,
                "retained_wall_ns":retained_wall_ns,"retained_cpu_ns":retained_cpu_ns,
                "pixel_hash":blake3::hash(candidate.last_frame()).to_hex().to_string(),
                "frame_size":[1600,900],"exact_pixels":true})
            );
        }
    }
    Ok(())
}
