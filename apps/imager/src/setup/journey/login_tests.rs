//! Exercise the actual overview row and source-owned sign-in controls.

#[test]
fn current_website_signins_brand_pixels() {
    use super::super::visual_review_tests::evidence;
    use ui::widget::Widget;

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

    fn images(widget: &dyn Widget, found: &mut Vec<(String, ui::kurbo::Rect)>) {
        if widget.name() == "SectionHeader" {
            let children = widget.children();
            assert_eq!(
                children[0].name(),
                "Image",
                "fixture source artwork belongs in its header"
            );
            let image = children[0].get_rect();
            let title = children[1].get_rect();
            assert!(image.x1 < title.x0, "logo must lead its actual source name");
            assert!(image.y0 >= widget.get_rect().y0 && image.y1 <= widget.get_rect().y1);
            let name = children[1].accessibility().unwrap().name.unwrap();
            assert!(
                name == "Slack" || name.starts_with("Firefox · "),
                "native default is not a profile; browser profile is meaningful: {name}"
            );
        }
        if widget.name() == "Image" {
            found.push((
                widget.accessibility().and_then(|node| node.name).unwrap(),
                widget.get_rect(),
            ));
        }
        for child in widget.children() {
            images(child, found);
        }
    }

    for (tag, viewport, theme) in [
        ("dark-1180x820-text1", (1180, 820), ui::Theme::dark()),
        (
            "dark-320x800-text2",
            (320, 800),
            ui::Theme::dark().with_text_scale(2.0),
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let selection = fixture(home.path());
        let mut app =
            TestApp::<crate::resource::GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.restart = restart_controller::Restart::default();
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.categories = [false, true, true];
            preview.belongings.sessions = selection;
            preview.dirty = true;
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme.with_reduce_motion(true));
        app.settle();
        app.effect(&InstallerPreviewEffect::ReviewServiceSessions)
            .unwrap();
        app.settle();
        let mut bounds = Vec::new();
        app.with_widget::<InstallerPreview, _>(|preview| images(preview, &mut bounds))
            .unwrap();
        evidence(app.harness(), &format!("website-logins-intrinsic-{tag}"));
        if viewport.0 == 1180 {
            let slack = bounds.iter().find(|(label, _)| label == "Slack").unwrap().1;
            let firefox = bounds
                .iter()
                .find(|(label, _)| label == "Firefox")
                .unwrap()
                .1;
            assert!(
                firefox.y0 - slack.y0 < 200.0,
                "source groups must size to their content, not stretch into empty space"
            );
        }
        for name in ["Slack", "Firefox"] {
            let (_, rect) = bounds.iter().find(|(label, _)| label == name).unwrap();
            // Taffy rounds both edges independently, not the size. Text
            // scaling can make the token fractional, yielding a 19 or 20px
            // cell depending on its row's unrounded origin.
            let edge = f64::from(app.harness().theme().tokens.metrics.icon_sm);
            let snapped = [edge.floor(), edge.ceil()];
            assert!(rect.width() > 0.0 && rect.height() > 0.0);
            assert!(
                snapped.contains(&rect.width()),
                "{name}: width must be the snapped icon token"
            );
            assert!(
                snapped.contains(&rect.height()),
                "{name}: height must be the snapped icon token"
            );
        }
        for name in ["Slack", "Firefox"] {
            // At large text sizes the second source is below the viewport.
            // Reveal each actual logo with the production scroll path before
            // checking its pixels, rather than treating offscreen geometry as paint.
            let body = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    super::super::visual_tests::body_scroll_geometry(preview)
                        .unwrap()
                        .0
                })
                .unwrap();
            app.harness().send(&ui::synthetic_input::cursor_moved(
                body.x0 + 2.0,
                body.center().y,
            ));
            let mut rect = bounds.iter().find(|(label, _)| label == name).unwrap().1;
            for _ in 0..200 {
                if rect.y0 >= body.y0 && rect.y1 <= body.y1 {
                    break;
                }
                app.harness()
                    .scroll(0.0, if rect.y0 < body.y0 { 1.0 } else { -1.0 });
                app.settle();
                let mut current = Vec::new();
                app.with_widget::<InstallerPreview, _>(|preview| images(preview, &mut current))
                    .unwrap();
                rect = current.iter().find(|(label, _)| label == name).unwrap().1;
            }
            assert!(
                rect.y0 >= body.y0 && rect.y1 <= body.y1,
                "{name} must be revealed by scrolling"
            );
            // Decode the production PNG, not a separately painted image or scene.
            let output = tempfile::tempdir().unwrap();
            let path = output.path().join("logins.png");
            app.harness().render_png(&path);
            let bytes = std::fs::read(path).unwrap();
            let raster = tilo_image_codec::DecodeImage {
                encoded: &bytes,
                max_dimension: 4096,
            }
            .decode_native()
            .unwrap();
            assert!(
                rect.x0 >= 0.0
                    && rect.y0 >= 0.0
                    && rect.x1 <= f64::from(raster.width)
                    && rect.y1 <= f64::from(raster.height),
                "{name} must be visible in the production frame"
            );
            let pixels: Vec<_> = (rect.y0.floor() as u32..rect.y1.ceil() as u32)
                .flat_map(|y| (rect.x0.floor() as u32..rect.x1.ceil() as u32).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let at = ((y * raster.width + x) * 4) as usize;
                    &raster.rgba[at..at + 3]
                })
                .collect();
            if name == "Slack" {
                for colour in &[
                    [224, 30, 90],
                    [54, 197, 240],
                    [46, 182, 125],
                    [236, 178, 46],
                ] {
                    assert!(
                        pixels.iter().any(|pixel| pixel
                            .iter()
                            .zip(colour)
                            .all(|(a, b)| a.abs_diff(*b) <= 12)),
                        "{name}: missing brand colour {colour:?}"
                    );
                }
            } else {
                assert!(
                    pixels
                        .iter()
                        .filter(|pixel| pixel[0] > 180 && pixel[1] < 180 && pixel[2] < 130)
                        .count()
                        > 3,
                    "Firefox artwork must paint orange pixels"
                );
            }
        }
    }
}

#[test]
fn current_website_signins_raster_matrix() {
    use super::super::visual_review_tests::evidence;
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
        ("dark", ui::Theme::dark()),
        ("light", ui::Theme::light()),
        ("high-contrast", ui::Theme::high_contrast()),
    ] {
        for viewport in [(1180, 820), (640, 800), (320, 800)] {
            for text_scale in [1.0, 2.0] {
                let home = tempfile::tempdir().unwrap();
                let selection = fixture(home.path());
                let browser = selection
                    .sources
                    .iter()
                    .find(|source| source.application == "Firefox")
                    .unwrap()
                    .id
                    .clone();
                let tag = format!("{tag}-{}x{}-text{text_scale}", viewport.0, viewport.1);
                let mut app =
                    TestApp::<crate::resource::GuidedInstallerShell>::launched(&["--installer"])
                        .unwrap();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.restart = restart_controller::Restart::default();
                    preview.journey.page = Page::Belongings;
                    preview.journey.migrate = Some(true);
                    preview.journey.categories = [false, true, true];
                    preview.belongings.sessions = selection;
                    preview.dirty = true;
                })
                .unwrap();
                app.harness().resize(viewport).unwrap();
                app.harness().set_theme(
                    theme
                        .clone()
                        .with_text_scale(text_scale)
                        .with_reduce_motion(true),
                );
                app.settle();
                app.effect(&InstallerPreviewEffect::ReviewServiceSessions)
                    .unwrap();
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| assert!(preview.sessions_review))
                    .unwrap();
                evidence(app.harness(), &format!("website-logins-expanded-{tag}"));
                app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
                    source_id: browser.clone(),
                    included: true,
                })
                .unwrap();
                app.effect(&InstallerPreviewEffect::SetPreviewServiceSession {
                    source_id: browser.clone(),
                    service: "discord".into(),
                    included: false,
                })
                .unwrap();
                app.settle();
                assert!(
                    app.harness()
                        .inspection_json()
                        .unwrap()
                        .contains("1 of 2 selected")
                );
                evidence(app.harness(), &format!("website-logins-partial-{tag}"));
                let sources = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        preview
                            .belongings
                            .sessions
                            .sources
                            .iter()
                            .map(|source| source.id.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap();
                for source_id in &sources {
                    app.effect(&InstallerPreviewEffect::ExpandServiceSessionGroup {
                        source_id: source_id.clone(),
                        expanded: false,
                    })
                    .unwrap();
                }
                app.settle();
                evidence(app.harness(), &format!("website-logins-collapsed-{tag}"));
                for source_id in &sources {
                    app.effect(&InstallerPreviewEffect::ExpandServiceSessionGroup {
                        source_id: source_id.clone(),
                        expanded: true,
                    })
                    .unwrap();
                }
                app.settle();
                let body = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        super::super::visual_tests::body_scroll_geometry(preview)
                            .unwrap()
                            .0
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
                evidence(
                    app.harness(),
                    &format!("website-logins-expanded-bottom-{tag}"),
                );
            }
        }
    }
}
use super::super::{InstallerPreview, InstallerPreviewEffect, Page, restart_controller};
use super::*;
use ui::framework::testing::TestApp;

fn fixture(home: &Path) -> Selection {
    let browser = super::tests::firefox(home, "profile", true);
    browser.execute_batch("INSERT INTO moz_cookies VALUES(2,'.discord.com','__dcfduid','private-login-cookie','/',2000000000000,1,1,256,'',0,0);").unwrap();
    let _native = super::tests::native_slack_database(home, false, false, true);
    let inventory = super::tests::inventory(home, &["profile"]);
    Selection::discover(home, &inventory.candidates).unwrap()
}

fn disclosures(root: &dyn ui::widget::Widget) -> Vec<ui::kurbo::Rect> {
    if root.name() == "SectionDisclosure" {
        return vec![root.get_rect()];
    }
    root.children().into_iter().flat_map(disclosures).collect()
}

#[test]
fn website_signins_row_routes_to_source_groups_not_flatpak_details() {
    let home = tempfile::tempdir().unwrap();
    let selection = fixture(home.path());
    let browser = selection
        .sources
        .iter()
        .find(|source| source.application == "Firefox")
        .unwrap()
        .id
        .clone();
    let browser_index = selection
        .sources
        .iter()
        .filter(|source| selection.detected.iter().any(|(id, _, _)| id == &source.id))
        .position(|source| source.id == browser)
        .unwrap();
    let native = selection
        .sources
        .iter()
        .find(|source| source.native_application)
        .unwrap()
        .id
        .clone();
    let mut app = TestApp::<crate::ImagerShell>::launched(&["--installer-preview"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = true;
        preview.restart = restart_controller::Restart::default();
        preview.journey.page = Page::Belongings;
        preview.journey.migrate = Some(true);
        preview.journey.categories = [false, true, true];
        preview.journey.details = false;
        preview.belongings.sessions = selection;
        use crate::setup::flatpak_transfer::{Installation, InstalledRef, Inventory};
        preview.belongings.flatpak = Some(Inventory {
            refs: (0..71)
                .map(|index| InstalledRef {
                    reference: format!("app/org.example.App{index}/x86_64/stable"),
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
    super::super::visual_tests::scroll_into_view(&mut app, "installer.sessions.overview");
    app.harness()
        .click_named("installer.sessions.overview")
        .unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(
            preview.sessions_review,
            "the Website sign-ins row must open its own view"
        );
        assert!(!preview.journey.details);
    })
    .unwrap();
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("Firefox"));
    assert!(inspection.contains("Slack"));
    assert!(!inspection.contains("Slack · Slack"));
    let heading = app
        .with_widget::<InstallerPreview, _>(|preview| {
            super::super::visual_tests::control(preview, "installer.heading")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .unwrap()
        })
        .unwrap();
    assert_eq!(heading, "Website sign-ins");
    assert_eq!(
        inspection
            .matches(&super::super::t("installer-logins-source-body"))
            .count(),
        1
    );
    assert!(
        inspection.contains("Image"),
        "known browser uses bundled recognition artwork"
    );
    for application in ["Firefox", "Slack"] {
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.role() == ui::accesskit::Role::Image
                    && node.label() == Some(application)),
            "the discovered {application} source must have its actual bundled logo"
        );
    }
    assert!(!inspection.contains("installer.flatpak"));
    assert!(
        !inspection.contains("installer.next"),
        "the sign-in review has Back, not a migration Continue action"
    );
    assert!(!inspection.contains("private-login-cookie"));
    assert!(!inspection.contains("private-native-slack-sentinel"));
    app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
        source_id: browser.clone(),
        included: true,
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SetPreviewServiceSession {
        source_id: browser.clone(),
        service: "discord".into(),
        included: false,
    })
    .unwrap();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("1 of 2 selected")
    );
    let mixed = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::CheckBox
                && node
                    .label()
                    .is_some_and(|name| name.contains("1 of 2 selected"))
        })
        .expect("partial group exposes its exact selected count")
        .1;
    assert_eq!(mixed.toggled(), Some(ui::accesskit::Toggled::Mixed));
    let chevron = app
        .with_widget::<InstallerPreview, _>(|preview| disclosures(preview)[browser_index])
        .unwrap();
    app.harness()
        .click_at(chevron.center().x, chevron.center().y);
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.belongings.sessions.collapsed.contains(&browser));
        assert!(
            super::super::visual_tests::control(
                preview,
                &format!("installer.logins.group.{browser}")
            )
            .is_some(),
            "collapsed groups keep their selection and count available"
        );
    })
    .unwrap();
    let chevron = app
        .with_widget::<InstallerPreview, _>(|preview| disclosures(preview)[browser_index])
        .unwrap();
    app.harness()
        .click_at(chevron.center().x, chevron.center().y);
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(!preview.belongings.sessions.collapsed.contains(&browser))
    })
    .unwrap();
    app.harness()
        .click_named(&format!("installer.logins.group.{browser}"))
        .unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview
                .belongings
                .sessions
                .included
                .iter()
                .filter(|(id, _)| id == &browser)
                .count(),
            2
        );
    })
    .unwrap();
    let group = format!("installer.logins.group.{browser}");
    // All selected -> none, then none -> all, through the actual control.
    for expected in [0, 2] {
        app.harness().click_named(&group).unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview
                    .belongings
                    .sessions
                    .included
                    .iter()
                    .filter(|(id, _)| id == &browser)
                    .count(),
                expected
            );
        })
        .unwrap();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&format!("{expected} of 2 selected"))
        );
    }
    app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
        source_id: native,
        included: false,
    })
    .unwrap();
    let before = app
        .with_widget::<InstallerPreview, _>(|preview| preview.belongings.sessions.included.clone())
        .unwrap();
    app.harness().click_named("installer.back").unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(!preview.sessions_review);
        assert_eq!(preview.journey.page, Page::Belongings);
        assert!(!preview.journey.details);
        assert_eq!(preview.belongings.sessions.included, before);
    })
    .unwrap();
}

#[test]
fn source_group_actions_reject_disabled_unknown_and_replaced_sources() {
    let home = tempfile::tempdir().unwrap();
    let selection = fixture(home.path());
    let browser = selection
        .sources
        .iter()
        .find(|source| source.application == "Firefox")
        .unwrap()
        .id
        .clone();
    let mut app = TestApp::<crate::ImagerShell>::launched(&["--installer-preview"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = true;
        preview.restart = restart_controller::Restart::default();
        preview.journey.page = Page::Belongings;
        preview.journey.migrate = Some(true);
        preview.journey.categories = [false, false, false];
        preview.sessions_review = true;
        preview.belongings.sessions = selection;
        preview.dirty = true;
    })
    .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
            source_id: browser.clone(),
            included: false
        })
        .is_err()
    );
    app.with_widget::<InstallerPreview, _>(|preview| preview.journey.categories[2] = true)
        .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
            source_id: "unknown-source".into(),
            included: true
        })
        .is_err()
    );
    let before = app
        .with_widget::<InstallerPreview, _>(|preview| {
            (
                preview.belongings.sessions.included.clone(),
                preview.belongings.sessions.decided.clone(),
            )
        })
        .unwrap();
    std::fs::rename(
        home.path().join(".mozilla/firefox/profile"),
        home.path().join(".mozilla/firefox/replaced"),
    )
    .unwrap();
    std::fs::create_dir(home.path().join(".mozilla/firefox/profile")).unwrap();
    // Both children belong to this now-stale source. Neither inclusion nor
    // exclusion may partially update the batch or its recorded decisions.
    for included in [false, true] {
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewServiceSessionGroup {
                source_id: browser.clone(),
                included,
            })
            .is_err()
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.belongings.sessions.included, before.0);
            assert_eq!(preview.belongings.sessions.decided, before.1);
        })
        .unwrap();
    }
}

#[test]
fn unselectable_login_group_is_disabled_and_does_not_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let (_path, _database) = super::tests::native_slack_database(home.path(), false, true, true);
    let inventory =
        crate::setup::account_transfer::discover(&crate::setup::account_transfer::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
    let mut selection = Selection::from_discovery(
        service_profiles::discover_without_slack_package(home.path(), &inventory.candidates)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(selection.detected.len(), 1);
    let id = selection.detected[0].0.clone();
    assert_eq!(selection.detected[0].2, Presence::NeedsAccess);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let dispatched = calls.clone();
    let view = selection.logins_view(
        true,
        |_, _, _| panic!("unsupported login must have no individual control"),
        move |_, _| dispatched.set(dispatched.get() + 1),
        |_, _| {},
    );
    let mut harness = ui::headless::FrameHarness::new(ui::State::new(), view, (1180, 820));
    let checkbox = harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::CheckBox)
        .unwrap()
        .1;
    assert!(checkbox.is_disabled());
    assert_eq!(checkbox.toggled(), Some(ui::accesskit::Toggled::False));
    let inspection = harness.inspection_json().unwrap();
    assert!(!inspection.contains("0 of 1 selected"));
    assert!(inspection.contains(&super::super::t_args(
        "installer-service-source-reconnect-required",
        &[(
            "source",
            super::super::t("installer-service-slack-label").into()
        )],
    )));
    harness
        .click_named(&format!("installer.logins.group.{id}"))
        .unwrap();
    assert_eq!(calls.get(), 0);
    for included in [true, false] {
        assert_eq!(selection.set_group(&id, included), Ok(false));
        assert!(selection.included.is_empty());
        assert!(selection.decided.is_empty());
    }
    // Even an ineligible batch cannot turn a stale source into a successful no-op.
    std::fs::rename(
        home.path().join(".config/Slack"),
        home.path().join("old-slack"),
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".config/Slack")).unwrap();
    assert_eq!(
        selection.set_group(&id, true),
        Err("installer-service-source-changed")
    );
}

#[test]
fn mixed_login_group_selects_only_eligible_children_and_keeps_reconnect_reason() {
    let home = tempfile::tempdir().unwrap();
    let mut selection = fixture(home.path());
    let source = selection
        .sources
        .iter()
        .find(|source| source.application == "Firefox")
        .unwrap();
    assert!(!source.supports_encrypted_cookie_capture());
    let id = source.id.clone();
    // Exercise a mixed discovery snapshot without granting any new source authority.
    let slack = selection
        .detected
        .iter_mut()
        .find(|(source, service, _)| source == &id && *service == Service::Slack)
        .unwrap();
    slack.2 = Presence::NeedsAccess;
    selection.included.retain(|(source, _)| source != &id);
    assert_eq!(selection.set_group(&id, true), Ok(true));
    assert!(selection.included.contains(&(id.clone(), Service::Discord)));
    assert!(!selection.included.contains(&(id.clone(), Service::Slack)));
    assert_eq!(selection.decided, vec![Service::Discord]);
    let view = selection.logins_view(true, |_, _, _| {}, |_, _| {}, |_, _| {});
    let harness = ui::headless::FrameHarness::new(ui::State::new(), view, (1180, 820));
    let inspection = harness.inspection_json().unwrap();
    assert!(inspection.contains("1 of 1 available sign-ins selected"));
    assert!(!inspection.contains("2 of 2 selected"));
    assert!(inspection.contains(&super::super::t_args(
        "installer-service-source-reconnect-required",
        &[(
            "source",
            super::super::t("installer-service-slack-label").into()
        )],
    )));
    assert_eq!(selection.set_group(&id, false), Ok(true));
    assert!(!selection.included.iter().any(|(source, _)| source == &id));
    assert_eq!(selection.decided, vec![Service::Discord]);
}
