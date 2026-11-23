//! What the desktop publishes: one verb vocabulary per resource, the shell's and the chrome's, and
//! the rows under the shell — its windows, its displays, the open search and the empty shell's
//! launchpad — each an item of a declared type offering its own verb of the same vocabulary, bound
//! to what it stands for. A person, the palette, an agent or a test reaches each the same way, by
//! its address: the verbs are the shell's ([`LayoutEffect`]) and the chrome's ([`ChromeEffect`]),
//! published through [`offer`] as every service's are.

use tilo_actions::{ResourcePresentation, ResourceSpec};
use tilo_resource::words::RowType;
use tilo_resource::{LiveRow, RowBindings, offer, typed_resource};
use tilo_session_core::desktop::navigation::{
    Chrome, ChromeEffect, Display, Launchpad, LaunchpadCard, LayoutEffect, Search, SearchRow,
    Shell, Window,
};
use tilo_session_core::desktop::window;
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};

use crate::i18n::{t, t_args};

/// The resource of the chrome above the windows.
pub use tilo_desktop_search::CHROME_RESOURCE;
/// The resource of the shell under the windows.
pub const SHELL_RESOURCE: &str = "desktop:shell";

/// The resource of the empty shell's launchpad.
pub const LAUNCHPAD_RESOURCE: &str = "desktop:launchpad";

/// The resource of the open search, a child of the shell's.
pub const SEARCH_RESOURCE: &str = "desktop:search";
/// The kind of the open search's resource, and of each of its rows.
pub const SEARCH_KIND: &str = <Search as RowType>::KIND;
pub const SEARCH_ROW_KIND: &str = <SearchRow as RowType>::KIND;

/// The kind of a window's resource.
pub const WINDOW_KIND: &str = <Window as RowType>::KIND;

/// The kind of a display's resource.
pub const DISPLAY_KIND: &str = <Display as RowType>::KIND;

/// The address of the open search's row `row` .
#[must_use]
pub fn search_row_id(row: usize) -> String {
    format!("{SEARCH_RESOURCE}:row:{row}")
}

/// The address of tab `tab` 's resource.
#[must_use]
pub fn tab_resource_id(tab: u64) -> String {
    window::resource(tab)
}

/// The address of display `display` 's resource.
#[must_use]
pub fn display_resource_id(display: u32) -> String {
    format!("desktop:display:{display}")
}

/// The kind of a card of the empty shell's launchpad.
pub const LAUNCHPAD_CARD_KIND: &str = <LaunchpadCard as RowType>::KIND;

/// The address of the launchpad's card `slug` .
#[must_use]
pub fn launchpad_card_id(slug: &str) -> String {
    format!("{LAUNCHPAD_RESOURCE}:{slug}")
}

/// The chord each verb answers to, where one shortcut of the table reaches
/// it whole: the row reads the keys the person's table binds now, so a
/// rebinding reads there too. A verb of two directions (stepping a strip,
/// switching a workspace, a width) takes the direction from its arguments
/// and shows none; the shortcut guide lists the chord of every direction and
/// of every other action.
const VERB_SHORTCUTS: &[(&str, ShortcutAction)] = &[
    ("reopen-closed", ShortcutAction::ReopenClosedTab),
    ("maximize", ShortcutAction::ToggleMaximize),
    ("fullscreen", ShortcutAction::ToggleFullscreen),
    ("split-right", ShortcutAction::DetachTab),
];

/// `resource` with the chord of each of its actions that a shortcut reaches shown beside it, as the
/// person's table binds it now.
fn with_shortcuts(mut resource: ResourceSpec, shortcuts: &ShortcutConfig) -> ResourceSpec {
    for action in &mut resource.actions {
        let bound = VERB_SHORTCUTS
            .iter()
            .find(|(verb, _)| *verb == action.id)
            .and_then(|(_, shortcut)| shortcuts.bindings.get(shortcut));
        if let Some(chord) = bound {
            action.type_mut().state.shortcut = Some(chord.display());
        }
    }
    resource
}

/// The chrome's resource: the verbs of everything over the shell, published as they are declared,
/// each showing the chord that runs it.
#[must_use]
pub fn chrome_resource(shortcuts: &ShortcutConfig) -> ResourceSpec {
    with_shortcuts(
        offer::<ChromeEffect>(typed_resource::<Chrome>(
            CHROME_RESOURCE,
            t("desktop-actions-chrome"),
        )),
        shortcuts,
    )
}

/// The open search as the shell publishes it: what is typed, the rows it shows, and whether it is
/// still filling in.
#[derive(Clone, Copy)]
pub struct OpenSearch<'a> {
    /// What is typed, or empty for the empty field.
    pub query: &'a str,
    /// The rows it shows, in the order the person sees them.
    pub rows: &'a [tilo_desktop_surface::SearchRow],
    /// Whether the search says it is still searching: the rows the card shows are not its answer
    /// yet — nothing is ready to show, or they are not cut to the card's room. A source still out
    /// does not make it searching: the rows that are ready are shown at once, and the late rows
    /// join them when they come (`expecting`).
    pub searching: bool,
}

/// A window the shell shows: the tab it stands in, the name the person reads it by, and — for a
/// window this session does not run (an X11 or Wayland window of another desktop) — the app that
/// showed it, which the shell can picture and read of, and never act on.
#[derive(Clone)]
pub struct Shown {
    /// Its tab.
    pub tab: u64,
    /// Its name in the bar, the strip and the catalogue.
    pub title: String,
    /// The app id of a window this session does not run, when it is one.
    pub foreign: Option<String>,
}

/// The shell's resource: its own verbs, with each window of `windows` a row of a declared type that
/// can be focused and closed — and read, for a window of another desktop — and, for each tab
/// of `not_responding`, waited for or force quit — across several displays each of `displays` (in
/// reading order) one that can be focused, and the open search (its query and rows), when open.
#[must_use]
pub fn shell_resource(
    windows: &[Shown],
    not_responding: &[u64],
    displays: &[u32],
    search: Option<OpenSearch<'_>>,
) -> ResourceSpec {
    let screens = displays.iter().enumerate().map(|(index, display)| {
        let number = index.saturating_add(1).to_string();
        LiveRow::typed_with::<Display>(
            display_resource_id(*display),
            t_args(
                "desktop-display-name",
                &[("number", number.as_str().into())],
            ),
            &RowBindings::new().bind(
                LayoutEffect::FOCUS_DISPLAY,
                [("display", display.to_string().into())],
            ),
        )
        .into_resource()
    });
    let windows = windows
        .iter()
        .map(|shown| window_row(shown, not_responding));
    let search = search.map(search_resource);
    offer::<LayoutEffect>(typed_resource::<Shell>(
        SHELL_RESOURCE,
        t("desktop-actions-shell"),
    ))
    .with_children(windows.chain(screens).chain(search))
}

/// One window of the shell: focusing and closing it, and — a window of another desktop —
/// reading it through the shell, and — one whose app stopped answering — waiting for it or force
/// quitting its app. Its verbs name the window they act on, as its row does.
///
/// The bar's window bar, a page and an agent read the same verbs by the same address.
fn window_row(shown: &Shown, not_responding: &[u64]) -> ResourceSpec {
    let tab = shown.tab.to_string();
    let names = &[("window", shown.title.as_str().into())];
    let hung = not_responding.contains(&shown.tab);
    let mut bindings = RowBindings::new()
        .bind(LayoutEffect::FOCUS, [("tab", tab.clone().into())])
        .bind(LayoutEffect::CLOSE_TAB, [("tab", tab.clone().into())]);
    if hung {
        bindings = bindings
            .bind(LayoutEffect::WAIT_WINDOW, [("tab", tab.clone().into())])
            .bind(LayoutEffect::FORCE_QUIT_APP, [("tab", tab.into())]);
    }
    let mut row =
        LiveRow::typed_with::<Window>(tab_resource_id(shown.tab), shown.title.clone(), &bindings)
            .labelled(LayoutEffect::FOCUS, t_args("shell-focus-window", names))
            .labelled(LayoutEffect::CLOSE_TAB, t_args("shell-close-window", names));
    if hung {
        row = row
            .labelled(
                LayoutEffect::WAIT_WINDOW,
                t_args("shell-wait-window", names),
            )
            .labelled(
                LayoutEffect::FORCE_QUIT_APP,
                t_args("shell-force-quit-window", names),
            );
    }
    let mut window = row.into_resource();
    // A window of another desktop has no surface of its own to ask what it shows: the shell
    // answers both read actions for it, and neither acts on it
    // (`tilo_session_core::desktop::window`).
    if shown.foreign.is_some() {
        window = window
            .with_action(window::look(t_args("shell-look-window", names)))
            .with_action(window::read(t_args("shell-read-window", names)));
    }
    window
}

/// The launchpad's resource: each card `cards` is a row of a declared type offering the shell's
/// launchpad verb, bound to the card it stands for.
/// A card is one declaration ([`crate::launchpad::LaunchpadAction`]): the host publishes it as a
/// host action, which is how the palette and the catalogue of actions name it, and this lists the
/// same verb to agents and scripts that invoke an action by the resource it is published on. Both
/// reach the verb through the host's one action API.
#[must_use]
pub fn launchpad_resource(cards: &[crate::launchpad::LaunchpadAction]) -> ResourceSpec {
    let cards = cards
        .iter()
        .map(|card| (card.slug(), card.label()))
        .chain([("apps", t("launchpad-open-app"))]);
    let rows = cards.map(|(slug, label)| {
        LiveRow::typed_with::<LaunchpadCard>(
            launchpad_card_id(slug),
            label.clone(),
            &RowBindings::new().bind(LayoutEffect::LAUNCHPAD, [("card", slug.into())]),
        )
        .labelled(LayoutEffect::LAUNCHPAD, label)
        .into_resource()
    });
    typed_resource::<Launchpad>(LAUNCHPAD_RESOURCE, t("desktop-actions-launchpad"))
        .with_children(rows)
}

/// The open search: what is typed, and each row it shows, which can be chosen. Its detail says it
/// is still searching until every source has answered and its rows are cut to the card: a reader of
/// the shell (a script, an agent, assistive technology) knows the rows are final once it is gone.
#[expect(
    clippy::expect_used,
    reason = "the search verb is one of the declared vocabulary's, so its spec publishes"
)]
fn search_resource(search: OpenSearch<'_>) -> ResourceSpec {
    let OpenSearch {
        query,
        rows,
        searching,
    } = search;
    let rows = rows.iter().enumerate().map(|(at, row)| {
        let index = u64::try_from(at).unwrap_or(u64::MAX);
        // The row's own action is the one this row chooses; the line its action asks for stays the
        // caller's.
        let bindings = RowBindings::new().bind(
            LayoutEffect::CHOOSE_SEARCH_ROW,
            [("row", serde_json::Value::from(index))],
        );
        let spec =
            LiveRow::typed_with::<SearchRow>(search_row_id(at), row.title.clone(), &bindings)
                .labelled(LayoutEffect::CHOOSE_SEARCH_ROW, row.title.clone())
                .into_resource();
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
    let typed = LayoutEffect::SEARCH
        .spec()
        .expect("the search verb is declared");
    let resource = typed_resource::<Search>(SEARCH_RESOURCE, label)
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
