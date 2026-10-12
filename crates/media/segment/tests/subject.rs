//! The subject model on real photographs, timed. Run on a machine holding
//! the models and photographs under `target/segment-models/`:
//! `cargo test -p tilo-segment --features infer --release -- --ignored --nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a timing run on photographs a few thousand pixels a side"
)]

use std::path::PathBuf;
use std::time::Instant;

use tilo_segment::{Family, Picture, SubjectModel, defringe, refine_edge};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/segment-models")
}

#[test]
#[ignore = "needs model files on disk"]
fn time_the_models_on_photographs() {
    let dir = dir();
    for (name, family) in [
        ("u2netp", Family::U2Net),
        ("silueta", Family::U2Net),
        ("u2net", Family::U2Net),
        ("isnet-general-use", Family::IsNet),
    ] {
        let path = dir.join(format!("{name}.onnx"));
        let Ok(bytes) = std::fs::read(&path) else {
            println!("{name}: no model at {}", path.display());
            continue;
        };
        for threads in [4, 16] {
            let began = Instant::now();
            let model = SubjectModel::load(bytes.clone(), family)
                .unwrap()
                .with_threads(threads);
            let loaded = began.elapsed();
            for photo in ["sheltie", "apple", "golden"] {
                let Ok(image) = image::open(dir.join(format!("{photo}.jpg"))) else {
                    continue;
                };
                let image = image.to_rgba8();
                let (width, height) = (image.width() as usize, image.height() as usize);
                let picture = Picture {
                    rgba: image.as_raw(),
                    width,
                    height,
                };
                let began = Instant::now();
                let matte = model.matte(&picture).unwrap();
                let inferred = began.elapsed();
                let began = Instant::now();
                let refined = refine_edge(&picture, &matte, 8.0).unwrap();
                let clean = defringe(&picture, &refined).unwrap();
                let finished = began.elapsed();
                println!(
                    "{name} threads {threads} {photo} {width}x{height}: load {loaded:?}, infer {inferred:?}, refine+defringe {finished:?}"
                );
                if threads == 4 {
                    let mut out =
                        image::RgbaImage::from_raw(width as u32, height as u32, clean).unwrap();
                    for (pixel, alpha) in out.pixels_mut().zip(&refined.values) {
                        pixel.0[3] = (alpha * 255.0).round() as u8;
                    }
                    out.save(dir.join(format!("out-{photo}-{name}.png")))
                        .unwrap();
                }
            }
        }
    }
}
