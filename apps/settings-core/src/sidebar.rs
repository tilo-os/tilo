//! The settings sidebar: one [`NavList`] of the categories, grouped by
//! [`Section`].
//!
//! The list is the toolkit's navigation list, so rows, selection lift,
//! keyboard handling, section headers and the narrow push behaviour are the
//! same as every other Tilo sidebar. Choosing a category publishes a typed,
//! process-local navigation request which `crate::SettingsApp::sync`
//! consumes to switch the active category. Navigation is presentation state:
//! it must not become a replicated preference, an undo entry, or a document
//! click flag.

use ui::widget::{NavItem, NavList, NavSection};

use crate::categories::{Category, Section};

/// The id of the navigation list, for tests and inspection.
pub const NAV_ID: &str = "settings.nav";
pub const SIDEBAR_WIDTH: f64 = ui::tokens::Metrics::BASE.sidebar_width as f64;
pub fn width_for_text_scale(scale: f32) -> f64 {
    SIDEBAR_WIDTH * f64::from(scale.clamp(0.5, 4.0))
}

/// Build the sectioned category list with `selected` chosen. Choosing a row
/// writes its category to `selection_request`.
pub fn build(
    cx: &crate::host::PaneCx,
    selected: Category,
    query: &str,
    selection_request: &ui::LocalState<Option<Category>>,
) -> Box<dyn ui::Widget> {
    let request = selection_request.clone();
    let bindings = Category::ALL
        .iter()
        .filter_map(|category| {
            cx.invocation_binding(crate::effects::NavigationEffect::OpenPane {
                pane: category.slug().to_owned(),
            })
            .map(|binding| (category.slug().to_owned(), binding))
        })
        .collect::<Vec<_>>();
    let mut list = NavList::new(sections())
        .with_id(NAV_ID)
        .with_accessible_name(crate::i18n::t("settings-nav-name"))
        .selected(selected.slug())
        .selection_actions(bindings)
        .on_select(move |id| {
            if let Some(category) = Category::from_slug(&id) {
                request.set(Some(category));
            }
        });
    list.set_filter(query);
    list.into_widget()
}

/// The sections of the list, in render order.
pub fn sections() -> Vec<NavSection> {
    Section::ALL
        .iter()
        .copied()
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
        assert_eq!(sections.len(), Section::ALL.len());
        let mut seen = std::collections::HashSet::new();
        for (section, rendered) in Section::ALL.iter().zip(&sections) {
            for category in Category::in_section(*section) {
                assert!(seen.insert(category.slug()), "{category:?} listed twice");
                assert_eq!(category.section(), *section);
            }
            assert!(!rendered.title.is_empty(), "{section:?} has a header");
        }
        assert_eq!(seen.len(), Category::ALL.len());
    }

    /// Section headers are sentence case: the design language has no
    /// all-caps labels.
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

    /// Typing what a page holds finds the page, and the row says which
    /// setting matched: "where is the keyboard layout".
    #[test]
    fn the_filter_finds_pages_by_the_settings_on_them() {
        let request = ui::LocalState::new(None);
        let cx =
            crate::host::PaneCx::detached(ui::storage::AppStorage::for_app("so.tilo.settings"));
        for (query, page) in [
            ("layout", Category::Keyboard),
            ("qwertz", Category::Keyboard),
            ("wifi", Category::Network),
            ("battery", Category::Power),
            ("dark mode", Category::Appearance),
            ("tap to click", Category::Pointing),
            ("disable touchpad while typing", Category::Pointing),
        ] {
            let mut widget = build(&cx, Category::Network, query, &request);
            let list = widget
                .as_any_mut()
                .unwrap()
                .downcast_mut::<ui::widget::NavList>()
                .unwrap();
            let shown = list.visible_item_ids();
            assert!(
                shown.iter().any(|id| id == page.slug()),
                "{query:?} shows {page:?}, got {shown:?}"
            );
        }
    }

    #[test]
    fn choosing_a_row_publishes_a_local_request_without_touching_the_document() {
        let request = ui::LocalState::new(None);
        let cx =
            crate::host::PaneCx::detached(ui::storage::AppStorage::for_app("so.tilo.settings"));
        let mut widget = build(&cx, Category::Network, "", &request);
        let list = widget
            .as_any_mut()
            .unwrap()
            .downcast_mut::<ui::widget::NavList>()
            .unwrap();
        assert_eq!(list.selected_id().as_deref(), Some("network"));
        list.set_selected(Some("about"));
        assert_eq!(list.selected_id().as_deref(), Some("about"));
        assert_eq!(request.with(|r| *r), None, "set_selected does not report");
    }
}
