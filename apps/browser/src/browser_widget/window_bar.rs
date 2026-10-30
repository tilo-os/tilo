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
    LocationSegment, LocationSuggestion, ToolbarAction, ToolbarEvent, ToolbarImage,
    ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

use crate::omnibox::{Row, RowKind};

use super::BrowserWidget;

/// What an entry of the window's bar runs: the command its row in the
/// window's own More menu runs.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BarCommand {
    /// The resource's operation with no arguments.
    Unit(&'static str, &'static str),
    Bookmark,
    Library,
    DeveloperTools,
    /// Zoom in, out (by the step) or back to 100 % (0).
    Zoom(i32),
    /// The site's information, or its ads and trackers.
    Site {
        information: bool,
    },
    PrivateWindow,
    Settings,
    /// The page again, in a window of the person's own: with their
    /// sign-ins.
    OwnWindow,
}

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
    /// What it says after back and forward: whether the window browses as
    /// a visitor, which changes once a credential is entered in it.
    leading: Vec<ToolbarAction>,
    favicon: Option<Vec<u8>>,
    suggestions: Vec<LocationSuggestion>,
    /// The Library's place while it is shown: its name, then its section's,
    /// each with the place it commits.
    library: Option<Vec<(String, &'static str)>>,
}

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

/// One omnibox row as the bar lists it: what it says, the line under it,
/// what choosing it commits (where the row goes: an address, or the search
/// engine's address for the words) and its kind's icon.
fn suggestion(row: &Row, engine: &str) -> LocationSuggestion {
    let icon = match row.kind {
        RowKind::Go => "globe",
        RowKind::History => "history",
        // The toolbar's mark for a saved page.
        RowKind::Bookmark => "star",
        RowKind::Search | RowKind::Suggestion => "search",
        RowKind::Extension => "box",
    };
    let detail = match row.kind {
        // A visited page's address under its title; the extension a
        // keyword's row goes to.
        RowKind::History | RowKind::Extension => row.detail.clone(),
        // A saved page says it is one, and where it leads.
        RowKind::Bookmark => crate::resource::CATALOG.t_args(
            "browser-suggestion-bookmark",
            &[("url", ui::i18n::ArgValue::Text(row.detail.clone()))],
        ),
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
            leading: self.custody_leading(),
            favicon: self.favicon_png.clone(),
            suggestions: self.bar_suggestions(),
            library: self.show_library.then(|| self.library_segments()),
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
        let bar = &self.bar_entries;
        let entry = |icon: &str, key: &str, command| bar.entry(icon, t(key), command);
        let unit = |icon: &str, key: &str, resource, action| {
            entry(icon, key, BarCommand::Unit(resource, action))
        };
        actions.extend([
            if loading {
                unit("x", "browser-stop", "browser.page", "stop")
            } else {
                unit("refresh", "browser-reload", "browser.page", "reload")
            },
            bar.entry(
                "star",
                t(if bookmarked {
                    "browser-page-bookmarked"
                } else {
                    "browser-bookmark-page"
                }),
                BarCommand::Bookmark,
            )
            .enabled(page && !bookmarked)
            .on(bookmarked),
            bar.entry(
                "history",
                t(if self.show_library {
                    "browser-library-close"
                } else {
                    "browser-library-open"
                }),
                BarCommand::Library,
            )
            .on(self.show_library),
        ]);
        if let Some(status) = self.capabilities.toolbar_status() {
            let downloads = BarCommand::Unit("browser.downloads", "show-downloads");
            actions.push(bar.entry("download", status, downloads));
        }
        actions.push(
            ToolbarAction::new("page", "globe", t("browser-menu-page"))
                .with_items(vec![
                    unit(
                        "file-text",
                        "browser-menu-reader-view",
                        "browser.page",
                        "reader-view",
                    )
                    .on(self.reader_view),
                    unit(
                        "picture-in-picture-2",
                        "browser-menu-picture-in-picture",
                        "browser.page",
                        "picture-in-picture",
                    )
                    .on(self.active_pip),
                    entry(
                        "code",
                        "browser-menu-developer-tools",
                        BarCommand::DeveloperTools,
                    ),
                ])
                .in_menu(),
        );
        actions.push(
            ToolbarAction::new("zoom", "plus", t("browser-menu-zoom"))
                .with_items(vec![
                    entry("plus", "browser-menu-zoom-in", BarCommand::Zoom(1)),
                    entry("minus", "browser-menu-zoom-out", BarCommand::Zoom(-1)),
                    bar.entry("rotate-ccw", self.daily.zoom_label(), BarCommand::Zoom(0)),
                ])
                .in_menu(),
        );
        let mut extensions = self.extensions.bar_items();
        extensions.push(unit(
            "settings",
            "browser-menu-manage-extensions",
            "browser.extensions",
            "show-extensions",
        ));
        actions.push(
            ToolbarAction::new("extensions", "box", t("browser-menu-extensions"))
                .with_items(extensions)
                .in_menu(),
        );
        actions.push(
            ToolbarAction::new("privacy", "shield", t("browser-menu-privacy"))
                .with_items(vec![
                    entry(
                        "info",
                        "browser-menu-site-information",
                        BarCommand::Site { information: true },
                    )
                    .enabled(page),
                    entry(
                        "shield",
                        "browser-menu-ads-and-trackers",
                        BarCommand::Site { information: false },
                    )
                    .enabled(page),
                    unit(
                        "pause",
                        "browser-menu-pause-history",
                        "browser.session",
                        "toggle-pause",
                    )
                    .on(self.history_paused()),
                    entry(
                        "eye-off",
                        "browser-menu-new-private-window",
                        BarCommand::PrivateWindow,
                    ),
                ])
                .in_menu(),
        );
        actions.push(
            entry(
                "settings",
                if self.show_settings {
                    "browser-menu-close-settings"
                } else {
                    "browser-menu-settings"
                },
                BarCommand::Settings,
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
        let location = if self.show_library {
            self.library_location()
        } else {
            self.address_location(icon)
        };
        WindowToolbar {
            sidebar: None,
            navigation: Some(self.bar_navigation()),
            location: Some(location),
            actions: self.bar_actions(),
            leading: self.custody_leading(),
            ..WindowToolbar::default()
        }
    }

    /// What says, right after back and forward, that the window was opened
    /// for an agent and browses as a visitor, without the person's
    /// sign-ins: a button saying so, whose menu opens the page in a window
    /// of the person's own. Their own window says nothing: it is the
    /// ordinary one; nor does a visitor's once a credential was entered in
    /// it, since its page may then hold a sign-in.
    pub(super) fn custody_leading(&self) -> Vec<ToolbarAction> {
        if self.browsing_mode != crate::browsing_mode::BrowsingMode::Visitor
            || self.page_holds_sign_in()
        {
            return Vec::new();
        }
        let page = super::is_supported_web_url(&self.current_uri);
        vec![
            ToolbarAction::new("custody", "bot", t("browser-visitor-window"))
                .labelled()
                .with_items(vec![
                    self.bar_entries
                        .entry(
                            "user",
                            t("browser-visitor-open-signed-in"),
                            BarCommand::OwnWindow,
                        )
                        .enabled(page),
                ]),
        ]
    }

    /// The Library in the bar's location: the Library, then its section.
    fn library_location(&self) -> ToolbarLocation {
        let segments: Vec<LocationSegment> = self
            .library_segments()
            .into_iter()
            .map(|(label, value)| LocationSegment {
                label,
                value: value.to_owned(),
            })
            .collect();
        ToolbarLocation {
            label: segments
                .last()
                .map(|segment| segment.label.clone())
                .unwrap_or_default(),
            name: t("browser-library-title"),
            segments,
            ..ToolbarLocation::default()
        }
    }

    /// The page's address in the bar's location.
    fn address_location(&self, icon: Option<String>) -> ToolbarLocation {
        ToolbarLocation {
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
            view_only: false,
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
        let pressed = self.bar_entries.pressed(&event);
        match event {
            // The browser declares no sidebar toggle.
            ToolbarEvent::ToggleSidebar => {}
            ToolbarEvent::Back => self.queue_unit_operation("browser.page", "back"),
            ToolbarEvent::Forward => self.queue_unit_operation("browser.page", "forward"),
            // A Library segment shows its place, never a page.
            ToolbarEvent::Commit { text } if self.show_library => {
                if let Some(resource) = Self::library_section_at(&text) {
                    self.queue_unit_operation(resource, "show");
                }
            }
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
            ToolbarEvent::Action { id } => match pressed {
                Some(BarCommand::Unit(resource, action)) => {
                    self.queue_unit_operation(resource, action);
                }
                Some(BarCommand::Bookmark) => {
                    if self.person_bookmarks.is_some() {
                        self.queue_unit_operation("browser.page", "bookmark-personal-page");
                    } else if let Some(action) = self.page_bookmark_action() {
                        self.submit(action);
                    }
                }
                Some(BarCommand::Library) => {
                    let (resource, action) = self.library_command();
                    self.queue_unit_operation(resource, action);
                }
                Some(BarCommand::DeveloperTools) => self
                    .engine
                    .send(ui::webview::WebViewCommand::ShowDeveloperTools),
                Some(BarCommand::Zoom(step)) => {
                    (self
                        .daily
                        .command(crate::daily_controls::Action::Zoom(step)))();
                }
                Some(BarCommand::Site { information }) => (self.site.open_command(information))(),
                Some(BarCommand::PrivateWindow) => self.private_window_request.set(true),
                // A window of the person's own is an ordinary Browser launch
                // of the page: the desktop's tab beside this one.
                Some(BarCommand::OwnWindow) => {
                    ui::shell_tabs::open_beside(Some(self.current_uri.clone()), false);
                }
                Some(BarCommand::Settings) => {
                    let (resource, action) = self.settings_command();
                    self.queue_unit_operation(resource, action);
                }
                None => {
                    self.extensions.bar_action(&id);
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

    fn names(actions: &[ToolbarAction]) -> Vec<String> {
        actions
            .iter()
            .flat_map(|action| {
                std::iter::once(action.name.clone())
                    .chain(action.items.iter().map(|item| item.name.clone()))
            })
            .collect()
    }

    /// Press the entry named by `key` (a menu's item too) in the bar.
    fn press(widget: &mut BrowserWidget, key: &str) {
        let name = t(key);
        let id = widget
            .window_toolbar()
            .actions
            .into_iter()
            .flat_map(|action| std::iter::once(action.clone()).chain(action.items))
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("the bar offers {name}"))
            .id;
        widget.run_window_toolbar_event(ToolbarEvent::Action { id });
    }

    #[test]
    fn the_bar_carries_every_command_of_the_more_menu() {
        let widget = BrowserWidget::new_headless();
        let declared = widget.window_toolbar();
        let bounded = declared.clone().bounded();
        // Nothing is cut: no action past the bar's limit, no item whose icon
        // the bar does not know.
        assert_eq!(names(&bounded.actions), names(&declared.actions));
        for key in [
            "browser-menu-reader-view",
            "browser-menu-picture-in-picture",
            "browser-menu-developer-tools",
            "browser-menu-zoom-in",
            "browser-menu-zoom-out",
            "browser-menu-site-information",
            "browser-menu-ads-and-trackers",
            "browser-menu-pause-history",
            "browser-menu-new-private-window",
            "browser-menu-settings",
            "browser-menu-manage-extensions",
        ] {
            let name = t(key);
            assert!(names(&bounded.actions).contains(&name), "{name}");
        }
        assert!(names(&bounded.actions).contains(&widget.daily.zoom_label()));
        // The row keeps reload, bookmark and the library.
        let row: Vec<_> = bounded
            .actions
            .iter()
            .filter(|action| !action.menu)
            .take(ui::window_toolbar::MAX_ACTIONS)
            .map(|action| action.name.clone())
            .collect();
        assert_eq!(
            row,
            [
                t("browser-reload"),
                t("browser-bookmark-page"),
                t("browser-library-open")
            ]
        );
    }

    #[test]
    fn a_bar_command_does_what_its_menu_row_does() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let sends = |widget: &mut BrowserWidget, name: &str, command: WebViewCommand| {
            let before = engine.commands().len();
            let id = widget
                .window_toolbar()
                .actions
                .into_iter()
                .flat_map(|action| std::iter::once(action.clone()).chain(action.items))
                .find(|action| action.name == name)
                .unwrap_or_else(|| panic!("the bar offers {name}"))
                .id;
            widget.run_window_toolbar_event(ToolbarEvent::Action { id });
            super::super::tests::layout_at(widget, 1200.0, 800.0);
            let after = engine.commands().split_off(before);
            assert!(after.contains(&command), "{name}: {command:?} in {after:?}");
        };
        sends(
            &mut widget,
            &t("browser-menu-zoom-in"),
            WebViewCommand::SetZoomLevel(1.0),
        );
        let reset = widget.daily.zoom_label();
        sends(&mut widget, &reset, WebViewCommand::SetZoomLevel(0.0));
        sends(
            &mut widget,
            &t("browser-menu-developer-tools"),
            WebViewCommand::ShowDeveloperTools,
        );
        // The rest queue the canonical action the More menu's row queues.
        for (key, resource, action) in [
            ("browser-menu-reader-view", "browser.page", "reader-view"),
            (
                "browser-menu-picture-in-picture",
                "browser.page",
                "picture-in-picture",
            ),
            (
                "browser-menu-pause-history",
                "browser.session",
                "toggle-pause",
            ),
            ("browser-menu-settings", "browser.settings", "show-settings"),
            (
                "browser-menu-manage-extensions",
                "browser.extensions",
                "show-extensions",
            ),
        ] {
            let mut page = BrowserWidget::new_headless();
            press(&mut page, key);
            assert!(
                page.pending_controls.iter().any(|invocation| {
                    invocation.address.resource == resource && invocation.address.action == action
                }),
                "{key} reaches {resource}/{action}"
            );
        }
        let mut page = BrowserWidget::new_headless();
        press(&mut page, "browser-menu-new-private-window");
        assert!(page.private_window_request.get());
    }

    /// On the Library the bar names it and its section, and the section
    /// segment shows that section rather than loading it as an address;
    /// the bar's search narrows the section and closing it clears that.
    #[test]
    fn the_bar_names_the_library_and_its_search_narrows_it() {
        let mut widget = BrowserWidget::new_headless();
        widget.show_library = true;
        widget.library_section = crate::effects::LibrarySection::History;
        let toolbar = widget.window_toolbar();
        let location = toolbar.location.expect("a location");
        assert!(!location.editable, "the Library is no address to type");
        assert_eq!(
            location
                .segments
                .iter()
                .map(|segment| segment.label.as_str())
                .collect::<Vec<_>>(),
            ["Library", "History"]
        );
        let address = widget.current_uri.clone();
        widget.run_window_toolbar_event(ToolbarEvent::Commit {
            text: location.segments[1].value.clone(),
        });
        assert_eq!(widget.current_uri, address, "a segment loads no page");

        assert_eq!(
            ui::widget::InPlace::of(&mut widget),
            ui::widget::InPlace::List,
            "the bar's search narrows the Library in place"
        );
        ui::widget::apply_filter(&mut widget, " rust ");
        assert_eq!(widget.library_filter.get(), "rust");
        assert!(
            widget
                .window_toolbar()
                .actions
                .iter()
                .all(|action| action.icon != "x" || action.name == t("browser-stop")),
            "no Clear filter of its own beside the bar's search"
        );
        ui::widget::apply_filter(&mut widget, "");
        assert_eq!(
            widget.library_filter.get(),
            "",
            "closing the search clears it"
        );
    }
}
