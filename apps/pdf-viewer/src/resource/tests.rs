use super::*;
use ui::tilo_actions::OperationKind;
use ui::tilo_resource::Effect;

#[test]
fn duplicating_a_rotated_page_keeps_its_orientation_and_source() {
    use ui::tilo_resource::Replica;

    for rotation in [90, 180, 270] {
        let mut replica = Replica::<PdfMarkup>::load(&PdfMarkup::planned(2)).expect("record");
        replica
            .apply(crate::actions::rotate_page_action("source-1", rotation))
            .expect("rotate original");
        replica
            .apply(
                PdfMarkup::DUPLICATE_PAGE.with((Id::from_raw("source-1"), Some("copy".to_owned()))),
            )
            .expect("duplicate rotated page");
        let state = replica.state().expect("state");
        let original = state
            .pages
            .get(&Id::from_raw("source-1"))
            .expect("original");
        let copy = state.pages.get(&Id::from_raw("copy")).expect("copy");
        assert_eq!(copy.rotation, rotation);
        assert_eq!(copy, original);
        let ids: Vec<_> = state.pages.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["source-1", "copy", "source-2"]);
    }
}

#[test]
fn published_pages_carry_duplicate_and_delete() {
    fn find<'a>(
        resource: &'a tilo_actions::Resource,
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
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

    let mut app = crate::scenarios::sample_reader();
    let mut state = crate::scenarios::sample_state();
    app.sync_settled(&mut state.doc, &state.root);
    app.stage_find_for_test("sandbox");
    assert!(
        app.find_match_count_for_test() > 0,
        "sample must yield find hits"
    );
    let rows: Vec<tilo_actions::Resource> = app
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
