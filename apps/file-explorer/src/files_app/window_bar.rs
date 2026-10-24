//! Files' toolbar as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! While the desktop's bar hosts them, the content header (the nav cluster,
//! the path bar and the view controls) leaves the window and the bar shows:
//! the places list's toggle, back and forward; the folder as the location, its levels as a breadcrumb
//! with drawn chevrons (a click on one goes there) and its path editable,
//! with the folders and files the typed path completes to listed under the
//! field; and the actions, Up, the view and New folder in the row and the
//! rest of the View options in the bar's overflow menu. Outside a Tilo
//! session the header stays in the window, as before.

use std::path::PathBuf;

use ui::window_toolbar::{
    LocationSegment, LocationSuggestion, ToolbarAction, ToolbarEvent, ToolbarLocation,
    ToolbarNavigation, WindowToolbar,
};

use super::FileExplorerWidget;
use crate::command::FileCommand;
use crate::i18n::t;
use crate::model::ViewMode;

/// The action ids Files' slots answer to.
const UP: &str = "up";
const VIEW: &str = "view";
const NEW_FOLDER: &str = "new-folder";
const SORT: &str = "sort";
const NEW_FILE: &str = "new-file";
const HIDDEN: &str = "hidden";
const TERMINAL: &str = "terminal";
const EXTRACT: &str = "extract";
/// What the cloud account's level of the location commits: its top folder.
const CLOUD_ROOT: &str = "tilo-files-cloud-root";
/// What the cloud folder's own level commits: nothing, it is open.
const CLOUD_FOLDER: &str = "tilo-files-cloud-folder";

/// Everything the slots are made from that is cheap to read: the window
/// declares them again, reading the folder's templates and completions,
/// only when one of these changed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a cache key: each bool is one independent fact the bar was built from, compared whole"
)]
pub(super) struct BarKey {
    folder: PathBuf,
    back: bool,
    forward: bool,
    view: ViewMode,
    hidden: bool,
    query: Option<String>,
    sidebar: bool,
    /// The cloud account and folder browsed, which the bar names instead.
    cloud: Option<(String, Option<String>)>,
}

fn action(id: &str, icon: &str, name: String, enabled: bool, on: bool) -> ToolbarAction {
    ToolbarAction {
        id: id.into(),
        icon: icon.into(),
        name,
        enabled,
        on,
        ..ToolbarAction::default()
    }
}

impl FileExplorerWidget {
    /// Move the content header into the window's bar while the bar hosts
    /// the slots, and back into the window when it no longer does. Returns
    /// whether the window's layout changed.
    pub(super) fn place_header(&mut self) -> bool {
        let hosted = ui::window_toolbar::is_hosted();
        if hosted == self.header_in_bar {
            if hosted {
                self.toolbar.set_bar_anchor(self.bar_menu_anchor());
            }
            return false;
        }
        self.header_in_bar = hosted;
        self.nav.set_sidebar_toggle_in_window_bar(hosted);
        if hosted {
            self.nav.set_content_header_leading(None);
            self.nav.set_content_header("", Vec::new());
            self.toolbar.set_bar_anchor(self.bar_menu_anchor());
        } else {
            // The controls come back into the window, built afresh.
            let (path_widget, actions_widget, toolbar) =
                crate::toolbar::FileToolbar::new(self.cmds.clone());
            self.toolbar = toolbar;
            self.nav
                .set_content_header("", vec![Box::new(actions_widget)]);
            // window-bar: fallback, the bar shows the path while it hosts the window
            self.nav
                .set_content_header_leading(Some(Box::new(path_widget)));
            ui::window_toolbar::declare(None);
        }
        true
    }

    /// Where the menus of the bar's actions hang from: the top trailing
    /// corner of the window's content, under the bar.
    fn bar_menu_anchor(&self) -> vello::kurbo::Rect {
        let r = self.rect;
        vello::kurbo::Rect::new(r.x0, r.y0, (r.x1 - 8.0).max(r.x0), r.y0 + 4.0)
    }

    /// The slots this window shows in its bar.
    pub(super) fn window_toolbar(&self) -> WindowToolbar {
        if let Some((account, folder)) = self.cloud_title() {
            return self.cloud_window_toolbar(account, folder);
        }
        let recent = self.is_recent_view();
        let home = self.home_path.as_deref();
        let trail = crate::breadcrumbs::breadcrumb_components(
            &self.current_path,
            home,
            &self.workspace_roots,
        );
        let label = trail
            .last()
            .map(|(label, _)| label.clone())
            .unwrap_or_default();
        let segments = trail
            .iter()
            .map(|(label, path)| LocationSegment {
                label: label.clone(),
                value: crate::model::format_path(path, home),
            })
            .collect();
        let actions = self.bar_actions(recent);
        WindowToolbar {
            sidebar: Some(self.nav.is_sidebar_open()),
            navigation: Some(ToolbarNavigation {
                back: !self.history.is_empty(),
                forward: !self.forward.is_empty(),
            }),
            location: Some(ToolbarLocation {
                label,
                shorter: Vec::new(),
                value: crate::model::format_path(&self.current_path, home),
                placeholder: t("files-path-placeholder"),
                icon: None,
                editable: !recent,
                name: t("files-location"),
                go_name: String::new(),
                segments,
                image: None,
                suggestions: self.bar_suggestions(),
                choices: Vec::new(),
                anchor: None,
                edited: false,
            }),
            actions,
            ..WindowToolbar::default()
        }
    }

    /// The slots while the window browses a cloud account: the account and
    /// the folder open in it as the location's levels. The folder actions
    /// act on folders on this device, so the bar offers none of them here.
    fn cloud_window_toolbar(&self, account: String, folder: Option<String>) -> WindowToolbar {
        let mut segments = vec![LocationSegment {
            label: account.clone(),
            value: CLOUD_ROOT.to_owned(),
        }];
        if let Some(folder) = &folder {
            segments.push(LocationSegment {
                label: folder.clone(),
                value: CLOUD_FOLDER.to_owned(),
            });
        }
        WindowToolbar {
            sidebar: Some(self.nav.is_sidebar_open()),
            location: Some(ToolbarLocation {
                label: folder.unwrap_or(account),
                name: t("files-location"),
                segments,
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }
    }

    /// The bar's actions: up, the view, what the folder offers to make
    /// (or an archive to extract), sort in a grid, the terminal, and
    /// hidden files.
    fn bar_actions(&self, recent: bool) -> Vec<ToolbarAction> {
        let archive = crate::archive::location(&self.current_path).is_some();
        let mut actions = vec![
            action(
                UP,
                "arrow-up",
                t("files-up"),
                self.current_path.parent().is_some() && !recent,
                false,
            ),
            match self.view_mode {
                ViewMode::List => action(VIEW, "layout-grid", t("files-show-as-grid"), true, false),
                ViewMode::Grid => action(VIEW, "list", t("files-show-as-list"), true, false),
            },
        ];
        if archive {
            actions.push(action(
                EXTRACT,
                "download",
                t("files-archive-extract"),
                true,
                false,
            ));
        } else if !recent {
            actions.push(action(
                NEW_FOLDER,
                "folder-plus",
                t("files-menu-new-folder"),
                true,
                false,
            ));
        }
        if self.view_mode == ViewMode::Grid {
            actions.push(action(
                SORT,
                "arrow-up-down",
                t("files-sort-by"),
                true,
                false,
            ));
        }
        if !archive && !recent {
            if !self.available_templates().is_empty() {
                actions.push(action(
                    NEW_FILE,
                    "file-plus",
                    t("files-menu-new-file"),
                    true,
                    false,
                ));
            }
            actions.push(action(
                TERMINAL,
                "square-terminal",
                t("files-menu-open-terminal"),
                true,
                false,
            ));
        }
        actions.push(action(
            HIDDEN,
            "eye",
            if self.show_hidden {
                t("files-menu-hide-hidden")
            } else {
                t("files-menu-show-hidden")
            },
            true,
            self.show_hidden,
        ));
        actions
    }

    /// The folders and files the path typed in the bar completes to.
    fn bar_suggestions(&self) -> Vec<LocationSuggestion> {
        let Some(query) = self.bar_query.as_deref() else {
            return Vec::new();
        };
        let home = self.home_path.as_deref();
        if query == crate::model::format_path(&self.current_path, home) {
            return Vec::new();
        }
        crate::path_bar::path_completions_at(query, home, &self.current_path)
            .into_iter()
            .take(ui::window_toolbar::MAX_SUGGESTIONS)
            .map(|suggestion| LocationSuggestion {
                label: suggestion.label,
                value: suggestion.value,
                icon: suggestion.icon.map(|icon| icon.name().to_owned()),
                detail: String::new(),
            })
            .collect()
    }

    fn bar_key(&self) -> BarKey {
        BarKey {
            folder: self.current_path.clone(),
            back: !self.history.is_empty(),
            forward: !self.forward.is_empty(),
            view: self.view_mode,
            hidden: self.show_hidden,
            query: self.bar_query.clone(),
            sidebar: self.nav.is_sidebar_open(),
            cloud: self.cloud_title(),
        }
    }

    /// Tell the bar the slots, when what they are made from changed.
    fn declare_window_toolbar(&mut self) {
        let key = self.bar_key();
        if self.declared_bar.as_ref() != Some(&key) {
            ui::window_toolbar::declare(Some(self.window_toolbar()));
            self.declared_bar = Some(key);
        }
    }

    /// Tell the bar the slots as they are now and run what the person did
    /// with them: the same commands the header's controls run. Returns
    /// whether anything ran.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        let moved = self.place_header();
        if !self.header_in_bar {
            self.declared_bar = None;
            return moved;
        }
        self.declare_window_toolbar();
        let events = ui::window_toolbar::take_events();
        let ran = !events.is_empty();
        for event in events {
            self.run_window_toolbar_event(event);
        }
        // A changed query or folder changes the slots.
        self.declare_window_toolbar();
        moved || ran
    }

    /// Run one thing the person did in the bar.
    fn run_window_toolbar_event(&mut self, event: ToolbarEvent) {
        let command = match event {
            ToolbarEvent::ToggleSidebar => {
                self.nav.toggle_sidebar();
                None
            }
            ToolbarEvent::Back => Some(FileCommand::Back),
            ToolbarEvent::Forward => Some(FileCommand::Forward),
            ToolbarEvent::Commit { text } if self.cloud.active => {
                // The account's level opens its top folder.
                (text == CLOUD_ROOT).then(|| {
                    FileCommand::Cloud(crate::effects::FilesEffect::BrowseCloud {
                        account: self.cloud.account.clone(),
                        folder: None,
                    })
                })
            }
            ToolbarEvent::Commit { text } => {
                // A typed path, a chosen completion or a breadcrumb level:
                // a folder opens in the window, a file in its app.
                self.bar_query = None;
                let path: PathBuf = crate::path_bar::resolve_location(
                    &text,
                    self.home_path.as_deref(),
                    &self.current_path,
                );
                Some(FileCommand::OpenPath(path))
            }
            ToolbarEvent::Edit { text } => {
                self.bar_query = Some(text);
                None
            }
            ToolbarEvent::Action { id } => match id.as_str() {
                UP => Some(FileCommand::Up),
                VIEW => Some(FileCommand::SetView(match self.view_mode {
                    ViewMode::List => ViewMode::Grid,
                    ViewMode::Grid => ViewMode::List,
                })),
                NEW_FOLDER => Some(FileCommand::NewFolder),
                SORT => Some(FileCommand::OpenSortMenu),
                NEW_FILE => {
                    let at = self.toolbar.view_menu_anchor();
                    self.open_templates_submenu(at);
                    None
                }
                HIDDEN => Some(FileCommand::ToggleHidden),
                TERMINAL => Some(FileCommand::OpenTerminal),
                EXTRACT => Some(FileCommand::ExtractCurrentArchive),
                _ => None,
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
        };
        if let Some(command) = command {
            self.apply_command(command);
        }
        self.structure_dirty = true;
    }
}
