//! The browser's toolbar is slots in the window's bar
//! ([`ui::window_toolbar`]): back and forward, the address as the window's
//! editable location (the site's domain at rest, the whole address while
//! edited, Enter or the go button navigating), and reload, bookmark and the
//! library. The page's favicon is the location's picture, standing where
//! the app's icon would. While the address is edited in the bar the browser
//! hears what is typed and answers with the omnibox's rows (pages visited
//! before, the engine's searches), which the bar lists under the field: the
//! only list of them, as the window draws none of its own while hosted.
//! Choosing a row commits where it goes (a search commits its engine's
//! address, never the words, which could read as an address).
//! Everything else the window's own row offers is in the bar too: the Web
//! Store offer and the downloads in progress as buttons, and the More menu's
//! commands and the extensions' popups in the bar's overflow menu, grouped
//! as menus so they stay within the bar's [`ui::window_toolbar::MAX_DECLARED_ACTIONS`].
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
const DOWNLOADS: &str = "downloads";
const FIND: &str = "find";
const READER_VIEW: &str = "reader-view";
const PICTURE_IN_PICTURE: &str = "picture-in-picture";
const DEVELOPER_TOOLS: &str = "developer-tools";
const ZOOM_IN: &str = "zoom-in";
const ZOOM_OUT: &str = "zoom-out";
const ZOOM_RESET: &str = "zoom-reset";
const SITE_INFORMATION: &str = "site-information";
const ADS_AND_TRACKERS: &str = "ads-and-trackers";
const PAUSE_HISTORY: &str = "pause-history";
const PRIVATE_WINDOW: &str = "private-window";
const SETTINGS: &str = "settings";
const MANAGE_EXTENSIONS: &str = "manage-extensions";

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
    /// Its buttons and menus, exactly as declared.
    actions: Vec<ToolbarAction>,
    favicon: Option<Vec<u8>>,
    suggestions: Vec<LocationSuggestion>,
}

/// One omnibox row as the bar lists it: what it says, the line under it,
/// what choosing it commits (where the row goes: an address, or the search
/// engine's address for the words) and its kind's icon.
/// The address field's mark for how the page arrived: a lock over
/// `https://`, an open lock over `http://` from another machine, which
/// anyone on the way can read or change. This machine's own pages are
/// neither.
fn connection_icon(uri: &str) -> Option<&'static str> {
    let url = url::Url::parse(uri).ok()?;
    match url.scheme() {
        "https" => Some("lock"),
        "http" if !this_machine(&url) => Some("lock-open"),
        _ => None,
    }
}

/// Whether `url` names this machine: `localhost`, a `.localhost` name or a
/// loopback address.
fn this_machine(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            name.eq_ignore_ascii_case("localhost")
                || name.to_ascii_lowercase().ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn suggestion(row: &Row, engine: &str) -> LocationSuggestion {
    let icon = match row.kind {
        RowKind::Go => "globe",
        RowKind::History => "history",
        RowKind::Search | RowKind::Suggestion => "search",
        RowKind::Extension => "box",
    };
    let detail = match row.kind {
        // A visited page's address under its title; the extension a
        // keyword's row goes to.
        RowKind::History | RowKind::Extension => row.detail.clone(),
        // The typed words: which engine they go to.
        RowKind::Search => crate::resource::CATALOG.t_args(
            "browser-suggestion-search",
            &[(
                "engine",
                ui::i18n::ArgValue::Text(crate::settings::search_engine_name(engine)),
            )],
        ),
        // An address as typed, and the engine's own searches, say it all.
        RowKind::Go | RowKind::Suggestion => String::new(),
    };
    LocationSuggestion {
        // A visited page by its title (its address when it has none), a
        // search by its words, an address as typed.
        label: row.title.clone(),
        value: super::row_destination(row, engine),
        icon: Some(icon.to_owned()),
        detail,
    }
}

impl BrowserWidget {
    fn bar_inputs(&self) -> BarInputs {
        BarInputs {
            uri: self.current_uri.clone(),
            navigation: self.bar_navigation(),
            actions: self.bar_actions(),
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
    pub(super) fn bar_suggestions(&self) -> Vec<LocationSuggestion> {
        if self.bar_query.is_none() {
            return Vec::new();
        }
        self.bar_rows
            .iter()
            .take(ui::window_toolbar::MAX_SUGGESTIONS)
            .map(|row| suggestion(row, &self.settings.search_engine))
            .collect()
    }

    /// Whether the page shown is bookmarked, in whichever bookmarks this
    /// window keeps.
    pub(super) fn page_bookmarked(&self) -> bool {
        if self.person_bookmarks.is_some() {
            self.personal_bookmarks
                .bookmarks
                .contains(&self.current_uri)
        } else {
            self.bookmarks.contains(&self.current_uri)
        }
    }

    /// The bar's buttons, then its menus: what the window's own row and its
    /// More menu offer outside a Tilo session.
    fn bar_actions(&self) -> Vec<ToolbarAction> {
        let loading = self.progress < 1.0;
        let page = !self.current_uri.trim().is_empty();
        let bookmarked = self.page_bookmarked();
        let mut actions = Vec::new();
        // On a Web Store item: the reason to be on the page, first in the row.
        actions.extend(self.extensions.bar_offer());
        // An extension's side panel beside the page closes from the row.
        actions.extend(self.extensions.bar_side_panel());
        actions.extend([
            ToolbarAction::new(
                if loading { STOP } else { RELOAD },
                if loading { "x" } else { "refresh" },
                t(if loading {
                    "browser-stop"
                } else {
                    "browser-reload"
                }),
            ),
            ToolbarAction::new(
                BOOKMARK,
                "star",
                t(if bookmarked {
                    "browser-page-bookmarked"
                } else {
                    "browser-bookmark-page"
                }),
            )
            .enabled(page && !bookmarked)
            .on(bookmarked),
            ToolbarAction::new(
                LIBRARY,
                "history",
                t(if self.show_library {
                    "browser-library-close"
                } else {
                    "browser-library-open"
                }),
            )
            .on(self.show_library),
        ]);
        if let Some(status) = self.capabilities.toolbar_status() {
            actions.push(ToolbarAction::new(DOWNLOADS, "download", status));
        }
        let item = |id: &str, icon: &str, key: &str| ToolbarAction::new(id, icon, t(key));
        actions.push(
            ToolbarAction::new("page", "globe", t("browser-menu-page"))
                .with_items(vec![
                    item(FIND, "search", "browser-menu-find"),
                    item(READER_VIEW, "file-text", "browser-menu-reader-view").on(self.reader_view),
                    item(
                        PICTURE_IN_PICTURE,
                        "picture-in-picture-2",
                        "browser-menu-picture-in-picture",
                    )
                    .on(self.active_pip),
                    item(DEVELOPER_TOOLS, "code", "browser-menu-developer-tools"),
                ])
                .in_menu(),
        );
        actions.push(
            ToolbarAction::new("zoom", "plus", t("browser-menu-zoom"))
                .with_items(vec![
                    item(ZOOM_IN, "plus", "browser-menu-zoom-in"),
                    item(ZOOM_OUT, "minus", "browser-menu-zoom-out"),
                    ToolbarAction::new(ZOOM_RESET, "rotate-ccw", self.daily.zoom_label()),
                ])
                .in_menu(),
        );
        let mut extensions = self.extensions.bar_items();
        extensions.push(item(
            MANAGE_EXTENSIONS,
            "settings",
            "browser-menu-manage-extensions",
        ));
        actions.push(
            ToolbarAction::new("extensions", "box", t("browser-menu-extensions"))
                .with_items(extensions)
                .in_menu(),
        );
        actions.push(
            ToolbarAction::new("privacy", "shield", t("browser-menu-privacy"))
                .with_items(vec![
                    item(SITE_INFORMATION, "info", "browser-menu-site-information").enabled(page),
                    item(ADS_AND_TRACKERS, "shield", "browser-menu-ads-and-trackers").enabled(page),
                    item(PAUSE_HISTORY, "pause", "browser-menu-pause-history")
                        .on(self.history_paused()),
                    item(PRIVATE_WINDOW, "eye-off", "browser-menu-new-private-window"),
                ])
                .in_menu(),
        );
        actions.push(
            item(
                SETTINGS,
                "settings",
                if self.show_settings {
                    "browser-menu-close-settings"
                } else {
                    "browser-menu-settings"
                },
            )
            .in_menu(),
        );
        actions
    }

    /// The slots this window shows in its bar.
    pub(super) fn window_toolbar(&self) -> WindowToolbar {
        let icon = if self.browsing_mode.is_private() {
            // What marks a private window, now that it has no toolbar of
            // its own to tint.
            Some("eye-off".to_owned())
        } else {
            connection_icon(&self.current_uri).map(str::to_owned)
        };
        WindowToolbar {
            sidebar: None,
            navigation: Some(self.bar_navigation()),
            location: Some(ToolbarLocation {
                label: site_label(&self.current_uri),
                shorter: Vec::new(),
                value: self.current_uri.clone(),
                placeholder: t("browser-address-placeholder"),
                icon,
                editable: true,
                name: t("browser-address"),
                go_name: t("browser-go"),
                segments: Vec::new(),
                image: self.favicon_png.clone().map(|png| ToolbarImage { png }),
                suggestions: self.bar_suggestions(),
                choices: Vec::new(),
                anchor: None,
                edited: false,
            }),
            actions: self.bar_actions(),
            ..WindowToolbar::default()
        }
    }

    /// Tell the window's bar the slots as they are now, and run what the
    /// person did with them: the same canonical actions the browser's own
    /// toolbar runs. Events invalidate composition only when they change it.
    pub(super) fn sync_window_toolbar(&mut self) {
        self.declare_window_toolbar();
        for event in ui::window_toolbar::take_events() {
            self.run_window_toolbar_event(event);
        }
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
                // The omnibox answers on this frame's suggestion pass. The
                // page's own address is the field put back: no query, so
                // nothing abandoned is suggested for again.
                self.bar_query = (text.trim() != self.current_uri.trim()).then_some(text);
                // Suggestions belong to the host's bar. Rebuilding the page
                // here discards its retained image and input state per key.
                return;
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
                // The More menu's rows run these same commands.
                DOWNLOADS => self.queue_unit_operation("browser.downloads", "show-downloads"),
                FIND => (self.daily.command(crate::daily_controls::Action::Open))(),
                READER_VIEW => self.queue_unit_operation("browser.page", "reader-view"),
                PICTURE_IN_PICTURE => {
                    self.queue_unit_operation("browser.page", "picture-in-picture");
                }
                DEVELOPER_TOOLS => self
                    .engine
                    .send(ui::webview::WebViewCommand::ShowDeveloperTools),
                ZOOM_IN => (self.daily.command(crate::daily_controls::Action::Zoom(1)))(),
                ZOOM_OUT => (self.daily.command(crate::daily_controls::Action::Zoom(-1)))(),
                ZOOM_RESET => (self.daily.command(crate::daily_controls::Action::Zoom(0)))(),
                SITE_INFORMATION => (self.site.open_command(true))(),
                ADS_AND_TRACKERS => (self.site.open_command(false))(),
                PAUSE_HISTORY => self.queue_unit_operation("browser.session", "toggle-pause"),
                PRIVATE_WINDOW => self.private_window_request.set(true),
                SETTINGS => {
                    let (resource, action) = self.settings_command();
                    self.queue_unit_operation(resource, action);
                }
                MANAGE_EXTENSIONS => {
                    self.queue_unit_operation("browser.extensions", "show-extensions");
                }
                other => {
                    self.extensions.bar_action(other);
                }
            },
            // The bar's own search and History control are run by the
            // framework on the app's widgets, never handed to its slots.
            ToolbarEvent::Search { .. }
            | ToolbarEvent::SearchStep { .. }
            | ToolbarEvent::SearchClose
            | ToolbarEvent::SearchOptions { .. }
            | ToolbarEvent::SearchCover { .. }
            | ToolbarEvent::SearchReplace { .. }
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

    #[test]
    fn the_bar_marks_a_page_that_came_unencrypted() {
        for (uri, icon) in [
            ("https://example.com/", Some("lock")),
            ("http://neverssl.com/", Some("lock-open")),
            ("http://192.168.1.1/", Some("lock-open")),
            ("http://localhost:3000/", None),
            ("http://app.localhost/", None),
            ("http://127.0.0.1:8080/", None),
            ("http://[::1]:8443/", None),
            ("chrome-extension://abc/options.html", None),
            ("", None),
        ] {
            assert_eq!(connection_icon(uri), icon, "{uri}");
            if let Some(icon) = icon {
                assert!(ui::icons::Icon::from_name(icon).is_some(), "{icon}");
            }
        }
    }
}

#[cfg(test)]
mod more_menu_tests {
    use super::*;
    use ui::webview::WebViewCommand;
    use ui::webview::test_engine::TestEngine;

    fn ids(actions: &[ToolbarAction]) -> Vec<String> {
        actions
            .iter()
            .flat_map(|action| {
                std::iter::once(action.id.clone())
                    .chain(action.items.iter().map(|item| item.id.clone()))
            })
            .collect()
    }

    #[test]
    fn the_bar_carries_every_command_of_the_more_menu() {
        let widget = BrowserWidget::new_headless();
        let declared = widget.window_toolbar();
        let bounded = declared.clone().bounded();
        // Nothing is cut: no action past the bar's limit, no item whose icon
        // the bar does not know.
        assert_eq!(ids(&bounded.actions), ids(&declared.actions));
        for id in [
            FIND,
            READER_VIEW,
            PICTURE_IN_PICTURE,
            DEVELOPER_TOOLS,
            ZOOM_IN,
            ZOOM_OUT,
            ZOOM_RESET,
            SITE_INFORMATION,
            ADS_AND_TRACKERS,
            PAUSE_HISTORY,
            PRIVATE_WINDOW,
            SETTINGS,
            MANAGE_EXTENSIONS,
        ] {
            assert!(
                ids(&bounded.actions).iter().any(|declared| declared == id),
                "{id}"
            );
        }
        // The row keeps reload, bookmark and the library.
        let row: Vec<_> = bounded
            .actions
            .iter()
            .filter(|action| !action.menu)
            .take(ui::window_toolbar::MAX_ACTIONS)
            .map(|action| action.id.as_str())
            .collect();
        assert_eq!(row, [RELOAD, BOOKMARK, LIBRARY]);
    }

    #[test]
    fn a_bar_command_does_what_its_menu_row_does() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let run = |widget: &mut BrowserWidget, id: &str| {
            widget.run_window_toolbar_event(ToolbarEvent::Action { id: id.into() });
            super::super::tests::layout_at(widget, 1200.0, 800.0);
        };
        let sends = |widget: &mut BrowserWidget, id: &str, command: WebViewCommand| {
            let before = engine.commands().len();
            run(widget, id);
            let after = engine.commands().split_off(before);
            assert!(after.contains(&command), "{id}: {command:?} in {after:?}");
        };
        sends(&mut widget, ZOOM_IN, WebViewCommand::SetZoomLevel(1.0));
        sends(&mut widget, ZOOM_RESET, WebViewCommand::SetZoomLevel(0.0));
        sends(
            &mut widget,
            DEVELOPER_TOOLS,
            WebViewCommand::ShowDeveloperTools,
        );
        run(&mut widget, FIND);
        assert!(widget.daily.open, "Find opens the find bar");
        // The rest queue the canonical action the More menu's row queues.
        for (id, resource, action) in [
            (READER_VIEW, "browser.page", "reader-view"),
            (PICTURE_IN_PICTURE, "browser.page", "picture-in-picture"),
            (PAUSE_HISTORY, "browser.session", "toggle-pause"),
            (SETTINGS, "browser.settings", "show-settings"),
            (DOWNLOADS, "browser.downloads", "show-downloads"),
            (MANAGE_EXTENSIONS, "browser.extensions", "show-extensions"),
        ] {
            let mut page = BrowserWidget::new_headless();
            page.run_window_toolbar_event(ToolbarEvent::Action { id: id.into() });
            assert!(
                page.pending_controls.iter().any(|invocation| {
                    invocation.address.resource == resource && invocation.address.action == action
                }),
                "{id} reaches {resource}/{action}"
            );
        }
        let mut page = BrowserWidget::new_headless();
        page.run_window_toolbar_event(ToolbarEvent::Action {
            id: PRIVATE_WINDOW.into(),
        });
        assert!(page.private_window_request.get());
    }
}
