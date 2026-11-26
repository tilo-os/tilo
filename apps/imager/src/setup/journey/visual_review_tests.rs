//! Development-only raster evidence from the production renderer and action path.

use super::*;
use crate::hosting::testing::TestHost;

pub(super) fn evidence(harness: &mut ui::headless::Driver, name: &str) {
    let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory).join("current-review");
    std::fs::create_dir_all(&directory).unwrap();
    harness.render_png(&directory.join(format!("{name}.png")));
    let theme = harness.theme();
    let mut metadata = format!(
        "ui_scale={} text_scale={}\n",
        theme.ui_scale, theme.text_scale
    );
    for (index, run) in harness
        .scene()
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .enumerate()
    {
        let font_file = format!("font-{}-{}.bin", run.font.data.len(), run.font.index);
        metadata.push_str(&format!(
            "run={index} size={} face_index={} font_file={font_file}\n",
            run.font_size, run.font.index
        ));
        let path = directory.join(font_file);
        if path.exists() {
            assert_eq!(std::fs::read(&path).unwrap(), run.font.data.as_ref());
        } else {
            std::fs::write(path, run.font.data.as_ref()).unwrap();
        }
    }
    fn image_geometry(root: &dyn Widget, metadata: &mut String) {
        if root.name() == "Image" {
            metadata.push_str(&format!(
                "image_rect={:?} label={:?}\n",
                root.get_rect(),
                root.accessibility().and_then(|node| node.name)
            ));
        }
        for child in root.children() {
            image_geometry(child, metadata);
        }
    }
    if let Some(preview) =
        crate::hosting::testing::find::<InstallerPreview>(harness.root_widget_mut())
    {
        image_geometry(preview, &mut metadata);
    }
    std::fs::write(directory.join(format!("{name}.txt")), metadata).unwrap();
    eprintln!("current raster evidence: {name}");
}

fn fixture(viewport: (u32, u32), theme: Theme) -> TestHost {
    let mut app = TestHost::launched(&["--setup"]);
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.device_name = "My computer".into();
        preview.journey.page = Page::Migrate;
        preview.dirty = true;
    })
    .unwrap();
    app.harness().resize(viewport).unwrap();
    app.harness().set_theme(theme.with_reduce_motion(true));
    app.settle();
    app
}

#[test]
#[cfg(unix)]
fn current_migrate_password_and_review_raster_matrix() {
    let private = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    for key in [
        "HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        let path = private.path().join(key);
        std::fs::create_dir_all(&path).unwrap();
        environment.set(key, path);
    }
    environment.remove("DISPLAY");
    environment.remove("WAYLAND_DISPLAY");
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for viewport in [(1180, 820), (640, 800), (320, 800)] {
            for text_scale in [1.0, 2.0] {
                let tag = format!("{tag}-{}x{}-text{text_scale}", viewport.0, viewport.1);
                let mut app = fixture(viewport, theme.clone().with_text_scale(text_scale));
                evidence(app.harness(), &format!("migrate-{tag}"));
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.journey.page = Page::Password;
                    preview.dirty = true;
                })
                .unwrap();
                app.harness()
                    .perform_named("installer.password", &tilo_ui_base::a11y::Action::Focus)
                    .unwrap();
                app.harness().type_text("short");
                app.harness().key("Tab");
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert!(
                        visual_tests::control(preview, "installer.password")
                            .unwrap()
                            .accessibility()
                            .unwrap()
                            .state
                            .invalid
                    );
                })
                .unwrap();
                evidence(app.harness(), &format!("password-blur-{tag}"));
                for effect in [
                    InstallerPreviewEffect::SetPreviewPassword {
                        value: "walnut telescope amber meadow".into(),
                    },
                    InstallerPreviewEffect::SetPreviewConfirmation {
                        value: "different unrelated installation words".into(),
                    },
                ] {
                    app.effect(&effect).unwrap();
                }
                app.settle();
                evidence(app.harness(), &format!("password-mismatch-{tag}"));
                app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
                    value: "walnut telescope amber meadow".into(),
                })
                .unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert!(preview.password_continue_enabled())
                })
                .unwrap();
                evidence(app.harness(), &format!("password-corrected-{tag}"));
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(preview.journey.page, Page::Devices)
                })
                .unwrap();
                evidence(app.harness(), &format!("name-computer-{tag}"));
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.journey.page = Page::Review;
                    preview.dirty = true;
                })
                .unwrap();
                app.settle();
                evidence(app.harness(), &format!("review-blocked-top-{tag}"));
                let body = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        visual_tests::body_scroll_geometry(preview).unwrap().0
                    })
                    .unwrap();
                app.harness().send(&tilo_ui_base::synthetic_input::cursor_moved(
                    body.x0 + 2.0,
                    body.center().y,
                ));
                for _ in 0..40 {
                    app.harness().scroll(0.0, -10.0);
                }
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(
                        preview.journey.page,
                        Page::Review,
                        "scrolling must not activate a review row"
                    )
                })
                .unwrap();
                evidence(app.harness(), &format!("review-blocked-bottom-{tag}"));
            }
        }
    }
}
