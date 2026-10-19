//! The browser's toolbar is slots in the window's bar
//! ([`ui::window_toolbar`]): back and forward, the address as the window's
//! editable location (the site's domain at rest, the whole address while
//! edited, Enter or the go button navigating), and reload, bookmark and the
//! library. While the desktop's bar shows them the window draws no toolbar
//! row of its own (`build_composition`); outside a Tilo session it keeps
//! its row.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

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
    back: bool,
    forward: bool,
    loading: bool,
    bookmarked: bool,
    library: bool,
}

impl BrowserWidget {
    fn bar_inputs(&self) -> BarInputs {
        BarInputs {
            uri: self.current_uri.clone(),
            back: self.can_go_back,
            forward: self.can_go_forward,
            loading: self.progress < 1.0,
            bookmarked: self.page_bookmarked(),
            library: self.show_library,
        }
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
            navigation: Some(ToolbarNavigation {
                back: self.can_go_back,
                forward: self.can_go_forward,
            }),
            location: Some(ToolbarLocation {
                label: site_label(&self.current_uri),
                value: self.current_uri.clone(),
                placeholder: t("browser-address-placeholder"),
                icon,
                editable: true,
                name: t("browser-address"),
                go_name: t("browser-go"),
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
        let inputs = self.bar_inputs();
        if self.declared_bar.as_ref() != Some(&inputs) {
            ui::window_toolbar::declare(Some(self.window_toolbar()));
            self.declared_bar = Some(inputs);
        }
        let events = ui::window_toolbar::take_events();
        let ran = !events.is_empty();
        for event in events {
            self.run_window_toolbar_event(event);
        }
        ran
    }

    /// Run one thing the person did in the bar.
    pub(super) fn run_window_toolbar_event(&mut self, event: ToolbarEvent) {
        match event {
            ToolbarEvent::Back => self.queue_unit_operation("browser.page", "back"),
            ToolbarEvent::Forward => self.queue_unit_operation("browser.page", "forward"),
            ToolbarEvent::Commit { text } => {
                // Exactly what Enter in the window's own address field does.
                self.address.set(text);
                self.navigate_to_input();
                self.reset_omnibox();
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
