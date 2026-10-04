#![allow(
    clippy::doc_markdown,
    reason = "the shared conformance support is included verbatim from the toolkit"
)]
include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
use crate::{
    doc::PhotosDoc,
    model::{Edits, OpenImage, PhotoLibrary},
};
use tilo_automerge::ApplyIncoming;
use ui::widget::Widget;

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn production_history_root_and_sync_preserve_the_exact_graph() {
    eprintln!("phase production_root_declares_framework_history");
    {
        assert!(
            ui::history_mode::capability::document_history_definition(crate::app_root().as_ref())
                .is_some()
        );
    }

    eprintln!("phase blank_production_sync_preserves_exact_graph");
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "runtime_metadata", "current").unwrap();
        let heads = doc.get_heads();
        crate::app_root().sync(&mut doc, &ROOT);
        assert_eq!(
            doc.get_heads(),
            heads,
            "sync must not author or migrate content"
        );
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        crate::app_root().sync(&mut reopened, &ROOT);
        assert_eq!(reopened.get_heads(), heads);
    }

    eprintln!("phase exact_current_roots_tombstones_unknown_authored_fields_survive_restore");
    {
        {
            let (open, _, _) = image_fixture();
            let mut doc = AutoCommit::new();
            PhotosDoc::set_image(&mut doc, Some(&open));
            let root = doc.get(ROOT, "photos_library").unwrap().unwrap().1;
            doc.put(&root, "future_authored_setting", "retained")
                .unwrap();
            let removed = doc.get(&root, "removed_photos").unwrap().unwrap().1;
            doc.put(&removed, "deleted-photo", true).unwrap();
            let selected = doc.get_heads();
            let saved = doc.save();
            PhotosDoc::set_image(&mut doc, None);
            doc.put(ROOT, "receipt", "current").unwrap();
            let heads = doc.get_heads();
            let plan =
                prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &selected)
                    .unwrap();
            apply_prepared_content_restore(&mut doc, &plan).unwrap();
            assert_eq!(doc.get_changes(&heads).len(), 1);
            let projection = project_content(
                &AutoCommit::load(&saved).unwrap(),
                &crate::framework_history::SCHEMA,
            )
            .unwrap();
            for _ in 0..2 {
                crate::app_root().sync(&mut doc, &ROOT);
                assert_eq!(
                    project_content(&doc, &crate::framework_history::SCHEMA).unwrap(),
                    projection
                );
                doc = AutoCommit::load(&doc.save()).unwrap();
            }
            assert_eq!(
                doc.get(ROOT, "receipt").unwrap().unwrap().0.as_str(),
                Some("current")
            );
            assert!(doc.get_change_by_hash(&selected[0]).is_some());
        }
    }

    eprintln!("phase typed_schema_refuses_defaulted_malformed_values_without_writes");
    {
        for case in 0..7 {
            let (mut doc, _, _) = fixture();
            let edits = doc.get(ROOT, "photos_edits").unwrap().unwrap().1;
            let library = doc.get(ROOT, "photos_library").unwrap().unwrap().1;
            match case {
                0 => doc.put(ROOT, "image", "{}").unwrap(),
                1 => doc.put(&edits, "rotate_quarters", "4").unwrap(),
                2 => doc
                    .put(&edits, "crop", r#"{"x":0,"y":0,"w":2,"h":1}"#)
                    .unwrap(),
                3 => doc.delete(&edits, "crop").unwrap(),
                4 => doc.put(&library, "photo_order", false).unwrap(),
                5 => doc.put(&library, "photo_core", "bad").unwrap(),
                _ => {
                    let tombstones = doc.get(&library, "removed_photos").unwrap().unwrap().1;
                    doc.put(tombstones, "id", "true").unwrap();
                }
            }
            let heads = doc.get_heads();
            let input = frozen_input(
                &doc,
                heads[0],
                FrozenPreviewAssets::default(),
                PreviewView::default(),
            );
            assert!(
                definition().scopes[0]
                    .presentation
                    .preview_factory()
                    .unwrap()(input)
                .is_err(),
                "case {case}"
            );
            assert_eq!(doc.get_heads(), heads);
        }
    }
}
#[allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    reason = "one end-to-end request custody sequence with local fixture helpers"
)]
#[test]
fn history_previews_use_actual_historic_pixels_and_never_substitute_current_ones() {
    eprintln!("phase same_path_changed_content_refreshes_actual_pixels");
    {
        let mut doc = AutoCommit::new();
        let store = tilo_content::ContentStore::memory();
        let mut app = crate::PhotosApp::with_content(store.clone());
        let mut expected = Vec::new();
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            let rgba = crate::pixels::Rgba::new(2, 1, color.repeat(2)).unwrap();
            let bytes = crate::pixels::encode_png(&rgba).unwrap();
            let hash = store.put(&bytes).unwrap();
            let open = OpenImage::from_path("same.png", 2, 1).with_content(hash);
            PhotosDoc::set_image(&mut doc, Some(&open));
            app.sync(&mut doc, &ROOT);
            expected.push(rgba);
            assert_eq!(app.edited_pixels(), expected.last());
        }
    }

    eprintln!(
        "phase same_path_old_new_restore_undo_redo_uses_actual_historic_pixels_and_current_metadata"
    );
    {
        let (old, old_rgba, old_bytes) = image_fixture();
        let new_rgba = crate::pixels::Rgba::new(6, 4, [0, 255, 0, 255].repeat(24)).unwrap();
        let new_bytes = crate::pixels::encode_png(&new_rgba).unwrap();
        let new = old
            .clone()
            .with_content(tilo_content::hash_bytes(&new_bytes));
        let mut doc = AutoCommit::new();
        PhotosDoc::set_image(&mut doc, Some(&old));
        let selected = doc.get_heads()[0];
        PhotosDoc::set_image(&mut doc, Some(&new));
        doc.put(ROOT, "view_only", "current").unwrap();
        let live = doc.get_heads()[0];
        for (hash, open, bytes, expected) in [
            (selected, &old, &old_bytes, &old_rgba),
            (live, &new, &new_bytes, &new_rgba),
        ] {
            let mut p = prepared(&doc, hash, assets(open, bytes), PreviewView::default());
            assert_eq!(
                painted_images(&painted(p.as_mut(), 1000., 1.))[0]
                    .data
                    .data(),
                expected.bytes
            );
        }
        let mut driver = managed_driver(doc, selected);
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        driver.click_named("history-back-live").unwrap();
        let store = tilo_content::ContentStore::memory();
        store.put(&old_bytes).unwrap();
        store.put(&new_bytes).unwrap();
        let mut app = crate::PhotosApp::with_content(store);
        app.sync(&mut driver.state_mut().doc, &ROOT);
        assert_eq!(app.edited_pixels(), Some(&old_rgba));
        assert!(driver.state_mut().undo(None, None).is_some());
        app.sync(&mut driver.state_mut().doc, &ROOT);
        assert_eq!(app.edited_pixels(), Some(&new_rgba));
        assert!(driver.state_mut().redo(None, None).is_some());
        app.sync(&mut driver.state_mut().doc, &ROOT);
        assert_eq!(app.edited_pixels(), Some(&old_rgba));
        let mut reopened = AutoCommit::load(&driver.state_mut().doc.save()).unwrap();
        let heads = reopened.get_heads();
        app.sync(&mut reopened, &ROOT);
        assert_eq!(app.edited_pixels(), Some(&old_rgba));
        assert_eq!(reopened.get_heads(), heads);
        assert_eq!(
            reopened.get(ROOT, "view_only").unwrap().unwrap().0.as_str(),
            Some("current")
        );
        assert!(reopened.get_change_by_hash(&selected).is_some());
    }

    eprintln!("phase corrupt_missing_and_existing_legacy_path_never_substitute_current_pixels");
    {
        let (open, _, bytes) = image_fixture();
        let mut doc = AutoCommit::new();
        PhotosDoc::set_image(&mut doc, Some(&open));
        let selected = doc.get_heads()[0];
        for asset in [FrozenPreviewAssets::default(), assets(&open, b"corrupt")] {
            let mut p = prepared(&doc, selected, asset, PreviewView::default());
            assert!(painted_images(&painted(p.as_mut(), 1000., 1.)).is_empty());
            assert!(p.accessibility().description.contains("unavailable"));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exists.png");
        std::fs::write(&path, &bytes).unwrap();
        PhotosDoc::set_image(
            &mut doc,
            Some(&OpenImage::from_path(&path.to_string_lossy(), 6, 4)),
        );
        let selected = doc.get_heads()[0];
        let mut p = prepared(
            &doc,
            selected,
            FrozenPreviewAssets::default(),
            PreviewView::default(),
        );
        assert!(painted_images(&painted(p.as_mut(), 1000., 1.)).is_empty());
        assert!(p.accessibility().description.contains("unavailable"));
    }

    eprintln!(
        "phase late_old_selection_compare_side_and_closed_host_assets_never_replace_current_preview"
    );
    {
        use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};
        #[derive(Default)]
        struct Held(RefCell<Vec<(HistoricalAssetRequest, HistoricalAssetResponse)>>);
        impl HistoricalAssetSource for Held {
            fn request(&self, r: HistoricalAssetRequest, s: HistoricalAssetResponse) {
                self.0.borrow_mut().push((r, s));
            }
        }
        let (old, _, old_bytes) = image_fixture();
        let new_bytes = crate::pixels::encode_png(
            &crate::pixels::Rgba::new(6, 4, [0, 255, 0, 255].repeat(24)).unwrap(),
        )
        .unwrap();
        let new = old
            .clone()
            .with_content(tilo_content::hash_bytes(&new_bytes));
        let mut doc = AutoCommit::new();
        PhotosDoc::set_image(&mut doc, Some(&old));
        let selected = doc.get_heads()[0];
        PhotosDoc::set_image(&mut doc, Some(&new));
        doc.put(ROOT, "timeline_marker", "live").unwrap();
        let live = doc.get_heads()[0];
        let held = Rc::new(Held::default());
        let def = definition();
        let registration = HistoryRegistration::new(
            def,
            vec![HistorySourceRegistration::HostDocument {
                scope: "photos",
                resource: HistoryResource::new("p", "r", def.app_id, ui::RecordId::for_tab(55)),
            }],
        )
        .unwrap()
        .with_asset_source("photos", held.clone())
        .unwrap();
        let mut driver = ui::headless::Driver::from_managed_root(
            ui::State::from_doc(doc),
            ManagedRoot::new(test_root(), registration),
            (1000, 750),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        select_version(&mut driver, selected);
        let stale = held.0.take();
        assert!(!stale.is_empty());
        select_version(&mut driver, live);
        let current = held.0.take();
        assert_eq!(current.len(), 2);
        for (request, response) in current {
            let image =
                PhotosDoc::image(&AutoCommit::load(request.snapshot.bytes()).unwrap()).unwrap();
            let bytes = if image.content == new.content {
                &new_bytes
            } else {
                &old_bytes
            };
            response.complete(Ok(BTreeMap::from([(
                crate::framework_history::asset_id(&image).unwrap(),
                Arc::from(bytes.clone()),
            )])));
        }
        driver.resize((1000, 750)).unwrap();
        driver.click_named("history-after").unwrap();
        let before = format!("{:?}", driver.a11y_nodes());
        assert!(!before.contains("Historical pixels unavailable"));
        for (_, response) in stale {
            response.complete(Err(
                "late old response must not replace active pixels".into()
            ));
        }
        driver.resize((1000, 750)).unwrap();
        assert_eq!(format!("{:?}", driver.a11y_nodes()), before);
        driver.click_named("history-before").unwrap();
        assert!(!format!("{:?}", driver.a11y_nodes()).contains("Historical pixels unavailable"));
        // Re-select in the SAME host after comparing Before. A resize pumps
        // completions without typing into the timeline's search field.
        let target = format!(
            "timeline-entry:{}",
            ui::widget::history_group::hex16(&ui::history_source::entry_id_from_hash(
                &selected.to_string()
            ))
        );
        assert!(
            driver.inspection_json().unwrap().contains(&target),
            "the selected row remains exported after Before comparison"
        );
        select_version(&mut driver, selected);
        let closed_same_host = held.0.take();
        assert!(!closed_same_host.is_empty());
        driver.click_named("history-back-live").unwrap();
        for (_, response) in closed_same_host {
            response.complete(Err("late same-host closed preview".into()));
        }
        driver.resize((1000, 750)).unwrap();
        assert!(!format!("{:?}", driver.a11y_nodes()).contains("late same-host closed preview"));
        // A distinct host/resource owns the next request. Closing it cannot deliver
        // either pixels or failure state into the still-mounted first host.
        let registration = HistoryRegistration::new(
            def,
            vec![HistorySourceRegistration::HostDocument {
                scope: "photos",
                resource: HistoryResource::new(
                    "other-profile",
                    "other-record",
                    def.app_id,
                    ui::RecordId::for_tab(66),
                ),
            }],
        )
        .unwrap()
        .with_asset_source("photos", held.clone())
        .unwrap();
        let mut closing_driver = ui::headless::Driver::from_managed_root(
            ui::State::from_doc(driver.state_mut().doc.clone()),
            ManagedRoot::new(test_root(), registration),
            (1000, 750),
        )
        .unwrap();
        closing_driver.click_named("history-open").unwrap();
        select_version(&mut closing_driver, selected);
        let closing = held.0.take();
        assert!(!closing.is_empty());
        closing_driver.click_named("history-back-live").unwrap();
        drop(closing_driver);
        let visible = format!("{:?}", driver.a11y_nodes());
        for (_, response) in closing {
            response.complete(Err("late closed host".into()));
        }
        driver.resize((1000, 750)).unwrap();
        assert_eq!(format!("{:?}", driver.a11y_nodes()), visible);
    }
}

fn test_root() -> Box<dyn Widget> {
    crate::app_root()
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "runtime_metadata", "initial").unwrap();
    doc.get_heads();
    PhotosDoc::set_image(&mut doc, Some(&OpenImage::from_path("historic.png", 4, 3)));
    let selected = doc.get_heads()[0];
    PhotosDoc::set_image(&mut doc, Some(&OpenImage::from_path("live.png", 4, 3)));
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "historic.png".into())
}
fn verify_restored(doc: &AutoCommit) {
    assert_eq!(PhotosDoc::image(doc).unwrap().name, "historic.png");
}
fn malformed_fixture(doc: &mut AutoCommit) {
    doc.put(ROOT, "image", 123).unwrap();
}

fn frozen_input(
    doc: &AutoCommit,
    selected: automerge::ChangeHash,
    assets: FrozenPreviewAssets,
    view: PreviewView,
) -> PreviewInput {
    let mut controller = HistoryController::new(
        HistoryResource::new(
            "test",
            "photos",
            "so.tilo.photos",
            ui::RecordId::for_tab(55),
        ),
        doc,
    )
    .unwrap();
    controller
        .select(
            doc,
            &[ui::history_source::entry_id_from_hash(
                &selected.to_string(),
            )],
        )
        .unwrap();
    PreviewInput {
        snapshot: controller.snapshot().unwrap().clone(),
        initial_view: view,
        assets,
    }
}
fn prepared(
    doc: &AutoCommit,
    selected: automerge::ChangeHash,
    assets: FrozenPreviewAssets,
    view: PreviewView,
) -> Box<dyn HistoryPreview> {
    definition().scopes[0]
        .presentation
        .preview_factory()
        .unwrap()(frozen_input(doc, selected, assets, view))
    .unwrap()
}
fn image_fixture() -> (OpenImage, crate::pixels::Rgba, Vec<u8>) {
    let rgba = crate::shots_pattern(6, 4);
    let bytes = crate::pixels::encode_png(&rgba).unwrap();
    let hash = tilo_content::hash_bytes(&bytes);
    (
        OpenImage::from_path("same.png", 6, 4).with_content(hash),
        rgba,
        bytes,
    )
}
fn assets(open: &OpenImage, bytes: &[u8]) -> FrozenPreviewAssets {
    FrozenPreviewAssets::new(std::collections::BTreeMap::from([(
        crate::framework_history::asset_id(open).unwrap(),
        std::sync::Arc::from(bytes),
    )]))
}
fn painted(p: &mut dyn HistoryPreview, width: f64, scale: f32) -> vello::Scene {
    let mut tr = ui::TextRenderer::new();
    let theme = ui::Theme::dark().with_text_scale(scale);
    tr.set_text_scale(scale);
    p.layout(
        vello::kurbo::Rect::new(0., 0., width, 600.),
        &mut tr,
        &theme,
    )
    .unwrap();
    let mut scene = vello::Scene::new();
    p.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut tr, &theme);
    scene
}
fn painted_images(scene: &vello::Scene) -> Vec<vello::peniko::ImageData> {
    scene
        .encoding()
        .resources
        .patches
        .iter()
        .filter_map(|p| {
            if let vello_encoding::Patch::Image { image, .. } = p {
                Some(image.clone())
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn frozen_selected_pixels_follow_real_crop_then_quarter_turn_and_recreate_pan_zoom() {
    let (open, rgba, bytes) = image_fixture();
    let mut doc = AutoCommit::new();
    PhotosDoc::set_image(&mut doc, Some(&open));
    for turns in 0..4 {
        let edits = Edits {
            crop: crate::Rect01 {
                x: 1. / 6.,
                y: 0.25,
                w: 0.5,
                h: 0.75,
            },
            rotate_quarters: turns,
        };
        PhotosDoc::set_edits(&mut doc, &edits);
        let selected = doc.get_heads()[0];
        let expected = crate::pixels::render(&rgba, edits.crop, turns);
        let mut p = prepared(
            &doc,
            selected,
            assets(&open, &bytes),
            PreviewView::default(),
        );
        for (width, scale) in [(1000., 1.), (360., 2.)] {
            let images = painted_images(&painted(p.as_mut(), width, scale));
            assert_eq!(images.len(), 1);
            assert_eq!(
                (images[0].width, images[0].height),
                (expected.w, expected.h)
            );
            assert_eq!(images[0].data.data(), expected.bytes);
            assert!(p.navigate(PreviewNavigation::Zoom {
                factor: 2.,
                anchor: vello::kurbo::Point::new(width / 2., 300.)
            }));
            assert!(p.navigate(PreviewNavigation::Scroll { dx: 25., dy: 40. }));
            let view = p.view();
            let mut recreated = prepared(&doc, selected, assets(&open, &bytes), view.clone());
            painted(recreated.as_mut(), width, scale);
            assert_eq!(recreated.view(), view);
            assert!(!p.navigate(PreviewNavigation::Zoom {
                factor: f64::NAN,
                anchor: vello::kurbo::Point::ZERO
            }));
            assert!(!p.navigate(PreviewNavigation::Scroll {
                dx: f64::INFINITY,
                dy: 0.
            }));
        }
    }
}
#[test]
fn grid_album_favorites_metadata_are_painted_and_paginated_at_narrow_large_text() {
    let (open, _, bytes) = image_fixture();
    let mut library = PhotoLibrary::default();
    for i in 0..27 {
        let mut p = crate::model::LibraryPhoto::from_image(open.clone());
        p.id = format!("photo-{i}");
        p.image.name = format!("Photo {i}");
        p.favorite = i == 0;
        p.tags = vec!["historic tag".into()];
        library.photos.push(p);
    }
    library.albums.push(crate::model::Album {
        id: "album".into(),
        name: "Recorded album".into(),
        photo_ids: library.photos.iter().map(|p| p.id.clone()).collect(),
    });
    let mut doc = AutoCommit::new();
    PhotosDoc::set_library(&mut doc, &library);
    let selected = doc.get_heads()[0];
    let mut p = prepared(
        &doc,
        selected,
        assets(&open, &bytes),
        PreviewView::default(),
    );
    assert!(p.accessibility().description.contains("Favorite"));
    assert!(p.accessibility().description.contains("historic tag"));
    assert!(p.accessibility().description.contains("Recorded album"));
    for (width, scale) in [(1000., 1.), (360., 2.)] {
        assert!(!painted_images(&painted(p.as_mut(), width, scale)).is_empty());
        p.navigate(PreviewNavigation::Scroll {
            dx: 0.,
            dy: -99999.,
        });
        let scene = painted(p.as_mut(), width, scale);
        assert!(!painted_images(&scene).is_empty());
        assert!(p.view().scroll.1 > 0.);
        assert!(p.navigate(PreviewNavigation::SelectPage("album:album:1".into())));
        assert!(p.accessibility().description.contains("Photo 26"));
        assert!(p.navigate(PreviewNavigation::Reveal("photo:photo-26".into())));
        assert_eq!(
            painted_images(&painted(p.as_mut(), width, scale))[0]
                .data
                .data(),
            crate::shots_pattern(6, 4).bytes
        );
        assert!(p.navigate(PreviewNavigation::SelectPage("library:0".into())));
    }
}
/// The preview is built only from the frozen snapshot and the assets admitted
/// for it: it is handed no content store, and its source reaches for none.
#[test]
fn frozen_preparation_layout_navigation_and_drop_have_no_live_store_or_transport_access() {
    let bytes = crate::pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
    let open = OpenImage::from_path("not-readable.png", 6, 4)
        .with_content(tilo_content::hash_bytes(&bytes));
    let mut doc = AutoCommit::new();
    PhotosDoc::set_image(&mut doc, Some(&open));
    let heads = doc.get_heads();
    // Invoke the production factory directly, excluding the asset provider's lookup.
    let factory = crate::framework_history::HISTORY.scopes[0]
        .presentation
        .preview_factory()
        .unwrap();
    let mut p = factory(frozen_input(
        &doc,
        heads[0],
        assets(&open, &bytes),
        PreviewView::default(),
    ))
    .unwrap();
    painted(p.as_mut(), 360., 2.);
    p.navigate(PreviewNavigation::NextPage);
    painted(p.as_mut(), 1000., 1.);
    p.accessibility();
    drop(p);
    assert_eq!(doc.get_heads(), heads);
    assert_eq!(doc.pending_ops(), 0);
    let source = include_str!("presentation.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    for forbidden in [
        "PhotosApp",
        "bytes_of(",
        "std::fs",
        "ContentStore",
        "ContentSource",
        "portal::",
        "clipboard",
        "ActionHost",
        "PhotosSearchProvider",
        "thread::",
    ] {
        assert!(
            !source.contains(forbidden),
            "presentation gained live capability {forbidden}"
        );
    }
}

#[test]
fn conflicts_preview_unflattened_but_restore_refuses_them() {
    eprintln!(
        "phase supported_conflict_branches_preview_without_flattening_but_restore_refuses_conflict"
    );
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "metadata", "base").unwrap();
        doc.get_heads();
        let mut peer = doc.fork();
        PhotosDoc::set_image(&mut doc, Some(&OpenImage::from_path("one.png", 4, 3)));
        PhotosDoc::set_image(&mut peer, Some(&OpenImage::from_path("two.png", 4, 3)));
        doc.merge_incoming(&mut peer).unwrap();
        let heads = doc.get_heads();
        let mut controller = HistoryController::new(
            HistoryResource::new("p", "r", "so.tilo.photos", ui::RecordId::for_tab(8)),
            &doc,
        )
        .unwrap();
        controller
            .select(
                &doc,
                &heads
                    .iter()
                    .map(|h| ui::history_source::entry_id_from_hash(&h.to_string()))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let p = crate::framework_history::HISTORY.scopes[0]
            .presentation
            .preview_factory()
            .unwrap()(PreviewInput {
            snapshot: controller.snapshot().unwrap().clone(),
            initial_view: PreviewView::default(),
            assets: FrozenPreviewAssets::default(),
        })
        .unwrap();
        assert!(p.accessibility().description.contains(".png"));
        assert_eq!(doc.get_heads(), heads);
        assert!(
            prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &heads)
                .is_err()
        );
    }
}
