//! The disk password page: what it checks as the person types, how a refusal reads, and that the
//! secret never leaves the masked fields.

use super::harness::{Walk, body_scroll_geometry, control, inspect_controls, paint_app};
use super::*;

const VIEWPORT: (u32, u32) = (1180, 820);
const VALID: &str = "several unrelated installation words";
const OTHER: &str = "different unrelated installation words";

fn password_page() -> Walk {
    Walk::at(Page::Password, VIEWPORT, Theme::dark())
}

/// Which of the two fields reads invalid, and whether Continue is offered; the secret stays masked
/// and no banner repeats a field's error.
fn check(app: &mut Walk, password: bool, confirmation: bool, enabled: bool) {
    app.view(|preview| {
        for (id, invalid) in [
            ("installer.password", password),
            ("installer.confirmation", confirmation),
        ] {
            let access = control(preview, id).unwrap().accessibility().unwrap();
            assert_eq!(access.state.invalid, invalid, "{id}");
            assert!(access.state.protected);
            assert!(access.value.is_none());
        }
        assert_eq!(preview.password_continue_enabled(), enabled);
        assert_eq!(preview.journey.page, Page::Password);
        assert!(
            control(preview, "installer.error").is_none(),
            "unexpected banner: {:?}; expected invalid={password}/{confirmation}",
            preview.error
        );
    });
}

fn typed(password: &str, confirmation: &str) -> [InstallerPreviewEffect; 2] {
    [
        InstallerPreviewEffect::SetPreviewPassword {
            value: password.into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: confirmation.into(),
        },
    ]
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

/// `field` is marked invalid, still masked, with `error` read out assertively right below it and
/// no banner repeating it.
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

fn invalid(preview: &InstallerPreview, id: &str) -> bool {
    control(preview, id)
        .unwrap()
        .accessibility()
        .unwrap()
        .state
        .invalid
}

/// Continue refused through the published action with `error`'s own words, the page still on the
/// password with that error under `field`.
fn refused_with(app: &mut Walk, field: &str, error: &str) {
    let refused = app
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap_err();
    let expected = t(error);
    assert!(
        matches!(refused, tilo_actions::ApiError::Failed(ref message) if message == &expected),
        "the semantic caller must receive the original validation refusal: {refused}"
    );
    app.settle();
    app.view(|preview| {
        assert_eq!(preview.journey.page, Page::Password);
        assert_eq!(preview.error, Some(error));
        inspect_password_validation(preview, field, error);
    });
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn the_password_page_checks_a_field_when_the_person_leaves_it;

    phase tab_blur_validates_short_password_and_corrections_update_both_fields;
    {
        let mut app = password_page();
        check(&mut app, false, false, false);
        app.focus("installer.password");
        app.harness().type_text("eightchr");
        app.settle();
        check(&mut app, false, false, false);
        app.harness().key("Tab");
        app.settle();
        check(&mut app, true, false, false);
        app.harness().type_text("different");
        app.settle();
        check(&mut app, true, true, false);
        // Semantic setters follow the same private edit path, with no public value setter.
        let [password, confirmation] = typed("walnut telescope amber meadow", "walnut telescope amber meadow");
        app.act([password]);
        check(&mut app, false, true, false);
        app.act([confirmation]);
        check(&mut app, false, false, true);
        app.act([InstallerPreviewEffect::SetPreviewPassword { value: "short".into() }]);
        check(&mut app, true, true, false);
    }

    phase pointer_blur_shows_primary_error_before_any_submission;
    {
        let mut app = password_page();
        app.focus("installer.password");
        app.harness().type_text("eightchr");
        app.settle();
        app.click("installer.confirmation");
        check(&mut app, true, false, false);
    }

    phase enter_and_pointer_blur_validate_without_submitting_or_exposing_secrets;
    {
        let mut app = password_page();
        app.focus("installer.password");
        app.harness().type_text("eightchr");
        app.harness().key("Enter");
        app.settle();
        check(&mut app, true, false, false);
        app.view(|preview| {
            assert!(control(preview, "installer.confirmation").unwrap().is_focused());
            assert!(preview.error.is_none());
        });
        app.harness().type_text("eightchr");
        app.settle();
        check(&mut app, true, false, false);
        app.click("installer.password");
        app.harness().type_text(" walnut meadow");
        app.settle();
        check(&mut app, false, true, false);
        app.focus("installer.confirmation");
        app.harness().key("End");
        app.harness().type_text(" walnut meadow");
        app.settle();
        check(&mut app, false, false, true);
        // Moving away from the corrected field must not resurrect stale errors.
        app.harness().key("Tab");
        app.settle();
        check(&mut app, false, false, true);
        // Edits in the middle survive validation-driven recomposition, including
        // the caret. If it jumps to the end these two corrections cannot match.
        app.focus("installer.password");
        app.harness().key("Home");
        app.harness().type_text("x");
        app.settle();
        app.harness().type_text("y");
        app.settle();
        check(&mut app, false, true, false);
        app.focus("installer.confirmation");
        app.harness().key("Home");
        app.harness().type_text("xy");
        app.settle();
        check(&mut app, false, false, true);
        assert!(!app.read().to_string().contains("eightchr"));
        let snapshot = app.harness().inspection_json().unwrap();
        assert!(!snapshot.contains("eightchr"));
        assert!(!snapshot.contains("walnut meadow"));
    }

    phase password_enter_focuses_confirmation_and_then_submits;
    {
        let mut app = password_page();
        app.edit(|preview| preview.journey.history = vec![Page::Destination, Page::Migrate]);
        app.settle();
        app.focus("installer.password");
        app.harness().type_text("keyboard walnut telescope amber meadow");
        app.harness().key("Enter");
        app.settle();
        app.view(|preview| {
            assert_eq!(preview.journey.page, Page::Password);
            assert!(
                preview.error.is_none(),
                "moving to confirmation is not a failed submission"
            );
            assert!(control(preview, "installer.confirmation").unwrap().is_focused());
            assert!(
                control(preview, "installer.password")
                    .unwrap()
                    .accessibility()
                    .unwrap()
                    .state
                    .protected
            );
        });
        app.harness().type_text("keyboard walnut telescope amber meadow");
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
        app.view(|preview| {
            assert_eq!(preview.journey.page, Page::Devices);
            assert!(
                preview.slide.is_none(),
                "reduced-motion navigation remains immediate"
            );
        });
    }
}

/// One walk through every refusal Continue gives through the published actions, each read back
/// as the field's own feedback, then the corrections that pass the unchanged policy.
#[test]
#[cfg(unix)]
fn a_refused_password_says_why_beside_the_field_and_a_correction_continues() {
    let mut app = password_page();
    app.act(typed("short", "short"));
    refused_with(
        &mut app,
        "installer.password",
        "installer-preview-password-short",
    );
    paint_app(
        &mut app,
        "installer-semantic-password-error",
        &["installer.back", "installer.next"],
        VIEWPORT,
        "dark",
    );
    assert_eq!(app.read()["page"], "password");
    app.view(|preview| {
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
    });

    app.act(typed(VALID, OTHER));
    refused_with(
        &mut app,
        "installer.confirmation",
        "installer-preview-password-mismatch",
    );
    app.view(|preview| {
        assert!(
            !invalid(preview, "installer.password"),
            "a valid password is not blamed for confirmation mismatch"
        );
    });
    paint_app(
        &mut app,
        "installer-password-mismatch",
        &[
            "installer.password",
            "installer.confirmation",
            "installer.next",
        ],
        VIEWPORT,
        "dark",
    );

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
        app.act(typed(sample, sample));
        refused_with(&mut app, "installer.password", error);
        app.view(|preview| {
            assert!(
                !invalid(preview, "installer.confirmation"),
                "policy failures do not blame a matching confirmation"
            );
        });
        app.read();
        app.view(|preview| assert_eq!(preview.error, Some(error)));
        paint_app(
            &mut app,
            error,
            &[
                "installer.password",
                "installer.confirmation",
                "installer.next",
            ],
            VIEWPORT,
            "dark",
        );
    }

    app.act(typed(VALID, VALID));
    app.act([InstallerPreviewEffect::ContinuePreview]);
    app.settle();
    app.view(|preview| {
        assert_eq!(preview.journey.page, Page::Devices);
        assert!(
            preview.error.is_none(),
            "successful correction clears the error"
        );
        assert!(control(preview, "installer.error").is_none());
        assert!(validation_copy(preview, "installer-preview-password-short").is_none());
    });
    app.act([InstallerPreviewEffect::BackInPreview]);
    for valid in ["10394857261039485726", "123456789012345a"] {
        app.act(typed(valid, valid));
        app.act([InstallerPreviewEffect::ContinuePreview]);
        app.view(|preview| {
            assert_eq!(
                preview.journey.page,
                Page::Devices,
                "both advertised numeric corrections satisfy the unchanged policy"
            );
            assert!(preview.error.is_none());
        });
        app.act([InstallerPreviewEffect::BackInPreview]);
    }
}

/// At 200% on a compact window a refused submission, by keyboard or through the published action,
/// scrolls the field to correct and its error into view together; the person then corrects it
/// in place and continues.
#[cfg(unix)]
#[test]
fn a_compact_refusal_reveals_the_field_and_its_correction_without_manual_scroll() {
    for (viewport, require_clipped) in
        [((480, 520), true), ((480, 800), false), ((320, 800), false)]
    {
        for semantic in [false, true] {
            for mismatch in [false, true] {
                compact_refusal(viewport, require_clipped, semantic, mismatch);
            }
        }
    }
}

#[cfg(unix)]
fn compact_refusal(viewport: (u32, u32), require_clipped: bool, semantic: bool, mismatch: bool) {
    let mut app = Walk::at(Page::Password, viewport, Theme::dark().with_ui_scale(2.0));
    app.act(typed(if mismatch { VALID } else { "short" }, OTHER));
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
    let (body, next) = app.view(|preview| {
        let (body, overflowing) = body_scroll_geometry(preview).unwrap();
        assert!(
            overflowing,
            "compact 200% password form must exercise scrolling"
        );
        inspect_controls(preview, &["installer.back", "installer.next"], viewport);
        (body, control(preview, "installer.next").unwrap().get_rect())
    });
    app.harness().click_at(body.center().x, body.center().y);
    for _ in 0..40 {
        app.harness()
            .scroll(0.0, if mismatch { 10.0 } else { -10.0 });
    }
    app.settle();
    if require_clipped {
        let input = app.rect(field);
        assert!(
            input.y0 < body.y0 || input.y1 > body.y1,
            "the affected field must initially need reveal: {field}, {input:?}, {body:?}"
        );
    }
    if semantic {
        let refused = app
            .effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap_err();
        assert!(
            matches!(refused, tilo_actions::ApiError::Failed(ref message) if message == &t(error))
        );
    } else {
        app.focus("installer.confirmation");
        app.harness().key("Enter");
    }
    app.settle();
    app.view(|preview| {
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
    });
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
    let (chosen, focused) = app.view(|preview| {
        (
            control(preview, other).unwrap().get_rect(),
            crate::layout::focused_id(preview),
        )
    });
    assert!(
        chosen.y0 >= body.y0 && chosen.y1 <= body.y1,
        "chosen field must fit before the actual click: viewport={viewport:?}, semantic={semantic}, mismatch={mismatch}, other={other}, chosen={chosen:?}, body={body:?}, error={error}, focused={focused:?}"
    );
    app.click(other);
    let chosen = app.rect(other);
    app.read();
    app.settle();
    app.view(|preview| {
        assert!(control(preview, other).unwrap().is_focused());
        assert_eq!(control(preview, other).unwrap().get_rect(), chosen);
        assert_eq!(preview.error, Some(error));
        assert!(preview.validation_focus.is_none());
    });
    app.harness().type_text("z");
    app.settle();
    app.view(|preview| {
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
    });

    let valid = "walnut telescope amber meadow";
    for id in ["installer.password", "installer.confirmation"] {
        app.focus(id);
        app.harness().key("Home");
        app.harness().set_modifiers(&["shift".into()]);
        app.harness().key("End");
        app.harness().set_modifiers(&[]);
        app.settle();
        let selection = app.view(|preview| {
            tilo_ui_base::widget::undo_points::get_state(control(preview, id).unwrap()).unwrap()
        });
        assert_eq!(
            selection.split(':').count(),
            3,
            "replacement has a selection"
        );
        app.read();
        app.settle();
        app.view(|preview| {
            assert_eq!(
                tilo_ui_base::widget::undo_points::get_state(control(preview, id).unwrap())
                    .unwrap(),
                selection,
                "observation preserves the private selection"
            );
        });
        app.harness().type_text("walnut ");
        app.settle();
        app.harness().type_text("telescope amber meadow");
        app.settle();
        app.view(|preview| {
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
        });
    }
    app.harness().key("Enter");
    app.settle();
    app.view(|preview| {
        assert_eq!(preview.journey.page, Page::Devices);
        assert!(preview.error.is_none());
    });
}

/// The labels, fields, heading and navigation share the page's axis at every width and theme.
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
            let mut app = Walk::at(Page::Password, viewport, theme.clone().with_ui_scale(scale));
            app.edit(|preview| preview.capture = true);
            app.settle();
            app.view(|preview| {
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
            });
        }
    }
}

/// Reached the way a person reaches it, the page keeps its guidance, a real Back and a Continue
/// disabled for every input it refuses, in every frame, and never publishes what was typed.
#[test]
#[cfg(unix)]
fn password_route_keeps_disabled_continue_and_real_back_in_every_frame() {
    fn has_guidance(root: &dyn Widget) -> bool {
        root.accessibility().is_some_and(|node| {
            node.name.as_deref() == Some(t("installer-preview-password-hint").as_str())
        }) || root.children().into_iter().any(has_guidance)
    }
    for (viewport, theme, tag) in [
        ((480, 800), Theme::light(), "compact-light"),
        ((1180, 820), Theme::dark(), "dark"),
    ] {
        let mut app = Walk::launched();
        app.edit(|preview| {
            preview.live = false;
            preview.capture = false;
            preview.linux = true;
            preview.journey = Journey::new();
        });
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.act([
            InstallerPreviewEffect::ChoosePreview {
                choice: "computer".into(),
            },
            InstallerPreviewEffect::ContinuePreview,
            InstallerPreviewEffect::ContinuePreview,
        ]);
        app.edit(|preview| preview.journey.page = Page::Password);
        app.settle();
        let footer = app.view(|preview| {
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
        });
        for (state, password, confirmation, disabled) in [
            ("empty", "", "", true),
            ("valid", VALID, VALID, false),
            ("mismatch", VALID, OTHER, true),
            ("short", "short", "short", true),
        ] {
            app.act(typed(password, confirmation));
            app.settle();
            app.view(|preview| {
                inspect_controls(preview, &["installer.back", "installer.next"], viewport);
                let next = control(preview, "installer.next").unwrap();
                let back = control(preview, "installer.back").unwrap();
                assert_eq!(next.accessibility().unwrap().state.disabled, disabled);
                assert!(!back.accessibility().unwrap().state.disabled);
                assert_eq!((back.get_rect(), next.get_rect()), footer);
                let next_rect = next.get_rect();
                let (body, _) = body_scroll_geometry(preview).unwrap();
                assert!(body.y1 <= next_rect.y0);
            });
            if disabled {
                app.harness()
                    .click_at(footer.1.center().x, footer.1.center().y);
                app.settle();
                assert!(
                    app.effect(&InstallerPreviewEffect::ContinuePreview)
                        .is_err()
                );
                app.view(|preview| assert_eq!(preview.journey.page, Page::Password));
            }
            let status = app.read();
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
        app.act([InstallerPreviewEffect::BackInPreview]);
        app.view(|preview| assert_eq!(preview.journey.page, Page::Migrate));
    }
}
