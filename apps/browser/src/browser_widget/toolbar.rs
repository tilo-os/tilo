//! The toolbar row: navigation, the address field, the commands a person uses
//! on every page, and one overflow menu for the rest.
//!
//! The row keeps the primary commands as icon buttons. Everything else (find,
//! zoom, Picture in Picture, history pause, developer tools, protection,
//! private window, settings) is a row of the trailing More button's menu
//! ([`MenuButton`]). In a compact window the site information, bookmark and
//! library commands join them: nothing is ever hidden, only moved.

use ui::prelude::Resource;

use ui::icons::Icon;
use ui::theme::Theme;
use ui::webview::WebViewCommand;
use ui::widget::floating_surface::PanelAlign;
use ui::widget::{
    Button, Chip, ChipLead, MenuButton, MenuItem, Row, TextInput, TextInputKind, Widget,
};

use super::{BrowserWidget, URL_INPUT_ID};

/// The overflow menu's control, the anchor of the transient notes that point
/// at the commands inside it.
pub(super) const OVERFLOW_ID: &str = "browser.overflow";

/// A resource action a menu row or a row hover action asks for. The widget
/// drains the queue in `sync`, so the request travels the same canonical
/// action path as the keyboard shortcut for it.
pub(super) type ResourceCommand = (String, String);

/// The toolbar's and its menu's copy, from the browser's catalogue.
fn t(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

impl BrowserWidget {
    /// A closure that queues `action` of `resource` for the next `sync`.
    pub(super) fn queue_command(&self, resource: &str, action: &str) -> impl FnMut() + 'static {
        let queue = self.pending_commands.clone();
        let command = (resource.to_owned(), action.to_owned());
        move || queue.update(|commands| commands.push(command.clone()))
    }

    /// Run what menu rows and row actions queued since the last sync.
    pub(super) fn run_pending_commands(&mut self) -> bool {
        let commands = self.pending_commands.with(|queue| !queue.is_empty());
        if !commands {
            return false;
        }
        let mut taken = Vec::new();
        self.pending_commands
            .update(|queue| taken = std::mem::take(queue));
        for (resource, action) in taken {
            self.queue_unit_operation(&resource, &action);
        }
        true
    }

    /// The resource and action a press of the library button performs.
    fn library_command(&self) -> (&'static str, &'static str) {
        if self.show_library {
            (crate::resource::Browser::KIND, "show-page")
        } else {
            ("browser.history", "show-history")
        }
    }

    /// The resource and action a press of the settings row performs.
    fn settings_command(&self) -> (&'static str, &'static str) {
        if self.show_settings {
            (crate::resource::Browser::KIND, "show-page")
        } else {
            ("browser.settings", "show-settings")
        }
    }

    /// The bookmark command: the hosted document edit, or the canonical
    /// action when the widget stands alone.
    fn bookmark_command(&self) -> Box<dyn FnMut()> {
        if self.is_framework_hosted() {
            let action = self.page_bookmark_action();
            let dispatcher = self.dispatch.clone();
            Box::new(move || {
                if let (Some(dispatcher), Some(action)) = (&dispatcher, action.clone()) {
                    dispatcher.submit(action);
                }
            })
        } else {
            Box::new(self.queue_command("browser.page", "bookmark"))
        }
    }

    /// The overflow menu: a More button whose menu holds every command the
    /// row has no icon for.
    fn overflow_menu(&self) -> MenuButton {
        // A menu row runs the same `FnMut` command a toolbar button would.
        fn row(label: &str, command: impl FnMut() + 'static) -> MenuItem {
            let command = std::cell::RefCell::new(command);
            MenuItem::action(label, move || (command.borrow_mut())())
        }
        let mut items = Vec::new();
        if self.compact_chrome {
            // What the narrow row has no room for.
            items.push(row(
                &t("browser-menu-site-information"),
                self.site.open_command(true),
            ));
            items.push(row(
                &t(if self.bookmarks.contains(&self.current_uri) {
                    "browser-page-bookmarked"
                } else {
                    "browser-bookmark-page"
                }),
                self.bookmark_command(),
            ));
            let (resource, action) = self.library_command();
            items.push(row(
                &t("browser-menu-library"),
                self.queue_command(resource, action),
            ));
            items.push(MenuItem::separator());
        }
        let developer_engine = self.engine.clone();
        let (settings_resource, settings_action) = self.settings_command();
        items.extend([
            row(
                &t("browser-menu-picture-in-picture"),
                self.queue_command("browser.page", "picture-in-picture"),
            )
            .with_checked(self.active_pip),
            row(
                &t("browser-menu-reader-view"),
                self.queue_command("browser.page", "reader-view"),
            )
            .with_checked(self.reader_view),
            row(
                &t("browser-menu-pause-history"),
                self.queue_command("browser.session", "toggle-pause"),
            )
            .with_checked(self.history_paused()),
            row(
                &t("browser-menu-find"),
                self.daily.command(crate::daily_controls::Action::Open),
            ),
            row(&t("browser-menu-developer-tools"), move || {
                developer_engine.send(WebViewCommand::ShowDeveloperTools);
            }),
            MenuItem::separator(),
            row(
                &t("browser-menu-zoom-in"),
                self.daily.command(crate::daily_controls::Action::Zoom(1)),
            ),
            row(
                &t("browser-menu-zoom-out"),
                self.daily.command(crate::daily_controls::Action::Zoom(-1)),
            ),
            row(
                &self.daily.zoom_label(),
                self.daily.command(crate::daily_controls::Action::Zoom(0)),
            ),
            MenuItem::separator(),
            row(
                &t("browser-menu-ads-and-trackers"),
                self.site.open_command(false),
            ),
            row(&t("browser-menu-new-private-window"), {
                let request = self.private_window_request.clone();
                move || request.set(true)
            }),
            row(
                &t(if self.show_settings {
                    "browser-menu-close-settings"
                } else {
                    "browser-menu-settings"
                }),
                self.queue_command(settings_resource, settings_action),
            ),
        ]);
        let more = t("browser-more-commands");
        Button::icon_only(OVERFLOW_ID, Icon::MoreHorizontal, &more)
            .tooltip(&more)
            .with_menu(items)
            .aligned(PanelAlign::Trailing)
    }

    /// The whole toolbar row.
    pub(super) fn build_toolbar(&self, theme: &Theme) -> Box<dyn Widget> {
        let back = self.bind_button(
            Button::icon_only("browser.back", Icon::ArrowLeft, &t("browser-back"))
                .tooltip(&t("browser-back-tooltip"))
                .with_enabled(self.can_go_back),
            "browser.page",
            "back",
        );
        let forward = self.bind_button(
            Button::icon_only("browser.forward", Icon::ArrowRight, &t("browser-forward"))
                .tooltip(&t("browser-forward-tooltip"))
                .with_enabled(self.can_go_forward),
            "browser.page",
            "forward",
        );
        let loading = self.progress < 1.0;
        let reload = self.bind_button(
            Button::icon_only(
                if loading {
                    "browser.stop"
                } else {
                    "browser.reload"
                },
                if loading { Icon::X } else { Icon::Refresh },
                &t(if loading {
                    "browser-stop"
                } else {
                    "browser-reload"
                }),
            )
            .tooltip(&t(if loading {
                "browser-stop"
            } else {
                "browser-reload-tooltip"
            })),
            "browser.page",
            if loading { "stop" } else { "reload" },
        );
        let address = TextInput::from_state(&self.address)
            .with_kind(TextInputKind::Url)
            .with_id(URL_INPUT_ID)
            .with_accessible_name(t("browser-address"))
            .with_placeholder(t("browser-address-placeholder"));

        let mut row = Row::new()
            .no_wrap()
            .with_id("browser.toolbar")
            .with_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s1)
            .push(Box::new(back));
        row = row.push(Box::new(forward)).push(Box::new(reload));
        if !self.compact_chrome {
            row = row.push(Box::new(self.site.launcher()));
        }
        if self.browsing_mode.is_private() {
            // With the toolbar's tint (`draw`), what marks a private
            // window, in compact chrome too; its tooltip says what that
            // means.
            row = row.push(Box::new(
                Chip::new(t("browser-private"))
                    .with_id("browser.private-indicator")
                    .with_lead(ChipLead::Glyph(Icon::EyeOff))
                    .with_accessible_name(t("browser-private-window"))
                    .tooltip(t("browser-private-tooltip")),
            ));
        }
        row = row.push(Box::new(address));
        if !self.compact_chrome {
            let bookmarked = if self.person_bookmarks.is_some() {
                self.personal_bookmarks
                    .bookmarks
                    .contains(&self.current_uri)
            } else {
                self.bookmarks.contains(&self.current_uri)
            };
            let label = t(if bookmarked {
                "browser-page-bookmarked"
            } else {
                "browser-bookmark-page"
            });
            let label = label.as_str();
            let hosted = self
                .is_framework_hosted()
                .then(|| self.page_bookmark_action())
                .flatten();
            if self.person_bookmarks.is_some() {
                row = row.push(Box::new(
                    self.bind_button(
                        Button::icon_only("browser.bookmark", Icon::Star, label).with_enabled(
                            !self.current_uri.trim().is_empty()
                                && !self
                                    .personal_bookmarks
                                    .bookmarks
                                    .contains(&self.current_uri),
                        ),
                        "browser.page",
                        "bookmark-personal-page",
                    ),
                ));
            } else {
                row = row.push(Box::new(
                    self.route_document_button(
                        Button::icon_only("browser.bookmark", Icon::Star, label)
                            .with_enabled(!self.current_uri.trim().is_empty()),
                        "browser.page",
                        "bookmark",
                        hosted,
                    ),
                ));
            }
            let (resource, action) = self.library_command();
            row = row.push(Box::new(
                self.bind_button(
                    Button::icon_only(
                        "browser.library",
                        Icon::History,
                        &t(if self.show_library {
                            "browser-library-close"
                        } else {
                            "browser-library-open"
                        }),
                    )
                    .tooltip(&t("browser-menu-library")),
                    resource,
                    action,
                ),
            ));
        }
        if let Some(status) = self.capabilities.toolbar_status() {
            // Like every other command in the row, an icon; the state it
            // reports is its name and tooltip.
            let button = Button::icon_only("browser.download-status", Icon::Download, &status)
                .tooltip(&status);
            row = row.push(Box::new(self.bind_button(
                button,
                "browser.downloads",
                "show-downloads",
            )));
        }
        for button in self.extensions.toolbar_buttons() {
            row = row.push(button);
        }
        row = row.push(Box::new(self.overflow_menu()));
        Box::new(row)
    }
}
