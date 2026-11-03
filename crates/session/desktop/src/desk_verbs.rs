//! What the desktop publishes: one verb vocabulary per resource, the desk's
//! and the chrome's, and the rows under the desk — its windows, its
//! displays, the open search and the empty desk's launchpad — each offering
//! its own verb of the same vocabulary, bound to the row it stands for.
//!
//! A person, the palette, an agent or a test reaches each the same way, by
//! its address: the verbs are the desk's ([`DeskEffect`]) and the chrome's
//! ([`ChromeEffect`]), published through [`offer`] as every service's are.

use tilo_actions::{ActionSpec, ResourcePresentation, ResourceSpec};
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect};
use tilo_resource::{Effect, offer};

use crate::i18n::{t, t_args};

/// The resource of the chrome above the windows.
pub use tilo_desktop_search::CHROME_RESOURCE;
/// The resource of the desk under the windows.
pub const DESK_RESOURCE: &str = "desktop:desk";

/// The resource of the empty desk's launchpad.
pub const LAUNCHPAD_RESOURCE: &str = "desktop:launchpad";

/// The resource of the open search, a child of the desk's.
pub const SEARCH_RESOURCE: &str = "desktop:search";
/// The kind of the open search's resource, and of each of its rows.
pub const SEARCH_KIND: &str = "desktop.search";
pub const SEARCH_ROW_KIND: &str = "desktop.search-row";

/// The kind of a window's resource.
pub const WINDOW_KIND: &str = "desktop.window";

/// The kind of a display's resource.
pub const DISPLAY_KIND: &str = "desktop.display";

/// The address of the open search's row `row`.
#[must_use]
pub fn search_row_id(row: usize) -> String {
    format!("{SEARCH_RESOURCE}:row:{row}")
}

/// The address of tab `tab`'s resource.
#[must_use]
pub fn tab_resource_id(tab: u64) -> String {
    format!("desktop:tab:{tab}")
}

/// The address of display `display`'s resource.
#[must_use]
pub fn display_resource_id(display: u32) -> String {
    format!("desktop:display:{display}")
}

/// The kind of a card of the empty desk's launchpad.
pub const LAUNCHPAD_CARD_KIND: &str = "desktop.launchpad-card";

/// The address of the launchpad's card `slug`.
#[must_use]
pub fn launchpad_card_id(slug: &str) -> String {
    format!("{LAUNCHPAD_RESOURCE}:{slug}")
}

/// The chrome's resource: the verbs of everything over the desk, published
/// as they are declared.
#[must_use]
pub fn chrome_resource() -> ResourceSpec {
    offer::<ChromeEffect>(ResourceSpec::new(
        CHROME_RESOURCE,
        "desktop",
        t("desktop-actions-chrome"),
    ))
}

/// The open search as the desk publishes it: what is typed, the rows it
/// shows, and whether it is still filling in.
#[derive(Clone, Copy)]
pub struct OpenSearch<'a> {
    /// What is typed, or empty for the empty field.
    pub query: &'a str,
    /// The rows it shows, in the order the person sees them.
    pub rows: &'a [tilo_desktop_surface::SearchRow],
    /// Whether a source has still to answer for the query, or the rows the
    /// card shows are not cut to its room yet: the search says it is still
    /// searching, and its rows will change.
    pub searching: bool,
}

/// The desk's resource: its own verbs, with each tab of `tabs`
/// (`(tab, title)`) a resource of its own that can be focused and closed,
/// across several displays each of `displays` (in reading order) one that
/// can be focused, and the open search (its query and rows), when open.
#[must_use]
pub fn desk_resource(
    tabs: &[(u64, String)],
    displays: &[u32],
    search: Option<OpenSearch<'_>>,
) -> ResourceSpec {
    let screens = displays.iter().enumerate().map(|(index, display)| {
        let number = index.saturating_add(1).to_string();
        let focus = bound(
            &DeskEffect::FocusDisplay {
                display: display.to_string(),
            },
            None,
        );
        ResourceSpec::new(
            display_resource_id(*display),
            DISPLAY_KIND,
            crate::i18n::t_args(
                "desktop-display-name",
                &[("number", number.as_str().into())],
            ),
        )
        .with_action(focus)
    });
    // Each window's verbs name the window they act on: an action row that
    // says "Focus a window" makes the person find the window again in the
    // next row.
    let windows = tabs.iter().map(|(id, title)| {
        let names_the_window = &[("window", title.as_str().into())];
        ResourceSpec::new(tab_resource_id(*id), WINDOW_KIND, title.clone())
            .with_action(bound(
                &DeskEffect::Focus {
                    tab: id.to_string(),
                },
                Some(t_args("desk-focus-window", names_the_window)),
            ))
            .with_action(bound(
                &DeskEffect::CloseTab {
                    tab: id.to_string(),
                },
                Some(t_args("desk-close-window", names_the_window)),
            ))
    });
    let search = search.map(search_resource);
    offer::<DeskEffect>(ResourceSpec::new(
        DESK_RESOURCE,
        "desktop",
        t("desktop-actions-desk"),
    ))
    .with_children(windows.chain(screens).chain(search))
}

/// The launchpad's resource: each card a resource of its own that offers
/// the desk's launchpad verb, bound to the card it stands for.
#[must_use]
pub fn launchpad_resource() -> ResourceSpec {
    let cards = crate::launchpad::LaunchpadAction::ALL
        .into_iter()
        .map(|card| (card.slug(), card.label()))
        .chain([("apps", t("launchpad-open-app"))]);
    let rows = cards.map(|(slug, label)| {
        ResourceSpec::new(launchpad_card_id(slug), LAUNCHPAD_CARD_KIND, label.clone()).with_action(
            bound(&DeskEffect::Launchpad { card: slug.into() }, Some(label)),
        )
    });
    ResourceSpec::new(
        LAUNCHPAD_RESOURCE,
        "desktop",
        t("desktop-actions-launchpad"),
    )
    .with_children(rows)
}

/// The open search: what is typed, and each row it shows, which can be
/// chosen. Its detail says it is still searching until every source has
/// answered and its rows are cut to the card: a reader of the desk (a
/// script, an agent, assistive technology) knows the rows are final once
/// it is gone.
fn search_resource(search: OpenSearch<'_>) -> ResourceSpec {
    let OpenSearch {
        query,
        rows,
        searching,
    } = search;
    let rows = rows.iter().enumerate().map(|(at, row)| {
        let index = u64::try_from(at).unwrap_or(u64::MAX);
        let choose = bound_asking(
            &DeskEffect::ChooseSearchRow {
                row: index,
                argument: None,
            },
            &["argument"],
            Some(row.title.clone()),
        );
        let spec = ResourceSpec::new(search_row_id(at), SEARCH_ROW_KIND, row.title.clone())
            .with_action(choose);
        // What the row is sits under its name, as the row's second line.
        match &row.subtitle {
            Some(subtitle) => spec.with_presentation(ResourcePresentation {
                detail: Some(subtitle.clone()),
                ..ResourcePresentation::default()
            }),
            None => spec,
        }
    });
    let label = if query.is_empty() {
        t("desktop-actions-search")
    } else {
        query.to_owned()
    };
    let typed = bound_asking(&DeskEffect::Search { query: None }, &["query"], None);
    let resource = ResourceSpec::new(SEARCH_RESOURCE, SEARCH_KIND, label)
        .with_action(typed)
        .with_children(rows);
    if searching {
        return resource.with_presentation(ResourcePresentation {
            detail: Some(t("desktop-search-searching")),
            ..ResourcePresentation::default()
        });
    }
    resource
}

/// `verb` as the action its row offers, named `label` when one is given.
fn bound(verb: &DeskEffect, label: Option<String>) -> ActionSpec {
    let mut spec = verb.bound();
    if let Some(label) = label {
        spec.type_mut().label = label;
    }
    spec
}

/// `verb` as the action its row offers, still asking its caller for the
/// arguments named in `asking`, named `label` when one is given.
fn bound_asking(verb: &DeskEffect, asking: &[&str], label: Option<String>) -> ActionSpec {
    let mut spec = verb.bound_asking(asking);
    if let Some(label) = label {
        spec.type_mut().label = label;
    }
    spec
}
