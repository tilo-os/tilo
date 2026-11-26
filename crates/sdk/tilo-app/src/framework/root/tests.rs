use super::*;
use ui_core::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchQuery};

#[test]
fn child_document_reads_are_bound_to_their_own_resource() {
    let mut child = ResourceSpec::new("book/page/body", "text", "Body");
    child.children.push(ResourceSpec::new(
        "private-child",
        "account",
        "Secret account",
    ));
    offer_document_reads(&mut child, &|id| id == "book/page/body");
    assert!(
        child.children[0].actions.is_empty(),
        "eligibility is not inherited"
    );
    let read = &child.actions[0];
    assert!(read.contract.reads_document());
    let encoded = read.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(encoded.type_id, READ_TYPE);
    let payload: serde_json::Value = tilo_actions::decode_json(&encoded.payload).unwrap();
    assert_eq!(payload["item"], "book/page/body");
    assert!(
        read.build_json(
            serde_json::json!({"item": "another/book"})
                .as_object()
                .unwrap()
        )
        .is_err()
    );
    let encoded = read
        .build_json(serde_json::json!({"version": "past"}).as_object().unwrap())
        .unwrap();
    let payload: serde_json::Value = tilo_actions::decode_json(&encoded.payload).unwrap();
    assert_eq!(payload["item"], "book/page/body");
    assert_eq!(payload["version"], "past");
}

/// The ids the framework keeps are the ones it publishes on every
/// app: the generic actions, `open`, `read`, history and presentation.
#[test]
fn the_reserved_ids_are_what_the_framework_publishes() {
    let reserved = reserved_action_ids();
    for id in [
        "apply",
        "group",
        "field-edit",
        "open",
        "read",
        "undo",
        "redo",
        "restore",
        "restore-reviewed",
        "review-restore",
        "checkpoint",
        "history",
        "diff",
        "render",
    ] {
        assert!(
            reserved.iter().any(|kept| kept == id),
            "{id} in {reserved:?}"
        );
    }
    assert!(
        !reserved.iter().any(|kept| kept == "rename"),
        "an app's own verb name is not kept"
    );
}

tilo_test_env::scenario! {
    fn search_entries_are_hits_ranked_and_found_page_by_page;

    phase a_search_entry_naming_an_action_is_a_hit_that_invokes_it;
    {
        let open = tilo_actions::ActionAddress::new("notes.page:1", "open");
        let search = ResourceSearch {
            name: "Notes",
            app: "so.tilo.notes",
            indexed: Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Groceries", "milk and eggs").invoking(open.clone()),
                    SearchEntry::new("Garden", "roses").anchored("page:2"),
                ],
                0,
            )),
            scopes: Vec::new(),
            queries: Vec::new(),
            readable: true,
            live: None,
            opens: None,
        };
        let groceries = search.search("groceries");
        assert_eq!(groceries.len(), 1);
        assert_eq!(groceries[0].action_address(), Some(open));
        let garden = search.search("garden");
        assert_eq!(garden.len(), 1);
        assert_eq!(garden[0].action_address(), None);
        assert_eq!(garden[0].action_id, "page:2");
    }

    phase a_search_entry_reads_as_its_group_subtitle_and_icon;
    {
        let reveal = tilo_actions::ActionAddress::new("video.marker:1", "reveal");
        let search = ResourceSearch {
            name: "Project",
            app: "so.tilo.project",
            indexed: Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Sunset", "the last light")
                        .invoking(reveal)
                        .grouped("Timeline markers")
                        .subtitled("0:21 · Drone")
                        .with_icon("star")
                        .boosted(5),
                    SearchEntry::new("Sunset take", ""),
                ],
                0,
            )),
            scopes: Vec::new(),
            queries: Vec::new(),
            readable: true,
            live: None,
            opens: None,
        };
        let hits = search.search("sunset");
        let marker = hits
            .iter()
            .find(|hit| hit.title == "Sunset")
            .expect("the marker is found");
        assert_eq!(marker.kind, "Timeline markers");
        assert_eq!(marker.subtitle.as_deref(), Some("0:21 · Drone"));
        assert_eq!(marker.icon, Some(ui_core::icons::Icon::Star));
        let plain = hits
            .iter()
            .find(|hit| hit.title == "Sunset take")
            .expect("the plain entry is found");
        assert_eq!(plain.kind, "Project", "an ungrouped hit is the resource's");
        assert!(plain.preview.is_none(), "no text, no preview");
        // Subtitles are searched too: a marker is found by the shot it is on.
        assert!(
            search
                .search("drone")
                .iter()
                .any(|hit| hit.title == "Sunset")
        );
    }

    phase every_entry_stays_findable_page_by_page;
    {
        let search = ResourceSearch::new(
            "Tracks",
            "so.tilo.test",
            Arc::new(IndexedEntries::built(
                (0..257)
                    .map(|n| SearchEntry::new(format!("Common track {n}"), ""))
                    .collect(),
                0,
            )),
        );
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = search.search_page(&SearchQuery::new("common"), cursor.as_ref(), 17, 5);
            assert!(page.results.len() <= 5);
            for hit in &page.results {
                assert!(seen.insert(hit.title.clone()), "{} came twice", hit.title);
            }
            let Some(next) = page.next else { break };
            assert!(search.action_directory_cursor_is_current(&next));
            cursor = Some(next);
        }
        assert_eq!(seen.len(), 257, "every match is reachable");
    }

    phase no_caller_materialises_more_than_one_page_at_once;
    {
        let search = ResourceSearch::new(
            "Tables",
            "so.tilo.test",
            Arc::new(IndexedEntries::built(
                (0..300)
                    .map(|n| SearchEntry::new(format!("table {n}"), ""))
                    .collect(),
                0,
            )),
        );
        assert_eq!(
            search.search_limited("", usize::MAX).len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
        assert_eq!(
            search
                .search_page(&SearchQuery::new(""), None, usize::MAX, usize::MAX)
                .results
                .len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
    }

    phase a_match_in_a_body_is_the_place_it_is_in;
    {
        let body = tilo_resource::SearchBody::new(Arc::new(Chapter(vec![
            "It was a dark and stormy night.",
            "The lighthouse keeper counted the waves.",
        ])));
        let search = ResourceSearch::new(
            "Library",
            "so.tilo.test",
            Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Storm Tales", "")
                        .invoking(tilo_actions::ActionAddress::new("book:1", "open"))
                        .grouped("Books")
                        .with_body(body.clone()),
                ],
                0,
            )),
        );
        let hits = search.search("lighthouse keeper");
        let place = hits
            .iter()
            .find(|hit| hit.title == "Paragraph 2")
            .expect("the paragraph the words are in is a hit");
        assert_eq!(
            place.action_address(),
            Some(tilo_actions::ActionAddress::new("book.location:1", "open"))
        );
        assert_eq!(place.kind, "Books", "a location is listed with its book");
        assert!(
            search
                .search("storm tales")
                .iter()
                .any(|hit| hit.title == "Storm Tales"),
            "the book itself is found by its title"
        );
        assert_eq!(body, body.clone(), "a body is the text it shares");
    }

    phase a_field_ranks_by_its_own_weight;
    {
        use tilo_resource::SearchWeight;
        let search = ResourceSearch::new(
            "Mail",
            "so.tilo.test",
            Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Lunch plans", "")
                        .field("ada@example.org", SearchWeight::Metadata),
                    SearchEntry::new("Quarterly report", "")
                        .field("ada@example.org", SearchWeight::Identity),
                ],
                0,
            )),
        );
        let hits = search.search("ada@example.org");
        assert_eq!(hits.len(), 2, "both are found by the address");
        assert_eq!(
            hits[0].title, "Quarterly report",
            "a match in an identity outranks one in metadata"
        );
    }
}

/// A chapter of a book: one paragraph per segment, each opening the
/// book where it is.
struct Chapter(Vec<&'static str>);

impl tilo_resource::SearchText for Chapter {
    fn segment_count(&self) -> usize {
        self.0.len()
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.0.len())
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<tilo_resource::SearchPiece<'_>> {
        let text = self.0.get(segment)?;
        (piece == 0).then_some(tilo_resource::SearchPiece {
            text,
            space_before: segment > 0,
        })
    }

    fn locates(&self, _segment: usize) -> bool {
        true
    }

    fn location(&self, segment: usize) -> Option<SearchEntry> {
        Some(
            SearchEntry::new(format!("Paragraph {}", segment.saturating_add(1)), "").invoking(
                tilo_actions::ActionAddress::new(format!("book.location:{segment}"), "open"),
            ),
        )
    }
}
