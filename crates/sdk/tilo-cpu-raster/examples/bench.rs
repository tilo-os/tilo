//! Times scene captures (`*.tscene`, written by the shell's
//! `cpu_raster_parity` test) through the CPU rasterizer: a cold first frame,
//! warm full frames and damage-limited frames, per thread count.
//!
//! ```text
//! bench [--size 1600x900] [--threads 0,2,3,4] [--runs 20] [--direct-blur]
//!       [--png DIR] SCENE.tscene...
//! ```
//!
//! Damage comes from `SCENE.damage` beside each capture (one `x y w h` per
//! line), when present, plus a cursor-sized and a bar-sized region. The
//! name `shadows` instead of a file is a built-in scene of overlapping windows
//! with blurred shadows, for comparing `--direct-blur`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line benchmark: failures end the run"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tilo_cpu_raster::color::{AlphaColor, Srgb};
use tilo_cpu_raster::{CpuRasterizer, Options, Region, Stats, Target};

struct Args {
    size: (u32, u32),
    threads: Vec<u16>,
    runs: usize,
    cached_blur: bool,
    png: Option<PathBuf>,
    scenes: Vec<PathBuf>,
}

fn parse() -> Args {
    let mut args = Args {
        size: (1600, 900),
        threads: vec![0, 2, 3, 4],
        runs: 20,
        cached_blur: true,
        png: None,
        scenes: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--size" => {
                let v = it.next().expect("--size WxH");
                let (w, h) = v.split_once('x').expect("--size WxH");
                args.size = (w.parse().unwrap(), h.parse().unwrap());
            }
            "--threads" => {
                args.threads = it
                    .next()
                    .expect("--threads N,N")
                    .split(',')
                    .map(|t| t.parse().unwrap())
                    .collect();
            }
            "--runs" => args.runs = it.next().expect("--runs N").parse().unwrap(),
            "--direct-blur" => args.cached_blur = false,
            "--png" => args.png = Some(it.next().expect("--png DIR").into()),
            _ => args.scenes.push(arg.into()),
        }
    }
    assert!(!args.scenes.is_empty(), "name at least one .tscene capture");
    args
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

struct Sample {
    total: Duration,
    stats: Stats,
}

fn run(
    raster: &mut CpuRasterizer,
    encoding: &vello_encoding::Encoding,
    buf: &mut [u8],
    size: (u32, u32),
    regions: Option<&[Region]>,
) -> Sample {
    let mut target = Target::new(buf, size.0, size.1, size.0 as usize * 4).unwrap();
    let start = Instant::now();
    let stats = raster.render(encoding, &mut target, regions, AlphaColor::<Srgb>::BLACK);
    Sample {
        total: start.elapsed(),
        stats,
    }
}

fn report(label: &str, samples: &[Sample]) {
    let pick = |f: fn(&Sample) -> Duration| median(samples.iter().map(f).collect());
    println!(
        "  {label:32} total {:7.2} ms  (decode {:6.2}  record {:6.2}  raster {:7.2})  draws {}/{}",
        ms(pick(|s| s.total)),
        ms(pick(|s| s.stats.decode)),
        ms(pick(|s| s.stats.record)),
        ms(pick(|s| s.stats.raster)),
        samples.last().map_or(0, |s| s.stats.recorded),
        samples.last().map_or(0, |s| s.stats.draws),
    );
}

fn main() {
    let args = parse();
    let (w, h) = args.size;
    println!(
        "{} CPUs available; size {w}x{h}; {} runs; blur {}",
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        args.runs,
        if args.cached_blur {
            "cached"
        } else {
            "per pixel"
        }
    );
    for path in &args.scenes {
        let encoding = if path.as_os_str() == "shadows" {
            shadows(args.size).encoding().clone()
        } else {
            tilo_cpu_raster::capture::read(std::fs::File::open(path).unwrap()).unwrap()
        };
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let shell_damage: Vec<Region> = std::fs::read_to_string(path.with_extension("damage"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let v: Vec<u32> = line
                    .split_whitespace()
                    .filter_map(|n| n.parse().ok())
                    .collect();
                (v.len() == 4).then(|| Region::new(v[0], v[1], v[2], v[3]))
            })
            .collect();
        println!(
            "{name}: {} draw tags, {} glyph runs",
            encoding.draw_tags.len(),
            encoding.resources.glyph_runs.len()
        );
        let mut damages: Vec<(String, Vec<Region>)> = vec![
            (
                "damage cursor 64x64".into(),
                vec![Region::new(w / 2, h / 2, 64, 64)],
            ),
            ("damage bar 1600x48".into(), vec![Region::new(0, 0, w, 48)]),
        ];
        if !shell_damage.is_empty() {
            let px: u32 = shell_damage.iter().map(|r| r.width * r.height).sum();
            damages.insert(
                0,
                (
                    format!("damage shell ({} rects, {px} px)", shell_damage.len()),
                    shell_damage,
                ),
            );
        }
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for &threads in &args.threads {
            println!(" threads {threads}");
            let options = Options {
                threads,
                cached_blur: args.cached_blur,
            };
            let mut raster = CpuRasterizer::new(options);
            let cold = run(&mut raster, &encoding, &mut buf, args.size, None);
            report(
                &format!(
                    "cold full (glyph {} blur {})",
                    cold.stats.glyph_misses, cold.stats.blur_misses
                ),
                &[cold],
            );
            let samples: Vec<_> = (0..args.runs)
                .map(|_| run(&mut raster, &encoding, &mut buf, args.size, None))
                .collect();
            report("full frame", &samples);
            if let Some(dir) = &args.png {
                std::fs::create_dir_all(dir).unwrap();
                let file =
                    std::fs::File::create(dir.join(format!("{name}-t{threads}.png"))).unwrap();
                let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
                enc.set_color(png::ColorType::Rgba);
                enc.write_header().unwrap().write_image_data(&buf).unwrap();
            }
            for (label, regions) in &damages {
                let samples: Vec<_> = (0..args.runs)
                    .map(|_| run(&mut raster, &encoding, &mut buf, args.size, Some(regions)))
                    .collect();
                report(label, &samples);
            }
        }
    }
}

/// Six overlapping windows with the shell's shadow, at fractional positions.
fn shadows((w, h): (u32, u32)) -> vello::Scene {
    use vello::kurbo::{Affine, Rect, RoundedRect};
    use vello::peniko::{Color, Fill};
    let mut scene = vello::Scene::new();
    let (w, h) = (f64::from(w), f64::from(h));
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(40, 44, 52),
        None,
        &Rect::new(0.0, 0.0, w, h),
    );
    for i in 0..6 {
        let x = 40.3 + f64::from(i) * w / 7.5;
        let y = 60.7 + f64::from(i % 3) * h / 6.0;
        let window = Rect::new(x, y, x + w * 0.4, y + h * 0.45);
        scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            window.inflate(0.0, 4.0),
            Color::from_rgba8(0, 0, 0, 110),
            12.0,
            16.0,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(245, 245, 245),
            None,
            &RoundedRect::from_rect(window, 12.0),
        );
    }
    scene
}
