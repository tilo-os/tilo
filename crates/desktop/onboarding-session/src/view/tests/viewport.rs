//! Every screen of the ceremony lays out inside the viewport it is shown in, and the actions a
//! person must reach stay visible, aligned and clear of what scrolls, at the laptop sizes, text
//! scales and contrasts the page is shown with.

use super::*;

#[test]
fn every_codecs_visual_state_builds_a_layout() {
    let (mut view, _) = view();
    for state in [
        OnboardingVisualState::Codecs,
        OnboardingVisualState::CodecsInstalling,
        OnboardingVisualState::CodecsWaitingForNetwork,
        OnboardingVisualState::CodecsInstalled,
        OnboardingVisualState::CodecsFailed,
    ] {
        view.set_visual_state_for_snapshot(state);
        assert!(view.codec_page_visible());
        layout(&mut view);
    }
}

tilo_test_env::scenario! {
    fn every_screen_lays_out_inside_the_viewport;

    phase every_screen_builds_a_layout;
    // Every screen must lay out — a ceremony that panics on the screen after the one under test is worse than no ceremony.
    {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        for step in [
            None,
            Some(KEY_BEGIN),
            Some(KEY_UNDERSTOOD),
            Some(KEY_CREATE),
            Some(KEY_BACKUP_SKIP_ACKNOWLEDGED),
            Some(KEY_BACKUP_CONTINUE),
        ] {
            if let Some(k) = step {
                press(&v, &mut doc, k);
                v.sync(&doc, &automerge::ROOT);
            }
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
        }

        for keys in [
            &[KEY_EXISTING][..],
            &[KEY_EXISTING, KEY_CHOOSE_NEARBY],
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
        ] {
            let (mut connect, _) = view();
            let mut connect_doc = AutoCommit::new();
            for key in keys {
                press(&connect, &mut connect_doc, key);
                connect.sync(&connect_doc, &automerge::ROOT);
            }
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = connect.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }

    phase narrow_layout_keeps_the_setup_surface_inside_the_viewport;
    {
        let (mut view, _) = view();
        let theme = Theme::dark();
        lay_out_as_root(&mut view, 420.0, 640.0, &theme);
        let panel = panel_rect(&mut view);
        assert!(panel.x0 >= 0.0 && panel.x1 <= 420.0, "panel={panel:?}");
        assert!(panel.y0 >= 0.0 && panel.y1 <= 640.0, "panel={panel:?}");
    }

    phase introduction_uses_a_compact_panel_on_a_large_display;
    {
        let (mut view, _) = view();
        let theme = Theme::dark();
        lay_out_as_root(&mut view, 1600.0, 900.0, &theme);
        let panel = panel_rect(&mut view);
        assert!(
            panel.height() <= f64::from(INTRO_PANEL_HEIGHT) + 0.5,
            "intro should not inherit the taller recovery panel: {panel:?}"
        );
        assert!(
            panel.height() >= 300.0,
            "intro content must retain a readable surface: {panel:?}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the fixture crosses identity pages and error states with laptop sizes, text scaling and high contrast to check action alignment"
)]
fn host_identity_actions_stay_visible_and_aligned_at_supported_laptop_sizes() {
    let scenarios: &[(&[&str], &[&str], bool)] = &[
        (&[KEY_EXISTING], &["onboarding.back.button"], false),
        (
            &[KEY_EXISTING, KEY_CHOOSE_NEARBY],
            &["onboarding.back.button", "onboarding.connect.nearby.button"],
            false,
        ),
        (
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
            &["onboarding.back.button", "onboarding.import.button"],
            false,
        ),
        (
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
            &["onboarding.back.button", "onboarding.import.button"],
            true,
        ),
        (
            &[KEY_BEGIN],
            &["onboarding.back.button", "onboarding.create.button"],
            false,
        ),
        (
            &[KEY_BEGIN],
            &["onboarding.back.button", "onboarding.create.button"],
            true,
        ),
        (
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
            &["onboarding.backup.continue.button"],
            false,
        ),
        (
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
            ],
            &["onboarding.finish.button"],
            false,
        ),
    ];

    for (width, height, theme) in vec![
        (1600.0, 900.0, Theme::dark()),
        (1366.0, 768.0, Theme::dark()),
        (1280.0, 720.0, Theme::dark()),
        (800.0, 600.0, Theme::dark()),
        (1280.0, 720.0, Theme::dark().with_text_scale(1.3)),
        (1280.0, 720.0, Theme::high_contrast().with_ui_scale(1.25)),
    ]
    .into_boxed_slice()
    {
        for &(keys, action_ids, error_state) in scenarios {
            let mut view = OnboardingView::with_minter_for_host_desktop(Box::new(|| {
                Ok(MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }));
            let mut document = AutoCommit::new();
            for key in keys {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            if error_state {
                view.error = Some("A deterministic error for responsive layout coverage".into());
            }

            let LaidOut {
                nodes: node_map, ..
            } = lay_out_as_root(&mut view, width, height, &theme);
            let panel = panel_rect(&mut view);
            assert!(
                panel.x0 >= f64::from(VIEWPORT_GUTTER) - 0.5
                    && panel.x1 <= f64::from(width - VIEWPORT_GUTTER) + 0.5
                    && panel.y0 >= f64::from(VIEWPORT_GUTTER) - 0.5
                    && panel.y1 <= f64::from(height - VIEWPORT_GUTTER) + 0.5,
                "setup panel must preserve its viewport gutter at {width}x{height}: {panel:?}"
            );

            let widget_rect = |id: &str| {
                node_map
                    .values()
                    .find_map(|&pointer| unsafe {
                        ((*pointer).test_id() == Some(id)).then(|| (*pointer).get_rect())
                    })
                    .unwrap_or_else(|| panic!("host identity page exposes {id}"))
            };
            let eyebrow = widget_rect("onboarding.progress.eyebrow");
            let stage = widget_rect("onboarding.progress.stage");
            let progress = node_map
                .values()
                .find_map(|&pointer| unsafe {
                    ((*pointer).name() == "ProgressBar").then(|| (*pointer).get_rect())
                })
                .expect("identity page progress bar");
            assert!(
                eyebrow.x0 >= panel.x0 + f64::from(theme.tokens.spacing.s3) - 0.5
                    && stage.x1 <= panel.x1 - f64::from(theme.tokens.spacing.s3) + 0.5,
                "setup header labels need an inset from both panel edges at {width}x{height}: panel={panel:?}, eyebrow={eyebrow:?}, stage={stage:?}"
            );
            for header in [eyebrow, stage, progress] {
                assert!(
                    header.y0 >= panel.y0 && header.y1 <= panel.y1,
                    "setup header must never be clipped at {width}x{height}: panel={panel:?}, header={header:?}, keys={keys:?}, error={error_state}"
                );
            }
            assert!(
                progress.y0 >= eyebrow.y1.max(stage.y1) + 4.0,
                "progress rule needs a deliberate gap below both labels: eyebrow={eyebrow:?}, stage={stage:?}, progress={progress:?}"
            );

            let scroll_pointer = node_map
                .values()
                .copied()
                .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
                .expect("identity page scroll viewport");
            let scroll = unsafe {
                (*scroll_pointer)
                    .as_any_mut()
                    .and_then(|widget| widget.downcast_mut::<Scrollable>())
                    .expect("Scrollable exposes geometry to the regression harness")
            };
            let content_viewport = scroll.content_viewport_rect();
            // The progress header is intentionally sticky above the body
            // viewport; only the scrolling content belongs inside this
            // clip. Geometry above already proves the header remains
            // inside the panel and separated from the progress rule.

            let backup_page = view.flow().screen() == Screen::Done
                && !view.identity_ready_page
                // The identity-mint failure variant deliberately replaces
                // the passphrase form with its recovery action.
                && node_map.values().any(|&pointer| unsafe {
                    (*pointer).test_id() == Some("onboarding.backup.passphrase")
                });
            if backup_page {
                assert!(
                    content_viewport.height() >= panel.height() * 0.45,
                    "backup body must retain a usable share of its panel: panel={panel:?}, viewport={content_viewport:?}"
                );
                let heading = widget_rect("onboarding.backup.heading");
                assert!(
                    heading.y0 >= content_viewport.y0 && heading.y1 <= content_viewport.y1,
                    "backup title must be fully visible in the shared body: heading={heading:?}, viewport={content_viewport:?}"
                );
                for control_id in [
                    "onboarding.backup.passphrase",
                    "onboarding.backup.confirmation",
                    "onboarding.backup.button",
                ] {
                    let control = widget_rect(control_id);
                    let earliest_offset = (control.y1 - content_viewport.y1).max(0.0);
                    let latest_offset =
                        (control.y0 - content_viewport.y0).min(f64::from(scroll.max_offset_y()));
                    assert!(
                        control.height() <= content_viewport.height()
                            && control.x0 >= content_viewport.x0 - 0.5
                            && control.x1 <= content_viewport.x1 + 0.5
                            && earliest_offset <= latest_offset + 0.5,
                        "backup form control must be wholly reachable inside the same viewport: id={control_id}, rect={control:?}, viewport={content_viewport:?}"
                    );
                }
                // Backup acknowledgement and progression are deliberately
                // persistent footer controls now; they must be visible in
                // the panel without scrolling, not reachable through the
                // scroll viewport above them.
                let footer_rects = [
                    widget_rect("onboarding.backup.skip-acknowledged.row"),
                    widget_rect("onboarding.backup.continue.button"),
                ];
                for control in footer_rects {
                    assert!(
                        // Footer uses the same s3 horizontal gutter as
                        // the scrolling content above it.
                        control.x0 >= panel.x0 + f64::from(theme.tokens.spacing.s3) - 0.5
                            && control.x1 <= panel.x1 - f64::from(theme.tokens.spacing.s3) + 0.5
                            && control.y0
                                >= content_viewport.y1 + f64::from(theme.tokens.spacing.s3) - 0.5
                            && control.y1 <= panel.y1 - 16.0,
                        "Backup footer control must stay persistently visible and clear of scrolling content at {width}x{height}: control={control:?}, viewport={content_viewport:?}, panel={panel:?}, theme={}",
                        theme.name,
                    );
                }
                continue;
            }

            let rects = action_ids
                .iter()
                .map(|id| widget_rect(id))
                .collect::<Vec<_>>();
            for rect in &rects {
                assert!(rect.area() > 0.0, "action={rect:?}");
                assert!(
                    rect.x0 >= 0.0
                        && rect.x1 <= f64::from(width)
                        && rect.y0 >= 0.0
                        && rect.y1 <= f64::from(height),
                    "action must remain inside {width}x{height}: {rect:?}"
                );
                assert!(
                    panel.x1 - rect.x1 >= 16.0 && panel.y1 - rect.y1 >= 16.0,
                    "sticky action needs a visible right/bottom inset: panel={panel:?}, action={rect:?}"
                );
            }
            let first_center = rects[0].center().y;
            assert!(
                rects
                    .iter()
                    .all(|rect| (rect.center().y - first_center).abs() <= 0.5),
                "footer actions must share a baseline: {rects:?}"
            );
            assert!(
                scroll.get_rect().y1 + 8.0 <= rects[0].y0,
                "the sticky footer must reserve layout space below scrolling content at {width}x{height}: scroll={:?}, actions={rects:?}, keys={keys:?}, error={error_state}, theme={}",
                scroll.get_rect(),
                theme.name,
            );

            if view.flow().screen() == Screen::Restore {
                let viewport = content_viewport;
                let max_offset = f64::from(scroll.max_offset_y());
                for field_id in [
                    "onboarding.connect.path.field",
                    "onboarding.connect.passphrase.field",
                ] {
                    let field = widget_rect(field_id);
                    let earliest_offset = (field.y1 - viewport.y1).max(0.0);
                    let latest_offset = (field.y0 - viewport.y0).min(max_offset);
                    assert!(
                        field.height() <= viewport.height()
                            && earliest_offset <= latest_offset + 0.5,
                        "required restore field must be fully reachable by scrolling at {width}x{height}: field={field:?}, viewport={viewport:?}, max_offset={max_offset}, error={error_state}"
                    );
                }
            }

            if view.flow().screen() == Screen::Done && view.identity_ready_page {
                // The ready screen says what the desktop is for and offers
                // exactly one way on: the desktop, with no tour to accept.
                // A long line of body copy may still need a couple of
                // lines scrolled at one size; that is why this asserts the
                // copy is reachable, never that it cannot need scrolling.
                assert!(
                    widget_rect("onboarding.done.heading").area() > 0.0
                        && widget_rect("onboarding.done.body").area() > 0.0,
                    "the ready screen keeps its title and line at {width}x{height}, theme={}",
                    theme.name
                );
                assert!(
                    node_map.values().any(|&ptr| unsafe {
                        (*ptr).test_id() == Some("onboarding.finish.button")
                    }) && !node_map.values().any(|&ptr| unsafe {
                        (*ptr).test_id() == Some("onboarding.tour.start.button")
                    }),
                    "the ready screen offers the desktop and no tour at {width}x{height}, theme={}",
                    theme.name
                );
            }

            if view.flow().screen() == Screen::Done && view.identity_ready_page {
                let guidance = widget_rect("onboarding.done.body");
                let max_offset = f64::from(scroll.max_offset_y());
                let earliest_offset = (guidance.y1 - content_viewport.y1).max(0.0);
                let latest_offset = (guidance.y0 - content_viewport.y0).min(max_offset);
                assert!(
                    guidance.height() <= content_viewport.height()
                        && earliest_offset <= latest_offset + 0.5,
                    "Ready-page next actions must be wholly reachable rather than clipped behind the footer at {width}x{height}: guidance={guidance:?}, viewport={content_viewport:?}, max_offset={max_offset}, theme={}",
                    theme.name,
                );
            }
        }
    }
}
