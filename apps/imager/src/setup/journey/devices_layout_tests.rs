//! Exercise the retained production computer-name form, not a sample device fleet.
use super::super::visual_tests::control;
use super::super::*;
use crate::resource::GuidedInstallerShell;
use ui::framework::testing::TestApp;

fn assert_no_fleet(root: &dyn Widget) {
    assert_ne!(root.name(), "Avatar");
    assert_ne!(root.name(), "IdentityDeviceConnections");
    assert_ne!(root.test_id(), Some("installer.devices.current-name"));
    if let Some(access) = root.accessibility() {
        assert_ne!(access.name.as_deref(), Some("Other devices you can add"));
    }
    for child in root.children() {
        assert_no_fleet(child);
    }
}

fn retained_form(root: &dyn Widget) -> Vec<(Rect, usize)> {
    [
        "installer.heading",
        "installer.intro",
        "installer.device-name",
    ]
    .into_iter()
    .map(|id| {
        let node = control(root, id).expect("production naming form");
        (
            node.get_rect(),
            std::ptr::from_ref(node).cast::<()>() as usize,
        )
    })
    .collect()
}

#[test]
#[cfg(unix)]
fn names_retain_form_anchors_and_mid_string_caret() {
    let mut intro_heights = Vec::new();
    for viewport in [(960, 820), (360, 640)] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = false;
            preview.restart = restart_controller::Restart::default();
            preview.journey.page = Page::Devices;
            preview.journey.name = "Ada".into();
            preview.journey.device_name = "PC".into();
            preview.dirty = true;
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        app.settle();
        let baseline = app
            .with_widget::<InstallerPreview, _>(|preview| {
                assert_no_fleet(preview);
                let heading = control(preview, "installer.heading").unwrap();
                assert_eq!(
                    heading.accessibility().unwrap().name.as_deref(),
                    Some("Name this computer")
                );
                let intro = control(preview, "installer.intro").unwrap();
                assert_eq!(
                    intro.accessibility().unwrap().name.as_deref(),
                    Some("This name identifies this computer on your other Tilo devices.")
                );
                retained_form(preview)
            })
            .unwrap();
        intro_heights.push(baseline[1].0.height());
        let heading = baseline[0].0;
        let intro = baseline[1].0;
        let input = baseline[2].0;
        assert!((heading.x0 - input.x0).abs() < 1.0);
        assert!((intro.x0 - input.x0).abs() < 1.0);
        assert!(heading.y1 <= intro.y0 && intro.y1 <= input.y0);
        assert!(input.x0 >= 0.0 && input.x1 <= f64::from(viewport.0));
        app.harness()
            .perform_named("installer.device-name", &ui::a11y::Action::Focus)
            .unwrap();
        app.harness().key("Home");
        for text in ["a", " longer ", "日本語のコンピューター ", "e\u{301} "] {
            app.harness().type_text(text);
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(
                    retained_form(preview),
                    baseline,
                    "typing must retain the form's widgets and geometry"
                );
                let input = control(preview, "installer.device-name").unwrap();
                assert!(input.is_focused());
                let caret = ui::undo::get_state(input).unwrap();
                assert_eq!(
                    caret.split(':').next().unwrap().parse::<usize>().unwrap(),
                    preview.journey.device_name.len() - 2
                );
                assert!(!preview.dirty, "typing must not rebuild the page");
            })
            .unwrap();
        }
    }
    assert!(
        intro_heights[1] > intro_heights[0],
        "purpose copy wraps on narrow screens"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn approved_name_explains_why_it_cannot_be_edited() {
    let (_custody, mut pairing) = pairing::certified_fixture();
    let deadline = tilo_test_env::hang_deadline();
    while pairing.busy() {
        assert!(std::time::Instant::now() < deadline);
        pairing.poll();
        std::thread::yield_now();
    }
    assert!(pairing.ready());
    let approved = pairing.prepared().unwrap();
    let label = approved.enrolled_device_label().unwrap().to_owned();
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.journey.page = Page::Devices;
        preview.journey.identity = Some(Identity::Existing);
        preview.journey.transfer = Some("device".into());
        preview.journey.device_name = label.clone();
        preview.pairing = pairing;
        preview.dirty = true;
    })
    .unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_no_fleet(preview);
        assert!(control(preview, "installer.device-name").is_none());
        assert_eq!(
            control(preview, "installer.devices.approved-name")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .as_deref(),
            Some(label.as_str())
        );
        assert_eq!(
            control(preview, "installer.devices.name-locked")
                .unwrap()
                .accessibility()
                .unwrap()
                .name
                .as_deref(),
            Some(t("installer-pairing-label-locked").as_str())
        );
    })
    .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: "Unapproved rename".into(),
        })
        .is_err()
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.journey.page = Page::Review;
        preview.dirty = true;
    })
    .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "devices".into(),
        })
        .is_err()
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.device_name, label);
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn live_and_preview_naming_forms_have_no_invented_fleet() {
    for live in [false, true] {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = live;
            preview.journey.page = Page::Devices;
            preview.journey.device_name = "Studio computer".into();
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_no_fleet(preview);
            assert!(control(preview, "installer.device-name").is_some());
        })
        .unwrap();
    }
}
