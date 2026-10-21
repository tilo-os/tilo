//! The library: a sheet over the video with the queue, recent videos and
//! shared playlists and bookmarks.
//!
//! The sheet is the toolkit's leading drawer, so it slides in over a scrim,
//! is floating material and dismisses on the scrim, `Escape` or Close. Its
//! rows are list rows: choosing a row plays it, and the actions a row offers
//! show on hover and focus.

use super::{
    Affine, Button, ButtonFace, Icon, LibraryRowHit, LibraryTab, LocalMediaEntry, PlayerApp, Point,
    Rect, Scene, SharedRowHit, TextRenderer, Theme,
};
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::vello::peniko::{BlendMode, Fill};
use ui::widget::{
    ButtonRole, ControlSize, EmptyState, EmptyStateContent, Interaction, ListRowFace, RowAction,
    SegmentedFace, ToggleGroup,
};

/// The remove action of a queued video.
const REMOVE_FROM_QUEUE: RowAction<'static> = RowAction {
    icon: Icon::X,
    label: "Remove from queue",
};
/// The delete action of a shared playlist or bookmark.
const DELETE_SHARED: RowAction<'static> = RowAction {
    icon: Icon::Trash2,
    label: "Delete",
};
/// The jump action of a bookmark on the video that is open.
const JUMP_TO_BOOKMARK: RowAction<'static> = RowAction {
    icon: Icon::Play,
    label: "Jump to bookmark",
};

impl LibraryTab {
    const LABELS: [&'static str; 3] = ["Queue", "Recent", "Shared"];

    fn index(self) -> usize {
        match self {
            Self::Queue => 0,
            Self::Recent => 1,
            Self::Shared => 2,
        }
    }
}

/// One row of a list in the sheet.
struct RowSpec<'a> {
    title: &'a str,
    meta: Option<&'a str>,
    selected: bool,
    focused: bool,
    actions: Vec<RowAction<'static>>,
}

/// Where a row and its actions landed.
struct RowPlaced {
    row: Rect,
    actions: Vec<Rect>,
}

/// Paint `specs` as one list from `*y` down in `x0..x1`, clipped to `clip`,
/// advancing `*y` past the last row.
fn paint_rows(
    ctx: &mut PaintCtx<'_>,
    specs: &[RowSpec<'_>],
    (x0, x1): (f64, f64),
    y: &mut f64,
    clip: Rect,
    cursor: Point,
) -> Vec<RowPlaced> {
    let mut placed = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let height = ListRowFace::new(spec.title).height(ctx.text, ctx.theme);
        let row = Rect::new(x0, *y, x1, *y + height);
        let probe = ListRowFace::new(spec.title)
            .actions(&spec.actions)
            .interaction(Interaction {
                hovered: true,
                ..Interaction::default()
            });
        let actions = probe.action_rects(row, ctx.theme);
        let visible = row.y1 > clip.y0 && row.y0 < clip.y1;
        let hovered = visible && clip.contains(cursor) && row.contains(cursor);
        if visible {
            let hovered_action = hovered
                .then(|| actions.iter().position(|rect| rect.contains(cursor)))
                .flatten();
            let mut face = ListRowFace::new(spec.title)
                .selected(spec.selected)
                .actions(&spec.actions)
                .hovered_action(hovered_action)
                .interaction(Interaction {
                    hovered,
                    focused: spec.focused,
                    ..Interaction::default()
                });
            if let Some(meta) = spec.meta {
                face = face.meta(meta);
            }
            if let Some(next) = specs.get(index + 1) {
                face = face.separator(next.selected);
            }
            face.paint(ctx, row);
        }
        placed.push(RowPlaced { row, actions });
        *y += height;
    }
    placed
}

impl PlayerApp {
    /// Follow `library_visible` with the sheet. The resource action is what
    /// changes the flag; the drawer only animates it.
    pub(super) fn sync_library_sheet(&mut self) {
        if self.library_visible != self.drawer.is_open() {
            if self.library_visible {
                self.drawer.open();
            } else {
                self.drawer.close();
            }
        }
    }

    fn clear_library_rects(&mut self) {
        self.library_row_hits.clear();
        self.shared_row_hits.clear();
        self.library_close_rect = Rect::ZERO;
        self.library_clear_rect = Rect::ZERO;
        self.library_queue_tab_rect = Rect::ZERO;
        self.library_recent_tab_rect = Rect::ZERO;
        self.library_shared_tab_rect = Rect::ZERO;
        self.library_list_rect = Rect::ZERO;
        self.shared_watch_later_rect = Rect::ZERO;
        self.shared_bookmark_rect = Rect::ZERO;
    }

    /// Paint the scrim and the sheet, and record every rect the pointer,
    /// keyboard and assistive technology act on.
    pub(super) fn paint_library_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.clear_library_rects();
        let sheet = self.drawer.sidebar_rect();
        if sheet.width() <= 0.0 || (!self.library_visible && self.drawer.reveal() <= 0.0) {
            return;
        }
        let panel_theme = self.drawer.paint_panel(scene, transform, theme);
        let theme = &panel_theme;
        let spacing = &theme.tokens.spacing;
        let (s1, s4) = (f64::from(spacing.s1), f64::from(spacing.s4));
        let sibling = f64::from(spacing.rhythm.sibling);
        let inner = Rect::new(sheet.x0 + s4, sheet.y0 + s4, sheet.x1 - s4, sheet.y1 - s4);
        let cursor = self.last_cursor;

        // Header: the title, Clear on Recent, Close.
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let header = Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + control);
        self.library_close_rect = Rect::new(header.x1 - control, header.y0, header.x1, header.y1);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(self.library_close_rect, Icon::X, "Close library")
                .hovered(self.library_close_rect.contains(cursor))
                .focused(self.library_focus_id.as_deref() == Some("library-close")),
        );
        let mut title_end = self.library_close_rect.x0 - s1;
        if self.library_tab == LibraryTab::Recent && !self.recent.is_empty() {
            let width = ButtonFace::new(Rect::ZERO, "Clear")
                .role(ButtonRole::Plain)
                .intrinsic_width(text, theme);
            self.library_clear_rect = Rect::new(title_end - width, header.y0, title_end, header.y1);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(self.library_clear_rect, "Clear")
                    .role(ButtonRole::Plain)
                    .hovered(self.library_clear_rect.contains(cursor))
                    .focused(self.library_focus_id.as_deref() == Some("library-clear")),
            );
            title_end = self.library_clear_rect.x0 - s1;
        }
        PaintCtx::new(scene, transform, text, theme).text_role_in_band(
            "Library",
            TextRole::Heading,
            theme.tokens.colors.text_primary,
            header.x0,
            header,
            Some(ui::num::f32_of_f64((title_end - header.x0).max(0.0))),
        );

        // Tabs: one strip across the sheet.
        let tab_h = SegmentedFace::height(ControlSize::Regular, theme);
        let tabs_y0 = header.y1 + sibling;
        let third = inner.width() / 3.0;
        let tab_rects: [Rect; 3] = std::array::from_fn(|i| {
            #[allow(clippy::cast_precision_loss, reason = "i is at most 3")]
            let edge = |n: usize| inner.x0 + third * n as f64;
            Rect::new(edge(i), tabs_y0, edge(i + 1), tabs_y0 + tab_h)
        });
        [
            self.library_queue_tab_rect,
            self.library_recent_tab_rect,
            self.library_shared_tab_rect,
        ] = tab_rects;
        let hovered_tab = tab_rects.iter().position(|rect| rect.contains(cursor));
        let tab_focus = self
            .library_focus_id
            .as_deref()
            .is_some_and(|id| id.starts_with("library-tab-"));
        ToggleGroup::paint(
            scene,
            transform,
            text,
            theme,
            &SegmentedFace::new(&tab_rects, &LibraryTab::LABELS, self.library_tab.index())
                .hovered(hovered_tab)
                .focused(tab_focus),
        );

        // The list under the tabs.
        let list_top = tabs_y0 + tab_h + sibling;
        let list = Rect::new(
            sheet.x0 + s1,
            list_top,
            sheet.x1 - s1,
            (sheet.y1 - s4).max(list_top),
        );
        self.library_list_rect = list;
        match self.library_tab {
            LibraryTab::Shared => self.paint_shared_library(scene, transform, text, theme, list),
            tab => self.paint_local_library(scene, transform, text, theme, list, tab),
        }
    }

    /// The queue or the recent list.
    fn paint_local_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
        tab: LibraryTab,
    ) {
        let entries: Vec<LocalMediaEntry> = match tab {
            LibraryTab::Queue => self.queue.clone(),
            _ => self.recent.iter().cloned().collect(),
        };
        let cursor = self.last_cursor;
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        if entries.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::ListVideo),
                ..EmptyStateContent::new(if tab == LibraryTab::Queue {
                    "Your queue is empty"
                } else {
                    "No recent videos"
                })
            };
            let _ = EmptyState::paint(&mut ctx, list, &content, cursor);
            return;
        }
        let shown = entries.len().min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let entries = entries.get(..shown).unwrap_or(&entries);
        let specs: Vec<RowSpec<'_>> = entries
            .iter()
            .map(|entry| {
                let active = self.current_queue_id.as_deref() == Some(entry.id.as_str());
                let focus = self.library_focus_id.as_deref();
                let remove_focus = format!("library-remove-{}", entry.id);
                RowSpec {
                    title: &entry.title,
                    meta: active.then_some("Playing"),
                    selected: active,
                    focused: focus == Some(entry.id.as_str())
                        || focus == Some(remove_focus.as_str()),
                    actions: if tab == LibraryTab::Queue {
                        vec![REMOVE_FROM_QUEUE]
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect();
        let scroll = self.library_scroll.max(0.0);
        let mut y = list.y0 - scroll;
        let top = y;
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &list);
        let placed = paint_rows(&mut ctx, &specs, (list.x0, list.x1), &mut y, list, cursor);
        ctx.scene.pop_layer();
        self.library_max_scroll = ((y - top) - list.height()).max(0.0);
        self.library_scroll = scroll.min(self.library_max_scroll);
        for (entry, placed) in entries.iter().zip(placed) {
            self.library_row_hits.push(LibraryRowHit {
                id: entry.id.clone(),
                row: placed.row,
                play: placed.row,
                remove: (tab == LibraryTab::Queue)
                    .then(|| placed.actions.first().copied())
                    .flatten(),
            });
        }
    }

    /// The shared tab: the two actions that add to the shared library, then
    /// its playlists and bookmarks.
    #[allow(
        clippy::too_many_lines,
        reason = "one tab: its two actions, its two lists and the hit rects they publish"
    )]
    fn paint_shared_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) {
        let spacing = &theme.tokens.spacing;
        let (s2, s3, s4) = (
            f64::from(spacing.s2),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
        );
        let cursor = self.last_cursor;
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let (inner_x0, inner_x1) = (list.x0 + s3, list.x1 - s3);
        let half = (inner_x1 - inner_x0 - s2) / 2.0;
        let has_media = self.player.has_media();
        self.shared_watch_later_rect =
            Rect::new(inner_x0, list.y0, inner_x0 + half, list.y0 + control);
        self.shared_bookmark_rect =
            Rect::new(inner_x1 - half, list.y0, inner_x1, list.y0 + control);
        for (rect, label, focus) in [
            (
                self.shared_watch_later_rect,
                "Watch later",
                "shared-watch-later",
            ),
            (
                self.shared_bookmark_rect,
                "Bookmark time",
                "shared-bookmark",
            ),
        ] {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(rect, label)
                    .disabled(!has_media)
                    .hovered(rect.contains(cursor))
                    .focused(self.library_focus_id.as_deref() == Some(focus)),
            );
        }
        let region = Rect::new(
            list.x0,
            (list.y0 + control + s4).min(list.y1),
            list.x1,
            list.y1,
        );
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let library = &self.shared_library;
        if library.playlists.is_empty() && library.annotations.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::ListVideo),
                message: Some(
                    "Everyone in this workspace sees the playlists and bookmarks added here.",
                ),
                ..EmptyStateContent::new("No shared playlists or bookmarks yet")
            };
            let _ = EmptyState::paint(&mut ctx, region, &content, cursor);
            return;
        }
        let limit = crate::actions::CONTEXTUAL_CHILD_LIMIT;
        let mut playlists: Vec<_> = library.playlists.iter().collect();
        playlists.sort_by(|(a_id, a), (b_id, b)| {
            a.name
                .cmp(&b.name)
                .then_with(|| a_id.as_str().cmp(b_id.as_str()))
        });
        playlists.truncate(limit);
        let mut annotations = library.annotations_in_order();
        annotations.truncate(limit);
        let focus = self.library_focus_id.as_deref();
        let playlist_specs: Vec<RowSpec<'_>> = playlists
            .iter()
            .map(|(id, playlist)| RowSpec {
                title: &playlist.name,
                meta: None,
                selected: false,
                focused: focus == Some(format!("shared-delete-{id}").as_str()),
                actions: vec![DELETE_SHARED],
            })
            .collect();
        let jumpable: Vec<bool> = annotations
            .iter()
            .map(|(_, annotation)| {
                has_media
                    && self.current_media_ref.as_deref() == Some(annotation.media_ref.as_str())
            })
            .collect();
        let times: Vec<String> = annotations
            .iter()
            .map(|(_, annotation)| ui::format::timecode(annotation.time_ms / 1000))
            .collect();
        let annotation_specs: Vec<RowSpec<'_>> = annotations
            .iter()
            .zip(&jumpable)
            .zip(&times)
            .map(|(((id, annotation), jumpable), time)| RowSpec {
                title: annotation.title(),
                meta: Some(time.as_str()),
                selected: false,
                focused: focus == Some(format!("shared-jump-{id}").as_str())
                    || focus == Some(format!("shared-delete-{id}").as_str()),
                actions: if *jumpable {
                    vec![JUMP_TO_BOOKMARK, DELETE_SHARED]
                } else {
                    vec![DELETE_SHARED]
                },
            })
            .collect();
        let scroll = self.library_scroll.max(0.0);
        let mut y = region.y0 - scroll;
        let top = y;
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &region);
        let mut hits: Vec<SharedRowHit> = Vec::new();
        let heading = |ctx: &mut PaintCtx<'_>, label: &str, y: &mut f64| {
            let line = ctx.line_box(TextRole::Subheading);
            let band = Rect::new(inner_x0, *y, inner_x1, *y + line + s2);
            if band.y1 > region.y0 && band.y0 < region.y1 {
                let ink = ctx.theme.tokens.colors.text_primary;
                ctx.text_role_in_band(label, TextRole::Subheading, ink, band.x0, band, None);
            }
            *y += line + s2;
        };
        if !playlists.is_empty() {
            heading(&mut ctx, "Playlists", &mut y);
            let placed = paint_rows(
                &mut ctx,
                &playlist_specs,
                (list.x0, list.x1),
                &mut y,
                region,
                cursor,
            );
            for ((id, _), placed) in playlists.iter().zip(placed) {
                if let Some(remove) = placed.actions.last().copied() {
                    hits.push(SharedRowHit {
                        id: id.to_string(),
                        jump: None,
                        remove,
                        annotation: false,
                    });
                }
            }
            y += s4;
        }
        if !annotations.is_empty() {
            heading(&mut ctx, "Bookmarks", &mut y);
            let placed = paint_rows(
                &mut ctx,
                &annotation_specs,
                (list.x0, list.x1),
                &mut y,
                region,
                cursor,
            );
            for (((id, _), jumpable), placed) in annotations.iter().zip(&jumpable).zip(placed) {
                if let Some(remove) = placed.actions.last().copied() {
                    hits.push(SharedRowHit {
                        id: id.to_string(),
                        jump: jumpable.then(|| placed.actions.first().copied()).flatten(),
                        remove,
                        annotation: true,
                    });
                }
            }
        }
        ctx.scene.pop_layer();
        self.library_max_scroll = ((y - top) - region.height()).max(0.0);
        self.library_scroll = scroll.min(self.library_max_scroll);
        self.shared_row_hits = hits;
    }
}

pub(super) fn shared_accessibility_targets(app: &PlayerApp) -> Vec<(String, String, Rect)> {
    let mut targets = Vec::new();
    for row in &app.shared_row_hits {
        if row.annotation {
            if let Some(jump) = row.jump {
                targets.push((
                    format!("shared-jump-{}", row.id),
                    format!("video-player.shared.jump.annotation.{}", row.id),
                    jump,
                ));
            }
            targets.push((
                format!("shared-delete-{}", row.id),
                format!("video-player.shared.delete.annotation.{}", row.id),
                row.remove,
            ));
        } else {
            targets.push((
                format!("shared-delete-{}", row.id),
                format!("video-player.shared.delete.playlist.{}", row.id),
                row.remove,
            ));
        }
    }
    targets
}

pub(super) fn shared_accessibility_nodes(app: &PlayerApp) -> Vec<ui::a11y::AccessNode> {
    use ui::a11y::{AccessNode, AccessState, Role};
    let mut nodes = vec![
        AccessNode::new(Role::Button)
            .with_name("Add current video to Watch later")
            .with_action_target("video-player.shared.watch-later")
            .with_bounds(app.shared_watch_later_rect)
            .with_state(AccessState {
                focusable: true,
                focused: app.library_focus_id.as_deref() == Some("shared-watch-later"),
                disabled: !app.player.has_media(),
                ..Default::default()
            }),
        AccessNode::new(Role::Button)
            .with_name("Bookmark current timestamp")
            .with_action_target("video-player.shared.bookmark")
            .with_bounds(app.shared_bookmark_rect)
            .with_state(AccessState {
                focusable: true,
                focused: app.library_focus_id.as_deref() == Some("shared-bookmark"),
                disabled: !app.player.has_media(),
                ..Default::default()
            }),
    ];
    nodes.extend(
        shared_accessibility_targets(app)
            .into_iter()
            .map(|(focus, target, rect)| {
                let is_jump = focus.starts_with("shared-jump-");
                AccessNode::new(Role::Button)
                    .with_name(if is_jump {
                        "Jump to timestamp annotation"
                    } else if target.contains(".annotation.") {
                        "Delete timestamp annotation"
                    } else {
                        "Delete shared playlist"
                    })
                    .with_action_target(target)
                    .with_bounds(rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: app.library_focus_id.as_deref() == Some(focus.as_str()),
                        ..Default::default()
                    })
            }),
    );
    nodes
}

pub(super) fn local_library_accessibility_nodes(app: &PlayerApp) -> Vec<ui::a11y::AccessNode> {
    use ui::a11y::{AccessNode, AccessState, Role};
    let mut nodes: Vec<_> = app
        .library_row_hits
        .iter()
        .map(|row| {
            let id = &row.id;
            let title = app
                .queue
                .iter()
                .chain(app.recent.iter())
                .find(|entry| entry.id == *id)
                .map_or("Local video", |entry| entry.title.as_str());
            AccessNode::new(Role::Button)
                .with_name(format!("Play {title}"))
                .with_description("Opens this device-local video")
                .with_action_target(format!("video-player.library.play.{id}"))
                .with_bounds(row.play)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref() == Some(id.as_str()),
                    ..Default::default()
                })
        })
        .collect();
    if app.library_tab == LibraryTab::Recent && !app.recent.is_empty() {
        nodes.push(
            AccessNode::new(Role::Button)
                .with_name("Clear recent videos")
                .with_description("Removes recent-history entries from this device")
                .with_action_target("video-player.library.clear")
                .with_bounds(app.library_clear_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref() == Some("library-clear"),
                    ..Default::default()
                }),
        );
    }
    nodes.extend(app.library_row_hits.iter().filter_map(|row| {
        row.remove.map(|remove| {
            AccessNode::new(Role::Button)
                .with_name("Remove from queue")
                .with_description("Removes this item from the session-only queue")
                .with_action_target(format!("video-player.library.remove.{}", row.id))
                .with_bounds(remove)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref()
                        == Some(format!("library-remove-{}", row.id).as_str()),
                    ..Default::default()
                })
        })
    }));
    nodes
}
