//! Review stills of the Tilo Community card in Settings > Users, as a member
//! and after leaving, in light, dark and high contrast. Written under the
//! target directory's `tmp/settings-community`.
//!
//! ```text
//! cargo test -p settings --test it community_review -- --ignored
//! ```

use settings::panes::users::UsersPane;
use settings_store::community::Membership;
use ui::offscreen::OffscreenRenderer;
use ui::theme::Theme;

#[test]
#[ignore = "review tool: needs an offscreen wgpu adapter and writes PNGs"]
fn render_community_card() {
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("settings-community");
    std::fs::create_dir_all(&root).expect("create the review directory");
    let settings = tempfile::tempdir().expect("scratch settings");
    let path = settings.path().join("settings.json");
    let (width, height) = (900, 1500);
    let mut renderer = OffscreenRenderer::new(width, height);
    for membership in [Membership::Auto, Membership::Left] {
        settings_store::community::set_membership_at(&path, membership).expect("save choice");
        for (label, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("high-contrast", Theme::high_contrast()),
        ] {
            let pane = UsersPane::new().with_settings_path(path.clone());
            let scene = settings::shots::build_widget_scene_with_theme(
                Box::new(pane),
                (width, height),
                &theme,
            );
            renderer.render_png(
                &scene,
                theme.bg_app,
                root.join(format!("users-{}-{label}.png", membership.as_str())),
            );
        }
    }
    println!("Settings community stills: {}", root.display());
}
