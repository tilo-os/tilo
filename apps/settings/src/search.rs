//! Settings panes as search entries.
//!
//! Every [`Category`] pane is findable from the unified Ctrl+K palette by its
//! title, summary, or any of its built-in keyword synonyms ("vpn", "night
//! light", "wallpaper", …). Each entry is anchored at its pane's row and
//! opens that pane, as an agent's `open` of the row and the sidebar do.
//!
//! This indexes **panes** (navigation), not individual toggles — those are
//! already exported to Ctrl+K as the app's effects.

use ui::tilo_resource::SearchEntry;

use crate::categories::Category;

pub const SETTINGS_KIND: &str = "Personal Settings";

/// Every Settings pane as a search entry: found by its name, its summary and
/// the words people use for it ("night light" finds Displays), each opening
/// its pane.
#[must_use]
pub fn pane_entries() -> Vec<SearchEntry> {
    Category::ALL
        .iter()
        .copied()
        .map(|category| {
            SearchEntry::new(category.label(), category.search_words().join(" "))
                .anchored(crate::effects::pane_row_id(category))
                .grouped(SETTINGS_KIND)
                .subtitled(category.subtitle())
                .with_icon(category.icon().name())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::{SearchProvider, SearchRegistry};

    /// The app's search source, as the framework builds it.
    fn source() -> impl SearchProvider {
        ui::framework::testing::search_source::<crate::resource::Settings>(pane_entries())
    }

    /// Keyword synonyms, browsing and budgeted paging over one provider.
    #[test]
    fn pane_search_finds_keywords_browses_and_pages_every_pane() {
        let provider = source();
        // "night light" is a Displays keyword synonym, not in the label.
        let hits = provider.search("night light");
        assert!(
            hits.iter()
                .any(|r| r.action_id == crate::effects::pane_row_id(Category::Displays)),
            "a keyword synonym surfaces its pane, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(hits.iter().all(|r| r.kind == SETTINGS_KIND));

        // The empty query browses every pane.
        assert_eq!(provider.search("").len(), Category::ALL.len());

        // Paging reaches the complete corpus with a hard per-page budget.
        let mut cursor = None;
        let mut titles = Vec::new();
        loop {
            let page =
                provider.search_page(&ui::search::SearchQuery::new(""), cursor.as_ref(), 1, 1);
            assert!(page.inspected <= 1, "a page must respect its work budget");
            assert!(
                page.results.len() <= 1,
                "a page must respect its result limit"
            );
            titles.extend(page.results.into_iter().map(|result| result.title));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            titles,
            Category::ALL
                .iter()
                .map(|category| category.label())
                .collect::<Vec<_>>(),
            "a continuation reaches late categories instead of truncating browse discovery"
        );
    }

    #[test]
    fn a_hit_is_anchored_at_its_panes_row() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(source()));
        let hit = registry
            .query("appearance")
            .into_iter()
            .find(|r| r.action_id == crate::effects::pane_row_id(Category::Appearance))
            .expect("Appearance is findable");
        assert_eq!(
            hit.anchor(),
            crate::effects::pane_row_id(Category::Appearance)
        );
    }

    /// Every hit's anchor names a pane's row, which opens exactly that pane;
    /// a removed pane names nothing.
    #[test]
    fn every_search_anchor_names_its_panes_row() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let hits = source().search("");
        assert_eq!(hits.len(), Category::ALL.len());
        for (category, hit) in Category::ALL.iter().copied().zip(hits) {
            assert_eq!(hit.action_id, crate::effects::pane_row_id(category));
            assert_eq!(crate::effects::pane_of_row(&hit.action_id), Some(category));
            let row = crate::effects::row(&app, &hit.action_id)
                .expect("the anchor names a row")
                .into_resource();
            let open = row
                .actions
                .iter()
                .find(|spec| spec.id == "open-pane")
                .expect("the row opens its pane");
            assert_eq!(open.kind, tilo_actions::OperationKind::View);
        }
        assert!(crate::effects::row(&app, "settings.section:removed-pane").is_none());
        assert!(crate::effects::pane_of_row("settings.section:removed-pane").is_none());
    }

    /// A hit opens its pane in the mounted `ResourceApp`, including the
    /// narrow navigation stack, as picking it in search does.
    #[test]
    fn every_pane_search_hit_opens_its_mounted_live_surface() {
        let provider = source();
        for width in [1200, 440] {
            let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
                .expect("Settings mounts in its production resource host");
            app.harness()
                .resize((width, 900))
                .expect("Settings lays out at this window width");
            for category in Category::ALL.iter().copied() {
                let anchor = crate::effects::pane_row_id(category);
                let hit = provider
                    .search(&category.label())
                    .into_iter()
                    .find(|hit| hit.action_id == anchor)
                    .expect("the pane is found by its displayed name");
                app.call("open", serde_json::json!({ "anchor": hit.anchor() }))
                    .expect("the mounted app opens the hit's anchor");
                app.harness().relayout();
                assert_eq!(
                    app.surface::<crate::SettingsApp, _>(|surface| surface.selected()),
                    Some(category),
                    "search opens {category:?} at width {width}"
                );
                if width == 440 {
                    assert_eq!(
                        app.surface::<crate::SettingsApp, _>(|surface| surface.sidebar_open()),
                        Some(false),
                        "the narrow search result opens content rather than the category list"
                    );
                }
            }
        }
    }
}
