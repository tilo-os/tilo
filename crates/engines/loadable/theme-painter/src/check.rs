//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. The painter
//! module it runs is written here as WAT and answered through the SDK's
//! wire encoding, so the check paints a part the way a theme's set does.

use std::fmt::Write as _;
use std::process::ExitCode;

use tilo_theme_sdk::kurbo::BezPath;
use tilo_theme_sdk::peniko::Color;
use tilo_theme_sdk::wire;
use tilo_theme_sdk::{Brush, PaintedPart, PartScene};

// The library exports only its unmangled entry points (the engine is no
// crate's dependency), so this binary links it by name, not as a crate.
#[link(name = "tilo_theme_painter_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    fn tilo_theme_painter_engine_check(module: *const u8, len: usize) -> bool;
}

/// `bytes` as a WAT data string.
fn data(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        let _ = write!(text, "\\{byte:02x}");
    }
    text
}

/// A module that answers the params request with an empty list and every other request with a square
/// that takes the pointer, and imports nothing.
fn painter_module() -> Result<Vec<u8>, wat::Error> {
    let params = wire::params_answer(&[]);
    let mut square = BezPath::new();
    square.move_to((0.0, 0.0));
    square.line_to((40.0, 0.0));
    square.line_to((40.0, 40.0));
    square.line_to((0.0, 40.0));
    square.close_path();
    let mut scene = PartScene::new();
    scene.fill(
        square.clone(),
        Brush::Solid(Color::from_rgb8(0xd0, 0x30, 0x20)),
    );
    let painted = PaintedPart::new(scene).with_hit(square);
    let answer = wire::paint_answer(Some(&painted));
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{answer}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {answer_len}))))))"#,
        params = data(&params),
        answer = data(&answer),
        params_len = params.len(),
        answer_len = answer.len(),
    ))
}

fn main() -> ExitCode {
    let Ok(module) = painter_module() else {
        eprintln!("tilo-theme-painter-engine: the check's module is not WAT");
        return ExitCode::FAILURE;
    };
    // SAFETY: `module` is a live slice for the call, and the library reads
    // nothing of it after it returns.
    if unsafe { tilo_theme_painter_engine_check(module.as_ptr(), module.len()) } {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
