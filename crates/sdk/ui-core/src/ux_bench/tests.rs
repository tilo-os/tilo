use super::*;

#[test]
fn journeys_reject_missing_outcomes_unknown_steps_and_free_semantic_success() {
    // phase: missing_outcome_cannot_be_a_successful_journey
    {
        let scenario = r#"{"name":"empty","goal":"Open Files","viewport":[800,600],"budget":{"clicks":2,"keys":4},"steps":[]}"#;
        assert!(Scenario::parse(scenario).is_err());
    }
    // phase: unknown_steps_and_invalid_scale_are_rejected
    {
        for steps in [
            r#"[{"op":"teleport"}]"#,
            r#"[{"op":"checkpoint","name":"../escape","checks":[]}]"#,
        ] {
            let raw = format!(
                r#"{{"name":"bad","goal":"test","viewport":[800,600],"scale":0,"budget":{{"clicks":2,"keys":4}},"steps":{steps}}}"#
            );
            assert!(Scenario::parse(&raw).is_err());
        }
    }
    // phase: semantic_invocation_is_not_counted_as_free_pointer_success
    {
        let mut counts = Counts::default();
        counts.record(&serde_json::json!({"click_named":"launch"}));
        counts.record(&serde_json::json!({"key":"Enter"}));
        counts.record(&serde_json::json!({"type":"files"}));
        counts.record(&serde_json::json!({"perform":{"target":"launch","action":"click"}}));
        assert_eq!(counts.clicks, 1);
        assert_eq!(counts.keys, 6);
        assert_eq!(counts.semantic_actions, 1);
    }
    // phase: target_relative_schema_rejects_unknown_regions_and_ambiguous_wheel_routes
    {
        for step in [
            r#"{"op":"wheel","target":"list","at":[30,30],"delta":[0,-20],"unit":"physical_pixels","phase":"moved"}"#,
            r#"{"op":"wheel","delta":[0,-20],"unit":"physical_pixels","phase":"moved"}"#,
            r#"{"op":"checkpoint","name":"bad","checks":[{"kind":"image_unchanged","from":"initial","region":{"target":"list","inset":[0,0,0,0],"typo":true}}]}"#,
        ] {
            let raw = format!(
                r#"{{"name":"invalid route","goal":"verify content","viewport":[400,300],"budget":{{"clicks":0,"keys":0}},"steps":[{step},{{"op":"checkpoint","name":"result","checks":[{{"kind":"visible","target":"row-0"}}]}}]}}"#
            );
            assert!(Scenario::parse(&raw).is_err());
        }
    }
}

#[test]
fn reports_keep_numeric_contrast_spacing_and_visibility_evidence() {
    // phase: contrast_detects_unreadable_and_readable_pairs
    {
        assert!(
            (pixel_contrast([0, 0, 0, 255], [255, 255, 255, 255]).unwrap() - 21.0).abs() < 0.001
        );
        assert!(pixel_contrast([130, 130, 130, 255], [128, 128, 128, 255]).unwrap() < 1.1);
        assert!(pixel_contrast([0, 0, 0, 0], [255, 255, 255, 255]).is_err());
    }
    // phase: spacing_and_overlap_have_numeric_evidence
    {
        let a = [10.0, 20.0, 100.0, 30.0];
        let b = [10.0, 58.0, 100.0, 30.0];
        assert_eq!(gap(a, b, Axis::Y), 8.0);
        assert_eq!(overlap_area(a, b), 0.0);
        assert_eq!(overlap_area(a, [20.0, 30.0, 20.0, 20.0]), 400.0);
    }
    // phase: report_retains_bad_spacing_contrast_and_click_budget
    {
        let mut driver = scrolling_driver(false);
        let scenario = Scenario::parse(r#"{
        "name":"negative control","goal":"read a row","viewport":[400,300],
        "budget":{"clicks":0,"keys":0},
        "steps":[
            {"op":"input","action":{"click_named":"row-0"}},
            {"op":"checkpoint","name":"result","checks":[
                {"kind":"visible","target":"row-0"},
                {"kind":"gap","first":"row-0","second":"row-1","axis":"y","min":200,"max":300},
                {"kind":"contrast","label":"negative control","foreground":[1,1],"background":[1,1],"min":4.5}
            ]}
        ]}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let report = run(&mut driver, &scenario, dir.path()).unwrap();
        assert!(!report.passed);
        assert_eq!(report.counts.clicks, 1);
        assert!(report.failures.iter().any(|f| f.contains("Gap")));
        assert!(report.failures.iter().any(|f| f.contains("Contrast")));
        assert!(report.failures.iter().any(|f| f.contains("click budget")));
        assert!(dir.path().join("index.html").exists());
        // The native App composites onto the window ground
        // (`surfaces.content`). A transparent offscreen image cannot be used
        // as evidence of that visible contrast.
        let png = image::open(dir.path().join("result.png"))
            .unwrap()
            .to_rgba8();
        assert!(
            png.pixels().all(|p| p.0[3] == 255),
            "capture must match the opaque native window ground"
        );
    }
    // phase: clipped_content_cannot_pass_visible_check
    {
        let mut driver = scrolling_driver(false);
        let scenario = Scenario::parse(r#"{"name":"clip","goal":"reach row 5","viewport":[400,300],"budget":{"clicks":0,"keys":0},"steps":[{"op":"checkpoint","name":"result","checks":[{"kind":"visible","target":"row-5"}]}]}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let report = run(&mut driver, &scenario, dir.path()).unwrap();
        assert!(
            !report.passed,
            "a row below a 120px scroll viewport is clipped even inside a 300px window"
        );
    }
}

fn scrolling_driver(nested: bool) -> crate::headless::FrameHarness {
    use crate::widget::{Column, Scrollable, Text};
    let mut content = Column::new().with_intrinsic_height();
    for i in 0..40 {
        content = content.push(Box::new(
            Text::new(&format!("Row {i}")).with_id(format!("row-{i}")),
        ));
    }
    let inner = Scrollable::new_vertical()
        .with_height(120.0)
        .with_child(Box::new(content));
    let root: Box<dyn crate::widget::Widget> = if nested {
        Box::new(
            Scrollable::new_vertical()
                .with_height(200.0)
                .with_child(Box::new(inner)),
        )
    } else {
        Box::new(inner)
    };
    crate::headless::FrameHarness::new(crate::State::new(), root, (400, 300))
}

fn wheel(driver: &mut crate::headless::FrameHarness, physical_y: f64, scale: f64) {
    use winit::{
        dpi::PhysicalPosition,
        event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent},
    };
    driver.set_input_scale(scale).unwrap();
    driver.send(&crate::synthetic_input::cursor_moved(
        60.0 * scale,
        60.0 * scale,
    ));
    driver.send(&WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, physical_y)),
        phase: TouchPhase::Moved,
    });
}

#[test]
fn scroll_measurement_follows_the_content_that_actually_moves() {
    // phase: nested_scroll_moves_inner_content_when_outer_does_not_overflow
    {
        let mut driver = scrolling_driver(true);
        let before = driver.targets()["row-0"].y0;
        let inner_before = driver.targets()["Scrollable[0]/Scrollable[0]"].y0;
        wheel(&mut driver, -40.0, 1.0);
        let after = driver.targets()["row-0"].y0;
        assert!(
            (after - before + 40.0).abs() < 0.5,
            "inner movement {before} -> {after}"
        );
        assert_eq!(
            driver.targets()["Scrollable[0]/Scrollable[0]"].y0,
            inner_before,
            "outer must not pan the inner viewport"
        );
    }
    // phase: physical_pixel_scroll_has_equal_logical_distance_at_retina_scale
    {
        let mut a = scrolling_driver(false);
        let mut b = scrolling_driver(false);
        wheel(&mut a, -40.0, 1.0);
        wheel(&mut b, -80.0, 2.0);
        assert!((a.targets()["row-0"].y0 - b.targets()["row-0"].y0).abs() < 0.5);
    }
    // phase: nested_scroll_boundary_does_not_move_a_fitting_outer_viewport
    {
        let mut driver = scrolling_driver(true);
        let before = driver.targets()["Scrollable[0]/Scrollable[0]"].y0;
        wheel(&mut driver, -10000.0, 1.0);
        wheel(&mut driver, -60.0, 1.0);
        assert_eq!(driver.targets()["Scrollable[0]/Scrollable[0]"].y0, before);
    }
    // phase: image_scroll_measurement_rejects_static_pixels_and_recovers_translation
    {
        let before = image::RgbaImage::from_fn(120, 120, |x, y| {
            image::Rgba([
                ((x * 13 + y * 7) % 255) as u8,
                ((x * 3 + y * 11) % 255) as u8,
                0,
                255,
            ])
        });
        let after =
            image::RgbaImage::from_fn(120, 120, |x, y| *before.get_pixel(x, (y + 17).min(119)));
        let measured = checks::image_shift(&before, &after, [10, 10, 100, 100], Axis::Y).unwrap();
        assert_eq!(measured.0, -17);
        assert!(checks::image_shift(&before, &before, [10, 10, 100, 100], Axis::Y).is_err());
    }
}

#[test]
fn region_evidence_cannot_be_certified_from_the_wrong_capture() {
    // phase: stationary_region_rejects_moving_content
    {
        let mut driver = scrolling_driver(false);
        let scenario = Scenario::parse(r#"{"name":"stationary anchor","goal":"keep header fixed","viewport":[400,300],"budget":{"clicks":0,"keys":0},"steps":[{"op":"wheel","at":[60,60],"delta":[0,-20],"unit":"physical_pixels","phase":"moved"},{"op":"checkpoint","name":"result","checks":[{"kind":"image_unchanged","from":"initial","region":[0,0,100,100]},{"kind":"visible","target":"row-3"}]}]}"#).unwrap();
        let out = tempfile::tempdir().unwrap();
        let report = run(&mut driver, &scenario, out.path()).unwrap();
        assert!(report.failures.iter().any(|f| f.contains("ImageUnchanged")));
    }
    // phase: mismatched_driver_cannot_certify_regions_outside_capture
    {
        let mut driver = scrolling_driver(false);
        let scenario = Scenario::parse(r#"{"name":"bad viewport","goal":"check region","viewport":[800,600],"budget":{"clicks":0,"keys":0},"steps":[{"op":"checkpoint","name":"result","checks":[{"kind":"image_unchanged","from":"initial","region":[500,400,100,100]},{"kind":"visible","target":"row-0"}]}]}"#).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert!(run(&mut driver, &scenario, out.path()).is_err());
    }
    // phase: blank_stationary_pixels_and_missing_wheel_targets_retain_failure_evidence
    {
        for steps in [
            r#"[{"op":"checkpoint","name":"result","checks":[{"kind":"image_unchanged","from":"initial","region":[300,200,50,50]},{"kind":"visible","target":"row-0"}]}]"#,
            r#"[{"op":"wheel","target":"missing","delta":[0,-20],"unit":"physical_pixels","phase":"moved"},{"op":"checkpoint","name":"result","checks":[{"kind":"visible","target":"row-0"}]}]"#,
        ] {
            let mut driver = scrolling_driver(false);
            let raw = format!(
                r#"{{"name":"negative evidence","goal":"verify content","viewport":[400,300],"budget":{{"clicks":0,"keys":0}},"steps":{steps}}}"#
            );
            let scenario = Scenario::parse(&raw).unwrap();
            let out = tempfile::tempdir().unwrap();
            let report = run(&mut driver, &scenario, out.path()).unwrap();
            assert!(!report.passed);
            assert!(out.path().join("report.json").exists());
            assert!(report.failures.iter().any(
                |f| f.contains("blank stationary") || f.contains("missing or ambiguous wheel")
            ));
        }
    }
    // phase: semantic_values_cannot_certify_wrong_content_or_selection
    {
        let mut driver = scrolling_driver(false);
        let scenario = Scenario::parse(r#"{"name":"wrong document","goal":"read expected content","viewport":[400,300],"budget":{"clicks":0,"keys":0},"steps":[{"op":"checkpoint","name":"result","checks":[{"kind":"semantic_value","target":"semantic:Row 0","equals":"wrong document bytes"},{"kind":"selected","target":"semantic:Row 0"},{"kind":"visible","target":"row-0"}]}]}"#).unwrap();
        let out = tempfile::tempdir().unwrap();
        let report = run(&mut driver, &scenario, out.path()).unwrap();
        assert!(!report.passed);
        assert_eq!(report.failures.len(), 2);
    }
}

/// A surface whose shape changes with its width: a navigation shell around a
/// page holding one primary action. Every width class must keep that action
/// reachable with the keyboard, not merely drawn.
fn responsive_surface(present_content: bool) -> Box<dyn crate::widget::Widget> {
    use crate::widget::{
        Button, Column, ContentKind, NavItem, NavList, NavPane, NavSection, NavShell,
    };
    let sidebar = NavList::new(vec![NavSection::untitled(
        "places",
        vec![NavItem::new("home", "Home")],
    )]);
    let mut shell = NavShell::new()
        .content_kind(ContentKind::Page)
        .sidebar(Box::new(sidebar))
        .content(Box::new(Column::new().with_intrinsic_height().push(
            Box::new(Button::new("save", "Save changes").on_view(|| {})),
        )));
    if present_content {
        // The journey is already on the page, as a person who chose a
        // destination is: this asks whether its action still takes the
        // keyboard once the window narrows, not whether the destination can
        // be reached.
        shell.show(NavPane::Content);
    }
    Box::new(shell)
}

fn tab_to_first_action(driver: &mut crate::headless::FrameHarness) {
    use winit::{event::ElementState, keyboard::Key};
    driver.send_host_event(&crate::synthetic_input::keyboard_input(
        crate::synthetic_input::key_event(
            Key::Named(winit::keyboard::NamedKey::Tab),
            None,
            ElementState::Pressed,
        ),
    ));
}

/// The journey that asks the question: on a page whose action is the point,
/// one Tab must land on that action.
fn focus_journey(name: &str, viewport: (u32, u32), keys: u32) -> Scenario {
    Scenario::parse(&format!(
        r#"{{"name":"{name} focus","goal":"reach the primary action with the keyboard","viewport":[{},{}],"budget":{{"clicks":0,"keys":{keys}}},"steps":[{{"op":"checkpoint","name":"result","checks":[{{"kind":"focus","target":"Save changes"}},{{"kind":"focus","target":"semantic:Save changes"}},{{"kind":"visible","target":"semantic:Save changes"}}]}}]}}"#,
        viewport.0, viewport.1
    ))
    .unwrap()
}

#[test]
fn keyboard_focus_on_the_primary_action_survives_every_width_class_and_theme() {
    // phase: focus_reaches_the_primary_action_at_every_supported_width
    {
        for (name, viewport) in crate::launch::RESPONSIVE_VIEWPORTS {
            for theme in ["dark", "light", "high-contrast"] {
                let mut driver = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    responsive_surface(true),
                    viewport,
                    crate::theme::Theme::named(theme).expect("a supported theme"),
                );
                tab_to_first_action(&mut driver);
                let out = tempfile::tempdir().unwrap();
                let report =
                    run(&mut driver, &focus_journey(name, viewport, 1), out.path()).unwrap();
                assert!(
                    report.passed,
                    "the primary action must hold focus at {name} ({}x{}) in {theme}: {:?}",
                    viewport.0, viewport.1, report.failures
                );
            }
        }
    }
    // phase: an_action_the_keyboard_cannot_reach_is_not_certified
    {
        // A phone keeps one column: choosing nothing yet shows the sidebar, so
        // the page's action is not where the keyboard goes. The same journey
        // must fail rather than pass on the target's name alone.
        let viewport = (390, 844);
        let mut driver = crate::headless::FrameHarness::new(
            crate::State::new(),
            responsive_surface(false),
            viewport,
        );
        tab_to_first_action(&mut driver);
        let out = tempfile::tempdir().unwrap();
        let report = run(
            &mut driver,
            &focus_journey("sidebar", viewport, 1),
            out.path(),
        )
        .unwrap();
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.contains("Focus")),
            "a page the keyboard never reached must fail the focus check: {:?}",
            report.failures
        );
    }
    // phase: focus_is_read_back_as_evidence_not_assumed_from_the_target
    {
        let mut driver = crate::headless::FrameHarness::new(
            crate::State::new(),
            responsive_surface(true),
            (1366, 768),
        );
        // Nothing has taken focus, so naming the action must not pass even
        // though it is visible.
        let scenario = Scenario::parse(r#"{"name":"unfocused","goal":"prove focus","viewport":[1366,768],"budget":{"clicks":0,"keys":0},"steps":[{"op":"checkpoint","name":"result","checks":[{"kind":"focus","target":"Save changes"},{"kind":"visible","target":"semantic:Save changes"}]}]}"#).unwrap();
        let out = tempfile::tempdir().unwrap();
        let report = run(&mut driver, &scenario, out.path()).unwrap();
        assert!(
            !report.passed,
            "an unfocused action is not evidence of focus"
        );
        let frame = report.frames.last().unwrap();
        let observed = frame
            .checks
            .iter()
            .find(|check| check.check.get("kind").and_then(Value::as_str) == Some("focus"))
            .map(|check| check.observed.clone())
            .unwrap();
        assert_eq!(observed["focused"], Value::Bool(false));
        assert!(
            out.path()
                .join(format!("{}.inspection.json", frame.name))
                .is_file(),
            "the focus owner ships with the run's evidence"
        );
    }
}
