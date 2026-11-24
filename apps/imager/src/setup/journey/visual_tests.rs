use super::*;
use crate::resource::ImagerShell;
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
    if preview.journey.page == Page::Password {
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
    let mut app = TestApp::<ImagerShell>::capture_at(Some(scenario), viewport).unwrap();
    app.harness().set_theme(theme.with_reduce_motion(true));
    paint_app(&mut app, scenario, ids, viewport, tag);
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

#[test]
fn viewport_growth_keeps_logical_type_and_controls_and_pointer_mapping() {
    fn heading_font(root: &mut dyn Widget, theme: &Theme) -> Option<f32> {
        if root.test_id() == Some("installer.heading") {
            let mut scene = Scene::new();
            root.draw_child(
                &mut scene,
                Affine::IDENTITY,
                &mut ui::TextRenderer::new(),
                theme,&PresenceCtx::default().draw_env(),
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
        let mut unscaled: Option<(f32, f64, f64)> = None;
        for host_scale in [1.0, 1.5, 2.0] {
            let initial = match host_scale {
                1.0 => (1180, 820),
                1.5 => (1770, 1230),
                _ => (2360, 1640),
            };
            let mut app =
                TestApp::<ImagerShell>::capture_at(Some("installer-migrate"), initial).unwrap();
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
                    "installer-migrate",
                    &["installer.ticket", "installer.next"],
                    viewport,
                    &format!("logical-size-{host_scale}"),
                );
                let dimensions = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        let heading_size =
                            heading_font(preview, &theme.clone().with_ui_scale(host_scale))
                                .unwrap();
                        let name = control(preview, "installer.ticket").unwrap().get_rect();
                        let next = control(preview, "installer.next").unwrap().get_rect();
                        let metrics = &theme.tokens.metrics;
                        assert!(
                            (name.width() - f64::from(metrics.content_min_width * host_scale))
                                .abs()
                                < 1.0
                        );
                        assert!((name.center().x - preview.rect.center().x).abs() < 1.0);
                        (heading_size, name.height(), next.height())
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
                if let Some((font, field, button)) = unscaled {
                    assert_eq!(dimensions.0, font * host_scale);
                    for (actual, base) in [(dimensions.1, field), (dimensions.2, button)] {
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
                    assert_eq!(preview.journey.page, Page::Migrate);
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
        let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = false;
            preview.capture = true;
            preview.journey = Journey::new();
            preview.journey.destination = Some(Destination::Computer);
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
        for field in ["destination", "device_name"] {
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
fn guided_pages_paint_at_small_normal_and_large_host_scales_in_both_themes() {
    let screens: &[(&str, &[&str])] = &[
        (
            "installer-destination",
            &["installer.choice.computer", "installer.choice.another"],
        ),
        (
            "installer-migrate",
            &["installer.ticket", "installer.next"],
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
fn resizing_or_changing_host_scale_discards_old_transition_geometry() {
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.history = vec![Page::Destination];
        preview.journey.page = Page::Migrate;
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
#[cfg(unix)]
fn destination_discloses_usb_route_before_the_install_questions() {
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    for runtime_check in ["0", "1"] {
        environment.set(
            super::super::internal_install_flow::RUNTIME_ALLOW_VAR,
            runtime_check,
        );
        let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.capture = false;
            preview.journey = Journey::new();
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            let name = control(preview, "installer.choice.computer")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .unwrap();
            let expected = if super::super::internal_install_flow::BUILD_ALLOW == Some("1")
                || runtime_check == "1"
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

#[cfg(target_os = "linux")]
#[test]
#[cfg(unix)]
fn unavailable_guided_host_paints_honest_state_and_refuses_semantic_navigation() {
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.capture = false;
            preview.capabilities.prepared_installation = false;
            preview.journey = Journey::new();
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
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    environment.set(super::super::internal_install_flow::RUNTIME_ALLOW_VAR, "1");
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.capture = false;
            preview.journey = Journey::new();
            preview.journey.destination = Some(Destination::Computer);
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
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    let viewport = (1180, 820);
    let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
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
        matches!(refused, tilo_actions::ApiError::Failed(ref message) if message == &expected),
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
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
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
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    let viewport = (1180, 820);
    let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.capture = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
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
            matches!(refused, tilo_actions::ApiError::Failed(ref message) if message == &expected),
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
    password_validation_scenario((480, 520), true);
}

#[cfg(unix)]
#[test]
fn compact_password_submission_keeps_feedback_readable_at_200_percent() {
    for viewport in [(480, 800), (320, 800)] {
        password_validation_scenario(viewport, false);
    }
}

#[cfg(unix)]
fn password_validation_scenario(viewport: (u32, u32), require_clipped: bool) {
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    for semantic in [false, true] {
        for mismatch in [false, true] {
            let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.live = false;
                preview.capture = false;
                preview.journey = Journey::new();
                preview.journey.destination = Some(Destination::Computer);
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
                    matches!(refused, tilo_actions::ApiError::Failed(ref message) if message == &t(error))
                );
            } else {
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
fn password_enter_focuses_confirmation_and_then_submits() {
    let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.history = vec![Page::Destination, Page::Migrate];
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
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
        assert!(
            preview.slide.is_none(),
            "reduced-motion navigation remains immediate"
        );
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn password_form_labels_and_navigation_share_the_real_page_alignment() {
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
            let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.live = false;
                preview.capture = true;
                preview.journey = Journey::new();
                preview.journey.destination = Some(Destination::Computer);
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
    let ownership = crate::setup::test_support::qualified_tempdir();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", ownership.path().join("state"));
    environment.set("XDG_CONFIG_HOME", ownership.path().join("config"));
    for (viewport, theme, tag) in [
        ((480, 800), Theme::light(), "compact-light"),
        ((1180, 820), Theme::dark(), "dark"),
    ] {
        let mut app = TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = false;
            preview.capture = false;
            preview.linux = true;
            preview.journey = Journey::new();
            preview.dirty = true;
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "computer".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
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
                assert_eq!(preview.journey.history.last(), Some(&Page::Migrate));
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
            assert_eq!(preview.journey.page, Page::Migrate);
        })
        .unwrap();
    }
}

#[test]
fn guided_scenario_gallery_renders_and_closes_every_actual_component() {
    use ui::framework::ResourceApp as _;
    let scenarios = ImagerShell::scenarios();
    assert!(
        SCENARIOS
            .iter()
            .all(|scenario| scenarios.contains(scenario))
    );
    for scenario in SCENARIOS {
        let mut app = TestApp::<ImagerShell>::capture_at(Some(scenario), (1180, 820)).unwrap();
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
                report.resource_error.is_none(),
                "{scenario}: record close: {:?}",
                report.resource_error
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
            report.resource_error.is_none(),
            "writer progress record: {:?}",
            report.resource_error
        );
        assert!(
            report.teardown_error.is_none(),
            "writer progress teardown: {:?}",
            report.teardown_error
        );
    }
}
