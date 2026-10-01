//! Drawing + event handling for [`MusicApp`].
//!
//! Split out of `app.rs` to keep the data/state logic separate from the
//! (large) rendering surface. Everything here draws straight into the Vello
//! scene and registers click `Hit`s for the event handler.

use super::{
    Action, Affine, AlbumPage, Arc, ArtistPage, AutoCommit, CacheState, Circle, Color, Dimension,
    Drag, Fill, HEADER_H, HashMap, HomeItem, HomeItemKind, HomeSection, Icon, Instant, MusicApp,
    NOWPLAYING_H, NodeId, ObjId, PermissionContext, PlaylistPage, Point, PresenceCtx, Rect,
    RepeatMode, SIDEBAR_BREAKPOINT, Scene, SearchTab, Size, Stroke, Style, TaffyTree, TextRenderer,
    TextWeight, Theme, View, Widget, art, elide, icon, is_live_album_id, is_live_id,
    is_live_playlist_id, lyrics, rrect,
};
use ui::widget::{AppModel, ModelPath};

use ui::paint::PaintCtx;
use ui::surface;
use ui::text_render::{NumericStyle, TextStyle};
use ui::tokens::Spacing;
use ui::widget::Continuable;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
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
    listeners: u64,
}

struct PlaylistDisp {
    id: String,
    name: String,
    count: usize,
}

/// A playlist row in the search results (cover + owner subtitle, navigable).
#[derive(Clone)]
struct ResultPlaylist {
    id: String,
    title: String,
    subtitle: String,
}

/// The "top result" hero shown at the head of the search `All` tab.
struct SearchHero {
    id: String,
    title: String,
    subtitle: String,
    /// "Song" / "Album" / "Artist" / "Playlist".
    kind_label: &'static str,
    /// Artists render as a circular avatar; everything else as a square cover.
    circular: bool,
    /// Whether the primary affordance is Play (a track) vs. Open (a page).
    playable: bool,
    /// What a click on the hero does (play the track, or open its page).
    action: Action,
}

/// The unified search-result model the `All`/per-type tabs render, built from
/// the live multi-type bundle when present, else the local sample library.
#[derive(Default)]
struct SearchResultsView {
    songs: Vec<String>,
    albums: Vec<AlbumDisp>,
    artists: Vec<ArtistDisp>,
    playlists: Vec<ResultPlaylist>,
    top: Option<SearchHero>,
    /// True when these came from the live YouTube-Music backend.
    live: bool,
}

impl SearchResultsView {
    fn is_empty(&self) -> bool {
        self.songs.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
            && self.playlists.is_empty()
    }
}

/// Minimum horizontal travel (logical px) for a now-playing-bar drag to count
/// as a prev/next swipe rather than a tap.
const BAR_SWIPE_PX: f64 = 56.0;

/// The content pane's horizontal gutter: section labels, chips, grids and the
/// track table all inset by this, and so do the header title and search field
/// (the header used a 24 px lead, leaving every page title 4 px left of the
/// content it named). Deliberately off the 4 px spacing scale — it is the
/// pane's authored gutter (the body's local `pad = 28.0` values name the same
/// distance) and is recorded as such in the 2026-09-16 alignment review.
const CONTENT_GUTTER: f64 = 28.0;
/// A `Spacing::BASE` step as the `f64` this painter's geometry uses.
#[allow(
    clippy::cast_lossless,
    reason = "const context: `f64::from` is not const; f32 -> f64 is exact"
)]
const fn step(px: f32) -> f64 {
    px as f64
}
/// The gap between two list rows' surfaces (HIG §7 "nothing touches"): a
/// row's hover / selection fill stops this far short of its pitch, so two
/// neighbouring rows never share an edge. The pitch itself is unchanged.
const ROW_GAP: f64 = step(Spacing::BASE.s1);
/// Inset of a cover tile inside a chip, so the chip's derived edge stays
/// closed around the art instead of being painted over by it.
const CHIP_ART_INSET: f64 = step(Spacing::BASE.s1);
/// Siblings of equal weight (tab pills, management pills): HIG §7b `s3`.
const SIBLING_GAP: f64 = step(Spacing::BASE.s3);
/// A label's clearance from the glyph beside it: HIG §7b `s2`.
const LABEL_GLYPH_GAP: f64 = step(Spacing::BASE.s2);
/// The large-text (≥ 135 %) frame's bands, authored for 2× text: a 96 px
/// header, 88 px rows (title + artist), a 116 px player (art beside a
/// title + byline block, play button on the right). Each is a floor — the
/// band grows when its measured lines plus padding need more — and every
/// block inside is centred on the band's midline from measured heights.
const LARGE_HEADER_H: f64 = 96.0;
const LARGE_ROW_H: f64 = 88.0;
const LARGE_PLAYER_H: f64 = 116.0;
/// Room above and below a band's text block before the band has to grow.
const LARGE_BAND_PAD: f64 = 12.0;
const LARGE_PLAYER_PAD: f64 = 18.0;
/// Between a row's title and artist lines / the player's title and byline.
const LARGE_ROW_LINE_GAP: f64 = 4.0;
const LARGE_PLAYER_LINE_GAP: f64 = 2.0;
/// The player's art is a square as tall as its text block (68 px at 2×), on
/// the content gutter; the play button is a 60 px circle on the right gutter.
const LARGE_PLAYER_ART_MIN: f64 = 68.0;
const LARGE_PLAY_DIAMETER: f64 = 60.0;
const LARGE_PLAYER_TEXT_GAP: f64 = 18.0;

/// The large-text frame's three bands, measured from the lines they hold.
/// Every band keeps its authored height as a floor (header 96, rows 88,
/// player 116 — the values 2× text happens to fit exactly) and grows when
/// its measured block plus padding needs more, so 135–400% text is contained
/// rather than assumed; every block inside is then centred on its band.
#[derive(Clone, Copy, Debug, PartialEq)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a height; the `_h` suffix keeps them distinct from the widths and offsets they are combined with"
)]
pub(super) struct LargeTextBands {
    pub(super) title_h: f64,
    pub(super) header_h: f64,
    pub(super) row_title_h: f64,
    pub(super) row_block_h: f64,
    pub(super) row_h: f64,
    pub(super) player_label_h: f64,
    pub(super) player_block_h: f64,
    pub(super) player_h: f64,
}

impl LargeTextBands {
    pub(super) fn measure(tr: &mut TextRenderer, title: &str) -> Self {
        let line = |tr: &mut TextRenderer, size: f32, weight: TextWeight| {
            f64::from(tr.measure_weighted("Ag", size, None, weight).1)
        };
        let title_h = f64::from(tr.measure_weighted(title, 18.0, None, TextWeight::Bold).1);
        let row_title_h = line(tr, 12.0, TextWeight::Semibold);
        let row_block_h = row_title_h + LARGE_ROW_LINE_GAP + line(tr, 10.0, TextWeight::Regular);
        let player_label_h = line(tr, 11.0, TextWeight::Semibold);
        let player_block_h =
            player_label_h + LARGE_PLAYER_LINE_GAP + line(tr, 9.0, TextWeight::Regular);
        Self {
            title_h,
            header_h: LARGE_HEADER_H.max(title_h + 2.0 * LARGE_BAND_PAD),
            row_title_h,
            row_block_h,
            row_h: LARGE_ROW_H.max(row_block_h + 2.0 * LARGE_BAND_PAD),
            player_label_h,
            player_block_h,
            player_h: LARGE_PLAYER_H.max(player_block_h + 2.0 * LARGE_PLAYER_PAD),
        }
    }
}

#[cfg(test)]
mod large_text_tests {
    use super::*;

    /// The ≥ 135 % frame used to place its header title, row lines and
    /// player block with literals that fit 2× text exactly. Each band now
    /// keeps that authored size at 2× and grows with the measured lines
    /// beyond it — so a 3× title, row or player never leaves its band.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the geometry pins authored whole-pixel constants, which are exact in binary floating point"
    )]
    fn large_text_bands_keep_their_authored_size_at_2x_and_grow_beyond() {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let two = LargeTextBands::measure(&mut tr, "Home");
        assert_eq!(two.header_h, LARGE_HEADER_H, "2× header keeps 96");
        assert_eq!(two.row_h, LARGE_ROW_H, "2× rows keep 88");
        assert_eq!(two.player_h, LARGE_PLAYER_H, "2× player keeps 116");
        assert!(
            two.row_block_h + 2.0 * LARGE_BAND_PAD <= two.row_h
                && two.player_block_h + 2.0 * LARGE_PLAYER_PAD <= two.player_h,
            "2× blocks fit their bands with padding: {two:?}"
        );

        // 4× (the renderer's ceiling): every block outgrows its authored
        // band, so every band must have grown to contain it.
        tr.set_text_scale(4.0);
        let four = LargeTextBands::measure(&mut tr, "Home");
        assert!(
            four.header_h > two.header_h && four.row_h > two.row_h && four.player_h > two.player_h,
            "4× bands grow past the authored sizes: {four:?}"
        );
        assert!(
            four.header_h >= four.title_h + 2.0 * LARGE_BAND_PAD
                && four.row_h >= four.row_block_h + 2.0 * LARGE_BAND_PAD
                && four.player_h >= four.player_block_h + 2.0 * LARGE_PLAYER_PAD,
            "4× bands contain their measured blocks: {four:?}"
        );
    }
}

/// Width of the track table's duration slot; `m:ss` values right-align to
/// `cols.dur + DURATION_SLOT_W`, and so does the column heading.
const DURATION_SLOT_W: f64 = 48.0;

/// Style for a duration or timecode: the same size/weight the callers draw
/// with, plus tabular lining figures so columns of `m:ss` values line up
/// digit for digit (HIG §7 "digits that line up use tabular figures").
fn numeric_style(size: f32, weight: TextWeight) -> TextStyle {
    TextStyle::new(size)
        .with_weight(weight)
        .with_numeric_style(NumericStyle::TabularLining)
}

/// Draw a duration right-aligned in `slot` and vertically centred in it on
/// its measured line height, with tabular figures. Shared by the track table
/// and the queue so both columns share one metric.
fn draw_duration_right_aligned(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    size: f32,
    color: Color,
    slot: Rect,
) {
    let style = numeric_style(size, TextWeight::Regular);
    let (w, h) = ctx.text.measure_style(text, None, &style);
    let origin = Point::new(
        slot.x1 - f64::from(w),
        slot.y0 + (slot.height() - f64::from(h)) / 2.0,
    );
    ctx.text(text, &style, color, origin, None);
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

fn accessible_result_playlist(
    playlist: &crate::backend::PlaylistHit,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("result-playlist:{}", playlist.id))
        .with_name(&playlist.title)
        .with_description(format!(
            "{}; {} songs",
            playlist.subtitle, playlist.track_count
        ))
        .with_position(position, total)
        .with_action_target(format!("music.open-playlist.{}", playlist.id))
}

// Vertical-center a single text line of `size` within a row of height `h`.
fn vy(top: f64, h: f64, size: f32) -> f64 {
    top + (h - f64::from(size) * 1.25) / 2.0
}

/// Column x-positions for a single track row within its usable `[left, right]`
/// span. The duration / download / like controls form a cluster pinned to the
/// right; the album sits in a *centred* column, clamped so it neither crowds
/// the title nor collides with that cluster. This keeps every row reading as
/// three balanced zones — title · album · controls — instead of the title
/// hugging the left with a cavernous gap across to everything bunched right.
pub(super) struct TrackColumns {
    pub text: f64,
    pub album: f64,
    pub like: f64,
    pub dl: f64,
    pub dur: f64,
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
        }
    }

    /// Hide the album column (an album's own page).
    fn without_album(self) -> Self {
        Self {
            show_album: false,
            ..self
        }
    }

    /// Leave the leading index column blank.
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
        icon: Icon::Search,
        label: "Search",
        view: View::Search,
    },
    NavDestination {
        icon: Icon::Songs,
        label: "Songs",
        view: View::Songs,
    },
    NavDestination {
        icon: Icon::Albums,
        label: "Albums",
        view: View::Albums,
    },
    NavDestination {
        icon: Icon::Artists,
        label: "Artists",
        view: View::Artists,
    },
    NavDestination {
        icon: Icon::HeartFilled,
        label: "Liked Songs",
        view: View::Liked,
    },
    NavDestination {
        icon: Icon::Download,
        label: "Downloads",
        view: View::Downloads,
    },
    NavDestination {
        icon: Icon::Queue,
        label: "Queue",
        view: View::Queue,
    },
];

/// Which kind of entity a detail page shows, with the state of the trailing
/// toggle its header offers: "Add to Library" for an album, "Follow" for an
/// artist, none for a playlist. Artists show a circular avatar instead of a
/// square cover.
#[derive(Clone, Copy)]
enum DetailKind {
    Album { saved: bool },
    Artist { following: bool },
    Playlist,
}

impl DetailKind {
    fn label(self) -> &'static str {
        match self {
            Self::Album { .. } => "ALBUM",
            Self::Artist { .. } => "ARTIST",
            Self::Playlist => "PLAYLIST",
        }
    }
}

/// What a detail page's header shows: the entity `id` (which also seeds its
/// art and keys its toggle), its kind, title and subtitle, and the queue its
/// Play and Shuffle buttons start.
struct DetailHeader<'a> {
    id: &'a str,
    kind: DetailKind,
    title: &'a str,
    subtitle: &'a str,
    queue: Vec<String>,
}

/// A run of queue rows: the tracks shown, the whole queue they index into,
/// and the queue index of the first track shown.
struct QueueSlice<'a> {
    rows: &'a [TrackDisp],
    queue: &'a [String],
    offset: usize,
}

/// Where a queue row sits for its reorder controls: its queue index, the
/// first index the list it is shown in starts at, and the queue length.
#[derive(Clone, Copy)]
struct QueuePosition {
    index: usize,
    first: usize,
    len: usize,
}

/// A [`TrackTable`] laid out in a body: the row span, pitch and columns every
/// row of the table shares.
struct TrackTableLayout<'a> {
    table: &'a TrackTable<'a>,
    left: f64,
    right: f64,
    row_h: f64,
    cols: TrackColumns,
}

/// One visible row of a [`TrackTable`]: its track and index in the table, its
/// top edge, and whether it is the current track or under the pointer.
struct TrackRow<'a> {
    track: &'a TrackDisp,
    index: usize,
    y: f64,
    is_current: bool,
    hovered: bool,
}

pub(super) fn track_row_columns(left: f64, right: f64, number_col: bool) -> TrackColumns {
    let dur = right - 56.0;
    let dl = right - 96.0;
    let like = dl - 34.0;
    let text = if number_col { left + 38.0 } else { left + 8.0 };
    // Centre the album column, but keep it clear of the title (a minimum title
    // width) and of the right-hand control cluster. When the row is too narrow
    // to honour both bounds they cross, so fall back to sitting just left of
    // the cluster (avoids a panicking `clamp`, degrades to a sensible column).
    // Bias the metadata column a little left of centre.  On a wide desktop a
    // mathematical midpoint made the title and album feel disconnected, with a
    // conspicuous empty channel between them.  This keeps a generous readable
    // title column while grouping the two pieces of song identity together.
    let target = left + (right - left) * 0.40;
    let lo = text + 200.0;
    let hi = like - 176.0;
    let album = if hi >= lo {
        target.clamp(lo, hi)
    } else {
        hi.max(text + 60.0)
    };
    TrackColumns {
        text,
        album,
        like,
        dl,
        dur,
    }
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

#[allow(
    clippy::similar_names,
    reason = "art_cx/art_cy are the x/y halves of one centre point; the shared axis-letter \
              suffix is the point, not an accident"
)]
pub(super) fn lyrics_layout(body: Rect, band_top: f64, band_bottom: f64) -> LyricsLayout {
    let pad = 28.0;
    if body.width() >= 760.0 {
        let col_w = (body.width() * 0.36).clamp(280.0, 420.0);
        let card_x0 = body.x0 + pad;
        let card_x1 = card_x0 + col_w;
        let art_side = (col_w - 48.0).clamp(180.0, 300.0);
        let art_cx = f64::midpoint(card_x0, card_x1);
        let art_cy = f64::midpoint(band_top, band_bottom) - 30.0;
        let art = Rect::new(
            art_cx - art_side / 2.0,
            art_cy - art_side / 2.0,
            art_cx + art_side / 2.0,
            art_cy + art_side / 2.0,
        );
        LyricsLayout {
            art: Some(art),
            lyrics: Rect::new(card_x1 + 24.0, body.y0, body.x1 - pad, body.y1),
        }
    } else {
        LyricsLayout {
            art: None,
            lyrics: Rect::new(body.x0, body.y0, body.x1, body.y1),
        }
    }
}

/// A monotonic wall-clock phase in `[0,1)` for shimmer/pulse animations
/// (~1.3s period). Process-relative so every skeleton sweeps in lockstep.
fn anim_phase() -> f64 {
    (anim_time() / 1.3).fract()
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

/// Shared responsive column policy for artwork grids. Compact windows retain
/// two useful tiles while roomy desktop canvases gain density without letting
/// cards become billboard-sized.
fn artwork_grid_columns(available: f64, target: f64, gap: f64) -> usize {
    if (300.0..600.0).contains(&available) {
        2
    } else {
        ((available + gap) / (target + gap)).floor().max(1.0) as usize
    }
}

/// A shimmering skeleton placeholder block: a rounded base with a soft
/// highlight band sweeping left→right, clipped to the block. This is the
/// loading affordance that replaces "fake title" text while real content
/// streams in.
fn draw_skeleton_block(ctx: &mut PaintCtx<'_>, r: Rect, radius: f64, c: &Palette) {
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    // High Contrast intentionally uses black for both base and raised
    // surfaces. A normal card-coloured skeleton would therefore disappear
    // completely, leaving no loading affordance. `skeleton` is a visible,
    // non-text decorative tier on that accessibility track.
    ctx.fill_round(r, radius, c.skeleton);
    // Motion-sensitive users get the same clearly-visible loading geometry
    // without any travelling bands or striped frozen frame.
    if c.reduce_motion {
        return;
    }
    // Moving highlight sweep, clipped to the block's rounded rect.
    ctx.scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        ctx.transform,
        &rrect(r, radius),
    );
    let phase = anim_phase();
    let band_w = (r.width() * 0.45).max(24.0);
    let travel = r.width() + band_w;
    let x = r.x0 - band_w + travel * phase;
    // Two overlapping bands give a soft-edged sweep without a gradient brush.
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        c.white5,
        None,
        &Rect::new(x, r.y0, x + band_w, r.y1),
    );
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        c.white10.multiply_alpha(0.7),
        None,
        &Rect::new(x + band_w * 0.28, r.y0, x + band_w * 0.72, r.y1),
    );
    ctx.scene.pop_layer();
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
        dyn ui::search::capability::SearchSource
    );

    fn take_structure_dirty(&mut self) -> bool {
        self.open_file_layout_visible != Some(matches!(self.view, View::Home))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if matches!(self.view, View::Home) {
            vec![&self.open_file_button]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if matches!(self.view, View::Home) {
            vec![&mut self.open_file_button]
        } else {
            Vec::new()
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
        let visible = matches!(self.view, View::Home);
        self.open_file_layout_visible = Some(visible);
        let children = if visible {
            vec![self.open_file_button.build_layout(taffy, theme, node_map)]
        } else {
            Vec::new()
        };
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

        if matches!(self.view, View::Home)
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(button_node) = children.first()
        {
            self.open_file_button
                .handle_event(event, &mut ctx.reborrow(ctx.path, *button_node));
        }
        if *ctx.handled {
            self.dirty = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.on_cursor_moved(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.handled,
                );
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
                self.on_left_press(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.handled,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.handled,
                );
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
                self.on_key_input(
                    logical_key,
                    text.as_deref(),
                    event,
                    ctx.scale_factor,
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
                    | (View::Search, "search")
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

        let search = AccessNode::new(Role::TextInput)
            .with_hashed_key("search")
            .with_name("Search songs, albums, artists, and playlists")
            .with_description("Shortcut: slash or Control F")
            .with_value(self.search_input.value())
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(Widget::is_focused(&self.search_input)),
            )
            .with_action_target("music.search");

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
                            .with_name("Search for music")
                            .with_description("Search for a song, album, or artist")
                            .with_action_target("music.empty-search"),
                    ]),
            );
        }
        if let Some((query, error)) = &self.search_error {
            page_content.push(
                AccessNode::new(Role::Group)
                    .with_name("Live search unavailable")
                    .with_description(format!("Could not search for {query}: {error}"))
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Try live search again")
                            .with_action_target("music.retry-search"),
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
            View::Search => "Search results".to_string(),
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
            View::Search => {
                let results = self.lib.search(self.search_input.value());
                let live_playlists = self
                    .live_bundle
                    .as_ref()
                    .filter(|(query, _)| query == self.search_input.value())
                    .map_or(&[][..], |(_, bundle)| bundle.playlists.as_slice());
                let total = results.tracks.len()
                    + results.albums.len()
                    + results.artists.len()
                    + live_playlists.len();
                list_items.extend(
                    results
                        .tracks
                        .iter()
                        .filter_map(|id| self.lib.track(id))
                        .take(ACCESSIBLE_PAGE_LIMIT)
                        .enumerate()
                        .map(|(index, track)| accessible_track(&self.lib, track, index + 1, total)),
                );
                let used = list_items.len();
                list_items.extend(
                    results
                        .albums
                        .iter()
                        .filter_map(|id| self.lib.album(id))
                        .take(ACCESSIBLE_PAGE_LIMIT.saturating_sub(used))
                        .enumerate()
                        .map(|(index, album)| {
                            accessible_album(&self.lib, album, used + index + 1, total)
                        }),
                );
                let used = list_items.len();
                list_items.extend(
                    live_playlists
                        .iter()
                        .take(ACCESSIBLE_PAGE_LIMIT.saturating_sub(used))
                        .enumerate()
                        .map(|(index, playlist)| {
                            accessible_result_playlist(playlist, used + index + 1, total)
                        }),
                );
                let used = list_items.len();
                list_items.extend(
                    results
                        .artists
                        .iter()
                        .filter_map(|id| self.lib.artist(id))
                        .take(ACCESSIBLE_PAGE_LIMIT.saturating_sub(used))
                        .enumerate()
                        .map(|(index, artist)| accessible_artist(artist, used + index + 1, total)),
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
        if target == "music.search" {
            match action {
                ui::a11y::Action::Focus | ui::a11y::Action::Activate => {
                    self.dispatch(Action::FocusSearch, state);
                    return true;
                }
                ui::a11y::Action::SetValue(value) => {
                    let value = value.chars().take(512).collect::<String>();
                    self.on_focus_search();
                    self.search_input.set_text(&value);
                    self.search_cache = None;
                    self.search_edit_at = Some(ui::time::now());
                    self.scroll = 0.0;
                    self.dirty = true;
                    return true;
                }
                _ => return false,
            }
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
            "music.empty-search" => Action::Nav(View::Search),
            "music.retry-search" => Action::RetrySearch,
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
    /// Fold the user's library into the unified Ctrl+K palette. The app retains
    /// one prebuilt [`LibrarySearchProvider`](crate::search_type::LibrarySearchProvider)
    /// per library revision, and this returns an O(1) shared-catalog clone;
    /// opening Ctrl+K therefore never reindexes the corpus. The provider is
    /// synchronous and offline-safe; the live YouTube-Music search is async and
    /// lives in the app's own search view.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.library_search_provider.clone())]
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
                let track_label = self.track_label(&track);
                self.on_nav(View::Playlist(playlist.clone()));
                self.revealed_membership = Some((playlist, track));
                self.toast = Some((
                    format!("Opened “{track_label}” in “{playlist_name}”"),
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
                Action::ToggleLike(_) => Some("Like song"),
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
                Action::FocusSearch if self.search_pill_rect.width() < 100.0 => Some("Search"),
                Action::Nav(View::Lyrics) => Some("Open lyrics"),
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

    /// Forward a raw window event to the header search `TextInput` on its real
    /// `handle_event` path (so it runs the same selection / clipboard / caret
    /// logic as any other field). The field is local-mode, so the doc handle it
    /// receives is inert; a throwaway taffy node satisfies the trait signature.
    /// Returns whether the widget consumed the event.
    fn forward_to_search(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
    ) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        // `new_leaf` on a fresh tree with a default `Style` cannot fail (see
        // `TaffyTreeExt` in `ui::widget`); this throwaway node only exists to
        // satisfy `TextInput::handle_event`'s trait signature.
        #[allow(
            clippy::expect_used,
            reason = "new_leaf on a fresh tree with a default Style cannot fail; this throwaway node only satisfies TextInput::handle_event's trait signature"
        )]
        let node = taffy
            .new_leaf(Style::default())
            .expect("a leaf on a fresh taffy tree");
        let mut handled = false;
        let mut focus_target = None;
        self.search_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
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
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.dirty = true;
        // Feed the search field cursor moves while it's being hovered or
        // is mid drag-select, so its hover state + selection track the
        // pointer (a drag that leaves the pill still extends).
        if self.search_input.is_dragging() || self.search_input.get_rect().contains(self.mouse) {
            let dragging = self.search_input.is_dragging();
            self.forward_to_search(event, scale_factor, modifiers, state);
            if dragging {
                *handled = true;
                return;
            }
        }
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
        if let Some(action) = self.row_menu_at(self.mouse) {
            self.dispatch(action, state);
            *handled = true;
        }
    }

    /// Left press: search field / volume popover dismiss / hamburger / transport
    /// drag start / now-playing swipe capture / topmost hit — in priority order.
    fn on_left_press(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // A press inside the search field routes to the real TextInput
        // (caret placement, drag-select, double-click word). Also switch
        // to the Search view + focus it so typing lands there.
        if self.search_input.get_rect().contains(self.mouse) {
            if self.view == View::Search {
                self.search_input.set_focus(true);
            } else {
                self.dispatch(Action::FocusSearch, state);
            }
            self.forward_to_search(event, scale_factor, modifiers, state);
            self.dirty = true;
            *handled = true;
            return;
        }
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
        // Hamburger toggle wins over everything (it sits above the
        // scrim hit that otherwise covers the whole content in overlay).
        if self.show_hamburger() && self.hamburger_rect.contains(self.mouse) {
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
            // Clicking anywhere outside the search field blurs it.
            if !matches!(action, Action::FocusSearch) {
                self.search_input.set_focus(false);
            }
            self.dispatch(action, state);
            *handled = true;
        } else {
            self.search_input.set_focus(false);
        }
    }

    /// Left release: end a search-field drag, resolve a now-playing swipe, or
    /// end a scrubber/volume drag.
    fn on_left_release(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // End an in-progress search-field selection drag on the widget.
        if self.search_input.is_dragging() {
            self.forward_to_search(event, scale_factor, modifiers, state);
            *handled = true;
            return;
        }
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

    /// A focused search field routes its keystrokes to the real `ui::TextInput`
    /// (Ctrl+A/C/X/V, selection, word nav, …); Escape/Enter stay at the app
    /// level, everything else falls through to [`Self::on_key`].
    fn on_key_input(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if self.search_input.is_focused() && self.name_prompt.is_none() {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.search_input.set_focus(false);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.request_live_search();
                    self.search_input.set_focus(false);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                _ => {}
            }
            let before = self.search_input.value().to_string();
            let consumed = self.forward_to_search(event, scale_factor, modifiers, state);
            if self.search_input.value() != before {
                // Debounce the live query a short beat after the last edit.
                self.search_edit_at = Some(ui::time::now());
            }
            if consumed {
                self.dirty = true;
                *handled = true;
                return;
            }
        }
        self.on_key(logical_key, text, modifiers, state, handled);
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
                    "/" => {
                        self.dispatch(Action::FocusSearch, state);
                        *handled = true;
                    }
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

    /// Paint an entity's real cover art (album / track / playlist) if it has
    /// loaded into the async image cache, else a procedural placeholder tile.
    /// `id` keys the cover cache; `label` seeds the placeholder's initials.
    fn draw_cover(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str, label: &str, radius: f64) {
        if let Some(img) = self.art_image(id) {
            ui::image_store::paint_image_cover(ctx.scene, ctx.transform, rect, &img, radius);
        } else {
            art::draw_album_art(ctx.scene, ctx.transform, rect, id, label, ctx.text, radius);
        }
    }

    /// Paint a circular artist avatar filling `disc` — the real image clipped
    /// to it if loaded, else the procedural two-tone circle with initials.
    fn draw_avatar(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        disc: Circle,
        id: &str,
        label: &str,
    ) {
        let (center, radius) = (disc.center, disc.radius);
        if let Some(img) = self.art_image(id) {
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                ctx.transform,
                &disc,
            );
            let r = Rect::new(
                center.x - radius,
                center.y - radius,
                center.x + radius,
                center.y + radius,
            );
            ui::image_store::paint_image_cover(ctx.scene, ctx.transform, r, &img, 0.0);
            ctx.scene.pop_layer();
        } else {
            let (base, accent) = art::art_colors(id);
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, base, None, &disc);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                accent.multiply_alpha(0.8),
                None,
                &Circle::new(
                    Point::new(center.x - radius * 0.24, center.y - radius * 0.24),
                    radius * 0.68,
                ),
            );
            let initials = art::initials(label);
            let sz = (radius * 0.6) as f32;
            let (tw, th) = ctx
                .text
                .measure_weighted(&initials, sz, None, TextWeight::Bold);
            ctx.text(
                &initials,
                &TextStyle::new(sz).with_weight(TextWeight::Bold),
                c.text.multiply_alpha(0.95),
                Point::new(
                    center.x - f64::from(tw) / 2.0,
                    center.y - f64::from(th) / 2.0,
                ),
                None,
            );
        }
    }

    // ---- master frame -----------------------------------------------------

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
        self.open_file_button.apply_layout_external(Rect::ZERO);
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
        // Apply any deferred `--shots` staging now that `rect` is known.
        self.apply_shot_stage();
        self.tick();
        if let Some((_, at)) = &self.toast
            && at.elapsed().as_secs_f32() > 2.6
        {
            self.toast = None;
        }

        // Advance the adaptive sidebar drawer: tick eases the reveal off a real
        // wall-clock delta, resolve re-decides docked-vs-overlay from the width
        // and hands back the sidebar + content rects (settling the reveal on the
        // first frame / a breakpoint crossing, honouring any `--shots` snap).
        self.drawer.tick(false);
        let c = Palette::from(ctx.theme);
        let r = self.rect;
        let large_text = ctx.theme.text_scale >= 1.35;
        // Large text always uses the overlay drawer. A 1280px physical window
        // is only 640px of effective reading width at 200%; treating it as a
        // docked desktop would either steal half the content or, previously,
        // remove navigation altogether.
        self.drawer.set_breakpoint(if large_text {
            f64::MAX
        } else {
            SIDEBAR_BREAKPOINT
        });
        self.drawer.set_overlay_width(if large_text {
            r.width().min(420.0)
        } else {
            280.0
        });
        let content_rect = self.drawer.resolve(self.rect);
        self.open_file_gap = f64::from(ctx.theme.padding_standard);
        if matches!(self.view, View::Home) {
            let measured = self.open_file_button.measure(
                Size {
                    width: None,
                    height: None,
                },
                Size {
                    width: taffy::AvailableSpace::Definite(
                        (content_rect.width() - self.open_file_gap * 4.0).max(0.0) as f32,
                    ),
                    height: taffy::AvailableSpace::MaxContent,
                },
                ctx.text,
            );
            self.open_file_button
                .apply_layout_external(Rect::from_origin_size(
                    Point::ORIGIN,
                    (f64::from(measured.width), f64::from(measured.height)),
                ));
        }

        // This app is deliberately custom-canvas rather than a stack of stock
        // widgets.  At 200% text the ordinary multi-column desktop geometry
        // cannot merely enlarge glyphs: it must become a single-column,
        // touchable listening view.  Taking this branch before the normal
        // sidebar/table layout prevents clipped text and overlapping controls.
        if large_text {
            self.render_large_text_frame(ctx, &c, r);
            // Keep the normal shared drawer semantics and an always-visible
            // keyboard/pointer affordance at 200%. The former large-text frame
            // silently omitted both, trapping users on the current screen.
            let scrim = self.drawer.scrim_rect();
            if scrim.width() > 0.5 {
                self.drawer.draw_scrim(ctx.scene, ctx.transform);
                self.push_hit(scrim, Action::CloseSidebar);
            }
            let side = self.sidebar_rect();
            if side.width() > 0.5 {
                self.draw_sidebar(ctx, &c, side);
            }
            self.draw_hamburger(ctx, &c, Rect::new(r.x0, r.y0, r.x1, r.y0 + 96.0));
            self.draw_large_text_toast(ctx, &c);
            // Accessibility-scale presentation keeps the same real modal and
            // context-menu widgets; only the underlying dense canvas changes.
            if self.name_prompt.is_some() {
                self.ensure_name_dialog();
                self.layout_name_dialog();
                if let Some(nd) = self.name_dialog.as_mut() {
                    nd.dialog.draw(
                        ctx.scene,
                        ctx.transform,
                        ctx.text,
                        ctx.theme,
                        &PresenceCtx::empty(),
                    );
                }
            }
            if self.context.is_open() {
                self.context.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &PresenceCtx::empty(),
                );
            }
            return;
        }

        // App background.
        ctx.scene.fill(Fill::NonZero, ctx.transform, c.bg, None, &r);

        // Content column begins after the docked sidebar inset (full-width in
        // overlay, where the sidebar floats over the content).
        let content_x0 = content_rect.x0;
        let header = Rect::new(content_x0, r.y0, r.x1, r.y0 + HEADER_H);
        let body = Rect::new(content_x0, r.y0 + HEADER_H, r.x1, r.y1 - NOWPLAYING_H);
        // The now-playing transport runs along the bottom of the content column,
        // starting to the right of the docked sidebar so the full-height sidebar
        // is never occluded (full-width in overlay, where the inset is 0).
        let nowplaying = self.nowplaying_rect();
        self.body_rect = body;

        // Body first so chrome draws on top (and chrome hits win).
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let start_y = body.y0 - self.scroll;
        let end_y = self.draw_body(ctx, &c, body, start_y);
        if self.open_file_button.get_rect().width() > 0.0 {
            self.open_file_button.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }
        ctx.scene.pop_layer();
        self.content_height = (end_y + self.scroll) - body.y0 + 24.0;
        let max = (self.content_height - body.height()).max(0.0);
        if self.scroll > max {
            self.scroll = max;
        }

        self.draw_header(ctx, &c, header);
        self.draw_nowplaying(ctx, &c, nowplaying);

        // Overlay flap: dim the content behind the drawer's scrim (alpha tracks
        // the eased reveal), and register a hit so a scrim tap dismisses it.
        let scrim = self.drawer.scrim_rect();
        if scrim.width() > 0.5 {
            self.drawer.draw_scrim(ctx.scene, ctx.transform);
            self.push_hit(scrim, Action::CloseSidebar);
        }

        // Sidebar (docked beside the content, or floating in overlay). Skip the
        // draw entirely when fully collapsed so its hits don't shadow content.
        let side = self.sidebar_rect();
        if side.width() > 0.5 {
            self.draw_sidebar(ctx, &c, side);
        }

        // Hamburger / close toggle over the content's top-left corner,
        // whenever the sidebar isn't fully docked-open.
        if self.show_hamburger() {
            self.draw_hamburger(ctx, &c, header);
        }

        // Collapsed-volume popover floats above the now-playing bar.
        self.draw_volume_popover(ctx, &c);

        // Queue hover preview floats above the now-playing bar's queue button.
        self.draw_queue_preview(ctx, &c);

        self.draw_toast(ctx);

        // Name-input overlay (create / rename playlist) — the shared
        // ModalDialog paints its own scrim, box, title, footer, and field.
        if self.name_prompt.is_some() {
            self.ensure_name_dialog();
            self.layout_name_dialog();
            if let Some(nd) = self.name_dialog.as_mut() {
                nd.dialog.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &PresenceCtx::empty(),
                );
            }
        } else {
            self.name_dialog = None;
        }

        // The context menu paints last so it sits on top of all content.
        if self.context.is_open() {
            // Dimming scrim that also dismisses on click (registered as a hit).
            self.context.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }
    }

    /// An intentionally compact, one-column layout for large accessibility
    /// text. It keeps the real song actions and transport controls, but drops
    /// secondary desktop columns so 200% text remains legible and tappable.
    #[allow(
        clippy::too_many_lines,
        reason = "the large-text canvas deliberately keeps its header, list, and transport geometry together so accessibility-scale hit targets cannot drift from their drawing"
    )]
    fn render_large_text_frame(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        ctx.scene.fill(Fill::NonZero, ctx.transform, c.bg, None, &r);
        let search_screen = self.view == View::Search;
        let title = self.view_title();
        let LargeTextBands {
            title_h,
            header_h,
            row_title_h,
            row_block_h,
            row_h,
            player_label_h,
            player_block_h,
            player_h,
        } = LargeTextBands::measure(ctx.text, &title);
        let title_band_h = header_h;
        let header_h = header_h + if search_screen { 58.0 } else { 0.0 };
        let header = Rect::new(r.x0, r.y0, r.x1, r.y0 + header_h);
        let body = Rect::new(r.x0, header.y1, r.x1, r.y1 - player_h);
        self.body_rect = body;
        // Centred on the measured line inside the header (the old literal
        // `y = 28.0` assumed `header.y0 == 0`) and on the content gutter.
        ctx.text(
            &title,
            &TextStyle::new(18.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(
                header.x0 + CONTENT_GUTTER,
                header.y0 + (title_band_h - title_h) / 2.0,
            ),
            None,
        );
        // Centred in the header (the old literal 20..76 assumed `header.y0 == 0`)
        // and on the raised surface so it reads in the light theme too.
        let search = Rect::new(
            header.x1 - CONTENT_GUTTER - 56.0,
            header.y0 + (header_h - 56.0) / 2.0,
            header.x1 - CONTENT_GUTTER,
            header.y0 + f64::midpoint(header_h, 56.0),
        );
        if search_screen {
            // Keep the same real TextInput used by the desktop header.  Its
            // native editing and assistive semantics stay intact; only the
            // roomy large-text geometry changes.
            let pill = Rect::new(
                header.x0 + 16.0,
                header.y0 + title_band_h,
                header.x1 - 16.0,
                header.y1 - 14.0,
            );
            let hovered = self.push_hit(pill, Action::FocusSearch);
            surface::card(ctx.scene, ctx.transform, pill, 16.0, c.card);
            if self.search_input.is_focused() {
                ctx.scene.stroke(
                    &Stroke::new(2.0),
                    ctx.transform,
                    c.accent,
                    None,
                    &rrect(pill, 16.0),
                );
            } else if hovered {
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    c.border,
                    None,
                    &rrect(pill, 16.0),
                );
            }
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Search,
                Point::new(pill.x0 + 24.0, pill.center().y),
                20.0,
                c.dim,
            );
            self.search_pill_rect = pill;
            self.search_input.set_font_size(16.0);
            self.search_input.apply_layout_external(Rect::new(
                pill.x0 + 48.0,
                pill.y0,
                pill.x1 - 16.0,
                pill.y1,
            ));
            self.search_input.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        } else {
            let hovered = self.push_hit(search, Action::FocusSearch);
            ctx.fill_round(search, 18.0, if hovered { c.white10 } else { c.panel });
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Search,
                search.center(),
                21.0,
                c.text,
            );
            self.search_pill_rect = search;
            self.search_input.set_font_size(14.0);
            self.search_input.apply_layout_external(Rect::ZERO);
        }
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            surface::edge_on(c.bg),
            None,
            &Rect::new(body.x0, body.y0, body.x1, body.y0 + 1.0),
        );

        let search_results = if self.view == View::Search {
            let query = self.search_input.value().trim();
            (!query.is_empty()).then(|| self.lib.search(query))
        } else {
            None
        };
        let mut track_ids: Vec<String> = match &self.view {
            View::Liked => self.lib.liked.iter().cloned().collect(),
            View::Downloads => {
                let mut ids = self
                    .device_cache
                    .iter()
                    .filter(|(_, state)| !matches!(state, CacheState::Streamed))
                    .filter_map(|(id, _)| self.lib.track(id).map(|track| track.id.clone()))
                    .collect::<Vec<_>>();
                let failed = self
                    .offline_errors
                    .keys()
                    .filter(|id| self.lib.track(id).is_some() && !ids.contains(id))
                    .cloned()
                    .collect::<Vec<_>>();
                ids.extend(failed);
                ids.sort_by(|left, right| {
                    self.lib
                        .track(left)
                        .map(|track| &track.title)
                        .cmp(&self.lib.track(right).map(|track| &track.title))
                });
                ids
            }
            View::Album(id) => self
                .lib
                .album_tracks(id)
                .into_iter()
                .map(|track| track.id.clone())
                .collect(),
            View::Playlist(id) => self
                .lib
                .playlist(id)
                .map_or_else(Vec::new, |playlist| playlist.track_ids.clone()),
            View::Queue => self.pb.queue.clone(),
            View::Artist(id) => self
                .lib
                .tracks_sorted()
                .into_iter()
                .filter(|track| track.artist_id == *id)
                .map(|track| track.id.clone())
                .collect(),
            View::Home | View::Songs => self
                .lib
                .tracks_sorted()
                .into_iter()
                .map(|track| track.id.clone())
                .collect(),
            View::Search => search_results
                .as_ref()
                .map_or_else(Vec::new, |results| results.tracks.clone()),
            View::Albums | View::Artists | View::Playlists | View::Lyrics => Vec::new(),
        };
        if matches!(self.view, View::Album(_) | View::Artist(_)) {
            let mut seen = std::collections::BTreeSet::new();
            track_ids.retain(|id| {
                self.lib.track(id).is_some_and(|track| {
                    seen.insert((
                        track.title.to_lowercase(),
                        track.artist_id.clone(),
                        track.duration_s,
                    ))
                })
            });
        }
        // Bound both materialization and the shared playback snapshot. Every
        // row action then clones an `Arc` (O(1)), never the complete queue.
        let queue = Arc::new(track_ids.into_iter().take(2_048).collect::<Vec<_>>());
        let mut rows: Vec<(String, String, String, Action, bool)> = queue
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let track = self.playback_track(id)?;
                let retry_download =
                    self.view == View::Downloads && self.offline_errors.contains_key(id);
                Some((
                    track.title.clone(),
                    if retry_download {
                        format!(
                            "Download failed · {}",
                            self.lib.artist_name(&track.artist_id)
                        )
                    } else {
                        self.lib.artist_name(&track.artist_id).to_string()
                    },
                    if retry_download {
                        "Retry".to_string()
                    } else {
                        ui::format::timecode(u64::from(track.duration_s))
                    },
                    if retry_download {
                        Action::SetOffline {
                            track: track.id.clone(),
                            keep: true,
                        }
                    } else {
                        Action::Play {
                            queue: Arc::clone(&queue),
                            index,
                        }
                    },
                    !retry_download && self.pb.current.as_deref() == Some(track.id.as_str()),
                ))
            })
            .collect();
        let active_lyric_at = self
            .lyrics
            .iter()
            .filter_map(|line| line.at_ms)
            .filter(|at| *at <= self.playhead_ms as u64)
            .max();
        match &self.view {
            View::Albums => rows.extend(self.lib.albums_sorted().into_iter().take(2_048).map(
                |album| {
                    (
                        album.title.clone(),
                        self.lib.artist_name(&album.artist_id).to_string(),
                        album.year.to_string(),
                        Action::Nav(View::Album(album.id.clone())),
                        false,
                    )
                },
            )),
            View::Artists => rows.extend(self.lib.artists_sorted().into_iter().take(2_048).map(
                |artist| {
                    (
                        artist.name.clone(),
                        if artist.genre.is_empty() {
                            "Artist".to_string()
                        } else {
                            artist.genre.clone()
                        },
                        String::new(),
                        Action::Nav(View::Artist(artist.id.clone())),
                        false,
                    )
                },
            )),
            View::Playlists => {
                let mut playlists = self.lib.playlists.iter().collect::<Vec<_>>();
                playlists.sort_by(|left, right| left.name.cmp(&right.name));
                rows.extend(playlists.into_iter().take(2_048).map(|playlist| {
                    (
                        playlist.name.clone(),
                        format!("{} songs", playlist.track_ids.len()),
                        String::new(),
                        Action::Nav(View::Playlist(playlist.id.clone())),
                        false,
                    )
                }));
            }
            View::Lyrics => rows.extend(self.lyrics.iter().take(2_048).map(|line| {
                (
                    line.text.clone(),
                    line.at_ms.map_or_else(String::new, ui::format::timecode),
                    String::new(),
                    Action::None,
                    line.at_ms.is_some() && line.at_ms == active_lyric_at,
                )
            })),
            View::Search => {
                if let Some(results) = &search_results {
                    rows.extend(
                        results
                            .albums
                            .iter()
                            .take(2_048 - rows.len())
                            .filter_map(|id| {
                                let album = self.lib.album(id)?;
                                Some((
                                    album.title.clone(),
                                    self.lib.artist_name(&album.artist_id).to_string(),
                                    "Album".to_string(),
                                    Action::Nav(View::Album(album.id.clone())),
                                    false,
                                ))
                            }),
                    );
                    rows.extend(
                        results
                            .artists
                            .iter()
                            .take(2_048 - rows.len())
                            .filter_map(|id| {
                                let artist = self.lib.artist(id)?;
                                Some((
                                    artist.name.clone(),
                                    artist.genre.clone(),
                                    "Artist".to_string(),
                                    Action::Nav(View::Artist(artist.id.clone())),
                                    false,
                                ))
                            }),
                    );
                }
                let needle = self.search_input.value().trim().to_lowercase();
                if !needle.is_empty() {
                    let mut playlists = self
                        .lib
                        .playlists
                        .iter()
                        .filter(|playlist| playlist.name.to_lowercase().contains(&needle))
                        .collect::<Vec<_>>();
                    playlists.sort_by(|left, right| left.name.cmp(&right.name));
                    rows.extend(
                        playlists
                            .into_iter()
                            .take(2_048 - rows.len())
                            .map(|playlist| {
                                (
                                    playlist.name.clone(),
                                    format!("{} songs", playlist.track_ids.len()),
                                    "Playlist".to_string(),
                                    Action::Nav(View::Playlist(playlist.id.clone())),
                                    false,
                                )
                            }),
                    );
                }
            }
            _ => {}
        }
        let mut y = body.y0 - self.scroll;
        if matches!(self.view, View::Home) && !rows.is_empty() {
            y = self.position_open_file_button(body.x0, y);
            self.open_file_button.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
        }
        for (title, subtitle, trailing, action, active) in &rows {
            let row = Rect::new(body.x0 + 16.0, y, body.x1 - 16.0, y + row_h);
            let title_top = row.y0 + (row_h - row_block_h) / 2.0;
            let artist_top = title_top + row_title_h + LARGE_ROW_LINE_GAP;
            let text_x = body.x0 + CONTENT_GUTTER;
            let text_x1 = body.x1 - CONTENT_GUTTER;
            let hover = self.body_hit(row, action.clone());
            if hover || *active {
                ctx.fill_round(
                    row,
                    14.0,
                    if hover {
                        c.white10
                    } else {
                        c.accent.multiply_alpha(0.15)
                    },
                );
            }
            let title = elide(
                ctx.text,
                title,
                12.0,
                TextWeight::Semibold,
                row.width() - 148.0,
            );
            let subtitle = elide(
                ctx.text,
                subtitle,
                10.0,
                TextWeight::Regular,
                row.width() - 148.0,
            );
            ctx.text(
                &title,
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                if *active { c.accent } else { c.text },
                Point::new(text_x, title_top),
                None,
            );
            ctx.text(
                &subtitle,
                &TextStyle::new(10.0),
                c.dim,
                Point::new(text_x, artist_top),
                None,
            );
            if !trailing.is_empty() {
                let slot = Rect::new(row.x0, row.y0, text_x1, row.y0 + row_h);
                draw_duration_right_aligned(ctx, trailing, 10.0, c.dim, slot);
            }
            y += row_h;
        }
        if rows.is_empty() {
            let (heading, detail) = match &self.view {
                View::Downloads => (
                    "No offline music",
                    "Keep a song offline to listen without a connection.",
                ),
                View::Liked => ("No liked songs", "Like a song and it will appear here."),
                View::Queue => ("Your queue is empty", "Choose a song to start listening."),
                View::Lyrics => ("No lyrics yet", "Play a song to find its lyrics."),
                View::Search => (
                    "Search your music",
                    "Type a song, artist, album, or playlist above.",
                ),
                View::Artist(id) if self.failed_pages.contains(id) => (
                    "Couldn’t load this artist",
                    "Check your connection, then go back and try again.",
                ),
                View::Album(id) if self.failed_pages.contains(id) => (
                    "Couldn’t load this album",
                    "Check your connection, then go back and try again.",
                ),
                View::Playlist(id) if self.failed_pages.contains(id) => (
                    "Couldn’t load this playlist",
                    "Check your connection, then go back and try again.",
                ),
                View::Home => (
                    "Your music starts here",
                    "Search for an artist, album, song, or playlist.",
                ),
                _ => ("Nothing here yet", "Your saved music will appear here."),
            };
            ctx.text(
                heading,
                &TextStyle::new(18.0).with_weight(TextWeight::Bold),
                c.text,
                Point::new(body.x0 + 34.0, body.y0 + 64.0),
                None,
            );
            let detail = elide(
                ctx.text,
                detail,
                11.0,
                TextWeight::Regular,
                body.width() - 68.0,
            );
            ctx.text(
                &detail,
                &TextStyle::new(11.0),
                c.dim,
                Point::new(body.x0 + 34.0, body.y0 + 116.0),
                None,
            );
            y = body.y0 + 160.0;
            if matches!(self.view, View::Home) {
                y = self.position_open_file_button(body.x0, y);
                self.open_file_button.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &PresenceCtx::empty(),
                );
            }
        }
        self.content_height = (y + self.scroll) - body.y0 + 20.0;
        self.scroll = self
            .scroll
            .clamp(0.0, (self.content_height - body.height()).max(0.0));

        let player = Rect::new(r.x0, r.y1 - player_h, r.x1, r.y1);
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, c.panel, None, &player);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.border,
            None,
            &Rect::new(player.x0, player.y0, player.x1, player.y0 + 1.0),
        );
        // Art, text block and play button all share the bar's midline (they
        // used to sit on three lines — 52, 56.5 and 58 px from the bar's
        // top — because each was placed by its own literal).
        let mid_y = player.y0 + player_h / 2.0;
        let play = Rect::from_center_size(
            (
                player.x1 - CONTENT_GUTTER - LARGE_PLAY_DIAMETER / 2.0,
                mid_y,
            ),
            (LARGE_PLAY_DIAMETER, LARGE_PLAY_DIAMETER),
        );
        if let Some(current) = self
            .pb
            .current
            .as_ref()
            .and_then(|id| self.playback_track(id))
        {
            // Art on the content gutter, like the header title, the rows and
            // the play button's right edge; a square as tall as the text
            // block beside it.
            let art_side = LARGE_PLAYER_ART_MIN.max(player_block_h);
            let art = Rect::from_center_size(
                (player.x0 + CONTENT_GUTTER + art_side / 2.0, mid_y),
                (art_side, art_side),
            );
            self.draw_cover(ctx, art, &current.album_id, &current.title, 10.0);
            let text_x = art.x1 + LARGE_PLAYER_TEXT_GAP;
            let text_budget = (play.x0 - LARGE_PLAYER_TEXT_GAP - text_x).max(120.0);
            let label = elide(
                ctx.text,
                &current.title,
                11.0,
                TextWeight::Semibold,
                text_budget,
            );
            let label_top = mid_y - player_block_h / 2.0;
            ctx.text(
                &label,
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(text_x, label_top),
                None,
            );
            let byline = elide(
                ctx.text,
                self.lib.artist_name(&current.artist_id),
                9.0,
                TextWeight::Regular,
                text_budget,
            );
            ctx.text(
                &byline,
                &TextStyle::new(9.0),
                c.dim,
                Point::new(text_x, label_top + player_label_h + LARGE_PLAYER_LINE_GAP),
                None,
            );
        } else {
            let (_, idle_h) =
                ctx.text
                    .measure_weighted("Nothing playing", 11.0, None, TextWeight::Regular);
            ctx.text(
                "Nothing playing",
                &TextStyle::new(11.0),
                c.dim,
                Point::new(player.x0 + CONTENT_GUTTER, mid_y - f64::from(idle_h) / 2.0),
                None,
            );
        }
        let play_hover = self.push_hit(play, Action::TogglePlay);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            if play_hover { c.accent_hover } else { c.accent },
            None,
            &Circle::new(play.center(), LARGE_PLAY_DIAMETER / 2.0),
        );
        icon::draw(
            ctx.scene,
            ctx.transform,
            if self.pb.playing {
                Icon::Pause
            } else {
                Icon::Play
            },
            play.center(),
            24.0,
            c.bg,
        );
    }

    /// The hamburger (or close-chevron) toggle that summons / dismisses the
    /// overlay sidebar. Sits in the header's top-left, left of the title.
    fn draw_hamburger(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, header: Rect) {
        let size = 34.0;
        let x0 = header.x0 + 14.0;
        let y0 = header.y0 + (HEADER_H - size) / 2.0;
        let btn = Rect::new(x0, y0, x0 + size, y0 + size);
        self.hamburger_rect = btn;
        let hover = btn.contains(self.mouse);
        surface::card(
            ctx.scene,
            ctx.transform,
            btn,
            10.0,
            if hover { c.white10 } else { c.white5 },
        );
        let cx = btn.center();
        let open = self.drawer.reveal() > 0.5;
        if open && self.drawer.is_overlay() {
            // Close affordance: a left chevron.
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::ChevronLeft,
                cx,
                18.0,
                c.text,
            );
        } else {
            // Three-line hamburger.
            let lw = 15.0;
            for i in 0..3 {
                let ly = cx.y - 5.0 + f64::from(i) * 5.0;
                ctx.fill_round(
                    Rect::new(cx.x - lw / 2.0, ly - 1.0, cx.x + lw / 2.0, ly + 1.0),
                    1.0,
                    c.text,
                );
            }
        }
    }

    // ---- sidebar ----------------------------------------------------------

    fn draw_sidebar(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        // Clip ALL sidebar content to the (possibly narrowing) panel rect, so a
        // collapse / slide never spills brand + nav pixels over the content
        // beside it. The clip is popped at the end of the method.
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, ctx.transform, &r);
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, c.panel, None, &r);
        // Right divider.
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.border,
            None,
            &Rect::new(r.x1 - 1.0, r.y0, r.x1, r.y1),
        );

        // No app-name wordmark or brand mark: the OS owns window chrome and
        // shows "Music" in the titlebar. The sidebar keeps only its collapse
        // affordance on this row and flows the nav up into the reclaimed space.

        // Docked-collapse affordance: a left-chevron button at the brand row's
        // right edge, shown only when the sidebar is docked + open (so there's
        // always a visible way to collapse it — the hamburger only reappears
        // once collapsed). `SidebarDrawer` owns the collapse *state machine*;
        // this button is the app's collapse *affordance*.
        if self.drawer.is_docked() && self.drawer.reveal() > 0.5 {
            let cb = Rect::new(r.x1 - 44.0, r.y0 + 18.0, r.x1 - 16.0, r.y0 + 46.0);
            let hover = self.push_hit(cb, Action::ToggleSidebar);
            if hover {
                ctx.fill_round(cb, 8.0, c.white10);
            }
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::ChevronLeft,
                cb.center(),
                16.0,
                if hover { c.text } else { c.dim },
            );
        }

        let mut y = r.y0 + 56.0;
        for dest in NAV_DESTINATIONS {
            y = self.nav_row(ctx, c, r, y, dest);
        }

        self.draw_sidebar_playlists(ctx, c, r, y);

        // Close the panel clip opened at the top of this method.
        ctx.scene.pop_layer();
    }

    /// The "PLAYLISTS" header + new-playlist affordance + the playlist list.
    fn draw_sidebar_playlists(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect, mut y: f64) {
        y += 14.0;
        ctx.text(
            "PLAYLISTS",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(r.x0 + 24.0, y),
            None,
        );
        // New-playlist plus — opens the inline create prompt.
        let plus_rect = Rect::new(r.x1 - 40.0, y - 4.0, r.x1 - 16.0, y + 20.0);
        let plus_hover = self.push_hit(plus_rect, Action::BeginNewPlaylist);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Plus,
            plus_rect.center(),
            16.0,
            if plus_hover { c.text } else { c.dim },
        );
        y += 26.0;

        let playlists = self.gather_playlists();
        for p in playlists {
            let active = self.view == View::Playlist(p.id.clone());
            let row = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 34.0 - ROW_GAP);
            let hover = self.push_hit(row, Action::Nav(View::Playlist(p.id.clone())));
            if active {
                // Selection is a lift with a derived edge (HIG §9.3), not a
                // bare wash that runs into the row below.
                surface::card(ctx.scene, ctx.transform, row, 8.0, c.white10);
            } else if hover {
                ctx.fill_round(row, 8.0, c.white5);
            }
            let col = if active { c.text } else { c.dim };
            let name = elide(
                ctx.text,
                &p.name,
                13.0,
                TextWeight::Medium,
                row.width() - 28.0,
            );
            ctx.text(
                &name,
                &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                col,
                Point::new(r.x0 + 24.0, vy(y, 34.0, 13.0)),
                None,
            );
            y += 34.0;
        }
    }

    fn nav_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        side: Rect,
        y: f64,
        dest: NavDestination,
    ) -> f64 {
        let NavDestination {
            icon: ic,
            label,
            view,
        } = dest;
        let active = self.view == view;
        let h = 42.0;
        let row = Rect::new(side.x0 + 12.0, y, side.x1 - 12.0, y + h);
        let hover = self.push_hit(row, Action::Nav(view));
        if active {
            // The selected destination is a lift with a derived edge (HIG
            // §9.3 / §9.4): accent stays on the ink and the marker bar, where
            // it means "you are here", not on the row's fill.
            surface::card(ctx.scene, ctx.transform, row, 10.0, c.white10);
            // Active marker bar.
            ctx.fill_round(
                Rect::new(side.x0, y + 9.0, side.x0 + 4.0, y + h - 9.0),
                2.0,
                c.accent,
            );
        } else if hover {
            ctx.fill_round(row, 10.0, c.white5);
        }
        let fg = if active {
            c.accent
        } else if hover {
            c.text
        } else {
            c.dim
        };
        icon::draw(
            ctx.scene,
            ctx.transform,
            ic,
            Point::new(row.x0 + 18.0, y + h / 2.0),
            19.0,
            fg,
        );
        ctx.text(
            label,
            &TextStyle::new(14.0).with_weight(TextWeight::Medium),
            fg,
            Point::new(row.x0 + 38.0, vy(y, h, 14.0)),
            None,
        );
        y + h + ROW_GAP
    }

    // ---- header -----------------------------------------------------------

    fn draw_header(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        ctx.scene.fill(Fill::NonZero, ctx.transform, c.bg, None, &r);
        // Leave room for the hamburger toggle in the corner when it's shown;
        // otherwise the title starts on the body's content gutter so the page
        // title, section labels, chips and grids share one left edge.
        let lead = if self.show_hamburger() {
            56.0
        } else {
            CONTENT_GUTTER
        };
        let mut x = r.x0 + lead;

        if !self.nav_back.is_empty() {
            let bz = Rect::new(x, r.y0 + 20.0, x + 32.0, r.y0 + 52.0);
            let hover = self.push_hit(bz, Action::Back);
            surface::card(
                ctx.scene,
                ctx.transform,
                bz,
                16.0,
                if hover { c.white10 } else { c.white5 },
            );
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::ChevronLeft,
                bz.center(),
                18.0,
                c.text,
            );
            x += 44.0;
        }

        let title = self.view_title();
        ctx.text(
            &title,
            &TextStyle::new(26.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(x, vy(r.y0, HEADER_H, 26.0)),
            None,
        );

        // On a narrow non-search page, a text field cannot coexist with the
        // title, optional Back button and hamburger without leaving a visibly
        // clipped placeholder. Use a compact, icon-labelled search affordance
        // there; it opens the full Search screen, whose field has room to edit.
        let compact_search = r.width() < 600.0 && !matches!(self.view, View::Search);
        if compact_search {
            self.draw_header_compact_search(ctx, c, r);
            return;
        }

        // Search pill on the right. The pill chrome (bg, focus/hover ring, the
        // magnifier) is drawn here; the editable text itself is a real
        // `ui::TextInput` laid into the pill's text area, so it carries its own
        // caret, selection, placeholder and full editing affordances.
        let pill_w = 320.0_f64.min(r.width() * 0.4);
        let pill = Rect::new(
            r.x1 - CONTENT_GUTTER - pill_w,
            r.y0 + 18.0,
            r.x1 - CONTENT_GUTTER,
            r.y0 + 54.0,
        );
        self.search_pill_rect = pill;
        let focused = self.search_input.is_focused();
        let hover = self.push_hit(pill, Action::FocusSearch);
        // Raised surface (like the home chips and cards), not the base panel
        // tone: in the light theme `panel` equals the header background, so the
        // field was invisible until hovered or focused.
        surface::card(
            ctx.scene,
            ctx.transform,
            pill,
            18.0,
            if hover { c.white10 } else { c.card },
        );
        if focused {
            ctx.scene.stroke(
                &Stroke::new(1.5),
                ctx.transform,
                c.accent,
                None,
                &rrect(pill, 18.0),
            );
        }
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Search,
            Point::new(pill.x0 + 20.0, pill.center().y),
            16.0,
            c.dim,
        );
        // The text field fills the pill after the magnifier gutter. Plain chrome
        // (no field border) so it reads as part of the pill.
        let text_rect = Rect::new(pill.x0 + 38.0, pill.y0, pill.x1 - 16.0, pill.y1);
        self.search_input.apply_layout_external(text_rect);
        self.search_input.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::empty(),
        );
    }

    /// The compact, icon-only search affordance a narrow header shows in place
    /// of the text field; it opens the full Search screen.
    fn draw_header_compact_search(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        // 36 px square on the content gutter, so its right edge lines up
        // with the body content below it (it sat 6 px outside before).
        let btn = Rect::new(
            r.x1 - CONTENT_GUTTER - 36.0,
            r.y0 + 18.0,
            r.x1 - CONTENT_GUTTER,
            r.y0 + 54.0,
        );
        self.search_pill_rect = btn;
        let hover = self.push_hit(btn, Action::FocusSearch);
        surface::card(
            ctx.scene,
            ctx.transform,
            btn,
            12.0,
            if hover { c.white10 } else { c.card },
        );
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Search,
            btn.center(),
            18.0,
            c.text,
        );
        // Do not retain a stale wide text-field rect that could accept a
        // pointer event behind this compact button.
        self.search_input.apply_layout_external(Rect::ZERO);
    }

    fn view_title(&self) -> String {
        match &self.view {
            View::Home => "Home".into(),
            View::Songs => "Songs".into(),
            View::Albums => "Albums".into(),
            View::Artists => "Artists".into(),
            View::Playlists => "Playlists".into(),
            View::Downloads => "Downloads".into(),
            View::Liked => "Liked Songs".into(),
            View::Search => "Search".into(),
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

    fn draw_body(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, y: f64) -> f64 {
        match self.view.clone() {
            View::Home => self.draw_home(ctx, c, body, y),
            View::Songs => {
                let ids: Vec<String> = self
                    .lib
                    .tracks_sorted()
                    .iter()
                    .map(|tk| tk.id.clone())
                    .collect();
                let rows = self.gather_tracks(&ids);
                let y = Self::draw_track_table_headings(ctx, c, body, y, true);
                self.draw_track_table(ctx, c, body, y, &TrackTable::new(&rows, &ids))
            }
            View::Albums => {
                let albums = self.gather_albums(None);
                self.draw_album_grid(ctx, c, body, y, &albums)
            }
            View::Artists => {
                let artists = self.gather_artists();
                self.draw_artist_grid(ctx, c, body, y, &artists)
            }
            View::Playlists => {
                let pls = self.gather_playlists();
                self.draw_playlist_grid(ctx, c, body, y, &pls)
            }
            View::Downloads => self.draw_downloads(ctx, c, body, y),
            View::Liked => self.draw_liked(ctx, c, body, y),
            View::Queue => self.draw_queue(ctx, c, body, y),
            View::Lyrics => self.draw_lyrics(ctx, c, body, y),
            View::Search => self.draw_search(ctx, c, body, y),
            View::Album(id) => self.draw_album_detail(ctx, c, body, y, &id),
            View::Artist(id) => self.draw_artist_detail(ctx, c, body, y, &id),
            View::Playlist(id) => self.draw_playlist_detail(ctx, c, body, y, &id),
        }
    }

    fn draw_home(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, mut y: f64) -> f64 {
        // Kick off the real Home fetch (charts / new releases / top artists).
        self.ensure_home();
        let pad = 28.0;
        y += 20.0;
        ctx.text(
            "Good evening",
            &TextStyle::new(28.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(body.x0 + pad, y),
            None,
        );
        y += 46.0;
        y = self.position_open_file_button(body.x0, y);

        // Real YouTube-Music content when it has landed.
        if let Some(home) = self.home.take() {
            for section in &home.sections {
                y = Self::section_label(ctx, c, body, y, &section.title);
                y = self.draw_home_section(ctx, c, body, y, section);
                y += 14.0;
            }
            self.home = Some(home);
            return y;
        }

        // While a real Home fetch is genuinely in flight, show a shimmering
        // skeleton grid rather than the sample fallback (or any "fake title"
        // text). Once a fetch has failed (offline build / network error) we stop
        // the skeleton and fall through to the honest state below, so Home never
        // shimmers forever.
        if self.home_is_loading() {
            return Self::draw_home_skeleton(ctx, c, body, y);
        }

        // Nothing live, and nothing in the library yet — the default build
        // starts empty. Show a composed, inviting zero-state instead of bare
        // section labels stacked over emptiness. Never fabricates content.
        if self.lib.playlists.is_empty() && self.lib.albums.is_empty() && self.lib.tracks.is_empty()
        {
            return self.draw_home_empty(ctx, c, body, y);
        }

        // The user's own library (built up as they play / like / save, or synced
        // from another device): quick-pick chips, jump-back-in, top tracks.
        y = self.draw_home_playlist_chips(ctx, c, body, y);

        // Jump back in — albums.
        y = Self::section_label(ctx, c, body, y, "Jump back in");
        let albums = self.gather_albums(Some(6));
        y = self.draw_album_grid(ctx, c, body, y, &albums);
        y += 10.0;

        // Top tracks.
        y = Self::section_label(ctx, c, body, y, "Top tracks");
        let ids: Vec<String> = self
            .lib
            .top_tracks(6)
            .iter()
            .map(|tk| tk.id.clone())
            .collect();
        let rows = self.gather_tracks(&ids);
        y = self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        );
        y
    }

    /// Quick-pick playlist chips (2-column grid). Returns the y below them.
    fn draw_home_playlist_chips(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
    ) -> f64 {
        let pad = 28.0;
        let pls = self.gather_playlists();
        let chip_h = 64.0;
        // Compact windows need complete playlist names more than they need a
        // nominal two-column layout. A single calm column also gives each
        // quick pick a proper touch target instead of two truncated slivers.
        let cols = if body.width() < 520.0 { 1 } else { 2 };
        let gap = 16.0;
        let chip_w = (body.width() - 2.0 * pad - gap * (cols as f64 - 1.0)) / cols as f64;
        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (chip_w + gap);
            let cy = y + (i / cols) as f64 * (chip_h + gap);
            let chip = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
            let hover = self.push_hit(chip, Action::Nav(View::Playlist(p.id.clone())));
            surface::card(
                ctx.scene,
                ctx.transform,
                chip,
                10.0,
                if hover { c.white10 } else { c.card },
            );
            // The cover sits inside the chip's edge rather than flush with
            // it, so the derived edge stays closed on the art side too
            // (kurbo's `inset` grows for positive values, hence the sign).
            self.draw_cover(
                ctx,
                Rect::new(cx, cy, cx + chip_h, cy + chip_h).inset(-CHIP_ART_INSET),
                &p.id,
                &p.name,
                10.0 - CHIP_ART_INSET,
            );
            let name = elide(
                ctx.text,
                &p.name,
                15.0,
                TextWeight::Semibold,
                chip_w - chip_h - 24.0,
            );
            ctx.text(
                &name,
                &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx + chip_h + 14.0, vy(cy, chip_h, 15.0)),
                None,
            );
        }
        let rows_n = pls.len().div_ceil(cols);
        y + rows_n as f64 * (chip_h + gap) + 16.0
    }

    fn section_label(ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, y: f64, label: &str) -> f64 {
        ctx.text(
            label,
            &TextStyle::new(19.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(body.x0 + 28.0, y),
            None,
        );
        y + 36.0
    }

    fn position_open_file_button(&mut self, left: f64, top: f64) -> f64 {
        let size = self.open_file_button.get_rect().size();
        self.open_file_button
            .apply_layout_external(Rect::from_origin_size(
                Point::new(left + self.open_file_gap * 2.0, top),
                size,
            ));
        top + size.height + self.open_file_gap
    }

    /// A composed, inviting zero-state for an empty Home (the default build
    /// starts with an empty library and no live content). A soft badge, a warm
    /// heading + subtext, and a single primary action — Search — that gets the
    /// user listening. Fabricates nothing (HIG §4/§7).
    fn draw_home_empty(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, y: f64) -> f64 {
        let cx = body.center().x;
        let mut yy = y + (body.height() * 0.14).clamp(48.0, 140.0);
        yy = Self::draw_home_empty_badge_and_heading(ctx, c, cx, yy);
        yy = Self::draw_home_empty_subtext(ctx, c, body, cx, yy);
        self.draw_home_empty_search_button(ctx, c, cx, yy)
    }

    /// The soft note badge + centred heading. Returns the y below them.
    fn draw_home_empty_badge_and_heading(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        cx: f64,
        mut yy: f64,
    ) -> f64 {
        let r = 42.0;
        let center = Point::new(cx, yy + r);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.card,
            None,
            &Circle::new(center, r),
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.accent.multiply_alpha(0.16),
            None,
            &Circle::new(center, r),
        );
        icon::draw(ctx.scene, ctx.transform, Icon::Note, center, 34.0, c.accent);
        yy += 2.0 * r + 30.0;

        let head = "Your music starts here";
        let hw = f64::from(
            ctx.text
                .measure_weighted(head, 25.0, None, TextWeight::Bold)
                .0,
        );
        ctx.text(
            head,
            &TextStyle::new(25.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(cx - hw / 2.0, yy),
            None,
        );
        yy + 42.0
    }

    /// The wrapped, centred subtext. Returns the y below it.
    fn draw_home_empty_subtext(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        cx: f64,
        mut yy: f64,
    ) -> f64 {
        let sub = "Search for a song, album or artist. Everything you play collects here — in sync across your devices.";
        let maxw = (body.width() - 56.0).clamp(180.0, 440.0);
        let mut cur = String::new();
        let mut lines: Vec<String> = Vec::new();
        for w in sub.split_whitespace() {
            let cand = if cur.is_empty() {
                w.to_string()
            } else {
                format!("{cur} {w}")
            };
            if f64::from(
                ctx.text
                    .measure_weighted(&cand, 14.0, None, TextWeight::Regular)
                    .0,
            ) > maxw
                && !cur.is_empty()
            {
                lines.push(std::mem::take(&mut cur));
                cur = w.to_string();
            } else {
                cur = cand;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        for line in &lines {
            let lw = f64::from(
                ctx.text
                    .measure_weighted(line, 14.0, None, TextWeight::Regular)
                    .0,
            );
            ctx.text(
                line,
                &TextStyle::new(14.0),
                c.dim,
                Point::new(cx - lw / 2.0, yy),
                None,
            );
            yy += 22.0;
        }
        yy + 18.0
    }

    /// Primary action: Search (the one thing to do from an empty Home).
    fn draw_home_empty_search_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        cx: f64,
        yy: f64,
    ) -> f64 {
        let bh = 44.0;
        let label = "Search";
        let labelw = f64::from(
            ctx.text
                .measure_weighted(label, 15.0, None, TextWeight::Semibold)
                .0,
        );
        let group = 18.0 + 10.0 + labelw;
        let bw = (group + 48.0).max(150.0);
        let btn = Rect::new(cx - bw / 2.0, yy, cx + bw / 2.0, yy + bh);
        let hover = self.push_hit(btn, Action::Nav(View::Search));
        ctx.fill_round(btn, bh / 2.0, if hover { c.accent_hover } else { c.accent });
        let gx = btn.center().x - group / 2.0;
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Search,
            Point::new(gx + 9.0, btn.center().y),
            16.0,
            Color::WHITE,
        );
        ctx.text(
            label,
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            Color::WHITE,
            Point::new(gx + 28.0, vy(btn.y0, bh, 15.0)),
            None,
        );
        yy + bh + 24.0
    }

    /// Draw one real Home section as a responsive grid of cover tiles. Track
    /// tiles play (queueing the whole section), album/artist tiles navigate to
    /// the real detail page.
    fn draw_home_section(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        section: &HomeSection,
    ) -> f64 {
        let pad = 28.0;
        let gap = 20.0;
        let target = 164.0;
        let avail = body.width() - 2.0 * pad;
        let cols = artwork_grid_columns(avail, target, gap);
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 52.0;

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

        for (i, item) in section.items.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            self.draw_home_section_tile_art(ctx, c, item, card, &mut queue);
            Self::draw_home_section_tile_labels(ctx, c, item, card);
        }
        let rows = section.items.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    /// The artwork (cover / avatar) in the top square of the tile `card`, its
    /// play-on-hover FAB (tracks and albums), and its hit target. A track tile
    /// plays `queue` from its own position.
    fn draw_home_section_tile_art(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        item: &HomeItem,
        card: Rect,
        queue: &mut SectionQueue<'_>,
    ) {
        let art_rect = Rect::new(card.x0, card.y0, card.x1, card.y0 + card.width());
        match item.kind {
            HomeItemKind::Track => {
                let lib_id = format!("yt:{}", item.video_id);
                let idx = queue.next;
                queue.next += 1;
                let hover = self.push_hit(
                    art_rect,
                    Action::Play {
                        queue: queue.ids.to_vec().into(),
                        index: idx,
                    },
                );
                self.draw_cover(ctx, art_rect, &lib_id, &item.title, 12.0);
                if hover {
                    let fab = Circle::new(Point::new(art_rect.x1 - 24.0, art_rect.y1 - 24.0), 18.0);
                    ctx.scene
                        .fill(Fill::NonZero, ctx.transform, c.accent, None, &fab);
                    icon::draw(
                        ctx.scene,
                        ctx.transform,
                        Icon::Play,
                        Point::new(fab.center.x + 1.0, fab.center.y),
                        14.0,
                        c.bg,
                    );
                }
            }
            HomeItemKind::Album => {
                let hover = self.push_hit(card, Action::Nav(View::Album(item.id.clone())));
                self.draw_cover(ctx, art_rect, &item.id, &item.title, 12.0);
                if hover {
                    let fab = Circle::new(Point::new(art_rect.x1 - 24.0, art_rect.y1 - 24.0), 18.0);
                    ctx.scene
                        .fill(Fill::NonZero, ctx.transform, c.accent, None, &fab);
                    icon::draw(
                        ctx.scene,
                        ctx.transform,
                        Icon::Play,
                        Point::new(fab.center.x + 1.0, fab.center.y),
                        14.0,
                        c.bg,
                    );
                }
            }
            HomeItemKind::Artist => {
                self.push_hit(card, Action::Nav(View::Artist(item.id.clone())));
                let radius = art_rect.width() / 2.0;
                let disc = Circle::new(art_rect.center(), radius);
                self.draw_avatar(ctx, c, disc, &item.id, &item.title);
            }
            HomeItemKind::Playlist => {}
        }
    }

    /// Title + subtitle beneath the artwork of the Home tile `card` (centred
    /// for artists, left-aligned otherwise).
    fn draw_home_section_tile_labels(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        item: &HomeItem,
        card: Rect,
    ) {
        let (cx, cy, card_w) = (card.x0, card.y0, card.width());
        let title_centered = item.kind == HomeItemKind::Artist;
        let title = elide(ctx.text, &item.title, 14.0, TextWeight::Semibold, card_w);
        let sub = elide(ctx.text, &item.subtitle, 12.0, TextWeight::Regular, card_w);
        if title_centered {
            let (tw, _) = ctx
                .text
                .measure_weighted(&title, 14.0, None, TextWeight::Semibold);
            let (sw, _) = ctx
                .text
                .measure_weighted(&sub, 12.0, None, TextWeight::Regular);
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx + (card_w - f64::from(tw)) / 2.0, cy + card_w + 8.0),
                None,
            );
            ctx.text(
                &sub,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(cx + (card_w - f64::from(sw)) / 2.0, cy + card_w + 28.0),
                None,
            );
        } else {
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx, cy + card_w + 8.0),
                None,
            );
            ctx.text(
                &sub,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(cx, cy + card_w + 28.0),
                None,
            );
        }
    }

    // ---- album grid -------------------------------------------------------

    fn draw_album_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        albums: &[AlbumDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 164.0;
        let avail = body.width() - 2.0 * pad;
        let cols = artwork_grid_columns(avail, target, gap);
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        for (i, a) in albums.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            self.draw_album_grid_card(ctx, c, a, card);
        }
        let rows = albums.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    /// One album card: cover, save toggle, play FAB on hover, title/subtitle.
    fn draw_album_grid_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        a: &AlbumDisp,
        card: Rect,
    ) {
        let (cx, cy, card_w) = (card.x0, card.y0, card.width());
        let hover = self.push_hit(card, Action::Nav(View::Album(a.id.clone())));
        if hover {
            ctx.fill_round(card.inflate(8.0, 8.0), 14.0, c.card_hover);
        }
        let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
        self.draw_cover(ctx, art_rect, &a.id, &a.title, 12.0);
        self.draw_album_grid_card_overlays(ctx, c, a, art_rect, hover);
        let title = elide(ctx.text, &a.title, 14.0, TextWeight::Semibold, card_w);
        ctx.text(
            &title,
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            c.text,
            Point::new(cx, cy + card_w + 8.0),
            None,
        );
        let sub = elide(
            ctx.text,
            &format!("{} · {}", a.artist, a.year),
            12.0,
            TextWeight::Regular,
            card_w,
        );
        ctx.text(
            &sub,
            &TextStyle::new(12.0),
            c.dim,
            Point::new(cx, cy + card_w + 28.0),
            None,
        );
    }

    /// The save (add-to-library) toggle and hover play FAB over an album
    /// card's cover art.
    fn draw_album_grid_card_overlays(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        a: &AlbumDisp,
        art_rect: Rect,
        hover: bool,
    ) {
        let saved = self.lib.is_album_saved(&a.id);
        // Add-to-library quick toggle: always shown when saved (so the
        // saved state reads at a glance), and on hover otherwise.
        if hover || saved {
            let add_c = Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0);
            let ah = self.push_hit(
                square_around(add_c, 34.0),
                Action::ToggleSaveAlbum(a.id.clone()),
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                c.scrim.multiply_alpha(0.55),
                None,
                &Circle::new(add_c, 16.0),
            );
            icon::draw(
                ctx.scene,
                ctx.transform,
                if saved { Icon::Downloaded } else { Icon::Plus },
                add_c,
                if saved { 15.0 } else { 16.0 },
                if saved {
                    c.success
                } else if ah {
                    c.text
                } else {
                    c.text.multiply_alpha(0.85)
                },
            );
        }
        // Play FAB on hover.
        if hover {
            let fab = Circle::new(Point::new(art_rect.x1 - 26.0, art_rect.y1 - 26.0), 20.0);
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, c.accent, None, &fab);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(fab.center.x + 1.0, fab.center.y),
                16.0,
                c.bg,
            );
            self.push_hit(
                Rect::new(
                    fab.center.x - 20.0,
                    fab.center.y - 20.0,
                    fab.center.x + 20.0,
                    fab.center.y + 20.0,
                ),
                Action::Play {
                    queue: a.queue.clone().into(),
                    index: 0,
                },
            );
        }
    }

    // ---- artist grid ------------------------------------------------------

    fn draw_artist_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        artists: &[ArtistDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 164.0;
        let avail = body.width() - 2.0 * pad;
        let cols = artwork_grid_columns(avail, target, gap);
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 50.0;

        for (i, a) in artists.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Artist(a.id.clone())));
            if hover {
                ctx.fill_round(card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            // Circular avatar (real image if loaded, else procedural).
            let center = Point::new(cx + card_w / 2.0, cy + card_w / 2.0);
            self.draw_avatar(ctx, c, Circle::new(center, card_w / 2.0), &a.id, &a.name);

            let name = elide(ctx.text, &a.name, 14.0, TextWeight::Semibold, card_w);
            let (nw, _) = ctx
                .text
                .measure_weighted(&name, 14.0, None, TextWeight::Semibold);
            ctx.text(
                &name,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx + (card_w - f64::from(nw)) / 2.0, cy + card_w + 8.0),
                None,
            );
            let genre = elide(ctx.text, &a.genre, 12.0, TextWeight::Regular, card_w);
            let (gw, _) = ctx
                .text
                .measure_weighted(&genre, 12.0, None, TextWeight::Regular);
            ctx.text(
                &genre,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(cx + (card_w - f64::from(gw)) / 2.0, cy + card_w + 26.0),
                None,
            );
            let _ = a.listeners;
        }
        let rows = artists.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- playlist grid ----------------------------------------------------

    fn draw_playlist_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        pls: &[PlaylistDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 164.0;
        let avail = body.width() - 2.0 * pad;
        let cols = artwork_grid_columns(avail, target, gap);
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        // "Create playlist" pill above the grid.
        let create = Rect::new(body.x0 + pad, y, body.x0 + pad + 184.0, y + 40.0);
        let ch = self.push_hit(create, Action::BeginNewPlaylist);
        ctx.fill_round(create, 20.0, if ch { c.accent_hover } else { c.accent });
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Plus,
            Point::new(create.x0 + 24.0, create.center().y),
            16.0,
            c.bg,
        );
        ctx.text(
            "New playlist",
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            c.bg,
            Point::new(create.x0 + 44.0, vy(create.y0, 40.0, 14.0)),
            None,
        );
        y += 56.0;

        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Playlist(p.id.clone())));
            if hover {
                ctx.fill_round(card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            self.draw_cover(ctx, art_rect, &p.id, &p.name, 12.0);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Playlist,
                Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0),
                22.0,
                c.text.multiply_alpha(0.85),
            );
            let title = elide(ctx.text, &p.name, 14.0, TextWeight::Semibold, card_w);
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx, cy + card_w + 8.0),
                None,
            );
            ctx.text(
                &format!("{} songs", p.count),
                &TextStyle::new(12.0),
                c.dim,
                Point::new(cx, cy + card_w + 28.0),
                None,
            );
        }
        let rows = pls.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- track table ------------------------------------------------------

    /// Quiet column labels make the roomy desktop Songs view scan as a table,
    /// while compact widths keep the calm, metadata-light mobile list.
    fn draw_track_table_headings(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        number_col: bool,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let cols = track_row_columns(left, right, number_col);
        let label_y = y + 10.0;
        ctx.text(
            "TITLE",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(cols.text, label_y),
            None,
        );
        if right - cols.text >= 560.0 && right - cols.album > 80.0 {
            ctx.text(
                "ALBUM",
                &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
                c.dim,
                Point::new(cols.album, label_y),
                None,
            );
        }
        // Durations are right-aligned to `cols.dur + DURATION_SLOT_W`
        // (`draw_track_row_download`); a numeric column's heading shares that
        // edge, so the label ends where the digits end instead of starting
        // 23 px to their left.
        let (time_w, _) = ctx
            .text
            .measure_weighted("TIME", 11.0, None, TextWeight::Semibold);
        ctx.text(
            "TIME",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(cols.dur + DURATION_SLOT_W - f64::from(time_w), label_y),
            None,
        );
        y + 30.0
    }

    /// The unified track-row table used by album, Liked Songs, Downloads **and**
    /// playlist detail — so every list shares one row look: the now-playing
    /// equalizer, like + download controls, and duration. Passing
    /// `playlist = Some(id)` additionally binds the right-click menu to that
    /// playlist and shows reorder (up/down) + remove affordances on hover.
    fn draw_track_table(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        table: &TrackTable<'_>,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        // Balanced column geometry: a right-pinned duration/download/like
        // cluster, a centred album column, and the title starting at `text_x`
        // — so a row reads as one connected line rather than title-left with a
        // cavern across to everything bunched on the right.
        let layout = TrackTableLayout {
            table,
            left,
            right,
            row_h: 52.0,
            cols: track_row_columns(left, right, table.number_col),
        };
        let row_h = layout.row_h;
        let shared_queue = Arc::new(table.queue.to_vec());

        for (i, tk) in table.rows.iter().enumerate() {
            let rr = Rect::new(left, y, right, y + row_h - ROW_GAP);
            let is_current = self.pb.current.as_deref() == Some(tk.id.as_str());
            let hovered = self.body_hit(
                rr,
                Action::Play {
                    queue: Arc::clone(&shared_queue),
                    index: i,
                },
            );
            // Right-click anywhere on the row opens its context menu (bound to
            // this playlist when we're in a playlist, so "Remove from playlist"
            // targets the right list).
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.push_row_menu(
                    rr,
                    Action::OpenTrackMenu {
                        track: tk.id.clone(),
                        in_playlist: table.playlist.map(str::to_string),
                        queue_index: None,
                    },
                );
            }
            if hovered {
                ctx.fill_round(rr, 8.0, c.white5);
            } else if table.playlist.is_some_and(|playlist| {
                self.revealed_membership
                    .as_ref()
                    .is_some_and(|(owner, track)| owner == playlist && track == &tk.id)
            }) {
                surface::card(ctx.scene, ctx.transform, rr, 8.0, c.white10);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                let row = TrackRow {
                    track: tk,
                    index: i,
                    y,
                    is_current,
                    hovered,
                };
                self.draw_track_row_content(ctx, c, &layout, &row);
            }
            y += row_h;
        }
        y
    }

    /// The content of one visible track row: index/equalizer, title/artist,
    /// album (if room), playlist reorder/remove (on hover), like, download
    /// state, and duration. Split out of [`Self::draw_track_table`]'s loop.
    fn draw_track_row_content(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        self.draw_track_row_index(ctx, c, layout, row);
        Self::draw_track_row_text(ctx, c, layout, row);
        self.draw_track_row_playlist_controls(ctx, c, layout, row);
        // Downloads dedicates the trailing cluster to a self-describing local
        // cache state/action; liking remains available from every library and
        // detail list without colliding with progress or Remove on narrow rows.
        if !matches!(self.view, View::Downloads) {
            self.draw_track_row_like(ctx, c, layout, row);
        }
        self.draw_track_row_download(ctx, c, layout, row);
    }

    /// The leading index column: a play glyph on hover, an animated equalizer
    /// for the current track, else the track number.
    fn draw_track_row_index(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        if !layout.table.number_col {
            return;
        }
        let (left, y, row_h) = (layout.left, row.y, layout.row_h);
        if row.hovered {
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(left + 14.0, y + row_h / 2.0),
                14.0,
                c.text,
            );
        } else if row.is_current {
            // Now-playing equalizer: animates while playing, static
            // (a fixed silhouette) when paused.
            let bars = eq_bars(self.pb.playing, c.reduce_motion);
            let mid = y + row_h / 2.0;
            for (bar_i, &frac) in bars.iter().enumerate() {
                let bx = left + 6.0 + bar_i as f64 * 6.0;
                let bh = 5.0 + frac * 13.0;
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    c.accent,
                    None,
                    &Rect::new(bx, mid - bh / 2.0, bx + 3.0, mid + bh / 2.0),
                );
            }
        } else {
            let num = format!("{}", row.track.no.max(row.index as u32 + 1));
            ctx.text(
                &num,
                &TextStyle::new(13.0),
                c.dim,
                Point::new(left + 8.0, vy(y, row_h, 13.0)),
                None,
            );
        }
    }

    /// Title, artist and (space permitting) album text.
    fn draw_track_row_text(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        let (y, row_h, right) = (row.y, layout.row_h, layout.right);
        let TrackColumns {
            text: text_x,
            album: album_x,
            dl: dl_x,
            ..
        } = layout.cols;
        let tk = row.track;
        let title_col = if row.is_current { c.accent } else { c.text };
        // The album column is useful on a desktop table, but at compact widths
        // it used to remain nominally enabled and squeeze the actual song
        // title down to a handful of characters.  Treat the metadata column as
        // one responsive unit: when it cannot leave a genuinely readable title
        // beside the trailing controls, give that space back to title/artist.
        let album_visible =
            layout.table.show_album && right - text_x >= 560.0 && right - album_x > 80.0;
        let text_right = if album_visible { album_x } else { dl_x - 28.0 };
        let text_width = (text_right - text_x - 16.0).max(0.0);
        let title = elide(ctx.text, &tk.title, 14.0, TextWeight::Medium, text_width);
        ctx.text(
            &title,
            &TextStyle::new(14.0).with_weight(TextWeight::Medium),
            title_col,
            Point::new(text_x, y + 9.0),
            None,
        );
        let artist = elide(ctx.text, &tk.artist, 12.0, TextWeight::Regular, text_width);
        ctx.text(
            &artist,
            &TextStyle::new(12.0),
            c.dim,
            Point::new(text_x, y + 28.0),
            None,
        );

        if album_visible {
            let al = elide(
                ctx.text,
                &tk.album,
                13.0,
                TextWeight::Regular,
                dl_x - 48.0 - album_x - 16.0,
            );
            ctx.text(
                &al,
                &TextStyle::new(13.0),
                c.dim,
                Point::new(album_x, vy(y, row_h, 13.0)),
                None,
            );
        }
    }

    /// Playlist reorder (up/down) + remove, on hover only. Tucked in the
    /// gap left of the like control.
    fn draw_track_row_playlist_controls(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        let pl_rm_x = layout.cols.like - 34.0;
        let pl_dn_x = pl_rm_x - 28.0;
        let pl_up_x = pl_dn_x - 28.0;
        let Some(pl) = layout.table.playlist else {
            return;
        };
        if !row.hovered {
            return;
        }
        let (i, mid_y) = (row.index, row.y + layout.row_h / 2.0);
        if i > 0 {
            let uc = Point::new(pl_up_x, mid_y);
            let uh = self.push_hit(
                square_around(uc, 24.0),
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i - 1,
                },
            );
            draw_chevron(ctx, uc, true, if uh { c.text } else { c.dim });
        }
        if i + 1 < layout.table.rows.len() {
            let dc = Point::new(pl_dn_x, mid_y);
            let dh = self.push_hit(
                square_around(dc, 24.0),
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i + 1,
                },
            );
            draw_chevron(ctx, dc, false, if dh { c.text } else { c.dim });
        }
        let rc = Point::new(pl_rm_x, mid_y);
        let rh = self.push_hit(
            square_around(rc, 24.0),
            Action::RemoveFromPlaylist {
                playlist: pl.to_string(),
                track: row.track.id.clone(),
            },
        );
        draw_x(ctx, rc, if rh { c.accent } else { c.dim });
    }

    /// Like toggle (heart) just left of the download control.
    fn draw_track_row_like(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        let tk = row.track;
        if self.is_local_file(&tk.id) {
            return;
        }
        let (y, row_h, like_x) = (row.y, layout.row_h, layout.cols.like);
        let liked = self.lib.is_liked(&tk.id);
        let like_rect = Rect::new(
            like_x - 14.0,
            y + row_h / 2.0 - 14.0,
            like_x + 14.0,
            y + row_h / 2.0 + 14.0,
        );
        let like_hover = self.push_hit(like_rect, Action::ToggleLike(tk.id.clone()));
        if liked || like_hover || row.hovered {
            icon::draw(
                ctx.scene,
                ctx.transform,
                if liked {
                    Icon::HeartFilled
                } else {
                    Icon::Heart
                },
                Point::new(like_x, y + row_h / 2.0),
                15.0,
                if liked {
                    c.accent
                } else if like_hover {
                    c.text
                } else {
                    c.dim
                },
            );
        }
    }

    /// Download state and its direct action. The Downloads view labels each
    /// row instead of duplicating detached status controls above the list.
    fn draw_track_row_download(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        layout: &TrackTableLayout<'_>,
        row: &TrackRow<'_>,
    ) {
        let tk = row.track;
        if self.is_local_file(&tk.id) {
            return;
        }
        let (y, row_h) = (row.y, layout.row_h);
        let (dl_x, dur_x) = (layout.cols.dl, layout.cols.dur);
        let state = self.lib.cache_state(&tk.id);
        let downloading = matches!(state, CacheState::Downloading(_));
        let failed = self.offline_errors.get(&tk.id).cloned();
        let remove = downloading || matches!(state, CacheState::Cached);
        let action = Action::SetOffline {
            track: tk.id.clone(),
            keep: !remove,
        };
        let status_rect = Rect::new(dl_x - 102.0, y, dl_x + 22.0, y + row_h);
        let hit = self.push_hit(
            if matches!(self.view, View::Downloads) {
                status_rect
            } else {
                square_around(Point::new(dl_x, y + row_h / 2.0), 28.0)
            },
            action,
        );
        let icon_center = Point::new(dl_x, y + row_h / 2.0);
        if downloading {
            draw_x(ctx, icon_center, if hit { c.accent } else { c.dim });
        } else if matches!(state, CacheState::Cached) {
            draw_check(ctx, icon_center, if hit { c.accent } else { c.success });
        } else {
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Download,
                icon_center,
                16.0,
                if hit {
                    c.accent
                } else if remove {
                    c.success
                } else if failed.is_some() {
                    c.accent
                } else {
                    c.dim
                },
            );
        }
        if matches!(self.view, View::Downloads) {
            let label = if let Some(error) = &failed {
                format!(
                    "Retry · {}",
                    elide(ctx.text, error, 11.0, TextWeight::Regular, 62.0)
                )
            } else {
                match state {
                    CacheState::Downloading(progress) => {
                        format!("{:.0}% · Cancel", progress * 100.0)
                    }
                    CacheState::Cached => "Cached · Remove".into(),
                    CacheState::Streamed => "Keep offline".into(),
                }
            };
            // The label ends a token step short of the glyph's left edge
            // (icon size 16 → half-width 8), so "Retry · Network co…" can
            // never run into the download icon it sits beside.
            let label_max = (icon_center.x - 8.0 - LABEL_GLYPH_GAP - status_rect.x0).max(0.0);
            let label = elide(ctx.text, &label, 11.0, TextWeight::Medium, label_max);
            ctx.text(
                &label,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                if hit {
                    c.text
                } else if failed.is_some() {
                    c.accent
                } else {
                    c.dim
                },
                Point::new(status_rect.x0, vy(y, row_h, 11.0)),
                None,
            );
        }

        // Duration (hidden when unknown — some live sources omit it).
        if tk.dur > 0 {
            let dur = ui::format::timecode(u64::from(tk.dur));
            // Tabular figures, measured and drawn with one style, so a column
            // of `m:ss` values lines up digit for digit and the row centres
            // the line on its measured height.
            let slot = Rect::new(dur_x, y, dur_x + DURATION_SLOT_W, y + row_h);
            draw_duration_right_aligned(ctx, &dur, 13.0, c.dim, slot);
        }
    }

    // ---- detail pages -----------------------------------------------------

    /// The shared header for an album/artist/playlist detail page: cover or
    /// avatar, kind label, title, subtitle, and the Play/Shuffle + optional
    /// library/follow buttons.
    fn draw_detail_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        header: DetailHeader<'_>,
    ) -> f64 {
        let pad = 28.0;
        let art_d = 168.0;
        let ax = body.x0 + pad;
        let ay = y + 12.0;
        let art = Rect::new(ax, ay, ax + art_d, ay + art_d);
        let tx = self.draw_detail_header_art_and_text(ctx, c, art, body.x1 - pad, &header);

        // Action buttons.
        let by = ay + 116.0;
        let shuf = self.draw_detail_header_transport(ctx, c, tx, by, header.queue);

        // The trailing pill, right of the shuffle button.
        let px = shuf.x1 + 12.0;
        let id = header.id.to_string();
        match header.kind {
            DetailKind::Album { saved } => {
                self.draw_detail_header_lib_button(ctx, c, Point::new(px, by), id, saved);
            }
            DetailKind::Artist { following } => {
                self.draw_detail_header_follow_button(ctx, c, Point::new(px, by), id, following);
            }
            DetailKind::Playlist => {}
        }

        y + art_d + 36.0
    }

    /// The cover/avatar in `art`, and the kind label, title and subtitle
    /// beside it, the title elided before `text_right`. Returns the x the
    /// action-button row starts at.
    fn draw_detail_header_art_and_text(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        art: Rect,
        text_right: f64,
        header: &DetailHeader<'_>,
    ) -> f64 {
        let DetailHeader {
            id: seed,
            title,
            subtitle,
            ..
        } = *header;
        let (ax, ay, art_d) = (art.x0, art.y0, art.width());
        if matches!(header.kind, DetailKind::Artist { .. }) {
            self.draw_avatar(ctx, c, Circle::new(art.center(), art_d / 2.0), seed, title);
        } else {
            self.draw_cover(ctx, art, seed, title, 14.0);
        }

        let tx = ax + art_d + 28.0;
        ctx.text(
            header.kind.label(),
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(tx, ay + 8.0),
            None,
        );
        let title_s = elide(ctx.text, title, 38.0, TextWeight::Bold, text_right - tx);
        ctx.text(
            &title_s,
            &TextStyle::new(38.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(tx, ay + 28.0),
            None,
        );
        ctx.text(
            subtitle,
            &TextStyle::new(14.0),
            c.dim,
            Point::new(tx, ay + 84.0),
            None,
        );
        tx
    }

    /// The Play + Shuffle buttons. Returns the shuffle button's rect so the
    /// caller can position any trailing pills after it.
    fn draw_detail_header_transport(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        tx: f64,
        by: f64,
        queue: Vec<String>,
    ) -> Rect {
        let play = Rect::new(tx, by, tx + 120.0, by + 40.0);
        let ph = self.push_hit(
            play,
            Action::Play {
                queue: queue.into(),
                index: 0,
            },
        );
        ctx.fill_round(play, 20.0, if ph { c.accent_hover } else { c.accent });
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Play,
            Point::new(play.x0 + 24.0, play.center().y),
            15.0,
            c.bg,
        );
        ctx.text(
            "Play",
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            c.bg,
            Point::new(play.x0 + 44.0, vy(by, 40.0, 14.0)),
            None,
        );

        let shuf = Rect::new(play.x1 + 12.0, by, play.x1 + 12.0 + 44.0, by + 40.0);
        let sh = self.push_hit(shuf, Action::ToggleShuffle);
        surface::card(
            ctx.scene,
            ctx.transform,
            shuf,
            20.0,
            if sh { c.white10 } else { c.card },
        );
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Shuffle,
            shuf.center(),
            18.0,
            if self.pb.shuffle {
                c.accent
            } else if sh {
                c.text
            } else {
                c.dim
            },
        );
        shuf
    }

    /// The trailing "Add to library" pill of an album header at `origin`.
    /// `saved` flips it to an "In Library" confirmed state.
    fn draw_detail_header_lib_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        origin: Point,
        id: String,
        saved: bool,
    ) {
        let (px, by) = (origin.x, origin.y);
        let pw = 150.0;
        let lib = Rect::new(px, by, px + pw, by + 40.0);
        let lh = self.push_hit(lib, Action::ToggleSaveAlbum(id));
        if saved {
            surface::card(ctx.scene, ctx.transform, lib, 20.0, c.success_wash);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Downloaded,
                Point::new(lib.x0 + 22.0, lib.center().y),
                16.0,
                c.success,
            );
            ctx.text(
                "In Library",
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.success,
                Point::new(lib.x0 + 42.0, vy(by, 40.0, 14.0)),
                None,
            );
        } else {
            surface::card(
                ctx.scene,
                ctx.transform,
                lib,
                20.0,
                if lh { c.white10 } else { c.card },
            );
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Plus,
                Point::new(lib.x0 + 22.0, lib.center().y),
                16.0,
                if lh { c.text } else { c.dim },
            );
            ctx.text(
                "Add to Library",
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                if lh { c.text } else { c.dim },
                Point::new(lib.x0 + 42.0, vy(by, 40.0, 14.0)),
                None,
            );
        }
    }

    /// The trailing "Follow" / "Following" toggle pill of an artist header at
    /// `origin`.
    fn draw_detail_header_follow_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        origin: Point,
        id: String,
        following: bool,
    ) {
        let (px, by) = (origin.x, origin.y);
        let pw = 118.0;
        let fol = Rect::new(px, by, px + pw, by + 40.0);
        let fh = self.push_hit(fol, Action::ToggleFollow(id));
        if following {
            surface::card(ctx.scene, ctx.transform, fol, 20.0, c.accent_wash);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Downloaded,
                Point::new(fol.x0 + 22.0, fol.center().y),
                15.0,
                c.accent,
            );
            ctx.text(
                "Following",
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.accent,
                Point::new(fol.x0 + 42.0, vy(by, 40.0, 14.0)),
                None,
            );
        } else {
            surface::card(
                ctx.scene,
                ctx.transform,
                fol,
                20.0,
                if fh { c.white10 } else { c.card },
            );
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Plus,
                Point::new(fol.x0 + 22.0, fol.center().y),
                16.0,
                if fh { c.text } else { c.dim },
            );
            ctx.text(
                "Follow",
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                if fh { c.text } else { c.dim },
                Point::new(fol.x0 + 42.0, vy(by, 40.0, 14.0)),
                None,
            );
        }
    }

    fn draw_album_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music album (its ordered track list) when available.
        self.ensure_album(id);
        if let Some(page) = self.album_pages.remove(id) {
            let r = self.draw_album_detail_live(ctx, c, body, y, &page);
            self.album_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_album_id(id) && !self.lib.albums.contains_key(id) {
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, c, body, y, "album");
            }
            return Self::draw_loading_detail(ctx, c, body, y, "ALBUM", id);
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
        let subtitle = format!("{artist} · {year} · {} songs", ids.len());
        let saved = self.lib.is_album_saved(id);
        let y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Album { saved },
                title: &title,
                subtitle: &subtitle,
                queue: ids.clone(),
            },
        );
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        )
    }

    /// Album detail rendered straight from a real [`AlbumPage`].
    fn draw_album_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        page: &AlbumPage,
    ) -> f64 {
        let ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let year = if page.year > 0 {
            format!(" \u{00b7} {}", page.year)
        } else {
            String::new()
        };
        let subtitle = format!("{}{year} \u{00b7} {} songs", page.artist_name, ids.len());
        let saved = self.lib.is_album_saved(&page.id);
        let y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Album { saved },
                title: &page.title,
                subtitle: &subtitle,
                queue: ids.clone(),
            },
        );
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album(),
        )
    }

    /// A shimmering **skeleton** shown while a real album/artist page loads —
    /// a placeholder cover + title/subtitle bars + several track rows, so the
    /// page reads as loading without any "fake title" text.
    fn draw_loading_detail(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        kind: &str,
        id: &str,
    ) -> f64 {
        let _ = (kind, id);
        let pad = 28.0;
        y += 12.0;
        let art = Rect::new(body.x0 + pad, y, body.x0 + pad + 168.0, y + 168.0);
        draw_skeleton_block(ctx, art, 14.0, c);
        let tx = art.x1 + 28.0;
        let right = body.x1 - pad;
        draw_skeleton_block(ctx, Rect::new(tx, y + 16.0, tx + 68.0, y + 30.0), 6.0, c);
        draw_skeleton_block(
            ctx,
            Rect::new(tx, y + 42.0, (tx + 340.0).min(right), y + 78.0),
            9.0,
            c,
        );
        draw_skeleton_block(
            ctx,
            Rect::new(tx, y + 90.0, (tx + 240.0).min(right), y + 110.0),
            7.0,
            c,
        );
        draw_skeleton_block(
            ctx,
            Rect::new(tx, y + 126.0, tx + 132.0, y + 158.0),
            18.0,
            c,
        );
        y += 168.0 + 30.0;
        // Skeleton track rows.
        for _ in 0..7 {
            let left = body.x0 + pad;
            draw_skeleton_block(
                ctx,
                Rect::new(left, y + 10.0, left + 16.0, y + 26.0),
                4.0,
                c,
            );
            draw_skeleton_block(
                ctx,
                Rect::new(
                    left + 34.0,
                    y + 8.0,
                    (left + 300.0).min(right - 80.0),
                    y + 22.0,
                ),
                6.0,
                c,
            );
            draw_skeleton_block(
                ctx,
                Rect::new(
                    left + 34.0,
                    y + 27.0,
                    (left + 200.0).min(right - 80.0),
                    y + 39.0,
                ),
                5.0,
                c,
            );
            draw_skeleton_block(
                ctx,
                Rect::new(right - 44.0, y + 16.0, right, y + 30.0),
                5.0,
                c,
            );
            y += 52.0;
        }
        y
    }

    /// An honest error state for a live detail page whose fetch failed (offline,
    /// geo-block, network error) — shown instead of an endless skeleton. The OS
    /// chrome's Back affordance returns the user; a visible Home recovery is
    /// included too because a deep link can legitimately have no local Back
    /// history to return to.
    fn draw_detail_error(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        kind: &str,
    ) -> f64 {
        let cx = body.center().x;
        let mut yy = y + (body.height() * 0.14).clamp(48.0, 140.0);
        let r = 38.0;
        let center = Point::new(cx, yy + r);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.card,
            None,
            &Circle::new(center, r),
        );
        icon::draw(ctx.scene, ctx.transform, Icon::Disc, center, 30.0, c.dim);
        yy += 2.0 * r + 28.0;
        let head = format!("Couldn't load this {kind}");
        let max_text_w = (body.width() - 32.0).max(1.0);
        let head = elide(ctx.text, &head, 20.0, TextWeight::Bold, max_text_w);
        let hw = f64::from(
            ctx.text
                .measure_weighted(&head, 20.0, None, TextWeight::Bold)
                .0,
        );
        ctx.text(
            &head,
            &TextStyle::new(20.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(cx - hw / 2.0, yy),
            None,
        );
        yy += 34.0;
        let sub = elide(
            ctx.text,
            "Check your connection and try again.",
            14.0,
            TextWeight::Regular,
            max_text_w,
        );
        let sw = f64::from(
            ctx.text
                .measure_weighted(&sub, 14.0, None, TextWeight::Regular)
                .0,
        );
        ctx.text(
            &sub,
            &TextStyle::new(14.0),
            c.dim,
            Point::new(cx - sw / 2.0, yy),
            None,
        );
        yy += 34.0;
        let back = Rect::new(cx - 62.0, yy, cx + 62.0, yy + 38.0);
        let hover = self.push_hit(back, Action::Nav(View::Home));
        ctx.fill_round(back, 19.0, if hover { c.accent_hover } else { c.accent });
        let label = "Back to Home";
        let lw = f64::from(
            ctx.text
                .measure_weighted(label, 13.0, None, TextWeight::Semibold)
                .0,
        );
        ctx.text(
            label,
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            c.bg,
            Point::new(back.center().x - lw / 2.0, vy(back.y0, back.height(), 13.0)),
            None,
        );
        back.y1 + 32.0
    }

    /// A shimmering skeleton grid for the Home page while the real charts /
    /// new-releases sections stream in.
    fn draw_home_skeleton(ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        let gap = 20.0;
        let target = 168.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        for _ in 0..2 {
            // Section title bar.
            draw_skeleton_block(
                ctx,
                Rect::new(body.x0 + pad, y, body.x0 + pad + 180.0, y + 22.0),
                7.0,
                c,
            );
            y += 40.0;
            for i in 0..cols {
                let cx = body.x0 + pad + i as f64 * (card_w + gap);
                draw_skeleton_block(ctx, Rect::new(cx, y, cx + card_w, y + card_w), 12.0, c);
                draw_skeleton_block(
                    ctx,
                    Rect::new(cx, y + card_w + 10.0, cx + card_w * 0.8, y + card_w + 24.0),
                    5.0,
                    c,
                );
                draw_skeleton_block(
                    ctx,
                    Rect::new(cx, y + card_w + 30.0, cx + card_w * 0.55, y + card_w + 42.0),
                    5.0,
                    c,
                );
            }
            y += card_w + 60.0;
        }
        y
    }

    fn draw_artist_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music artist page (top songs, albums, subscribers).
        self.ensure_artist(id);
        if let Some(page) = self.artist_pages.remove(id) {
            let r = self.draw_artist_detail_live(ctx, c, body, y, &page);
            self.artist_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_id(id) && !self.lib.artists.contains_key(id) {
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, c, body, y, "artist");
            }
            return Self::draw_loading_detail(ctx, c, body, y, "ARTIST", id);
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
        let subtitle = format!(
            "{genre} · {} monthly listeners",
            ui::format::compact_count(listeners)
        );
        let followed = self.lib.is_artist_followed(id);
        let y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Artist {
                    following: followed,
                },
                title: &name,
                subtitle: &subtitle,
                queue: top_ids.clone(),
            },
        );

        let mut y = Self::section_label(ctx, c, body, y + 8.0, "Popular");
        let rows = self.gather_tracks(&top_ids);
        y = self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &top_ids).without_album(),
        );
        y += 12.0;
        y = Self::section_label(ctx, c, body, y, "Albums");
        self.draw_album_grid(ctx, c, body, y, &albums)
    }

    /// A full artist page rendered from a real [`ArtistPage`]: header (play +
    /// shuffle + follow), popular tracks, a sectioned discography (Albums vs.
    /// Singles & EPs), related artists, and an About/bio panel.
    fn draw_artist_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        page: &ArtistPage,
    ) -> f64 {
        let subtitle = if page.subscriber_count > 0 {
            format!(
                "{} subscribers",
                ui::format::compact_count(page.subscriber_count)
            )
        } else {
            "Artist".to_string()
        };
        let top_ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let followed = self.lib.is_artist_followed(&page.id);
        let mut y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Artist {
                    following: followed,
                },
                title: &page.name,
                subtitle: &subtitle,
                queue: top_ids.clone(),
            },
        );

        // Popular / top tracks.
        if !top_ids.is_empty() {
            y = Self::section_label(ctx, c, body, y + 8.0, "Popular");
            let rows = self.gather_tracks(&top_ids);
            y = self.draw_track_table(
                ctx,
                c,
                body,
                y,
                &TrackTable::new(&rows, &top_ids).without_album(),
            );
            y += 12.0;
        }

        y = self.draw_artist_detail_discography(ctx, c, body, y, page);
        y = self.draw_artist_detail_related(ctx, c, body, y, page);
        Self::draw_artist_detail_bio(ctx, c, body, y, page)
    }

    /// Discography, sectioned by release type. Full albums first, then the
    /// shorter Singles & EPs — the way listeners expect a discography to read.
    fn draw_artist_detail_discography(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
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
            y = Self::section_label(ctx, c, body, y, "Albums");
            y = self.draw_album_grid(ctx, c, body, y, &albums);
            y += 12.0;
        }
        if !singles.is_empty() {
            y = Self::section_label(ctx, c, body, y, "Singles & EPs");
            y = self.draw_album_grid(ctx, c, body, y, &singles);
            y += 12.0;
        }
        y
    }

    /// Related / similar artists ("Fans also like").
    fn draw_artist_detail_related(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.similar.is_empty() {
            return y;
        }
        y = Self::section_label(ctx, c, body, y, "Fans also like");
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
                    "Artist".into()
                },
                listeners: a.subscriber_count,
            })
            .collect();
        y = self.draw_artist_grid(ctx, c, body, y, &related);
        y + 12.0
    }

    /// About / bio: word-wrapped (the text renderer clips at `max_w`, so this
    /// wraps manually to show the whole bio across several lines), capped at
    /// a readable number of lines, with an optional Wikipedia link.
    fn draw_artist_detail_bio(
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.description.is_empty() {
            return y;
        }
        let pad = 28.0;
        y = Self::section_label(ctx, c, body, y, "About");
        let maxw = (body.width() - 2.0 * pad).min(640.0);
        let mut line = String::new();
        let mut lines_drawn = 0;
        for word in page.description.split_whitespace() {
            if lines_drawn >= 8 {
                break;
            }
            let cand = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if f64::from(
                ctx.text
                    .measure_weighted(&cand, 13.0, None, TextWeight::Regular)
                    .0,
            ) > maxw
                && !line.is_empty()
            {
                ctx.text(
                    &line,
                    &TextStyle::new(13.0),
                    c.dim,
                    Point::new(body.x0 + pad, y),
                    None,
                );
                y += 20.0;
                lines_drawn += 1;
                line = word.to_string();
            } else {
                line = cand;
            }
        }
        if !line.is_empty() && lines_drawn < 8 {
            ctx.text(
                &line,
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y),
                None,
            );
            y += 20.0;
        }
        if !page.wikipedia_url.is_empty() {
            y += 4.0;
            ctx.text(
                "Read more on Wikipedia",
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.accent,
                Point::new(body.x0 + pad, y),
                None,
            );
            y += 22.0;
        }
        y
    }

    fn draw_playlist_detail(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
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
                let bottom = self.draw_playlist_detail_live(ctx, c, body, y, &page);
                self.playlist_pages.insert(id.to_string(), page);
                return bottom;
            }
            if self.failed_pages.contains(id) {
                return self.draw_detail_error(ctx, c, body, y, "playlist");
            }
            return Self::draw_loading_detail(ctx, c, body, y, "PLAYLIST", id);
        }

        let (name, desc, ids) = {
            let p = self.lib.playlist(id);
            let name = p.map(|p| p.name.clone()).unwrap_or_default();
            let desc = p.map(|p| p.description.clone()).unwrap_or_default();
            let ids = p.map(|p| p.track_ids.clone()).unwrap_or_default();
            (name, desc, ids)
        };
        let total: u32 = self.gather_tracks(&ids).iter().map(|r| r.dur).sum();
        let subtitle = format!(
            "{} · {} songs · {}",
            desc,
            ids.len(),
            ui::format::timecode(u64::from(total))
        );
        let mut y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id,
                kind: DetailKind::Playlist,
                title: &name,
                subtitle: &subtitle,
                queue: ids.clone(),
            },
        );

        // Playlist management row: an outlined pill per action.
        let pad = 28.0;
        self.draw_playlist_management_row(ctx, c, body, y - 8.0, id);
        y += 36.0;

        // A Ctrl+K membership hit names a specific row, not merely its owning
        // playlist. Move it into view on the next paint (the current paint has
        // already established its geometry), and keep the row visibly marked.
        if let Some((playlist, track)) = &self.revealed_membership
            && playlist == id
        {
            if let Some(index) = ids.iter().position(|candidate| candidate == track) {
                let row_document_y = y + self.scroll + index as f64 * 52.0;
                let content_document_end = y + self.scroll + ids.len() as f64 * 52.0 + 24.0;
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
            ctx.text(
                "This playlist is empty. Right-click a song \u{2192} Add to playlist\u{2026}",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y + 6.0),
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        // Unified row look (equalizer + like + download + duration); the playlist
        // context adds reorder/remove affordances and binds the row menu.
        self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_album().in_playlist(id),
        )
    }

    /// Playlist management row: set cover, rename, and delete. Offline batch
    /// download is intentionally absent until a byte-backed cache exists.
    fn draw_playlist_management_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        py: f64,
        id: &str,
    ) {
        let pad = 28.0;
        let mut mx = body.x0 + pad;
        for (label, w, action, danger) in [
            (
                "Set cover\u{2026}",
                104.0,
                Action::PickPlaylistCover(id.to_string()),
                false,
            ),
            (
                "Rename",
                88.0,
                Action::BeginRenamePlaylist(id.to_string()),
                false,
            ),
            ("Delete", 84.0, Action::DeletePlaylist(id.to_string()), true),
        ] {
            let r = Rect::new(mx, py, mx + w, py + 32.0);
            let hov = self.push_hit(r, action);
            let ink = if hov {
                if danger { c.accent } else { c.text }
            } else {
                c.dim
            };
            surface::card(
                ctx.scene,
                ctx.transform,
                r,
                16.0,
                if hov { c.white10 } else { c.card },
            );
            ctx.text(
                label,
                &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                ink,
                Point::new(r.x0 + 16.0, vy(r.y0, 32.0, 13.0)),
                None,
            );
            mx = r.x1 + SIBLING_GAP;
        }
    }

    /// A live (YouTube-Music) playlist rendered from a real [`PlaylistPage`]:
    /// cover + owner header with Play / Shuffle, then the ordered track list.
    fn draw_playlist_detail_live(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        page: &PlaylistPage,
    ) -> f64 {
        let pad = 28.0;
        let ids: Vec<String> = page
            .tracks
            .iter()
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let y = self.draw_detail_header(
            ctx,
            c,
            body,
            y,
            DetailHeader {
                id: &page.id,
                kind: DetailKind::Playlist,
                title: &page.title,
                subtitle: &page.subtitle,
                queue: ids.clone(),
            },
        );
        let mut y = y;
        if !page.description.is_empty() {
            let line = elide(
                ctx.text,
                &page.description,
                13.0,
                TextWeight::Regular,
                (body.width() - 2.0 * pad).min(640.0),
            );
            ctx.text(
                &line,
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y),
                None,
            );
            y += 24.0;
        }
        if ids.is_empty() {
            ctx.text(
                "This playlist has no playable tracks.",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y + 6.0),
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(ctx, c, body, y, &TrackTable::new(&rows, &ids))
    }

    // ---- queue ------------------------------------------------------------

    /// A small floating card previewing the next few queued songs, shown while
    /// the pointer hovers the now-playing bar's queue button. Anchored just above
    /// the button and right-aligned to it.
    fn draw_queue_preview(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette) {
        if !self.queue_preview_active() {
            return;
        }
        let btn = self.queue_btn_rect;
        let ids = self.queue_preview_tracks();
        let rows = self.gather_tracks(&ids);

        let row_h = 42.0;
        let head_h = 26.0;
        let pad = 10.0;
        let body_rows = rows.len().max(1) as f64;
        let w = 300.0_f64.min(self.rect.width() - 16.0);
        let h = head_h + body_rows * row_h + pad;
        let x1 = (btn.x1 + 8.0).min(self.rect.x1 - 8.0);
        let x0 = (x1 - w).max(self.rect.x0 + 8.0);
        let y1 = btn.y0 - 10.0;
        let y0 = (y1 - h).max(self.rect.y0 + 8.0);
        let panel = Rect::new(x0, y0, x1, y1);

        // A floating panel: Panel role ground + border (HIG §9.5).
        ctx.fill_round(panel, 12.0, c.panel_ground);
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            c.panel_border,
            None,
            &surface::edge_outline(panel, 12.0),
        );
        // Swallow clicks landing on the card so they don't fall through to the
        // content behind it.
        self.push_hit(panel, Action::None);

        ctx.text(
            "Up next",
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(panel.x0 + 14.0, panel.y0 + 8.0),
            None,
        );
        let mut ry = panel.y0 + head_h;
        if rows.is_empty() {
            ctx.text(
                "Nothing queued",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(panel.x0 + 14.0, ry + 10.0),
                None,
            );
            return;
        }
        for tk in &rows {
            let art = Rect::new(panel.x0 + 8.0, ry + 5.0, panel.x0 + 40.0, ry + 37.0);
            self.draw_cover(ctx, art, &tk.cover_id(), &tk.title, 6.0);
            let tx = art.x1 + 10.0;
            let maxw = panel.x1 - tx - 12.0;
            let title = elide(ctx.text, &tk.title, 13.0, TextWeight::Medium, maxw);
            let artist = elide(ctx.text, &tk.artist, 11.0, TextWeight::Regular, maxw);
            ctx.text(
                &title,
                &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                c.text,
                Point::new(tx, ry + 7.0),
                None,
            );
            ctx.text(
                &artist,
                &TextStyle::new(11.0),
                c.dim,
                Point::new(tx, ry + 24.0),
                None,
            );
            ry += row_h;
        }
    }

    fn draw_queue(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        y += 20.0;
        y = self.draw_queue_now_playing(ctx, c, body, y);
        // Up next.
        let upnext: Vec<String> = self
            .pb
            .queue
            .iter()
            .skip(self.pb.queue_index + 1)
            .cloned()
            .collect();
        ctx.text(
            &format!("Next up · {} tracks", upnext.len()),
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(body.x0 + pad, y),
            None,
        );
        y += 28.0;
        let rows = self.gather_tracks(&upnext);
        // Build a queue context: index offset by current+1.
        let full = self.pb.queue.clone();
        let offset = self.pb.queue_index + 1;
        let slice = QueueSlice {
            rows: &rows,
            queue: &full,
            offset,
        };
        let y2 = self.draw_track_rows_with_offset(ctx, c, body, y, &slice);
        if upnext.is_empty() {
            ctx.text(
                "Queue is empty — play an album or playlist.",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y + 6.0),
                None,
            );
            return y + 40.0;
        }
        y2
    }

    /// The "Now playing" card above the up-next list. Returns the y below it
    /// (unchanged if nothing is playing).
    fn draw_queue_now_playing(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        let Some(cur) = self.pb.current.clone() else {
            return y;
        };
        ctx.text(
            "Now playing",
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.dim,
            Point::new(body.x0 + pad, y),
            None,
        );
        y += 28.0;
        let row = self.gather_tracks(std::slice::from_ref(&cur));
        let Some(tk) = row.first() else {
            return y;
        };
        let rr = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
        surface::card(ctx.scene, ctx.transform, rr, 10.0, c.accent_wash);
        self.draw_cover(
            ctx,
            Rect::new(rr.x0 + 8.0, rr.y0 + 8.0, rr.x0 + 56.0, rr.y0 + 56.0),
            &tk.cover_id(),
            &tk.title,
            8.0,
        );
        ctx.text(
            &tk.title,
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.accent,
            Point::new(rr.x0 + 68.0, rr.y0 + 14.0),
            None,
        );
        ctx.text(
            &tk.artist,
            &TextStyle::new(13.0),
            c.dim,
            Point::new(rr.x0 + 68.0, rr.y0 + 36.0),
            None,
        );
        y + 80.0
    }

    fn draw_track_rows_with_offset(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        slice: &QueueSlice<'_>,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        let shared_queue = Arc::new(slice.queue.to_vec());
        for (i, tk) in slice.rows.iter().enumerate() {
            let abs = slice.offset + i; // absolute queue index
            let rr = Rect::new(left, y, right, y + row_h - ROW_GAP);
            let hovered = self.body_hit(
                rr,
                Action::Play {
                    queue: Arc::clone(&shared_queue),
                    index: abs,
                },
            );
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.push_row_menu(
                    rr,
                    Action::OpenTrackMenu {
                        track: tk.id.clone(),
                        in_playlist: None,
                        queue_index: Some(abs),
                    },
                );
            }
            if hovered {
                ctx.fill_round(rr, 8.0, c.white5);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                let row = Rect::new(left, y, right, y + row_h);
                let controls = hovered.then_some(QueuePosition {
                    index: abs,
                    first: slice.offset,
                    len: slice.queue.len(),
                });
                self.draw_queue_row_content(ctx, c, row, tk, controls);
            }
            y += row_h;
        }
        y
    }

    /// One queue row's title/artist, reorder/remove controls, and duration,
    /// in `row`. `controls` places the row in the queue and is present only
    /// while the row is hovered, when the reorder/remove controls show.
    fn draw_queue_row_content(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        row: Rect,
        tk: &TrackDisp,
        controls: Option<QueuePosition>,
    ) {
        let (left, y, row_h) = (row.x0, row.y0, row.height());
        let dur_x = row.x1 - 48.0;
        let rm_x = dur_x - 34.0;
        let dn_x = rm_x - 30.0;
        let up_x = dn_x - 30.0;
        ctx.text(
            &tk.title,
            &TextStyle::new(14.0).with_weight(TextWeight::Medium),
            c.text,
            Point::new(left + 8.0, y + 9.0),
            None,
        );
        ctx.text(
            &tk.artist,
            &TextStyle::new(12.0),
            c.dim,
            Point::new(left + 8.0, y + 28.0),
            None,
        );
        // Reorder + remove on hover.
        if let Some(QueuePosition {
            index: abs,
            first,
            len,
        }) = controls
        {
            if abs > first {
                let uc = Point::new(up_x, y + row_h / 2.0);
                let uh = self.push_hit(
                    square_around(uc, 26.0),
                    Action::MoveQueueItem {
                        from: abs,
                        to: abs - 1,
                    },
                );
                draw_chevron(ctx, uc, true, if uh { c.text } else { c.dim });
            }
            if abs + 1 < len {
                let dc = Point::new(dn_x, y + row_h / 2.0);
                let dh = self.push_hit(
                    square_around(dc, 26.0),
                    Action::MoveQueueItem {
                        from: abs,
                        to: abs + 1,
                    },
                );
                draw_chevron(ctx, dc, false, if dh { c.text } else { c.dim });
            }
            let rc = Point::new(rm_x, y + row_h / 2.0);
            let rh = self.push_hit(square_around(rc, 26.0), Action::RemoveFromQueue(abs));
            draw_x(ctx, rc, if rh { c.accent } else { c.dim });
        }
        let dur = ui::format::timecode(u64::from(tk.dur));
        // Same tabular metric as the track table so the queue's durations line
        // up digit for digit.
        let slot = Rect::new(dur_x, y, dur_x + 40.0, y + row_h);
        draw_duration_right_aligned(ctx, &dur, 13.0, c.dim, slot);
    }

    // ---- downloads --------------------------------------------------------

    fn draw_downloads(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        y += 16.0;
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
        let banner = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
        surface::card(ctx.scene, ctx.transform, banner, 12.0, c.card);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Download,
            Point::new(banner.x0 + 34.0, banner.center().y),
            24.0,
            c.success,
        );
        ctx.text(
            &format!("{cached} available offline · {active} downloading"),
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.text,
            Point::new(banner.x0 + 60.0, banner.y0 + 14.0),
            None,
        );
        ctx.text(if body.width() < 520.0 {
                "Stored only on this device."
            } else {
                "Downloaded music is stored only on this device and plays without a network connection."
            }, &TextStyle::new(12.0), c.dim, Point::new(banner.x0 + 60.0, banner.y0 + 36.0), None);
        y += 84.0;

        let ids = self
            .lib
            .tracks
            .keys()
            .filter(|id| !matches!(self.lib.cache_state(id), CacheState::Streamed))
            .cloned()
            .collect::<Vec<_>>();
        let mut ids = ids;
        for track_id in self.offline_errors.keys() {
            if !ids.contains(track_id) {
                ids.push(track_id.clone());
            }
        }
        if ids.is_empty() {
            ctx.text(
                "No offline music yet. Use Keep offline on a song to download it.",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y),
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(
            ctx,
            c,
            body,
            y,
            &TrackTable::new(&rows, &ids).without_numbers(),
        )
    }

    // ---- liked songs ------------------------------------------------------

    fn draw_liked(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        let ids = self.lib.liked_ids();
        let total_s: u64 = ids
            .iter()
            .filter_map(|id| self.lib.track(id))
            .map(|tk| u64::from(tk.duration_s))
            .sum();

        y = self.draw_liked_banner(ctx, c, body, y, &ids, total_s);

        if ids.is_empty() {
            ctx.text(
                "No liked songs yet. Tap the heart on any track.",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y),
                None,
            );
            return y + 40.0;
        }
        y = Self::draw_liked_download_all(ctx, c, body, y);
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(ctx, c, body, y, &TrackTable::new(&rows, &ids))
    }

    /// Banner with a heart sigil, title, song count/total time, and (when
    /// non-empty) a play-all FAB. Returns the y just below the banner.
    fn draw_liked_banner(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        ids: &[String],
        total_s: u64,
    ) -> f64 {
        let pad = 28.0;
        let banner = Rect::new(body.x0 + pad, y + 16.0, body.x1 - pad, y + 16.0 + 88.0);
        // A gradient-ish two-tone block (reuse album-art palette of the heart).
        let (base, accent) = art::art_colors("liked-songs");
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &rrect(banner, 14.0),
        );
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, base, None, &banner);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            accent.multiply_alpha(0.7),
            None,
            &Circle::new(Point::new(banner.x1 - 40.0, banner.y1), 80.0),
        );
        ctx.scene.pop_layer();
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::HeartFilled,
            Point::new(banner.x0 + 50.0, banner.center().y),
            44.0,
            Color::WHITE,
        );
        ctx.text(
            "Liked Songs",
            &TextStyle::new(24.0).with_weight(TextWeight::Bold),
            Color::WHITE,
            Point::new(banner.x0 + 92.0, banner.y0 + 22.0),
            None,
        );
        ctx.text(
            &format!("{} songs · {}", ids.len(), ui::format::timecode(total_s)),
            &TextStyle::new(14.0),
            Color::WHITE.multiply_alpha(0.85),
            Point::new(banner.x0 + 92.0, banner.y0 + 54.0),
            None,
        );
        // Play-all FAB.
        if !ids.is_empty() {
            let fab = Circle::new(Point::new(banner.x1 - 44.0, banner.center().y), 22.0);
            let ph = self.push_hit(
                square_around(fab.center, 50.0),
                Action::Play {
                    queue: ids.to_vec().into(),
                    index: 0,
                },
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                if ph { c.accent_hover } else { c.accent },
                None,
                &fab,
            );
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(fab.center.x + 1.0, fab.center.y),
                18.0,
                c.bg,
            );
        }
        y + 16.0 + 88.0 + 16.0
    }

    /// A truthful hint: each row exposes Keep offline. Batch downloads remain
    /// absent until they can share the same cancellable lifecycle contract.
    /// Returns the y below it.
    fn draw_liked_download_all(ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, y: f64) -> f64 {
        let pad = 28.0;
        let dl = Rect::new(body.x0 + pad, y - 4.0, body.x0 + pad + 252.0, y + 28.0);
        surface::card(ctx.scene, ctx.transform, dl, 16.0, c.card);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Download,
            Point::new(dl.x0 + 20.0, dl.center().y),
            15.0,
            c.dim.multiply_alpha(0.6),
        );
        ctx.text(
            "Use Keep offline on a song",
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.dim,
            Point::new(dl.x0 + 36.0, vy(dl.y0, 32.0, 13.0)),
            None,
        );
        y + 44.0
    }

    // ---- lyrics -----------------------------------------------------------

    fn draw_lyrics(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, y: f64) -> f64 {
        let _ = y; // lyrics own the whole body: laid out + auto-scrolled here.
        let pad = 28.0;
        // Resolve lyrics for the current track (cached by id).
        self.ensure_lyrics();
        // Fill the body (no wheel scroll — the active line auto-follows below).
        let bottom = body.y1 - 24.0;

        let top = body.y0 + 22.0;
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        let Some(tk) = cur else {
            Self::draw_lyrics_no_track(ctx, c, body);
            return bottom;
        };

        self.draw_lyrics_synced_badge(ctx, c, body, top);

        let band_top = top + 70.0;
        let band_bot = bottom;
        let layout = lyrics_layout(body, band_top, band_bot);
        match layout.art {
            Some(art) => self.draw_lyrics_now_playing(ctx, c, art, &tk),
            None => Self::draw_lyrics_title(ctx, c, Point::new(body.x0 + pad, top), &tk),
        }
        let band = Rect::new(layout.lyrics.x0, band_top, layout.lyrics.x1, band_bot);

        if self.lyrics.is_empty() {
            self.draw_lyrics_empty(ctx, c, band);
            return bottom;
        }

        self.draw_lyrics_lines(ctx, c, band);
        bottom
    }

    /// "Play a track to see its lyrics." — no track is playing at all.
    fn draw_lyrics_no_track(ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect) {
        let msg = "Play a track to see its lyrics.";
        let (tw, _) = ctx
            .text
            .measure_weighted(msg, 15.0, None, TextWeight::Regular);
        ctx.text(
            msg,
            &TextStyle::new(15.0),
            c.dim,
            Point::new(
                body.x0 + (body.width() - f64::from(tw)) / 2.0,
                body.center().y,
            ),
            None,
        );
    }

    /// Time-synced badge (top-right), shown once the current track has
    /// synced lyrics.
    fn draw_lyrics_synced_badge(&self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, top: f64) {
        if !self.lyrics_synced || self.lyrics.is_empty() {
            return;
        }
        let pad = 28.0;
        let tag = "SYNCED";
        let (tw, _) = ctx
            .text
            .measure_weighted(tag, 10.0, None, TextWeight::Semibold);
        let pill = Rect::new(
            body.x1 - pad - f64::from(tw) - 20.0,
            top - 2.0,
            body.x1 - pad,
            top + 18.0,
        );
        surface::card(ctx.scene, ctx.transform, pill, 9.0, c.accent_wash);
        ctx.text(
            tag,
            &TextStyle::new(10.0).with_weight(TextWeight::Semibold),
            c.accent,
            Point::new(pill.x0 + 10.0, vy(pill.y0, 20.0, 10.0)),
            None,
        );
    }

    /// Two-column composition on wide bodies: a now-playing card (cover art +
    /// track info) fills the left column and the lyric lines centre within
    /// the right column — so the view is balanced instead of a centred column
    /// with a large empty left half. This draws that card around `art`.
    fn draw_lyrics_now_playing(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        art: Rect,
        tk: &TrackDisp,
    ) {
        let cover = self.best_cover_id(&tk.id, &tk.album_id);
        self.draw_cover(ctx, art, &cover, &tk.title, 18.0);
        let cx = art.center().x;
        let (tw, _) = ctx
            .text
            .measure_weighted(&tk.title, 20.0, None, TextWeight::Bold);
        ctx.text(
            &tk.title,
            &TextStyle::new(20.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(cx - f64::from(tw) / 2.0, art.y1 + 30.0),
            None,
        );
        let (aw, _) = ctx
            .text
            .measure_weighted(&tk.artist, 14.0, None, TextWeight::Regular);
        ctx.text(
            &tk.artist,
            &TextStyle::new(14.0),
            c.dim,
            Point::new(cx - f64::from(aw) / 2.0, art.y1 + 56.0),
            None,
        );
    }

    /// Narrow bodies keep one full-width centred column with the track title
    /// top-left at `origin`.
    fn draw_lyrics_title(ctx: &mut PaintCtx<'_>, c: &Palette, origin: Point, tk: &TrackDisp) {
        ctx.text(
            &tk.title,
            &TextStyle::new(22.0).with_weight(TextWeight::Bold),
            c.text,
            origin,
            None,
        );
        ctx.text(
            &tk.artist,
            &TextStyle::new(14.0),
            c.dim,
            Point::new(origin.x, origin.y + 30.0),
            None,
        );
    }

    /// Clean "no lyrics" empty state, centred in the lyrics `band` — NO
    /// dev/compile-feature text.
    fn draw_lyrics_empty(&self, ctx: &mut PaintCtx<'_>, c: &Palette, band: Rect) {
        let msg = if self.lyrics_pending_for.is_some() {
            "Finding lyrics\u{2026}"
        } else {
            "No lyrics found for this track."
        };
        let (tw, _) = ctx
            .text
            .measure_weighted(msg, 15.0, None, TextWeight::Regular);
        ctx.text(
            msg,
            &TextStyle::new(15.0),
            c.dim,
            Point::new(
                band.x0 + (band.width() - f64::from(tw)) / 2.0,
                band.center().y,
            ),
            None,
        );
    }

    /// The scrolling, auto-synced lyric lines in the lyrics column's `band`,
    /// clipped to it (plus a little headroom for the first line).
    fn draw_lyrics_lines(&self, ctx: &mut PaintCtx<'_>, c: &Palette, band: Rect) {
        let pad = 28.0;
        let (band_top, band_bot) = (band.y0, band.y1);
        // The synced current line (last line whose timestamp <= playhead).
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

        let line_h = 34.0;
        let line_count = self.lyrics.len();
        let band_h = (band_bot - band_top).max(1.0);
        // Auto-scroll so the active line sits ~42% down the band and moves in
        // time with playback; plain (unsynced) lyrics anchor to the top.
        let focus = current.unwrap_or(0);
        let auto = if self.lyrics_synced && current.is_some() {
            let want = focus as f64 * line_h - band_h * 0.42;
            want.clamp(0.0, ((line_count as f64 * line_h) - band_h).max(0.0))
        } else {
            0.0
        };

        // Clip the scrolling band to the lyrics column so lines never spill onto
        // the header / card / bar.
        let clip = Rect::new(band.x0, band_top - 6.0, band.x1, band_bot);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        let maxw = (band.width() - 2.0 * pad).clamp(1.0, 620.0);
        for (i, line) in self.lyrics.iter().enumerate() {
            let ly = band_top + i as f64 * line_h - auto;
            if ly < band_top - line_h || ly > band_bot {
                continue;
            }
            let active = Some(i) == current;
            let dist = (i as i64 - focus as i64).unsigned_abs() as f32;
            let (col, size, weight) = if active {
                (c.accent, 21.0_f32, TextWeight::Bold)
            } else if self.lyrics_synced {
                // Fade neighbouring lines out with distance from the current one.
                let alpha = (0.92 - dist * 0.14).clamp(0.30, 0.92);
                (c.text.multiply_alpha(alpha), 16.0, TextWeight::Medium)
            } else {
                (c.text.multiply_alpha(0.86), 16.0, TextWeight::Medium)
            };
            let text = if line.text.trim().is_empty() {
                "\u{266a}"
            } else {
                line.text.as_str()
            };
            let (tw, th) = ctx
                .text
                .measure_weighted(text, size, Some(maxw as f32), weight);
            let x = band.x0 + (band.width() - f64::from(tw)) / 2.0;
            // Centre each line in its slot on the measured line height: the
            // 21 px active line is taller than its 16 px neighbours, and
            // top-anchoring it pushed its glyphs into the line below.
            let y = ly + (line_h - f64::from(th)) / 2.0;
            ctx.text(
                text,
                &TextStyle::new(size).with_weight(weight),
                col,
                Point::new(x, y),
                Some(maxw as f32),
            );
        }
        ctx.scene.pop_layer();
    }

    // ---- search -----------------------------------------------------------

    fn draw_search(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let q = self.search_input.value().to_string();
        if q.trim().is_empty() {
            return self.draw_search_browse_all(ctx, c, body, y);
        }

        let searching = self.live_search_pending.as_deref() == Some(q.trim());
        let results = self.gather_search_results(q.trim());

        if let Some((failed_query, error)) = &self.search_error
            && failed_query == q.trim()
        {
            let message = elide(
                ctx.text,
                error,
                12.0,
                TextWeight::Regular,
                body.width() - 210.0,
            );
            let banner = Rect::new(body.x0 + 28.0, y + 8.0, body.x1 - 28.0, y + 48.0);
            surface::card(ctx.scene, ctx.transform, banner, 10.0, c.accent_wash);
            ctx.text(
                &format!("Couldn’t search live music: {message}"),
                &TextStyle::new(12.0),
                c.text,
                Point::new(banner.x0 + 12.0, vy(banner.y0, 40.0, 12.0)),
                Some((banner.width() - 118.0) as f32),
            );
            let retry = Rect::new(
                banner.x1 - 92.0,
                banner.y0 + 5.0,
                banner.x1 - 6.0,
                banner.y1 - 5.0,
            );
            let hover = self.push_hit(retry, Action::RetrySearch);
            ctx.fill_round(retry, 15.0, if hover { c.accent_hover } else { c.accent });
            ctx.text(
                "Try again",
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.bg,
                Point::new(retry.x0 + 12.0, vy(retry.y0, retry.height(), 12.0)),
                None,
            );
            y += 56.0;
        }

        if results.is_empty() {
            return self.draw_search_no_results(ctx, c, body, y, q.trim(), searching);
        }

        // Spotify-style filter tabs.
        y = self.draw_search_tabs(ctx, c, body, y + 6.0, &results);
        y += 6.0;

        match self.search_tab {
            SearchTab::All => self.draw_search_all(ctx, c, body, y, &results, searching),
            SearchTab::Songs => {
                if results.songs.is_empty() {
                    return self.draw_tab_empty(ctx, c, body, y, searching, "songs");
                }
                let rows = self.gather_tracks(&results.songs);
                self.draw_track_table(
                    ctx,
                    c,
                    body,
                    y + 4.0,
                    &TrackTable::new(&rows, &results.songs),
                )
            }
            SearchTab::Albums => {
                if results.albums.is_empty() {
                    return self.draw_tab_empty(ctx, c, body, y, searching, "albums");
                }
                self.draw_album_grid(ctx, c, body, y + 4.0, &results.albums)
            }
            SearchTab::Artists => {
                if results.artists.is_empty() {
                    return self.draw_tab_empty(ctx, c, body, y, searching, "artists");
                }
                self.draw_artist_grid(ctx, c, body, y + 4.0, &results.artists)
            }
            SearchTab::Playlists => {
                if results.playlists.is_empty() {
                    return self.draw_tab_empty(ctx, c, body, y, searching, "playlists");
                }
                self.draw_result_playlist_grid(ctx, c, body, y + 4.0, &results.playlists)
            }
        }
    }

    /// The empty-query "Browse all" state: a genre-tile grid derived from the
    /// library's artists.
    fn draw_search_browse_all(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        y += 16.0;
        ctx.text(
            "Browse all",
            &TextStyle::new(19.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(body.x0 + pad, y),
            None,
        );
        y += 36.0;
        // Genre tiles derived from artists' genres.
        let mut genres: Vec<String> = self.lib.artists.values().map(|a| a.genre.clone()).collect();
        genres.sort();
        genres.dedup();
        let gap = 16.0;
        let cols = 3usize;
        let tile_w = (body.width() - 2.0 * pad - gap * (cols as f64 - 1.0)) / cols as f64;
        let tile_h = 96.0;
        for (i, g) in genres.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (tile_w + gap);
            let cy = y + (i / cols) as f64 * (tile_h + gap);
            let tile = Rect::new(cx, cy, cx + tile_w, cy + tile_h);
            // The chip is clickable: a tap filters Search to this genre.
            let hover = self.push_hit(tile, Action::BrowseGenre(g.clone()));
            let (base, accent) = art::art_colors(g);
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                ctx.transform,
                &rrect(tile, 12.0),
            );
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, base, None, &tile);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                accent.multiply_alpha(0.7),
                None,
                &Circle::new(Point::new(tile.x1 - 16.0, tile.y1 - 6.0), 42.0),
            );
            // Lift the tile on hover so the affordance reads as interactive.
            if hover {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    Color::WHITE.multiply_alpha(0.12),
                    None,
                    &tile,
                );
            }
            ctx.scene.pop_layer();
            ctx.text(
                g,
                &TextStyle::new(16.0).with_weight(TextWeight::Bold),
                Color::WHITE,
                Point::new(cx + 16.0, cy + 14.0),
                Some((tile_w - 24.0) as f32),
            );
        }
        let rows = genres.len().div_ceil(cols).max(1);
        y + rows as f64 * (tile_h + gap)
    }

    /// "Searching…" / "No results" state, with a hint about live search
    /// availability.
    fn draw_search_no_results(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        q: &str,
        searching: bool,
    ) -> f64 {
        let pad = 28.0;
        y += 24.0;
        let msg = if searching {
            format!("Searching YouTube Music for \u{201c}{q}\u{201d}\u{2026}")
        } else {
            format!("No results for \u{201c}{q}\u{201d}")
        };
        ctx.text(
            &msg,
            &TextStyle::new(15.0),
            c.dim,
            Point::new(body.x0 + pad, y),
            None,
        );
        let hint = if self.backend_live {
            "Press Enter to search YouTube Music"
        } else if searching {
            ""
        } else {
            "Streaming search is unavailable here — your saved library is still searchable."
        };
        if !hint.is_empty() {
            ctx.text(
                hint,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(body.x0 + pad, y + 24.0),
                None,
            );
        }
        y + 56.0
    }

    /// Build the unified search-result model for `q` from the live multi-type
    /// bundle when it matches the query, else from the local sample library
    /// (so offline / `--shots` still surface songs, albums, artists & playlists).
    fn gather_search_results(&mut self, q: &str) -> SearchResultsView {
        if let Some(view) = self.gather_search_results_live(q) {
            return view;
        }
        self.gather_search_results_local(q)
    }

    /// The live multi-type bundle, when it's for this exact query.
    fn gather_search_results_live(&self, q: &str) -> Option<SearchResultsView> {
        let (bq, bundle) = self.live_bundle.as_ref()?;
        if bq.trim() != q {
            return None;
        }
        let songs: Vec<String> = bundle
            .tracks
            .iter()
            .map(|h| format!("yt:{}", h.video_id))
            .collect();
        let albums: Vec<AlbumDisp> = bundle
            .albums
            .iter()
            .map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: a.artist.clone(),
                year: a.year,
                queue: Vec::new(),
            })
            .collect();
        let artists: Vec<ArtistDisp> = bundle
            .artists
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
                    "Artist".into()
                },
                listeners: a.subscriber_count,
            })
            .collect();
        let playlists: Vec<ResultPlaylist> = bundle
            .playlists
            .iter()
            .map(|p| ResultPlaylist {
                id: p.id.clone(),
                title: p.title.clone(),
                subtitle: p.subtitle.clone(),
            })
            .collect();
        let top = bundle.top.as_ref().map(Self::hero_of);
        Some(SearchResultsView {
            songs,
            albums,
            artists,
            playlists,
            top,
            live: true,
        })
    }

    /// The sample library search (tracks/albums/artists) plus any local
    /// playlists whose name matches — the fallback when there's no matching
    /// live bundle.
    fn gather_search_results_local(&mut self, q: &str) -> SearchResultsView {
        let res = self.local_search_cached(q);
        let songs = res.tracks.clone();
        let albums: Vec<AlbumDisp> = res
            .albums
            .iter()
            .filter_map(|id| self.lib.album(id))
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
        let artists: Vec<ArtistDisp> = res
            .artists
            .iter()
            .filter_map(|id| self.lib.artist(id))
            .map(|a| ArtistDisp {
                id: a.id.clone(),
                name: a.name.clone(),
                genre: a.genre.clone(),
                listeners: a.monthly_listeners,
            })
            .collect();
        let needle = q.to_lowercase();
        let playlists: Vec<ResultPlaylist> = self
            .lib
            .playlists
            .iter()
            .filter(|p| p.name.to_lowercase().contains(&needle))
            .map(|p| ResultPlaylist {
                id: p.id.clone(),
                title: p.name.clone(),
                subtitle: format!("{} songs", p.track_ids.len()),
            })
            .collect();
        // Top result: the first artist, else first album, else first song.
        let top = artists
            .first()
            .map(|a| SearchHero {
                id: a.id.clone(),
                title: a.name.clone(),
                subtitle: a.genre.clone(),
                kind_label: "Artist",
                circular: true,
                playable: false,
                action: Action::Nav(View::Artist(a.id.clone())),
            })
            .or_else(|| {
                albums.first().map(|a| SearchHero {
                    id: a.id.clone(),
                    title: a.title.clone(),
                    subtitle: format!("Album \u{00b7} {}", a.artist),
                    kind_label: "Album",
                    circular: false,
                    playable: false,
                    action: Action::Nav(View::Album(a.id.clone())),
                })
            });
        SearchResultsView {
            songs,
            albums,
            artists,
            playlists,
            top,
            live: false,
        }
    }

    /// Turn a live [`SearchTop`] into a renderable hero.
    fn hero_of(top: &crate::backend::SearchTop) -> SearchHero {
        use crate::backend::SearchTop;
        match top {
            SearchTop::Track(h) => {
                let id = format!("yt:{}", h.video_id);
                SearchHero {
                    title: h.title.clone(),
                    subtitle: format!("Song \u{00b7} {}", h.artist),
                    kind_label: "Song",
                    circular: false,
                    playable: true,
                    action: Action::Play {
                        queue: vec![id.clone()].into(),
                        index: 0,
                    },
                    id,
                }
            }
            SearchTop::Album(a) => SearchHero {
                id: a.id.clone(),
                title: a.title.clone(),
                subtitle: format!("Album \u{00b7} {}", a.artist),
                kind_label: "Album",
                circular: false,
                playable: false,
                action: Action::Nav(View::Album(a.id.clone())),
            },
            SearchTop::Artist(a) => SearchHero {
                id: a.id.clone(),
                title: a.name.clone(),
                subtitle: if a.subscriber_count > 0 {
                    format!(
                        "Artist \u{00b7} {} subscribers",
                        ui::format::compact_count(a.subscriber_count)
                    )
                } else {
                    "Artist".into()
                },
                kind_label: "Artist",
                circular: true,
                playable: false,
                action: Action::Nav(View::Artist(a.id.clone())),
            },
            SearchTop::Playlist(p) => SearchHero {
                id: p.id.clone(),
                title: p.title.clone(),
                subtitle: format!("Playlist \u{00b7} {}", p.subtitle),
                kind_label: "Playlist",
                circular: false,
                playable: false,
                action: Action::Nav(View::Playlist(p.id.clone())),
            },
        }
    }

    /// The row of filter tabs (All · Songs · Albums · Artists · Playlists). Only
    /// tabs that have (or can fetch) results are shown; the active tab is filled.
    fn draw_search_tabs(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        res: &SearchResultsView,
    ) -> f64 {
        let pad = 28.0;
        let mut tab_x = body.x0 + pad;
        let tab_h = 32.0;
        for tab in SearchTab::ALL {
            // The type tabs only appear when the live backend can fetch that
            // type on demand, or the mix already surfaced some of it.
            let has = match tab {
                SearchTab::All => true,
                SearchTab::Songs => !res.songs.is_empty() || res.live,
                SearchTab::Albums => !res.albums.is_empty() || res.live,
                SearchTab::Artists => !res.artists.is_empty() || res.live,
                SearchTab::Playlists => !res.playlists.is_empty() || res.live,
            };
            if !has {
                continue;
            }
            let label = tab.label();
            let (tw, _) = ctx
                .text
                .measure_weighted(label, 13.0, None, TextWeight::Semibold);
            let tab_w = f64::from(tw) + 30.0;
            let pill = Rect::new(tab_x, y, tab_x + tab_w, y + tab_h);
            let active = self.search_tab == tab;
            let hov = self.push_hit(pill, Action::SelectSearchTab(tab));
            if active {
                ctx.fill_round(pill, tab_h / 2.0, c.text);
            } else {
                surface::card(
                    ctx.scene,
                    ctx.transform,
                    pill,
                    tab_h / 2.0,
                    if hov { c.white10 } else { c.card },
                );
            }
            let ink = if active { c.bg } else { c.text };
            ctx.text(
                label,
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                ink,
                Point::new(tab_x + 15.0, vy(y, tab_h, 13.0)),
                None,
            );
            tab_x += tab_w + SIBLING_GAP;
        }
        y + tab_h + SIBLING_GAP
    }

    /// The curated `All` mix: a top-result hero beside the top songs, then a
    /// horizontal-ish grid each of albums, artists and playlists.
    fn draw_search_all(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        res: &SearchResultsView,
        searching: bool,
    ) -> f64 {
        y = self.draw_search_all_top(ctx, c, body, y, res, searching);
        self.draw_search_all_grids(ctx, c, body, y, res)
    }

    /// Top result hero + top songs, side by side on wide bodies (else a
    /// "Searching…" line when nothing has landed yet).
    fn draw_search_all_top(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        res: &SearchResultsView,
        searching: bool,
    ) -> f64 {
        let pad = 28.0;
        let wide = body.width() >= 720.0;
        let top_songs: Vec<String> = res.songs.iter().take(4).cloned().collect();
        if let Some(hero) = &res.top {
            y = Self::section_label(ctx, c, body, y, "Top result");
            let hero_w = if wide {
                (body.width() - 2.0 * pad) * 0.42
            } else {
                body.width() - 2.0 * pad
            };
            let hero_h = 172.0;
            let hx = body.x0 + pad;
            let hero_rect = Rect::new(hx, y, hx + hero_w, y + hero_h);
            self.draw_search_hero(ctx, c, hero_rect, hero);

            if wide && !top_songs.is_empty() {
                // Songs list to the right of the hero.
                let sx = hero_rect.x1 + 24.0;
                let song_body = Rect::new(sx - pad, body.y0, body.x1, body.y1);
                let mut sy = y - 2.0;
                ctx.text(
                    "Songs",
                    &TextStyle::new(15.0).with_weight(TextWeight::Bold),
                    c.text,
                    Point::new(sx, sy),
                    None,
                );
                sy += 28.0;
                let rows = self.gather_tracks(&top_songs);
                let slice = QueueSlice {
                    rows: &rows,
                    queue: &top_songs,
                    offset: 0,
                };
                let songs_bottom = self.draw_track_rows_with_offset(ctx, c, song_body, sy, &slice);
                // Clear the taller of the two columns so neither overlaps the
                // sections below.
                y = songs_bottom.max(y + hero_h) + 20.0;
            } else {
                y += hero_h + 20.0;
                if !top_songs.is_empty() {
                    y = Self::section_label(ctx, c, body, y, "Songs");
                    let rows = self.gather_tracks(&top_songs);
                    y = self.draw_track_table(ctx, c, body, y, &TrackTable::new(&rows, &top_songs));
                    y += 8.0;
                }
            }
        } else if !top_songs.is_empty() {
            y = Self::section_label(ctx, c, body, y, "Songs");
            let rows = self.gather_tracks(&top_songs);
            y = self.draw_track_table(ctx, c, body, y, &TrackTable::new(&rows, &top_songs));
            y += 8.0;
        } else if searching {
            ctx.text(
                "Searching YouTube Music\u{2026}",
                &TextStyle::new(13.0),
                c.dim,
                Point::new(body.x0 + pad, y + 4.0),
                None,
            );
            y += 28.0;
        }
        y
    }

    /// Albums / Artists / Playlists sections, each capped at 6 results.
    fn draw_search_all_grids(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        mut y: f64,
        res: &SearchResultsView,
    ) -> f64 {
        if !res.albums.is_empty() {
            y = Self::section_label(ctx, c, body, y + 4.0, "Albums");
            let take: Vec<AlbumDisp> = res.albums.iter().take(6).cloned().collect();
            y = self.draw_album_grid(ctx, c, body, y, &take);
            y += 8.0;
        }
        if !res.artists.is_empty() {
            y = Self::section_label(ctx, c, body, y + 4.0, "Artists");
            let take: Vec<ArtistDisp> = res.artists.iter().take(6).cloned().collect();
            y = self.draw_artist_grid(ctx, c, body, y, &take);
            y += 8.0;
        }
        if !res.playlists.is_empty() {
            y = Self::section_label(ctx, c, body, y + 4.0, "Playlists");
            let take: Vec<ResultPlaylist> = res.playlists.iter().take(6).cloned().collect();
            y = self.draw_result_playlist_grid(ctx, c, body, y, &take);
        }
        y
    }

    /// A placeholder line for a per-type tab whose results haven't landed yet.
    fn draw_tab_empty(
        &self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        searching: bool,
        kind: &str,
    ) -> f64 {
        let pad = 28.0;
        let msg = if searching || self.backend_live {
            format!("Finding {kind}\u{2026}")
        } else {
            format!("No {kind} for this search")
        };
        ctx.text(
            &msg,
            &TextStyle::new(14.0),
            c.dim,
            Point::new(body.x0 + pad, y + 12.0),
            None,
        );
        y + 44.0
    }

    /// The large "top result" card. Clicking it plays (a song) or opens the
    /// entity's page (album / artist / playlist).
    fn draw_search_hero(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        rect: Rect,
        hero: &SearchHero,
    ) {
        let hov = self.push_hit(rect, hero.action.clone());
        surface::card(
            ctx.scene,
            ctx.transform,
            rect,
            14.0,
            if hov { c.white10 } else { c.card },
        );
        let art_d = rect.height() - 56.0;
        let ax = rect.x0 + 20.0;
        let ay = rect.y0 + 20.0;
        if hero.circular {
            let ctr = Point::new(ax + art_d / 2.0, ay + art_d / 2.0);
            self.draw_avatar(ctx, c, Circle::new(ctr, art_d / 2.0), &hero.id, &hero.title);
        } else {
            self.draw_cover(
                ctx,
                Rect::new(ax, ay, ax + art_d, ay + art_d),
                &hero.id,
                &hero.title,
                10.0,
            );
        }
        let tx = ax + art_d + 18.0;
        let maxw = (rect.x1 - tx - 20.0).max(40.0);
        let title = elide(ctx.text, &hero.title, 26.0, TextWeight::Bold, maxw);
        ctx.text(
            &title,
            &TextStyle::new(26.0).with_weight(TextWeight::Bold),
            c.text,
            Point::new(tx, ay + 6.0),
            None,
        );
        let sub = elide(ctx.text, &hero.subtitle, 13.0, TextWeight::Regular, maxw);
        ctx.text(
            &sub,
            &TextStyle::new(13.0),
            c.dim,
            Point::new(tx, ay + 44.0),
            None,
        );

        // Kind pill bottom-left; a Play FAB bottom-right for a song hero.
        let pill_y = rect.y1 - 30.0;
        let (pw, _) = ctx
            .text
            .measure_weighted(hero.kind_label, 12.0, None, TextWeight::Semibold);
        let pill = Rect::new(
            rect.x0 + 20.0,
            pill_y - 4.0,
            rect.x0 + 20.0 + f64::from(pw) + 24.0,
            pill_y + 20.0,
        );
        surface::card(ctx.scene, ctx.transform, pill, 12.0, c.white10);
        ctx.text(
            hero.kind_label,
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.text,
            Point::new(pill.x0 + 12.0, vy(pill.y0, 24.0, 12.0)),
            None,
        );
        if hero.playable {
            let fab = Circle::new(Point::new(rect.x1 - 30.0, rect.y1 - 30.0), 20.0);
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, c.accent, None, &fab);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(fab.center.x + 1.0, fab.center.y),
                16.0,
                c.bg,
            );
        }
    }

    /// Grid of search-result playlists (cover + title + owner subtitle), each
    /// navigating to its (live) playlist page. No create affordance.
    fn draw_result_playlist_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        body: Rect,
        y: f64,
        pls: &[ResultPlaylist],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Playlist(p.id.clone())));
            if hover {
                ctx.fill_round(card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            self.draw_cover(ctx, art_rect, &p.id, &p.title, 12.0);
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Playlist,
                Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0),
                22.0,
                c.text.multiply_alpha(0.85),
            );
            let title = elide(ctx.text, &p.title, 14.0, TextWeight::Semibold, card_w);
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(cx, cy + card_w + 8.0),
                None,
            );
            let sub = elide(ctx.text, &p.subtitle, 12.0, TextWeight::Regular, card_w);
            ctx.text(
                &sub,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(cx, cy + card_w + 28.0),
                None,
            );
        }
        let rows = pls.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- now playing bar --------------------------------------------------

    fn draw_nowplaying(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, c.panel, None, &r);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.border,
            None,
            &Rect::new(r.x0, r.y0, r.x1, r.y0 + 1.0),
        );
        // Only the wide transport draws a queue button; clear the captured rect
        // so the compact bar never reports a stale hover target.
        self.queue_btn_rect = Rect::ZERO;

        // Narrow windows can't fit the full transport (art + title + 5 transport
        // buttons + scrubber + queue + volume) without overlap. Below this width
        // the bar collapses to art + title + prev/play/next, with a full-width
        // progress line along the top edge.
        if r.width() < 720.0 {
            self.draw_nowplaying_compact(ctx, c, r);
            return;
        }

        self.draw_nowplaying_left(ctx, c, r);

        // Right block reserved for queue + volume. As the bar narrows the
        // volume slider shrinks toward a floor, then collapses to a button that
        // opens a small popover (see below); on very narrow widths the whole
        // bar drops to the compact transport (no volume) above.
        let w = r.width();
        let show_slider = w >= 900.0;
        let slider_w = ((w - 720.0) * 0.32).clamp(58.0, 150.0);
        let right_w = if show_slider { slider_w + 118.0 } else { 108.0 };
        let center = Rect::new(r.x0 + 300.0, r.y0, r.x1 - right_w, r.y1);

        self.draw_nowplaying_transport(ctx, c, r, center);
        self.draw_nowplaying_scrubber(ctx, c, r, center);
        self.draw_nowplaying_right(ctx, c, r, right_w, show_slider);
    }

    /// Left: art + title/artist + like + lyrics affordance (or "Nothing
    /// playing" when the queue is empty).
    fn draw_nowplaying_left(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());

        let lx = r.x0 + 16.0;
        if let Some(tk) = &cur {
            let art = Rect::new(lx, r.y0 + 14.0, lx + 64.0, r.y0 + 78.0);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(ctx, art, &cover, &tk.title, 8.0);
            // Clicking the art jumps to its album.
            if !tk.album_id.is_empty() {
                self.push_hit(art, Action::Nav(View::Album(tk.album_id.clone())));
            }
            let tx = art.x1 + 14.0;
            // Reserve room on the right of the title block for the heart +
            // lyrics buttons so they never overlap the title text.
            let heart_c = Point::new(r.x0 + 300.0 - 48.0, r.y0 + 30.0);
            let lyr_c = Point::new(r.x0 + 300.0 - 22.0, r.y0 + 30.0);
            let maxw = (heart_c.x - tx - 28.0).max(40.0);
            let title = elide(ctx.text, &tk.title, 14.0, TextWeight::Semibold, maxw);
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(tx, r.y0 + 26.0),
                None,
            );
            let secondary = self.playback_error.as_deref().unwrap_or(&tk.artist);
            let artist = elide(ctx.text, secondary, 12.0, TextWeight::Regular, maxw);
            ctx.text(
                &artist,
                &TextStyle::new(12.0),
                if self.playback_error.is_some() {
                    c.accent
                } else {
                    c.dim
                },
                Point::new(tx, r.y0 + 48.0),
                None,
            );

            self.draw_nowplaying_left_shortcuts(ctx, c, &tk.id, heart_c, lyr_c);
        } else {
            ctx.text(
                "Nothing playing",
                &TextStyle::new(14.0),
                c.dim,
                Point::new(lx, vy(r.y0, NOWPLAYING_H, 14.0)),
                None,
            );
        }
    }

    /// Heart / like toggle + Lyrics shortcut, next to the now-playing title.
    fn draw_nowplaying_left_shortcuts(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        track_id: &str,
        heart_c: Point,
        lyr_c: Point,
    ) {
        if self
            .local_file
            .as_ref()
            .is_some_and(|file| file.track.id == track_id)
        {
            return;
        }
        let liked = self.lib.is_liked(track_id);
        let hh = self.push_hit(
            square_around(heart_c, 28.0),
            Action::ToggleLike(track_id.to_string()),
        );
        icon::draw(
            ctx.scene,
            ctx.transform,
            if liked {
                Icon::HeartFilled
            } else {
                Icon::Heart
            },
            heart_c,
            18.0,
            if liked {
                c.accent
            } else if hh {
                c.text
            } else {
                c.dim
            },
        );
        // Lyrics shortcut.
        let lh = self.push_hit(square_around(lyr_c, 28.0), Action::Nav(View::Lyrics));
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Note,
            lyr_c,
            18.0,
            if self.view == View::Lyrics {
                c.accent
            } else if lh {
                c.text
            } else {
                c.dim
            },
        );
    }

    /// Shuffle / prev / play-pause / next / repeat, centred within `center`.
    fn draw_nowplaying_transport(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        r: Rect,
        center: Rect,
    ) {
        let cxm = f64::midpoint(center.x0, center.x1);
        let btn_y = r.y0 + 30.0;
        let spacing = 44.0;
        // shuffle
        let shuf_c = Point::new(cxm - 2.0 * spacing, btn_y);
        let sh = self.push_hit(square_around(shuf_c, 30.0), Action::ToggleShuffle);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Shuffle,
            shuf_c,
            18.0,
            if self.pb.shuffle {
                c.accent
            } else if sh {
                c.text
            } else {
                c.dim
            },
        );
        // prev
        let prev_c = Point::new(cxm - spacing, btn_y);
        let pv = self.push_hit(square_around(prev_c, 30.0), Action::Prev);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Prev,
            prev_c,
            18.0,
            if pv { c.text } else { c.dim_strong },
        );
        // play/pause big
        let play_c = Point::new(cxm, btn_y);
        let pl = self.push_hit(square_around(play_c, 40.0), Action::TogglePlay);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            if pl {
                c.text
            } else {
                c.text.multiply_alpha(0.94)
            },
            None,
            &Circle::new(play_c, 20.0),
        );
        if self.pb.playing {
            icon::draw(ctx.scene, ctx.transform, Icon::Pause, play_c, 16.0, c.panel);
        } else {
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(play_c.x + 1.0, play_c.y),
                16.0,
                c.panel,
            );
        }
        // next
        let next_c = Point::new(cxm + spacing, btn_y);
        let nx = self.push_hit(square_around(next_c, 30.0), Action::Next);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Next,
            next_c,
            18.0,
            if nx { c.text } else { c.dim_strong },
        );
        // repeat
        let rep_c = Point::new(cxm + 2.0 * spacing, btn_y);
        let rp = self.push_hit(square_around(rep_c, 30.0), Action::CycleRepeat);
        let rep_icon = if self.pb.repeat == RepeatMode::One {
            Icon::RepeatOne
        } else {
            Icon::Repeat
        };
        let rep_col = if self.pb.repeat == RepeatMode::Off {
            if rp { c.text } else { c.dim }
        } else {
            c.accent
        };
        icon::draw(ctx.scene, ctx.transform, rep_icon, rep_c, 18.0, rep_col);
    }

    /// The elapsed/total timecodes + draggable progress track.
    fn draw_nowplaying_scrubber(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        r: Rect,
        center: Rect,
    ) {
        let dur = self.current_duration_ms();
        let pos = self.playhead_ms.min(dur as f64).max(0.0);
        let frac = if dur > 0 { pos / dur as f64 } else { 0.0 };
        let bar_y = r.y0 + 64.0;
        let elapsed = ui::format::timecode((pos / 1000.0) as u64);
        let total = ui::format::timecode(dur / 1000);
        let bar_x0 = center.x0 + 48.0;
        let bar_x1 = center.x1 - 48.0;
        // Tabular figures: the elapsed value ticks every second and would
        // otherwise shift as digit widths change. Both labels centre on the
        // track's centre line from their measured line height.
        let style = numeric_style(11.0, TextWeight::Regular);
        let (ew, eh) = ctx.text.measure_style(&elapsed, None, &style);
        let track_cy = bar_y + 2.0;
        ctx.text.draw_style(
            ctx.scene,
            &elapsed,
            c.dim,
            ctx.transform
                * Affine::translate((
                    bar_x0 - f64::from(ew) - 10.0,
                    track_cy - f64::from(eh) / 2.0,
                )),
            None,
            &style,
        );
        let (_, th) = ctx.text.measure_style(&total, None, &style);
        ctx.text.draw_style(
            ctx.scene,
            &total,
            c.dim,
            ctx.transform * Affine::translate((bar_x1 + 10.0, track_cy - f64::from(th) / 2.0)),
            None,
            &style,
        );
        let track = Rect::new(bar_x0, bar_y, bar_x1, bar_y + 4.0);
        self.scrubber_rect = Rect::new(bar_x0, bar_y - 8.0, bar_x1, bar_y + 12.0);
        ctx.fill_round(track, 2.0, c.track);
        let fillx = bar_x0 + (bar_x1 - bar_x0) * frac;
        if fillx > bar_x0 {
            ctx.fill_round(Rect::new(bar_x0, bar_y, fillx, bar_y + 4.0), 2.0, c.accent);
        }
        let knob_hover =
            self.scrubber_rect.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        if knob_hover {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                c.text,
                None,
                &Circle::new(Point::new(fillx, bar_y + 2.0), 6.0),
            );
        }
    }

    /// Right: queue + volume. The queue button toggles the queue view (and
    /// restores the prior view on toggle-off); hovering it previews the next
    /// few songs. Capture its rect so the hover overlay + the toggle target
    /// share the exact laid-out geometry.
    fn draw_nowplaying_right(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        r: Rect,
        right_w: f64,
        show_slider: bool,
    ) {
        let q_c = Point::new(r.x1 - right_w + 28.0, r.y0 + NOWPLAYING_H / 2.0);
        let q_rect = square_around(q_c, 30.0);
        self.queue_btn_rect = q_rect;
        let qh = self.push_hit(q_rect, Action::ToggleQueue);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Queue,
            q_c,
            18.0,
            if self.view == View::Queue {
                c.accent
            } else if qh {
                c.text
            } else {
                c.dim
            },
        );

        let vol_icon_c = Point::new(r.x1 - right_w + 64.0, r.y0 + NOWPLAYING_H / 2.0);
        let muted = self.pb.volume == 0;
        let vbar_y = r.y0 + NOWPLAYING_H / 2.0;
        if show_slider {
            // Inline slider (shrinks with the bar). The speaker icon is a mute
            // toggle: click to silence, click to restore.
            self.volume_btn_rect = Rect::ZERO;
            self.volume_popover_open = false;
            let mh = self.push_hit(square_around(vol_icon_c, 28.0), Action::ToggleMute);
            icon::draw(
                ctx.scene,
                ctx.transform,
                if muted {
                    Icon::VolumeMute
                } else {
                    Icon::VolumeHigh
                },
                vol_icon_c,
                18.0,
                if muted {
                    c.accent
                } else if mh {
                    c.text
                } else {
                    c.dim
                },
            );
            let vbar_x0 = vol_icon_c.x + 18.0;
            let vbar_x1 = r.x1 - 24.0;
            self.draw_volume_slider(
                ctx,
                c,
                Rect::new(vbar_x0, vbar_y - 8.0, vbar_x1, vbar_y + 8.0),
            );
        } else {
            // Collapsed: a single volume button that opens a slider popover.
            self.volume_rect = Rect::ZERO;
            let btn = square_around(vol_icon_c, 32.0);
            self.volume_btn_rect = btn;
            let bh = self.push_hit(btn, Action::ToggleVolumePopover);
            if self.volume_popover_open || bh {
                ctx.fill_round(btn, 9.0, c.white10);
            }
            let icon = if muted {
                Icon::VolumeMute
            } else {
                Icon::VolumeHigh
            };
            icon::draw(
                ctx.scene,
                ctx.transform,
                icon,
                vol_icon_c,
                18.0,
                if muted {
                    c.accent
                } else if bh || self.volume_popover_open {
                    c.text
                } else {
                    c.dim
                },
            );
        }
    }

    /// Draw a horizontal volume slider filling `hit` (its full interactive
    /// rect), registering the drag target. Shared by the inline bar slider and
    /// the collapsed-volume popover.
    fn draw_volume_slider(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, hit: Rect) {
        self.volume_rect = hit;
        let vol = f64::from(self.pb.volume) / 100.0;
        let vbar_y = hit.center().y;
        let x0 = hit.x0;
        let x1 = hit.x1;
        let vtrack = Rect::new(x0, vbar_y - 2.0, x1, vbar_y + 2.0);
        ctx.fill_round(vtrack, 2.0, c.track);
        let vfill = x0 + (x1 - x0) * vol;
        if vfill > x0 {
            ctx.fill_round(
                Rect::new(x0, vbar_y - 2.0, vfill, vbar_y + 2.0),
                2.0,
                c.accent,
            );
        }
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.text,
            None,
            &Circle::new(Point::new(vfill, vbar_y), 5.0),
        );
    }

    /// The collapsed-volume popover: a small floating panel with a horizontal
    /// slider + a mute toggle, anchored above the volume button. Drawn on top of
    /// the bar (after it) so it never clips. Uses the shared drag target so the
    /// existing scrubber/volume drag logic drives it.
    fn draw_volume_popover(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette) {
        if !self.volume_popover_open || self.volume_btn_rect.width() <= 0.0 {
            self.volume_popover_rect = Rect::ZERO;
            self.volume_popover_slider = Rect::ZERO;
            return;
        }
        let btn = self.volume_btn_rect;
        let pw = 220.0;
        let ph = 56.0;
        let x0 = (btn.center().x - pw / 2.0)
            .max(self.rect.x0 + 8.0)
            .min(self.rect.x1 - pw - 8.0);
        let y1 = btn.y0 - 10.0;
        let y0 = y1 - ph;
        let panel = Rect::new(x0, y0, x0 + pw, y1);
        self.volume_popover_rect = panel;
        // A floating panel: Panel role ground + border (HIG §9.5).
        ctx.fill_round(panel, 12.0, c.panel_ground);
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            c.panel_border,
            None,
            &surface::edge_outline(panel, 12.0),
        );
        // Block clicks inside the panel from falling through.
        self.push_hit(panel, Action::None);

        let muted = self.pb.volume == 0;
        let icon_c = Point::new(panel.x0 + 24.0, panel.center().y);
        let mh = self.push_hit(square_around(icon_c, 30.0), Action::ToggleMute);
        icon::draw(
            ctx.scene,
            ctx.transform,
            if muted {
                Icon::VolumeMute
            } else {
                Icon::VolumeHigh
            },
            icon_c,
            18.0,
            if muted {
                c.accent
            } else if mh {
                c.text
            } else {
                c.dim
            },
        );
        let pct = format!("{}", self.pb.volume);
        let (pw_txt, _) = ctx
            .text
            .measure_weighted(&pct, 12.0, None, TextWeight::Medium);
        ctx.text(
            &pct,
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.dim,
            Point::new(panel.x1 - 16.0 - f64::from(pw_txt), vy(panel.y0, ph, 12.0)),
            None,
        );
        let slider = Rect::new(
            icon_c.x + 20.0,
            panel.center().y - 8.0,
            panel.x1 - 44.0,
            panel.center().y + 8.0,
        );
        self.volume_popover_slider = slider;
        self.draw_volume_slider(ctx, c, slider);
    }

    /// Compact now-playing transport for narrow windows: art + title on the
    /// left, prev/play/next on the right, a full-width progress line (also the
    /// scrubber) along the top edge. Volume + queue are dropped (reachable from
    /// the sidebar) so nothing overlaps.
    fn draw_nowplaying_compact(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());

        self.draw_nowplaying_compact_progress(ctx, c, r);

        let controls_w = 132.0;
        self.draw_nowplaying_compact_left(ctx, c, r, controls_w, cur.as_ref());
        self.draw_nowplaying_compact_transport(ctx, c, r, controls_w);
    }

    /// Scrub / progress track: a proper rounded bar lifted off the very
    /// bottom edge so it's clearly visible and grabbable — not a hairline
    /// glued to the window edge. The hit strip still reaches the bottom edge
    /// so an edge-of-window press seeks too, while its top clears the
    /// transport buttons.
    fn draw_nowplaying_compact_progress(&mut self, ctx: &mut PaintCtx<'_>, c: &Palette, r: Rect) {
        let dur = self.current_duration_ms();
        let pos = self.playhead_ms.min(dur as f64).max(0.0);
        let frac = if dur > 0 { pos / dur as f64 } else { 0.0 };
        let inset = 14.0;
        let track_h = 6.0;
        let track_x0 = r.x0 + inset;
        let track_x1 = r.x1 - inset;
        let track_bottom = r.y1 - 9.0;
        let track_top = track_bottom - track_h;
        let track = Rect::new(track_x0, track_top, track_x1, track_bottom);
        ctx.fill_round(track, track_h / 2.0, c.track);
        let fillx = track_x0 + (track_x1 - track_x0) * frac;
        if fillx > track_x0 {
            ctx.fill_round(
                Rect::new(track_x0, track_top, fillx, track_bottom),
                track_h / 2.0,
                c.accent,
            );
        }
        // Grabbable hit strip: from just above the track down to the bottom edge.
        self.scrubber_rect = Rect::new(track_x0, track_top - 6.0, track_x1, r.y1);
        // Knob for a clear grab affordance (on hover / drag, or whenever there
        // is progress to grab).
        let sc_active =
            self.scrubber_rect.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        if sc_active || frac > 0.0 {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                c.text,
                None,
                &Circle::new(
                    Point::new(fillx, f64::midpoint(track_top, track_bottom)),
                    6.0,
                ),
            );
        }
        self.volume_rect = Rect::ZERO;
        self.volume_btn_rect = Rect::ZERO;
    }

    /// Art + title/artist (or "Nothing playing"), left of the transport.
    fn draw_nowplaying_compact_left(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        r: Rect,
        controls_w: f64,
        cur: Option<&TrackDisp>,
    ) {
        let lx = r.x0 + 14.0;
        if let Some(tk) = cur {
            let art = Rect::new(lx, r.y0 + 16.0, lx + 60.0, r.y0 + 76.0);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(ctx, art, &cover, &tk.title, 8.0);
            self.push_hit(art, Action::Nav(View::Lyrics));
            let tx = art.x1 + 12.0;
            let maxw = (r.x1 - controls_w - tx - 10.0).max(40.0);
            let title = elide(ctx.text, &tk.title, 14.0, TextWeight::Semibold, maxw);
            ctx.text(
                &title,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text,
                Point::new(tx, r.y0 + 28.0),
                None,
            );
            let artist = elide(ctx.text, &tk.artist, 12.0, TextWeight::Regular, maxw);
            ctx.text(
                &artist,
                &TextStyle::new(12.0),
                c.dim,
                Point::new(tx, r.y0 + 50.0),
                None,
            );
        } else {
            ctx.text(
                "Nothing playing",
                &TextStyle::new(14.0),
                c.dim,
                Point::new(lx, vy(r.y0, NOWPLAYING_H, 14.0)),
                None,
            );
        }
    }

    /// Prev / play-pause / next, right-aligned within `controls_w`.
    fn draw_nowplaying_compact_transport(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        c: &Palette,
        r: Rect,
        controls_w: f64,
    ) {
        let cy = r.y0 + NOWPLAYING_H / 2.0 + 2.0;
        let px = r.x1 - controls_w;
        let prev_c = Point::new(px + 18.0, cy);
        let pv = self.push_hit(square_around(prev_c, 32.0), Action::Prev);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Prev,
            prev_c,
            18.0,
            if pv { c.text } else { c.dim_strong },
        );
        let play_c = Point::new(px + 62.0, cy);
        let pl = self.push_hit(square_around(play_c, 42.0), Action::TogglePlay);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            if pl {
                c.text
            } else {
                c.text.multiply_alpha(0.94)
            },
            None,
            &Circle::new(play_c, 20.0),
        );
        if self.pb.playing {
            icon::draw(ctx.scene, ctx.transform, Icon::Pause, play_c, 16.0, c.panel);
        } else {
            icon::draw(
                ctx.scene,
                ctx.transform,
                Icon::Play,
                Point::new(play_c.x + 1.0, play_c.y),
                16.0,
                c.panel,
            );
        }
        let next_c = Point::new(px + 106.0, cy);
        let nx = self.push_hit(square_around(next_c, 32.0), Action::Next);
        icon::draw(
            ctx.scene,
            ctx.transform,
            Icon::Next,
            next_c,
            18.0,
            if nx { c.text } else { c.dim_strong },
        );
    }

    // ---- toast ------------------------------------------------------------

    /// Keep transient feedback inside the already-reserved player chrome at
    /// large text sizes. The stock bottom-centred toast is intentionally roomy
    /// at 100%, but at 200% it can obscure both the final rows and transport.
    fn draw_large_text_toast(&self, ctx: &mut PaintCtx<'_>, c: &Palette) {
        let Some((message, _)) = self.toast.as_ref() else {
            return;
        };
        let player_top = self.rect.y1 - 116.0;
        let status_area = Rect::new(
            self.rect.x0,
            player_top + 1.0,
            (self.rect.x1 - 100.0).max(self.rect.x0),
            self.rect.y1,
        );
        // The toast temporarily *replaces* the player's metadata/status area.
        // Clearing that reserved region avoids half-visible large glyphs from
        // the underlying title while leaving the transport button untouched.
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, c.panel, None, &status_area);
        let pill = Rect::new(
            self.rect.x0 + 16.0,
            player_top + 22.0,
            (self.rect.x1 - 108.0).max(self.rect.x0 + 120.0),
            player_top + 66.0,
        );
        ctx.fill_round(pill, 14.0, c.panel_ground);
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            c.border,
            None,
            &rrect(pill, 14.0),
        );
        let label = elide(
            ctx.text,
            message,
            8.0,
            TextWeight::Semibold,
            (pill.width() - 24.0).max(40.0),
        );
        ctx.text(
            &label,
            &TextStyle::new(8.0).with_weight(TextWeight::Semibold),
            c.text,
            Point::new(pill.x0 + 12.0, vy(pill.y0, pill.height(), 8.0)),
            None,
        );
    }

    fn draw_toast(&self, ctx: &mut PaintCtx<'_>) {
        let Some((msg, _)) = self.toast.as_ref() else {
            return;
        };
        let area = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1 - NOWPLAYING_H,
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
                listeners: a.monthly_listeners,
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

fn square_around(c: Point, size: f64) -> Rect {
    Rect::new(
        c.x - size / 2.0,
        c.y - size / 2.0,
        c.x + size / 2.0,
        c.y + size / 2.0,
    )
}

/// A small up/down chevron for reorder controls.
fn draw_chevron(ctx: &mut PaintCtx<'_>, center: Point, up: bool, color: Color) {
    let s = 5.0;
    let dy = if up { -1.0 } else { 1.0 };
    let mut p = vello::kurbo::BezPath::new();
    p.move_to((center.x - s, center.y + dy * s * 0.5));
    p.line_to((center.x, center.y - dy * s * 0.5));
    p.line_to((center.x + s, center.y + dy * s * 0.5));
    ctx.scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        ctx.transform,
        color,
        None,
        &p,
    );
}

/// A small "x" (remove) glyph.
fn draw_x(ctx: &mut PaintCtx<'_>, center: Point, color: Color) {
    let s = 5.0;
    let a = vello::kurbo::Line::new((center.x - s, center.y - s), (center.x + s, center.y + s));
    let b = vello::kurbo::Line::new((center.x + s, center.y - s), (center.x - s, center.y + s));
    ctx.scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        ctx.transform,
        color,
        None,
        &a,
    );
    ctx.scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        ctx.transform,
        color,
        None,
        &b,
    );
}

/// A compact checkmark for a byte-backed offline object.
fn draw_check(ctx: &mut PaintCtx<'_>, center: Point, color: Color) {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((center.x - 6.0, center.y));
    path.line_to((center.x - 1.5, center.y + 4.5));
    path.line_to((center.x + 7.0, center.y - 5.0));
    ctx.scene.stroke(
        &Stroke::new(2.0).with_caps(vello::kurbo::Cap::Round),
        ctx.transform,
        color,
        None,
        &path,
    );
}

/// Theme-derived palette resolved once per frame.
struct Palette {
    bg: Color,
    panel: Color,
    card: Color,
    /// Skeleton base: distinct from `card` on the all-black high-contrast
    /// theme, where raised and base surfaces intentionally coincide.
    skeleton: Color,
    card_hover: Color,
    text: Color,
    dim: Color,
    dim_strong: Color,
    accent: Color,
    accent_hover: Color,
    success: Color,
    /// The 1 px rule on a `surfaces.base` panel (sidebar divider, the
    /// now-playing bar's top hairline), derived against that ground so it
    /// registers on every preset. Never stroked on a raised surface — that
    /// is what [`Palette::edge`] is for (HIG §9.5).
    border: Color,
    /// The opaque "live" wash (now-playing row, synced badge, following /
    /// in-library pills): accent over the body ground, resolved so its edge
    /// can be derived against what actually reaches the screen.
    accent_wash: Color,
    success_wash: Color,
    /// Floating panel ground + border (queue preview, volume popover).
    panel_ground: Color,
    panel_border: Color,
    /// Scrubber / volume track: a hairline-strength step on the bar's base
    /// ground, so it registers in light themes where `lift` washes vanish.
    track: Color,
    white5: Color,
    white10: Color,
    scrim: Color,
    reduce_motion: bool,
}

impl Palette {
    fn from(theme: &Theme) -> Self {
        let col = &theme.tokens.colors;
        Self {
            bg: col.neutral[0],
            panel: col.surfaces.base,
            card: col.surfaces.raised,
            skeleton: if theme.name == "High Contrast" {
                col.neutral[4]
            } else {
                col.surfaces.raised
            },
            card_hover: col.neutral[3].multiply_alpha(0.6),
            text: theme.text_primary,
            dim: theme.text_dim,
            dim_strong: col.neutral[7],
            accent: theme.accent_primary,
            accent_hover: theme.accent_hover,
            success: theme.status_success,
            border: surface::edge_on(col.surfaces.base),
            accent_wash: ui::tokens::colors::over(col.neutral[0], theme.accent_primary, 0.16),
            success_wash: ui::tokens::colors::over(col.neutral[0], theme.status_success, 0.18),
            panel_ground: ui::chrome::panel_ground(theme),
            panel_border: ui::chrome::panel_border(theme),
            track: surface::edge_on(col.surfaces.base),
            white5: col.lift(0.35),
            white10: col.lift(0.72),
            scrim: col.surfaces.overlay,
            reduce_motion: theme.reduce_motion,
        }
    }
}
