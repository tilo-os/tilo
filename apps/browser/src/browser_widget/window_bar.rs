//! The browser's toolbar is slots in the window's bar
//! ([`ui::window_toolbar`]): back and forward, the address as the window's
//! editable location (the site's domain at rest, the whole address while
//! edited, Enter or the go button navigating), and reload, bookmark and the
//! library. The page's favicon is the location's picture, standing where
//! the app's icon would. While the address is edited in the bar the browser
//! hears what is typed and answers with the omnibox's rows (pages visited
//! before, the engine's searches), which the bar lists under the field.
//! While the desktop's bar shows them the window draws no toolbar row of
//! its own (`build_composition`); outside a Tilo session it keeps its row.

use ui::window_toolbar::{
    LocationSuggestion, ToolbarAction, ToolbarEvent, ToolbarImage, ToolbarLocation,
    ToolbarNavigation, WindowToolbar,
};

use crate::omnibox::{Row, RowKind};

use super::BrowserWidget;

/// The action ids the browser's slots answer to.
const RELOAD: &str = "reload";
const STOP: &str = "stop";
const BOOKMARK: &str = "bookmark";
const LIBRARY: &str = "library";

fn t(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

/// What the bar shows of `uri` at rest: its site, without the scheme or a
/// leading `www.`. Empty for no page.
pub(super) fn site_label(uri: &str) -> String {
    let uri = uri.trim();
    if uri.is_empty() {
        return String::new();
    }
    url::Url::parse(uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map_or_else(
            || uri.to_owned(),
            |host| host.strip_prefix("www.").unwrap_or(&host).to_owned(),
        )
}

/// Everything the slots are made from: the window declares them again only
/// when one of these changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BarInputs {
    uri: String,
    /// The bar's back and forward slots, exactly as declared.
    navigation: ToolbarNavigation,
    loading: bool,
    bookmarked: bool,
    library: bool,
    favicon: Option<Vec<u8>>,
    suggestions: Vec<LocationSuggestion>,
}

/// One omnibox row as the bar lists it: what it says, what choosing it
/// commits (an address, or the words to search for), and its kind's icon.
fn suggestion(row: &Row) -> LocationSuggestion {
    let icon = match row.kind {
        RowKind::Go => "globe",
        RowKind::History => "history",
        RowKind::Search | RowKind::Suggestion => "search",
    };
    LocationSuggestion {
        // A visited page by its title (its address when it has none), a
        // search by its words, an address as typed.
        label: row.title.clone(),
        value: row.url.clone(),
        icon: Some(icon.to_owned()),
    }
}

impl BrowserWidget {
    fn bar_inputs(&self) -> BarInputs {
        BarInputs {
            uri: self.current_uri.clone(),
            navigation: self.bar_navigation(),
            loading: self.progress < 1.0,
            bookmarked: self.page_bookmarked(),
            library: self.show_library,
            favicon: self.favicon_png.clone(),
            suggestions: self.bar_suggestions(),
        }
    }

    /// Which of back and forward can go.
    fn bar_navigation(&self) -> ToolbarNavigation {
        ToolbarNavigation {
            back: self.can_go_back,
            forward: self.can_go_forward,
        }
    }

    /// What the bar lists under its edited address: the omnibox's rows for
    /// what the bar said is typed there, nothing while it is not edited.
    fn bar_suggestions(&self) -> Vec<LocationSuggestion> {
        if self.bar_query.is_none() {
            return Vec::new();
        }
        self.suggestions
            .items()
            .iter()
            .take(ui::window_toolbar::MAX_SUGGESTIONS)
            .map(suggestion)
            .collect()
    }

    /// Whether the page shown is bookmarked, in whichever bookmarks this
    /// window keeps.
    fn page_bookmarked(&self) -> bool {
        if self.person_bookmarks.is_some() {
            self.personal_bookmarks
                .bookmarks
                .contains(&self.current_uri)
        } else {
            self.bookmarks.contains(&self.current_uri)
        }
    }

    /// The slots this window shows in its bar.
    pub(super) fn window_toolbar(&self) -> WindowToolbar {
        let loading = self.progress < 1.0;
        let page = !self.current_uri.trim().is_empty();
        let icon = if self.browsing_mode.is_private() {
            // What marks a private window, now that it has no toolbar of
            // its own to tint.
            Some("eye-off".to_owned())
        } else if self.current_uri.starts_with("https://") {
            Some("lock".to_owned())
        } else {
            None
        };
        let bookmarked = self.page_bookmarked();
        WindowToolbar {
            sidebar: None,
            navigation: Some(self.bar_navigation()),
            location: Some(ToolbarLocation {
                label: site_label(&self.current_uri),
                value: self.current_uri.clone(),
                placeholder: t("browser-address-placeholder"),
                icon,
                editable: true,
                name: t("browser-address"),
                go_name: t("browser-go"),
                segments: Vec::new(),
                image: self.favicon_png.clone().map(|png| ToolbarImage { png }),
                suggestions: self.bar_suggestions(),
            }),
            actions: vec![
                ToolbarAction {
                    id: (if loading { STOP } else { RELOAD }).into(),
                    icon: (if loading { "x" } else { "refresh" }).into(),
                    name: t(if loading {
                        "browser-stop"
                    } else {
                        "browser-reload"
                    }),
                    enabled: true,
                    on: false,
                },
                ToolbarAction {
                    id: BOOKMARK.into(),
                    icon: "star".into(),
                    name: t(if bookmarked {
                        "browser-page-bookmarked"
                    } else {
                        "browser-bookmark-page"
                    }),
                    enabled: page && !bookmarked,
                    on: bookmarked,
                },
                ToolbarAction {
                    id: LIBRARY.into(),
                    icon: "history".into(),
                    name: t(if self.show_library {
                        "browser-library-close"
                    } else {
                        "browser-library-open"
                    }),
                    enabled: true,
                    on: self.show_library,
                },
            ],
        }
    }

    /// Tell the window's bar the slots as they are now, and run what the
    /// person did with them: the same canonical actions the browser's own
    /// toolbar runs. Returns whether anything ran.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        self.declare_window_toolbar();
        let events = ui::window_toolbar::take_events();
        let ran = !events.is_empty();
        for event in events {
            self.run_window_toolbar_event(event);
        }
        ran
    }

    /// Tell the window's bar the slots, when what they are made from
    /// changed.
    pub(super) fn declare_window_toolbar(&mut self) {
        let inputs = self.bar_inputs();
        if self.declared_bar.as_ref() != Some(&inputs) {
            ui::window_toolbar::declare(Some(self.window_toolbar()));
            self.declared_bar = Some(inputs);
        }
    }

    /// Run one thing the person did in the bar.
    pub(super) fn run_window_toolbar_event(&mut self, event: ToolbarEvent) {
        match event {
            // The browser declares no sidebar toggle.
            ToolbarEvent::ToggleSidebar => {}
            ToolbarEvent::Back => self.queue_unit_operation("browser.page", "back"),
            ToolbarEvent::Forward => self.queue_unit_operation("browser.page", "forward"),
            ToolbarEvent::Commit { text } => {
                // Exactly what Enter in the window's own address field does;
                // a chosen suggestion commits its address or its search.
                self.bar_query = None;
                self.address.set(text);
                self.navigate_to_input();
                self.reset_omnibox();
            }
            ToolbarEvent::Edit { text } => {
                // The omnibox answers on this frame's suggestion pass.
                self.bar_query = Some(text);
            }
            ToolbarEvent::Action { id } => match id.as_str() {
                RELOAD => self.queue_unit_operation("browser.page", "reload"),
                STOP => self.queue_unit_operation("browser.page", "stop"),
                BOOKMARK => {
                    if self.person_bookmarks.is_some() {
                        self.queue_unit_operation("browser.page", "bookmark-personal-page");
                    } else if let Some(action) = self.page_bookmark_action() {
                        self.submit(action);
                    }
                }
                LIBRARY => {
                    let (resource, action) = self.library_command();
                    self.queue_unit_operation(resource, action);
                }
                _ => {}
            },
            // The bar's own search and History control are run by the
            // framework on the app's widgets, never handed to its slots.
            ToolbarEvent::Search { .. }
            | ToolbarEvent::SearchStep { .. }
            | ToolbarEvent::SearchClose
            | ToolbarEvent::History => return,
        }
        self.structure_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_shows_the_site_at_rest() {
        assert_eq!(
            site_label("https://www.duckduckgo.com/?q=rust"),
            "duckduckgo.com"
        );
        assert_eq!(site_label("http://127.0.0.1:42975/"), "127.0.0.1");
        assert_eq!(site_label(""), "");
    }
}
