use super::*;

#[derive(Clone, Debug)]
struct Row(ResourceKind, i32, &'static str);
impl SearchRow for Row {
    fn kind(&self) -> &ResourceKind {
        &self.0
    }
    fn score(&self) -> i32 {
        self.1
    }
    fn title(&self) -> &str {
        self.2
    }
    fn subtitle(&self) -> Option<&str> {
        Some("detail")
    }
}
#[test]
fn scope_defaults_and_fallback_preserve_native_routing() {
    assert_eq!(SearchScope::default(), SearchScope::Workspace);
    assert_eq!(SearchScope::All.label(), "Everywhere");
    assert!(scope_allows(SearchScope::ActiveApp, "apps", None, false));
    assert!(!scope_allows(
        SearchScope::ActiveApp,
        "apps",
        Some("notes"),
        false
    ));
    assert!(scope_allows(
        SearchScope::ActiveApp,
        "actions",
        Some("notes"),
        true
    ));
}
#[test]
fn strongest_group_precedes_kind_order_and_ties_preserve_provider_order() {
    let rows = rank(vec![
        (Row(ResourceKind::Action, 25, "action"), 100),
        (Row(ResourceKind::File, 120, "file"), 120),
        (Row(ResourceKind::Application, 100, "app z"), 100),
        (Row(ResourceKind::Application, 100, "app a"), 100),
        (Row(ResourceKind::Application, 10, "weak app"), 100),
    ]);
    assert_eq!(
        rows.iter().map(|r| r.2).collect::<Vec<_>>(),
        ["app z", "app a", "weak app", "file", "action"]
    );
}
#[test]
fn fair_budget_keeps_totals_and_single_kind_is_exempt() {
    let mut rows = vec![Row(ResourceKind::Application, 100, "app")];
    rows.extend((0..20).map(|_| Row(ResourceKind::File, 50, "file")));
    rows.extend((0..10).map(|_| Row(ResourceKind::Note, 25, "note")));
    let groups = group(rows, 8);
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.results.len(), g.total))
            .collect::<Vec<_>>(),
        [(1, 1), (3, 20), (4, 10)]
    );
    let entries = command_entries(&groups);
    assert_eq!(entries.len(), 8);
    assert_eq!(entries[0].id, "result:0");
    assert_eq!(entries[0].group.as_deref(), Some("Applications"));
    assert_eq!(entries[0].icon, Some(crate::icons::Icon::LayoutGrid));
    assert_eq!(entries[1].group_total, Some(20));
    assert_eq!(entries[0].description.as_deref(), Some("detail"));
    assert_eq!(
        group(vec![Row(ResourceKind::File, 0, "file"); 20], 8)[0]
            .results
            .len(),
        20
    );
}
#[test]
fn application_identity_and_metadata_share_native_match_bands() {
    assert_eq!(
        application_match_score("hex-editor", "hex editor", "dev.qos.hex-editor", None, None),
        Some(100)
    );
    assert_eq!(
        application_match_score(
            "office spreadsheet",
            "sheets",
            "dev.qos.sheets",
            Some("office"),
            Some("spreadsheet editor")
        ),
        Some(15)
    );
    assert_eq!(
        application_match_score("clc", "calculator", "dev.qos.calculator", None, None),
        Some(10)
    );
    assert_eq!(
        application_match_score("missing", "calculator", "dev.qos.calculator", None, None),
        None
    );
}
#[test]
fn filters_targets_match_semantic_activation_after_draw() {
    use crate::search::system::filters::{FilterAction, SearchFilters};
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use vello::kurbo::{Affine, Rect};
    let mut filters = SearchFilters::default();
    filters.configure(
        vec![ResourceKind::Application, ResourceKind::File],
        vec![],
        vec![
            (SearchScope::Workspace, "Workspace".into()),
            (SearchScope::All, "Everywhere".into()),
        ],
        SearchScope::All,
    );
    let mut renderer = TextRenderer::new();
    let mut scene = vello::Scene::new();
    let rows = filters.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        &Theme::default(),
        Rect::new(0., 0., 680., 36.),
        36.,
        1,
        None,
    );
    assert!(rows >= 1);
    let target = filters.rect_for_path(&[0, 1]).unwrap();
    assert_eq!(
        filters.hit(target.center()),
        Some(FilterAction::ToggleKind(ResourceKind::Application))
    );
    assert_eq!(
        filters.action_for_path(&[1, 1]),
        Some(FilterAction::Scope(SearchScope::All))
    );
    let node = filters.accessibility();
    assert_eq!(node.children[0].children.len(), 3);
    assert_eq!(node.children[1].children.len(), 2);
}
struct ProviderFixture {
    id: &'static str,
    kind: ResourceKind,
    stale: bool,
}
impl Provider<(), Row> for ProviderFixture {
    fn id(&self) -> &str {
        self.id
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        std::slice::from_ref(&self.kind)
    }
    fn query_page(
        &self,
        _: &(),
        query: &Query<'_>,
        _: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage<Row> {
        assert_eq!(budget, SearchBudget::PALETTE);
        assert_eq!(query.wanted_kinds, std::slice::from_ref(&self.kind));
        if self.stale {
            SearchPage::stale("changed")
        } else {
            SearchPage::exhausted("v1", vec![Row(self.kind.clone(), 100, self.id)])
        }
    }
    fn resolve(&self, _: &(), _: &str, _: &Query<'_>) -> Option<Row> {
        None
    }
}
#[test]
fn typed_registry_routes_scope_and_kinds_and_discards_stale_pages() {
    let apps = ProviderFixture {
        id: "apps",
        kind: ResourceKind::Application,
        stale: false,
    };
    let files = ProviderFixture {
        id: "files",
        kind: ResourceKind::File,
        stale: false,
    };
    let stale = ProviderFixture {
        id: "stale",
        kind: ResourceKind::Note,
        stale: true,
    };
    let mut registry = Registry::new();
    registry.register(&apps);
    registry.register(&files);
    registry.register(&stale);
    let mut query = Query {
        needle: "x",
        scope: SearchScope::All,
        active_provider_id: Some("apps"),
        wanted_kinds: &[],
    };
    assert_eq!(
        registry
            .query(&(), &query)
            .iter()
            .map(|r| r.2)
            .collect::<Vec<_>>(),
        ["apps", "files"]
    );
    query.scope = SearchScope::ActiveApp;
    assert_eq!(registry.query(&(), &query).len(), 1);
    query.scope = SearchScope::All;
    query.wanted_kinds = &[ResourceKind::File];
    assert_eq!(registry.query(&(), &query)[0].2, "files");
    let oversized = "x".repeat(crate::search::MAX_SEARCH_QUERY_SCALARS + 1);
    query.needle = &oversized;
    assert!(registry.query(&(), &query).is_empty());
}

#[test]
fn the_chip_strip_places_every_chip_however_narrow_the_band() {
    use super::filters::lay_out_chip_strip;
    use vello::kurbo::Rect;
    let band = Rect::new(0.0, 0.0, 240.0, 36.0);
    let natural = [40.0, 44.0, 92.0, 50.0, 46.0, 60.0, 58.0];
    for rows_available in [1usize, 2, 3] {
        let strip = lay_out_chip_strip(
            &natural,
            band,
            band.x1 - 100.0,
            rows_available,
            36.0,
            24.0,
            8.0,
        );
        assert_eq!(
            strip.rects.len(),
            natural.len(),
            "every chip gets a rect at {rows_available} row(s)"
        );
        assert!(
            strip.rows_needed >= 1,
            "the strip always needs at least one row"
        );
        for (i, r) in strip.rects.iter().enumerate() {
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "chip {i} of {} is not clickable at {rows_available} \
                     row(s): {r:?}",
                natural.len()
            );
            assert!(
                r.x0 >= band.x0 - 0.001,
                "chip {i} starts left of the band: {r:?}"
            );
        }
        for i in 0..strip.rects.len() {
            for j in (i + 1)..strip.rects.len() {
                assert!(
                    strip.rects[i].intersect(strip.rects[j]).area() <= 0.0,
                    "chips {i} and {j} overlap at {rows_available} \
                         row(s): {:?} / {:?}",
                    strip.rects[i],
                    strip.rects[j]
                );
            }
        }
    }
    // And when there is room, nothing is squeezed: the chips keep
    // their natural widths.
    let wide = Rect::new(0.0, 0.0, 900.0, 36.0);
    let strip = lay_out_chip_strip(&natural, wide, wide.x1, 1, 36.0, 24.0, 8.0);
    assert_eq!(strip.rows_needed, 1);
    for (i, r) in strip.rects.iter().enumerate() {
        assert_eq!(
            r.width(),
            natural[i],
            "chip {i} was squeezed with room to spare"
        );
    }
}
