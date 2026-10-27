use super::*;
use std::path::PathBuf;
use ui::headless::Framed;

#[test]
fn a_missing_decoder_exposes_one_formats_action_and_clears_on_a_new_open() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut app = PlayerApp::new(RunMode::Capture);
        app.handle_media_event(media::MediaEvent::MissingDecoder {
            codec: "HEVC".into(),
        });
        assert!(
            !app.formats_visible(),
            "the decoder has not reported failure yet"
        );
        app.handle_media_event(media::MediaEvent::Error("decoder missing".into()));
        let mut framed = Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((960.0, 640.0)),
            theme,
        );
        assert!(
            framed
                .window_toolbar()
                .actions
                .iter()
                .any(|action| action.id == window_bar::OPEN && action.enabled),
            "another video stays reachable from the window's bar"
        );
        let root = framed.accessibility().expect("player accessibility");
        let offers: Vec<_> = root
            .children
            .iter()
            .filter(|node| node.action_target.as_deref() == Some("video-player.formats"))
            .collect();
        assert_eq!(offers.len(), 1, "one visible decoder remedy");
        assert_eq!(
            offers[0].name.as_deref(),
            Some("Play more video and audio formats")
        );
        assert!(
            framed.status_action_rect.width() > 0.0 && framed.status_action_rect.height() > 0.0
        );
        assert!(framed.effect_statuses().iter().all(|status| *status
            != EffectStatus::unavailable(
                VideoEffect::OPEN_FORMATS,
                "no_missing_decoder",
                crate::i18n::t("video-formats-unavailable")
            )));
        let index = framed
            .status_access_index()
            .expect("the formats control is addressable");
        assert!(framed.focus_synthetic_accessibility(&[index]));
        assert!(framed.status_focused);
        let tree = framed
            .accessibility()
            .expect("focused player accessibility");
        assert_eq!(
            tree.children[index].action_target.as_deref(),
            Some("video-player.formats"),
            "the formats remedy replaces the empty state's Open"
        );
        assert!(tree.children[index].state.focused);
        assert_eq!(
            framed.synthetic_accessibility_rect(&[index]),
            Some(framed.status_action_rect)
        );
        framed
            .open_named("/tilo-test-absent-video.mp4")
            .expect_err("a new failed file open");
        assert!(
            !framed.formats_visible(),
            "the prior decoder does not describe the new file"
        );
        assert!(framed.effect_statuses().iter().any(|status| *status
            == EffectStatus::unavailable(
                VideoEffect::OPEN_FORMATS,
                "no_missing_decoder",
                crate::i18n::t("video-formats-unavailable")
            )));
    }
}

/// A video dropped anywhere on the window opens like one chosen, with no
/// hover before it (Wayland may report only the drop).
#[test]
fn a_dropped_video_opens_without_a_hover() {
    let mut app = PlayerApp::new(RunMode::Capture);
    let mut doc = automerge::AutoCommit::new();
    ui::testing::Dispatch::new().doc(&mut doc).send(
        &mut app,
        &WindowEvent::DroppedFile("/tmp/Dropped.mp4".into()),
    );
    assert_eq!(app.title, "Dropped.mp4");
    assert_eq!(
        app.player.engine().unwrap().source(),
        Some(&media::Source::LocalFile(PathBuf::from("/tmp/Dropped.mp4")))
    );
}

#[test]
fn opening_sources_sets_identity_without_exposing_paths() {
    {
        // Point the mount at a scratch root so the classification is testable.
        let root = std::env::temp_dir().join(format!("tilo-vp-app-{}", std::process::id()));
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ROOT", &root);
        let ws = source_for_path(&root.join("media/a.mp4"));
        assert!(matches!(ws, media::Source::Workspace(_)));
        let local = source_for_path(Path::new("/home/u/b.mp4"));
        assert!(matches!(local, media::Source::LocalFile(_)));
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        app.open_path(&PathBuf::from("/tmp/Holiday.mp4"))
            .expect("source identity available");
        assert_eq!(app.title, "Holiday.mp4");
        assert!(app.player.is_playing(), "autoplay on open");
        assert_eq!(
            app.player.engine().unwrap().source(),
            Some(&media::Source::LocalFile(PathBuf::from("/tmp/Holiday.mp4")))
        );
    }
    {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Original.mp4"))
            .expect("initial source identity");
        let original_id = app.current_source_id.clone();
        let original_title = app.title.clone();
        let original_source = app
            .player
            .engine()
            .and_then(|engine| engine.source())
            .cloned();
        app.source_generation = u64::MAX;

        let error = app
            .open_path(Path::new("/tmp/New.mp4"))
            .expect_err("source generation must not wrap into an old resource identity");
        assert!(error.contains("identity generation exhausted"));
        assert_eq!(app.source_generation, u64::MAX);
        assert_eq!(app.current_source_id, original_id);
        assert_eq!(app.title, original_title);
        assert_eq!(
            app.player.engine().and_then(|engine| engine.source()),
            original_source.as_ref()
        );
    }
    {
        let mut app = synth_app();
        let path = Path::new("/home/alice/Private videos/launch-cut.mp4");
        app.open_path(path)
            .expect("local source opens in the synthesized engine");
        let source_id = app.current_source_id.as_deref().unwrap_or_default();
        assert!(source_id.starts_with("local-media-"));
        assert!(!source_id.contains("/home/alice"));
        assert!(!source_id.contains("launch-cut"));
    }
    {
        let mut app = PlayerApp::for_shot(Some("metadata"));
        let old_source = app.current_source_id.clone().unwrap();
        let source = media::Source::Remote {
            url: "https://demo.local/clip.mp4".into(),
        };
        app.set_current_source(&source)
            .expect("source identity available");
        assert_ne!(app.current_source_id.as_deref(), Some(old_source.as_str()));
        let track = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
        assert!(
            app.perform(VideoEffect::SelectAudioTrack {
                track: crate::actions::audio_resource_id(&old_source, &track.stream_id),
            })
            .is_err(),
            "a row of the previous open names no stream of this one"
        );
    }
}

#[test]
fn portal_open_is_single_flight_generation_fenced_and_retained() {
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
        ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
        assert_eq!(recorder.pick_requests().len(), 1);
        app.drain_portal();
        assert_eq!(app.title, "Chosen.webm");
        assert!(
            app.portal_open.is_none(),
            "channel cleared after the outcome"
        );
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        app.trigger_open(None).expect("first picker starts");
        let first_generation = app.portal_open.as_ref().map(|open| open.generation);
        assert!(
            app.trigger_open(None).is_err(),
            "a second request must not replace the first portal receiver"
        );
        assert_eq!(
            app.portal_open.as_ref().map(|open| open.generation),
            first_generation
        );

        app.open_path(Path::new("/tmp/Direct.mp4"))
            .expect("source identity available");
        assert!(
            app.portal_open.is_none(),
            "a direct open drops the stale picker receiver before it can win later"
        );
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        app.portal_generation = u64::MAX;
        let error = app
            .trigger_open(None)
            .expect_err("generation must not wrap");
        assert!(error.contains("generation exhausted"));
        assert!(app.portal_open.is_none());
        assert_eq!(app.portal_generation, u64::MAX);
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Stale.webm")));
        ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
        app.portal_generation += 1;
        app.drain_portal();
        assert_eq!(app.title, "No media");
        assert!(app.current_source_id.is_none());
        assert!(app.portal_open.is_none());
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Failed("permission denied".into()));
        ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
        app.drain_portal();
        assert_eq!(app.open_error.as_deref(), Some("permission denied"));
        assert!(app.player.latest_frame().is_none());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn markers_and_metadata_targets_stay_fresh_and_canonical() {
    {
        let mut app = synth_app();
        let path = Path::new("/tmp/review-cut.mp4");
        app.open_path(path).expect("first source identity");
        let first_source_id = app.current_source_id.clone();
        let media_ref = app.current_media_ref.clone();
        app.open_path(path).expect("reopened source identity");
        assert_ne!(app.current_source_id, first_source_id);
        assert_eq!(app.current_media_ref, media_ref);
        assert!(
            !app.current_media_ref
                .as_deref()
                .unwrap_or_default()
                .contains("review-cut"),
            "the durable reference remains opaque"
        );
    }
    {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/review-cut.mp4"))
            .expect("source identity");
        let media_ref = app.current_media_ref.clone().expect("durable media ref");
        let mut library = crate::shared::Library::default();
        library.annotations.push(
            Id::from_raw("review-marker"),
            crate::shared::TimestampAnnotation {
                media_ref,
                time_ms: 12_500,
                text: "Tighten this cut".into(),
            },
        );
        library.annotations.push(
            Id::from_raw("elsewhere"),
            crate::shared::TimestampAnnotation {
                media_ref: "another-video".into(),
                time_ms: 30_000,
                text: String::new(),
            },
        );
        app.present_library(&library);
        app.perform(VideoEffect::JumpToAnnotation {
            annotation: "review-marker".into(),
        })
        .expect("the note belongs to the open video");
        assert_eq!(app.player.position_ms(), 12_500);
        let error = app
            .perform(VideoEffect::JumpToAnnotation {
                annotation: "elsewhere".into(),
            })
            .expect_err("a note on another video is refused");
        assert!(error.contains("different video"));
        assert!(
            app.perform(VideoEffect::JumpToAnnotation {
                annotation: "gone".into(),
            })
            .is_err(),
            "a deleted note is refused"
        );
        assert_eq!(app.player.position_ms(), 12_500);
    }
    {
        let mut app = PlayerApp::for_shot(Some("metadata"));
        let audio = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
        let chapter = app.shot_metadata.as_ref().unwrap().chapters[0].clone();
        let stale_source = "https://old.example/clip.mkv";
        assert!(
            app.perform(VideoEffect::SelectAudioTrack {
                track: crate::actions::audio_resource_id(stale_source, &audio.stream_id),
            })
            .is_err()
        );
        let position = app.player.position_ms();
        assert!(
            app.perform(VideoEffect::JumpToChapter {
                chapter: crate::actions::chapter_resource_id(stale_source, &chapter.id),
            })
            .is_err()
        );
        assert_eq!(app.player.position_ms(), position, "nothing moved");
        let source_id = app.current_source_id.clone().unwrap();
        app.perform(VideoEffect::JumpToChapter {
            chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
        })
        .expect("a chapter of the open source");
        assert_eq!(
            app.player.position_ms(),
            0,
            "the first chapter starts the clip"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn library_controls_emit_canonical_host_actions_and_are_accessible() {
    {
        let mut app = PlayerApp::for_shot(Some("library"));
        let focused_id = app.queue[1].id.clone();
        app.library_row_hits = vec![
            LibraryRowHit {
                id: app.queue[0].id.clone(),
                row: Rect::ZERO,
                play: Rect::new(0.0, 0.0, 44.0, 44.0),
                remove: Some(Rect::new(44.0, 0.0, 88.0, 44.0)),
            },
            LibraryRowHit {
                id: focused_id.clone(),
                row: Rect::ZERO,
                play: Rect::new(0.0, 44.0, 44.0, 88.0),
                remove: Some(Rect::new(44.0, 44.0, 88.0, 88.0)),
            },
        ];
        assert!(Widget::focus_synthetic_accessibility(&mut app, &[4]));
        assert_eq!(app.library_focus_id.as_deref(), Some(focused_id.as_str()));
        assert_eq!(
            Widget::synthetic_accessibility_rect(&app, &[4]),
            Some(Rect::new(0.0, 44.0, 44.0, 88.0))
        );
    }
    {
        let mut app = PlayerApp::for_shot(Some("library"));
        app.library_queue_tab_rect = Rect::new(44.0, 0.0, 132.0, 44.0);
        app.library_recent_tab_rect = Rect::new(132.0, 0.0, 220.0, 44.0);
        app.library_shared_tab_rect = Rect::new(220.0, 0.0, 308.0, 44.0);

        let tree = Widget::accessibility(&app).expect("application accessibility tree");
        assert_eq!(tree.children[0].name.as_deref(), Some("Queue"));
        assert!(tree.children[0].state.selected);
        assert_eq!(tree.children[1].name.as_deref(), Some("Recent"));
        assert!(!tree.children[1].state.selected);
        assert_eq!(tree.children[2].name.as_deref(), Some("Shared"));
        assert!(!tree.children[2].state.selected);
        let mut names = Vec::new();
        for node in &tree.children {
            names.extend(node.name.clone());
        }
        assert!(
            !names
                .iter()
                .any(|name| name == "Library" || name == "Close library"),
            "the sheet has no title or close of its own: {names:?}"
        );

        assert!(Widget::focus_synthetic_accessibility(&mut app, &[1]));
        assert_eq!(app.library_focus_id.as_deref(), Some("library-tab-recent"));

        app.perform(VideoEffect::ShowLibrary {
            tab: "recent".into(),
        })
        .unwrap();
        assert_eq!(app.library_tab, LibraryTab::Recent);
        assert!(
            app.perform(VideoEffect::ShowLibrary {
                tab: "elsewhere".into()
            })
            .is_err(),
            "an unknown tab is refused"
        );
        app.perform(VideoEffect::HideLibrary).unwrap();
        assert!(!app.library_visible);
    }
    // The library filling a narrow window leads with the way back, and
    // pressing it (or its accessible node) returns to the picture.
    {
        let mut app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (420.0, 800.0),
            1.0,
        );
        Framed::harness_mut(&mut app).relayout();
        let back = app.library_back_rect;
        assert!(back.width() > 0.0, "the way back leads the library screen");
        let tree = Widget::accessibility(&*app).expect("application accessibility tree");
        let node = tree
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("video-player.library.back"))
            .expect("the way back is addressable");
        assert_eq!(node.name.as_deref(), Some("Back to video"));
        assert_eq!(node.bounds.map(ui::a11y::AccessBounds::to_rect), Some(back));
        let mut doc = automerge::AutoCommit::new();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        assert!(app.perform_semantic_action(
            "video-player.library.back",
            &ui::a11y::Action::Activate,
            &mut edits,
            &(),
        ));
        assert!(
            app.library_visible,
            "the library is one action away again, not lost"
        );
    }
    {
        let app = PlayerApp::for_shot(Some("shared"));
        assert!(
            app.handle_app_shortcut_input(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ),
            "Escape is the library's close key"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn player_resources_and_transport_guards_are_published_honestly() {
    {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        let mut app = lay_out(app, (1280.0, 720.0), 1.0);
        app.perform(VideoEffect::PictureInPicture).unwrap();
        let request =
            ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut *app)
                .expect("PiP request");
        assert_eq!(request.id, "video-player.current-video");
        assert_eq!(
            request.role,
            ui::floating_surface::FloatingRole::PictureInPicture
        );
        assert_eq!(request.aspect_ratio, Some(16.0 / 9.0));
        assert!(
            matches!(request.source, ui::floating_surface::FloatingSource::AppElement { ref app_id, ref element_id, .. } if app_id == "so.tilo.video-player" && element_id == "video_surface")
        );
    }
    {
        let app = PlayerApp::for_shot(Some("metadata"));
        let rows: Vec<_> = crate::actions::live_rows(&app.player_rows(), 32)
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect();
        let kinds: Vec<&str> = rows.iter().map(|row| row.kind.as_str()).collect();
        assert_eq!(kinds, ["media.video", "media.queue", "media.history"]);
        let media = &rows[0];
        assert_eq!(media.label, "Demo clip");
        let tracks = &media.children[0];
        assert_eq!(tracks.kind, "media.tracks");
        assert_eq!(
            tracks
                .children
                .iter()
                .map(|track| track.label.as_str())
                .collect::<Vec<_>>(),
            [
                "English stereo is selected",
                "Deutsch 5.1",
                "English captions is selected"
            ]
        );
        assert!(
            tracks.children.iter().all(|track| track
                .actions
                .iter()
                .all(|action| action.kind != ui::tilo_actions::OperationKind::Document)),
            "a live row never changes the document"
        );
        let source_id = app.current_source_id.clone().unwrap();
        let chapter = crate::actions::chapter_resource_id(&source_id, "scene");
        assert!(
            crate::actions::live_row(&app.player_rows(), &chapter).is_some(),
            "a row resolves by id"
        );
    }
    {
        let empty = PlayerApp::new(RunMode::Capture);
        assert!(empty.effect_statuses().iter().any(|status| {
            format!("{status:?}").contains("toggle-playback")
                && format!("{status:?}").contains("no_media")
        }));
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        assert!(
            !app.effect_statuses()
                .iter()
                .any(|status| { format!("{status:?}").contains("toggle-playback") })
        );
    }
}

#[test]
fn library_sheet_rows_tabs_and_scrim_answer_the_pointer() {
    use ui::framework::testing::TestApp;
    fn surface<T>(
        app: &mut TestApp<crate::resource::VideoPlayerShell>,
        f: impl FnOnce(&mut PlayerApp) -> T,
    ) -> T {
        app.with_surface::<PlayerApp, T>(f).expect("mounted player")
    }
    let mut app = TestApp::<crate::resource::VideoPlayerShell>::new().unwrap();
    surface(&mut app, |player| {
        player
            .player
            .set_engine(media::MediaEngine::new_synthesized());
        player.stage_capture(None, Some("library"));
    });
    app.harness()
        .relayout_themed(
            ui::headless::viewport((1200.0, 800.0)),
            Theme::dark().with_reduce_motion(true),
        )
        .unwrap();
    app.settle();
    let next = surface(&mut app, |player| player.queue[1].id.clone());
    let row = surface(&mut app, |player| {
        player
            .library_row_hits
            .iter()
            .find(|hit| hit.id == next)
            .cloned()
            .expect("queued row placed")
    });
    app.harness()
        .click_at(row.play.center().x, row.play.center().y);
    app.settle();
    assert_eq!(
        surface(&mut app, |player| player.current_queue_id.clone()),
        Some(next.clone()),
        "row body plays its canonical queue entry"
    );
    let remove = surface(&mut app, |player| {
        player
            .library_row_hits
            .iter()
            .find(|hit| hit.id == next)
            .and_then(|hit| hit.remove)
            .expect("queued row removal control")
    });
    app.harness().click_at(remove.center().x, remove.center().y);
    app.settle();
    assert!(
        surface(&mut app, |player| player
            .queue
            .iter()
            .all(|entry| entry.id != next)),
        "remove changes the queue without replaying the row"
    );
    let recent = surface(&mut app, |player| player.library_recent_tab_rect.center());
    app.harness().click_at(recent.x, recent.y);
    app.settle();
    assert_eq!(
        surface(&mut app, |player| player.library_tab_name()),
        "recent"
    );
    // Wide, the library is docked beside the picture: a press on the
    // picture leaves it open.
    app.harness().click_at(1160.0, 400.0);
    app.settle();
    assert!(
        surface(&mut app, |player| player.library_visible),
        "a press beside the docked library keeps it"
    );
    // Narrower, it takes the window as a screen of its own; "Back to
    // video" puts it away and the picture is one press away again.
    app.harness()
        .relayout_themed(
            ui::headless::viewport((420.0, 800.0)),
            Theme::dark().with_reduce_motion(true),
        )
        .unwrap();
    app.settle();
    assert!(
        surface(&mut app, |player| player.library_visible),
        "a library too wide to share the window stays open over it"
    );
    let back = surface(&mut app, |player| player.library_back_rect.center());
    app.harness().click_at(back.x, back.y);
    app.settle();
    assert!(
        !surface(&mut app, |player| player.library_visible),
        "the way back closes the actual hosted library"
    );
    assert!(
        !surface(&mut app, |player| {
            player.last_cursor = Point::new(400.0, 400.0);
            player.handle_app_pointer(&mut ui::widget::Edits::lend(
                &mut automerge::AutoCommit::new(),
            ))
        }),
        "the closed library no longer captures the same press"
    );
}

/// Run the production layout passes (sync → build → compute → apply) for
/// `app` at `viewport`, with the accessibility text scale applied the way
/// the live host applies it.
fn lay_out(app: PlayerApp, viewport: (f32, f32), text_scale: f32) -> Framed<PlayerApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport(viewport),
        Theme::dark().with_text_scale(text_scale),
    )
}

/// [`lay_out`] with reduced motion, so a sheet that opens is open in the
/// first frame instead of sliding in.
fn lay_out_still(app: PlayerApp, viewport: (f32, f32), text_scale: f32) -> Framed<PlayerApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport(viewport),
        Theme::dark()
            .with_text_scale(text_scale)
            .with_reduce_motion(true),
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn player_chrome_geometry_follows_the_theme_at_every_text_scale() {
    // Case `library_sheet_stacks_tabs_and_rows`: the sheet is one
    // column: the tab strip across it, then the rows, at 100% and 200%
    // text, with the actions clear of one another.
    for scale in [1.0_f32, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (1200.0, 800.0),
            scale,
        );
        let sheet = app.drawer.sidebar_rect();
        assert!(sheet.width() > 0.0, "{scale}×: the sheet is open");
        assert!(
            sheet.center().x < app.rect.center().x,
            "{scale}×: the sheet opens on the leading side, under the bar's sidebar toggle"
        );
        for rect in [
            app.library_queue_tab_rect,
            app.library_recent_tab_rect,
            app.library_shared_tab_rect,
        ] {
            assert!(
                sheet.contains_rect(rect) && rect.width() > 0.0,
                "{scale}×: {rect:?} lies inside the sheet {sheet:?}"
            );
        }
        assert!(
            (app.library_queue_tab_rect.y0 - sheet.y0 - f64::from(theme.tokens.spacing.s4)).abs()
                < 0.01,
            "{scale}×: the tabs lead the sheet, with no title above them"
        );
        assert!(
            (app.library_queue_tab_rect.x1 - app.library_recent_tab_rect.x0).abs() < 0.01
                && (app.library_recent_tab_rect.x1 - app.library_shared_tab_rect.x0).abs() < 0.01,
            "{scale}×: tabs form one contiguous strip"
        );
        assert!(
            app.library_list_rect.y0 >= app.library_queue_tab_rect.y1,
            "{scale}×: rows start under the tabs"
        );
        assert_eq!(app.library_row_hits.len(), app.queue.len().min(32));
        let mut previous_bottom = app.library_list_rect.y0 - app.library_scroll;
        for row in &app.library_row_hits {
            assert!(
                (row.row.y0 - previous_bottom).abs() < 0.01 && row.row.height() > 0.0,
                "{scale}×: rows stack with no gap or overlap"
            );
            previous_bottom = row.row.y1;
            let remove = row.remove.expect("a queued row can be removed");
            assert!(
                row.row.contains_rect(remove),
                "{scale}×: the remove action sits inside its row"
            );
        }
    }
    // Case `library_never_covers_the_transport`: wide, the library docks
    // at its own width beside the picture and the player lays out beside
    // it; narrower than its own width plus the picture's, it takes the
    // content area as a screen of its own and leads with the way back.
    {
        let mut app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (1200.0, 800.0),
            1.0,
        );
        Framed::harness_mut(&mut app).relayout();
        let sheet = app.drawer.sidebar_rect();
        assert!(app.drawer.is_docked(), "wide, the library docks");
        assert!(
            app.player.get_rect().x0 >= sheet.x1 - 0.5,
            "the picture {:?} stands beside the library {sheet:?}",
            app.player.get_rect()
        );
        let mut app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (420.0, 800.0),
            1.0,
        );
        Framed::harness_mut(&mut app).relayout();
        let sheet = app.drawer.sidebar_rect();
        assert!(
            app.drawer.is_push(),
            "a picture too narrow to keep its library shows the library whole"
        );
        assert_eq!(sheet, app.rect, "the library fills the content area");
        let back = app.library_back_rect;
        assert!(
            back.width() > 0.0 && sheet.contains_rect(back),
            "the way back to the picture {back:?} leads the library {sheet:?}"
        );
        assert!(
            back.y1 <= app.library_queue_tab_rect.y0,
            "it stands above the tabs it shares the sheet with"
        );
        let mut app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (420.0, 800.0),
            1.0,
        );
        Framed::harness_mut(&mut app).relayout();
        app.perform(VideoEffect::HideLibrary).unwrap();
        Framed::harness_mut(&mut app).relayout();
        assert!(
            !app.library_visible,
            "the same toggle puts the picture back"
        );
        assert_eq!(app.library_back_rect, Rect::ZERO);
    }
    // Case `library_sheet_follows_its_drawer`: closing the library moves
    // no hit rect into the video.
    {
        let mut app = lay_out_still(
            PlayerApp::for_shot_open(None, Some("library")),
            (1200.0, 800.0),
            1.0,
        );
        app.perform(VideoEffect::HideLibrary).unwrap();
        let theme = Theme::dark().with_reduce_motion(true);
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        app.sync_library_sheet();
        app.drawer.tick(&theme);
        app.paint_library_panel(&mut scene, Affine::IDENTITY, &mut text, &theme);
        assert_eq!(app.library_queue_tab_rect, Rect::ZERO);
        assert!(app.library_row_hits.is_empty());
    }
    {
        let mut app = PlayerApp::new(RunMode::Capture);
        let doc = automerge::AutoCommit::new();
        // Pumping the app before any file is opened must not fabricate a frame.
        app.sync(&doc, &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(
            app.player.latest_frame().is_none(),
            "empty state: no picture until a file is opened"
        );
        assert_eq!(app.title, "No media");
        assert!(!app.player.is_playing());
    }
}

/// A player pinned to the deterministic synthesized backend, so the app's
/// open/transport wiring is exercised hermetically regardless of build
/// features (real `GStreamer` decode is proven separately by the
/// `--features decode` integration test with a real generated file).
fn synth_app() -> PlayerApp {
    let mut app = PlayerApp::new(RunMode::Capture);
    app.player.set_engine(media::MediaEngine::new_synthesized());
    app
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn decoder_lifecycle_confirms_opens_only_on_a_real_frame() {
    {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
            .expect("source identity available");
        let doc = automerge::AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        assert!(
            app.player.latest_frame().is_some(),
            "a frame is produced once opened"
        );
        assert!(
            app.player.duration_ms() > 0,
            "duration mirrored from the engine"
        );
        assert!(app.player.is_playing(), "autoplay after open");
    }
}

#[test]
fn capture_waits_for_the_requested_file_in_the_shared_visual_lifecycle() {
    let requested = "/a-missing-tilo-capture-fixture.mp4";
    let app = PlayerApp::for_shot_open(Some(requested), None);
    assert!(
        Widget::has_pending_visual_work(&app),
        "a real-file capture cannot settle without its actual decoded picture"
    );
    assert!(
        !app.has_media_frame(),
        "no synthesized demo substitutes for the file"
    );
    let loading = PlayerApp::for_shot_open(None, Some("loading"));
    assert!(
        !Widget::has_pending_visual_work(&loading),
        "an explicitly requested loading capture need not await decoding"
    );
}

#[test]
fn capture_scenarios_stage_the_two_failure_states() {
    // A failed open leaves nothing open; a decoder that gives up on an
    // opened video keeps its title. Both tell the person the same thing.
    let failed_open = PlayerApp::for_shot_open(None, Some("error"));
    assert_eq!(failed_open.open_error.as_deref(), Some(DECODER_FAILURE));
    assert!(!failed_open.player.has_media());
    let gave_up = PlayerApp::for_shot_open(None, Some("native-error"));
    assert_eq!(gave_up.open_error.as_deref(), Some(DECODER_FAILURE));
    assert!(gave_up.player.has_media());
    assert_eq!(gave_up.title, "Demo clip");
    assert!(
        gave_up.shot_loading,
        "the picture is held back for the failure"
    );
}

#[test]
fn transport_settings_reach_the_engine_and_snapshot() {
    {
        let mut app = synth_app();
        app.preferences.autoplay = false;
        configure_player_settings(&mut app.player, app.preferences);
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        assert!(
            !app.player.is_playing(),
            "disabled autoplay is honoured on open"
        );

        let doc = automerge::AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        app.preferences.loop_enabled = true;
        configure_player_settings(&mut app.player, app.preferences);
        let end = app.player.duration_ms();
        app.player.seek_to_ms(end);
        app.player.pause();
        app.maybe_loop_current_media();
        assert_eq!(app.player.position_ms(), 0, "repeat restarts at the end");
        assert!(app.player.is_playing(), "repeat resumes playback");
    }
    {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
            .expect("source identity available");
        let doc = automerge::AutoCommit::new();
        app.sync(&doc, &automerge::ROOT); // baseline the synth clock
        tilo_test_env::wait_until("the playhead advances while playing", || {
            app.sync(&doc, &automerge::ROOT);
            app.player.position_ms() > 0
        });

        // Seek mid-play: the target lands in both the widget cache and the engine.
        app.player.seek_to_ms(50_000);
        assert_eq!(app.player.position_ms(), 50_000);
        assert_eq!(app.player.engine().unwrap().position_ms(), 50_000);

        // Playback continues forward from the seek target, not back to the start.
        tilo_test_env::wait_until("playback rides on from the seek", || {
            app.sync(&doc, &automerge::ROOT);
            let position = app.player.position_ms();
            assert!(position >= 50_000, "playback went back to {position} ms");
            position > 50_000
        });
    }
    {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
            .expect("source identity available");
        app.player.set_volume(0.3);
        assert!((app.player.volume() - 0.3).abs() < 1e-6);
        assert!((app.player.engine().unwrap().volume() - 0.3).abs() < 1e-6);
        app.player.toggle_mute();
        assert!(
            (app.player.engine().unwrap().volume() - 0.0).abs() < 1e-6,
            "mute drops the engine volume to zero"
        );
    }
    {
        let mut app = synth_app();
        app.open_path(Path::new("/tmp/Clip.mp4"))
            .expect("source identity available");
        app.perform(VideoEffect::SetPlaybackRate { rate: 2.0 })
            .unwrap();
        let engine = app.player.engine().expect("engine");
        assert!((engine.rate() - 2.0).abs() < f32::EPSILON);
        assert!((engine.transport_state(unix_now_ms()).rate - 2.0).abs() < f32::EPSILON);
    }
    {
        let mut app = synth_app();
        app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
            .expect("source identity available");
        app.player.set_focus(true);
        let was_playing = app.player.is_playing();
        assert!(app.route_key_input(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space),
            ElementState::Pressed,
            winit::event::Modifiers::default(),
        ));
        assert_ne!(
            app.player.is_playing(),
            was_playing,
            "Space reached the focused player through the app"
        );
    }
}

fn transport(playing: bool, position_ms: u64, epoch_ms: u64) -> media::TransportState {
    media::TransportState {
        playing,
        position_ms,
        rate: 1.0,
        epoch_ms,
    }
}

fn shared(state: media::TransportState) -> crate::shared::SharedTransport {
    crate::shared::SharedTransport {
        playing: state.playing,
        position_ms: state.position_ms,
        rate: f64::from(state.rate),
        epoch_ms: state.epoch_ms,
    }
}

#[test]
fn watching_together_follows_moves_and_says_its_own_once() {
    let start = 1_000_000;
    // A player that starts playing says so; its own change coming back
    // is not followed.
    let mut here = Together::default();
    assert_eq!(
        here.step("clip", None, transport(false, 0, start), start, 60_000),
        TogetherStep::Stay,
        "a paused player that nobody else watches says nothing"
    );
    let playing = transport(true, 0, start + 10);
    assert_eq!(
        here.step("clip", None, playing, start + 10, 60_000),
        TogetherStep::Publish(playing)
    );
    assert_eq!(
        here.step("clip", Some(&shared(playing)), playing, start + 20, 60_000),
        TogetherStep::Stay,
        "its own change is not followed back"
    );
    // Its engine still starting: the playhead has not moved yet. A
    // leader never seeks itself toward its own announcement.
    assert_eq!(
        here.step(
            "clip",
            Some(&shared(playing)),
            transport(true, 0, start + 900),
            start + 900,
            60_000
        ),
        TogetherStep::Stay,
        "a leader does not ride along with itself"
    );

    // Another player joins the same video and follows; while its engine
    // catches up it says nothing of its own.
    let mut there = Together::default();
    let local = transport(false, 0, start + 500);
    assert_eq!(
        there.step("clip", Some(&shared(playing)), local, start + 500, 60_000),
        TogetherStep::Follow(playing)
    );
    assert_eq!(
        there.step("clip", Some(&shared(playing)), local, start + 900, 60_000),
        TogetherStep::Follow(playing),
        "settling: a lagging engine is not a move"
    );
    // Small drift rides; a seek far away is a move everyone follows.
    let riding = transport(true, 2_100, start + 2_000);
    assert_eq!(
        there.step(
            "clip",
            Some(&shared(playing)),
            riding,
            start + 2_000,
            60_000
        ),
        TogetherStep::Follow(playing)
    );
    let sought = transport(true, 30_000, start + 2_100);
    assert_eq!(
        there.step(
            "clip",
            Some(&shared(playing)),
            sought,
            start + 2_100,
            60_000
        ),
        TogetherStep::Publish(sought)
    );
    assert_eq!(
        here.step(
            "clip",
            Some(&shared(sought)),
            playing,
            start + 2_200,
            60_000
        ),
        TogetherStep::Follow(sought),
        "the first player follows the seek"
    );
}

#[test]
fn a_video_watched_long_ago_starts_on_its_own() {
    let now = 50_000_000;
    let long_ago = shared(transport(false, 12_000, now - TOGETHER_PAUSE_LIVES_MS - 1));
    let mut here = Together::default();
    assert_eq!(
        here.step(
            "clip",
            Some(&long_ago),
            transport(false, 0, now),
            now,
            60_000
        ),
        TogetherStep::Stay,
        "an old pause is not joined"
    );
    let finished = shared(transport(true, 0, now - 120_000));
    let mut there = Together::default();
    assert_eq!(
        there.step(
            "clip",
            Some(&finished),
            transport(false, 0, now),
            now,
            60_000
        ),
        TogetherStep::Stay,
        "a playback that ran past the end is over"
    );
}

#[test]
fn a_playback_put_back_after_the_end_does_not_swallow_a_replay() {
    // This player played the clip to its end and said so. The room then
    // put back the older "playing from the start" (it undid the change),
    // which has long run past the end. Seeking back and replaying must
    // stay with the person, not snap to the end again.
    let start = 50_000_000;
    let playing = transport(true, 0, start);
    let mut here = Together::default();
    assert_eq!(
        here.step("clip", None, playing, start, 6_000),
        TogetherStep::Publish(playing)
    );
    assert_eq!(
        here.step("clip", Some(&shared(playing)), playing, start + 50, 6_000),
        TogetherStep::Stay,
        "its own play came back"
    );
    let now = start + 7_000;
    let ended = transport(false, 6_000, now);
    assert_eq!(
        here.step("clip", Some(&shared(playing)), ended, now, 6_000),
        TogetherStep::Publish(ended)
    );
    assert_eq!(
        here.step("clip", Some(&shared(ended)), ended, now + 50, 6_000),
        TogetherStep::Stay,
        "its own pause came back"
    );
    assert_eq!(
        here.step("clip", Some(&shared(playing)), ended, now + 100, 6_000),
        TogetherStep::Stay,
        "an old playback past the end is not followed"
    );
    let replay = transport(true, 0, now + 200);
    assert_eq!(
        here.step("clip", Some(&shared(playing)), replay, now + 200, 6_000),
        TogetherStep::Publish(replay),
        "the replay is this player's move"
    );
}

#[test]
fn a_video_that_cannot_be_decoded_leaves_the_queue() {
    let mut app = PlayerApp::for_shot(None);
    let good = app.ensure_queued(
        source_for_path(Path::new("/videos/good.mp4")),
        "good.mp4".into(),
    );
    let broken = app.ensure_queued(
        source_for_path(Path::new("/videos/broken.mp4")),
        "broken.mp4".into(),
    );
    app.current_queue_id = Some(broken.clone());
    app.handle_media_event(media::MediaEvent::Error("decode failed".into()));
    assert!(app.open_error.is_some(), "the person is told it can't play");
    let ids: Vec<_> = app.queue.iter().map(|entry| entry.id.clone()).collect();
    assert!(ids.contains(&good), "the good video stays");
    assert!(
        !ids.contains(&broken),
        "the broken one is not kept beside it"
    );
}

#[test]
fn the_current_row_says_whether_the_video_plays_or_is_paused() {
    let mut app = synth_app();
    app.open_path(Path::new("/tmp/Clip.mp4"))
        .expect("source identity available");
    let current_label = |app: &PlayerApp| {
        crate::actions::live_rows(&app.player_rows(), 32)
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .find(|row| row.kind == "media.queue")
            .and_then(|queue| {
                queue
                    .children
                    .into_iter()
                    .find(|row| row.label.starts_with("Clip.mp4"))
            })
            .map(|row| row.label)
    };
    app.player.pause();
    assert_eq!(app.current_row_state(), "Paused");
    assert_eq!(current_label(&app).as_deref(), Some("Clip.mp4 is paused"));
    app.player.play();
    assert_eq!(app.current_row_state(), "Playing");
    assert_eq!(current_label(&app).as_deref(), Some("Clip.mp4 is playing"));
}

#[test]
fn revealing_the_controls_never_changes_transport() {
    let mut app = PlayerApp::for_shot(None);
    app.player.pause();
    let position = app.player.position_ms();
    app.perform(VideoEffect::Reveal).unwrap();
    assert!(!app.player.is_playing());
    assert_eq!(app.player.position_ms(), position);
}

/// A recent row for `path`, as this device would have saved it.
fn recent_row(path: &str, position_ms: u64) -> (RecentMedia, String) {
    let source = source_for_path(Path::new(path));
    let id = format!("local-queue-{}", resume_key(&source));
    (
        RecentMedia {
            id: id.clone(),
            title: source_title(&source),
            source,
            position_ms,
        },
        id,
    )
}

#[test]
fn the_volume_and_speed_this_device_chose_are_kept_and_restored() {
    use ui::framework::testing::TestApp;

    // A live window keeps this device's settings on its own profile.
    let mut app = TestApp::<crate::resource::VideoPlayerShell>::new()
        .expect("a live window opens on a private profile");
    let stored = |app: &mut TestApp<crate::resource::VideoPlayerShell>| {
        app.with_surface::<PlayerApp, _>(|player| player.stored_settings())
            .flatten()
    };
    let fresh = stored(&mut app).expect("the window keeps its settings");
    assert!((fresh.volume - crate::preferences::DEFAULT_VOLUME).abs() < f32::EPSILON);
    assert!((fresh.rate - crate::preferences::DEFAULT_RATE).abs() < f32::EPSILON);

    // The person drags the volume to a third and picks 1.5×. (The
    // production path, past the effect door's open-video guard.)
    app.with_surface::<PlayerApp, _>(|player| {
        player
            .perform(VideoEffect::SetVolume { volume: 0.35 })
            .expect("the volume follows");
        player
            .perform(VideoEffect::SetPlaybackRate { rate: 1.5 })
            .expect("the speed follows");
    })
    .expect("the window shows the player");
    assert!(
        (stored(&mut app).expect("settings").rate - 1.5).abs() < f32::EPSILON,
        "a chosen speed is kept at once"
    );

    // The volume is kept once the drag settles, and the value it came to
    // rest at is the one kept.
    tilo_test_env::wait_until("the chosen volume settles into the settings", || {
        app.settle();
        stored(&mut app).is_some_and(|stored| (stored.volume - 0.35).abs() < 1e-6)
    });

    // The next launch's player follows what this device kept.
    let kept = stored(&mut app).expect("settings");
    let mut restarted = synth_app();
    restarted.adopt_settings(&kept);
    assert!(
        (restarted.player.volume() - 0.35).abs() < 1e-6,
        "the volume it was left at plays: {}",
        restarted.player.volume()
    );
    let engine = restarted.player.engine().expect("an engine");
    assert!(
        (engine.rate() - 1.5).abs() < f32::EPSILON,
        "1.5× is restored: {}",
        engine.rate()
    );
    let speed = restarted
        .player
        .settings()
        .iter()
        .find(|setting| setting.id == "playback-rate")
        .expect("the speed is in the settings menu");
    assert_eq!(
        speed.control,
        ui::widget::video_player::PlayerSettingControl::Choice {
            options: PLAYBACK_RATES
                .iter()
                .map(|rate| format!("{rate}×"))
                .collect(),
            selected: 5,
        },
        "the menu shows the restored speed, so it is not lying"
    );

    // Closing the video builds a fresh player; it plays at the same
    // levels, so a clip opened next is not suddenly loud again.
    restarted.perform(VideoEffect::Close).unwrap();
    assert!((restarted.player.volume() - 0.35).abs() < 1e-6);
    assert!((restarted.player.engine().expect("an engine").rate() - 1.5).abs() < f32::EPSILON);
}

#[test]
fn reopening_a_video_resumes_where_it_was_left_and_does_not_autoplay() {
    let (row, id) = recent_row("/videos/Lecture.mp4", 30_000);
    let mut app = synth_app();
    app.adopt_settings(&PlayerSettings {
        autoplay: true,
        recent_history: vec![row],
        ..PlayerSettings::default()
    });
    app.open_local_entry(&id)
        .expect("the recent row opens its video");
    assert!(
        !app.player.is_playing(),
        "a video resumed mid-way does not start playing on its own"
    );
    let doc = automerge::AutoCommit::new();
    tilo_test_env::wait_until("the decoder reports the lecture's length", || {
        app.sync(&doc, &automerge::ROOT);
        app.pending_resume.is_none()
    });
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(
        app.player.position_ms(),
        30_000,
        "it continues where this device left it, not at 0:00"
    );
    assert!(!app.player.is_playing(), "and stays paused");

    // A video with nothing stored opens as the autoplay preference says.
    let (fresh_row, fresh_id) = recent_row("/videos/Talk.mp4", 0);
    app.adopt_settings(&PlayerSettings {
        autoplay: true,
        recent_history: vec![fresh_row],
        ..PlayerSettings::default()
    });
    app.open_local_entry(&fresh_id)
        .expect("the other recent row opens");
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(app.player.position_ms(), 0, "a new video starts at 0:00");
    assert!(app.player.is_playing(), "and autoplay starts it");
}

#[test]
fn finishing_a_video_clears_its_resume_point_so_a_replay_starts_at_the_top() {
    let (row, id) = recent_row("/videos/Seminar.mp4", 20_000);
    let source = row.source.clone();
    let mut app = synth_app();
    app.adopt_settings(&PlayerSettings {
        recent_history: vec![row],
        ..PlayerSettings::default()
    });
    app.open_local_entry(&id).expect("the recent row opens");
    let doc = automerge::AutoCommit::new();
    tilo_test_env::wait_until("the video resumes where it was left", || {
        app.sync(&doc, &automerge::ROOT);
        app.player.position_ms() == 20_000
    });
    assert_eq!(app.position_of(&source), 20_000, "the row carries it");

    // Playback reaches the end.
    let end = app.player.duration_ms();
    assert!(end > 0, "the decoder reported a length");
    app.player.seek_to_ms(end);
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(
        app.resume.point(&resume_key(&source)),
        None,
        "a finished video has nowhere to resume"
    );
    assert_eq!(app.position_of(&source), 0, "its saved row says so too");

    // Opened again, it starts at the beginning.
    app.open_local_entry(&id).expect("the row opens again");
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(app.player.position_ms(), 0, "the replay starts at 0:00");
}

#[test]
fn a_paused_video_keeps_where_it_stopped_in_its_recent_row() {
    let (row, id) = recent_row("/videos/Panel.mp4", 0);
    let mut app = synth_app();
    app.adopt_settings(&PlayerSettings {
        recent_history: vec![row],
        ..PlayerSettings::default()
    });
    app.open_local_entry(&id).expect("the recent row opens");
    let doc = automerge::AutoCommit::new();
    app.sync(&doc, &automerge::ROOT);
    app.player.seek_to_ms(12_000);
    app.player.pause();
    app.sync(&doc, &automerge::ROOT);

    let entry = app
        .recent
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .expect("the video is in Recent");
    let saved = app
        .saved_recent_row(&entry)
        .expect("the row is kept on this device");
    assert_eq!(
        saved.position_ms, 12_000,
        "pausing writes where playback stopped, into the row a restart reads"
    );
}
