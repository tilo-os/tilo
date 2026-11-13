use super::*;
use crate::actions::PdfEffect;
use ui::framework::LiveRow;
use ui::tilo_actions::OperationKind;
use ui::tilo_resource::Effect;

#[test]
fn duplicating_a_rotated_page_keeps_its_orientation_and_source() {
    use ui::tilo_resource::Replica;

    // The degrees asked for, and the orientation the page then has: a turn
    // is kept within one revolution, whichever way and however far it goes.
    for (degrees, rotation) in [
        (0, 0),
        (90, 90),
        (180, 180),
        (270, 270),
        (360, 0),
        (450, 90),
        (-90, 270),
    ] {
        let mut replica = Replica::<PdfMarkup>::load(&PdfMarkup::planned(2)).expect("record");
        replica
            .apply(crate::actions::rotate_page_action("source-1", degrees))
            .unwrap_or_else(|error| panic!("rotate original by {degrees}: {error:?}"));
        replica
            .apply(
                PdfMarkup::DUPLICATE_PAGE.with((Id::from_raw("source-1"), Some("copy".to_owned()))),
            )
            .unwrap_or_else(|error| panic!("duplicate the page turned by {degrees}: {error:?}"));
        let state = replica.state().expect("state");
        let original = state
            .pages
            .get(&Id::from_raw("source-1"))
            .expect("original");
        let copy = state.pages.get(&Id::from_raw("copy")).expect("copy");
        assert_eq!(copy.rotation, rotation, "turned by {degrees}");
        assert_eq!(copy, original, "turned by {degrees}");
        let ids: Vec<_> = state.pages.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["source-1", "copy", "source-2"], "turned by {degrees}");
    }
}

#[test]
fn published_pages_carry_duplicate_and_delete() {
    fn find<'a>(
        resource: &'a tilo_actions::ResourceSpec,
        id: &str,
    ) -> Option<&'a tilo_actions::ResourceSpec> {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        resource.children.iter().find_map(|child| find(child, id))
    }
    let replica =
        ui::tilo_resource::Replica::<PdfMarkup>::load(&PdfMarkup::planned(2)).expect("record");
    let published = replica.published().expect("published");
    let page_id =
        ui::tilo_resource::items::item_resource_id::<PdfMarkup>("pages", &Id::from_raw("source-1"));
    let page = find(&published, &page_id).expect("published page");
    let ids: Vec<&str> = page
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for id in ["duplicate-page", "delete-page", "move-page", "rotate-page"] {
        assert!(ids.contains(&id), "{ids:?}");
    }
}

#[serial_test::serial(pdfium)]
#[test]
fn the_surface_publishes_find_hits_beside_the_resource_never_a_document_root() {
    let specs = PdfEffect::specs();
    let ids: Vec<&str> = specs.iter().map(|a| a.id.as_str()).collect();
    for expected in [
        "previous-page",
        "next-page",
        "zoom-in",
        "zoom-out",
        "fit-page",
        "find",
        "open-pdf",
        "save-as",
    ] {
        assert!(
            ids.contains(&expected),
            "palette `{expected}` is a typed effect of the reader: {ids:?}"
        );
    }
    let kind = |id: &str| {
        specs
            .iter()
            .find(|action| action.id == id)
            .map(|action| action.kind)
    };
    assert_eq!(kind("find"), Some(OperationKind::View));
    assert_eq!(kind("open-pdf"), Some(OperationKind::Effect));
    assert!(
        !ids.contains(&"go-to") && !ids.contains(&"go-to-match"),
        "navigation to a place is offered only bound to its row: {ids:?}"
    );

    let Some((mut app, _state)) = crate::app::tests::open_sample_review() else {
        return;
    };
    app.stage_find_for_test("godwit");
    assert!(
        app.find_match_count_for_test() > 0,
        "the sample must yield find hits"
    );
    let rows: Vec<tilo_actions::ResourceSpec> = app
        .document_rows()
        .into_iter()
        .map(LiveRow::into_resource)
        .collect();
    assert!(
        rows.iter().all(|resource| resource.kind != "pdf.document"),
        "the surface never republishes pdf.document beside the resource"
    );
    let hits = rows
        .iter()
        .find(|resource| resource.kind == "pdf.search")
        .expect("the find slice");
    assert!(
        !hits.children.is_empty()
            && hits.children.iter().all(|hit| {
                hit.kind == "pdf.search-result"
                    && hit.actions.iter().all(|action| {
                        action.kind == OperationKind::View && action.id == "go-to-match"
                    })
            }),
        "find hits are navigable rows with view operations only"
    );
    let hit = &hits.children[0];
    assert_eq!(
        app.document_row(hit.id.as_str())
            .map(|resolved| resolved.id().to_owned()),
        Some(hit.id.as_str().to_owned()),
        "a hit beyond the listed page resolves by id"
    );

    app.stage_find_for_test("");
    assert!(
        app.document_rows()
            .iter()
            .all(|row| !row.id().starts_with("pdf.search")),
        "no find slice without a query"
    );
}

#[test]
fn older_markup_keeps_its_annotations_and_leaves_old_threads_behind() {
    use ui::tilo_resource::evolution;
    let mut replica = ui::tilo_resource::Replica::<PdfMarkup>::new();
    let doc = replica.doc_mut();
    let root = ui::automerge::ROOT;
    evolution::write_version(doc, 2).unwrap();
    evolution::fixture(
        doc,
        &root,
        &ui::serde_json::json!({
            "annotations": {
                "a1": {
                    "id": "a1",
                    "page_id": "p1",
                    "mark": r#"{"kind":"note","x":1.0,"y":2.0}"#,
                },
            },
            "comments": {
                "c1": { "id": "c1", "page_id": "p1", "text": "Old", "created": 1 },
            },
        }),
    )
    .unwrap();
    doc.commit();
    let mut old = doc.fork();
    assert!(evolution::migrate::<PdfMarkup>(&mut old).unwrap());
    let markup = evolution::read_checked::<PdfMarkup>(&old).unwrap();
    assert!(markup.comments.is_empty());
    let note = markup.annotations.iter().next().expect("the annotation");
    assert_eq!(note.kind, crate::model::AnnoKind::Note { x: 1.0, y: 2.0 });
}

#[test]
fn a_relative_path_is_refused_for_every_path_action() {
    // Every effect that takes a path, by every way a path is relative.
    let relative = [
        "notes.pdf",
        "./notes.pdf",
        "../notes.pdf",
        "~/notes.pdf",
        "",
    ];
    let absolute = std::env::temp_dir().join("notes.pdf");
    let absolute = absolute.to_str().expect("a UTF-8 temporary directory");
    for id in ["open-pdf", "save-as", "insert-pages"] {
        for path in relative {
            let mut arguments = serde_json::Map::new();
            arguments.insert("path".into(), path.into());
            let Err(error) = PdfEffect::decode(id, &arguments) else {
                panic!("{id} takes the relative {path:?}");
            };
            assert!(
                matches!(
                    &error,
                    ui::tilo_resource::EffectDecodeError::Argument { name, source }
                        if name == "path" && source.to_string().contains("absolute")
                ),
                "{id} with {path:?}: {error}"
            );
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("path".into(), absolute.into());
        if let Err(error) = PdfEffect::decode(id, &arguments) {
            panic!("{id} refuses the absolute {absolute:?}: {error}");
        }
    }
}
