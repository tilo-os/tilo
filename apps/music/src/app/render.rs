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

use super::{
    Action, Affine, AlbumPage, Arc, ArtistPage, AutoCommit, CacheState, Color, Dimension, Drag,
    Fill, HashMap, HomeItem, HomeItemKind, HomeSection, Instant, MusicApp, NodeId, ObjId,
    PermissionContext, PlaylistPage, Point, PresenceCtx, Rect, RepeatMode, Scene, Size, Style,
    TaffyTree, TextRenderer, TextStyle, Theme, View, Widget, elide, icon::Glyph, is_live_album_id,
    is_live_id, is_live_playlist_id, lyrics,
};
use ui::widget::{AppModel, ModelPath};

use ui::chrome::{self, NavCount, NavRowFace, NavSectionFace, SelectionState};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::surface;
use ui::tokens::TextRole;
use ui::widget::Continuable;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, ButtonRole, CardFace, ControlSize, EmptyState, EmptyStateContent,
    Interaction, MediaArt, MediaBadge, MediaCardFace, MediaSize, SkeletonFace, SkeletonShape,
    Slider, SliderFace,
};
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
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
    reduce_motion: bool,
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
            reduce_motion: theme.reduce_motion,
        }
    }
}

/// The measured height of one line of `style`.
fn line_of(ctx: &mut PaintCtx<'_>, style: &TextStyle) -> f64 {
    f64::from(ctx.text.measure_style("Ag", None, style).1)
}

/// Paint one line of `text`, elided to `max_w`, with its line box's top at
/// `origin`. Returns the line's height.
fn text_line(
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
fn text_right_in_band(
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
fn square_around(c: Point, side: f64) -> Rect {
    Rect::from_center_size(c, (side, side))
}

/// Whether two rects are on screen inside `clip` (a scrolled-away row neither
/// paints nor catches clicks).
fn visible_in(rect: Rect, clip: Rect) -> bool {
    rect.y1 >= clip.y0 && rect.y0 <= clip.y1
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
            queue_offset: None,
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
/// column, the title block, an optional album column that shares the free
/// width with the title (so no dead gap opens between them), the row's
/// actions and the right-aligned duration.
#[derive(Clone, Copy, Debug)]
pub(super) struct TrackColumns {
    pub index: Option<Rect>,
    pub title_x: f64,
    pub title_x1: f64,
    pub album: Option<(f64, f64)>,
    /// The leading edge of the action cluster.
    pub actions_x: f64,
    /// The trailing edge the duration ends at.
    pub duration_x1: f64,
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
        gap,
        min_text,
    } = w;
    let index = (index_w > 0.0).then(|| Rect::new(left, 0.0, left + index_w, 0.0));
    let title_x = if index_w > 0.0 {
        left + index_w + gap
    } else {
        left
    };
    let duration_x1 = right;
    let actions_x = right - duration_w - gap - action_w * actions as f64;
    let free = (actions_x - gap - title_x).max(0.0);
    let (title_x1, album) = if album && free >= 2.0 * min_text + gap {
        let title_w = ((free - gap) * 0.55).max(min_text);
        let album_x = title_x + title_w + gap;
        (title_x + title_w, Some((album_x, actions_x - gap)))
    } else {
        (actions_x - gap, None)
    };
    TrackColumns {
        index,
        title_x,
        title_x1,
        album,
        actions_x,
        duration_x1,
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
        icon: Icon::ThumbsUp,
        label: "Liked songs",
        view: View::Liked,
    },
    NavDestination {
        icon: Icon::Download,
        label: "Downloads",
        view: View::Downloads,
    },
    NavDestination {
        icon: Icon::ListVideo,
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
/// art and its toggle), its kind, title, a secondary line and a caption,
/// and the queue its Play and Shuffle buttons start.
struct DetailHeader<'a> {
    id: &'a str,
    kind: DetailKind,
    /// The kind of page, as the overline above the title ("Album").
    label: &'a str,
    title: &'a str,
    secondary: &'a str,
    caption: &'a str,
    queue: Vec<String>,
}

/// One visible row of a [`TrackTable`]: its track and index in the table, its
/// band, and whether it is the current track or under the pointer.
struct TrackRow<'a> {
    track: &'a TrackDisp,
    index: usize,
    band: Rect,
    is_current: bool,
    hovered: bool,
}

/// Two-column geometry for the lyrics view. On wide bodies a now-playing card
/// (cover art + track info) fills the left column and the lyric lines centre
/// within the remaining right column — so the view reads as a balanced pair
/// instead of a centred column with a large empty left half. Narrow bodies
/// keep a single full-width centred column (no art).
pub(super) struct LyricsLayout {
    /// Cover-art square for the left card, or `None` on narrow layouts.
    pub art: Option<Rect>,
    /// Horizontal band the lyric lines are centred within.
    pub lyrics: Rect,
}

/// Lay the lyrics view out in `body`: two columns once the body holds a
/// cover of `art_max` beside a readable lyrics column (`2 × art_max`),
/// separated and inset by `gutter`.
pub(super) fn lyrics_layout(
    body: Rect,
    band_top: f64,
    band_bottom: f64,
    art_max: f64,
    gutter: f64,
) -> LyricsLayout {
    if body.width() >= 3.0 * art_max + 3.0 * gutter {
        let side = art_max.min(band_bottom - band_top).max(0.0);
        let x0 = body.x0 + gutter;
        let art = Rect::new(x0, band_top, x0 + side, band_top + side);
        LyricsLayout {
            art: Some(art),
            lyrics: Rect::new(art.x1 + gutter, body.y0, body.x1 - gutter, body.y1),
        }
    } else {
        LyricsLayout {
            art: None,
            lyrics: Rect::new(body.x0 + gutter, body.y0, body.x1 - gutter, body.y1),
        }
    }
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

/// A responsive grid of media cards: as many columns of at least the small
/// media extent as fit, the media grid's own column and row gaps.
#[derive(Clone, Copy, Debug)]
struct Grid {
    x0: f64,
    cols: usize,
    card_w: f64,
    col_gap: f64,
    row_gap: f64,
}

impl Grid {
    fn of(theme: &Theme, look: &Look, body: Rect) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        let col_gap = f64::from(rhythm.section);
        let row_gap = f64::from(rhythm.group);
        let avail = (body.width() - 2.0 * look.gutter).max(0.0);
        let cols = ui::widget::card_columns(avail, MediaSize::Small.extent(theme), col_gap);
        Self {
            x0: body.x0 + look.gutter,
            cols,
            card_w: ui::widget::card_column_width(avail, cols, col_gap),
            col_gap,
            row_gap,
        }
    }

    fn x(&self, col: usize) -> f64 {
        self.x0 + col as f64 * (self.card_w + self.col_gap)
    }
}

/// One card of a media grid: what it shows and what a click on it does.
struct GridCard<'a> {
    art_id: &'a str,
    title: &'a str,
    meta: [Option<&'a str>; 2],
    badges: &'a [MediaBadge<'a>],
    action: Action,
    /// A Play action over the art while hovered.
    play: Option<Action>,
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
        _state: &mut AutoCommit,
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
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::widget::controls::ActionControls
    );

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.open_file_button, &self.search_button]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.open_file_button, &mut self.search_button]
    }

    /// The name prompt is a modal dialog: it paints in the window's top
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
        if self.name_prompt.is_some() {
            self.ensure_name_dialog();
            self.layout_name_dialog();
            if let Some(nd) = self.name_dialog.as_mut() {
                nd.dialog
                    .draw_overlay(scene, transform, text_renderer, theme, presence);
            }
        }
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.ensure_seeded(state);
        if self.playback_loaded {
            self.reload_library(state);
        } else {
            self.reload(state);
            self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
            self.playback_loaded = true;
        }
        // Progress in-flight downloads and re-apply their device-local cache
        // projection on top of the just-reloaded shared library.
        self.advance_downloads(state);
        // External media control (media keys / desktop widget) and a completed
        // file-portal cover pick both mutate the document, so they run here
        // where the CRDT handle is available. The portal conversion completion
        // follows the pick; decode/resize work itself runs off-thread.
        self.drain_mpris(state);
        self.drain_cover_pick(state);
        self.drain_local_file_pick();
        self.drain_cover_embed(state);
        self.apply_pending_media_handoff(state);
        self.publish_mpris();
        self.publish_handoff();
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::MUSIC_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
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
        let children = vec![
            self.open_file_button.build_layout(taffy, theme, node_map),
            self.search_button.build_layout(taffy, theme, node_map),
        ];
        #[allow(
            clippy::expect_used,
            reason = "the child belongs to this same tree; Widget::build_layout has no error return"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            )
            .expect("a parent and child in the same taffy tree");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
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
                ctx.state,
                ctx.handled,
            );
            return;
        }

        if let Ok(children) = ctx.taffy.children(ctx.node_id) {
            if let Some(node) = children.first() {
                self.open_file_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
            if !*ctx.handled
                && let Some(node) = children.get(1)
            {
                self.search_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
        }
        if *ctx.handled {
            self.dirty = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.on_cursor_moved(event, ctx.scale_factor, ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.on_right_click(ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(ctx.state, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(*delta, ctx.scale_factor, ctx.handled);
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
                    ctx.state,
                    ctx.handled,
                );
            }
            _ => {}
        }
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
                    | (View::Songs, "songs")
                    | (View::Albums, "albums")
                    | (View::Artists, "artists")
                    | (View::Playlists, "playlists")
                    | (View::Downloads, "downloads")
                    | (View::Liked, "liked")
                    | (View::Browse, "search")
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
        // The custom-painted empty-state button is page content, not a
        // transport control. Give it a separate semantic group and route it to
        // the same reducer as the visible button.
        if matches!(self.view, View::Home)
            && !self.home_is_loading()
            && self.lib.playlists.is_empty()
            && self.lib.albums.is_empty()
            && self.lib.tracks.is_empty()
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
            View::Songs => "Songs".to_string(),
            View::Albums => "Albums".to_string(),
            View::Artists => "Artists".to_string(),
            View::Playlists => "Playlists".to_string(),
            View::Downloads => "Downloads".to_string(),
            View::Liked => "Liked Songs".to_string(),
            View::Browse => "Search results".to_string(),
            View::Queue => "Queue".to_string(),
            View::Lyrics => "Lyrics".to_string(),
            View::Album(id) => self.lib.album_title(id).to_string(),
            View::Artist(id) => self.lib.artist_name(id).to_string(),
            View::Playlist(id) => self
                .lib
                .playlists
                .iter()
                .find(|playlist| playlist.id == *id)
                .map_or_else(|| "Playlist".to_string(), |playlist| playlist.name.clone()),
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
        }
        if !list_items.is_empty() {
            page_content.push(
                AccessNode::new(Role::List)
                    .with_name(format!("{page_name} items"))
                    .with_children(list_items),
            );
        }
        let page = AccessNode::new(Role::TabPanel)
            .with_hashed_key(format!("page:{page_name}"))
            .with_name(page_name)
            .with_children(page_content);

        let children = vec![navigation, search, page, player];
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
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
        // Keep frames flowing while playing / a toast fades, and — crucially —
        // while the streaming worker has an in-flight search or stream
        // resolution, so its result is drained (and applied to the audio
        // engine) promptly without waiting for the next input event.
        self.pb.playing
            || self.audio.needs_poll()
            || self.local_file_pick.is_some()
            || self.toast.is_some()
            || self.live_search_pending.is_some()
            || self.live_search.is_busy()
            || self.resolve_in_flight > 0
            || self.images_pending()
            || self.lyrics_pending_for.is_some()
            || self.search_edit_at.is_some()
            || self.home_is_loading()
            || self.detail_loading()
            // Keep frames flowing so the download spinner spins + progresses.
            || !self.downloads.is_empty()
            || !self.offline_resolves.is_empty()
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
    }
}

impl ui::search::capability::SearchSource for MusicApp {
    /// Fold the user's library and the live catalogue into the platform
    /// search. The app retains one prebuilt
    /// [`LibrarySearchProvider`](crate::search_type::LibrarySearchProvider)
    /// per library revision, and this returns an O(1) shared-catalog clone, so
    /// opening search never reindexes the corpus. The live `YouTube Music`
    /// catalogue answers asynchronously through
    /// [`LiveCatalogProvider`](crate::live_search::LiveCatalogProvider).
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![
            Box::new(self.library_search_provider.clone()),
            Box::new(self.live_search.provider()),
        ]
    }

    /// Activate a live-catalogue hit: a song plays, anything else opens its
    /// page. Library hits carry canonical action addresses and never land
    /// here.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AppModel, path: &ModelPath) {
        let Some(hit) = crate::live_search::LiveHit::parse(action_id) else {
            return;
        };
        let (state, _) = ui::doc::document(doc, path);
        self.inject_live_hits();
        let action = match hit {
            crate::live_search::LiveHit::Track(id) => {
                if self.playback_track(&id).is_none() {
                    return;
                }
                Action::Play {
                    queue: vec![id].into(),
                    index: 0,
                }
            }
            crate::live_search::LiveHit::Album(id) => Action::Nav(View::Album(id)),
            crate::live_search::LiveHit::Artist(id) => Action::Nav(View::Artist(id)),
            crate::live_search::LiveHit::Playlist(id) => Action::Nav(View::Playlist(id)),
        };
        self.dispatch(action, state);
    }
}

impl ui::widget::controls::ActionControls for MusicApp {
    /// Search opened from the app's own painted controls (a Browse card, the
    /// empty library's action) goes out before the header buttons' intents.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.search_opener.take() {
            return Some(invocation);
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
}

impl ui::resources::PublishesResources for MusicApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// library door — keep only device-local player, queue and open-file extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return crate::actions::live_device_resources(&self.music_resource_ctx());
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "music.library" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for MusicApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its music action, so it records one undo boundary for it (see
    /// `MusicApp::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl MusicApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive authority boundary keeps every Music operation and its validation auditable together"
    )]
    pub(super) fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match (kind, operation) {
            (
                tilo_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenView { id },
            ) => {
                // A forged operation must not acquire arbitrary local routing
                // authority. Check the exported allowlist before converting it
                // to the exact view this app implements.
                if crate::actions::known_view_label(&id).is_none() {
                    return Err("unknown Music destination".into());
                }
                let Some(view) = Self::top_level_view(&id) else {
                    return Err("unsupported Music destination".into());
                };
                self.on_nav(view);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::PlayTrack { .. },
            ) => {
                return Err(
                    "streaming playback requires the lifecycle-capable operation host".into(),
                );
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::PlayQueueItem { .. },
            ) => {
                return Err("queue playback requires the lifecycle-capable operation host".into());
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::SetPlayback { playback },
            ) => {
                if playback.playing
                    && (self.pb.current != playback.current || !self.audio.has_stream())
                {
                    return Err(
                        "streaming playback requires the lifecycle-capable operation host".into(),
                    );
                }
                let _ = self.apply_resource_playback(playback)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::SetOffline { tracks, keep },
            ) => {
                if tracks.iter().any(|id| !self.lib.tracks.contains_key(id)) {
                    return Err("offline-cache target is no longer in the library".into());
                }
                if keep {
                    for id in tracks {
                        self.request_offline_download(&id)?;
                    }
                    self.toast = Some(("Preparing offline download".into(), ui::time::now()));
                    self.dirty = true;
                    return Ok(true);
                }
                // A stale client may still ask to remove legacy metadata. Do
                // all device-local object removals before refreshing the
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
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::RemoveQueueItem { index, track },
            ) => {
                self.apply_resource_remove_queue_item(index, &track)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::MoveQueueItem { index, track, to },
            ) => {
                self.apply_resource_move_queue_item(index, &track, to)?;
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenAlbum { id },
            ) => {
                if !self.lib.albums.contains_key(&id) {
                    return Err("album is no longer in the library".into());
                }
                self.on_nav(View::Album(id));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenArtist { id },
            ) => {
                if !self.lib.artists.contains_key(&id) {
                    return Err("artist is no longer in the library".into());
                }
                self.on_nav(View::Artist(id));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenPlaylist { id },
            ) => {
                if self.lib.playlist(&id).is_none() {
                    return Err("playlist is no longer in the library".into());
                }
                self.on_nav(View::Playlist(id));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenPlaylistMembership { playlist, track },
            ) => {
                let Some(current_playlist) = self.lib.playlist(&playlist) else {
                    return Err("playlist is no longer in the library".into());
                };
                if !current_playlist.track_ids.iter().any(|id| id == &track) {
                    return Err("playlist membership is no longer present".into());
                }
                let playlist_name = current_playlist.name.clone();
                let track_title = self
                    .playback_track(&track)
                    .map_or_else(|| track.clone(), |t| t.title.clone());
                self.on_nav(View::Playlist(playlist.clone()));
                self.revealed_membership = Some((playlist, track));
                self.toast = Some((
                    format!("Opened “{track_title}” in “{playlist_name}”"),
                    ui::time::now(),
                ));
            }
            _ => return Ok(false),
        }
        self.dirty = true;
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        match (kind, operation) {
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::ChooseAudioFile,
            ) => {
                self.begin_local_file_pick(invocation)?;
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::SetOffline { tracks, keep: true },
            ) => {
                if self.offline_invocation.is_some() {
                    return Err("another offline download operation is running".into());
                }
                let pending = tracks
                    .iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>();
                if pending.is_empty() {
                    return Err("offline download requires at least one track".into());
                }
                for track_id in &pending {
                    self.request_offline_download(track_id)?;
                }
                self.offline_invocation = Some((invocation, pending));
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::PlayTrack { id },
            ) => {
                if self.resource_stream_invocation.is_some() {
                    return Err("another Music stream-start operation is still resolving".into());
                }
                let request = self.apply_resource_play_track(&id)?;
                self.register_playback_start(invocation, request, 0, false)
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::PlayQueueItem { index, track },
            ) => {
                if self.resource_stream_invocation.is_some() {
                    return Err("another Music stream-start operation is still resolving".into());
                }
                let request = self.apply_resource_play_queue_item(index, &track)?;
                self.register_playback_start(invocation, request, 0, false)
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::MusicOperation::SetPlayback { playback },
            ) if playback.playing
                && (self.pb.current != playback.current
                    || !self.pb.playing
                    || !self.audio.has_stream()) =>
            {
                self.begin_resource_playback(invocation, playback)
            }
            _ => self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                }),
        }
    }
}

/// Map an x coordinate to a `0..=1` fraction within `r`.
fn frac_in(r: Rect, x: f64) -> f64 {
    if r.width() <= 0.0 {
        return 0.0;
    }
    ((x - r.x0) / r.width()).clamp(0.0, 1.0)
}

impl MusicApp {
    /// Hover labels for icon-only controls. Text-labelled rows and navigation
    /// deliberately return `None`; repeating their visible label would add
    /// noise rather than information.
    fn tooltip_at(&self, point: Point) -> Option<&str> {
        if self.toggle_rect.contains(point) {
            return Some(if self.drawer.is_open() {
                "Hide sidebar"
            } else {
                "Show sidebar"
            });
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
        if self.drain_menu_selection(ctx.state) {
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

    /// Track the pointer, forwarding to the search field while it's hovered or
    /// mid drag-select, and resolve an in-progress scrubber/volume drag.
    fn on_cursor_moved(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        state: &mut AutoCommit,
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
                    let f = frac_in(self.scrubber_rect, self.mouse.x);
                    self.dispatch(Action::SeekFrac(f), state);
                }
                Drag::Volume => {
                    let f = frac_in(self.volume_rect, self.mouse.x);
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

    /// Right-click: open a track's context menu where one is registered.
    fn on_right_click(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        if let Some((row, action)) = self.row_menu_at(self.mouse) {
            self.menu_anchor_row = Some(row);
            self.dispatch(action, state);
            *handled = true;
        }
    }

    /// Left press: volume popover dismiss / sidebar toggle / transport drag
    /// start / now-playing swipe capture / topmost hit — in priority order.
    fn on_left_press(&mut self, state: &mut AutoCommit, handled: &mut bool) {
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
        // The sidebar toggle wins over everything (it sits above the scrim
        // hit that otherwise covers the whole content in overlay).
        if self.toggle_rect.width() > 0.0 && self.toggle_rect.contains(self.mouse) {
            self.dispatch(Action::ToggleSidebar, state);
            *handled = true;
            return;
        }
        // Transport bars get drag priority.
        if self.scrubber_rect.contains(self.mouse) {
            self.drag = Some(Drag::Scrubber);
            let f = frac_in(self.scrubber_rect, self.mouse.x);
            self.dispatch(Action::SeekFrac(f), state);
            *handled = true;
            return;
        }
        if self.volume_rect.contains(self.mouse) {
            self.drag = Some(Drag::Volume);
            let f = frac_in(self.volume_rect, self.mouse.x);
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

    /// Left release: resolve a now-playing swipe, or end a scrubber/volume
    /// drag.
    fn on_left_release(&mut self, state: &mut AutoCommit, handled: &mut bool) {
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

    /// Scroll the body, clamped to content height.
    fn on_wheel(&mut self, delta: MouseScrollDelta, scale_factor: f64, handled: &mut bool) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 48.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
        };
        let max = (self.content_height - self.body_rect.height()).max(0.0);
        self.scroll = (self.scroll - dy).clamp(0.0, max);
        self.dirty = true;
        *handled = true;
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
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // The name-input overlay is modal and handled upstream in
        // `handle_event` (the shared ModalDialog owns typing / Esc / buttons),
        // so `on_key` never runs with it open.

        // (The focused search field routes its keystrokes to the real
        // `ui::TextInput` up in `handle_event`, before `on_key` is reached.)

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
    /// `rect`: the loaded image, else the toolkit's generated placeholder.
    /// `id` keys the cover cache.
    fn draw_cover(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str) {
        let image = self.art_image(id);
        let art = image
            .as_ref()
            .map_or(MediaArt::Placeholder, MediaArt::Image);
        MediaCardFace::new(art).paint(ctx, rect);
    }

    /// A large cover that names its subject: the placeholder carries the
    /// title's initials. The face sets the title under its art; only the art
    /// is kept.
    fn draw_titled_cover(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str, title: &str) {
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

    // ---- controls ---------------------------------------------------------

    /// Paint a labelled button at `rect` and register its hit. Returns
    /// whether the pointer is over it.
    fn button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        label: &str,
        icon: Option<Icon>,
        role: ButtonRole,
        action: Action,
    ) -> bool {
        let mut face = ButtonFace::new(rect, label).role(role);
        if let Some(icon) = icon {
            face = face.icon(icon);
        }
        let hit = face.hit_rect(ctx.theme);
        let hovered = self.push_hit(hit, action);
        let face = face.hovered(hovered);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        hovered
    }

    /// Paint an icon button `face` and register its hit. `on` shows a toggle
    /// that is on as the toolbar does: a full lift of the ground.
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        face: ButtonFace<'_>,
        on: bool,
        action: Action,
    ) -> bool {
        let hovered = self.push_hit(face.hit_rect(ctx.theme), action);
        let face = face.hovered(hovered && !on).pressed(on);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        hovered
    }

    /// An icon button for a playback glyph the shared icon set lacks
    /// (shuffle, repeat): the plain button's face with the glyph in its ink.
    fn glyph_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        glyph: Glyph,
        name: &str,
        on: bool,
        action: Action,
    ) {
        let face = ButtonFace::new(rect, "")
            .accessible_name(name)
            .role(ButtonRole::Plain);
        let hovered = self.push_hit(face.hit_rect(ctx.theme), action);
        let face = face.hovered(hovered && !on).pressed(on);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        let (_, _, ink) = face.colors(ctx.theme);
        let side = f64::from(ctx.theme.tokens.metrics.icon_md);
        glyph.draw(
            ctx.scene,
            ctx.transform,
            square_around(rect.center(), side),
            ink,
        );
    }

    /// The width a button with `label` (and `icon`) needs at `size`.
    fn button_width(
        ctx: &mut PaintCtx<'_>,
        label: &str,
        icon: Option<Icon>,
        size: ControlSize,
    ) -> f64 {
        let mut face = ButtonFace::new(Rect::ZERO, label).control_size(size);
        if let Some(icon) = icon {
            face = face.icon(icon);
        }
        face.intrinsic_width(ctx.text, ctx.theme)
    }

    // ---- master frame -----------------------------------------------------

    /// Height of the window's header row: the shell's header metric, grown
    /// with the column title at large text.
    fn header_height(ctx: &mut PaintCtx<'_>, look: &Look) -> f64 {
        let title = ctx.theme.text(TextRole::Heading);
        f64::from(ctx.theme.tokens.metrics.header_height)
            .max(line_of(ctx, &title) + 2.0 * look.related)
    }

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
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
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
        self.toggle_rect = Rect::ZERO;
        self.hits.clear();
        self.row_menu_hits.clear();
        self.sync_context_menu_geometry(ctx.theme);
        // Fold in any streaming-worker results (resolved stream URLs → audio
        // engine, real search hits → library), then keep live search results
        // visible after a `sync` reloaded the catalog from the document.
        self.drain_worker();
        self.inject_live_hits();
        self.poll_lyrics();
        self.maybe_prefetch();
        self.maybe_dispatch_live_search();
        self.pump_live_search();
        // Apply any deferred `--shots` staging now that `rect` is known.
        self.apply_shot_stage();
        self.tick();
        if let Some((_, at)) = &self.toast
            && at.elapsed().as_secs_f32() > 2.6
        {
            self.toast = None;
        }

        // The sidebar drawer decides docked vs floating from the window width
        // (its breakpoint grows with the text), eases its reveal and hands
        // back the content column.
        let look = Look::of(ctx.theme);
        self.drawer.scale_for(ctx.theme);
        self.drawer.tick(ctx.theme);
        let mut content = self.drawer.resolve(self.rect);
        if let Some(open) = self.staged_sidebar.take() {
            self.drawer.snap(open);
            content = self.drawer.resolve(self.rect);
        }
        let r = self.rect;
        let header_h = Self::header_height(ctx, &look);
        let compact = Self::compact_bar(ctx.theme, &self.drawer, content.width());
        self.bar_h = Self::bar_height(ctx, &look, compact);
        let header = Rect::new(content.x0, r.y0, r.x1, r.y0 + header_h);
        let bar = self.nowplaying_rect();
        let body = Rect::new(content.x0, header.y1, r.x1, bar.y0);
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
        let start_y = body.y0 - self.scroll;
        let end_y = self.draw_body(ctx, &look, body, start_y);
        ctx.scene.pop_layer();
        self.content_height = (end_y + self.scroll) - body.y0 + look.gutter;
        let max = (self.content_height - body.height()).max(0.0);
        if self.scroll > max {
            self.scroll = max;
        }
        self.open_staged_track_menu();

        self.draw_header(ctx, &look, header);
        self.draw_nowplaying(ctx, &look, bar);

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

        // The context menu paints last so it sits on top of all content.
        if self.context.is_open() {
            self.context.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }
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
        let row = self.row_menu_hits.iter().find_map(|hit| match &hit.action {
            Action::OpenTrackMenu { track: t, .. } if *t == track => Some(hit.rect),
            _ => None,
        });
        if let Some(row) = row {
            self.mouse = Point::new(row.x0 + row.width() * 0.3, row.center().y);
            self.menu_anchor_row = Some(row);
            self.open_track_menu(track, None, None);
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

        if !self.toggle_in_content() {
            let toggle = Rect::new(
                side.x0 + look.sibling,
                side.y0 + (header_h - look.target) / 2.0,
                side.x0 + look.sibling + look.target,
                side.y0 + f64::midpoint(header_h, look.target),
            );
            self.toggle_rect = toggle;
            let hovered = toggle.contains(self.mouse);
            self.drawer
                .draw_toggle(ctx.scene, ctx.transform, ctx.text, &local, toggle, hovered);
        }

        let row_h = chrome::nav_row_height(&local);
        let mut y = side.y0 + header_h;
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

    /// The content column's header: the sidebar toggle while the sidebar is
    /// hidden, Back, the page title, and the open-file and search actions.
    /// One hairline separates it from the page.
    fn draw_header(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        let mid = r.center().y;
        let mut x = r.x0 + look.sibling;
        if self.toggle_in_content() {
            let toggle = Rect::new(
                x,
                mid - look.target / 2.0,
                x + look.target,
                mid + look.target / 2.0,
            );
            self.toggle_rect = toggle;
            let hovered = toggle.contains(self.mouse);
            self.drawer.draw_toggle(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                toggle,
                hovered,
            );
            x = toggle.x1 + look.related;
        }
        if !self.nav_back.is_empty() {
            let back = Rect::new(
                x,
                mid - look.target / 2.0,
                x + look.target,
                mid + look.target / 2.0,
            );
            self.icon_button(
                ctx,
                ButtonFace::icon_only(back, Icon::ChevronLeft, "Back")
                    .control_size(ControlSize::Regular),
                false,
                Action::Back,
            );
            x = back.x1 + look.related;
        }
        if x <= r.x0 + look.sibling {
            // The title starts on the content gutter when nothing leads it.
            x = r.x0 + look.gutter;
        }

        // The two header actions are retained buttons; they lay out against
        // the trailing edge.
        let side = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let search = Rect::new(
            r.x1 - look.sibling - side,
            mid - side / 2.0,
            r.x1 - look.sibling,
            mid + side / 2.0,
        );
        let open = Rect::new(
            search.x0 - look.related - side,
            search.y0,
            search.x0 - look.related,
            search.y1,
        );
        self.search_button.apply_layout_external(search);
        self.open_file_button.apply_layout_external(open);
        self.open_file_button.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::empty(),
        );
        self.search_button.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::empty(),
        );

        if let Some(title) = self.header_title() {
            let style = ctx.theme.text(TextRole::Heading);
            text_in_band(
                ctx,
                &title,
                &style,
                look.text,
                x,
                r,
                open.x0 - look.sibling - x,
            );
        }
        surface::separator(
            ctx.scene,
            ctx.transform,
            r.x0,
            r.x1,
            r.y1,
            0.0,
            ctx.theme.tokens.colors.surfaces.content,
        );
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

    fn draw_home(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        // Kick off the real Home fetch (charts / new releases / top artists).
        self.ensure_home();
        let mut y = y + look.gutter;

        // Real YouTube-Music content when it has landed.
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
            return Self::draw_home_skeleton(ctx, look, body, y);
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
            .map(|it| format!("yt:{}", it.video_id))
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
            .filter_map(|(item, art_id)| {
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
                    HomeItemKind::Playlist => return None,
                };
                let play = matches!(item.kind, HomeItemKind::Track).then(|| action.clone());
                Some(GridCard {
                    art_id,
                    title: &item.title,
                    meta: [Some(item.subtitle.as_str()).filter(|s| !s.is_empty()), None],
                    badges: &[],
                    action,
                    play,
                })
            })
            .collect();
        self.draw_media_grid(ctx, look, body, y, &cards)
    }

    /// The cover-cache key of a Home item.
    fn home_item_art(item: &HomeItem) -> String {
        match item.kind {
            HomeItemKind::Track => format!("yt:{}", item.video_id),
            _ => item.id.clone(),
        }
    }

    /// The Home grid while it loads: two sections of card-shaped skeletons.
    fn draw_home_skeleton(ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, mut y: f64) -> f64 {
        let theme = ctx.theme;
        let grid = Grid::of(theme, look, body);
        let heading = SkeletonFace::new(SkeletonShape::Text { lines: 1 });
        let card = SkeletonFace::new(SkeletonShape::Media(ui::widget::MediaAspect::Square));
        for _ in 0..2 {
            let w = (grid.card_w * 1.5).min(body.width() - 2.0 * look.gutter);
            let h = heading.height(ctx.text, theme, w);
            heading.paint(ctx, Rect::new(grid.x0, y, grid.x0 + w, y + h));
            y += h + look.sibling;
            let card_h = card.height(ctx.text, theme, grid.card_w);
            for col in 0..grid.cols {
                let x = grid.x(col);
                card.paint(ctx, Rect::new(x, y, x + grid.card_w, y + card_h));
            }
            y += card_h + look.gutter;
        }
        y
    }

    // ---- media grids ------------------------------------------------------

    /// Lay `cards` out as a media grid from `y`; every card of a row shares
    /// the row's tallest text block so titles line up. Returns the y below
    /// the grid.
    fn draw_media_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
        cards: &[GridCard<'_>],
    ) -> f64 {
        let theme = ctx.theme;
        let g = Grid::of(theme, look, body);
        let images: Vec<Option<vello::peniko::ImageData>> = cards
            .iter()
            .map(|card| self.art_image(card.art_id))
            .collect();
        let faces: Vec<MediaCardFace<'_>> = cards
            .iter()
            .zip(&images)
            .map(|(card, image)| {
                let art = image
                    .as_ref()
                    .map_or(MediaArt::Placeholder, MediaArt::Image);
                let mut face = MediaCardFace::new(art)
                    .title(card.title)
                    .badges(card.badges);
                for meta in card.meta.iter().flatten() {
                    face = face.meta(meta);
                }
                if card.play.is_some() {
                    face = face.hover_action(Icon::Play);
                }
                face
            })
            .collect();
        for (row_cards, row_faces) in cards.chunks(g.cols).zip(faces.chunks(g.cols)) {
            let text_h = row_faces
                .iter()
                .map(|face| face.text_height(ctx.text, theme, g.card_w))
                .fold(0.0, f64::max);
            let card_h = g.card_w + text_h;
            for (col, (card, face)) in row_cards.iter().zip(row_faces).enumerate() {
                let x = g.x(col);
                let rect = Rect::new(x, y, x + g.card_w, y + card_h);
                if !visible_in(rect, self.body_rect) {
                    continue;
                }
                let hovered = self.body_hit(rect, card.action.clone());
                if hovered
                    && let Some(play) = &card.play
                    && let Some(target) = face.hover_action_rect(rect, theme)
                {
                    self.push_hit(target, play.clone());
                }
                face.interaction(Interaction {
                    hovered,
                    ..Interaction::default()
                })
                .paint(ctx, rect);
            }
            y += card_h + g.row_gap;
        }
        if cards.is_empty() { y } else { y - g.row_gap }
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
                badges: &[],
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
                badges: &[],
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
                badges: &[],
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
        let label = "New playlist";
        let w = Self::button_width(ctx, label, Some(Icon::Plus), ControlSize::Regular);
        let h = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let x = body.x0 + look.gutter;
        self.button(
            ctx,
            Rect::new(x, y, x + w, y + h),
            label,
            Some(Icon::Plus),
            ButtonRole::Secondary,
            Action::BeginNewPlaylist,
        );
        let y = y + h + look.gutter;
        if pls.is_empty() {
            return Self::note(ctx, look, body, y, "Playlists you make appear here.");
        }
        self.draw_playlist_cards(ctx, look, body, y, pls)
    }

    // ---- track table ------------------------------------------------------

    /// The track list every view shares (songs, an album, a playlist, liked
    /// songs, downloads, the queue): index or equalizer, title over artist,
    /// album, the row's actions (shown on hover, or muted for a liked or
    /// downloaded song) and a right-aligned duration in tabular figures.
    #[allow(
        clippy::too_many_lines,
        reason = "one table pass measures its columns, registers each row's hits and paints it"
    )]
    fn draw_track_table(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        mut y: f64,
        table: &TrackTable<'_>,
    ) -> f64 {
        let theme = ctx.theme;
        let left = body.x0 + look.gutter;
        let right = body.x1 - look.gutter;
        let row_h = ui::widget::ListRowFace::new("")
            .secondary("")
            .height(ctx.text, theme);
        let numeric = theme.text(TextRole::Caption).numeric();
        let index_w = if table.number_col {
            f64::from(ctx.text.measure_style("000", None, &numeric).0)
        } else {
            0.0
        };
        let duration_w = f64::from(ctx.text.measure_style("00:00", None, &numeric).0);
        let action_w = ButtonFace::height(ControlSize::Small, theme);
        let downloads = matches!(self.view, View::Downloads);
        let reorder = table.playlist.is_some() || table.queue_offset.is_some();
        let actions = if downloads {
            1
        } else {
            2 + if reorder { 3 } else { 0 }
        };
        let cols = track_columns(
            left,
            right,
            ColumnWidths {
                index: index_w,
                duration: duration_w,
                action: action_w,
                actions,
                gap: look.sibling,
                min_text: MediaSize::Small.extent(theme),
            },
            table.show_album || downloads,
        );
        let shared_queue = Arc::new(table.queue.to_vec());
        let offset = table.queue_offset.unwrap_or(0);
        let ground = theme.tokens.colors.surfaces.content;

        for (i, tk) in table.rows.iter().enumerate() {
            let band = Rect::new(left, y, right, y + row_h);
            y += row_h;
            if !visible_in(band, self.body_rect) {
                continue;
            }
            let row_rect = Rect::new(left - look.related, band.y0, right + look.related, band.y1);
            let is_current = if table.queue_offset.is_some() {
                self.pb.queue_index == offset + i && self.pb.current.is_some()
            } else {
                self.pb.current.as_deref() == Some(tk.id.as_str())
            };
            let hovered = self.body_hit(
                row_rect,
                Action::Play {
                    queue: Arc::clone(&shared_queue),
                    index: offset + i,
                },
            );
            // Right-click anywhere on the row opens its context menu (bound to
            // this playlist when in one, so "Remove from playlist" targets it).
            self.push_row_menu(
                row_rect,
                Action::OpenTrackMenu {
                    track: tk.id.clone(),
                    in_playlist: table.playlist.map(str::to_string),
                    queue_index: table.queue_offset.map(|o| o + i),
                },
            );
            let revealed = table.playlist.is_some_and(|playlist| {
                self.revealed_membership
                    .as_ref()
                    .is_some_and(|(owner, track)| owner == playlist && track == &tk.id)
            });
            chrome::paint_row_selection(
                ctx.scene,
                ctx.transform,
                row_rect,
                SelectionState::new(revealed, hovered, false),
                ground,
                theme,
            );
            let row = TrackRow {
                track: tk,
                index: i,
                band,
                is_current,
                hovered,
            };
            self.draw_track_row(ctx, look, &cols, table, &row);
        }
        y
    }

    /// One row's content inside its band.
    #[allow(
        clippy::too_many_lines,
        reason = "each column of the row is a few lines; together they are one row"
    )]
    fn draw_track_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        cols: &TrackColumns,
        table: &TrackTable<'_>,
        row: &TrackRow<'_>,
    ) {
        let theme = ctx.theme;
        let band = row.band;
        let tk = row.track;
        let numeric = theme.text(TextRole::Caption).numeric();

        // Index column: Play under the pointer, the equalizer for the playing
        // track, else the track number.
        if let Some(index) = cols.index {
            let slot = Rect::new(index.x0, band.y0, index.x1, band.y1);
            if row.hovered {
                let side = f64::from(theme.tokens.metrics.icon_sm);
                Icon::Play.draw(
                    ctx.scene,
                    ctx.transform,
                    square_around(Point::new(slot.x1 - side / 2.0, slot.center().y), side),
                    look.text,
                );
            } else if row.is_current {
                self.draw_equalizer(ctx, look, slot);
            } else {
                let num = format!("{}", tk.no.max(row.index as u32 + 1));
                text_right_in_band(ctx, &num, &numeric, look.faint, slot.x1, slot);
            }
        }

        // Title over artist, centred as one block.
        let title_style = if row.is_current {
            theme.text(TextRole::Body).strong()
        } else {
            theme.text(TextRole::Body)
        };
        let caption = theme.text(TextRole::Caption);
        let block = line_of(ctx, &title_style) + line_of(ctx, &caption);
        let top = band.y0 + (band.height() - block) / 2.0;
        let text_w = cols.title_x1 - cols.title_x;
        let title_h = text_line(
            ctx,
            &tk.title,
            &title_style,
            look.text,
            Point::new(cols.title_x, top),
            text_w,
        );
        text_line(
            ctx,
            &tk.artist,
            &caption,
            look.dim,
            Point::new(cols.title_x, top + title_h),
            text_w,
        );

        let downloads = matches!(self.view, View::Downloads);
        if let Some((album_x, album_x1)) = cols.album {
            if downloads {
                let (status, failed) = self.download_status(&tk.id);
                let ink = if failed { look.danger } else { look.dim };
                text_in_band(
                    ctx,
                    &status,
                    &caption,
                    ink,
                    album_x,
                    band,
                    album_x1 - album_x,
                );
            } else {
                let body = theme.text(TextRole::Body);
                text_in_band(
                    ctx,
                    &tk.album,
                    &body,
                    look.dim,
                    album_x,
                    band,
                    album_x1 - album_x,
                );
            }
        }

        // The action cluster, left to right.
        let action_w = ButtonFace::height(ControlSize::Small, theme);
        let mid = band.center().y;
        let slot = |k: usize| {
            let x = cols.actions_x + k as f64 * action_w;
            Rect::new(x, mid - action_w / 2.0, x + action_w, mid + action_w / 2.0)
        };
        let local = self.is_local_file(&tk.id);
        if downloads {
            if !local {
                self.draw_download_action(ctx, look, slot(0), &tk.id, true);
            }
        } else {
            let mut k = 0;
            if table.playlist.is_some() || table.queue_offset.is_some() {
                if row.hovered {
                    self.draw_reorder_actions(ctx, table, row, [slot(0), slot(1), slot(2)]);
                }
                k = 3;
            }
            if !local {
                self.draw_like_action(ctx, look, slot(k), &tk.id, row.hovered);
                self.draw_download_action(ctx, look, slot(k + 1), &tk.id, row.hovered);
            }
        }

        // Duration (hidden when unknown — some live sources omit it).
        if tk.dur > 0 {
            let dur = ui::format::timecode(u64::from(tk.dur));
            text_right_in_band(ctx, &dur, &numeric, look.dim, cols.duration_x1, band);
        }
    }

    /// The now-playing equalizer: three bars that move while the track plays
    /// and hold still when it is paused (or motion is reduced).
    fn draw_equalizer(&self, ctx: &mut PaintCtx<'_>, look: &Look, slot: Rect) {
        let bars = eq_bars(self.pb.playing, look.reduce_motion);
        let bar_w = look.tight - 1.0;
        let pitch = look.tight + look.tight / 2.0;
        let max_h = f64::from(ctx.theme.tokens.metrics.icon_sm);
        let x0 = slot.x1 - (pitch * 2.0 + bar_w);
        let mid = slot.center().y;
        for (i, &frac) in bars.iter().enumerate() {
            let x = x0 + i as f64 * pitch;
            let h = max_h * (0.3 + 0.7 * frac);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                look.text,
                None,
                &Rect::new(x, mid - h / 2.0, x + bar_w, mid + h / 2.0),
            );
        }
    }

    /// Move up, move down and remove for a playlist or queue row.
    fn draw_reorder_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        table: &TrackTable<'_>,
        row: &TrackRow<'_>,
        slots: [Rect; 3],
    ) {
        let i = row.index;
        let len = table.rows.len();
        let (up, down, remove) = if let Some(pl) = table.playlist {
            (
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i.saturating_sub(1),
                },
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i + 1,
                },
                Action::RemoveFromPlaylist {
                    playlist: pl.to_string(),
                    track: row.track.id.clone(),
                },
            )
        } else {
            let abs = table.queue_offset.unwrap_or(0) + i;
            (
                Action::MoveQueueItem {
                    from: abs,
                    to: abs.saturating_sub(1),
                },
                Action::MoveQueueItem {
                    from: abs,
                    to: abs + 1,
                },
                Action::RemoveFromQueue(abs),
            )
        };
        if i > 0 {
            self.icon_button(
                ctx,
                ButtonFace::icon_only(slots[0], Icon::ChevronUp, "Move up")
                    .control_size(ControlSize::Small),
                false,
                up,
            );
        }
        if i + 1 < len {
            self.icon_button(
                ctx,
                ButtonFace::icon_only(slots[1], Icon::ChevronDown, "Move down")
                    .control_size(ControlSize::Small),
                false,
                down,
            );
        }
        let name = if table.playlist.is_some() {
            "Remove from playlist"
        } else {
            "Remove from queue"
        };
        self.icon_button(
            ctx,
            ButtonFace::icon_only(slots[2], Icon::X, name).control_size(ControlSize::Small),
            false,
            remove,
        );
    }

    /// Like: a plain button under the pointer; a liked song keeps a muted
    /// filled mark when the row is at rest.
    fn draw_like_action(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        slot: Rect,
        track: &str,
        row_hovered: bool,
    ) {
        let liked = self.lib.is_liked(track);
        let glyph = square_around(slot.center(), f64::from(ctx.theme.tokens.metrics.icon_sm));
        if row_hovered {
            let name = if liked { "Unlike" } else { "Like" };
            self.icon_button(
                ctx,
                ButtonFace::icon_only(slot, Icon::ThumbsUp, name).control_size(ControlSize::Small),
                false,
                Action::ToggleLike(track.to_string()),
            );
            if liked {
                Icon::ThumbsUp.draw_filled(ctx.scene, ctx.transform, glyph, look.text);
            }
        } else if liked {
            self.push_hit(slot, Action::ToggleLike(track.to_string()));
            Icon::ThumbsUp.draw_filled(ctx.scene, ctx.transform, glyph, look.dim);
        }
    }

    /// The offline state of a song and its one action: keep offline, cancel
    /// a download, remove a download or retry a failed one. At rest only a
    /// downloaded (or failed) song shows its mark, muted.
    fn draw_download_action(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        slot: Rect,
        track: &str,
        show: bool,
    ) {
        let state = self.lib.cache_state(track);
        let failed = self.offline_errors.contains_key(track);
        let downloading = matches!(state, CacheState::Downloading(_));
        let cached = matches!(state, CacheState::Cached);
        let (icon, name, keep) = if failed {
            (Icon::Refresh, "Retry download", true)
        } else if downloading {
            (Icon::X, "Cancel download", false)
        } else if cached {
            (Icon::Trash2, "Remove download", false)
        } else {
            (Icon::Download, "Keep offline", true)
        };
        let action = Action::SetOffline {
            track: track.to_string(),
            keep,
        };
        if show {
            self.icon_button(
                ctx,
                ButtonFace::icon_only(slot, icon, name).control_size(ControlSize::Small),
                false,
                action,
            );
            return;
        }
        let glyph = square_around(slot.center(), f64::from(ctx.theme.tokens.metrics.icon_sm));
        if failed {
            self.push_hit(slot, action);
            Icon::TriangleAlert.draw(ctx.scene, ctx.transform, glyph, look.danger);
        } else if cached {
            self.push_hit(slot, action);
            Icon::Check.draw(ctx.scene, ctx.transform, glyph, look.dim);
        } else if let CacheState::Downloading(progress) = state {
            self.push_hit(slot, action);
            let pct = format!("{:.0}%", progress * 100.0);
            let numeric = ctx.theme.text(TextRole::Caption).numeric();
            text_right_in_band(ctx, &pct, &numeric, look.dim, slot.x1, slot);
        }
    }

    /// A song's offline status in words, and whether it failed.
    fn download_status(&self, track: &str) -> (String, bool) {
        if let Some(error) = self.offline_errors.get(track) {
            return (format!("Download failed: {error}"), true);
        }
        match self.lib.cache_state(track) {
            CacheState::Downloading(progress) => {
                (format!("Downloading, {:.0}%", progress * 100.0), false)
            }
            CacheState::Cached => ("Available offline".into(), false),
            CacheState::Streamed => ("Not downloaded".into(), false),
        }
    }

    // ---- detail pages -----------------------------------------------------

    /// The hero of an album, artist or playlist page: the cover, the kind of
    /// page, the title, one secondary line and one caption, and the page's
    /// actions. The cover sits beside the text on a wide body and above it
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
        self.draw_titled_cover(ctx, art, header.id, header.title);

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
        let block_h = caption_h
            + 2.0 * look.tight
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
        ty += text_line(
            ctx,
            header.label,
            &caption,
            look.dim,
            Point::new(tx, ty),
            text_w,
        );
        ty += look.tight;
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

        // The page's actions: Play first (the one primary action), then
        // Shuffle, then the page's own.
        let queue = header.queue;
        let mut buttons: Vec<(String, Option<Icon>, ButtonRole, Action)> = vec![
            (
                "Play".into(),
                Some(Icon::Play),
                ButtonRole::Primary,
                Action::Play {
                    queue: queue.clone().into(),
                    index: 0,
                },
            ),
            (
                "Shuffle".into(),
                None,
                ButtonRole::Secondary,
                Action::ShufflePlay(queue.into()),
            ),
        ];
        let id = header.id.to_string();
        match header.kind {
            DetailKind::Album { saved } => buttons.push(if saved {
                (
                    "Saved".into(),
                    Some(Icon::Check),
                    ButtonRole::Secondary,
                    Action::ToggleSaveAlbum(id),
                )
            } else {
                (
                    "Save to library".into(),
                    Some(Icon::Plus),
                    ButtonRole::Secondary,
                    Action::ToggleSaveAlbum(id),
                )
            }),
            DetailKind::Artist { following } => buttons.push(if following {
                (
                    "Following".into(),
                    Some(Icon::Check),
                    ButtonRole::Secondary,
                    Action::ToggleFollow(id),
                )
            } else {
                (
                    "Follow".into(),
                    Some(Icon::Plus),
                    ButtonRole::Secondary,
                    Action::ToggleFollow(id),
                )
            }),
            DetailKind::Playlist { own: true } => {
                buttons.push((
                    "Rename".into(),
                    Some(Icon::Pencil),
                    ButtonRole::Secondary,
                    Action::BeginRenamePlaylist(id.clone()),
                ));
                buttons.push((
                    "Set cover".into(),
                    Some(Icon::Image),
                    ButtonRole::Secondary,
                    Action::PickPlaylistCover(id.clone()),
                ));
                buttons.push((
                    "Delete playlist".into(),
                    Some(Icon::Trash2),
                    ButtonRole::Destructive,
                    Action::DeletePlaylist(id),
                ));
            }
            DetailKind::Playlist { own: false } => {}
        }
        let h = ButtonFace::height(ControlSize::Regular, theme);
        let mut bx = tx;
        for (label, icon, role, action) in buttons {
            let w = Self::button_width(ctx, &label, icon, ControlSize::Regular);
            if bx > tx && bx + w > right {
                bx = tx;
                ty += h + look.related;
            }
            self.button(
                ctx,
                Rect::new(bx, ty, bx + w, ty + h),
                &label,
                icon,
                role,
                action,
            );
            bx += w + look.related;
        }
        ty += h;
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
        let label = album_label(year);
        let caption = length_caption(&rows);
        let saved = self.lib.is_album_saved(id);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Album { saved },
                label: &label,
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
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let ids = self.unique_recordings(&ids);
        let rows = self.gather_tracks(&ids);
        let label = album_label(page.year);
        let caption = length_caption(&rows);
        let saved = self.lib.is_album_saved(&page.id);
        let y = self.draw_detail_header(
            ctx,
            look,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Album { saved },
                label: &label,
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
                label: "Artist",
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
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let followed = self.lib.is_artist_followed(&page.id);
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
                label: "Artist",
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
                label: "Playlist",
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
                let row_document_y = y + self.scroll + index as f64 * row_h;
                let content_document_end = y + self.scroll + ids.len() as f64 * row_h + look.gutter;
                let max_scroll = (content_document_end - body.y0 - body.height()).max(0.0);
                let desired =
                    (row_document_y - body.y0 - body.height() * 0.42).clamp(0.0, max_scroll);
                if (self.scroll - desired).abs() > 0.5 {
                    self.scroll = desired;
                    self.dirty = true;
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
            .map(|h| format!("yt:{}", h.video_id))
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
                label: "Playlist",
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
        self.draw_track_table(ctx, look, body, y, &TrackTable::new(&rows, &ids))
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
            self.draw_cover(ctx, art, &tk.cover_id());
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
        let full = self.pb.queue.clone();
        if let Some(cur) = self.pb.current.clone() {
            y = Self::section_heading(ctx, look, body, y, "Now playing");
            let rows = self.gather_tracks(std::slice::from_ref(&cur));
            let offset = self.pb.queue_index;
            y = self.draw_track_table(
                ctx,
                look,
                body,
                y,
                &TrackTable::new(&rows, &full).in_queue(offset),
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
            .filter(|id| !matches!(self.lib.cache_state(id), CacheState::Streamed))
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
            .filter(|id| matches!(self.lib.cache_state(id), CacheState::Cached))
            .count();
        let active = self
            .lib
            .tracks
            .keys()
            .filter(|id| matches!(self.lib.cache_state(id), CacheState::Downloading(_)))
            .count()
            .max(self.downloads.len() + self.offline_resolves.len());
        let status = match (cached, active) {
            (0, 0) => "Nothing is available offline yet.".to_string(),
            (0, active) => format!("Downloading {}", songs_label(active)),
            (cached, 0) => format!("{} available offline", songs_label(cached)),
            (cached, active) => format!(
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
                icon: Some(Icon::ThumbsUp),
                message: Some("Like a song and it appears here."),
                ..EmptyStateContent::new("No liked songs yet")
            };
            return self.empty_page(ctx, body, y, &content, None);
        }
        let rows = self.gather_tracks(&ids);
        let summary = length_caption(&rows);
        let mut y = Self::note(ctx, look, body, y, &summary) + look.section;
        let h = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let mut x = body.x0 + look.gutter;
        for (label, icon, role, action) in [
            (
                "Play",
                Some(Icon::Play),
                ButtonRole::Primary,
                Action::Play {
                    queue: ids.clone().into(),
                    index: 0,
                },
            ),
            (
                "Shuffle",
                None,
                ButtonRole::Secondary,
                Action::ShufflePlay(ids.clone().into()),
            ),
        ] {
            let w = Self::button_width(ctx, label, icon, ControlSize::Regular);
            self.button(
                ctx,
                Rect::new(x, y, x + w, y + h),
                label,
                icon,
                role,
                action,
            );
            x += w + look.related;
        }
        y += h + look.gutter;
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
        let Some(tk) = cur else {
            let content = EmptyStateContent {
                icon: Some(Icon::Mic),
                ..EmptyStateContent::new("Play a song to see its lyrics.")
            };
            return self.empty_page(ctx, body, body.y0, &content, None);
        };

        let band_top = body.y0 + look.gutter;
        let band_bot = body.y1 - look.gutter;
        let art_max = MediaSize::Regular.extent(ctx.theme);
        let layout = lyrics_layout(body, band_top, band_bot, art_max, look.gutter);
        let lyrics_top = match layout.art {
            Some(art) => {
                self.draw_lyrics_now_playing(ctx, look, art, &tk);
                band_top
            }
            None => Self::draw_lyrics_title(ctx, look, layout.lyrics, band_top, &tk) + look.gutter,
        };
        let band = Rect::new(layout.lyrics.x0, lyrics_top, layout.lyrics.x1, band_bot);
        if self.lyrics_synced && !self.lyrics.is_empty() {
            let label = "Synced";
            let h = ui::widget::BadgeFace::height(ctx.theme);
            let w = ui::widget::BadgeFace::intrinsic_width(label, ctx.text, ctx.theme);
            let at = Rect::new(
                band.x1 - w,
                body.y0 + look.section,
                band.x1,
                body.y0 + look.section + h,
            );
            ui::widget::BadgeFace::new(at, label)
                .style(ui::widget::ChipStyle::Neutral)
                .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        }
        if self.lyrics.is_empty() {
            let msg = if self.lyrics_pending_for.is_some() {
                "Finding lyrics…"
            } else {
                "No lyrics found for this song."
            };
            let style = ctx.theme.text(TextRole::Body);
            let (w, _) = ctx.text.measure_style(msg, None, &style);
            ctx.text(
                msg,
                &style,
                look.dim,
                Point::new(band.center().x - f64::from(w) / 2.0, band.center().y),
                None,
            );
            return band_bot;
        }
        self.draw_lyrics_lines(ctx, look, band);
        band_bot
    }

    /// On a wide body the cover and the song's title fill the left column.
    fn draw_lyrics_now_playing(
        &self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        art: Rect,
        tk: &TrackDisp,
    ) {
        let cover = self.best_cover_id(&tk.id, &tk.album_id);
        self.draw_titled_cover(ctx, art, &cover, &tk.album);
        let title = ctx.theme.text(TextRole::Title);
        let body = ctx.theme.text(TextRole::Body);
        let y = art.y1 + look.section;
        let h = text_line(
            ctx,
            &tk.title,
            &title,
            look.text,
            Point::new(art.x0, y),
            art.width(),
        );
        text_line(
            ctx,
            &tk.artist,
            &body,
            look.dim,
            Point::new(art.x0, y + h),
            art.width(),
        );
    }

    /// A narrow body names the song above the lyrics. Returns the y below.
    fn draw_lyrics_title(
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        column: Rect,
        y: f64,
        tk: &TrackDisp,
    ) -> f64 {
        let title = ctx.theme.text(TextRole::Title);
        let body = ctx.theme.text(TextRole::Body);
        let w = column.width();
        let h = text_line(
            ctx,
            &tk.title,
            &title,
            look.text,
            Point::new(column.x0, y),
            w,
        );
        y + h
            + text_line(
                ctx,
                &tk.artist,
                &body,
                look.dim,
                Point::new(column.x0, y + h),
                w,
            )
    }

    /// The lyric lines in `band`, following the playhead: the current line
    /// in the title role, its neighbours quieter with distance.
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
        let line_count = self.lyrics.len();
        let band_h = band.height().max(1.0);
        // Auto-scroll so the active line sits ~42% down the band and moves in
        // time with playback; plain (unsynced) lyrics anchor to the top.
        let focus = current.unwrap_or(0);
        let auto = if self.lyrics_synced && current.is_some() {
            let want = focus as f64 * line_h - band_h * 0.42;
            want.clamp(0.0, ((line_count as f64 * line_h) - band_h).max(0.0))
        } else {
            0.0
        };
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
            let (w, h) = ctx.text.measure_style(&shown, None, st);
            let x = band.x0 + (band.width() - f64::from(w)) / 2.0;
            let y = ly + (line_h - f64::from(h)) / 2.0;
            ctx.text(&shown, st, ink, Point::new(x, y), None);
        }
        ctx.scene.pop_layer();
    }

    // ---- browse -----------------------------------------------------------

    /// Browse: the library's genres as cards. A card opens the platform
    /// search for Music with the genre filled in.
    fn draw_browse(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64) -> f64 {
        let theme = ctx.theme;
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
        let rhythm = &theme.tokens.spacing.rhythm;
        let gap = f64::from(rhythm.section);
        let avail = (body.width() - 2.0 * look.gutter).max(0.0);
        let cols = ui::widget::card_columns(avail, MediaSize::Regular.extent(theme), gap);
        let w = ui::widget::card_column_width(avail, cols, gap);
        let caption = theme.text(TextRole::Caption);
        let caption_h = line_of(ctx, &caption);
        for (i, (genre, artists)) in genres.iter().enumerate() {
            let x = body.x0 + look.gutter + (i % cols) as f64 * (w + gap);
            let face = CardFace::new().title(genre).pressable();
            let h = face.height_for_body(ctx.text, theme, w, caption_h);
            let top = y + (i / cols) as f64 * (h + gap);
            let rect = Rect::new(x, top, x + w, top + h);
            let hovered = self.body_hit(rect, Action::BrowseGenre(genre.clone()));
            let face = face.interaction(Interaction {
                hovered,
                ..Interaction::default()
            });
            face.paint(ctx, rect);
            let inner = face.body_rect(ctx.text, theme, rect);
            let label = if *artists == 1 {
                "1 artist".to_string()
            } else {
                format!("{artists} artists")
            };
            text_line(
                ctx,
                &label,
                &caption,
                look.dim,
                Point::new(inner.x0, inner.y0),
                inner.width(),
            );
            if i + 1 == genres.len() {
                y = rect.y1;
            }
        }
        y
    }

    // ---- now playing bar --------------------------------------------------

    /// The transport along the bottom of the content column: the song and its
    /// like and lyrics actions, the playback controls over the scrubber, and
    /// the queue and volume. It is part of the content, set apart by one
    /// hairline. While the sidebar floats (a narrow window) it folds to the
    /// song, previous/play/next and the scrubber.
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
        self.draw_cover(ctx, art, &cover);
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
            ButtonFace::icon_only(like, Icon::ThumbsUp, if liked { "Unlike" } else { "Like" })
                .control_size(ControlSize::Regular),
            false,
            Action::ToggleLike(tk.id.clone()),
        );
        if liked {
            let side = f64::from(ctx.theme.tokens.metrics.icon_md);
            Icon::ThumbsUp.draw_filled(
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
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let row_w = 5.0 * control + 4.0 * look.related;
        let mut x = r.center().x - row_w / 2.0;
        let y0 = r.y0;
        let mut next_slot = || {
            let rect = Rect::new(x, y0, x + control, y0 + control);
            x += control + look.related;
            rect
        };
        let (shuffle, prev, play, next, repeat) = (
            next_slot(),
            next_slot(),
            next_slot(),
            next_slot(),
            next_slot(),
        );
        self.glyph_button(
            ctx,
            shuffle,
            Glyph::Shuffle,
            "Shuffle",
            self.pb.shuffle,
            Action::ToggleShuffle,
        );
        self.icon_button(
            ctx,
            ButtonFace::icon_only(prev, Icon::SkipBack, "Previous track")
                .control_size(ControlSize::Regular),
            false,
            Action::Prev,
        );
        let (icon, name) = if self.pb.playing {
            (Icon::Pause, "Pause")
        } else {
            (Icon::Play, "Play")
        };
        let face = ButtonFace::icon_only(play, icon, name).role(ButtonRole::Primary);
        let hovered = self.push_hit(face.hit_rect(ctx.theme), Action::TogglePlay);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &face.hovered(hovered),
        );
        self.icon_button(
            ctx,
            ButtonFace::icon_only(next, Icon::SkipForward, "Next track")
                .control_size(ControlSize::Regular),
            false,
            Action::Next,
        );
        let (glyph, name) = match self.pb.repeat {
            RepeatMode::One => (Glyph::RepeatOne, "Repeat one"),
            RepeatMode::All => (Glyph::Repeat, "Repeat all"),
            RepeatMode::Off => (Glyph::Repeat, "Repeat"),
        };
        self.glyph_button(
            ctx,
            repeat,
            glyph,
            name,
            self.pb.repeat != RepeatMode::Off,
            Action::CycleRepeat,
        );

        let scrub = Rect::new(r.x0, y0 + control + look.tight, r.x1, r.y1);
        self.draw_scrubber(ctx, look, scrub);
    }

    /// Elapsed time, the draggable position and the total time, in `r`.
    fn draw_scrubber(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
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
        let track = face.track_rect();
        self.scrubber_rect = Rect::new(track.x0, slider.y0, track.x1, slider.y1);
        let active = self.scrubber_rect.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        let face = face
            .hovered(active)
            .dragging(self.drag == Some(Drag::Scrubber));
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
            ButtonFace::icon_only(queue, Icon::ListVideo, "Queue")
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
        let track = face.track_rect();
        self.volume_rect = Rect::new(track.x0, hit.y0, track.x1, hit.y1);
        let active = self.volume_rect.contains(self.mouse) || self.drag == Some(Drag::Volume);
        let face = face
            .hovered(active)
            .dragging(self.drag == Some(Drag::Volume));
        Slider::paint(ctx.scene, ctx.transform, ctx.theme, &face);
    }

    /// The collapsed-volume popover: a floating panel with the mute toggle,
    /// a slider and the level, anchored above the volume button.
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
        let x0 = (btn.center().x - pw / 2.0)
            .max(self.rect.x0 + look.related)
            .min(self.rect.x1 - pw - look.related);
        let y1 = btn.y0 - look.related;
        let panel = Rect::new(x0, y1 - ph, x0 + pw, y1);
        self.volume_popover_rect = panel;
        let radius = f64::from(theme.tokens.radii.panel);
        surface::shadow(
            ctx.scene,
            ctx.transform,
            panel,
            radius,
            &theme.tokens.elevation.floating,
        );
        surface::panel(ctx.scene, ctx.transform, panel, radius, theme);
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

    /// The narrow transport: the song beside previous/play/next, over the
    /// scrubber. Volume and the queue are reachable from the sidebar.
    fn draw_nowplaying_compact(&mut self, ctx: &mut PaintCtx<'_>, look: &Look, r: Rect) {
        self.volume_rect = Rect::ZERO;
        self.volume_btn_rect = Rect::ZERO;
        let control = ButtonFace::height(ControlSize::Regular, ctx.theme);
        let caption = ctx.theme.text(TextRole::Caption);
        let scrub_h = line_of(ctx, &caption).max(look.section + look.related);
        let top = Rect::new(r.x0, r.y0, r.x1, r.y1 - scrub_h - look.tight);
        let controls_w = 3.0 * control + 2.0 * look.related;
        let mid = top.center().y;
        let mut x = top.x1 - controls_w;
        let mut next_slot = || {
            let rect = Rect::new(x, mid - control / 2.0, x + control, mid + control / 2.0);
            x += control + look.related;
            rect
        };
        let (prev, play, next) = (next_slot(), next_slot(), next_slot());

        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        let body = ctx.theme.text(TextRole::Body);
        let text_x1 = prev.x0 - look.sibling;
        if let Some(tk) = cur {
            let art = Rect::new(top.x0, top.y0, top.x0 + top.height(), top.y1);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(ctx, art, &cover);
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

        self.icon_button(
            ctx,
            ButtonFace::icon_only(prev, Icon::SkipBack, "Previous track")
                .control_size(ControlSize::Regular),
            false,
            Action::Prev,
        );
        let (icon, name) = if self.pb.playing {
            (Icon::Pause, "Pause")
        } else {
            (Icon::Play, "Play")
        };
        let face = ButtonFace::icon_only(play, icon, name).role(ButtonRole::Primary);
        let hovered = self.push_hit(face.hit_rect(ctx.theme), Action::TogglePlay);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &face.hovered(hovered),
        );
        self.icon_button(
            ctx,
            ButtonFace::icon_only(next, Icon::SkipForward, "Next track")
                .control_size(ControlSize::Regular),
            false,
            Action::Next,
        );
        let scrub = Rect::new(r.x0, r.y1 - scrub_h, r.x1, r.y1);
        self.draw_scrubber(ctx, look, scrub);
    }

    // ---- toast ------------------------------------------------------------

    fn draw_toast(&self, ctx: &mut PaintCtx<'_>) {
        let Some((msg, _)) = self.toast.as_ref() else {
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
            &ToastCard::new(msg, ToastKind::Info),
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

    /// Register a click target that only counts when visible inside the body
    /// viewport (so scrolled-away rows don't catch clicks under the chrome).
    fn body_hit(&mut self, rect: Rect, action: Action) -> bool {
        let cy = f64::midpoint(rect.y0, rect.y1);
        if cy < self.body_rect.y0 || cy > self.body_rect.y1 {
            return false;
        }
        self.push_hit(rect, action)
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

/// The overline of an album page: "Album", or "Album from 2023".
fn album_label(year: u32) -> String {
    if year > 0 {
        format!("Album from {year}")
    } else {
        "Album".to_string()
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
