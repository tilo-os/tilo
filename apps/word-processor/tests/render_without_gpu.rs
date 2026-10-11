//! A confined agent worker has no GPU (`ui::offscreen::disable_gpu`), yet the
//! `render` view still answers with the document's picture: drawn on the CPU,
//! at the size the view reports.
//!
//! Disabling the GPU is process-wide, so the body runs in a re-executed
//! child of this test binary and leaves every other test's renderer alone.

use std::process::Command;

use serde_json::json;
use word_processor::framework::WordProcessorApp;

/// Set in the child so the same test function runs the body.
const CHILD_MARKER: &str = "TILO_WORD_PROCESSOR_RENDER_WITHOUT_GPU_CHILD";

#[test]
fn render_answers_a_picture_without_a_gpu() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        render_without_a_gpu();
        return;
    }
    // libtest omits the binary's own crate from `it::render_without_gpu`.
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let name = format!("{module}::render_answers_a_picture_without_a_gpu");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the GPU-less child failed or ran nothing ({}):\n{stdout}\n{stderr}",
        output.status
    );
}

fn render_without_a_gpu() {
    ui::offscreen::disable_gpu();
    assert!(
        ui::offscreen::OffscreenRenderer::try_new(4, 4).is_err(),
        "no GPU renderer starts once the GPU is disabled"
    );
    ui::i18n::I18nManager::init("en-US");
    let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
    app.harness()
        .type_text("Pictures are drawn without a GPU, word for word.");
    let shown = app
        .call("render", json!({}))
        .unwrap()
        .expect("render answers a picture");
    let address = shown["content"].as_str().expect("a stored picture");
    let hash = ui::tilo_actions::content_hash(address).expect("a content address");
    let png = app.content().read(hash).expect("the picture is stored");
    let picture = ui::png::decode_rgba8(&png).unwrap();
    assert_eq!(
        (u64::from(picture.width), u64::from(picture.height)),
        (
            shown["width"].as_u64().unwrap(),
            shown["height"].as_u64().unwrap()
        ),
        "the picture is the size the view reports"
    );
    assert!(picture.width > 100 && picture.height > 100, "{shown}");
    let side = |pixels: u32| usize::try_from(pixels).unwrap();
    assert_eq!(
        Some(picture.rgba.len()),
        side(picture.width)
            .checked_mul(side(picture.height))
            .and_then(|area| area.checked_mul(4))
    );
    let pixels = picture.rgba.chunks_exact(4);
    let total = pixels.len();
    let translucent: Vec<(usize, &[u8])> = pixels
        .clone()
        .enumerate()
        .filter(|(_, pixel)| pixel[3] != 255)
        .collect();
    assert!(
        translucent.is_empty(),
        "the view's surface covers the whole picture: {} of {total} translucent, first {:?}, corner {:?}, {shown}",
        translucent.len(),
        translucent.iter().take(8).collect::<Vec<_>>(),
        &picture.rgba[..4]
    );
    // The page and the typed words: more than one colour, the backdrop most.
    let backdrop = picture.rgba[..4].to_vec();
    let differing = pixels.filter(|pixel| *pixel != backdrop.as_slice()).count();
    assert!(
        differing > 100 && differing < total,
        "the document is drawn over the surface: {differing} of {total} pixels differ"
    );
}
