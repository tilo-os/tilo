//! Measure the Store's real widget and text paths without a display or worker.
//! The optional catalogue is the existing screenshot fixture, never live data.

use ui::headless::FrameHarness;
use ui::permissions::UserId;
use ui::text_render::TextRenderer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let condition = match std::env::args().nth(1).as_deref() {
        None => "empty",
        Some("--fixture") => "fixture",
        Some("--full-page") => "full-page",
        Some(_) => return Err("expected no argument, --fixture or --full-page".into()),
    };
    let start = ui::time::now();
    let text = TextRenderer::new();
    let fonts_ms = start.elapsed().as_secs_f64() * 1000.0;
    let faces = text.font_system.db().faces().count();
    for family in [
        ui::text_render::UI_SANS_FAMILY,
        ui::text_render::UI_SERIF_FAMILY,
        ui::text_render::UI_MONOSPACE_FAMILY,
    ] {
        if !text
            .font_system
            .db()
            .faces()
            .any(|face| face.families.iter().any(|name| name.0 == family))
        {
            return Err(format!("required product font unavailable: {family}").into());
        }
    }
    let start = ui::time::now();
    let mut root = store::StoreApp::new();
    if condition == "fixture" {
        root.set_items(store::fixture::fixture_catalog());
    } else if condition == "full-page" {
        let catalogue = store::fixture::fixture_catalog();
        if catalogue.is_empty() {
            return Err("screenshot catalogue unavailable".into());
        }
        root.set_items(
            catalogue
                .iter()
                .cycle()
                .take(48)
                .enumerate()
                .map(|(index, item)| {
                    let mut item = item.clone();
                    item.id = format!("so.tilo.performance.fixture-{index}");
                    item.name = format!("{} {index}", item.name);
                    item.description = format!("{} {index}", item.description);
                    item
                })
                .collect(),
        );
    }
    let items = root.items().len();
    let construct_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = ui::time::now();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (868, 772),
        ui::Theme::dark(),
        text,
        UserId("isolated-startup-layout-probe".into()),
    );
    let first_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
    let rect = frame.root_widget().get_rect();
    if faces == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err("fonts or actual Store layout unavailable".into());
    }
    let start = ui::time::now();
    frame.relayout_at((868, 772))?;
    let retained_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
    frame.begin_profiling();
    let start = ui::time::now();
    frame.recompose()?;
    let rebuilt_warm_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
    let rebuilt_profile = frame.take_profile_samples();
    if rebuilt_profile.is_empty() {
        return Err("rebuilt frame profiling unavailable".into());
    }
    println!(
        "{}",
        serde_json::json!({
            "scope": "Store widget CPU initialization and headless layout/draw; no app worker, GPU, native window or OS timing",
            "condition": condition,
            "catalogue_items": items,
            "font_faces": faces,
            "fonts_ms": fonts_ms,
            "construct_ms": construct_ms,
            "first_frame_ms": first_frame_ms,
            "retained_frame_ms": retained_frame_ms,
            "rebuilt_warm_frame_ms": rebuilt_warm_frame_ms,
            "rebuilt_profile": rebuilt_profile,
            "root_rect": [rect.x0, rect.y0, rect.x1, rect.y1],
        })
    );
    Ok(())
}
