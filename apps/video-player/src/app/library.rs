//! The library: the queue, recent videos and shared playlists and bookmarks, in the
//! window's drawer. Wide it docks beside the picture; narrower it takes the content area
//! as a screen of its own, with "Back to video" leading its tabs. The bar's sidebar toggle
//! and Escape show and hide it.

use super::{
    Affine, Button, ButtonFace, Icon, LibraryRowHit, LibraryTab, LocalMediaEntry, PlayerState,
    Point, Rect, Scene, SharedRowHit, TextRenderer, Theme,
};
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::vello::peniko::{BlendMode, Fill};
use ui::widget::{
    ButtonRole, ControlSize, EmptyState, EmptyStateContent, Interaction, ListRowFace, RowAction,
    SegmentedFace, ToggleGroup,
};

impl LibraryTab {
    fn labels() -> [String; 3] {
        ["video-queue", "video-recent", "video-shared"].map(crate::i18n::t)
    }

    fn index(self) -> usize {
        match self {
            Self::Queue => 0,
            Self::Recent => 1,
            Self::Shared => 2,
        }
    }
}

fn action(icon: Icon, label: &str) -> (Icon, String) {
    (icon, crate::i18n::t(label))
}

struct RowSpec<'a> {
    title: &'a str,
    meta: Option<&'a str>,
    selected: bool,
    focused: bool,
    actions: Vec<(Icon, String)>,
}

struct RowPlaced {
    row: Rect,
    actions: Vec<Rect>,
}

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
        let actions: Vec<RowAction<'_>> = spec
            .actions
            .iter()
            .map(|(icon, label)| RowAction { icon: *icon, label })
            .collect();
        let probe = ListRowFace::new(spec.title)
            .actions(&actions)
            .interaction(Interaction {
                hovered: true,
                ..Interaction::default()
            });
        let rects = probe.action_rects(row, ctx.theme);
        let visible = row.y1 > clip.y0 && row.y0 < clip.y1;
        let hovered = visible && clip.contains(cursor) && row.contains(cursor);
        if visible {
            let hovered_action = hovered
                .then(|| rects.iter().position(|rect| rect.contains(cursor)))
                .flatten();
            let mut face = ListRowFace::new(spec.title)
                .selected(spec.selected)
                .actions(&actions)
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
        placed.push(RowPlaced {
            row,
            actions: rects,
        });
        *y += height;
    }
    placed
}

impl PlayerState {
    /// Follow `library_visible` with the drawer, which only animates the flag the
    /// resource action changes.
    pub(super) fn sync_library_sheet(&mut self) {
        if self.library_visible != self.drawer.is_open() {
            if self.library_visible {
                self.drawer.open();
            } else {
                self.drawer.close();
            }
        }
    }

    /// Lay the library out: docked beside the picture where the picture keeps its minimum
    /// width, else a screen of its own, else floating down to the transport. A docked
    /// library that changed width asks for the player to be laid out again.
    pub(super) fn resolve_library(&mut self) {
        let content = self.drawer.resolve(self.rect);
        if self.drawer.is_overlay()
            && !self.library_takes_the_window()
            && self.transport_top > self.rect.y0
        {
            self.drawer.resolve(Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x1,
                self.transport_top.min(self.rect.y1),
            ));
        }
        let inset = if self.drawer.is_docked() {
            (content.x0 - self.rect.x0).max(0.0)
        } else {
            0.0
        };
        if (inset - self.content_inset).abs() > 0.5 {
            self.content_inset = inset;
            self.content_inset_stale = true;
            self.dirty = true;
        }
    }

    /// Too narrow to dock, the library takes the content area as a screen of its own, as
    /// the bar's sidebar toggle does in every other app.
    pub(super) fn library_takes_the_window(&self) -> bool {
        self.drawer.is_push()
    }

    fn clear_library_rects(&mut self) {
        self.library_row_hits.clear();
        self.shared_row_hits.clear();
        self.library_clear_rect = Rect::ZERO;
        self.library_back_rect = Rect::ZERO;
        self.library_queue_tab_rect = Rect::ZERO;
        self.library_recent_tab_rect = Rect::ZERO;
        self.library_shared_tab_rect = Rect::ZERO;
        self.library_list_rect = Rect::ZERO;
        self.shared_watch_later_rect = Rect::ZERO;
        self.shared_bookmark_rect = Rect::ZERO;
    }

    fn paint_library_tabs(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        cursor: Point,
    ) -> f64 {
        let tab_h = SegmentedFace::height(ControlSize::Regular, theme);
        let labels = LibraryTab::labels();
        let tab_labels = labels.each_ref().map(String::as_str);
        let segments = tab_labels.map(ui::widget::Segment::new);
        let packed = SegmentedFace::partition_segments(
            Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + tab_h),
            &segments,
            ui::widget::SegmentedStyle::Tabs,
            ControlSize::Regular,
            text,
            theme,
        );
        let tab_rects: [Rect; 3] =
            std::array::from_fn(|i| packed.get(i).copied().unwrap_or_default());
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
            &SegmentedFace::new(&tab_rects, &tab_labels, self.library_tab.index())
                .style(ui::widget::SegmentedStyle::Tabs)
                .hovered(hovered_tab)
                .focused(tab_focus),
        );
        tab_h
    }

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

        // Filling a narrow window, the library leads with the way back to the picture, as
        // a pushed screen does.
        let mut inner = inner;
        if self.library_takes_the_window() {
            let control = ButtonFace::height(ControlSize::Regular, theme);
            let back = crate::i18n::t("video-back-to-video");
            let width = ButtonFace::new(Rect::ZERO, &back)
                .icon(Icon::ChevronLeft)
                .role(ButtonRole::Plain)
                .intrinsic_width(text, theme);
            self.library_back_rect =
                Rect::new(inner.x0, inner.y0, inner.x0 + width, inner.y0 + control);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(self.library_back_rect, &back)
                    .icon(Icon::ChevronLeft)
                    .role(ButtonRole::Plain)
                    .hovered(self.library_back_rect.contains(cursor))
                    .focused(self.library_focus_id.as_deref() == Some("library-back")),
            );
            inner.y0 = self.library_back_rect.y1 + sibling;
        }

        let tab_h = self.paint_library_tabs(scene, transform, text, theme, inner, cursor);

        // Clear, on Recent with something to clear, at the trailing end of its own row.
        let mut list_top = inner.y0 + tab_h + sibling;
        if self.library_tab == LibraryTab::Recent && !self.recent.is_empty() {
            let control = ButtonFace::height(ControlSize::Regular, theme);
            let clear = crate::i18n::t("video-library-clear");
            let width = ButtonFace::new(Rect::ZERO, &clear)
                .role(ButtonRole::Plain)
                .intrinsic_width(text, theme);
            self.library_clear_rect =
                Rect::new(inner.x1 - width, list_top, inner.x1, list_top + control);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(self.library_clear_rect, &clear)
                    .role(ButtonRole::Plain)
                    .hovered(self.library_clear_rect.contains(cursor))
                    .focused(self.library_focus_id.as_deref() == Some("library-clear")),
            );
            list_top = self.library_clear_rect.y1 + sibling;
        }

        // The list under the tabs.
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
            let empty = if tab == LibraryTab::Queue {
                crate::i18n::t("video-queue-empty")
            } else {
                crate::i18n::t("video-recent-empty")
            };
            let content = EmptyStateContent {
                icon: Some(Icon::ListVideo),
                ..EmptyStateContent::new(&empty)
            };
            let _ = EmptyState::paint(&mut ctx, list, &content, cursor);
            return;
        }
        let shown = entries.len().min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let entries = entries.get(..shown).unwrap_or(&entries);
        let current_state = self.current_row_state();
        let specs: Vec<RowSpec<'_>> = entries
            .iter()
            .map(|entry| {
                let active = self.current_queue_id.as_deref() == Some(entry.id.as_str());
                let focus = self.library_focus_id.as_deref();
                let remove_focus = format!("library-remove-{}", entry.id);
                RowSpec {
                    title: &entry.title,
                    meta: active.then_some(current_state),
                    selected: active,
                    focused: focus == Some(entry.id.as_str())
                        || focus == Some(remove_focus.as_str()),
                    actions: if tab == LibraryTab::Queue {
                        vec![action(Icon::X, "video-remove-from-queue")]
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
                crate::i18n::t("video-watch-later"),
                "shared-watch-later",
            ),
            (
                self.shared_bookmark_rect,
                crate::i18n::t("video-bookmark-time"),
                "shared-bookmark",
            ),
        ] {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(rect, &label)
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
            let title = crate::i18n::t("video-shared-empty");
            let detail = crate::i18n::t("video-shared-empty-detail");
            let content = EmptyStateContent {
                icon: Some(Icon::ListVideo),
                message: Some(&detail),
                ..EmptyStateContent::new(&title)
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
                actions: vec![action(Icon::Trash2, "video-delete")],
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
                    vec![
                        action(Icon::Play, "video-jump-to-bookmark"),
                        action(Icon::Trash2, "video-delete"),
                    ]
                } else {
                    vec![action(Icon::Trash2, "video-delete")]
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
            heading(&mut ctx, &crate::i18n::t("video-playlists"), &mut y);
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
            heading(&mut ctx, &crate::i18n::t("video-bookmarks"), &mut y);
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

pub(super) fn shared_accessibility_targets(app: &PlayerState) -> Vec<(String, String, Rect)> {
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

pub(super) fn shared_accessibility_nodes(app: &PlayerState) -> Vec<ui::a11y::AccessNode> {
    use ui::a11y::{AccessNode, AccessState, Role};
    let mut nodes = vec![
        AccessNode::new(Role::Button)
            .with_name(crate::i18n::t("video-add-to-watch-later"))
            .with_action_target("video-player.shared.watch-later")
            .with_bounds(app.shared_watch_later_rect)
            .with_state(AccessState {
                focusable: true,
                focused: app.library_focus_id.as_deref() == Some("shared-watch-later"),
                disabled: !app.player.has_media(),
                ..Default::default()
            }),
        AccessNode::new(Role::Button)
            .with_name(crate::i18n::t("video-bookmark-timestamp"))
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
                        crate::i18n::t("video-jump-to-note")
                    } else if target.contains(".annotation.") {
                        crate::i18n::t("video-delete-note")
                    } else {
                        crate::i18n::t("video-delete-shared-playlist")
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

pub(super) fn local_library_accessibility_nodes(app: &PlayerState) -> Vec<ui::a11y::AccessNode> {
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
                .map_or_else(
                    || crate::i18n::t("video-local-video"),
                    |entry| entry.title.clone(),
                );
            AccessNode::new(Role::Button)
                .with_name(crate::i18n::t_args(
                    "video-play",
                    &[("title", ui::i18n::ArgValue::Text(title))],
                ))
                .with_description(crate::i18n::t("video-local-only-detail"))
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
                .with_name(crate::i18n::t("video-clear-recent"))
                .with_description(crate::i18n::t("video-clear-recent-detail"))
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
                .with_name(crate::i18n::t("video-remove-from-queue"))
                .with_description(crate::i18n::t("video-remove-from-queue-detail"))
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
