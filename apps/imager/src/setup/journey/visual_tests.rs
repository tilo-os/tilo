//! Paint the real guided widgets offscreen, without opening an installer window.
use super::*;
use crate::resource::GuidedInstallerShell;
use std::path::PathBuf;
use ui::framework::testing::TestApp;

pub(super) fn control<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| control(child, id))
}

fn inspect_controls(preview: &InstallerPreview, ids: &[&str], viewport: (u32, u32)) {
    for id in ids {
        let rect = control(preview, id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .get_rect();
        assert!(
            rect.width() >= 24.0 && rect.height() >= 20.0,
            "{id}: {rect:?}"
        );
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= f64::from(viewport.0),
            "horizontal clipping {id}: {rect:?}"
        );
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1),
            "vertical clipping {id}: {rect:?}"
        );
    }
    let showing_password = preview.journey.page == Page::Password;
    #[cfg(unix)]
    let showing_password = showing_password && !preview.restart.blocks();
    if showing_password && !preview.unlocking_preparation() {
        let first = control(preview, "installer.password").unwrap().get_rect();
        let second = control(preview, "installer.confirmation")
            .unwrap()
            .get_rect();
        assert!((first.x0 - second.x0).abs() < 1.0);
        assert!((first.x1 - second.x1).abs() < 1.0);
        assert!(second.y0 >= first.y1, "password fields must not overlap");
    }
}

fn paint_screen(scenario: &str, ids: &[&str], viewport: (u32, u32), theme: Theme, tag: &str) {
    let mut app = TestApp::<GuidedInstallerShell>::capture_at(Some(scenario), viewport).unwrap();
    app.harness().set_theme(theme.with_reduce_motion(true));
    paint_app(&mut app, scenario, ids, viewport, tag);
}

/// Scroll the real body until `id` is inside the viewport. A control below
/// the fold is reachable with the same wheel input a person uses, never by
/// lowering the assertion or editing its rectangle.
pub(super) fn scroll_into_view<A: ui::framework::ResourceApp>(app: &mut TestApp<A>, id: &str) {
    let pointer = app
        .with_widget::<InstallerPreview, _>(|preview| (preview.rect.center().x, 100.0))
        .unwrap();
    app.harness().click_at(pointer.0, pointer.1);
    for _ in 0..80 {
        app.settle();
        let found = app
            .with_widget::<InstallerPreview, _>(|preview| {
                control(preview, id).map(|control| (control.get_rect(), preview.rect.y1))
            })
            .flatten();
        let Some((rect, viewport_bottom)) = found else {
            panic!("missing {id}");
        };
        if rect.y0 >= 0.0 && rect.y1 <= viewport_bottom {
            return;
        }
        app.harness().scroll(0.0, -20.0);
        app.settle();
    }
    panic!("{id} never scrolled into view");
}

pub(super) fn paint_app<A: ui::framework::ResourceApp>(
    app: &mut TestApp<A>,
    scenario: &str,
    ids: &[&str],
    viewport: (u32, u32),
    tag: &str,
) {
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| inspect_controls(preview, ids, viewport))
        .unwrap();
    let pixels = app.harness().render_rgba();
    let expected = usize::try_from(viewport.0)
        .unwrap()
        .checked_mul(usize::try_from(viewport.1).unwrap())
        .unwrap()
        .checked_mul(4)
        .unwrap();
    assert_eq!(
        pixels.len(),
        expected,
        "the actual scene must be rasterized"
    );
    let ground = pixels.get(..4).unwrap();
    let painted = pixels
        .chunks_exact(4)
        .filter(|pixel| *pixel != ground)
        .count();
    assert!(painted > 200, "{scenario}/{tag}: blank or unpainted frame");
    if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
        let directory = PathBuf::from(directory).join("installer-journey");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "{scenario}-{tag}-{}x{}.png",
            viewport.0, viewport.1
        ));
        app.harness().render_png(&path);
        eprintln!("installer visual evidence: {}", path.display());
    }
}

/// Resize the production widget tree, rasterize it, then click its current
/// rectangles. Neither a paint-only fixture nor a separate input transform is
/// involved: the same host theme drives layout, text and pointer dispatch.
#[test]
fn viewport_growth_keeps_logical_type_and_controls_and_pointer_mapping() {
    fn heading_font(root: &mut dyn Widget, theme: &Theme) -> Option<f32> {
        if root.test_id() == Some("installer.heading") {
            let mut scene = Scene::new();
            root.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut ui::TextRenderer::new(),
                theme,
                &PresenceCtx::default(),
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(!runs.is_empty(), "the heading must paint real glyphs");
            let size = runs[0].font_size;
            assert!(runs.iter().all(|run| run.font_size == size));
            return Some(size);
        }
        root.children_mut()
            .into_iter()
            .find_map(|child| heading_font(child, theme))
    }
    for theme in [Theme::dark(), Theme::light()] {
        let mut unscaled: Option<(f32, f64, f64, f64)> = None;
        for host_scale in [1.0, 1.5, 2.0] {
            // Keep the initial logical workspace constant across host DPI.
            // Compact-page tests separately exercise small-window scrolling.
            let initial = match host_scale {
                1.0 => (1180, 820),
                1.5 => (1770, 1230),
                _ => (2360, 1640),
            };
            let mut app =
                TestApp::<GuidedInstallerShell>::capture_at(Some("installer-profile"), initial)
                    .unwrap();
            app.harness().set_theme(
                theme
                    .clone()
                    .with_ui_scale(host_scale)
                    .with_reduce_motion(true),
            );
            let mut original = None;
            for viewport in [initial, (5120, 2822), (3840, 2160), (2560, 1440)] {
                app.harness().resize(viewport).unwrap();
                paint_app(
                    &mut app,
                    "installer-profile",
                    &["installer.name", "installer.next"],
                    viewport,
                    &format!("logical-size-{host_scale}"),
                );
                let dimensions = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        let heading_size =
                            heading_font(preview, &theme.clone().with_ui_scale(host_scale))
                                .unwrap();
                        let name = control(preview, "installer.name").unwrap().get_rect();
                        let next = control(preview, "installer.next").unwrap().get_rect();
                        let avatar = control(preview, "installer.profile.avatar")
                            .unwrap()
                            .get_rect();
                        let metrics = &theme.tokens.metrics;
                        assert!(
                            (name.width() - f64::from(metrics.content_min_width * host_scale))
                                .abs()
                                < 1.0
                        );
                        assert!((name.center().x - preview.rect.center().x).abs() < 1.0);
                        (heading_size, name.height(), next.height(), avatar.height())
                    })
                    .unwrap();
                if let Some(expected) = original {
                    assert_eq!(
                        dimensions, expected,
                        "viewport growth must add whitespace, not magnify: {viewport:?}"
                    );
                } else {
                    original = Some(dimensions);
                }
                if let Some((font, field, button, avatar)) = unscaled {
                    assert_eq!(dimensions.0, font * host_scale);
                    for (actual, base) in [
                        (dimensions.1, field),
                        (dimensions.2, button),
                        (dimensions.3, avatar),
                    ] {
                        assert!(
                            (actual - base * f64::from(host_scale)).abs() < 0.01,
                            "host UI scale must apply exactly once: {actual}, {base}, {host_scale}"
                        );
                    }
                } else {
                    unscaled = Some(dimensions);
                }
                let next = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        control(preview, "installer.next")
                            .unwrap()
                            .get_rect()
                            .center()
                    })
                    .unwrap();
                app.harness().click_at(next.x, next.y);
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(
                        preview.journey.page,
                        Page::Devices,
                        "pointer coordinates must match the resized painted control"
                    );
                    preview.journey.page = Page::Password;
                    preview.dirty = true;
                })
                .unwrap();
                paint_app(
                    &mut app,
                    "installer-password",
                    &[
                        "installer.password",
                        "installer.confirmation",
                        "installer.next",
                    ],
                    viewport,
                    &format!("logical-size-{host_scale}"),
                );
                app.harness().click_named("installer.back").unwrap();
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(preview.journey.page, Page::Profile);
                })
                .unwrap();
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn destination_review_edit_exposes_cancel_and_restores_choices_through_public_actions() {
    for (viewport, theme) in [((480, 800), Theme::dark()), ((1180, 820), Theme::light())] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            // Keep the launched resource's real action registration. Only
            // stage the reviewed choices, rather than using a static capture.
            preview.live = false;
            preview.capture = true;
            preview.restart = restart_controller::Restart::default();
            preview.journey = Journey::new("Ada Lovelace".into());
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.identity = Some(Identity::New);
            preview.journey.page = Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        let original = app
            .effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(original["page"], "review");
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            assert!(preview.journey.cancels_edit());
            inspect_controls(preview, &["installer.back", "installer.next"], viewport);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "another".into(),
        })
        .unwrap();
        assert_eq!(
            app.effect(&InstallerPreviewEffect::ReadPreview)
                .unwrap()
                .value
                .unwrap()["destination"],
            "another"
        );
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        let restored = app
            .effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(restored["page"], "review");
        for field in ["destination", "name", "device_name", "identity", "migrate"] {
            assert_eq!(
                restored[field], original[field],
                "changed {field} on cancel"
            );
        }
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.destination, Some(Destination::Computer));
            inspect_controls(preview, &["installer.back"], viewport);
        })
        .unwrap();
    }
}

#[test]
fn review_summary_scrolls_without_moving_navigation_at_large_text_sizes() {
    for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let viewport = (2560, 1440);
        let mut app =
            TestApp::<GuidedInstallerShell>::capture_at(Some("installer-review"), viewport)
                .unwrap();
        app.harness()
            .set_theme(theme.with_ui_scale(2.0).with_reduce_motion(true));
        app.settle();
        paint_app(
            &mut app,
            "installer-review-top",
            &["installer.next", "installer.back"],
            viewport,
            tag,
        );
        let (next, back, last, heading) = app
            .with_widget::<InstallerPreview, _>(|preview| {
                inspect_controls(preview, &["installer.next", "installer.back"], viewport);
                (
                    control(preview, "installer.next").unwrap().get_rect(),
                    control(preview, "installer.back").unwrap().get_rect(),
                    control(preview, "installer.review.flatpak")
                        .unwrap()
                        .get_rect(),
                    control(preview, "installer.heading").unwrap().get_rect(),
                )
            })
            .unwrap();
        assert!(last.y1 > next.y0, "the real summary must overflow its body");
        eprintln!(
            "Review initial geometry {tag}: heading={heading:?}, next={next:?}, back={back:?}, last={last:?}"
        );
        assert!(
            heading.y0 >= 0.0 && heading.y1 < next.y0,
            "Review heading must initially be visible above footer: heading={heading:?}, next={next:?}, back={back:?}, last={last:?}"
        );
        // Pointer input is in logical window coordinates. Anchor it to the
        // visible heading inside the real scroll body, not window padding.
        app.harness().click_at(
            f64::midpoint(heading.x0, heading.x1),
            f64::midpoint(heading.y0, heading.y1),
        );
        for _ in 0..40 {
            app.harness().scroll(0.0, -10.0);
        }
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let last = control(preview, "installer.review.flatpak")
                .unwrap()
                .get_rect();
            assert!(
                last.y0 >= 0.0 && last.y1 <= next.y0,
                "summary stays above footer: {last:?}"
            );
            assert_eq!(control(preview, "installer.next").unwrap().get_rect(), next);
            assert_eq!(control(preview, "installer.back").unwrap().get_rect(), back);
        })
        .unwrap();
        paint_app(
            &mut app,
            "installer-review-scrolled",
            &["installer.next", "installer.back"],
            viewport,
            tag,
        );
    }
}

#[test]
fn guided_pages_paint_at_small_normal_and_large_host_scales_in_both_themes() {
    let screens: &[(&str, &[&str])] = &[
        (
            "installer-destination",
            &["installer.choice.computer", "installer.choice.another"],
        ),
        (
            "installer-profile",
            &["installer.avatar", "installer.name", "installer.next"],
        ),
        (
            "installer-password",
            &[
                "installer.password",
                "installer.confirmation",
                "installer.next",
            ],
        ),
        (
            "installer-devices",
            &["installer.device-name", "installer.next"],
        ),
        (
            "installer-belongings",
            &[
                "installer.category.files",
                "installer.category.accounts",
                "installer.details",
                "installer.next",
            ],
        ),
        (
            "installer-storage",
            &["installer.storage.size", "installer.next"],
        ),
        (
            "installer-review",
            &["installer.review.destination", "installer.next"],
        ),
    ];
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for (viewport, scale) in [
            ((480, 800), 1.0),
            ((800, 600), 1.0),
            ((1180, 820), 1.0),
            ((2560, 1440), 2.0),
        ] {
            for (scenario, ids) in screens {
                paint_screen(
                    scenario,
                    ids,
                    viewport,
                    theme.clone().with_ui_scale(scale),
                    name,
                );
            }
        }
    }
}

#[cfg(unix)]
#[track_caller]
fn wait_for_preview(
    app: &mut TestApp<GuidedInstallerShell>,
    predicate: impl Fn(&InstallerPreview) -> bool,
) {
    fn ready(root: &dyn Widget, predicate: &impl Fn(&InstallerPreview) -> bool) -> bool {
        if let Some(preview) = root
            .as_any()
            .and_then(|widget| widget.downcast_ref::<InstallerPreview>())
        {
            return predicate(preview);
        }
        root.children()
            .into_iter()
            .any(|child| ready(child, predicate))
    }
    let settled = app
        .harness()
        .wait_until(ui::time::Duration::from_secs(30), |harness| {
            ready(harness.root_widget(), &predicate)
        });
    if !settled {
        let state = app.effect(&InstallerPreviewEffect::ReadPreview).unwrap();
        panic!("installer preparation did not settle: {state:?}");
    }
}

#[cfg(unix)]
fn wait_for_fresh_preparation(app: &mut TestApp<GuidedInstallerShell>) {
    wait_for_preview(app, |preview| {
        preview.restart.projection()["state"] == "missing"
            && !preview.restart.busy()
            && !preview.personal_busy()
            && !preview.direct.active
    });
    let state = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(state["restart"]["state"], "missing", "{state}");
    assert_eq!(state["page"], "destination", "{state}");
}

#[cfg(unix)]
fn identify_other_computer(app: &mut TestApp<GuidedInstallerShell>) {
    for (field, value) in [
        ("manufacturer", "Test Vendor"),
        ("model", "Other Test Laptop"),
        ("variant", "OTHER-TEST-BOARD"),
    ] {
        app.effect(&InstallerPreviewEffect::SetTargetIdentityField {
            field: field.into(),
            value: value.into(),
        })
        .unwrap();
    }
}

#[cfg(unix)]
fn choose(app: &mut TestApp<GuidedInstallerShell>, choice: &str) {
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: choice.into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    let page = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()["page"]
        .clone();
    if page == "target" {
        identify_other_computer(app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
    }
}

#[cfg(target_os = "linux")]
fn pairing_name_app(viewport: (u32, u32), theme: Theme) -> TestApp<GuidedInstallerShell> {
    // Live semantic actions hosted offscreen on a fresh test profile; capture mode
    // intentionally omits this mutable action registry.
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.harness().set_theme(theme.with_reduce_motion(true));
    app.harness().resize(viewport).unwrap();
    wait_for_fresh_preparation(&mut app);
    // This route prepares another computer without opening the current host's
    // disk compatibility check, which owns navigation until explicitly reviewed.
    for choice in ["another", "fresh", "existing"] {
        choose(&mut app, choice);
    }
    app
}

#[cfg(target_os = "linux")]
#[test]
fn pairing_name_and_approval_code_paint_at_all_host_scales_in_both_themes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set("XDG_CONFIG_HOME", root.join("config"));
    for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
            let mut app = pairing_name_app(viewport, theme.clone().with_ui_scale(scale));
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: "device".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
                value: "My future computer".into(),
            })
            .unwrap();
            paint_app(
                &mut app,
                "installer-pairing-name",
                &["installer.device-name", "installer.pairing.start"],
                viewport,
                tag,
            );
            let (_custody, waiting) = pairing::waiting_fixture();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.pairing = waiting;
                // The fixture substitutes only the owned process started by the
                // action; production apply marks this initial transition dirty.
                preview.dirty = true;
            })
            .unwrap();
            wait_for_preview(&mut app, |preview| {
                preview.pairing.projection()["verification_code"] == "123456"
            });
            paint_app(
                &mut app,
                "installer-pairing-approval",
                &["installer.pairing.code", "installer.pairing.cancel"],
                viewport,
                tag,
            );
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(control(preview, "installer.device-name").is_none());
                assert!(control(preview, "installer.pairing.start").is_none());
                assert!(preview.pairing.busy());
                assert!(!preview.pairing.ready());
                assert!(!control(preview, "installer.back").unwrap().is_enabled());
            })
            .unwrap();
            assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
            app.effect(&InstallerPreviewEffect::CancelExistingDevicePairing)
                .unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| assert!(!preview.pairing.busy()))
                .unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn account_overview_and_explicit_provider_choice_paint_with_matching_navigation() {
    let custody = tempfile::tempdir().unwrap();
    let root = custody.path().canonicalize().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set("TILO_OAUTH_SPOTIFY_CLIENT_ID", "installer-render-client");
    env.set("XDG_CONFIG_HOME", root.join("config"));
    for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
            env.set("XDG_STATE_HOME", root.join(format!("{tag}-{}", viewport.0)));
            let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
            wait_for_fresh_preparation(&mut app);
            // Identity/account rendering does not require a writable host disk.
            for choice in ["another", "fresh", "new"] {
                choose(&mut app, choice);
            }
            app.effect(&InstallerPreviewEffect::SetPreviewName {
                value: "Ada Lovelace".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            wait_for_preview(&mut app, |preview| {
                preview.journey.page == Page::Devices
                    && !preview.personal_busy()
                    && !preview.restart.busy()
            });
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.journey.page = Page::Accounts;
                preview.dirty = true;
            })
            .unwrap();
            app.harness().resize(viewport).unwrap();
            app.harness()
                .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
            wait_for_preview(&mut app, |preview| {
                preview.accounts.can_continue() && preview.restart.projection()["checking"] == false
            });
            paint_app(
                &mut app,
                "installer-account-overview",
                &["installer.account.add", "installer.next"],
                viewport,
                tag,
            );
            let overview = app.harness().inspection_json().unwrap();
            assert!(!overview.contains("installer.account.provider.google-drive"));
            app.effect(&InstallerPreviewEffect::AccountStep {
                step: "add-account".into(),
                selection: None,
            })
            .unwrap();
            paint_app(
                &mut app,
                "installer-account-providers",
                &["installer.account.provider.google-drive", "installer.back"],
                viewport,
                tag,
            );
            assert!(
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .is_err(),
                "a service choice cannot be skipped by Continue"
            );
            paint_spotify_choices(&mut app, viewport, tag);
            app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
            app.settle();
            let returned = app.harness().inspection_json().unwrap();
            assert!(returned.contains("installer.account.add"));
            assert!(!returned.contains("installer.account.provider.google-drive"));
        }
    }
}

fn paint_spotify_choices(app: &mut TestApp<GuidedInstallerShell>, viewport: (u32, u32), tag: &str) {
    let providers = app.harness().inspection_json().unwrap();
    assert!(providers.contains("installer.account.provider.spotify"));
    assert!(!providers.contains("installer.account.provider.spotify-streaming"));
    app.effect(&InstallerPreviewEffect::AccountStep {
        step: "choose-provider".into(),
        selection: Some("spotify-options".into()),
    })
    .unwrap();
    paint_app(
        app,
        "installer-spotify-purpose",
        &[
            "installer.account.spotify.spotify",
            "installer.account.spotify.spotify-streaming",
            "installer.back",
        ],
        viewport,
        tag,
    );
    let purpose = app.harness().inspection_json().unwrap();
    assert!(!purpose.contains("installer.account.provider.google-drive"));
    assert!(!purpose.contains("installer.account.confirm"));
    app.effect(&InstallerPreviewEffect::AccountStep {
        step: "choose-provider".into(),
        selection: Some("spotify-streaming".into()),
    })
    .unwrap();
    paint_app(
        app,
        "installer-spotify-playback-consent",
        &["installer.account.confirm", "installer.back"],
        viewport,
        tag,
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        let back = control(preview, "installer.back").unwrap().get_rect();
        let connect = control(preview, "installer.account.confirm")
            .unwrap()
            .get_rect();
        assert!(connect.x0 > back.x1, "Connect belongs at the trailing edge");
        assert!(
            (connect.y0 - back.y0).abs() < 1.0,
            "Back and Connect must share the navigation row"
        );
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    app.settle();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("installer.account.spotify.spotify-streaming")
    );
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    app.settle();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("installer.account.provider.spotify")
    );
}

#[cfg(target_os = "linux")]
#[cfg(unix)]
fn account_motion_fixture() -> TestApp<GuidedInstallerShell> {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = true;
        preview.capture = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey.page = Page::Accounts;
        preview.pending_slide = None;
        preview.slide = None;
        preview.dirty = true;
    })
    .unwrap();
    app.harness().resize((1180, 820)).unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(false));
    app.settle();
    app
}

#[cfg(target_os = "linux")]
fn account_motion_step(
    app: &mut TestApp<GuidedInstallerShell>,
    step: &str,
    selection: Option<&str>,
) {
    app.effect(&InstallerPreviewEffect::AccountStep {
        step: step.into(),
        selection: selection.map(str::to_owned),
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
fn motion_frames(app: &mut TestApp<GuidedInstallerShell>, name: &str, direction: f64) {
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.slide.as_ref().unwrap().0, direction);
        assert!(preview.outgoing.is_some());
    })
    .unwrap();
    let mut previous = None;
    for percent in [0, 25, 50, 75, 100] {
        app.with_widget::<InstallerPreview, _>(|preview| {
            let presence = if percent == 100 {
                // The last frame is the settled page, which the production draw
                // must release instead of keeping a capture pose forever.
                ui::motion::Presence::opened()
            } else {
                ui::motion::Presence::held_at(u16::try_from(percent * 10).unwrap())
            };
            preview.slide = Some((direction, presence));
        })
        .unwrap();
        // with_widget relays the production layout/draw pass; this rasterizes
        // that actual cached scene without substituting a screenshot widget.
        let pixels = app.harness().render_rgba();
        assert_eq!(pixels.len(), 1180 * 820 * 4);
        if let Some(previous) = previous.replace(pixels.clone()) {
            assert_ne!(pixels, previous, "{name} at {percent}% did not move");
        }
        if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
            let directory = PathBuf::from(directory).join("installer-motion");
            std::fs::create_dir_all(&directory).unwrap();
            app.harness()
                .render_png(&directory.join(format!("{name}-{percent:03}.png")));
        }
    }
    // A held capture pose stays until released; at rest at its end, the
    // next frame retires the transition and its departing scene.
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.slide = Some((direction, ui::motion::Presence::opened()));
    })
    .unwrap();
    app.harness().render_rgba();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.slide.is_none());
        assert!(preview.outgoing.is_none());
    })
    .unwrap();
}

/// Sample the actual page scene while the production transition is active.
/// Stretch only the test's clock so rasterization speed cannot finish the
/// animation before assertions; neither page is replaced by a fixture widget.
fn personal_motion_frames(
    app: &mut TestApp<GuidedInstallerShell>,
    name: &str,
    direction: f64,
    viewport: (u32, u32),
    control_height: f64,
) {
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            control(preview, "installer.next")
                .unwrap()
                .get_rect()
                .height(),
            control_height,
            "{name}: controls resized"
        );
        assert_eq!(
            preview.slide.as_ref().expect("page transition was lost").0,
            direction
        );
        assert!(
            preview.outgoing.is_some(),
            "{name}: departing scene was lost"
        );
    })
    .unwrap();
    let mut previous = None;
    for percent in [0, 50, 100] {
        app.with_widget::<InstallerPreview, _>(|preview| {
            let presence = if percent == 100 {
                ui::motion::Presence::opened()
            } else {
                ui::motion::Presence::held_at(u16::try_from(percent * 10).unwrap())
            };
            preview.slide = Some((direction, presence));
        })
        .unwrap();
        let pixels = app.harness().render_rgba();
        assert_eq!(pixels.len(), viewport.0 as usize * viewport.1 as usize * 4);
        if let Some(previous) = previous.replace(pixels.clone()) {
            assert_ne!(pixels, previous, "{name}: {percent}% did not move");
        }
        if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
            let directory = PathBuf::from(directory).join("installer-motion");
            std::fs::create_dir_all(&directory).unwrap();
            app.harness().render_png(&directory.join(format!(
                "{name}-{}x{}-{percent:03}.png",
                viewport.0, viewport.1
            )));
        }
    }
    // A held capture pose stays until released; at rest at its end, the
    // next frame retires the transition and its departing scene.
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.slide = Some((direction, ui::motion::Presence::opened()));
    })
    .unwrap();
    app.harness().render_rgba();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.slide.is_none());
        assert!(preview.outgoing.is_none());
    })
    .unwrap();
}

fn validation_copy<'a>(root: &'a dyn Widget, error: &str) -> Option<&'a dyn Widget> {
    if root.accessibility().is_some_and(|node| {
        node.name.as_deref() == Some(t(error).as_str())
            && node.live == Some(ui::a11y::Politeness::Assertive)
    }) {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| validation_copy(child, error))
}

fn inspect_password_validation(preview: &InstallerPreview, field: &str, error: &str) {
    let input = control(preview, field).unwrap();
    let access = input.accessibility().unwrap();
    assert!(access.state.invalid, "rejected field is marked invalid");
    assert!(
        access.state.protected,
        "validation must not unmask the password"
    );
    assert!(
        access.value.is_none(),
        "validation must not expose the password"
    );
    let copy = validation_copy(preview, error).expect("assertive field validation copy");
    assert!(
        copy.get_rect().y0 >= input.get_rect().y1,
        "validation belongs immediately below its input"
    );
    assert!(
        (copy.get_rect().x0 - input.get_rect().x0).abs() < 1.0,
        "validation aligns with its input"
    );
    assert!(
        control(preview, "installer.error").is_none(),
        "field errors are not repeated as banners"
    );
}

pub(super) fn body_scroll_geometry(root: &mut dyn Widget) -> Option<(vello::kurbo::Rect, bool)> {
    if let Some(scroll) = root
        .as_any_mut()
        .and_then(|widget| widget.downcast_mut::<Scrollable>())
    {
        return Some((scroll.content_viewport_rect(), scroll.vertical_overflows()));
    }
    root.children_mut()
        .into_iter()
        .find_map(body_scroll_geometry)
}

#[test]
#[cfg(unix)]
fn personal_pages_keep_navigation_fixed_and_scroll_errors_above_it() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for (viewport, host_scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
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
            preview.journey.history = vec![Page::Destination, Page::Migration, Page::Identity];
            preview.dirty = true;
        })
        .unwrap();
        app.harness().set_theme(
            Theme::dark()
                .with_ui_scale(host_scale)
                .with_reduce_motion(true),
        );
        app.harness().resize(viewport).unwrap();
        app.settle();
        let (back, next) = app
            .with_widget::<InstallerPreview, _>(|preview| {
                inspect_controls(
                    preview,
                    &["installer.name", "installer.back", "installer.next"],
                    viewport,
                );
                let heading = control(preview, "installer.heading").unwrap().get_rect();
                let name = control(preview, "installer.name").unwrap().get_rect();
                let (body, overflowing) = body_scroll_geometry(preview).unwrap();
                assert!(!overflowing, "short profile should fit: {body:?}");
                let visible_center = f64::midpoint(heading.y0, name.y1);
                assert!(
                    (visible_center - body.center().y).abs() < 40.0 * f64::from(host_scale),
                    "short body stays centered: heading={heading:?}, name={name:?}, body={body:?}"
                );
                (
                    control(preview, "installer.back").unwrap().get_rect(),
                    control(preview, "installer.next").unwrap().get_rect(),
                )
            })
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Password;
            preview.dirty = true;
        })
        .unwrap();
        for effect in [
            InstallerPreviewEffect::SetPreviewPassword {
                value: "several unrelated installation words".into(),
            },
            InstallerPreviewEffect::SetPreviewConfirmation {
                value: "several unrelated installation words".into(),
            },
            InstallerPreviewEffect::ContinuePreview,
        ] {
            app.effect(&effect).unwrap();
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Devices);
            assert_eq!(control(preview, "installer.back").unwrap().get_rect(), back);
            assert_eq!(control(preview, "installer.next").unwrap().get_rect(), next);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(control(preview, "installer.back").unwrap().get_rect(), back);
            assert_eq!(control(preview, "installer.next").unwrap().get_rect(), next);
        })
        .unwrap();
        // Submit a real short password through the semantic action (the
        // visible Continue is disabled), then overflow the error/form.
        app.effect(&InstallerPreviewEffect::SetPreviewPassword {
            value: "short".into(),
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Password);
            assert!(
                preview.error.is_some(),
                "semantic Continue must show validation"
            );
            inspect_password_validation(
                preview,
                "installer.password",
                "installer-preview-password-short",
            );
        })
        .unwrap();
        app.harness()
            .set_theme(Theme::dark().with_ui_scale(2.0).with_reduce_motion(true));
        app.harness().resize((480, 800)).unwrap();
        app.settle();
        let (back, next, body) = app
            .with_widget::<InstallerPreview, _>(|preview| {
                inspect_controls(preview, &["installer.back", "installer.next"], (480, 800));
                let (body, overflowing) = body_scroll_geometry(preview).unwrap();
                assert!(overflowing, "the real error/form must overflow: {body:?}");
                let next = control(preview, "installer.next").unwrap().get_rect();
                assert!(body.y1 <= next.y0, "scroll body must stop above navigation");
                (
                    control(preview, "installer.back").unwrap().get_rect(),
                    next,
                    body,
                )
            })
            .unwrap();
        app.harness().click_at(body.center().x, body.center().y);
        for _ in 0..40 {
            app.harness().scroll(0.0, -10.0);
        }
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(control(preview, "installer.back").unwrap().get_rect(), back);
            assert_eq!(control(preview, "installer.next").unwrap().get_rect(), next);
            let error = validation_copy(preview, "installer-preview-password-short")
                .unwrap()
                .get_rect();
            let confirmation = control(preview, "installer.confirmation")
                .unwrap()
                .get_rect();
            assert!(
                confirmation.y0 >= body.y0 && confirmation.y1 <= body.y1,
                "confirmation remains reachable with the validation: {confirmation:?}, {body:?}"
            );
            assert!(
                error.y0 >= body.y0 && error.y1 <= body.y1,
                "validation error is reachable within scrolling body: {error:?}, {body:?}"
            );
        })
        .unwrap();
        paint_app(
            &mut app,
            "installer-password-error-scrolled",
            &["installer.back", "installer.next"],
            (480, 800),
            "dark",
        );
    }
}

#[test]
#[cfg(unix)]
fn personal_pages_keep_control_scale_and_rasterize_forward_and_back_motion() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for (viewport, host_scale) in [((1180, 820), 1.0), ((2560, 1440), 1.0), ((2560, 1440), 2.0)] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            // Synthetic public profile only; keep the launched app's real
            // effect registry, journey navigation, shared widgets and painter.
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
        let mut theme = Theme::dark()
            .with_ui_scale(host_scale)
            .with_reduce_motion(false);
        theme.tokens.motion.durations.base_ms = 30_000;
        app.harness().set_theme(theme);
        app.harness().resize(viewport).unwrap();
        app.settle();
        let next_height = app
            .with_widget::<InstallerPreview, _>(|preview| {
                inspect_controls(preview, &["installer.name", "installer.next"], viewport);
                control(preview, "installer.next")
                    .unwrap()
                    .get_rect()
                    .height()
            })
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        personal_motion_frames(&mut app, "profile-to-password", 1.0, viewport, next_height);
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Password;
            preview.dirty = true;
        })
        .unwrap();
        for effect in [
            InstallerPreviewEffect::SetPreviewPassword {
                value: "several unrelated installation words".into(),
            },
            InstallerPreviewEffect::SetPreviewConfirmation {
                value: "several unrelated installation words".into(),
            },
            InstallerPreviewEffect::ContinuePreview,
        ] {
            app.effect(&effect).unwrap();
        }
        personal_motion_frames(&mut app, "password-to-devices", 1.0, viewport, next_height);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Devices);
            inspect_controls(
                preview,
                &["installer.device-name", "installer.next"],
                viewport,
            );
            assert_eq!(
                control(preview, "installer.next")
                    .unwrap()
                    .get_rect()
                    .height(),
                next_height
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        personal_motion_frames(&mut app, "devices-to-password", -1.0, viewport, next_height);
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        personal_motion_frames(&mut app, "password-to-profile", -1.0, viewport, next_height);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Profile)
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn resizing_or_changing_host_scale_discards_old_transition_geometry() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    // Captures publish inert descriptions, not executable action bindings.
    // Exercise transitions on the launched root's real effect runner.
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey = Journey::new("Ada Lovelace".into());
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.migrate = Some(false);
        preview.journey.identity = Some(Identity::New);
        preview.journey.page = Page::Profile;
        preview.dirty = true;
    })
    .unwrap();
    app.harness().resize((1180, 820)).unwrap();
    let mut theme = Theme::dark().with_reduce_motion(false);
    theme.tokens.motion.durations.base_ms = 30_000;
    app.harness().set_theme(theme.clone());
    app.settle();
    for change_scale in [false, true] {
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                preview.slide.is_some(),
                "stable geometry retains page transitions"
            );
            assert!(preview.outgoing.is_some());
        })
        .unwrap();
        if change_scale {
            app.harness().set_theme(theme.clone().with_ui_scale(1.5));
        } else {
            app.harness().resize((5120, 2822)).unwrap();
        }
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.slide.is_none());
            assert!(preview.outgoing.is_none());
            assert!(preview.pending_slide.is_none());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                preview.slide.is_some(),
                "navigation resumes at the new geometry"
            );
            assert!(preview.outgoing.is_some());
            preview.slide = None;
            preview.outgoing = None;
        })
        .unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn account_decision_pages_slide_forward_back_and_block_pointer_until_settled() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_OAUTH_SPOTIFY_CLIENT_ID", "installer-motion-client");
    let mut app = account_motion_fixture();
    account_motion_step(&mut app, "add-account", None);
    motion_frames(&mut app, "accounts-overview-to-providers", 1.0);
    account_motion_step(&mut app, "choose-provider", Some("spotify-options"));
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.accounts.navigation_step(), "spotify-purpose");
    })
    .unwrap();
    motion_frames(&mut app, "accounts-providers-to-purpose", 1.0);
    account_motion_step(&mut app, "choose-provider", Some("spotify-streaming"));
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.accounts.navigation_step(), "confirm");
        let direction = preview.slide.as_ref().unwrap().0;
        // Hold the midpoint for deterministic pointer delivery.
        preview.slide = Some((direction, ui::motion::Presence::held_at(500)));
    })
    .unwrap();
    app.harness().click_named("installer.back").unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview.accounts.navigation_step(),
            "confirm",
            "a pointer must not activate a destination rectangle while its page is moving"
        );
    })
    .unwrap();
    // A deliberate public action remains usable by automation while painting.
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    motion_frames(&mut app, "accounts-consent-to-purpose", -1.0);
    account_motion_step(&mut app, "back", None);
    motion_frames(&mut app, "accounts-purpose-to-providers", -1.0);
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.harness().click_named("installer.back").unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview.accounts.navigation_step(),
            "overview",
            "pointer navigation must resume after the transition finishes"
        );
        assert!(preview.slide.is_none());
        assert!(preview.outgoing.is_none());
        assert!(preview.pending_slide.is_none());
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn account_field_pages_advance_through_public_continue_and_preserve_keyboard_focus() {
    let mut app = account_motion_fixture();
    account_motion_step(&mut app, "add-account", None);
    motion_frames(&mut app, "accounts-field-provider-selection", 1.0);
    account_motion_step(&mut app, "choose-provider", Some("webdav"));
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.slide = Some((1.0, ui::motion::Presence::held_at(500)));
    })
    .unwrap();
    app.harness()
        .perform_named("installer.account.dav.server", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().type_text("https://dav.example.test");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(
            preview.slide.is_some(),
            "keyboard edits must work during the page slide"
        );
        assert!(
            control(preview, "installer.account.dav.server")
                .unwrap()
                .is_focused(),
            "editing must preserve focus through the page's production recomposition"
        );
        assert_eq!(preview.accounts.navigation_step(), "dav-server");
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Accounts);
        assert_eq!(preview.accounts.navigation_step(), "dav-user");
    })
    .unwrap();
    motion_frames(&mut app, "accounts-server-to-user", 1.0);
    app.effect(&InstallerPreviewEffect::SetAccountField {
        field: "login".into(),
        value: "Ada".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.accounts.navigation_step(), "dav-password");
    })
    .unwrap();
    motion_frames(&mut app, "accounts-user-to-password", 1.0);
    // No final confirmation: this story proves field navigation, not a network
    // login, and never launches an authorization browser or a graphical app.
}

/// Called only after native qualification discovered an actual signed
/// installation as its original user. No application inventory is fabricated.
#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
pub(super) fn paint_native_flatpak_inventory(
    home: &std::path::Path,
    inventory: &super::super::flatpak_transfer::Inventory,
) {
    assert!(inventory.refs.iter().any(|item| item.app_id().is_some()));
    for destination in [Destination::Computer, Destination::Another] {
        for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
                // Actions need a live headless host; captures only publish metadata.
                let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
                app.harness().resize(viewport).unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.restart = restart_controller::Restart::default();
                    preview.journey.page = Page::Belongings;
                    preview.journey.destination = Some(destination);
                    preview.journey.migrate = Some(true);
                    preview.journey.categories = [true, true, false];
                    preview.journey.details = false;
                    preview.belongings = belongings::Belongings {
                        home: Some(home.to_owned()),
                        flatpak: Some(inventory.clone()),
                        ..Default::default()
                    };
                    preview.dirty = true;
                })
                .unwrap();
                app.harness()
                    .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
                let expected = t_args(
                    if destination == Destination::Computer {
                        "installer-preview-flatpak-found"
                    } else {
                        "installer-preview-flatpak-system-found"
                    },
                    &[(
                        "count",
                        inventory
                            .refs
                            .iter()
                            .filter(|item| item.app_id().is_some())
                            .count()
                            .to_string()
                            .into(),
                    )],
                );
                assert!(app.harness().inspection_json().unwrap().contains(&expected));
                paint_app(
                    &mut app,
                    if destination == Destination::Computer {
                        "installer-native-apps-shared-home"
                    } else {
                        "installer-native-apps-portable-home"
                    },
                    &[
                        "installer.category.flatpak",
                        "installer.details",
                        "installer.next",
                    ],
                    viewport,
                    tag,
                );
                let expanded = app
                    .effect(&InstallerPreviewEffect::TogglePreviewDetails)
                    .unwrap()
                    .value
                    .unwrap();
                let apps = expanded["belongings"]["apps"].as_array().unwrap();
                assert!(apps.iter().any(|app| app["included"] == true));
                let row = apps.iter().position(|app| app["included"] == true).unwrap();
                let native_app = &apps[row];
                assert!(inventory.refs.iter().any(|item| item.app_id().is_some()
                    && (item.name.trim() == native_app["name"].as_str().unwrap()
                        || item.name.trim().is_empty()
                            && item.app_id() == native_app["name"].as_str())));
                app.settle();
                let view = app.harness().inspection_json().unwrap();
                assert!(view.contains(native_app["name"].as_str().unwrap()));
                assert!(
                    !view.contains(home.to_str().unwrap()),
                    "full source location remains behind explicit details action"
                );
                let app_id = format!("installer.flatpak.app.{row}");
                paint_app(
                    &mut app,
                    if destination == Destination::Computer {
                        "installer-native-app-names-shared-home"
                    } else {
                        "installer-native-app-names-portable-home"
                    },
                    &[&app_id, "installer.details", "installer.next"],
                    viewport,
                    tag,
                );
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn destination_discloses_usb_route_before_personal_setup() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for runtime_gate in ["0", "1"] {
        environment.set(
            super::super::internal_install_flow::RUNTIME_GATE_VAR,
            runtime_gate,
        );
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.capture = false;
            preview.restart = restart_controller::Restart::default();
            preview.journey = Journey::new(String::new());
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            assert!(preview.journey.name.is_empty());
            let name = control(preview, "installer.choice.computer")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .unwrap();
            let expected = if super::super::internal_install_flow::BUILD_GATE == Some("1")
                || runtime_gate == "1"
            {
                t("installer-preview-computer-body")
            } else {
                t("installer-preview-computer-usb-body")
            };
            assert!(
                name.contains(&expected),
                "the first choice must disclose its actual route: {name}"
            );
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn live_migration_choice_does_not_inspect_sources_before_profile() {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.stage(Some("installer-migration"));
        preview.restart = restart_controller::Restart::default();
        preview.live = true;
        preview.capabilities.prepared_installation = true;
        preview.journey.identity = None;
        preview.journey.name.clear();
        preview.belongings = belongings::Belongings::default();
        preview.dirty = true;
    })
    .unwrap();
    app.settle();
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: "migrate".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: "new".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Profile);
        assert!(
            preview.belongings_worker.is_none(),
            "inventory must not race identity setup"
        );
        assert!(preview.belongings.home.is_none());
        assert!(preview.belongings.roots.is_none());
        assert!(preview.belongings.accounts.candidates.is_empty());
    })
    .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::CheckBelongings)
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn belongings_entry_discovers_real_metadata_after_profile_and_review_edit() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::write(
        config.join("Nextcloud/nextcloud.cfg"),
        r"[Accounts]
0\url=https://cloud.example
0\http_user=ada
0\authType=http
",
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &config);
    for edit in [false, true] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        wait_for_fresh_preparation(&mut app);
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.stage(Some("installer-belongings"));
            preview.live = true;
            preview.capabilities.prepared_installation = true;
            preview.journey.destination = Some(Destination::Another);
            preview.belongings = belongings::Belongings::default();
            if edit {
                preview.journey.page = Page::Review;
                preview.journey.migrate = Some(false);
                preview.journey.edit("belongings").unwrap();
            } else {
                // The ordinary interactive route continues from Devices into
                // Your data; discovery runs there, never before identity.
                preview.journey.page = Page::Devices;
                preview.journey.migrate = Some(true);
                preview.journey.identity = Some(Identity::New);
                preview.journey.name = "Ada".into();
                preview.journey.device_name = "Ada's computer".into();
            }
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        if edit {
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: "migrate".into(),
            })
            .unwrap();
        }
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Belongings);
            // A fast discovery may already have been polled by the effect's
            // frame. The granted roots and discovered provider below prove the
            // production operation, not the lifetime of its receiver.
        })
        .unwrap();
        wait_for_preview(&mut app, |preview| {
            preview.belongings_worker.is_none()
                && preview.belongings.home.as_ref() == Some(&home)
                && preview.belongings.roots.is_some()
                && preview
                    .belongings
                    .accounts
                    .candidates
                    .iter()
                    .any(|account| {
                        account.provider == super::super::account_transfer::Provider::Nextcloud
                    })
        });
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.belongings.home.as_ref(), Some(&home));
            assert!(preview.belongings.roots.is_some());
            assert!(
                preview
                    .belongings
                    .accounts
                    .candidates
                    .iter()
                    .any(|account| {
                        account.provider == super::super::account_transfer::Provider::Nextcloud
                    })
            );
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn profile_keeps_picture_action_and_name_without_redundant_explanation() {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.stage(Some("installer-profile"));
        preview.restart = restart_controller::Restart::default();
    })
    .unwrap();
    app.settle();
    let view = app.harness().inspection_json().unwrap();
    assert!(view.contains(&t("installer-preview-avatar-change")));
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(control(preview, "installer.profile.avatar-help").is_none());
        assert!(control(preview, "installer.avatar").is_some());
        assert!(control(preview, "installer.name").is_some());
        assert!(control(preview, "installer.category.files").is_none());
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Devices);
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Profile);
        assert_eq!(preview.journey.name, "Ada Lovelace");
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn account_overview_explains_where_to_connect_after_setup() {
    let mut app = account_motion_fixture();
    let view = app.harness().inspection_json().unwrap();
    assert!(view.contains(&t("installer-accounts-intro")));
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.accounts.navigation_step(), "overview");
        assert!(control(preview, "installer.account.add").is_some());
    })
    .unwrap();
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.harness().resize((800, 600)).unwrap();
        paint_app(
            &mut app,
            "installer-accounts-connect-later",
            &["installer.account.add", "installer.next"],
            (800, 600),
            tag,
        );
    }
}

#[test]
fn migration_route_consequences_are_present_in_the_semantic_choice() {
    for (destination, key) in [
        (
            Destination::Computer,
            "installer-preview-migrate-shared-body",
        ),
        (
            Destination::Another,
            "installer-preview-migrate-portable-body",
        ),
    ] {
        let mut app =
            TestApp::<GuidedInstallerShell>::capture(Some("installer-migration")).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.destination = Some(destination);
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-preview-migration-intro"))
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            let name = control(preview, "installer.choice.migrate")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .unwrap();
            assert!(
                name.contains(&t(key)),
                "route consequences must be announced: {name}"
            );
        })
        .unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn unavailable_guided_host_paints_honest_state_and_refuses_semantic_navigation() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        wait_for_fresh_preparation(&mut app);
        app.with_widget::<InstallerPreview, _>(|preview| {
            // Explicit unavailable-host fixture through the production capability
            // gate, not an installation receipt or a synthetic success route.
            preview.live = true;
            preview.capture = false;
            preview.capabilities.prepared_installation = false;
            preview.journey = Journey::new(String::new());
            preview.dirty = true;
        })
        .unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.harness().resize((800, 600)).unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-guided-host-unavailable"))
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(control(preview, "installer.choice.computer").is_none());
            assert!(!preview.guided_host_available());
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: "computer".into()
            })
            .is_err()
        );
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        paint_app(
            &mut app,
            "installer-guided-host-unavailable",
            &[],
            (800, 600),
            tag,
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn missing_direct_preparation_stays_disabled_and_refuses_installation() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    environment.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            // Intentionally missing custody/host plan; no preparation success is
            // injected. The real live admission must reject this review state.
            preview.live = true;
            preview.capture = false;
            preview.restart = restart_controller::Restart::default();
            preview.journey = Journey::new("Ada Lovelace".into());
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(false);
            preview.journey.identity = Some(Identity::New);
            preview.journey.page = Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.harness().resize((800, 600)).unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.direct_available());
            let button = control(preview, "installer.prepare-direct").unwrap();
            assert!(button.accessibility().unwrap().state.disabled);
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::PrepareDirectInstallation)
                .is_err()
        );
        paint_app(
            &mut app,
            "installer-direct-preparation-missing",
            &["installer.prepare-direct"],
            (800, 600),
            tag,
        );
    }
}

#[test]
#[cfg(unix)]
fn semantic_validation_error_matches_inline_feedback_and_survives_read_preview() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    let viewport = (1180, 820);
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        // Public profile fixture only. Exercise the launched resource's real
        // effects, validation, shared controls and visible error composition.
        preview.live = false;
        preview.capture = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey = Journey::new("Ada Lovelace".into());
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.migrate = Some(false);
        preview.journey.identity = Some(Identity::New);
        preview.journey.device_name = "My computer".into();
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.harness().resize(viewport).unwrap();
    for effect in [
        InstallerPreviewEffect::SetPreviewPassword {
            value: "short".into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: "short".into(),
        },
    ] {
        app.effect(&effect).unwrap();
    }
    let refused = app
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap_err();
    let expected = t("installer-preview-password-short");
    assert!(
        matches!(refused, tilo_actions::DoorError::Failed(ref message) if message == &expected),
        "the semantic caller must receive the original validation refusal: {refused}"
    );
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Password);
        assert_eq!(preview.error, Some("installer-preview-password-short"));
        inspect_password_validation(
            preview,
            "installer.password",
            "installer-preview-password-short",
        );
    })
    .unwrap();
    paint_app(
        &mut app,
        "installer-semantic-password-error",
        &["installer.back", "installer.next"],
        viewport,
        "dark",
    );
    let observed = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(observed["page"], "password");
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview.error,
            Some("installer-preview-password-short"),
            "observing state must preserve validation feedback"
        );
        inspect_password_validation(
            preview,
            "installer.password",
            "installer-preview-password-short",
        );
    })
    .unwrap();
    for effect in [
        InstallerPreviewEffect::SetPreviewPassword {
            value: "several unrelated installation words".into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: "several unrelated installation words".into(),
        },
        InstallerPreviewEffect::ContinuePreview,
    ] {
        app.effect(&effect).unwrap();
    }
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Devices);
        assert!(
            preview.error.is_none(),
            "successful correction clears the error"
        );
        assert!(control(preview, "installer.error").is_none());
        assert!(validation_copy(preview, "installer-preview-password-short").is_none());
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn password_mismatch_marks_confirmation_and_uses_shared_field_feedback() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey = Journey::new("Ada Lovelace".into());
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.identity = Some(Identity::New);
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.harness().resize((1180, 820)).unwrap();
    for effect in [
        InstallerPreviewEffect::SetPreviewPassword {
            value: "several unrelated installation words".into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: "different unrelated installation words".into(),
        },
    ] {
        app.effect(&effect).unwrap();
    }
    assert!(
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .is_err()
    );
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.error, Some("installer-preview-password-mismatch"));
        inspect_password_validation(
            preview,
            "installer.confirmation",
            "installer-preview-password-mismatch",
        );
        assert!(
            !control(preview, "installer.password")
                .unwrap()
                .accessibility()
                .unwrap()
                .state
                .invalid,
            "a valid password is not blamed for confirmation mismatch"
        );
    })
    .unwrap();
    paint_app(
        &mut app,
        "installer-password-mismatch",
        &[
            "installer.password",
            "installer.confirmation",
            "installer.next",
        ],
        (1180, 820),
        "dark",
    );
}

#[test]
#[cfg(unix)]
fn password_policy_feedback_explains_the_actual_requirement_through_public_effects() {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    let viewport = (1180, 820);
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
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.harness().resize(viewport).unwrap();
    for (sample, error) in [
        ("short", "installer-preview-password-short"),
        (
            " several unrelated installation words",
            "installer-preview-password-spaces",
        ),
        (
            "several unrelated installation words ",
            "installer-preview-password-spaces",
        ),
        ("password123456789", "installer-preview-password-common"),
        ("1234567890123456", "installer-preview-password-numeric"),
        ("aaaaaaaaaaaaaaa", "installer-preview-password-repetitive"),
        ("abcdabcdabcdabcd", "installer-preview-password-repetitive"),
    ] {
        for effect in [
            InstallerPreviewEffect::SetPreviewPassword {
                value: sample.into(),
            },
            InstallerPreviewEffect::SetPreviewConfirmation {
                value: sample.into(),
            },
        ] {
            app.effect(&effect).unwrap();
        }
        let refused = app
            .effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap_err();
        let expected = t(error);
        assert!(
            matches!(refused, tilo_actions::DoorError::Failed(ref message) if message == &expected),
            "the semantic refusal must explain the actual policy failure"
        );
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Password);
            assert_eq!(preview.error, Some(error));
            inspect_password_validation(preview, "installer.password", error);
            assert!(
                !control(preview, "installer.confirmation")
                    .unwrap()
                    .accessibility()
                    .unwrap()
                    .state
                    .invalid,
                "policy failures do not blame a matching confirmation"
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ReadPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| assert_eq!(preview.error, Some(error)))
            .unwrap();
        paint_app(
            &mut app,
            error,
            &[
                "installer.password",
                "installer.confirmation",
                "installer.next",
            ],
            viewport,
            "dark",
        );
    }
    for valid in ["10394857261039485726", "123456789012345a"] {
        for effect in [
            InstallerPreviewEffect::SetPreviewPassword {
                value: valid.into(),
            },
            InstallerPreviewEffect::SetPreviewConfirmation {
                value: valid.into(),
            },
            InstallerPreviewEffect::ContinuePreview,
        ] {
            app.effect(&effect).unwrap();
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.journey.page,
                Page::Devices,
                "both advertised numeric corrections satisfy the unchanged policy"
            );
            assert!(preview.error.is_none());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn compact_password_submission_reveals_invalid_field_and_correction_without_manual_scroll() {
    // A short window at 200% genuinely clips either correction field, while
    // retaining enough body height for the control and its concise caption.
    password_validation_scenario((480, 520), true);
}

#[cfg(unix)]
#[test]
fn compact_password_submission_keeps_feedback_readable_at_200_percent() {
    // The 800px window need not clip the short-password input. Keep this
    // everyday compact size covered without claiming it proves offscreen reveal.
    for viewport in [(480, 800), (320, 800)] {
        password_validation_scenario(viewport, false);
    }
}

#[cfg(unix)]
fn password_validation_scenario(viewport: (u32, u32), require_clipped: bool) {
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for semantic in [false, true] {
        for mismatch in [false, true] {
            let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.live = false;
                preview.capture = false;
                preview.restart = restart_controller::Restart::default();
                preview.journey = Journey::new("Ada Lovelace".into());
                preview.journey.destination = Some(Destination::Computer);
                preview.journey.identity = Some(Identity::New);
                preview.journey.page = Page::Password;
                preview.dirty = true;
            })
            .unwrap();
            app.harness()
                .set_theme(Theme::dark().with_ui_scale(2.0).with_reduce_motion(true));
            app.harness().resize(viewport).unwrap();
            let password = if mismatch {
                "several unrelated installation words"
            } else {
                "short"
            };
            for effect in [
                InstallerPreviewEffect::SetPreviewPassword {
                    value: password.into(),
                },
                InstallerPreviewEffect::SetPreviewConfirmation {
                    value: "different unrelated installation words".into(),
                },
            ] {
                app.effect(&effect).unwrap();
            }
            app.settle();
            let (field, error, other) = if mismatch {
                (
                    "installer.confirmation",
                    "installer-preview-password-mismatch",
                    "installer.password",
                )
            } else {
                (
                    "installer.password",
                    "installer-preview-password-short",
                    "installer.confirmation",
                )
            };
            let (body, next) = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    let (body, overflowing) = body_scroll_geometry(preview).unwrap();
                    assert!(
                        overflowing,
                        "compact 200% password form must exercise scrolling"
                    );
                    inspect_controls(preview, &["installer.back", "installer.next"], viewport);
                    (body, control(preview, "installer.next").unwrap().get_rect())
                })
                .unwrap();
            app.harness().click_at(body.center().x, body.center().y);
            for _ in 0..40 {
                app.harness()
                    .scroll(0.0, if mismatch { 10.0 } else { -10.0 });
            }
            app.settle();
            if require_clipped {
                app.with_widget::<InstallerPreview, _>(|preview| {
                    let input = control(preview, field).unwrap().get_rect();
                    assert!(
                        input.y0 < body.y0 || input.y1 > body.y1,
                        "the affected field must initially need reveal: {field}, {input:?}, {body:?}"
                    );
                })
                .unwrap();
            }
            if semantic {
                let refused = app
                    .effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap_err();
                assert!(
                    matches!(refused, tilo_actions::DoorError::Failed(ref message) if message == &t(error))
                );
            } else {
                // Continue remains visible but disabled. Enter in the actual
                // confirmation input still exercises submission and reveal.
                app.harness()
                    .perform_named("installer.confirmation", &ui::a11y::Action::Focus)
                    .unwrap();
                app.harness().key("Enter");
            }
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.page, Page::Password);
                inspect_password_validation(preview, field, error);
                let input = control(preview, field).unwrap();
                assert!(input.is_focused(), "refused submission focuses the field to correct");
                let input = input.get_rect();
                let correction = validation_copy(preview, error).unwrap().get_rect();
                assert!(input.y0 >= body.y0 && correction.y1 <= body.y1,
                    "input and actionable error must be revealed together without manual scrolling: {input:?}, {correction:?}, {body:?}");
                assert_eq!(control(preview, "installer.next").unwrap().get_rect(), next);
                assert!(preview.validation_focus.is_none(), "submission focus request is consumed");
            }).unwrap();
            paint_app(
                &mut app,
                &format!(
                    "installer-password-refusal-{}-{}",
                    if semantic { "semantic" } else { "keyboard" },
                    if mismatch { "mismatch" } else { "short" }
                ),
                &["installer.back", "installer.next"],
                viewport,
                "dark-200",
            );

            // Move from the rejected input to its neighbour through the real
            // keyboard path. Focus navigation must reveal an offscreen input;
            // clicking its old, clipped coordinates is not a reachable action.
            if mismatch {
                app.harness().set_modifiers(&["shift".into()]);
            }
            app.harness().key("Tab");
            app.harness().set_modifiers(&[]);
            app.settle();
            let (chosen, focused) = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    (
                        control(preview, other).unwrap().get_rect(),
                        page::focused_id(preview),
                    )
                })
                .unwrap();
            assert!(
                chosen.y0 >= body.y0 && chosen.y1 <= body.y1,
                "chosen field must fit before the actual click: viewport={viewport:?}, semantic={semantic}, mismatch={mismatch}, other={other}, chosen={chosen:?}, body={body:?}, error={error}, focused={focused:?}"
            );
            app.harness().click_at(chosen.center().x, chosen.center().y);
            app.settle();
            // Blurring a field can add live validation and reflow the form.
            // Read-only observation, unlike validation, must not change geometry.
            let chosen = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    control(preview, other).unwrap().get_rect()
                })
                .unwrap();
            app.effect(&InstallerPreviewEffect::ReadPreview).unwrap();
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(control(preview, other).unwrap().is_focused());
                assert_eq!(control(preview, other).unwrap().get_rect(), chosen);
                assert_eq!(preview.error, Some(error));
                assert!(preview.validation_focus.is_none());
            })
            .unwrap();
            app.harness().type_text("z");
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(control(preview, other).unwrap().is_focused());
                let value = if mismatch {
                    &preview.journey.password
                } else {
                    &preview.journey.confirmation
                };
                assert!(
                    value.contains('z'),
                    "actual keyboard typing reaches the person's chosen masked field"
                );
                inspect_password_validation(preview, field, error);
                assert!(
                    !preview.password_continue_enabled(),
                    "an unrelated edit does not correct the rejected field"
                );
                assert!(preview.validation_focus.is_none());
                assert!(
                    control(preview, other)
                        .unwrap()
                        .accessibility()
                        .unwrap()
                        .value
                        .is_none()
                );
            })
            .unwrap();

            // Correct both inputs with real keyboard selection and typing.
            // Separate frames make caret resets during validation observable.
            let valid = "walnut telescope amber meadow";
            for id in ["installer.password", "installer.confirmation"] {
                app.harness()
                    .perform_named(id, &ui::a11y::Action::Focus)
                    .unwrap();
                app.harness().key("Home");
                app.harness().set_modifiers(&["shift".into()]);
                app.harness().key("End");
                app.harness().set_modifiers(&[]);
                app.settle();
                let selection = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        ui::undo::get_state(control(preview, id).unwrap()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    selection.split(':').count(),
                    3,
                    "replacement has a selection"
                );
                app.effect(&InstallerPreviewEffect::ReadPreview).unwrap();
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(
                        ui::undo::get_state(control(preview, id).unwrap()).unwrap(),
                        selection,
                        "observation preserves the private selection"
                    );
                })
                .unwrap();
                app.harness().type_text("walnut ");
                app.settle();
                app.harness().type_text("telescope amber meadow");
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    let input = control(preview, id).unwrap();
                    assert!(input.is_focused(), "correction retains keyboard focus");
                    assert_eq!(
                        if id == "installer.password" {
                            &preview.journey.password
                        } else {
                            &preview.journey.confirmation
                        },
                        valid,
                        "selection replacement and caret survive validation reflow"
                    );
                    assert!(!input.accessibility().unwrap().state.invalid);
                    let (body, _) = body_scroll_geometry(preview).unwrap();
                    let input = control(preview, id).unwrap().get_rect();
                    assert!(
                        input.y0 >= body.y0 && input.y1 <= body.y1,
                        "the corrected field stays visible: {input:?}, {body:?}"
                    );
                    if id == "installer.password" {
                        inspect_password_validation(
                            preview,
                            "installer.confirmation",
                            "installer-preview-password-mismatch",
                        );
                        assert!(!preview.password_continue_enabled());
                    } else {
                        assert!(preview.password_continue_enabled());
                        assert!(preview.password_validation().is_none());
                        assert!(preview.error.is_none());
                        assert!(validation_copy(preview, error).is_none());
                    }
                })
                .unwrap();
            }
            app.harness().key("Enter");
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.page, Page::Devices);
                assert!(preview.error.is_none());
            })
            .unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn personal_fields_enter_advances_and_password_enter_focuses_confirmation() {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.migrate = Some(false);
        preview.journey.identity = Some(Identity::New);
        preview.journey.page = Page::Profile;
        preview.journey.name = "Keyboard User".into();
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.settle();
    app.harness()
        .perform_named("installer.name", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("Enter");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview.journey.page,
            Page::Devices,
            "the interactive profile advances to Devices"
        );
        // An older preparation can still restore the retained unlock page.
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    app.settle();
    app.harness()
        .perform_named("installer.password", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness()
        .type_text("keyboard walnut telescope amber meadow");
    app.harness().key("Enter");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Password);
        assert!(
            preview.error.is_none(),
            "moving to confirmation is not a failed submission"
        );
        assert!(
            control(preview, "installer.confirmation")
                .unwrap()
                .is_focused()
        );
        assert!(
            control(preview, "installer.password")
                .unwrap()
                .accessibility()
                .unwrap()
                .state
                .protected
        );
    })
    .unwrap();
    app.harness()
        .type_text("keyboard walnut telescope amber meadow");
    // Deliver both presses to the actual focused input before the next frame.
    // A queued duplicate must not continue past Devices into Belongings.
    let enter = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter);
    let event = winit::event::WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: winit::event::KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: winit::event::ElementState::Pressed,
            },
            enter.clone(),
            None,
            winit::keyboard::KeyLocation::Standard,
            false,
            None,
            enter,
        ),
        is_synthetic: false,
    };
    assert!(app.harness().dispatch_to_root(&event));
    assert!(app.harness().dispatch_to_root(&event));
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Devices);
        assert_eq!(preview.journey.migrate, Some(false));
        assert_eq!(preview.journey.name, "Keyboard User");
        assert!(
            preview.slide.is_none(),
            "reduced-motion navigation remains immediate"
        );
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn account_one_field_enter_advances_without_starting_a_connection() {
    let mut app = account_motion_fixture();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    account_motion_step(&mut app, "add-account", None);
    account_motion_step(&mut app, "choose-provider", Some("webdav"));
    app.harness()
        .perform_named("installer.account.dav.server", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().type_text("https://dav.example.test");
    app.harness().key("Enter");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Accounts);
        assert_eq!(preview.accounts.navigation_step(), "dav-user");
        assert!(!preview.accounts.active());
    })
    .unwrap();
    app.harness()
        .perform_named("installer.account.dav.login", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().type_text("Keyboard Ada");
    app.harness().key("Enter");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.accounts.navigation_step(), "dav-password");
        assert!(
            !preview.accounts.active(),
            "field navigation does not authorize a connection"
        );
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn profile_keyboard_name_updates_retained_avatar_and_preserves_mid_string_caret() {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.restart = restart_controller::Restart::default();
        preview.journey.page = Page::Profile;
        preview.journey.name = "Ada Lovelace".into();
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.settle();
    let inspect = |preview: &InstallerPreview, expected_name: &str, expected_initials: &str| {
        let picture = control(preview, "installer.profile.avatar").unwrap();
        let children = picture.children();
        let avatar = children[0]
            .as_any()
            .unwrap()
            .downcast_ref::<Avatar>()
            .unwrap();
        assert_eq!(
            avatar.initials(),
            expected_initials,
            "inspect the actual rendered avatar"
        );
        assert_eq!(
            avatar.accessibility().unwrap().name.as_deref(),
            Some(expected_name)
        );
        std::ptr::from_ref(avatar) as usize
    };
    let inspect_input = |input: &dyn Widget| {
        let state = ui::undo::get_state(input).expect("actual input caret/selection state");
        let parts: Vec<_> = state.split(':').collect();
        assert!(
            (1..=3).contains(&parts.len()),
            "unexpected input state: {state}"
        );
        let caret = parts[0].parse::<usize>().unwrap();
        if let Some(affinity) = parts.get(1) {
            assert!(matches!(*affinity, "a" | "b"), "invalid affinity: {state}");
        }
        let anchor = parts.get(2).map(|anchor| anchor.parse::<usize>().unwrap());
        (caret, anchor)
    };
    let avatar_address = app
        .with_widget::<InstallerPreview, _>(|preview| inspect(preview, "Ada Lovelace", "AL"))
        .unwrap();
    app.harness()
        .perform_named("installer.name", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("Home");
    app.harness().type_text("Grace ");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.name, "Grace Ada Lovelace");
        assert_eq!(
            inspect(preview, "Grace Ada Lovelace", "GL"),
            avatar_address,
            "typing updates the retained avatar rather than replacing the page"
        );
        let input = control(preview, "installer.name").unwrap();
        assert!(input.is_focused());
        assert_eq!(
            inspect_input(input),
            (6, None),
            "successive frames preserve the insertion caret without a selection"
        );
    })
    .unwrap();
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("End");
    app.harness().set_modifiers(&[]);
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        let input = control(preview, "installer.name").unwrap();
        assert!(input.is_focused());
        assert_eq!(inspect_input(input), (18, Some(6)));
        assert_eq!(inspect(preview, "Grace Ada Lovelace", "GL"), avatar_address);
    })
    .unwrap();
    app.harness().type_text("Hopper");
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.name, "Grace Hopper");
        assert_eq!(inspect(preview, "Grace Hopper", "GH"), avatar_address);
        let input = control(preview, "installer.name").unwrap();
        assert!(input.is_focused());
        assert_eq!(inspect_input(input), (12, None));
        assert_eq!(preview.journey.page, Page::Profile);
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn personal_form_labels_and_navigation_share_the_real_page_alignment() {
    fn field_label<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if root.name() == "Field" && control(root, id).is_some() {
            return root
                .children()
                .into_iter()
                .find(|child| child.name() == "FieldLabel");
        }
        root.children()
            .into_iter()
            .find_map(|child| field_label(child, id))
    }
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        for (viewport, scale) in [((320, 720), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
            let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.live = false;
                preview.capture = true;
                preview.restart = restart_controller::Restart::default();
                preview.journey = Journey::new("Ada Lovelace".into());
                preview.journey.destination = Some(Destination::Computer);
                preview.journey.identity = Some(Identity::New);
                preview.journey.page = Page::Password;
                preview.dirty = true;
            })
            .unwrap();
            app.harness()
                .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
            app.harness().resize(viewport).unwrap();
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                let password = control(preview, "installer.password").unwrap().get_rect();
                let confirmation = control(preview, "installer.confirmation")
                    .unwrap()
                    .get_rect();
                let heading = control(preview, "installer.heading").unwrap().get_rect();
                let back = control(preview, "installer.back").unwrap().get_rect();
                let next = control(preview, "installer.next").unwrap().get_rect();
                assert!((password.x0 - confirmation.x0).abs() < 1.0);
                assert!((password.width() - confirmation.width()).abs() < 1.0);
                assert!(
                    (heading.x0 - password.x0).abs() < 1.0,
                    "heading and above-field labels use the form's leading edge"
                );
                for (id, input) in [
                    ("installer.password", password),
                    ("installer.confirmation", confirmation),
                ] {
                    let label = field_label(preview, id)
                        .expect("actual shared FieldLabel")
                        .get_rect();
                    assert!(
                        (label.x0 - input.x0).abs() < 1.0,
                        "{id}: label={label:?}, input={input:?}"
                    );
                    assert!(label.y1 <= input.y0, "label belongs above its control");
                }
                assert!(confirmation.y0 >= password.y1);
                assert!((password.center().x - preview.rect.center().x).abs() < 1.0);
                assert!(
                    (f64::midpoint(back.x0, next.x1) - password.center().x).abs() < 1.0,
                    "navigation is centered on the same page axis"
                );
                let (scroll, _) = body_scroll_geometry(preview).unwrap();
                assert!(
                    scroll.y1 <= next.y0,
                    "the body cannot cover the pinned navigation"
                );
                assert!(next.y1 <= preview.rect.y1);
                assert!(password.width() >= 180.0, "usable compact form");
            })
            .unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn password_route_keeps_disabled_continue_and_real_back_in_every_frame() {
    fn has_guidance(root: &dyn Widget) -> bool {
        root.accessibility().is_some_and(|node| {
            node.name.as_deref() == Some(t("installer-preview-password-hint").as_str())
        }) || root.children().into_iter().any(has_guidance)
    }
    let custody = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", custody.path().join("state"));
    environment.set("XDG_CONFIG_HOME", custody.path().join("config"));
    for (viewport, theme, tag) in [
        ((480, 800), Theme::light(), "compact-light"),
        ((1180, 820), Theme::dark(), "dark"),
    ] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            // Suppress native work, not navigation: reach Password through
            // the production effects so Back has the real route history.
            preview.live = false;
            preview.capture = false;
            preview.linux = true;
            preview.restart = restart_controller::Restart::default();
            preview.journey = Journey::new("Ada Lovelace".into());
            preview.dirty = true;
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        for choice in ["computer", "fresh", "new"] {
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: choice.into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap();
        }
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Password;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let footer = app
            .with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.page, Page::Password);
                assert_eq!(preview.journey.history.last(), Some(&Page::Profile));
                assert!(
                    has_guidance(preview),
                    "inline password guidance must be visible"
                );
                (
                    control(preview, "installer.back").unwrap().get_rect(),
                    control(preview, "installer.next").unwrap().get_rect(),
                )
            })
            .unwrap();
        for (state, password, confirmation, disabled) in [
            ("empty", "", "", true),
            (
                "valid",
                "several unrelated installation words",
                "several unrelated installation words",
                false,
            ),
            (
                "mismatch",
                "several unrelated installation words",
                "different unrelated installation words",
                true,
            ),
            ("short", "short", "short", true),
        ] {
            app.effect(&InstallerPreviewEffect::SetPreviewPassword {
                value: password.into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
                value: confirmation.into(),
            })
            .unwrap();
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                inspect_controls(preview, &["installer.back", "installer.next"], viewport);
                let next = control(preview, "installer.next").unwrap();
                let back = control(preview, "installer.back").unwrap();
                assert_eq!(next.accessibility().unwrap().state.disabled, disabled);
                assert!(!back.accessibility().unwrap().state.disabled);
                assert_eq!((back.get_rect(), next.get_rect()), footer);
                let next_rect = next.get_rect();
                let (body, _) = body_scroll_geometry(preview).unwrap();
                assert!(body.y1 <= next_rect.y0);
            })
            .unwrap();
            if disabled {
                app.harness()
                    .click_at(footer.1.center().x, footer.1.center().y);
                app.settle();
                assert!(
                    app.effect(&InstallerPreviewEffect::ContinuePreview)
                        .is_err()
                );
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(preview.journey.page, Page::Password);
                })
                .unwrap();
            }
            let status = app
                .effect(&InstallerPreviewEffect::ReadPreview)
                .unwrap()
                .value
                .unwrap();
            assert_eq!(status["page"], "password");
            if !password.is_empty() {
                assert!(
                    !status.to_string().contains(password),
                    "private input cannot enter public status"
                );
            }
            paint_app(
                &mut app,
                &format!("installer-password-route-{state}"),
                &[
                    "installer.password",
                    "installer.confirmation",
                    "installer.back",
                    "installer.next",
                ],
                viewport,
                tag,
            );
        }
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Profile);
        })
        .unwrap();
    }
}

#[test]
fn guided_scenario_gallery_renders_and_closes_every_actual_component() {
    use ui::framework::ResourceApp as _;
    assert_eq!(GuidedInstallerShell::scenarios(), SCENARIOS);
    for scenario in GuidedInstallerShell::scenarios() {
        let mut app =
            TestApp::<GuidedInstallerShell>::capture_at(Some(scenario), (1180, 820)).unwrap();
        app.harness()
            .set_theme(Theme::dark().with_reduce_motion(true));
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.name(), "InstallerPreview");
            if preview.journey.page != Page::Destination && preview.journey.page != Page::Ready {
                inspect_controls(preview, &["installer.back"], (1180, 820));
            }
            if let Some(next) = control(preview, "installer.next") {
                let rect = next.get_rect();
                if preview.journey.page == Page::Password {
                    assert!(next.accessibility().unwrap().state.disabled);
                }
                inspect_controls(preview, &["installer.next"], (1180, 820));
                let (body, _) = body_scroll_geometry(preview).unwrap();
                assert!(
                    body.y1 <= rect.y0,
                    "{scenario}: body cannot cover the footer"
                );
            }
        })
        .expect("the guided gallery must use the actual guided product");
        assert_eq!(app.harness().render_rgba().len(), 1180 * 820 * 4);
        app.harness().close_components();
        let reports = app.harness().component_close_reports();
        assert!(
            !reports.is_empty(),
            "{scenario}: actual close must be observed"
        );
        for report in reports {
            assert!(
                report.view_error.is_none(),
                "{scenario}: view close: {:?}",
                report.view_error
            );
            assert!(
                report.record_error.is_none(),
                "{scenario}: record close: {:?}",
                report.record_error
            );
            assert!(
                report.teardown_error.is_none(),
                "{scenario}: teardown: {:?}",
                report.teardown_error
            );
        }
    }
}

#[test]
fn image_writer_progress_capture_closes_through_the_canonical_lifecycle() {
    let mut app =
        TestApp::<crate::resource::ImagerShell>::capture_at(Some("progress"), (1180, 820)).unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.settle();
    assert_eq!(app.harness().render_rgba().len(), 1180 * 820 * 4);
    app.harness().close_components();
    let reports = app.harness().component_close_reports();
    assert!(
        !reports.is_empty(),
        "the actual writer capture must produce a close report"
    );
    for report in reports {
        assert!(
            report.view_error.is_none(),
            "writer progress view: {:?}",
            report.view_error
        );
        assert!(
            report.record_error.is_none(),
            "writer progress record: {:?}",
            report.record_error
        );
        assert!(
            report.teardown_error.is_none(),
            "writer progress teardown: {:?}",
            report.teardown_error
        );
    }
}
