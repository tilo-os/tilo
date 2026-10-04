//! Settings panes as search entries.
//!
//! Every [`Category`] pane is findable from the unified Ctrl+K palette by its
//! title, summary, or any of its built-in keyword synonyms ("vpn", "night
//! light", "wallpaper", …). Each entry invokes its pane's row's open
//! effect, so search, agents, accessibility and the sidebar all
//! open a pane the same way.
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
            SearchEntry::new(category.label(), category.keywords().join(" "))
                .invoking(crate::effects::open_pane_address(category))
                .grouped(SETTINGS_KIND)
                .subtitled(category.subtitle())
                .with_icon(category.icon().name())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
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
            hits.iter().any(|r| r.action_address()
                == Some(crate::effects::open_pane_address(Category::Displays))),
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
            let page = provider.search_page("", cursor.as_ref(), 1, 1);
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
    fn result_addresses_the_exported_open_action() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(source()));
        let hit = registry
            .query("appearance")
            .into_iter()
            .find(|r| {
                r.action_address() == Some(crate::effects::open_pane_address(Category::Appearance))
            })
            .expect("Appearance is findable");
        assert_eq!(
            hit.action_address(),
            Some(crate::effects::open_pane_address(Category::Appearance))
        );
    }

    /// Every search address names a pane's row, which offers exactly that
    /// view; a removed pane or action names nothing.
    #[test]
    fn every_search_address_names_its_panes_row() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let hits = source().search("");
        assert_eq!(hits.len(), Category::ALL.len());
        for (category, hit) in Category::ALL.iter().copied().zip(hits) {
            let address = hit.action_address().expect("canonical action address");
            assert_eq!(address, crate::effects::open_pane_address(category));
            let row = crate::effects::row(&app, &address.resource)
                .expect("the search address names a row")
                .into_resource();
            let rows = [row];
            let spec = tilo_actions::door::resolve(&rows, &address)
                .expect("the row offers the search address");
            assert_eq!(spec.kind, tilo_actions::OperationKind::View);
        }

        assert!(crate::effects::row(&app, "settings.section:removed-pane").is_none());
        let stale_action = ActionAddress::new(
            crate::effects::pane_row_id(Category::Accessibility),
            "removed-action",
        );
        let current = crate::effects::row(&app, &stale_action.resource)
            .expect("the current row remains")
            .into_resource();
        let rows = [current];
        assert!(tilo_actions::door::resolve(&rows, &stale_action).is_none());
    }
}
