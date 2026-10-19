use super::*;
use taffy::prelude::{Style, TaffyTree};

use taffy::prelude::{AvailableSpace, Size};
use ui::a11y::Action as AccessAction;
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::Affine;

use automerge::ROOT;

/// Present `doc`'s library to `app` as the framework does after a change,
/// then sync it.
fn sync(app: &mut PhotosApp, doc: &mut AutoCommit) {
    present(app, doc);
    Widget::sync(app, doc, &ROOT);
    settle(app, doc);
}

/// Apply what `app` submitted to `doc`, as the host does after a pass,
/// and show it the result.
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

fn test_app() -> PhotosApp {
    test_app_on(ContentStore::memory())
}
fn test_app_on(content: ContentStore) -> PhotosApp {
    PhotosApp::with_content(content)
        .with_dispatcher(ui::framework::testing::dispatcher("so.tilo.photos"))
}
fn restore(doc: &mut AutoCommit, selected: &[automerge::ChangeHash]) {
    ui::history_source::steps::restore(doc, selected, &[]).unwrap();
}
/// Change the document's content: every photograph removed.
fn empty_library(doc: &mut AutoCommit) {
    let mut library = crate::test_doc::library(doc);
    library.photos.retain(|_, _| false);
    crate::test_doc::set_library(doc, &library);
}
fn setup() -> (PhotosApp, AutoCommit, String) {
    let mut doc = crate::test_doc::new_doc();
    let image = OpenImage::fixture("historical-missing.png", 4, 3);
    let id = crate::test_doc::put_image(&mut doc, &image);
    let mut app = test_app();
    app.show_photo(&id);
    sync(&mut app, &mut doc);
    (app, doc, id)
}
#[test]
fn thumbnails_follow_photo_identity_and_visibility() {
    eprintln!("phase newly_visible_timeline_photo_uses_immutable_thumbnail_outside_queued_window");
    {
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..80 {
            let mut photo = crate::model::LibraryPhoto::from_image(OpenImage::new(
                &format!("missing-timeline-{index}.png"),
                30,
                20,
                format!("{index:064x}"),
            ));
            photo.captured_at = Some(format!("2026-{:02}-{:02}", 1 + index / 28, 1 + index % 28));
            library.add_photo(photo);
        }
        let target = library.photos.values().nth(2).unwrap().clone();
        crate::test_doc::set_library(&mut doc, &library);
        let mut app = test_app();
        app.show_library();
        app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 900.0);
        sync(&mut app, &mut doc);
        assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
        app.view_timeline(
            &crate::test_doc::library(&doc),
            Some(&crate::actions::TimelineAnchor::Day {
                day: target.captured_at.clone().unwrap(),
            }),
        )
        .unwrap();
        let mut text = TextRenderer::new();
        let mut missing = Scene::new();
        app.draw_library(&mut missing, Affine::IDENTITY, &mut text, &Theme::dark());
        assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
        assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
        app.library_thumbnails.insert(
            thumbnail_key(&target.image),
            to_peniko(&crate::shots_pattern(30, 20)),
        );
        let mut hydrated = Scene::new();
        app.draw_library(&mut hydrated, Affine::IDENTITY, &mut text, &Theme::dark());
        assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
        let images = |scene: &Scene| {
            scene
                .encoding()
                .resources
                .patches
                .iter()
                .filter(|patch| matches!(patch, vello_encoding::Patch::Image { .. }))
                .count()
        };
        assert_eq!(
            images(&hydrated),
            images(&missing) + 1,
            "the production painter records the cached immutable image even before it enters the queued thumbnail window"
        );
    }

    eprintln!("phase changed_photo_identity_invalidates_cached_thumbnail_and_late_old_result");
    {
        let (mut app, mut doc, id) = setup();
        let old = app.thumbnail_generation.unwrap();
        app.library_thumbnails
            .insert(id.clone(), to_peniko(&crate::shots_pattern(4, 3)));
        let mut lib = crate::test_doc::library(&doc);
        lib.photos.iter_mut().next().unwrap().1.image.content = "a".repeat(64);
        crate::test_doc::set_library(&mut doc, &lib);
        sync(&mut app, &mut doc);
        assert!(!app.library_thumbnails.contains_key(&id));
        app.accept_thumbnail_result(ThumbnailResult {
            generation: old,
            id: id.clone(),
            preview: Some(crate::shots_pattern(4, 3)),
        });
        assert!(!app.library_thumbnails.contains_key(&id));
    }
}

fn dispatch_unmatched_action(app: &mut PhotosApp, doc: &mut AutoCommit) {
    app.perform_semantic_action(
        "nonexistent",
        &AccessAction::Activate,
        &mut ui::widget::Edits::lend(doc),
        &ROOT,
    );
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per phase")]
#[test]
fn changed_targets_invalidate_metadata_and_album_drafts() {
    eprintln!(
        "phase changed_target_invalidates_metadata_draft_before_input_but_unrelated_change_preserves_it"
    );
    {
        let (mut app, mut doc, id) = setup();
        let mut lib = crate::test_doc::library(&doc);
        lib.photos.iter_mut().next().unwrap().1.tags =
            crate::model::labels(["restored".to_owned()]);
        crate::test_doc::set_library(&mut doc, &lib);
        let selected = doc.get_heads();
        lib.photos.iter_mut().next().unwrap().1.tags = crate::model::labels(["live".to_owned()]);
        crate::test_doc::set_library(&mut doc, &lib);
        sync(&mut app, &mut doc);
        app.begin_metadata_editor(&id);
        app.metadata_tags_input.set_text("unsaved draft");
        crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default().rotated_right());
        sync(&mut app, &mut doc);
        assert!(app.metadata_editor);
        assert_eq!(app.metadata_tags_input.value(), "unsaved draft");
        restore(&mut doc, &selected);
        present(&mut app, &doc);
        present(&mut app, &doc);
        dispatch_unmatched_action(&mut app, &mut doc);
        assert!(
            !app.metadata_editor,
            "stale draft must detach before any new input"
        );
        app.save_metadata_editor();
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::library(&doc)
                .photos
                .values()
                .next()
                .unwrap()
                .tags(),
            vec!["restored"]
        );
        app.begin_metadata_editor(&id);
        app.metadata_tags_input.set_text("next edit");
        app.save_metadata_editor();
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::library(&doc)
                .photos
                .values()
                .next()
                .unwrap()
                .tags(),
            vec!["next edit"]
        );
    }

    eprintln!("phase changed_second_batch_photo_invalidates_the_whole_metadata_draft");
    {
        let (mut app, mut doc, first) = setup();
        let second =
            crate::model::LibraryPhoto::from_image(OpenImage::fixture("second-missing.png", 4, 3));
        let second_id = second.id.clone();
        let mut library = crate::test_doc::library(&doc);
        library.add_photo(second);
        crate::test_doc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);
        app.begin_metadata_editor_for_ids(vec![first, second_id.clone()]);
        app.metadata_tags_input.set_text("unsaved batch draft");
        library.photos.iter_mut().nth(1).unwrap().1.tags =
            crate::model::labels(["remote edit".to_owned()]);
        crate::test_doc::set_library(&mut doc, &library);
        present(&mut app, &doc);
        dispatch_unmatched_action(&mut app, &mut doc);
        assert!(
            !app.metadata_editor,
            "a changed non-first batch target invalidates the draft before input"
        );
        app.save_metadata_editor();
        present(&mut app, &doc);
        assert_eq!(
            crate::test_doc::library(&doc)
                .photo(&second_id)
                .unwrap()
                .tags(),
            vec!["remote edit"]
        );
    }

    eprintln!("phase changed_album_or_deleted_member_detaches_draft_and_cannot_resurrect_it");
    {
        let (mut app, mut doc, id) = setup();
        let selected = doc.get_heads();
        let mut lib = crate::test_doc::library(&doc);
        lib.add_album(crate::model::Album {
            id: "photos.album:membership".into(),
            name: "Original".into(),
            photo_ids: crate::model::labels([id]),
        });
        crate::test_doc::set_library(&mut doc, &lib);
        sync(&mut app, &mut doc);
        app.active_album = Some("photos.album:membership".into());
        app.begin_edit_album();
        app.album_name_input.set_text("stale draft");
        restore(&mut doc, &selected);
        present(&mut app, &doc);
        sync(&mut app, &mut doc);
        assert!(!app.album_editor);
        app.save_album_editor();
        settle(&mut app, &mut doc);
        assert!(crate::test_doc::library(&doc).albums.is_empty());
        app.begin_new_album();
        app.album_name_input.set_text("Next album");
        app.save_album_editor();
        settle(&mut app, &mut doc);
        assert_eq!(
            crate::test_doc::library(&doc)
                .albums
                .values()
                .next()
                .unwrap()
                .name,
            "Next album"
        );
    }
}
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn late_opens_imports_and_restores_settle_against_current_content() {
    eprintln!("phase pending_resource_open_cannot_overwrite_changed_content_and_settles_once");
    {
        let (mut app, mut doc, _) = setup();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("import.png");
        std::fs::write(
            &path,
            pixels::encode_png(&crate::shots_pattern(4, 3)).unwrap(),
        )
        .unwrap();
        let invocation = tilo_actions::InvocationId::new();
        app.answer_effect(
            PhotosEffect::Import {
                path: Some(path.to_string_lossy().into()),
            },
            invocation,
            &crate::test_doc::library(&doc),
        )
        .unwrap();
        empty_library(&mut doc);
        let heads = doc.get_heads();
        sync(&mut app, &mut doc);
        assert!(crate::test_doc::library(&doc).photos.is_empty());
        assert_eq!(doc.get_heads(), heads);
        assert!(matches!(
            app.take_settled(invocation),
            Some(tilo_actions::InvocationUpdate::Cancelled { .. })
        ));
        assert!(app.take_settled(invocation).is_none(), "it settles once");
    }

    eprintln!(
        "phase already_dispatched_picker_reports_actual_outcome_without_importing_stale_selection"
    );
    {
        for outcome in [0, 1, 2] {
            let (mut app, mut doc, _) = setup();
            let invocation = tilo_actions::InvocationId::new();
            let (tx, rx) = std::sync::mpsc::channel();
            app.picker = Some(rx);
            app.resource_invocation = Some(invocation);
            app.import_content = Some(LiveContent {
                library: crate::test_doc::library(&doc),
            });
            empty_library(&mut doc);
            sync(&mut app, &mut doc);
            tx.send(match outcome {
                0 => ui::portal::PortalOutcome::Opened(std::path::PathBuf::from("chosen.png")),
                1 => ui::portal::PortalOutcome::Cancelled,
                _ => ui::portal::PortalOutcome::Failed("actual picker failure".into()),
            })
            .unwrap();
            let heads = doc.get_heads();
            sync(&mut app, &mut doc);
            assert_eq!(doc.get_heads(), heads);
            assert!(crate::test_doc::library(&doc).photos.is_empty());
            match app.take_settled(invocation).unwrap() {
                tilo_actions::InvocationUpdate::Failed {
                    invocation: id,
                    error,
                } => {
                    assert_eq!(id, invocation);
                    assert_eq!(
                        error.code,
                        if outcome == 0 {
                            "photos.stale_selection"
                        } else {
                            "photos.portal_failed"
                        }
                    );
                    if outcome == 0 {
                        assert!(error.message.contains("chosen.png"));
                    } else {
                        assert_eq!(error.message, "actual picker failure");
                    }
                }
                tilo_actions::InvocationUpdate::Cancelled { invocation: id, .. } => {
                    assert_eq!(id, invocation);
                    assert_eq!(outcome, 1);
                }
                other => panic!("unexpected settlement {other:?}"),
            }
            assert!(app.take_settled(invocation).is_none());
        }
    }

    eprintln!("phase failed_import_reports_failure_even_when_previous_photo_is_still_decoded");
    {
        let mut doc = crate::test_doc::new_doc();
        let store = ContentStore::memory();
        let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
        let open = OpenImage::new("existing.png", 6, 4, store.put(&bytes).unwrap());
        let id = crate::test_doc::put_image(&mut doc, &open);
        let mut app = test_app_on(store);
        app.show_photo(&id);
        sync(&mut app, &mut doc);
        assert!(app.open_image_path().is_some());
        let invocation = tilo_actions::InvocationId::new();
        let tmp = tempfile::tempdir().unwrap();
        app.answer_effect(
            PhotosEffect::Import {
                path: Some(tmp.path().join("absent.png").to_string_lossy().into()),
            },
            invocation,
            &crate::test_doc::library(&doc),
        )
        .unwrap();
        sync(&mut app, &mut doc);
        assert!(matches!(
            app.take_settled(invocation),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
        assert_eq!(app.open_photo_id(), Some(id.as_str()));
        assert_eq!(crate::test_doc::library(&doc).photos.len(), 1);
    }

    eprintln!("phase content_restore_drops_active_viewer_drag_before_next_pointer_move");
    {
        let mut doc = crate::test_doc::new_doc();
        let store = ContentStore::memory();
        let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
        let open = OpenImage::new("same.png", 6, 4, store.put(&bytes).unwrap());
        let id = crate::test_doc::put_image(&mut doc, &open);
        crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default().rotated_right());
        let selected = doc.get_heads();
        crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default());
        let mut app = test_app_on(store);
        app.show_photo(&id);
        sync(&mut app, &mut doc);
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let theme = Theme::dark();
        let node = app.build_layout(&mut taffy, &theme, &mut nodes);
        taffy
            .set_style(
                node,
                Style {
                    size: Size::from_lengths(800., 600.),
                    ..Default::default()
                },
            )
            .unwrap();
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(800.),
                    height: AvailableSpace::Definite(600.),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
        let center = app.viewer.as_ref().unwrap().get_rect().center();
        let device_id = winit::event::DeviceId::dummy();
        let mut handled = false;
        let mut focus = None;
        for event in [
            winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            },
            winit::event::WindowEvent::MouseInput {
                device_id,
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        ] {
            app.viewer.as_mut().unwrap().handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.,
                    modifiers: &winit::event::Modifiers::default(),
                    state: ui::widget::Edits::lend(&mut doc),
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }
        restore(&mut doc, &selected);
        present(&mut app, &doc);
        app.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(center.x + 40., center.y + 20.),
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert_eq!(app.viewer.as_ref().unwrap().pan(), vello::kurbo::Vec2::ZERO);
        assert!((app.viewer.as_ref().unwrap().zoom() - 1.).abs() < f32::EPSILON);
        app.edit_shown(|photo| PhotoLibrary::ROTATE_RIGHT.with(photo));
        sync(&mut app, &mut doc);
        assert_eq!(crate::test_doc::photo_edits(&doc, &id).rotate_quarters, 2);
    }
}

fn album_membership_fixture() -> (PhotosApp, AutoCommit, [String; 4]) {
    let mut doc = crate::test_doc::new_doc();
    // Browse order deliberately differs from lexical identity order.
    let photos: Vec<_> = ["c", "a", "d", "b"]
        .into_iter()
        .map(|label| {
            LibraryPhoto::from_image(OpenImage::new(
                &format!("{label}.png"),
                4,
                3,
                label.repeat(64),
            ))
        })
        .collect();
    let ids = std::array::from_fn(|i| photos[i].id.clone());
    crate::test_doc::set_library(
        &mut doc,
        &crate::model::PhotoLibrary::of(photos, Vec::new()),
    );
    let mut app = test_app();
    app.library_mode = true;
    sync(&mut app, &mut doc);
    (app, doc, ids)
}

fn activate_album_control(app: &mut PhotosApp, doc: &mut AutoCommit, target: &str) {
    assert!(app.perform_semantic_action(
        target,
        &AccessAction::Activate,
        &mut ui::widget::Edits::lend(doc),
        &ROOT
    ));
    sync(app, doc);
}

fn seed_membership_album(doc: &mut AutoCommit, ids: &[String; 4]) {
    let mut library = crate::test_doc::library(doc);
    library.add_album(crate::model::Album {
        id: "photos.album:membership".into(),
        name: "Original".into(),
        photo_ids: crate::model::labels([ids[0].clone(), ids[3].clone()]),
    });
    crate::test_doc::set_library(doc, &library);
}

#[test]
fn album_membership_edits_save_selected_ids_in_browse_order() {
    eprintln!("phase album_membership_create_saves_selected_ids_in_browse_order");
    {
        let (mut app, mut doc, ids) = album_membership_fixture();
        activate_album_control(&mut app, &mut doc, "photos:album-editor:new");
        app.album_name_input.set_text("Selected photographs");
        // Select in reverse browse order; toggle a third photograph off again.
        for i in [3, 1, 0, 1] {
            activate_album_control(
                &mut app,
                &mut doc,
                &format!("photos:library-photo:{}", ids[i]),
            );
        }
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = crate::test_doc::library(&doc);
        assert_eq!(library.albums.len(), 1);
        assert_eq!(
            library.albums.values().next().unwrap().name,
            "Selected photographs"
        );
        let album = library.albums.values().next().unwrap();
        assert_eq!(
            library.album_photos(album).collect::<Vec<_>>(),
            [ids[0].as_str(), ids[3].as_str()],
            "an album shows its photographs in browse order"
        );
        assert!(!app.album_editor);
    }

    eprintln!("phase album_membership_rename_preserves_selected_ids_and_order");
    {
        let (mut app, mut doc, ids) = album_membership_fixture();
        seed_membership_album(&mut doc, &ids);
        sync(&mut app, &mut doc);
        activate_album_control(
            &mut app,
            &mut doc,
            "photos:library-album:photos.album:membership",
        );
        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Renamed album");
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = crate::test_doc::library(&doc);
        assert_eq!(library.albums.len(), 1);
        let album = library.album("photos.album:membership").unwrap();
        assert_eq!(album.name, "Renamed album");
        assert_eq!(
            library.album_photos(album).collect::<Vec<_>>(),
            [ids[0].as_str(), ids[3].as_str()]
        );
        assert!(!app.album_editor);
    }

    eprintln!("phase album_membership_after_restore_supports_normal_edit_and_browse_order");
    {
        let (mut app, mut doc, ids) = album_membership_fixture();
        seed_membership_album(&mut doc, &ids);
        let selected = doc.get_heads();
        let mut library = crate::test_doc::library(&doc);
        library.albums.iter_mut().next().unwrap().1.name = "Later album".into();
        library.albums.iter_mut().next().unwrap().1.photo_ids =
            crate::model::labels([ids[1].clone(), ids[2].clone()]);
        crate::test_doc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);
        activate_album_control(
            &mut app,
            &mut doc,
            "photos:library-album:photos.album:membership",
        );
        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Unsaved later draft");
        restore(&mut doc, &selected);
        present(&mut app, &doc);
        sync(&mut app, &mut doc);
        assert!(!app.album_editor, "restore must detach the obsolete draft");
        let restored = crate::test_doc::library(&doc);
        let album = restored.album("photos.album:membership").unwrap();
        assert_eq!(album.name, "Original");
        assert_eq!(
            restored.album_photos(album).collect::<Vec<_>>(),
            [ids[0].as_str(), ids[3].as_str()]
        );

        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Edited after restore");
        activate_album_control(&mut app, &mut doc, "photos:library-album:all");
        for i in [3, 2] {
            activate_album_control(
                &mut app,
                &mut doc,
                &format!("photos:library-photo:{}", ids[i]),
            );
        }
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = crate::test_doc::library(&doc);
        let album = library.album("photos.album:membership").unwrap();
        assert_eq!(album.name, "Edited after restore");
        assert_eq!(
            library.album_photos(album).collect::<Vec<_>>(),
            [ids[0].as_str(), ids[2].as_str()]
        );
        assert_eq!(library.photos.len(), 4);
        assert!(!app.album_editor);
    }
}
