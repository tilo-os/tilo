use super::*;
use std::sync::atomic::Ordering;

use taffy::prelude::Size;
use ui::a11y::{Action as AccessAction, Politeness};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::DateIndexIntent;
use vello::Scene;
use vello::kurbo::Affine;

use super::jobs::thumbnail_request;
use crate::timeline_projection::{TimelineGeometry, TimelinePhoto};

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
    let state =
        <PhotoLibrary as ui::tilo_resource::Resource>::read(doc).expect("the library reads");
    app.adopt_resource_state(&Rc::new(state));
}
use crate::actions::PhotosEffect;
use crate::model::{Album, LibraryPhoto};
use crate::shots_pattern;
use automerge::ROOT;
use ui::tilo_resource::{Invoke, Resource as _};

/// Apply `invoke` to `doc`, as the door does.
fn apply(doc: &mut AutoCommit, invoke: &Invoke<PhotoLibrary>) -> Result<(), String> {
    let action = invoke.build(0)?;
    tilo_actions::Action::apply(&action, doc).map_err(|error| error.to_string())
}

/// What `invoke` invokes, to compare two.
fn invocation(
    invoke: Option<&Invoke<PhotoLibrary>>,
) -> Option<(&'static str, serde_json::Map<String, serde_json::Value>)> {
    invoke.map(|invoke| (invoke.id, invoke.arguments.clone()))
}

/// The library as published, with every child resource.
fn published(doc: &AutoCommit) -> tilo_actions::Resource {
    let library = crate::test_doc::library(doc);
    ui::tilo_resource::publish::<PhotoLibrary>("photos.library", "Photos", &library)
}

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
        apply(doc, &action).expect("the scenario's actions apply");
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

/// Lay the header row out at `width`, as a frame would.
fn lay_out_header(app: &mut PhotosApp, width: f64) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    app.rect = Rect::new(0.0, 0.0, width, 800.0);
    app.chrome.drawer.scale_for(&theme);
    let content = app.chrome.drawer.resolve(app.rect);
    app.chrome.header = Rect::new(content.x0, 0.0, width, chrome::header_height(&theme));
    app.place_chrome(&mut text, &theme);
}

/// The names of the commands the header toolbar shows.
fn toolbar_names(app: &PhotosApp) -> Vec<String> {
    app.chrome
        .toolbar
        .accessibility()
        .map(|toolbar| {
            toolbar
                .children
                .into_iter()
                .filter_map(|node| node.name)
                .collect()
        })
        .unwrap_or_default()
}

/// The semantic route of the header command named `name`.
fn toolbar_target(app: &PhotosApp, name: &str) -> Option<String> {
    app.chrome.toolbar.accessibility().and_then(|toolbar| {
        toolbar
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some(name))
            .and_then(|node| node.action_target)
    })
}

#[test]
fn timeline_layout_stays_legible_justified_and_virtually_bounded() {
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
            header_height: 28.0,
            day_gap: 24.0,
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
}

#[test]
fn canvas_and_places_feedback_stay_honest_and_presentation_only() {
    eprintln!("phase unavailable_bytes_and_bad_bytes_have_distinct_honest_canvas_states");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let waiting = OpenImage::new("/not-present/yet.png", 1200, 800, "0".repeat(64));
        app.show_photo(&crate::test_doc::put_image(&mut doc, &waiting));
        sync(&mut app, &mut doc);
        assert_eq!(app.canvas_state, CanvasState::Loading);
        assert!(!app.has_image());
        assert_eq!(app.empty_state_text().0, tr("photos-loading-title"));
        assert_eq!(
            app.command_hits(),
            vec![Hit::Open],
            "a photo whose bytes have not arrived offers no edit or export"
        );

        let bad = OpenImage::new(
            "bad.png",
            1200,
            800,
            app.content.put(b"not image data").unwrap(),
        );
        app.show_photo(&crate::test_doc::put_image(&mut doc, &bad));
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
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::new(
            "/photos/person.png",
            20,
            10,
            tilo_content::hash_bytes(b"people-review"),
        ));
        let photo_id = photo.id.clone();
        apply(
            &mut doc,
            &Invoke::edit(&crate::resource::insert(&photo)).unwrap(),
        )
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
        assert_eq!(
            crate::test_doc::photo(&doc, &photo_id).unwrap().people(),
            ["Sam"]
        );
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
        let mut doc = crate::test_doc::new_doc();
        let first = LibraryPhoto::from_image(OpenImage::new(
            "/photos/first.png",
            20,
            10,
            tilo_content::hash_bytes(b"first-person"),
        ));
        let first_id = first.id.clone();
        let mut second = LibraryPhoto::from_image(OpenImage::new(
            "/photos/second.png",
            20,
            10,
            tilo_content::hash_bytes(b"second-person"),
        ));
        second.people = crate::model::labels(["Alex".to_owned()]);
        apply(
            &mut doc,
            &Invoke::edit(&crate::resource::insert(&first)).unwrap(),
        )
        .unwrap();
        apply(
            &mut doc,
            &Invoke::edit(&crate::resource::insert(&second)).unwrap(),
        )
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
        assert_eq!(
            crate::test_doc::photo(&doc, &first_id).unwrap().people(),
            ["Sam"]
        );
        assert!(
            app.people_controller().cluster("review-me").unwrap().hidden,
            "an approved local proposal leaves the active review queue"
        );

        app.select_people_item(PeopleSelection::Approved("Sam".to_owned()));
        app.people_name_input.set_text("Samuel");
        app.handle_people_intent(PeopleIntent::SaveName);
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::photo(&doc, &first_id).unwrap().people(),
            ["Samuel"]
        );
        app.handle_people_intent(PeopleIntent::MergeApprovedInto("Alex".to_owned()));
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::photo(&doc, &first_id).unwrap().people(),
            ["Alex"]
        );

        app.select_people_item(PeopleSelection::Approved("Alex".to_owned()));
        app.handle_people_intent(PeopleIntent::BrowseApproved);
        assert_eq!(app.active_person.as_deref(), Some("Alex"));
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
        assert!(!app.people_mode);
    }

    eprintln!("phase people_rejection_is_reversible_and_accessibility_activates_the_same_control");
    {
        let mut app = test_app();
        let doc = crate::test_doc::new_doc();
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
            &doc,
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
            &doc,
        ));
        let cluster = app.people_controller().cluster("dismiss-me").unwrap();
        assert!(!cluster.hidden && !cluster.rejected);
    }
}

#[test]
fn header_controls_never_paint_through_each_other() {
    fn overlaps(left: Rect, right: Rect) -> bool {
        left.x0 < right.x1 && left.x1 > right.x0 && left.y0 < right.y1 && left.y1 > right.y0
    }

    for (width, scale, library) in [
        (1_280.0, 1.0, true),
        (1_280.0, 1.0, false),
        (800.0, 1.0, true),
        (560.0, 1.0, false),
        (1_280.0, 2.0, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header.png");
        std::fs::write(&path, pixels::encode_png(&shots_pattern(40, 20)).unwrap()).unwrap();
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        app.open_path(&path.to_string_lossy());
        settle(&mut app, &mut doc);
        if library {
            app.show_library();
        }
        sync(&mut app, &mut doc);
        let theme = Theme::dark().with_text_scale(scale);
        let mut text = TextRenderer::new();
        text.set_text_scale(scale);
        app.rect = Rect::new(0.0, 0.0, width, 800.0);
        app.chrome.drawer.scale_for(&theme);
        let content = app.chrome.drawer.resolve(app.rect);
        app.chrome.header = Rect::new(content.x0, 0.0, width, chrome::header_height(&theme));
        app.place_chrome(&mut text, &theme);

        let mut controls = vec![("toggle".to_owned(), app.chrome.toggle)];
        controls.extend(app.toolbar_item_rects());
        controls.retain(|(_, rect)| rect.width() > 0.0);
        assert!(
            controls.len() >= 4,
            "{width}px at {scale}x: the header lost its controls: {controls:?}"
        );
        for (index, (name, rect)) in controls.iter().enumerate() {
            assert!(
                rect.x0 >= -0.5 && rect.x1 <= width + 0.5,
                "{width}px at {scale}x: `{name}` is outside the window: {rect:?}"
            );
            for (other_name, other) in controls.iter().skip(index + 1) {
                assert!(
                    !overlaps(*rect, *other),
                    "{width}px at {scale}x: `{name}` {rect:?} overlaps `{other_name}` {other:?}"
                );
            }
        }
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
            header_height: 28.0,
            day_gap: 24.0,
        });
        let markers = projection.date_markers(0.0);
        assert_eq!(markers.len(), 2, "one boundary per capture day");
        assert!(
            !markers[0].2 && markers[1].2,
            "the newest day opens the timeline; the year change after it is a major boundary"
        );
        assert!(markers[0].1 < markers[1].1);
        assert_eq!(timeline_date_label(&markers[0].0), "Feb 03, 2026");
        assert_eq!(timeline_date_label(&markers[1].0), "Dec 31, 2025");
    }

    eprintln!("phase canonical_timeline_day_operation_jumps_to_its_shared_capture_boundary");
    {
        let mut doc = crate::test_doc::new_doc();
        let mut newer = LibraryPhoto::from_image(OpenImage::fixture("newer.png", 20, 10));
        newer.captured_at = Some("2026-09-07 08:14".into());
        let mut older = LibraryPhoto::from_image(OpenImage::fixture("older.png", 20, 10));
        older.captured_at = Some("2026-08-01".into());
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![newer, older], Vec::new()),
        );
        let mut app = test_app();
        app.view_timeline(
            &crate::test_doc::library(&doc),
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
        let image = OpenImage::fixture("queue.png", 2, 2);
        for language in ["eng", "deu"] {
            sender
                .try_send(AnalysisJob {
                    invocation: tilo_actions::InvocationId::new(),
                    photo_id: "photos.photo:queue".to_owned(),
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
        let doc = crate::test_doc::new_doc();

        let cancelled = tilo_actions::InvocationId::new();
        app.cancelled_analysis.insert(cancelled);
        sender
            .send(AnalysisResult {
                invocation: cancelled,
                photo_id: "photos.photo:missing".to_owned(),
                content: String::new(),
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
                content: String::new(),
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
        assert!(crate::test_doc::library(&doc).photos.is_empty());
    }

    eprintln!("phase drain_preserves_ocr_and_vision_components_from_separate_jobs");
    {
        let mut app = test_app();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let mut doc = crate::test_doc::new_doc();
        let content = "a".repeat(64);
        let photo = LibraryPhoto::from_image(OpenImage::new(
            "drain-components.png",
            20,
            10,
            content.clone(),
        ));
        let id = photo.id.clone();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![photo], Vec::new()),
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
                content: content.clone(),
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
                content,
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
        let analysis = crate::test_doc::photo(&doc, &id)
            .unwrap()
            .analysis()
            .unwrap();
        assert_eq!(analysis.ocr_text, "receipt forty two");
        assert_eq!(analysis.description, "A bicycle beside a lake");
        assert_eq!(analysis.objects, ["bicycle"]);
        assert_eq!(analysis.ocr_provenance.unwrap().source, "local-ocr");
        assert_eq!(analysis.vision_provenance.unwrap().source, "local-vision");
    }

    eprintln!("phase library_analysis_commits_each_real_result_with_provenance_and_finishes");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let content = "a".repeat(64);
        let image = OpenImage::new("/photos/one.png", 20, 10, &content);
        let photo = LibraryPhoto::from_image(image);
        let id = photo.id.clone();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![photo], Vec::new()),
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
            content: content.clone(),
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
        let stored = crate::test_doc::photo(&doc, &id)
            .and_then(|photo| photo.analysis())
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
        use winit::event::{DeviceId, TouchPhase, WindowEvent};
        use winit::keyboard::ModifiersState;

        let mut app = test_app();
        app.library_mode = true;
        app.canvas_rect = Rect::new(0.0, 48.0, 1_280.0, 800.0);
        let mut doc = crate::test_doc::new_doc();
        let pinch = ui::testing::Dispatch::new().doc(&mut doc).send(
            &mut app,
            &WindowEvent::PinchGesture {
                device_id: DeviceId::dummy(),
                delta: 0.2,
                phase: TouchPhase::Moved,
            },
        );
        let handled = pinch.handled;
        assert!(handled);
        let after_pinch = app.timeline_density;

        let ctrl = winit::event::Modifiers::from(ModifiersState::CONTROL);
        let handled = ui::testing::Dispatch::new()
            .modifiers(ctrl)
            .doc(&mut doc)
            .focus_target(pinch.focus_target)
            .send(&mut app, &ui::synthetic_input::mouse_wheel(0.0, -1.0))
            .handled;
        assert!(handled);
        assert_eq!(app.timeline_density, after_pinch + 1);
    }

    eprintln!("phase density_reanchors_the_same_first_photo");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..90 {
            let mut photo = LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/photos/anchor-{index:03}.png"),
                30,
                20,
            ));
            photo.captured_at = Some("2026-06-18".into());
            library.add_photo(photo);
        }
        crate::test_doc::set_library(&mut doc, &library);
        app.show_library();
        app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 800.0);
        app.timeline_density = 1;
        sync(&mut app, &mut doc);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
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
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
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
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..5 {
            let mut photo = LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/photos/library-{index}.png"),
                20,
                10,
            ));
            photo.tags = crate::model::labels(["golden hour".to_owned()]);
            library.add_photo(photo);
        }
        crate::test_doc::set_library(&mut doc, &library);
        app.show_library();
        app.canvas_rect = Rect::new(0.0, 48.0, 390.0, 796.0);
        sync(&mut app, &mut doc);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &theme);

        assert!(app.metadata_filter_cards.is_empty());
        assert!(
            app.library_cards
                .first()
                .is_some_and(|(_, rect, _)| rect.y0 < app.canvas_rect.y1),
            "the first photograph must remain above the fold at 200%"
        );
        assert!(
            app.command_hits().contains(&Hit::Smaller) && app.command_hits().contains(&Hit::Larger),
            "density stays reachable from the header at 200%"
        );

        let before = app.timeline_density;
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Focus,
            &doc,
        ));
        assert_eq!(
            app.timeline_density, before,
            "semantic focus must not navigate"
        );
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.timeline_density, before + 1);
        assert_eq!(crate::test_doc::library(&doc), library);
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
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.add_photo(photo.clone());
        library.add_album(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: crate::model::labels([photo.id]),
        });
        crate::test_doc::set_library(&mut doc, &library);
        let before = crate::test_doc::library(&doc);
        let payload = PhotosEffect::ViewAlbum {
            album_id: "photos.album:trip".into(),
        };
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .unwrap()
            .is_some()
        );
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            crate::test_doc::library(&doc),
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
                &crate::test_doc::library(&doc)
            )
            .is_err()
        );
        assert_eq!(crate::test_doc::library(&doc), before);
    }

    eprintln!("phase opening_favorites_uses_a_local_view_and_refuses_an_empty_stale_collection");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut favorite =
            LibraryPhoto::from_image(OpenImage::fixture("/photos/favorite.png", 20, 10));
        favorite.favorite = true;
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![favorite], Vec::new()),
        );
        let before = crate::test_doc::library(&doc);
        let payload = PhotosEffect::ViewFavorites;
        assert!(
            app.answer_effect(
                payload.clone(),
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .expect("favorite collection is present")
            .is_some()
        );
        assert!(app.library_mode);
        assert!(app.active_favorites);
        assert_eq!(
            crate::test_doc::library(&doc),
            before,
            "opening a view is not a write"
        );

        crate::test_doc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .is_err()
        );
    }

    eprintln!("phase metadata_views_are_local_filters_with_honest_stale_results");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut in_folder = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        in_folder.folder = Some("Travel".into());
        in_folder.people = crate::model::labels(["Élodie".to_owned()]);
        in_folder.tags = crate::model::labels(["golden-hour".to_owned()]);
        let other = LibraryPhoto::from_image(OpenImage::fixture("/photos/two.png", 20, 10));
        let library = crate::model::PhotoLibrary::of(vec![in_folder, other], Vec::new());
        crate::test_doc::set_library(&mut doc, &library);

        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
            Some("Travel".into()),
            None,
            None,
        )
        .expect("existing folder enters a view");
        sync(&mut app, &mut doc);
        assert!(app.library_mode);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_folder.as_deref(), Some("Travel"));
        assert!(app.active_person.is_none());

        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
            None,
            Some("Élodie".into()),
            None,
        )
        .expect("existing person enters a view");
        sync(&mut app, &mut doc);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_person.as_deref(), Some("Élodie"));
        assert!(app.active_folder.is_none());
        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
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
                &crate::test_doc::library(&doc),
                Some("Missing".into()),
                None,
                None
            )
            .is_err()
        );
    }

    eprintln!("phase people_and_places_keep_complete_paged_collections");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..29 {
            let mut photo = LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/photos/{index}.png"),
                40,
                20,
            ));
            photo.people = crate::model::labels([format!("Person {index:02}")]);
            photo.location = Some(crate::model::PhotoLocation {
                latitude: 10.0 + f64::from(index) / 100.0,
                longitude: 20.0,
                label: Some(format!("Place {index:02}")),
                provenance: crate::model::LocationProvenance::default(),
            });
            library.add_photo(photo);
            library.add_album(crate::model::Album {
                id: format!("photos.album:album-{index:02}"),
                name: format!("Album {index:02}"),
                photo_ids: ui::tilo_resource::Set::new(),
            });
        }
        crate::test_doc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);

        assert_eq!(app.library_people.len(), 29);
        assert_eq!(app.places_items.len(), 29);
        assert_eq!(app.library_albums.len(), 29);
        assert!(app.page_collection(CollectionPageHit::PeopleNext));
        assert!(app.page_collection(CollectionPageHit::PlacesNext));
        assert_eq!((app.people_page, app.places_page), (1, 1));
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-next",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.people_page, 2);
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-previous",
            &AccessAction::Activate,
            &doc,
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
        let mut doc = crate::test_doc::new_doc();
        let first = LibraryPhoto::from_image(OpenImage::fixture("/photos/first.png", 20, 10));
        let second = LibraryPhoto::from_image(OpenImage::fixture("/photos/second.png", 20, 10));
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(
                vec![first.clone(), second.clone()],
                vec![Album {
                    id: "photos.album:trip".into(),
                    name: "Trip".into(),
                    photo_ids: ui::tilo_resource::Set::new(),
                }],
            ),
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
            crate::test_doc::library(&doc)
                .all_photos()
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
            &doc,
        ));
        land(&app, &mut doc);
        let expected_favorite =
            Invoke::group(selected_ids.iter().map(|id| {
                Invoke::edit(&crate::model::photo_paths(id).favorite.set(false)).unwrap()
            }));
        assert_eq!(
            invocation(app.last_library_action.as_ref()),
            invocation(Some(&expected_favorite)),
            "the AT batch control submits the field edits an agent would"
        );
        assert!(
            crate::test_doc::library(&doc)
                .all_photos()
                .all(|photo| !photo.favorite),
            "the same batch command also has an explicit unfavorite result"
        );

        app.begin_selection();
        app.select_all_visible();
        assert!(app.perform_library_semantic_action(
            "photos:selection:choose-album",
            &AccessAction::Activate,
            &doc,
        ));
        assert!(app.selection_album_chooser);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:trip",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:trip"),
            "the target album is an explicit, stable choice rather than the current view"
        );
        app.add_selected_to_destination_album();
        land(&app, &mut doc);
        let paths = crate::model::album_paths("photos.album:trip").photo_ids;
        let expected_membership = Invoke::group(
            selected_ids
                .iter()
                .map(|id| Invoke::edit(&paths.insert(id)).unwrap()),
        );
        assert_eq!(
            invocation(app.last_library_action.as_ref()),
            invocation(Some(&expected_membership)),
            "the pointer batch control files each photograph as an agent would"
        );
        let library = crate::test_doc::library(&doc);
        let album = library.album("photos.album:trip").unwrap();
        assert_eq!(
            library.album_photos(album).collect::<Vec<_>>(),
            [first.id.as_str(), second.id.as_str()],
            "the album holds both, in browse order"
        );
    }

    eprintln!("phase destination_chooser_lists_every_album_and_refuses_a_stale_target");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        let albums: Vec<_> = (1..=7)
            .map(|number| Album {
                id: format!("photos.album:destination-{number}"),
                name: format!("Destination {number}"),
                photo_ids: ui::tilo_resource::Set::new(),
            })
            .collect();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![photo.clone()], albums.clone()),
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
            &doc,
        ));
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination-next",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.selection_album_page, 1);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:destination-7")
        );
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![photo], Vec::new()),
        );
        present(&mut app, &doc);
        assert!(!app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(
            app.notice.as_deref(),
            Some(tr("photos-album-gone").as_str())
        );
    }

    eprintln!("phase album_editor_keeps_its_draft_local_while_the_timeline_scrolls");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..49 {
            let mut photo = LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/photos/library-{index}.png"),
                20,
                10,
            ));
            photo.captured_at = Some(format!("2026-06-{:02}", 1 + index % 28));
            library.add_photo(photo);
        }
        crate::test_doc::set_library(&mut doc, &library);
        app.show_library();
        sync(&mut app, &mut doc);
        app.begin_new_album();
        sync(&mut app, &mut doc);
        assert!(app.album_editor);
        app.sync_inspector();
        assert!(
            app.chrome.inspector.is_open(),
            "the editor opens in the inspector"
        );
        app.canvas_rect = Rect::new(0.0, 56.0, 920.0, 772.0);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_library(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
        let before = crate::test_doc::library(&doc);
        app.pointer = Point::new(400.0, 400.0);
        ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(&mut app, &ui::synthetic_input::mouse_wheel(0.0, -3.0));
        assert!(!app.can_page_library(false));
        assert!(app.can_page_library(true));
        lay_out_header(&mut app, 1280.0);
        let semantics = app
            .chrome
            .toolbar
            .accessibility()
            .expect("paging uses shared toolbar controls");
        let pager = &semantics.children;
        assert!(pager.iter().any(|node| node.state.disabled
            && node.name.as_deref() == Some(tr("photos-a11y-previous-photos").as_str())));
        assert!(pager.iter().any(|node| !node.state.disabled
            && node.name.as_deref() == Some(tr("photos-a11y-next-photos").as_str())));

        assert!(app.perform_library_semantic_action(
            "photos:album-editor:next-page",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.library_offset, 24);
        assert!(app.perform_library_semantic_action(
            "photos:album-editor:previous-page",
            &AccessAction::Activate,
            &doc,
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
        assert_eq!(crate::test_doc::library(&doc), before);
    }

    eprintln!("phase concurrent_album_drafts_mint_distinct_stable_ids");
    {
        let mut left = test_app();
        let mut right = test_app();
        left.begin_new_album();
        right.begin_new_album();
        let left_id = left.album_editor_id.expect("left draft id");
        let right_id = right.album_editor_id.expect("right draft id");
        assert!(left_id.starts_with("photos.album:"));
        assert!(right_id.starts_with("photos.album:"));
        assert_ne!(left_id, right_id);
    }

    eprintln!("phase albums_destination_materializes_bounded_covers_and_semantic_routes");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/pictures/cover.png", 800, 600));
        let photo_id = photo.id.clone();
        let album_id = "photos.album:weekend".to_owned();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(
                vec![photo],
                vec![crate::model::Album {
                    id: album_id.clone(),
                    name: "Weekend".to_owned(),
                    photo_ids: crate::model::labels([photo_id.clone(), "missing".to_owned()]),
                }],
            ),
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
            &doc,
        ));
        assert!(app.album_editor);
        app.cancel_album_editor();
        assert!(app.perform_library_semantic_action(
            &format!("photos:library-album:{album_id}"),
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.active_album.as_deref(), Some(album_id.as_str()));
        assert_eq!(app.library_destination, LibraryDestination::Timeline);
    }
}

#[test]
fn operations_fail_canonically_cancel_terminally_and_confirm_removal() {
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
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/remove.png", 20, 10));
        let id = photo.id.clone();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![photo], Vec::new()),
        );
        app.show_library();
        sync(&mut app, &mut doc);
        let target = format!("photos:library-remove:{id}");
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &doc));
        assert!(crate::test_doc::library(&doc).photo(&id).is_some());
        assert_eq!(app.library_remove_confirm.as_deref(), Some(id.as_str()));
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &doc));
        settle(&mut app, &mut doc);
        assert!(crate::test_doc::library(&doc).photo(&id).is_none());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn accessible_toolbars_menus_and_metadata_sheet_route_real_controls() {
    eprintln!("phase accessibility_exposes_and_routes_every_header_command");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        sync(&mut app, &mut doc);

        let empty = app.accessibility_tree();
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.children[0].role, Role::Image);
        let commands = toolbar_names(&app);
        assert!(
            !commands.contains(&tr("photos-open")),
            "the empty state offers Open itself; the header does not repeat it"
        );
        assert!(
            !commands.contains(&tr("photos-command-rotate-right")),
            "rotate is not offered without a photograph"
        );
        let mut hosted =
            ui::framework::testing::TestApp::<crate::resource::PhotosShell>::new().unwrap();
        assert!(
            hosted.effect(&PhotosEffect::RotateShownRight).is_err(),
            "the production resource door refuses rotation without a photograph"
        );

        let image = OpenImage::fixture("/tmp/accessible.png", 100, 50);
        app.show_photo(&crate::test_doc::put_image(&mut doc, &image));
        app.original = Some(("/tmp/accessible.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        present(&mut app, &doc);
        app.refresh();
        sync(&mut app, &mut doc);
        lay_out_header(&mut app, 1280.0);
        let target = toolbar_target(&app, &tr("photos-command-rotate-right"))
            .expect("a photograph offers rotate right");
        assert!(Widget::perform_semantic_action(
            &mut app,
            &target,
            &AccessAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::photo_edits(&doc, app.open_photo_id().unwrap()).rotate_quarters,
            1
        );

        sync(&mut app, &mut doc);
        let opened = app.accessibility_tree();
        assert_eq!(
            opened.children[0].name.as_deref(),
            Some("accessible.png — edited")
        );
        assert!(
            opened
                .children
                .iter()
                .any(|node| node.role == Role::StaticText && node.live == Some(Politeness::Polite)),
            "the status bar is a polite live region"
        );
    }

    eprintln!("phase narrow_header_keeps_every_command_reachable_through_more");
    {
        let mut hosted =
            ui::framework::testing::TestApp::<crate::resource::PhotosShell>::capture_at(
                Some("opened"),
                (520, 800),
            )
            .unwrap();
        assert!(
            hosted
                .harness()
                .controls()
                .iter()
                .any(|control| { control.name.as_deref() == Some("More commands") }),
            "a narrow header moves commands into More"
        );
        hosted
            .harness()
            .perform_named("toolbar.overflow", &AccessAction::Activate)
            .expect("the actual host admits the overflow view action");
        hosted.settle();
        assert!(
            hosted
                .harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| { node.role() == ui::accesskit::Role::Menu }),
            "the admitted overflow opens a real menu"
        );
        assert!(
            hosted.harness().controls().iter().any(|control| {
                control.role == ui::accesskit::Role::MenuItem
                    && control.name.as_deref() == Some(tr("photos-save").as_str())
            }),
            "Save stays reachable from the menu"
        );
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
            let mut doc = crate::test_doc::new_doc();
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
                            size: Size::from_lengths(w, h),
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
            }
        }
    }

    eprintln!(
        "phase opened_viewer_exposes_shared_context_without_creating_a_second_metadata_store"
    );
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let image = OpenImage::fixture("/tmp/harbour.png", 1_200, 800);
        app.show_photo(&crate::test_doc::put_image(&mut doc, &image));
        let mut library = crate::test_doc::library(&doc);
        let (_, photo) = library
            .photos
            .iter_mut()
            .next()
            .expect("opening creates a library record");
        photo.captured_at = Some("2026-09-19 at dusk".to_owned());
        photo.people = crate::model::labels(["Ada".to_owned(), "Mina".to_owned()]);
        photo.tags = crate::model::labels(["harbour".to_owned(), "travel".to_owned()]);
        photo.folder = Some("Weekends".to_owned());
        photo.location = Some(crate::model::PhotoLocation {
            latitude: 52.0,
            longitude: 4.0,
            label: Some("Harbour front".to_owned()),
            provenance: crate::model::LocationProvenance::default(),
        });
        photo.set_analysis(Some(crate::model::ImageAnalysis {
            description: "Two friends walking beside the harbour at dusk".to_owned(),
            ..Default::default()
        }));
        crate::test_doc::set_library(&mut doc, &library);
        let before = crate::test_doc::library(&doc);
        app.original = Some((image.name.clone(), shots_pattern(1_200, 800)));
        present(&mut app, &doc);
        app.refresh();

        let canvas = app.accessibility_tree().children[0].clone();
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
        let refreshed = crate::test_doc::library(&doc);
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
        let mut doc = crate::test_doc::new_doc();
        let image = OpenImage::fixture("/pictures/München-夏.png", 800, 400);
        app.show_photo(&crate::test_doc::put_image(&mut doc, &image));
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
        let state = PhotoLibrary::read(&doc).unwrap();
        let entry = PhotoLibrary::search(&state)
            .into_iter()
            .find(|entry| entry.title.contains('夏'))
            .expect("the photograph is a search entry");
        let anchor = entry.anchor.expect("a hit opens the photograph it found");
        assert!(has_resource_id(&published(&doc), &anchor));
    }

    eprintln!("phase platform_search_narrows_photos_by_people_places_and_tags");
    {
        use ui::search::{FacetFilter, SearchProvider, SearchQuery};

        let mut doc = crate::test_doc::new_doc();
        let mut beach = LibraryPhoto::from_image(OpenImage::fixture("/photos/beach.png", 40, 20));
        beach.people = crate::model::labels(["Noor".to_owned()]);
        beach.tags = crate::model::labels(["summer".to_owned()]);
        let mut city = LibraryPhoto::from_image(OpenImage::fixture("/photos/city.png", 40, 20));
        city.people = crate::model::labels(["Sam".to_owned()]);
        let (beach_id, city_id) = (beach.id.clone(), city.id.clone());
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![beach, city], Vec::new()),
        );
        let search = ui::framework::testing::search_source::<crate::model::PhotoLibrary>(
            crate::search::library_search_entries(&crate::test_doc::library(&doc)),
        );
        let facets = SearchProvider::facets(&search);
        let person = facets
            .iter()
            .find(|facet| facet.key == "person")
            .expect("people narrow a search");
        assert!(person.values.iter().any(|value| value.value == "Noor"));
        assert!(facets.iter().any(|facet| facet.key == "tag"));

        let query = SearchQuery::new("png").with_filter(FacetFilter::new("person", "Noor"));
        let page = search.search_page(&query, None, usize::MAX, usize::MAX);
        let anchors: Vec<String> = page
            .results
            .iter()
            .map(ui::search::SearchResult::anchor)
            .collect();
        assert!(
            anchors.contains(&crate::resource::photo_resource_id(&beach_id)),
            "{anchors:?}"
        );
        assert!(
            !anchors.contains(&crate::resource::photo_resource_id(&city_id)),
            "{anchors:?}"
        );
        assert!(
            page.results
                .iter()
                .all(|hit| matches!(hit.row, Some(ui::search::SearchRow::Media { .. }))),
            "a photo hit reads as media with its thumbnail"
        );
    }

    eprintln!("phase rejected_search_refresh_keeps_last_good_index_and_exports_diagnostic");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let first = LibraryPhoto::from_image(OpenImage::fixture("/photos/first.png", 20, 10));
        let first_id = first.id.clone();
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![first], Vec::new()),
        );
        sync(&mut app, &mut doc);
        assert!(
            app.library_index
                .resource(&crate::resource::photo_resource_id(&first_id))
                .is_some()
        );

        let mut library = crate::test_doc::library(&doc);
        let second = LibraryPhoto::from_image(OpenImage::fixture("/photos/second.png", 20, 10));
        let second_id = second.id.clone();
        library.add_photo(second);
        crate::test_doc::set_library(&mut doc, &library);
        app.force_search_refresh_error = true;
        sync(&mut app, &mut doc);

        assert!(
            app.library_index
                .resource(&crate::resource::photo_resource_id(&first_id))
                .is_some()
        );
        assert!(
            app.library_index
                .resource(&crate::resource::photo_resource_id(&second_id))
                .is_none()
        );
        assert!(
            app.status().contains(
                app.search_refresh_error
                    .as_deref()
                    .expect("a refused index refresh is explained")
            )
        );
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

#[test]
fn thumbnail_queue_is_bounded_wakes_its_host_and_rejects_stale_results() {
    eprintln!("phase a_large_library_fills_a_bounded_background_thumbnail_queue");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..500 {
            library.add_photo(LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/unavailable/library-{index}.png"),
                32_000,
                32_000,
            )));
        }
        crate::test_doc::set_library(&mut doc, &library);
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
        let mut scope = ui::wake::WakeScope::new();
        let _entry = scope.enter();
        let (sender, receiver) = std::sync::mpsc::channel();
        scope
            .bind(move |event| {
                let _ = sender.send(event);
            })
            .unwrap();
        let app = test_app();
        let stored = app
            .content
            .put(&pixels::encode_png(&shots_pattern(16, 8)).unwrap())
            .unwrap();
        let missing = tilo_content::hash_bytes(b"never stored");
        for (content, succeeds) in [(stored, true), (missing, false)] {
            let image = OpenImage::new("thumbnail.png", 16, 8, content);
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
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/after.png", 16, 8));
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
fn controls_contain_their_text_share_insets_and_keep_pointer_targets() {
    eprintln!("phase header_row_contains_its_controls_at_any_text_scale");
    {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut app = PhotosApp::new();
            let mut doc = crate::test_doc::new_doc();
            stage(&mut app, &mut doc, "opened");
            sync(&mut app, &mut doc);
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            app.rect = Rect::new(0.0, 0.0, 1_280.0, 800.0);
            app.chrome.drawer.scale_for(&theme);
            let content = app.chrome.drawer.resolve(app.rect);
            app.chrome.header = Rect::new(content.x0, 0.0, 1_280.0, chrome::header_height(&theme));
            app.place_chrome(&mut text, &theme);
            let header = app.chrome.header;
            for (name, rect) in app.toolbar_item_rects() {
                assert!(
                    rect.y0 >= header.y0 - 0.5 && rect.y1 <= header.y1 + 0.5,
                    "{scale}x: `{name}` {rect:?} escapes the header row {header:?}"
                );
            }
        }
    }

    eprintln!("phase library_card_actions_share_one_edge_inset");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
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
                        size: Size::from_lengths(1200.0, 800.0),
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

    eprintln!("phase album_management_lives_in_the_header_and_the_inspector");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.add_photo(photo.clone());
        library.add_album(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: crate::model::labels([photo.id]),
        });
        crate::test_doc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);
        app.show_library();
        app.active_album = Some("photos.album:trip".into());
        sync(&mut app, &mut doc);
        assert!(
            app.command_hits().contains(&Hit::EditAlbum),
            "an open album is edited from the header"
        );

        app.begin_edit_album();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        lay_out_header(&mut app, 1280.0);
        app.sync_inspector();
        app.chrome.inspector.resolve(app.rect);
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let panel = app.chrome.inspector.sidebar_rect();
        assert!(panel.width() > 0.0, "the editor opens in the inspector");
        let control = ui::widget::ControlSize::Regular.height(&theme);
        assert!(app.album_editor_hits.len() >= 3);
        for (hit, rect) in &app.album_editor_hits {
            assert!(
                rect.height() >= control - 0.5,
                "{hit:?} is not a regular control: {rect:?}"
            );
            assert!(
                rect.x0 >= panel.x0 && rect.x1 <= panel.x1 && rect.y1 <= panel.y1,
                "{hit:?} {rect:?} escapes the inspector {panel:?}"
            );
        }
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
        app.library_analysis_capability = Some(tilo_vision::LocalVisionCapability {
            profile_id: "local".into(),
            model_id: "vision".into(),
            model_label: "Local vision".into(),
            revision: "0123456789abcdef".into(),
        });
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
        assert!(start.width() > 0.0, "the phone layout keeps Analyze photos");
        assert!(
            start.height() >= ui::widget::ControlSize::Regular.height(&Theme::dark()) - 0.5,
            "Analyze photos is a regular control: {start:?}"
        );
        assert!(start.x0 >= app.canvas_rect.x0 && start.x1 <= app.canvas_rect.x1);
        let tree = app.accessibility_tree();
        assert!(
            tree.children
                .iter()
                .any(|node| node.name.as_deref() == Some(tr("photos-analysis-label").as_str()))
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
        assert!(stop.width() >= 32.0 && stop.height() >= 32.0);

        let mut doc = crate::test_doc::new_doc();
        app.answer_effect(
            PhotosEffect::StopImport,
            tilo_actions::InvocationId::new(),
            &crate::test_doc::library(&doc),
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
        app.draw_directory_import_details(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
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
            receipt.failures.len()
        );
        assert!(dialog.description.as_deref().is_some_and(|text| text
            == tr_args(
                "photos-import-issues-total",
                &[
                    ("kept", ArgValue::Number(9.0)),
                    ("count", ArgValue::Number(27.0)),
                ],
            )));
        assert!(dialog.children.iter().all(|node| {
            !node.name.as_deref().unwrap_or_default().contains('/')
                && !node
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .contains("/home/")
        }));
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
            rect.width() >= 32.0
                && rect.height() >= 32.0
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
            &PresenceCtx::empty(),
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
        let open = OpenImage::new("fixture.png", 3, 2, source_hash.clone());
        let edits = crate::model::Edits {
            crop: crate::model::Rect01 {
                x: 1.0 / 3.0,
                y: 0.0,
                w: 2.0 / 3.0,
                h: 1.0,
            },
            rotate_quarters: 1,
        };
        let mut doc = crate::test_doc::new_doc();
        let resource_id = crate::test_doc::put_image(&mut doc, &open);
        crate::test_doc::set_photo_edits(&mut doc, &resource_id, edits);

        let exported = <PhotosEffect as ui::tilo_resource::Effect>::specs()
            .into_iter()
            .find(|spec| spec.id == "export-edited-content")
            .expect("the library exports a photograph's edited content");
        assert_eq!(exported.kind, tilo_actions::OperationKind::Effect);
        assert_eq!(
            exported.contract.retry,
            tilo_actions::RetryPolicy::Idempotent
        );
        assert_eq!(
            exported.contract.result,
            tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            }
        );

        let payload = PhotosEffect::ExportEditedContent {
            photo: ui::tilo_resource::Id::from_raw(resource_id.clone()),
        };
        let mut app = editing(PhotosApp::with_content(store.clone()));
        let started = app
            .answer_effect(
                payload.clone(),
                tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc),
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
            crate::test_doc::photo(&doc, &resource_id).map(|photo| photo.image.content),
            Some(source_hash)
        );
        assert_eq!(crate::test_doc::photo_edits(&doc, &resource_id), edits);

        // The photograph as it is when the export runs: edits since are in it.
        crate::test_doc::set_photo_edits(&mut doc, &resource_id, crate::model::Edits::default());
        let output = app
            .answer_effect(
                payload,
                tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc),
            )
            .expect("export succeeds")
            .expect("bounded content production completes");
        let unedited = store
            .read(tilo_actions::content_hash(&output.resources[0]).expect("one content result"))
            .expect("produced bytes are retrievable");
        assert_eq!(
            pixels::decode(&unedited).expect("produced PNG decodes"),
            pixels::render(
                &original,
                crate::model::Edits::default().crop,
                crate::model::Edits::default().rotate_quarters
            ),
            "the export renders the edits the photograph has now"
        );
    }

    eprintln!("phase edited_content_effect_rejects_missing_or_misdirected_source_bytes");
    {
        let missing = "f".repeat(64);
        let open = OpenImage::new("missing.png", 3, 2, missing);
        let mut doc = crate::test_doc::new_doc();
        let photo_id = crate::test_doc::put_image(&mut doc, &open);
        let encoded = |resource_id: String| PhotosEffect::ExportEditedContent {
            photo: ui::tilo_resource::Id::from_raw(resource_id),
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
                    &crate::test_doc::library(&doc)
                )
                .is_err()
            );
        }
    }
}

/// The layout tree holds pointers into the People buttons and the viewer,
/// and the framework syncs again between building that tree and measuring
/// it. A sync must therefore never replace the People buttons, and replacing
/// the viewer must report a structure change so the tree is rebuilt instead
/// of measured through a freed widget.
#[test]
fn sync_never_frees_a_laid_out_child() {
    let mut app = PhotosApp::new();
    let mut doc = crate::test_doc::new_doc();
    stage(&mut app, &mut doc, "people");
    sync(&mut app, &mut doc);
    app.refresh_people_controls();
    let laid_out = app.people_controls.as_ptr();
    assert!(
        !app.people_controls.is_empty(),
        "the People surface has controls"
    );
    app.people_show_hidden = !app.people_show_hidden;
    sync(&mut app, &mut doc);
    assert!(
        std::ptr::eq(app.people_controls.as_ptr(), laid_out),
        "a sync between building the tree and measuring it keeps the People buttons"
    );

    let mut app = PhotosApp::new();
    let mut doc = crate::test_doc::new_doc();
    stage(&mut app, &mut doc, "opened");
    sync(&mut app, &mut doc);
    assert!(
        app.viewer.is_some(),
        "the opened scenario shows a photograph"
    );
    assert!(
        app.take_structure_dirty(),
        "a new viewer invalidates the tree that pointed at the old one"
    );
    assert!(!app.take_structure_dirty(), "reading the flag clears it");
    sync(&mut app, &mut doc);
    assert!(
        !app.take_structure_dirty(),
        "a sync that keeps the photograph keeps the viewer"
    );
}

/// The room's host behind the door, announcing a web entry for links.
struct LinkHost;

impl tilo_actions::remote::ActionHost for LinkHost {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
    > {
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::CataloguePage {
            locator: Some(tilo_actions::remote::ResourceViewLocator {
                origin: "https://host.example".into(),
                room: "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90".into(),
                host_device: "ab".repeat(32),
                provider: query.provider.unwrap_or_default(),
                web_entry: Some("https://tilo.so/".into()),
            }),
            exhausted: true,
            ..tilo_actions::remote::CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionHostError::NotPermitted,
        ))
    }

    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::remote::ActionHostError> {
        Ok(())
    }
}

#[derive(Default)]
struct LinkClipboard(std::sync::Mutex<Option<String>>);

impl ui::clipboard::ClipboardBackend for LinkClipboard {
    fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
        *self.0.lock().unwrap() = Some(text.into());
        true
    }

    fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
        self.0.lock().unwrap().clone()
    }

    fn name(&self) -> &'static str {
        "photos-share-test-clipboard"
    }
}

#[test]
fn share_copies_the_open_photographs_link() {
    use ui::framework::testing::TestApp;
    let _served = ui::RemoteActions::enter(
        std::sync::Arc::new(LinkHost),
        tilo_actions::remote::Caller::local_person("owner"),
    );
    let services = ui::host::HostServices::null()
        .with_clipboard(std::sync::Arc::new(LinkClipboard::default()));
    let _scope = ui::host::enter(&services);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lake.png");
    std::fs::write(
        &path,
        crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap(),
    )
    .unwrap();
    let mut app =
        TestApp::<crate::resource::PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
    let lake = app.state().all_photos().next().unwrap().id.clone();
    let item = crate::resource::photo_resource_id(&lake);
    app.call_on(&item, PhotosEffect::VIEW.id, serde_json::json!({}))
        .unwrap();
    app.settle();

    app.surface_mut::<PhotosApp>().share_open_photo();
    app.settle();
    let copied = ui::clipboard_paste().expect("the link is copied");
    assert!(copied.starts_with("https://tilo.so/app/#/"), "{copied}");
    assert_eq!(
        app.surface_mut::<PhotosApp>().notice.as_deref(),
        Some(tr("photos-share-copied").as_str())
    );
}

/// Making thumbnails is published as an offloadable transform of the
/// thumbnails package, and its local run makes exactly what that package's
/// entrypoint makes on another device: content named twice is one input,
/// and an input that is not an image fails the whole batch.
#[test]
fn make_thumbnails_runs_the_offloadable_batch_and_matches_the_package_entrypoint() {
    let spec = <PhotosEffect as ui::tilo_resource::Effect>::specs()
        .into_iter()
        .find(|spec| spec.id == "make-thumbnails")
        .expect("the library makes thumbnails");
    let offload = spec
        .contract
        .offload
        .clone()
        .expect("making thumbnails declares its offload");
    assert_eq!(offload.app, thumbnail::batch::PACKAGE);
    assert_eq!(offload.entrypoint, thumbnail::batch::ENTRYPOINT);
    assert!(
        spec.contract.validate_declaration().is_empty(),
        "{:?}",
        spec.contract.validate_declaration()
    );
    assert!(spec.params()[0].is_content_list());

    let store = tilo_content::ContentStore::memory();
    let photographs: Vec<Vec<u8>> = [(3, 2), (40, 17)]
        .into_iter()
        .map(|(width, height)| {
            let rgba = (0..width * height * 4)
                .map(|index| u8::try_from(index % 251).expect("a byte"))
                .collect();
            pixels::encode_png(&pixels::Rgba::new(width, height, rgba).expect("an image"))
                .expect("a PNG")
        })
        .collect();
    let hashes: Vec<String> = photographs
        .iter()
        .map(|bytes| store.put(bytes).expect("stage a photograph"))
        .collect();
    let mut app = PhotosApp::with_content(store.clone());
    let doc = crate::test_doc::new_doc();
    let library = crate::test_doc::library(&doc);
    let output = app
        .answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec![
                    hashes[0].clone(),
                    tilo_actions::content_resource(&hashes[1]).expect("an address"),
                    hashes[0].clone(),
                ],
            },
            tilo_actions::InvocationId::new(),
            &library,
        )
        .expect("thumbnails are made")
        .expect("the batch answers at once");
    let expected = thumbnail::batch::thumbnail_batch(&photographs).expect("the package's batch");
    let made: Vec<Vec<u8>> = output
        .resources
        .iter()
        .map(|resource| {
            store
                .read(tilo_actions::content_hash(resource).expect("content"))
                .expect("a stored thumbnail")
        })
        .collect();
    assert_eq!(made, expected, "byte for byte the entrypoint's thumbnails");

    let broken = store.put(b"not a photograph").expect("stage bytes");
    assert_eq!(
        app.answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec![hashes[0].clone(), broken],
            },
            tilo_actions::InvocationId::new(),
            &library,
        ),
        Err(thumbnail::batch::BatchError::Undecodable { index: 1 }.to_string())
    );
    assert!(
        app.answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec!["/home/someone/photo.png".into()],
            },
            tilo_actions::InvocationId::new(),
            &library,
        )
        .is_err(),
        "a path is never content"
    );
}
