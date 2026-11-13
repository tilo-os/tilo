//! The settings sidebar: one [`NavList`] of the categories, grouped by.
//! [`Section`].

use ui::widget::{NavItem, NavList, NavSection};

use crate::categories::{Category, Section};

pub const NAV_ID: &str = "settings.nav";
pub const SIDEBAR_WIDTH: f64 = ui::tokens::Metrics::BASE.sidebar_width as f64;
pub fn width_for_text_scale(scale: f32) -> f64 {
    SIDEBAR_WIDTH * f64::from(scale.clamp(0.5, 4.0))
}

/// The sectioned category list, choosing what `selection` holds and
/// narrowed to what `filter` holds; both follow the list, so the window bar's
/// search writes what it asks for to `filter`. Choosing a row runs its
/// pane's open effect through `actions`.
pub fn build(
    actions: &ui::framework::Actions<crate::resource::Settings>,
    selection: &ui::LocalState<Option<String>>,
    filter: &ui::LocalState<String>,
) -> NavList {
    let bindings = Category::ALL
        .iter()
        .filter_map(|category| {
            actions
                .effect(&crate::effects::SettingsEffect::from(
                    crate::effects::NavigationEffect::OpenPane {
                        pane: category.slug().to_owned(),
                    },
                ))
                .map(|binding| (category.slug().to_owned(), binding))
        })
        .collect::<Vec<_>>();
    list(selection, filter).selection_actions(bindings)
}

pub fn list(
    selection: &ui::LocalState<Option<String>>,
    filter: &ui::LocalState<String>,
) -> NavList {
    NavList::new(sections())
        .with_id(NAV_ID)
        .with_accessible_name(crate::i18n::t("settings-nav-name"))
        .bind_selection(selection)
        .bind_filter(filter)
}

/// The sections of the list, in render order: only those this build has a
/// page in, so the web's Settings shows no heading over nothing.
pub fn sections() -> Vec<NavSection> {
    Section::ALL
        .iter()
        .copied()
        .filter(|section| Category::in_section(*section).next().is_some())
        .map(|section| {
            let items = Category::in_section(section)
                .map(|category| {
                    NavItem::new(category.slug(), category.label())
                        .icon(category.icon())
                        .search_terms(category.search_terms())
                })
                .collect();
            NavSection::new(section.slug(), section.label(), items).filterable()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_holds_every_category_once_under_its_section() {
        let sections = sections();
        let shown: Vec<Section> = Section::ALL
            .iter()
            .copied()
            .filter(|section| Category::in_section(*section).next().is_some())
            .collect();
        assert_eq!(sections.len(), shown.len());
        assert!(
            sections.iter().all(|section| !section.items.is_empty()),
            "no heading stands over nothing"
        );
        let mut seen = std::collections::HashSet::new();
        for (section, rendered) in shown.iter().zip(&sections) {
            for category in Category::in_section(*section) {
                assert!(seen.insert(category.slug()), "{category:?} listed twice");
                assert_eq!(category.section(), *section);
            }
            assert!(!rendered.title.is_empty(), "{section:?} has a header");
        }
        assert_eq!(seen.len(), Category::ALL.len());
    }

    #[test]
    fn section_headers_are_sentence_case() {
        for section in Section::ALL {
            let label = section.label();
            assert_ne!(label, label.to_uppercase(), "{label} is all caps");
        }
    }

    #[test]
    fn every_category_has_its_own_icon() {
        let mut icons = std::collections::HashSet::new();
        for category in Category::ALL {
            assert!(
                icons.insert(category.icon()),
                "{category:?} shares its icon with another page"
            );
        }
    }

    #[test]
    fn the_filter_finds_pages_by_the_settings_on_them() {
        for (query, page) in [
            ("layout", Category::Keyboard),
            ("qwertz", Category::Keyboard),
            ("wifi", Category::Network),
            ("battery", Category::Power),
            ("dark mode", Category::Appearance),
            ("tap to click", Category::Pointing),
            ("disable touchpad while typing", Category::Pointing),
        ] {
            let shown = list(
                &ui::LocalState::new(Some(Category::Network.slug().to_owned())),
                &ui::LocalState::new(query.to_owned()),
            )
            .visible_item_ids();
            assert!(
                shown.iter().any(|id| id == page.slug()),
                "{query:?} shows {page:?}, got {shown:?}"
            );
        }
    }
}
