//! Drawing + event handling for [`MusicApp`].
//!
//! Split out of `app.rs` to keep the data/state logic separate from the
//! (large) rendering surface. Music paints its window itself, but every part
//! of it is the toolkit's: the sidebar is a [`SidebarDrawer`] with
//! [`chrome::paint_nav_row`] rows, controls are [`ButtonFace`]s and
//! [`SliderFace`]s, covers are [`MediaCardFace`]s and every text names a
//! [`TextRole`]. Clicks register `Hit`s for the event handler.
//!
//! [`SidebarDrawer`]: ui::widget::SidebarDrawer

use super::page_part::PagePart;
use super::{
    Action, Affine, AlbumPage, Arc, ArtistPage, AutoCommit, CacheState, Color, Dimension, Drag,
    Fill, HashMap, HomeItem, HomeItemKind, HomeSection, Instant, MusicApp, NodeId, ObjId,
    PermissionContext, PlaylistPage, Point, PresenceCtx, Rect, RepeatMode, Scene, Size, Style,
    TaffyTree, TextRenderer, TextStyle, Theme, View, Widget, elide, is_live_album_id, is_live_id,
    is_live_playlist_id, live_track_id, lyrics,
};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use ui::chrome::{self, HeaderRowFace, NavCount, NavRowFace, NavSectionFace, SelectionState};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::surface;
use ui::tokens::TextRole;
use ui::widget::Continuable;
use ui::widget::data_table::DataTableFace;
use ui::widget::toast::{ToastAnchor, ToastCard, paint_transient};
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent, MediaArt,
    MediaCardFace, MediaSize, SkeletonFace, SkeletonShape, Slider, SliderFace, TransportControl,
    TransportFace,
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
    album_id: String,
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

/// How far a now-playing-bar drag travels before it counts as a prev/next
/// swipe rather than a tap: two pointer targets.
const BAR_SWIPE_PX: f64 = 2.0 * ui::tokens::Metrics::BASE.min_pointer_target as f64;

/// The painted frame's ink and spacing, resolved from the theme once per
/// frame. Every gap is a step of the theme's spacing rhythm.
#[derive(Clone, Copy)]
struct Look {
    text: Color,
    dim: Color,
    faint: Color,
    danger: Color,
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
            danger: theme.tokens.colors.semantic.danger,
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

/// Paint one line of `text` starting at `x`, elided to `max_w` and centred on
/// its measured line box inside `band`.
fn text_in_band(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    x: f64,
    band: Rect,
    max_w: f64,
) {
    let h = line_of(ctx, style);
    let top = ui::text_metrics::centered_text_top(band.y0, band.height(), h);
    text_line(ctx, text, style, color, Point::new(x, top), max_w);
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

fn accessible_track(
    lib: &crate::model::Library,
    track: &crate::model::Track,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, AccessState, Role};

    let liked = lib.is_liked(&track.id);
    let mut node = AccessNode::new(Role::ListItem)
        .with_hashed_key(format!("track:{}", track.id))
        .with_name(&track.title)
        .with_description(format!(
            "{}; {}; {}",
            lib.artist_name(&track.artist_id),
            lib.album_title(&track.album_id),
            ui::format::timecode(u64::from(track.duration_s))
        ))
        .with_position(position, total)
        .with_children(vec![
            AccessNode::new(Role::Button)
                .with_name(format!("Play {}", track.title))
                .with_action_target(format!("music.play-track.{}", track.id)),
            AccessNode::new(Role::ToggleButton)
                .with_name(format!("Like {}", track.title))
                .with_state(AccessState::new().checked(liked))
                .with_action_target(format!("music.toggle-like.{}", track.id)),
        ]);
    if !lib.tracks.contains_key(&track.id) {
        node.children.truncate(1);
    }
    node
}

fn accessible_download(
    lib: &crate::model::Library,
    track: &crate::model::Track,
    state: CacheState,
    failure: Option<&str>,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, Role};

    let (description, action_name, action_target) = if let Some(error) = failure {
        (
            format!(
                "{}; download failed: {error}. Retry the offline download.",
                lib.artist_name(&track.artist_id)
            ),
            format!("Retry offline download for {}", track.title),
            format!("music.retry-offline.{}", track.id),
        )
    } else if matches!(state, CacheState::Downloading(_)) {
        (
            format!(
                "{}; offline download in progress",
                lib.artist_name(&track.artist_id)
            ),
            format!("Cancel offline download for {}", track.title),
            format!("music.remove-offline.{}", track.id),
        )
    } else {
        (
            format!(
                "{}; available offline; {}",
                lib.artist_name(&track.artist_id),
                ui::format::timecode(u64::from(track.duration_s))
            ),
            format!("Remove offline download for {}", track.title),
            format!("music.remove-offline.{}", track.id),
        )
    };
    let mut children = vec![
        AccessNode::new(Role::Button)
            .with_name(action_name)
            .with_action_target(action_target),
    ];
    if failure.is_none() && matches!(state, CacheState::Cached) {
        children.insert(
            0,
            AccessNode::new(Role::Button)
                .with_name(format!("Play {}", track.title))
                .with_action_target(format!("music.play-track.{}", track.id)),
        );
    }
    AccessNode::new(Role::ListItem)
        .with_hashed_key(format!("download:{}", track.id))
        .with_name(&track.title)
        .with_description(description)
        .with_position(position, total)
        .with_children(children)
}

fn accessible_album(
    lib: &crate::model::Library,
    album: &crate::model::Album,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("album:{}", album.id))
        .with_name(&album.title)
        .with_description(format!(
            "{}; {}",
            lib.artist_name(&album.artist_id),
            album.year
        ))
        .with_position(position, total)
        .with_action_target(format!("music.open-album.{}", album.id))
}

fn accessible_artist(
    artist: &crate::model::Artist,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("artist:{}", artist.id))
        .with_name(&artist.name)
        .with_description(format!(
            "{}; {} monthly listeners",
            artist.genre, artist.monthly_listeners
        ))
        .with_position(position, total)
        .with_action_target(format!("music.open-artist.{}", artist.id))
}

fn accessible_playlist(
    playlist: &crate::model::Playlist,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    let resource = crate::actions::playlist_resource_id(&playlist.id);
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("playlist:{}", playlist.id))
        .with_name(&playlist.name)
        .with_description(format!("{} songs", playlist.track_ids.len()))
        .with_position(position, total)
        .with_action_target(format!("music.open-playlist.{}", playlist.id))
        .with_children(vec![
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name(format!("Rename {}", playlist.name))
                .with_action_target(format!("music.rename-playlist.{resource}")),
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name(format!("Delete {}", playlist.name))
                .with_action_target(format!("music.delete-playlist.{resource}")),
        ])
}

fn accessible_playlist_track(
    lib: &crate::model::Library,
    playlist: &crate::model::Playlist,
    track: &crate::model::Track,
    position: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, Role};
    let total = playlist.track_ids.len();
    let resource = crate::actions::playlist_track_resource_id(&playlist.id, &track.id);
    let mut node = accessible_track(lib, track, position, total);
    let mut actions = vec![
        AccessNode::new(Role::Button)
            .with_name(format!("Remove {} from {}", track.title, playlist.name))
            .with_action_target(format!("music.remove-playlist-track.{resource}")),
    ];
    if position > 1 {
        actions.push(
            AccessNode::new(Role::Button)
                .with_name(format!("Move {} up", track.title))
                .with_action_target(format!("music.move-up-playlist-track.{resource}")),
        );
    }
    if position < total {
        actions.push(
            AccessNode::new(Role::Button)
                .with_name(format!("Move {} down", track.title))
                .with_action_target(format!("music.move-down-playlist-track.{resource}")),
        );
    }
    node.children.extend(actions);
    node
}

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

/// A sidebar navigation row: its icon, label and the view it opens.
struct NavDestination {
    icon: Icon,
    label: &'static str,
    view: View,
}

const NAV_DESTINATIONS: [NavDestination; 8] = [
    NavDestination {
        icon: Icon::Home,
        label: "Home",
        view: View::Home,
    },
    NavDestination {
        icon: Icon::LayoutGrid,
        label: "Browse",
        view: View::Browse,
    },
    NavDestination {
        icon: Icon::Music,
        label: "Songs",
        view: View::Songs,
    },
    NavDestination {
        icon: Icon::GalleryVerticalEnd,
        label: "Albums",
        view: View::Albums,
    },
    NavDestination {
        icon: Icon::Users,
        label: "Artists",
        view: View::Artists,
    },
    NavDestination {
        icon: Icon::Heart,
        label: "Liked songs",
        view: View::Liked,
    },
    NavDestination {
        icon: Icon::Download,
        label: "Downloads",
        view: View::Downloads,
    },
    NavDestination {
        icon: Icon::ListMusic,
        label: "Queue",
        view: View::Queue,
    },
];

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
pub(super) fn lyrics_scroll(current: Option<usize>, line_h: f64, column_h: f64) -> f64 {
    current.map_or(0.0, |line| line as f64 * line_h - column_h / 3.0)
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
            schema_version: 1,
            bytes,
        })
    }
    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != 1 {
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
        dyn ui::widget::WindowHeader,
        dyn ui::widget::controls::ActionControls
    );

    fn take_structure_dirty(&mut self) -> bool {
        // Reported once: an app that is not rebuilt right away (a tab behind
        // another) must not ask again on every frame.
        let visible = Some((
            matches!(self.view, View::Home),
            !self.nav_back.is_empty(),
            self.source_widget_mode(),
        ));
        let file_changed =
            std::mem::replace(&mut self.open_file_layout_visible, visible) != visible;
        self.history.take_rebuilt() || file_changed
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.search_button];
        if self.view == View::Home {
            children.push(&self.open_file_button);
        }
        if !self.nav_back.is_empty() {
            children.push(&self.back_button);
        }
        children.extend(self.history.button().map(|button| button as &dyn Widget));
        children.extend(
            self.track_tables
                .iter()
                .filter_map(|slot| slot.table.visible_widget()),
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
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.search_button];
        if self.view == View::Home {
            children.push(&mut self.open_file_button);
        }
        if !self.nav_back.is_empty() {
            children.push(&mut self.back_button);
        }
        children.extend(
            self.history
                .button_mut()
                .map(|button| button as &mut (dyn Widget + '_)),
        );
        children.extend(
            self.track_tables
                .iter_mut()
                .filter_map(|slot| slot.table.visible_widget_mut()),
        );
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
        let (state, _) = ui::doc::document_ref(state, path);
        if self.playback_loaded {
            self.reload_library(state);
        } else {
            self.reload(state);
            self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
            self.playback_loaded = true;
        }
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        // External controls and completed effects enter the same authored
        // action door as events, before the next read-only projection.
        // The host admits row control effects after delivering input. Their
        // callbacks queue requests; apply them at the next sync through the
        // same canonical actions as the palette, without waiting for another
        // pointer event.
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
        self.drawer.scale_for(theme);
        self.refresh_spotify_control_flags();
        let mut children = vec![self.search_button.build_layout(taffy, theme, node_map)];
        if self.view == View::Home {
            children.push(self.open_file_button.build_layout(taffy, theme, node_map));
        }
        if !self.nav_back.is_empty() {
            children.push(self.back_button.build_layout(taffy, theme, node_map));
        }
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
        #[allow(
            clippy::expect_used,
            reason = "the child belongs to this same tree; Widget::build_layout has no error return"
        )]
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` is the id this same widget inserted into this exact tree in
        // `build_layout` earlier in the same frame (the five-pass contract
        // guarantees layout was computed for it by now), so `layout` cannot
        // fail here in practice; `apply_layout`'s `()` return leaves no error
        // path, and that signature is shared framework-wide.
        #[allow(
            clippy::expect_used,
            reason = "node was inserted into this tree by this widget's own build_layout earlier in the same frame; Widget::apply_layout's Result-free signature is shared by every implementor and out of this crate's scope to change"
        )]
        let layout = taffy
            .layout(node)
            .expect("apply_layout runs against the same tree + node build_layout returned");
        self.rect = ui::widget::placed(layout, origin);
    }

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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root rect is the whole window — always paint.
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

        // The header row's window controls: History, then the sidebar
        // toggle (with its shortcut, and Escape or a press on the scrim
        // for a floating sidebar).
        let history_hovered = self.history.is_hovered();
        if self
            .history
            .handle_event(event, self.history_rect, ctx.scale_factor)
        {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        self.dirty |= history_hovered != self.history.is_hovered();
        let toggle_hovered = self.drawer.toggle_rect().contains(self.mouse);
        if self.drawer.handle_event(event, ctx) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.dirty |= toggle_hovered != self.drawer.toggle_rect().contains(p);
        }

        if let Ok(children) = ctx.taffy.children(ctx.node_id) {
            if let Some(node) = children.first() {
                self.search_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
            if !*ctx.handled
                && self.view == View::Home
                && let Some(node) = children.get(1)
            {
                self.open_file_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
            let back_index = 1 + usize::from(self.view == View::Home);
            if !*ctx.handled
                && !self.nav_back.is_empty()
                && let Some(node) = children.get(back_index)
            {
                self.back_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
        }
        if *ctx.handled {
            self.dirty = true;
            return;
        }
        // The body's scrolling: wheel and touchpad over it, its bar, and
        // Page Up/Down, Home/End and the arrows.
        if let Some(theme) = self.frame_theme.take() {
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
        let controls = vec![
            AccessNode::new(Role::Button)
                .with_name("Previous track")
                .with_description("Shortcut: left arrow")
                .with_action_target("music.previous"),
            AccessNode::new(Role::ToggleButton)
                .with_name("Play or pause")
                .with_description("Shortcut: space")
                .with_action_target("music.toggle-play")
                .with_state(AccessState::new().checked(self.pb.playing)),
            AccessNode::new(Role::Button)
                .with_name("Next track")
                .with_description("Shortcut: right arrow")
                .with_action_target("music.next"),
            AccessNode::new(Role::ToggleButton)
                .with_name("Shuffle")
                .with_state(AccessState::new().checked(self.pb.shuffle))
                .with_action_target("music.toggle-shuffle"),
            AccessNode::new(Role::Button)
                .with_name("Repeat mode")
                .with_value(self.pb.repeat.as_str())
                .with_action_target("music.cycle-repeat"),
            AccessNode::new(Role::ToggleButton)
                .with_name("Mute")
                .with_description("Shortcut: M")
                .with_action_target("music.toggle-mute")
                .with_state(AccessState::new().checked(self.pb.volume == 0)),
            AccessNode::new(Role::Slider)
                .with_name("Playback position")
                .with_value(ui::format::timecode((self.playhead_ms / 1000.0) as u64))
                .with_description("Increment or decrement by five percent")
                .with_action_target("music.seek"),
            AccessNode::new(Role::Slider)
                .with_name("Volume")
                .with_value(format!("{}%", self.pb.volume))
                .with_action_target("music.volume"),
            AccessNode::new(Role::Button)
                .with_name("Open queue")
                .with_action_target("music.nav.queue"),
            AccessNode::new(Role::Button)
                .with_name("Open lyrics")
                .with_action_target("music.nav.lyrics"),
        ];
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
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .busy(self.active_resolve_request.is_some()),
            )
            .with_children(controls);
        let selected_nav = |id: &str| {
            matches!(
                (&self.view, id),
                (View::Home, "home")
                    | (View::Recommendations, "recommendations")
                    | (View::Songs, "songs")
                    | (View::Albums, "albums")
                    | (View::Artists, "artists")
                    | (View::Playlists, "playlists")
                    | (View::Downloads, "downloads")
                    | (View::Liked, "liked")
                    | (View::Browse, "browse")
                    | (View::Queue, "queue")
                    | (View::Lyrics, "lyrics")
            )
        };
        let navigation = AccessNode::new(Role::List)
            .with_hashed_key("navigation")
            .with_name("Music navigation")
            .with_children(
                crate::actions::NAVIGATION_VIEWS
                    .iter()
                    .enumerate()
                    .map(|(index, (id, label))| {
                        AccessNode::new(Role::ListItem)
                            .with_name(*label)
                            .with_position(index + 1, crate::actions::NAVIGATION_VIEWS.len())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(selected_nav(id)),
                            )
                            .with_action_target(format!("music.nav.{id}"))
                    })
                    .collect(),
            );

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
            page_content.push(
                AccessNode::new(Role::Group)
                    .with_name("Empty music library")
                    .with_description("Your music starts here")
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Search music")
                            .with_description("Search for a song, album, or artist")
                            .with_action_target("music.open-search"),
                    ]),
            );
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
                        .playlists
                        .iter()
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
                if let Some(playlist) = self
                    .lib
                    .playlists
                    .iter()
                    .find(|playlist| playlist.id == *id)
                {
                    list_items.extend(
                        playlist
                            .track_ids
                            .iter()
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
                        .with_name("New playlist")
                        .with_action_target("music.new-playlist"),
                );
                let total = self.lib.playlists.len();
                list_items.extend(
                    self.lib
                        .playlists
                        .iter()
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
                        .playlists
                        .iter()
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
                                .with_name(&line.text)
                                .with_position(index + 1, self.lyrics.len())
                        }),
                );
            }
            View::Recommendations => {}
        }
        if !list_items.is_empty() {
            page_content.push(
                AccessNode::new(Role::List)
                    .with_name(format!("{page_name} items"))
                    .with_children(list_items),
            );
        }
        page_content.extend(self.page_parts.iter().flat_map(PagePart::access_nodes));
        let page = AccessNode::new(Role::TabPanel)
            .with_hashed_key(format!("page:{page_name}"))
            .with_name(page_name)
            .with_children(page_content);

        let mut children = Vec::new();
        children.extend(self.drawer.toggle_accessibility());
        children.extend([navigation, search, page, player]);
        children.extend(self.history.accessibility(self.history_rect));
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
        if self.history.perform_semantic_action(target, action) {
            self.dirty = true;
            return true;
        }
        if target == "sidebar-toggle" && matches!(action, ui::a11y::Action::Activate) {
            self.dispatch(Action::ToggleSidebar, state);
            return true;
        }
        if target == "music.seek" {
            let delta = match action {
                ui::a11y::Action::Increment => 0.05,
                ui::a11y::Action::Decrement => -0.05,
                _ => return false,
            };
            let duration = self.current_duration_ms() as f64;
            let fraction = if duration > 0.0 {
                (self.playhead_ms / duration + delta).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.dispatch(Action::SeekFrac(fraction), state);
            return true;
        }
        if target == "music.volume" {
            let delta = match action {
                ui::a11y::Action::Increment => 0.05,
                ui::a11y::Action::Decrement => -0.05,
                _ => return false,
            };
            let fraction = (f64::from(self.pb.volume) / 100.0 + delta).clamp(0.0, 1.0);
            self.dispatch(Action::VolumeFrac(fraction), state);
            return true;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let action = match target {
            "music.previous" => Action::Prev,
            "music.toggle-play" => Action::TogglePlay,
            "music.next" => Action::Next,
            "music.toggle-shuffle" => Action::ToggleShuffle,
            "music.cycle-repeat" => Action::CycleRepeat,
            "music.toggle-mute" => Action::ToggleMute,
            "music.open-search" => Action::OpenSearch,
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
                    let known_local = self.lib.playlists.iter().any(|playlist| playlist.id == id);
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
                    let Some(from) = self
                        .lib
                        .playlist(&playlist)
                        .and_then(|playlist| playlist.track_ids.iter().position(|id| id == &track))
                    else {
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

    fn get_rect(&self) -> Rect {
        self.rect
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
            || self.toast.is_some()
            || self.live_search_pending.is_some()
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
            MusicEffect::SpotifyDevices
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
            MusicEffect::ToggleLike { .. } => {
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
                if self.resource_stream_invocation.is_some() {
                    return Err("another Music stream-start operation is still resolving".into());
                }
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
            transport => {
                let next = crate::actions::next_playback(&transport, &self.pb, &|id| {
                    self.playback_track(id).is_some()
                })
                .ok_or_else(|| {
                    format!(
                        "`{}` is not a playback effect",
                        ui::tilo_resource::Effect::id(&transport)
                    )
                })??;
                self.dirty = true;
                // Only starting a source waits for the audio it starts.
                if next.playing && (self.pb.current != next.current || !self.pb.playing) {
                    self.begin_resource_playback(invocation, next)
                } else {
                    self.apply_resource_playback(next)?;
                    Ok(done)
                }
            }
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
                if !current_playlist.track_ids.iter().any(|id| id == &track) {
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
        if self.drag.is_none() && self.bar_press.is_none() && self.route_page_widgets(event, ctx) {
            if let WindowEvent::CursorMoved { .. } = event {
                self.on_cursor_moved(event, ctx.scale_factor, &mut ctx.state, ctx.handled);
            }
            self.dirty = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.on_cursor_moved(event, ctx.scale_factor, &mut ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(&mut ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(&mut ctx.state, ctx.handled);
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

    /// Hover labels for icon-only controls. Text-labelled rows and navigation
    /// deliberately return `None`; repeating their visible label would add
    /// noise rather than information.
    fn tooltip_at(&self, point: Point) -> Option<&str> {
        if self.drawer.toggle_rect().contains(point) {
            return Some(if self.drawer.is_open() {
                "Hide sidebar"
            } else {
                "Show sidebar"
            });
        }
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
                Action::ToggleVolumePopover => Some("Volume"),
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
                Action::ToggleSidebar => Some("Show or hide navigation"),
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

    /// Track the pointer and resolve an in-progress scrubber/volume drag.
    fn on_cursor_moved(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.dirty = true;
        if let Some(drag) = self.drag {
            match drag {
                Drag::Scrubber => {
                    let f = SliderFace::new(self.scrubber_rect, 0.0).value_at(self.mouse);
                    self.dispatch(Action::SeekFrac(f), state);
                }
                Drag::Volume => {
                    let f = SliderFace::new(self.volume_rect, 0.0).value_at(self.mouse);
                    self.dispatch(Action::VolumeFrac(f), state);
                }
            }
            *handled = true;
        } else if self.bar_press.is_some() {
            // A now-playing-bar swipe is in progress; consume moves so
            // the gesture is resolved on release.
            *handled = true;
        }
    }

    /// Left press: search field / volume popover dismiss / hamburger / transport
    /// drag start / now-playing swipe capture / topmost hit — in priority order.
    fn on_left_press(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        // A press outside the open volume popover (and its button)
        // dismisses it.
        if self.volume_popover_open
            && !self.volume_popover_rect.contains(self.mouse)
            && !self.volume_btn_rect.contains(self.mouse)
        {
            self.volume_popover_open = false;
            self.dirty = true;
            *handled = true;
            return;
        }
        // Transport bars get drag priority.
        if self.scrubber_hit.contains(self.mouse) {
            self.drag = Some(Drag::Scrubber);
            let f = SliderFace::new(self.scrubber_rect, 0.0).value_at(self.mouse);
            self.dispatch(Action::SeekFrac(f), state);
            *handled = true;
            return;
        }
        if self.volume_hit.contains(self.mouse) {
            self.drag = Some(Drag::Volume);
            let f = SliderFace::new(self.volume_rect, 0.0).value_at(self.mouse);
            self.dispatch(Action::VolumeFrac(f), state);
            *handled = true;
            return;
        }
        // Now-playing bar swipe: a horizontal drag over the bar's empty
        // area (or the artwork) is captured here and resolved on release
        // — swipe left → next, right → previous. Transport buttons still
        // click through (they aren't swipe-eligible).
        let bar = self.nowplaying_rect();
        if bar.contains(self.mouse) {
            let hit = self.hit_test(self.mouse);
            let swipeable = matches!(hit, None | Some(Action::Nav(View::Lyrics | View::Album(_))));
            if swipeable {
                self.bar_press = Some((self.mouse, hit));
                *handled = true;
                return;
            }
        }
        // Topmost matching hit wins (later-drawn = on top).
        if let Some(action) = self.hit_test(self.mouse) {
            self.dispatch(action, state);
            *handled = true;
        }
    }

    /// Left release: end a search-field drag, resolve a now-playing swipe, or
    /// end a scrubber/volume drag.
    fn on_left_release(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        // Resolve a now-playing-bar swipe: a large horizontal delta skips
        // track; otherwise it was a tap on the captured target.
        if let Some((start, tap)) = self.bar_press.take() {
            let dx = self.mouse.x - start.x;
            if dx <= -BAR_SWIPE_PX {
                self.dispatch(Action::Next, state);
            } else if dx >= BAR_SWIPE_PX {
                self.dispatch(Action::Prev, state);
            } else if let Some(a) = tap {
                self.dispatch(a, state);
            }
            *handled = true;
            return;
        }
        if self.drag.is_some() {
            self.drag = None;
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

        match key {
            Key::Named(NamedKey::Escape) => {
                // Dismiss an open overlay sidebar.
                if self.drawer.is_overlay() && self.drawer.is_open() {
                    self.dispatch(Action::CloseSidebar, state);
                    *handled = true;
                }
            }
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

    /// Paint an icon button `face` and register its hit. `on` marks a toggle
    /// that is on (the toolkit's toggled face).
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        face: ButtonFace<'_>,
        on: bool,
        action: Action,
    ) -> bool {
        let hovered = self.push_hit(face.hit_rect(ctx.theme), action);
        let face = face.hovered(hovered).toggled(on);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        hovered
    }

    /// Paint one media transport control and register its hit; `idle`
    /// rests it, disabled, with nothing to play.
    fn transport_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        control: TransportControl,
        idle: bool,
        action: Action,
    ) -> bool {
        let face = TransportFace::new(rect, control).disabled(idle);
        let action = if idle { Action::None } else { action };
        let hovered = self.push_hit(face.hit_rect(ctx.theme), action) && !idle;
        TransportFace::paint(ctx.scene, ctx.transform, ctx.theme, &face.hovered(hovered));
        hovered
    }

    // ---- master frame -----------------------------------------------------

    /// Whether the now-playing bar folds to its compact form: while the
    /// sidebar floats, or when the content column is too narrow to hold the
    /// song, the transport and the volume side by side.
    fn compact_bar(theme: &Theme, drawer: &ui::widget::SidebarDrawer, width: f64) -> bool {
        drawer.is_overlay() || width < 2.0 * MediaSize::Large.extent(theme)
    }

    /// Height of the now-playing bar: a row of transport controls over the
    /// scrubber (wide), or the track and its controls over the scrubber
    /// (compact), inside the sibling inset.
    fn bar_height(ctx: &mut PaintCtx<'_>, look: &Look, compact: bool) -> f64 {
        // The row is as tall as its tallest control: the round play button.
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme).max(TransportFace::side(
            TransportControl::PlayPause { playing: false },
            ctx.theme,
        ));
        let caption = ctx.theme.text(TextRole::Caption);
        let scrub = line_of(ctx, &caption).max(look.section + look.related);
        let top = if compact {
            let body = ctx.theme.text(TextRole::Body);
            control.max(line_of(ctx, &body) + line_of(ctx, &caption))
        } else {
            control
        };
        2.0 * look.sibling + top + look.tight + scrub
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame pipeline deliberately keeps its ordered worker, layout, overlay, and chrome passes together"
    )]
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
        if let Some(notice) = &self.toast
            && notice.at.elapsed().as_secs_f32() > 2.6
        {
            self.toast = None;
        }

        // The sidebar drawer decides docked vs floating from the window width
        // (its breakpoint grows with the text), eases its reveal and hands
        // back the content column.
        let look = Look::of(ctx.theme);
        self.drawer.scale_for(ctx.theme);
        self.drawer.tick(ctx.theme);
        let content = self.drawer.resolve(self.rect);
        let r = self.rect;
        // The window's header row: across the content column, with the
        // toggle in it while the sidebar is hidden or floats (a floating
        // sidebar opens under the row); a docked sidebar holds the toggle
        // in its own row.
        let header_area = Rect::new(content.x0, r.y0, r.x1, r.y1);
        let history = self.history.is_shown();
        let mut header = if self.toggle_in_content() {
            self.drawer
                .header_with_history(header_area, ctx.theme, history)
        } else {
            let side = self.sidebar_rect();
            let _ = self
                .drawer
                .header(Rect::new(side.x0, r.y0, side.x1, r.y1), ctx.theme);
            HeaderRowFace::new(header_area, ctx.theme).with_history_if(history)
        };
        if !self.nav_back.is_empty() {
            header = header.with_back();
        }
        let content = self.drawer.resolve(self.rect);
        let header_h = header.row.height();
        let compact = Self::compact_bar(ctx.theme, &self.drawer, content.width());
        self.bar_h = Self::bar_height(ctx, &look, compact);
        let bar = self.nowplaying_rect();
        // The body scrolls in the toolkit's region; its content is laid out
        // beside the bar's lane while it overflows.
        self.body_scroll.set_viewport(
            Rect::new(content.x0, header.row.y1, r.x1, bar.y0),
            ctx.theme,
        );
        let body = self.body_scroll.content_rect();
        self.body_rect = body;

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

        self.draw_header(ctx, &look, &header);
        self.draw_nowplaying(ctx, &look, bar);
        self.open_staged_player_menu();

        // A floating drawer dims what is behind it, and a tap on that scrim
        // dismisses it.
        let scrim = self.drawer.scrim_rect();
        if scrim.width() > 0.5 {
            self.drawer.draw_scrim(ctx.scene, ctx.transform, ctx.theme);
            self.push_hit(scrim, Action::CloseSidebar);
        }
        // The sidebar (docked beside the content, or floating over it). Skip
        // it entirely when fully collapsed so its hits don't shadow content.
        if self.sidebar_rect().width() > 0.5 {
            self.draw_sidebar(ctx, &look, header_h);
        }

        // Collapsed-volume popover floats above the now-playing bar.
        self.draw_volume_popover(ctx, &look);
        // Queue hover preview floats above the now-playing bar's queue button.
        self.draw_queue_preview(ctx, &look);
        self.draw_toast(ctx);

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

    /// Open a `--shots`-staged More menu over the compact transport, once
    /// the bar has been laid out this frame.
    fn open_staged_player_menu(&mut self) {
        if !std::mem::take(&mut self.staged_player_menu) {
            return;
        }
        let more = self.hits.iter().find_map(|hit| match hit.action {
            Action::OpenPlayerMenu(rect) => Some(rect),
            _ => None,
        });
        if let Some(more) = more {
            self.open_player_menu(more);
        }
    }

    // ---- sidebar ----------------------------------------------------------

    /// The navigation sidebar: the drawer's panel, the show/hide toggle in its
    /// header, the destinations and the playlists section, all painted as the
    /// toolkit's navigation rows.
    fn draw_sidebar(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, header_h: f64) {
        let side = self.sidebar_rect();
        // Clip to the (possibly sliding) panel so a collapse never spills
        // rows over the content beside it.
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &side,
        );
        let local = self.drawer.paint_panel(ctx.scene, ctx.transform, ctx.theme);

        // A docked sidebar holds the toggle in its own header row; a
        // floating one opens under the window's row, which keeps it.
        let mut y = if self.toggle_in_content() {
            side.y0 + look.related
        } else {
            self.drawer
                .paint_toggle(ctx.scene, ctx.transform, ctx.text, &local);
            side.y0 + header_h
        };
        let row_h = chrome::nav_row_height(&local);
        for dest in NAV_DESTINATIONS {
            let rect = Rect::new(side.x0, y, side.x1, y + row_h);
            let selected = self.view == dest.view;
            let hovered = self.push_hit(rect, Action::Nav(dest.view));
            let face = NavRowFace::new(rect, dest.label)
                .icon(dest.icon)
                .state(SelectionState::new(selected, hovered, false));
            chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, &local, &face);
            y += row_h;
        }

        // The playlists section, with its one "new playlist" action.
        let section_h = chrome::nav_section_height(&local, false);
        let section = Rect::new(side.x0, y, side.x1, y + section_h);
        let mut face = NavSectionFace::new(section, "Playlists").action(Icon::Plus);
        let layout = chrome::nav_section_layout(&local, &face);
        if let Some(plus) = layout.action {
            face.action_hover = self.push_hit(plus, Action::BeginNewPlaylist);
        }
        chrome::paint_nav_section(ctx.scene, ctx.transform, ctx.text, &local, &face);
        y += section_h;

        for p in self.gather_playlists() {
            let rect = Rect::new(side.x0, y, side.x1, y + row_h);
            let selected = self.view == View::Playlist(p.id.clone());
            let hovered = self.push_hit(rect, Action::Nav(View::Playlist(p.id.clone())));
            let count = u32::try_from(p.count).unwrap_or(u32::MAX);
            let face = NavRowFace::new(rect, &p.name)
                .count(NavCount::Neutral(count))
                .state(SelectionState::new(selected, hovered, false));
            chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, &local, &face);
            y += row_h;
        }
        ctx.scene.pop_layer();
    }

    // ---- header -----------------------------------------------------------

    /// The content column's header row: the sidebar toggle while the
    /// sidebar is hidden or floats, Back, the page title, the open-file and
    /// search actions, and the window's History control in the trailing
    /// corner. One hairline separates it from the page.
    fn draw_header(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, face: &HeaderRowFace) {
        if self.toggle_in_content() {
            self.drawer
                .paint_toggle(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        }
        if face.back.width() > 0.0 {
            self.back_button.apply_layout_external(face.back);
            self.back_button.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }

        // The two header actions are retained buttons; they end where the
        // row's actions band ends, before History.
        let band = face.actions;
        let mid = band.center().y;
        let side = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let gap = face.metrics().gap;
        let search = Rect::new(band.x1 - side, mid - side / 2.0, band.x1, mid + side / 2.0);
        let open = Rect::new(
            search.x0 - gap - side,
            search.y0,
            search.x0 - gap,
            search.y1,
        );
        self.search_button.apply_layout_external(search);
        self.open_file_button.apply_layout_external(open);
        if self.view == View::Home {
            self.open_file_button.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }
        self.search_button.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::empty(),
        );
        self.history_rect = face.history_rect();
        self.history.paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            self.history_rect,
        );

        if let Some(title) = self.header_title() {
            let style = ctx.theme.text(TextRole::Heading);
            let x = face.title.x0;
            text_in_band(
                ctx,
                &title,
                &style,
                look.text,
                x,
                face.row,
                open.x0 - look.sibling - x,
            );
        }
        face.paint_rule(ctx.scene, ctx.transform, ctx.theme);
    }

    /// The header's title: the destination's name. A detail page names its
    /// subject in its own hero, so its header carries no title.
    fn header_title(&self) -> Option<String> {
        match &self.view {
            View::Album(_) | View::Artist(_) | View::Playlist(_) => None,
            _ => Some(self.view_title()),
        }
    }

    fn view_title(&self) -> String {
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
                let rows = self.gather_tracks(&ids);
                let y = y + look.section;
                self.draw_track_table(ctx, look, body, y, &TrackTable::new(&rows, &ids))
            }
            View::Albums => {
                let albums = self.gather_albums(None);
                self.draw_album_grid(ctx, look, body, y + look.gutter, &albums)
            }
            View::Artists => {
                let artists = self.gather_artists();
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
            View::Browse => self.draw_browse(ctx, look, body, y),
            View::Album(id) => self.draw_album_detail(ctx, look, body, y, &id),
            View::Artist(id) => self.draw_artist_detail(ctx, look, body, y, &id),
            View::Playlist(id) => self.draw_playlist_detail(ctx, look, body, y, &id),
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
        rect.y1
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
        if !self.lib.tracks.is_empty() {
            y = self.draw_recommendations(
                ctx,
                (body.x0 + look.gutter, body.x1 - look.gutter),
                y,
                false,
            ) + look.gutter;
        }

        // Remote catalogue content when it has landed.
        if let Some(home) = self.home.take() {
            for section in &home.sections {
                y = Self::section_heading(ctx, look, body, y, &section.title);
                y = self.draw_home_section(ctx, look, body, y, section);
                y += look.gutter;
            }
            self.home = Some(home);
            return y;
        }

        // While a real Home fetch is in flight, show the shape of the grid
        // it will fill. Once a fetch has failed (offline build / network
        // error) the honest state below settles instead.
        if self.home_is_loading() {
            return self.draw_home_skeleton(ctx, look, body, y);
        }

        // Nothing live, and nothing in the library yet: the first-run state.
        if self.lib.playlists.is_empty() && self.lib.albums.is_empty() && self.lib.tracks.is_empty()
        {
            let content = EmptyStateContent {
                icon: Some(Icon::Music),
                message: Some("Find a song, album or artist, or open an audio file."),
                action_label: Some("Search music"),
                ..EmptyStateContent::new("Your music starts here")
            };
            return self.empty_page(ctx, body, y, &content, Some(Action::OpenSearch));
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
        self.draw_track_table(
            ctx,
            look,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
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
        let len = table.rows.len();
        let rows: Vec<_> = table
            .rows
            .iter()
            .enumerate()
            .map(|(i, tk)| self.track_cell(tk, i, len, table, downloads))
            .collect();
        // A membership opened from search stays lifted on its row.
        let selected = table.playlist.and_then(|playlist| {
            let (owner, track) = self.revealed_membership.as_ref()?;
            (owner == playlist)
                .then(|| table.rows.iter().position(|tk| &tk.id == track))
                .flatten()
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
        } else if table.playlist.is_some_and(|id| !id.starts_with("spotify:"))
            || table.queue_offset.is_some()
        {
            Actions::Reorder {
                playlist: table.playlist.is_some_and(|id| !id.starts_with("spotify:")),
            }
        } else {
            Actions::Track
        };
        let action_count = match actions {
            Actions::Download => 1,
            Actions::Track => 2,
            Actions::Reorder { .. } => 5,
        };
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
        }
    }

    /// One row of a track table as its cells show it.
    fn track_cell(
        &self,
        tk: &TrackDisp,
        index: usize,
        len: usize,
        table: &TrackTable<'_>,
        downloads: bool,
    ) -> super::track_table::TrackCell {
        use super::track_table::{Offline, Place, Playback};
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
            number: tk.no.max(index as u32 + 1),
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
            place: Place::of(index, len),
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
            WindowEvent::CursorMoved { position, .. } => {
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor)
            }
            _ => self.mouse,
        };
        let covered = !self.body_rect.contains(mouse)
            || self
                .hits
                .get(self.body_hits..)
                .is_some_and(|chrome| chrome.iter().any(|hit| hit.rect.contains(mouse)))
            || (self.volume_popover_open && self.volume_popover_rect.contains(mouse));
        let mut asked = Vec::new();
        let mut tooltip = None;
        for (index, slot) in self.track_tables.iter_mut().enumerate() {
            if covered {
                // The pointer is not over the page: no row stays lifted.
                if let WindowEvent::CursorMoved { device_id, .. } = event {
                    slot.table.handle_event(
                        &WindowEvent::CursorLeft {
                            device_id: *device_id,
                        },
                        ctx,
                    );
                }
                continue;
            }
            slot.table.handle_event(event, ctx);
            tooltip = tooltip.or_else(|| slot.table.tooltip_at(mouse));
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
                    grid.handle_event(
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
                grid.handle_event(event, ctx);
            }
            cards.extend(grid.take_asked());
        }
        for part in &mut self.page_parts {
            if covered {
                if let WindowEvent::CursorMoved { device_id, .. } = event {
                    part.handle_event(
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
                part.handle_event(event, ctx);
            }
            if self.table_tooltip.is_none()
                && let Some(tip) = part.tooltip_at(mouse)
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
        let slot = self.track_tables.get(index)?;
        let offset = slot.queue_offset.unwrap_or(0);
        let row = |i: usize| slot.table.rows().get(i);
        let up = matches!(ask, RowEvent::MoveUp(_));
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
            RowEvent::MoveUp(i) | RowEvent::MoveDown(i) => {
                let to = if up { i.saturating_sub(1) } else { i + 1 };
                Some(match &slot.playlist {
                    Some(playlist) => Action::MovePlaylistItem {
                        playlist: playlist.clone(),
                        from: i,
                        to,
                    },
                    None => Action::MoveQueueItem {
                        from: offset + i,
                        to: offset + to,
                    },
                })
            }
            RowEvent::Remove(i) => match &slot.playlist {
                Some(playlist) => row(i).map(|track| Action::RemoveFromPlaylist {
                    playlist: playlist.clone(),
                    track: track.id.clone(),
                }),
                None => Some(Action::RemoveFromQueue(offset + i)),
            },
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
            self.page_parts.push(super::page_part::PagePart::new());
        }
        let clip = self.body_rect;
        let Some(part) = self.page_parts.get_mut(index) else {
            return top;
        };
        part.update(key, build);
        part.place(ctx, span, top, clip)
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
        let wide = body.width() >= 3.0 * MediaSize::Regular.extent(theme);
        let side = if wide {
            MediaSize::Regular.extent(theme)
        } else {
            MediaSize::Small.extent(theme)
        }
        .min(right - left);
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

        // The page's actions, all labelled the same way (a label that names
        // what happens, no icons): Play first (the one primary action), then
        // Shuffle, then the page's own.
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
            DetailKind::Playlist { own: true } => {
                buttons.push((
                    "Rename",
                    ButtonRole::Secondary,
                    Action::BeginRenamePlaylist(id.clone()),
                ));
                buttons.push((
                    "Set cover",
                    ButtonRole::Secondary,
                    Action::PickPlaylistCover(id.clone()),
                ));
                buttons.push((
                    "Delete playlist",
                    ButtonRole::Destructive,
                    Action::DeletePlaylist(id),
                ));
            }
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
                row = row.push(Box::new(
                    Button::new(&format!("music.page.{label}"), label)
                        .role(role)
                        .on_click(super::page_part::asking(asked, action)),
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
            let ids = p.map(|p| p.track_ids.clone()).unwrap_or_default();
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

    /// A floating panel previewing the next few queued songs, shown while
    /// the pointer hovers the now-playing bar's queue button. Anchored just
    /// above the button and right-aligned to it.
    fn draw_queue_preview(&mut self, ctx: &mut PaintCtx<'_>, look: &Look) {
        if !self.queue_preview_active() {
            return;
        }
        let theme = ctx.theme;
        let btn = self.queue_btn_rect;
        let ids = self.queue_preview_tracks();
        let rows = self.gather_tracks(&ids);
        let panel_theme = theme.on_ground(chrome::panel_ground(theme));
        let heading = theme.text(TextRole::Subheading);
        let body = theme.text(TextRole::Body);
        let caption = theme.text(TextRole::Caption);
        let pad = look.sibling;
        let head_h = line_of(ctx, &heading);
        let row_h = (line_of(ctx, &body) + line_of(ctx, &caption)).max(look.target) + look.related;
        let w = MediaSize::Large
            .extent(theme)
            .min(self.rect.width() - 2.0 * look.related);
        let h = 2.0 * pad + head_h + look.related + rows.len().max(1) as f64 * row_h;
        let x1 = btn.x1.min(self.rect.x1 - look.related);
        let x0 = (x1 - w).max(self.rect.x0 + look.related);
        let y1 = btn.y0 - look.related;
        let y0 = (y1 - h).max(self.rect.y0 + look.related);
        let panel = Rect::new(x0, y0, x1, y1);
        let radius = f64::from(theme.tokens.radii.panel);
        surface::shadow(
            ctx.scene,
            ctx.transform,
            panel,
            radius,
            &theme.tokens.elevation.floating,
        );
        surface::panel(ctx.scene, ctx.transform, panel, radius, theme);
        // Swallow clicks landing on the panel so they don't fall through.
        self.push_hit(panel, Action::None);

        let mut y = panel.y0 + pad;
        let text_w = panel.width() - 2.0 * pad;
        y += text_line(
            ctx,
            "Up next",
            &heading,
            panel_theme.text_primary,
            Point::new(panel.x0 + pad, y),
            text_w,
        );
        y += look.related;
        if rows.is_empty() {
            text_line(
                ctx,
                "Nothing queued",
                &body,
                panel_theme.text_dim,
                Point::new(panel.x0 + pad, y),
                text_w,
            );
            return;
        }
        let side = row_h - look.related;
        for tk in &rows {
            let art = Rect::new(panel.x0 + pad, y, panel.x0 + pad + side, y + side);
            self.draw_cover(ctx, art, &tk.cover_id(), &tk.album);
            let tx = art.x1 + look.sibling;
            let maxw = panel.x1 - pad - tx;
            let block = line_of(ctx, &body) + line_of(ctx, &caption);
            let top = y + (side - block) / 2.0;
            let th = text_line(
                ctx,
                &tk.title,
                &body,
                panel_theme.text_primary,
                Point::new(tx, top),
                maxw,
            );
            text_line(
                ctx,
                &tk.artist,
                &caption,
                panel_theme.text_dim,
                Point::new(tx, top + th),
                maxw,
            );
            y += row_h;
        }
    }

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
        let line_h = line_of(ctx, &active_style) + look.related;
        let focus = current.unwrap_or(0);
        let auto = lyrics_scroll(current, line_h, band.height());
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &band,
        );
        let maxw = band.width().max(1.0);
        for (i, line) in self.lyrics.iter().enumerate() {
            let ly = band.y0 + i as f64 * line_h - auto;
            if ly < band.y0 - line_h || ly > band.y1 {
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
            let text = if line.text.trim().is_empty() {
                "…"
            } else {
                line.text.as_str()
            };
            let shown = elide(ctx.text, text, st, maxw);
            let h = ctx.text.measure_style(&shown, None, st).1;
            let y = ly + (line_h - f64::from(h)) / 2.0;
            ctx.text(&shown, st, ink, Point::new(band.x0, y), None);
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

    /// The transport along the bottom of the content column: the song and its
    /// like and lyrics actions, the playback controls over the scrubber, and
    /// the queue and volume. It is part of the content, set apart by one
    /// hairline. On a narrow content column it folds to the song,
    /// previous/play/next, volume and a More menu over the scrubber.
    fn draw_nowplaying(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        surface::separator(
            ctx.scene,
            ctx.transform,
            r.x0,
            r.x1,
            r.y0,
            0.0,
            ctx.theme.tokens.colors.surfaces.content,
        );
        // Only the wide transport draws a queue button; clear the captured rect
        // so the compact bar never reports a stale hover target.
        self.queue_btn_rect = Rect::ZERO;
        let inner = Rect::new(
            r.x0 + look.section,
            r.y0 + look.sibling,
            r.x1 - look.section,
            r.y1 - look.sibling,
        );
        if Self::compact_bar(ctx.theme, &self.drawer, r.width()) {
            self.draw_nowplaying_compact(ctx, look, inner);
            return;
        }
        let w = inner.width();
        let theme = ctx.theme;
        // Three columns: the song, the transport, the queue and volume. The
        // volume slider folds to a button with a popover on a narrower bar.
        let show_slider = w >= 2.5 * MediaSize::Large.extent(theme);
        let side_w = (w * 0.3).min(MediaSize::Large.extent(theme) * 1.25);
        let left = Rect::new(inner.x0, inner.y0, inner.x0 + side_w, inner.y1);
        let right = Rect::new(inner.x1 - side_w, inner.y0, inner.x1, inner.y1);
        let center = Rect::new(
            left.x1 + look.gutter,
            inner.y0,
            right.x0 - look.gutter,
            inner.y1,
        );
        self.draw_nowplaying_left(ctx, look, left);
        self.draw_nowplaying_transport(ctx, look, center);
        self.draw_nowplaying_right(ctx, look, right, show_slider);
    }

    /// The song: cover, title over artist, and its like and lyrics actions
    /// beside the title (or "Nothing playing").
    fn draw_nowplaying_left(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        let body = ctx.theme.text(TextRole::Body);
        let Some(tk) = cur else {
            text_in_band(ctx, "Nothing playing", &body, look.dim, r.x0, r, r.width());
            return;
        };
        let art = Rect::new(r.x0, r.y0, r.x0 + r.height(), r.y1);
        let cover = self.best_cover_id(&tk.id, &tk.album_id);
        self.draw_cover(ctx, art, &cover, &tk.album);
        // Clicking the art jumps to its album.
        if !tk.album_id.is_empty() {
            self.push_hit(art, Action::Nav(View::Album(tk.album_id.clone())));
        }
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let shortcuts = !self.is_local_file(&tk.id);
        let actions_w = if shortcuts {
            2.0 * control + look.tight
        } else {
            0.0
        };
        let tx = art.x1 + look.sibling;
        let text_x1 = r.x1 - actions_w - look.related;
        let title = body.strong();
        let caption = ctx.theme.text(TextRole::Caption);
        let block = line_of(ctx, &title) + line_of(ctx, &caption);
        let top = r.center().y - block / 2.0;
        let th = text_line(
            ctx,
            &tk.title,
            &title,
            look.text,
            Point::new(tx, top),
            text_x1 - tx,
        );
        let (secondary, ink) = match self.playback_error.as_deref() {
            Some(error) => (error, look.danger),
            None => (tk.artist.as_str(), look.dim),
        };
        text_line(
            ctx,
            secondary,
            &caption,
            ink,
            Point::new(tx, top + th),
            text_x1 - tx,
        );
        if !shortcuts {
            return;
        }
        // Like and lyrics follow the song's name, as one group with it.
        let shown_w = [(&tk.title, &title), (&secondary.to_string(), &caption)]
            .into_iter()
            .map(|(text, style)| {
                let shown = elide(ctx.text, text, style, text_x1 - tx);
                f64::from(ctx.text.measure_style(&shown, None, style).0)
            })
            .fold(0.0, f64::max);
        let x0 = (tx + shown_w + look.sibling).min(r.x1 - actions_w);
        let mid = r.center().y;
        let like = Rect::new(x0, mid - control / 2.0, x0 + control, mid + control / 2.0);
        let lyrics = Rect::new(
            like.x1 + look.tight,
            like.y0,
            like.x1 + look.tight + control,
            like.y1,
        );
        let liked = self.lib.is_liked(&tk.id);
        self.icon_button(
            ctx,
            ButtonFace::icon_only(like, Icon::Heart, if liked { "Unlike" } else { "Like" })
                .control_size(ControlSize::Regular),
            false,
            Action::ToggleLike(tk.id.clone()),
        );
        if liked {
            let side = f64::from(ctx.theme.tokens.metrics.icon_md);
            Icon::Heart.draw_filled(
                ctx.scene,
                ctx.transform,
                square_around(like.center(), side),
                look.text,
            );
        }
        self.icon_button(
            ctx,
            ButtonFace::icon_only(lyrics, Icon::Mic, "Lyrics").control_size(ControlSize::Regular),
            self.view == View::Lyrics,
            Action::Nav(View::Lyrics),
        );
    }

    /// Shuffle, previous, play/pause, next and repeat, centred over the
    /// scrubber with its elapsed and total times.
    fn draw_nowplaying_transport(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        let theme = ctx.theme;
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let playing = TransportControl::PlayPause {
            playing: self.pb.playing,
        };
        let play_side = TransportFace::side(playing, theme);
        let skip_side = TransportFace::side(TransportControl::Next, theme);
        let row_h = play_side.max(control);
        // Shuffle and repeat flank the three media controls a group's
        // width away; previous, play and next stand together.
        let row_w =
            2.0 * control + 2.0 * skip_side + play_side + 2.0 * look.sibling + 2.0 * look.related;
        let mid = r.y0 + row_h / 2.0;
        let mut x = r.center().x - row_w / 2.0;
        let mut place = |side: f64, gap: f64| {
            let rect = Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0);
            x += side + gap;
            rect
        };
        let shuffle = place(control, look.sibling);
        let prev = place(skip_side, look.related);
        let play = place(play_side, look.related);
        let next = place(skip_side, look.sibling);
        let repeat = place(control, 0.0);
        // With nothing to play the transport rests, disabled.
        let idle = self.pb.current.is_none() && self.pb.queue.is_empty();
        let act = |action: Action| (!idle).then_some(action);
        self.icon_button(
            ctx,
            ButtonFace::icon_only(shuffle, Icon::Shuffle, "Shuffle")
                .control_size(ControlSize::Regular)
                .disabled(idle),
            self.pb.shuffle && !idle,
            act(Action::ToggleShuffle).unwrap_or(Action::None),
        );
        self.transport_button(ctx, prev, TransportControl::Previous, idle, Action::Prev);
        self.transport_button(ctx, play, playing, idle, Action::TogglePlay);
        self.transport_button(ctx, next, TransportControl::Next, idle, Action::Next);
        let (icon, name) = match self.pb.repeat {
            RepeatMode::One => (Icon::Repeat1, "Repeat one"),
            RepeatMode::All => (Icon::Repeat, "Repeat all"),
            RepeatMode::Off => (Icon::Repeat, "Repeat"),
        };
        self.icon_button(
            ctx,
            ButtonFace::icon_only(repeat, icon, name)
                .control_size(ControlSize::Regular)
                .disabled(idle),
            self.pb.repeat != RepeatMode::Off && !idle,
            act(Action::CycleRepeat).unwrap_or(Action::None),
        );

        let y0 = r.y0 + row_h - control;
        let scrub = Rect::new(r.x0, y0 + control + look.tight, r.x1, r.y1);
        self.draw_scrubber(ctx, look, scrub, idle);
    }

    /// Elapsed time, the draggable position and the total time, in `r`.
    fn draw_scrubber(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect, idle: bool) {
        let dur = self.current_duration_ms();
        let pos = self.playhead_ms.min(dur as f64).max(0.0);
        let frac = if dur > 0 { pos / dur as f64 } else { 0.0 };
        let elapsed = ui::format::timecode((pos / 1000.0) as u64);
        let total = ui::format::timecode(dur / 1000);
        let numeric = ctx.theme.text(TextRole::Caption).numeric();
        let time_w = f64::from(ctx.text.measure_style("00:00", None, &numeric).0);
        text_right_in_band(ctx, &elapsed, &numeric, look.dim, r.x0 + time_w, r);
        text_in_band(ctx, &total, &numeric, look.dim, r.x1 - time_w, r, time_w);
        let slider = Rect::new(
            r.x0 + time_w + look.related,
            r.y0,
            r.x1 - time_w - look.related,
            r.y1,
        );
        let face = SliderFace::new(slider, frac);
        self.scrubber_rect = slider;
        self.scrubber_hit = face.hit_rect(ctx.theme);
        let active = self.scrubber_hit.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        let face = face
            .hovered(active && !idle)
            .dragging(self.drag == Some(Drag::Scrubber))
            .disabled(idle);
        Slider::paint(ctx.scene, ctx.transform, ctx.theme, &face);
    }

    /// Queue and volume. The queue button opens the queue (and restores the
    /// prior view when toggled off); hovering it previews the next songs.
    fn draw_nowplaying_right(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        r: Rect,
        show_slider: bool,
    ) {
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let mid = r.center().y;
        let muted = self.pb.volume == 0;
        let volume_icon = if muted { Icon::VolumeX } else { Icon::Volume2 };
        let mut x = r.x1;
        if show_slider {
            // Inline slider; the speaker is a mute toggle.
            self.volume_btn_rect = Rect::ZERO;
            self.volume_popover_open = false;
            let slider_w = MediaSize::Small.extent(ctx.theme);
            let slider = Rect::new(x - slider_w, mid - control / 2.0, x, mid + control / 2.0);
            self.draw_volume_slider(ctx, slider);
            x = slider.x0 - look.tight;
            let mute = Rect::new(x - control, mid - control / 2.0, x, mid + control / 2.0);
            self.icon_button(
                ctx,
                ButtonFace::icon_only(mute, volume_icon, if muted { "Unmute" } else { "Mute" })
                    .control_size(ControlSize::Regular),
                false,
                Action::ToggleMute,
            );
            x = mute.x0 - look.related;
        } else {
            // Collapsed: one volume button that opens a slider popover.
            self.volume_rect = Rect::ZERO;
            self.volume_hit = Rect::ZERO;
            let btn = Rect::new(x - control, mid - control / 2.0, x, mid + control / 2.0);
            self.volume_btn_rect = btn;
            self.icon_button(
                ctx,
                ButtonFace::icon_only(btn, volume_icon, "Volume")
                    .control_size(ControlSize::Regular),
                self.volume_popover_open,
                Action::ToggleVolumePopover,
            );
            x = btn.x0 - look.related;
        }
        let queue = Rect::new(x - control, mid - control / 2.0, x, mid + control / 2.0);
        self.queue_btn_rect = queue;
        self.icon_button(
            ctx,
            ButtonFace::icon_only(queue, Icon::ListMusic, "Queue")
                .control_size(ControlSize::Regular),
            self.view == View::Queue,
            Action::ToggleQueue,
        );
    }

    /// A horizontal volume slider filling `hit` (its whole interactive rect),
    /// registering the drag target. Shared by the inline slider and the
    /// collapsed-volume popover.
    fn draw_volume_slider(&mut self, ctx: &mut PaintCtx<'_>, hit: Rect) {
        let face = SliderFace::new(hit, f64::from(self.pb.volume) / 100.0);
        self.volume_rect = hit;
        self.volume_hit = face.hit_rect(ctx.theme);
        let active = self.volume_hit.contains(self.mouse) || self.drag == Some(Drag::Volume);
        let face = face
            .hovered(active)
            .dragging(self.drag == Some(Drag::Volume));
        Slider::paint(ctx.scene, ctx.transform, ctx.theme, &face);
    }

    /// The collapsed-volume popover: the shared anchored floating Panel
    /// (placed and painted as every popover is) with the mute toggle, a
    /// slider and the level, opening above the volume button.
    fn draw_volume_popover(&mut self, ctx: &mut PaintCtx<'_>, look: &Look) {
        if !self.volume_popover_open || self.volume_btn_rect.width() <= 0.0 {
            self.volume_popover_rect = Rect::ZERO;
            self.volume_popover_slider = Rect::ZERO;
            return;
        }
        let theme = ctx.theme;
        let btn = self.volume_btn_rect;
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let numeric = theme.text(TextRole::Caption).numeric();
        let pct_w = f64::from(ctx.text.measure_style("100", None, &numeric).0);
        let pw = control
            + MediaSize::Small.extent(theme)
            + pct_w
            + 2.0 * look.sibling
            + 2.0 * look.related;
        let ph = control + 2.0 * look.related;
        let (panel, _) = ui::widget::floating_surface::place_aligned(
            btn,
            (pw, ph),
            ui::widget::PopoverSide::Above,
            ui::widget::PanelAlign::Trailing,
            f64::from(theme.tokens.spacing.s1),
            ui::widget::floating_surface::panel_room(self.rect, theme),
        );
        self.volume_popover_rect = panel;
        ui::widget::floating_surface::paint(ctx.scene, ctx.transform, panel, theme);
        // Block clicks inside the panel from falling through.
        self.push_hit(panel, Action::None);

        let panel_theme = theme.on_ground(chrome::panel_ground(theme));
        let muted = self.pb.volume == 0;
        let mute = Rect::new(
            panel.x0 + look.related,
            panel.y0 + look.related,
            panel.x0 + look.related + control,
            panel.y1 - look.related,
        );
        let face = ButtonFace::icon_only(
            mute,
            if muted { Icon::VolumeX } else { Icon::Volume2 },
            if muted { "Unmute" } else { "Mute" },
        );
        let hovered = self.push_hit(face.hit_rect(&panel_theme), Action::ToggleMute);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &panel_theme,
            &face.hovered(hovered),
        );
        let pct = format!("{}", self.pb.volume);
        text_right_in_band(
            ctx,
            &pct,
            &numeric,
            panel_theme.text_dim,
            panel.x1 - look.sibling,
            panel,
        );
        let slider = Rect::new(
            mute.x1 + look.related,
            mute.y0,
            panel.x1 - look.sibling - pct_w - look.related,
            mute.y1,
        );
        self.volume_popover_slider = slider;
        self.draw_volume_slider(ctx, slider);
    }

    /// The narrow transport: the song beside previous/play/next, then the
    /// volume button and a More menu that holds shuffle, repeat, like,
    /// lyrics and the queue, over the scrubber. Everything the wide bar
    /// offers stays one press away.
    fn draw_nowplaying_compact(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        self.volume_rect = Rect::ZERO;
        self.volume_hit = Rect::ZERO;
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let caption = ctx.theme.text(TextRole::Caption);
        let scrub_h = line_of(ctx, &caption).max(look.section + look.related);
        let top = Rect::new(r.x0, r.y0, r.x1, r.y1 - scrub_h - look.tight);
        let playing = TransportControl::PlayPause {
            playing: self.pb.playing,
        };
        let play_side = TransportFace::side(playing, ctx.theme);
        let skip_side = TransportFace::side(TransportControl::Next, ctx.theme);
        let controls_w =
            2.0 * skip_side + play_side + 2.0 * control + 2.0 * look.related + 2.0 * look.sibling;
        let mid = top.center().y;
        let square = |x: f64, side: f64| Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0);
        let prev = square(top.x1 - controls_w, skip_side);
        let play = square(prev.x1 + look.related, play_side);
        let next = square(play.x1 + look.related, skip_side);
        let volume = square(next.x1 + look.sibling, control);
        let more = square(volume.x1 + look.sibling, control);
        let idle = self.pb.current.is_none() && self.pb.queue.is_empty();

        self.draw_compact_song(ctx, look, top, prev.x0 - look.sibling);

        self.transport_button(ctx, prev, TransportControl::Previous, idle, Action::Prev);
        self.transport_button(ctx, play, playing, idle, Action::TogglePlay);
        self.transport_button(ctx, next, TransportControl::Next, idle, Action::Next);
        // Volume opens the same slider popover as the mid-width bar.
        self.volume_btn_rect = volume;
        let volume_icon = if self.pb.volume == 0 {
            Icon::VolumeX
        } else {
            Icon::Volume2
        };
        self.icon_button(
            ctx,
            ButtonFace::icon_only(volume, volume_icon, "Volume").control_size(ControlSize::Regular),
            self.volume_popover_open,
            Action::ToggleVolumePopover,
        );
        self.icon_button(
            ctx,
            ButtonFace::icon_only(more, Icon::MoreHorizontal, "More controls")
                .control_size(ControlSize::Regular),
            false,
            Action::OpenPlayerMenu(more),
        );
        let scrub = Rect::new(r.x0, r.y1 - scrub_h, r.x1, r.y1);
        self.draw_scrubber(ctx, look, scrub, idle);
    }

    /// The compact bar's song: its cover (which opens the lyrics) beside
    /// title over artist, or "Nothing playing", ending at `text_x1`.
    fn draw_compact_song(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, top: Rect, text_x1: f64) {
        let caption = ctx.theme.text(TextRole::Caption);
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        let body = ctx.theme.text(TextRole::Body);
        if let Some(tk) = cur {
            let art = Rect::new(top.x0, top.y0, top.x0 + top.height(), top.y1);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(ctx, art, &cover, &tk.album);
            self.push_hit(art, Action::Nav(View::Lyrics));
            let tx = art.x1 + look.sibling;
            let title = body.strong();
            let block = line_of(ctx, &title) + line_of(ctx, &caption);
            let ty = top.center().y - block / 2.0;
            let th = text_line(
                ctx,
                &tk.title,
                &title,
                look.text,
                Point::new(tx, ty),
                text_x1 - tx,
            );
            text_line(
                ctx,
                &tk.artist,
                &caption,
                look.dim,
                Point::new(tx, ty + th),
                text_x1 - tx,
            );
        } else {
            text_in_band(
                ctx,
                "Nothing playing",
                &body,
                look.dim,
                top.x0,
                top,
                text_x1 - top.x0,
            );
        }
    }

    // ---- toast ------------------------------------------------------------

    fn draw_toast(&self, ctx: &mut PaintCtx<'_>) {
        let Some(notice) = self.toast.as_ref() else {
            return;
        };
        let area = Rect::new(
            self.body_rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1 - self.bar_h,
        );
        paint_transient(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            area,
            ToastAnchor::BottomCenter,
            &ToastCard::new(&notice.text, notice.kind),
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
                album_id: tk.album_id.clone(),
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
            .playlists
            .iter()
            .map(|p| PlaylistDisp {
                id: p.id.clone(),
                name: p.name.clone(),
                count: p.track_ids.len(),
            })
            .collect()
    }
}

impl TrackDisp {
    /// Cover-cache key for this track: the track id (live results record a
    /// per-track cover) falling back to album art via the placeholder.
    fn cover_id(&self) -> String {
        self.id.clone()
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

impl ui::widget::WindowHeader for MusicApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        self.history.set_control(control);
        self.dirty = true;
    }
}
