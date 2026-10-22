//! The window around the photo surfaces.
//!
//! A sidebar lists the library's destinations and its albums; a status bar
//! keeps the count or the size of the photograph. The toolbar is the window
//! bar's slots ([`ui::window_toolbar`]): the sidebar's toggle, Back while a
//! photograph or a person is open, the title of what is shown as the
//! window's location and the commands for it; search is the bar's own.
//! Every piece is the shared chrome: the drawer is `SidebarDrawer`, its rows
//! are `chrome::paint_nav_row` and the bar is `chrome::status_bar`. Nothing
//! here picks a colour, a size or a radius.

use automerge::AutoCommit;
use ui::a11y::{AccessNode, AccessState, Politeness, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{Hit, LibraryDestination, PhotosApp, SelectionHit};
use crate::strings::{tr, tr_count, tr_with};

use ui::chrome::{NavCount, NavRowFace, NavSectionFace, SelectionState};
use ui::widget::SidebarDrawer;

/// What a sidebar row opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NavTarget {
    Destination(LibraryDestination),
    Favorites,
    Album(String),
}

impl NavTarget {
    /// The stable semantic route of the row, shared with the library's
    /// accessibility actions.
    pub(super) fn action_target(&self) -> String {
        match self {
            Self::Destination(LibraryDestination::Timeline) => "photos:library:timeline".into(),
            Self::Destination(LibraryDestination::Albums) => "photos:library:albums".into(),
            Self::Destination(LibraryDestination::People) => "photos:library:people".into(),
            Self::Destination(LibraryDestination::Places) => "photos:library:places".into(),
            Self::Destination(LibraryDestination::Analysis) => "photos:library:analysis".into(),
            Self::Favorites => "photos:library-favorites".into(),
            Self::Album(id) => format!("photos:library-album:{id}"),
        }
    }
}

/// One painted sidebar row and where it landed.
#[derive(Clone, Debug)]
pub(super) struct NavEntry {
    pub target: NavTarget,
    pub label: String,
    pub icon: Option<Icon>,
    pub count: Option<usize>,
    pub selected: bool,
    pub rect: Rect,
}

/// One command of the window bar: the window's own [`Hit`] it runs, as the
/// bar names and draws it.
struct Command {
    hit: Hit,
    label: String,
    icon: Icon,
    toggle: Option<bool>,
    enabled: bool,
}

impl Command {
    fn icon(hit: Hit, label: String, icon: Icon) -> Self {
        Self {
            hit,
            label,
            icon,
            toggle: None,
            enabled: true,
        }
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn toggled(mut self, on: bool) -> Self {
        self.toggle = Some(on);
        self
    }

    /// The slot id the bar hands back for this command.
    fn id(&self) -> String {
        format!("{:?}", self.hit)
    }

    /// The command as the window bar's action.
    fn action(&self) -> ToolbarAction {
        ToolbarAction::new(self.id(), self.icon.name(), self.label.clone())
            .enabled(self.enabled)
            .on(self.toggle == Some(true))
    }
}

/// The chrome's own state and the geometry of its last paint.
pub(super) struct Chrome {
    pub drawer: SidebarDrawer,
    /// The trailing panel the album and details editors open in: docked
    /// beside a wide timeline, a floating drawer over a narrow one.
    pub inspector: SidebarDrawer,
    pub nav: Vec<NavEntry>,
    pub nav_hovered: Option<usize>,
    pub sidebar_scroll: f64,
    pub title: String,
    pub status_left: String,
    pub status_right: String,
}

impl Chrome {
    pub(super) fn new() -> Self {
        let base = ui::tokens::Metrics::BASE;
        let inspector_width = f64::from(base.panel_width_list);
        // The inspector docks only where the sidebar, it and a readable
        // timeline all fit; narrower, it floats.
        let inspector_breakpoint =
            f64::from(base.sidebar_width + base.panel_width_list + base.content_min_width);
        Self {
            drawer: SidebarDrawer::new(),
            inspector: SidebarDrawer::new()
                .right()
                .docked_width(inspector_width)
                .breakpoint(inspector_breakpoint)
                .closed(),
            nav: Vec::new(),
            nav_hovered: None,
            sidebar_scroll: 0.0,
            title: String::new(),
            status_left: String::new(),
            status_right: String::new(),
        }
    }
}

/// The header row's height under `theme`: the same row a `NavShell` draws.
pub(super) fn header_height(theme: &Theme) -> f64 {
    ui::chrome::HeaderRowFace::height(theme)
}

/// The inspector's parts: its panel, its header row, its body and the
/// footer its actions stand in.
pub(super) struct InspectorFrame {
    pub panel: Rect,
    pub body: Rect,
    pub footer: Rect,
}

impl PhotosApp {
    /// The library's destinations in sidebar order.
    fn destinations() -> [(LibraryDestination, &'static str, Icon); 5] {
        [
            (LibraryDestination::Timeline, "photos-timeline", Icon::Image),
            (
                LibraryDestination::Albums,
                "photos-albums",
                Icon::GalleryVerticalEnd,
            ),
            (LibraryDestination::People, "photos-people", Icon::Users),
            (LibraryDestination::Places, "photos-places", Icon::Globe),
            (
                LibraryDestination::Analysis,
                "photos-analyze",
                Icon::Sparkles,
            ),
        ]
    }

    /// Whether the timeline shows the whole library, with no album or
    /// metadata filter narrowing it.
    pub(super) fn timeline_unfiltered(&self) -> bool {
        self.active_album.is_none()
            && !self.active_favorites
            && self.active_folder.is_none()
            && self.active_person.is_none()
            && self.active_tag.is_none()
            && self.active_object.is_none()
            && self.active_pet.is_none()
    }

    /// The sidebar rows, in order, before they are placed.
    fn nav_entries(&self) -> Vec<NavEntry> {
        let browsing = self.library_mode;
        let destination = if self.people_mode {
            LibraryDestination::People
        } else {
            self.library_destination
        };
        let mut entries = Vec::new();
        for (index, (target, key, icon)) in Self::destinations().into_iter().enumerate() {
            let selected = browsing
                && destination == target
                && (target != LibraryDestination::Timeline || self.timeline_unfiltered());
            entries.push(NavEntry {
                target: NavTarget::Destination(target),
                label: tr(key),
                icon: Some(icon),
                count: (target == LibraryDestination::Timeline).then_some(self.library_photo_count),
                selected,
                rect: Rect::ZERO,
            });
            if index == 0 {
                entries.push(NavEntry {
                    target: NavTarget::Favorites,
                    label: tr("photos-favorites"),
                    icon: Some(Icon::Star),
                    count: Some(self.library_favorite_count),
                    selected: browsing
                        && destination == LibraryDestination::Timeline
                        && self.active_favorites,
                    rect: Rect::ZERO,
                });
            }
        }
        for (id, name, count) in &self.library_albums {
            entries.push(NavEntry {
                target: NavTarget::Album(id.clone()),
                label: name.clone(),
                icon: None,
                count: Some(*count),
                selected: browsing
                    && destination == LibraryDestination::Timeline
                    && self.active_album.as_deref() == Some(id.as_str()),
                rect: Rect::ZERO,
            });
        }
        entries
    }

    /// The title of what the content column shows.
    pub(super) fn chrome_title(&self) -> String {
        if !self.library_mode {
            return self
                .viewer_details
                .as_ref()
                .map(|details| details.name.clone())
                .unwrap_or_default();
        }
        let destination = if self.people_mode {
            LibraryDestination::People
        } else {
            self.library_destination
        };
        match destination {
            LibraryDestination::Timeline if self.selection_mode => {
                if self.selected_photo_ids.is_empty() {
                    tr("photos-select-photos")
                } else {
                    tr_count("photos-count-selected", self.selected_photo_ids.len())
                }
            }
            LibraryDestination::Timeline => {
                if let Some(id) = &self.active_album {
                    self.library_albums
                        .iter()
                        .find(|(album, _, _)| album == id)
                        .map_or_else(|| tr("photos-timeline"), |(_, name, _)| name.clone())
                } else if self.active_favorites {
                    tr("photos-favorites")
                } else if let Some(label) = self
                    .active_folder
                    .as_ref()
                    .or(self.active_person.as_ref())
                    .or(self.active_tag.as_ref())
                    .or(self.active_object.as_ref())
                    .or(self.active_pet.as_ref())
                {
                    label.clone()
                } else {
                    tr("photos-timeline")
                }
            }
            LibraryDestination::Albums => tr("photos-albums"),
            LibraryDestination::People => match &self.people_selection {
                Some(super::PeopleSelection::Approved(name)) => name.clone(),
                Some(super::PeopleSelection::Suggestion(id)) => self
                    .people_controller
                    .cluster(id)
                    .and_then(|cluster| cluster.display_name.clone())
                    .unwrap_or_else(|| tr("photos-people-suggested")),
                None => tr("photos-people"),
            },
            LibraryDestination::Places => tr("photos-places"),
            LibraryDestination::Analysis => tr("photos-analyze"),
        }
    }

    /// The commands the window bar offers for what is shown. A command that
    /// does not apply is left out rather than shown dead.
    fn commands(&self) -> Vec<Command> {
        // Where the content offers to open a photo itself (an empty
        // library, no photo, an unreadable one), the header does not repeat
        // it.
        let mut commands = if self.content_offers_open() {
            Vec::new()
        } else {
            vec![Command::icon(
                Hit::Open,
                tr("photos-open"),
                Icon::FolderOpen,
            )]
        };
        if !self.library_mode {
            commands.extend(self.viewer_commands());
            return commands;
        }
        let destination = if self.people_mode {
            LibraryDestination::People
        } else {
            self.library_destination
        };
        match destination {
            LibraryDestination::Timeline if self.selection_mode => {
                return self.selection_commands();
            }
            LibraryDestination::Timeline => self.timeline_commands(&mut commands),
            LibraryDestination::Albums => {
                commands.push(Command::icon(
                    Hit::NewAlbum,
                    tr("photos-new-album"),
                    Icon::FolderPlus,
                ));
            }
            // With no located photos there is no map to list or zoom; the
            // empty state is all there is.
            LibraryDestination::Places if self.places_items.is_empty() => {}
            LibraryDestination::Places => {
                commands.push(
                    Command::icon(Hit::PlacesList, tr("photos-places-list"), Icon::List)
                        .toggled(self.places_list_mode),
                );
                if !self.places_list_mode {
                    commands.push(Command::icon(
                        Hit::PlacesZoomOut,
                        tr("photos-zoom-out"),
                        Icon::Minus,
                    ));
                    commands.push(Command::icon(
                        Hit::PlacesZoomIn,
                        tr("photos-zoom-in"),
                        Icon::Plus,
                    ));
                }
            }
            LibraryDestination::People => commands.extend(self.people_commands()),
            LibraryDestination::Analysis => {}
        }
        commands
    }

    /// The timeline's commands after Open: selecting, paging an album
    /// being edited, the thumbnails' size, and editing or making an album.
    fn timeline_commands(&self, commands: &mut Vec<Command>) {
        if self.library_total > 0 {
            commands.push(
                Command::icon(Hit::Select, tr("photos-select"), Icon::ListChecks)
                    .toggled(self.selection_mode),
            );
            if self.album_editor && self.library_total > self.library_page_step() {
                commands.push(
                    Command::icon(
                        Hit::PreviousPage,
                        tr("photos-a11y-previous-photos"),
                        Icon::ChevronLeft,
                    )
                    .enabled(self.can_page_library(false)),
                );
                commands.push(
                    Command::icon(
                        Hit::NextPage,
                        tr("photos-a11y-next-photos"),
                        Icon::ChevronRight,
                    )
                    .enabled(self.can_page_library(true)),
                );
            }
            commands.push(Command::icon(
                Hit::Smaller,
                tr("photos-smaller-thumbnails"),
                Icon::Minus,
            ));
            commands.push(Command::icon(
                Hit::Larger,
                tr("photos-larger-thumbnails"),
                Icon::Plus,
            ));
        }
        if self.active_album.is_some() {
            commands.push(Command::icon(
                Hit::EditAlbum,
                tr("photos-edit-album"),
                Icon::Pencil,
            ));
        } else {
            commands.push(Command::icon(
                Hit::NewAlbum,
                tr("photos-new-album"),
                Icon::FolderPlus,
            ));
        }
    }

    /// The People overview's command: showing the suggestions you hid,
    /// offered once there are some.
    fn people_commands(&self) -> Option<Command> {
        let any_hidden = self
            .people_controller
            .all_clusters()
            .any(|cluster| cluster.hidden || cluster.rejected);
        (self.people_selection.is_none() && (any_hidden || self.people_show_hidden)).then(|| {
            Command::icon(
                Hit::PeopleHidden,
                tr("photos-people-show-hidden"),
                Icon::Eye,
            )
            .toggled(self.people_show_hidden)
        })
    }

    /// Whether the content column shows an empty state whose one action
    /// opens a photo.
    pub(super) fn content_offers_open(&self) -> bool {
        if self.library_mode {
            !self.people_mode
                && self.library_destination == LibraryDestination::Timeline
                && self.library_items.is_empty()
                && self.timeline_unfiltered()
        } else {
            self.viewer.is_none()
                && matches!(
                    self.canvas_state,
                    super::CanvasState::Empty | super::CanvasState::Error
                )
        }
    }

    /// The commands for the open photograph: turn, crop and revert it, then
    /// pass it on.
    fn viewer_commands(&self) -> Vec<Command> {
        let mut commands = Vec::new();
        if !self.has_image() {
            return commands;
        }
        commands.push(Command::icon(
            Hit::RotateLeft,
            tr("photos-command-rotate-left"),
            Icon::RotateCcw,
        ));
        commands.push(Command::icon(
            Hit::RotateRight,
            tr("photos-command-rotate-right"),
            Icon::RotateCw,
        ));
        commands.push(Command::icon(
            Hit::CropToView,
            tr("photos-command-crop"),
            Icon::Scissors,
        ));
        if self
            .rendered
            .as_ref()
            .is_some_and(|rendered| !rendered.edits.is_identity())
        {
            commands.push(Command::icon(Hit::Reset, tr("photos-reset"), Icon::Undo2));
        }
        commands.push(Command::icon(Hit::Copy, tr("photos-copy"), Icon::Copy));
        commands.push(Command::icon(Hit::Share, tr("photos-share"), Icon::Send));
        commands.push(Command::icon(Hit::Save, tr("photos-save"), Icon::Download));
        commands
    }

    /// The commands for photographs chosen in the timeline. Those that need a
    /// choice appear once something is chosen.
    fn selection_commands(&self) -> Vec<Command> {
        let mut commands = vec![Command::icon(
            Hit::Selection(SelectionHit::SelectAll),
            tr("photos-select-all"),
            Icon::ListChecks,
        )];
        if !self.selected_photo_ids.is_empty() {
            commands.push(Command::icon(
                Hit::Selection(SelectionHit::Favorite),
                if self.selection_all_favorite {
                    tr("photos-unfavorite")
                } else {
                    tr("photos-favorite")
                },
                Icon::Star,
            ));
            commands.push(Command::icon(
                Hit::Selection(SelectionHit::EditDetails),
                tr("photos-edit-details"),
                Icon::Pencil,
            ));
            if !self.library_albums.is_empty() {
                let destination = self.selection_destination_album.as_deref().and_then(|id| {
                    self.library_albums
                        .iter()
                        .find(|(album, _, _)| album == id)
                        .map(|(_, name, _)| name.clone())
                });
                commands.push(match destination {
                    Some(name) => Command::icon(
                        Hit::Selection(SelectionHit::AddToAlbum),
                        tr_with("photos-add-to-named-album", "name", &name),
                        Icon::GalleryVerticalEnd,
                    ),
                    None => Command::icon(
                        Hit::Selection(SelectionHit::ChooseAlbum),
                        tr("photos-add-to-album"),
                        Icon::GalleryVerticalEnd,
                    ),
                });
            }
            commands.push(Command::icon(
                Hit::Selection(SelectionHit::Clear),
                tr("photos-clear-selection"),
                Icon::X,
            ));
        }
        // Done leads: it is what ends choosing.
        commands.insert(
            0,
            Command::icon(
                Hit::Selection(SelectionHit::Done),
                tr("photos-done"),
                Icon::Check,
            ),
        );
        commands
    }

    /// The commands the header offers now, in order.
    #[cfg(test)]
    pub(super) fn command_hits(&self) -> Vec<Hit> {
        self.commands()
            .into_iter()
            .map(|command| command.hit)
            .collect()
    }

    /// The window bar's slots for what is shown: the sidebar's toggle, Back
    /// while a photograph or a person is open, the title as the window's
    /// location and the commands. A command whose published action this
    /// window may not run is offered disabled.
    pub(crate) fn window_bar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar {
            sidebar: Some(self.chrome.drawer.is_open()),
            // What is open, else (no photograph) the app's name.
            location: Some(ToolbarLocation {
                label: if self.chrome.title.is_empty() {
                    ui::widget::AppWidget::app_title(self).to_owned()
                } else {
                    self.chrome.title.clone()
                },
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        };
        if self.header_back_visible() {
            toolbar.leading.push(ToolbarAction::new(
                format!("{:?}", Hit::Library),
                "arrow-left",
                tr("photos-library"),
            ));
        }
        toolbar.actions = self
            .commands()
            .iter()
            .map(|command| {
                let mut action = command.action();
                if let (Some(actions), Some(effect)) = (&self.actions, command.hit.effect())
                    && actions.effect(&effect).is_none()
                {
                    action.enabled = false;
                }
                action
            })
            .collect();
        toolbar
    }

    /// Declare the window bar's slots.
    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_bar()));
    }

    /// Run what the person did in the window's bar as the window's own hit,
    /// the path assistive technology's activation takes too.
    pub(super) fn apply_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            let hit = match event {
                ToolbarEvent::ToggleSidebar => {
                    self.chrome.drawer.toggle();
                    continue;
                }
                ToolbarEvent::Action { id } if id == format!("{:?}", Hit::Library) => Hit::Library,
                ToolbarEvent::Action { id } => {
                    let Some(command) = self.commands().into_iter().find(|c| c.id() == id) else {
                        continue;
                    };
                    if !command.enabled {
                        continue;
                    }
                    command.hit
                }
                _ => continue,
            };
            self.pending.set(Some(hit));
            self.drain_pending();
        }
        self.declare_window_toolbar();
    }

    /// Recompute the title and the status bar's readouts from the model.
    pub(super) fn refresh_chrome_text(&mut self, _doc: &AutoCommit) {
        self.chrome.title = self.chrome_title();
        // An empty library's empty state already says so; a "0 photos"
        // readout under it would only repeat it in a band of its own.
        let (left, right) = if self.library_mode && self.library_photo_count == 0 {
            (String::new(), String::new())
        } else if self.library_mode {
            let destination = if self.people_mode {
                LibraryDestination::People
            } else {
                self.library_destination
            };
            match destination {
                LibraryDestination::Timeline => (
                    tr_count("photos-count-photos", self.library_total),
                    String::new(),
                ),
                LibraryDestination::Albums => (
                    tr_count("photos-count-albums", self.library_albums.len()),
                    String::new(),
                ),
                LibraryDestination::People => (
                    tr_count("photos-count-people", self.people_approved.len()),
                    String::new(),
                ),
                LibraryDestination::Places => (
                    tr_count("photos-count-located", self.places_items.len()),
                    String::new(),
                ),
                LibraryDestination::Analysis => (
                    tr_count("photos-count-photos", self.library_photo_count),
                    String::new(),
                ),
            }
        } else if let Some(photo) = self.shown_photo()
            && self.canvas_state == super::CanvasState::Ready
        {
            let edits = photo.edits;
            let (w, h) = photo.image.edited_size(&edits);
            (
                super::dimensions_label(w, h),
                if edits.is_identity() {
                    String::new()
                } else {
                    tr("photos-edited-state")
                },
            )
        } else {
            (String::new(), String::new())
        };
        self.chrome.status_left = self.search_refresh_error.clone().unwrap_or(left);
        self.chrome.status_right = right;
    }

    /// The viewer always offers Library navigation, including before a
    /// photograph is opened. In the library, a selected person offers the
    /// same back control to return to People.
    pub(super) fn header_back_visible(&self) -> bool {
        if self.library_mode {
            self.people_selection.is_some()
                && (self.people_mode || self.library_destination == LibraryDestination::People)
        } else {
            true
        }
    }

    /// Whether the status bar has anything to show.
    pub(super) fn has_status(&self) -> bool {
        !self.chrome.status_left.is_empty() || !self.chrome.status_right.is_empty()
    }

    /// Lay out the sidebar's rows, and declare the window bar's slots for
    /// what this frame shows.
    pub(super) fn place_chrome(&mut self, theme: &Theme) {
        self.place_nav(theme);
        self.declare_window_toolbar();
    }

    /// Place the sidebar rows from the sidebar's top.
    fn place_nav(&mut self, theme: &Theme) {
        let sidebar = self.chrome.drawer.sidebar_rect();
        let mut entries = self.nav_entries();
        let row_h = ui::chrome::nav_row_height(theme);
        let mut y = sidebar.y0 + f64::from(theme.tokens.spacing.s3) - self.chrome.sidebar_scroll;
        let mut albums_started = false;
        for entry in &mut entries {
            if matches!(entry.target, NavTarget::Album(_)) && !albums_started {
                albums_started = true;
                y += ui::chrome::nav_section_height(theme, false);
            }
            entry.rect = Rect::new(sidebar.x0, y, sidebar.x1, y + row_h);
            y += row_h;
        }
        self.chrome.nav = entries;
    }

    /// The rect the albums section header was painted in, if any.
    fn albums_section_rect(&self, theme: &Theme) -> Option<Rect> {
        let first = self
            .chrome
            .nav
            .iter()
            .find(|entry| matches!(entry.target, NavTarget::Album(_)))?;
        let h = ui::chrome::nav_section_height(theme, false);
        Some(Rect::new(
            first.rect.x0,
            first.rect.y0 - h,
            first.rect.x1,
            first.rect.y0,
        ))
    }

    /// Paint the sidebar: its panel (the region docked, a floating drawer
    /// narrow), the destinations and the albums.
    pub(super) fn paint_sidebar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sidebar = self.chrome.drawer.sidebar_rect();
        if sidebar.width() <= 0.0 {
            return;
        }
        let local = self.chrome.drawer.paint_panel(scene, transform, theme);
        let body = sidebar;
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        if let Some(rect) = self.albums_section_rect(&local) {
            let title = tr("photos-albums");
            let face = NavSectionFace::new(rect, &title);
            ui::chrome::paint_nav_section(scene, transform, text, &local, &face);
        }
        for (index, entry) in self.chrome.nav.iter().enumerate() {
            let mut face = NavRowFace::new(entry.rect, &entry.label).state(SelectionState::new(
                entry.selected,
                self.chrome.nav_hovered == Some(index),
                false,
            ));
            if let Some(icon) = entry.icon {
                face = face.icon(icon);
            }
            if let Some(count) = entry.count {
                face = face.count(NavCount::Neutral(u32::try_from(count).unwrap_or(u32::MAX)));
            }
            ui::chrome::paint_nav_row(scene, transform, text, &local, &face);
        }
        scene.pop_layer();
    }

    /// Open the inspector while an editor is open, close it after.
    pub(super) fn sync_inspector(&mut self) {
        let wanted = self.library_mode && (self.album_editor || self.metadata_editor);
        if wanted != self.chrome.inspector.is_open() {
            if wanted {
                self.chrome.inspector.open();
            } else {
                self.chrome.inspector.close();
            }
        }
    }

    /// Paint the inspector's panel and return where its parts are, with the
    /// theme to draw them in; `None` while it is closed.
    pub(super) fn paint_inspector(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        title: &str,
    ) -> Option<(InspectorFrame, Theme)> {
        let panel = self.chrome.inspector.sidebar_rect();
        if panel.width() <= 0.0 || !self.chrome.inspector.is_open() {
            return None;
        }
        let local = self.chrome.inspector.paint_panel(scene, transform, theme);
        let hh = header_height(&local);
        let inset = f64::from(local.tokens.spacing.rhythm.section);
        let header = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + hh);
        let control_h = ui::widget::ControlSize::Regular.height(&local);
        let footer = Rect::new(
            panel.x0 + inset,
            panel.y1 - inset - control_h,
            panel.x1 - inset,
            panel.y1 - inset,
        );
        let body = Rect::new(
            panel.x0 + inset,
            header.y1 + inset,
            panel.x1 - inset,
            (footer.y0 - inset).max(header.y1 + inset),
        );
        ui::widget::toolbar::paint_band(scene, transform, &local, header);
        let mut ctx = PaintCtx::new(scene, transform, text, &local);
        let style = local.text(ui::TextRole::Heading);
        let shown =
            ctx.text
                .elide_to_width_style(title, (header.width() - 2.0 * inset) as f32, &style);
        ctx.text_role_in_band(
            &shown,
            ui::TextRole::Heading,
            local.text_primary,
            header.x0 + inset,
            header,
            None,
        );
        Some((
            InspectorFrame {
                panel,
                body,
                footer,
            },
            local,
        ))
    }

    /// Paint the status bar along the bottom of the content column.
    pub(super) fn paint_status(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.status_rect.height() <= 0.0 {
            return;
        }
        ui::chrome::status_bar::draw(
            scene,
            transform,
            text,
            theme,
            self.status_rect,
            &self.chrome.status_left,
            &self.chrome.status_right,
        );
    }

    /// Open a sidebar row.
    pub(super) fn open_nav_target(&mut self, target: &NavTarget) {
        self.library_mode = true;
        self.notice = None;
        match target {
            NavTarget::Destination(LibraryDestination::Timeline) => {
                self.people_mode = false;
                self.library_destination = LibraryDestination::Timeline;
                self.clear_timeline_filters();
            }
            NavTarget::Destination(LibraryDestination::Albums) => self.show_albums(),
            NavTarget::Destination(LibraryDestination::People) => self.show_people(),
            NavTarget::Destination(LibraryDestination::Places) => {
                self.people_mode = false;
                if self.library_destination != LibraryDestination::Places {
                    self.places_fit_pending = true;
                }
                self.library_destination = LibraryDestination::Places;
            }
            NavTarget::Destination(LibraryDestination::Analysis) => {
                self.people_mode = false;
                self.show_library_analysis();
            }
            NavTarget::Favorites => {
                self.people_mode = false;
                self.library_destination = LibraryDestination::Timeline;
                self.clear_timeline_filters();
                self.active_favorites = true;
            }
            NavTarget::Album(id) => {
                self.people_mode = false;
                self.library_destination = LibraryDestination::Timeline;
                self.clear_timeline_filters();
                self.active_album = Some(id.clone());
            }
        }
        if self.chrome.drawer.is_overlay() {
            self.chrome.drawer.close();
        }
    }

    /// Show the whole library again.
    pub(super) fn clear_timeline_filters(&mut self) {
        self.active_album = None;
        self.active_favorites = false;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
    }

    /// Route an event to the chrome: the sidebar. Returns whether the
    /// chrome took it.
    pub(super) fn handle_chrome_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};

        if let WindowEvent::CursorMoved { .. } = event {
            self.chrome.nav_hovered = if self.chrome.drawer.sidebar_contains(self.pointer) {
                self.chrome
                    .nav
                    .iter()
                    .position(|entry| entry.rect.contains(self.pointer))
            } else {
                None
            };
        }
        let released = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        );
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        // A floating inspector stands over the sidebar: what lands on it is
        // the editor's.
        if self.chrome.inspector.is_open()
            && self.chrome.inspector.is_overlay()
            && self.chrome.inspector.sidebar_contains(self.pointer)
        {
            return false;
        }
        if pressed && self.chrome.drawer.press_dismisses(self.pointer) {
            *ctx.handled = true;
            return true;
        }
        if self.chrome.drawer.sidebar_contains(self.pointer) {
            if let WindowEvent::MouseWheel { delta, .. } = event {
                let pixels = super::wheel_pixels(delta, &self.layout_theme);
                let content = self
                    .chrome
                    .nav
                    .last()
                    .map_or(0.0, |entry| entry.rect.y1 + self.chrome.sidebar_scroll);
                let visible = self.chrome.drawer.sidebar_rect().y1;
                let max = (content - visible).max(0.0);
                self.chrome.sidebar_scroll = (self.chrome.sidebar_scroll + pixels).clamp(0.0, max);
                *ctx.handled = true;
                return true;
            }
            if released {
                if let Some(target) = self
                    .chrome
                    .nav
                    .iter()
                    .find(|entry| entry.rect.contains(self.pointer))
                    .map(|entry| entry.target.clone())
                {
                    self.open_nav_target(&target);
                }
                *ctx.handled = true;
                return true;
            }
            if pressed {
                *ctx.handled = true;
                return true;
            }
        }
        *ctx.handled || self.pending.get().is_some()
    }

    /// The chrome's accessibility: the sidebar's destinations and albums and
    /// the status bar. The toolbar is the window bar's.
    pub(super) fn chrome_semantics(&self) -> Vec<AccessNode> {
        let mut nodes = Vec::new();
        let sidebar = self.chrome.drawer.sidebar_rect();
        if sidebar.width() > 0.0 {
            let rows = self
                .chrome
                .nav
                .iter()
                .map(|entry| {
                    let mut node = AccessNode::new(Role::TreeItem)
                        .with_name(entry.label.clone())
                        .with_bounds(entry.rect)
                        .with_state(AccessState::new().focusable(true).selected(entry.selected))
                        .with_action_target(entry.target.action_target());
                    if let Some(count) = entry.count {
                        node = node.with_description(count.to_string());
                    }
                    node
                })
                .collect::<Vec<_>>();
            nodes.push(
                AccessNode::new(Role::Tree)
                    .with_name(tr("photos-library"))
                    .with_bounds(sidebar)
                    .with_hashed_key("photos:sidebar")
                    .with_children(rows),
            );
        }
        if self.has_status() {
            nodes.push(
                AccessNode::new(Role::StaticText)
                    .with_name(
                        [&self.chrome.status_left, &self.chrome.status_right]
                            .into_iter()
                            .filter(|part| !part.is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                    .with_bounds(self.status_rect)
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:status"),
            );
        }
        nodes
    }
}
