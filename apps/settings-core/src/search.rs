//! Settings panes as search entries.
//!
//! Every [`Category`] pane is findable from the unified Ctrl+K palette by its
//! title, summary, or any of its built-in keyword synonyms ("vpn", "night
//! light", "wallpaper", …). Each entry is anchored at its pane's row and
//! opens that pane, as an agent's `open` of the row and the sidebar do.
//!
//! This indexes **panes** (navigation), not individual toggles — those are
//! already exported to Ctrl+K as the app's effects. The one exception is
//! the types of file and link Apps & files lists: "open .rs with" is
//! found and lands on that type's row ([`type_entries`]).

use ui::tilo_resource::SearchEntry;

use crate::categories::Category;

pub const SETTINGS_KIND: &str = "Settings";

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
        .chain(type_entries())
        .collect()
}

/// Every type of file and link the taxonomy names, as a search entry:
/// "Open .rs files with", in its category, anchored at the type's row on
/// Apps & files ([`crate::effects::default_app_row_id`]).
fn type_entries() -> Vec<SearchEntry> {
    let words = crate::i18n::t("settings-default-apps-search-words");
    desktop_policy::TypeCategory::ALL
        .iter()
        .flat_map(|&category| {
            let entry = category.entry();
            let name = crate::i18n::type_category(category);
            let files = entry.extensions.iter().map(move |ext| {
                (
                    format!("ext:{ext}"),
                    crate::i18n::t_args(
                        "settings-default-apps-search-ext",
                        &[("ext", ui::i18n::ArgValue::Text((*ext).to_owned()))],
                    ),
                )
            });
            let links = entry.schemes.iter().map(move |scheme| {
                (
                    format!("scheme:{scheme}"),
                    crate::i18n::t_args(
                        "settings-default-apps-search-scheme",
                        &[("scheme", ui::i18n::ArgValue::Text((*scheme).to_owned()))],
                    ),
                )
            });
            let words = words.clone();
            files.chain(links).map(move |(kind, title)| {
                SearchEntry::new(title, format!("{words} {name}"))
                    .anchored(crate::effects::default_app_row_id(&kind))
                    .grouped(SETTINGS_KIND)
                    .subtitled(name.clone())
                    .with_icon(category.icon())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::{SearchProvider, SearchRegistry};

    /// Whether a hit opens a pane, rather than a type of file's row.
    fn is_pane(action: &str) -> bool {
        crate::effects::pane_of_row(action).is_some()
    }

    /// "open .rs with" finds the `.rs` row of Apps & files, and every
    /// type hit is anchored at a type's row.
    #[test]
    fn a_type_of_file_is_found_by_what_people_type() {
        ui::i18n::I18nManager::init("en-US");
        let provider = source();
        let hit = provider
            .search("open .rs with")
            .into_iter()
            .find(|hit| hit.action_id == crate::effects::default_app_row_id("ext:rs"))
            .expect("the .rs type is found");
        assert_eq!(hit.title, "Open .rs files with");
        assert_eq!(hit.anchor(), crate::effects::default_app_row_id("ext:rs"));
        let https = provider
            .search("https links")
            .into_iter()
            .find(|hit| hit.action_id == crate::effects::default_app_row_id("scheme:https"));
        assert!(https.is_some(), "a link's scheme is found too");
        for hit in provider.search("") {
            assert!(
                is_pane(&hit.action_id)
                    || hit
                        .action_id
                        .strip_prefix(crate::effects::DEFAULT_APP_ANCHOR)
                        .is_some_and(|kind| kind.starts_with("ext:") || kind.starts_with("scheme:")),
                "{} names neither a pane nor a type",
                hit.action_id
            );
        }
    }

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
        assert_eq!(
            provider
                .search("")
                .iter()
                .filter(|hit| is_pane(&hit.action_id))
                .count(),
            Category::ALL.len()
        );

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
            titles.extend(
                page.results
                    .into_iter()
                    .filter(|result| is_pane(&result.action_id))
                    .map(|result| result.title),
            );
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
        let hits: Vec<_> = source()
            .search("")
            .into_iter()
            .filter(|hit| is_pane(&hit.action_id))
            .collect();
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
