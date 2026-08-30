//! Dev-only: dump a PNG of every settings pane (wide) plus the narrow
//! list/content screens, all in ONE process so the expensive vello shader
//! compilation is paid once. Usage: `cargo run -p settings --example dump_panes -- <outdir>`.

use settings::SettingsApp;
use settings::categories::Category;
use settings::shots::build_widget_scene;
use ui::offscreen::OffscreenRenderer;
use vello::peniko::Color;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/settings-panes".to_string());
    std::fs::create_dir_all(&out).unwrap();

    let wide = (1200u32, 760u32);
    let narrow = (440u32, 820u32);

    let mut wide_r = OffscreenRenderer::new(wide.0, wide.1);
    for cat in Category::ALL {
        let app = SettingsApp::with_selected(*cat).with_width_hint(wide.0 as f32);
        let scene = build_widget_scene(Box::new(app), wide);
        let path = format!("{out}/panel-{}.png", cat.slug());
        wide_r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(&path));
        println!("wrote {path}");
    }
    // Wide flagship (external accounts) + a search-filtered sidebar.
    {
        let app = SettingsApp::with_selected(Category::Displays)
            .with_width_hint(wide.0 as f32)
            .with_search_shot("light");
        let scene = build_widget_scene(Box::new(app), wide);
        let path = format!("{out}/settings-search.png");
        wide_r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(&path));
        println!("wrote {path}");
    }

    let mut narrow_r = OffscreenRenderer::new(narrow.0, narrow.1);
    {
        let app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(narrow.0 as f32)
            .with_narrow_list_shot();
        let scene = build_widget_scene(Box::new(app), narrow);
        let path = format!("{out}/settings-narrow-list.png");
        narrow_r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(&path));
        println!("wrote {path}");
    }
    {
        let app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(narrow.0 as f32)
            .with_narrow_content_shot();
        let scene = build_widget_scene(Box::new(app), narrow);
        let path = format!("{out}/settings-narrow-content.png");
        narrow_r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(&path));
        println!("wrote {path}");
    }
}
