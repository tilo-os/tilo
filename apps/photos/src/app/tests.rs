use super::*;
use crate::doc::PhotosDoc;

/// Present `doc`'s library to `app` as the framework does after a change,
/// then sync it.
fn sync(app: &mut PhotosApp, doc: &mut AutoCommit) {
    present(app, doc);
    Widget::sync(app, doc, &ROOT);
    settle(app, doc);
}

/// Apply what `app` submitted to `doc`, as the host does after a pass,
/// without showing it the result yet.
fn land(app: &PhotosApp, doc: &mut AutoCommit) {
    if let Some(dispatch) = &app.dispatch {
        ui::framework::testing::resolve(dispatch, doc).expect("submitted edits apply");
    }
}

/// Apply what `app` submitted to `doc`, as the host does after a pass, and
/// show it the result.
fn settle(app: &mut PhotosApp, doc: &mut AutoCommit) {
    let Some(dispatch) = app.dispatch.clone() else {
        return;
    };
    if !ui::framework::testing::resolve(&dispatch, doc)
        .expect("submitted edits apply")
        .is_empty()
    {
        present(app, doc);
        Widget::sync(app, doc, &ROOT);
    }
}

/// Present `doc`'s library to `app`, as the framework does after a change.
fn present(app: &mut PhotosApp, doc: &AutoCommit) {
    let state = <crate::resource::Photos as ui::tilo_resource::Resource>::read(doc)
        .expect("the library reads");
    app.adopt_resource_state(&Rc::new(state));
}
use crate::actions::{PhotosEffect, PhotosResources};
use crate::model::{Album, LibraryPhoto};
use crate::shots_pattern;
use automerge::ROOT;
use tilo_actions::Action;

/// `app`, editing through a test's dispatcher of the library.
fn editing(app: PhotosApp) -> PhotosApp {
    app.with_dispatcher(ui::framework::testing::dispatcher("so.tilo.photos"))
}

/// A surface on an in-memory content store, editing through a test's
/// dispatcher.
fn test_app() -> PhotosApp {
    editing(PhotosApp::new())
}

/// Stage the screenshot scenario `name` as a capture does: its photographs
/// in the app's content store, its actions applied, its view state posed on
/// the next sync.
fn stage(app: &mut PhotosApp, doc: &mut AutoCommit, name: &str) {
    crate::sample::put_content(&app.content);
    for action in crate::sample::scenario(Some(name)) {
        action.apply(doc).expect("the scenario's actions apply");
    }
    app.stage_scenario(name);
}

fn has_resource_id(resource: &tilo_actions::Resource, id: &str) -> bool {
    resource.id.0 == id
        || resource
            .children
            .iter()
            .any(|child| has_resource_id(child, id))
}

#[test]
fn timeline_layout_stays_legible_justified_and_virtually_bounded() {
    eprintln!("phase timeline_geometry_keeps_200_percent_text_legible_and_density_bounded");
    {
        assert_eq!(timeline_column_count(390.0, 4, false), 2);
        assert_eq!(timeline_column_count(620.0, 4, false), 2);
        assert_eq!(timeline_column_count(390.0, 4, true), 1);
        assert_eq!(timeline_column_count(1_440.0, 0, false), 4);
        assert_eq!(timeline_column_count(1_440.0, 4, false), 6);
        assert_eq!(timeline_column_count(1_440.0, 4, true), 2);
    }

    eprintln!("phase high_text_timeline_uses_readable_card_measures");
    {
        let width = 1_440.0;
        assert_eq!(timeline_column_count(width, 4, true), 2);
        let two_column_card_width = (width - f64::from(PAD * 2.0) - 8.0) / 2.0;
        assert!(
            two_column_card_width >= 600.0,
            "200% text needs a real reading measure, got {two_column_card_width}"
        );
    }

    eprintln!("phase justified_timeline_preserves_aspects_and_never_crosses_a_day");
    {
        let item = |id: &str, day: &str, aspect_ratio| TimelinePhoto {
            id: id.into(),
            name: id.into(),
            favorite: false,
            day: day.into(),
            captured_at: day.into(),
            aspect_ratio,
        };
        let mut projection = TimelineProjection::default();
        projection.replace([
            item("wide", "2026-06-18", 2.0),
            item("portrait", "2026-06-18", 0.5),
            item("square", "2026-06-18", 1.5),
            item("next-day", "2026-06-17", 1.5),
        ]);
        projection.set_geometry(TimelineGeometry {
            width: 600.0,
            target_height: 160.0,
            gap: 12.0,
            compact: false,
        });
        let layout = projection.visible_photos(0.0, projection.total_height());
        assert_eq!(layout.len(), 4);
        let first_day: Vec<_> = layout
            .iter()
            .filter(|entry| entry.photo.day == "2026-06-18")
            .collect();
        assert_eq!(first_day.len(), 3);
        assert!(
            first_day
                .iter()
                .all(|entry| (entry.y - first_day[0].y).abs() < f64::EPSILON)
        );
        let first_day_width = first_day.iter().map(|entry| entry.width).sum::<f64>() + 24.0;
        assert!((first_day_width - 600.0).abs() <= 1.0);
        let next_day = layout
            .iter()
            .find(|entry| entry.photo.id == "next-day")
            .unwrap();
        assert!(next_day.y >= first_day[0].y + first_day[0].height);
        assert!(
            layout
                .iter()
                .all(|entry| entry.width >= 44.0 && entry.height >= 76.0)
        );
        assert!(
            layout
                .iter()
                .all(|entry| (entry.width / entry.height - entry.photo.aspect_ratio).abs() < 0.01)
        );
    }

    eprintln!(
        "phase timeline_virtual_window_has_stable_overscan_without_materialising_the_library"
    );
    {
        assert_eq!(timeline_virtual_range(500, 12, 4, 5), 60..90);
        assert_eq!(timeline_virtual_range(3, 4, 4, 1), 3..3);
    }
}

#[test]
fn canvas_and_places_feedback_stay_honest_and_presentation_only() {
    eprintln!("phase unavailable_bytes_and_bad_bytes_have_distinct_honest_canvas_states");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let waiting =
            OpenImage::from_path("/not-present/yet.png", 1200, 800).with_content("0".repeat(64));
        app.show_photo(&PhotosDoc::put_image(&mut doc, &waiting));
        sync(&mut app, &mut doc);
        assert_eq!(app.canvas_state, CanvasState::Loading);
        assert!(!app.has_image());
        assert_eq!(app.empty_state_text().0, tr("photos-loading-title"));
        assert!(
            app.buttons
                .iter()
                .filter(|(hit, _)| !matches!(hit, Hit::Open | Hit::Library))
                .all(|(_, button)| !button.is_enabled())
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.png");
        std::fs::write(&path, b"not image data").unwrap();
        let bad = OpenImage::from_path(&path.to_string_lossy(), 1200, 800);
        app.show_photo(&PhotosDoc::put_image(&mut doc, &bad));
        sync(&mut app, &mut doc);
        assert_eq!(app.canvas_state, CanvasState::Error);
        assert_eq!(app.empty_state_text().0, tr("photos-error-title"));
    }

    eprintln!("phase places_camera_feedback_is_presentation_only_and_respects_reduced_motion");
    {
        let mut app = test_app();
        let original = app.places_viewport;
        app.zoom_places_by(1.0);
        assert!(app.places_viewport.zoom > original.zoom);
        assert!(app.places_camera_feedback.is_some());
        assert!(app.needs_animation_frame());

        app.set_motion_reduced(true);
        assert!(app.places_camera_feedback.is_none());
        assert!(!app.needs_animation_frame());

        app.zoom_places_by(1.0);
        assert!(app.places_camera_feedback.is_none());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn people_review_commits_only_explicit_canonical_actions() {
    eprintln!("phase people_scan_cursor_only_advances_after_the_admitted_batch_succeeds");
    {
        let mut progress = PeopleScanProgress {
            next_offset: 12,
            ..PeopleScanProgress::default()
        };
        let (start, len) = progress.batch_range(25, 12).unwrap();
        assert_eq!((start, len), (12, 12));
        progress.record_failure("local model timed out".to_owned());
        assert_eq!(
            progress.next_offset, 12,
            "a failed batch must retry its same photos"
        );
        assert_eq!(progress.status, "error");
        assert_eq!(
            progress.last_error.as_deref(),
            Some("local model timed out")
        );

        progress.advance_after_success(start, len, 25);
        assert_eq!(progress.next_offset, 24);
        assert_eq!(progress.status, "paused");
        let (last_start, last_len) = progress.batch_range(25, 12).unwrap();
        assert_eq!((last_start, last_len), (24, 1));
        progress.advance_after_success(last_start, last_len, 25);
        assert_eq!(
            progress.next_offset, 0,
            "the bounded cursor completes every photo before wrapping"
        );
        assert_eq!(progress.status, "complete");
    }

    eprintln!("phase people_review_commits_only_an_explicit_membership_action");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(
            OpenImage::from_path("/photos/person.png", 20, 10)
                .with_content(tilo_content::hash_bytes(b"people-review")),
        );
        let photo_id = photo.id.clone();
        crate::actions::PhotosAction::ImportPhoto { photo }
            .apply(&mut doc)
            .unwrap();
        assert!(app.people_controller_mut().add_cluster(
            "manual-person",
            [photo_id.clone()],
            crate::people::ProposalOrigin::ImportedOrManual,
        ));
        assert_eq!(
            app.people_controller_mut()
                .confirm_suggestion("manual-person", "Sam"),
            crate::people::PeopleChange::Updated
        );
        assert!(app.approve_people_cluster("manual-person"));
        settle(&mut app, &mut doc);
        assert_eq!(PhotosDoc::photo(&doc, &photo_id).unwrap().people, ["Sam"]);
        assert_eq!(
            app.people_controller()
                .cluster("manual-person")
                .unwrap()
                .origin,
            crate::people::ProposalOrigin::ImportedOrManual
        );
    }

    eprintln!(
        "phase people_surface_completes_review_browse_rename_and_merge_through_canonical_actions"
    );
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let first = LibraryPhoto::from_image(
            OpenImage::from_path("/photos/first.png", 20, 10)
                .with_content(tilo_content::hash_bytes(b"first-person")),
        );
        let first_id = first.id.clone();
        let mut second = LibraryPhoto::from_image(
            OpenImage::from_path("/photos/second.png", 20, 10)
                .with_content(tilo_content::hash_bytes(b"second-person")),
        );
        second.people = vec!["Alex".to_owned()];
        crate::actions::PhotosAction::ImportPhoto { photo: first }
            .apply(&mut doc)
            .unwrap();
        crate::actions::PhotosAction::ImportPhoto { photo: second }
            .apply(&mut doc)
            .unwrap();
        assert!(app.people_controller_mut().add_cluster(
            "review-me",
            [first_id.clone()],
            crate::people::ProposalOrigin::ImportedOrManual,
        ));

        app.select_people_item(PeopleSelection::Suggestion("review-me".to_owned()));
        app.people_name_input.set_text("Sam");
        app.handle_people_intent(PeopleIntent::ApproveSuggestion);
        settle(&mut app, &mut doc);
        assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Sam"]);
        assert!(
            app.people_controller().cluster("review-me").unwrap().hidden,
            "an approved local proposal leaves the active review queue"
        );

        app.select_people_item(PeopleSelection::Approved("Sam".to_owned()));
        app.people_name_input.set_text("Samuel");
        app.handle_people_intent(PeopleIntent::SaveName);
        settle(&mut app, &mut doc);
        assert_eq!(
            PhotosDoc::photo(&doc, &first_id).unwrap().people,
            ["Samuel"]
        );
        app.handle_people_intent(PeopleIntent::MergeApprovedInto("Alex".to_owned()));
        settle(&mut app, &mut doc);
        assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Alex"]);

        app.select_people_item(PeopleSelection::Approved("Alex".to_owned()));
        app.handle_people_intent(PeopleIntent::BrowseApproved);
        assert_eq!(app.active_person.as_deref(), Some("Alex"));
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
        assert!(!app.people_mode);
    }

    eprintln!("phase people_rejection_is_reversible_and_accessibility_activates_the_same_control");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        assert!(
            app.people_controller_mut().add_cluster(
                "dismiss-me",
                [
                    "photos.photo:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_owned()
                ],
                crate::people::ProposalOrigin::ImportedOrManual,
            )
        );
        app.library_mode = true;
        app.people_mode = true;
        app.library_destination = LibraryDestination::People;
        app.select_people_item(PeopleSelection::Suggestion("dismiss-me".to_owned()));
        app.refresh_people_controls();

        let reject = app
            .people_controls
            .iter()
            .position(|control| matches!(control.intent, PeopleIntent::RejectSuggestion))
            .unwrap();
        assert!(app.perform_library_semantic_action(
            &format!("photos:people:control:{reject}"),
            &AccessAction::Activate,
            &mut doc,
        ));
        let cluster = app.people_controller().cluster("dismiss-me").unwrap();
        assert!(cluster.hidden && cluster.rejected);

        app.select_people_item(PeopleSelection::Suggestion("dismiss-me".to_owned()));
        app.refresh_people_controls();
        let restore = app
            .people_controls
            .iter()
            .position(|control| matches!(control.intent, PeopleIntent::RestoreSuggestion))
            .unwrap();
        assert!(app.perform_library_semantic_action(
            &format!("photos:people:control:{restore}"),
            &AccessAction::Activate,
            &mut doc,
        ));
        let cluster = app.people_controller().cluster("dismiss-me").unwrap();
        assert!(!cluster.hidden && !cluster.rejected);
    }
}

#[test]
fn library_masthead_rows_never_overlap_at_200_percent() {
    eprintln!("phase compact_header_rows_are_non_overlapping_at_200_percent");
    {
        let header =
            compact_library_header_geometry(Rect::new(0.0, 94.0, 390.0, 820.0), true, true);
        let title = Rect::new(12.0, header.title_y, 378.0, header.title_y + 26.0);
        let count = Rect::new(12.0, header.count_y, 192.0, header.count_y + 22.0);
        let albums = Rect::new(12.0, header.albums_y, 378.0, header.albums_y + 44.0);
        let album_actions = header.album_actions;
        assert!(title.y1 <= count.y0);
        assert!(count.y1 <= header.controls.y0);
        assert!(header.controls.y1 <= albums.y0);
        assert!(albums.y1 <= album_actions.y0);
        assert!((album_actions.height() - 44.0).abs() < f64::EPSILON);
        assert!(album_actions.y1 <= header.timeline_y);
        assert!(header.timeline_y > 94.0);
    }

    eprintln!("phase compact_library_masthead_keeps_its_count_out_of_the_control_row");
    {
        let header =
            compact_library_header_geometry(Rect::new(0.0, 94.0, 640.0, 820.0), false, false);
        let title = Rect::new(12.0, header.title_y, 220.0, header.title_y + 24.0);
        let count = Rect::new(12.0, header.count_y, 220.0, header.count_y + 18.0);

        assert!(
            title.y1 <= count.y0,
            "the count must not read as a title suffix"
        );
        assert!(
            count.y1 <= header.timeline_y,
            "the first date group must remain below the compact library context"
        );
        assert!(
            header.controls.x0 > count.x1,
            "shared density and selection controls must retain their own hit lane"
        );
    }

    eprintln!("phase library_masthead_controls_never_paint_through_each_other");
    {
        fn overlaps(left: Rect, right: Rect) -> bool {
            left.x0 < right.x1 && left.x1 > right.x0 && left.y0 < right.y1 && left.y1 > right.y0
        }

        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/header.png", 40, 20));
        let photo_id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: vec![Album {
                    id: "photos.album:header".into(),
                    name: "Header".into(),
                    photo_ids: vec![photo_id],
                }],
            },
        );
        app.show_library();
        sync(&mut app, &mut doc);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();

        app.canvas_rect = Rect::new(0.0, 0.0, 1_280.0, 800.0);
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        app.draw_album_editor(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        let mut desktop_controls: Vec<Rect> =
            app.places_hits.iter().map(|(_, rect)| *rect).collect();
        desktop_controls.extend(app.selection_hits.iter().map(|(_, rect)| *rect));
        desktop_controls.extend(app.timeline_density_hits.iter().map(|(_, rect)| *rect));
        desktop_controls.extend(app.album_editor_hits.iter().map(|(_, rect)| *rect));
        for (index, rect) in desktop_controls.iter().enumerate() {
            assert!(
                desktop_controls
                    .iter()
                    .skip(index + 1)
                    .all(|other| !overlaps(*rect, *other)),
                "desktop masthead controls overlap: {rect:?}"
            );
        }

        app.canvas_rect = Rect::new(0.0, 0.0, 390.0, 760.0);
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        let select = app
            .selection_hits
            .iter()
            .find_map(|(hit, rect)| (*hit == SelectionHit::Start).then_some(*rect))
            .expect("compact library keeps Select visible");
        assert!(
            app.timeline_density_hits
                .iter()
                .all(|(_, density)| !overlaps(select, *density)),
            "compact density controls must not paint underneath Select"
        );
    }
}

#[test]
fn date_index_follows_timeline_rows_boundaries_and_honest_labels() {
    eprintln!("phase date_index_uses_timeline_rows_and_preserves_virtual_scroll_anchor");
    {
        let mut app = test_app();
        app.timeline_index_metrics = ScrollMetrics::new(100.0, 10.0, 20.0);
        assert!(app.apply_timeline_index_intent(&DateIndexIntent::ScrollTo { position: 0.5 }));
        assert!((app.timeline_scroll_y - 45.0).abs() < f64::EPSILON);
        // A hover preview may redraw a label but must never mutate the scroll
        // anchor or create an accidental shared action.
        assert!(app.apply_timeline_index_intent(&DateIndexIntent::PreviewChanged(None)));
        assert!((app.timeline_scroll_y - 45.0).abs() < f64::EPSILON);
    }

    eprintln!("phase date_index_labels_keep_non_iso_values_honest");
    {
        assert_eq!(timeline_date_label("2026-09-18"), "Sep 18, 2026");
        assert_eq!(timeline_date_label("Undated"), "Undated");
        assert_eq!(timeline_date_label("2026-13-01"), "2026-13-01");
        assert_eq!(capture_day("2026-09-18 08:14"), "2026-09-18");
        assert_eq!(
            capture_day("2026-09-18T08:14"),
            "2026-09-18",
            "the stored ISO form groups by its date"
        );
        assert_eq!(timeline_date_label("2026-07-12T18:16"), "Jul 12, 2026");
        assert_eq!(capture_day("Jun 2026"), "Jun");
    }

    eprintln!("phase date_index_boundaries_are_chronological_and_mark_year_changes");
    {
        let item = |id: &str, day: &str| TimelinePhoto {
            id: id.into(),
            name: id.into(),
            favorite: false,
            day: day.into(),
            captured_at: day.into(),
            aspect_ratio: 1.5,
        };
        let mut projection = TimelineProjection::default();
        projection.replace([
            item("new", "2026-02-03"),
            item("same-day", "2026-02-03"),
            item("old", "2025-12-31"),
        ]);
        projection.set_geometry(TimelineGeometry {
            width: 600.0,
            target_height: 160.0,
            gap: 12.0,
            compact: false,
        });
        let markers = projection.date_markers(0.0);
        assert_eq!(markers.len(), 2, "one boundary per capture day");
        assert!(
            markers[0].2 && markers[1].2,
            "each year change is a major boundary"
        );
        assert!(markers[0].1 < markers[1].1);
        assert_eq!(timeline_date_label(&markers[0].0), "Feb 03, 2026");
        assert_eq!(timeline_date_label(&markers[1].0), "Dec 31, 2025");
    }

    eprintln!("phase canonical_timeline_day_operation_jumps_to_its_shared_capture_boundary");
    {
        let mut doc = AutoCommit::new();
        let mut newer = LibraryPhoto::from_image(OpenImage::from_path("newer.png", 20, 10));
        newer.captured_at = Some("2026-09-07 08:14".into());
        let mut older = LibraryPhoto::from_image(OpenImage::from_path("older.png", 20, 10));
        older.captured_at = Some("2026-08-01".into());
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![newer, older],
                ..Default::default()
            },
        );
        let mut app = test_app();
        app.view_timeline(
            &PhotosDoc::library(&doc),
            Some(&crate::actions::TimelineAnchor::Day {
                day: "2026-08-01".into(),
            }),
        )
        .expect("a published day anchor jumps to its timeline boundary");
        assert!(app.library_mode);
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
        assert!(app.timeline_scroll_y > 0.0);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn analysis_queue_is_shared_bounded_and_commits_only_current_results() {
    eprintln!("phase every_local_analysis_kind_uses_the_same_bounded_queue");
    {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let image = OpenImage::from_path("queue.png", 2, 2);
        for language in ["eng", "deu"] {
            sender
                .try_send(AnalysisJob {
                    invocation: tilo_actions::InvocationId::new(),
                    photo_id: "photos.photo:queue".to_owned(),
                    content: None,
                    image: image.clone(),
                    kind: AnalysisJobKind::Ocr {
                        language: language.to_owned(),
                    },
                })
                .unwrap();
        }
        assert!(
            sender
                .try_send(AnalysisJob {
                    invocation: tilo_actions::InvocationId::new(),
                    photo_id: "photos.photo:queue".to_owned(),
                    content: None,
                    image,
                    kind: AnalysisJobKind::Ocr {
                        language: "fra".to_owned(),
                    },
                })
                .is_err()
        );
    }

    eprintln!("phase cancelled_or_stale_analysis_can_never_persist");
    {
        let mut app = test_app();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let doc = AutoCommit::new();

        let cancelled = tilo_actions::InvocationId::new();
        app.cancelled_analysis.insert(cancelled);
        sender
            .send(AnalysisResult {
                invocation: cancelled,
                photo_id: "photos.photo:missing".to_owned(),
                content: None,
                result: AnalysisOutcome::Image {
                    component: ImageAnalysisComponent::Ocr,
                    result: Ok(crate::model::ImageAnalysis {
                        ocr_text: "must not persist".to_owned(),
                        ..crate::model::ImageAnalysis::default()
                    }),
                },
            })
            .unwrap();
        present(&mut app, &doc);
        app.drain_analysis_results();
        assert!(app.take_settled(cancelled).is_none());

        let stale = tilo_actions::InvocationId::new();
        sender
            .send(AnalysisResult {
                invocation: stale,
                photo_id: "photos.photo:missing".to_owned(),
                content: None,
                result: AnalysisOutcome::Image {
                    component: ImageAnalysisComponent::Ocr,
                    result: Ok(crate::model::ImageAnalysis {
                        ocr_text: "also must not persist".to_owned(),
                        ..crate::model::ImageAnalysis::default()
                    }),
                },
            })
            .unwrap();
        present(&mut app, &doc);
        app.drain_analysis_results();
        assert!(matches!(
            app.take_settled(stale),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "photos.analysis_stale"
        ));
        assert!(PhotosDoc::library(&doc).photos.is_empty());
    }

    eprintln!("phase drain_preserves_ocr_and_vision_components_from_separate_jobs");
    {
        let mut app = test_app();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let mut doc = AutoCommit::new();
        let content = "a".repeat(64);
        let photo = LibraryPhoto::from_image(
            OpenImage::from_path("drain-components.png", 20, 10).with_content(content.clone()),
        );
        let id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                ..Default::default()
            },
        );

        let ocr_provenance = crate::model::AnalysisProvenance {
            source: "local-ocr".into(),
            engine: "tilo-ocr".into(),
            language: "eng".into(),
            content: content.clone(),
            scope: "local".into(),
            ..Default::default()
        };
        sender
            .send(AnalysisResult {
                invocation: tilo_actions::InvocationId::new(),
                photo_id: id.clone(),
                content: Some(content.clone()),
                result: AnalysisOutcome::Image {
                    component: ImageAnalysisComponent::Ocr,
                    result: Ok(crate::model::ImageAnalysis {
                        ocr_text: "receipt forty two".into(),
                        provenance: ocr_provenance.clone(),
                        ocr_provenance: Some(ocr_provenance),
                        ..Default::default()
                    }),
                },
            })
            .unwrap();
        let vision_provenance = crate::model::AnalysisProvenance {
            source: "local-vision".into(),
            engine: "tilo-vision".into(),
            model_id: "signed-model".into(),
            model_revision: "b".repeat(64),
            content: content.clone(),
            scope: "local".into(),
            ..Default::default()
        };
        sender
            .send(AnalysisResult {
                invocation: tilo_actions::InvocationId::new(),
                photo_id: id.clone(),
                content: Some(content),
                result: AnalysisOutcome::Image {
                    component: ImageAnalysisComponent::Vision,
                    result: Ok(crate::model::ImageAnalysis {
                        description: "A bicycle beside a lake".into(),
                        objects: vec!["bicycle".into()],
                        provenance: vision_provenance.clone(),
                        vision_provenance: Some(vision_provenance),
                        ..Default::default()
                    }),
                },
            })
            .unwrap();

        present(&mut app, &doc);

        app.drain_analysis_results();
        settle(&mut app, &mut doc);
        let analysis = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
        assert_eq!(analysis.ocr_text, "receipt forty two");
        assert_eq!(analysis.description, "A bicycle beside a lake");
        assert_eq!(analysis.objects, ["bicycle"]);
        assert_eq!(analysis.ocr_provenance.unwrap().source, "local-ocr");
        assert_eq!(analysis.vision_provenance.unwrap().source, "local-vision");
    }

    eprintln!("phase library_analysis_commits_each_real_result_with_provenance_and_finishes");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let content = "a".repeat(64);
        let image = OpenImage::from_path("/photos/one.png", 20, 10).with_content(&content);
        let photo = LibraryPhoto::from_image(image);
        let id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: Vec::new(),
            },
        );
        let invocation = tilo_actions::InvocationId::new();
        app.library_analysis.begin(
            invocation,
            [id.clone()],
            0,
            "Verified model".into(),
            "b".repeat(64),
        );
        assert_eq!(
            app.library_analysis
                .take_next(|_| Some("one.png".into()))
                .as_deref(),
            Some(id.as_str())
        );
        app.active_analysis.insert(invocation);
        present(&mut app, &doc);
        app.drain_library_analysis_result(AnalysisResult {
            invocation,
            photo_id: id.clone(),
            content: Some(content.clone()),
            result: AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Vision,
                result: Ok(crate::model::ImageAnalysis {
                    description: "A bicycle beside a lake".into(),
                    objects: vec!["bicycle".into()],
                    provenance: crate::model::AnalysisProvenance {
                        source: "local-vision".into(),
                        engine: "tilo-vision".into(),
                        model_id: "vision-local".into(),
                        model_revision: "b".repeat(64),
                        content,
                        scope: "local".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
        });
        settle(&mut app, &mut doc);
        let stored = PhotosDoc::photo(&doc, &id)
            .and_then(|photo| photo.analysis)
            .expect("result committed through the typed document action");
        assert_eq!(stored.provenance.scope, "shared");
        assert_eq!(stored.provenance.model_id, "vision-local");
        assert_eq!(
            app.library_analysis.phase,
            crate::library_analysis::LibraryAnalysisPhase::Complete
        );
        assert!(matches!(
            app.take_settled(invocation),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn density_changes_share_one_event_path_and_reanchor_the_first_photo() {
    eprintln!("phase pinch_and_ctrl_wheel_use_the_same_density_event_path");
    {
        use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
        use winit::keyboard::ModifiersState;

        let mut app = test_app();
        app.library_mode = true;
        app.canvas_rect = Rect::new(0.0, 48.0, 1_280.0, 800.0);
        let mut doc = AutoCommit::new();
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut focus = None;
        let mut handled = false;
        let no_modifiers = winit::event::Modifiers::default();
        app.handle_event(
            &WindowEvent::PinchGesture {
                device_id: DeviceId::dummy(),
                delta: 0.2,
                phase: TouchPhase::Moved,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &no_modifiers,
                state: &mut doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
        assert!(handled);
        let after_pinch = app.timeline_density;

        handled = false;
        let ctrl = winit::event::Modifiers::from(ModifiersState::CONTROL);
        app.handle_event(
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &ctrl,
                state: &mut doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
        assert!(handled);
        assert_eq!(app.timeline_density, after_pinch + 1);
    }

    eprintln!("phase density_reanchors_the_same_first_photo");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..90 {
            let mut photo = LibraryPhoto::from_image(OpenImage::from_path(
                &format!("/photos/anchor-{index:03}.png"),
                30,
                20,
            ));
            photo.captured_at = Some("2026-06-18".into());
            library.photos.push(photo);
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.show_library();
        app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 800.0);
        app.timeline_density = 1;
        sync(&mut app, &mut doc);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_library(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        app.timeline_scroll_y = 900.0;
        let (photo, screen_y) = {
            let visible = app
                .timeline_projection
                .visible_photos(app.timeline_scroll_y, app.timeline_viewport_height);
            let first = visible
                .iter()
                .find(|entry| entry.y + entry.height >= 0.0)
                .unwrap();
            (first.photo.id.clone(), first.y)
        };
        assert!(app.adjust_timeline_density(true));
        app.draw_library(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let visible = app
            .timeline_projection
            .visible_photos(app.timeline_scroll_y, app.timeline_viewport_height);
        let restored = visible
            .iter()
            .find(|entry| entry.photo.id == photo)
            .expect("the previous first photo remains visible after density changes");
        assert!((restored.y - screen_y).abs() < 0.001);
    }

    eprintln!("phase enlarged_library_keeps_photos_first_with_density_navigation");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..5 {
            let mut photo = LibraryPhoto::from_image(OpenImage::from_path(
                &format!("/photos/library-{index}.png"),
                20,
                10,
            ));
            photo.tags = vec!["golden hour".into()];
            library.photos.push(photo);
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.show_library();
        app.large_text_toolbar = true;
        app.canvas_rect = Rect::new(0.0, 48.0, 390.0, 796.0);
        sync(&mut app, &mut doc);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        app.draw_library(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );

        assert!(app.metadata_filter_cards.is_empty());
        assert!(
            app.album_editor_hits.is_empty(),
            "200% keeps album management out of the photo-first masthead"
        );
        assert!(
            app.library_cards
                .first()
                .is_some_and(|(_, rect, _)| rect.y0 < app.canvas_rect.y1),
            "the first photograph must remain above the fold at 200%"
        );
        let semantics = app.accessibility_tree();
        let children = &semantics.children[1].children;
        assert!(
            children.iter().any(|node| {
                node.action_target.as_deref() == Some("photos:timeline:density-in")
            })
        );
        assert!(!children.iter().any(|node| {
            node.action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("photos:library-page:"))
        }));

        let before = app.timeline_density;
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Focus,
            &mut doc,
        ));
        assert_eq!(
            app.timeline_density, before,
            "semantic focus must not navigate"
        );
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.timeline_density, before + 1);
        assert_eq!(PhotosDoc::library(&doc), library);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn collection_views_are_local_complete_and_refuse_stale_ids() {
    eprintln!(
        "phase opening_an_album_operation_only_changes_the_local_filter_and_refuses_stale_ids"
    );
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.push(photo.clone());
        library.albums.push(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![photo.id],
        });
        PhotosDoc::set_library(&mut doc, &library);
        let before = PhotosDoc::library(&doc);
        let payload = PhotosEffect::ViewAlbum {
            album_id: "photos.album:trip".into(),
        };
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc)
            )
            .unwrap()
            .is_some()
        );
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "navigation must not alter membership"
        );

        let stale = PhotosEffect::ViewAlbum {
            album_id: "photos.album:gone".into(),
        };
        assert!(
            app.answer_effect(
                stale,
                ui::tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc)
            )
            .is_err()
        );
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    eprintln!("phase opening_favorites_uses_a_local_view_and_refuses_an_empty_stale_collection");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut favorite =
            LibraryPhoto::from_image(OpenImage::from_path("/photos/favorite.png", 20, 10));
        favorite.favorite = true;
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![favorite],
                albums: Vec::new(),
            },
        );
        let before = PhotosDoc::library(&doc);
        let payload = PhotosEffect::ViewFavorites;
        assert!(
            app.answer_effect(
                payload.clone(),
                ui::tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc)
            )
            .expect("favorite collection is present")
            .is_some()
        );
        assert!(app.library_mode);
        assert!(app.active_favorites);
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "opening a view is not a write"
        );

        PhotosDoc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc)
            )
            .is_err()
        );
    }

    eprintln!("phase metadata_views_are_local_filters_with_honest_stale_results");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut in_folder =
            LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        in_folder.folder = Some("Travel".into());
        in_folder.people = vec!["Élodie".into()];
        in_folder.tags = vec!["golden-hour".into()];
        let other = LibraryPhoto::from_image(OpenImage::from_path("/photos/two.png", 20, 10));
        let library = crate::model::PhotoLibrary {
            photos: vec![in_folder, other],
            ..Default::default()
        };
        PhotosDoc::set_library(&mut doc, &library);

        app.view_metadata_filter(&PhotosDoc::library(&doc), Some("Travel".into()), None, None)
            .expect("existing folder enters a view");
        sync(&mut app, &mut doc);
        assert!(app.library_mode);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_folder.as_deref(), Some("Travel"));
        assert!(app.active_person.is_none());

        app.view_metadata_filter(&PhotosDoc::library(&doc), None, Some("Élodie".into()), None)
            .expect("existing person enters a view");
        sync(&mut app, &mut doc);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_person.as_deref(), Some("Élodie"));
        assert!(app.active_folder.is_none());
        app.view_metadata_filter(
            &PhotosDoc::library(&doc),
            None,
            None,
            Some("golden-hour".into()),
        )
        .expect("existing tag enters a view");
        sync(&mut app, &mut doc);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_tag.as_deref(), Some("golden-hour"));
        assert!(app.active_folder.is_none());
        assert!(app.active_person.is_none());
        assert!(
            app.view_metadata_filter(
                &PhotosDoc::library(&doc),
                Some("Missing".into()),
                None,
                None
            )
            .is_err()
        );
    }

    eprintln!("phase people_places_and_albums_keep_complete_paged_collections");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..29 {
            let mut photo = LibraryPhoto::from_image(OpenImage::from_path(
                &format!("/photos/{index}.png"),
                40,
                20,
            ));
            photo.people = vec![format!("Person {index:02}")];
            photo.location = Some(crate::model::PhotoLocation {
                latitude: 10.0 + f64::from(index) / 100.0,
                longitude: 20.0,
                label: Some(format!("Place {index:02}")),
                provenance: crate::model::LocationProvenance::default(),
            });
            library.photos.push(photo);
            library.albums.push(crate::model::Album {
                id: format!("photos.album:album-{index:02}"),
                name: format!("Album {index:02}"),
                photo_ids: Vec::new(),
            });
        }
        PhotosDoc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);

        assert_eq!(app.library_people.len(), 29);
        assert_eq!(app.places_items.len(), 29);
        assert_eq!(app.library_albums.len(), 29);
        assert!(app.page_collection(CollectionPageHit::PeopleNext));
        assert!(app.page_collection(CollectionPageHit::PlacesNext));
        assert!(app.page_collection(CollectionPageHit::AlbumsNext));
        assert_eq!(
            (app.people_page, app.places_page, app.albums_page),
            (1, 1, 1)
        );
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-next",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.people_page, 2);
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-previous",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.people_page, 1);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end parity test deliberately compares favorite and membership envelopes across UI, AT, and resource routes"
)]
#[test]
fn selection_and_album_editing_page_everything_and_mint_canonical_actions() {
    eprintln!("phase selection_is_local_but_its_favorite_and_album_results_are_canonical_actions");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let first = LibraryPhoto::from_image(OpenImage::from_path("/photos/first.png", 20, 10));
        let second = LibraryPhoto::from_image(OpenImage::from_path("/photos/second.png", 20, 10));
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first.clone(), second.clone()],
                albums: vec![Album {
                    id: "photos.album:trip".into(),
                    name: "Trip".into(),
                    photo_ids: Vec::new(),
                }],
            },
        );
        app.library_cards = vec![
            (first.id.clone(), Rect::new(0.0, 0.0, 44.0, 44.0), false),
            (second.id.clone(), Rect::new(48.0, 0.0, 92.0, 44.0), false),
        ];
        app.library_albums = vec![("photos.album:trip".into(), "Trip".into(), 0)];
        app.begin_selection();
        app.toggle_library_selection(&first.id, false);
        app.toggle_library_selection(&second.id, true);
        assert_eq!(
            app.selected_photo_ids.len(),
            2,
            "range selects visible cards"
        );
        app.set_selected_favorite(true);
        settle(&mut app, &mut doc);
        assert!(
            PhotosDoc::library(&doc)
                .photos
                .iter()
                .all(|photo| photo.favorite)
        );
        assert!(!app.selection_mode, "selection itself never persists");

        app.library_cards = vec![
            (first.id.clone(), Rect::new(0.0, 0.0, 44.0, 44.0), true),
            (second.id.clone(), Rect::new(48.0, 0.0, 92.0, 44.0), true),
        ];
        app.begin_selection();
        app.select_all_visible();
        let mut selected_ids = [first.id.clone(), second.id.clone()];
        selected_ids.sort_unstable();
        assert!(app.perform_library_semantic_action(
            "photos:selection:favorite",
            &AccessAction::Activate,
            &mut doc,
        ));
        land(&app, &mut doc);
        let favorites = PhotosResources
            .publish_resources(&doc)
            .pop()
            .expect("library root")
            .actions
            .into_iter()
            .find(|action| action.id == "set-favorites")
            .expect("favorite batch is published");
        let expected_favorite = favorites
            .build_args([
                (
                    "photo_ids".into(),
                    tilo_actions::ArgValue::Array(
                        selected_ids
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                ),
                ("favorite".into(), tilo_actions::ArgValue::Boolean(false)),
            ])
            .expect("published favorite contract accepts the selection result");
        assert_eq!(
            app.last_library_action.as_ref(),
            Some(&expected_favorite),
            "the AT batch control and resource action produce byte-identical envelopes"
        );
        assert!(
            PhotosDoc::library(&doc)
                .photos
                .iter()
                .all(|photo| !photo.favorite),
            "the same batch command also has an explicit unfavorite result"
        );

        app.begin_selection();
        app.select_all_visible();
        assert!(app.perform_library_semantic_action(
            "photos:selection:choose-album",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert!(app.selection_album_chooser);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:trip",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:trip"),
            "the target album is an explicit, stable choice rather than the current view"
        );
        app.add_selected_to_destination_album();
        land(&app, &mut doc);
        let album = PhotosResources
            .publish_resource(&doc, "photos.album:trip")
            .expect("chosen album remains published");
        let membership = album
            .actions
            .into_iter()
            .find(|action| action.id == "set-album-membership")
            .expect("album membership is published");
        let expected_membership = membership
            .build_args([(
                "photo_ids".into(),
                tilo_actions::ArgValue::Array(
                    selected_ids
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            )])
            .expect("published membership contract accepts the selection result");
        assert_eq!(
            app.last_library_action.as_ref(),
            Some(&expected_membership),
            "the pointer batch control and resource action produce byte-identical envelopes"
        );
        assert_eq!(
            PhotosDoc::library(&doc).albums[0].photo_ids,
            vec![first.id, second.id],
            "album membership uses the same absolute document action agents invoke"
        );
    }

    eprintln!("phase destination_chooser_pages_every_album_and_refuses_a_stale_target");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let albums: Vec<_> = (1..=SELECTION_ALBUMS_PER_PAGE + 1)
            .map(|number| Album {
                id: format!("photos.album:destination-{number}"),
                name: format!("Destination {number}"),
                photo_ids: Vec::new(),
            })
            .collect();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo.clone()],
                albums: albums.clone(),
            },
        );
        present(&mut app, &doc);
        app.library_albums = albums
            .iter()
            .map(|album| (album.id.clone(), album.name.clone(), 0))
            .collect();
        app.library_cards = vec![(photo.id.clone(), Rect::ZERO, false)];
        app.begin_selection();
        app.select_all_visible();
        assert!(app.perform_library_semantic_action(
            "photos:selection:choose-album",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination-next",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.selection_album_page, 1);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:destination-7")
        );
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: Vec::new(),
            },
        );
        present(&mut app, &doc);
        assert!(!app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.notice.as_deref(),
            Some("That destination album is no longer available")
        );
    }

    eprintln!("phase album_editor_pages_large_libraries_without_changing_shared_membership");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..49 {
            library
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    &format!("/photos/library-{index}.png"),
                    20,
                    10,
                )));
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.library_total = library.photos.len();
        app.library_mode = true;
        app.album_editor = true;
        app.canvas_rect = Rect::new(0.0, 0.0, 390.0, 760.0);
        let before = PhotosDoc::library(&doc);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let (_, previous) = app
            .album_page_buttons
            .iter()
            .find(|(hit, _)| *hit == AlbumEditorHit::PreviousPage)
            .expect("the editor has a previous-page control");
        let (_, next) = app
            .album_page_buttons
            .iter()
            .find(|(hit, _)| *hit == AlbumEditorHit::NextPage)
            .expect("the editor has a next-page control");
        assert!(!previous.is_enabled());
        assert!(next.is_enabled());
        let semantics = app.accessibility_tree();
        let pager = &semantics.children[1].children;
        assert!(pager.iter().any(|node| {
            node.action_target.as_deref() == Some("photos:album-editor:previous-page")
                && node.state.disabled
                && node.name.as_deref() == Some("Show previous photographs")
        }));
        assert!(pager.iter().any(|node| {
            node.action_target.as_deref() == Some("photos:album-editor:next-page")
                && !node.state.disabled
                && node.name.as_deref() == Some("Show next photographs")
        }));

        assert!(app.perform_library_semantic_action(
            "photos:album-editor:next-page",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.library_offset, 24);
        assert!(app.perform_library_semantic_action(
            "photos:album-editor:previous-page",
            &AccessAction::Activate,
            &mut doc,
        ));
        // The preceding activation is valid from the second page and leaves
        // the editor's membership draft local.
        assert_eq!(app.library_offset, 0);

        app.page_library(true);
        assert_eq!(app.library_offset, 24);
        app.page_library(true);
        assert_eq!(app.library_offset, 48);
        app.page_library(true);
        assert_eq!(app.library_offset, 48);
        app.page_library(false);
        assert_eq!(app.library_offset, 24);
        app.page_library(false);
        assert_eq!(app.library_offset, 0);
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    eprintln!("phase concurrent_album_drafts_mint_distinct_stable_ids");
    {
        let doc = AutoCommit::new();
        let mut left = test_app();
        let mut right = test_app();
        left.begin_new_album(&doc);
        right.begin_new_album(&doc);
        let left_id = left.album_editor_id.expect("left draft id");
        let right_id = right.album_editor_id.expect("right draft id");
        assert!(left_id.starts_with("photos.album:"));
        assert!(right_id.starts_with("photos.album:"));
        assert_ne!(left_id, right_id);
    }

    eprintln!("phase albums_destination_materializes_bounded_covers_and_semantic_routes");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/pictures/cover.png", 800, 600));
        let photo_id = photo.id.clone();
        let album_id = "photos.album:weekend".to_owned();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: vec![crate::model::Album {
                    id: album_id.clone(),
                    name: "Weekend".to_owned(),
                    photo_ids: vec![photo_id.clone(), "missing".to_owned()],
                }],
            },
        );
        app.show_albums();
        sync(&mut app, &mut doc);
        assert_eq!(app.library_destination, LibraryDestination::Albums);
        assert_eq!(
            app.library_album_covers.get(&album_id),
            Some(&vec![photo_id])
        );

        assert!(app.perform_library_semantic_action(
            &format!("photos:album-edit:{album_id}"),
            &AccessAction::Activate,
            &mut doc,
        ));
        assert!(app.album_editor);
        app.cancel_album_editor();
        assert!(app.perform_library_semantic_action(
            &format!("photos:library-album:{album_id}"),
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.active_album.as_deref(), Some(album_id.as_str()));
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
    }
}

#[test]
fn operations_fail_canonically_cancel_terminally_and_confirm_removal() {
    eprintln!("phase unresolved_share_reference_returns_a_canonical_operation_failure");
    {
        let mut app = test_app();
        let doc = AutoCommit::new();
        let payload = PhotosEffect::ShareReference {
            resource_id: "photos.photo:missing".into(),
        };

        let error = app
            .answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc),
            )
            .expect_err("a missing resource cannot claim that its reference was shared");
        assert_eq!(error, tr("photos-share-failed"));
        assert_eq!(
            app.notice.as_deref(),
            Some(tr("photos-share-failed").as_str())
        );
    }

    eprintln!("phase cancelling_a_portal_invocation_is_terminal_and_drops_late_results");
    {
        let mut app = test_app();
        let invocation = tilo_actions::InvocationId::new();
        let (tx, rx) = std::sync::mpsc::channel();
        app.picker = Some(rx);
        app.resource_invocation = Some(invocation);

        app.cancel_invocation(invocation);
        assert!(app.picker.is_none());
        assert!(app.resource_invocation.is_none());
        assert!(
            tx.send(ui::portal::PortalOutcome::Opened("/late.png".into()))
                .is_err(),
            "a late portal completion must have no receiver after cancellation"
        );

        let direct = tilo_actions::InvocationId::new();
        app.pending_resource_open = Some((direct, "/late-direct.png".into()));
        app.cancel_invocation(direct);
        assert!(app.pending_resource_open.is_none());
    }

    eprintln!("phase library_removal_requires_confirmation_and_routes_through_the_action_host");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/remove.png", 20, 10));
        let id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: Vec::new(),
            },
        );
        app.show_library();
        sync(&mut app, &mut doc);
        let target = format!("photos:library-remove:{id}");
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        assert!(PhotosDoc::library(&doc).photo(&id).is_some());
        assert_eq!(app.library_remove_confirm.as_deref(), Some(id.as_str()));
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        settle(&mut app, &mut doc);
        assert!(PhotosDoc::library(&doc).photo(&id).is_none());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn accessible_toolbars_menus_and_metadata_sheet_route_real_controls() {
    eprintln!("phase accessibility_exposes_and_routes_every_toolbar_control");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        sync(&mut app, &mut doc);

        let empty = app.accessibility_tree();
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.children[0].role, Role::Toolbar);
        assert_eq!(
            empty.children[0].children.len(),
            SLOTS
                .iter()
                .filter(|slot| matches!(slot, Slot::Item(_)))
                .count()
        );
        assert_eq!(empty.children[1].role, Role::Image);
        assert_eq!(
            empty.children[0].children[0].action_target.as_deref(),
            Some("photos:toolbar:photos-open")
        );
        assert!(
            empty.children[0]
                .children
                .iter()
                .find(|node| {
                    node.action_target.as_deref() == Some("photos:toolbar:photos-rotate-right")
                })
                .is_some_and(|node| node.state.disabled),
            "rotate must be unavailable without a photograph"
        );
        assert!(
            !app.perform_toolbar_semantic_action(
                "photos:toolbar:photos-rotate-right",
                &AccessAction::Activate
            ),
            "a disabled semantic control must reject activation"
        );

        let image = OpenImage::from_path("/tmp/accessible.png", 100, 50);
        app.show_photo(&PhotosDoc::put_image(&mut doc, &image));
        app.original = Some(("/tmp/accessible.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        present(&mut app, &doc);
        app.refresh();
        sync(&mut app, &mut doc);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "photos:toolbar:photos-rotate-right",
            &AccessAction::Activate,
            &mut doc,
            &ROOT,
        ));
        settle(&mut app, &mut doc);
        assert_eq!(
            PhotosDoc::photo_edits(&doc, app.open_photo_id().unwrap()).rotate_quarters,
            1
        );

        sync(&mut app, &mut doc);
        let opened = app.accessibility_tree();
        assert_eq!(
            opened.children[1].name.as_deref(),
            Some("accessible.png — edited")
        );
        let status = opened
            .children
            .last()
            .expect("opened image has status text");
        assert_eq!(status.role, Role::StaticText);
        assert_eq!(status.live, Some(Politeness::Polite));
    }

    eprintln!("phase compact_more_is_a_keyboard_menu_with_a_share_action");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("/tmp/compact.png", 100, 50);
        app.show_photo(&PhotosDoc::put_image(&mut doc, &image));
        app.original = Some(("/tmp/compact.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        sync(&mut app, &mut doc);
        app.compact_more_open = true;

        // Crop and Reset start disabled, so Down must land on Copy rather
        // than trapping a keyboard user on unavailable commands.
        app.step_compact_more_focus(true);
        assert_eq!(app.focused_compact_more_hit(), Some(Hit::Copy));
        app.step_compact_more_focus(true);
        assert_eq!(app.focused_compact_more_hit(), Some(Hit::Share));

        let toolbar = app.toolbar_semantics();
        let menu = toolbar
            .children
            .iter()
            .find(|node| node.role == Role::Menu)
            .expect("narrow overflow exposes a menu");
        assert!(menu.children.iter().any(|item| {
            item.action_target.as_deref() == Some("photos:toolbar:photos-share")
                && item.state.focused
                && item.state.focusable
        }));
    }

    eprintln!("phase metadata_sheet_focus_tracks_each_adaptive_section");
    {
        let mut app = test_app();
        for focus in 0..5 {
            app.set_metadata_editor_focus(focus);
            assert_eq!(app.metadata_editor_focus, focus);
            assert_eq!(app.metadata_folder_input.is_focused(), focus == 0);
            assert_eq!(app.metadata_people_input.is_focused(), focus == 1);
            assert_eq!(app.metadata_tags_input.is_focused(), focus == 2);
        }
        app.set_metadata_editor_focus(99);
        assert_eq!(app.metadata_editor_focus, 4);
        assert!(
            !app.metadata_folder_input.is_focused()
                && !app.metadata_people_input.is_focused()
                && !app.metadata_tags_input.is_focused()
                && !app.metadata_captured_at_input.is_focused(),
            "button focus must leave a single semantic keyboard owner"
        );

        app.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
        for focus in 0..6 {
            app.set_metadata_editor_focus(focus);
            assert_eq!(app.metadata_editor_focus, focus);
            assert_eq!(app.metadata_captured_at_input.is_focused(), focus == 0);
            assert_eq!(app.metadata_place_input.is_focused(), focus == 1);
            assert_eq!(app.metadata_latitude_input.is_focused(), focus == 2);
            assert_eq!(app.metadata_longitude_input.is_focused(), focus == 3);
        }
        app.set_metadata_editor_focus(99);
        assert_eq!(app.metadata_editor_focus, 5);
    }

    eprintln!("phase metadata_sheet_footer_never_covers_a_focused_field");
    {
        for (scale, (w, h)) in [(1.0_f32, (1200.0_f32, 800.0_f32)), (2.0, (800.0, 900.0))] {
            let mut app = test_app();
            let mut doc = AutoCommit::new();
            app.show_library();
            stage(&mut app, &mut doc, "metadata-edit");
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(theme.text_scale);
            for frame in 0..3 {
                if frame == 1 {
                    assert!(
                        app.metadata_editor,
                        "the metadata-edit scenario opens the sheet"
                    );
                    app.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
                    app.set_metadata_editor_focus(3);
                }
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                sync(&mut app, &mut doc);
                let child = app.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy
                    .new_with_children(
                        Style {
                            size: Size {
                                width: Dimension::length(w),
                                height: Dimension::length(h),
                            },
                            ..Default::default()
                        },
                        &[child],
                    )
                    .unwrap();
                let measure_map = node_map.clone();
                let trr = &mut tr;
                taffy
                    .compute_layout_with_measure(
                        root,
                        Size {
                            width: taffy::AvailableSpace::Definite(w),
                            height: taffy::AvailableSpace::Definite(h),
                        },
                        |known, available, id, _, _| {
                            measure_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                                (*p).measure(known, available, trr)
                            })
                        },
                    )
                    .unwrap();
                app.apply_layout(&taffy, child, Point::ORIGIN);
                let mut scene = Scene::new();
                app.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
            let body = app.metadata_editor_body;
            let captured = app.metadata_longitude_input.get_rect();
            assert!(
                captured.y0 >= body.y0 - 0.01 && captured.y1 <= body.y1 + 0.01,
                "{scale}×: the focused Longitude field {captured:?} is inside the visible body {body:?}"
            );
            let footer: Vec<_> = app
                .metadata_editor_buttons
                .iter()
                .filter(|(hit, _)| {
                    matches!(hit, MetadataEditorHit::Cancel | MetadataEditorHit::Save)
                })
                .map(|(_, button)| button.get_rect())
                .collect();
            assert_eq!(footer.len(), 2);
            assert!(
                footer.iter().all(|rect| rect.y0 >= body.y1 - 0.01),
                "{scale}×: retained Cancel and Save sit below the visible body {body:?}: {footer:?}"
            );
            if (scale - 1.0).abs() < f32::EPSILON {
                assert!(
                    app.metadata_editor_scroll.abs() < f64::EPSILON,
                    "1× holds the whole sheet without scrolling"
                );
            } else {
                assert!(
                    app.metadata_editor_scroll > 0.0,
                    "2× on 800×900 needs the field list to scroll to reveal Longitude"
                );
            }
        }
    }

    eprintln!(
        "phase opened_viewer_exposes_shared_context_without_creating_a_second_metadata_store"
    );
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("/tmp/harbour.png", 1_200, 800);
        app.show_photo(&PhotosDoc::put_image(&mut doc, &image));
        let mut library = PhotosDoc::library(&doc);
        let photo = library
            .photos
            .first_mut()
            .expect("opening creates a library record");
        photo.captured_at = Some("2026-09-19 at dusk".to_owned());
        photo.people = vec!["Ada".to_owned(), "Mina".to_owned()];
        photo.tags = vec!["harbour".to_owned(), "travel".to_owned()];
        photo.folder = Some("Weekends".to_owned());
        photo.location = Some(crate::model::PhotoLocation {
            latitude: 52.0,
            longitude: 4.0,
            label: Some("Harbour front".to_owned()),
            provenance: crate::model::LocationProvenance::default(),
        });
        photo.analysis = Some(crate::model::ImageAnalysis {
            description: "Two friends walking beside the harbour at dusk".to_owned(),
            ..Default::default()
        });
        PhotosDoc::set_library(&mut doc, &library);
        let before = PhotosDoc::library(&doc);
        app.original = Some((image.path.clone(), shots_pattern(1_200, 800)));
        present(&mut app, &doc);
        app.refresh();

        let canvas = app.accessibility_tree().children[1].clone();
        let description = canvas
            .description
            .expect("open image carries its shared context");
        for expected in [
            "Two friends walking beside the harbour at dusk",
            "2026-09-19 at dusk",
            "Harbour front",
            "Ada, Mina",
            "harbour, travel",
            "Weekends",
        ] {
            assert!(
                description.contains(expected),
                "missing {expected:?}: {description}"
            );
        }
        let refreshed = PhotosDoc::library(&doc);
        assert_eq!(
            refreshed, before,
            "viewer presentation must not write metadata"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn in_app_search_registers_pages_and_keeps_the_last_good_index() {
    eprintln!("phase the_system_searches_the_library_and_the_window_keeps_its_own_filter");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("/pictures/München-夏.png", 800, 400);
        app.show_photo(&PhotosDoc::put_image(&mut doc, &image));
        sync(&mut app, &mut doc);
        let before = app.library_index.stats();
        sync(&mut app, &mut doc);
        assert_eq!(
            app.library_index.stats(),
            before,
            "unchanged sync rebuilt search"
        );

        assert!(
            ui::search::capability::search_providers(&app).is_empty(),
            "the system searches the library's own entries, not a second index"
        );
        let state = <crate::resource::Photos as ui::tilo_resource::Resource>::read(&doc).unwrap();
        let entry = <crate::resource::Photos as ui::tilo_resource::Resource>::search(&state)
            .into_iter()
            .find(|entry| entry.title.contains('夏'))
            .expect("the photograph is a search entry");
        let address = entry.action.expect("a hit opens what it found");
        assert_eq!(address.action, "view");
        assert!(
            PhotosResources
                .publish_resources(&doc)
                .iter()
                .any(|resource| has_resource_id(resource, &address.resource))
        );
    }

    eprintln!("phase in_app_full_text_search_advances_in_bounded_index_pages");
    {
        let mut doc = AutoCommit::new();
        let photos = (0..400)
            .map(|index| {
                let path = format!("needle-photo-{index:03}.png");
                LibraryPhoto::from_image(OpenImage::from_path(&path, 40, 20))
            })
            .collect();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos,
                ..Default::default()
            },
        );
        let mut app = test_app();
        app.library_index.sync(&PhotosDoc::library(&doc)).unwrap();
        app.library_query = "needle".into();

        app.advance_library_query();
        assert!(!app.library_query_complete);
        assert!(app.library_query_matches.len() <= ui::search::DEFAULT_SEARCH_RESULT_LIMIT);

        for _ in 0..16 {
            app.advance_library_query();
            if app.library_query_complete {
                break;
            }
        }
        assert!(app.library_query_complete);
        assert_eq!(app.library_query_matches.len(), 400);
    }

    eprintln!("phase rejected_search_refresh_keeps_last_good_index_and_exports_diagnostic");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let first = LibraryPhoto::from_image(OpenImage::from_path("/photos/first.png", 20, 10));
        let first_id = first.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first],
                albums: Vec::new(),
            },
        );
        sync(&mut app, &mut doc);
        assert!(app.library_index.resource(&first_id).is_some());

        let mut library = PhotosDoc::library(&doc);
        let second = LibraryPhoto::from_image(OpenImage::from_path("/photos/second.png", 20, 10));
        let second_id = second.id.clone();
        library.photos.push(second);
        PhotosDoc::set_library(&mut doc, &library);
        app.force_search_refresh_error = true;
        sync(&mut app, &mut doc);

        assert!(app.library_index.resource(&first_id).is_some());
        assert!(app.library_index.resource(&second_id).is_none());
        assert!(app.status.contains("kept its previous index"));
        let diagnostic = app
            .search_status_row()
            .expect("a rejected refresh is a row a caller can read")
            .into_resource();
        assert_eq!(diagnostic.id.0, "photos.search-status");
        assert_eq!(diagnostic.kind, "photos.diagnostic");
        assert!(
            diagnostic
                .actions
                .iter()
                .any(|action| action.id == "view-library")
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn thumbnail_queue_is_bounded_wakes_its_host_and_rejects_stale_results() {
    eprintln!("phase a_large_library_fills_a_bounded_background_thumbnail_queue");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..500 {
            library
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    &format!("/unavailable/library-{index}.png"),
                    32_000,
                    32_000,
                )));
        }
        PhotosDoc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);

        assert_eq!(app.thumbnail_visible_ids.len(), 24);
        assert!(app.thumbnail_queued.len() <= THUMBNAIL_QUEUE_LIMIT);
        assert!(app.thumbnail_queued.len() >= 2);
        assert!(app.library_thumbnails.len() <= THUMBNAIL_QUEUE_LIMIT);
        assert!(
            app.thumbnail_generation
                .is_some_and(|generation| generation > 0)
        );
    }

    eprintln!("phase a_stale_thumbnail_result_cannot_populate_a_replaced_library_slice");
    {
        let mut app = test_app();
        app.thumbnail_generation = Some(8);
        app.thumbnail_visible_ids = vec!["photos.photo:current".into()];
        app.thumbnail_queued.insert("photos.photo:current".into());
        app.accept_thumbnail_result(ThumbnailResult {
            generation: 7,
            id: "photos.photo:current".into(),
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.is_empty());
        assert!(app.thumbnail_queued.contains("photos.photo:current"));

        app.accept_thumbnail_result(ThumbnailResult {
            generation: 8,
            id: "photos.photo:current".into(),
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.contains_key("photos.photo:current"));
        assert!(!app.thumbnail_queued.contains("photos.photo:current"));
    }

    eprintln!("phase completed_thumbnail_wakes_its_idle_host_for_success_and_failure");
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thumbnail.png");
        std::fs::write(&path, pixels::encode_png(&shots_pattern(16, 8)).unwrap()).unwrap();
        let mut scope = ui::wake::WakeScope::new();
        let _entry = scope.enter();
        let (sender, receiver) = std::sync::mpsc::channel();
        scope
            .bind(move |event| {
                let _ = sender.send(event);
            })
            .unwrap();
        let app = test_app();
        for (source, succeeds) in [(path, true), (directory.path().join("missing.png"), false)] {
            let image = OpenImage::from_path(&source.to_string_lossy(), 16, 8);
            let request = thumbnail_request(&image, &app.content);
            let key = request.key();
            app.thumbnail_service.request(request);
            let event = receiver
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("thumbnail completion must wake the idle host without user input");
            assert!(scope.accepts(&event));
            let settled = app.thumbnail_service.peek(&key).unwrap();
            assert_eq!(settled.ready().is_some(), succeeds);
        }
    }

    eprintln!("phase exhausted_thumbnail_generation_fails_closed_without_reusing_a_token");
    {
        let mut app = test_app();
        app.thumbnail_generation = Some(u64::MAX);
        app.thumbnail_visible_ids = vec!["photos.photo:before".into()];
        app.library_thumbnails.insert(
            "photos.photo:before".into(),
            to_peniko(&shots_pattern(16, 8)),
        );
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/after.png", 16, 8));
        let id = photo.id.clone();

        app.schedule_library_thumbnail(vec![photo], true);
        assert_eq!(app.thumbnail_generation, None);
        assert!(app.thumbnail_queued.is_empty());
        assert!(app.library_thumbnails.is_empty());

        app.accept_thumbnail_result(ThumbnailResult {
            generation: u64::MAX,
            id,
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.is_empty());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn controls_contain_their_text_share_insets_and_keep_44px_targets() {
    eprintln!("phase toolbar_capsules_and_status_band_contain_their_text_at_any_scale");
    {
        assert!(
            (btn_h(1.0) - BTN_H).abs() < f32::EPSILON,
            "1× keeps the authored capsule"
        );
        assert!(
            (status_h(1.0) - BAR_H * 0.6).abs() < f32::EPSILON,
            "1× keeps the authored band"
        );
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let (_, label_line) = tr.measure("More", BUTTON_LABEL_SIZE, None);
        let (_, status_line) = tr.measure("harbour.png · 1200 × 800", STATUS_TEXT_SIZE, None);
        assert!(
            btn_h(2.0) >= label_line + 2.0 * BTN_PAD_Y,
            "2× capsule {} contains a {label_line} px label",
            btn_h(2.0)
        );
        assert!(
            status_h(2.0) >= status_line + 2.0 * STATUS_PAD_Y,
            "2× band {} contains a {status_line} px line",
            status_h(2.0)
        );
        // Icon-only items stay square at every scale.
        assert!((item_width(Face::IconOnly, "", btn_h(2.0)) - btn_h(2.0)).abs() < f32::EPSILON);
    }

    eprintln!("phase library_card_actions_share_one_edge_inset");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        app.show_library();
        stage(&mut app, &mut doc, "library");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        // Two frames: the first lays out roomy, the second refits the toolbar
        // to the width it produced (see `build_layout`).
        for _ in 0..2 {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            sync(&mut app, &mut doc);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(1200.0),
                            height: Dimension::length(800.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: taffy::AvailableSpace::Definite(1200.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                    |known, available, id, _, _| {
                        measure_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                            (*p).measure(known, available, trr)
                        })
                    },
                )
                .unwrap();
            app.apply_layout(&taffy, child, Point::ORIGIN);
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &ui::presence::PresenceCtx::empty(),
            );
        }
        let rects = app.library_card_action_rects();
        assert!(!rects.is_empty(), "the library scenario paints cards");
        for (card, details, _remove, star) in &rects {
            let left_inset = details.x0 - card.x0;
            let right_inset = card.x1 - star.x1;
            assert!(
                (left_inset - right_inset).abs() < 0.5,
                "card {card:?}: Details inset {left_inset} vs star inset {right_inset}"
            );
        }
    }

    eprintln!("phase regular_album_management_keeps_every_visible_control_at_44px");
    {
        let mut app = test_app();
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.push(photo.clone());
        library.albums.push(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![photo.id],
        });
        PhotosDoc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);
        app.show_library();
        app.active_album = Some("photos.album:trip".into());
        // Album management lives in the desktop masthead. Compact layouts
        // intentionally reclaim that row for the photo timeline and expose
        // the same canonical actions through their responsive overflow.
        app.canvas_rect = Rect::new(0.0, 0.0, 1_280.0, 900.0);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::empty();
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );
        assert_eq!(app.album_editor_hits.len(), 2);
        assert!(
            app.album_editor_hits
                .iter()
                .all(|(_, rect)| rect.height() >= 44.0)
        );

        app.begin_edit_album();
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );

        assert!(
            app.album_editor_hits
                .iter()
                .all(|(_, rect)| rect.height() >= 44.0)
        );
        assert!(
            app.album_page_buttons
                .iter()
                .all(|(_, button)| button.get_rect().height() >= 44.0)
        );
        assert!(app.album_editor_hits.iter().all(|(_, rect)| {
            rect.x0 >= app.canvas_rect.x0
                && rect.x1 <= app.canvas_rect.x1
                && rect.y0 >= app.canvas_rect.y0
                && rect.y1 <= app.canvas_rect.y1
        }));
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn background_work_dashboards_pause_cancel_page_and_reflow() {
    eprintln!("phase visible_analysis_pause_uses_the_published_operation_envelope");
    {
        let mut app = test_app();
        app.library_analysis.begin(
            tilo_actions::InvocationId::new(),
            ["photos.photo:one".to_owned()],
            0,
            "Verified model".to_owned(),
            "r1".to_owned(),
        );
        assert!(app.activate_library_analysis(LibraryAnalysisHit::Pause));
        assert_eq!(
            app.library_analysis.phase,
            crate::library_analysis::LibraryAnalysisPhase::Paused
        );
    }

    eprintln!("phase analysis_dashboard_reflows_to_phone_width_with_real_shared_controls");
    {
        let mut app = test_app();
        app.show_library_analysis();
        app.canvas_rect = Rect::new(0.0, 0.0, 390.0, 760.0);
        app.library_analysis_coverage = (12, 34, 5);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_library_analysis(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let start = app
            .library_analysis_buttons
            .iter()
            .find(|(hit, _)| *hit == LibraryAnalysisHit::Start)
            .map(|(_, button)| button.get_rect())
            .expect("start control");
        assert!(start.width() >= 350.0);
        assert!(start.height() >= 44.0);
        assert!(start.x0 >= app.canvas_rect.x0 && start.x1 <= app.canvas_rect.x1);
        let tree = app.accessibility_tree();
        assert!(
            tree.children
                .iter()
                .any(|node| node.name.as_deref() == Some("Photo analysis"))
        );
    }

    eprintln!("phase folder_status_effect_and_semantic_stop_share_cancellation");
    {
        let mut app = test_app();
        app.canvas_rect = Rect::new(0.0, 48.0, 1_000.0, 720.0);
        app.rect = Rect::new(0.0, 0.0, 1_000.0, 768.0);
        app.show_directory_import_progress_for_shot();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_directory_import_status(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
        let stop = app
            .directory_import_buttons
            .iter()
            .find(|(hit, _)| *hit == DirectoryStatusHit::Stop)
            .map(|(_, button)| button.get_rect())
            .expect("stop button");
        assert!(stop.width() >= 44.0 && stop.height() >= 40.0);

        let mut doc = AutoCommit::new();
        app.answer_effect(
            PhotosEffect::StopImport,
            tilo_actions::InvocationId::new(),
            &PhotosDoc::library(&doc),
        )
        .expect("the stop effect is answered");
        sync(&mut app, &mut doc);
        let job = app
            .directory_import
            .as_ref()
            .expect("job remains until worker exits");
        assert!(job.cancelled);
        assert!(job.cancel.load(Ordering::Acquire));

        app.show_directory_import_progress_for_shot();
        assert!(app.perform_directory_semantic_action(
            "photos:directory-import:stop",
            &AccessAction::Activate,
        ));
        let job = app
            .directory_import
            .as_ref()
            .expect("semantic stop keeps receipt pending");
        assert!(job.cancelled && job.cancel.load(Ordering::Acquire));
    }

    eprintln!("phase folder_issue_details_are_bounded_paged_and_semantically_operable");
    {
        let mut app = test_app();
        app.rect = Rect::new(0.0, 0.0, 1_000.0, 760.0);
        app.canvas_rect = Rect::new(0.0, 48.0, 1_000.0, 730.0);
        app.show_directory_import_errors_for_shot();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_directory_import_details(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
        let receipt = app.directory_import_receipt.as_ref().expect("receipt");
        assert!(receipt.failures.len() <= DIRECTORY_FAILURE_DETAIL_LIMIT);
        assert_eq!(receipt.failure_count, 27);
        let dialog = app
            .accessibility_tree()
            .children
            .into_iter()
            .find(|node| node.role == Role::Dialog)
            .expect("accessible details dialog");
        assert!(dialog.state.modal);
        assert_eq!(
            dialog
                .children
                .iter()
                .filter(|node| node.role == Role::ListItem)
                .count(),
            DIRECTORY_FAILURES_PER_PAGE
        );
        assert!(
            dialog
                .description
                .as_deref()
                .is_some_and(|text| text.contains("18 additional"))
        );
        assert!(dialog.children.iter().all(|node| {
            !node.name.as_deref().unwrap_or_default().contains('/')
                && !node
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .contains("/home/")
        }));
        assert!(app.perform_directory_semantic_action(
            "photos:directory-import:details-next",
            &AccessAction::Activate,
        ));
        assert_eq!(app.directory_import_details_page, 1);
        assert!(app.perform_directory_semantic_action(
            "photos:directory-import:details-close",
            &AccessAction::Activate,
        ));
        assert!(!app.directory_import_details_open);
        assert!(app.perform_directory_semantic_action(
            "photos:directory-import:dismiss",
            &AccessAction::Activate,
        ));
        assert!(app.directory_import_receipt.is_none());
    }

    eprintln!("phase folder_status_and_details_adapt_to_phone_geometry");
    {
        let mut app = test_app();
        app.rect = Rect::new(0.0, 0.0, 390.0, 780.0);
        app.canvas_rect = Rect::new(0.0, 48.0, 390.0, 750.0);
        app.show_directory_import_progress_for_shot();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_directory_import_status(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::high_contrast(),
        );
        let controls = app
            .directory_import_buttons
            .iter()
            .filter_map(|(_, button)| {
                let rect = button.get_rect();
                (rect.area() > 0.0).then_some(rect)
            })
            .collect::<Vec<_>>();
        assert_eq!(controls.len(), 2);
        assert!(controls.iter().all(|rect| {
            rect.width() >= 44.0
                && rect.height() >= 40.0
                && rect.x0 >= app.canvas_rect.x0
                && rect.x1 <= app.canvas_rect.x1
        }));
        assert!(controls[0].x1 <= controls[1].x0 || controls[1].x1 <= controls[0].x0);

        app.show_directory_import_errors_for_shot();
        app.draw_directory_import_details(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::high_contrast(),
        );
        let layout = app
            .directory_import_details_layout
            .as_ref()
            .expect("phone details layout");
        assert!(layout.card.x0 >= app.rect.x0 && layout.card.x1 <= app.rect.x1);
        assert!(layout.card.y0 >= app.rect.y0 && layout.card.y1 <= app.rect.y1);
        assert!(layout.close.is_some_and(|rect| rect.width() >= 32.0));
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn edited_content_effect_returns_exact_pngs_and_rejects_bad_sources() {
    eprintln!("phase edited_content_effect_stores_and_returns_the_exact_transformed_png");
    {
        let original = pixels::Rgba::new(
            3,
            2,
            vec![
                1, 10, 0, 255, 2, 10, 0, 255, 3, 10, 0, 255, 1, 20, 0, 255, 2, 20, 0, 255, 3, 20,
                0, 255,
            ],
        )
        .expect("asymmetric source");
        let source_png = pixels::encode_png(&original).expect("source PNG");
        let store = tilo_content::ContentStore::memory();
        let source_hash = store.put(&source_png).expect("stage source content");
        let open = OpenImage::from_path("fixture.png", 3, 2).with_content(source_hash.clone());
        let edits = crate::model::Edits {
            crop: crate::model::Rect01 {
                x: 1.0 / 3.0,
                y: 0.0,
                w: 2.0 / 3.0,
                h: 1.0,
            },
            rotate_quarters: 1,
        };
        let mut doc = AutoCommit::new();
        let resource_id = PhotosDoc::put_image(&mut doc, &open);
        PhotosDoc::set_photo_edits(&mut doc, &resource_id, edits);

        let resources = PhotosResources.publish_resources(&doc);
        let exported = tilo_actions::flatten(&resources)
            .into_iter()
            .find(|entry| {
                entry.resource_id.as_str() == resource_id
                    && entry.action.id == crate::actions::EXPORT_EDITED_CONTENT_ACTION_ID
            })
            .expect("addressed image exports edited content");
        assert_eq!(exported.action.kind, tilo_actions::OperationKind::Effect);
        assert_eq!(
            exported.action.contract.retry,
            tilo_actions::RetryPolicy::Idempotent
        );
        assert_eq!(
            exported.action.contract.result,
            tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            }
        );

        let payload = PhotosEffect::ExportEditedContent {
            resource_id: resource_id.clone(),
            source_content: source_hash.clone(),
            edits,
        };
        let mut app = editing(PhotosApp::with_content(store.clone()));
        let started = app
            .answer_effect(
                payload.clone(),
                tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc),
            )
            .expect("export succeeds");
        let Some(output) = started else {
            panic!("bounded content production completes with its live output");
        };
        assert_eq!(output.resources.len(), 1);
        let produced_hash =
            tilo_actions::content_hash(&output.resources[0]).expect("one canonical content result");
        assert_ne!(
            produced_hash, source_hash,
            "edited bytes must not reuse the source hash"
        );
        let stored = store
            .read(produced_hash)
            .expect("produced bytes are retrievable");
        let decoded = pixels::decode(&stored).expect("produced PNG decodes");
        assert_eq!(
            decoded,
            pixels::render(&original, edits.crop, edits.rotate_quarters)
        );
        // The rendering is an output: the photograph is still its original plus
        // the edits recorded against it.
        assert_eq!(
            PhotosDoc::photo(&doc, &resource_id).and_then(|photo| photo.image.content),
            Some(source_hash)
        );
        assert_eq!(PhotosDoc::photo_edits(&doc, &resource_id), edits);

        PhotosDoc::set_photo_edits(&mut doc, &resource_id, crate::model::Edits::default());
        assert!(
            app.answer_effect(
                payload,
                tilo_actions::InvocationId::new(),
                &PhotosDoc::library(&doc)
            )
            .is_err(),
            "a stale edit snapshot must not produce different content"
        );
    }

    eprintln!("phase edited_content_effect_rejects_missing_or_misdirected_source_bytes");
    {
        let missing = "f".repeat(64);
        let open = OpenImage::from_path("missing.png", 3, 2).with_content(missing.clone());
        let mut doc = AutoCommit::new();
        let photo_id = PhotosDoc::put_image(&mut doc, &open);
        let edits = PhotosDoc::photo_edits(&doc, &photo_id);
        let encoded = |resource_id: String| PhotosEffect::ExportEditedContent {
            resource_id,
            source_content: missing.clone(),
            edits,
        };
        let mut app = test_app();
        for payload in [
            encoded(photo_id),
            encoded("photos.photo:wrong-target".into()),
        ] {
            assert!(
                app.answer_effect(
                    payload.clone(),
                    tilo_actions::InvocationId::new(),
                    &PhotosDoc::library(&doc)
                )
                .is_err()
            );
        }
    }
}
