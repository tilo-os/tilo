// Palette search: a query's work is budgeted per frame, its hits are grouped,
// and exact matches are reached however many candidates come before them.

#[test]
fn palette_search_is_budgeted_grouped_and_reaches_exact_matches() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    {
        let materialized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut palette =
            ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
        palette.load_open_resources(&[]);
        palette.set_providers(vec![Box::new(LazyCorpus {
            materialized: Arc::clone(&materialized),
        })]);
        materialized.store(0, std::sync::atomic::Ordering::Relaxed);
        palette.open();
        assert_eq!(
            materialized.load(std::sync::atomic::Ordering::Relaxed),
            PROVIDER_HIT_BUDGET
        );
        assert_eq!(palette.len(), PER_GROUP_CAP);
        palette.activate_selected();
        assert!(
            palette.take_operation().is_none(),
            "provider discovery must not become dispatch authority"
        );
    }

    // Case `actions_and_content_both_surface_grouped`:
    // Both an ACTION (from the resource tree) and a searchable CONTENT hit
    // (from a folded-in provider) surface, grouped, for one query.
    {
        let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
            SearchField::with_language("en_US")
        });
        p.load_open_resources(&[thing_resource()]);
        p.set_providers(vec![Box::new(People)]);
        p.open();

        let groups: Vec<String> = p.results.groups().iter().map(|g| g.kind.clone()).collect();
        assert!(
            groups.contains(&ACTIONS_KIND.to_string()),
            "Actions group present: {groups:?}"
        );
        assert!(
            groups.contains(&"People".to_string()),
            "content group present: {groups:?}"
        );

        let labels = p.result_labels();
        assert!(labels.iter().any(|l| l.contains("Mark done")), "{labels:?}");
        assert!(labels.iter().any(|l| l == "Ada Lovelace"), "{labels:?}");
    }
}

#[test]
fn progressive_palette_reaches_exact_match_beyond_one_hundred_thousand_candidates() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let catalog = crate::search::SearchCatalog::new("Adversarial corpus");
    for id in 0..100_000usize {
        let resource_id = format!("item:{id:06}");
        catalog
            .upsert(
                &resource_id,
                "abc c d",
                SearchResult::for_action(
                    "Items",
                    format!("Broad candidate {id}"),
                    tilo_actions::ActionAddress::new(&resource_id, "open"),
                ),
            )
            .expect("valid broad candidate");
    }
    let target_id = "item:999999";
    catalog
        .upsert(
            target_id,
            "abc d",
            SearchResult::for_action(
                "Items",
                "The only exact abc d result",
                tilo_actions::ActionAddress::new(target_id, "open"),
            ),
        )
        .expect("valid exact target");

    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.set_providers(vec![Box::new(catalog)]);
    // The whitespace-bearing query intentionally cannot take the token
    // fast path. Every false candidate shares its n-grams, so this proves
    // progressive exact verification continues across work pages.
    palette.set_query("abc d");
    let generation = palette.search_generation;
    let mut ticks = 0usize;
    while !palette
        .result_labels()
        .iter()
        .any(|label| label == "The only exact abc d result")
    {
        let progress = palette.advance_search_page();
        assert!(progress.inspected <= PROVIDER_HIT_BUDGET);
        assert_eq!(progress.generation, generation);
        ticks = ticks.saturating_add(1);
        assert!(progress.pending || ticks <= 3_126, "target was skipped");
        assert!(ticks <= 3_126, "search did not finish in bounded pages");
    }
    assert!(
        palette
            .referenced_action_resource_ids()
            .iter()
            .any(|resource_id| resource_id == target_id),
        "the exact result must retain its canonical resource action"
    );
    assert!(
        palette
            .take_search_announcement()
            .is_some_and(|announcement| announcement.contains("result"))
    );

    palette.set_query("a different query");
    assert_ne!(palette.search_generation, generation);
    assert!(
        !palette
            .result_labels()
            .iter()
            .any(|label| label == "The only exact abc d result")
    );
    let restarted = palette.advance_search_page();
    assert_eq!(restarted.generation, palette.search_generation);
    assert!(restarted.inspected <= PROVIDER_HIT_BUDGET);
}
