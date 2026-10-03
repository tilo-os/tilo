//! The toolbar row: navigation, the address field, the commands a person uses
//! on every page, and one overflow menu for the rest.
//!
//! The row keeps the primary commands as icon buttons. Everything else (find,
//! zoom, Picture in Picture, history pause, developer tools, protection,
//! private window, settings) is a row of the trailing [`Toolbar`], which is
//! one control wide, so those commands always sit in its overflow menu. In a
//! compact window the site information, bookmark and library commands join
//! them: nothing is ever hidden, only moved.

use ui::icons::Icon;
use ui::theme::Theme;
use ui::webview::WebViewCommand;
use ui::widget::toolbar::Orientation;
use ui::widget::{Button, Container, Row, TextInput, TextInputKind, Toolbar, Widget};

use super::{BrowserWidget, URL_INPUT_ID};

/// The overflow menu's control, the anchor of the transient notes that point
/// at the commands inside it.
pub(super) const OVERFLOW_ID: &str = "browser.overflow";

/// A resource action a menu row or a row hover action asks for. The widget
/// drains the queue in `sync`, so the request travels the same canonical
/// action path as the keyboard shortcut for it.
pub(super) type ResourceCommand = (String, String);

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
            ("browser", "show-page")
        } else {
            ("browser.history", "show")
        }
    }

    /// The resource and action a press of the settings row performs.
    fn settings_command(&self) -> (&'static str, &'static str) {
        if self.show_settings {
            ("browser", "show-page")
        } else {
            ("browser.settings", "show")
        }
    }

    /// The bookmark command: the hosted document edit, or the canonical
    /// action when the widget stands alone.
    fn bookmark_command(&self) -> Box<dyn FnMut()> {
        if self.is_framework_hosted() {
            let action = self.page_bookmark_action();
            let edits = self.pending_library_edits.clone();
            Box::new(move || {
                if let Some(action) = action.clone() {
                    edits.update(|queue| queue.push(action));
                }
            })
        } else {
            Box::new(self.queue_command("browser.page", "bookmark"))
        }
    }

    /// The overflow menu: a [`Toolbar`] exactly one control wide, so every
    /// command in it is a row of the "More commands" menu.
    fn overflow_menu(&self) -> Toolbar {
        let mut menu = Toolbar::new(Orientation::Horizontal);
        if self.compact_chrome {
            // What the narrow row has no room for.
            menu = menu
                .push_button("Site information", self.site.open_command(true))
                .push_button(
                    if self.bookmarks.contains(&self.current_uri) {
                        "Page bookmarked"
                    } else {
                        "Bookmark this page"
                    },
                    self.bookmark_command(),
                );
            let (resource, action) = self.library_command();
            menu = menu
                .push_button(
                    "History, bookmarks and downloads",
                    self.queue_command(resource, action),
                )
                .push_separator();
        }
        let developer_engine = self.engine.clone();
        let (settings_resource, settings_action) = self.settings_command();
        menu.push_toggle(
            "Picture in Picture",
            self.active_pip,
            self.queue_command("browser.page", "picture-in-picture"),
        )
        .push_toggle(
            "Pause history",
            self.history_paused(),
            self.queue_command("browser.session", "toggle-history-pause"),
        )
        .push_button(
            "Find in page",
            self.daily.command(crate::daily_controls::Action::Open),
        )
        .push_button("Developer tools", move || {
            developer_engine.send(WebViewCommand::ShowDeveloperTools);
        })
        .push_separator()
        .push_button(
            "Zoom in",
            self.daily.command(crate::daily_controls::Action::Zoom(1)),
        )
        .push_button(
            "Zoom out",
            self.daily.command(crate::daily_controls::Action::Zoom(-1)),
        )
        .push_button(
            &self.daily.zoom_label(),
            self.daily.command(crate::daily_controls::Action::Zoom(0)),
        )
        .push_separator()
        .push_button("Ads and trackers", self.site.open_command(false))
        .push_button("New private window", {
            let request = self.private_window_request.clone();
            move || request.set(true)
        })
        .push_button(
            if self.show_settings {
                "Close settings"
            } else {
                "Settings"
            },
            self.queue_command(settings_resource, settings_action),
        )
    }

    /// The whole toolbar row.
    pub(super) fn build_toolbar(&self, theme: &Theme) -> Box<dyn Widget> {
        let back = self.bind_button(
            Button::icon_only(
                "browser.back",
                Icon::ArrowLeft,
                "Back; hold or right-click for history",
            )
            .tooltip("Back (Alt+Left); hold or right-click for history")
            .with_enabled(self.can_go_back),
            "browser.page",
            "back",
        );
        let forward = self.bind_button(
            Button::icon_only(
                "browser.forward",
                Icon::ArrowRight,
                "Forward; hold or right-click for history",
            )
            .tooltip("Forward (Alt+Right); hold or right-click for history")
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
                if loading { "Stop loading" } else { "Reload" },
            )
            .tooltip(if loading {
                "Stop loading"
            } else {
                "Reload (Ctrl+R)"
            }),
            "browser.page",
            if loading { "stop" } else { "reload" },
        );
        let address = TextInput::from_state(&self.address)
            .with_kind(TextInputKind::Url)
            .with_id(URL_INPUT_ID)
            .with_accessible_name("Address and search")
            .with_placeholder("Search or enter address");

        let mut row = Row::new()
            .no_wrap()
            .with_id("browser.toolbar")
            .with_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s1)
            .push(Box::new(back));
        row = row.push(Box::new(forward)).push(Box::new(reload));
        if !self.compact_chrome {
            row = row.push(Box::new(self.site.launcher()));
        }
        row = row.push(Box::new(address));
        if !self.compact_chrome {
            let bookmarked = self.bookmarks.contains(&self.current_uri);
            let label = if bookmarked {
                "Page bookmarked"
            } else {
                "Bookmark this page"
            };
            let hosted = self
                .is_framework_hosted()
                .then(|| self.page_bookmark_action())
                .flatten();
            row = row.push(Box::new(
                self.route_document_button(
                    Button::icon_only("browser.bookmark", Icon::Star, label)
                        .with_enabled(!self.current_uri.trim().is_empty()),
                    "browser.page",
                    "bookmark",
                    hosted,
                ),
            ));
            let (resource, action) = self.library_command();
            row = row.push(Box::new(
                self.bind_button(
                    Button::icon_only(
                        "browser.library",
                        Icon::History,
                        if self.show_library {
                            "Close browser library"
                        } else {
                            "Open browser library"
                        },
                    )
                    .tooltip("History, bookmarks and downloads"),
                    resource,
                    action,
                ),
            ));
        }
        if let Some(status) = self.capabilities.toolbar_status() {
            let button = if self.compact_chrome {
                Button::icon_only("browser.download-status", Icon::Download, &status)
            } else {
                Button::new("browser.download-status", &status, None)
                    .with_icon(Icon::Download)
                    .with_accessible_name(&format!("Downloads: {status}"))
                    .tooltip("Show downloads")
            };
            row = row.push(Box::new(self.bind_button(
                button,
                "browser.downloads",
                "show",
            )));
        }
        row = row.push(Box::new(
            Container::new()
                .with_id(OVERFLOW_ID)
                .with_no_padding()
                .with_width(theme.tokens.metrics.min_pointer_target)
                .with_child(Box::new(self.overflow_menu())),
        ));
        Box::new(row)
    }
}
