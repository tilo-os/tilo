use super::harness::{Walk, body_scroll_geometry, control, inspect_controls, paint_app};
use super::*;
use crate::hosting::testing::TestHost;

fn paint_screen(scenario: &str, ids: &[&str], viewport: (u32, u32), theme: Theme, tag: &str) {
    let mut app = TestHost::capture_at(Some(scenario), viewport);
    app.harness().set_theme(theme.with_reduce_motion(true));
    paint_app(&mut app, scenario, ids, viewport, tag);
}

#[test]
fn viewport_growth_keeps_logical_type_and_controls_and_pointer_mapping() {
    fn heading_font(root: &mut dyn Widget, theme: &Theme) -> Option<f32> {
        if root.test_id() == Some("installer.heading") {
            let mut scene = Scene::new();
            root.draw_child(
                &mut scene,
                Affine::IDENTITY,
                &mut tilo_ui_base::TextRenderer::new(),
                theme,
                &tilo_ui_base::draw::DrawEnv::default(),
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
            let mut app = TestHost::capture_at(Some("installer-migrate"), initial);
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
                        // The column is centred in the room the page's scroll bar leaves: its
                        // gutter is kept on the trailing edge on every step.
                        let (body, _) = body_scroll_geometry(preview).unwrap();
                        let room = body.x1 - f64::from(metrics.scrollbar_gutter * host_scale);
                        assert!((name.center().x - f64::midpoint(body.x0, room)).abs() < 1.0);
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
        let mut app = Walk::at(Page::Review, viewport, theme);
        app.edit(|preview| preview.capture = true);
        let original = app.read();
        assert_eq!(original["page"], "review");
        app.act([InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        }]);
        app.settle();
        app.view(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            assert!(preview.journey.cancels_edit());
            inspect_controls(preview, &["installer.back", "installer.next"], viewport);
        });
        app.act([InstallerPreviewEffect::ChoosePreview {
            choice: "another".into(),
        }]);
        assert_eq!(app.read()["destination"], "another");
        app.act([InstallerPreviewEffect::BackInPreview]);
        let restored = app.read();
        assert_eq!(restored["page"], "review");
        for field in ["destination", "device_name"] {
            assert_eq!(
                restored[field], original[field],
                "changed {field} on cancel"
            );
        }
        app.act([InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        }]);
        app.settle();
        app.view(|preview| {
            assert_eq!(preview.journey.destination, Some(Destination::Computer));
            inspect_controls(preview, &["installer.back"], viewport);
        });
    }
}

#[test]
fn guided_pages_paint_at_small_normal_and_large_host_scales_in_both_themes() {
    let screens: &[(&str, &[&str])] = &[
        (
            "installer-destination",
            &["installer.choice.computer", "installer.choice.another"],
        ),
        ("installer-migrate", &["installer.ticket", "installer.next"]),
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

#[test]
#[cfg(unix)]
fn resizing_or_changing_host_scale_discards_old_transition_geometry() {
    let mut app = Walk::launched();
    app.edit(|preview| {
        preview.live = false;
        preview.journey = Journey::new();
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.history = vec![Page::Destination];
        preview.journey.page = Page::Migrate;
    });
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

/// A Linux host's first choice names the route it takes, and a host that cannot install from here
/// paints that honestly in every theme and refuses the published actions that would install.
#[cfg(target_os = "linux")]
#[test]
fn a_host_discloses_its_route_and_refuses_what_it_cannot_install() {
    use super::super::internal_install_flow::{BUILD_ALLOW, RUNTIME_ALLOW_VAR};
    fn live(variables: &[(&str, &str)], edit: impl FnOnce(&mut InstallerPreview)) -> Walk {
        let mut app = Walk::launched_with(variables);
        app.edit(|preview| {
            preview.live = true;
            preview.capture = false;
            preview.journey = Journey::new();
            edit(preview);
        });
        app
    }
    for runtime_check in ["0", "1"] {
        let mut app = live(&[(RUNTIME_ALLOW_VAR, runtime_check)], |_| {});
        app.settle();
        app.view(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            let name = control(preview, "installer.choice.computer")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .unwrap();
            let expected = if BUILD_ALLOW == Some("1") || runtime_check == "1" {
                t("installer-preview-computer-body")
            } else {
                t("installer-preview-computer-usb-body")
            };
            assert!(
                name.contains(&expected),
                "the first choice must disclose its actual route: {name}"
            );
        });
    }
    for (tag, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        {
            let mut app = live(&[], |preview| {
                preview.capabilities.prepared_installation = false;
            });
            app.harness()
                .set_theme(theme.clone().with_reduce_motion(true));
            app.harness().resize((800, 600)).unwrap();
            app.settle();
            assert!(
                app.harness()
                    .inspection_json()
                    .unwrap()
                    .contains(&t("installer-guided-host-unavailable"))
            );
            app.view(|preview| {
                assert!(control(preview, "installer.choice.computer").is_none());
                assert!(!preview.guided_host_available());
            });
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
        let mut app = live(&[(RUNTIME_ALLOW_VAR, "1")], |preview| {
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.page = Page::Review;
        });
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.harness().resize((800, 600)).unwrap();
        app.settle();
        app.view(|preview| {
            assert!(!preview.direct_available());
            let button = control(preview, "installer.prepare-direct").unwrap();
            assert!(button.accessibility().unwrap().state.disabled);
        });
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

/// Every shot of the journey, and the image writer's progress, draws the real product and closes
/// through the canonical component lifecycle.
#[test]
fn guided_scenario_gallery_renders_and_closes_every_actual_component() {
    let scenarios = crate::hosting::scenarios();
    assert!(
        SCENARIOS
            .iter()
            .all(|scenario| scenarios.contains(scenario))
    );
    for scenario in SCENARIOS.iter().copied().chain(["progress"]) {
        let mut app = TestHost::capture_at(Some(scenario), (1180, 820));
        app.harness()
            .set_theme(Theme::dark().with_reduce_motion(true));
        app.settle();
        if scenario != "progress" {
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.name(), "InstallerPreview");
                if preview.journey.page != Page::Destination && preview.journey.page != Page::Ready
                {
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
        }
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
