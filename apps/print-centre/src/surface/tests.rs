use super::*;
use crate::resource::PrintCentreShell;
use ui::framework::ResourceApp;
use ui::prelude::TestApp;
use ui::theme::Theme;
use ui::tilo_resource::Atom;
use vello::kurbo::Rect;

fn registry() -> ui::tilo_actions::ActionRegistry {
    ui::tilo_actions::ActionRegistry::with(<PrintDefaults as ui::tilo_resource::Resource>::register)
}

thread_local! {
    /// What CUPS and SANE report to a live window in a test, instead of
    /// asking the machine the test runs on.
    pub(super) static INSTALLED: std::cell::RefCell<Option<Snapshot>> =
        const { std::cell::RefCell::new(None) };
}

/// A live Print Centre whose refresh found the studio printer, its
/// documents and its scanner.
pub(super) fn live_with_studio() -> TestApp<PrintCentreShell> {
    INSTALLED.with(|installed| *installed.borrow_mut() = Some(demo_snapshot()));
    let mut app = TestApp::<PrintCentreShell>::new().expect("launch");
    tilo_test_env::wait_until("the refresh to report the studio printer", || {
        app.settle();
        app.harness()
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .any(|child| child.kind == "print-centre.printer")
    });
    app
}

/// The print centre `app`'s window shows.
pub(super) fn centre(app: &mut TestApp<PrintCentreShell>) -> Shared {
    app.with_widget(|window: &mut PrintCentreWindow| Rc::clone(&window.centre))
        .expect("the Print Centre window is mounted")
}

/// The tree Print Centre publishes once a refresh has found printers,
/// documents and scanners.
fn populated_resources() -> Vec<ui::tilo_actions::ResourceSpec> {
    live_with_studio().harness().resources()
}

// The defaults' actions all take an argument; the sweep validates the
// whole published surface (the pages, every printer, document and
// scanner) and Ctrl+K.
ui::action_contract_tests! {
    seed: ui::automerge::AutoCommit::new(),
    registry: registry(),
    resources: |_| populated_resources(),
    palette: ["Print a document", "Open print queue", "Find network printers", "Open printer"],
}

/// Print Centre's own widget in a captured frame, without the window
/// chrome around it.
fn print_centre(node: &ui::headless::InspectionNode) -> &ui::headless::InspectionNode {
    fn find(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.widget == "ResourceApp" {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    find(node).expect("Print Centre in the frame")
}

fn queue(
    state: os_actions::printing::QueueState,
    accepting_jobs: bool,
) -> os_actions::printing::PrinterQueue {
    os_actions::printing::PrinterQueue {
        name: "Studio".into(),
        state,
        default: false,
        accepting_jobs,
        reason: None,
        device_uri: None,
        telemetry: os_actions::printing::PrinterTelemetry::default(),
    }
}

fn row_id(surface: &ui::serde_json::Value, kind: &str) -> String {
    surface
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["resource_kind"] == kind))
        .and_then(|row| row["resource"].as_str())
        .unwrap_or_else(|| panic!("no {kind} row in {surface}"))
        .to_owned()
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn scenarios_and_resources_expose_real_navigation_and_backend_states() {
    eprintln!("phase every_scenario_captures_at_its_viewport");
    {
        for scenario in PrintCentreShell::scenarios()
            .iter()
            .map(|name| Some(*name))
            .chain([None])
        {
            let mut app = TestApp::<PrintCentreShell>::capture(scenario)
                .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
            let (width, height) = PrintCentreShell::shot_viewport(scenario);
            assert_eq!(app.harness().inspection().viewport, [width, height]);
        }
        let mut queue = TestApp::<PrintCentreShell>::capture(Some("queue")).unwrap();
        let frame = queue.harness().inspection_json().unwrap();
        assert!(frame.contains("Travel itinerary"), "{frame}");
        let mut empty = TestApp::<PrintCentreShell>::capture(Some("empty")).unwrap();
        let frame = empty.harness().inspection_json().unwrap();
        assert!(frame.contains("Set up your first printer"), "{frame}");
    }

    eprintln!("phase published_tree_exposes_real_navigation_and_device_operations");
    {
        let mut app = live_with_studio();
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        let surface = app.agent_surface();
        let actions = surface
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource_kind"] == "print-centre.defaults")
            .filter_map(|row| row["action"].as_str())
            .collect::<Vec<_>>();
        for expected in [
            "refresh",
            "open-queue",
            "open-history",
            "run-diagnostics",
            "find-printers",
            "print-document",
            "scan-document",
            "field-edit",
        ] {
            assert!(
                actions.contains(&expected),
                "missing {expected}: {actions:?}"
            );
        }
        let printer = row_id(&surface, "print-centre.printer");
        let job = row_id(&surface, "print-centre.job");
        let scanner = row_id(&surface, "print-centre.scanner");

        // A page opens through the effect a control sends, a printer
        // through Print Centre's own view naming it; its row offers what
        // can be done to it, not a view of the window.
        app.effect(&PrintCentreEffect::OpenHistory).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Shipping labels"), "{frame}");
        assert!(
            app.call_on(&printer, "open-printer", ui::serde_json::json!({}))
                .is_err(),
            "the printer's row carries no view of the window"
        );
        app.effect(&PrintCentreEffect::OpenPrinter {
            printer: "Studio_printer".into(),
        })
        .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("print-centre.device"), "{frame}");
        app.call_on(&scanner, "scan-with", ui::serde_json::json!({}))
            .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("print-centre.scan.resolution"), "{frame}");
        assert!(
            app.agent_surface()
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == job.as_str() && row["action"] == "cancel-document"),
            "a printing document can be cancelled"
        );

        // Cancelling from the dashboard asks first, in the queue.
        app.effect(&PrintCentreEffect::ConfirmCancel {
            job: "Studio_printer-42".into(),
        })
        .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Confirm cancel"), "{frame}");

        // Steps with nothing to act on are refused: no search found a
        // printer to add, no page was scanned, no document is set up.
        assert!(
            app.effect(&PrintCentreEffect::AddPrinter {
                printer: "ipps://studio.local/ipp/print".into()
            })
            .is_err()
        );
        assert!(app.effect(&PrintCentreEffect::RotateScanPage).is_err());
        assert!(app.effect(&PrintCentreEffect::SendToPrinter).is_err());

        // A printer the snapshot does not list is refused, not guessed.
        assert!(
            app.effect(&PrintCentreEffect::OpenPrinter {
                printer: "Elsewhere".into()
            })
            .is_err()
        );
    }

    eprintln!("phase scan_review_steps_are_offered_only_with_a_page_to_act_on");
    {
        let no_page = |effect| EffectStatus::unavailable(effect, "no-page", "Scan a page first.");
        let no_scanner = EffectStatus::unavailable(
            PrintCentreEffect::SCAN_PAGE,
            "no-scanner",
            "Open scanning with a scanner first.",
        );
        let blocked =
            |centre: &PrintCentre, status: &EffectStatus| centre.effect_statuses().contains(status);
        let mut centre = PrintCentre::new();
        centre.show(Some(&demo_snapshot()));
        centre.open_scan();
        assert!(!blocked(&centre, &no_scanner));
        assert!(blocked(
            &centre,
            &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
        ));
        centre.scan_finished(Ok(demo_scanned_page()));
        assert!(!blocked(
            &centre,
            &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
        ));
        assert!(!blocked(
            &centre,
            &no_page(PrintCentreEffect::COPY_RECOGNIZED_TEXT)
        ));
        assert!(blocked(
            &centre,
            &no_page(PrintCentreEffect::NEXT_SCAN_PAGE)
        ));
        centre.rotate_scan_page();
        assert!(
            centre
                .scan
                .result
                .as_ref()
                .is_some_and(|page| page.ocr_needs_refresh)
        );
        centre.scan_another();
        assert_eq!(centre.scan.pages.len(), 1);
        assert!(blocked(
            &centre,
            &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
        ));
        centre.open_dashboard();
        assert!(blocked(&centre, &no_scanner));
    }

    eprintln!("phase scan_controls_change_the_person_s_defaults");
    {
        let mut app = live_with_studio();
        app.effect(&PrintCentreEffect::ScanDocument).unwrap();
        app.harness()
            .perform_semantic_named(
                "print-centre.scan.resolution",
                Some("print-centre.scan.resolution:segment:2"),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        app.settle();
        assert_eq!(app.state().scan_resolution, 600);
        app.harness()
            .perform_semantic_named(
                "print-centre.scan.language",
                Some("print-centre.scan.language:segment:1"),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        app.settle();
        assert_eq!(app.state().ocr_language.0, "deu");
    }

    eprintln!("phase backend_failures_remain_distinct_from_legitimate_empty_results");
    {
        let unavailable = Snapshot {
            queues: Err("cups socket closed".into()),
            jobs: Err("jobs timed out".into()),
            scanners: Err("scanner service missing".into()),
        };
        assert_eq!(
            failed_areas(&unavailable, true, true, true),
            vec!["printers", "documents", "scanners"]
        );

        let empty = Snapshot {
            queues: Ok(Vec::new()),
            jobs: Ok(Vec::new()),
            scanners: Ok(Vec::new()),
        };
        assert!(failed_areas(&empty, true, true, true).is_empty());
        let door = Door::default();
        assert!(
            views::unavailable_notice(&failed_areas(&unavailable, true, true, true), &door)
                .is_some()
        );
        assert!(
            views::unavailable_notice(&failed_areas(&empty, true, true, true), &door).is_none()
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn phone_and_large_text_layouts_keep_actions_visible_and_measured() {
    eprintln!("phase scan_review_actions_wrap_inside_phone_viewports");
    {
        fn inspect(widget: &ui::headless::InspectionNode, width: f64, buttons: &mut usize) {
            let [x, y, w, h] = widget.rect;
            let rect = Rect::new(x, y, x + w, y + h);
            if widget.widget == "Button" && rect.area() > 0.0 {
                *buttons += 1;
                assert!(rect.width() >= 44.0, "button is too narrow: {rect:?}");
                assert!(rect.height() >= 44.0, "button is too short: {rect:?}");
                assert!(rect.x0 >= -0.5, "button escaped left edge: {rect:?}");
                assert!(
                    rect.x1 <= width + 0.5,
                    "button escaped {width}px phone viewport: {rect:?}"
                );
            }
            for child in &widget.children {
                inspect(child, width, buttons);
            }
        }

        for width in [360_u32, 390] {
            let mut centre =
                TestApp::<PrintCentreShell>::capture_at(Some("scan-result"), (width, 760)).unwrap();
            centre
                .harness()
                .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
            let mut buttons = 0;
            inspect(
                print_centre(&centre.harness().inspection().root),
                f64::from(width),
                &mut buttons,
            );
            assert!(buttons >= 8, "expected the complete scan-review action set");
        }
    }

    eprintln!("phase print_setup_footer_stays_visible_and_touch_sized_on_phones");
    {
        fn collect_buttons(widget: &ui::headless::InspectionNode, buttons: &mut Vec<Rect>) {
            let [x, y, w, h] = widget.rect;
            let rect = Rect::new(x, y, x + w, y + h);
            if widget.widget == "Button" && rect.area() > 0.0 {
                buttons.push(rect);
            }
            for child in &widget.children {
                collect_buttons(child, buttons);
            }
        }

        for width in [360_u32, 520] {
            let height = 820_u32;
            let mut centre =
                TestApp::<PrintCentreShell>::capture_at(Some("print"), (width, height)).unwrap();
            centre
                .harness()
                .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
            let snapshot = centre.harness().inspection();
            let surface = print_centre(&snapshot.root);

            let mut buttons = Vec::new();
            collect_buttons(surface, &mut buttons);
            buttons.sort_by(|left, right| left.y0.total_cmp(&right.y0));
            // The document's own pager sits in its card, above the form;
            // the two answers to the sheet are the last two buttons.
            let root_bottom_band = surface.rect[1] + surface.rect[3];
            let mut footer_buttons: Vec<&Rect> = buttons
                .iter()
                .filter(|rect| rect.y1 > root_bottom_band * 0.85)
                .collect();
            footer_buttons.sort_by(|left, right| left.x0.total_cmp(&right.x0));
            assert_eq!(
                footer_buttons.len(),
                2,
                "Cancel and Print must stay visible; buttons were {buttons:?}"
            );
            let root_bottom = root_bottom_band;
            for rect in footer_buttons {
                assert!(
                    rect.width() >= 44.0,
                    "footer button is too narrow: {rect:?}"
                );
                assert!(
                    rect.height() >= 44.0,
                    "footer button is too short: {rect:?}"
                );
                assert!(
                    rect.x0 >= 0.0 && rect.x1 <= f64::from(width),
                    "button escaped viewport: {rect:?}"
                );
                assert!(
                    rect.y1 <= root_bottom,
                    "button fell below compact layout: {rect:?}"
                );
            }
        }
    }

    eprintln!("phase dashboard_cards_and_actions_stay_measured_at_large_text_scales");
    {
        fn rect_for(widget: &ui::headless::InspectionNode, id: &str) -> Option<Rect> {
            if widget.test_id.as_deref() == Some(id) {
                let [x, y, width, height] = widget.rect;
                return Some(Rect::new(x, y, x + width, y + height));
            }
            widget.children.iter().find_map(|child| rect_for(child, id))
        }

        fn collect_cards(widget: &ui::headless::InspectionNode, cards: &mut Vec<Rect>) {
            let [x, y, width, height] = widget.rect;
            let dashboard_group = widget.widget == "Card"
                || matches!(
                    widget.test_id.as_deref(),
                    Some(
                        "print-centre.scan.open"
                            | "print-centre.history.open"
                            | "print-centre.diagnostics.open"
                    )
                );
            if dashboard_group && width > 0.0 && height > 0.0 {
                cards.push(Rect::new(x, y, x + width, y + height));
            }
            for child in &widget.children {
                collect_cards(child, cards);
            }
        }

        let ids = [
            "print-centre.queue",
            "print-centre.job.cancel",
            "print-centre.device.open.Studio_printer",
            "print-centre.scan.open",
            "print-centre.history.open",
            "print-centre.diagnostics.open",
        ];
        for text_scale in [1.0, 2.0] {
            let mut centre =
                TestApp::<PrintCentreShell>::capture_at(Some("active"), (1440, 900)).unwrap();
            centre
                .harness()
                .set_theme(Theme::dark().with_text_scale(text_scale));
            centre
                .harness()
                .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
            let snapshot = centre.harness().inspection();
            let root = &snapshot.root;
            let controls = ids.map(|id| {
                let rect = rect_for(root, id).unwrap_or_else(|| panic!("missing {id}"));
                assert!(
                    rect.x0 >= -0.5 && rect.x1 <= 1440.5,
                    "{id} escaped the viewport at {text_scale}x text: {rect:?}"
                );
                assert!(
                    rect.width() >= 44.0 && rect.height() >= 44.0,
                    "{id} lost its touch target at {text_scale}x text: {rect:?}"
                );
                rect
            });
            for (index, pair) in controls[1..].windows(2).enumerate() {
                let (before, after) = (pair[0], pair[1]);
                assert!(
                    before.y1 + 16.0 <= after.y0,
                    "{} and {} overlap at {text_scale}x text: {before:?} {after:?}",
                    ids[index + 1],
                    ids[index + 2],
                );
            }

            let mut cards = Vec::new();
            collect_cards(root, &mut cards);
            assert!(
                cards.len() >= 5,
                "expected distinct active-job, printer, scan, history, and device-help groups"
            );
            for (index, card) in cards.iter().enumerate() {
                assert!(
                    card.x0 >= -0.5 && card.x1 <= 1440.5,
                    "card {index} escaped the viewport at {text_scale}x text: {card:?}"
                );
                for other in cards.iter().skip(index + 1) {
                    let overlaps = card.x0 < other.x1
                        && other.x0 < card.x1
                        && card.y0 < other.y1
                        && other.y0 < card.y1;
                    assert!(
                        !overlaps,
                        "measured dashboard cards overlap at {text_scale}x text: {card:?} {other:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn backend_telemetry_is_humanized_and_never_invented() {
    eprintln!("phase backend_states_are_translated_to_people_facing_copy");
    {
        use os_actions::printing::QueueState;
        assert_eq!(
            friendly_queue_state(&queue(QueueState::Ready, true)),
            "Ready"
        );
        assert_eq!(
            friendly_queue_state(&queue(QueueState::Ready, false)),
            "Not accepting new documents"
        );
        assert_eq!(
            friendly_queue_state(&queue(QueueState::Offline, true)),
            "Cannot reach this printer"
        );
    }

    eprintln!("phase supply_gauge_uses_the_lowest_known_cartridge");
    {
        let mut printer = queue(os_actions::printing::QueueState::Ready, true);
        printer.telemetry.supplies = Some(vec![
            os_actions::printing::PrinterSupply {
                name: "Black".into(),
                color: None,
                level_percent: Some(82),
            },
            os_actions::printing::PrinterSupply {
                name: "Cyan".into(),
                color: None,
                level_percent: Some(14),
            },
        ]);
        let (fraction, percent) = supply_level(&printer).unwrap();
        assert!((fraction - 0.14).abs() < f32::EPSILON);
        assert_eq!(percent, 14);
    }

    eprintln!("phase missing_supply_telemetry_is_not_invented");
    {
        let printer = queue(os_actions::printing::QueueState::Ready, true);
        assert_eq!(supply_level(&printer), None);
    }

    eprintln!("phase backend_identifiers_are_humanized_before_display");
    {
        assert_eq!(display_name("Studio_printer"), "Studio printer");
        assert_eq!(display_name("office--laser_2"), "office laser 2");
        assert_eq!(display_name("  Nearby_Printer  "), "Nearby Printer");
    }

    eprintln!("phase device_reasons_are_translated_without_leaking_backend_jargon");
    {
        assert_eq!(
            friendly_reason("media-empty-warning"),
            "Add paper to continue."
        );
        assert_eq!(
            friendly_reason("marker-supply-low-warning"),
            "A supply is running low; printing can usually continue for now."
        );
        assert_eq!(
            friendly_reason("vendor-specific-0x92"),
            "The printer reported a condition that may need attention."
        );
    }
}

#[test]
fn scan_output_is_opaque_byte_exact_private_and_previewable() {
    eprintln!("phase scan_result_fixture_is_opaque_and_visually_structured");
    {
        let pixels = demo_scanned_page_pixels(320, 440);
        assert_eq!(pixels.len(), 320 * 440 * 4);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel.last() == Some(&255))
        );
        let distinct = pixels
            .chunks_exact(4)
            .map(<[u8]>::to_vec)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            distinct.len() >= 4,
            "fixture must expose blank-preview regressions"
        );
    }

    eprintln!("phase scanned_pages_are_saved_byte_exactly_with_private_permissions");
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt.png");
        let bytes = b"deterministic scanned page bytes";
        save_scanned_page(&path, bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    eprintln!("phase selected_image_produces_real_bounded_preview_pixels");
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boarding-pass.png");
        let mut source =
            image::RgbaImage::from_pixel(1_200, 600, image::Rgba([245, 245, 240, 255]));
        for y in 80..180 {
            for x in 120..1_080 {
                source.put_pixel(x, y, image::Rgba([24, 92, 170, 255]));
            }
        }
        source.save(&path).unwrap();

        let preview = load_print_preview(&path, 0).expect("a selected PNG has a real preview");
        assert!(preview.width > 0 && preview.height > 0);
        assert_eq!(
            preview.width.max(preview.height),
            720,
            "the preview is drawn at the size it is kept"
        );
        assert_eq!(
            (preview.page, preview.pages),
            (0, 1),
            "an image is one page"
        );
        assert!(
            preview
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [24, 92, 170]),
            "preview pixels come from the selected document"
        );
    }
}

fn scanned_page(width: u32) -> ScannedPage {
    ScannedPage {
        png: Arc::from(Vec::<u8>::new()),
        rgba: Arc::from(vec![
            255;
            usize::try_from(width).unwrap_or(0).saturating_mul(4)
        ]),
        width,
        height: 1,
        recognized_text: None,
        ocr_note: "No text was found on this page.".into(),
        ocr_language: "English".into(),
        ocr_needs_refresh: false,
        ocr_in_progress: false,
    }
}

#[test]
fn new_scanned_document_requires_consent_and_resets_only_the_document() {
    let _serial = serial();
    let mut app = live_with_studio();
    for edit in [
        PrintDefaults::paths().scan_resolution.set(600),
        PrintDefaults::paths().ocr_language.set(Atom("deu".into())),
    ] {
        app.invoke(ui::tilo_resource::Invoke::edit(&edit).unwrap())
            .unwrap();
    }
    {
        let shared = centre(&mut app);
        let mut centre = shared.borrow_mut();
        centre.open_scan();
        centre.scan_finished(Ok(scanned_page(10)));
        centre.scan_finished(Ok(scanned_page(20)));
        centre.rotate_scan_page();
        centre.scan.crop.set(CropSelection {
            left: 0.25,
            ..CropSelection::default()
        });
    }
    app.settle();
    let (edited, scanner) = {
        let shared = centre(&mut app);
        let centre = shared.borrow();
        (
            centre.scan.pages[1].png.clone(),
            centre.selected_scanner.clone(),
        )
    };
    assert!(matches!(
        app.effect(&PrintCentreEffect::NewScannedDocument),
        Err(ui::tilo_actions::DoorError::ConfirmationRequired)
    ));
    assert_eq!(centre(&mut app).borrow().scan.pages.len(), 2);

    app.harness()
        .click_named("print-centre.scan.new-document")
        .unwrap();
    assert!(app.asking().is_some());
    app.harness().key("Escape");
    app.settle();
    assert!(app.asking().is_none());
    // Cancelling confirmation returns to the palette; dismiss it too.
    app.harness().key("Escape");
    app.settle();
    {
        let shared = centre(&mut app);
        let centre = shared.borrow();
        assert_eq!(centre.scan.pages.len(), 2);
        assert_eq!(centre.scan.selected, 1);
        assert_eq!(centre.scan.result.as_ref().unwrap().png, edited);
    }
    app.harness()
        .click_named("print-centre.scan.new-document")
        .unwrap();
    assert!(app.confirm_asked().is_some());
    {
        let shared = centre(&mut app);
        let mut centre = shared.borrow_mut();
        assert!(centre.scan.pages.is_empty());
        assert!(centre.scan.result.is_none());
        assert_eq!(centre.scan.selected, 0);
        assert_eq!(centre.scan.crop.get(), CropSelection::default());
        assert_eq!(centre.selected_scanner, scanner);
        assert_eq!(centre.scan.resolution.get(), 2);
        assert_eq!(centre.scan.language.get(), 1);
        assert!(
            !ui::attention::items()
                .iter()
                .any(|item| item.title().contains("scan")),
            "a fresh page is no failure"
        );
        assert_eq!(centre.page, Page::Scan);
        centre.scan_finished(Ok(scanned_page(30)));
        assert_eq!(centre.scan.pages.len(), 1);
        assert_eq!(centre.scan.selected, 0);
    }
}

#[test]
fn new_scanned_document_refuses_pending_scan_and_save() {
    let mut app = live_with_studio();
    {
        let shared = centre(&mut app);
        let mut centre = shared.borrow_mut();
        centre.open_scan();
        centre.scan_finished(Ok(scanned_page(10)));
        centre.scan_another();
        centre.scan.in_progress = true;
        centre.open_dashboard();
    }
    app.settle();
    assert!(
        app.effect_confirmed(&PrintCentreEffect::NewScannedDocument)
            .is_err()
    );
    {
        let shared = centre(&mut app);
        let mut centre = shared.borrow_mut();
        assert!(centre.scan.in_progress);
        assert_eq!(centre.scan.pages.len(), 1);
        centre.scan_finished(Ok(scanned_page(20)));
        centre.scan_saving = true;
    }
    app.settle();
    assert!(
        app.effect_confirmed(&PrintCentreEffect::NewScannedDocument)
            .is_err()
    );
    {
        let shared = centre(&mut app);
        let mut centre = shared.borrow_mut();
        assert_eq!(centre.scan.pages.len(), 2);
        assert!(centre.scan_saving);
        centre.scan_saving = false;
    }
    app.settle();
    app.effect_confirmed(&PrintCentreEffect::NewScannedDocument)
        .unwrap();
    assert!(centre(&mut app).borrow().scan.pages.is_empty());
}

#[test]
fn returning_to_scanning_preserves_unsaved_edits_and_pending_work() {
    let mut centre = PrintCentre::new();
    centre.show(Some(&demo_snapshot()));
    centre.open_scan();
    centre.scan_finished(Ok(scanned_page(10)));
    centre.scan_finished(Ok(scanned_page(20)));
    centre.rotate_scan_page();
    let edited = centre.scan.pages[1].png.clone();
    let scanner = centre.selected_scanner.clone();

    let navigations: [fn(&mut PrintCentre); 7] = [
        PrintCentre::open_dashboard,
        PrintCentre::open_queue,
        PrintCentre::open_history,
        PrintCentre::open_diagnostics,
        PrintCentre::open_discovery,
        |centre| centre.open_device("Studio_printer"),
        PrintCentre::close_print_setup,
    ];
    for navigate in navigations {
        navigate(&mut centre);
        centre.open_scan();
        assert_eq!(centre.scan.pages.len(), 2);
        assert_eq!(centre.scan.selected, 1);
        assert_eq!(centre.scan.result.as_ref().unwrap().png, edited);
        assert_eq!(centre.selected_scanner, scanner);
    }

    centre.scan_another();
    centre.scan.in_progress = true;
    for navigate in navigations {
        navigate(&mut centre);
        centre.open_scan();
        assert!(
            centre.scan.in_progress,
            "navigation must not allow a duplicate scan"
        );
        assert!(centre.scan.result.is_none());
    }
    centre.scan_finished(Ok(scanned_page(30)));
    assert_eq!(centre.scan.pages.len(), 3);
    assert_eq!(centre.scan.pages[1].png, edited);
    assert_eq!(centre.scan.selected, 2);
}

#[test]
fn scan_page_edits_invalidate_ocr_and_export_in_order() {
    eprintln!("phase multipage_scan_session_preserves_reorders_navigates_and_deletes_pages");
    {
        let mut centre = PrintCentre::new();
        centre.show(Some(&demo_snapshot()));
        centre.open_scan();
        centre.scan_finished(Ok(scanned_page(10)));
        centre.scan_another();
        assert_eq!(
            centre.scan.pages.len(),
            1,
            "scan another preserves page one"
        );
        centre.scan_finished(Ok(scanned_page(20)));
        assert_eq!(centre.scan.selected, 1);

        centre.move_scan_page_earlier();
        assert_eq!(centre.scan.selected, 0);
        assert_eq!(centre.scan.pages[0].width, 20);
        centre.next_scan_page();
        assert_eq!(centre.scan.result.as_ref().map(|page| page.width), Some(10));

        centre.remove_scan_page();
        assert_eq!(centre.scan.pages.len(), 1);
        assert_eq!(centre.scan.selected, 0);
        assert_eq!(centre.scan.result.as_ref().map(|page| page.width), Some(20));
    }

    eprintln!("phase rotation_updates_saved_pixels_and_invalidates_stale_ocr");
    {
        let mut page = scanned_page(3);
        page.recognized_text = Some("stale orientation".into());
        page.rotate_clockwise().unwrap();
        assert_eq!((page.width, page.height), (1, 3));
        assert!(!page.png.is_empty());
        let decoded = image::load_from_memory(&page.png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1, 3));
        assert!(page.recognized_text.is_none());
        assert!(page.ocr_needs_refresh);
    }

    eprintln!("phase crop_updates_preview_and_export_bytes_and_invalidates_ocr");
    {
        let mut page = ScannedPage {
            png: Arc::from(Vec::<u8>::new()),
            rgba: Arc::from(vec![255; 8 * 6 * 4]),
            width: 8,
            height: 6,
            recognized_text: Some("stale text".into()),
            ocr_note: "Recognized 2 words.".into(),
            ocr_language: "English".into(),
            ocr_needs_refresh: false,
            ocr_in_progress: false,
        };
        page.crop_to(CropSelection {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 0.5,
        })
        .unwrap();
        assert_eq!((page.width, page.height), (4, 3));
        assert_eq!(page.rgba.len(), 4 * 3 * 4);
        assert!(image::load_from_memory(&page.png).is_ok());
        assert!(page.recognized_text.is_none());
        assert!(page.ocr_needs_refresh);
    }

    eprintln!("phase whole_document_export_keeps_page_order_and_ocr_layer");
    {
        let mut first = scanned_page(3);
        first.recognized_text = Some("FIRST PAGE".into());
        let mut second = scanned_page(5);
        second.recognized_text = Some("SECOND PAGE".into());
        let bytes = scanned_document_pdf(&[first, second]);
        let pdf = String::from_utf8_lossy(&bytes);
        assert!(pdf.starts_with("%PDF-1.7"));
        assert!(pdf.contains("/Count 2"));
        assert!(pdf.find("FIRST PAGE") < pdf.find("SECOND PAGE"));
        assert!(pdf.contains("3 Tr"));
    }
}

/// The print form offers the options the chosen printer reports, and
/// refuses a value it does not: a field the driver cannot honour is never
/// drawn, and a choice it never listed is never sent.
#[test]
fn the_form_offers_what_the_driver_reports_and_refuses_what_it_does_not() {
    // The form's own state only: a print sheet here, not a window, so this
    // test shares nothing with the ones that page a person for attention.
    let mut centre = PrintCentre::new();
    centre.selected_printer = Some("Studio_printer".into());
    centre.print_options = demo_printer_options();

    // A value the driver reported is taken.
    centre
        .set_print_option("page-size", "A5")
        .expect("the printer reports A5");
    assert_eq!(centre.print_choice.page_size.as_deref(), Some("A5"));
    centre
        .set_print_option("two-sided", "DuplexTumble")
        .expect("the printer reports short-edge duplex");
    assert_eq!(centre.print_choice.duplex.as_deref(), Some("DuplexTumble"));

    // A value it did not report is refused, and nothing changes.
    assert!(centre.set_print_option("page-size", "Tabloid").is_err());
    assert_eq!(centre.print_choice.page_size.as_deref(), Some("A5"));
    assert!(centre.set_print_option("staple", "yes").is_err());

    // A printer that reports no duplex mode draws no duplex choice: the
    // plain both-sides toggle stands in its place.
    centre.print_options = os_actions::printing::PrinterOptions {
        duplex_modes: Vec::new(),
        ..demo_printer_options()
    };
    let reported = centre.print_options.clone();
    centre.take_print_options(reported);
    assert_eq!(
        centre.print_choice.duplex, None,
        "a mode the new printer does not offer is dropped"
    );
}

/// A forty-page document's card pages through it, and the range typed is
/// what the job asks for — or the job does not leave the window.
#[test]
fn a_long_document_pages_and_a_range_is_checked_before_it_is_sent() {
    let mut centre = PrintCentre::new();
    centre.pending_document = Some(PathBuf::from("Travel itinerary.pdf"));
    centre.selected_printer = Some("Studio_printer".into());
    centre.preview_pages = 40;
    centre.preview_page = 0;
    centre.print_preview = demo_print_preview();

    // The pager moves one page at a time and stops at either end.
    centre.preview_page = 0;
    centre.turn_preview_page(-1);
    assert_eq!(centre.preview_page, 0, "page one has nothing before it");
    centre.preview_page = 39;
    centre.turn_preview_page(1);
    assert_eq!(centre.preview_page, 39, "page forty has nothing after it");

    // A range that `lp` cannot read is refused with its own reason.
    assert!(os_actions::printing::PageRange::parse("9-2").is_err());
    assert_eq!(
        os_actions::printing::PageRange::parse("3-5")
            .expect("a range")
            .as_str(),
        "3-5"
    );
}
