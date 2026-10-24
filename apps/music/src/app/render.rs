//! Drawing + event handling for [`MusicApp`].
//!
//! Split out of `app.rs` to keep the data/state logic separate from the
//! (large) rendering surface. Music paints its content column itself, but
//! every part of it is the toolkit's: controls are [`ButtonFace`]s, covers
//! are [`MediaCardFace`]s and every text names a [`TextRole`]. Clicks
//! register `Hit`s for the event handler. The sidebar and the transport are
//! the window shell's (`shell.rs`).

use super::page_part::PagePart;

mod accessible;
mod now_playing;
mod query_view;
use super::{
    Action, Affine, AlbumPage, Arc, ArtistPage, AutoCommit, CacheState, Color, Fill, HashMap,
    HomeItem, HomeItemKind, HomeSection, Instant, MusicApp, NodeId, ObjId, PlaylistPage, Point,
    PresenceCtx, Rect, Scene, Size, Style, TaffyTree, TextRenderer, TextStyle, Theme, View, Widget,
    elide, is_live_album_id, is_live_id, is_live_playlist_id, live_track_id, lyrics,
};
use accessible::{
    accessible_album, accessible_artist, accessible_download, accessible_playlist,
    accessible_playlist_track, accessible_track,
};
pub(super) use now_playing::NowPlaying;
pub(super) use query_view::SearchView;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::Continuable;
use ui::widget::data_table::DataTableFace;
use ui::widget::toast::ToastAnchor;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent, MediaArt,
    MediaCardFace, MediaSize, SkeletonFace, SkeletonShape,
};
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

// ---- owned per-frame display rows (decouple draw from &self.lib borrow) ---

struct TrackDisp {
    id: String,
    title: String,
    artist: String,
    album: String,
    dur: u32,
    no: u32,
}

#[derive(Clone)]
struct AlbumDisp {
    id: String,
    title: String,
    artist: String,
    year: u32,
    queue: Vec<String>,
}

#[derive(Clone)]
struct ArtistDisp {
    id: String,
    name: String,
    genre: String,
}

struct PlaylistDisp {
    id: String,
    name: String,
    count: usize,
}

/// The painted frame's ink and spacing, resolved from the theme once per
/// frame. Every gap is a step of the theme's spacing rhythm.
#[derive(Clone, Copy)]
struct Look {
    text: Color,
    dim: Color,
    faint: Color,
    /// The content gutter and the gap between groups of sections.
    gutter: f64,
    /// Between two sections of a page.
    section: f64,
    /// Between siblings: cards, buttons in a row.
    sibling: f64,
    /// Inside one cluster: a title and its secondary line, a bar's tools.
    related: f64,
    tight: f64,
    /// The minimum pointer target: icon buttons, the sidebar toggle.
    target: f64,
}

impl Look {
    fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            text: theme.text_primary,
            dim: theme.text_dim,
            faint: theme.text_tertiary,
            gutter: f64::from(rhythm.group),
            section: f64::from(rhythm.section),
            sibling: f64::from(rhythm.sibling),
            related: f64::from(rhythm.related),
            tight: f64::from(rhythm.tight),
            target: f64::from(theme.tokens.metrics.min_pointer_target),
        }
    }
}

/// The measured height of one line of `style`.
fn line_of(ctx: &mut PaintCtx<'_>, style: &TextStyle) -> f64 {
    f64::from(ctx.text.measure_style("Ag", None, style).1)
}

/// Paint one line of `text`, elided to `max_w`, with its line box's top at
/// `origin`. Returns the line's height.
pub(super) fn text_line(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    origin: Point,
    max_w: f64,
) -> f64 {
    let shown = elide(ctx.text, text, style, max_w);
    ctx.text(&shown, style, color, origin, None);
    line_of(ctx, style)
}

/// Paint one line of `text` ending at `x1`, centred on its line box inside
/// `band`. Returns the x the text starts at.
pub(super) fn text_right_in_band(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    x1: f64,
    band: Rect,
) -> f64 {
    let (w, h) = ctx.text.measure_style(text, None, style);
    let x = x1 - f64::from(w);
    let top = ui::text_metrics::centered_text_top(band.y0, band.height(), f64::from(h));
    ctx.text(text, style, color, Point::new(x, top), None);
    x
}

/// A square of `side` centred on `c`.
pub(super) fn square_around(c: Point, side: f64) -> Rect {
    Rect::from_center_size(c, (side, side))
}

/// Keep the active-screen accessibility snapshot useful without cloning an
/// unbounded library into AccessKit/AT-SPI on every frame.  Global search uses
/// the paged resource provider; this tree represents what a person can
/// currently browse in the app and deliberately follows the same bounded
/// materialisation rule.
pub(super) const ACCESSIBLE_PAGE_LIMIT: usize = 100;

/// What a track table lists and how: the rows, the queue a click plays from,
/// whether the album column and the leading track numbers show, and the
/// playlist the rows belong to (which binds the row menu to it and adds the
/// reorder/remove affordances).
struct TrackTable<'a> {
    rows: &'a [TrackDisp],
    queue: &'a [String],
    show_album: bool,
    number_col: bool,
    playlist: Option<&'a str>,
    /// Whether a header names the columns (when there is more than one).
    header: bool,
    /// Rows stand for queue entries from this index on (the queue view):
    /// a click plays that entry and hover offers reorder/remove in the queue.
    queue_offset: Option<usize>,
}

impl<'a> TrackTable<'a> {
    /// A numbered table with an album column, not bound to a playlist.
    fn new(rows: &'a [TrackDisp], queue: &'a [String]) -> Self {
        Self {
            rows,
            queue,
            show_album: true,
            number_col: true,
            playlist: None,
            header: true,
            queue_offset: None,
        }
    }

    /// No column header: the table continues one already headed above it.
    fn without_header(self) -> Self {
        Self {
            header: false,
            ..self
        }
    }

    /// Hide the album column (an album's own page).
    fn without_album(self) -> Self {
        Self {
            show_album: false,
            ..self
        }
    }

    /// Leave out the leading index column.
    fn without_numbers(self) -> Self {
        Self {
            number_col: false,
            ..self
        }
    }

    /// Bind the rows to playlist `id`.
    fn in_playlist(self, id: &'a str) -> Self {
        Self {
            playlist: Some(id),
            ..self
        }
    }

    /// The rows are the queue from `offset` on.
    fn in_queue(self, offset: usize) -> Self {
        Self {
            queue_offset: Some(offset),
            number_col: false,
            ..self
        }
    }
}

/// Column edges of a track table inside its `[left, right]` span: an index
/// column, the title block sized to its widest title, an optional album
/// column that starts right after it and takes the rest of the free width
/// (so no dead gap opens between them), the row's actions and the
/// right-aligned duration.
#[derive(Clone, Copy, Debug)]
pub(super) struct TrackColumns {
    pub title_x: f64,
    pub title_x1: f64,
    pub album: Option<(f64, f64)>,
    /// The leading edge of the action cluster.
    #[cfg(test)]
    pub actions_x: f64,
}

/// The measured widths a track table lays its columns out from.
#[derive(Clone, Copy, Debug)]
pub(super) struct ColumnWidths {
    /// The index column (0 for none).
    pub index: f64,
    /// The widest duration.
    pub duration: f64,
    /// One action button, and how many the row holds.
    pub action: f64,
    pub actions: usize,
    /// The widest title or artist line of the rows.
    pub title: f64,
    /// Between two columns.
    pub gap: f64,
    /// The narrowest a title or album column may get; the album column only
    /// shows when both keep it.
    pub min_text: f64,
}

/// Lay out a track table's columns inside `[left, right]`, with an album
/// column when `album` asks for one and it fits.
pub(super) fn track_columns(left: f64, right: f64, w: ColumnWidths, album: bool) -> TrackColumns {
    let ColumnWidths {
        index: index_w,
        duration: duration_w,
        action: action_w,
        actions,
        title: title_need,
        gap,
        min_text,
    } = w;
    let title_x = if index_w > 0.0 {
        left + index_w + gap
    } else {
        left
    };
    let actions_x = right - duration_w - gap - action_w * actions as f64;
    let free = (actions_x - gap - title_x).max(0.0);
    let (title_x1, album) = if album && free >= 2.0 * min_text + gap {
        // The title column is as wide as its widest line, never narrower
        // than a readable column and never more than half the free width.
        let title_w = title_need.clamp(min_text, ((free - gap) / 2.0).max(min_text));
        let album_x = title_x + title_w + gap;
        (title_x + title_w, Some((album_x, actions_x - gap)))
    } else {
        (actions_x - gap, None)
    };
    TrackColumns {
        title_x,
        title_x1,
        album,
        #[cfg(test)]
        actions_x,
    }
}

/// A Home section's track queue, and the queue position of the next track
/// tile drawn.
struct SectionQueue<'a> {
    ids: &'a [String],
    next: usize,
}

/// Which kind of entity a detail page shows, with the state of the toggle
/// its header offers: "Save to library" for an album, "Follow" for an
/// artist, playlist management for a playlist.
#[derive(Clone, Copy)]
enum DetailKind {
    Album { saved: bool },
    Artist { following: bool },
    Playlist { own: bool },
}

/// What a detail page's header shows: the entity `id` (which also keys its
/// art and its toggle), its kind, title, one secondary line and one
/// caption, and the queue its Play and Shuffle buttons start.
struct DetailHeader<'a> {
    id: &'a str,
    kind: DetailKind,
    title: &'a str,
    secondary: &'a str,
    caption: &'a str,
    queue: Vec<String>,
}

/// The lyrics view's one column: on the content gutter like every other
/// page, bounded to a readable width, inside the gutter top and bottom. The
/// song itself is named by the transport bar, so the column is lyrics only.
pub(super) fn lyrics_column(body: Rect, readable: f64, gutter: f64) -> Rect {
    let x0 = body.x0 + gutter;
    let x1 = (x0 + readable).min(body.x1 - gutter).max(x0);
    Rect::new(
        x0,
        body.y0 + gutter,
        x1,
        (body.y1 - gutter).max(body.y0 + gutter),
    )
}

/// How far the lyric lines are scrolled up in a column `column_h` high:
/// synced lyrics keep the current line a third of the way down, so the
/// eye finds it in one place; plain lyrics read from the top.
/// Lines wrap, so each has its own height: the current line's top is
/// `current_top` down the unscrolled column.
pub(super) fn lyrics_scroll(current_top: Option<f64>, column_h: f64) -> f64 {
    current_top.map_or(0.0, |top| top - column_h / 3.0)
}

/// Monotonic seconds since first use — the shared animation clock.
fn anim_time() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(ui::time::now).elapsed().as_secs_f64()
}

/// Normalised `0..=1` heights for the 3-bar now-playing equalizer. While
/// `playing` the bars bounce on out-of-phase sinusoids; paused they hold a
/// static silhouette.
fn eq_bars(playing: bool, reduce_motion: bool) -> [f64; 3] {
    if !playing || reduce_motion {
        return [0.35, 0.72, 0.48];
    }
    let ti = anim_time();
    let speeds = [9.0, 12.5, 7.3];
    let phases = [0.0, 1.7, 3.4];
    let mut out = [0.0; 3];
    for ((o, speed), phase) in out.iter_mut().zip(speeds).zip(phases) {
        *o = 0.15 + 0.85 * (0.5 + 0.5 * (ti * speed + phase).sin());
    }
    out
}

/// The now-playing equalizer, ending at `end`: three bars that move while
/// the track plays and hold still when it is paused (or motion is reduced).
pub(super) fn paint_equalizer(ctx: &mut PaintCtx<'_>, playing: bool, end: Point) {
    let theme = ctx.theme;
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let bars = eq_bars(playing, theme.reduce_motion);
    let bar_w = tight - 1.0;
    let pitch = tight + tight / 2.0;
    let max_h = f64::from(theme.tokens.metrics.icon_sm);
    let x0 = end.x - (pitch * 2.0 + bar_w);
    for (i, &frac) in bars.iter().enumerate() {
        let x = x0 + i as f64 * pitch;
        let h = max_h * (0.3 + 0.7 * frac);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            theme.text_primary,
            None,
            &Rect::new(x, end.y - h / 2.0, x + bar_w, end.y + h / 2.0),
        );
    }
}

/// One card of a media grid: what it shows and what a click on it does.
struct GridCard<'a> {
    art_id: &'a str,
    title: &'a str,
    meta: [Option<&'a str>; 2],
    action: Action,
    /// A Play action over the art while hovered.
    play: Option<Action>,
    /// A person (an artist): a round portrait with the name centred under
    /// it, instead of a square cover.
    portrait: bool,
}

impl Continuable for MusicApp {
    fn continuation_metadata(
        &self,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let track_id = self.pb.current.as_ref()?;
        if self
            .local_file
            .as_ref()
            .is_some_and(|file| &file.track.id == track_id)
        {
            return None;
        }
        if track_id.is_empty() || track_id.len() > 256 || track_id.contains(['/', '\\']) {
            return None;
        }
        // Intentionally omit `playing`, queue and volume. A receiving device
        // may offer this paused position, but must never autoplay or alter its
        // local audio policy merely because another device reported it.
        let bytes = serde_json::to_vec(&serde_json::json!({
            "track_id": track_id,
            "position_ms": self.playhead_ms.max(0.0) as u64,
            "paused": true,
        }))
        .ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::MEDIA_POSITION_V1,
            bytes,
        })
    }
    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != ui::collaboration::semantic_schema::MEDIA_POSITION_V1 {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&metadata.bytes) else {
            return false;
        };
        let Some(track_id) = value.get("track_id").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !self.lib.tracks.contains_key(track_id) {
            return false;
        }
        self.retire_replaced_playback();
        self.pb.current = Some(track_id.to_owned());
        self.pb.queue = vec![track_id.to_owned()];
        self.pb.queue_index = 0;
        self.bound_track.clone_from(&self.pb.current);
        self.shuffle_order.clear();
        let requested_position = value
            .get("position_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let duration = self.current_duration_ms();
        let position = if duration == 0 {
            requested_position
        } else {
            requested_position.min(duration)
        };
        self.pb.position_ms = position;
        self.pb.playing = false;
        self.playhead_ms = position as f64;
        true
    }
}

impl Widget for MusicApp {
    ui::provides!(
        dyn ui::widget::Continuable,
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Queryable
    );

    /// The name prompt and an open menu own the input over the whole window
    /// while they are up: Escape closes them, never the shell's screen.
    fn modal_capture(&self) -> Option<Rect> {
        (self.name_prompt.is_some() || self.context.is_open()).then_some(self.rect)
    }

    /// A drag of songs keeps the pointer while it crosses into the shell's
    /// sidebar, to be let go on a playlist there.
    fn wants_pointer_capture(&self) -> bool {
        self.search.drag.is_some()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Reported once: an app that is not rebuilt right away (a tab behind
        // another) must not ask again on every frame.
        let visible = Some(self.source_widget_mode());
        std::mem::replace(&mut self.open_file_layout_visible, visible) != visible
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        children.extend(
            self.track_tables
                .iter()
                .filter_map(|slot| slot.table.host.visible().map(|table| table as &dyn Widget)),
        );
        children.extend(
            self.media_grids
                .iter()
                .filter_map(|grid| grid.host.visible().map(|grid| grid as &dyn Widget)),
        );
        let mode = self.source_widget_mode();
        match mode {
            2 | 3 => children.extend(
                self.source_buttons
                    .iter()
                    .filter(|button| {
                        button.test_id()
                            == Some(if mode == 3 {
                                "music-spotify-create-playlist"
                            } else {
                                "music-sources"
                            })
                    })
                    .map(|button| button as &dyn Widget),
            ),
            1 => children.extend(
                self.source_buttons
                    .iter()
                    .filter(|button| button.test_id() == Some("music-spotify-queue-refresh"))
                    .map(|button| button as &dyn Widget),
            ),
            _ => {}
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mode = self.source_widget_mode();
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        children.extend(self.track_tables.iter_mut().filter_map(|slot| {
            slot.table
                .host
                .visible_mut()
                .map(|table| table as &mut (dyn Widget + '_))
        }));
        children.extend(self.media_grids.iter_mut().filter_map(|grid| {
            grid.host
                .visible_mut()
                .map(|grid| grid as &mut (dyn Widget + '_))
        }));
        match mode {
            2 | 3 => children.extend(
                self.source_buttons
                    .iter_mut()
                    .filter(|button| {
                        button.test_id()
                            == Some(if mode == 3 {
                                "music-spotify-create-playlist"
                            } else {
                                "music-sources"
                            })
                    })
                    .map(|button| button as &mut (dyn Widget + '_)),
            ),
            1 => children.extend(
                self.source_buttons
                    .iter_mut()
                    .filter(|button| button.test_id() == Some("music-spotify-queue-refresh"))
                    .map(|button| button as &mut (dyn Widget + '_)),
            ),
            _ => {}
        }
        children
    }

    /// The context menu and the name prompt paint in the window's top
    /// layer, over the app and its chrome.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // The cover flying between the band and Now Playing.
        self.paint_now_playing_flight(&mut PaintCtx::new(scene, transform, text_renderer, theme));
        // The songs being dragged follow the pointer over the whole window,
        // onto the sidebar's playlists.
        self.paint_track_drag(&mut PaintCtx::new(scene, transform, text_renderer, theme));
        if self.context.is_open() {
            self.context
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.name_prompt.is_some() {
            self.ensure_name_dialog();
            self.layout_name_dialog();
            if let Some(nd) = self.name_dialog.as_mut() {
                nd.dialog
                    .draw_overlay(scene, transform, text_renderer, theme, presence);
            }
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.pump_live_search();
        self.open_anchors();
        self.poll_query();
        self.declare_window_toolbar();
        let (state, _) = ui::doc::document_ref(state, path);
        if self.playback_loaded {
            self.reload_library(state);
        } else {
            self.reload(state);
            self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
            self.playback_loaded = true;
        }
        self.show_shell();
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        // External controls and completed effects enter the same authored
        // action door as events, before the next read-only projection.
        // The host admits row control effects after delivering input. Their
        // callbacks queue requests; apply them at the next sync through the
        // same canonical actions as the palette, without waiting for another
        // pointer event.
        self.take_window_toolbar_events();
        self.take_shell_events(state);
        if self.back_requested.replace(false) {
            self.on_back();
            self.dirty = true;
        }
        self.drain_table_requests(state);
        // Progress in-flight downloads and re-apply their device-local cache
        // projection on top of the just-reloaded shared library.
        self.advance_downloads(state);
        self.drain_mpris(state);
        self.drain_cover_pick(state);
        self.drain_local_file_pick();
        self.drain_cover_embed(state);
        self.apply_pending_media_handoff(state);
        self.deliver_effect_replies();
        self.publish_mpris();
        self.publish_handoff();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `Widget::build_layout` returns a bare `NodeId`, not a `Result` — a
        // framework-wide contract this one widget can't change — so a
        // `new_leaf` failure (which a freshly-created tree with one valid
        // leaf style cannot produce) has no propagation path here.
        self.refresh_spotify_control_flags();
        let mut children = Vec::new();
        let mode = self.source_widget_mode();
        match mode {
            2 | 3 => children.extend(
                self.source_buttons
                    .iter_mut()
                    .filter(|button| {
                        button.test_id()
                            == Some(if mode == 3 {
                                "music-spotify-create-playlist"
                            } else {
                                "music-sources"
                            })
                    })
                    .map(|button| button.build_layout(taffy, theme, node_map)),
            ),
            1 => children.extend(
                self.source_buttons
                    .iter_mut()
                    .filter(|button| button.test_id() == Some("music-spotify-queue-refresh"))
                    .map(|button| button.build_layout(taffy, theme, node_map)),
            ),
            _ => {}
        }
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.render_frame(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // While the context menu is open it owns the pointer + keyboard: route
        // the event to it, then drain any selection into the Action pipeline.
        if self.context.is_open() && self.route_context_menu(event, ctx) {
            *ctx.handled = true;
            return;
        }

        // The name-prompt dialog is modal: while it's up the shared ModalDialog
        // captures every event (typing, clicks, Esc) and nothing leaks to the
        // library behind the scrim.
        if self.name_prompt.is_some() {
            self.route_name_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.handled,
            );
            return;
        }

        self.route_source_widgets(event, ctx);
        if *ctx.handled {
            return;
        }

        // The body's scrolling: wheel and touchpad over it, its bar, and
        // Page Up/Down, Home/End and the arrows.
        if let Some(theme) = self.frame_theme.take() {
            // Keyboard scrolling keeps the bar visible, but pointer browsing
            // must not pin a focused scrollbar on screen forever.
            match event {
                WindowEvent::KeyboardInput { .. } => self.body_scroll.set_focused(true),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Focused(false) => self.body_scroll.set_focused(false),
                _ => {}
            }
            self.dirty |= self.body_scroll.handle_event(event, ctx, &theme);
            self.frame_theme = Some(theme);
            if *ctx.handled {
                self.dirty = true;
                return;
            }
        }
        self.on_input(event, ctx);
    }

    fn name(&self) -> &'static str {
        "MusicApp"
    }

    /// The music library is custom-painted, so it must also publish the full
    /// active screen explicitly.  Navigation, search, content rows and the
    /// player all route to the exact same reducers as pointer/keyboard input.
    /// Large collections are capped here; the canonical resource/search layer
    /// remains paged and resolves every later item lazily by stable id.
    #[allow(
        clippy::too_many_lines,
        reason = "the semantic tree intentionally mirrors every Music view in one exhaustive match; splitting it would separate accessibility coverage from the view enum it audits"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let current = self
            .pb
            .current
            .as_ref()
            .and_then(|id| self.playback_track(id))
            .map_or_else(
                || "Nothing playing".to_string(),
                |track| track.title.clone(),
            );
        let player_status = if let Some(error) = &self.playback_error {
            error.clone()
        } else if self.active_resolve_request.is_some() {
            "Loading audio source".to_string()
        } else if self.pb.playing && !self.audio.is_playing_real() {
            "Waiting for audio output".to_string()
        } else {
            "Playback controls".to_string()
        };
        let player = AccessNode::new(Role::Group)
            .with_hashed_key("player")
            .with_name("Music player")
            .with_value(format!("{current}; volume {}%", self.pb.volume))
            .with_description(player_status)
            .with_state(AccessState::new().busy(self.active_resolve_request.is_some()));
        let search = AccessNode::new(Role::Button)
            .with_hashed_key("search")
            .with_name("Search music")
            .with_description("Search songs, albums, artists and playlists")
            .with_action_target("music.open-search");

        let mut page_content = Vec::new();
        if self.source_widget_mode() > 0 {
            page_content.push(
                AccessNode::new(Role::Group)
                    .with_hashed_key("music-source-status")
                    .with_name(self.source_status())
                    .with_description(self.source_error.clone().unwrap_or_default())
                    .with_state(
                        AccessState::new().busy(
                            self.source_invocation.is_some() || !self.spotify_jobs.is_empty(),
                        ),
                    ),
            );
        }
        // The custom-painted empty-state button is page content, not a
        // transport control. Give it a separate semantic group and route it to
        // the same reducer as the visible button.
        if matches!(self.view, View::Home)
            && !self.home_is_loading()
            && self.lib.playlists.is_empty()
            && self.lib.albums.is_empty()
            && self.lib.tracks.is_empty()
            && self
                .home
                .as_ref()
                .is_none_or(crate::backend::HomeContent::is_empty)
        {
            if !self.catalogue_connected && super::ONLINE_CATALOGUE {
                page_content.push(
                    AccessNode::new(Role::Group)
                        .with_hashed_key("home-empty")
                        .with_name(super::HOME_EMPTY_TITLE)
                        .with_description(super::HOME_EMPTY_MESSAGE)
                        .with_children(vec![
                            AccessNode::new(Role::Button)
                                .with_name(super::HOME_EMPTY_ACTION)
                                .with_description("Choose where your music comes from")
                                .with_action_target("music.connect-source"),
                        ]),
                );
            } else {
                let (message, offers_search) = super::first_run_copy(super::ONLINE_CATALOGUE);
                let search = offers_search.then(|| {
                    AccessNode::new(Role::Button)
                        .with_name("Search music")
                        .with_description("Search for a song, album, or artist")
                        .with_action_target("music.open-search")
                });
                page_content.push(
                    AccessNode::new(Role::Group)
                        .with_hashed_key("library-empty")
                        .with_name("Empty music library")
                        .with_description(message)
                        .with_children(search.into_iter().collect()),
                );
            }
        }
        let page_name = match &self.view {
            View::Home => "Home".to_string(),
            View::Recommendations => crate::i18n::t("music-recommendations-title"),
            View::Songs => "Songs".to_string(),
            View::Albums => "Albums".to_string(),
            View::Artists => "Artists".to_string(),
            View::Playlists => "Playlists".to_string(),
            View::Downloads => "Downloads".to_string(),
            View::Liked => "Liked songs".to_string(),
            View::Browse => "Browse".to_string(),
            View::Queue => "Queue".to_string(),
            View::Lyrics => "Lyrics".to_string(),
            View::NowPlaying => "Now playing".to_string(),
            View::Search => "Search".to_string(),
            View::Kept(id) => self
                .kept_views()
                .get(id)
                .map_or_else(|| "Search".to_string(), |kept| kept.name.clone()),
            View::Album(id) => self.album_pages.get(id).map_or_else(
                || self.lib.album_title(id).to_string(),
                |page| page.title.clone(),
            ),
            View::Artist(id) => self.artist_pages.get(id).map_or_else(
                || self.lib.artist_name(id).to_string(),
                |page| page.name.clone(),
            ),
            View::Playlist(id) => self
                .playlist_pages
                .get(id)
                .map(|page| page.title.clone())
                .or_else(|| {
                    self.lib
                        .playlists()
                        .find(|playlist| playlist.id == *id)
                        .map(|playlist| playlist.name.clone())
                })
                .unwrap_or_else(|| "Playlist".to_string()),
        };

        let mut list_items = Vec::new();
        match &self.view {
            View::Songs => {
                let items = self.lib.tracks_sorted();
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, track)| accessible_track(&self.lib, track, index + 1, total)),
                );
            }
            View::Downloads => {
                let mut items = self
                    .device_cache
                    .iter()
                    .filter(|(_, state)| !matches!(state, CacheState::Streamed))
                    .filter_map(|(id, state)| self.lib.track(id).map(|track| (track, *state)))
                    .collect::<Vec<_>>();
                let failed = self
                    .offline_errors
                    .keys()
                    .filter(|id| !items.iter().any(|(track, _)| track.id == **id))
                    .filter_map(|id| {
                        self.lib
                            .track(id)
                            .map(|track| (track, CacheState::Streamed))
                    })
                    .collect::<Vec<_>>();
                items.extend(failed);
                items.sort_by(|(left, _), (right, _)| left.title.cmp(&right.title));
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, (track, state))| {
                            accessible_download(
                                &self.lib,
                                track,
                                state,
                                self.offline_errors.get(&track.id).map(String::as_str),
                                index + 1,
                                total,
                            )
                        }),
                );
            }
            View::Liked => {
                let items = self
                    .lib
                    .liked
                    .iter()
                    .filter_map(|id| self.lib.track(id))
                    .collect::<Vec<_>>();
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, track)| accessible_track(&self.lib, track, index + 1, total)),
                );
            }
            View::Queue => {
                let items = self
                    .pb
                    .queue
                    .iter()
                    .filter_map(|id| self.playback_track(id))
                    .collect::<Vec<_>>();
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, track)| accessible_track(&self.lib, track, index + 1, total)),
                );
            }
            View::Album(id) => {
                let items = self.lib.album_tracks(id);
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, track)| accessible_track(&self.lib, track, index + 1, total)),
                );
            }
            View::Artist(id) => {
                let items = self.lib.artist_albums(id);
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, album)| accessible_album(&self.lib, album, index + 1, total)),
                );
            }
            View::Playlist(id) => {
                if let Some(playlist) = self.lib.playlists().find(|playlist| playlist.id == *id) {
                    list_items.extend(
                        playlist
                            .tracks
                            .values()
                            .filter_map(|track| self.lib.track(track))
                            .take(ACCESSIBLE_PAGE_LIMIT)
                            .enumerate()
                            .map(|(index, track)| {
                                accessible_playlist_track(&self.lib, playlist, track, index + 1)
                            }),
                    );
                }
            }
            View::Albums => {
                let items = self.lib.albums_sorted();
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, album)| accessible_album(&self.lib, album, index + 1, total)),
                );
            }
            View::Artists => {
                let items = self.lib.artists_sorted();
                let total = items.len();
                list_items.extend(
                    items
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, artist)| accessible_artist(artist, index + 1, total)),
                );
            }
            View::Playlists => {
                page_content.push(
                    AccessNode::new(Role::Button)
                        .with_hashed_key("new-playlist")
                        .with_name("New playlist")
                        .with_action_target("music.new-playlist"),
                );
                let total = self.lib.playlists.len();
                list_items.extend(
                    self.lib
                        .playlists()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, playlist)| accessible_playlist(playlist, index + 1, total)),
                );
            }
            View::Home => {
                let albums = self.lib.albums_sorted();
                let tracks = self.lib.top_tracks(6);
                let total = self.lib.playlists.len() + albums.len().min(6) + tracks.len();
                list_items.extend(
                    self.lib
                        .playlists()
                        .enumerate()
                        .map(|(index, playlist)| accessible_playlist(playlist, index + 1, total)),
                );
                let used = list_items.len();
                list_items.extend(
                    albums
                        .into_iter()
                        .take(6)
                        .take(ACCESSIBLE_PAGE_LIMIT.saturating_sub(used))
                        .enumerate()
                        .map(|(index, album)| {
                            accessible_album(&self.lib, album, used + index + 1, total)
                        }),
                );
                let used = list_items.len();
                list_items.extend(
                    tracks
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT.saturating_sub(used))
                        .enumerate()
                        .map(|(index, track)| {
                            accessible_track(&self.lib, track, used + index + 1, total)
                        }),
                );
            }
            View::Browse => {
                let mut genres: Vec<&str> = self
                    .lib
                    .artists
                    .values()
                    .map(|artist| artist.genre.as_str())
                    .filter(|genre| !genre.is_empty())
                    .collect();
                genres.sort_unstable();
                genres.dedup();
                let total = genres.len();
                list_items.extend(
                    genres
                        .into_iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, genre)| {
                            AccessNode::new(Role::ListItem)
                                .with_hashed_key(format!("genre:{genre}"))
                                .with_name(genre)
                                .with_description(format!("Search for {genre}"))
                                .with_position(index + 1, total)
                        }),
                );
            }
            View::Lyrics => {
                page_content.extend(
                    self.lyrics
                        .iter()
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, line)| {
                            AccessNode::new(Role::Paragraph)
                                .with_hashed_key(format!("lyric:{index}"))
                                .with_name(&line.text)
                                .with_position(index + 1, self.lyrics.len())
                        }),
                );
            }
            View::NowPlaying => page_content.push(
                AccessNode::new(Role::Paragraph)
                    .with_hashed_key("now-playing")
                    .with_name(self.now_playing.spoken()),
            ),
            // A query view's rows are its track lists' and cards' own.
            View::Recommendations | View::Search | View::Kept(_) => {}
        }
        if !list_items.is_empty() {
            page_content.push(
                AccessNode::new(Role::List)
                    .with_hashed_key("page-items")
                    .with_name(format!("{page_name} items"))
                    .with_children(list_items),
            );
        }
        page_content.extend(self.page_parts.iter().flat_map(PagePart::access_nodes));
        let page = AccessNode::new(Role::TabPanel)
            .with_hashed_key(format!("page:{page_name}"))
            .with_name(page_name)
            .with_children(page_content);

        let mut children = vec![search, page, player];
        children.extend(
            self.toast
                .access_node()
                .map(|toast| toast.with_hashed_key("toast")),
        );
        Some(
            AccessNode::new(Role::Group)
                .with_name("Music")
                .with_description("Music library and playback controls")
                .with_children(children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all synthetic Music targets are validated and routed together so no assistive action bypasses the normal reducer"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if let Some(id) = target.strip_prefix("music.page-widget.") {
            let handled = self
                .page_parts
                .iter_mut()
                .any(|part| part.perform(id, action, state));
            let asked = self
                .page_parts
                .iter()
                .flat_map(PagePart::take_asked)
                .collect::<Vec<_>>();
            for action in asked {
                self.dispatch(action, state);
            }
            self.dirty |= handled;
            return handled;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let action = match target {
            "music.open-search" => Action::OpenSearch,
            "music.connect-source" => Action::SpotifyEffect(crate::actions::MusicEffect::Sources),
            "music.new-playlist" => Action::BeginNewPlaylist,
            _ => {
                if let Some(id) = target.strip_prefix("music.nav.") {
                    let Some(view) = Self::top_level_view(id) else {
                        return false;
                    };
                    Action::Nav(view)
                } else if let Some(id) = target.strip_prefix("music.play-track.") {
                    if self.playback_track(id).is_none() {
                        return false;
                    }
                    Action::Play {
                        queue: vec![id.to_string()].into(),
                        index: 0,
                    }
                } else if let Some(id) = target.strip_prefix("music.toggle-like.") {
                    if self.lib.track(id).is_none() {
                        return false;
                    }
                    Action::ToggleLike(id.to_string())
                } else if let Some(id) = target.strip_prefix("music.retry-offline.") {
                    if self.lib.track(id).is_none() || !self.offline_errors.contains_key(id) {
                        return false;
                    }
                    Action::SetOffline {
                        track: id.to_string(),
                        keep: true,
                    }
                } else if let Some(id) = target.strip_prefix("music.remove-offline.") {
                    if self.lib.track(id).is_none() || !self.device_cache.contains_key(id) {
                        return false;
                    }
                    Action::SetOffline {
                        track: id.to_string(),
                        keep: false,
                    }
                } else if let Some(id) = target.strip_prefix("music.open-album.") {
                    if self.lib.album(id).is_none() {
                        return false;
                    }
                    Action::Nav(View::Album(id.to_string()))
                } else if let Some(id) = target.strip_prefix("music.open-artist.") {
                    if self.lib.artist(id).is_none() {
                        return false;
                    }
                    Action::Nav(View::Artist(id.to_string()))
                } else if let Some(id) = target.strip_prefix("music.open-playlist.") {
                    let known_local = self.lib.playlists().any(|playlist| playlist.id == id);
                    let known_live = self.live_bundle.as_ref().is_some_and(|(_, bundle)| {
                        bundle.playlists.iter().any(|playlist| playlist.id == id)
                    });
                    if !known_local && !known_live {
                        return false;
                    }
                    Action::Nav(View::Playlist(id.to_string()))
                } else if let Some(resource) = target.strip_prefix("music.rename-playlist.") {
                    let Some(id) = resource.strip_prefix("music.playlist:") else {
                        return false;
                    };
                    if self.lib.playlist(id).is_none() {
                        return false;
                    }
                    Action::BeginRenamePlaylist(id.to_string())
                } else if let Some(resource) = target.strip_prefix("music.delete-playlist.") {
                    let Some(id) = resource.strip_prefix("music.playlist:") else {
                        return false;
                    };
                    if self.lib.playlist(id).is_none() {
                        return false;
                    }
                    Action::DeletePlaylist(id.to_string())
                } else if let Some(resource) = target.strip_prefix("music.remove-playlist-track.") {
                    let Some((playlist, track)) =
                        crate::actions::parse_playlist_track_resource_id(resource)
                    else {
                        return false;
                    };
                    Action::RemoveFromPlaylist { playlist, track }
                } else if let Some((resource, delta)) = target
                    .strip_prefix("music.move-up-playlist-track.")
                    .map(|resource| (resource, -1isize))
                    .or_else(|| {
                        target
                            .strip_prefix("music.move-down-playlist-track.")
                            .map(|resource| (resource, 1))
                    })
                {
                    let Some((playlist, track)) =
                        crate::actions::parse_playlist_track_resource_id(resource)
                    else {
                        return false;
                    };
                    let Some(from) = self.lib.playlist(&playlist).and_then(|playlist| {
                        playlist
                            .tracks
                            .position(&ui::tilo_resource::Id::from_raw(track.as_str()))
                    }) else {
                        return false;
                    };
                    let to = from.saturating_add_signed(delta);
                    Action::MovePlaylistItem { playlist, from, to }
                } else {
                    return false;
                }
            }
        };
        self.dispatch(action, state);
        true
    }

    fn tooltip(&self) -> Option<&str> {
        self.tooltip_at(self.mouse)
    }

    fn needs_animation_frame(&self) -> bool {
        // Settled playback needs observation updates, not a full repaint at
        // every display refresh. Leave the executor time to serve its bounded
        // PCM output lane, especially when the renderer uses the CPU.
        // Startup, worker replies and visible animations still wake promptly.
        (self.pb.playing && ui::time::now() >= self.last_tick + super::PLAYBACK_POLL_INTERVAL)
            || self.audio.needs_poll()
            || self.resource_stream_invocation.is_some()
            || !self.effect_replies.is_empty()
            || self.local_file_pick.is_some()
            || self.live_search_pending.is_some()
            || self.search.needs_frame()
            || self.search.highlight.is_some()
            || self.now_playing.in_flight(ui::time::now())
            || self.track_tables.iter().any(|slot| slot.table.rows_moving())
            || self.source_invocation.is_some()
            || !self.spotify_jobs.is_empty()
            || self.pending_catalogue_query.is_some()
            || self.resolve_in_flight > 0
            || self.images_pending()
            || self.lyrics_pending_for.is_some()
            || self.search_edit_at.is_some()
            || self.home_is_loading()
            || self.detail_loading()
            // Keep frames flowing so the download spinner spins + progresses.
            || !self.downloads.is_empty()
            || !self.offline_resolves.is_empty()
            || self
                .frame_theme
                .as_ref()
                .is_some_and(|theme| self.body_scroll.needs_animation_frame(theme))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.pb
            .playing
            .then_some(self.last_tick + super::PLAYBACK_POLL_INTERVAL)
            .into_iter()
            .chain(self.toast.deadline())
            .chain(self.search.next_placeholder_step(ui::time::now()))
            .chain(
                self.children()
                    .into_iter()
                    .filter_map(Widget::next_wake_deadline),
            )
            .min()
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.body_scroll.primary_vertical_scroll()
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.dirty = true;
        self.body_scroll.apply_primary_vertical_scroll(action)
    }

    fn has_pending_visual_work(&self) -> bool {
        // A capture waits for the covers it shows.
        !self.images.is_idle()
    }

    /// A glide after a flick scrolls the body as the wheel does; it
    /// changes no model, so it asks for no sync.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.dirty |= self.body_scroll.advance();
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn needs_redraw(&self) -> bool {
        // `render_frame` clears this after painting.  Returning `true` here
        // used to wake an otherwise paused, settled library at display rate,
        // duplicating the work already requested by `needs_animation_frame`
        // for playback, downloads, worker results, and transient UI.
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        // Recording a frame can outlast the playback interval. Start the
        // next interval after that work so a slow paint cannot immediately
        // request another paint and crowd out the output worker.
        self.last_tick = ui::time::now();
    }
}

impl MusicApp {
    fn route_source_widgets(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Ok(nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let mode = self.source_widget_mode();
        let count = match mode {
            1..=3 => 1,
            _ => return,
        };
        let nodes = nodes.iter().skip(nodes.len().saturating_sub(count));
        if matches!(mode, 2 | 3) {
            for (button, node) in self
                .source_buttons
                .iter_mut()
                .filter(|button| {
                    button.test_id()
                        == Some(if mode == 3 {
                            "music-spotify-create-playlist"
                        } else {
                            "music-sources"
                        })
                })
                .zip(nodes)
            {
                if !*ctx.handled {
                    button.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
                }
            }
        } else if let Some(node) = nodes.last()
            && let Some(button) = self
                .source_buttons
                .iter_mut()
                .find(|button| button.test_id() == Some("music-spotify-queue-refresh"))
            && !*ctx.handled
        {
            button.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
        }
    }

    fn begin_source_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: &crate::actions::MusicEffect,
    ) -> Result<ui::framework::Effected, String> {
        use crate::actions::MusicEffect;
        if !self.allow_network {
            return Err("Network access is unavailable in this session".into());
        }
        if self.source_invocation.is_some() || !self.spotify_jobs.is_empty() {
            return Err("A music source request is already running".into());
        }
        self.ensure_worker();
        let request = if matches!(effect, MusicEffect::DisconnectSpotify) {
            crate::stream::StreamRequest::DisconnectSpotify
        } else if matches!(effect, MusicEffect::ConnectSpotify) {
            crate::stream::StreamRequest::ConnectSpotify
        } else if matches!(effect, MusicEffect::ConnectSpotifyPlayback) {
            crate::stream::StreamRequest::ConnectSpotifyPlayback
        } else {
            crate::stream::StreamRequest::Source {
                spotify: !matches!(effect, MusicEffect::BrowseYouTube),
                sign_in: matches!(effect, MusicEffect::ConnectSpotifyLibrary),
            }
        };
        self.worker
            .as_ref()
            .ok_or("Music worker unavailable")?
            .request(request)?;
        self.source_error = None;
        self.source_invocation = Some(invocation);
        self.source_disconnect_pending = matches!(effect, MusicEffect::DisconnectSpotify);
        self.dirty = true;
        Ok(ui::framework::Effected::Pending)
    }

    /// Start `effect`, invoked as `invocation`. A view, a queue edit or a
    /// transport change that needs no new source answers at once; starting
    /// a stream, a download or the file picker answers once that work
    /// settles, through [`Self::take_effect_result`].
    ///
    /// # Errors
    /// Why the effect does not apply to what this device shows and plays
    /// now: a target gone from the library, a stale queue occurrence, a
    /// stream start already in flight.
    pub(crate) fn begin_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: crate::actions::MusicEffect,
    ) -> Result<ui::framework::Effected, String> {
        use crate::actions::MusicEffect;
        if matches!(effect, MusicEffect::SetVolume { volume } if volume > 0) {
            // A volume above zero ends a mute, as the slider's does.
            self.premute_volume = None;
        }
        match effect {
            MusicEffect::PlaybackStatus => {
                Ok(ui::framework::Effected::Done(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "track": self.pb.current,
                        "queue": self.pb.queue,
                        "queue_index": self.pb.queue_index,
                        "volume": self.pb.volume,
                        "shuffle": self.pb.shuffle,
                        "repeat": self.pb.repeat.as_str(),
                        "requested_playing": self.pb.playing,
                        "confirmed_playing": self.audio.is_playing_real(),
                        "control_pending": self.audio.is_control_pending(),
                        "resolving": self.active_resolve_request.is_some(),
                        "spotify_stage": self.audio.spotify_startup_stage(),
                        "rendered_buffers": self.audio.frames_rendered(),
                        "rendered_counter_unit": if media::audio_output::is_governed() { "host_pcm_frames" } else { "sink_buffers" },
                        "sink": self.audio.actual_audio_sink_name(),
                        "position_ms": self.observed_position_ms(),
                        "duration_ms": self.observed_duration_ms(),
                        "error": self.playback_error,
                    })),
                    resources: Vec::new(),
                }))
            }
            MusicEffect::Sources => {
                self.open_sources_menu();
                Ok(ui::framework::Effected::done())
            }
            MusicEffect::ConnectSpotifyLibrary
            | MusicEffect::ConnectSpotify
            | MusicEffect::ConnectSpotifyPlayback
            | MusicEffect::DisconnectSpotify
            | MusicEffect::BrowseSpotify
            | MusicEffect::BrowseYouTube => self.begin_source_effect(invocation, &effect),
            MusicEffect::SpotifyAccountStatus
            | MusicEffect::SpotifyDevices
            | MusicEffect::SpotifyQueue
            | MusicEffect::SpotifyLibraryMore
            | MusicEffect::SpotifyCreatePlaylist { .. }
            | MusicEffect::SpotifyTransfer { .. }
            | MusicEffect::SpotifyRemotePlay { .. }
            | MusicEffect::SpotifyRemotePause { .. }
            | MusicEffect::SpotifySave { .. }
            | MusicEffect::SpotifyUnsave { .. }
            | MusicEffect::SpotifyRenamePlaylist { .. }
            | MusicEffect::SpotifyPlaylistAdd { .. }
            | MusicEffect::SpotifyPlaylistRemove { .. } => {
                self.begin_spotify_operation(invocation, &effect)
            }
            MusicEffect::NewSpotifyPlaylist => {
                if !self.spotify_catalogue {
                    return Err(crate::i18n::t("music-spotify-select-first"));
                }
                self.on_begin_new_playlist(None);
                self.name_prompt = Some(super::NamePrompt::NewSpotifyPlaylist);
                self.dirty = true;
                Ok(ui::framework::Effected::done())
            }
            other => self.begin_player_effect(invocation, other),
        }
    }

    fn begin_player_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: crate::actions::MusicEffect,
    ) -> Result<ui::framework::Effected, String> {
        use crate::actions::{MusicEffect, Place};
        let done = ui::framework::Effected::done();
        match effect {
            MusicEffect::Search { query } => {
                if !self.allow_network {
                    return Err("Catalogue search is unavailable in this view".into());
                }
                self.complete_catalogue_search(Err(
                    "A newer catalogue query superseded this request".into(),
                ));
                self.search_query = query;
                self.request_live_search();
                if self.active_search_request.is_none() {
                    return Err(self.search_error.as_ref().map_or_else(
                        || "Catalogue worker unavailable".into(),
                        |(_, error)| error.clone(),
                    ));
                }
                self.catalogue_invocation = Some(invocation);
                Ok(ui::framework::Effected::Pending)
            }
            MusicEffect::ToggleLike { .. } | MusicEffect::ToggleLikePlaying => {
                Err("Catalogue likes require the admitted resource effect".into())
            }
            MusicEffect::OpenFile => {
                self.begin_local_file_pick(invocation)?;
                Ok(ui::framework::Effected::Pending)
            }
            MusicEffect::Open { place } => self.show(place).map(|()| done),
            MusicEffect::OpenAlbum { album } => self.show(Place::Album(album)).map(|()| done),
            MusicEffect::OpenArtist { artist } => self.show(Place::Artist(artist)).map(|()| done),
            MusicEffect::Play { track, queued } => {
                self.supersede_pending_stream_start();
                let request = match queued {
                    None => self.apply_resource_play_track(&track)?,
                    Some(index) => self.apply_resource_play_queue_item(index, &track)?,
                };
                self.register_playback_start(invocation, request, 0, false)
            }
            MusicEffect::KeepOffline { tracks } => self.begin_offline_downloads(invocation, tracks),
            MusicEffect::RemoveOffline { tracks } => {
                if tracks.iter().any(|id| !self.lib.tracks.contains_key(id)) {
                    return Err("offline-cache target is no longer in the library".into());
                }
                // Do all device-local object removals before refreshing the
                // projection; the manifest itself is atomically replaced.
                for id in tracks {
                    if let Some(download) = self.downloads.remove(&id) {
                        download.cancel();
                    }
                    self.offline_resolves.retain(|_, pending| pending != &id);
                    self.offline_errors.remove(&id);
                    self.media_cache
                        .remove(&self.media_cache_root, &id)
                        .map_err(|error| format!("could not remove offline media: {error}"))?;
                    self.update_device_cache(&id, CacheState::Streamed);
                    self.settle_offline_track(&id, Some("offline download cancelled".into()));
                }
                self.dirty = true;
                Ok(done)
            }
            MusicEffect::Remove { index, track } => {
                self.apply_resource_remove_queue_item(index, &track)?;
                self.dirty = true;
                Ok(done)
            }
            MusicEffect::MoveUp { index, track } => {
                self.apply_resource_move_queue_item(index, &track, index.saturating_sub(1))?;
                self.dirty = true;
                Ok(done)
            }
            MusicEffect::MoveDown { index, track } => {
                self.apply_resource_move_queue_item(index, &track, index.saturating_add(1))?;
                self.dirty = true;
                Ok(done)
            }
            // Next, previous and mute take the steps the person's own
            // controls take ([`Self::stepped_playback`]).
            step @ (MusicEffect::Next | MusicEffect::Previous | MusicEffect::ToggleMute) => {
                let next = self.stepped_playback(&step);
                self.dirty = true;
                self.start_or_apply_playback(invocation, next)
            }
            transport => self.begin_transport_effect(invocation, &transport),
        }
    }

    /// A transport or queue effect that only changes this device's playback.
    fn begin_transport_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        transport: &crate::actions::MusicEffect,
    ) -> Result<ui::framework::Effected, String> {
        let (mut next, edit) = crate::actions::next_playback(transport, &self.pb, &|id| {
            self.playback_track(id).is_some()
        })
        .ok_or_else(|| {
            format!(
                "`{}` is not a playback effect",
                ui::tilo_resource::Effect::id(transport)
            )
        })??;
        self.dirty = true;
        // The shuffled order follows the edit exactly as the person's own
        // queue edits move it: a song put next is heard next, and what has
        // played stays played.
        let before = edit.and_then(|edit| self.follow_shuffled_edit(edit, &mut next));
        let effected = self.start_or_apply_playback(invocation, next);
        if effected.is_err()
            && let Some(before) = before
            && self.shuffle_order.len() != self.pb.queue.len()
        {
            self.shuffle_order = before;
        }
        effected
    }

    /// Move to `next`: only starting a source waits for the audio it starts.
    fn start_or_apply_playback(
        &mut self,
        invocation: tilo_actions::InvocationId,
        next: crate::model::Playback,
    ) -> Result<ui::framework::Effected, String> {
        if next.playing && (self.pb.current != next.current || !self.pb.playing) {
            self.begin_resource_playback(invocation, next)
        } else {
            self.apply_resource_playback(next)?;
            Ok(ui::framework::Effected::done())
        }
    }

    fn begin_offline_downloads(
        &mut self,
        invocation: tilo_actions::InvocationId,
        tracks: Vec<String>,
    ) -> Result<ui::framework::Effected, String> {
        if self.offline_invocation.is_some() {
            return Err("another offline download operation is running".into());
        }
        let pending = tracks
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if pending.is_empty() {
            return Err("offline download requires at least one track".into());
        }
        if pending.iter().any(|id| !self.lib.tracks.contains_key(id)) {
            return Err("offline-cache target is no longer in the library".into());
        }
        for track_id in &pending {
            self.request_offline_download(track_id)?;
        }
        self.offline_invocation = Some((invocation, pending));
        self.dirty = true;
        Ok(ui::framework::Effected::Pending)
    }

    /// Start the effect a published operation's `payload` names, as the
    /// host does when it is invoked.
    #[cfg(test)]
    pub(crate) fn start_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
    ) -> Result<ui::framework::Effected, String> {
        let effect =
            <crate::actions::MusicEffect as ui::tilo_resource::Effect>::from_payload(payload)?;
        self.begin_effect(invocation, effect)
    }

    /// Show `place`, once it is still in the library.
    ///
    /// # Errors
    /// Why `place` is not there to show any more.
    pub(super) fn show(&mut self, place: crate::actions::Place) -> Result<(), String> {
        use crate::actions::Place;
        match place {
            Place::View(id) => {
                // A forged destination must not acquire arbitrary local
                // routing authority: only the exported destinations open.
                if crate::actions::known_view_label(&id).is_none() {
                    return Err("unknown Music destination".into());
                }
                let view = Self::top_level_view(&id)
                    .ok_or_else(|| "unsupported Music destination".to_string())?;
                self.on_nav(view);
            }
            Place::Album(id) => {
                if !self.lib.albums.contains_key(&id)
                    && !self.known_catalogue_entity(&crate::actions::album_resource_id(&id))
                {
                    return Err("album is no longer in the library".into());
                }
                self.on_nav(View::Album(id));
            }
            Place::Artist(id) => {
                if !self.lib.artists.contains_key(&id)
                    && !self.known_catalogue_entity(&crate::actions::artist_resource_id(&id))
                {
                    return Err("artist is no longer in the library".into());
                }
                self.on_nav(View::Artist(id));
            }
            Place::Playlist(id) => {
                if self.lib.playlist(&id).is_none()
                    && !self.known_catalogue_entity(&crate::actions::playlist_resource_id(&id))
                {
                    return Err("playlist is no longer in the library".into());
                }
                self.on_nav(View::Playlist(id));
            }
            Place::Membership { playlist, track } => {
                let Some(current_playlist) = self.lib.playlist(&playlist) else {
                    return Err("playlist is no longer in the library".into());
                };
                if !current_playlist.contains(&track) {
                    return Err("playlist membership is no longer present".into());
                }
                self.on_nav(View::Playlist(playlist.clone()));
                self.revealed_membership = Some((playlist, track));
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// The outcome of the effect invoked as `invocation`, once the stream,
    /// download or picker it started has settled; taken once.
    pub(crate) fn take_effect_result(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.expire_resource_stream_if_due(ui::time::now());
        self.resource_stream_results
            .iter()
            .position(|update| update.invocation() == invocation)
            .map(|index| self.resource_stream_results.remove(index))
            .or_else(|| {
                self.offline_results
                    .iter()
                    .position(|update| update.invocation() == invocation)
                    .map(|index| self.offline_results.remove(index))
            })
    }

    /// Answer `invocation`'s caller through `reply` once its work settles.
    pub(crate) fn await_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.effect_replies.push((invocation, reply));
    }

    /// Answer every waiting caller whose work has settled.
    pub(super) fn deliver_effect_replies(&mut self) {
        let waiting = std::mem::take(&mut self.effect_replies);
        for (invocation, reply) in waiting {
            match self.take_effect_result(invocation) {
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                    reply.answer(Ok(output));
                }
                Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                    reply.answer(Err(error));
                }
                Some(tilo_actions::InvocationUpdate::Cancelled { .. }) => {
                    reply.answer(Ok(tilo_actions::ActionOutput::default()));
                }
                Some(_) | None => self.effect_replies.push((invocation, reply)),
            }
        }
    }
}

impl MusicApp {
    /// Pointer and key input the header, the sidebar and the scroll region
    /// left for the page.
    fn on_input(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.search.drag.is_some() && self.drag_input(event, ctx) {
            return;
        }
        if self.route_page_widgets(event, ctx) {
            if let WindowEvent::CursorMoved { .. } = event {
                self.on_cursor_moved(event, ctx.scale_factor);
            }
            self.dirty = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.on_cursor_moved(event, ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(&mut ctx.state, ctx.handled);
            }
            // A mouse's own Back and Forward buttons walk the view stack.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Back,
                ..
            } => {
                self.go_back();
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Forward,
                ..
            } => {
                self.go_forward();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                self.on_key(
                    logical_key,
                    text.as_deref(),
                    *ctx.modifiers,
                    &mut ctx.state,
                    ctx.handled,
                );
            }
            _ => {}
        }
    }

    /// Pointer and key input while songs are dragged onto a playlist: the
    /// pointer carries them, a release drops them, Escape lets them go.
    /// Returns whether the drag took the event.
    fn drag_input(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at = ctx.logical(position);
                self.mouse = at;
                if let Some(drag) = self.search.drag.as_mut() {
                    drag.at = at;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.drop_tracks(&mut ctx.state),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.search.drag = None,
            _ => return false,
        }
        self.dirty = true;
        *ctx.handled = true;
        true
    }

    /// Hover labels for icon-only controls. Text-labelled rows and navigation
    /// deliberately return `None`; repeating their visible label would add
    /// noise rather than information.
    fn tooltip_at(&self, point: Point) -> Option<&str> {
        if let Some(name) = &self.table_tooltip {
            return Some(name.as_str());
        }
        self.hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(point))
            .and_then(|hit| match &hit.action {
                Action::Back => Some("Back"),
                Action::TogglePlay => Some(if self.pb.playing { "Pause" } else { "Play" }),
                Action::Next => Some("Next track"),
                Action::Prev => Some("Previous track"),
                Action::ToggleShuffle => Some("Shuffle"),
                Action::CycleRepeat => Some("Change repeat mode"),
                Action::ToggleMute => Some(if self.pb.volume == 0 {
                    "Unmute"
                } else {
                    "Mute"
                }),
                Action::OpenPlayerMenu(_) => Some("More controls"),
                Action::ToggleLike(track) => Some(if self.lib.is_liked(track) {
                    "Unlike"
                } else {
                    "Like"
                }),
                Action::SetOffline { track, keep: true } => {
                    if let Some(error) = self.offline_errors.get(track) {
                        Some(error.as_str())
                    } else {
                        Some("Keep offline")
                    }
                }
                Action::SetOffline { keep: false, .. } => Some("Cancel or remove offline download"),
                Action::ToggleQueue => Some("Show or hide queue"),
                Action::BeginNewPlaylist => Some("New playlist"),
                Action::Nav(View::Lyrics) => Some("Lyrics"),
                Action::MoveQueueItem { from, to } | Action::MovePlaylistItem { from, to, .. } => {
                    Some(if to < from { "Move up" } else { "Move down" })
                }
                Action::RemoveFromQueue(_) => Some("Remove from queue"),
                Action::RemoveFromPlaylist { .. } => Some("Remove from playlist"),
                _ => None,
            })
    }

    pub(super) fn hit_test(&self, p: Point) -> Option<Action> {
        for h in self.hits.iter().rev() {
            if h.rect.contains(p) {
                return Some(h.action.clone());
            }
        }
        None
    }

    /// Route an event to the open context menu; returns whether `handle_event`
    /// should return immediately (the menu consumed it, or an outside click
    /// dismissed it).
    fn route_context_menu(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let mut menu_handled = false;
        let mut ft = None;
        self.context.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut menu_handled,
                focus_target: &mut ft,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if self.drain_menu_selection(&mut ctx.state) {
            self.dirty = true;
            return true;
        }
        if menu_handled
            || matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
        {
            // The menu consumed the click (selection / outside-dismiss).
            self.dirty = true;
            return true;
        }
        false
    }

    /// Track the pointer.
    fn on_cursor_moved(&mut self, event: &WindowEvent, scale_factor: f64) {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.dirty = true;
    }

    /// Left press: the topmost hit under the pointer.
    fn on_left_press(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        // Topmost matching hit wins (later-drawn = on top).
        if let Some(action) = self.hit_test(self.mouse) {
            self.dispatch(action, state);
            *handled = true;
        }
    }

    /// Whether the Home page is genuinely still loading — a fetch is in flight
    /// and hasn't come back (nor failed). The single source of truth for the
    /// Home skeleton *and* the animation loop, so a failed/offline fetch stops
    /// both instead of shimmering (and spinning frames) forever.
    pub(crate) fn home_is_loading(&self) -> bool {
        self.home_requested && self.home.is_none() && !self.home_failed
    }

    /// Whether the current detail view is a real (live) artist/album page whose
    /// content hasn't landed yet — so its loading skeleton must keep animating.
    fn detail_loading(&self) -> bool {
        match &self.view {
            View::Artist(id) => {
                is_live_id(id)
                    && !self.artist_pages.contains_key(id)
                    && !self.lib.artists.contains_key(id)
                    && !self.failed_pages.contains(id)
            }
            View::Album(id) => {
                is_live_album_id(id)
                    && !self.album_pages.contains_key(id)
                    && !self.lib.albums.contains_key(id)
                    && !self.failed_pages.contains(id)
            }
            View::Playlist(id) => {
                is_live_playlist_id(id)
                    && !self.playlist_pages.contains_key(id)
                    && self.lib.playlist(id).is_none()
                    && !self.failed_pages.contains(id)
            }
            _ => false,
        }
    }

    fn on_key(
        &mut self,
        key: &Key,
        _text: Option<&str>,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        // The name-input overlay is modal and handled upstream in
        // `handle_event` (the shared ModalDialog owns typing / Esc / buttons),
        // so `on_key` never runs with it open.

        if self.result_key(key, state) {
            *handled = true;
            return;
        }
        // Alt with Left or Right walks the view stack, as in a browser.
        if modifiers.state().alt_key() {
            match key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.go_back();
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.go_forward();
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }
        match key {
            Key::Named(NamedKey::Space) => {
                self.dispatch(Action::TogglePlay, state);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if modifiers.state().shift_key() {
                    self.dispatch(Action::Next, state);
                } else {
                    let dur = self.current_duration_ms() as f64;
                    let f = if dur > 0.0 {
                        (self.playhead_ms + 5000.0) / dur
                    } else {
                        0.0
                    };
                    self.dispatch(Action::SeekFrac(f), state);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if modifiers.state().shift_key() {
                    self.dispatch(Action::Prev, state);
                } else {
                    let dur = self.current_duration_ms() as f64;
                    let f = if dur > 0.0 {
                        (self.playhead_ms - 5000.0) / dur
                    } else {
                        0.0
                    };
                    self.dispatch(Action::SeekFrac(f), state);
                }
                *handled = true;
            }
            Key::Character(c) => {
                let lc = c.to_lowercase();
                match lc.as_str() {
                    "n" => {
                        self.dispatch(Action::Next, state);
                        *handled = true;
                    }
                    "p" => {
                        self.dispatch(Action::Prev, state);
                        *handled = true;
                    }
                    "s" => {
                        self.dispatch(Action::ToggleShuffle, state);
                        *handled = true;
                    }
                    "r" => {
                        self.dispatch(Action::CycleRepeat, state);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---- artwork ----------------------------------------------------------

    /// Paint an entity's cover art (album / track / playlist / artist) into
    /// `rect`: the loaded image, else the toolkit's generated placeholder,
    /// which carries the initials of `title`. The face sets the title under
    /// its art; only the art is kept. `id` keys the cover cache.
    fn draw_cover(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str, title: &str) {
        let image = self.art_image(id);
        let art = image
            .as_ref()
            .map_or(MediaArt::Placeholder, MediaArt::Image);
        let face = MediaCardFace::new(art).title(title);
        let text_h = face.text_height(ctx.text, ctx.theme, rect.width());
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &rect,
        );
        face.paint(ctx, Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + text_h));
        ctx.scene.pop_layer();
    }

    /// A person's picture, round as people are: the artist's photo, else
    /// the neutral placeholder with their initials, in the disc that fills
    /// the shorter side of `rect` (a circular media card, art only).
    fn draw_portrait(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str, name: &str) {
        let side = rect.width().min(rect.height());
        let image = self.art_image(id);
        let art = image
            .as_ref()
            .map_or(MediaArt::Placeholder, MediaArt::Image);
        MediaCardFace::new(art)
            .aspect(ui::widget::MediaAspect::Circle)
            .seed(name)
            .paint(ctx, square_around(rect.center(), side));
    }

    // ---- controls ---------------------------------------------------------

    // ---- master frame -----------------------------------------------------

    pub(super) fn render_frame(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        self.hits.clear();
        self.track_tables_drawn = 0;
        self.media_grids_drawn = 0;
        self.page_parts_drawn = 0;
        self.sync_context_menu_geometry(ctx.theme);
        // Fold in any streaming-worker results (resolved stream URLs → audio
        // engine, real search hits → library), then keep live search results
        // visible after a `sync` reloaded the catalog from the document.
        self.drain_worker();
        self.inject_live_hits();
        self.poll_lyrics();
        self.maybe_prefetch();
        // Apply any deferred `--shots` staging now that `rect` is known.
        self.apply_shot_stage();
        self.tick();
        self.dirty |= self.toast.expire();

        // The shell holds the sidebar and the transport; this is the
        // content column.
        let look = Look::of(ctx.theme);
        let r = self.rect;
        // The toolbar is the window bar's: the page starts at the top.
        self.declare_window_toolbar();
        // The body scrolls in the toolkit's region; its content is laid out
        // beside the bar's lane while it overflows.
        self.body_scroll.set_viewport(r, ctx.theme);
        let body = self.body_scroll.content_rect();
        self.body_rect = body;
        self.follow_now_playing(ctx.theme, ui::time::now());

        // Body first so chrome draws on top (and chrome hits win). The window
        // ground is the framework's; nothing here fills it.
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let scrolled = self.scroll_y();
        let start_y = body.y0 - scrolled;
        let end_y = self.draw_body(ctx, &look, body, start_y);
        ctx.scene.pop_layer();
        self.body_hits = self.hits.len();
        self.track_tables.truncate(self.track_tables_drawn);
        self.media_grids.truncate(self.media_grids_drawn);
        self.page_parts.truncate(self.page_parts_drawn);
        self.content_height = (end_y + scrolled) - body.y0 + look.gutter;
        self.body_scroll
            .set_content_size(body.width(), self.content_height);
        // An offset asked for this frame (or during it) is clamped to the
        // content it was laid out against.
        if let Some(target) = self.scroll_target.take() {
            self.body_scroll
                .set_offset(vello::kurbo::Vec2::new(0.0, target));
        }
        self.body_scroll
            .paint_bars(ctx.scene, ctx.transform, ctx.theme);
        if self.frame_theme.as_ref().map(|theme| theme.name.as_str())
            != Some(ctx.theme.name.as_str())
            || self
                .frame_theme
                .as_ref()
                .is_some_and(|theme| (theme.text_scale - ctx.theme.text_scale).abs() > f32::EPSILON)
        {
            self.frame_theme = Some(ctx.theme.clone());
        }
        self.open_staged_track_menu();
        self.open_staged_facet();

        self.draw_toast(ctx);
        self.paint_highlight(ctx);

        if self.name_prompt.is_none() {
            self.name_dialog = None;
        }
    }

    /// Open a `--shots`-staged track menu over its row, once the row has been
    /// laid out this frame.
    fn open_staged_track_menu(&mut self) {
        let Some(track) = self.staged_menu_track.take() else {
            return;
        };
        let row = self.track_tables.iter().find_map(|slot| {
            let index = slot.table.rows().iter().position(|row| row.id == track)?;
            slot.table.row_rect(index)
        });
        if let Some(row) = row {
            self.mouse = Point::new(row.x0 + row.width() * 0.3, row.center().y);
            self.menu_anchor_row = Some(row);
            self.open_track_menu(track, None, None);
        }
    }

    // ---- sidebar ----------------------------------------------------------

    /// Where the window is: the destination's name, or the album, artist or
    /// playlist shown.
    pub(super) fn view_title(&self) -> String {
        match &self.view {
            View::Home => "Home".into(),
            View::Recommendations => crate::i18n::t("music-recommendations-title"),
            View::Songs => "Songs".into(),
            View::Albums => "Albums".into(),
            View::Artists => "Artists".into(),
            View::Playlists => "Playlists".into(),
            View::Downloads => "Downloads".into(),
            View::Liked => "Liked songs".into(),
            View::Browse => "Browse".into(),
            View::Queue => "Queue".into(),
            View::Lyrics => "Lyrics".into(),
            View::NowPlaying => "Now playing".into(),
            View::Search => "Search".into(),
            View::Kept(id) => self
                .kept_views()
                .get(id)
                .map_or_else(|| "Search".to_string(), |kept| kept.name.clone()),
            View::Album(id) => self
                .album_pages
                .get(id)
                .map_or_else(|| self.lib.album_title(id).to_string(), |p| p.title.clone()),
            View::Artist(id) => self
                .artist_pages
                .get(id)
                .map(|p| p.name.clone())
                .or_else(|| self.lib.artist(id).map(|a| a.name.clone()))
                .unwrap_or_else(|| "Artist".to_string()),
            View::Playlist(id) => self
                .lib
                .playlist(id)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
        }
    }

    // ---- body dispatch ----------------------------------------------------

    fn draw_body(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        match self.view.clone() {
            View::Home => self.draw_home(ctx, look, body, y),
            View::Recommendations => self.draw_recommendations(
                ctx,
                (body.x0 + look.gutter, body.x1 - look.gutter),
                y + look.gutter,
                true,
            ),
            View::Songs => {
                let ids: Vec<String> = self
                    .lib
                    .tracks_sorted()
                    .iter()
                    .map(|tk| tk.id.clone())
                    .collect();
                if ids.is_empty() {
                    return self.empty_library_page(ctx, body, y, "No songs yet");
                }
                let rows = self.gather_tracks(&ids);
                let y = y + look.section;
                self.draw_track_table(ctx, look, body, y, &TrackTable::new(&rows, &ids))
            }
            View::Albums => {
                let albums = self.gather_albums(None);
                if albums.is_empty() {
                    return self.empty_library_page(ctx, body, y, "No albums yet");
                }
                self.draw_album_grid(ctx, look, body, y + look.gutter, &albums)
            }
            View::Artists => {
                let artists = self.gather_artists();
                if artists.is_empty() {
                    return self.empty_library_page(ctx, body, y, "No artists yet");
                }
                self.draw_artist_grid(ctx, look, body, y + look.gutter, &artists)
            }
            View::Playlists => {
                let pls = self.gather_playlists();
                self.draw_playlist_grid(ctx, look, body, y + look.gutter, &pls)
            }
            View::Downloads => self.draw_downloads(ctx, look, body, y),
            View::Liked => self.draw_liked(ctx, look, body, y),
            View::Queue => self.draw_queue(ctx, look, body, y),
            View::Lyrics => self.draw_lyrics(ctx, look, body, y),
            View::NowPlaying => self.draw_now_playing(ctx, look, body, y),
            View::Browse => self.draw_browse(ctx, look, body, y),
            View::Album(id) => self.draw_album_detail(ctx, look, body, y, &id),
            View::Artist(id) => self.draw_artist_detail(ctx, look, body, y, &id),
            View::Playlist(id) => self.draw_playlist_detail(ctx, look, body, y, &id),
            View::Search | View::Kept(_) => self.draw_query_view(ctx, look, body, y),
        }
    }

    /// A section heading on the content gutter. Returns the y below it.
    fn section_heading(
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        label: &str,
    ) -> f64 {
        let style = ctx.theme.text(TextRole::Heading);
        let h = text_line(
            ctx,
            label,
            &style,
            look.text,
            Point::new(body.x0 + look.gutter, y),
            body.width() - 2.0 * look.gutter,
        );
        y + h + look.sibling
    }

    /// A one-sentence note on the content gutter (an empty list, a status).
    /// Returns the y below it.
    fn note(ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64, text: &str) -> f64 {
        let style = ctx.theme.text(TextRole::Body);
        let h = text_line(
            ctx,
            text,
            &style,
            look.dim,
            Point::new(body.x0 + look.gutter, y),
            body.width() - 2.0 * look.gutter,
        );
        y + h
    }

    /// An empty or failed page: the toolkit's empty state centred in the
    /// rest of the body, with its one action registered. Returns the body's
    /// bottom (the empty state owns the page).
    fn empty_page(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        y: f64,
        content: &EmptyStateContent<'_>,
        action: Option<Action>,
    ) -> f64 {
        let rect = Rect::new(body.x0, y, body.x1, body.y1.max(y));
        let hit = EmptyState::paint(ctx, rect, content, self.mouse);
        if let (Some(hit), Some(action)) = (hit, action) {
            self.push_hit(hit, action);
        }
        // The state fills what is left of the view, and the body adds its
        // closing gutter below whatever a page returns: return the end less
        // that gutter, so an empty page fits and shows no scrollbar.
        (rect.y1 - Look::of(ctx.theme).gutter).max(y)
    }

    // ---- home -------------------------------------------------------------

    fn provider_note(ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64, text: &str) -> f64 {
        let style = ctx.theme.text(TextRole::Body);
        let width = ((body.width() - 2.0 * look.gutter).max(1.0)) as f32;
        let height = ctx.text.measure_style(text, Some(width), &style).1;
        ctx.text(
            text,
            &style,
            look.dim,
            Point::new(body.x0 + look.gutter, y),
            Some(width),
        );
        y + f64::from(height)
    }

    /// This page positions retained controls manually. Use the toolkit's
    /// shared button face metrics; geometry does not acquire widget authority.
    fn source_control_size(button: &Button, ctx: &mut PaintCtx<'_>) -> Size<f32> {
        let label = button.test_id().unwrap_or_default();
        let face = ButtonFace::new(Rect::ZERO, label);
        Size {
            width: face.intrinsic_width(ctx.text, ctx.theme) as f32,
            height: ButtonFace::height(ControlSize::Regular, ctx.theme) as f32,
        }
    }

    fn position_source_buttons(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let left = body.x0 + look.gutter;
        let right = body.x1 - look.gutter;
        let width = (right - left).max(1.0) as f32;
        let mut x = left;
        let mut row_height: f64 = 0.0;
        let mode = self.source_widget_mode();
        if !super::ONLINE_CATALOGUE {
            // No online catalogue: there is no source to choose or connect.
            for button in &mut self.source_buttons {
                button.apply_layout_external(Rect::ZERO);
            }
            return y;
        }
        for button in self.source_buttons.iter_mut().filter(|button| {
            button.test_id()
                == Some(if mode == 3 {
                    "music-spotify-create-playlist"
                } else {
                    "music-sources"
                })
        }) {
            let size = Self::source_control_size(button, ctx);
            if x > left && x + f64::from(size.width) > right {
                y += row_height + look.sibling;
                x = left;
                row_height = 0.0;
            }
            let rect = Rect::from_origin_size(
                (x, y),
                (
                    f64::from(size.width).min(f64::from(width)),
                    f64::from(size.height),
                ),
            );
            button.apply_layout_external(if rect.intersect(body).area() > 0.0 {
                rect
            } else {
                Rect::ZERO
            });
            if button.get_rect().area() > 0.0 {
                button.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &PresenceCtx::empty(),
                );
            }
            x += f64::from(size.width) + look.sibling;
            row_height = row_height.max(f64::from(size.height));
        }
        y += row_height + look.sibling;
        if self.source_invocation.is_some() || self.source_error.is_some() {
            y = Self::provider_note(ctx, look, body, y, &self.source_status()) + look.sibling;
        }
        y + look.sibling
    }

    fn draw_spotify_remote_queue(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        if !self.spotify_catalogue && self.spotify_remote_queue.is_none() {
            return y;
        }
        if let Some(button) = self
            .source_buttons
            .iter_mut()
            .find(|button| button.test_id() == Some("music-spotify-queue-refresh"))
        {
            let width = (body.width() - 2.0 * look.gutter).max(1.0) as f32;
            let size = Self::source_control_size(button, ctx);
            button.apply_layout_external(Rect::from_origin_size(
                (body.x0 + look.gutter, y),
                (
                    f64::from(size.width).min(f64::from(width)),
                    f64::from(size.height),
                ),
            ));
            button.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
            y += f64::from(size.height) + look.sibling;
        }
        y = Self::section_heading(
            ctx,
            look,
            body,
            y,
            &crate::i18n::t("music-spotify-remote-queue"),
        );
        let mut lines = vec![crate::i18n::t("music-spotify-remote-queue-help")];
        if let Some(error) = &self.source_error {
            lines.push(error.clone());
        }
        if let Some((snapshot, observed)) = &self.spotify_remote_queue {
            lines.push(crate::i18n::t_args(
                "music-spotify-remote-queue-age",
                &[("seconds", observed.elapsed().as_secs().to_string().into())],
            ));
            let queue = &snapshot["remote_queue"];
            if queue["currently_playing"].is_object() {
                lines.push(super::spotify::remote_item_label(
                    &queue["currently_playing"],
                ));
            }
            if let Some(items) = queue["queue"].as_array() {
                if items.is_empty() {
                    lines.push(crate::i18n::t("music-spotify-remote-queue-empty"));
                }
                lines.extend(items.iter().take(50).enumerate().map(|(i, item)| {
                    format!("{}. {}", i + 1, super::spotify::remote_item_label(item))
                }));
            }
            if snapshot["truncated"].as_bool() == Some(true) {
                lines.push(crate::i18n::t("music-spotify-remote-queue-truncated"));
            }
        } else {
            lines.push(crate::i18n::t("music-spotify-remote-queue-unloaded"));
        }
        for line in lines {
            y = Self::provider_note(ctx, look, body, y, &line) + look.sibling;
        }
        Self::section_heading(
            ctx,
            look,
            body,
            y + look.sibling,
            &crate::i18n::t("music-spotify-local-queue"),
        )
    }

    fn draw_home(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        // Kick off the real Home fetch (charts / new releases / top artists).
        self.ensure_home();
        let mut y = y + look.gutter;
        self.refresh_spotify_control_flags();
        y = self.position_source_buttons(ctx, look, body, y);
        // Put the library and catalogue first: recommendations complement
        // browsing instead of pushing every cover below the first screen.
        // Remote catalogue content when it has landed.
        if let Some(home) = self.home.take() {
            for section in &home.sections {
                y = Self::section_heading(ctx, look, body, y, &section.title);
                y = self.draw_home_section(ctx, look, body, y, section);
                y += look.gutter;
            }
            self.home = Some(home);
            return self.draw_home_recommendations(ctx, look, body, y);
        }

        // While a real Home fetch is in flight, show the shape of the grid
        // it will fill. Once a fetch has failed (offline build / network
        // error) the honest state below settles instead.
        if self.home_is_loading() {
            return self.draw_home_skeleton(ctx, look, body, y);
        }

        // Nothing in the library and no source connected: invite connecting
        // one, having fetched nothing.
        if self.lib.playlists.is_empty()
            && self.lib.albums.is_empty()
            && self.lib.tracks.is_empty()
            && !self.catalogue_connected
            && super::ONLINE_CATALOGUE
        {
            let content = EmptyStateContent {
                icon: Some(Icon::Music),
                message: Some(super::HOME_EMPTY_MESSAGE),
                action_label: Some(super::HOME_EMPTY_ACTION),
                ..EmptyStateContent::new(super::HOME_EMPTY_TITLE)
            };
            let action = Action::SpotifyEffect(crate::actions::MusicEffect::Sources);
            return self.empty_page(ctx, body, y, &content, Some(action));
        }

        // Nothing live, and nothing in the library yet: the first-run state.
        if self.lib.playlists.is_empty() && self.lib.albums.is_empty() && self.lib.tracks.is_empty()
        {
            let (message, offers_search) = super::first_run_copy(super::ONLINE_CATALOGUE);
            let content = EmptyStateContent {
                icon: Some(Icon::Music),
                message: Some(message),
                action_label: offers_search.then_some("Search music"),
                ..EmptyStateContent::new("Your music starts here")
            };
            let action = offers_search.then_some(Action::OpenSearch);
            return self.empty_page(ctx, body, y, &content, action);
        }

        // The user's own library (built up as they play / like / save, or
        // synced from another device).
        let playlists = self.gather_playlists();
        if !playlists.is_empty() {
            y = Self::section_heading(ctx, look, body, y, "Your playlists");
            y = self.draw_playlist_cards(ctx, look, body, y, &playlists);
            y += look.gutter;
        }
        y = Self::section_heading(ctx, look, body, y, "Jump back in");
        let albums = self.gather_albums(Some(6));
        y = self.draw_album_grid(ctx, look, body, y, &albums);
        y += look.gutter;

        y = Self::section_heading(ctx, look, body, y, "Top songs");
        let ids: Vec<String> = self
            .lib
            .top_tracks(6)
            .iter()
            .map(|tk| tk.id.clone())
            .collect();
        let rows = self.gather_tracks(&ids);
        let y = self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        );
        self.draw_home_recommendations(ctx, look, body, y + look.gutter)
    }

    fn draw_home_recommendations(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
    ) -> f64 {
        if self.lib.tracks.is_empty() {
            return y;
        }
        self.draw_recommendations(
            ctx,
            (body.x0 + look.gutter, body.x1 - look.gutter),
            y,
            false,
        )
    }

    /// One real Home section as a grid of cover cards. Track cards play
    /// (queueing the whole section); album and artist cards open the real
    /// detail page.
    fn draw_home_section(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        section: &HomeSection,
    ) -> f64 {
        // Library ids for the track queue (a chart section plays as a queue).
        let track_ids: Vec<String> = section
            .items
            .iter()
            .filter(|it| it.kind == HomeItemKind::Track)
            .map(|it| live_track_id(&it.video_id))
            .collect();
        let mut queue = SectionQueue {
            ids: &track_ids,
            next: 0,
        };
        let ids: Vec<String> = section.items.iter().map(Self::home_item_art).collect();
        let cards: Vec<GridCard<'_>> = section
            .items
            .iter()
            .zip(&ids)
            .map(|(item, art_id)| {
                let action = match item.kind {
                    HomeItemKind::Track => {
                        let index = queue.next;
                        queue.next += 1;
                        Action::Play {
                            queue: queue.ids.to_vec().into(),
                            index,
                        }
                    }
                    HomeItemKind::Album => Action::Nav(View::Album(item.id.clone())),
                    HomeItemKind::Artist => Action::Nav(View::Artist(item.id.clone())),
                    HomeItemKind::Playlist => Action::Nav(View::Playlist(item.id.clone())),
                };
                let play = matches!(item.kind, HomeItemKind::Track).then(|| action.clone());
                GridCard {
                    art_id,
                    title: &item.title,
                    meta: [Some(item.subtitle.as_str()).filter(|s| !s.is_empty()), None],
                    portrait: matches!(item.kind, HomeItemKind::Artist),
                    action,
                    play,
                }
            })
            .collect();
        self.draw_media_grid(ctx, look, body, y, &cards)
    }

    /// The cover-cache key of a Home item.
    fn home_item_art(item: &HomeItem) -> String {
        match item.kind {
            HomeItemKind::Track => live_track_id(&item.video_id),
            _ => item.id.clone(),
        }
    }

    /// The Home grid while it loads: two sections, each a heading and one
    /// row of the toolkit's loading cards.
    fn draw_home_skeleton(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let (left, right) = (body.x0 + look.gutter, body.x1 - look.gutter);
        let extent = MediaSize::Small.extent(theme);
        let columns = ui::widget::card_columns(
            (right - left).max(0.0),
            extent,
            f64::from(theme.tokens.spacing.rhythm.section),
        );
        let heading = SkeletonFace::new(SkeletonShape::Text { lines: 1 });
        let clip = self.body_rect;
        for _ in 0..2 {
            let w = (extent * 1.5).min(right - left);
            let h = heading.height(ctx.text, theme, w);
            heading.paint(ctx, Rect::new(left, y, left + w, y + h));
            y += h + look.sibling;
            let index = self.media_grids_drawn;
            self.media_grids_drawn += 1;
            if self.media_grids.len() <= index {
                self.media_grids
                    .push(super::media_grid::MediaGridHost::new());
            }
            if let Some(grid) = self.media_grids.get_mut(index) {
                grid.update_loading(columns);
                y = grid.place(ctx, (left, right), y, clip);
            }
            y += look.gutter;
        }
        y
    }

    // ---- media grids ------------------------------------------------------

    /// Lay `cards` out as a media grid from `y`: the toolkit's grid of
    /// cover cards, embedded in the page. The cards take their own hover,
    /// press and Play action; what they ask for comes back through
    /// [`Self::route_page_widgets`]. Returns the y below the grid.
    fn draw_media_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        cards: &[GridCard<'_>],
    ) -> f64 {
        let specs = cards
            .iter()
            .map(|card| super::media_grid::CardSpec {
                art_id: card.art_id.to_owned(),
                title: card.title.to_owned(),
                meta: card
                    .meta
                    .iter()
                    .flatten()
                    .map(|line| (*line).to_owned())
                    .collect(),
                image: self.art_image(card.art_id),
                portrait: card.portrait,
                action: card.action.clone(),
                play: card.play.clone(),
                menu: match &card.action {
                    Action::Nav(View::Playlist(id)) if id.starts_with("spotify:playlist:") => {
                        Some(Action::OpenSpotifyPlaylistMenu(id.clone()))
                    }
                    _ => None,
                },
            })
            .collect();
        let index = self.media_grids_drawn;
        self.media_grids_drawn += 1;
        if self.media_grids.len() <= index {
            self.media_grids
                .push(super::media_grid::MediaGridHost::new());
        }
        let clip = self.body_rect;
        let Some(grid) = self.media_grids.get_mut(index) else {
            return y;
        };
        grid.update(specs);
        grid.place(ctx, (body.x0 + look.gutter, body.x1 - look.gutter), y, clip)
    }

    fn draw_album_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        albums: &[AlbumDisp],
    ) -> f64 {
        let years: Vec<String> = albums
            .iter()
            .map(|a| {
                if a.year > 0 {
                    a.year.to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        let cards: Vec<GridCard<'_>> = albums
            .iter()
            .zip(&years)
            .map(|(a, year)| GridCard {
                art_id: &a.id,
                title: &a.title,
                meta: [
                    Some(a.artist.as_str()).filter(|s| !s.is_empty()),
                    Some(year.as_str()).filter(|s| !s.is_empty()),
                ],
                portrait: false,
                action: Action::Nav(View::Album(a.id.clone())),
                play: (!a.queue.is_empty()).then(|| Action::Play {
                    queue: a.queue.clone().into(),
                    index: 0,
                }),
            })
            .collect();
        self.draw_media_grid(ctx, look, body, y, &cards)
    }

    fn draw_artist_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        artists: &[ArtistDisp],
    ) -> f64 {
        let cards: Vec<GridCard<'_>> = artists
            .iter()
            .map(|a| GridCard {
                art_id: &a.id,
                title: &a.name,
                meta: [Some(a.genre.as_str()).filter(|s| !s.is_empty()), None],
                portrait: true,
                action: Action::Nav(View::Artist(a.id.clone())),
                play: None,
            })
            .collect();
        self.draw_media_grid(ctx, look, body, y, &cards)
    }

    /// The user's playlists as cover cards.
    fn draw_playlist_cards(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        pls: &[PlaylistDisp],
    ) -> f64 {
        let counts: Vec<String> = pls.iter().map(|p| songs_label(p.count)).collect();
        let cards: Vec<GridCard<'_>> = pls
            .iter()
            .zip(&counts)
            .map(|(p, count)| GridCard {
                art_id: &p.id,
                title: &p.name,
                meta: [Some(count.as_str()), None],
                portrait: false,
                action: Action::Nav(View::Playlist(p.id.clone())),
                play: None,
            })
            .collect();
        self.draw_media_grid(ctx, look, body, y, &cards)
    }

    /// The Playlists destination: a "New playlist" action over the cards.
    fn draw_playlist_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        pls: &[PlaylistDisp],
    ) -> f64 {
        let span = (body.x0 + look.gutter, body.x1 - look.gutter);
        if self.spotify_catalogue {
            let y = self.position_source_buttons(ctx, look, body, y);
            return if pls.is_empty() {
                Self::note(ctx, look, body, y, "Playlists you make appear here.")
            } else {
                self.draw_playlist_cards(ctx, look, body, y, pls)
            };
        }
        let y = self.page_part(ctx, span, y, "new-playlist".into(), |asked| {
            Box::new(
                ui::widget::Row::new().no_wrap().push(Box::new(
                    Button::new("music.new-playlist", "New playlist")
                        .with_icon(Icon::Plus)
                        .role(ButtonRole::Secondary)
                        .on_click(super::page_part::asking(asked, Action::BeginNewPlaylist)),
                )),
            )
        }) + look.gutter;
        if pls.is_empty() {
            return Self::note(ctx, look, body, y, "Playlists you make appear here.");
        }
        self.draw_playlist_cards(ctx, look, body, y, pls)
    }

    // ---- track table ------------------------------------------------------

    /// The track list every view shares (songs, an album, a playlist, liked
    /// songs, downloads, the queue): the toolkit's table, embedded in the
    /// page and windowed by its scroll. Its rows lift, play, open their menu
    /// and show their actions themselves; what they ask for comes back
    /// through [`Self::route_page_widgets`]. Returns the y below the table.
    fn draw_track_table(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        table: &TrackTable<'_>,
    ) -> f64 {
        use super::track_table::TrackTableHost;
        let theme = ctx.theme;
        let left = body.x0 + look.gutter;
        let right = body.x1 - look.gutter;
        let downloads = matches!(self.view, View::Downloads);
        let shape = Self::track_table_shape(ctx, look, (left, right), table, downloads);
        let offset = table.queue_offset.unwrap_or(0);
        let rows: Vec<_> = table
            .rows
            .iter()
            .enumerate()
            .map(|(i, tk)| self.track_cell(tk, i, table, downloads))
            .collect();
        // A membership opened from search stays lifted on its row, as does
        // the song the keyboard is on in a query view's results.
        let selected = table
            .playlist
            .and_then(|playlist| {
                let (owner, track) = self.revealed_membership.as_ref()?;
                (owner == playlist)
                    .then(|| table.rows.iter().position(|tk| &tk.id == track))
                    .flatten()
            })
            .or_else(|| {
                let song = self.result_cursor_song()?;
                table.rows.iter().position(|tk| tk.id == song)
            });

        let slot_index = self.track_tables_drawn;
        self.track_tables_drawn += 1;
        if self.track_tables.len() <= slot_index {
            self.track_tables.push(super::TrackTableSlot {
                table: TrackTableHost::new(shape, theme),
                queue: Arc::default(),
                playlist: None,
                queue_offset: None,
            });
        }
        let clip = self.body_rect;
        let Some(slot) = self.track_tables.get_mut(slot_index) else {
            return y;
        };
        if slot.queue.as_slice() != table.queue {
            slot.queue = Arc::new(table.queue.to_vec());
        }
        slot.playlist = table.playlist.map(str::to_owned);
        slot.queue_offset = table.queue_offset.map(|_| offset);
        slot.table.update(shape, rows, selected, theme);
        if slot_index == 0
            && let Some(ids) = self.restore_selection.take()
        {
            // Back on a view: its songs are selected as they were left.
            slot.table.select_ids(&ids);
        }
        if slot_index == 0
            && let Some(row) = self.staged_row_pointer.take()
        {
            slot.table.stage_pointer(row);
        }
        slot.table.place(ctx, (left, right), y, clip)
    }

    /// A track table's columns in `[left, right]`: which show, and their
    /// measured widths.
    fn track_table_shape(
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        (left, right): (f64, f64),
        table: &TrackTable<'_>,
        downloads: bool,
    ) -> super::track_table::Shape {
        use super::track_table::{Actions, Second, Shape};
        let theme = ctx.theme;
        let numeric = theme.text(TextRole::Caption).numeric();
        let index_w = if table.number_col {
            f64::from(ctx.text.measure_style("000", None, &numeric).0)
        } else {
            0.0
        };
        let duration_w = f64::from(ctx.text.measure_style("00:00", None, &numeric).0);
        let action_w = super::track_table::action_side(theme);
        let actions = if downloads {
            Actions::Download
        } else {
            Actions::Track
        };
        let action_count = actions.count();
        let title_style = theme.text(TextRole::Body).strong();
        let caption = theme.text(TextRole::Caption);
        let title_need = table
            .rows
            .iter()
            .map(|tk| {
                let title = ctx.text.measure_style(&tk.title, None, &title_style).0;
                let artist = ctx.text.measure_style(&tk.artist, None, &caption).0;
                f64::from(title.max(artist))
            })
            .fold(0.0, f64::max);
        let second_column = table.show_album || downloads;
        let cols = track_columns(
            left,
            right,
            ColumnWidths {
                index: index_w,
                duration: duration_w,
                action: action_w,
                actions: action_count,
                title: title_need,
                gap: look.gutter,
                min_text: MediaSize::Small.extent(theme),
            },
            second_column,
        );
        // Widths on the half pixel, so a table is rebuilt for a real change
        // of its columns and never for float noise.
        let half = |v: f64| ((v * 2.0).round() / 2.0) as f32;
        Shape {
            numbers: table.number_col,
            second: cols.album.map(|_| {
                if downloads {
                    Second::Status
                } else {
                    Second::Album
                }
            }),
            title_width: cols.album.map(|_| half(cols.title_x1 - cols.title_x)),
            header: table.header,
            actions,
            row_height: half(DataTableFace::new(&[], &[]).two_line().row_height(theme)),
            index_width: half(index_w),
            duration_width: half(duration_w),
            action_width: half(action_w),
            moves: table.queue_offset.is_some(),
        }
    }

    /// One row of a track table as its cells show it.
    fn track_cell(
        &self,
        tk: &TrackDisp,
        index: usize,
        table: &TrackTable<'_>,
        downloads: bool,
    ) -> super::track_table::TrackCell {
        use super::track_table::{Offline, Playback};
        let current = if let Some(offset) = table.queue_offset {
            self.pb.queue_index == offset + index && self.pb.current.is_some()
        } else {
            self.pb.current.as_deref() == Some(tk.id.as_str())
        };
        let failed = self.offline_errors.contains_key(&tk.id);
        let offline = if failed {
            Offline::Failed
        } else {
            match self.cache_state(&tk.id) {
                CacheState::Downloading(progress) => {
                    Offline::Downloading((progress * 100.0).round().clamp(0.0, 100.0) as u8)
                }
                CacheState::Cached => Offline::Cached,
                CacheState::Streamed => Offline::Streamed,
            }
        };
        let second = if downloads {
            self.download_status(&tk.id).0
        } else {
            tk.album.clone()
        };
        super::track_table::TrackCell {
            id: tk.id.clone(),
            title: tk.title.clone(),
            artist: tk.artist.clone(),
            second,
            number: if matches!(self.view, View::Album(_)) {
                tk.no.max(index as u32 + 1)
            } else {
                index as u32 + 1
            },
            duration: if tk.dur > 0 {
                ui::format::timecode(u64::from(tk.dur))
            } else {
                String::new()
            },
            playback: match (current, self.pb.playing) {
                (false, _) => Playback::Idle,
                (true, false) => Playback::Paused,
                (true, true) => Playback::Playing,
            },
            liked: self.lib.is_liked(&tk.id),
            offline,
            offline_error: self.offline_errors.get(&tk.id).cloned(),
            local: self.is_local_file(&tk.id),
        }
    }

    /// Route `event` to the page's embedded widgets (its track tables and
    /// cover grids) and turn what their rows and cards asked for into
    /// actions. Chrome painted over the page (the queue
    /// preview, a floating sidebar and its scrim) takes the pointer first.
    /// Returns whether a table took the event.
    fn route_page_widgets(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        );
        if !pointer {
            return false;
        }
        let mouse = match event {
            WindowEvent::CursorMoved { position, .. } => ctx.logical(position),
            _ => self.mouse,
        };
        let covered = !self.body_rect.contains(mouse)
            || self
                .hits
                .get(self.body_hits..)
                .is_some_and(|chrome| chrome.iter().any(|hit| hit.rect.contains(mouse)));
        let mut asked = Vec::new();
        let mut tooltip = None;
        for (index, slot) in self.track_tables.iter_mut().enumerate() {
            if covered {
                // The pointer is not over the page: no row stays lifted.
                if let WindowEvent::CursorMoved { device_id, .. } = event {
                    slot.table.host.handle_event(
                        &WindowEvent::CursorLeft {
                            device_id: *device_id,
                        },
                        ctx,
                    );
                }
                continue;
            }
            slot.table.host.handle_event(event, ctx);
            tooltip = tooltip.or_else(|| slot.table.host.tooltip_at(mouse));
            asked.extend(slot.table.take_events().into_iter().map(|ask| (index, ask)));
        }
        if tooltip != self.table_tooltip {
            self.table_tooltip = tooltip;
            self.dirty = true;
        }
        let mut cards = Vec::new();
        for grid in &mut self.media_grids {
            if covered {
                // The pointer is not over the page: no card stays lifted.
                if let WindowEvent::CursorMoved { device_id, .. } = event {
                    grid.host.handle_event(
                        &WindowEvent::CursorMoved {
                            device_id: *device_id,
                            position: winit::dpi::PhysicalPosition::new(f64::MIN, f64::MIN),
                        },
                        ctx,
                    );
                }
                continue;
            }
            if !*ctx.handled {
                grid.host.handle_event(event, ctx);
            }
            cards.extend(grid.take_asked());
        }
        for part in &mut self.page_parts {
            if covered {
                if let WindowEvent::CursorMoved { device_id, .. } = event {
                    part.host.handle_event(
                        &WindowEvent::CursorMoved {
                            device_id: *device_id,
                            position: winit::dpi::PhysicalPosition::new(f64::MIN, f64::MIN),
                        },
                        ctx,
                    );
                }
                continue;
            }
            if !*ctx.handled {
                part.host.handle_event(event, ctx);
            }
            if self.table_tooltip.is_none()
                && let Some(tip) = part.host.tooltip_at(mouse)
            {
                self.table_tooltip = Some(tip);
                self.dirty = true;
            }
            cards.extend(part.take_asked());
        }
        let took = *ctx.handled;
        for action in cards {
            self.dispatch(action, &mut ctx.state);
            self.dirty = true;
        }
        for (index, ask) in asked {
            if let Some(action) = self.row_action(index, ask) {
                self.dispatch(action, &mut ctx.state);
                self.dirty = true;
            }
        }
        took
    }

    fn drain_table_requests(&mut self, state: &mut ui::widget::Edits<'_>) {
        let requests: Vec<_> = self
            .track_tables
            .iter()
            .enumerate()
            .flat_map(|(index, slot)| {
                slot.table
                    .take_events()
                    .into_iter()
                    .map(move |ask| (index, ask))
            })
            .collect();
        for (index, ask) in requests {
            if let Some(action) = self.row_action(index, ask) {
                self.dispatch(action, state);
                self.dirty = true;
            }
        }
    }

    /// The action a row of track table `index` asked for.
    fn row_action(&mut self, index: usize, ask: super::track_table::RowEvent) -> Option<Action> {
        use super::track_table::{Offline, RowEvent};
        match ask {
            // The table selected the row itself; nothing plays.
            RowEvent::Select(_) => return None,
            RowEvent::Drag(row) => {
                self.begin_track_drag(index, row);
                return None;
            }
            _ => {}
        }
        let slot = self.track_tables.get(index)?;
        let offset = slot.queue_offset.unwrap_or(0);
        let row = |i: usize| slot.table.rows().get(i);
        match ask {
            RowEvent::Play(i) => Some(Action::Play {
                queue: Arc::clone(&slot.queue),
                index: offset + i,
            }),
            RowEvent::Menu(i, rect) => row(i).map(|track| {
                self.menu_anchor_row = Some(rect);
                Action::OpenTrackMenu {
                    track: track.id.clone(),
                    in_playlist: slot.playlist.clone(),
                    queue_index: slot.queue_offset.map(|o| o + i),
                }
            }),
            RowEvent::Like(i) => row(i).map(|track| Action::ToggleLike(track.id.clone())),
            RowEvent::Offline(i) => row(i).map(|track| Action::SetOffline {
                track: track.id.clone(),
                keep: matches!(track.offline, Offline::Streamed | Offline::Failed),
            }),
            RowEvent::Select(_) | RowEvent::Drag(_) => None,
            RowEvent::More(i) => row(i).map(|track| {
                let rect = slot.table.row_rect(i).unwrap_or(Rect::ZERO);
                self.menu_anchor_row = Some(rect);
                Action::OpenTrackMenu {
                    track: track.id.clone(),
                    in_playlist: slot.playlist.clone(),
                    queue_index: slot.queue_offset.map(|o| o + i),
                }
            }),
        }
    }

    /// Place the next embedded part of the page from `top` across
    /// `[left, right]`, rebuilt with `build` when `key` changed. Returns the
    /// y below it.
    pub(super) fn page_part(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        top: f64,
        key: String,
        build: impl FnOnce(&super::page_part::Asked) -> ui::widget::Element,
    ) -> f64 {
        let index = self.page_parts_drawn;
        self.page_parts_drawn += 1;
        if self.page_parts.len() <= index {
            self.page_parts
                .push(super::page_part::PagePart::new("music.page-widget."));
        }
        let clip = self.body_rect;
        let Some(part) = self.page_parts.get_mut(index) else {
            return top;
        };
        part.update(key, build);
        part.host.place(ctx, span, top, clip)
    }

    /// A song's offline status in words, and whether it failed. A failure
    /// says only that; its Retry button names the cause.
    fn download_status(&self, track: &str) -> (String, bool) {
        if self.offline_errors.contains_key(track) {
            return ("Download failed".into(), true);
        }
        match self.cache_state(track) {
            CacheState::Downloading(progress) => {
                (format!("Downloading, {:.0}%", progress * 100.0), false)
            }
            CacheState::Cached => ("Available offline".into(), false),
            CacheState::Streamed => ("Not downloaded".into(), false),
        }
    }

    // ---- detail pages -----------------------------------------------------

    /// The hero of an album, artist or playlist page: the cover (a round
    /// portrait for an artist), the title, one secondary line and one
    /// caption, and the page's actions. The cover sits beside the text on a wide body and above it
    /// on a narrow one. Returns the y below the hero.
    #[allow(
        clippy::too_many_lines,
        reason = "the hero lays out its art, its text and its wrapping action row together"
    )]
    fn draw_detail_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        header: DetailHeader<'_>,
    ) -> f64 {
        let theme = ctx.theme;
        let y = y + look.gutter;
        let left = body.x0 + look.gutter;
        let right = body.x1 - look.gutter;
        // Reserve a readable text column, not three large covers. At high
        // UI scales the old cover-multiple breakpoint stacked a desktop hero
        // and pushed virtually its entire track list below the fold.
        let row_h = DataTableFace::new(&[], &[]).two_line().row_height(theme);
        let hero_budget = (body.height() - 4.0 * row_h - 3.0 * look.gutter).max(3.0 * look.target);
        let side = MediaSize::Small
            .extent(theme)
            .min(right - left)
            .min(hero_budget);
        let wide = right - left >= side + look.gutter + MediaSize::Regular.extent(theme);
        let art = Rect::new(left, y, left + side, y + side);
        if matches!(header.kind, DetailKind::Artist { .. }) {
            self.draw_portrait(ctx, art, header.id, header.title);
        } else {
            self.draw_cover(ctx, art, header.id, header.title);
        }

        let tx = if wide { art.x1 + look.gutter } else { left };
        let text_w = (right - tx).max(0.0);
        let caption = theme.text(TextRole::Caption);
        let title = theme.text(TextRole::LargeTitle);
        let body_style = theme.text(TextRole::Body);
        let title_lines = ctx.text.clamp_to_lines(
            header.title,
            &title,
            ui::num::f32_of_f64(text_w.max(1.0)),
            2,
        );
        let caption_h = line_of(ctx, &caption);
        let body_h = line_of(ctx, &body_style);
        let title_h = line_of(ctx, &title);
        let button_h = ButtonFace::height(ControlSize::Regular, theme);
        let block_h = look.tight
            + title_lines.len() as f64 * title_h
            + if header.secondary.is_empty() {
                0.0
            } else {
                body_h
            }
            + if header.caption.is_empty() {
                0.0
            } else {
                caption_h
            }
            + look.section
            + button_h;
        // Beside the cover the text block sits on the cover's bottom edge;
        // under it, the block follows the cover.
        let mut ty = if wide {
            (art.y1 - block_h).max(art.y0)
        } else {
            art.y1 + look.section
        };
        for line in title_lines {
            ctx.text(&line, &title, look.text, Point::new(tx, ty), None);
            ty += title_h;
        }
        ty += look.tight;
        if !header.secondary.is_empty() {
            ty += text_line(
                ctx,
                header.secondary,
                &body_style,
                look.dim,
                Point::new(tx, ty),
                text_w,
            );
        }
        if !header.caption.is_empty() {
            ty += text_line(
                ctx,
                header.caption,
                &caption,
                look.faint,
                Point::new(tx, ty),
                text_w,
            );
        }
        ty += look.section;

        // Keep playback primary; playlist management lives behind More.
        let queue = header.queue;
        let mut buttons: Vec<(&str, ButtonRole, Action)> = vec![
            (
                "Play",
                ButtonRole::Primary,
                Action::Play {
                    queue: queue.clone().into(),
                    index: 0,
                },
            ),
            (
                "Shuffle",
                ButtonRole::Secondary,
                Action::ShufflePlay(queue.into()),
            ),
        ];
        let id = header.id.to_string();
        let spotify_menu_label = crate::i18n::t("music-spotify-playlist-actions");
        match header.kind {
            DetailKind::Album { saved } => buttons.push((
                if saved {
                    "Remove from library"
                } else {
                    "Save to library"
                },
                ButtonRole::Secondary,
                Action::ToggleSaveAlbum(id),
            )),
            DetailKind::Artist { following } => buttons.push((
                if following { "Unfollow" } else { "Follow" },
                ButtonRole::Secondary,
                Action::ToggleFollow(id),
            )),
            DetailKind::Playlist { own: true } => buttons.push((
                "Playlist actions",
                ButtonRole::Secondary,
                Action::OpenPlaylistMenu(id),
            )),
            DetailKind::Playlist { own: false } if id.starts_with("spotify:playlist:") => buttons
                .push((
                    &spotify_menu_label,
                    ButtonRole::Secondary,
                    Action::OpenSpotifyPlaylistMenu(id),
                )),
            DetailKind::Playlist { own: false } => {}
        }
        // The actions are the toolkit's buttons in a wrapping row.
        let key = format!("{buttons:?}");
        let gap = ui::num::f32_of_f64(look.related);
        let ty = self.page_part(ctx, (tx, right), ty, key, move |asked| {
            let mut row = ui::widget::Row::new().with_gap(gap);
            for (label, role, action) in buttons {
                let id = format!("music.page.{label}");
                let menu = matches!(
                    action,
                    Action::OpenPlaylistMenu(_) | Action::OpenSpotifyPlaylistMenu(_)
                );
                let button = if menu {
                    Button::icon_only(&id, Icon::MoreHorizontal, label)
                } else {
                    Button::new(&id, label).role(role)
                };
                row = row.push(Box::new(
                    button.on_click(super::page_part::asking(asked, action)),
                ));
            }
            Box::new(row)
        });
        art.y1.max(ty) + look.gutter
    }

    /// `ids` with recordings that appear twice (a local track and the same
    /// recording resolved live) kept once, in order.
    fn unique_recordings(&self, ids: &[String]) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        ids.iter()
            .filter(|id| {
                self.playback_track(id).is_none_or(|track| {
                    seen.insert((
                        track.title.to_lowercase(),
                        track.artist_id.clone(),
                        track.duration_s,
                    ))
                })
            })
            .cloned()
            .collect()
    }

    fn draw_album_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music album (its ordered track list) when available.
        self.ensure_album(id);
        if let Some(page) = self.album_pages.remove(id) {
            let r = self.draw_album_detail_live(ctx, look, body, y, &page);
            self.album_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_album_id(id) && !self.lib.albums.contains_key(id) {
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, body, y, "album");
            }
            return Self::draw_loading_detail(ctx, look, body, y);
        }
        let (title, artist, year, ids) = {
            let al = self.lib.album(id);
            let title = al.map(|a| a.title.clone()).unwrap_or_default();
            let artist = al
                .map(|a| self.lib.artist_name(&a.artist_id).to_string())
                .unwrap_or_default();
            let year = al.map_or(0, |a| a.year);
            let ids: Vec<String> = self
                .lib
                .album_tracks(id)
                .iter()
                .map(|tk| tk.id.clone())
                .collect();
            (title, artist, year, ids)
        };
        let ids = self.unique_recordings(&ids);
        let rows = self.gather_tracks(&ids);
        let caption = album_caption(year, &rows);
        let saved = self.lib.is_album_saved(id);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Album { saved },
                title: &title,
                secondary: &artist,
                caption: &caption,
                queue: ids.clone(),
            },
        );
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        )
    }

    /// Album detail rendered straight from a real [`AlbumPage`].
    fn draw_album_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        page: &AlbumPage,
    ) -> f64 {
        let ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| live_track_id(&h.video_id))
            .collect();
        let ids = self.unique_recordings(&ids);
        let rows = self.gather_tracks(&ids);
        let caption = album_caption(page.year, &rows);
        let saved = self.lib.is_album_saved(&page.id);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Album { saved },
                title: &page.title,
                secondary: &page.artist_name,
                caption: &caption,
                queue: ids.clone(),
            },
        );
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        )
    }

    /// A detail page while it loads: the shapes of its cover, title and
    /// rows, so nothing moves when the content lands.
    fn draw_loading_detail(ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let theme = ctx.theme;
        let mut y = y + look.gutter;
        let left = body.x0 + look.gutter;
        let right = body.x1 - look.gutter;
        let side = MediaSize::Small.extent(theme).min(right - left);
        let cover = SkeletonFace::new(SkeletonShape::Media(ui::widget::MediaAspect::Square));
        let cover_h = cover.height(ctx.text, theme, side);
        cover.paint(ctx, Rect::new(left, y, left + side, y + cover_h));
        y += cover_h + look.gutter;
        let row = SkeletonFace::new(SkeletonShape::Row);
        let row_h = row.height(ctx.text, theme, right - left);
        for _ in 0..6 {
            row.paint(ctx, Rect::new(left, y, right, y + row_h));
            y += row_h;
        }
        y
    }

    /// A library page with nothing in it yet (Songs, Albums, Artists): what
    /// fills it, and the way to find something to play.
    fn empty_library_page(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        y: f64,
        title: &str,
    ) -> f64 {
        let content = EmptyStateContent {
            icon: Some(Icon::Music),
            message: Some("Music you play, like or save builds your library."),
            action_label: Some("Search music"),
            ..EmptyStateContent::new(title)
        };
        self.empty_page(ctx, body, y, &content, Some(Action::OpenSearch))
    }

    /// An honest error for a live detail page whose fetch failed (offline,
    /// geo-block, network error), with a way home: a deep link can open a
    /// page with no Back history to return to.
    fn draw_detail_error(&mut self, ctx: &mut PaintCtx<'_>, body: Rect, y: f64, kind: &str) -> f64 {
        let title = format!("Couldn’t load this {kind}");
        let content = EmptyStateContent {
            icon: Some(Icon::WifiOff),
            message: Some("Check your connection and try again."),
            action_label: Some("Go to Home"),
            error: true,
            ..EmptyStateContent::new(&title)
        };
        self.empty_page(ctx, body, y, &content, Some(Action::Nav(View::Home)))
    }

    fn draw_artist_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music artist page (top songs, albums, subscribers).
        self.ensure_artist(id);
        if let Some(page) = self.artist_pages.remove(id) {
            let r = self.draw_artist_detail_live(ctx, look, body, y, &page);
            self.artist_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_id(id) && !self.lib.artists.contains_key(id) {
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, body, y, "artist");
            }
            return Self::draw_loading_detail(ctx, look, body, y);
        }
        let (name, genre, listeners, top_ids, albums) = {
            let ar = self.lib.artist(id);
            let name = ar.map(|a| a.name.clone()).unwrap_or_default();
            let genre = ar.map(|a| a.genre.clone()).unwrap_or_default();
            let listeners = ar.map_or(0, |a| a.monthly_listeners);
            let mut top: Vec<&crate::model::Track> = self
                .lib
                .tracks
                .values()
                .filter(|tk| tk.artist_id == id)
                .collect();
            top.sort_by_key(|tk| std::cmp::Reverse(tk.plays));
            let top_ids: Vec<String> = top.iter().take(5).map(|tk| tk.id.clone()).collect();
            let albums = self.gather_albums_for_artist(id);
            (name, genre, listeners, top_ids, albums)
        };
        let caption = format!("{} monthly listeners", ui::format::compact_count(listeners));
        let followed = self.lib.is_artist_followed(id);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Artist {
                    following: followed,
                },
                title: &name,
                secondary: &genre,
                caption: &caption,
                queue: top_ids.clone(),
            },
        );

        let mut y = Self::section_heading(ctx, look, body, y, "Popular");
        let rows = self.gather_tracks(&top_ids);
        y = self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &top_ids).without_album(),
        );
        if albums.is_empty() {
            return y;
        }
        y = Self::section_heading(ctx, look, body, y + look.gutter, "Albums");
        self.draw_album_grid(ctx, look, body, y, &albums)
    }

    /// A full artist page rendered from a real [`ArtistPage`]: header (play +
    /// shuffle + follow), popular tracks, a sectioned discography (albums vs.
    /// singles and EPs), related artists, and the artist's biography.
    fn draw_artist_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        page: &ArtistPage,
    ) -> f64 {
        let caption = if page.subscriber_count > 0 {
            format!(
                "{} subscribers",
                ui::format::compact_count(page.subscriber_count)
            )
        } else {
            String::new()
        };
        let top_ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| live_track_id(&h.video_id))
            .collect();
        let followed = page
            .provider_followed
            .unwrap_or_else(|| self.lib.is_artist_followed(&page.id));
        let mut y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Artist {
                    following: followed,
                },
                title: &page.name,
                secondary: "",
                caption: &caption,
                queue: top_ids.clone(),
            },
        );

        if !top_ids.is_empty() {
            y = Self::section_heading(ctx, look, body, y, "Popular");
            let rows = self.gather_tracks(&top_ids);
            y = self.draw_track_table(
                ctx,
                look,
                body,
                y,
                &TrackTable::new(&rows, &top_ids).without_album(),
            );
            y += look.gutter;
        }

        y = self.draw_artist_detail_discography(ctx, look, body, y, page);
        y = self.draw_artist_detail_related(ctx, look, body, y, page);
        Self::draw_artist_detail_bio(ctx, look, body, y, page)
    }

    /// Discography, sectioned by release type. Full albums first, then the
    /// shorter singles and EPs — the way listeners expect a discography to read.
    fn draw_artist_detail_discography(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        use crate::backend::AlbumKind;
        let to_disp = |al: &crate::backend::ArtistAlbum, name: &str| AlbumDisp {
            id: al.id.clone(),
            title: al.title.clone(),
            artist: name.to_string(),
            year: al.year,
            queue: Vec::new(),
        };
        let albums: Vec<AlbumDisp> = page
            .albums
            .iter()
            .filter(|al| al.kind == AlbumKind::Album)
            .map(|al| to_disp(al, &page.name))
            .collect();
        let singles: Vec<AlbumDisp> = page
            .albums
            .iter()
            .filter(|al| {
                matches!(
                    al.kind,
                    AlbumKind::Single | AlbumKind::Ep | AlbumKind::Other
                )
            })
            .map(|al| to_disp(al, &page.name))
            .collect();
        if !albums.is_empty() {
            y = Self::section_heading(ctx, look, body, y, "Albums");
            y = self.draw_album_grid(ctx, look, body, y, &albums);
            y += look.gutter;
        }
        if !singles.is_empty() {
            y = Self::section_heading(ctx, look, body, y, "Singles and EPs");
            y = self.draw_album_grid(ctx, look, body, y, &singles);
            y += look.gutter;
        }
        y
    }

    /// Related / similar artists ("Fans also like").
    fn draw_artist_detail_related(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.similar.is_empty() {
            return y;
        }
        y = Self::section_heading(ctx, look, body, y, "Fans also like");
        let related: Vec<ArtistDisp> = page
            .similar
            .iter()
            .map(|a| ArtistDisp {
                id: a.id.clone(),
                name: a.name.clone(),
                genre: if a.subscriber_count > 0 {
                    format!(
                        "{} subscribers",
                        ui::format::compact_count(a.subscriber_count)
                    )
                } else {
                    String::new()
                },
            })
            .collect();
        y = self.draw_artist_grid(ctx, look, body, y, &related);
        y + look.gutter
    }

    /// The artist's biography, wrapped to a readable width and bounded to a
    /// few lines.
    fn draw_artist_detail_bio(
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.description.is_empty() {
            return y;
        }
        let mut y = Self::section_heading(ctx, look, body, y, "About");
        let style = ctx.theme.text(TextRole::Body);
        let readable = MediaSize::Large.extent(ctx.theme) * 2.0;
        let maxw = (body.width() - 2.0 * look.gutter).min(readable).max(1.0);
        let line = line_of(ctx, &style);
        for text in ctx
            .text
            .clamp_to_lines(&page.description, &style, ui::num::f32_of_f64(maxw), 8)
        {
            ctx.text(
                &text,
                &style,
                look.dim,
                Point::new(body.x0 + look.gutter, y),
                None,
            );
            y += line;
        }
        y
    }

    fn draw_playlist_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // A live (YouTube-Music) playlist opened from a search result: fetch +
        // render its real track list. Falls back to a loading skeleton until it
        // lands, then to the local path if it's actually one of the user's own.
        if is_live_playlist_id(id) && self.lib.playlist(id).is_none() {
            self.ensure_playlist(id);
            if let Some(page) = self.playlist_pages.remove(id) {
                let bottom = self.draw_playlist_detail_live(ctx, look, body, y, &page);
                self.playlist_pages.insert(id.to_string(), page);
                return bottom;
            }
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, body, y, "playlist");
            }
            return Self::draw_loading_detail(ctx, look, body, y);
        }

        let (name, desc, ids) = {
            let p = self.lib.playlist(id);
            let name = p.map(|p| p.name.clone()).unwrap_or_default();
            let desc = p.map(|p| p.description.clone()).unwrap_or_default();
            let ids = p.map(crate::model::Playlist::track_ids).unwrap_or_default();
            (name, desc, ids)
        };
        let rows = self.gather_tracks(&ids);
        let caption = length_caption(&rows);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Playlist { own: true },
                title: &name,
                secondary: &desc,
                caption: &caption,
                queue: ids.clone(),
            },
        );

        // A Ctrl+K membership hit names a specific row, not merely its owning
        // playlist. Move it into view on the next paint (the current paint has
        // already established its geometry), and keep the row visibly marked.
        if let Some((playlist, track)) = &self.revealed_membership
            && playlist == id
        {
            if let Some(index) = ids.iter().position(|candidate| candidate == track) {
                let row_h = ui::widget::ListRowFace::new("")
                    .secondary("")
                    .height(ctx.text, ctx.theme);
                let scrolled = self.scroll_y();
                let row_document_y = y + scrolled + index as f64 * row_h;
                let content_document_end = y + scrolled + ids.len() as f64 * row_h + look.gutter;
                let max_scroll = (content_document_end - body.y0 - body.height()).max(0.0);
                let desired =
                    (row_document_y - body.y0 - body.height() * 0.42).clamp(0.0, max_scroll);
                if (scrolled - desired).abs() > 0.5 {
                    self.set_scroll_y(desired);
                }
            } else {
                // The authority-door check should have caught this, but never
                // leave a stale local highlight around if a peer changes the
                // list between operation delivery and the following frame.
                self.revealed_membership = None;
            }
        }

        if ids.is_empty() {
            return Self::note(
                ctx,
                look,
                body,
                y,
                "Add songs to this playlist from a song’s menu.",
            );
        }
        // Unified row look (equalizer + like + download + duration); the playlist
        // context adds reorder/remove affordances and binds the row menu.
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album().in_playlist(id),
        )
    }

    /// A live (YouTube-Music) playlist rendered from a real [`PlaylistPage`]:
    /// cover + owner header with Play / Shuffle, then the ordered track list.
    fn draw_playlist_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        page: &PlaylistPage,
    ) -> f64 {
        let ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| live_track_id(&h.video_id))
            .collect();
        let rows = self.gather_tracks(&ids);
        let caption = length_caption(&rows);
        let mut y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Playlist { own: false },
                title: &page.title,
                secondary: &page.subtitle,
                caption: &caption,
                queue: ids.clone(),
            },
        );
        if !page.description.is_empty() {
            let style = ctx.theme.text(TextRole::Body);
            let readable = MediaSize::Large.extent(ctx.theme) * 2.0;
            let maxw = (body.width() - 2.0 * look.gutter).min(readable).max(1.0);
            let line = line_of(ctx, &style);
            for text in
                ctx.text
                    .clamp_to_lines(&page.description, &style, ui::num::f32_of_f64(maxw), 3)
            {
                ctx.text(
                    &text,
                    &style,
                    look.dim,
                    Point::new(body.x0 + look.gutter, y),
                    None,
                );
                y += line;
            }
            y += look.section;
        }
        if ids.is_empty() {
            return Self::note(ctx, look, body, y, "This playlist has no playable songs.");
        }
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).in_playlist(&page.id),
        )
    }

    // ---- queue ------------------------------------------------------------

    fn draw_queue(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let mut y = y + look.gutter;
        y = self.draw_spotify_remote_queue(ctx, look, body, y);
        let full = self.pb.queue.clone();
        if let Some(cur) = self.pb.current.clone() {
            y = Self::section_heading(ctx, look, body, y, "Now playing");
            let rows = self.gather_tracks(std::slice::from_ref(&cur));
            let offset = self.pb.queue_index;
            // One row under its own heading: the columns are named once,
            // over the list that follows.
            y = self.draw_track_table(
                ctx,
                look,
                body,
                y,
                &TrackTable::new(&rows, &full)
                    .in_queue(offset)
                    .without_header(),
            );
            y += look.gutter;
        }
        let upnext: Vec<String> = full.iter().skip(self.pb.queue_index + 1).cloned().collect();
        y = Self::section_heading(ctx, look, body, y, "Next up");
        if upnext.is_empty() {
            return Self::note(
                ctx,
                look,
                body,
                y,
                "Play an album or a playlist to fill the queue.",
            );
        }
        let rows = self.gather_tracks(&upnext);
        let offset = self.pb.queue_index + 1;
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &full).in_queue(offset),
        )
    }

    // ---- downloads --------------------------------------------------------

    fn draw_downloads(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let y = y + look.gutter;
        let mut ids = self
            .lib
            .tracks
            .keys()
            .filter(|id| !matches!(self.cache_state(id), CacheState::Streamed))
            .cloned()
            .collect::<Vec<_>>();
        for track_id in self.offline_errors.keys() {
            if !ids.contains(track_id) {
                ids.push(track_id.clone());
            }
        }
        if ids.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::Download),
                message: Some("Keep a song offline to listen without a connection."),
                ..EmptyStateContent::new("No downloads yet")
            };
            return self.empty_page(ctx, body, y, &content, None);
        }
        let cached = self
            .lib
            .tracks
            .keys()
            .filter(|id| matches!(self.cache_state(id), CacheState::Cached))
            .count();
        let active = self
            .lib
            .tracks
            .keys()
            .filter(|id| matches!(self.cache_state(id), CacheState::Downloading(_)))
            .count()
            .max(self.downloads.len() + self.offline_resolves.len());
        let failed = self.offline_errors.len();
        let status = match (cached, active, failed) {
            (0, 0, 0) => "Nothing is available offline yet.".to_string(),
            (0, 0, 1) => "1 download failed".to_string(),
            (0, 0, failed) => format!("{failed} downloads failed"),
            (0, active, _) => format!("Downloading {}", songs_label(active)),
            (cached, 0, _) => format!("{} available offline", songs_label(cached)),
            (cached, active, _) => format!(
                "{} available offline, {active} downloading",
                songs_label(cached)
            ),
        };
        let y = Self::note(ctx, look, body, y, &status) + look.section;
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_numbers(),
        )
    }

    // ---- liked songs ------------------------------------------------------

    fn draw_liked(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let y = y + look.gutter;
        let ids = self.lib.liked_ids();
        if ids.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::Heart),
                message: Some("Like a song and it appears here."),
                ..EmptyStateContent::new("No liked songs yet")
            };
            return self.empty_page(ctx, body, y, &content, None);
        }
        let rows = self.gather_tracks(&ids);
        let summary = length_caption(&rows);
        let mut y = Self::note(ctx, look, body, y, &summary) + look.section;
        let buttons = [
            (
                "Play",
                ButtonRole::Primary,
                Action::Play {
                    queue: ids.clone().into(),
                    index: 0,
                },
            ),
            (
                "Shuffle",
                ButtonRole::Secondary,
                Action::ShufflePlay(ids.clone().into()),
            ),
        ];
        let key = format!("{buttons:?}");
        let gap = ui::num::f32_of_f64(look.related);
        let span = (body.x0 + look.gutter, body.x1 - look.gutter);
        y = self.page_part(ctx, span, y, key, move |asked| {
            let mut row = ui::widget::Row::new().with_gap(gap);
            for (label, role, action) in buttons {
                row = row.push(Box::new(
                    Button::new(&format!("music.page.{label}"), label)
                        .role(role)
                        .on_click(super::page_part::asking(asked, action)),
                ));
            }
            Box::new(row)
        });
        y += look.gutter;
        self.draw_track_table(ctx, look, body, y, &TrackTable::new(&rows, &ids))
    }

    // ---- lyrics -----------------------------------------------------------

    fn draw_lyrics(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let _ = y; // lyrics own the whole body: laid out + auto-scrolled here.
        // Resolve lyrics for the current track (cached by id).
        self.ensure_lyrics();
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        if cur.is_none() {
            let content = EmptyStateContent {
                icon: Some(Icon::Mic),
                ..EmptyStateContent::new("Play a song to see its lyrics.")
            };
            return self.empty_page(ctx, body, body.y0, &content, None);
        }

        let readable = MediaSize::Large.extent(ctx.theme) * 2.0;
        let column = lyrics_column(body, readable, look.gutter);
        if self.lyrics.is_empty() {
            let msg = if self.lyrics_pending_for.is_some() {
                "Finding lyrics…"
            } else {
                "No lyrics found for this song."
            };
            Self::note(ctx, look, body, column.y0, msg);
            return column.y1;
        }
        self.draw_lyrics_lines(ctx, look, column);
        column.y1
    }

    /// The lyric lines in `band`, set flush with the page's leading edge and
    /// following the playhead: the current line in the title role, its
    /// neighbours quieter with distance.
    fn draw_lyrics_lines(&self, ctx: &mut PaintCtx<'_>, look: &Look, band: Rect) {
        let pos = self.playhead_ms as u64;
        let current = if self.lyrics_synced {
            lyrics::Lyrics {
                lines: self.lyrics.clone(),
                synced: true,
            }
            .current_line(pos)
        } else {
            None
        };
        let active_style = ctx.theme.text(TextRole::Title);
        let style = ctx.theme.text(TextRole::Heading);
        let focus = current.unwrap_or(0);
        let maxw = ui::num::f32_of_f64(band.width().max(1.0));
        // Every line wraps to the column, the one being sung too; a blank
        // line (a pause in the song) is a short gap, not a glyph.
        let gap = look.related;
        let blank = line_of(ctx, &style) / 2.0;
        let mut tops = Vec::with_capacity(self.lyrics.len());
        let mut heights = Vec::with_capacity(self.lyrics.len());
        let mut top = 0.0;
        for (i, line) in self.lyrics.iter().enumerate() {
            let st = if Some(i) == current {
                &active_style
            } else {
                &style
            };
            let text = line.text.trim();
            let height = if text.is_empty() {
                blank
            } else {
                f64::from(ctx.text.measure_style(text, Some(maxw), st).1)
            };
            tops.push(top);
            heights.push(height);
            top += height + gap;
        }
        let auto = lyrics_scroll(current.and_then(|i| tops.get(i).copied()), band.height());
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &band,
        );
        for (i, line) in self.lyrics.iter().enumerate() {
            let text = line.text.trim();
            let (Some(&top), Some(&height)) = (tops.get(i), heights.get(i)) else {
                continue;
            };
            let ly = band.y0 + top - auto;
            // Do not leave half a lyric line cut against the header or player.
            if text.is_empty() || ly < band.y0 || ly + height > band.y1 {
                continue;
            }
            let active = Some(i) == current;
            let near = i.abs_diff(focus) <= 1;
            let (st, ink) = if active {
                (&active_style, look.text)
            } else if !self.lyrics_synced || near {
                (&style, look.dim)
            } else {
                (&style, look.faint)
            };
            ctx.text(text, st, ink, Point::new(band.x0, ly), Some(maxw));
        }
        ctx.scene.pop_layer();
    }

    // ---- browse -----------------------------------------------------------

    /// Browse: the library's genres as cards. A card opens the platform
    /// search for Music with the genre filled in.
    fn draw_browse(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let mut y = y + look.gutter;
        let mut genres: Vec<(String, usize)> = Vec::new();
        for artist in self.lib.artists.values() {
            if artist.genre.is_empty() {
                continue;
            }
            match genres.iter_mut().find(|(g, _)| *g == artist.genre) {
                Some((_, n)) => *n += 1,
                None => genres.push((artist.genre.clone(), 1)),
            }
        }
        genres.sort();
        if genres.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::LayoutGrid),
                message: Some("Genres appear here as your library grows."),
                action_label: Some("Search music"),
                ..EmptyStateContent::new("Nothing to browse yet")
            };
            return self.empty_page(ctx, body, y, &content, Some(Action::OpenSearch));
        }
        y = Self::section_heading(ctx, look, body, y, "Genres");
        // A genre is a card; pressing it opens search for the genre.
        let key = format!("{genres:?}");
        let span = (body.x0 + look.gutter, body.x1 - look.gutter);
        self.page_part(ctx, span, y, key, move |asked| {
            let cards = genres
                .into_iter()
                .map(|(genre, artists)| {
                    let label = if artists == 1 {
                        "1 artist".to_string()
                    } else {
                        format!("{artists} artists")
                    };
                    Box::new(
                        ui::widget::Card::new(Box::new(
                            ui::widget::Text::new(&label)
                                .with_role(TextRole::Caption)
                                .dimmed(),
                        ))
                        .with_title(genre.clone())
                        .on_activate(
                            format!("music.genre.{genre}"),
                            super::page_part::asking(asked, Action::BrowseGenre(genre)),
                        ),
                    ) as ui::widget::Element
                })
                .collect();
            Box::new(ui::widget::CardGrid::new(cards))
        })
    }

    // ---- now playing bar --------------------------------------------------

    // ---- toast ------------------------------------------------------------

    fn draw_toast(&self, ctx: &mut PaintCtx<'_>) {
        let area = Rect::new(self.body_rect.x0, self.rect.y0, self.rect.x1, self.rect.y1);
        self.toast.paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            area,
            ToastAnchor::BottomCenter,
        );
    }

    // ---- gather helpers (own data so draw can borrow &mut self) -----------

    fn gather_tracks(&self, ids: &[String]) -> Vec<TrackDisp> {
        ids.iter()
            .filter_map(|id| self.playback_track(id))
            .map(|tk| TrackDisp {
                id: tk.id.clone(),
                title: tk.title.clone(),
                artist: self.lib.artist_name(&tk.artist_id).to_string(),
                album: self.lib.album_title(&tk.album_id).to_string(),
                dur: tk.duration_s,
                no: tk.track_no,
            })
            .collect()
    }

    fn gather_albums(&self, limit: Option<usize>) -> Vec<AlbumDisp> {
        let mut v: Vec<AlbumDisp> = self
            .lib
            .albums_sorted()
            .iter()
            .map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: self.lib.artist_name(&a.artist_id).to_string(),
                year: a.year,
                queue: self
                    .lib
                    .album_tracks(&a.id)
                    .iter()
                    .map(|tk| tk.id.clone())
                    .collect(),
            })
            .collect();
        if let Some(n) = limit {
            v.truncate(n);
        }
        v
    }

    fn gather_albums_for_artist(&self, artist: &str) -> Vec<AlbumDisp> {
        self.lib
            .artist_albums(artist)
            .iter()
            .map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: self.lib.artist_name(&a.artist_id).to_string(),
                year: a.year,
                queue: self
                    .lib
                    .album_tracks(&a.id)
                    .iter()
                    .map(|tk| tk.id.clone())
                    .collect(),
            })
            .collect()
    }

    fn gather_artists(&self) -> Vec<ArtistDisp> {
        self.lib
            .artists_sorted()
            .iter()
            .map(|a| ArtistDisp {
                id: a.id.clone(),
                name: a.name.clone(),
                genre: a.genre.clone(),
            })
            .collect()
    }

    fn gather_playlists(&self) -> Vec<PlaylistDisp> {
        self.lib
            .playlists()
            .map(|p| PlaylistDisp {
                id: p.id.clone(),
                name: p.name.clone(),
                count: p.len(),
            })
            .collect()
    }
}

/// "1 song" / "12 songs".
fn songs_label(n: usize) -> String {
    if n == 1 {
        "1 song".to_string()
    } else {
        format!("{n} songs")
    }
}

/// The caption of an album page: when it came out, then how long it is
/// ("Released in 2023, 12 songs, 48 min").
fn album_caption(year: u32, rows: &[TrackDisp]) -> String {
    let length = length_caption(rows);
    if year > 0 {
        format!("Released in {year}, {length}")
    } else {
        length
    }
}

/// How many songs a list holds and how long it plays: "12 songs, 48 min".
fn length_caption(rows: &[TrackDisp]) -> String {
    let total: u64 = rows.iter().map(|row| u64::from(row.dur)).sum();
    let count = songs_label(rows.len());
    if total == 0 {
        return count;
    }
    let minutes = total.div_ceil(60);
    if minutes >= 60 {
        format!("{count}, {} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{count}, {minutes} min")
    }
}
