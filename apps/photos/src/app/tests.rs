use super::*;
use crate::actions::{PHOTOS_OPERATION_TYPE, PhotosOperation, PhotosResources};
use crate::model::{Album, LibraryPhoto};
use crate::shots_pattern;
use automerge::ROOT;
use tilo_actions::Action;

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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let waiting =
            OpenImage::from_path("/not-present/yet.png", 1200, 800).with_content("0".repeat(64));
        PhotosDoc::set_image(&mut doc, Some(&waiting));
        app.sync(&mut doc, &ROOT);
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
        PhotosDoc::set_image(&mut doc, Some(&bad));
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.canvas_state, CanvasState::Error);
        assert_eq!(app.empty_state_text().0, tr("photos-error-title"));
    }

    eprintln!("phase places_camera_feedback_is_presentation_only_and_respects_reduced_motion");
    {
        let mut app = PhotosApp::new();
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        assert!(app.approve_people_cluster(&mut doc, "manual-person"));
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.handle_people_intent(PeopleIntent::ApproveSuggestion, &mut doc);
        assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Sam"]);
        assert!(
            app.people_controller().cluster("review-me").unwrap().hidden,
            "an approved local proposal leaves the active review queue"
        );

        app.select_people_item(PeopleSelection::Approved("Sam".to_owned()));
        app.people_name_input.set_text("Samuel");
        app.handle_people_intent(PeopleIntent::SaveName, &mut doc);
        assert_eq!(
            PhotosDoc::photo(&doc, &first_id).unwrap().people,
            ["Samuel"]
        );
        app.handle_people_intent(PeopleIntent::MergeApprovedInto("Alex".to_owned()), &mut doc);
        assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Alex"]);

        app.select_people_item(PeopleSelection::Approved("Alex".to_owned()));
        app.handle_people_intent(PeopleIntent::BrowseApproved, &mut doc);
        assert_eq!(app.active_person.as_deref(), Some("Alex"));
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
        assert!(!app.people_mode);
    }

    eprintln!("phase people_rejection_is_reversible_and_accessibility_activates_the_same_control");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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

        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
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
        PhotosDoc::seed(&mut doc);
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
        let mut app = PhotosApp::new();
        app.view_timeline(
            &doc,
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
        let mut app = PhotosApp::new();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);

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
        app.drain_analysis_results(&mut doc);
        assert!(app.resource_result.is_none());

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
        app.drain_analysis_results(&mut doc);
        assert!(matches!(
            app.resource_result,
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "photos.analysis_stale"
        ));
        assert!(PhotosDoc::library(&doc).photos.is_empty());
    }

    eprintln!("phase drain_preserves_ocr_and_vision_components_from_separate_jobs");
    {
        let mut app = PhotosApp::new();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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

        app.drain_analysis_results(&mut doc);
        let analysis = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
        assert_eq!(analysis.ocr_text, "receipt forty two");
        assert_eq!(analysis.description, "A bicycle beside a lake");
        assert_eq!(analysis.objects, ["bicycle"]);
        assert_eq!(analysis.ocr_provenance.unwrap().source, "local-ocr");
        assert_eq!(analysis.vision_provenance.unwrap().source, "local-vision");
    }

    eprintln!("phase library_analysis_commits_each_real_result_with_provenance_and_finishes");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.drain_library_analysis_result(
            AnalysisResult {
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
            },
            &mut doc,
        );
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
            app.resource_result,
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(ui::undo::take_committed_action(&mut app).is_some());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn density_changes_share_one_event_path_and_reanchor_the_first_photo() {
    eprintln!("phase pinch_and_ctrl_wheel_use_the_same_density_event_path");
    {
        use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
        use winit::keyboard::ModifiersState;

        let mut app = PhotosApp::new();
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
        let mut app = PhotosApp::new();
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
        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);

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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:trip".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    PHOTOS_OPERATION_TYPE,
                    &payload.payload
                ),
                &mut doc,
                &ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "navigation must not alter membership"
        );

        let stale = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:gone".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    PHOTOS_OPERATION_TYPE,
                    &stale.payload
                ),
                &mut doc,
                &ROOT
            )
            .is_err()
        );
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    eprintln!("phase opening_favorites_uses_a_local_view_and_refuses_an_empty_stale_collection");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewFavorites,
        )
        .expect("favorite view operation encodes");
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    PHOTOS_OPERATION_TYPE,
                    &payload.payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("favorite collection is present"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(app.library_mode);
        assert!(app.active_favorites);
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "opening a view is not a write"
        );

        PhotosDoc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        assert!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    PHOTOS_OPERATION_TYPE,
                    &payload.payload
                ),
                &mut doc,
                &ROOT
            )
            .is_err()
        );
    }

    eprintln!("phase metadata_views_are_local_filters_with_honest_stale_results");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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

        app.view_metadata_filter(&doc, Some("Travel".into()), None, None)
            .expect("existing folder enters a view");
        app.sync(&mut doc, &ROOT);
        assert!(app.library_mode);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_folder.as_deref(), Some("Travel"));
        assert!(app.active_person.is_none());

        app.view_metadata_filter(&doc, None, Some("Élodie".into()), None)
            .expect("existing person enters a view");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_person.as_deref(), Some("Élodie"));
        assert!(app.active_folder.is_none());
        app.view_metadata_filter(&doc, None, None, Some("golden-hour".into()))
            .expect("existing tag enters a view");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_tag.as_deref(), Some("golden-hour"));
        assert!(app.active_folder.is_none());
        assert!(app.active_person.is_none());
        assert!(
            app.view_metadata_filter(&doc, Some("Missing".into()), None, None)
                .is_err()
        );
    }

    eprintln!("phase people_places_and_albums_keep_complete_paged_collections");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);

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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.set_selected_favorite(&mut doc, true);
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
        app.add_selected_to_destination_album(&mut doc);
        let album = PhotosResources
            .publish_resource(&doc, "photos.album:trip")
            .expect("chosen album remains published");
        let membership = album
            .actions
            .into_iter()
            .find(|action| action.id == "set-membership")
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        let mut left = PhotosApp::new();
        let mut right = PhotosApp::new();
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ShareReference {
                resource_id: "photos.photo:missing".into(),
            },
        )
        .expect("share operation encodes");

        let error = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
            ),
            &mut doc,
            &ROOT,
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
        let mut app = PhotosApp::new();
        let doc = AutoCommit::new();
        let invocation = tilo_actions::InvocationId::new();
        let (tx, rx) = std::sync::mpsc::channel();
        app.picker = Some(rx);
        app.resource_invocation = Some(invocation);

        let update = Widget::poll_resource_operation(&mut app, invocation, true, &doc, &ROOT)
            .expect("cancellation must resolve the accepted invocation");
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Cancelled { invocation: id, .. } if id == invocation
        ));
        assert!(app.picker.is_none());
        assert!(app.resource_invocation.is_none());
        assert!(
            tx.send(ui::portal::PortalOutcome::Opened("/late.png".into()))
                .is_err(),
            "a late portal completion must have no receiver after cancellation"
        );

        let direct = tilo_actions::InvocationId::new();
        app.pending_resource_open = Some((direct, "/late-direct.png".into()));
        assert!(matches!(
            Widget::poll_resource_operation(&mut app, direct, true, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Cancelled { invocation, .. })
                if invocation == direct
        ));
        assert!(app.pending_resource_open.is_none());
    }

    eprintln!("phase library_removal_requires_confirmation_and_routes_through_the_action_host");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);
        let target = format!("photos:library-remove:{id}");
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        assert!(PhotosDoc::library(&doc).photo(&id).is_some());
        assert_eq!(app.library_remove_confirm.as_deref(), Some(id.as_str()));
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        assert!(PhotosDoc::library(&doc).photo(&id).is_none());
        assert!(ui::undo::take_committed_action(&mut app).is_some());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn accessible_toolbars_menus_and_metadata_sheet_route_real_controls() {
    eprintln!("phase accessibility_exposes_and_routes_every_toolbar_control");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        app.sync(&mut doc, &ROOT);

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
                &AccessAction::Activate,
                &mut doc,
            ),
            "a disabled semantic control must reject activation"
        );

        let image = OpenImage::from_path("/tmp/accessible.png", 100, 50);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.original = Some(("/tmp/accessible.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        app.refresh(&doc);
        app.sync(&mut doc, &ROOT);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "photos:toolbar:photos-rotate-right",
            &AccessAction::Activate,
            &mut doc,
            &ROOT,
        ));
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
        assert!(
            ui::undo::take_committed_action(&mut app).is_some(),
            "the semantic route must create the normal undo boundary"
        );

        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/tmp/compact.png", 100, 50);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.original = Some(("/tmp/compact.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
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
            let mut app = PhotosApp::new();
            let mut doc = AutoCommit::new();
            app.show_library();
            let staging = tempfile::tempdir().expect("staging dir");
            crate::seed_scenario(&mut app, &mut doc, "metadata-edit", staging.path());
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
                app.sync(&mut doc, &automerge::ROOT);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/tmp/harbour.png", 1_200, 800);
        PhotosDoc::set_image(&mut doc, Some(&image));
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
        app.refresh(&doc);

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

#[test]
fn in_app_search_registers_pages_and_keeps_the_last_good_index() {
    eprintln!("phase synchronized_app_registers_the_canonical_photo_search_provider");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/pictures/München-夏.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.sync(&mut doc, &ROOT);
        let before = app.search_provider.stats();
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.search_provider.stats(),
            before,
            "unchanged sync rebuilt search"
        );

        let providers = ui::search::capability::search_providers(&app);
        assert_eq!(providers.len(), 1);
        let hit = providers[0].search("夏").pop().unwrap();
        let address = hit.action_address().unwrap();
        assert!(matches!(address.action.as_str(), "view" | "open"));
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
        PhotosDoc::seed(&mut doc);
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
        let mut app = PhotosApp::new();
        app.search_provider.sync(&doc).unwrap();
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let first = LibraryPhoto::from_image(OpenImage::from_path("/photos/first.png", 20, 10));
        let first_id = first.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first],
                albums: Vec::new(),
            },
        );
        app.sync(&mut doc, &ROOT);
        assert!(app.search_provider.resource(&first_id).is_some());

        let mut library = PhotosDoc::library(&doc);
        let second = LibraryPhoto::from_image(OpenImage::from_path("/photos/second.png", 20, 10));
        let second_id = second.id.clone();
        library.photos.push(second);
        PhotosDoc::set_library(&mut doc, &library);
        app.force_search_refresh_error = true;
        app.sync(&mut doc, &ROOT);

        assert!(app.search_provider.resource(&first_id).is_some());
        assert!(app.search_provider.resource(&second_id).is_none());
        assert!(app.status.contains("kept its previous index"));
        let diagnostic = ui::resources::resource(&app, &doc, "photos.search-status")
            .expect("rejected refresh is an exact addressable resource");
        assert_eq!(diagnostic.kind, "photos.diagnostic");
        assert!(diagnostic.actions.iter().any(|action| action.id == "open"));
        assert!(
            ui::resources::resources(&app, &doc)
                .iter()
                .any(|resource| resource.id.0 == "photos.search-status")
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn thumbnail_queue_is_bounded_wakes_its_host_and_rejects_stale_results() {
    eprintln!("phase a_large_library_fills_a_bounded_background_thumbnail_queue");
    {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        app.sync(&mut doc, &ROOT);

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
        let mut app = PhotosApp::new();
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
        let app = PhotosApp::new();
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
        let mut app = PhotosApp::new();
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        app.show_library();
        let staging = tempfile::tempdir().expect("staging dir");
        crate::seed_scenario(&mut app, &mut doc, "library", staging.path());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        // Two frames: the first lays out roomy, the second refits the toolbar
        // to the width it produced (see `build_layout`).
        for _ in 0..2 {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            app.sync(&mut doc, &automerge::ROOT);
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
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.push(photo.clone());
        library.albums.push(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![photo.id],
        });
        PhotosDoc::set_library(&mut doc, &library);
        app.sync(&mut doc, &ROOT);
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

        app.begin_edit_album(&doc);
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
        let mut app = PhotosApp::new();
        let doc = AutoCommit::new();
        app.library_analysis.begin(
            tilo_actions::InvocationId::new(),
            ["photos.photo:one".to_owned()],
            0,
            "Verified model".to_owned(),
            "r1".to_owned(),
        );
        assert!(app.activate_library_analysis(LibraryAnalysisHit::Pause, &doc));
        assert_eq!(
            app.library_analysis.phase,
            crate::library_analysis::LibraryAnalysisPhase::Paused
        );
        let encoded = app
            .last_library_action
            .as_ref()
            .expect("visible control records its canonical operation");
        assert_eq!(
            crate::actions::decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(crate::actions::PhotosOperation::PauseLibraryAnalysis)
        );
    }

    eprintln!("phase analysis_dashboard_reflows_to_phone_width_with_real_shared_controls");
    {
        let mut app = PhotosApp::new();
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

    eprintln!("phase folder_status_pointer_and_semantic_stop_share_cancellation");
    {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

        let mut app = PhotosApp::new();
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
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut focus = None;
        let modifiers = winit::event::Modifiers::default();
        for event in [
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(stop.center().x, stop.center().y),
            },
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            // The host creates a fresh propagation flag for every window event.
            // Reusing the flag across this synthetic sequence makes the initial
            // cursor move suppress the later press/release and never exercises
            // the stop action at all.
            let mut handled = false;
            app.handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut doc,
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &tree,
                },
            );
        }
        let invocation = ui::widget::controls::take_control_invocation(&mut app)
            .expect("pointer stop is exported through the host action door");
        assert_eq!(
            invocation.address.resource,
            "tilo.control:photos-directory-stop"
        );
        assert_eq!(invocation.address.action, "invoke");
        let payload = serde_json::to_vec("photos-directory-stop").unwrap();
        let started = app
            .start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &payload,
                ),
                &mut doc,
                &ROOT,
            )
            .expect("host dispatches the retained button")
            .expect("retained button accepts its operation");
        assert!(matches!(
            started,
            ui::widget::ResourceOperationStart::Completed(_)
        ));
        app.sync(&mut doc, &ROOT);
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
        let mut app = PhotosApp::new();
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
        let mut app = PhotosApp::new();
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
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(&mut doc, Some(&open));
        PhotosDoc::set_edits(&mut doc, &edits);
        let resource_id = crate::actions::resource_id(&open);

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

        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ExportEditedContent {
                resource_id: resource_id.clone(),
                source_content: source_hash.clone(),
                edits,
            },
        )
        .expect("export operation encodes");
        let mut app = PhotosApp::with_content(store.clone());
        let started = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
            ),
            &mut doc,
            &ROOT,
        )
        .expect("export succeeds")
        .expect("Photos handles its operation");
        let ui::widget::ResourceOperationStart::Completed(output) = started else {
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
            PhotosDoc::image(&doc).and_then(|image| image.content),
            Some(source_hash)
        );
        assert_eq!(PhotosDoc::edits(&doc), edits);

        PhotosDoc::set_edits(&mut doc, &crate::model::Edits::default());
        assert!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    PHOTOS_OPERATION_TYPE,
                    &payload.payload
                ),
                &mut doc,
                &ROOT
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
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(&mut doc, Some(&open));
        let edits = PhotosDoc::edits(&doc);
        let encoded = |resource_id: String| {
            tilo_actions::EncodedAction::encode_operation(
                PHOTOS_OPERATION_TYPE,
                &PhotosOperation::ExportEditedContent {
                    resource_id,
                    source_content: missing.clone(),
                    edits,
                },
            )
            .expect("export operation encodes")
        };
        let mut app = PhotosApp::new();
        for payload in [
            encoded(crate::actions::resource_id(&open)),
            encoded("photos.image:wrong-target".into()),
        ] {
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        PHOTOS_OPERATION_TYPE,
                        &payload.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .is_err()
            );
        }
    }
}
