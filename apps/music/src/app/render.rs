//! Drawing + event handling for [`MusicApp`].
//!
//! Split out of `app.rs` to keep the data/state logic separate from the
//! (large) rendering surface. Everything here draws straight into the Vello
//! scene and registers click `Hit`s for the event handler.

// Draw helpers take independent geometry/style parameters (rect, color, text,
// weight, …) a caller picks per call site; grouping them into a struct would
// relocate the same fields across this file's ~90+ call sites without
// reducing complexity.
#![allow(clippy::too_many_arguments)]

use super::{
    Action, Affine, AlbumPage, ArtistPage, AutoCommit, CacheState, Circle, Color, Dimension, Drag,
    Fill, HEADER_H, HashMap, HomeItem, HomeItemKind, HomeSection, Icon, Instant, MusicApp,
    NOWPLAYING_H, NodeId, ObjId, PermissionContext, PlaylistPage, Point, PresenceCtx, Rect,
    RepeatMode, Scene, SearchTab, Size, Stroke, Style, TaffyTree, TextRenderer, TextWeight, Theme,
    View, Widget, art, draw_text, elide, fill_rrect, icon, is_live_album_id, is_live_id,
    is_live_playlist_id, lyrics, rrect,
};

use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use vello::kurbo::Shape;
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
    cache: CacheState,
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

/// Per-row boolean state for [`MusicApp::draw_track_row_content`]: whether
/// this row is the current track, hovered, has room for the album column,
/// and shows a numbered (vs. blank) leading index column.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this struct IS the bool-bundling fix for draw_track_row_content's argument \
              count; its four fields are independent per-row flags, and splitting them into \
              sub-structs would not express any real invariant between them"
)]
struct TrackRowFlags {
    is_current: bool,
    hovered: bool,
    show_album: bool,
    number_col: bool,
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
    let target = left + (right - left) * 0.46;
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
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Normalised `0..=1` heights for the 3-bar now-playing equalizer. While
/// `playing` the bars bounce on out-of-phase sinusoids; paused they hold a
/// static silhouette.
fn eq_bars(playing: bool) -> [f64; 3] {
    if !playing {
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

/// A shimmering skeleton placeholder block: a rounded base with a soft
/// highlight band sweeping left→right, clipped to the block. This is the
/// loading affordance that replaces "fake title" text while real content
/// streams in.
fn draw_skeleton_block(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: &Palette) {
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    fill_rrect(scene, t, r, radius, c.card);
    // Moving highlight sweep, clipped to the block's rounded rect.
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &rrect(r, radius),
    );
    let phase = anim_phase();
    let band_w = (r.width() * 0.45).max(24.0);
    let travel = r.width() + band_w;
    let x = r.x0 - band_w + travel * phase;
    // Two overlapping bands give a soft-edged sweep without a gradient brush.
    scene.fill(
        Fill::NonZero,
        t,
        c.white5,
        None,
        &Rect::new(x, r.y0, x + band_w, r.y1),
    );
    scene.fill(
        Fill::NonZero,
        t,
        c.white10.multiply_alpha(0.7),
        None,
        &Rect::new(x + band_w * 0.28, r.y0, x + band_w * 0.72, r.y1),
    );
    scene.pop_layer();
}

impl Widget for MusicApp {
    fn continuation_metadata(
        &self,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let track_id = self.pb.current.as_ref()?;
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
        state: &mut AutoCommit,
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
        self.pb.current = Some(track_id.to_owned());
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
        crate::doc::write_playback(state, &self.pb);
        true
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.ensure_seeded(state);
        if self.playback_loaded {
            self.reload_library(state);
            // Library and playback are both shared. Reload the catalog every
            // frame, but only adopt playback when the document itself changed
            // so a peer's play/pause/seek immediately controls this device
            // without jittering the local clock between edits.
            self.reconcile_document_playback(state);
        } else {
            self.reload(state);
            self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
            self.playback_loaded = true;
        }
        // Progress any in-flight offline downloads (and re-apply their live
        // progress on top of the just-reloaded terminal cache states).
        self.advance_downloads(state);
        // External media control (media keys / desktop widget) and a completed
        // file-portal cover pick both mutate the document, so they run here
        // where the CRDT handle is available. The portal conversion completion
        // follows the pick; decode/resize work itself runs off-thread.
        self.drain_mpris(state);
        self.drain_cover_pick(state);
        self.drain_cover_embed(state);
        self.publish_mpris();
    }

    /// Publish the open library + its playlists/tracks so the host's Ctrl+K action
    /// palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its music action, so it records one undo boundary for it (see
    /// [`MusicApp::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the user's library into the unified Ctrl+K palette. The app retains
    /// one prebuilt [`LibrarySearchProvider`](crate::search_type::LibrarySearchProvider)
    /// per library revision, and this returns an O(1) shared-catalog clone;
    /// opening Ctrl+K therefore never reindexes the corpus. The provider is
    /// synchronous and offline-safe; the live YouTube-Music search is async and
    /// lives in the app's own search view.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.library_search_provider.clone())]
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::MUSIC_OPERATION_TYPE]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
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
                qos_actions::OperationKind::Effect,
                crate::actions::MusicOperation::PlayTrack { id },
            ) => {
                if !self.lib.tracks.contains_key(&id) {
                    return Err("track is no longer in the library".into());
                }
                self.pb.queue = vec![id.clone()];
                self.shuffle_order.clear();
                self.play_index(0);
                self.start_stream(&id);
            }
            (
                qos_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenAlbum { id },
            ) => {
                if !self.lib.albums.contains_key(&id) {
                    return Err("album is no longer in the library".into());
                }
                self.on_nav(View::Album(id));
            }
            (
                qos_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenArtist { id },
            ) => {
                if !self.lib.artists.contains_key(&id) {
                    return Err("artist is no longer in the library".into());
                }
                self.on_nav(View::Artist(id));
            }
            (
                qos_actions::OperationKind::View,
                crate::actions::MusicOperation::OpenPlaylist { id },
            ) => {
                if self.lib.playlist(&id).is_none() {
                    return Err("playlist is no longer in the library".into());
                }
                self.on_nav(View::Playlist(id));
            }
            (
                qos_actions::OperationKind::View,
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
                    Instant::now(),
                ));
            }
            _ => return Ok(false),
        }
        self.dirty = true;
        Ok(true)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `Widget::build_layout` returns a bare `NodeId`, not a `Result` — a
        // framework-wide contract this one widget can't change — so a
        // `new_leaf` failure (which a freshly-created tree with one valid
        // leaf style cannot produce) has no propagation path here.
        #[allow(
            clippy::expect_used,
            reason = "new_leaf on a fresh tree with a valid Style cannot fail; Widget::build_layout's Result-free signature is shared by every implementor and out of this crate's scope to change"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .expect("a leaf on a fresh taffy tree");
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // While the context menu is open it owns the pointer + keyboard: route
        // the event to it, then drain any selection into the Action pipeline.
        if self.context.is_open()
            && self.route_context_menu(event, scale_factor, *modifiers, state, path, node_id, taffy)
        {
            *handled = true;
            return;
        }

        // The name-prompt dialog is modal: while it's up the shared ModalDialog
        // captures every event (typing, clicks, Esc) and nothing leaks to the
        // library behind the scrim.
        if self.name_prompt.is_some() {
            self.route_name_event(event, scale_factor, *modifiers, state, handled);
            return;
        }

        match event {
            WindowEvent::CursorMoved { .. } => {
                self.on_cursor_moved(event, scale_factor, *modifiers, state, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.on_right_click(state, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(event, scale_factor, *modifiers, state, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(event, scale_factor, *modifiers, state, handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(*delta, scale_factor, handled);
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
                    scale_factor,
                    *modifiers,
                    state,
                    handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MusicApp"
    }

    /// The music library is custom-painted, including its transport.  Export
    /// the now-playing state and the four controls that are useful without a
    /// pointer; activating one calls the exact same local `Action` reducer as
    /// a rendered button or keyboard shortcut.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let current = self
            .pb
            .current
            .as_ref()
            .and_then(|id| self.lib.tracks.get(id))
            .map_or_else(
                || "Nothing playing".to_string(),
                |track| track.title.clone(),
            );
        let controls = vec![
            AccessNode::new(Role::Button)
                .with_name("Previous track")
                .with_action_target("music.previous"),
            AccessNode::new(Role::ToggleButton)
                .with_name("Play or pause")
                .with_action_target("music.toggle-play")
                .with_state(AccessState::new().checked(self.pb.playing)),
            AccessNode::new(Role::Button)
                .with_name("Next track")
                .with_action_target("music.next"),
            AccessNode::new(Role::ToggleButton)
                .with_name("Mute")
                .with_action_target("music.toggle-mute")
                .with_state(AccessState::new().checked(self.pb.volume == 0)),
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
            .with_name("Music player")
            .with_value(format!("{current}; volume {}%", self.pb.volume))
            .with_description(player_status)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .busy(self.active_resolve_request.is_some()),
            )
            .with_children(controls);
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
        let mut children = vec![player];
        children.extend(page_content);
        Some(
            AccessNode::new(Role::Group)
                .with_name("Music")
                .with_description("Music library and playback controls")
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
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
            "music.toggle-mute" => Action::ToggleMute,
            "music.empty-search" => Action::Nav(View::Search),
            "music.retry-search" => Action::RetrySearch,
            _ => return false,
        };
        self.dispatch(action, state);
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames flowing while playing / a toast fades, and — crucially —
        // while the streaming worker has an in-flight search or stream
        // resolution, so its result is drained (and applied to the audio
        // engine) promptly without waiting for the next input event.
        self.pb.playing
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

/// Map an x coordinate to a `0..=1` fraction within `r`.
fn frac_in(r: Rect, x: f64) -> f64 {
    if r.width() <= 0.0 {
        return 0.0;
    }
    ((x - r.x0) / r.width()).clamp(0.0, 1.0)
}

impl MusicApp {
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
            scale_factor,
            &modifiers,
            state,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    /// Route an event to the open context menu; returns whether `handle_event`
    /// should return immediately (the menu consumed it, or an outside click
    /// dismissed it).
    fn route_context_menu(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut menu_handled = false;
        let mut ft = None;
        self.context.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut menu_handled,
            &mut ft,
            node_id,
            taffy,
        );
        if self.drain_menu_selection(state) {
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
                self.search_edit_at = Some(Instant::now());
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
    fn draw_cover(
        &self,
        scene: &mut Scene,
        t: Affine,
        rect: Rect,
        id: &str,
        label: &str,
        tr: &mut TextRenderer,
        radius: f64,
    ) {
        if let Some(img) = self.art_image(id) {
            ui::image_store::paint_image_cover(scene, t, rect, &img, radius);
        } else {
            art::draw_album_art(scene, t, rect, id, label, tr, radius);
        }
    }

    /// Paint a circular artist avatar — the real image clipped to a circle if
    /// loaded, else the procedural two-tone circle with initials.
    fn draw_avatar(
        &self,
        scene: &mut Scene,
        t: Affine,
        center: Point,
        radius: f64,
        id: &str,
        label: &str,
        tr: &mut TextRenderer,
        c: &Palette,
    ) {
        if let Some(img) = self.art_image(id) {
            let circle = Circle::new(center, radius);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &circle);
            let r = Rect::new(
                center.x - radius,
                center.y - radius,
                center.x + radius,
                center.y + radius,
            );
            ui::image_store::paint_image_cover(scene, t, r, &img, 0.0);
            scene.pop_layer();
        } else {
            let (base, accent) = art::art_colors(id);
            scene.fill(Fill::NonZero, t, base, None, &Circle::new(center, radius));
            scene.fill(
                Fill::NonZero,
                t,
                accent.multiply_alpha(0.8),
                None,
                &Circle::new(
                    Point::new(center.x - radius * 0.24, center.y - radius * 0.24),
                    radius * 0.68,
                ),
            );
            let initials = art::initials(label);
            let sz = (radius * 0.6) as f32;
            let (tw, th) = tr.measure_weighted(&initials, sz, None, TextWeight::Bold);
            draw_text(
                tr,
                scene,
                t,
                &initials,
                center.x - f64::from(tw) / 2.0,
                center.y - f64::from(th) / 2.0,
                sz,
                c.text.multiply_alpha(0.95),
                TextWeight::Bold,
                None,
            );
        }
    }

    // ---- master frame -----------------------------------------------------

    pub(super) fn render_frame(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hits.clear();
        self.row_menu_hits.clear();
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
        let content_rect = self.drawer.resolve(self.rect);

        let c = Palette::from(theme);
        let r = self.rect;

        // App background.
        scene.fill(Fill::NonZero, t, c.bg, None, &r);

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
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        let start_y = body.y0 - self.scroll;
        let end_y = self.draw_body(scene, t, tr, &c, body, start_y);
        scene.pop_layer();
        self.content_height = (end_y + self.scroll) - body.y0 + 24.0;
        let max = (self.content_height - body.height()).max(0.0);
        if self.scroll > max {
            self.scroll = max;
        }

        self.draw_header(scene, t, tr, &c, theme, header);
        self.draw_nowplaying(scene, t, tr, &c, nowplaying);

        // Overlay flap: dim the content behind the drawer's scrim (alpha tracks
        // the eased reveal), and register a hit so a scrim tap dismisses it.
        let scrim = self.drawer.scrim_rect();
        if scrim.width() > 0.5 {
            self.drawer.draw_scrim(scene, t);
            self.push_hit(scrim, Action::CloseSidebar);
        }

        // Sidebar (docked beside the content, or floating in overlay). Skip the
        // draw entirely when fully collapsed so its hits don't shadow content.
        let side = self.sidebar_rect();
        if side.width() > 0.5 {
            self.draw_sidebar(scene, t, tr, &c, side);
        }

        // Hamburger / close toggle over the content's top-left corner,
        // whenever the sidebar isn't fully docked-open.
        if self.show_hamburger() {
            self.draw_hamburger(scene, t, &c, header);
        }

        // Collapsed-volume popover floats above the now-playing bar.
        self.draw_volume_popover(scene, t, tr, &c);

        // Queue hover preview floats above the now-playing bar's queue button.
        self.draw_queue_preview(scene, t, tr, &c);

        self.draw_toast(scene, t, tr, theme);

        // Name-input overlay (create / rename playlist) — the shared
        // ModalDialog paints its own scrim, box, title, footer, and field.
        if self.name_prompt.is_some() {
            self.ensure_name_dialog();
            self.layout_name_dialog();
            if let Some(nd) = self.name_dialog.as_mut() {
                nd.dialog.draw(scene, t, tr, theme, &PresenceCtx::empty());
            }
        } else {
            self.name_dialog = None;
        }

        // The context menu paints last so it sits on top of all content.
        if self.context.is_open() {
            // Dimming scrim that also dismisses on click (registered as a hit).
            self.context
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        }
    }

    /// The hamburger (or close-chevron) toggle that summons / dismisses the
    /// overlay sidebar. Sits in the header's top-left, left of the title.
    fn draw_hamburger(&mut self, scene: &mut Scene, t: Affine, c: &Palette, header: Rect) {
        let size = 34.0;
        let x0 = header.x0 + 14.0;
        let y0 = header.y0 + (HEADER_H - size) / 2.0;
        let btn = Rect::new(x0, y0, x0 + size, y0 + size);
        self.hamburger_rect = btn;
        let hover = btn.contains(self.mouse);
        fill_rrect(
            scene,
            t,
            btn,
            10.0,
            if hover { c.white10 } else { c.white5 },
        );
        let cx = btn.center();
        let open = self.drawer.reveal() > 0.5;
        if open && self.drawer.is_overlay() {
            // Close affordance: a left chevron.
            icon::draw(scene, t, Icon::ChevronLeft, cx, 18.0, c.text);
        } else {
            // Three-line hamburger.
            let lw = 15.0;
            for i in 0..3 {
                let ly = cx.y - 5.0 + f64::from(i) * 5.0;
                fill_rrect(
                    scene,
                    t,
                    Rect::new(cx.x - lw / 2.0, ly - 1.0, cx.x + lw / 2.0, ly + 1.0),
                    1.0,
                    c.text,
                );
            }
        }
    }

    // ---- sidebar ----------------------------------------------------------

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
    ) {
        // Clip ALL sidebar content to the (possibly narrowing) panel rect, so a
        // collapse / slide never spills brand + nav pixels over the content
        // beside it. The clip is popped at the end of the method.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &r);
        scene.fill(Fill::NonZero, t, c.panel, None, &r);
        // Right divider.
        scene.fill(
            Fill::NonZero,
            t,
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
                fill_rrect(scene, t, cb, 8.0, c.white10);
            }
            icon::draw(
                scene,
                t,
                Icon::ChevronLeft,
                cb.center(),
                16.0,
                if hover { c.text } else { c.dim },
            );
        }

        let mut y = r.y0 + 56.0;
        let nav = [
            (Icon::Home, "Home", View::Home),
            (Icon::Search, "Search", View::Search),
            (Icon::Songs, "Songs", View::Songs),
            (Icon::Albums, "Albums", View::Albums),
            (Icon::Artists, "Artists", View::Artists),
            (Icon::HeartFilled, "Liked Songs", View::Liked),
            (Icon::Download, "Downloads", View::Downloads),
            (Icon::Queue, "Queue", View::Queue),
        ];
        for (ic, label, view) in nav {
            let active = self.view == view;
            y = self.nav_row(scene, t, tr, c, r, y, ic, label, view, active);
        }

        self.draw_sidebar_playlists(scene, t, tr, c, r, y);

        // Close the panel clip opened at the top of this method.
        scene.pop_layer();
    }

    /// The "PLAYLISTS" header + new-playlist affordance + the playlist list.
    fn draw_sidebar_playlists(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
        mut y: f64,
    ) {
        y += 14.0;
        draw_text(
            tr,
            scene,
            t,
            "PLAYLISTS",
            r.x0 + 24.0,
            y,
            11.0,
            c.dim,
            TextWeight::Semibold,
            None,
        );
        // New-playlist plus — opens the inline create prompt.
        let plus_rect = Rect::new(r.x1 - 40.0, y - 4.0, r.x1 - 16.0, y + 20.0);
        let plus_hover = self.push_hit(plus_rect, Action::BeginNewPlaylist);
        icon::draw(
            scene,
            t,
            Icon::Plus,
            plus_rect.center(),
            16.0,
            if plus_hover { c.text } else { c.dim },
        );
        y += 26.0;

        let playlists = self.gather_playlists();
        for p in playlists {
            let active = self.view == View::Playlist(p.id.clone());
            let row = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 34.0);
            let hover = self.push_hit(row, Action::Nav(View::Playlist(p.id.clone())));
            if active || hover {
                fill_rrect(scene, t, row, 8.0, c.white5);
            }
            let col = if active { c.text } else { c.dim };
            let name = elide(tr, &p.name, 13.0, TextWeight::Medium, row.width() - 28.0);
            draw_text(
                tr,
                scene,
                t,
                &name,
                r.x0 + 24.0,
                vy(y, 34.0, 13.0),
                13.0,
                col,
                TextWeight::Medium,
                None,
            );
            y += 34.0;
        }
    }

    fn nav_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        side: Rect,
        y: f64,
        ic: Icon,
        label: &str,
        view: View,
        active: bool,
    ) -> f64 {
        let h = 42.0;
        let row = Rect::new(side.x0 + 12.0, y, side.x1 - 12.0, y + h);
        let hover = self.push_hit(row, Action::Nav(view));
        if active {
            fill_rrect(scene, t, row, 10.0, c.accent.multiply_alpha(0.16));
            // Active marker bar.
            fill_rrect(
                scene,
                t,
                Rect::new(side.x0, y + 9.0, side.x0 + 4.0, y + h - 9.0),
                2.0,
                c.accent,
            );
        } else if hover {
            fill_rrect(scene, t, row, 10.0, c.white5);
        }
        let fg = if active {
            c.accent
        } else if hover {
            c.text
        } else {
            c.dim
        };
        icon::draw(
            scene,
            t,
            ic,
            Point::new(row.x0 + 18.0, y + h / 2.0),
            19.0,
            fg,
        );
        draw_text(
            tr,
            scene,
            t,
            label,
            row.x0 + 38.0,
            vy(y, h, 14.0),
            14.0,
            fg,
            TextWeight::Medium,
            None,
        );
        y + h + 2.0
    }

    // ---- header -----------------------------------------------------------

    fn draw_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        theme: &Theme,
        r: Rect,
    ) {
        scene.fill(Fill::NonZero, t, c.bg, None, &r);
        // Leave room for the hamburger toggle in the corner when it's shown.
        let lead = if self.show_hamburger() { 56.0 } else { 24.0 };
        let mut x = r.x0 + lead;

        if !self.nav_back.is_empty() {
            let bz = Rect::new(x, r.y0 + 20.0, x + 32.0, r.y0 + 52.0);
            let hover = self.push_hit(bz, Action::Back);
            fill_rrect(scene, t, bz, 16.0, if hover { c.white10 } else { c.white5 });
            icon::draw(scene, t, Icon::ChevronLeft, bz.center(), 18.0, c.text);
            x += 44.0;
        }

        let title = self.view_title();
        draw_text(
            tr,
            scene,
            t,
            &title,
            x,
            vy(r.y0, HEADER_H, 26.0),
            26.0,
            c.text,
            TextWeight::Bold,
            None,
        );

        // Search pill on the right. The pill chrome (bg, focus/hover ring, the
        // magnifier) is drawn here; the editable text itself is a real
        // `ui::TextInput` laid into the pill's text area, so it carries its own
        // caret, selection, placeholder and full editing affordances.
        let pill_w = 320.0_f64.min(r.width() * 0.4);
        let pill = Rect::new(r.x1 - 24.0 - pill_w, r.y0 + 18.0, r.x1 - 24.0, r.y0 + 54.0);
        self.search_pill_rect = pill;
        let focused = self.search_input.is_focused();
        let hover = self.push_hit(pill, Action::FocusSearch);
        fill_rrect(scene, t, pill, 18.0, c.panel);
        if focused {
            scene.stroke(&Stroke::new(1.5), t, c.accent, None, &rrect(pill, 18.0));
        } else if hover {
            scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(pill, 18.0));
        }
        icon::draw(
            scene,
            t,
            Icon::Search,
            Point::new(pill.x0 + 20.0, pill.center().y),
            16.0,
            c.dim,
        );
        // The text field fills the pill after the magnifier gutter. Plain chrome
        // (no field border) so it reads as part of the pill.
        let text_rect = Rect::new(pill.x0 + 38.0, pill.y0, pill.x1 - 16.0, pill.y1);
        self.search_input.apply_layout_external(text_rect);
        self.search_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
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

    fn draw_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
    ) -> f64 {
        match self.view.clone() {
            View::Home => self.draw_home(scene, t, tr, c, body, y),
            View::Songs => {
                let ids: Vec<String> = self
                    .lib
                    .tracks_sorted()
                    .iter()
                    .map(|tk| tk.id.clone())
                    .collect();
                let rows = self.gather_tracks(&ids);
                self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true, None)
            }
            View::Albums => {
                let albums = self.gather_albums(None);
                self.draw_album_grid(scene, t, tr, c, body, y, &albums)
            }
            View::Artists => {
                let artists = self.gather_artists();
                self.draw_artist_grid(scene, t, tr, c, body, y, &artists)
            }
            View::Playlists => {
                let pls = self.gather_playlists();
                self.draw_playlist_grid(scene, t, tr, c, body, y, &pls)
            }
            View::Downloads => self.draw_downloads(scene, t, tr, c, body, y),
            View::Liked => self.draw_liked(scene, t, tr, c, body, y),
            View::Queue => self.draw_queue(scene, t, tr, c, body, y),
            View::Lyrics => self.draw_lyrics(scene, t, tr, c, body, y),
            View::Search => self.draw_search(scene, t, tr, c, body, y),
            View::Album(id) => self.draw_album_detail(scene, t, tr, c, body, y, &id),
            View::Artist(id) => self.draw_artist_detail(scene, t, tr, c, body, y, &id),
            View::Playlist(id) => self.draw_playlist_detail(scene, t, tr, c, body, y, &id),
        }
    }

    fn draw_home(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        // Kick off the real Home fetch (charts / new releases / top artists).
        self.ensure_home();
        let pad = 28.0;
        y += 20.0;
        draw_text(
            tr,
            scene,
            t,
            "Good evening",
            body.x0 + pad,
            y,
            28.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        y += 46.0;

        // Real YouTube-Music content when it has landed.
        if let Some(home) = self.home.take() {
            for section in &home.sections {
                y = Self::section_label(scene, t, tr, c, body, y, &section.title);
                y = self.draw_home_section(scene, t, tr, c, body, y, section);
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
            return Self::draw_home_skeleton(scene, t, c, body, y);
        }

        // Nothing live, and nothing in the library yet — the default build
        // starts empty. Show a composed, inviting zero-state instead of bare
        // section labels stacked over emptiness. Never fabricates content.
        if self.lib.playlists.is_empty() && self.lib.albums.is_empty() && self.lib.tracks.is_empty()
        {
            return self.draw_home_empty(scene, t, tr, c, body, y);
        }

        // The user's own library (built up as they play / like / save, or synced
        // from another device): quick-pick chips, jump-back-in, top tracks.
        y = self.draw_home_playlist_chips(scene, t, tr, c, body, y);

        // Jump back in — albums.
        y = Self::section_label(scene, t, tr, c, body, y, "Jump back in");
        let albums = self.gather_albums(Some(6));
        y = self.draw_album_grid(scene, t, tr, c, body, y, &albums);
        y += 10.0;

        // Top tracks.
        y = Self::section_label(scene, t, tr, c, body, y, "Top tracks");
        let ids: Vec<String> = self
            .lib
            .top_tracks(6)
            .iter()
            .map(|tk| tk.id.clone())
            .collect();
        let rows = self.gather_tracks(&ids);
        y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true, None);
        y
    }

    /// Quick-pick playlist chips (2-column grid). Returns the y below them.
    fn draw_home_playlist_chips(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
    ) -> f64 {
        let pad = 28.0;
        let pls = self.gather_playlists();
        let chip_h = 64.0;
        let cols = 2usize;
        let gap = 16.0;
        let chip_w = (body.width() - 2.0 * pad - gap * (cols as f64 - 1.0)) / cols as f64;
        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (chip_w + gap);
            let cy = y + (i / cols) as f64 * (chip_h + gap);
            let chip = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
            let hover = self.push_hit(chip, Action::Nav(View::Playlist(p.id.clone())));
            fill_rrect(
                scene,
                t,
                chip,
                10.0,
                if hover { c.card_hover } else { c.card },
            );
            self.draw_cover(
                scene,
                t,
                Rect::new(cx, cy, cx + chip_h, cy + chip_h),
                &p.id,
                &p.name,
                tr,
                10.0,
            );
            let name = elide(
                tr,
                &p.name,
                15.0,
                TextWeight::Semibold,
                chip_w - chip_h - 24.0,
            );
            draw_text(
                tr,
                scene,
                t,
                &name,
                cx + chip_h + 14.0,
                vy(cy, chip_h, 15.0),
                15.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
        }
        let rows_n = pls.len().div_ceil(cols);
        y + rows_n as f64 * (chip_h + gap) + 16.0
    }

    fn section_label(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        label: &str,
    ) -> f64 {
        draw_text(
            tr,
            scene,
            t,
            label,
            body.x0 + 28.0,
            y,
            19.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        y + 36.0
    }

    /// A composed, inviting zero-state for an empty Home (the default build
    /// starts with an empty library and no live content). A soft badge, a warm
    /// heading + subtext, and a single primary action — Search — that gets the
    /// user listening. Fabricates nothing (HIG §4/§7).
    fn draw_home_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
    ) -> f64 {
        let cx = body.center().x;
        let mut yy = y + (body.height() * 0.14).clamp(48.0, 140.0);
        yy = Self::draw_home_empty_badge_and_heading(scene, t, tr, c, cx, yy);
        yy = Self::draw_home_empty_subtext(scene, t, tr, c, body, cx, yy);
        self.draw_home_empty_search_button(scene, t, tr, c, cx, yy)
    }

    /// The soft note badge + centred heading. Returns the y below them.
    fn draw_home_empty_badge_and_heading(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        cx: f64,
        mut yy: f64,
    ) -> f64 {
        let r = 42.0;
        let center = Point::new(cx, yy + r);
        scene.fill(Fill::NonZero, t, c.card, None, &Circle::new(center, r));
        scene.fill(
            Fill::NonZero,
            t,
            c.accent.multiply_alpha(0.16),
            None,
            &Circle::new(center, r),
        );
        icon::draw(scene, t, Icon::Note, center, 34.0, c.accent);
        yy += 2.0 * r + 30.0;

        let head = "Your music starts here";
        let hw = f64::from(tr.measure_weighted(head, 25.0, None, TextWeight::Bold).0);
        draw_text(
            tr,
            scene,
            t,
            head,
            cx - hw / 2.0,
            yy,
            25.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        yy + 42.0
    }

    /// The wrapped, centred subtext. Returns the y below it.
    fn draw_home_empty_subtext(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
                tr.measure_weighted(&cand, 14.0, None, TextWeight::Regular)
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
            let lw = f64::from(tr.measure_weighted(line, 14.0, None, TextWeight::Regular).0);
            draw_text(
                tr,
                scene,
                t,
                line,
                cx - lw / 2.0,
                yy,
                14.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            yy += 22.0;
        }
        yy + 18.0
    }

    /// Primary action: Search (the one thing to do from an empty Home).
    fn draw_home_empty_search_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        cx: f64,
        yy: f64,
    ) -> f64 {
        let bh = 44.0;
        let label = "Search";
        let labelw = f64::from(
            tr.measure_weighted(label, 15.0, None, TextWeight::Semibold)
                .0,
        );
        let group = 18.0 + 10.0 + labelw;
        let bw = (group + 48.0).max(150.0);
        let btn = Rect::new(cx - bw / 2.0, yy, cx + bw / 2.0, yy + bh);
        let hover = self.push_hit(btn, Action::Nav(View::Search));
        fill_rrect(
            scene,
            t,
            btn,
            bh / 2.0,
            if hover { c.accent_hover } else { c.accent },
        );
        let gx = btn.center().x - group / 2.0;
        icon::draw(
            scene,
            t,
            Icon::Search,
            Point::new(gx + 9.0, btn.center().y),
            16.0,
            Color::WHITE,
        );
        draw_text(
            tr,
            scene,
            t,
            label,
            gx + 28.0,
            vy(btn.y0, bh, 15.0),
            15.0,
            Color::WHITE,
            TextWeight::Semibold,
            None,
        );
        yy + bh + 24.0
    }

    /// Draw one real Home section as a responsive grid of cover tiles. Track
    /// tiles play (queueing the whole section), album/artist tiles navigate to
    /// the real detail page.
    fn draw_home_section(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        section: &HomeSection,
    ) -> f64 {
        let pad = 28.0;
        let gap = 20.0;
        let target = 168.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 52.0;

        // Library ids for the track queue (a chart section plays as a queue).
        let track_ids: Vec<String> = section
            .items
            .iter()
            .filter(|it| it.kind == HomeItemKind::Track)
            .map(|it| format!("yt:{}", it.video_id))
            .collect();
        let mut ti = 0usize;

        for (i, item) in section.items.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            self.draw_home_section_tile_art(
                scene, t, tr, c, item, cx, cy, card_w, card_h, art_rect, &track_ids, &mut ti,
            );
            Self::draw_home_section_tile_labels(scene, t, tr, c, item, cx, cy, card_w);
        }
        let rows = section.items.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    /// The tile's artwork (cover / avatar), its play-on-hover FAB (tracks and
    /// albums), and its hit target.
    fn draw_home_section_tile_art(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        item: &HomeItem,
        cx: f64,
        cy: f64,
        card_w: f64,
        card_h: f64,
        art_rect: Rect,
        track_ids: &[String],
        ti: &mut usize,
    ) {
        match item.kind {
            HomeItemKind::Track => {
                let lib_id = format!("yt:{}", item.video_id);
                let idx = *ti;
                *ti += 1;
                let hover = self.push_hit(
                    art_rect,
                    Action::Play {
                        queue: track_ids.to_vec(),
                        index: idx,
                    },
                );
                self.draw_cover(scene, t, art_rect, &lib_id, &item.title, tr, 12.0);
                if hover {
                    let fab = Circle::new(Point::new(art_rect.x1 - 24.0, art_rect.y1 - 24.0), 18.0);
                    scene.fill(Fill::NonZero, t, c.accent, None, &fab);
                    icon::draw(
                        scene,
                        t,
                        Icon::Play,
                        Point::new(fab.center.x + 1.0, fab.center.y),
                        14.0,
                        c.bg,
                    );
                }
            }
            HomeItemKind::Album => {
                let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
                let hover = self.push_hit(card, Action::Nav(View::Album(item.id.clone())));
                self.draw_cover(scene, t, art_rect, &item.id, &item.title, tr, 12.0);
                if hover {
                    let fab = Circle::new(Point::new(art_rect.x1 - 24.0, art_rect.y1 - 24.0), 18.0);
                    scene.fill(Fill::NonZero, t, c.accent, None, &fab);
                    icon::draw(
                        scene,
                        t,
                        Icon::Play,
                        Point::new(fab.center.x + 1.0, fab.center.y),
                        14.0,
                        c.bg,
                    );
                }
            }
            HomeItemKind::Artist => {
                self.push_hit(
                    Rect::new(cx, cy, cx + card_w, cy + card_h),
                    Action::Nav(View::Artist(item.id.clone())),
                );
                let center = Point::new(cx + card_w / 2.0, cy + card_w / 2.0);
                self.draw_avatar(scene, t, center, card_w / 2.0, &item.id, &item.title, tr, c);
            }
            HomeItemKind::Playlist => {}
        }
    }

    /// Title + subtitle beneath a Home tile (centred for artists, left-aligned
    /// otherwise).
    fn draw_home_section_tile_labels(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        item: &HomeItem,
        cx: f64,
        cy: f64,
        card_w: f64,
    ) {
        let title_centered = item.kind == HomeItemKind::Artist;
        let title = elide(tr, &item.title, 14.0, TextWeight::Semibold, card_w);
        let sub = elide(tr, &item.subtitle, 12.0, TextWeight::Regular, card_w);
        if title_centered {
            let (tw, _) = tr.measure_weighted(&title, 14.0, None, TextWeight::Semibold);
            let (sw, _) = tr.measure_weighted(&sub, 12.0, None, TextWeight::Regular);
            draw_text(
                tr,
                scene,
                t,
                &title,
                cx + (card_w - f64::from(tw)) / 2.0,
                cy + card_w + 8.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            draw_text(
                tr,
                scene,
                t,
                &sub,
                cx + (card_w - f64::from(sw)) / 2.0,
                cy + card_w + 28.0,
                12.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        } else {
            draw_text(
                tr,
                scene,
                t,
                &title,
                cx,
                cy + card_w + 8.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            draw_text(
                tr,
                scene,
                t,
                &sub,
                cx,
                cy + card_w + 28.0,
                12.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    // ---- album grid -------------------------------------------------------

    fn draw_album_grid(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        albums: &[AlbumDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        for (i, a) in albums.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            self.draw_album_grid_card(scene, t, tr, c, a, cx, cy, card_w, card_h);
        }
        let rows = albums.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    /// One album card: cover, save toggle, play FAB on hover, title/subtitle.
    fn draw_album_grid_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        a: &AlbumDisp,
        cx: f64,
        cy: f64,
        card_w: f64,
        card_h: f64,
    ) {
        let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
        let hover = self.push_hit(card, Action::Nav(View::Album(a.id.clone())));
        if hover {
            fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
        }
        let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
        self.draw_cover(scene, t, art_rect, &a.id, &a.title, tr, 12.0);
        self.draw_album_grid_card_overlays(scene, t, c, a, art_rect, hover);
        let title = elide(tr, &a.title, 14.0, TextWeight::Semibold, card_w);
        draw_text(
            tr,
            scene,
            t,
            &title,
            cx,
            cy + card_w + 8.0,
            14.0,
            c.text,
            TextWeight::Semibold,
            None,
        );
        let sub = elide(
            tr,
            &format!("{} · {}", a.artist, a.year),
            12.0,
            TextWeight::Regular,
            card_w,
        );
        draw_text(
            tr,
            scene,
            t,
            &sub,
            cx,
            cy + card_w + 28.0,
            12.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
    }

    /// The save (add-to-library) toggle and hover play FAB over an album
    /// card's cover art.
    fn draw_album_grid_card_overlays(
        &mut self,
        scene: &mut Scene,
        t: Affine,
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
            scene.fill(
                Fill::NonZero,
                t,
                c.scrim.multiply_alpha(0.55),
                None,
                &Circle::new(add_c, 16.0),
            );
            icon::draw(
                scene,
                t,
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
            scene.fill(Fill::NonZero, t, c.accent, None, &fab);
            icon::draw(
                scene,
                t,
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
                    queue: a.queue.clone(),
                    index: 0,
                },
            );
        }
    }

    // ---- artist grid ------------------------------------------------------

    fn draw_artist_grid(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        artists: &[ArtistDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 50.0;

        for (i, a) in artists.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Artist(a.id.clone())));
            if hover {
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            // Circular avatar (real image if loaded, else procedural).
            let center = Point::new(cx + card_w / 2.0, cy + card_w / 2.0);
            self.draw_avatar(scene, t, center, card_w / 2.0, &a.id, &a.name, tr, c);

            let name = elide(tr, &a.name, 14.0, TextWeight::Semibold, card_w);
            let (nw, _) = tr.measure_weighted(&name, 14.0, None, TextWeight::Semibold);
            draw_text(
                tr,
                scene,
                t,
                &name,
                cx + (card_w - f64::from(nw)) / 2.0,
                cy + card_w + 8.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            let genre = elide(tr, &a.genre, 12.0, TextWeight::Regular, card_w);
            let (gw, _) = tr.measure_weighted(&genre, 12.0, None, TextWeight::Regular);
            draw_text(
                tr,
                scene,
                t,
                &genre,
                cx + (card_w - f64::from(gw)) / 2.0,
                cy + card_w + 26.0,
                12.0,
                c.dim,
                TextWeight::Regular,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        pls: &[PlaylistDisp],
    ) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        // "Create playlist" pill above the grid.
        let create = Rect::new(body.x0 + pad, y, body.x0 + pad + 184.0, y + 40.0);
        let ch = self.push_hit(create, Action::BeginNewPlaylist);
        fill_rrect(
            scene,
            t,
            create,
            20.0,
            if ch { c.accent_hover } else { c.accent },
        );
        icon::draw(
            scene,
            t,
            Icon::Plus,
            Point::new(create.x0 + 24.0, create.center().y),
            16.0,
            c.bg,
        );
        draw_text(
            tr,
            scene,
            t,
            "New playlist",
            create.x0 + 44.0,
            vy(create.y0, 40.0, 14.0),
            14.0,
            c.bg,
            TextWeight::Semibold,
            None,
        );
        y += 56.0;

        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Playlist(p.id.clone())));
            if hover {
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            self.draw_cover(scene, t, art_rect, &p.id, &p.name, tr, 12.0);
            icon::draw(
                scene,
                t,
                Icon::Playlist,
                Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0),
                22.0,
                c.text.multiply_alpha(0.85),
            );
            let title = elide(tr, &p.name, 14.0, TextWeight::Semibold, card_w);
            draw_text(
                tr,
                scene,
                t,
                &title,
                cx,
                cy + card_w + 8.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            draw_text(
                tr,
                scene,
                t,
                &format!("{} songs", p.count),
                cx,
                cy + card_w + 28.0,
                12.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
        let rows = pls.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- track table ------------------------------------------------------

    /// The unified track-row table used by album, Liked Songs, Downloads **and**
    /// playlist detail — so every list shares one row look: the now-playing
    /// equalizer, like + download controls, and duration. Passing
    /// `playlist = Some(id)` additionally binds the right-click menu to that
    /// playlist and shows reorder (up/down) + remove affordances on hover.
    fn draw_track_table(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        rows: &[TrackDisp],
        queue: &[String],
        show_album: bool,
        number_col: bool,
        playlist: Option<&str>,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        // Balanced column geometry: a right-pinned duration/download/like
        // cluster, a centred album column, and the title starting at `text_x`
        // — so a row reads as one connected line rather than title-left with a
        // cavern across to everything bunched on the right.
        let cols = track_row_columns(left, right, number_col);
        let row_count = rows.len();

        for (i, tk) in rows.iter().enumerate() {
            let rr = Rect::new(left, y, right, y + row_h);
            let is_current = self.pb.current.as_deref() == Some(tk.id.as_str());
            let hovered = self.body_hit(
                rr,
                Action::Play {
                    queue: queue.to_vec(),
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
                        in_playlist: playlist.map(str::to_string),
                        queue_index: None,
                    },
                );
            }
            if hovered {
                fill_rrect(scene, t, rr, 8.0, c.white5);
            } else if playlist.is_some_and(|playlist| {
                self.revealed_membership
                    .as_ref()
                    .is_some_and(|(owner, track)| owner == playlist && track == &tk.id)
            }) {
                fill_rrect(scene, t, rr, 8.0, c.accent.multiply_alpha(0.16));
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.draw_track_row_content(
                    scene,
                    t,
                    tr,
                    c,
                    y,
                    row_h,
                    left,
                    right,
                    i,
                    row_count,
                    tk,
                    TrackRowFlags {
                        is_current,
                        hovered,
                        show_album,
                        number_col,
                    },
                    playlist,
                    &cols,
                );
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        y: f64,
        row_h: f64,
        left: f64,
        right: f64,
        i: usize,
        row_count: usize,
        tk: &TrackDisp,
        flags: TrackRowFlags,
        playlist: Option<&str>,
        cols: &TrackColumns,
    ) {
        let TrackRowFlags {
            is_current,
            hovered,
            show_album,
            number_col,
        } = flags;
        let (text_x, album_x, dl_x, dur_x, like_x) =
            (cols.text, cols.album, cols.dl, cols.dur, cols.like);
        self.draw_track_row_index(
            scene, t, tr, c, y, row_h, left, i, tk, is_current, hovered, number_col,
        );
        Self::draw_track_row_text(
            scene, t, tr, c, y, row_h, right, tk, is_current, show_album, text_x, album_x, dl_x,
        );
        self.draw_track_row_playlist_controls(
            scene, t, c, y, row_h, i, row_count, hovered, playlist, tk, like_x,
        );
        self.draw_track_row_like(scene, t, c, y, row_h, tk, hovered, like_x);
        self.draw_track_row_download(scene, t, tr, c, y, row_h, tk, dl_x, dur_x);
    }

    /// The leading index column: a play glyph on hover, an animated equalizer
    /// for the current track, else the track number.
    fn draw_track_row_index(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        y: f64,
        row_h: f64,
        left: f64,
        i: usize,
        tk: &TrackDisp,
        is_current: bool,
        hovered: bool,
        number_col: bool,
    ) {
        if !number_col {
            return;
        }
        if hovered {
            icon::draw(
                scene,
                t,
                Icon::Play,
                Point::new(left + 14.0, y + row_h / 2.0),
                14.0,
                c.text,
            );
        } else if is_current {
            // Now-playing equalizer: animates while playing, static
            // (a fixed silhouette) when paused.
            let bars = eq_bars(self.pb.playing);
            let mid = y + row_h / 2.0;
            for (bar_i, &frac) in bars.iter().enumerate() {
                let bx = left + 6.0 + bar_i as f64 * 6.0;
                let bh = 5.0 + frac * 13.0;
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.accent,
                    None,
                    &Rect::new(bx, mid - bh / 2.0, bx + 3.0, mid + bh / 2.0),
                );
            }
        } else {
            let num = format!("{}", tk.no.max(i as u32 + 1));
            draw_text(
                tr,
                scene,
                t,
                &num,
                left + 8.0,
                vy(y, row_h, 13.0),
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    /// Title, artist and (space permitting) album text.
    fn draw_track_row_text(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        y: f64,
        row_h: f64,
        right: f64,
        tk: &TrackDisp,
        is_current: bool,
        show_album: bool,
        text_x: f64,
        album_x: f64,
        dl_x: f64,
    ) {
        let title_col = if is_current { c.accent } else { c.text };
        let title = elide(
            tr,
            &tk.title,
            14.0,
            TextWeight::Medium,
            album_x - text_x - 16.0,
        );
        draw_text(
            tr,
            scene,
            t,
            &title,
            text_x,
            y + 9.0,
            14.0,
            title_col,
            TextWeight::Medium,
            None,
        );
        let artist = elide(
            tr,
            &tk.artist,
            12.0,
            TextWeight::Regular,
            album_x - text_x - 16.0,
        );
        draw_text(
            tr,
            scene,
            t,
            &artist,
            text_x,
            y + 28.0,
            12.0,
            c.dim,
            TextWeight::Regular,
            None,
        );

        if show_album && right - album_x > 80.0 {
            let al = elide(
                tr,
                &tk.album,
                13.0,
                TextWeight::Regular,
                dl_x - 48.0 - album_x - 16.0,
            );
            draw_text(
                tr,
                scene,
                t,
                &al,
                album_x,
                vy(y, row_h, 13.0),
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    /// Playlist reorder (up/down) + remove, on hover only. Tucked in the
    /// gap left of the like control.
    fn draw_track_row_playlist_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        y: f64,
        row_h: f64,
        i: usize,
        row_count: usize,
        hovered: bool,
        playlist: Option<&str>,
        tk: &TrackDisp,
        like_x: f64,
    ) {
        let pl_rm_x = like_x - 34.0;
        let pl_dn_x = pl_rm_x - 28.0;
        let pl_up_x = pl_dn_x - 28.0;
        let Some(pl) = playlist else {
            return;
        };
        if !hovered {
            return;
        }
        if i > 0 {
            let uc = Point::new(pl_up_x, y + row_h / 2.0);
            let uh = self.push_hit(
                square_around(uc, 24.0),
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i - 1,
                },
            );
            draw_chevron(scene, t, uc, true, if uh { c.text } else { c.dim });
        }
        if i + 1 < row_count {
            let dc = Point::new(pl_dn_x, y + row_h / 2.0);
            let dh = self.push_hit(
                square_around(dc, 24.0),
                Action::MovePlaylistItem {
                    playlist: pl.to_string(),
                    from: i,
                    to: i + 1,
                },
            );
            draw_chevron(scene, t, dc, false, if dh { c.text } else { c.dim });
        }
        let rc = Point::new(pl_rm_x, y + row_h / 2.0);
        let rh = self.push_hit(
            square_around(rc, 24.0),
            Action::RemoveFromPlaylist {
                playlist: pl.to_string(),
                track: tk.id.clone(),
            },
        );
        draw_x(scene, t, rc, if rh { c.accent } else { c.dim });
    }

    /// Like toggle (heart) just left of the download control.
    fn draw_track_row_like(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        y: f64,
        row_h: f64,
        tk: &TrackDisp,
        hovered: bool,
        like_x: f64,
    ) {
        let liked = self.lib.is_liked(&tk.id);
        let like_rect = Rect::new(
            like_x - 14.0,
            y + row_h / 2.0 - 14.0,
            like_x + 14.0,
            y + row_h / 2.0 + 14.0,
        );
        let like_hover = self.push_hit(like_rect, Action::ToggleLike(tk.id.clone()));
        if liked || like_hover || hovered {
            icon::draw(
                scene,
                t,
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

    /// Download toggle (cached / in-progress spinner / streamed) plus the
    /// duration readout.
    fn draw_track_row_download(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        y: f64,
        row_h: f64,
        tk: &TrackDisp,
        dl_x: f64,
        dur_x: f64,
    ) {
        let dl_rect = Rect::new(
            dl_x - 12.0,
            y + row_h / 2.0 - 14.0,
            dl_x + 16.0,
            y + row_h / 2.0 + 14.0,
        );
        let dl_hover = self.push_hit(dl_rect, Action::ToggleDownload(tk.id.clone()));
        match tk.cache {
            CacheState::Cached => {
                let badge = Circle::new(Point::new(dl_x, y + row_h / 2.0), 9.0);
                scene.fill(Fill::NonZero, t, c.success, None, &badge);
                icon::draw(
                    scene,
                    t,
                    Icon::Downloaded,
                    Point::new(dl_x, y + row_h / 2.0),
                    12.0,
                    c.bg,
                );
            }
            CacheState::Downloading(f) => {
                // A spinning "working" indicator: a faint track ring with
                // a rotating accent arc whose sweep grows as the download
                // nears done. The rotation (off the shared animation
                // clock) reads as actively working — clearly distinct from
                // the idle download arrow and the finished check badge.
                let center = Point::new(dl_x, y + row_h / 2.0);
                let radius = 9.0;
                scene.stroke(
                    &Stroke::new(2.0),
                    t,
                    c.dim.multiply_alpha(0.4),
                    None,
                    &Circle::new(center, radius),
                );
                // Spin ~1 turn every 1.1s; sweep between a quarter-turn
                // (just started) and a near-full turn (almost done).
                let spin = anim_time() * (std::f64::consts::TAU / 1.1);
                let sweep = std::f64::consts::FRAC_PI_2
                    + (std::f64::consts::TAU - std::f64::consts::FRAC_PI_2) * f64::from(f);
                let arc = vello::kurbo::Arc::new(center, (radius, radius), spin, sweep, 0.0);
                scene.stroke(
                    &Stroke::new(2.0).with_caps(vello::kurbo::Cap::Round),
                    t,
                    c.accent,
                    None,
                    &arc.to_path(0.2),
                );
            }
            CacheState::Streamed => {
                icon::draw(
                    scene,
                    t,
                    Icon::Download,
                    Point::new(dl_x, y + row_h / 2.0),
                    16.0,
                    if dl_hover { c.text } else { c.dim },
                );
            }
        }

        // Duration (hidden when unknown — some live sources omit it).
        if tk.dur > 0 {
            let dur = ui::format::timecode(u64::from(tk.dur));
            let (dw, _) = tr.measure_weighted(&dur, 13.0, None, TextWeight::Regular);
            draw_text(
                tr,
                scene,
                t,
                &dur,
                dur_x + (48.0 - f64::from(dw)),
                vy(y, row_h, 13.0),
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    // ---- detail pages -----------------------------------------------------

    /// The shared header for an album/artist/playlist detail page: cover or
    /// avatar, kind label, title, subtitle, and the Play/Shuffle + optional
    /// library/follow buttons.
    fn draw_detail_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        seed: &str,
        kind: &str,
        title: &str,
        subtitle: &str,
        queue: Vec<String>,
        circular: bool,
        lib_button: Option<(String, bool)>,
        follow: Option<(String, bool)>,
    ) -> f64 {
        let pad = 28.0;
        let art_d = 168.0;
        let ax = body.x0 + pad;
        let ay = y + 12.0;
        let tx = self.draw_detail_header_art_and_text(
            scene, t, tr, c, body, ax, ay, art_d, pad, seed, kind, title, subtitle, circular,
        );

        // Action buttons.
        let by = ay + 116.0;
        let shuf = self.draw_detail_header_transport(scene, t, tr, c, tx, by, queue);

        // Running x for the trailing pills, right of the shuffle button.
        let mut px = shuf.x1 + 12.0;
        if let Some((id, saved)) = lib_button {
            px = self
                .draw_detail_header_lib_button(scene, t, tr, c, px, by, id, saved)
                .x1
                + 12.0;
        }
        if let Some((id, following)) = follow {
            self.draw_detail_header_follow_button(scene, t, tr, c, px, by, id, following);
        }

        y + art_d + 36.0
    }

    /// The cover/avatar, kind label, title and subtitle. Returns the x the
    /// action-button row starts at.
    fn draw_detail_header_art_and_text(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        ax: f64,
        ay: f64,
        art_d: f64,
        pad: f64,
        seed: &str,
        kind: &str,
        title: &str,
        subtitle: &str,
        circular: bool,
    ) -> f64 {
        if circular {
            let center = Point::new(ax + art_d / 2.0, ay + art_d / 2.0);
            self.draw_avatar(scene, t, center, art_d / 2.0, seed, title, tr, c);
        } else {
            self.draw_cover(
                scene,
                t,
                Rect::new(ax, ay, ax + art_d, ay + art_d),
                seed,
                title,
                tr,
                14.0,
            );
        }

        let tx = ax + art_d + 28.0;
        draw_text(
            tr,
            scene,
            t,
            kind,
            tx,
            ay + 8.0,
            12.0,
            c.dim,
            TextWeight::Semibold,
            None,
        );
        let title_s = elide(tr, title, 38.0, TextWeight::Bold, body.x1 - tx - pad);
        draw_text(
            tr,
            scene,
            t,
            &title_s,
            tx,
            ay + 28.0,
            38.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            subtitle,
            tx,
            ay + 84.0,
            14.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        tx
    }

    /// The Play + Shuffle buttons. Returns the shuffle button's rect so the
    /// caller can position any trailing pills after it.
    fn draw_detail_header_transport(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        tx: f64,
        by: f64,
        queue: Vec<String>,
    ) -> Rect {
        let play = Rect::new(tx, by, tx + 120.0, by + 40.0);
        let ph = self.push_hit(play, Action::Play { queue, index: 0 });
        fill_rrect(
            scene,
            t,
            play,
            20.0,
            if ph { c.accent_hover } else { c.accent },
        );
        icon::draw(
            scene,
            t,
            Icon::Play,
            Point::new(play.x0 + 24.0, play.center().y),
            15.0,
            c.bg,
        );
        draw_text(
            tr,
            scene,
            t,
            "Play",
            play.x0 + 44.0,
            vy(by, 40.0, 14.0),
            14.0,
            c.bg,
            TextWeight::Semibold,
            None,
        );

        let shuf = Rect::new(play.x1 + 12.0, by, play.x1 + 12.0 + 44.0, by + 40.0);
        let sh = self.push_hit(shuf, Action::ToggleShuffle);
        scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(shuf, 20.0));
        icon::draw(
            scene,
            t,
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

    /// Optional trailing "Add to library" pill (album header). `saved` flips
    /// it to an "In Library" confirmed state. Returns its rect.
    fn draw_detail_header_lib_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        px: f64,
        by: f64,
        id: String,
        saved: bool,
    ) -> Rect {
        let pw = 150.0;
        let lib = Rect::new(px, by, px + pw, by + 40.0);
        let lh = self.push_hit(lib, Action::ToggleSaveAlbum(id));
        if saved {
            fill_rrect(scene, t, lib, 20.0, c.success.multiply_alpha(0.18));
            icon::draw(
                scene,
                t,
                Icon::Downloaded,
                Point::new(lib.x0 + 22.0, lib.center().y),
                16.0,
                c.success,
            );
            draw_text(
                tr,
                scene,
                t,
                "In Library",
                lib.x0 + 42.0,
                vy(by, 40.0, 14.0),
                14.0,
                c.success,
                TextWeight::Semibold,
                None,
            );
        } else {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                if lh { c.text } else { c.border },
                None,
                &rrect(lib, 20.0),
            );
            icon::draw(
                scene,
                t,
                Icon::Plus,
                Point::new(lib.x0 + 22.0, lib.center().y),
                16.0,
                if lh { c.text } else { c.dim },
            );
            draw_text(
                tr,
                scene,
                t,
                "Add to Library",
                lib.x0 + 42.0,
                vy(by, 40.0, 14.0),
                14.0,
                if lh { c.text } else { c.dim },
                TextWeight::Semibold,
                None,
            );
        }
        lib
    }

    /// Optional "Follow" / "Following" toggle pill (artist header).
    fn draw_detail_header_follow_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        px: f64,
        by: f64,
        id: String,
        following: bool,
    ) {
        let pw = 118.0;
        let fol = Rect::new(px, by, px + pw, by + 40.0);
        let fh = self.push_hit(fol, Action::ToggleFollow(id));
        if following {
            fill_rrect(scene, t, fol, 20.0, c.accent.multiply_alpha(0.16));
            icon::draw(
                scene,
                t,
                Icon::Downloaded,
                Point::new(fol.x0 + 22.0, fol.center().y),
                15.0,
                c.accent,
            );
            draw_text(
                tr,
                scene,
                t,
                "Following",
                fol.x0 + 42.0,
                vy(by, 40.0, 14.0),
                14.0,
                c.accent,
                TextWeight::Semibold,
                None,
            );
        } else {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                if fh { c.text } else { c.border },
                None,
                &rrect(fol, 20.0),
            );
            icon::draw(
                scene,
                t,
                Icon::Plus,
                Point::new(fol.x0 + 22.0, fol.center().y),
                16.0,
                if fh { c.text } else { c.dim },
            );
            draw_text(
                tr,
                scene,
                t,
                "Follow",
                fol.x0 + 42.0,
                vy(by, 40.0, 14.0),
                14.0,
                if fh { c.text } else { c.dim },
                TextWeight::Semibold,
                None,
            );
        }
    }

    fn draw_album_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music album (its ordered track list) when available.
        self.ensure_album(id);
        if let Some(page) = self.album_pages.remove(id) {
            let r = self.draw_album_detail_live(scene, t, tr, c, body, y, &page);
            self.album_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_album_id(id) && !self.lib.albums.contains_key(id) {
            if self.failed_pages.contains(id) {
                return Self::draw_detail_error(scene, t, tr, c, body, y, "album");
            }
            return Self::draw_loading_detail(scene, t, tr, c, body, y, "ALBUM", id);
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            id,
            "ALBUM",
            &title,
            &subtitle,
            ids.clone(),
            false,
            Some((id.to_string(), saved)),
            None,
        );
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true, None)
    }

    /// Album detail rendered straight from a real [`AlbumPage`].
    fn draw_album_detail_live(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            &page.id,
            "ALBUM",
            &page.title,
            &subtitle,
            ids.clone(),
            false,
            Some((page.id.clone(), saved)),
            None,
        );
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true, None)
    }

    /// A shimmering **skeleton** shown while a real album/artist page loads —
    /// a placeholder cover + title/subtitle bars + several track rows, so the
    /// page reads as loading without any "fake title" text.
    fn draw_loading_detail(
        scene: &mut Scene,
        t: Affine,
        _tr: &mut TextRenderer,
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
        draw_skeleton_block(scene, t, art, 14.0, c);
        let tx = art.x1 + 28.0;
        let right = body.x1 - pad;
        draw_skeleton_block(
            scene,
            t,
            Rect::new(tx, y + 16.0, tx + 68.0, y + 30.0),
            6.0,
            c,
        );
        draw_skeleton_block(
            scene,
            t,
            Rect::new(tx, y + 42.0, (tx + 340.0).min(right), y + 78.0),
            9.0,
            c,
        );
        draw_skeleton_block(
            scene,
            t,
            Rect::new(tx, y + 90.0, (tx + 240.0).min(right), y + 110.0),
            7.0,
            c,
        );
        draw_skeleton_block(
            scene,
            t,
            Rect::new(tx, y + 126.0, tx + 132.0, y + 158.0),
            18.0,
            c,
        );
        y += 168.0 + 30.0;
        // Skeleton track rows.
        for _ in 0..7 {
            let left = body.x0 + pad;
            draw_skeleton_block(
                scene,
                t,
                Rect::new(left, y + 10.0, left + 16.0, y + 26.0),
                4.0,
                c,
            );
            draw_skeleton_block(
                scene,
                t,
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
                scene,
                t,
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
                scene,
                t,
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
    /// chrome's Back affordance returns the user; this just explains the gap.
    fn draw_detail_error(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        kind: &str,
    ) -> f64 {
        let cx = body.center().x;
        let mut yy = y + (body.height() * 0.14).clamp(48.0, 140.0);
        let r = 38.0;
        let center = Point::new(cx, yy + r);
        scene.fill(Fill::NonZero, t, c.card, None, &Circle::new(center, r));
        icon::draw(scene, t, Icon::Disc, center, 30.0, c.dim);
        yy += 2.0 * r + 28.0;
        let head = format!("Couldn't load this {kind}");
        let hw = f64::from(tr.measure_weighted(&head, 20.0, None, TextWeight::Bold).0);
        draw_text(
            tr,
            scene,
            t,
            &head,
            cx - hw / 2.0,
            yy,
            20.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        yy += 34.0;
        let sub = "Check your connection and try again.";
        let sw = f64::from(tr.measure_weighted(sub, 14.0, None, TextWeight::Regular).0);
        draw_text(
            tr,
            scene,
            t,
            sub,
            cx - sw / 2.0,
            yy,
            14.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        yy + 40.0
    }

    /// A shimmering skeleton grid for the Home page while the real charts /
    /// new-releases sections stream in.
    fn draw_home_skeleton(
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        let gap = 20.0;
        let target = 168.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        for _ in 0..2 {
            // Section title bar.
            draw_skeleton_block(
                scene,
                t,
                Rect::new(body.x0 + pad, y, body.x0 + pad + 180.0, y + 22.0),
                7.0,
                c,
            );
            y += 40.0;
            for i in 0..cols {
                let cx = body.x0 + pad + i as f64 * (card_w + gap);
                draw_skeleton_block(scene, t, Rect::new(cx, y, cx + card_w, y + card_w), 12.0, c);
                draw_skeleton_block(
                    scene,
                    t,
                    Rect::new(cx, y + card_w + 10.0, cx + card_w * 0.8, y + card_w + 24.0),
                    5.0,
                    c,
                );
                draw_skeleton_block(
                    scene,
                    t,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        id: &str,
    ) -> f64 {
        // Real YouTube-Music artist page (top songs, albums, subscribers).
        self.ensure_artist(id);
        if let Some(page) = self.artist_pages.remove(id) {
            let r = self.draw_artist_detail_live(scene, t, tr, c, body, y, &page);
            self.artist_pages.insert(id.to_string(), page);
            return r;
        }
        if is_live_id(id) && !self.lib.artists.contains_key(id) {
            if self.failed_pages.contains(id) {
                return Self::draw_detail_error(scene, t, tr, c, body, y, "artist");
            }
            return Self::draw_loading_detail(scene, t, tr, c, body, y, "ARTIST", id);
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            id,
            "ARTIST",
            &name,
            &subtitle,
            top_ids.clone(),
            true,
            None,
            Some((id.to_string(), followed)),
        );

        let mut y = Self::section_label(scene, t, tr, c, body, y + 8.0, "Popular");
        let rows = self.gather_tracks(&top_ids);
        y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &top_ids, false, true, None);
        y += 12.0;
        y = Self::section_label(scene, t, tr, c, body, y, "Albums");
        self.draw_album_grid(scene, t, tr, c, body, y, &albums)
    }

    /// A full artist page rendered from a real [`ArtistPage`]: header (play +
    /// shuffle + follow), popular tracks, a sectioned discography (Albums vs.
    /// Singles & EPs), related artists, and an About/bio panel.
    fn draw_artist_detail_live(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            &page.id,
            "ARTIST",
            &page.name,
            &subtitle,
            top_ids.clone(),
            true,
            None,
            Some((page.id.clone(), followed)),
        );

        // Popular / top tracks.
        if !top_ids.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y + 8.0, "Popular");
            let rows = self.gather_tracks(&top_ids);
            y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &top_ids, false, true, None);
            y += 12.0;
        }

        y = self.draw_artist_detail_discography(scene, t, tr, c, body, y, page);
        y = self.draw_artist_detail_related(scene, t, tr, c, body, y, page);
        Self::draw_artist_detail_bio(scene, t, tr, c, body, y, page)
    }

    /// Discography, sectioned by release type. Full albums first, then the
    /// shorter Singles & EPs — the way listeners expect a discography to read.
    fn draw_artist_detail_discography(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            y = Self::section_label(scene, t, tr, c, body, y, "Albums");
            y = self.draw_album_grid(scene, t, tr, c, body, y, &albums);
            y += 12.0;
        }
        if !singles.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y, "Singles & EPs");
            y = self.draw_album_grid(scene, t, tr, c, body, y, &singles);
            y += 12.0;
        }
        y
    }

    /// Related / similar artists ("Fans also like").
    fn draw_artist_detail_related(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.similar.is_empty() {
            return y;
        }
        y = Self::section_label(scene, t, tr, c, body, y, "Fans also like");
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
        y = self.draw_artist_grid(scene, t, tr, c, body, y, &related);
        y + 12.0
    }

    /// About / bio: word-wrapped (the text renderer clips at `max_w`, so this
    /// wraps manually to show the whole bio across several lines), capped at
    /// a readable number of lines, with an optional Wikipedia link.
    fn draw_artist_detail_bio(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        page: &ArtistPage,
    ) -> f64 {
        if page.description.is_empty() {
            return y;
        }
        let pad = 28.0;
        y = Self::section_label(scene, t, tr, c, body, y, "About");
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
                tr.measure_weighted(&cand, 13.0, None, TextWeight::Regular)
                    .0,
            ) > maxw
                && !line.is_empty()
            {
                draw_text(
                    tr,
                    scene,
                    t,
                    &line,
                    body.x0 + pad,
                    y,
                    13.0,
                    c.dim,
                    TextWeight::Regular,
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
            draw_text(
                tr,
                scene,
                t,
                &line,
                body.x0 + pad,
                y,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            y += 20.0;
        }
        if !page.wikipedia_url.is_empty() {
            y += 4.0;
            draw_text(
                tr,
                scene,
                t,
                "Read more on Wikipedia",
                body.x0 + pad,
                y,
                12.0,
                c.accent,
                TextWeight::Semibold,
                None,
            );
            y += 22.0;
        }
        y
    }

    fn draw_playlist_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
                let bottom = self.draw_playlist_detail_live(scene, t, tr, c, body, y, &page);
                self.playlist_pages.insert(id.to_string(), page);
                return bottom;
            }
            if self.failed_pages.contains(id) {
                return Self::draw_detail_error(scene, t, tr, c, body, y, "playlist");
            }
            return Self::draw_loading_detail(scene, t, tr, c, body, y, "PLAYLIST", id);
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            id,
            "PLAYLIST",
            &name,
            &subtitle,
            ids.clone(),
            false,
            None,
            None,
        );

        // Playlist management row: an outlined pill per action.
        let pad = 28.0;
        self.draw_playlist_management_row(scene, t, tr, c, body, y - 8.0, id, &ids);
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
            draw_text(
                tr,
                scene,
                t,
                "This playlist is empty. Right-click a song \u{2192} Add to playlist\u{2026}",
                body.x0 + pad,
                y + 6.0,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        // Unified row look (equalizer + like + download + duration); the playlist
        // context adds reorder/remove affordances and binds the row menu.
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true, Some(id))
    }

    /// Playlist management row: an outlined pill per action (download all,
    /// set cover, rename, delete).
    fn draw_playlist_management_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        py: f64,
        id: &str,
        ids: &[String],
    ) {
        let pad = 28.0;
        let mut mx = body.x0 + pad;
        for (label, w, action, danger) in [
            (
                "Download all",
                128.0,
                Action::DownloadAll(ids.to_vec()),
                false,
            ),
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
            let stroke = if hov {
                if danger { c.accent } else { c.text }
            } else {
                c.border
            };
            scene.stroke(&Stroke::new(1.0), t, stroke, None, &rrect(r, 16.0));
            draw_text(
                tr,
                scene,
                t,
                label,
                r.x0 + 16.0,
                vy(r.y0, 32.0, 13.0),
                13.0,
                ink,
                TextWeight::Medium,
                None,
            );
            mx = r.x1 + 10.0;
        }
    }

    /// A live (YouTube-Music) playlist rendered from a real [`PlaylistPage`]:
    /// cover + owner header with Play / Shuffle, then the ordered track list.
    fn draw_playlist_detail_live(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            scene,
            t,
            tr,
            c,
            body,
            y,
            &page.id,
            "PLAYLIST",
            &page.title,
            &page.subtitle,
            ids.clone(),
            false,
            None,
            None,
        );
        let mut y = y;
        if !page.description.is_empty() {
            let line = elide(
                tr,
                &page.description,
                13.0,
                TextWeight::Regular,
                (body.width() - 2.0 * pad).min(640.0),
            );
            draw_text(
                tr,
                scene,
                t,
                &line,
                body.x0 + pad,
                y,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            y += 24.0;
        }
        if ids.is_empty() {
            draw_text(
                tr,
                scene,
                t,
                "This playlist has no playable tracks.",
                body.x0 + pad,
                y + 6.0,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true, None)
    }

    // ---- queue ------------------------------------------------------------

    /// A small floating card previewing the next few queued songs, shown while
    /// the pointer hovers the now-playing bar's queue button. Anchored just above
    /// the button and right-aligned to it.
    fn draw_queue_preview(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
    ) {
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

        fill_rrect(scene, t, panel, 12.0, c.panel);
        scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(panel, 12.0));
        // Swallow clicks landing on the card so they don't fall through to the
        // content behind it.
        self.push_hit(panel, Action::None);

        draw_text(
            tr,
            scene,
            t,
            "Up next",
            panel.x0 + 14.0,
            panel.y0 + 8.0,
            12.0,
            c.dim,
            TextWeight::Semibold,
            None,
        );
        let mut ry = panel.y0 + head_h;
        if rows.is_empty() {
            draw_text(
                tr,
                scene,
                t,
                "Nothing queued",
                panel.x0 + 14.0,
                ry + 10.0,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            return;
        }
        for tk in &rows {
            let art = Rect::new(panel.x0 + 8.0, ry + 5.0, panel.x0 + 40.0, ry + 37.0);
            self.draw_cover(scene, t, art, &tk.cover_id(), &tk.title, tr, 6.0);
            let tx = art.x1 + 10.0;
            let maxw = panel.x1 - tx - 12.0;
            let title = elide(tr, &tk.title, 13.0, TextWeight::Medium, maxw);
            let artist = elide(tr, &tk.artist, 11.0, TextWeight::Regular, maxw);
            draw_text(
                tr,
                scene,
                t,
                &title,
                tx,
                ry + 7.0,
                13.0,
                c.text,
                TextWeight::Medium,
                None,
            );
            draw_text(
                tr,
                scene,
                t,
                &artist,
                tx,
                ry + 24.0,
                11.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            ry += row_h;
        }
    }

    fn draw_queue(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        y += 20.0;
        y = self.draw_queue_now_playing(scene, t, tr, c, body, y);
        // Up next.
        let upnext: Vec<String> = self
            .pb
            .queue
            .iter()
            .skip(self.pb.queue_index + 1)
            .cloned()
            .collect();
        draw_text(
            tr,
            scene,
            t,
            &format!("Next up · {} tracks", upnext.len()),
            body.x0 + pad,
            y,
            15.0,
            c.dim,
            TextWeight::Semibold,
            None,
        );
        y += 28.0;
        let rows = self.gather_tracks(&upnext);
        // Build a queue context: index offset by current+1.
        let full = self.pb.queue.clone();
        let offset = self.pb.queue_index + 1;
        let y2 = self.draw_track_rows_with_offset(scene, t, tr, c, body, y, &rows, &full, offset);
        if upnext.is_empty() {
            draw_text(
                tr,
                scene,
                t,
                "Queue is empty — play an album or playlist.",
                body.x0 + pad,
                y + 6.0,
                13.0,
                c.dim,
                TextWeight::Regular,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        let Some(cur) = self.pb.current.clone() else {
            return y;
        };
        draw_text(
            tr,
            scene,
            t,
            "Now playing",
            body.x0 + pad,
            y,
            15.0,
            c.dim,
            TextWeight::Semibold,
            None,
        );
        y += 28.0;
        let row = self.gather_tracks(std::slice::from_ref(&cur));
        let Some(tk) = row.first() else {
            return y;
        };
        let rr = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
        fill_rrect(scene, t, rr, 10.0, c.accent.multiply_alpha(0.12));
        self.draw_cover(
            scene,
            t,
            Rect::new(rr.x0 + 8.0, rr.y0 + 8.0, rr.x0 + 56.0, rr.y0 + 56.0),
            &tk.cover_id(),
            &tk.title,
            tr,
            8.0,
        );
        draw_text(
            tr,
            scene,
            t,
            &tk.title,
            rr.x0 + 68.0,
            rr.y0 + 14.0,
            15.0,
            c.accent,
            TextWeight::Semibold,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            &tk.artist,
            rr.x0 + 68.0,
            rr.y0 + 36.0,
            13.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        y + 80.0
    }

    fn draw_track_rows_with_offset(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        rows: &[TrackDisp],
        full_queue: &[String],
        offset: usize,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        let qlen = full_queue.len();
        let dur_x = right - 48.0;
        let rm_x = dur_x - 34.0;
        let dn_x = rm_x - 30.0;
        let up_x = dn_x - 30.0;
        for (i, tk) in rows.iter().enumerate() {
            let abs = offset + i; // absolute queue index
            let rr = Rect::new(left, y, right, y + row_h);
            let hovered = self.body_hit(
                rr,
                Action::Play {
                    queue: full_queue.to_vec(),
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
                fill_rrect(scene, t, rr, 8.0, c.white5);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.draw_queue_row_content(
                    scene, t, tr, c, y, row_h, left, tk, hovered, abs, offset, qlen, up_x, dn_x,
                    rm_x, dur_x,
                );
            }
            y += row_h;
        }
        y
    }

    /// One queue row's title/artist, reorder/remove-on-hover, and duration.
    fn draw_queue_row_content(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        y: f64,
        row_h: f64,
        left: f64,
        tk: &TrackDisp,
        hovered: bool,
        abs: usize,
        offset: usize,
        qlen: usize,
        up_x: f64,
        dn_x: f64,
        rm_x: f64,
        dur_x: f64,
    ) {
        draw_text(
            tr,
            scene,
            t,
            &tk.title,
            left + 8.0,
            y + 9.0,
            14.0,
            c.text,
            TextWeight::Medium,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            &tk.artist,
            left + 8.0,
            y + 28.0,
            12.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        // Reorder + remove on hover.
        if hovered {
            if abs > offset {
                let uc = Point::new(up_x, y + row_h / 2.0);
                let uh = self.push_hit(
                    square_around(uc, 26.0),
                    Action::MoveQueueItem {
                        from: abs,
                        to: abs - 1,
                    },
                );
                draw_chevron(scene, t, uc, true, if uh { c.text } else { c.dim });
            }
            if abs + 1 < qlen {
                let dc = Point::new(dn_x, y + row_h / 2.0);
                let dh = self.push_hit(
                    square_around(dc, 26.0),
                    Action::MoveQueueItem {
                        from: abs,
                        to: abs + 1,
                    },
                );
                draw_chevron(scene, t, dc, false, if dh { c.text } else { c.dim });
            }
            let rc = Point::new(rm_x, y + row_h / 2.0);
            let rh = self.push_hit(square_around(rc, 26.0), Action::RemoveFromQueue(abs));
            draw_x(scene, t, rc, if rh { c.accent } else { c.dim });
        }
        let dur = ui::format::timecode(u64::from(tk.dur));
        let (dw, _) = tr.measure_weighted(&dur, 13.0, None, TextWeight::Regular);
        draw_text(
            tr,
            scene,
            t,
            &dur,
            dur_x + (40.0 - f64::from(dw)),
            vy(y, row_h, 13.0),
            13.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
    }

    // ---- downloads --------------------------------------------------------

    fn draw_downloads(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        y += 16.0;
        let cached = self.lib.downloaded_tracks();
        let count = cached.len();
        let total_s: u64 = cached.iter().map(|tk| u64::from(tk.duration_s)).sum();
        let ids: Vec<String> = cached.iter().map(|tk| tk.id.clone()).collect();

        // Stat banner: only real facts — how many tracks are marked to keep
        // offline and their combined runtime. (The bytes are cached lazily by
        // the streaming layer on play, so we don't fabricate a size here.)
        let banner = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
        fill_rrect(scene, t, banner, 12.0, c.card);
        icon::draw(
            scene,
            t,
            Icon::Downloaded,
            Point::new(banner.x0 + 34.0, banner.center().y),
            24.0,
            c.success,
        );
        draw_text(
            tr,
            scene,
            t,
            &format!("{count} songs kept for offline"),
            banner.x0 + 60.0,
            banner.y0 + 14.0,
            15.0,
            c.text,
            TextWeight::Semibold,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            &ui::format::timecode(total_s),
            banner.x0 + 60.0,
            banner.y0 + 36.0,
            12.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        y += 84.0;

        if ids.is_empty() {
            draw_text(
                tr,
                scene,
                t,
                "Nothing kept offline yet. Tap the download icon on any track.",
                body.x0 + pad,
                y,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, false, None)
    }

    // ---- liked songs ------------------------------------------------------

    fn draw_liked(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        let ids = self.lib.liked_ids();
        let total_s: u64 = ids
            .iter()
            .filter_map(|id| self.lib.track(id))
            .map(|tk| u64::from(tk.duration_s))
            .sum();

        y = self.draw_liked_banner(scene, t, tr, c, body, y, &ids, total_s);

        if ids.is_empty() {
            draw_text(
                tr,
                scene,
                t,
                "No liked songs yet. Tap the heart on any track.",
                body.x0 + pad,
                y,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            return y + 40.0;
        }
        y = self.draw_liked_download_all(scene, t, tr, c, body, y, &ids);
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true, None)
    }

    /// Banner with a heart sigil, title, song count/total time, and (when
    /// non-empty) a play-all FAB. Returns the y just below the banner.
    fn draw_liked_banner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            t,
            &rrect(banner, 14.0),
        );
        scene.fill(Fill::NonZero, t, base, None, &banner);
        scene.fill(
            Fill::NonZero,
            t,
            accent.multiply_alpha(0.7),
            None,
            &Circle::new(Point::new(banner.x1 - 40.0, banner.y1), 80.0),
        );
        scene.pop_layer();
        icon::draw(
            scene,
            t,
            Icon::HeartFilled,
            Point::new(banner.x0 + 50.0, banner.center().y),
            44.0,
            Color::WHITE,
        );
        draw_text(
            tr,
            scene,
            t,
            "Liked Songs",
            banner.x0 + 92.0,
            banner.y0 + 22.0,
            24.0,
            Color::WHITE,
            TextWeight::Bold,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            &format!("{} songs · {}", ids.len(), ui::format::timecode(total_s)),
            banner.x0 + 92.0,
            banner.y0 + 54.0,
            14.0,
            Color::WHITE.multiply_alpha(0.85),
            TextWeight::Regular,
            None,
        );
        // Play-all FAB.
        if !ids.is_empty() {
            let fab = Circle::new(Point::new(banner.x1 - 44.0, banner.center().y), 22.0);
            let ph = self.push_hit(
                square_around(fab.center, 50.0),
                Action::Play {
                    queue: ids.to_vec(),
                    index: 0,
                },
            );
            scene.fill(
                Fill::NonZero,
                t,
                if ph { c.accent_hover } else { c.accent },
                None,
                &fab,
            );
            icon::draw(
                scene,
                t,
                Icon::Play,
                Point::new(fab.center.x + 1.0, fab.center.y),
                18.0,
                c.bg,
            );
        }
        y + 16.0 + 88.0 + 16.0
    }

    /// Download-all pill (keep every liked song offline in one gesture).
    /// Returns the y below it.
    fn draw_liked_download_all(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        ids: &[String],
    ) -> f64 {
        let pad = 28.0;
        let dl = Rect::new(body.x0 + pad, y - 4.0, body.x0 + pad + 128.0, y + 28.0);
        let dlh = self.push_hit(dl, Action::DownloadAll(ids.to_vec()));
        scene.stroke(
            &Stroke::new(1.0),
            t,
            if dlh { c.text } else { c.border },
            None,
            &rrect(dl, 16.0),
        );
        icon::draw(
            scene,
            t,
            Icon::Download,
            Point::new(dl.x0 + 20.0, dl.center().y),
            15.0,
            if dlh { c.text } else { c.dim },
        );
        draw_text(
            tr,
            scene,
            t,
            "Download all",
            dl.x0 + 36.0,
            vy(dl.y0, 32.0, 13.0),
            13.0,
            if dlh { c.text } else { c.dim },
            TextWeight::Medium,
            None,
        );
        y + 44.0
    }

    // ---- lyrics -----------------------------------------------------------

    fn draw_lyrics(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
    ) -> f64 {
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
            Self::draw_lyrics_no_track(scene, t, tr, c, body);
            return bottom;
        };

        self.draw_lyrics_synced_badge(scene, t, tr, c, body, top);

        let band_top = top + 70.0;
        let band_bot = bottom;
        let region =
            self.draw_lyrics_now_playing(scene, t, tr, c, body, top, band_top, band_bot, &tk);

        if self.lyrics.is_empty() {
            self.draw_lyrics_empty(scene, t, tr, c, region, band_top, band_bot);
            return bottom;
        }

        self.draw_lyrics_lines(scene, t, tr, c, region, band_top, band_bot, pad);
        bottom
    }

    /// "Play a track to see its lyrics." — no track is playing at all.
    fn draw_lyrics_no_track(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
    ) {
        let msg = "Play a track to see its lyrics.";
        let (tw, _) = tr.measure_weighted(msg, 15.0, None, TextWeight::Regular);
        draw_text(
            tr,
            scene,
            t,
            msg,
            body.x0 + (body.width() - f64::from(tw)) / 2.0,
            body.center().y,
            15.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
    }

    /// Time-synced badge (top-right), shown once the current track has
    /// synced lyrics.
    fn draw_lyrics_synced_badge(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        top: f64,
    ) {
        if !self.lyrics_synced || self.lyrics.is_empty() {
            return;
        }
        let pad = 28.0;
        let tag = "SYNCED";
        let (tw, _) = tr.measure_weighted(tag, 10.0, None, TextWeight::Semibold);
        let pill = Rect::new(
            body.x1 - pad - f64::from(tw) - 20.0,
            top - 2.0,
            body.x1 - pad,
            top + 18.0,
        );
        fill_rrect(scene, t, pill, 9.0, c.accent.multiply_alpha(0.16));
        draw_text(
            tr,
            scene,
            t,
            tag,
            pill.x0 + 10.0,
            vy(pill.y0, 20.0, 10.0),
            10.0,
            c.accent,
            TextWeight::Semibold,
            None,
        );
    }

    /// Two-column composition on wide bodies: a now-playing card (cover art +
    /// track info) fills the left column and the lyric lines centre within
    /// the right column — so the view is balanced instead of a centred column
    /// with a large empty left half. Narrow bodies keep one full-width
    /// centred column with the track title top-left. Returns the lyrics
    /// column region.
    fn draw_lyrics_now_playing(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        top: f64,
        band_top: f64,
        band_bot: f64,
        tk: &TrackDisp,
    ) -> Rect {
        let pad = 28.0;
        let layout = lyrics_layout(body, band_top, band_bot);
        let region = layout.lyrics;
        if let Some(art) = layout.art {
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(scene, t, art, &cover, &tk.title, tr, 18.0);
            let cx = art.center().x;
            let (tw, _) = tr.measure_weighted(&tk.title, 20.0, None, TextWeight::Bold);
            draw_text(
                tr,
                scene,
                t,
                &tk.title,
                cx - f64::from(tw) / 2.0,
                art.y1 + 30.0,
                20.0,
                c.text,
                TextWeight::Bold,
                None,
            );
            let (aw, _) = tr.measure_weighted(&tk.artist, 14.0, None, TextWeight::Regular);
            draw_text(
                tr,
                scene,
                t,
                &tk.artist,
                cx - f64::from(aw) / 2.0,
                art.y1 + 56.0,
                14.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        } else {
            draw_text(
                tr,
                scene,
                t,
                &tk.title,
                body.x0 + pad,
                top,
                22.0,
                c.text,
                TextWeight::Bold,
                None,
            );
            draw_text(
                tr,
                scene,
                t,
                &tk.artist,
                body.x0 + pad,
                top + 30.0,
                14.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
        region
    }

    /// Clean "no lyrics" empty state — NO dev/compile-feature text.
    fn draw_lyrics_empty(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        region: Rect,
        band_top: f64,
        band_bot: f64,
    ) {
        let msg = if self.lyrics_pending_for.is_some() {
            "Finding lyrics\u{2026}"
        } else {
            "No lyrics found for this track."
        };
        let (tw, _) = tr.measure_weighted(msg, 15.0, None, TextWeight::Regular);
        draw_text(
            tr,
            scene,
            t,
            msg,
            region.x0 + (region.width() - f64::from(tw)) / 2.0,
            f64::midpoint(band_top, band_bot),
            15.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
    }

    /// The scrolling, auto-synced lyric lines, clipped to `region`.
    fn draw_lyrics_lines(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        region: Rect,
        band_top: f64,
        band_bot: f64,
        pad: f64,
    ) {
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
        let clip = Rect::new(region.x0, band_top - 6.0, region.x1, band_bot);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        let maxw = (region.width() - 2.0 * pad).clamp(1.0, 620.0);
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
            let (tw, _) = tr.measure_weighted(text, size, Some(maxw as f32), weight);
            let x = region.x0 + (region.width() - f64::from(tw)) / 2.0;
            draw_text(
                tr,
                scene,
                t,
                text,
                x,
                ly,
                size,
                col,
                weight,
                Some(maxw as f32),
            );
        }
        scene.pop_layer();
    }

    // ---- search -----------------------------------------------------------

    fn draw_search(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let q = self.search_input.value().to_string();
        if q.trim().is_empty() {
            return self.draw_search_browse_all(scene, t, tr, c, body, y);
        }

        let searching = self.live_search_pending.as_deref() == Some(q.trim());
        let results = self.gather_search_results(q.trim());

        if let Some((failed_query, error)) = &self.search_error
            && failed_query == q.trim()
        {
            let message = elide(tr, error, 12.0, TextWeight::Regular, body.width() - 210.0);
            let banner = Rect::new(body.x0 + 28.0, y + 8.0, body.x1 - 28.0, y + 48.0);
            fill_rrect(scene, t, banner, 10.0, c.accent.multiply_alpha(0.13));
            draw_text(
                tr,
                scene,
                t,
                &format!("Couldn’t search live music: {message}"),
                banner.x0 + 12.0,
                vy(banner.y0, 40.0, 12.0),
                12.0,
                c.text,
                TextWeight::Regular,
                Some((banner.width() - 118.0) as f32),
            );
            let retry = Rect::new(
                banner.x1 - 92.0,
                banner.y0 + 5.0,
                banner.x1 - 6.0,
                banner.y1 - 5.0,
            );
            let hover = self.push_hit(retry, Action::RetrySearch);
            fill_rrect(
                scene,
                t,
                retry,
                15.0,
                if hover { c.accent_hover } else { c.accent },
            );
            draw_text(
                tr,
                scene,
                t,
                "Try again",
                retry.x0 + 12.0,
                vy(retry.y0, retry.height(), 12.0),
                12.0,
                c.bg,
                TextWeight::Semibold,
                None,
            );
            y += 56.0;
        }

        if results.is_empty() {
            return self.draw_search_no_results(scene, t, tr, c, body, y, q.trim(), searching);
        }

        // Spotify-style filter tabs.
        y = self.draw_search_tabs(scene, t, tr, c, body, y + 6.0, &results);
        y += 6.0;

        match self.search_tab {
            SearchTab::All => self.draw_search_all(scene, t, tr, c, body, y, &results, searching),
            SearchTab::Songs => {
                if results.songs.is_empty() {
                    return self.draw_tab_empty(scene, t, tr, c, body, y, searching, "songs");
                }
                let rows = self.gather_tracks(&results.songs);
                self.draw_track_table(
                    scene,
                    t,
                    tr,
                    c,
                    body,
                    y + 4.0,
                    &rows,
                    &results.songs,
                    true,
                    true,
                    None,
                )
            }
            SearchTab::Albums => {
                if results.albums.is_empty() {
                    return self.draw_tab_empty(scene, t, tr, c, body, y, searching, "albums");
                }
                self.draw_album_grid(scene, t, tr, c, body, y + 4.0, &results.albums)
            }
            SearchTab::Artists => {
                if results.artists.is_empty() {
                    return self.draw_tab_empty(scene, t, tr, c, body, y, searching, "artists");
                }
                self.draw_artist_grid(scene, t, tr, c, body, y + 4.0, &results.artists)
            }
            SearchTab::Playlists => {
                if results.playlists.is_empty() {
                    return self.draw_tab_empty(scene, t, tr, c, body, y, searching, "playlists");
                }
                self.draw_result_playlist_grid(scene, t, tr, c, body, y + 4.0, &results.playlists)
            }
        }
    }

    /// The empty-query "Browse all" state: a genre-tile grid derived from the
    /// library's artists.
    fn draw_search_browse_all(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
    ) -> f64 {
        let pad = 28.0;
        y += 16.0;
        draw_text(
            tr,
            scene,
            t,
            "Browse all",
            body.x0 + pad,
            y,
            19.0,
            c.text,
            TextWeight::Bold,
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
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &rrect(tile, 12.0),
            );
            scene.fill(Fill::NonZero, t, base, None, &tile);
            scene.fill(
                Fill::NonZero,
                t,
                accent.multiply_alpha(0.7),
                None,
                &Circle::new(Point::new(tile.x1 - 16.0, tile.y1 - 6.0), 42.0),
            );
            // Lift the tile on hover so the affordance reads as interactive.
            if hover {
                scene.fill(
                    Fill::NonZero,
                    t,
                    Color::WHITE.multiply_alpha(0.12),
                    None,
                    &tile,
                );
            }
            scene.pop_layer();
            draw_text(
                tr,
                scene,
                t,
                g,
                cx + 16.0,
                cy + 14.0,
                16.0,
                Color::WHITE,
                TextWeight::Bold,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
        draw_text(
            tr,
            scene,
            t,
            &msg,
            body.x0 + pad,
            y,
            15.0,
            c.dim,
            TextWeight::Regular,
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
            draw_text(
                tr,
                scene,
                t,
                hint,
                body.x0 + pad,
                y + 24.0,
                12.0,
                c.dim,
                TextWeight::Regular,
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
                        queue: vec![id.clone()],
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            let (tw, _) = tr.measure_weighted(label, 13.0, None, TextWeight::Semibold);
            let tab_w = f64::from(tw) + 30.0;
            let pill = Rect::new(tab_x, y, tab_x + tab_w, y + tab_h);
            let active = self.search_tab == tab;
            let hov = self.push_hit(pill, Action::SelectSearchTab(tab));
            if active {
                fill_rrect(scene, t, pill, tab_h / 2.0, c.text);
            } else if hov {
                fill_rrect(scene, t, pill, tab_h / 2.0, c.white10);
            } else {
                scene.stroke(
                    &Stroke::new(1.0),
                    t,
                    c.border,
                    None,
                    &rrect(pill, tab_h / 2.0),
                );
            }
            let ink = if active { c.bg } else { c.text };
            draw_text(
                tr,
                scene,
                t,
                label,
                tab_x + 15.0,
                vy(y, tab_h, 13.0),
                13.0,
                ink,
                TextWeight::Semibold,
                None,
            );
            tab_x += tab_w + 10.0;
        }
        y + tab_h + 12.0
    }

    /// The curated `All` mix: a top-result hero beside the top songs, then a
    /// horizontal-ish grid each of albums, artists and playlists.
    fn draw_search_all(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        res: &SearchResultsView,
        searching: bool,
    ) -> f64 {
        y = self.draw_search_all_top(scene, t, tr, c, body, y, res, searching);
        self.draw_search_all_grids(scene, t, tr, c, body, y, res)
    }

    /// Top result hero + top songs, side by side on wide bodies (else a
    /// "Searching…" line when nothing has landed yet).
    fn draw_search_all_top(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
            y = Self::section_label(scene, t, tr, c, body, y, "Top result");
            let hero_w = if wide {
                (body.width() - 2.0 * pad) * 0.42
            } else {
                body.width() - 2.0 * pad
            };
            let hero_h = 172.0;
            let hx = body.x0 + pad;
            let hero_rect = Rect::new(hx, y, hx + hero_w, y + hero_h);
            self.draw_search_hero(scene, t, tr, c, hero_rect, hero);

            if wide && !top_songs.is_empty() {
                // Songs list to the right of the hero.
                let sx = hero_rect.x1 + 24.0;
                let song_body = Rect::new(sx - pad, body.y0, body.x1, body.y1);
                let mut sy = y - 2.0;
                draw_text(
                    tr,
                    scene,
                    t,
                    "Songs",
                    sx,
                    sy,
                    15.0,
                    c.text,
                    TextWeight::Bold,
                    None,
                );
                sy += 28.0;
                let rows = self.gather_tracks(&top_songs);
                let songs_bottom = self.draw_track_rows_with_offset(
                    scene, t, tr, c, song_body, sy, &rows, &top_songs, 0,
                );
                // Clear the taller of the two columns so neither overlaps the
                // sections below.
                y = songs_bottom.max(y + hero_h) + 20.0;
            } else {
                y += hero_h + 20.0;
                if !top_songs.is_empty() {
                    y = Self::section_label(scene, t, tr, c, body, y, "Songs");
                    let rows = self.gather_tracks(&top_songs);
                    y = self.draw_track_table(
                        scene, t, tr, c, body, y, &rows, &top_songs, true, true, None,
                    );
                    y += 8.0;
                }
            }
        } else if !top_songs.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y, "Songs");
            let rows = self.gather_tracks(&top_songs);
            y = self.draw_track_table(
                scene, t, tr, c, body, y, &rows, &top_songs, true, true, None,
            );
            y += 8.0;
        } else if searching {
            draw_text(
                tr,
                scene,
                t,
                "Searching YouTube Music\u{2026}",
                body.x0 + pad,
                y + 4.0,
                13.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
            y += 28.0;
        }
        y
    }

    /// Albums / Artists / Playlists sections, each capped at 6 results.
    fn draw_search_all_grids(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        res: &SearchResultsView,
    ) -> f64 {
        if !res.albums.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y + 4.0, "Albums");
            let take: Vec<AlbumDisp> = res.albums.iter().take(6).cloned().collect();
            y = self.draw_album_grid(scene, t, tr, c, body, y, &take);
            y += 8.0;
        }
        if !res.artists.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y + 4.0, "Artists");
            let take: Vec<ArtistDisp> = res.artists.iter().take(6).cloned().collect();
            y = self.draw_artist_grid(scene, t, tr, c, body, y, &take);
            y += 8.0;
        }
        if !res.playlists.is_empty() {
            y = Self::section_label(scene, t, tr, c, body, y + 4.0, "Playlists");
            let take: Vec<ResultPlaylist> = res.playlists.iter().take(6).cloned().collect();
            y = self.draw_result_playlist_grid(scene, t, tr, c, body, y, &take);
        }
        y
    }

    /// A placeholder line for a per-type tab whose results haven't landed yet.
    fn draw_tab_empty(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
        draw_text(
            tr,
            scene,
            t,
            &msg,
            body.x0 + pad,
            y + 12.0,
            14.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        y + 44.0
    }

    /// The large "top result" card. Clicking it plays (a song) or opens the
    /// entity's page (album / artist / playlist).
    fn draw_search_hero(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        rect: Rect,
        hero: &SearchHero,
    ) {
        let hov = self.push_hit(rect, hero.action.clone());
        fill_rrect(
            scene,
            t,
            rect,
            14.0,
            if hov { c.card_hover } else { c.card },
        );
        let art_d = rect.height() - 56.0;
        let ax = rect.x0 + 20.0;
        let ay = rect.y0 + 20.0;
        if hero.circular {
            let ctr = Point::new(ax + art_d / 2.0, ay + art_d / 2.0);
            self.draw_avatar(scene, t, ctr, art_d / 2.0, &hero.id, &hero.title, tr, c);
        } else {
            self.draw_cover(
                scene,
                t,
                Rect::new(ax, ay, ax + art_d, ay + art_d),
                &hero.id,
                &hero.title,
                tr,
                10.0,
            );
        }
        let tx = ax + art_d + 18.0;
        let maxw = (rect.x1 - tx - 20.0).max(40.0);
        let title = elide(tr, &hero.title, 26.0, TextWeight::Bold, maxw);
        draw_text(
            tr,
            scene,
            t,
            &title,
            tx,
            ay + 6.0,
            26.0,
            c.text,
            TextWeight::Bold,
            None,
        );
        let sub = elide(tr, &hero.subtitle, 13.0, TextWeight::Regular, maxw);
        draw_text(
            tr,
            scene,
            t,
            &sub,
            tx,
            ay + 44.0,
            13.0,
            c.dim,
            TextWeight::Regular,
            None,
        );

        // Kind pill bottom-left; a Play FAB bottom-right for a song hero.
        let pill_y = rect.y1 - 30.0;
        let (pw, _) = tr.measure_weighted(hero.kind_label, 12.0, None, TextWeight::Semibold);
        let pill = Rect::new(
            rect.x0 + 20.0,
            pill_y - 4.0,
            rect.x0 + 20.0 + f64::from(pw) + 24.0,
            pill_y + 20.0,
        );
        fill_rrect(scene, t, pill, 12.0, c.white10);
        draw_text(
            tr,
            scene,
            t,
            hero.kind_label,
            pill.x0 + 12.0,
            vy(pill.y0, 24.0, 12.0),
            12.0,
            c.text,
            TextWeight::Semibold,
            None,
        );
        if hero.playable {
            let fab = Circle::new(Point::new(rect.x1 - 30.0, rect.y1 - 30.0), 20.0);
            scene.fill(Fill::NonZero, t, c.accent, None, &fab);
            icon::draw(
                scene,
                t,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            self.draw_cover(scene, t, art_rect, &p.id, &p.title, tr, 12.0);
            icon::draw(
                scene,
                t,
                Icon::Playlist,
                Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0),
                22.0,
                c.text.multiply_alpha(0.85),
            );
            let title = elide(tr, &p.title, 14.0, TextWeight::Semibold, card_w);
            draw_text(
                tr,
                scene,
                t,
                &title,
                cx,
                cy + card_w + 8.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            let sub = elide(tr, &p.subtitle, 12.0, TextWeight::Regular, card_w);
            draw_text(
                tr,
                scene,
                t,
                &sub,
                cx,
                cy + card_w + 28.0,
                12.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
        let rows = pls.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- now playing bar --------------------------------------------------

    fn draw_nowplaying(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
    ) {
        scene.fill(Fill::NonZero, t, c.panel, None, &r);
        scene.fill(
            Fill::NonZero,
            t,
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
            self.draw_nowplaying_compact(scene, t, tr, c, r);
            return;
        }

        self.draw_nowplaying_left(scene, t, tr, c, r);

        // Right block reserved for queue + volume. As the bar narrows the
        // volume slider shrinks toward a floor, then collapses to a button that
        // opens a small popover (see below); on very narrow widths the whole
        // bar drops to the compact transport (no volume) above.
        let w = r.width();
        let show_slider = w >= 900.0;
        let slider_w = ((w - 720.0) * 0.32).clamp(58.0, 150.0);
        let right_w = if show_slider { slider_w + 118.0 } else { 108.0 };
        let center = Rect::new(r.x0 + 300.0, r.y0, r.x1 - right_w, r.y1);

        self.draw_nowplaying_transport(scene, t, c, r, center);
        self.draw_nowplaying_scrubber(scene, t, tr, c, r, center);
        self.draw_nowplaying_right(scene, t, c, r, right_w, show_slider);
    }

    /// Left: art + title/artist + like + lyrics affordance (or "Nothing
    /// playing" when the queue is empty).
    fn draw_nowplaying_left(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
    ) {
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());

        let lx = r.x0 + 16.0;
        if let Some(tk) = &cur {
            let art = Rect::new(lx, r.y0 + 14.0, lx + 64.0, r.y0 + 78.0);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(scene, t, art, &cover, &tk.title, tr, 8.0);
            // Clicking the art jumps to its album.
            self.push_hit(art, Action::Nav(View::Album(tk.album_id.clone())));
            let tx = art.x1 + 14.0;
            // Reserve room on the right of the title block for the heart +
            // lyrics buttons so they never overlap the title text.
            let heart_c = Point::new(r.x0 + 300.0 - 48.0, r.y0 + 30.0);
            let lyr_c = Point::new(r.x0 + 300.0 - 22.0, r.y0 + 30.0);
            let maxw = (heart_c.x - tx - 28.0).max(40.0);
            let title = elide(tr, &tk.title, 14.0, TextWeight::Semibold, maxw);
            draw_text(
                tr,
                scene,
                t,
                &title,
                tx,
                r.y0 + 26.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            let secondary = self.playback_error.as_deref().unwrap_or(&tk.artist);
            let artist = elide(tr, secondary, 12.0, TextWeight::Regular, maxw);
            draw_text(
                tr,
                scene,
                t,
                &artist,
                tx,
                r.y0 + 48.0,
                12.0,
                if self.playback_error.is_some() {
                    c.accent
                } else {
                    c.dim
                },
                TextWeight::Regular,
                None,
            );

            self.draw_nowplaying_left_shortcuts(scene, t, c, &tk.id, heart_c, lyr_c);
        } else {
            draw_text(
                tr,
                scene,
                t,
                "Nothing playing",
                lx,
                vy(r.y0, NOWPLAYING_H, 14.0),
                14.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    /// Heart / like toggle + Lyrics shortcut, next to the now-playing title.
    fn draw_nowplaying_left_shortcuts(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        track_id: &str,
        heart_c: Point,
        lyr_c: Point,
    ) {
        let liked = self.lib.is_liked(track_id);
        let hh = self.push_hit(
            square_around(heart_c, 28.0),
            Action::ToggleLike(track_id.to_string()),
        );
        icon::draw(
            scene,
            t,
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
            scene,
            t,
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
        scene: &mut Scene,
        t: Affine,
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
            scene,
            t,
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
            scene,
            t,
            Icon::Prev,
            prev_c,
            18.0,
            if pv { c.text } else { c.dim_strong },
        );
        // play/pause big
        let play_c = Point::new(cxm, btn_y);
        let pl = self.push_hit(square_around(play_c, 40.0), Action::TogglePlay);
        scene.fill(
            Fill::NonZero,
            t,
            if pl {
                c.text
            } else {
                c.text.multiply_alpha(0.94)
            },
            None,
            &Circle::new(play_c, 20.0),
        );
        if self.pb.playing {
            icon::draw(scene, t, Icon::Pause, play_c, 16.0, c.panel);
        } else {
            icon::draw(
                scene,
                t,
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
            scene,
            t,
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
        icon::draw(scene, t, rep_icon, rep_c, 18.0, rep_col);
    }

    /// The elapsed/total timecodes + draggable progress track.
    fn draw_nowplaying_scrubber(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
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
        let (ew, _) = tr.measure_weighted(&elapsed, 11.0, None, TextWeight::Regular);
        let bar_x0 = center.x0 + 48.0;
        let bar_x1 = center.x1 - 48.0;
        draw_text(
            tr,
            scene,
            t,
            &elapsed,
            bar_x0 - f64::from(ew) - 10.0,
            bar_y - 6.0,
            11.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        draw_text(
            tr,
            scene,
            t,
            &total,
            bar_x1 + 10.0,
            bar_y - 6.0,
            11.0,
            c.dim,
            TextWeight::Regular,
            None,
        );
        let track = Rect::new(bar_x0, bar_y, bar_x1, bar_y + 4.0);
        self.scrubber_rect = Rect::new(bar_x0, bar_y - 8.0, bar_x1, bar_y + 12.0);
        fill_rrect(scene, t, track, 2.0, c.white10);
        let fillx = bar_x0 + (bar_x1 - bar_x0) * frac;
        if fillx > bar_x0 {
            fill_rrect(
                scene,
                t,
                Rect::new(bar_x0, bar_y, fillx, bar_y + 4.0),
                2.0,
                c.accent,
            );
        }
        let knob_hover =
            self.scrubber_rect.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        if knob_hover {
            scene.fill(
                Fill::NonZero,
                t,
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
        scene: &mut Scene,
        t: Affine,
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
            scene,
            t,
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
                scene,
                t,
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
                scene,
                t,
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
                fill_rrect(scene, t, btn, 9.0, c.white10);
            }
            let icon = if muted {
                Icon::VolumeMute
            } else {
                Icon::VolumeHigh
            };
            icon::draw(
                scene,
                t,
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
    fn draw_volume_slider(&mut self, scene: &mut Scene, t: Affine, c: &Palette, hit: Rect) {
        self.volume_rect = hit;
        let vol = f64::from(self.pb.volume) / 100.0;
        let vbar_y = hit.center().y;
        let x0 = hit.x0;
        let x1 = hit.x1;
        let vtrack = Rect::new(x0, vbar_y - 2.0, x1, vbar_y + 2.0);
        fill_rrect(scene, t, vtrack, 2.0, c.white10);
        let vfill = x0 + (x1 - x0) * vol;
        if vfill > x0 {
            fill_rrect(
                scene,
                t,
                Rect::new(x0, vbar_y - 2.0, vfill, vbar_y + 2.0),
                2.0,
                c.accent,
            );
        }
        scene.fill(
            Fill::NonZero,
            t,
            c.text,
            None,
            &Circle::new(Point::new(vfill, vbar_y), 5.0),
        );
    }

    /// The collapsed-volume popover: a small floating panel with a horizontal
    /// slider + a mute toggle, anchored above the volume button. Drawn on top of
    /// the bar (after it) so it never clips. Uses the shared drag target so the
    /// existing scrubber/volume drag logic drives it.
    fn draw_volume_popover(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
    ) {
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
        fill_rrect(scene, t, panel, 12.0, c.toast_bg);
        scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(panel, 12.0));
        // Block clicks inside the panel from falling through.
        self.push_hit(panel, Action::None);

        let muted = self.pb.volume == 0;
        let icon_c = Point::new(panel.x0 + 24.0, panel.center().y);
        let mh = self.push_hit(square_around(icon_c, 30.0), Action::ToggleMute);
        icon::draw(
            scene,
            t,
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
        let (pw_txt, _) = tr.measure_weighted(&pct, 12.0, None, TextWeight::Medium);
        draw_text(
            tr,
            scene,
            t,
            &pct,
            panel.x1 - 16.0 - f64::from(pw_txt),
            vy(panel.y0, ph, 12.0),
            12.0,
            c.dim,
            TextWeight::Medium,
            None,
        );
        let slider = Rect::new(
            icon_c.x + 20.0,
            panel.center().y - 8.0,
            panel.x1 - 44.0,
            panel.center().y + 8.0,
        );
        self.volume_popover_slider = slider;
        self.draw_volume_slider(scene, t, c, slider);
    }

    /// Compact now-playing transport for narrow windows: art + title on the
    /// left, prev/play/next on the right, a full-width progress line (also the
    /// scrubber) along the top edge. Volume + queue are dropped (reachable from
    /// the sidebar) so nothing overlaps.
    fn draw_nowplaying_compact(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
    ) {
        let cur = self
            .pb
            .current
            .clone()
            .and_then(|id| self.gather_tracks(&[id]).into_iter().next());

        self.draw_nowplaying_compact_progress(scene, t, c, r);

        let controls_w = 132.0;
        self.draw_nowplaying_compact_left(scene, t, tr, c, r, controls_w, cur.as_ref());
        self.draw_nowplaying_compact_transport(scene, t, c, r, controls_w);
    }

    /// Scrub / progress track: a proper rounded bar lifted off the very
    /// bottom edge so it's clearly visible and grabbable — not a hairline
    /// glued to the window edge. The hit strip still reaches the bottom edge
    /// so an edge-of-window press seeks too, while its top clears the
    /// transport buttons.
    fn draw_nowplaying_compact_progress(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        r: Rect,
    ) {
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
        fill_rrect(scene, t, track, track_h / 2.0, c.white10);
        let fillx = track_x0 + (track_x1 - track_x0) * frac;
        if fillx > track_x0 {
            fill_rrect(
                scene,
                t,
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
            scene.fill(
                Fill::NonZero,
                t,
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
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        r: Rect,
        controls_w: f64,
        cur: Option<&TrackDisp>,
    ) {
        let lx = r.x0 + 14.0;
        if let Some(tk) = cur {
            let art = Rect::new(lx, r.y0 + 16.0, lx + 60.0, r.y0 + 76.0);
            let cover = self.best_cover_id(&tk.id, &tk.album_id);
            self.draw_cover(scene, t, art, &cover, &tk.title, tr, 8.0);
            self.push_hit(art, Action::Nav(View::Lyrics));
            let tx = art.x1 + 12.0;
            let maxw = (r.x1 - controls_w - tx - 10.0).max(40.0);
            let title = elide(tr, &tk.title, 14.0, TextWeight::Semibold, maxw);
            draw_text(
                tr,
                scene,
                t,
                &title,
                tx,
                r.y0 + 28.0,
                14.0,
                c.text,
                TextWeight::Semibold,
                None,
            );
            let artist = elide(tr, &tk.artist, 12.0, TextWeight::Regular, maxw);
            draw_text(
                tr,
                scene,
                t,
                &artist,
                tx,
                r.y0 + 50.0,
                12.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        } else {
            draw_text(
                tr,
                scene,
                t,
                "Nothing playing",
                lx,
                vy(r.y0, NOWPLAYING_H, 14.0),
                14.0,
                c.dim,
                TextWeight::Regular,
                None,
            );
        }
    }

    /// Prev / play-pause / next, right-aligned within `controls_w`.
    fn draw_nowplaying_compact_transport(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        c: &Palette,
        r: Rect,
        controls_w: f64,
    ) {
        let cy = r.y0 + NOWPLAYING_H / 2.0 + 2.0;
        let px = r.x1 - controls_w;
        let prev_c = Point::new(px + 18.0, cy);
        let pv = self.push_hit(square_around(prev_c, 32.0), Action::Prev);
        icon::draw(
            scene,
            t,
            Icon::Prev,
            prev_c,
            18.0,
            if pv { c.text } else { c.dim_strong },
        );
        let play_c = Point::new(px + 62.0, cy);
        let pl = self.push_hit(square_around(play_c, 42.0), Action::TogglePlay);
        scene.fill(
            Fill::NonZero,
            t,
            if pl {
                c.text
            } else {
                c.text.multiply_alpha(0.94)
            },
            None,
            &Circle::new(play_c, 20.0),
        );
        if self.pb.playing {
            icon::draw(scene, t, Icon::Pause, play_c, 16.0, c.panel);
        } else {
            icon::draw(
                scene,
                t,
                Icon::Play,
                Point::new(play_c.x + 1.0, play_c.y),
                16.0,
                c.panel,
            );
        }
        let next_c = Point::new(px + 106.0, cy);
        let nx = self.push_hit(square_around(next_c, 32.0), Action::Next);
        icon::draw(
            scene,
            t,
            Icon::Next,
            next_c,
            18.0,
            if nx { c.text } else { c.dim_strong },
        );
    }

    // ---- toast ------------------------------------------------------------

    fn draw_toast(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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
            scene,
            t,
            tr,
            theme,
            area,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, ToastKind::Info),
        );
    }

    // ---- gather helpers (own data so draw can borrow &mut self) -----------

    fn gather_tracks(&self, ids: &[String]) -> Vec<TrackDisp> {
        ids.iter()
            .filter_map(|id| self.lib.track(id))
            .map(|tk| TrackDisp {
                id: tk.id.clone(),
                title: tk.title.clone(),
                artist: self.lib.artist_name(&tk.artist_id).to_string(),
                album: self.lib.album_title(&tk.album_id).to_string(),
                album_id: tk.album_id.clone(),
                dur: tk.duration_s,
                cache: self.lib.cache_state(&tk.id),
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
    /// Seed for procedural art — keyed to the album id so an album's tracks
    /// share a cover with the grid/detail art.
    #[allow(dead_code)]
    fn album_seed(&self) -> String {
        self.album_id.clone()
    }

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
fn draw_chevron(scene: &mut Scene, t: Affine, center: Point, up: bool, color: Color) {
    let s = 5.0;
    let dy = if up { -1.0 } else { 1.0 };
    let mut p = vello::kurbo::BezPath::new();
    p.move_to((center.x - s, center.y + dy * s * 0.5));
    p.line_to((center.x, center.y - dy * s * 0.5));
    p.line_to((center.x + s, center.y + dy * s * 0.5));
    scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        &p,
    );
}

/// A small "x" (remove) glyph.
fn draw_x(scene: &mut Scene, t: Affine, center: Point, color: Color) {
    let s = 5.0;
    let a = vello::kurbo::Line::new((center.x - s, center.y - s), (center.x + s, center.y + s));
    let b = vello::kurbo::Line::new((center.x + s, center.y - s), (center.x - s, center.y + s));
    scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        &a,
    );
    scene.stroke(
        &Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        &b,
    );
}

/// Theme-derived palette resolved once per frame.
struct Palette {
    bg: Color,
    panel: Color,
    card: Color,
    card_hover: Color,
    text: Color,
    dim: Color,
    dim_strong: Color,
    accent: Color,
    accent_hover: Color,
    success: Color,
    border: Color,
    white5: Color,
    white10: Color,
    toast_bg: Color,
    scrim: Color,
}

impl Palette {
    fn from(theme: &Theme) -> Self {
        let col = &theme.tokens.colors;
        Self {
            bg: col.neutral[0],
            panel: col.surfaces.base,
            card: col.surfaces.raised,
            card_hover: col.neutral[3].multiply_alpha(0.6),
            text: theme.text_primary,
            dim: theme.text_dim,
            dim_strong: col.neutral[7],
            accent: theme.accent_primary,
            accent_hover: theme.accent_hover,
            success: theme.status_success,
            border: theme.border_standard,
            white5: col.lift(0.35),
            white10: col.lift(0.72),
            toast_bg: col.surfaces.raised,
            scrim: col.surfaces.overlay,
        }
    }
}
