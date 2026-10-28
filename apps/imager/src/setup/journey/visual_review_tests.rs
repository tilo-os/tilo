//! Development-only raster evidence from the production renderer and action path.
use super::*;
use crate::resource::GuidedInstallerShell;
use ui::framework::testing::TestApp;

pub(super) fn evidence(harness: &mut ui::headless::FrameHarness, name: &str) {
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
    // Save the actual emitted font bytes, not the requested family. Review
    // tooling can decode their name tables without guessing the host's font.
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
    if let Some(preview) = harness.widget_mut::<InstallerPreview>() {
        image_geometry(preview, &mut metadata);
    }
    std::fs::write(directory.join(format!("{name}.txt")), metadata).unwrap();
    eprintln!("current raster evidence: {name}");
}

fn fixture(viewport: (u32, u32), theme: Theme) -> TestApp<GuidedInstallerShell> {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey = Journey::new("Ada Lovelace".into());
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.migrate = Some(false);
        preview.journey.identity = Some(Identity::New);
        preview.journey.device_name = "My computer".into();
        preview.journey.page = Page::Profile;
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
fn current_personal_and_review_raster_matrix() {
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
                evidence(app.harness(), &format!("profile-{tag}"));
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.journey.page = Page::Password;
                    preview.dirty = true;
                })
                .unwrap();
                app.harness()
                    .perform_named("installer.password", &ui::a11y::Action::Focus)
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
                    // Observe the normal build's actual admission gates. No
                    // source, custody, package or preparation approval is injected.
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
                app.harness().send(&ui::synthetic_input::cursor_moved(
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

#[test]
#[cfg(unix)]
fn current_belongings_71_rows_raster_matrix() {
    use super::super::flatpak_transfer::{Installation, InstalledRef, Inventory};
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
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.journey.page = Page::Belongings;
                    preview.journey.destination = Some(Destination::Another);
                    preview.journey.migrate = Some(true);
                    preview.journey.categories = [false, true, false];
                    preview.journey.details = true;
                    preview.belongings.flatpak = Some(Inventory {
                        refs: (0..71)
                            .map(|index| InstalledRef {
                                reference: format!(
                                    "app/org.example.Application{index}/x86_64/stable"
                                ),
                                origin: "example".into(),
                                installation: Installation::User,
                                name: format!("Application {index}"),
                            })
                            .collect(),
                        remotes: Vec::new(),
                    });
                    preview.dirty = true;
                })
                .unwrap();
                app.settle();
                let (body, footer) = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        assert_eq!(preview.flatpak_apps().len(), 71);
                        (
                            visual_tests::body_scroll_geometry(preview).unwrap().0,
                            visual_tests::control(preview, "installer.next")
                                .unwrap()
                                .get_rect(),
                        )
                    })
                    .unwrap();
                evidence(app.harness(), &format!("belongings-top-{tag}"));
                app.harness().send(&ui::synthetic_input::cursor_moved(
                    body.x0 + 2.0,
                    body.center().y,
                ));
                let target = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        let last = visual_tests::control(preview, "installer.flatpak.app.70")
                            .unwrap()
                            .get_rect();
                        (last.y1 - body.y1).max(0.0) / 2.0
                    })
                    .unwrap();
                // Wheel input, not a second layout or painter. Stop near the
                // midpoint using observed production row geometry.
                let first_top = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        visual_tests::control(preview, "installer.flatpak.app.0")
                            .unwrap()
                            .get_rect()
                            .y0
                    })
                    .unwrap();
                for _ in 0..1000 {
                    let moved = app
                        .with_widget::<InstallerPreview, _>(|preview| {
                            first_top
                                - visual_tests::control(preview, "installer.flatpak.app.0")
                                    .unwrap()
                                    .get_rect()
                                    .y0
                        })
                        .unwrap();
                    if moved >= target {
                        break;
                    }
                    app.harness().scroll(0.0, -1.0);
                }
                app.settle();
                evidence(app.harness(), &format!("belongings-middle-{tag}"));
                for _ in 0..100 {
                    app.harness().scroll(0.0, -10.0);
                }
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(
                        visual_tests::control(preview, "installer.next")
                            .unwrap()
                            .get_rect(),
                        footer
                    );
                    let last = visual_tests::control(preview, "installer.flatpak.app.70")
                        .unwrap()
                        .get_rect();
                    assert!(
                        last.y0 >= body.y0 && last.y1 <= body.y1,
                        "last row must actually be visible: {last:?}, body={body:?}"
                    );
                })
                .unwrap();
                evidence(app.harness(), &format!("belongings-bottom-{tag}"));
            }
        }
    }
}
