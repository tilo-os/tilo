use super::*;
use crate::hosting::testing::TestHost;
use std::path::PathBuf;

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

pub(super) fn paint_app(
    app: &mut TestHost,
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
#[cfg(unix)]
fn destination_review_edit_exposes_cancel_and_restores_choices_through_public_actions() {
    for (viewport, theme) in [((480, 800), Theme::dark()), ((1180, 820), Theme::light())] {
        let mut app = TestHost::launched(&["--setup"]);
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

fn validation_copy<'a>(root: &'a dyn Widget, error: &str) -> Option<&'a dyn Widget> {
    if root.accessibility().is_some_and(|node| {
        node.name.as_deref() == Some(t(error).as_str())
            && node.live == Some(tilo_ui_base::a11y::Politeness::Assertive)
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
        .and_then(|widget| widget.downcast_mut::<ui::widget::Scrollable>())
    {
        return Some((scroll.content_viewport_rect(), scroll.vertical_overflows()));
    }
    root.children_mut()
        .into_iter()
        .find_map(body_scroll_geometry)
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
        let mut app = TestHost::launched(&["--setup"]);
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
        let mut app = TestHost::launched(&["--setup"]);
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
        let mut app = TestHost::launched(&["--setup"]);
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
    let mut app = TestHost::launched(&["--setup"]);
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
    let mut app = TestHost::launched(&["--setup"]);
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
    let mut app = TestHost::launched(&["--setup"]);
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
            let mut app = TestHost::launched(&["--setup"]);
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
                    .perform_named("installer.confirmation", &tilo_ui_base::a11y::Action::Focus)
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
                    .perform_named(id, &tilo_ui_base::a11y::Action::Focus)
                    .unwrap();
                app.harness().key("Home");
                app.harness().set_modifiers(&["shift".into()]);
                app.harness().key("End");
                app.harness().set_modifiers(&[]);
                app.settle();
                let selection = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        tilo_ui_base::widget::undo_points::get_state(control(preview, id).unwrap())
                            .unwrap()
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
                        tilo_ui_base::widget::undo_points::get_state(control(preview, id).unwrap())
                            .unwrap(),
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
    let mut app = TestHost::launched(&["--setup"]);
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
        .perform_named("installer.password", &tilo_ui_base::a11y::Action::Focus)
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
fn guided_scenario_gallery_renders_and_closes_every_actual_component() {
    let scenarios = crate::hosting::scenarios();
    assert!(
        SCENARIOS
            .iter()
            .all(|scenario| scenarios.contains(scenario))
    );
    for scenario in SCENARIOS {
        let mut app = TestHost::capture_at(Some(scenario), (1180, 820));
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
        for report in app.harness().component_close_reports() {
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
    let mut app = TestHost::capture_at(Some("progress"), (1180, 820));
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.settle();
    assert_eq!(app.harness().render_rgba().len(), 1180 * 820 * 4);
    app.harness().close_components();
    let reports = app.harness().component_close_reports();
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
