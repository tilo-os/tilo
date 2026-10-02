//! Looking at an artwork: what an agent sees after it paints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::{ArtworkResource, view_ops};
use ui::prelude::{Invoke, Replica};

fn landscape() -> Replica<ArtworkResource> {
    let mut art = Replica::<ArtworkResource>::new();
    for action in canvas::sample::landscape() {
        art.apply(Invoke::apply(&action).unwrap()).unwrap();
    }
    art
}

fn out(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/canvas-look");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A look renders the artwork the agent painted, at the size asked, and a
/// close look at a region magnifies it; both are real PNGs.
#[test]
fn a_look_shows_the_painting_whole_and_up_close() {
    let art = landscape().state().unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let (whole, info) = view_ops::look(&mut renderer, &mut text, &art, &Look::default()).unwrap();
    assert_eq!((whole.width, whole.height), (1024, 768), "{info}");
    assert!(whole.png.starts_with(b"\x89PNG"));
    std::fs::write(out("whole.png"), &whole.png).unwrap();

    let close = Look {
        region: Some([700.0, 100.0, 320.0, 240.0]),
        size: Some(640),
        grid: Some(true),
        layers: None,
    };
    let (detail, info) = view_ops::look(&mut renderer, &mut text, &art, &close).unwrap();
    assert_eq!((detail.width, detail.height), (640, 480));
    assert_eq!(info["pixels_per_unit"], 2.0);
    std::fs::write(out("detail.png"), &detail.png).unwrap();

    let gridded = Look {
        grid: Some(true),
        ..Look::default()
    };
    let (grid, _) = view_ops::look(&mut renderer, &mut text, &art, &gridded).unwrap();
    std::fs::write(out("grid.png"), &grid.png).unwrap();

    let outside = Look {
        region: Some([5000.0, 5000.0, 10.0, 10.0]),
        ..Look::default()
    };
    let error = view_ops::look(&mut renderer, &mut text, &art, &outside)
        .err()
        .unwrap();
    assert!(error.contains("outside the artwork"), "{error}");
}
