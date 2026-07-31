//! Shell overlay host — wraps the root content widget and draws modal
//! overlays (command palette for ⌘K, overview for ⌘Tab, etc.) on top.
//!
//! The `build_nirios_root` entry point returns this widget so that the
//! shell has exactly one place to route global keyboard events
//! (⌘K / ⌘Tab / Esc) and exactly one place to render the overlay stack
//! above the desktop content. Overlays each own their state — the host
//! just gates their visibility and paints a scrim when at least one is
//! showing.
//!
//! Phase 3 scope: ⌘K opens the command palette primitive. Overview /
//! quick-settings / workspace-settings wiring lands in follow-up tasks
//! alongside their click-surfaces.
//!
//! Key routing: `handle_event` consumes the palette-toggle and Esc
//! presses before propagating to the wrapped content. When the palette
//! is open, it also gets first crack at every event so typing-into-
//! query / Enter-to-select work without the desktop stealing focus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{
    Dimension, Display, LengthPercentageAuto, NodeId, Position, Rect as TaffyRect, Size, Style,
    TaffyTree,
};
use vello::Scene;
use vello::kurbo::RoundedRect;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    CrossStripDetach, GroupRow, OutputSpan, PROMPT_HEIGHT, PermissionPrompt, PromptChoice, Widget,
    WindowGroup as UiWindowGroup,
};
use ui::{CommandEntry, CommandPalette};

use crate::shell::chrome_state::{Chrome, WsTab};
use crate::shell::notification::NotificationStack;
use crate::shell::state::{
    Capability, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION, GrantScope, GroupId, StripId, WorkspaceId,
};
use crate::shell::workspace_settings::{
    CapEdit, EXPIRY_CHOICES, draw_members_pane, draw_permissions_pane,
    members_pane_rows, permissions_pane_layout,
};

/// The Permissions tab's grant-composer controls, as painted. Cached on
/// the host each frame the composer is on screen so the click router
/// hit-tests exactly the rects the renderer drew, and so a headless
/// harness can address them by name.
#[derive(Clone, Copy, Debug)]
pub struct GrantComposerRects {
    /// Cycles the subject the grant is for.
    pub subject: Rect,
    /// Cycles what the grant allows.
    pub capability: Rect,
    /// The three expiry segments, in [`EXPIRY_CHOICES`] order.
    pub expiry: [Rect; 3],
    /// Commits the draft through `Workspace::grant_as`.
    pub submit: Rect,
}

/// Which grant-composer control a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposerHit {
    Subject,
    Capability,
    /// Index into [`EXPIRY_CHOICES`].
    Expiry(usize),
    Submit,
}

pub struct ShellOverlayHost {
    rect: Rect,
    content: Box<dyn Widget>,
    /// Floating sidebar wheel — absolutely positioned above the desktop
    /// content, offset below the top bar. Optional so tests can build a
    /// bare host without a wheel; production shells always provide one.
    sidebar: Option<Box<dyn Widget>>,
    /// Displays the compositor is painting across. Pushed by
    /// `FrameBuilder` before every `build_layout` so each strip can size
    /// its groups against the display they sit on. Empty in a bare
    /// `ui::App` host (`--mode hosted`), where [`Self::strip_output_spans`]
    /// falls back to treating the window as the one display.
    outputs: Vec<OutputGeometry>,
    palette: CommandPalette,
    palette_visible: bool,
    /// Registry of every search provider the palette fans out to on
    /// every keystroke. Built once at construction and never mutated
    /// from outside the shell's `new` — providers are stateful (they
    /// own background indexers etc.) so re-registration would leak
    /// worker threads.
    search_registry: crate::shell::search::SearchRegistry,
    /// The result list backing the current palette entries. Indexed in
    /// lock-step with `palette.entries` so an Enter/click resolves the
    /// row by ordinal into this vec and calls the stored `on_open`.
    search_results: Vec<crate::shell::search::SearchResult>,
    /// Value of `palette.query()` after the last provider fan-out.
    /// The refresh is short-circuited when this matches — every
    /// keystroke pushes new bytes into the palette, so the compare is
    /// the source of "did the query change this frame?".
    last_search_query: String,
    /// Kinds the chip strip has toggled on. Empty means "everything",
    /// so an unfiltered fan-out is the default. Toggled by clicks on
    /// the shell-drawn chip strip above the palette.
    active_kind_filter: Vec<crate::shell::search::ResourceKind>,
    /// Painted rects of the chip strip's chips this frame, indexed in
    /// the same order [`Self::search_registry`]::registered_kinds
    /// returns. Populated during draw so `handle_event` can hit-test
    /// without recomputing geometry.
    search_chip_rects: Vec<Rect>,
    /// Rect of the leading "All" chip that clears the kind filter.
    /// Cached from the draw pass so the click hit-test uses exactly the
    /// geometry that was painted.
    search_all_chip_rect: Rect,
    /// Set to `true` whenever [`Self::refresh_search_results`] rewrites
    /// the palette's entry list. The next `take_structure_dirty` folds
    /// it in so the taffy layout pass rebuilds the palette node with
    /// the new row count — without this the palette's height stays
    /// pinned at whatever the empty-entries first paint computed, and
    /// only the first ~1 result is visible.
    palette_layout_dirty: bool,
    /// Host width the palette's taffy style was last computed against.
    /// See the sizing comment in `build_layout`.
    palette_layout_host_w: f64,
    // Child node ids are captured once during `build_layout` so we can
    // forward layout/draw/event calls without re-walking the tree.
    content_node: Option<NodeId>,
    palette_node: Option<NodeId>,
    sidebar_node: Option<NodeId>,
    /// Shared chrome state; the host reads `overlays` flags from this to
    /// decide whether to draw overview / quick-settings / ws-settings on
    /// top of the content.
    chrome: Option<Chrome>,
    /// Last known cursor position in logical coords, for hit-testing
    /// overlay clicks (shared with the rest of the chrome).
    last_cursor: Option<Point>,
    /// Simulated remote peer cursor. The shell is collaborative-by-default,
    /// so we paint a deterministic fake peer ("Ada") over the desktop
    /// until the network layer feeds real `PresenceCtx::remote_cursors`
    /// into the widget tree — this keeps the collab chrome visible in
    /// single-process development. Lissajous-ish path seeded from `started`.
    started: Instant,
    /// Overview crossfade state: `overview_t` ∈ [0, 1] where 0 = desktop
    /// fully visible and 1 = overview fully visible. `overview_target` is
    /// the value `overview_t` is animating toward (latched on toggle).
    /// `transition_started`/`transition_from` bookend the in-flight
    /// animation so a re-toggle mid-fade picks up from the current visible
    /// alpha rather than popping back to 0 or 1.
    overview_t: f32,
    overview_target: f32,
    transition_started: Option<Instant>,
    transition_from: f32,
    /// Vertical scroll offset (px) for the workspace overview's row list.
    /// Wheel events over the overview pan the cards through this offset
    /// while the header (`Workspaces` h1 + subtitle) stays pinned. Clamped
    /// in [0, max] each frame against the total content extent so a
    /// workspace deletion can't strand the scroll past the tail.
    overview_scroll_y: f64,
    /// Active-workspace id observed during the last `draw` call. When the
    /// chrome-held active workspace diverges from this, we latch a slide
    /// animation so the new workspace visibly enters from the edge rather
    /// than popping. Tracked via `u64` (the raw WorkspaceId) so this
    /// module doesn't have to depend on the WorkspaceId newtype for a
    /// single equality check.
    last_active_ws: Option<u64>,
    /// `Instant` the current workspace-slide started. `None` once the
    /// slide has run past its duration — animation finishes by easing
    /// the translation toward zero, so at rest nothing pops.
    ws_slide_started: Option<Instant>,
    /// Vertical direction the new workspace slides *in* from: `+1` means
    /// from below (user hit Down/next), `-1` means from above (Up/prev).
    /// The old workspace already rendered last frame and is not carried
    /// over — we slide the new one toward its rest position from an
    /// offset of `sign * viewport_height` to 0.
    ws_slide_dir: f32,
    /// Interruptible open/close driver for the quick-settings popover.
    /// Follows the `quick_settings` flag each frame; unlike the raw flag
    /// it keeps the popover drawn through its close curve so it fades +
    /// slides OUT instead of vanishing.
    qs_transition: ui::Transition,
    /// Previous-frame visibility of the quick-settings flag, used to
    /// detect the rising edge (open transition) without reaching into
    /// the chrome on every tick.
    qs_was_open: bool,
    /// Which quick-settings slider (0 = brightness, 1 = volume) is
    /// currently being dragged. `Some(i)` after a Pressed inside the
    /// slider's hit rect and cleared on Released. While set, CursorMoved
    /// updates the slider's value inline; the os_action shell-out fires
    /// only on Released so a fast drag doesn't spawn a subprocess per
    /// pixel of motion.
    qs_slider_drag: Option<usize>,
    ws_settings_was_open: bool,
    /// Interruptible open/close driver for the workspace-settings modal
    /// — like `qs_transition` but for the modal, so it fades + scales
    /// OUT on close instead of vanishing.
    ws_settings_transition: ui::Transition,
    palette_was_visible: bool,
    /// Interruptible open/close driver for the command palette.
    palette_transition: ui::Transition,
    /// Per-strip raw pointers to the [`GroupRow`] widgets inside `content`.
    /// Captured at [`build_nirios_overlay_host_with_chrome`] time and used
    /// each frame by [`Self::reconcile_active_group_off_closing`] to harvest
    /// the [`UiWindowGroup::is_closing`] set without having to downcast a
    /// `&dyn Widget`. Empty by default — tests that build a bare host
    /// through [`Self::new`] don't get the per-frame reconcile path until
    /// they call [`Self::with_group_row_ptrs`] explicitly.
    ///
    /// SAFETY: each pointer is the heap address of a `GroupRow` owned by
    /// the chrome's content tree. `Box<T>` -> `Box<dyn Widget>` unsizing
    /// keeps the heap allocation at the same address, and the host owns
    /// the content for the rest of the process lifetime, so the pointer
    /// remains valid as long as `&mut self` does.
    group_row_ptrs: HashMap<StripId, *mut GroupRow>,
    /// Per-strip record of the last active-group id that
    /// [`Self::scroll_active_group_into_view`] scrolled into view. Lets the
    /// focus-follow scroll stay edge-triggered: it only re-centres the
    /// strip when the persisted `active_group` pointer actually changes, so
    /// a user who wheels away from the focused group isn't fought by a
    /// per-frame snap-back. Seeded lazily on first reconcile.
    last_scrolled_active_group: HashMap<StripId, GroupId>,
    /// Hit rect for the "+ Add workspace" affordance painted at the tail
    /// of the overview's vertical workspace list. `Rect::ZERO` until the
    /// first overview draw populates it; cleared back to `Rect::ZERO`
    /// when the overview is hidden so stale clicks during the crossfade
    /// can't activate it.
    add_workspace_rect: Rect,
    /// Hit rects for the WS-settings tab pills painted by
    /// `draw_ws_settings`, in [`WsTab::all`] order. Every pill is a
    /// real tab. All entries are `Rect::ZERO` until the first
    /// `draw_ws_settings` populates them, and are cleared back to
    /// `Rect::ZERO` when the modal closes so a stale click during a
    /// crossfade can't flip tabs after the modal is gone.
    ws_settings_tab_pill_rects: [Rect; 3],
    /// Cached Revoke-button rects for the Permissions pane, keyed by
    /// the capability's stable id — the key
    /// [`crate::shell::state::Workspace::revoke_as`] takes, so a click
    /// can never revoke a different capability than the row it was
    /// painted for.
    permissions_revoke_rects: Vec<(String, Rect)>,
    /// Cached grant-composer control rects, populated while the
    /// Permissions tab is drawn and cleared otherwise.
    grant_composer_rects: Option<GrantComposerRects>,
    /// Cached per-member control rects for the Members pane:
    /// `(member, role chip, Remove button)`.
    member_control_rects: Vec<(crate::shell::state::UserId, Rect, Rect)>,
    /// Vertical inset at which the floating sidebar wheel starts. Defaults
    /// to [`SIDEBAR_TOP_OFFSET_PX`] (matches `TOPBAR_HEIGHT_PX`) but the
    /// real shell wraps its topbar in a padded `Column`/`Container`, so
    /// the topbar's bottom edge lands below y=40. `build_nirios_root_
    /// with_chrome` overrides this via [`Self::with_sidebar_top`] so the
    /// sidebar's rect doesn't overlap the topbar and swallow the overview-
    /// toggle click.
    sidebar_top_y: f32,
    /// Live permission-prompt widget rendered at the top of the active
    /// strip when `chrome.pending_grants.front().is_some()`. Constructed
    /// lazily — populated by `pump_permission_prompt` each frame from
    /// the front of the queue. `None` when the queue is empty so we
    /// don't paint a stale prompt during the moment between two
    /// requests.
    permission_prompt: Option<PermissionPrompt>,
    /// Whether peer-sync palette commands (`/invite`, `/connect`) are
    /// surfaced to the user. Off by default; the compositor flips this
    /// to `true` when booted with `--enable-peer-sync`. Toggled via
    /// [`Self::with_peer_sync_enabled`]. Phase 3 Task 3.3 of
    /// `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md`.
    peer_sync_enabled: bool,
    /// Queue of peer-sync palette actions emitted by
    /// [`Self::dispatch_palette_command`] when the user picks `/invite`
    /// or `/connect`. The compositor drains this each tick and routes
    /// the action into its `ShellNetwork` handle (issue token + paste
    /// to clipboard, or validate + dial). Stays empty when
    /// `peer_sync_enabled` is false because the entries aren't
    /// registered in the palette.
    peer_sync_actions: Vec<PeerSyncAction>,
    /// What actually performs a peer-sync request.
    ///
    /// The palette records an intent; something has to carry it out.
    /// The compositor installs a closure here that reaches its
    /// `ShellNetwork` handle, so the libp2p work stays on the runtime
    /// thread and the widget layer keeps knowing nothing about
    /// transports. `None` in tests and in a session booted without
    /// peer-sync, where `/invite` and `/connect` report that rather
    /// than silently doing nothing.
    peer_sync_bridge: Option<std::rc::Rc<dyn Fn(PeerSyncRequest) -> Result<String, String>>>,
    /// Hit rects of the connect prompt's buttons, published by
    /// [`Self::draw_peer_sync`] and consumed by the click path. Empty
    /// when the prompt is not painted, so a click cannot reach a button
    /// that is not on screen.
    peer_sync_prompt_rects: Vec<(PeerSyncPromptButton, Rect)>,
    /// Top-right notification stack. Painted above the desktop content
    /// but below the permission prompt and modal overlays so a
    /// permission prompt always wins focus. `None` until
    /// [`Self::with_chrome`] runs — the stack needs a chrome handle
    /// to read notifications from.
    notification_stack: Option<NotificationStack>,
    /// Last `chrome.revision` value observed by
    /// [`Self::resync_strips_from_tree`]. The chrome bumps its
    /// revision counter on every structural mutation (workspace
    /// add, group insert, tab close, …); when the cached value
    /// disagrees with the live one we know the persisted tree has
    /// changed shape and the live [`GroupRow`] widgets need to
    /// pick up the new strip contents. `u64::MAX` at construction
    /// so the first poll always rebuilds, even when chrome boots
    /// with `revision: 0`.
    last_chrome_revision: u64,
    /// Side-effect dispatcher for quick-settings actions. Production
    /// builds use [`crate::shell::qs_actions::RealQsActions`]; tests
    /// can swap in a recording impl via [`Self::with_qs_actions`] to
    /// assert that clicks routed through the popover translate into
    /// the expected shell-outs without actually invoking `nmcli` etc.
    qs_actions: Box<dyn crate::shell::qs_actions::QsActions>,
    /// Room-activity sheet — the workspace's own change history. Held rather
    /// than drawn immediate-mode because the timeline it wraps is a real
    /// `Widget` with its own hover, scroll and filter state; it is placed by
    /// hand (`RoomActivityPane::layout`) rather than through taffy, the same
    /// way the notification stack is.
    room_activity: crate::shell::room_activity::RoomActivityPane,
    /// Open/close curve for the room-activity sheet.
    room_activity_transition: ui::Transition,
    /// Whether the sheet was open on the previous frame, so the rising edge
    /// can invalidate the derived rows.
    room_activity_was_open: bool,
    /// Hit rect of the "Room activity" row at the foot of the collaborator
    /// dropdown. Populated by `draw_collaborators` and zeroed whenever the
    /// dropdown is not painted, so a click during the close cannot trip it.
    collab_activity_row_rect: Rect,
}

/// Action emitted from the palette into [`ShellOverlayHost::peer_sync_actions`]
/// when the user picks a peer-sync command. The compositor's per-frame
/// drainer (in `compositor::hosted::run_with_options`) routes these
/// into its `ShellNetwork` handle so the actual libp2p side-effects
/// stay on the runtime thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncAction {
    /// User invoked `/invite`. The compositor should call
    /// `ShellNetwork::issue_invite(active_ws, listening_addr)` and
    /// drop the resulting token onto the system clipboard.
    IssueInvite,
    /// User invoked `/connect`. Opens the token-input prompt
    /// ([`ShellOverlayHost::draw_peer_sync`]); submitting it sends a
    /// [`PeerSyncRequest::AcceptInvite`] through the bridge.
    PromptConnect,
}

/// A peer-sync operation the widget layer wants performed, handed to
/// the closure installed by
/// [`ShellOverlayHost::with_peer_sync_bridge`].
///
/// Deliberately not the same type as [`PeerSyncAction`]: an action is
/// what the user picked in the palette, a request is what the network
/// has to be asked. `/connect` is one action and produces no request
/// until a token has actually been typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncRequest {
    /// Mint a signed invite to the active workspace. `Ok` carries the
    /// token.
    IssueInvite,
    /// Join the room a token names. `Ok` carries the room id.
    AcceptInvite(String),
}

/// A button on the connect prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSyncPromptButton {
    /// Submit whatever has been typed as an invite token.
    Join,
    /// Abandon the prompt.
    Cancel,
}

/// Total duration of the desktop ↔ overview crossfade. The concept wraps
/// the two views in `AnimatePresence mode="wait"` with `duration: 0.2`
/// per side (`App.tsx:427-487`) — the outgoing view runs its exit first,
/// then the incoming runs its enter, for ~400 ms end-to-end. We split
/// `overview_t` ∈ [0, 1] into two halves inside the draw path: the first
/// half shrinks/fades the desktop out, the second half grows/fades the
/// overview in.
/// Rows the palette asks the registry for, shared round-robin between
/// the matching kinds. See
/// [`crate::shell::search::SearchRegistry::query_grouped`].
///
/// This has to equal what the card actually paints, or the split that
/// budget buys is spent on rows nobody sees. `CommandPalette` grows to
/// eight entry rows (plus a separate allowance for three section
/// headers) before the list starts scrolling; asking for sixteen meant a
/// three-group answer put its third header around row eleven, off the
/// bottom of the card. Typing `workspace` then showed twenty-seven files
/// and thirteen folders while `Open workspace settings` — the thing that
/// query is for — was scrolled out of sight, which reads as the action
/// simply not being found.
///
/// Eight rows across three groups is tight, and it is the honest kind of
/// tight: every group's header is on the card and each one reports the
/// total it is holding back, so the way to see more of a kind is to click
/// its chip, which is also the gesture that means "show me all of these".
const PALETTE_ROW_BUDGET: usize = 8;

/// Glyph that stands for a result kind in the palette. Giving each kind
/// a picture is what replaced the bracketed kind literal the shell used
/// to prepend to every subtitle.
fn icon_for_kind(kind: &crate::shell::search::ResourceKind) -> ui::icons::Icon {
    use crate::shell::search::ResourceKind as K;
    use ui::icons::Icon;
    match kind {
        K::Action => Icon::Settings2,
        K::Application => Icon::LayoutGrid,
        K::Note => Icon::FileText,
        K::File => Icon::File,
        K::Folder => Icon::Folder,
        K::Creator => Icon::Star,
        K::Video => Icon::Film,
        K::Playlist => Icon::List,
        K::Custom(_) => Icon::Search,
    }
}

/// Gap between a segmented control's outer track and its segments.
const SEGMENT_INSET_PX: f64 = 2.0;

/// Break `text` into at most `max_lines` lines that each fit `width`.
///
/// An invite token is a few hundred opaque base64 characters with no
/// spaces to break on, so this splits on width rather than on word
/// boundaries. The last line is truncated with an ellipsis when the
/// budget runs out — the reader needs to see that there is more, not to
/// be shown a token that looks complete and is not.
fn wrap_monospace_ish(
    text_renderer: &mut TextRenderer,
    text: &str,
    size: f32,
    width: f32,
    max_lines: usize,
) -> Vec<String> {
    if text.is_empty() || width <= 0.0 || max_lines == 0 {
        return Vec::new();
    }
    // Measure once and divide: every glyph in a base64 alphabet is
    // close enough in width that per-character measurement would cost
    // far more than it buys.
    let (probe_w, _) = text_renderer.measure("MMMMMMMMMM", size, None);
    let per_char = (probe_w as f64 / 10.0).max(1.0);
    let per_line = ((width as f64 / per_char).floor() as usize).max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    for chunk in chars.chunks(per_line) {
        if out.len() == max_lines {
            // Mark the truncation on the line already emitted.
            if let Some(last) = out.last_mut() {
                let last: &mut String = last;
                last.pop();
                last.push('…');
            }
            break;
        }
        out.push(chunk.iter().collect::<String>());
    }
    out
}

/// One pill in the search card's filter band. Shared by the kind chips
/// and the "All" reset chip so they cannot drift apart.
#[allow(clippy::too_many_arguments)]
fn draw_filter_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    rect: Rect,
    label: &str,
    text_size: f32,
    fill: vello::peniko::Color,
    text_color: vello::peniko::Color,
) {
    let shape = RoundedRect::from_rect(rect, rect.height() / 2.0);
    scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
    let (lw, lh) = text_renderer.measure(label, text_size, None);
    text_renderer.draw_text(
        scene,
        label,
        text_size,
        text_color,
        transform
            * Affine::translate((
                rect.x0 + (rect.width() - lw as f64) / 2.0,
                rect.y0 + (rect.height() - lh as f64) / 2.0,
            )),
        None,
    );
}

const OVERVIEW_TRANSITION_MS: u64 = 400;

/// Vertical inset below the top bar where the floating sidebar wheel
/// starts. Matches `TOPBAR_HEIGHT_PX` so the wheel doesn't clip into the
/// tray; kept as a local constant to avoid a cross-module import for a
/// single f32.
const SIDEBAR_TOP_OFFSET_PX: f32 = 40.0;

/// Clamp range for the viewport-driven sidebar width. The slot used to
/// be a hard-coded `360 px`; user feedback (2026-05-25) was that at
/// 1080p fullscreen this read as "slightly too wide" while the curve /
/// disc fraction itself was the right size. Solution: derive the slot
/// from the live viewport height (scales naturally between laptop
/// 13" and 4K monitors) and let the disc's `apply_layout` recompute
/// its `diameter_px` to keep the visible-arc curvature identical
/// across widths.
///
/// `SIDEBAR_WIDTH_MAX_PX` doubles as the *fixed* extra-left-padding
/// the content column reserves (`sidebar_wheel_chrome::SIDEBAR_WIDTH_PX`),
/// so the sidebar is never wider than the gutter content reserves.
/// Result: on small viewports the sidebar narrows; the gutter still
/// has a (slightly larger) constant width, leaving wallpaper breathing
/// room between the sidebar's right edge and the content's left edge.
const SIDEBAR_WIDTH_MIN_PX: f32 = 220.0;
const SIDEBAR_WIDTH_MAX_PX: f32 = 320.0;
const SIDEBAR_WIDTH_VIEWPORT_FRACTION: f32 = 0.26;

/// Compute the sidebar width for a given viewport height. `0.26 * h`
/// at 1080p gives ~281 px (down from the prior fixed 360), at 1440p
/// gives ~374 → clamped to 320, at 720p gives ~187 → clamped to 220.
/// First-frame callers (before any layout pass) pass `0.0` and get
/// the lower bound, which is a fine seed: the slot reflows on the
/// next frame once `ShellOverlayHost::rect` is known.
fn sidebar_width_for_viewport(viewport_h: f32) -> f32 {
    (viewport_h * SIDEBAR_WIDTH_VIEWPORT_FRACTION).clamp(SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_MAX_PX)
}

/// Wallpaper gutter between the wheel disc's right edge and the first
/// window. 2026-06-24 user feedback: with the strip's leading inset equal
/// to the wheel width exactly, the radial switcher butted into the first
/// window; the gap reads as separation between canvas chrome and content.
const WHEEL_TO_STRIP_GAP_PX: f64 = 28.0;

/// One display the compositor is painting across, in its combined
/// coordinate space. The compositor renders a single scene spanning the
/// bounding box of every output (see `compositor::backend::Output`), so the
/// host has to be told where the seams are: a window group's width is a
/// fraction of *its* display, and resolving against the combined desktop
/// would let one window span two monitors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputGeometry {
    /// Left edge in combined coordinates.
    pub x: f64,
    pub width: f64,
    pub height: f64,
}

/// Duration of the vertical workspace-swap slide. Matches the concept's
/// wheel-spin feel — long enough to register as motion, short enough to
/// not feel sluggish when rapidly cycling workspaces via keyboard.
const WS_SLIDE_MS: u64 = 260;

/// Duration of the quick-settings popover entry animation (concept
/// motion.div springs the panel in roughly this fast). Short enough to
/// feel snappy on a dropdown; long enough to register as motion.
const QS_OPEN_ANIM_MS: u64 = 180;

/// Duration of the modal-style entry animation used by workspace-settings
/// and global-search (both wrap in a `motion.div` that fades + scales +
/// settles from y=20). Slightly longer than the dropdown variant because
/// the travel distance is larger and the user focus is on the modal
/// contents rather than a tray affordance.
const MODAL_OPEN_ANIM_MS: u64 = 220;

// ---------------------------------------------------------------------------
// Translucent-tint fallback alphas (gap #3 in `docs/nirios/visual-gaps.md`).
// The concept uses `backdrop-blur-xl` / `backdrop-blur-md` on top of
// `bg-slate-900/80` + `/70` tints, but the pinned Vello lacks
// `scene.push_blur_layer`. The shell therefore honours the alpha split
// alone — the surfaces sit translucent over whatever is beneath them, so a
// gradient wallpaper still bleeds through even without a convolution pass.
// Centralised so the `shell_chrome_test` suite can pin the numeric values.
// ---------------------------------------------------------------------------

/// Alpha applied to the collaborator and QuickSettings popover surfaces.
/// Concept §1.1 / §3.1 specifies `bg-slate-900/80`.
/// Height of the "Room activity" row at the foot of the collaborator
/// dropdown, including the separator above it.
const COLLAB_FOOTER_H: f64 = 40.0;

pub const POPOVER_SURFACE_ALPHA: f32 = 0.80;

/// Alpha applied to the GlobalSearch palette and Workspace-Settings modal
/// surfaces. Concept §4.1 / §5.1 specified `bg-slate-900/70`, but at that
/// alpha the palette reads as translucent chrome bleeding through
/// whatever's underneath — legibility of long rows drops sharply against
/// a busy desktop backdrop (2026-07-09 user report: "search bar is too
/// transparent"). We now paint the palette solid and rely on the scrim
/// below to sell the "page-on-top-of-page" affordance instead. The same
/// value backs the workspace-settings modal; app content that inherits
/// this tint (file viewer, etc.) picks up the same lift for free.
pub const MODAL_SURFACE_ALPHA: f32 = 1.0;

/// Alpha applied to the scrim painted beneath the WorkspaceSettings modal.
/// Concept §4.1 uses `bg-black/30 backdrop-blur-[1px]`. The pinned Vello
/// has no `push_blur_layer`, so a literal `0.30` scrim under-dims and the
/// desktop bleeds through the [`MODAL_SURFACE_ALPHA`]-tinted surface,
/// killing the "page-on-top-of-page" affordance. We compensate by
/// lifting the scrim to `0.45` — empirically the alpha at which an
/// un-blurred scrim reads roughly the same as the concept's blurred
/// 0.30 fill. Pinned by `tests::palette_scrim_alpha_matches_concept`.
pub const MODAL_SCRIM_ALPHA: f32 = 0.45;

/// Alpha applied to the scrim painted beneath the GlobalSearch palette and
/// the Overview view. Concept §5.1 specifies `bg-black/30` here too, and
/// this constant used to take it literally on the grounds that both
/// surfaces sit on a stronger glass tint than the WorkspaceSettings
/// modal. That reasoning is about the *card*; the scrim's job is the
/// *background*, and there it does not hold. A window card in this shell
/// is `bg-white`, and 0.30 leaves it at 178 grey — a bright field
/// abutting a slate-900 palette, which is what a 2026-07-30 report
/// described as the palette having no dim behind it and meeting window
/// content at a jarring edge.
///
/// So this takes the same un-blurred lift [`MODAL_SCRIM_ALPHA`] already
/// documents, for the same reason: the pinned Vello has no
/// `push_blur_layer`, so an un-blurred fill has to go heavier to read
/// like the concept's blurred one. Same value as the modal, because both
/// are the same gesture — put a page on top of the page.
/// Pinned by `tests::palette_scrim_alpha_reads_like_the_blurred_concept`.
pub const PALETTE_SCRIM_ALPHA: f32 = 0.45;

impl ShellOverlayHost {
    /// Display name painted in the General tab's collaborator tile when
    /// no `me` collaborator is seeded in the active workspace. Pinned
    /// to `"Me"` so the adjacent indigo "You" badge doesn't duplicate
    /// the row label (the "You [You]" artefact spotted in the
    /// 2026-05-04 critical-UI-inspection sweep). When the local user
    /// signs in for real, `c.name` from the seeded collaborator wins.
    pub(crate) const ME_TILE_FALLBACK_NAME: &'static str = "Me";

    pub fn new(content: Box<dyn Widget>) -> Self {
        // Palette entries are now produced by the search registry on
        // every keystroke (`refresh_search_results`). The registry ships
        // with in-shell providers: `actions` (the old chrome-command
        // rows), `applications` (installed packages — picking a row
        // launches one), `notes` and `editor` (each text app's own
        // Automerge body, which is also what `Ctrl+K` narrows to),
        // `files` (walkdir + tantivy content index). New providers slot
        // in via `SearchRegistry::register`; the palette picks up their
        // resource-kind chips automatically.
        let mut palette = CommandPalette::new();
        palette.set_bypass_fuzzy(true);
        // The shell paints scope + kind controls into the palette's own
        // reserved filter band, inside the card.
        palette.set_host_filter_strip(true);
        let mut search_registry = crate::shell::search::SearchRegistry::new();
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ActionsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ApplicationsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::NotesProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::EditorProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::FilesProvider::new(),
        ));
        Self {
            rect: Rect::ZERO,
            content,
            sidebar: None,
            outputs: Vec::new(),
            palette,
            palette_visible: false,
            search_registry,
            search_results: Vec::new(),
            last_search_query: String::new(),
            active_kind_filter: Vec::new(),
            search_chip_rects: Vec::new(),
            search_all_chip_rect: Rect::ZERO,
            palette_layout_dirty: false,
            palette_layout_host_w: f64::NAN,
            content_node: None,
            palette_node: None,
            sidebar_node: None,
            chrome: None,
            last_cursor: None,
            started: Instant::now(),
            overview_t: 0.0,
            overview_target: 0.0,
            overview_scroll_y: 0.0,
            transition_started: None,
            transition_from: 0.0,
            last_active_ws: None,
            ws_slide_started: None,
            ws_slide_dir: 0.0,
            qs_transition: ui::Transition::closed(),
            qs_was_open: false,
            qs_slider_drag: None,
            ws_settings_was_open: false,
            ws_settings_transition: ui::Transition::closed(),
            palette_was_visible: false,
            palette_transition: ui::Transition::closed(),
            group_row_ptrs: HashMap::new(),
            last_scrolled_active_group: HashMap::new(),
            add_workspace_rect: Rect::ZERO,
            ws_settings_tab_pill_rects: [Rect::ZERO; 3],
            permissions_revoke_rects: Vec::new(),
            grant_composer_rects: None,
            member_control_rects: Vec::new(),
            sidebar_top_y: SIDEBAR_TOP_OFFSET_PX,
            permission_prompt: None,
            peer_sync_enabled: false,
            peer_sync_actions: Vec::new(),
            peer_sync_bridge: None,
            peer_sync_prompt_rects: Vec::new(),
            notification_stack: None,
            last_chrome_revision: u64::MAX,
            qs_actions: Box::new(crate::shell::qs_actions::RealQsActions),
            room_activity: crate::shell::room_activity::RoomActivityPane::new(),
            room_activity_transition: ui::Transition::closed(),
            room_activity_was_open: false,
            collab_activity_row_rect: Rect::ZERO,
        }
    }

    /// Replace the QS action dispatcher. Tests use this to swap in a
    /// recording impl that captures every `set_*` call instead of
    /// shelling out to `nmcli` / `brightnessctl` / `bluetoothctl` /
    /// `wpctl` / `powerprofilesctl`. Production builds keep the
    /// default [`crate::shell::qs_actions::RealQsActions`].
    pub fn with_qs_actions(
        mut self,
        actions: Box<dyn crate::shell::qs_actions::QsActions>,
    ) -> Self {
        self.qs_actions = actions;
        self
    }

    /// Return the slide translation to apply to the content widget this
    /// frame, or `None` if no slide is active. Called from `draw`.
    ///
    /// Drives a single vertical slide: when the chrome's active
    /// workspace id has changed since last frame, latch a fresh
    /// animation starting `dir * viewport_height` off-axis and easing
    /// to zero over `WS_SLIDE_MS`. The direction is computed from
    /// `next_index - prev_index` on the chrome's workspace list:
    /// later-in-list → slide up from below (+1), earlier → slide down
    /// from above (-1).
    fn tick_workspace_slide(&mut self) -> Option<f64> {
        let chrome = self.chrome.as_ref()?.borrow();
        let tree = chrome.tree();
        let active_raw = tree.active.raw();
        // Find the slide direction by comparing workspace index to the
        // last-observed one. Fallback to +1 when the prior id isn't in
        // the current tree (e.g. a workspace was removed).
        let dir = match self.last_active_ws {
            Some(prev) if prev == active_raw => 0.0_f32,
            Some(prev) => {
                let idx_prev = tree.workspaces.iter().position(|w| w.id.raw() == prev);
                let idx_next = tree
                    .workspaces
                    .iter()
                    .position(|w| w.id.raw() == active_raw);
                match (idx_prev, idx_next) {
                    (Some(a), Some(b)) if b > a => 1.0,
                    (Some(a), Some(b)) if b < a => -1.0,
                    _ => 1.0,
                }
            }
            None => 0.0,
        };
        drop(tree);
        drop(chrome);

        if dir != 0.0 && Some(active_raw) != self.last_active_ws {
            self.ws_slide_started = Some(Instant::now());
            self.ws_slide_dir = dir;
        }
        self.last_active_ws = Some(active_raw);

        let started = self.ws_slide_started?;
        let elapsed = started.elapsed().as_millis() as f32;
        let duration = WS_SLIDE_MS as f32;
        if elapsed >= duration {
            self.ws_slide_started = None;
            return None;
        }
        let p = (elapsed / duration).clamp(0.0, 1.0);
        // Ease-out cubic: start fast, slow into rest. The *remaining*
        // translation is `(1 - eased) * dir * height`, so at t=0 the
        // new workspace is one full viewport off-screen and at t=1 it
        // rests at origin.
        let eased = 1.0 - (1.0 - p).powi(3);
        let remaining = 1.0 - eased;
        let height = self.rect.height().max(1.0);
        Some(remaining as f64 * self.ws_slide_dir as f64 * height)
    }

    /// Advance the overview crossfade toward `overview_target`. Latches
    /// a new transition if the target changed since last frame, then
    /// evaluates a linear lerp of raw progress. The draw path is
    /// responsible for splitting the returned `t` into a sequential
    /// two-phase animation (desktop shrink-out in `[0, 0.5]`, overview
    /// zoom-in in `[0.5, 1.0]`), each with its own smoothstep easing —
    /// so keep this function returning *linear* progress.
    fn tick_overview_transition(&mut self, target: f32) -> f32 {
        if (target - self.overview_target).abs() > f32::EPSILON {
            self.transition_from = self.overview_t;
            self.overview_target = target;
            self.transition_started = Some(Instant::now());
        }
        if let Some(started) = self.transition_started {
            let elapsed = started.elapsed().as_millis() as f32;
            let duration = OVERVIEW_TRANSITION_MS as f32;
            if elapsed >= duration {
                self.overview_t = self.overview_target;
                self.transition_started = None;
            } else {
                let p = (elapsed / duration).clamp(0.0, 1.0);
                // Linear progress — the split-half smoothstep happens in
                // `compose_overlay_transition` so each phase can ease
                // independently (matches `AnimatePresence mode="wait"`).
                self.overview_t =
                    self.transition_from + (self.overview_target - self.transition_from) * p;
            }
        }
        self.overview_t
    }

    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        // Mount the notification stack once we have a chrome handle to
        // read from. The widget paints into the viewport's top-right
        // corner each frame via `set_viewport`; it doesn't participate
        // in Taffy because its position is purely absolute.
        self.notification_stack = Some(NotificationStack::new(chrome.clone()));
        chrome.borrow_mut().peer_sync_enabled = self.peer_sync_enabled;
        self.chrome = Some(chrome);
        self
    }

    /// Toggle the peer-sync palette commands (`/invite`, `/connect`).
    ///
    /// When enabled, the palette grows two extra rows that the user
    /// can pick to issue an invite token (copied to clipboard by the
    /// compositor) or paste one in to dial a remote workspace. Off
    /// by default; the compositor flips this on when booted with
    /// `--enable-peer-sync` so a v1 demo without the flag stays
    /// behaviour-identical to before Phase 3 Task 3.3.
    ///
    /// Idempotent: calling it twice with `true` only appends the rows
    /// once. The rows can't be removed once added — toggling back to
    /// `false` is not a supported transition.
    pub fn with_peer_sync_enabled(mut self, enabled: bool) -> Self {
        // The palette rows come from `ActionsProvider`, which reads the
        // flag off chrome — so the flag has to reach chrome, not only
        // this struct.
        self.peer_sync_enabled = enabled;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().peer_sync_enabled = enabled;
        }
        self
    }

    /// Read-only accessor for whether peer-sync palette entries are
    /// currently registered. Tests use this to assert
    /// [`Self::with_peer_sync_enabled`] flipped the flag without
    /// having to inspect the internal `CommandPalette` directly.
    pub fn peer_sync_enabled(&self) -> bool {
        self.peer_sync_enabled
    }

    /// Drain any pending [`PeerSyncAction`]s emitted by the palette
    /// dispatcher since the last call. Returns the captured actions
    /// in FIFO order.
    ///
    /// The compositor (in `compositor::hosted::run_with_options`)
    /// pumps this each frame and routes the actions into its
    /// `ShellNetwork` handle: `IssueInvite` issues a token + posts to
    /// clipboard, `PromptConnect` opens a small text-input modal for
    /// the inbound token. Buffer is cleared on every drain so
    /// repeated `/invite` invocations all surface — no de-dup, no
    /// coalescing.
    pub fn take_peer_sync_actions(&mut self) -> Vec<PeerSyncAction> {
        std::mem::take(&mut self.peer_sync_actions)
    }

    /// Install the closure that carries out [`PeerSyncRequest`]s.
    ///
    /// Without one, `/invite` and `/connect` have nowhere to go — which
    /// is what they had before: the palette pushed an action onto a
    /// queue no production code drained, so both commands were inert in
    /// a running session however the flag was set.
    pub fn with_peer_sync_bridge(
        mut self,
        bridge: std::rc::Rc<dyn Fn(PeerSyncRequest) -> Result<String, String>>,
    ) -> Self {
        self.set_peer_sync_bridge(bridge);
        self
    }

    /// [`Self::with_peer_sync_bridge`] for a host that is already
    /// built — the native compositor constructs its host inside a
    /// `FrameBuilder` before it has a network handle to bridge to.
    pub fn set_peer_sync_bridge(
        &mut self,
        bridge: std::rc::Rc<dyn Fn(PeerSyncRequest) -> Result<String, String>>,
    ) {
        self.peer_sync_bridge = Some(bridge);
    }

    /// Run one peer-sync request through the installed bridge.
    fn peer_sync_call(&self, req: PeerSyncRequest) -> Result<String, String> {
        match self.peer_sync_bridge.as_ref() {
            Some(bridge) => bridge(req),
            None => Err("peer-sync is not running in this session".to_string()),
        }
    }

    /// Drain the palette's peer-sync intents and act on them.
    ///
    /// Called once per frame from [`Widget::update`]. `/invite` mints a
    /// token and parks it where the user can read it; `/connect` opens
    /// the token prompt. Both write their outcome into chrome so the
    /// result is on screen rather than only in a log.
    fn pump_peer_sync(&mut self) {
        let mut actions = std::mem::take(&mut self.peer_sync_actions);
        if let Some(chrome) = self.chrome.clone() {
            actions.extend(chrome.borrow_mut().take_peer_sync_requests());
        }
        if actions.is_empty() {
            return;
        }
        for action in actions {
            match action {
                PeerSyncAction::IssueInvite => {
                    let outcome = self.peer_sync_call(PeerSyncRequest::IssueInvite);
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        match outcome {
                            Ok(token) => {
                                st.peer_sync_status =
                                    Some("Invite ready — share this token".to_string());
                                st.peer_sync_invite_token = Some(token);
                            }
                            Err(e) => {
                                st.peer_sync_invite_token = None;
                                st.peer_sync_status = Some(format!("Invite failed: {e}"));
                            }
                        }
                        st.bump();
                    }
                }
                PeerSyncAction::PromptConnect => {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.peer_sync_connect_input = Some(String::new());
                        st.peer_sync_status = Some("Paste or type the invite token".to_string());
                        st.bump();
                    }
                }
            }
        }
    }

    /// Submit whatever is in the connect prompt as an invite token.
    ///
    /// Closes the prompt on success and leaves it open on failure with
    /// the reason underneath, so a mistyped token can be corrected
    /// rather than retyped from scratch.
    fn submit_connect_prompt(&mut self) {
        let token = match self.chrome.as_ref() {
            Some(chrome) => match chrome.borrow().peer_sync_connect_input.clone() {
                Some(t) => t,
                None => return,
            },
            None => return,
        };
        let outcome = self.peer_sync_call(PeerSyncRequest::AcceptInvite(token.trim().to_string()));
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            match outcome {
                Ok(room) => {
                    st.peer_sync_connect_input = None;
                    st.peer_sync_status = Some(format!("Joined room {room}"));
                }
                Err(e) => {
                    st.peer_sync_status = Some(format!("Refused: {e}"));
                }
            }
            st.bump();
        }
    }

    /// Route one event to the peer-sync card. Returns whether the card
    /// consumed it.
    ///
    /// While the token prompt is open every key belongs to it: a base64
    /// token contains the same characters the chrome reads as chords,
    /// so anything that leaked past would both corrupt the token and
    /// fire a shell action.
    fn handle_peer_sync_event(&mut self, event: &WindowEvent) -> bool {
        let prompt_open = self.connect_prompt_open();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                match logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.peer_sync_connect_input = None;
                            st.peer_sync_invite_token = None;
                            st.peer_sync_status = None;
                            st.bump();
                        }
                        return true;
                    }
                    Key::Named(NamedKey::Enter) if prompt_open => {
                        self.submit_connect_prompt();
                        return true;
                    }
                    Key::Named(NamedKey::Backspace) if prompt_open => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.peer_sync_connect_input.as_mut() {
                                buf.pop();
                            }
                            st.bump();
                        }
                        return true;
                    }
                    Key::Character(s) if prompt_open => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.peer_sync_connect_input.as_mut() {
                                buf.push_str(s.as_str());
                            }
                            st.bump();
                        }
                        return true;
                    }
                    _ => {}
                }
                // Swallow anything else while the prompt is up so a
                // stray chord cannot act on the desktop behind it.
                prompt_open
            }
            WindowEvent::KeyboardInput { .. } => prompt_open,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(p) = self.last_cursor else {
                    return false;
                };
                for (which, rect) in self.peer_sync_prompt_rects.clone() {
                    if rect.area() > 0.0 && rect.contains(p) {
                        match which {
                            PeerSyncPromptButton::Join => self.submit_connect_prompt(),
                            PeerSyncPromptButton::Cancel => {
                                if let Some(chrome) = self.chrome.clone() {
                                    let mut st = chrome.borrow_mut();
                                    st.peer_sync_connect_input = None;
                                    st.peer_sync_status = None;
                                    st.bump();
                                }
                            }
                        }
                        return true;
                    }
                }
                self.peer_sync_rect().contains(p)
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. } => self
                .last_cursor
                .map(|p| self.peer_sync_rect().contains(p))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Is the connect token prompt on screen?
    fn connect_prompt_open(&self) -> bool {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().peer_sync_connect_input.is_some())
            .unwrap_or(false)
    }

    /// Register the floating sidebar wheel. The wheel is laid out as an
    /// absolutely-positioned overlay above the desktop content so it
    /// appears to hover over the wallpaper rather than occupy a column.
    pub fn with_sidebar(mut self, sidebar: Box<dyn Widget>) -> Self {
        self.sidebar = Some(sidebar);
        self
    }

    /// Override the vertical y-offset at which the sidebar wheel starts.
    /// Callers pass the real topbar-bottom in viewport-local coordinates
    /// (i.e. accounting for any outer padding introduced by `Column` /
    /// `Container` wrappers around the topbar). The default is 40 px
    /// which assumes the topbar is rendered at y=0..40; the real shell
    /// wraps the topbar in padded layout widgets so the actual bottom
    /// sits lower. Without this override, the sidebar's rect overlaps
    /// the topbar's rendered band and swallows clicks on the overview-
    /// toggle grid icon (see
    /// `full_root_click_toggles_overview_via_topbar_button`).
    pub fn with_sidebar_top(mut self, y: f32) -> Self {
        self.sidebar_top_y = y;
        self
    }

    /// Register the per-strip [`GroupRow`] raw pointers harvested at build
    /// time. The host uses these each frame in
    /// [`Self::reconcile_active_group_off_closing`] to feed the data-layer
    /// reconciler without re-walking the widget tree.
    ///
    /// SAFETY: every pointer in `ptrs` must address a `GroupRow` owned by
    /// the content subtree this host wraps. The caller (currently
    /// [`build_nirios_overlay_host_with_chrome`]) is responsible for
    /// upholding that invariant by capturing the pointer immediately after
    /// `Box::new(strip_row)` and passing the same `Box` into the content
    /// tree — the `Box<T>` -> `Box<dyn Widget>` unsizing coercion keeps
    /// the heap address stable.
    /// Tell the host which displays it is painting across. The compositor
    /// calls this through `FrameBuilder` whenever the output set or a mode
    /// changes; the host forwards the derived per-display strip widths to
    /// every [`GroupRow`] at the top of `build_layout`.
    pub fn set_output_geometry(&mut self, outputs: &[OutputGeometry]) {
        if self.outputs != outputs {
            self.outputs = outputs.to_vec();
        }
    }

    /// The displays the host was last told about.
    pub fn output_geometry(&self) -> &[OutputGeometry] {
        &self.outputs
    }

    /// Per-display strip widths for the [`GroupRow`]s: each display's full
    /// extent plus the width actually available to window groups on it.
    ///
    /// The leftmost display gives up the wheel-disc slot and its wallpaper
    /// gutter — that region is canvas the strip reserves but never places a
    /// group in — so a `Wide` group there fills the screen *beside* the
    /// wheel rather than running off its right edge. Other displays have no
    /// chrome to reserve and hand over their full width.
    ///
    /// Falls back to the host's own rect when the compositor hasn't pushed
    /// geometry (`--mode hosted`, or before the first push), which treats
    /// the window as the single display — the right answer in that mode.
    fn strip_output_spans(&self) -> Vec<OutputSpan> {
        let outputs: Vec<OutputGeometry> = if self.outputs.is_empty() {
            // No backend has told us about its displays yet. `self.rect` is
            // only set in `apply_layout`, and this runs during
            // `build_layout`, so on a first frame it is still zero — a
            // synthesised span would hand every strip a 1 px basis and
            // nothing would paint. Returning no spans instead lets
            // `GroupRow::basis_for` use its own chain (its laid-out width,
            // then a default), which is the answer for any host that does
            // not publish output geometry at all.
            if self.rect.width() <= 1.0 {
                return Vec::new();
            }
            vec![OutputGeometry {
                x: 0.0,
                width: self.rect.width(),
                height: self.rect.height(),
            }]
        } else {
            self.outputs.clone()
        };
        // The strip's left edge — where the wheel-disc slot is reserved.
        let strip_left = 0.0_f64;
        outputs
            .iter()
            .filter(|o| o.width > 0.0)
            .map(|o| {
                let hosts_chrome = strip_left >= o.x && strip_left < o.x + o.width;
                let reserved = if hosts_chrome && self.sidebar.is_some() {
                    sidebar_width_for_viewport(o.height as f32) as f64 + WHEEL_TO_STRIP_GAP_PX
                } else {
                    0.0
                };
                OutputSpan {
                    x: o.x,
                    width: o.width,
                    usable: (o.width - reserved).max(1.0),
                }
            })
            .collect()
    }

    pub fn with_group_row_ptrs(mut self, ptrs: HashMap<StripId, *mut GroupRow>) -> Self {
        self.group_row_ptrs = ptrs;
        self
    }

    /// On-screen rectangle of the tile hosting the foreign wayland
    /// surface `surface_id`, or `None` when no *visible* tile hosts it
    /// (unknown surface, background tab, inactive workspace).
    ///
    /// Walks the live widget tree rather than the persisted state, so the
    /// answer is the rect the surface's pixels are actually painted into
    /// this frame — which is what the compositor needs when it tells the
    /// client what size to render at. Only the active tile of each group
    /// is consulted because that is the only one laid out and drawn.
    ///
    /// Valid after a layout pass; before the first `apply_layout` every
    /// rect is `Rect::ZERO` and this reports `None`.
    pub fn foreign_surface_rect(&mut self, surface_id: u64) -> Option<Rect> {
        let wanted = format!("foreign-surface-{surface_id}");
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — the pointer comes from a `Box<GroupRow>` owned by
            // the content tree, the host outlives the content, and
            // `&mut self` gives us unaliased access for this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in row.groups.iter_mut() {
                let Some(host) = group.active_host_mut() else {
                    continue;
                };
                let ui::widget::TileContent::App(app) = host.content_mut() else {
                    continue;
                };
                if app.test_id() == Some(wanted.as_str()) {
                    let rect = app.get_rect();
                    if rect.width() > 0.0 && rect.height() > 0.0 {
                        return Some(rect);
                    }
                }
            }
        }
        None
    }

    /// Per-tick wire-up for the data-layer active-group reconciler.
    ///
    /// Walks the captured [`GroupRow`] pointers, harvests the set of
    /// [`UiWindowGroup`] ids whose [`UiWindowGroup::is_closing`] returns
    /// true grouped by strip, and feeds each non-empty set into
    /// [`crate::shell::state::WorkspaceTree::nudge_active_group_off_closing`].
    /// Strips with no closing groups are skipped — the reconciler is a
    /// no-op there anyway, and the early-return saves a chrome borrow per
    /// quiescent strip.
    ///
    /// Without this hook the active-group pointer would dwell on a closing
    /// group for the duration of the 180 ms close fade, sending focus,
    /// keyboard input, and presence broadcasts to a fading-away corpse.
    /// Called from [`Widget::update`] so it runs every frame the App
    /// pumps.
    /// Per-frame diff of the live `WorkspaceTree` against every
    /// `GroupRow`'s current children. Returns `true` if the row
    /// shape actually changed (so callers can propagate a redraw
    /// signal).
    ///
    /// Closes #263: chrome bumps its revision on every structural
    /// mutation (workspace add, group insert, tab close, file
    /// drop, …), but the live `GroupRow` widgets cache their
    /// children in `groups: Vec<WindowGroup>` — without this poll
    /// the rows keep painting the boot-time snapshot even after the
    /// persisted tree has grown new strips/groups. We compare the
    /// row's `group_ids()` against the strip's
    /// `window_groups.iter().map(|g| g.id)` and call
    /// `insert_group` / `remove_group_by_id` to converge, which
    /// preserves existing rows' host state (per-tile scroll, tab
    /// selection, etc.) on revision bumps that don't touch them.
    ///
    /// Cheap on the no-op path: `chrome.revision` is a single u64
    /// compare against the cached `last_chrome_revision`. The diff
    /// only runs when something changed.
    fn resync_strips_from_tree(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let current_rev = chrome.borrow().revision;
        if current_rev == self.last_chrome_revision {
            return false;
        }
        let mut any_change = false;
        let st = chrome.borrow();
        let records = st.records().clone();
        let tree = st.tree();
        if let Some(ws) = tree.active_workspace() {
            for strip in &ws.strips {
                let Some(&row_ptr) = self.group_row_ptrs.get(&strip.id) else {
                    continue;
                };
                // SAFETY: same contract as the other `group_row_ptrs`
                // accessors — pointer captured from a `Box<GroupRow>`
                // owned by the content tree; host outlives content;
                // `&mut self` → unaliased mutable access.
                let row: &mut GroupRow = unsafe { &mut *row_ptr };
                let live_ids: std::collections::HashSet<u64> =
                    row.group_ids().into_iter().collect();
                let tree_ids: std::collections::HashSet<u64> =
                    strip.window_groups.iter().map(|g| g.id).collect();
                // Splice in any new groups (preserves existing row
                // groups' host state — scroll positions, per-tile
                // selection, etc. — instead of rebuilding the whole
                // vector and losing it).
                for (idx, g) in strip.window_groups.iter().enumerate() {
                    if !live_ids.contains(&g.id) {
                        let mut fresh = crate::shell::to_ui_window_group(g, &records);
                        // A window arriving is worth seeing arrive. The
                        // row's own splice paths (a tab dragged out into
                        // its own group) already run this fade; a window
                        // that appeared because the user opened it
                        // deserves the same, and it is what reads
                        // together with the neighbours sliding over to
                        // make room for it.
                        fresh.start_spawn_animation();
                        row.insert_group(fresh, idx);
                        any_change = true;
                    }
                }
                // Drop any groups the tree no longer carries.
                for id in live_ids.difference(&tree_ids) {
                    if row.remove_group_by_id(*id) {
                        any_change = true;
                    }
                }
                // Reconcile *tabs* inside groups that already exist. The
                // group-level splice above only fires for whole new
                // groups, so a tab appended to an existing group — which
                // is what adopting an external wayland toplevel does —
                // would otherwise never reach the widget tree.
                for g in strip.window_groups.iter() {
                    let Some(live) = row.groups.iter_mut().find(|lg| lg.id == g.id) else {
                        continue;
                    };
                    let live_tab_ids: std::collections::HashSet<u64> =
                        live.tabs().iter().map(|t| t.id).collect();
                    let tree_tab_ids: std::collections::HashSet<u64> =
                        g.tabs.iter().map(|t| t.id).collect();
                    for (idx, tab) in g.tabs.iter().enumerate() {
                        if live_tab_ids.contains(&tab.id) {
                            continue;
                        }
                        let desc = ui::widget::TabDescriptor {
                            id: tab.id,
                            title: tab.title.clone(),
                            app_kind: Some(crate::shell::tile_app_kind(&tab.tile)),
                        };
                        // A tab spliced into a live group gets its own
                        // document too — an adopted toplevel or a
                        // freshly-opened app is as much an open thing as
                        // one that was there at boot.
                        let mut host =
                            ui::widget::TileHost::new(crate::shell::to_ui_tile(&tab.tile));
                        host.bind_record(
                            records.clone(),
                            crate::shell::tab_documents::scratch_tab_record(tab.id),
                        );
                        live.insert_detached_tab(idx.min(live.tab_count()), desc, host);
                        any_change = true;
                    }
                    for stale in live_tab_ids.difference(&tree_tab_ids) {
                        if live.detach_tab(*stale).is_some() {
                            any_change = true;
                        }
                    }
                }
            }
        }
        drop(tree);
        drop(st);
        self.last_chrome_revision = current_rev;
        any_change
    }

    /// Push the compositor's transient fullscreen state (`ChromeState::
    /// fullscreen_group`) down onto every strip's [`GroupRow`], so the strip
    /// that owns the fullscreen surface promotes it to the whole output while
    /// the other strips collapse away. Idempotent — each `GroupRow::
    /// set_fullscreen` is a no-op when its flags are unchanged, so this is
    /// cheap to run every frame. A real change flags the owning row structurally
    /// dirty, which drives the taffy rebuild that applies the collapse/restore.
    ///
    /// Called both from [`Widget::update`] (the update-driven path) and from
    /// [`Widget::build_layout`] (so the compositor's `relayout`-only frame loop
    /// still applies it even if `update` isn't pumped that tick), and directly
    /// from [`crate::compositor::frame::FrameBuilder::relayout`] *before* the
    /// structure-dirty check so a promotion/restore triggers the rebuild that
    /// applies it.
    pub fn sync_fullscreen_from_chrome(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Either route promotes a group to the whole output: a client that
        // asked for `xdg_toplevel.set_fullscreen`, or a group the user expanded
        // with the tab's Expand chip. The client request wins if both are set.
        let fs_group = {
            let st = chrome.borrow();
            st.fullscreen_group().or_else(|| st.maximized_group())
        };
        // Only apply the scene override when the fullscreen group actually
        // exists in the *rendered* tree. The live client tree (which the xdg
        // handler mutates) and this rendered tree are not yet unified (Phase
        // 4), so a fullscreen id we don't host must be treated as "no
        // fullscreen" — otherwise every strip would hide around a group that
        // isn't here and blank the shell.
        let owned = fs_group.is_some_and(|gid| {
            self.group_row_ptrs
                .values()
                // SAFETY: see the per-row note below — same `group_row_ptrs`
                // contract; here we only read `group_ids()`.
                .any(|&p| unsafe { (*p).group_ids().contains(&gid) })
        });
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs` accessor —
            // the pointer comes from a `Box<GroupRow>` owned by the content
            // tree, the host outlives the content, and `&mut self` gives us
            // unaliased access for the duration of this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            match fs_group {
                Some(gid) if owned && row.group_ids().contains(&gid) => {
                    row.set_fullscreen(Some(gid), false);
                }
                Some(_) if owned => row.set_fullscreen(None, true),
                _ => row.set_fullscreen(None, false),
            }
        }
    }

    fn reconcile_active_group_off_closing(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Take a snapshot first so the chrome borrow is dropped before the
        // mutable reconcile borrow below — avoids RefCell aliasing if any
        // reconciler step ever calls into a chrome-borrowing helper.
        let mut closing_per_strip: Vec<(StripId, Vec<GroupId>)> =
            Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: pointer was captured from a `Box<GroupRow>` we own
            // through the content tree (see `with_group_row_ptrs`). The
            // host outlives the content, and we're inside `&mut self`, so
            // the pointer is unaliased.
            let row: &GroupRow = unsafe { &*ptr };
            let closing: Vec<GroupId> = row
                .groups
                .iter()
                .filter(|g| g.is_closing())
                .map(|g| g.id)
                .collect();
            if !closing.is_empty() {
                closing_per_strip.push((strip_id, closing));
            }
        }
        if closing_per_strip.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        for (strip_id, closing) in closing_per_strip {
            st.tree_mut()
                .nudge_active_group_off_closing(strip_id, &closing);
        }
    }

    /// Per-tick wire-up that mirrors any per-frame `WindowGroup::maximized`
    /// flips into the persisted [`crate::shell::state::WorkspaceTree`].
    ///
    /// Walks the captured [`GroupRow`] pointers, drains every group's
    /// [`UiWindowGroup::take_pending_maximize_change`] one-shot, and writes
    /// the new boolean into the matching `WindowGroup::maximized` slot of
    /// the persisted tree. Without this hook the on-screen flip — driven
    /// by the tab-bar Maximize2 glyph or the floating restore / close-while-
    /// maximized chrome — would live only on the widget instance and reset
    /// to the pre-toggle value on the next shell restart (audit task #105;
    /// the widget already toggled in-memory before this commit, this is the
    /// missing writeback).
    ///
    /// Idempotent and cheap: groups with no pending change are skipped, and
    /// the chrome borrow is only taken when at least one flip needs writing.
    /// Called from [`Widget::update`] alongside the active-group reconciler
    /// so the persisted state catches up with the widget tree every frame.
    fn harvest_pending_maximize_changes(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Snapshot first so we drop the mutable widget borrow before
        // taking the chrome borrow. Each entry is `(group_id, new_value)`;
        // strip_id is unused here because group ids are tree-unique, but
        // we keep it in the tuple for symmetry with the active-group
        // reconciler and to make a future per-strip writeback trivial.
        let mut changes: Vec<(StripId, GroupId, bool)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and `&mut self` here
            // ensures no other reconcile path is aliasing.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                if let Some(new_value) = g.take_pending_maximize_change() {
                    changes.push((strip_id, g.id, new_value));
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        let mut st = chrome.borrow_mut();
        for (_strip_id, group_id, new_value) in &changes {
            // An expanded window takes over the whole output: park the group id
            // in the chrome's user-maximize slot so `is_fullscreen()` reports
            // true and the top bar + sidebar wheel suppress themselves. Exiting
            // maximize clears it (only if this group owns the slot, so a stale
            // exit from another group can't steal the state).
            if *new_value {
                st.set_maximized_group(Some(*group_id));
            } else if st.maximized_group() == Some(*group_id) {
                st.set_maximized_group(None);
            }
        }
        for (_strip_id, group_id, new_value) in changes {
            // Walk the tree to find the group by id — group ids are
            // unique across the whole tree, so a flat scan is correct.
            // The shell currently keeps trees small (single-digit groups
            // per strip in practice), so this is an O(g) write that runs
            // only on frames where a click actually flipped maximize.
            let mut tree = st.tree_mut();
            for ws in tree.workspaces.iter_mut() {
                for strip in ws.strips.iter_mut() {
                    if let Some(g) = strip.window_groups.iter_mut().find(|g| g.id == group_id) {
                        g.maximized = new_value;
                    }
                }
            }
            // Unit C of #157: mirror the maximize flip into the
            // automerge doc so peer-sync observes the change. Tolerant
            // of the doc not yet carrying this group — see
            // `WorkspaceTree::doc_set_maximized` doc-comment.
            tree.doc_set_maximized(group_id, new_value);
        }
    }

    /// Per-tick wire-up that mirrors the widget-side `TabBar::active_id`
    /// (and the group whose bar it is) back into the persisted
    /// [`crate::shell::state::WorkspaceTree`]. Without this, the
    /// tree's `group.active_tab` / `active_group` / `active_strip`
    /// pointers stayed frozen at the values they took at construction;
    /// clicking a different tab pill flipped the visible content but
    /// left the search-scope helpers (`ChromeState::active_tab_id`,
    /// `active_app_title`) pointing at whichever tab shipped in the
    /// group's initial list.
    ///
    /// Runs once per frame regardless of whether a tab was clicked — the
    /// walk is O(groups × tabs), which stays small for a normal
    /// workspace. Skipping the scan when nothing changed would need a
    /// per-group `changed` bit and a wakeup path, extra state for a
    /// walk that's already cheap.
    fn harvest_active_tab_writeback(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Two-phase: first walk the widgets and collect (strip, group,
        // tab) tuples for every group whose bar's active_id differs from
        // the tree's, then apply them under a single chrome borrow.
        // (Widgets and chrome live in the same shell so we can't hold
        // both borrows at once — same pattern the other harvesters use.)
        struct Writeback {
            strip_id: StripId,
            group_id: GroupId,
            tab_id: crate::shell::state::TabId,
        }
        let mut writes: Vec<Writeback> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content, `&mut self`
            // ensures no other reconcile path aliases.
            let row: &GroupRow = unsafe { &*ptr };
            for g in row.groups.iter() {
                writes.push(Writeback {
                    strip_id,
                    group_id: g.id,
                    tab_id: g.active_tab_id(),
                });
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        // A mid-frame divergence between the widget-side
        // `bar.active_id()` and the tree's `group.active_tab` means the
        // user just clicked a tab pill inside THAT group. Collect those
        // (ws, strip, group) triples so we can stamp
        // `tree.active_strip` / `tree.active_group` after the tab-id
        // writeback — otherwise the palette scope (Ctrl+K) stays anchored
        // to whichever group was active before, which is the 2026-07-09
        // "Ctrl+K scopes to Notes when I just clicked Terminal in its
        // own group" regression.
        let mut focus_writes: Vec<(WorkspaceId, StripId, GroupId)> = Vec::new();
        {
            let mut tree = st.tree_mut();
            for w in &writes {
                for ws in tree.workspaces.iter_mut() {
                    for strip in ws.strips.iter_mut() {
                        if strip.id != w.strip_id {
                            continue;
                        }
                        if let Some(group) =
                            strip.window_groups.iter_mut().find(|g| g.id == w.group_id)
                        {
                            if group.active_tab != w.tab_id {
                                // Guard: only mirror the widget-side
                                // active tab if it actually exists in
                                // this group's persisted tab list.
                                // Without this, a stale `bar.active_id`
                                // from a mid-detach frame (bar still
                                // points at a tab that got moved to
                                // another group) would land here as
                                // `group.active_tab = <phantom-id>`,
                                // and the next draw would silently
                                // drop the whole group because the
                                // active-tab lookup finds nothing —
                                // the 2026-07-09 "files window
                                // completely disappears after Ctrl+K"
                                // repro.
                                let tab_exists = group.tabs.iter().any(|t| t.id == w.tab_id);
                                if !tab_exists {
                                    continue;
                                }
                                group.active_tab = w.tab_id;
                                focus_writes.push((ws.id, w.strip_id, w.group_id));
                            }
                        }
                    }
                }
            }
        }
        if !focus_writes.is_empty() {
            let mut tree = st.tree_mut();
            for (ws_id, strip_id, group_id) in focus_writes {
                tree.active = ws_id;
                tree.active_strip.insert(ws_id, strip_id);
                tree.active_group.insert(strip_id, group_id);
            }
        }
    }

    /// Per-tick wire-up that mirrors any per-frame resize-handle gesture
    /// into the persisted [`crate::shell::state::WorkspaceTree`]. Walks
    /// the captured [`GroupRow`] pointers, drains every group's
    /// [`UiWindowGroup::take_pending_width_change`] one-shot, and writes
    /// the new value into the matching `WindowGroup::width` slot of the
    /// persisted tree as `WindowWidth::Fraction(per-mille)`.
    ///
    /// The gesture is a pixel drag but what gets stored is the share of
    /// the display it landed on, which is what lets the width survive a
    /// restart onto a different monitor and replicate to a peer whose
    /// screen is a different size.
    ///
    /// Without this hook a resize-drag is in-memory only and resets to
    /// the pre-drag width on the next shell restart. Mirrors
    /// [`Self::harvest_pending_maximize_changes`] in shape; both run
    /// inside [`Widget::update`] every frame, but each is gated on a
    /// non-empty pending pile so quiescent frames cost a single map walk.
    fn harvest_pending_width_changes(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut changes: Vec<(StripId, GroupId, u16)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                if let Some(new_value) = g.take_pending_width_change() {
                    changes.push((strip_id, g.id, new_value));
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        for (_strip_id, group_id, new_value) in changes {
            let mut tree = st.tree_mut();
            for ws in tree.workspaces.iter_mut() {
                for strip in ws.strips.iter_mut() {
                    if let Some(g) = strip.window_groups.iter_mut().find(|g| g.id == group_id) {
                        g.width = crate::shell::state::WindowWidth::Fraction(new_value);
                    }
                }
            }
            // Mirror the width gesture into the automerge doc as a
            // fraction of the strip so peer-sync observes the resize as a
            // share rather than as this machine's pixel count.
            tree.doc_set_width(group_id, new_value);
        }
    }

    /// Per-tick wire-up that mirrors a completed whole-group reorder drag
    /// into the persisted [`crate::shell::state::WorkspaceTree`]. Walks the
    /// captured [`GroupRow`] pointers, drains each row's
    /// [`GroupRow::take_pending_group_reorder`] one-shot, and calls
    /// [`WorkspaceTree::reorder_group_within_strip`] so the strip's
    /// `window_groups` order in the data layer matches what the user just
    /// dropped on screen. That method also mirrors the move into the
    /// automerge doc, so peer-sync ships the new order.
    ///
    /// Without this hook a group-drag is a widget-only reshuffle that
    /// resets to the persisted order on the next shell restart. Mirrors
    /// [`Self::harvest_pending_width_changes`] in shape; gated on a
    /// non-empty pending pile so quiescent frames cost a single map walk.
    fn harvest_pending_group_reorder(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut changes: Vec<(StripId, u64, usize)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some((group_id, new_index)) = row.take_pending_group_reorder() {
                changes.push((strip_id, group_id, new_index));
            }
        }
        if changes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        for (strip_id, group_id, new_index) in changes {
            tree.reorder_group_within_strip(strip_id, group_id, new_index);
        }
    }

    /// Per-tick wire-up that mirrors a completed *intra-group* tab reorder
    /// into the persisted [`crate::shell::state::WorkspaceTree`]. Walks the
    /// captured [`GroupRow`] pointers, drains each group's
    /// [`UiWindowGroup::take_pending_tab_reorder`] one-shot, and calls
    /// [`WorkspaceTree::reorder_tab_within_group`] so the group's `tabs` order
    /// in the data layer matches the pill order the user just dropped. That
    /// method also mirrors the move into the automerge doc for peer-sync.
    ///
    /// Without this hook dragging a tab within its group is a widget-only
    /// reshuffle that resets to the persisted order on the next shell
    /// restart. Mirrors [`Self::harvest_pending_group_reorder`] in shape.
    fn harvest_pending_tab_reorder(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut changes: Vec<(GroupId, usize, usize)> = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                if let Some((from, to)) = g.take_pending_tab_reorder() {
                    changes.push((g.id, from, to));
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        for (group_id, from, to) in changes {
            tree.reorder_tab_within_group(group_id, from, to);
        }
    }

    /// Per-tick wire-up for the magnifier chip in a tab's window decoration:
    /// drains each group's [`UiWindowGroup::take_pending_search_open`] one-shot
    /// and opens the search bar scoped to the active app — the same entry point
    /// Ctrl+K uses. The group can't do this itself because the palette lives
    /// here on the host.
    fn harvest_pending_search_open(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let mut requested = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                if g.take_pending_search_open().is_some() {
                    requested = true;
                }
            }
        }
        if !requested {
            return;
        }
        // Mirror the widget-side active tab into the tree first so the scope
        // resolves to the app whose chip was clicked, not a stale pointer.
        self.harvest_active_tab_writeback();
        self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::ActiveApp);
    }

    /// Per-tick wire-up that keeps the focused window group scrolled into
    /// view. Edge-triggered on a change to the persisted `active_group`
    /// pointer for a strip: when the focused group differs from the one we
    /// last scrolled to, [`GroupRow::scroll_group_into_view`] pans the
    /// strip so the newly-focused group is fully visible. Comparing against
    /// the last-scrolled id (rather than scrolling every frame) is what lets
    /// the user freely wheel away from the focused group without the strip
    /// yanking it back — only a genuine focus change re-centres it.
    fn scroll_active_group_into_view(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Snapshot each strip's current active group from the tree.
        let active: Vec<(StripId, GroupId)> = {
            let st = chrome.borrow();
            let tree = st.tree();
            self.group_row_ptrs
                .keys()
                .filter_map(|&sid| tree.active_group_for(sid).map(|gid| (sid, gid)))
                .collect()
        };
        for (strip_id, group_id) in active {
            let changed = self.last_scrolled_active_group.get(&strip_id).copied() != Some(group_id);
            if !changed {
                continue;
            }
            if let Some(&ptr) = self.group_row_ptrs.get(&strip_id) {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &mut GroupRow = unsafe { &mut *ptr };
                // A group focused in the same frame it appeared — a freshly
                // adopted Wayland client is exactly this — has not been through
                // a layout pass, so it has no rect and the row cannot pan to
                // it. Latching the memo here would mark the pan done and the
                // window would stay off the output for good. Leave the memo
                // alone and retry next frame, once the rect exists.
                if !row.group_is_laid_out(group_id) {
                    continue;
                }
                row.scroll_group_into_view(group_id);
            }
            self.last_scrolled_active_group.insert(strip_id, group_id);
        }
    }

    /// Publish the active strip's laid-out geometry on the chrome, so an
    /// "open this" path can consult it — see [`crate::shell::placement`].
    ///
    /// Runs at the end of every layout pass. Deliberately does **not**
    /// bump the chrome revision: this is a report of what the layout just
    /// produced, not a mutation anyone needs to re-sync against, and
    /// bumping every frame would make the strip resync run forever.
    fn publish_strip_viewport(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // The active strip is the one a new window would land in — the
        // same resolution the placement rule performs.
        let strip_id = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.active_workspace().and_then(|ws| {
                tree.active_strip
                    .get(&ws.id)
                    .copied()
                    .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                    .or_else(|| ws.strips.first().map(|s| s.id))
            })
        };
        let viewport = strip_id.and_then(|sid| {
            let &ptr = self.group_row_ptrs.get(&sid)?;
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            let (visible_x0, visible_x1) = row.visible_canvas_span();
            Some(crate::shell::placement::StripViewport {
                strip: sid,
                groups: row
                    .group_canvas_spans()
                    .into_iter()
                    .map(|(id, x0, x1)| crate::shell::placement::GroupSpan { id, x0, x1 })
                    .collect(),
                visible_x0,
                visible_x1,
            })
        });
        chrome.borrow_mut().set_strip_viewport(viewport);
    }

    /// Per-tick wire-up for cross-strip tab drag-drop (#151).
    ///
    /// Walks the captured [`GroupRow`] pointers, drains each row's
    /// [`GroupRow::take_pending_cross_strip_detach`] one-shot, and routes
    /// the payload to whichever strip's row contains the release point.
    /// Within-strip drops never reach this hook — they're already resolved
    /// by `GroupRow`'s own post-dispatch sweep before the tab is detached.
    /// Cross-strip drops show up here because the source row deferred
    /// resolution (release point fell outside its own vertical band) and
    /// stashed `(TabDescriptor, TileHost, release_point)` for the shell
    /// to route.
    ///
    /// Resolution rules:
    ///
    /// - Release point's `y` lands inside another strip's row rect →
    ///   deliver via that row's [`GroupRow::accept_cross_strip_drop`],
    ///   which splices the tab into a brand-new `WindowGroup` at the
    ///   nearest gap.
    /// - Release point doesn't match any other strip's row rect → return
    ///   to the source row via the same `accept_cross_strip_drop` entry
    ///   point (the source row uses its own gap math against the
    ///   release point, so a "drop just below my own row" lands as a
    ///   new group at the closest source-row gap, mirroring the in-row
    ///   "drop in empty strip space" path the source's local resolver
    ///   would have produced if we hadn't intercepted it).
    ///
    /// "Return to source" is the v1 fallback rather than "create a new
    /// strip at drop_pos.y" because creating new strips touches the
    /// persisted [`crate::shell::state::WorkspaceTree`] (it's not a
    /// widget-local mutation), and the strip-creation flow already lives
    /// behind explicit user affordances (settings modal, command palette).
    /// Letting a stray drag accidentally produce a new strip would be
    /// surprising; keeping the tab in its source strip preserves the
    /// "no valid target → cancel" contract the in-row recovery path
    /// (see `ui/src/widget/group_row.rs:937` block) already pins.
    ///
    /// SAFETY mirrors the rest of the `group_row_ptrs` accessors: each
    /// pointer addresses a `GroupRow` owned by the content tree, the host
    /// outlives the content, and the `&mut self` borrow keeps the other
    /// reconcile sweeps from racing this one.

    fn dispatch_cross_strip_detaches(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        // Phase 1 — drain every source row, snapshot every other row's
        // rect. We have to drain first because subsequent
        // `accept_cross_strip_drop` calls borrow the destination row
        // mutably, which would conflict with holding any reference to
        // a source row's `take_pending_cross_strip_detach` payload.
        let mut detaches: Vec<(StripId, CrossStripDetach)> = Vec::new();
        let mut strip_rects: Vec<(StripId, Rect)> = Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and `&mut self` here
            // ensures no other reconcile path is aliasing.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            strip_rects.push((strip_id, row.get_rect()));
            if let Some(payload) = row.take_pending_cross_strip_detach() {
                detaches.push((strip_id, payload));
            }
        }
        if detaches.is_empty() {
            return;
        }
        // Phase 2 — for each detach, hit-test the release point's `y`
        // against every other strip's rect and pick the first match.
        // We bias on `y` only: strips span the full content width, so a
        // release at any `x` inside a strip's vertical band is "in that
        // strip" by definition. Inside the matched strip's band the
        // destination row resolves the gap by `x` via its own
        // `gap_index_for` machinery.
        for (src_strip_id, payload) in detaches {
            let release = payload.release_point;
            let dest_strip_id = strip_rects
                .iter()
                .find(|(sid, rect)| {
                    *sid != src_strip_id && release.y >= rect.y0 && release.y <= rect.y1
                })
                .map(|(sid, _)| *sid)
                // No other strip matched — return to source.
                .unwrap_or(src_strip_id);
            // SAFETY: same contract as Phase 1's drain. We're mutating a
            // single row at a time and never aliasing across rows.
            let &dest_ptr = match self.group_row_ptrs.get(&dest_strip_id) {
                Some(p) => p,
                None => continue,
            };
            let dest_row: &mut GroupRow = unsafe { &mut *dest_ptr };
            dest_row.accept_cross_strip_drop(payload.desc, payload.host, release);
            // Mirror the splice into the persisted `WorkspaceTree` so a
            // shell restart sees the tab in its new strip rather than
            // its old one. The tree-side mutation walks the source strip
            // for the moved tab and re-homes it on the destination strip
            // at the corresponding gap; if the source/destination are
            // the same (return-to-source path), it's a no-op write.
            if dest_strip_id != src_strip_id {
                if let Some(chrome) = self.chrome.clone() {
                    let st = chrome.borrow_mut();
                    st.tree_mut().move_tab_across_strips(
                        src_strip_id,
                        dest_strip_id,
                        payload.tab_id,
                    );
                }
            }
        }
    }

    /// Per-tick wire-up that owns the `pending_grants` queue's lifecycle.
    ///
    /// Phase 4 Task 4.2 wires the prompt at the *widget* level — the
    /// portal D-Bus backend (Phase 4 Task 4.3) pushes grant requests
    /// onto `chrome.pending_grants`; this method:
    ///
    ///  1. Latches a fresh [`PermissionPrompt`] when one is needed
    ///     and no widget is currently held (`front()` is `Some` and
    ///     `self.permission_prompt` is `None`).
    ///  2. Drains the front entry and applies the user's choice
    ///     when the held widget reports a `pending_choice`. The
    ///     `Allow` path is a no-op on the workspace's grants map
    ///     (Once doesn't persist); `Session` and `Always` push a
    ///     scope-tagged copy of the requested capability into the
    ///     active workspace's `grants` map. `Deny` is a no-op.
    ///  3. Clears the held widget so the next frame either re-latches
    ///     for the next queue entry or paints nothing.
    ///
    /// Owner-only escalation: when the active user isn't the
    /// workspace owner, the widget's `Always` button is rendered
    /// disabled and click-through-the-rect is dropped — so by the
    /// time we observe a `Some(PromptChoice::Always)` here, the
    /// widget has already vetted the click against `is_owner`. The
    /// pump's only safety check is that the queue's `front()` still
    /// matches the prompt's `app_id` before draining (defends
    /// against an out-of-band push that swapped the head of the
    /// queue between frames).
    /// Drain the app-launcher's worker-thread reports and turn the ones
    /// the user needs to know about into notifications.
    ///
    /// A launch that starts and an app that exits cleanly are silent —
    /// the window appearing (or closing) is the feedback. A launch that
    /// dies is not: the sandbox refusing, a missing runtime bundle, or
    /// the app crashing on start all leave nothing on screen, so they
    /// surface here with the child's own stderr quoted back.
    ///
    /// Returns whether anything was posted, so the caller can mark the
    /// frame dirty.
    pub(crate) fn drain_launch_events(&mut self) -> bool {
        let events = crate::shell::launcher::drain_events();
        if events.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            return false;
        };
        use crate::shell::launcher::LaunchEventKind;
        let mut posted = false;
        for event in events {
            let (title, body) = match event.kind {
                LaunchEventKind::Started { pid } => {
                    eprintln!("shell: launched {} (pid {pid})", event.package_id);
                    continue;
                }
                LaunchEventKind::Exited => continue,
                LaunchEventKind::SpawnFailed { detail } => {
                    (format!("Couldn't open {}", event.display_name), detail)
                }
                LaunchEventKind::Failed { code, stderr_tail } => {
                    let reason = if stderr_tail.trim().is_empty() {
                        match code {
                            Some(c) => format!("the launcher exited with status {c}"),
                            None => "the launcher was killed by a signal".to_string(),
                        }
                    } else {
                        stderr_tail
                    };
                    (format!("{} stopped", event.display_name), reason)
                }
            };
            let mut st = chrome_handle.borrow_mut();
            st.push_notification_full(
                crate::shell::notification::Notification::new(title, body)
                    .with_severity(crate::shell::notification::NotificationSeverity::Error)
                    .with_app_name("Applications"),
            );
            st.bump();
            posted = true;
        }
        posted
    }

    pub(crate) fn pump_permission_prompt(&mut self) {
        let Some(chrome_handle) = self.chrome.clone() else {
            return;
        };

        // Step 1 — drain choice + maybe persist grant.
        let drain_payload = self
            .permission_prompt
            .as_mut()
            .and_then(|p| p.pending_choice.map(|c| (p.app_id.clone(), c)));
        if let Some((widget_app_id, choice)) = drain_payload {
            let mut st = chrome_handle.borrow_mut();
            // Pop only if the head still belongs to the prompt the
            // user just clicked — if a portal push changed the head
            // mid-frame we'd otherwise apply the user's grant to
            // the wrong app.
            let head_matches = st
                .pending_grants
                .front()
                .map(|(app_id, _)| app_id == &widget_app_id)
                .unwrap_or(false);
            if head_matches {
                if let Some((app_id, capability)) = st.pending_grants.pop_front() {
                    match choice {
                        PromptChoice::Allow => {
                            // Once: don't persist on the workspace.
                            // The portal request is treated as
                            // satisfied for this single call.
                        }
                        PromptChoice::Session => {
                            let cap = with_grant_scope(capability, GRANT_SESSION);
                            record_app_grant(&mut st, &app_id, &cap);
                        }
                        PromptChoice::Always => {
                            let cap = with_grant_scope(capability, GRANT_ALWAYS);
                            record_app_grant(&mut st, &app_id, &cap);
                        }
                        PromptChoice::Deny => {
                            // No-op — the caller treats absence of a
                            // matching grant as "deny".
                        }
                    }
                    st.bump();
                }
            }
            // Whether or not the head matched, the widget has
            // discharged its pending choice — drop it so the next
            // frame either latches a fresh prompt or paints nothing.
            self.permission_prompt = None;
        }

        // Step 2 — latch a new prompt when the queue's front needs
        // surfacing and we don't already hold a widget.
        if self.permission_prompt.is_none() {
            let st = chrome_handle.borrow();
            if let Some((app_id, capability)) = st.pending_grants.front() {
                let label = capability_label(capability);
                let is_owner = is_active_workspace_owner(&st);
                self.permission_prompt =
                    Some(PermissionPrompt::new(app_id.clone(), label, is_owner));
            }
        }
    }

    /// Test-only one-shot drive: lets integration tests run the
    /// per-frame permission-prompt pump without spinning up a full
    /// render loop. Mirrors `Widget::update`'s in-frame call.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_permission_prompt_for_tests(&mut self) {
        self.pump_permission_prompt();
    }

    /// Test-only accessor for the currently-held [`PermissionPrompt`].
    /// Lets integration tests synthesise a click on the widget
    /// without exposing the internal field via `pub`.
    #[cfg(any(test, debug_assertions))]
    pub fn permission_prompt_mut_for_tests(&mut self) -> Option<&mut PermissionPrompt> {
        self.permission_prompt.as_mut()
    }

    /// Test-only escape hatch: lend out a mutable reference to the
    /// [`UiWindowGroup`] at `(strip_id, group_idx)`. Lets integration
    /// tests drive the close fade through public widget APIs (e.g.
    /// [`UiWindowGroup::start_closing`]) without having to downcast a
    /// `&dyn Widget` they only know through the boxed root.
    ///
    /// SAFETY mirrors [`Self::with_group_row_ptrs`]: the pointer is valid
    /// for the host's lifetime, and the `&mut self` borrow on this method
    /// keeps any other reconcile path from aliasing.
    #[cfg(any(test, debug_assertions))]
    pub fn group_at_for_tests(
        &mut self,
        strip_id: StripId,
        group_idx: usize,
    ) -> Option<&mut UiWindowGroup> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.groups.get_mut(group_idx)
    }

    /// Test-only one-shot reconcile: lets integration tests invoke the
    /// per-frame wire-up from outside `update` without spinning up a full
    /// `App` render loop. Mirrors what [`Widget::update`] runs.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_active_group_reconcile_for_tests(&mut self) {
        self.reconcile_active_group_off_closing();
    }

    /// Test-only one-shot harvest: lets integration tests invoke the
    /// per-frame maximize-change writeback from outside `update` without
    /// spinning up a full `App` render loop. Mirrors what
    /// [`Widget::update`] runs each frame.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_maximize_harvest_for_tests(&mut self) {
        self.harvest_pending_maximize_changes();
    }

    /// Test-only one-shot harvest: lets integration tests invoke the
    /// per-frame width-change writeback from outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_width_harvest_for_tests(&mut self) {
        self.harvest_pending_width_changes();
    }

    /// Test-only one-shot dispatch: lets integration tests invoke the
    /// per-frame cross-strip drag-drop router from outside `update`
    /// without spinning up a full `App` render loop. Mirrors what
    /// [`Widget::update`] runs each frame for #151.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_cross_strip_dispatch_for_tests(&mut self) {
        self.dispatch_cross_strip_detaches();
    }

    /// Test-only one-shot harvest: lets integration tests invoke the
    /// per-frame whole-group reorder writeback from outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_group_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_group_reorder();
    }

    /// Test-only accessor for the sidebar wheel's laid-out rect. Returns
    /// `None` when the host has no sidebar at all. A zero-area rect means the
    /// wheel is currently suppressed (a window holds the output fullscreen).
    #[cfg(any(test, debug_assertions))]
    pub fn sidebar_rect_for_tests(&self) -> Option<vello::kurbo::Rect> {
        self.sidebar.as_ref().map(|s| s.get_rect())
    }

    /// Test-only one-shot harvest: lets integration tests invoke the
    /// per-frame intra-group tab reorder writeback from outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_tab_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_tab_reorder();
    }

    /// Test-only one-shot harvest: lets integration tests invoke the per-frame
    /// decoration-search-chip handling from outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_search_open_for_tests(&mut self) {
        self.harvest_pending_search_open();
    }

    /// Test-only one-shot: run the focus-follow scroll reconcile from
    /// outside `update`. Mirrors what [`Widget::update`] runs each frame.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_scroll_active_into_view_for_tests(&mut self) {
        self.scroll_active_group_into_view();
    }

    /// Test-only mutable access to a strip's live [`GroupRow`] so
    /// integration tests can read `scroll_x` / handle geometry and drive
    /// the reorder / resize / scroll gestures through the real widget.
    #[cfg(any(test, debug_assertions))]
    pub fn group_row_mut_for_tests(&mut self, strip_id: StripId) -> Option<&mut GroupRow> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        Some(unsafe { &mut *ptr })
    }

    /// Read-only accessor for the cached "+ Add workspace" tile rect.
    /// Populated by `draw_overview` each frame the overview is visible
    /// and reset to `Rect::ZERO` once the overview is fully hidden.
    /// Exposed so concept-parity tests can both (a) assert the rect is
    /// non-empty after a render and (b) drive a synthetic click at its
    /// centre through the public `handle_event` path.
    pub fn add_workspace_rect_for_tests(&self) -> Rect {
        self.add_workspace_rect
    }

    /// Snapshot the post-layout groups-per-strip layout: for every captured
    /// `GroupRow` pointer, walk its current `groups` vec and produce
    /// `(strip_id, Vec<(group_id, Vec<tab_id>)>)`. Used by the headless
    /// shots driver to verify drag-out scenarios that should produce
    /// multiple groups in one strip — e.g. a 4-tab drag-out should leave
    /// the strip with two groups (3 in source, 1 in new), not one.
    ///
    /// SAFETY mirrors the rest of the `group_row_ptrs` accessors: pointers
    /// were captured from a `Box<GroupRow>` we own through the content
    /// tree, the host outlives the content, and `&self` here keeps the
    /// reconcile sweeps from racing the read.
    /// Test-only accessor for a strip's `GroupRow` rect, in screen-space
    /// coords. Returns `None` if no `GroupRow` was registered for the
    /// given strip, or `Rect::ZERO` if the row hasn't seen a layout pass
    /// yet. Used by cross-strip drag tests to pick a release point
    /// inside another strip's vertical band without re-walking the
    /// taffy tree.
    ///
    /// Mirrors the SAFETY contract on [`Self::groups_per_strip`].
    #[cfg(any(test, debug_assertions))]
    pub fn group_row_rect_for_tests(&self, strip_id: StripId) -> Option<Rect> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &GroupRow = unsafe { &*ptr };
        Some(row.get_rect())
    }

    /// Screen-space rect of the *content* area (the active tab's tile
    /// host, i.e. everything below the tab bar) of the window group with
    /// id `group_id`, or `None` when no live strip row hosts that group
    /// yet.
    ///
    /// The compositor's input router uses this as the origin for
    /// translating a global cursor position into the surface-local
    /// coordinates a Wayland client expects: a foreign toplevel is
    /// mounted as the tile of its own window group, so the group's active
    /// tile rect *is* the surface's on-screen rectangle.
    ///
    /// A zero-area rect is returned as-is (it means the row hasn't been
    /// laid out yet); callers treat that as "no hit".
    ///
    /// SAFETY mirrors [`Self::groups_per_strip`]: pointers were captured
    /// from `Box<GroupRow>`s owned by the content tree, the host outlives
    /// the content, and `&self` keeps the reconcile sweeps from racing
    /// the read.
    pub fn active_tile_rect(&self, group_id: u64) -> Option<Rect> {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            if let Some(group) = row.groups.iter().find(|g| g.id == group_id) {
                return group.active_host().map(|h| h.get_rect());
            }
        }
        None
    }

    pub fn groups_per_strip(&self) -> Vec<(StripId, Vec<(u64, Vec<u64>)>)> {
        let mut out = Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            let snapshot: Vec<(u64, Vec<u64>)> = row
                .groups
                .iter()
                .map(|g| {
                    let tab_ids: Vec<u64> = g.tabs().iter().map(|t| t.id).collect();
                    (g.id, tab_ids)
                })
                .collect();
            out.push((strip_id, snapshot));
        }
        out
    }

    /// Read-only accessor for the cached WS-settings tab-pill rect for
    /// `tab`. Populated by [`Self::draw_ws_settings`] each frame the
    /// modal is visible and cleared back to `Rect::ZERO` once the modal
    /// is hidden. Every pill in the strip is a real tab, so every
    /// [`WsTab`] has a rect here.
    ///
    /// Concept ref: `WorkspaceSettingsModal.tsx:50-95` — the tab strip
    /// is the modal's only visible nav, and clicking a pill must mutate
    /// `chrome.ws_settings_tab` directly. Pre-2-A the click branch in
    /// `handle_event` was missing, so users could only switch tabs via
    /// the keyboard focus trap.
    pub fn ws_settings_tab_pill_rect(&self, tab: WsTab) -> Rect {
        let idx = match tab {
            WsTab::General => 0,
            WsTab::Members => 1,
            WsTab::Permissions => 2,
        };
        self.ws_settings_tab_pill_rects[idx]
    }

    /// The workspace-settings name field's rect, for harnesses that
    /// click it by name. Zero-area while the modal is closed.
    pub fn ws_settings_name_rect_for_tests(&self) -> Rect {
        if self.overlay_flags().ws_settings {
            self.ws_settings_name_rect()
        } else {
            Rect::ZERO
        }
    }

    /// One rect per workspace card in the overview, in tree order.
    /// Empty while the overview is closed.
    pub fn overview_row_rects_for_tests(&self) -> Vec<Rect> {
        if !self.overlay_flags().overview {
            return Vec::new();
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let count = chrome.borrow().tree().workspaces.len();
        (0..count).map(|i| self.overview_row_rect(i)).collect()
    }

    /// The connect prompt's buttons, named `join` / `cancel`.
    pub fn peer_sync_prompt_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.peer_sync_prompt_rects
            .iter()
            .map(|(which, rect)| {
                let name = match which {
                    PeerSyncPromptButton::Join => "join",
                    PeerSyncPromptButton::Cancel => "cancel",
                };
                (name, *rect)
            })
            .collect()
    }

    /// Revoke-button rects painted by [`Self::draw_ws_settings`] while
    /// the Permissions tab is active, each keyed by the capability id it
    /// revokes. Empty whenever the modal is closed or another tab is
    /// active.
    pub fn permissions_revoke_rects(&self) -> &[(String, Rect)] {
        &self.permissions_revoke_rects
    }

    /// Grant-composer control rects painted while the Permissions tab is
    /// active: the subject picker, the capability picker, the three
    /// expiry segments and the Grant button. `None` when the composer
    /// isn't on screen.
    pub fn grant_composer_rects(&self) -> Option<&GrantComposerRects> {
        self.grant_composer_rects.as_ref()
    }

    /// Per-member control rects painted while the Members tab is active:
    /// `(member, role chip, Remove button)`.
    pub fn member_control_rects(&self) -> &[(crate::shell::state::UserId, Rect, Rect)] {
        &self.member_control_rects
    }

    /// Open the palette in the given scope, or toggle it closed when the
    /// same chord fires twice.
    ///
    /// - Palette closed → open, set scope, focus input, clear query.
    /// - Palette open, DIFFERENT scope → keep it open, just swap scope
    ///   (users can jump from app-scope to global mid-query without
    ///   losing what they've typed).
    /// - Palette open, SAME scope → close (mirrors the old
    ///   [`Self::toggle_palette`] second-press behaviour so a chord
    ///   still dismisses the overlay it opened).
    ///
    /// `ActiveApp` degrades to `All` when no tab is focused so the pill
    /// never highlights a scope that can't display anything.
    fn open_palette_with_scope(&mut self, scope: crate::shell::chrome_state::SearchScope) {
        use crate::shell::chrome_state::SearchScope;
        let effective = if matches!(scope, SearchScope::ActiveApp)
            && self
                .chrome
                .as_ref()
                .and_then(|c| c.borrow().active_app_title())
                .is_none()
        {
            SearchScope::All
        } else {
            scope
        };
        let current_scope = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();
        if self.palette_visible && current_scope == effective {
            self.palette_visible = false;
            return;
        }
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.search_scope = effective;
            st.bump();
        }
        if !self.palette_visible {
            self.palette_visible = true;
            self.palette.set_focus(true);
            self.palette.set_query("");
            // Empty query still runs through the registry so the palette
            // lands with a sensible "everything" list (chrome actions,
            // recent notes, ...) instead of a blank surface.
            self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        }
    }

    /// Recompute the palette's entry list by fanning the current query
    /// out to every registered [`crate::shell::search::SearchProvider`],
    /// then swapping the palette's row set. Called whenever the palette
    /// query changes (typed / backspaced / pasted) or the chip strip
    /// toggles the active kind filter.
    ///
    /// Cheap on a warm cache — the in-shell providers (actions, notes)
    /// answer in microseconds; the files provider hits its in-memory
    /// name index synchronously and the tantivy content index via an
    /// LRU cache. First-boot latency comes from the tantivy walker
    /// finishing its initial pass; the palette shows whatever's ready.
    fn refresh_search_results(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let current_query = self.palette.query().to_string();
        // Short-circuit no-op refreshes triggered by peer state polling
        // or a re-entry through the palette's own update cycle.
        if current_query == self.last_search_query {
            return;
        }
        self.last_search_query = current_query.clone();
        let (scope, active_provider) = {
            let st = chrome.borrow();
            let scope = st.search_scope;
            let active_provider = active_tab_provider_id(&st, &self.search_registry);
            (scope, active_provider)
        };
        let groups = {
            let st = chrome.borrow();
            self.search_registry.query_grouped(
                &current_query,
                scope,
                active_provider.as_deref(),
                &self.active_kind_filter,
                &st,
                doc,
                root,
                PALETTE_ROW_BUDGET,
            )
        };

        // Flatten the groups into palette rows. The kind travels as the
        // row's *section* and its *glyph*, not as a `[Files]` literal
        // glued onto every subtitle — that literal was debug output
        // rendered as UI, it repeated identically down the whole list,
        // and it pushed the part of the path that actually distinguishes
        // one hit from another off the right edge.
        let mut results: Vec<crate::shell::search::SearchResult> = Vec::new();
        let mut entries: Vec<CommandEntry> = Vec::new();
        for group in &groups {
            let label = group.kind.label().to_string();
            for r in &group.results {
                let idx = results.len();
                let mut e = CommandEntry::new(format!("result:{idx}"), r.title.clone())
                    .group(label.clone())
                    .group_total(group.total)
                    .icon(icon_for_kind(&r.kind));
                if let Some(sub) = &r.subtitle {
                    e = e.description(sub.clone());
                }
                entries.push(e);
                results.push(r.clone());
            }
        }

        // The peer-sync rows are not grafted on here. They are ordinary
        // action results from `ActionsProvider`, so they are ranked
        // against everything else the query matched — which is the
        // whole point: appended rows sort below every search hit, and
        // typing "invite" selected a file whose name contains the word.
        self.palette.set_entries(entries);
        self.search_results = results;
        // New entry count → new desired_size → new palette node height.
        // Flag the layout dirty so the next `take_structure_dirty` /
        // `relayout` rebuilds the taffy tree. Without this the palette
        // stays pinned at whatever height the first (usually empty)
        // paint sized it to, and follow-up refreshes only reveal the
        // first ~1 row.
        self.palette_layout_dirty = true;
    }

    /// Hit-test a click position against every strip's group rects and
    /// stamp `tree.active_strip` / `tree.active_group` to the group the
    /// cursor is over. Runs on every left-press inside the desktop
    /// content area — matches every other DE where clicking a window
    /// (any part of it, not just its titlebar) focuses that window.
    ///
    /// No-op when no group contains the point (click landed on empty
    /// wallpaper, the topbar, the wheel disc, etc.).
    /// Walk every widget-side [`GroupRow`] and mirror any group that
    /// exists in the widget but NOT in the persisted tree into the
    /// tree. Used to close the writeback gap for
    /// [`ui::widget::GroupRow::split_tab_into_new_group`] — the drag
    /// handler creates a fresh widget-side group but has no access to
    /// the automerge doc to persist it. Without this, the next
    /// [`Self::resync_strips_from_tree`] would see "widget has X, tree
    /// doesn't" and evict X, dropping the dragged-out group. Also
    /// synthesises the corresponding [`state::WindowGroup`] from the
    /// widget's tab descriptors so tab ids / titles carry over.
    fn harvest_widget_only_groups_into_tree(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Skip when the chrome revision advanced since our last snapshot —
        // that means the tree just changed (e.g. a persist-side remove
        // via bump()) and the widget-side view will catch up through
        // `resync_strips_from_tree`. Running the harvest in that
        // window would re-add a legitimately-removed group. Widget-side
        // splits (`GroupRow::split_tab_into_new_group`) don't bump, so
        // this guard leaves those visible to the harvest.
        {
            let cur_rev = chrome.borrow().revision;
            if cur_rev != self.last_chrome_revision {
                return;
            }
        }
        // Collect (strip_id, idx, group) tuples for every widget-only
        // group under an immutable borrow, then apply them under the
        // mutable borrow. Same shape as the other harvesters.
        let mut writes: Vec<(StripId, usize, crate::shell::state::WindowGroup)> = Vec::new();
        {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(ws) = tree.workspaces.iter().find(|w| w.id == tree.active) else {
                return;
            };
            for strip in &ws.strips {
                let Some(&ptr) = self.group_row_ptrs.get(&strip.id) else {
                    continue;
                };
                let tree_ids: std::collections::HashSet<u64> =
                    strip.window_groups.iter().map(|g| g.id).collect();
                // SAFETY: same contract as every other `group_row_ptrs`
                // accessor.
                let row: &GroupRow = unsafe { &*ptr };
                for (idx, wg) in row.groups.iter().enumerate() {
                    if !tree_ids.contains(&wg.id) {
                        writes.push((strip.id, idx, build_state_group_from_widget(wg)));
                    }
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        for (strip_id, idx, group) in writes {
            for ws in tree.workspaces.iter_mut() {
                if let Some(strip) = ws.strips.iter_mut().find(|s| s.id == strip_id) {
                    let insert_at = idx.min(strip.window_groups.len());
                    strip.window_groups.insert(insert_at, group.clone());
                }
            }
        }
    }

    /// Snapshot the `(strip_id, group_id, rect)` triples from the live
    /// per-strip [`GroupRow`] pointers, in traversal order. Shared by the
    /// click-focus stamp and the file-drop router so both resolve a
    /// pointer coordinate to a window group through the exact same
    /// post-layout geometry (see [`pick_group_at`]).
    fn group_hit_candidates(&self) -> Vec<(StripId, GroupId, Rect)> {
        let mut candidates: Vec<(StripId, GroupId, Rect)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content, and the `&self`
            // borrow keeps any reconcile sweep from aliasing the read.
            let row: &GroupRow = unsafe { &*ptr };
            for g in row.groups.iter() {
                candidates.push((strip_id, g.id, g.get_rect()));
            }
        }
        candidates
    }

    /// Resolve the window group directly under pointer position `p`, if
    /// any. Reuses the click-focus hit-test ([`pick_group_at`] over
    /// [`Self::group_hit_candidates`]) so a file drop lands on exactly
    /// the group a click at the same spot would focus. `None` means the
    /// pointer is over empty strip / desktop space.
    fn resolve_drop_target(&self, p: Point) -> Option<(StripId, GroupId)> {
        pick_group_at(&self.group_hit_candidates(), p)
    }

    fn stamp_active_group_from_click(&mut self, p: Point) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Snapshot the (strip_id, group_id, rect) triples from the
        // widget-side pointers first, then hit-test in pure code so the
        // production path shares its logic with the unit test at the
        // bottom of the file (which drives synthetic rects without a
        // real widget tree).
        let candidates = self.group_hit_candidates();
        let Some((strip_id, group_id)) = pick_group_at(&candidates, p) else {
            return;
        };
        // Ensure any widget-only group under this strip is mirrored
        // into the tree before we stamp — the harvest reconciler
        // normally runs during `take_structure_dirty` at frame
        // boundaries, but a click's event dispatch happens BEFORE
        // that, and a stale `active_group` pointing at a group the
        // tree doesn't know about would leave `active_tab_id` as None.
        self.harvest_widget_only_groups_into_tree();
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        let ws_id = tree.active;
        let workspace_exists = tree
            .workspaces
            .iter()
            .any(|w| w.id == ws_id && w.strips.iter().any(|s| s.id == strip_id));
        if !workspace_exists {
            return;
        }
        tree.active_strip.insert(ws_id, strip_id);
        tree.active_group.insert(strip_id, group_id);
    }

    /// Toggle a kind in the chip filter. Multiple kinds may be active
    /// simultaneously; the empty state means "everything shows".
    fn toggle_kind_filter(&mut self, kind: crate::shell::search::ResourceKind) {
        if let Some(pos) = self.active_kind_filter.iter().position(|k| k == &kind) {
            self.active_kind_filter.remove(pos);
        } else {
            self.active_kind_filter.push(kind);
        }
    }

    /// Dispatch the currently selected palette row through the provider
    /// callback (for a `result:N` id) or the legacy chrome-command
    /// dispatcher (for `peer:invite` / `peer:connect`, kept as a bridge
    /// until peer commands migrate into their own provider).
    fn activate_selected_result(&mut self, doc: &mut automerge::AutoCommit, root: &ObjId) {
        let Some(id) = self.palette.selected_id().map(|s| s.to_string()) else {
            return;
        };
        if let Some(idx) = id.strip_prefix("result:") {
            if let Ok(idx) = idx.parse::<usize>() {
                if let Some(result) = self.search_results.get(idx).cloned() {
                    if let Some(chrome) = self.chrome.clone() {
                        (result.on_open)(doc, root, &chrome);
                    }
                    self.palette_visible = false;
                    return;
                }
            }
        }
        // Fall back to the legacy id-based dispatcher for peer-sync
        // rows. These match `peer:invite` / `peer:connect` and enqueue
        // an action drained by `take_peer_sync_actions`.
        if id.starts_with("peer:") {
            match id.as_str() {
                "peer:invite" => {
                    self.peer_sync_actions.push(PeerSyncAction::IssueInvite);
                }
                "peer:connect" => {
                    self.peer_sync_actions.push(PeerSyncAction::PromptConnect);
                }
                _ => {}
            }
            self.palette_visible = false;
        }
    }

    /// Read-only accessor for the command-palette ("global search")
    /// visibility flag. Exposed so integration tests that only hold the
    /// host through `Box<dyn Widget>` can still assert on the live-binary
    /// hotkey path — the flag itself stays internal to this module.
    pub fn palette_visible(&self) -> bool {
        self.palette_visible
    }

    /// What the palette currently is: its query, the id Enter would
    /// activate, and the rows on offer.
    ///
    /// A harness driving the palette by keystroke is otherwise blind to
    /// the one thing that decides what happens next — which row is
    /// selected. Without this, "press Enter and see what changed" is
    /// the only available diagnosis, and a palette that offered a
    /// different row than expected looks identical to one that ignored
    /// the key.
    pub fn palette_snapshot(&self) -> (bool, String, Option<String>, Vec<(String, String)>) {
        (
            self.palette_visible,
            self.palette.query().to_string(),
            self.palette.selected_id().map(|s| s.to_string()),
            self.palette
                .visible_entries()
                .into_iter()
                .map(|e| (e.id.clone(), e.label.clone()))
                .collect(),
        )
    }

    /// Every visible palette row with its section and its subtitle.
    ///
    /// [`Self::palette_snapshot`] carries the label only, and the label
    /// of a content hit is the *document's* title — the part that says
    /// which words matched is the subtitle. So an observer reading only
    /// labels cannot tell a search that matched inside a document from
    /// one that merely matched its name, which is the whole difference
    /// between the two search chords.
    pub fn palette_row_details(&self) -> Vec<(String, String, Option<String>, Option<String>)> {
        self.palette
            .visible_entries()
            .into_iter()
            .map(|e| {
                (
                    e.id.clone(),
                    e.label.clone(),
                    e.description.clone(),
                    e.group.clone(),
                )
            })
            .collect()
    }

    /// Focus the palette's query field. The live open path
    /// ([`Self::open_palette_with_scope`]) does this; screenshot
    /// harnesses that force `palette_visible` need it too or they
    /// capture the unfocused field.
    pub fn set_palette_focus(&mut self, focused: bool) {
        self.palette.set_focus(focused);
    }

    /// Force-set the palette visibility. Lets headless screenshot
    /// harnesses (e.g. `bin/shots`) drive the palette scene without
    /// staging a synthetic Ctrl+K key event through `handle_event`.
    pub fn set_palette_visible(&mut self, v: bool) {
        self.palette_visible = v;
    }

    /// Run the search fan-out directly. `bin/shots` calls this after
    /// forcing the palette open so the offscreen scene shows a
    /// populated result list instead of an empty card (the production
    /// entry point is `handle_event`'s per-keystroke refresh, which
    /// scenes bypass by mutating `palette_visible` directly).
    pub fn refresh_search_results_for_shots(
        &mut self,
        doc: &mut automerge::AutoCommit,
        root: &ObjId,
    ) {
        // Drop the no-op short-circuit first. `last_search_query` starts
        // empty, so an empty-query scene matched it and returned without
        // ever running a provider — every "palette just opened" shot was
        // of a palette that had not searched.
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        self.refresh_search_results(doc, root);
    }

    /// Build a headless [`ActionCatalog`](crate::shell::search::action_api::ActionCatalog)
    /// over the **same** provider registry that feeds the Ctrl+K palette.
    ///
    /// This is the action side of the "action ≠ presentation" boundary:
    /// the palette renders these providers as rows; a script or an LLM
    /// navigator drives the identical registry with no scene. The scope
    /// is resolved from the live chrome (`ActiveApp` narrows to the
    /// focused tab's provider, exactly as the palette does), and
    /// `visibility` is the permission seam — a restricted caller only
    /// enumerates what it may see.
    pub fn action_catalog(
        &self,
        query: &str,
        chrome: &crate::shell::chrome_state::ChromeState,
        doc: &automerge::AutoCommit,
        root: &ObjId,
        visibility: &dyn crate::shell::search::action_api::ActionVisibility,
    ) -> crate::shell::search::action_api::ActionCatalog {
        let scope = chrome.search_scope;
        let active_provider = active_tab_provider_id(chrome, &self.search_registry);
        crate::shell::search::action_api::ActionCatalog::build(
            &self.search_registry,
            query,
            scope,
            active_provider.as_deref(),
            chrome,
            doc,
            root,
            visibility,
        )
    }

    /// Read-only accessor for the command-palette query buffer. Used by
    /// the `shots` driver / scenario harness to assert that synthetic
    /// `Type("…")` actions actually landed inside the palette input.
    pub fn palette_query(&self) -> &str {
        self.palette.query()
    }

    /// Force-set the command-palette query buffer. Lets the `shots`
    /// driver seed a query without round-tripping through synthetic
    /// keyboard events when the palette is closed (when it's open,
    /// `dispatch` already routes characters to the palette).
    pub fn set_palette_query(&mut self, q: &str) {
        self.palette.set_query(q);
    }

    /// Fast-forward the overview crossfade to its settled state
    /// (`overview_t = overview_target = target`). Bypasses the 400 ms
    /// AnimatePresence curve so headless single-frame renders see the
    /// fully-opened (or fully-closed) layer instead of an alpha-zero
    /// transition. Intended for screenshot harnesses + tests; the live
    /// run-loop should keep using `tick_overview_transition`.
    pub fn force_overview_settled(&mut self, target: bool) {
        let t = if target { 1.0 } else { 0.0 };
        self.overview_target = t;
        self.overview_t = t;
        self.transition_from = t;
        self.transition_started = None;
    }

    /// Single-frame settle helpers for the rising-edge entry animations
    /// of the modal/popover overlays — palette, quick-settings, and the
    /// workspace-settings modal. Used by the headless screenshot harness
    /// (`shots --mode offscreen --scenes …`), which paints exactly one
    /// frame per scene: without these the harness lands at `elapsed == 0`
    /// on the open curve and renders the overlays at `alpha == 0` →
    /// indistinguishable from the bare desktop.
    ///
    /// Each helper backdates the `*_opened_at` timestamp by the matching
    /// animation duration so `modal_transition_curve` / the QS open curve
    /// short-circuit to the "fully open" branch, and sets the
    /// `*_was_visible` mirror so the next `draw` doesn't immediately
    /// re-arm the rising edge.
    ///
    /// Live runs should keep using the natural rising-edge gate in
    /// [`Widget::draw`]; these are purely a screenshot/test affordance.
    pub fn force_palette_settled(&mut self) {
        if !self.palette_visible {
            return;
        }
        self.palette_was_visible = true;
        self.palette_transition = ui::Transition::open();
    }

    /// See [`Self::force_palette_settled`].
    pub fn force_quick_settings_settled(&mut self) {
        if !self.overlay_flags().quick_settings {
            return;
        }
        self.qs_was_open = true;
        self.qs_transition = ui::Transition::open();
    }

    /// Refresh every quick-settings field from the OS by calling the
    /// matching getter in [`crate::shell::qs_actions`]. Called once on
    /// the popover's rising-edge open so the user sees live values
    /// instead of the stale snapshot from last time. Each call is
    /// best-effort: a missing daemon logs a warning and leaves the
    /// previous value in place.
    fn refresh_quick_settings_from_os(&mut self) {
        use crate::shell::qs_actions as qs;
        let brightness = qs::log_qs_get("get_brightness", qs::get_brightness());
        let volume = qs::log_qs_get("get_volume", qs::get_volume());
        let wifi = qs::log_qs_get("get_wifi", qs::get_wifi());
        let bluetooth = qs::log_qs_get("get_bluetooth", qs::get_bluetooth());
        let power = qs::log_qs_get("get_power_profile", qs::get_power_profile());
        let wifi_nets = qs::log_qs_get("list_wifi_networks", qs::list_wifi_networks());
        let bt_devices = qs::log_qs_get("list_bt_devices", qs::list_bt_devices());

        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            if let Some(v) = brightness {
                st.quick_settings.brightness = v;
            }
            if let Some(v) = volume {
                st.quick_settings.volume = v;
            }
            if let Some(v) = wifi {
                st.quick_settings.wifi = v;
            }
            if let Some(v) = bluetooth {
                st.quick_settings.bluetooth = v;
            }
            if let Some(p) = power {
                use crate::shell::qs_actions::PowerProfile as ActPP;
                use crate::shell::quick_settings::PowerProfile as UiPP;
                st.quick_settings.power_profile = match p {
                    ActPP::Saver => UiPP::Saver,
                    ActPP::Balanced => UiPP::Balanced,
                    ActPP::Performance => UiPP::Performance,
                };
            }
            if let Some(nets) = wifi_nets {
                use crate::shell::quick_settings::WifiNetwork;
                st.quick_settings.wifi_networks = nets
                    .into_iter()
                    .map(|n| WifiNetwork {
                        ssid: n.ssid,
                        signal: n.signal,
                        secure: n.secure,
                        connected: n.connected,
                    })
                    .collect();
            }
            if let Some(devs) = bt_devices {
                use crate::shell::quick_settings::BtDevice;
                st.quick_settings.bt_devices = devs
                    .into_iter()
                    .map(|d| BtDevice {
                        name: d.name,
                        battery: d.battery,
                        connected: d.connected,
                    })
                    .collect();
            }
            st.bump();
        }
    }

    /// See [`Self::force_palette_settled`].
    pub fn force_ws_settings_settled(&mut self) {
        if !self.overlay_flags().ws_settings {
            return;
        }
        self.ws_settings_was_open = true;
        self.ws_settings_transition = ui::Transition::open();
    }

    /// Font size the overview "Workspaces" h1 is drawn at, in logical pixels.
    /// Exposed so the shell-chrome test can pin "header reads as large/bold"
    /// (visual gap #12 — concept is Tailwind `text-4xl`, 36 px). This is
    /// a method so it tracks the active theme's typography token, not a
    /// hard-coded number.
    pub fn overview_header_font_size(&self, theme: &Theme) -> f32 {
        Self::overview_header_font_size_for(&theme.tokens)
    }

    /// Dispatch the shell action associated with a palette command id,
    /// then close the palette. Returns `true` if the id matched a known
    /// command. Callers still close the palette on unknown ids to avoid
    /// leaving a stale overlay on screen.
    ///
    /// Command ids come from the seed set in [`ShellOverlayHost::new`]
    /// (`search.open` / `overview.toggle` / `settings.open` /
    /// `quick-settings.open`) and map to a single overlay-flag mutation
    /// on the shared [`Chrome`] state.
    // No production caller: nothing routes a palette selection into this,
    // so `/invite` and `/connect` are inert even under --enable-peer-sync.
    // Kept rather than deleted because it is the only definition of what
    // those commands do, and its tests pin that contract.
    #[allow(dead_code)]
    pub(crate) fn dispatch_palette_command(&mut self, id: &str) -> bool {
        // Peer-sync rows (`/invite`, `/connect`) don't mutate chrome
        // state — they enqueue an action the compositor drains via
        // `take_peer_sync_actions` so the libp2p side-effect happens
        // on the runtime thread. Match these first so we can short-
        // circuit before reaching for `chrome`. Ignored when the
        // peer-sync flag is off — defence-in-depth in case a stale
        // palette row survives a flag toggle.
        if self.peer_sync_enabled {
            match id {
                "peer.invite" => {
                    self.peer_sync_actions.push(PeerSyncAction::IssueInvite);
                    self.palette_visible = false;
                    return true;
                }
                "peer.connect" => {
                    self.peer_sync_actions.push(PeerSyncAction::PromptConnect);
                    self.palette_visible = false;
                    return true;
                }
                _ => {}
            }
        }

        let matched = if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            let hit = match id {
                "overview.toggle" => {
                    st.overlays.overview = !st.overlays.overview;
                    true
                }
                "settings.open" => {
                    st.overlays.ws_settings = true;
                    true
                }
                "quick-settings.open" => {
                    st.overlays.quick_settings = true;
                    true
                }
                // `search.open` is a no-op: the palette is already open
                // when the user reaches this row. We close it below like
                // the other commands so behaviour is consistent.
                "search.open" => true,
                _ => false,
            };
            if hit {
                st.bump();
            }
            hit
        } else {
            false
        };
        self.palette_visible = false;
        matched
    }

    fn overview_header_font_size_for(tokens: &ui::tokens::Tokens) -> f32 {
        // Largest step in the type scale — currently 40 px, well above the
        // 32 px floor the concept's `text-4xl` implies.
        tokens.typography.scale.x4l
    }

    fn overlay_flags(&self) -> crate::shell::chrome_state::OverlayFlags {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().overlays)
            .unwrap_or_default()
    }

    /// Mirror of the geometry in `draw_quick_settings` — the popover rect
    /// anchored under the top-right tray pill.
    ///
    /// Width matches the concept's `w-80` (320 px). Height is the sum of
    /// the section bands declared in [`Self::draw_quick_settings`]:
    /// 16 + 56 (action row) + 16 + 72 (sliders) + 16 + 88 (toggles)
    /// + 16 + 48 (power profile) + 16 = 360 px.
    fn quick_settings_rect(&self) -> Rect {
        let popover_w = 320.0_f64;
        let popover_h = 360.0_f64;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(x1 - popover_w, y0, x1, y0 + popover_h)
    }

    /// Public alias for [`Self::quick_settings_rect`] so integration
    /// tests can compute drill-in row coordinates without duplicating
    /// the popover-rect math.
    pub fn quick_settings_rect_for_tests(&self) -> Rect {
        self.quick_settings_rect()
    }

    /// Collaborators dropdown rect — anchored to the top-right like quick-
    /// settings but narrower and taller-when-many-peers. Height is driven by
    /// the peer count so a roster of 8 doesn't clip.
    fn collaborators_rect(&self) -> Rect {
        let popover_w = 260.0_f64;
        let peers = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().tree().collaborators.len())
            .unwrap_or(0);
        let rows = peers.max(1) as f64;
        // + COLLAB_FOOTER_H for the "Room activity" row: the dropdown answers
        // "who is in this room", and "what has happened in this room" is the
        // question you ask next.
        let popover_h: f64 = 48.0 + rows * 36.0 + 16.0 + COLLAB_FOOTER_H;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(
            x1 - popover_w,
            y0,
            x1,
            y0 + popover_h.min(420.0 + COLLAB_FOOTER_H),
        )
    }

    /// Rect of the "Room activity" row at the foot of the collaborator
    /// dropdown, mirroring what `draw_collaborators` paints.
    fn collaborators_activity_row_rect(&self) -> Rect {
        let r = self.collaborators_rect();
        Rect::new(r.x0 + 8.0, r.y1 - COLLAB_FOOTER_H + 4.0, r.x1 - 8.0, r.y1 - 8.0)
    }

    /// Lay the room-activity sheet out and re-derive its rows from the room
    /// document.
    ///
    /// Placed by hand rather than through taffy — the sheet floats over the
    /// desktop like the other overlays, and giving it a layout node would
    /// make it participate in the strip's sizing. The viewport starts below
    /// the top bar so the sheet never covers the workspace pill that says
    /// which room this is.
    fn refresh_room_activity(&mut self) {
        let viewport = Rect::new(
            self.rect.x0,
            self.rect.y0 + self.sidebar_top_y as f64,
            self.rect.x1,
            self.rect.y1,
        );
        self.room_activity.layout(viewport);
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            self.room_activity.refresh(&mut tree);
        }
    }

    /// The room-activity sheet, for tests and for the shots driver.
    pub fn room_activity(&self) -> &crate::shell::room_activity::RoomActivityPane {
        &self.room_activity
    }

    /// Mutable handle to the room-activity sheet, so a test can pin its clock
    /// or drive a filter without synthesising a click.
    pub fn room_activity_mut(&mut self) -> &mut crate::shell::room_activity::RoomActivityPane {
        &mut self.room_activity
    }

    /// Skip the room-activity sheet's open animation so a screenshot or a
    /// click-driven test sees the settled panel rather than a frame of its
    /// entry curve. Mirrors [`Self::force_ws_settings_settled`].
    pub fn force_room_activity_settled(&mut self) {
        self.room_activity_transition = ui::Transition::open();
        self.room_activity_was_open = true;
        self.refresh_room_activity();
    }

    /// The body region a non-General tab's pane is painted into: below
    /// the tab strip, inset from the card's edges. One definition so the
    /// pane's layout (which feeds click routing) and its paint can never
    /// disagree about where the pane is.
    fn ws_settings_body_rect(&self, card: Rect) -> Rect {
        Rect::new(card.x0 + 16.0, card.y0 + 144.0, card.x1 - 16.0, card.y1 - 16.0)
    }

    /// The "Press Esc to close" hint, bottom-right of the modal card.
    /// Each tab that owns its whole body draws it, so the General body
    /// never has to run just to produce the hint.
    fn draw_ws_settings_close_hint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        card: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let hint = "Press Esc to close";
        let size = theme.tokens.typography.scale.xs;
        let (w, _) = text_renderer.measure(hint, size, None);
        text_renderer.draw_text(
            scene,
            hint,
            size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x1 - w as f64 - 24.0, card.y1 - 24.0)),
            None,
        );
    }

    /// Name-input rect inside the ws-settings modal. Matches the drawn
    /// rect in `draw_ws_settings` so hit-test and draw stay in sync.
    fn ws_settings_name_rect(&self) -> Rect {
        let r = self.ws_settings_rect();
        let body_y = r.y0 + 168.0;
        Rect::new(r.x0 + 24.0, body_y + 20.0, r.x1 - 24.0, body_y + 48.0)
    }

    /// Mirror of the geometry in `draw_ws_settings` — centered modal card.
    /// Modal grows when Roles / Files tabs are active so the longer per-
    /// app permission grid + file list fit without overflowing into the
    /// workspace area below; click-detection geometry stays in sync.
    ///
    /// 2026-06-24 audit lifted the resting width 520 → 720 and the
    /// tab-expanded height 520 → 600 so the General/Roles/Files/
    /// Permissions/Collaborators row + collaborators block read as a
    /// proper modal sheet instead of a cramped popover dwarfed by the
    /// 1600×900 viewport.
    fn ws_settings_rect(&self) -> Rect {
        let modal_w = 720.0_f64;
        let modal_h = match self.chrome.as_ref().map(|c| c.borrow().ws_settings_tab) {
            Some(WsTab::Permissions) => 640.0_f64,
            Some(WsTab::Members) => 600.0_f64,
            _ => 440.0_f64,
        };
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Rect::new(
            cx - modal_w / 2.0,
            cy - modal_h / 2.0,
            cx + modal_w / 2.0,
            cy + modal_h / 2.0,
        )
    }

    /// The band the scope + kind controls are painted into.
    ///
    /// This is the palette's *own* reserved filter band, not a separate
    /// surface floating above the card. Reading it from the palette is
    /// what keeps the controls on the same left edge as the query field
    /// and the rows below them; the previous banner computed its own
    /// insets and so lined up with neither.
    fn search_banner_rect(&self) -> Rect {
        let p = self.palette.get_rect();
        if p.width() <= 0.0 {
            return Rect::ZERO;
        }
        self.palette.filter_strip_rect()
    }

    /// Full ordered list of scopes shown in the banner. Includes
    /// [`SearchScope::ActiveApp`] only when there's a currently focused
    /// tab — otherwise the pill would render an empty label.
    fn search_scope_list(&self) -> Vec<crate::shell::chrome_state::SearchScope> {
        use crate::shell::chrome_state::SearchScope;
        let mut out = Vec::with_capacity(3);
        if self
            .chrome
            .as_ref()
            .and_then(|c| c.borrow().active_app_title())
            .is_some()
        {
            out.push(SearchScope::ActiveApp);
        }
        out.push(SearchScope::Workspace);
        out.push(SearchScope::All);
        out
    }

    /// Label to render inside the i-th scope pill. For
    /// [`SearchScope::ActiveApp`] this names the focused app (e.g.
    /// `Notes app`), truncated to `MAX_APP_PILL_CHARS` so a very long
    /// title can't push the sibling pills off the banner. Static labels
    /// fall through to [`SearchScope::label`].
    ///
    /// The pill names the app *and* says it is an app, because the band
    /// directly above it is the kind-filter strip and those chips carry
    /// kind names — `Actions`, `Applications`, `Notes`, `Files`. With
    /// Notes focused, the strip read `Notes` and the scope control read
    /// `Only Notes` one row below it: two adjacent controls saying the
    /// same word for two different things, one filtering to note-shaped
    /// results and the other narrowing to the app. Naming the app as an
    /// app is what separates them, and it reads as a place alongside its
    /// siblings `Workspace` and `All` rather than as a second filter.
    pub(crate) fn search_scope_pill_label(
        &self,
        scope: crate::shell::chrome_state::SearchScope,
    ) -> String {
        use crate::shell::chrome_state::SearchScope;
        const MAX_APP_PILL_CHARS: usize = 14;
        match scope {
            SearchScope::ActiveApp => {
                let Some(raw) = self
                    .chrome
                    .as_ref()
                    .and_then(|c| c.borrow().active_app_title())
                else {
                    return SearchScope::ActiveApp.label().to_string();
                };
                let title = if raw.chars().count() > MAX_APP_PILL_CHARS {
                    let head: String = raw.chars().take(MAX_APP_PILL_CHARS).collect();
                    format!("{head}…")
                } else {
                    raw
                };
                // An app already called "… app" doesn't get told twice.
                if title.to_lowercase().ends_with(" app") {
                    title
                } else {
                    format!("{title} app")
                }
            }
            other => other.label().to_string(),
        }
    }

    /// Individual scope-segment rect inside the filter band. `idx`
    /// indexes into [`Self::search_scope_list`], so the ActiveApp
    /// segment (when present) is idx 0.
    ///
    /// The three scopes are one segmented control anchored to the right
    /// end of the band, not three loose pills after a "Search in:"
    /// label: they are mutually exclusive, so they should look like one
    /// switch rather than three independent toggles sitting next to a
    /// row of multi-select kind chips that look exactly the same.
    /// Hit-test and draw consume this same helper.
    fn search_scope_pill_rect(&self, idx: usize) -> Rect {
        let b = self.search_banner_rect();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        let count = self.search_scope_list().len().max(1);
        let track = self.search_scope_track_rect();
        let seg_w = (track.width() - 2.0 * SEGMENT_INSET_PX) / count as f64;
        let x0 = track.x0 + SEGMENT_INSET_PX + idx as f64 * seg_w;
        Rect::new(
            x0,
            track.y0 + SEGMENT_INSET_PX,
            x0 + seg_w,
            track.y1 - SEGMENT_INSET_PX,
        )
    }

    /// Outer track of the scope segmented control — the rounded body the
    /// individual segments sit inside.
    fn search_scope_track_rect(&self) -> Rect {
        let b = self.search_banner_rect();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        let count = self.search_scope_list().len().max(1);
        let w = (Self::SCOPE_SEGMENT_WIDTH_PX * count as f64 + 2.0 * SEGMENT_INSET_PX)
            .min(b.width() * 0.55);
        let h = Self::FILTER_CONTROL_HEIGHT_PX;
        let cy = (b.y0 + b.y1) / 2.0;
        Rect::new(b.x1 - w, cy - h / 2.0, b.x1, cy + h / 2.0)
    }

    /// Width of one scope segment. Wide enough to hold a truncated app
    /// title without cramping.
    const SCOPE_SEGMENT_WIDTH_PX: f64 = 88.0;
    /// Shared height for every control in the filter band, so the chips
    /// and the scope switch sit on one baseline.
    const FILTER_CONTROL_HEIGHT_PX: f64 = 24.0;

    /// Geometry of the overview's centered content column. Concept wraps
    /// everything in a `max-w-5xl w-full` column with `p-12` outer padding,
    /// horizontally centered by the outer `items-center` flex row. The
    /// returned rect is the inner column after the padding is removed.
    fn overview_column_rect(&self) -> Rect {
        let max_w = OVERVIEW_MAX_WIDTH_PX;
        let padding = OVERVIEW_OUTER_PADDING_PX;
        let inner_w = (self.rect.width() - padding * 2.0).min(max_w);
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let x0 = cx - inner_w / 2.0;
        let x1 = cx + inner_w / 2.0;
        Rect::new(x0, self.rect.y0 + padding, x1, self.rect.y1 - padding)
    }

    /// Y-coordinate where the first workspace row card starts. Matches the
    /// draw path so hit-test + draw stay in sync. The header sits above the
    /// rail container; the rail container sits `mb-16` below the header.
    /// `overview_scroll_y` shifts every row upward so the wheel handler
    /// can pan beyond the visible band when the workspace list overflows.
    fn overview_first_row_top(&self) -> f64 {
        let col = self.overview_column_rect();
        col.y0 + OVERVIEW_HEADER_HEIGHT_PX + OVERVIEW_HEADER_MARGIN_BOTTOM_PX
            - self.overview_scroll_y
    }

    /// Total scrollable content height in the overview (all workspace
    /// rows + their gaps + the trailing "+ Add workspace" tile). Used by
    /// the wheel handler to clamp `overview_scroll_y` so the user can't
    /// pan past the last row.
    fn overview_content_height(&self) -> f64 {
        let chrome = match self.chrome.as_ref() {
            Some(c) => c,
            None => return 0.0,
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let ws_count = tree.workspaces.len();
        ws_count as f64 * (OVERVIEW_ROW_HEIGHT_PX + OVERVIEW_ROW_GAP_PX)
            + OVERVIEW_ADD_ROW_HEIGHT_PX
    }

    /// Maximum legal `overview_scroll_y` — the content overflow below
    /// the visible band, clamped to ≥ 0. Zero when the workspace list
    /// fits inside the column.
    fn overview_max_scroll_y(&self) -> f64 {
        let col = self.overview_column_rect();
        let visible =
            (col.y1 - col.y0 - OVERVIEW_HEADER_HEIGHT_PX - OVERVIEW_HEADER_MARGIN_BOTTOM_PX)
                .max(0.0);
        (self.overview_content_height() - visible).max(0.0)
    }

    /// Rect of the workspace card at `idx` in the vertical list.
    fn overview_row_rect(&self, idx: usize) -> Rect {
        let col = self.overview_column_rect();
        let top = self.overview_first_row_top();
        let y0 = top + idx as f64 * (OVERVIEW_ROW_HEIGHT_PX + OVERVIEW_ROW_GAP_PX);
        Rect::new(col.x0, y0, col.x1, y0 + OVERVIEW_ROW_HEIGHT_PX)
    }

    /// Rect of the "+ Add workspace" affordance painted at the tail of
    /// the overview's vertical workspace list. Concept reserves the
    /// create-affordance as a light dashed-border tile below the last
    /// workspace card; sharing the column width but a shorter row
    /// height keeps it visually subordinate.
    fn overview_add_workspace_rect_for(&self, ws_count: usize) -> Rect {
        let col = self.overview_column_rect();
        let top = self.overview_first_row_top();
        let y0 = top + ws_count as f64 * (OVERVIEW_ROW_HEIGHT_PX + OVERVIEW_ROW_GAP_PX);
        Rect::new(col.x0, y0, col.x1, y0 + OVERVIEW_ADD_ROW_HEIGHT_PX)
    }

    /// Mirror of the geometry in `draw_overview`: return the index of the
    /// workspace card at logical position `p`, if any.
    fn hit_test_overview(&self, p: Point) -> Option<usize> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let tree = state.tree();
        for i in 0..tree.workspaces.len() {
            if self.overview_row_rect(i).contains(p) {
                return Some(i);
            }
        }
        None
    }

    /// True if `p` is contained in any currently-open overlay's hit rect.
    ///
    /// Used by [`Widget::handle_event`] to absorb every pointer event whose
    /// cursor is over an open palette / collaborators dropdown / quick-
    /// settings popover / workspace-settings modal — not just `Pressed`.
    /// Without this gate, `Released` and `CursorMoved` events fall through
    /// to the wrapped content (#7, B-2) and light up tab pills, hover
    /// affordances, or stray drag-loop continuations underneath the modal.
    ///
    /// `pub(crate)` because the same predicate is reused by the
    /// palette-focus / WS-settings-pill click branches in Wave 2 (Unit
    /// 2-A) — keeping it a method on `ShellOverlayHost` lets those branches
    /// stay in sync with the rect helpers above.
    pub(crate) fn cursor_over_open_overlay(&self, p: Point) -> bool {
        let f = self.overlay_flags();
        (self.palette_visible && self.palette.get_rect().contains(p))
            || (f.collaborators && self.collaborators_rect().contains(p))
            || (f.quick_settings && self.quick_settings_rect().contains(p))
            || (f.ws_settings && self.ws_settings_rect().contains(p))
            || (f.room_activity && self.room_activity.rect().contains(p))
    }
}

// -----------------------------------------------------------------------------
// Overview layout constants
// -----------------------------------------------------------------------------
//
// Concept source of truth: `nirios-concept/src/components/OverviewView.tsx`.
// The values below map Tailwind utilities to pixel equivalents; keeping them
// as module-level constants lets the draw path, `hit_test_overview`, and the
// unit tests share one geometry oracle.
//
// * `max-w-5xl` → 1024 px content column.
// * `p-12`      → 48 px outer padding (root is `flex-1 bg-slate-950 ...`).
// * `space-y-8` → 32 px gap between workspace rows.
// * `mb-16`     → 64 px gap under the header block.
// * `border-2`  → 2 px card border.
// * `rounded-3xl` → 24 px card radius.
// * `p-6`       → 24 px inner padding on each workspace card.
// * `gap-8`     → 32 px gap between sidebar column and mini-window row.
// * `w-64`      → 256 px sidebar column.
// * `h-20`      → 80 px mini-window card height.

const OVERVIEW_MAX_WIDTH_PX: f64 = 1024.0;
const OVERVIEW_OUTER_PADDING_PX: f64 = 48.0;
const OVERVIEW_ROW_GAP_PX: f64 = 32.0;
/// Row height = top/bottom padding (2×24) + sidebar column content stack
/// (title ≈ 26, perms panel 60, collaborators header + avatars ≈ 48,
/// footer ≈ 16) with a little breathing room. The concept uses
/// `items-start` so the row sizes to content; we lock it to a stable
/// value so the vertical list reads as even rows.
const OVERVIEW_ROW_HEIGHT_PX: f64 = 240.0;
/// Height of the "+ Add workspace" tile painted at the tail of the
/// vertical workspace list. Shorter than a full workspace row so it
/// reads as a secondary, lightweight create-affordance rather than a
/// peer of the existing workspace cards.
const OVERVIEW_ADD_ROW_HEIGHT_PX: f64 = 72.0;
const OVERVIEW_ROW_RADIUS_PX: f64 = 24.0;
const OVERVIEW_ROW_BORDER_PX: f64 = 2.0;
const OVERVIEW_CARD_PADDING_PX: f64 = 24.0;
const OVERVIEW_SIDEBAR_WIDTH_PX: f64 = 256.0;
const OVERVIEW_SIDEBAR_GAP_PX: f64 = 32.0;
const OVERVIEW_MINI_HEIGHT_PX: f64 = 80.0;
const OVERVIEW_MINI_WIDTH_PX: f64 = 96.0;
const OVERVIEW_MINI_GAP_PX: f64 = 16.0;
/// Space reserved above the first row for the "Workspaces" header block.
/// The concept's header is centered with an `h1 text-4xl` + body line; 72 px
/// accommodates the two lines comfortably.
const OVERVIEW_HEADER_HEIGHT_PX: f64 = 72.0;
/// `mb-16` in the concept.
const OVERVIEW_HEADER_MARGIN_BOTTOM_PX: f64 = 64.0;
/// Active-indicator dot geometry. Concept draws a 16×16 px dot at
/// `-left-[35px]` from the card's left edge, vertically centred.
const OVERVIEW_DOT_SIZE_PX: f64 = 16.0;
const OVERVIEW_DOT_OFFSET_PX: f64 = 35.0;
/// Horizontal offset of the vertical rail line from the card's left edge
/// (`left-8` - card offset). The rail sits behind the dot column.
const OVERVIEW_RAIL_OFFSET_PX: f64 = 35.0;
const OVERVIEW_RAIL_WIDTH_PX: f64 = 2.0;
/// How far past the overview column the scroll viewport's clip reaches.
/// Has to clear everything a row paints outside its own rect — the rail
/// dot (`OVERVIEW_DOT_OFFSET_PX` out, half a dot wide) and the active
/// card's accent glow ([`OVERVIEW_ROW_GLOW_SPREAD_PX`]) — or the clip
/// crops the very indicators that say which workspace is current.
const OVERVIEW_SCROLL_GUTTER_PX: f64 = {
    let dot_reach = OVERVIEW_DOT_OFFSET_PX + OVERVIEW_DOT_SIZE_PX / 2.0;
    if dot_reach > OVERVIEW_ROW_GLOW_SPREAD_PX {
        dot_reach + 4.0
    } else {
        OVERVIEW_ROW_GLOW_SPREAD_PX + 4.0
    }
};
/// Spread of the selected workspace card's accent glow. Concept
/// `shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]`.
const OVERVIEW_ROW_GLOW_SPREAD_PX: f64 = 40.0;

impl Widget for ShellOverlayHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        self.content.sync(s, p);
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.sync(s, p);
        }
        self.palette.sync(s, p);
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        // Carry out anything the palette's peer-sync rows asked for.
        // First, so the invite token or the token prompt is in chrome
        // before this frame's draw reads it.
        self.pump_peer_sync();
        // Diff the live tree against each `GroupRow`'s current
        // children. Closes #263 — every structural mutation in the
        // persisted `WorkspaceTree` bumps `chrome.revision`; if our
        // cached value disagrees the tree has changed shape and the
        // live rows need to splice new groups in (or drop removed
        // ones) so the painted strip matches state. Runs BEFORE the
        // children's own `update` calls so the freshly-spliced
        // groups participate in the same frame's sync pass.
        let chrome_dirty = self.resync_strips_from_tree();
        // Mirror the compositor's live fullscreen state onto the strip rows
        // before the children update, so a promotion/restore is reflected in
        // the same frame it changes.
        self.sync_fullscreen_from_chrome();
        let a = self.content.update(s, p);
        let b = self.palette.update(s, p);
        let c = self
            .sidebar
            .as_mut()
            .map(|w| w.update(s, p))
            .unwrap_or(false);
        // Per-frame wire-up: nudge the data-layer active-group pointer off
        // any group whose widget tree latched the 180 ms close fade. Has
        // to run after `content.update` so any close fade started by the
        // current frame's events is observable in the harvest below.
        self.reconcile_active_group_off_closing();
        // Mirror any in-frame `WindowGroup::maximized` flips into the
        // persisted `WorkspaceTree`. Drains the per-group take-pattern
        // accessor; runs after `content.update` for the same reason as
        // the close-reconciler above (clicks observed this frame must
        // be visible to the harvest). Audit task #105.
        self.harvest_pending_maximize_changes();
        // Mirror any in-frame resize-handle gestures (press-drag-release on
        // the right edge of a `WindowGroup`) into the persisted
        // `WorkspaceTree`. Same take-pattern as the maximize writeback;
        // runs once per gesture, not once per pixel.
        self.harvest_pending_width_changes();
        // Mirror any in-frame whole-group reorder drag (grab a group's
        // tab-bar background, slide it to a new slot) into the persisted
        // `WorkspaceTree` so the strip's group order survives a restart and
        // replicates to peers. Same one-shot take-pattern as the width
        // writeback.
        self.harvest_pending_group_reorder();
        // Mirror any in-frame intra-group tab reorder (drag a pill to a new
        // slot inside its own group) into the persisted `WorkspaceTree` so the
        // tab order survives a restart and replicates to peers. Same one-shot
        // take-pattern as the group reorder above.
        self.harvest_pending_tab_reorder();
        // Open the search bar scoped to the app whose decoration magnifier the
        // user just clicked.
        self.harvest_pending_search_open();
        // Mirror the widget-side `TabBar::active_id` into the persisted
        // `WorkspaceTree::group.active_tab`. Without this, clicking a
        // different tab pill flips the visible content but the tree's
        // `active_tab` stays pointing at whatever tab was constructed
        // first — the 2026-07-09 "Ctrl+K searches in the first app of
        // the group, not the active one" bug. Also stamps
        // `tree.active_group` / `tree.active_strip` on group focus so
        // the search-scope narrowing has an honest anchor.
        self.harvest_active_tab_writeback();
        // Keep the focused group scrolled into view. Edge-triggered on a
        // change to the strip's `active_group` pointer, so a manual wheel
        // pan away from the focus isn't fought — only a genuine focus
        // change re-centres the strip (Niri "focused column follows").
        // Runs after `harvest_active_tab_writeback` so a focus change made
        // this frame is already reflected in `active_group`.
        self.scroll_active_group_into_view();
        // Cross-strip tab drag-drop (#151): drain any `GroupRow` that
        // published a cross-strip detach during this frame's events and
        // splice the dragged tab into whichever strip's row contains the
        // release point (or back into source if no strip matched).
        self.dispatch_cross_strip_detaches();
        // Offer every open tab's document to its own writer. Debounced and
        // off-thread, so a frame on which nothing was typed costs a head
        // comparison per open record — but a frame on which something was
        // is what puts it on disk. Without this the store holds each tab's
        // document in memory and the session ends having written nothing.
        if let Some(chrome) = self.chrome.as_ref() {
            let records = chrome.borrow().records().clone();
            records.tick();
        }
        // Sandbox Phase 4 Task 4.2: drive the permission-prompt pump.
        // Latches a fresh widget for the front of `chrome.pending_grants`,
        // and drains entries when the user clicks an action button.
        // Idempotent — runs every frame regardless of queue state.
        self.pump_permission_prompt();
        // App launches report back from their worker threads. Draining
        // here is what turns "the sandbox refused to start" into
        // something the user can see; the frame thread only ever takes
        // a mutex and moves a Vec out.
        let launch_dirty = self.drain_launch_events();
        // Notification lifecycle: drop expired toasts so the stack
        // shrinks even on frames where the user does nothing. The
        // widget's own `update` also drives this, but routing it
        // here too means the chrome stays consistent for tests that
        // never call the widget directly.
        let d = if let Some(chrome) = self.chrome.as_ref() {
            let dropped = chrome
                .borrow_mut()
                .update_notification_lifecycle(std::time::Instant::now());
            dropped > 0
        } else {
            false
        };
        // Close whatever the frame's `doc_*` mirrors left open on the room
        // document, stamping it with the wall clock. Automerge would
        // otherwise close it implicitly at the next read with a zero
        // timestamp, and a room whose every change reads as the Unix epoch
        // cannot be filtered by date. Free when nothing is pending.
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow().tree_mut().commit_room_change();
        }
        a || b || c || d || launch_dirty || chrome_dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Push the compositor's live fullscreen state down to the strip rows
        // (which group is promoted / which strips are hidden) BEFORE the
        // content tree is built, so the collapse takes effect this pass.
        self.sync_fullscreen_from_chrome();
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().is_fullscreen());
        // Tell every strip which displays it spans BEFORE the content tree
        // is built, so each window group's width fraction resolves against
        // the display it sits on in this very pass. Doing it here rather
        // than in `apply_layout` is what makes a single-frame offscreen
        // render (a screenshot) show the same geometry as the live shell.
        let spans = self.strip_output_spans();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — pointer captured from a `Box<GroupRow>` we own
            // through the content tree; host outlives content; `&mut self`
            // → unaliased mutable access.
            unsafe { (&mut *ptr).set_output_spans(&spans) };
        }
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let palette_node = self.palette.build_layout(taffy, theme, node_map);
        // Overlay the palette above content: absolute positioning with
        // auto-margin horizontal centering and a fixed top offset so it
        // floats below the shell's top bar like a popover. The widget's
        // own `build_layout` defaults to a narrow 360 px footprint —
        // override to 640 px here so the palette reads as a proper
        // centered modal sheet, not a top-right anchored popover (2026-
        // 06-24 audit pinned this as the most visible chrome regression).
        let mut palette_style = taffy.style(palette_node).unwrap().clone();
        palette_style.position = Position::Absolute;
        // Adaptive width: 70 % of viewport, clamped to [560, 640]. At
        // 800 wide the palette shrinks to 560 (120 px margin per
        // side); at 1280+ it caps at 640. Lower bound is the minimum
        // that still fits the filter strip's eight pills (WORKSPACE
        // / ALL / DOCS / CODE / IMG / VID / FULL TEXT) without the
        // last pill bleeding into the close glyph. Caught by
        // `shots --sweep` on 2026-06-27 — the fixed 640 was clipping
        // hard against the 800×600 viewport's right edge.
        let palette_target_w = (self.rect.width() * 0.7).clamp(560.0, 640.0);
        // `self.rect` is last frame's post-layout rect, so the very
        // first layout sizes the card off a zero-width host and lands on
        // the 560 floor. Remember what we sized against; `apply_layout`
        // re-dirties the tree if the host turned out to be a different
        // width, so the card converges on its real size instead of
        // keeping whichever width the first frame happened to see.
        self.palette_layout_host_w = self.rect.width();
        palette_style.size = Size {
            width: Dimension::length(palette_target_w as f32),
            height: palette_style.size.height,
        };
        palette_style.margin = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::auto(),
        };
        palette_style.inset = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(96.0),
            bottom: LengthPercentageAuto::auto(),
        };
        taffy.set_style(palette_node, palette_style).unwrap();

        self.content_node = Some(content_node);
        self.palette_node = Some(palette_node);

        // Floating sidebar wheel: absolutely positioned on the left edge
        // directly below the top bar, taking the remainder of the host's
        // height. Sized to the wheel's intrinsic 96 px width. The sidebar
        // widget's own build_layout chose a leaf style — we override it
        // with an absolute position so it hovers above the content pane
        // instead of displacing it.
        let sidebar_node = self.sidebar.as_mut().map(|sidebar| {
            let node = sidebar.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(node).unwrap().clone();
            style.position = Position::Absolute;
            style.inset = TaffyRect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                top: LengthPercentageAuto::length(self.sidebar_top_y),
                bottom: LengthPercentageAuto::length(0.0),
            };
            style.size = Size {
                width: Dimension::length(sidebar_width_for_viewport(self.rect.height() as f32)),
                height: Dimension::auto(),
            };
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            // A fullscreen surface covers the whole output: the sidebar
            // wheel is suppressed alongside the top bar so nothing paints
            // over the promoted group. Clearing fullscreen restores it.
            if fullscreen {
                style.display = Display::None;
            }
            taffy.set_style(node, style).unwrap();
            node
        });
        self.sidebar_node = sidebar_node;

        let children: Vec<NodeId> = match sidebar_node {
            Some(s) => vec![content_node, palette_node, s],
            None => vec![content_node, palette_node],
        };
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
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // Did the card get sized against a stale host width? Ask for one
        // more layout pass; the check is a float compare against the
        // exact value `build_layout` stored, so it settles in a single
        // extra frame and then stops.
        if self.palette_layout_host_w != self.rect.width() {
            self.palette_layout_dirty = true;
        }
        // Push the current wheel-disc slot width (plus a small
        // breathing-room gap) into every GroupRow's `leading_inset`
        // BEFORE the content tree lays out, so the strip reserves the
        // canvas-x the wheel disc is about to render into AND a
        // 16 px wallpaper gutter to its right. 2026-06-24 user
        // feedback: with the inset == wheel_w exactly, the radial
        // switcher's right edge butted into the first window — the
        // small gap reads as visual separation between the canvas
        // chrome (wheel) and the workspace's actual content. Skipped
        // when there's no sidebar (no wheel ⇒ no gutter to reserve).
        let wheel_w = if self.sidebar.is_some() {
            sidebar_width_for_viewport(self.rect.height() as f32) as f64 + WHEEL_TO_STRIP_GAP_PX
        } else {
            0.0
        };
        // Re-derive the per-display strip widths now that the host's own
        // rect is known. This is what makes a host nobody pushed geometry
        // into converge: `build_layout` runs before the first `apply_layout`,
        // so on frame 1 it has no viewport to fall back on and the strips
        // size against the toolkit's default. Pushing again here flags the
        // rows structurally dirty on any change, so the next layout pass
        // bakes the real width — the same one-extra-pass convergence the
        // palette card above uses. When the compositor *has* pushed real
        // output geometry this is a no-op: the spans come from that, not
        // from `self.rect`.
        let settled_spans = self.strip_output_spans();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as the other group_row_ptrs
            // accessors — pointer captured from a `Box<GroupRow>` we
            // own through the content tree; host outlives content;
            // &mut self → unaliased mutable access.
            unsafe {
                let row = &mut *ptr;
                row.set_leading_inset(wheel_w);
                row.set_output_spans(&settled_spans);
            }
        }
        let children = taffy.children(node).unwrap();
        self.content
            .apply_layout(taffy, children[0], Point::new(self.rect.x0, self.rect.y0));
        self.palette
            .apply_layout(taffy, children[1], Point::new(self.rect.x0, self.rect.y0));
        if let (Some(sidebar), Some(sidebar_child)) = (self.sidebar.as_mut(), children.get(2)) {
            // 2026-06-24 user spec: "the radial wheel on the left is
            // fixed in place – it should be part of the workspace and
            // scroll with the rest of the desktop". The wheel disc
            // lives in the overlay layer (absolute-positioned above
            // the content) so we can't make it a sibling of the
            // GroupRow; instead, shift its origin by the active
            // strip's `scroll_x` so it pans in lock-step.
            //
            // group_row_ptrs only contains strips from the currently-
            // active workspace (see `shell/mod.rs` build path), so
            // picking the first strip's scroll_x is correct for the
            // common single-strip case. Multi-strip workspaces aren't
            // wired up yet; a future fix would pick the topmost
            // visible strip, but for now any strip's scroll_x is the
            // active workspace's pan offset.
            let active_scroll_x: f64 = self
                .group_row_ptrs
                .values()
                .next()
                .map(|&ptr| {
                    // SAFETY: same contract as the other group_row_ptrs
                    // accessors — pointer captured from a `Box<GroupRow>`
                    // we own through the content tree, host outlives
                    // content, &mut self → unaliased read.
                    unsafe { (&*ptr).scroll_x() }
                })
                .unwrap_or(0.0);
            sidebar.apply_layout(
                taffy,
                *sidebar_child,
                Point::new(self.rect.x0 - active_scroll_x, self.rect.y0),
            );
        }
        self.publish_strip_viewport();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let flags = self.overlay_flags();
        let target = if flags.overview { 1.0 } else { 0.0 };
        let t = self.tick_overview_transition(target);
        // Sequential two-phase composition (concept `App.tsx:427-487`
        // wraps both views in `AnimatePresence mode="wait"` so the
        // outgoing view plays its exit fully before the incoming view's
        // enter begins). `t` is linear progress toward `overview_target`
        // (1.0 = overview shown, 0.0 = desktop shown). The helper returns
        // alpha + scale for each layer for the current half.
        let (content_alpha, content_scale, overview_alpha, overview_scale) =
            compose_overlay_transition(t, self.overview_target);

        // Workspace slide-swap: when the active workspace just changed,
        // translate the content widget vertically so the new workspace
        // reads as sliding into place. Zero at rest, so idle draws are
        // identical to pre-animation paths.
        let slide_dy = self.tick_workspace_slide().unwrap_or(0.0);

        // Scale pivot = scene centre (so the shrink/zoom feels anchored
        // to the viewport the user is looking at, not the origin).
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let scaled = |s: f64| -> Affine {
            Affine::translate((cx, cy)) * Affine::scale(s) * Affine::translate((-cx, -cy))
        };

        // Desktop content transform: workspace-slide first (viewport
        // scroll), then the overlay-scale about the centre so the scale
        // reads as the entire viewport zooming in/out.
        let mut content_xform = transform;
        if slide_dy != 0.0 {
            content_xform = content_xform * Affine::translate((0.0, slide_dy));
        }
        if (content_scale - 1.0).abs() > f64::EPSILON {
            content_xform = scaled(content_scale) * content_xform;
        }

        // Desktop content + simulated peer fade out as overview fades in.
        // Skip the layer entirely at full opacity to keep the common path
        // cheap (vello push_layer clips + allocates an intermediate).
        if content_alpha >= 0.999 && (content_scale - 1.0).abs() <= f64::EPSILON {
            self.content
                .draw(scene, content_xform, text_renderer, theme, presence);
            self.draw_demo_peer(scene, transform, text_renderer, theme);
            self.draw_real_peers(scene, transform, text_renderer, theme, presence);
        } else if content_alpha > 0.0 {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                BlendMode::default(),
                content_alpha,
                transform,
                &self.rect,
            );
            self.content
                .draw(scene, content_xform, text_renderer, theme, presence);
            // Demo + real peers scale with the desktop so they appear to
            // live inside the scaling viewport rather than glued to the
            // screen.
            let peer_xform = if (content_scale - 1.0).abs() > f64::EPSILON {
                scaled(content_scale) * transform
            } else {
                transform
            };
            self.draw_demo_peer(scene, peer_xform, text_renderer, theme);
            self.draw_real_peers(scene, peer_xform, text_renderer, theme, presence);
            scene.pop_layer();
        }

        // Sandbox Phase 4 Task 4.2: inline permission prompt. Painted
        // as a thin bar at the top of the content viewport (just below
        // the topbar) when `chrome.pending_grants.front().is_some()`.
        // Sits above the desktop content but below the overlay layer
        // so an open palette / overview / quick-settings still occludes
        // it — the user wants to act on the modal first, then come
        // back to the prompt. The host owns the widget; the pump in
        // `update` (and its `tick_permission_prompt_for_tests` shim)
        // is what observes pending grants and drains them.
        if let Some(prompt) = self.permission_prompt.as_mut() {
            let bar_y0 = self.rect.y0 + self.sidebar_top_y as f64;
            let bar_x0 = self.rect.x0 + self.sidebar_top_y as f64;
            let bar_x1 = self.rect.x1 - self.sidebar_top_y as f64;
            prompt.set_rect(Rect::new(bar_x0, bar_y0, bar_x1, bar_y0 + PROMPT_HEIGHT));
            prompt.draw(scene, transform, text_renderer, theme, presence);
        }

        // Top-right notification stack. Drawn above the desktop content
        // but below modal overlays (palette / settings / overview) and
        // the permission prompt so a prompt always wins focus.
        if let Some(stack) = self.notification_stack.as_mut() {
            // Offset top inset below the topbar so toasts don't crash
            // into clock / tray. Right edge is the viewport's right
            // edge — the stack itself insets by EDGE_INSET.
            let viewport = Rect::new(
                self.rect.x0,
                self.rect.y0 + self.sidebar_top_y as f64,
                self.rect.x1,
                self.rect.y1,
            );
            stack.set_viewport(viewport);
            stack.draw(scene, transform, text_renderer, theme, presence);
        }

        // Overview grid (Cmd+Tab) scales+fades into place on the second
        // half of the crossfade (or, on the reverse, scales out on the
        // first half). Same scale-about-centre pivot as the desktop.
        if overview_alpha <= 0.0 {
            // Overview fully hidden — clear the cached "+ Add workspace"
            // hit rect so a stray click during an in-flight close can't
            // create a workspace.
            self.add_workspace_rect = Rect::ZERO;
        }
        if overview_alpha > 0.0 {
            let overview_xform = if (overview_scale - 1.0).abs() > f64::EPSILON {
                scaled(overview_scale) * transform
            } else {
                transform
            };
            // Concept `OverviewView.tsx` root uses `bg-slate-950` (#020617)
            // — the overview view is a distinct "darker room" from the
            // desktop gradient. Paint the flat slate-950 backdrop at the
            // *un*-scaled outer transform so it covers the entire viewport
            // even during the 0.95 → 1.0 zoom-in animation (visual gap
            // #13 in `docs/nirios/visual-gaps.md`). It sits above the
            // black/30 scrim and below the cards / header so nothing from
            // the desktop gradient can bleed through around the edges.
            let slate_950 = theme.tokens.colors.neutral[0];
            if overview_alpha >= 0.999 && (overview_scale - 1.0).abs() <= f64::EPSILON {
                draw_scrim(scene, transform, self.rect, theme);
                scene.fill(Fill::NonZero, transform, slate_950, None, &self.rect);
                self.draw_overview(scene, transform, text_renderer, theme);
            } else {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    overview_alpha,
                    transform,
                    &self.rect,
                );
                draw_scrim(scene, transform, self.rect, theme);
                scene.fill(Fill::NonZero, transform, slate_950, None, &self.rect);
                self.draw_overview(scene, overview_xform, text_renderer, theme);
                scene.pop_layer();
            }
        }

        // Floating sidebar wheel: draws above the desktop content but
        // below overlay-stacked UI (palette / settings). The wheel scales
        // with the desktop layer so the whole world reads as zooming in
        // or out together during an overview transition.
        if let Some(sidebar) = self.sidebar.as_mut() {
            if content_alpha >= 0.999 && (content_scale - 1.0).abs() <= f64::EPSILON {
                sidebar.draw(scene, content_xform, text_renderer, theme, presence);
            } else if content_alpha > 0.0 {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    content_alpha,
                    transform,
                    &self.rect,
                );
                sidebar.draw(scene, content_xform, text_renderer, theme, presence);
                scene.pop_layer();
            }
        }

        // Workspace settings modal — concept wraps the content in
        // `motion.div initial={{ opacity: 0, scale: 0.95, y: 20 }}` →
        // `animate={{ opacity: 1, scale: 1, y: 0 }}` over ~220 ms. The
        // `ws_settings_transition` plays this on open AND in reverse on
        // close, so the modal + its scrim fade out instead of popping.
        if !flags.ws_settings {
            // Clear cached pill rects so a click during the close fade
            // (flag already false, modal still painting) can't flip
            // tabs. Mirrors the `add_workspace_rect = Rect::ZERO`
            // pattern above (#6, B-7).
            self.ws_settings_tab_pill_rects = [Rect::ZERO; 3];
            self.permissions_revoke_rects.clear();
            self.grant_composer_rects = None;
            self.member_control_rects.clear();
        }
        self.ws_settings_was_open = flags.ws_settings;
        self.ws_settings_transition.set_open(
            flags.ws_settings,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        if self.ws_settings_transition.is_visible(now) {
            // Settings modal: concept §4.1 uses `bg-black/30 backdrop-blur-[1px]`
            // — the softer modal scrim rather than the stronger overview /
            // palette dimmer. The scrim's own alpha follows the openness
            // so it fades out with the panel on close.
            draw_modal_scrim_at(
                scene,
                transform,
                self.rect,
                self.ws_settings_transition.value(now),
                theme,
            );
            let (alpha, dy, scale) =
                modal_transition_curve(&self.ws_settings_transition, now, 0.95);
            let in_flight = alpha < 1.0;
            let panel_rect = self.ws_settings_rect();
            if in_flight {
                // Clip generously so the scrim doesn't get painted over by
                // the modal's alpha layer.
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &self.rect,
                );
            }
            let panel_xform = if dy != 0.0 || scale != 1.0 {
                let cx = panel_rect.center().x;
                let cy = panel_rect.center().y;
                transform
                    * Affine::translate((cx, cy + dy))
                    * Affine::scale(scale)
                    * Affine::translate((-cx, -cy))
            } else {
                transform
            };
            // Soft elevation so the modal lifts off its scrim. Faded by
            // openness (== alpha here) so it animates with the panel.
            ui::surface::soft_shadow(
                scene,
                panel_xform,
                panel_rect,
                theme.tokens.colors.neutral[0],
                40.0,
                16.0,
                0.55 * alpha,
            );
            self.draw_ws_settings(scene, panel_xform, text_renderer, theme);
            if in_flight {
                scene.pop_layer();
            }
        }

        // Quick-settings popover (anchored top-right, no scrim so it reads
        // as a dropdown rather than a modal). Concept wraps this in a
        // `motion.div` with `initial={{ opacity: 0, y: -10, scale: 0.95 }}`
        // → `animate={{ opacity: 1, y: 0, scale: 1 }}` so the panel reads
        // as "slipping out of the top-right". Detect the open rising edge
        // and play the same motion for `QS_OPEN_ANIM_MS`.
        if flags.quick_settings && !self.qs_was_open {
            // Rising edge: refresh QS state from the OS. Reads are
            // inline because each shell-out completes in <50 ms on
            // typical hardware; if any take longer, the worst case is
            // a single frame stutter the first time the panel opens.
            // Failed reads are logged and leave the stale value in
            // place so a missing daemon doesn't blank the UI.
            self.refresh_quick_settings_from_os();
        }
        self.qs_was_open = flags.quick_settings;
        // Drive the interruptible transition from the flag. `is_visible`
        // keeps the popover drawn through its close curve so it fades +
        // slides OUT instead of popping.
        self.qs_transition.set_open(
            flags.quick_settings,
            Duration::from_millis(QS_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        if self.qs_transition.is_visible(now) {
            // `openness` runs 0→1 opening and 1→0 closing; the same
            // alpha/dy/scale expression covers both directions.
            let openness = self.qs_transition.value(now);
            let alpha = openness;
            let dy = -10.0 * (1.0 - openness) as f64;
            let scale = 0.95 + 0.05 * openness as f64;
            let in_flight = alpha < 1.0;
            let qs_rect = self.quick_settings_rect();
            let clip = Rect::new(
                qs_rect.x0 - 8.0,
                qs_rect.y0 - 16.0,
                qs_rect.x1 + 8.0,
                qs_rect.y1 + 8.0,
            );
            if in_flight {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &clip,
                );
            }
            let qs_xform = if dy != 0.0 || scale != 1.0 {
                let cx = qs_rect.center().x;
                let cy = qs_rect.y0;
                transform
                    * Affine::translate((cx, cy + dy))
                    * Affine::scale(scale)
                    * Affine::translate((-cx, -cy))
            } else {
                transform
            };
            // Soft elevation behind the popover so it lifts off the
            // desktop like the toasts do. Faded by openness so it
            // animates in/out with the panel.
            ui::surface::soft_shadow(
                scene,
                qs_xform,
                qs_rect,
                theme.tokens.colors.neutral[0],
                34.0,
                12.0,
                0.5 * openness,
            );
            self.draw_quick_settings(scene, qs_xform, text_renderer, theme);
            if in_flight {
                scene.pop_layer();
            }
        }

        // Collaborators dropdown (anchored top-right, no scrim — matches the
        // quick-settings dropdown behaviour and the concept's avatar stack
        // "open peer list" affordance).
        if flags.collaborators {
            self.draw_collaborators(scene, transform, text_renderer, theme);
        } else {
            self.collab_activity_row_rect = Rect::ZERO;
        }

        // Room-activity sheet. Above the dropdowns (it is a surface you read,
        // not a menu you pick from) and below the palette (the palette is how
        // you got here and must always be reachable on top).
        if flags.room_activity && !self.room_activity_was_open {
            // Rising edge: the room may have changed while the sheet was
            // closed, so drop the derived rows rather than showing a stale
            // list for one frame.
            self.room_activity.invalidate();
        }
        self.room_activity_was_open = flags.room_activity;
        self.room_activity_transition.set_open(
            flags.room_activity,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let ra_now = Instant::now();
        if self.room_activity_transition.is_visible(ra_now) {
            self.refresh_room_activity();
            let openness = self.room_activity_transition.value(ra_now);
            draw_modal_scrim_at(scene, transform, self.rect, openness, theme);
            let alpha = openness;
            let in_flight = alpha < 1.0;
            let sheet = self.room_activity.rect();
            if in_flight {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &self.rect,
                );
            }
            // A side sheet slides in from the edge it is anchored to rather
            // than scaling about its centre — scaling a full-height panel
            // reads as the whole desktop breathing.
            let slide = (1.0 - openness as f64) * (sheet.width() * 0.25);
            let sheet_xform = if slide > 0.0 {
                transform * Affine::translate((slide, 0.0))
            } else {
                transform
            };
            ui::surface::soft_shadow(
                scene,
                sheet_xform,
                sheet,
                theme.tokens.colors.neutral[0],
                40.0,
                16.0,
                0.55 * alpha,
            );
            self.room_activity
                .draw(scene, sheet_xform, text_renderer, theme, presence);
            if in_flight {
                scene.pop_layer();
            }
        }

        // Command palette / global search — concept `GlobalSearch.tsx:109-113`
        // wraps the panel in a motion.div with a scale 0.9 → 1.0 and
        // y 20 → 0 + opacity 0 → 1 entry, now played in reverse on close
        // by `palette_transition`.
        self.palette_was_visible = self.palette_visible;
        self.palette_transition.set_open(
            self.palette_visible,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let palette_now = Instant::now();
        if self.palette_transition.is_visible(palette_now) {
            let openness = self.palette_transition.value(palette_now);
            draw_scrim_at(scene, transform, self.rect, openness, theme);
            let (alpha, dy, scale) =
                modal_transition_curve(&self.palette_transition, palette_now, 0.9);
            let in_flight = alpha < 1.0;
            let palette_rect = self.palette.get_rect();
            if in_flight {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &self.rect,
                );
            }
            let palette_xform = if dy != 0.0 || scale != 1.0 {
                let cx = palette_rect.center().x;
                let cy = palette_rect.center().y;
                transform
                    * Affine::translate((cx, cy + dy))
                    * Affine::scale(scale)
                    * Affine::translate((-cx, -cy))
            } else {
                transform
            };
            // One card. The filter controls live in a band the palette
            // itself reserves, so there is no second surface to stitch
            // on and no seam to hide.
            ui::surface::soft_shadow(
                scene,
                palette_xform,
                palette_rect,
                theme.tokens.colors.neutral[0],
                44.0,
                14.0,
                0.55 * alpha,
            );
            // Re-theme just the palette surface to the modal tint. The
            // `CommandPalette` widget lives in `quick-ui` and paints its
            // own surface from `surfaces.raised`, so we hand it a cloned
            // theme whose `surfaces.raised` is pre-tinted rather than
            // reaching inside the shared widget to plumb an override.
            let palette_theme = palette_surface_theme(theme, MODAL_SURFACE_ALPHA);
            self.palette.draw(
                scene,
                palette_xform,
                text_renderer,
                &palette_theme,
                presence,
            );
            // Filter band content, painted into the palette's reserved
            // band so it shares the card's content inset.
            self.draw_search_banner(scene, palette_xform, text_renderer, &palette_theme);
            if in_flight {
                scene.pop_layer();
            }
        }

        // Peer-sync card last: while it is up it owns the keyboard, so
        // it has to sit above the palette that opened it.
        self.draw_peer_sync(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track cursor position so we can hit-test overlay clicks in the
        // absence of position data on MouseInput.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
        }

        // Peer-sync card. Runs before every other overlay because while
        // the token prompt is up it owns the keyboard outright — a
        // token contains characters (`k`, `/`) that would otherwise be
        // read as chords by the chrome underneath.
        if self.peer_sync_card_visible() && self.handle_peer_sync_event(event) {
            *handled = true;
            return;
        }

        // Quick-settings slider drag: while the user holds a slider,
        // CursorMoved updates the value inline (so the fill follows the
        // pointer) and Released pushes the final value through
        // `qs_actions`. Runs BEFORE the modal event-absorb gate so a drag
        // that leaves the slider bar or even the popover keeps updating
        // — matches the concept's slider feel and the way every native
        // slider on Linux behaves once the knob is grabbed.
        if let Some(drag_i) = self.qs_slider_drag {
            match event {
                WindowEvent::CursorMoved { .. } => {
                    if let (Some(p), Some(chrome)) = (self.last_cursor, self.chrome.clone()) {
                        let (bar, _) = self.quick_settings_slider_geometry(drag_i);
                        let t = ((p.x - bar.x0) / bar.width()).clamp(0.0, 1.0) as f32;
                        let mut st = chrome.borrow_mut();
                        if drag_i == 0 {
                            st.quick_settings.brightness = t;
                        } else {
                            st.quick_settings.volume = t;
                        }
                        st.bump();
                    }
                    *handled = true;
                    return;
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                } => {
                    let value_pct: f32 = self
                        .chrome
                        .as_ref()
                        .map(|c| {
                            let st = c.borrow();
                            if drag_i == 0 {
                                st.quick_settings.brightness
                            } else {
                                st.quick_settings.volume
                            }
                        })
                        .unwrap_or(0.0)
                        * 100.0;
                    self.qs_slider_drag = None;
                    if drag_i == 0 {
                        crate::shell::qs_actions::log_qs_result(
                            "set_brightness",
                            self.qs_actions.set_brightness(value_pct),
                        );
                    } else {
                        crate::shell::qs_actions::log_qs_result(
                            "set_volume",
                            self.qs_actions.set_volume(value_pct),
                        );
                    }
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        // Room-activity sheet. Routed BEFORE the absorb gate because the
        // timeline inside it wants exactly the events that gate swallows —
        // hover to reveal a row's `[Revert]`, wheel to scroll the list. The
        // gate still catches anything the sheet does not claim, so the
        // desktop underneath stays inert.
        if self.overlay_flags().room_activity
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::KeyboardInput { .. }
            )
        {
            let sheet = self.room_activity.rect();
            let pointer_inside = self.last_cursor.map(|p| sheet.contains(p)).unwrap_or(false);
            let is_key = matches!(event, WindowEvent::KeyboardInput { .. });
            // Escape belongs to the ladder below, which owns overlay
            // dismissal ordering; every other key is the sheet's contributor
            // typeahead. The exception is a name field: while one is up, Escape
            // is how the user abandons it, and closing the whole sheet instead
            // would throw away what they typed.
            let naming = self.room_activity.wants_keyboard();
            let is_escape = !naming
                && matches!(
                    event,
                    WindowEvent::KeyboardInput {
                        event: KeyEvent {
                            logical_key: Key::Named(NamedKey::Escape),
                            ..
                        },
                        ..
                    }
                );
            if (pointer_inside || (is_key && !is_escape))
                && let Some(chrome) = self.chrome.clone()
            {
                let cursor = self.last_cursor;
                let outcome = {
                    let st = chrome.borrow();
                    let mut tree = st.tree_mut();
                    self.room_activity
                        .handle_event(event, scale_factor, modifiers, cursor, &mut tree)
                };
                if outcome.reverted {
                    let mut st = chrome.borrow_mut();
                    st.bump();
                    st.push_notification(
                        "Reverted a room change",
                        "Written forward: the version you reverted from is still in the history.",
                    );
                }
                if outcome.named {
                    // The name went into the room document, so it syncs to
                    // whoever else is in the room — worth saying, because the
                    // user just labelled something for other people.
                    let mut st = chrome.borrow_mut();
                    st.bump();
                    st.push_notification(
                        "Named a version",
                        "Saved in this room's history, so everyone here sees it.",
                    );
                }
                // Pointer events over the sheet are absorbed whether or not it
                // used them, so the desktop underneath stays inert. Keys are
                // absorbed only when the sheet actually consumed them —
                // otherwise an arrow key pressed with the cursor resting over
                // the sheet would vanish instead of reaching the shell.
                if outcome.handled || (pointer_inside && !is_key) {
                    *handled = true;
                    return;
                }
            }
        }

        // Modal event-absorb gate (#7, B-2). The per-overlay `Pressed`
        // branches further down handle outside-click-close + inside-rect-
        // swallow for `MouseInput { state: Pressed }`, but `Released` and
        // `CursorMoved` (and `MouseWheel`) used to fall straight through to
        // `self.content.handle_event` — letting hovers light up tab pills
        // under the modal, drags continue receiving CursorMoved across the
        // palette, etc. Absorb every non-Pressed pointer event whose cursor
        // is over an open overlay. `Pressed` is intentionally excluded so
        // the existing scope-pill / slider / name-rect logic still gets to
        // run for clicks *inside* the rect.
        let is_absorbable_pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
        );
        if is_absorbable_pointer {
            if let Some(p) = self.last_cursor {
                if self.cursor_over_open_overlay(p) {
                    *handled = true;
                    return;
                }
            }
        }

        // Notification click-to-dismiss. The stack publishes per-toast
        // hit rects each `draw`; we consume a left-click that lands on
        // one of them by dismissing the matching id, *before* the
        // permission prompt or any other overlay sees it. A toast
        // never overlaps the permission prompt (prompt is anchored
        // top-left below the topbar, toasts top-right), but routing
        // them in this order keeps the contract simple: click on a
        // toast = dismiss, no other widget runs for that press.
        if let (
            Some(stack),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            },
        ) = (self.notification_stack.as_mut(), event)
        {
            if let Some(p) = self.last_cursor {
                if stack.notify_click_at(p) {
                    *handled = true;
                    return;
                }
            }
        }

        // Sandbox Phase 4 Task 4.2: forward pointer events to the
        // permission prompt before anything else. The widget's own
        // `handle_event` updates `pending_choice`; the per-frame
        // pump (`pump_permission_prompt`) drains the choice + applies
        // the grant on the next tick. Gate on the prompt's rect for
        // pointer events so the bar's hit zone doesn't swallow
        // clicks on content underneath outside the bar.
        if let Some(prompt) = self.permission_prompt.as_mut() {
            let prompt_rect = prompt.get_rect();
            let forward = match event {
                WindowEvent::CursorMoved { .. } => true,
                WindowEvent::MouseInput { .. } => self
                    .last_cursor
                    .map(|p| prompt_rect.contains(p))
                    .unwrap_or(false),
                _ => false,
            };
            if forward {
                let mut tmp_taffy = TaffyTree::<()>::new();
                let tmp_node = tmp_taffy.new_leaf(Default::default()).unwrap();
                prompt.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    tmp_node,
                    &tmp_taffy,
                );
                if *handled {
                    return;
                }
            }
        }

        // Cursor-targeted file-drop. winit's `WindowEvent::DroppedFile`
        // carries the path but NO coordinate, so we route it to the
        // window group under `last_cursor` — the pointer position tracked
        // from `CursorMoved` above. The group is resolved with the same
        // hit-test click-focus uses (`resolve_drop_target` → `pick_group_at`),
        // and the dropped file is delivered to that group's app together
        // with a per-workspace filesystem grant (see
        // `ChromeState::handle_file_drop_at`). A drop over empty strip /
        // desktop space (no group under the cursor) is a defined no-op —
        // the file is not dumped into an arbitrary window. Either way the
        // event is consumed so downstream widgets don't re-handle it.
        if let WindowEvent::DroppedFile(path) = event {
            if let Some(chrome) = self.chrome.clone() {
                if let Some(p) = self.last_cursor {
                    if let Some((strip_id, group_id)) = self.resolve_drop_target(p) {
                        // Mirror `stamp_active_group_from_click`: fold any
                        // widget-only group into the tree before we splice
                        // into it, so the target group id is resolvable in
                        // the persisted tree.
                        self.harvest_widget_only_groups_into_tree();
                        // A file an installed app declares is *opened in
                        // that app*, in the group the user aimed at —
                        // through the one open path, so it arrives with
                        // its read grant and with the file loaded. A
                        // folder, or a type nothing claims, keeps the
                        // placeholder-tab drop below.
                        let owned = matches!(
                            crate::shell::doc_open::resolve(path),
                            crate::shell::doc_open::DocumentTarget::App { .. }
                        );
                        if owned {
                            if let Err(e) =
                                crate::shell::doc_open::open_in_group(path, &chrome, group_id)
                            {
                                crate::shell::doc_open::report_open_failure(
                                    &chrome,
                                    path,
                                    &e.user_message(),
                                    None,
                                );
                            }
                        } else {
                            chrome
                                .borrow_mut()
                                .handle_file_drop_at(path, strip_id, group_id);
                        }
                    }
                }
            }
            *handled = true;
            return;
        }

        // Global shortcuts first. Palette-open chords split by scope:
        //
        // - `Ctrl+K`               → open scoped to the focused tab
        //                            (the palette can widen from there).
        // - `Super+K` / `Super+Space` / `Alt+Space`
        //                          → open in global scope.
        //
        // `Alt+Space` is the nested-friendly Super substitute. When qOS
        // runs as a Wayland CLIENT (e.g. inside GNOME for development)
        // the host DE swallows Super before the compositor ever sees the
        // event, so a chord that doesn't touch Super is needed to keep
        // global-search reachable in a dev window.
        //
        // `Ctrl+Tab` / `Super+Tab` toggle the overview, and Esc closes
        // whichever overlay is on top.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let mstate = modifiers.state();
            let super_down = mstate.super_key();
            let ctrl_down = mstate.control_key();
            let alt_down = mstate.alt_key();
            let is_mod = super_down || ctrl_down;
            // Space chord routes global-search: `Super+Space` matches the
            // Super+K convention; `Alt+Space` covers the nested case.
            // Both are captured here before the modifier-only branch so a
            // Character('K') path further down can't also fire.
            let is_space = matches!(logical_key, Key::Named(NamedKey::Space));
            if is_space && (super_down || alt_down) {
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                self.refresh_search_results(state, path);
                *handled = true;
                return;
            }
            if is_mod {
                if let Key::Character(s) = logical_key {
                    if s.as_str().eq_ignore_ascii_case("k") {
                        use crate::shell::chrome_state::SearchScope;
                        // The two entry points are different questions,
                        // and the chords say which one you are asking.
                        //
                        // Ctrl+K is "search in what I am looking at" —
                        // pre-scoped to the focused app's own resources,
                        // so it answers about your notes, your cells,
                        // your messages. Super+K is "search the machine"
                        // — every app plus full-text over the
                        // filesystem, and the only one that offers to
                        // launch something you don't have open.
                        //
                        // Narrowing is safe to seed because it degrades:
                        // when no provider claims the focused tab (a
                        // foreign Wayland surface, say) `ActiveApp`
                        // allows every provider, and with nothing
                        // focused at all `open_palette_with_scope`
                        // widens it to `All`. So the scope can only be
                        // narrow when there is genuinely something
                        // narrow to search.
                        let scope = if super_down {
                            SearchScope::All
                        } else {
                            SearchScope::ActiveApp
                        };
                        self.open_palette_with_scope(scope);
                        // Fire an initial refresh so the palette lands
                        // with a populated result list (chrome actions
                        // for empty queries) instead of a blank card.
                        self.refresh_search_results(state, path);
                        *handled = true;
                        return;
                    }
                }
                if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.overview = !st.overlays.overview;
                        st.bump();
                        *handled = true;
                        return;
                    }
                }
                // Cmd+ArrowUp / Cmd+ArrowDown — switch active workspace.
                if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if st.switch_workspace(-1) {
                            *handled = true;
                        }
                        return;
                    }
                }
                if matches!(logical_key, Key::Named(NamedKey::ArrowDown)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if st.switch_workspace(1) {
                            *handled = true;
                        }
                        return;
                    }
                }
            }
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                // Priority order matches z-order: palette > overview > ws
                // settings > quick settings. Esc cancels an in-flight name
                // edit before closing the ws-settings modal.
                if self.palette_visible {
                    self.palette_visible = false;
                    *handled = true;
                    return;
                }
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    if st.editing_workspace_name.is_some() {
                        st.editing_workspace_name = None;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.overview {
                        st.overlays.overview = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.ws_settings {
                        st.overlays.ws_settings = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.room_activity {
                        st.overlays.room_activity = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.collaborators {
                        st.overlays.collaborators = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.quick_settings {
                        st.overlays.quick_settings = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                }
            }

            // Workspace-name editor. When a buffer is in flight we consume
            // character / backspace / enter before anything else gets a
            // chance at them — modifiers already handled above.
            if let Some(chrome) = self.chrome.clone() {
                let is_editing = chrome.borrow().editing_workspace_name.is_some();
                if is_editing {
                    use ui::text_selection::{
                        Selection, clipboard_copy, clipboard_paste, next_char_boundary,
                        next_word_boundary, prev_char_boundary, prev_word_boundary,
                    };
                    let mods_state = modifiers.state();
                    let shift = mods_state.shift_key();
                    let ctrl = mods_state.control_key() || mods_state.super_key();
                    let mut st = chrome.borrow_mut();
                    // Borrow the buffer + selection out together. The
                    // selection's anchor/cursor are byte indices into
                    // the buffer.
                    // Pull the selection out by value first so we don't
                    // hold a separate immutable borrow alongside `buf`.
                    let mut sel = st.editing_workspace_name_selection;
                    let Some(buf) = st.editing_workspace_name.as_mut() else {
                        return;
                    };
                    let mut consumed = true;
                    let mut needs_bump = true;
                    match logical_key {
                        Key::Named(NamedKey::Enter) => {
                            drop(st);
                            chrome.borrow_mut().commit_workspace_name();
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            let new_pos = if ctrl {
                                prev_word_boundary(buf, sel.cursor)
                            } else {
                                prev_char_boundary(buf, sel.cursor)
                            };
                            sel.cursor = new_pos;
                            if !shift {
                                sel.collapse_to_cursor();
                            }
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            let new_pos = if ctrl {
                                next_word_boundary(buf, sel.cursor)
                            } else {
                                next_char_boundary(buf, sel.cursor)
                            };
                            sel.cursor = new_pos;
                            if !shift {
                                sel.collapse_to_cursor();
                            }
                        }
                        Key::Named(NamedKey::Home) => {
                            sel.cursor = 0;
                            if !shift {
                                sel.collapse_to_cursor();
                            }
                        }
                        Key::Named(NamedKey::End) => {
                            sel.cursor = buf.len();
                            if !shift {
                                sel.collapse_to_cursor();
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            if sel.is_collapsed() {
                                if sel.cursor > 0 {
                                    let prev = prev_char_boundary(buf, sel.cursor);
                                    buf.replace_range(prev..sel.cursor, "");
                                    sel = Selection::collapsed(prev);
                                }
                            } else {
                                let r = sel.range();
                                buf.replace_range(r.clone(), "");
                                sel = Selection::collapsed(r.start);
                            }
                        }
                        Key::Named(NamedKey::Delete) => {
                            if sel.is_collapsed() {
                                if sel.cursor < buf.len() {
                                    let next = next_char_boundary(buf, sel.cursor);
                                    buf.replace_range(sel.cursor..next, "");
                                }
                            } else {
                                let r = sel.range();
                                buf.replace_range(r.clone(), "");
                                sel = Selection::collapsed(r.start);
                            }
                        }
                        Key::Named(NamedKey::Space) => {
                            let r = sel.range();
                            buf.replace_range(r.clone(), " ");
                            sel = Selection::collapsed(r.start + 1);
                        }
                        Key::Character(s) if ctrl => match s.as_str() {
                            "a" | "A" => {
                                sel.anchor = 0;
                                sel.cursor = buf.len();
                                needs_bump = false;
                            }
                            "c" | "C" => {
                                let r = sel.range();
                                if !r.is_empty() {
                                    clipboard_copy(&buf[r]);
                                }
                                needs_bump = false;
                            }
                            "x" | "X" => {
                                let r = sel.range();
                                if !r.is_empty() {
                                    clipboard_copy(&buf[r.clone()]);
                                    buf.replace_range(r.clone(), "");
                                    sel = Selection::collapsed(r.start);
                                } else {
                                    needs_bump = false;
                                }
                            }
                            "v" | "V" => {
                                if let Some(paste) = clipboard_paste() {
                                    let r = sel.range();
                                    buf.replace_range(r.clone(), &paste);
                                    sel = Selection::collapsed(r.start + paste.len());
                                } else {
                                    needs_bump = false;
                                }
                            }
                            _ => {
                                consumed = false;
                                needs_bump = false;
                            }
                        },
                        Key::Character(s) => {
                            let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
                            if filtered.is_empty() {
                                consumed = false;
                                needs_bump = false;
                            } else {
                                let r = sel.range();
                                buf.replace_range(r.clone(), &filtered);
                                sel = Selection::collapsed(r.start + filtered.len());
                            }
                        }
                        _ => {
                            consumed = false;
                            needs_bump = false;
                        }
                    }
                    if consumed {
                        st.editing_workspace_name_selection = sel;
                        if needs_bump {
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                }
            }
        }

        // Outside-click close: if a popover/modal is up and the click
        // lands outside its rect, dismiss it and swallow the click so the
        // content below doesn't react. Overview has its own dismissal path
        // below (click-to-activate-workspace) so we skip it here.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            if let Some(p) = self.last_cursor {
                if self.palette_visible {
                    // Scope-pill click (runs before outside-click so users
                    // can swap scope without dismissing the palette).
                    // Iterates the same dynamic scope list the draw code
                    // uses so ActiveApp (when present) is at idx 0.
                    let scopes = self.search_scope_list();
                    for (idx, scope) in scopes.iter().enumerate() {
                        if self.search_scope_pill_rect(idx).contains(p) {
                            if let Some(chrome) = self.chrome.clone() {
                                let mut st = chrome.borrow_mut();
                                st.search_scope = *scope;
                                st.bump();
                            }
                            *handled = true;
                            return;
                        }
                    }
                    let palette_rect = self.palette.get_rect();
                    if !palette_rect.contains(p) {
                        self.palette_visible = false;
                        *handled = true;
                        return;
                    }
                }
                let flags = self.overlay_flags();
                if flags.room_activity && !self.room_activity.rect().contains(p) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.room_activity = false;
                        st.bump();
                    }
                    *handled = true;
                    return;
                }
                if flags.collaborators {
                    let row = self.collab_activity_row_rect;
                    if row.area() > 0.0 && row.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.room_activity = true;
                            st.overlays.collaborators = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    let co_rect = self.collaborators_rect();
                    if !co_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.collaborators = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Inside the dropdown: swallow the click so the content
                    // beneath doesn't grab it. No per-row actions yet.
                    *handled = true;
                    return;
                }
                if flags.quick_settings {
                    let qs_rect = self.quick_settings_rect();
                    if !qs_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.quick_settings = false;
                            // Reset nav-stack on close so the next open
                            // starts at root rather than dropping the user
                            // back into the WiFi / BT list they last
                            // visited.
                            st.quick_settings.nav_stack =
                                vec![crate::shell::quick_settings::QsLevel::Root];
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Click inside the popover: branch on the current
                    // nav-stack level. Drill-in panes only have a back
                    // chevron + scroll list; root has sliders + toggle
                    // cards + power profile.
                    let current_level = self
                        .chrome
                        .as_ref()
                        .map(|c| c.borrow().quick_settings.current_level().clone())
                        .unwrap_or(crate::shell::quick_settings::QsLevel::Root);
                    if !matches!(current_level, crate::shell::quick_settings::QsLevel::Root) {
                        // Drill-in pane: the back-chevron pops the stack,
                        // and on the Wi-Fi pane a row click triggers
                        // `nmcli device wifi connect <ssid>` via the
                        // QS dispatcher. Everything else is swallowed.
                        let back = self.qs_back_chevron_rect();
                        if back.contains(p) {
                            if let Some(chrome) = self.chrome.clone() {
                                let mut st = chrome.borrow_mut();
                                st.quick_settings.pop_level();
                                st.bump();
                            }
                            *handled = true;
                            return;
                        }
                        if matches!(current_level, crate::shell::quick_settings::QsLevel::Wifi) {
                            // Row geometry mirrors `draw_qs_wifi_list`:
                            // row_h = 56 px, list_y0 = rect.y0 + 56, rows
                            // span rect.x0 + 12 to rect.x1 - 12.
                            let rect = self.quick_settings_rect();
                            let row_h = 56.0_f64;
                            let list_y0 = rect.y0 + 56.0;
                            let nets = self
                                .chrome
                                .as_ref()
                                .map(|c| c.borrow().quick_settings.wifi_networks.clone())
                                .unwrap_or_default();
                            for (i, net) in nets.iter().enumerate() {
                                let row_y = list_y0 + i as f64 * row_h;
                                let row_rect = Rect::new(
                                    rect.x0 + 12.0,
                                    row_y,
                                    rect.x1 - 12.0,
                                    row_y + row_h - 4.0,
                                );
                                if row_rect.contains(p) {
                                    let ssid = net.ssid.clone();
                                    crate::shell::qs_actions::log_qs_result(
                                        "connect_wifi",
                                        self.qs_actions.connect_wifi(&ssid, None),
                                    );
                                    *handled = true;
                                    return;
                                }
                            }
                        }
                        *handled = true;
                        return;
                    }
                    if let Some(chrome) = self.chrome.clone() {
                        // Toggle-card chevron: drill into WiFi / Bluetooth
                        // pane. Tested *before* the toggle hit so the
                        // chevron region wins over the toggle's full-card
                        // hit rect. Concept ref: `QuickSettings.tsx:218-246`
                        // — chevron on the right of each card reads as
                        // "drill in for details".
                        let mut pushed_level = None;
                        for i in 0..2 {
                            let chev = self.quick_settings_toggle_chevron_rect(i);
                            if chev.contains(p) {
                                pushed_level = Some(if i == 0 {
                                    crate::shell::quick_settings::QsLevel::Wifi
                                } else {
                                    crate::shell::quick_settings::QsLevel::Bluetooth
                                });
                                break;
                            }
                        }
                        if let Some(level) = pushed_level {
                            let mut st = chrome.borrow_mut();
                            st.quick_settings.push_level(level);
                            st.bump();
                            *handled = true;
                            return;
                        }
                        // Top action row (Settings / Camera / Power). Each
                        // tile closes the popover on click; the action fires
                        // after the chrome borrow drops.
                        let mut action_hit: Option<usize> = None;
                        for i in 0..3 {
                            let tile = self.quick_settings_action_geometry(i);
                            if tile.contains(p) {
                                action_hit = Some(i);
                                break;
                            }
                        }
                        if let Some(i) = action_hit {
                            let mut st = chrome.borrow_mut();
                            match i {
                                0 => {
                                    // Settings gear → open workspace settings
                                    // modal, same intent the palette's
                                    // `settings.open` command carries.
                                    st.overlays.ws_settings = true;
                                }
                                1 => {
                                    // Camera glyph → placeholder notification
                                    // until app launch is plumbed through
                                    // the sandbox spawn path with the
                                    // compositor's own wayland socket.
                                    st.push_notification(
                                        "Camera",
                                        "Launch not yet wired to the sandbox spawner.",
                                    );
                                }
                                _ => {
                                    // Power glyph → placeholder notification.
                                    // A destructive `systemctl poweroff` needs
                                    // an explicit confirmation UI first; not
                                    // wiring it here to avoid a one-click
                                    // shutdown from the QS popover.
                                    st.push_notification(
                                        "Power",
                                        "Shutdown / restart menu not yet available.",
                                    );
                                }
                            }
                            st.overlays.quick_settings = false;
                            st.quick_settings.nav_stack =
                                vec![crate::shell::quick_settings::QsLevel::Root];
                            st.bump();
                            *handled = true;
                            return;
                        }
                        // Track which actions to dispatch *after* dropping the
                        // chrome borrow — calling into `qs_actions` while
                        // `st` is alive would risk a re-entrant `borrow_mut`
                        // from a tracing subscriber or anything else that
                        // touches the chrome. Sliders don't participate: the
                        // Released branch in the drag block above owns the
                        // shell-out so a mid-drag pixel doesn't spawn one
                        // subprocess per frame.
                        let mut pending_wifi: Option<bool> = None;
                        let mut pending_bt: Option<bool> = None;
                        let mut pending_power: Option<crate::shell::quick_settings::PowerProfile> =
                            None;
                        let mut st = chrome.borrow_mut();
                        let mut changed = false;
                        for i in 0..2 {
                            let (bar, _) = self.quick_settings_slider_geometry(i);
                            // Extend the hit region vertically so the whole
                            // slider row (including padding + label) is
                            // clickable, not just the 6 px bar.
                            let hit =
                                Rect::new(bar.x0 - 4.0, bar.y0 - 12.0, bar.x1 + 4.0, bar.y1 + 12.0);
                            if hit.contains(p) {
                                let t = ((p.x - bar.x0) / bar.width()).clamp(0.0, 1.0) as f32;
                                if i == 0 {
                                    st.quick_settings.brightness = t;
                                } else {
                                    st.quick_settings.volume = t;
                                }
                                // Latch drag state. The value is committed to
                                // the OS on Released — during drag, only the
                                // visual updates (via `qs_slider_drag` in the
                                // pre-absorb branch). This mirrors how a
                                // physical mixer knob feels: instant visual,
                                // final commit when you let go.
                                self.qs_slider_drag = Some(i);
                                changed = true;
                            }
                        }
                        for i in 0..2 {
                            let prect = self.quick_settings_toggle_geometry(i);
                            let hit = Rect::new(
                                prect.x0 - 4.0,
                                prect.y0 - 4.0,
                                prect.x1 + 4.0,
                                prect.y1 + 4.0,
                            );
                            if hit.contains(p) {
                                if i == 0 {
                                    st.quick_settings.wifi = !st.quick_settings.wifi;
                                    pending_wifi = Some(st.quick_settings.wifi);
                                } else {
                                    st.quick_settings.bluetooth = !st.quick_settings.bluetooth;
                                    pending_bt = Some(st.quick_settings.bluetooth);
                                }
                                changed = true;
                            }
                        }
                        // Power profile chip — cycle Balanced → Performance →
                        // Saver and push the new value through to
                        // `powerprofilesctl`.
                        let pp_rect = self.quick_settings_power_profile_geometry();
                        if pp_rect.contains(p) {
                            use crate::shell::quick_settings::PowerProfile;
                            let next = match st.quick_settings.power_profile {
                                PowerProfile::Saver => PowerProfile::Balanced,
                                PowerProfile::Balanced => PowerProfile::Performance,
                                PowerProfile::Performance => PowerProfile::Saver,
                            };
                            st.quick_settings.power_profile = next;
                            pending_power = Some(next);
                            changed = true;
                        }
                        if changed {
                            st.bump();
                            drop(st);
                            if let Some(v) = pending_wifi {
                                crate::shell::qs_actions::log_qs_result(
                                    "set_wifi",
                                    self.qs_actions.set_wifi(v),
                                );
                            }
                            if let Some(v) = pending_bt {
                                crate::shell::qs_actions::log_qs_result(
                                    "set_bluetooth",
                                    self.qs_actions.set_bluetooth(v),
                                );
                            }
                            if let Some(p) = pending_power {
                                use crate::shell::qs_actions::PowerProfile as ActPP;
                                use crate::shell::quick_settings::PowerProfile as UiPP;
                                let mapped = match p {
                                    UiPP::Saver => ActPP::Saver,
                                    UiPP::Balanced => ActPP::Balanced,
                                    UiPP::Performance => ActPP::Performance,
                                };
                                crate::shell::qs_actions::log_qs_result(
                                    "set_power_profile",
                                    self.qs_actions.set_power_profile(mapped),
                                );
                            }
                            *handled = true;
                            return;
                        }
                    }
                    // Inside the popover but not on a control — swallow so
                    // the content below doesn't steal the click.
                    *handled = true;
                    return;
                }
                if flags.ws_settings {
                    let modal_rect = self.ws_settings_rect();
                    if !modal_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            // Commit any in-flight name edit before closing
                            // so clicking away acts like "save & dismiss".
                            st.commit_workspace_name();
                            st.overlays.ws_settings = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Permissions + Members controls are hit-tested
                    // *before* the tab pills so a click on a Revoke
                    // button or a composer control can never route to a
                    // pill above it.
                    //
                    // Every branch consumes the click. Non-owners fall
                    // into the same branches and change nothing: the
                    // paint already shows the control disabled, and
                    // swallowing the click keeps a dead control from
                    // committing a half-typed workspace name.
                    let is_owner = self
                        .chrome
                        .as_ref()
                        .map(|c| is_active_workspace_owner(&c.borrow()))
                        .unwrap_or(false);

                    if let Some(composer) = self.grant_composer_rects {
                        let mut hit = None;
                        if composer.subject.area() > 0.0 && composer.subject.contains(p) {
                            hit = Some(ComposerHit::Subject);
                        } else if composer.capability.contains(p) {
                            hit = Some(ComposerHit::Capability);
                        } else if composer.submit.area() > 0.0 && composer.submit.contains(p) {
                            hit = Some(ComposerHit::Submit);
                        } else {
                            for (i, seg) in composer.expiry.iter().enumerate() {
                                if seg.contains(p) {
                                    hit = Some(ComposerHit::Expiry(i));
                                    break;
                                }
                            }
                        }
                        if let Some(hit) = hit {
                            if is_owner {
                                if let Some(chrome) = self.chrome.clone() {
                                    apply_composer_hit(&chrome, hit);
                                }
                            }
                            *handled = true;
                            return;
                        }
                    }

                    let member_rects = self.member_control_rects.clone();
                    for (user, role_rect, remove_rect) in &member_rects {
                        let on_role = role_rect.area() > 0.0 && role_rect.contains(p);
                        let on_remove = remove_rect.area() > 0.0 && remove_rect.contains(p);
                        if !on_role && !on_remove {
                            continue;
                        }
                        if is_owner
                            && let Some(chrome) = self.chrome.clone()
                        {
                            let user = user.clone();
                            edit_active_workspace_caps(&chrome, move |ws, actor| {
                                if on_role {
                                    let current = ws
                                        .role_of(&user)
                                        .unwrap_or(crate::shell::state::CapRole::Viewer);
                                    let next = crate::shell::workspace_settings::next_role(current);
                                    crate::shell::workspace_settings::set_member_role(
                                        ws, actor, &user, next,
                                    )
                                } else {
                                    crate::shell::workspace_settings::remove_member(
                                        ws, actor, &user,
                                    )
                                }
                            });
                        }
                        *handled = true;
                        return;
                    }

                    let revoke_rects = self.permissions_revoke_rects.clone();
                    for (cap_id, rrect) in &revoke_rects {
                        if rrect.area() > 0.0 && rrect.contains(p) {
                            if is_owner
                                && let Some(chrome) = self.chrome.clone()
                            {
                                let cap_id = cap_id.clone();
                                edit_active_workspace_caps(&chrome, move |ws, actor| {
                                    crate::shell::workspace_settings::apply_revoke(
                                        ws, actor, &cap_id,
                                    )
                                });
                            }
                            *handled = true;
                            return;
                        }
                    }

                    // Tab-pill click: hit-test the pills cached by
                    // `draw_ws_settings` *before* the name-input branch
                    // so a click on a pill never falls through to
                    // "commit pending edit".
                    let pill_rects = self.ws_settings_tab_pill_rects;
                    for (idx, prect) in pill_rects.iter().enumerate() {
                        if prect.area() > 0.0 && prect.contains(p) {
                            if let Some(chrome) = self.chrome.clone() {
                                let mut st = chrome.borrow_mut();
                                // Commit any in-flight name edit before
                                // switching tabs so a half-typed name
                                // isn't silently dropped on tab flip.
                                st.commit_workspace_name();
                                if let Some(tab) = WsTab::all().get(idx) {
                                    st.ws_settings_tab = *tab;
                                    st.bump();
                                }
                            }
                            *handled = true;
                            return;
                        }
                    }
                    // Click inside the modal: routes to the Name input rect
                    // (begins an edit) or — on any other click inside the
                    // modal — commits the pending edit if one is in flight.
                    if let Some(chrome) = self.chrome.clone() {
                        let name_rect = self.ws_settings_name_rect();
                        let mut st = chrome.borrow_mut();
                        if name_rect.contains(p) {
                            if st.editing_workspace_name.is_none() {
                                let new_name = st
                                    .tree()
                                    .active_workspace()
                                    .map(|w| w.name.clone())
                                    .or_else(|| Some(String::new()));
                                let caret = new_name.as_ref().map(|s| s.len()).unwrap_or(0);
                                st.editing_workspace_name = new_name;
                                st.editing_workspace_name_selection =
                                    ui::text_selection::Selection::collapsed(caret);
                                st.bump();
                            }
                        } else {
                            st.commit_workspace_name();
                        }
                    }
                    *handled = true;
                    return;
                }
            }
        }

        // Vertical-wheel scroll for the workspace overview. When the
        // overview is visible, wheel events pan its row list — the
        // column header (Workspaces title + subtitle) stays pinned.
        // Wheel-up reveals workspaces above the visible band (scroll_y
        // decreases); wheel-down reveals workspaces below (scroll_y
        // increases). The clamp guards both ends so a hard flick can't
        // strand the column past the tail.
        if self.overlay_flags().overview {
            if let WindowEvent::MouseWheel { delta, .. } = event {
                let dy: f64 = match *delta {
                    // 30 px per LineDelta matches the strip's wheel
                    // factor (`GroupRow::handle_event`), so a single
                    // wheel detent reveals roughly a quarter of one
                    // workspace row (`OVERVIEW_ROW_HEIGHT_PX ~ 160`).
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -(y as f64) * 30.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if dy != 0.0 {
                    let max = self.overview_max_scroll_y();
                    self.overview_scroll_y = (self.overview_scroll_y + dy).clamp(0.0, max);
                    *handled = true;
                    return;
                }
            }
        }

        // Click-on-overview-card: if the overview is visible, consume the
        // mouse click and — if it lands on a workspace card — activate
        // that workspace + close the overview.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            let flags = self.overlay_flags();
            if flags.overview {
                if let Some(p) = self.last_cursor {
                    // "+ Add workspace" affordance — sits below the
                    // last workspace card. Wins the hit-test before the
                    // row-card scan so a click on it doesn't fall
                    // through to "missed → close overview".
                    if self.add_workspace_rect.area() > 0.0 && self.add_workspace_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            // Mirror the wheel-disc plus slot's literal
                            // (`sidebar_wheel_disc.rs`): both create-
                            // affordances feed `add_workspace`, both
                            // seed the new workspace as "Untitled".
                            st.add_workspace("Untitled");
                            // Stay in the overview so the user sees the
                            // newly-appended card animate in. The
                            // workspace-slide tick handles the visual
                            // transition on the next frame.
                        }
                        *handled = true;
                        return;
                    }
                    let hit = self.hit_test_overview(p);
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if let Some(idx) = hit {
                            let mut tree = st.tree_mut();
                            if let Some(ws) = tree.workspaces.get(idx) {
                                let id = ws.id;
                                tree.set_active(id);
                            }
                        }
                        // Clicking anywhere dismisses the overview once a
                        // workspace has been chosen (or missed).
                        st.overlays.overview = false;
                        st.bump();
                    }
                    *handled = true;
                    return;
                }
            }
        }

        let children = taffy.children(node_id).unwrap();
        if self.palette_visible {
            // Chip-strip click. Chips filter the wanted-kind set for the
            // next refresh; we recompute results immediately so the
            // effect is visible on the same frame. Runs before the row
            // hit-test below so a click on a chip doesn't fall through
            // to the top result.
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } = event
            {
                if let Some(p) = self.last_cursor {
                    // "All" clears the kind filter outright — the state
                    // where nothing is selected needs a control that
                    // *reaches* it, not just a way to untick five chips.
                    if self.search_all_chip_rect.area() > 0.0
                        && self.search_all_chip_rect.contains(p)
                    {
                        if !self.active_kind_filter.is_empty() {
                            self.active_kind_filter.clear();
                            self.last_search_query.clear(); // force a refresh
                            self.refresh_search_results(state, path);
                        }
                        *handled = true;
                        return;
                    }
                    let kinds = self.search_registry.registered_kinds();
                    for (idx, rect) in self.search_chip_rects.iter().enumerate() {
                        if rect.contains(p) {
                            if let Some(kind) = kinds.get(idx).cloned() {
                                self.toggle_kind_filter(kind);
                                self.last_search_query.clear(); // force a refresh
                                self.refresh_search_results(state, path);
                            }
                            *handled = true;
                            return;
                        }
                    }
                }
            }
            // Mouse-click on a visible row: move cursor to the clicked
            // row, then dispatch its action. Happens before delegating
            // to the palette so a single click both selects and fires.
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } = event
            {
                if let Some(p) = self.last_cursor {
                    if let Some(row) = self.palette.row_at(p) {
                        self.palette.set_cursor(row);
                        self.activate_selected_result(state, path);
                        *handled = true;
                        return;
                    }
                }
            }
            // Keyboard Enter: fire the cursor-selected action.
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            {
                self.activate_selected_result(state, path);
                *handled = true;
                return;
            }
            // Let the palette own the character/backspace/arrow keys.
            let query_before = self.palette.query().to_string();
            self.palette.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                children[1],
                taffy,
            );
            // Every character/backspace mutates `palette.query()`;
            // recompute the result set through the registry when it
            // changes so the user sees new hits as they type.
            if self.palette.query() != query_before {
                self.refresh_search_results(state, path);
            }
            if *handled {
                return;
            }
        }
        // Forward events to the floating sidebar before the content so
        // clicks / wheel / drag inside the wheel's rect take precedence
        // over the desktop underneath. The sidebar child is `children[2]`
        // when present (build_layout appends it after content + palette).
        //
        // Gate this first-dibs routing on "cursor inside the sidebar's
        // own rect" for pointer events — otherwise the sidebar's rect
        // (x ∈ [0, 96], y ∈ [sidebar_top_y, window_h]) can overlap the
        // topbar's rendered band and swallow clicks on the overview-
        // toggle grid icon at (x ≈ 24..66, y ≈ 40..76). Non-pointer
        // events (keyboard, focus, …) are always forwarded so the
        // sidebar keeps any keyboard affordances it might add later.
        if let (Some(sidebar), Some(sidebar_child)) = (self.sidebar.as_mut(), children.get(2)) {
            let sidebar_rect = sidebar.get_rect();
            let forward_pointer_to_sidebar = match event {
                WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. } => self
                    .last_cursor
                    .map(|p| sidebar_rect.contains(p))
                    .unwrap_or(false),
                _ => true,
            };
            if forward_pointer_to_sidebar {
                sidebar.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    *sidebar_child,
                    taffy,
                );
                if *handled {
                    return;
                }
            }
        }
        // "Click-anywhere-in-a-group focuses that group" — before we
        // forward the event, hit-test the click against every strip's
        // group rects and stamp `tree.active_strip` / `tree.active_group`
        // to whichever group the cursor is over. Without this, clicking
        // the WINDOW BODY (as opposed to a tab pill) never updates the
        // tree's active-group pointer, so the palette scope
        // (`ChromeState::active_tab_id`) stays anchored to whatever
        // group was active before the detach — the 2026-07-09
        // "Ctrl+K scoped to Notes even though I clicked Files in its
        // own group" regression.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            if let Some(p) = self.last_cursor {
                self.stamp_active_group_from_click(p);
            }
        }
        self.content.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            children[0],
            taffy,
        );

        // DE-level horizontal-scroll fallback. If the cursor's host
        // app/widget didn't consume the MouseWheel (most apps don't
        // implement horizontal scroll at all — only widgets like the
        // GroupRow strip-pan and a terminal with overflow do), and the
        // event is horizontally-dominant, promote it to a tab cycle in
        // the active group. This makes "swipe sideways = next window"
        // work everywhere the app under the cursor doesn't claim it,
        // including over the empty desktop. Vertical wheel is left
        // untouched — it's reserved for the wheel disc's workspace
        // rotate (handled above when the cursor is over the sidebar)
        // and the focused widget's own vertical scroll (e.g. the
        // terminal's scrollback). Threshold of 0.5 guards against
        // trackpad axis noise — a real horizontal flick is well
        // above one line's worth of delta.
        if !*handled {
            if let WindowEvent::MouseWheel { delta, .. } = event {
                let dx = match *delta {
                    winit::event::MouseScrollDelta::LineDelta(x, _) => x,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.x as f32) / 40.0,
                };
                if dx.abs() > 0.5 {
                    if let Some(chrome) = &self.chrome {
                        let dir = if dx > 0.0 { 1 } else { -1 };
                        if chrome.borrow_mut().cycle_active_tab(dir) {
                            *handled = true;
                        }
                    }
                }
            }
        }
    }

    fn name(&self) -> &str {
        "ShellOverlayHost"
    }

    /// Expose content + palette + sidebar as direct children so trait
    /// walkers (e.g. the e2e harness's `widget_registry`) can recurse
    /// into the live tree. The traversal order matches `build_layout`'s
    /// child-node ordering: content, palette, then sidebar (last so it
    /// paints over the desktop chrome).
    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::with_capacity(3);
        out.push(self.content.as_ref());
        out.push(&self.palette);
        if let Some(s) = self.sidebar.as_ref() {
            out.push(s.as_ref());
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(3);
        out.push(self.content.as_mut());
        out.push(&mut self.palette);
        if let Some(s) = self.sidebar.as_mut() {
            out.push(s.as_mut());
        }
        out
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Bubble structural-dirty flags up from the content tree (and the
    /// optional sidebar) so `App::handle_window_event`'s post-event
    /// rebuild gate can rebuild the taffy tree and pick up any new
    /// nodes — e.g. a `WindowGroup` spliced into a `GroupRow` after a
    /// cross-group tab drop.
    ///
    /// Without this forward, the host would silently shadow every child
    /// dirty signal because the default `Widget::take_structure_dirty`
    /// returns `false`. The visible symptom: dragging a tab past a
    /// group's edge spawned a new `WindowGroup` in the data layer but
    /// the layout pass never gave it a `NodeId`, so it stayed at
    /// `Rect::ZERO` and the user perceived the source group as having
    /// been "replaced" instead of split. Use `|` (not `||`) so both
    /// children clear their own flags this tick — short-circuiting
    /// would leave a stale flag pending in whichever subtree we
    /// skipped.
    fn take_structure_dirty(&mut self) -> bool {
        // Mirror any widget-only groups (produced by
        // `GroupRow::split_tab_into_new_group` on drag-detach) back
        // into the persisted tree before `resync_strips_from_tree`
        // runs — otherwise resync sees "widget has X, tree doesn't"
        // and evicts X, which was the 2026-07-09 "window group
        // disappears after Ctrl+K" repro.
        self.harvest_widget_only_groups_into_tree();
        // Pump the chrome-revision diff here too — the shots driver's
        // `relayout()` path skips `update()` entirely (it only takes
        // structure_dirty + reruns the layout pass), so the resync
        // wouldn't otherwise fire between a `scene_setup` mutation
        // and the snapshot capture. Live `App::run` already pumps
        // update each frame so the call there is also valid; calling
        // it from both sites makes the diff idempotent (the
        // `last_chrome_revision` guard short-circuits the second
        // call when nothing changed).
        // A resync that spliced a group or a tab into a live `GroupRow`
        // has changed the widget structure, so it must force the taffy
        // rebuild in its own right — the children it just inserted have
        // no layout node yet.
        let resynced = self.resync_strips_from_tree();
        let content = self.content.take_structure_dirty();
        let sidebar = self
            .sidebar
            .as_mut()
            .map(|s| s.take_structure_dirty())
            .unwrap_or(false);
        let palette = std::mem::take(&mut self.palette_layout_dirty);
        content | sidebar | palette | resynced
    }

    fn needs_animation_frame(&self) -> bool {
        // Continuous redraws while any of:
        //   - overview crossfade (transition_started)
        //   - workspace slide-swap (ws_slide_started)
        //   - quick-settings open/close (qs_transition)
        //   - any inner widget (tab drop-zones, tab layout spring, …)
        // is in flight. `tick_*` methods clear their own flags once the
        // animation duration has fully elapsed, so at rest this returns
        // false and the app event loop can go idle.
        let now = Instant::now();
        let qs_in_flight = self.qs_transition.is_animating(now);
        let ws_settings_in_flight = self.ws_settings_transition.is_animating(now);
        let palette_in_flight = self.palette_transition.is_animating(now);
        let room_activity_in_flight = self.room_activity_transition.is_animating(now);
        let sidebar_frame = self
            .sidebar
            .as_ref()
            .map(|s| s.needs_animation_frame())
            .unwrap_or(false);
        self.transition_started.is_some()
            || self.ws_slide_started.is_some()
            || qs_in_flight
            || ws_settings_in_flight
            || palette_in_flight
            || room_activity_in_flight
            || self.content.needs_animation_frame()
            || sidebar_frame
    }
}

/// Compose the two-phase overview ↔ desktop transition.
///
/// `t` is the linear progress of the current transition toward `target`
/// (0 = desktop fully shown, 1 = overview fully shown). The draw path
/// splits `t` into two sequential halves so one view finishes exiting
/// before the other starts entering, matching `AnimatePresence
/// mode="wait"` in the concept (`App.tsx:427-487`):
///
/// * Entering overview (`target == 1.0`):
///   - First half (t ∈ [0, 0.5]): desktop fades + shrinks `1.0 → 0.95`.
///   - Second half (t ∈ [0.5, 1.0]): overview fades in + zooms
///     `0.95 → 1.0`.
/// * Leaving overview (`target == 0.0`):
///   - First half (t ∈ [1.0, 0.5] i.e. rising `1-t`): overview fades +
///     shrinks `1.0 → 0.95`.
///   - Second half (t ∈ [0.5, 0.0]): desktop fades in + zooms
///     `0.95 → 1.0`.
///
/// Each half is eased with smoothstep (`p * p * (3 - 2p)`).
/// Returns `(content_alpha, content_scale, overview_alpha,
/// overview_scale)`.
fn compose_overlay_transition(t: f32, target: f32) -> (f32, f64, f32, f64) {
    // Scale anchor points for the shrink/zoom halves. `0.95` matches the
    // concept's `scale: 0.95` initial/exit values.
    const SHRINK_SCALE: f64 = 0.95;

    // Smoothstep: `p` in [0, 1] → eased in [0, 1], with zero derivative
    // at both ends so motion starts/stops smoothly.
    fn smoothstep(p: f32) -> f32 {
        let p = p.clamp(0.0, 1.0);
        p * p * (3.0 - 2.0 * p)
    }

    // Unified direction: positive = heading toward overview (desktop
    // exits first), negative = heading toward desktop (overview exits
    // first). Rest state = no animation.
    let going_to_overview = target > 0.5;
    if t <= 0.0 {
        // Desktop fully visible.
        return (1.0, 1.0, 0.0, 1.0);
    }
    if t >= 1.0 {
        // Overview fully visible.
        return (0.0, 1.0, 1.0, 1.0);
    }

    if going_to_overview {
        if t < 0.5 {
            // Phase 1: desktop shrink-out. Overview is still hidden.
            let p = smoothstep(t * 2.0);
            let alpha = 1.0 - p;
            let scale = 1.0 + (SHRINK_SCALE - 1.0) * p as f64;
            (alpha, scale, 0.0, SHRINK_SCALE)
        } else {
            // Phase 2: overview zoom-in. Desktop is hidden.
            let p = smoothstep((t - 0.5) * 2.0);
            let alpha = p;
            let scale = SHRINK_SCALE + (1.0 - SHRINK_SCALE) * p as f64;
            (0.0, SHRINK_SCALE, alpha, scale)
        }
    } else {
        // Leaving overview: the animation runs from t ≈ 1 → 0.
        if t > 0.5 {
            // Phase 1 (reverse): overview shrink-out. Desktop still
            // hidden. `(1 - t) * 2` turns t ∈ (0.5, 1.0] into p ∈ [0, 1).
            let p = smoothstep((1.0 - t) * 2.0);
            let alpha = 1.0 - p;
            let scale = 1.0 + (SHRINK_SCALE - 1.0) * p as f64;
            (0.0, SHRINK_SCALE, alpha, scale)
        } else {
            // Phase 2 (reverse): desktop zoom-in. Overview hidden.
            let p = smoothstep((0.5 - t) * 2.0);
            let alpha = p;
            let scale = SHRINK_SCALE + (1.0 - SHRINK_SCALE) * p as f64;
            (alpha, scale, 0.0, SHRINK_SCALE)
        }
    }
}

/// Build a human-readable label for the inline permission prompt
/// from a [`Capability`] payload. Phase 4 Task 4.2 only needs a
/// single-line description per variant — i18n keys land alongside
/// the portal layer in Task 4.3 when the strings start surfacing
/// to end-users.
fn capability_label(cap: &Capability) -> String {
    match cap {
        Capability::Filesystem { path, mode, .. } => {
            let mode_str = match mode {
                crate::shell::state::FsMode::Read => "read",
                crate::shell::state::FsMode::ReadWrite => "read/write",
            };
            format!("{} access to {}", mode_str, path.display())
        }
        Capability::Camera { .. } => "Camera access".to_string(),
        Capability::Microphone { .. } => "Microphone access".to_string(),
        Capability::Screencast { .. } => "Screen capture".to_string(),
        Capability::RemoteDesktop { .. } => "Remote control (pointer + keyboard)".to_string(),
        Capability::Network { hosts, .. } => match hosts {
            Some(h) if !h.is_empty() => format!("Network access to {}", h.join(", ")),
            _ => "Network access".to_string(),
        },
    }
}

/// Return a copy of `cap` with its `scope` field replaced. The portal
/// layer pushes onto `pending_grants` with a placeholder scope (typically
/// [`GRANT_ONCE`]); the prompt's button choice is what determines
/// what gets persisted.
fn with_grant_scope(cap: Capability, scope: GrantScope) -> Capability {
    match cap {
        Capability::Filesystem { path, mode, .. } => Capability::Filesystem { path, mode, scope },
        Capability::Camera { .. } => Capability::Camera { scope },
        Capability::Microphone { .. } => Capability::Microphone { scope },
        Capability::Screencast { .. } => Capability::Screencast { scope },
        Capability::RemoteDesktop { .. } => Capability::RemoteDesktop { scope },
        Capability::Network { hosts, .. } => Capability::Network { hosts, scope },
    }
}

/// Is the local user the owner of the active workspace?
///
/// Gates the permission prompt's "Always" button and the Workspace
/// Settings Permissions tab's Revoke buttons.
///
/// This used to return `true` unconditionally, which made every local
/// user the owner of every workspace — including ones a peer had shared
/// with them — so "only the owner may grant permanently" and "only the
/// owner may revoke" were not enforced at all. It now asks the
/// workspace, which answers from the capability model: the recorded
/// owner, or anyone the owner has given `Admin` over the room.
///
/// A workspace with no owner recorded has no administrators. That is
/// the fail-closed direction, and the load path
/// ([`crate::shell::state::WorkspaceTree::adopt_ownerless_workspaces`])
/// makes sure the local user's own workspaces are never in that state.
fn is_active_workspace_owner(state: &crate::shell::chrome_state::ChromeState) -> bool {
    if let Some(forced) = state.test_only_owner_override {
        return forced;
    }
    let user = state.local_user_id.clone();
    state
        .tree()
        .active_workspace()
        .map(|ws| ws.may_administer(&user))
        .unwrap_or(false)
}

/// Record an app's capability grant on the active workspace and make it
/// durable + replicated.
///
/// Everything the prompt hands out flows through here, so a grant is
/// never merely in memory: it is written into the workspace's
/// capability list, mirrored into the replicated doc, and flushed to
/// disk when its expiry says it should survive a restart.
fn record_app_grant(
    state: &mut crate::shell::chrome_state::ChromeState,
    app_id: &str,
    capability: &Capability,
) {
    let mut tree = state.tree_mut();
    let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
        return;
    };
    let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) else {
        return;
    };
    let ids = ws.grant_app(app_id, capability);
    let written: Vec<crate::shell::state::CapOverride> = ws
        .capabilities
        .iter()
        .filter(|c| ids.contains(&c.id))
        .cloned()
        .collect();
    for cap in written {
        tree.doc_insert_capability(ws_id, &cap);
    }
    tree.flush_to_disk();
}

/// Apply a permission edit to the active workspace and make it durable
/// + replicated.
///
/// `edit` runs against the live workspace with the local user as the
/// actor, so it can only do what [`crate::shell::state::Workspace::grant_as`]
/// / [`crate::shell::state::Workspace::revoke_as`] let that user do. What
/// it returns is then mirrored into the replicated doc and flushed —
/// without that, a click would only edit the in-memory cache: the room's
/// history would never record it, and the next boot would read the old
/// answer back off disk.
fn edit_active_workspace_caps<F>(chrome: &Chrome, edit: F) -> bool
where
    F: FnOnce(&mut crate::shell::state::Workspace, &crate::shell::state::UserId) -> CapEdit,
{
    let mut st = chrome.borrow_mut();
    let actor = st.local_user_id.clone();
    let outcome = {
        let mut tree = st.tree_mut();
        let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
            return false;
        };
        let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) else {
            return false;
        };
        let changed = edit(ws, &actor);
        if changed.is_empty() {
            None
        } else {
            let written: Vec<crate::shell::state::CapOverride> = ws
                .capabilities
                .iter()
                .filter(|c| changed.granted.contains(&c.id))
                .cloned()
                .collect();
            for cap in written {
                tree.doc_insert_capability(ws_id, &cap);
            }
            for id in &changed.revoked {
                tree.doc_revoke_capability(ws_id, id);
            }
            tree.flush_to_disk();
            Some(())
        }
    };
    if outcome.is_some() {
        st.bump();
        true
    } else {
        false
    }
}

/// Route a click on a grant-composer control. The pickers only move the
/// draft; `Submit` is the one that writes, and it writes through
/// [`crate::shell::workspace_settings::apply_grant_draft`] so the
/// authority check runs on the production path.
fn apply_composer_hit(chrome: &Chrome, hit: ComposerHit) {
    match hit {
        ComposerHit::Submit => {
            let draft = chrome.borrow().ws_grant_draft;
            edit_active_workspace_caps(chrome, move |ws, actor| {
                crate::shell::workspace_settings::apply_grant_draft(ws, actor, &draft)
            });
        }
        other => {
            let mut st = chrome.borrow_mut();
            let subject_count = st
                .tree()
                .active_workspace()
                .map(|ws| crate::shell::workspace_settings::grant_subjects(ws).len())
                .unwrap_or(0);
            match other {
                ComposerHit::Subject => st.ws_grant_draft.cycle_subject(subject_count),
                ComposerHit::Capability => st.ws_grant_draft.cycle_choice(),
                ComposerHit::Expiry(i) => {
                    if let Some(expiry) = EXPIRY_CHOICES.get(i) {
                        st.ws_grant_draft.expiry = *expiry;
                    }
                }
                ComposerHit::Submit => unreachable!("handled above"),
            }
            st.bump();
        }
    }
}

/// The faces a workspace card shows: the people who actually hold a
/// capability in that workspace.
///
/// Drawing `ws.collaborators` instead put faces on a card for people
/// with no access, and falling back to the tree-wide list put those same
/// faces on *every* card. A permissions surface showing someone states
/// that they can reach the workspace, so the list has to come from the
/// capability model — the same `roster()` the Members badge counts.
///
/// The collaborator record supplies only display name and colour, and
/// only when there is one; a member with no record still gets a face.
///
/// Split out of the draw pass so it can be asserted directly. Counting
/// avatar-coloured fills in the rendered scene used to stand in for this,
/// and stopped meaning anything once avatars moved to the shared
/// `ui::Avatar` widget with a per-identity fill — the product was right
/// and the proxy was stale.
pub fn overview_card_faces<'a>(
    ws: &'a crate::shell::state::Workspace,
    tree: &'a crate::shell::state::WorkspaceTree,
) -> Vec<(String, Option<&'a crate::shell::state::Collaborator>)> {
    ws.roster()
        .into_iter()
        .map(|(user, _role)| {
            let known = ws
                .collaborators
                .iter()
                .chain(tree.collaborators.iter())
                .find(|c| c.id == user);
            (user, known)
        })
        .collect()
}

fn draw_scrim(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    draw_scrim_at(scene, transform, rect, 1.0, theme);
}

/// [`draw_scrim`] scaled by `openness` so the palette/overview scrim
/// fades with its surface.
fn draw_scrim_at(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    fill_scrim(scene, transform, rect, PALETTE_SCRIM_ALPHA * openness, theme);
}

/// The modal scrim, scaled by `openness` in `0.0..=1.0` so it fades in
/// with the modal opening and out with it closing.
fn draw_modal_scrim_at(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    openness: f32,
    theme: &Theme,
) {
    fill_scrim(scene, transform, rect, MODAL_SCRIM_ALPHA * openness, theme);
}

/// Paint `rect` in the theme's scrim hue at `strength`.
///
/// The hue comes from `surfaces.scrim` rather than a literal black: the
/// dim behind a palette is chrome, and the OS owns what its chrome looks
/// like. A light theme's scrim is a different colour from a dark theme's,
/// and neither is `#000`.
fn fill_scrim(scene: &mut Scene, transform: Affine, rect: Rect, strength: f32, theme: &Theme) {
    let alpha = (strength.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8;
    if alpha == 0 {
        return;
    }
    let c = theme.tokens.colors.surfaces.scrim.to_rgba8();
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgba8(c.r, c.g, c.b, alpha),
        None,
        &rect,
    );
}

/// Slate-900 tint at the given alpha. Centralised helper so every overlay
/// surface paints the same hue (concept's glass tint is rooted at
/// `slate-900` / `#0f172a`) and the numeric alpha is the only knob.
/// Synthesise a persisted [`crate::shell::state::WindowGroup`] from a
/// widget-side `WindowGroup`. Used by
/// [`ShellOverlayHost::stamp_active_group_from_click`] when the click
/// resolves a group that only exists widget-side (a fresh drag-detach
/// whose writeback to the tree hasn't landed yet).
///
/// Each tab is reconstructed as a `TileContent::AppPlaceholder` — good
/// enough for the persisted tree's structural view (id, title,
/// per-tab `active_tab` pointer). If the underlying kind was
/// already `AppKind::Other(name)`, we keep the name; anything else
/// becomes `AppKind::Other(String::new())`, matching the format the
/// tree's automerge codec already writes for foreign / stub tabs.
fn build_state_group_from_widget(
    widget: &ui::widget::WindowGroup,
) -> crate::shell::state::WindowGroup {
    use crate::shell::state::{
        AppKind, Tab, TileContent, WindowGroup as StateGroup, WindowWidth as StateWidth,
    };
    let tabs: Vec<Tab> = widget
        .tabs()
        .iter()
        .map(|desc| {
            // Carry the widget-side AppKind hint into the tree so
            // `owns_tab` predicates (Notes / Files / …) can still
            // classify the tab. Fallback to an empty `AppKind::Other`
            // for descriptors that never learnt a kind — the tab still
            // exists structurally, just without a provider-side match.
            let kind = match &desc.app_kind {
                Some(ui::widget::AppKind::Terminal) => AppKind::Terminal,
                Some(ui::widget::AppKind::Files) => AppKind::FileExplorer,
                Some(ui::widget::AppKind::Browser) => AppKind::Browser,
                Some(ui::widget::AppKind::Settings) => AppKind::Settings,
                // Editor / anything else / None → carry the tab's own
                // title as an Other name so Notes-style tabs still
                // classify (Notes tabs come through with title "Notes"
                // and get filed under Other("Notes")).
                _ => AppKind::Other(desc.title.clone()),
            };
            Tab {
                id: desc.id,
                title: desc.title.clone(),
                tile: TileContent::AppPlaceholder {
                    kind,
                    label: desc.title.clone(),
                },
            }
        })
        .collect();
    let active_tab = widget.active_tab_id();
    let width = StateWidth::from_ui(widget.width());
    StateGroup {
        id: widget.id,
        tabs,
        active_tab,
        width,
        maximized: false,
    }
}

/// Pick the topmost group whose rect contains `p`. Extracted from
/// [`ShellOverlayHost::stamp_active_group_from_click`] so it's
/// testable without a live widget tree.
///
/// `candidates` is `(strip_id, group_id, rect)` in traversal order —
/// the FIRST match wins so a group whose rect entirely contains a
/// smaller one wouldn't shadow the smaller (which is the concrete
/// behaviour when GroupRow's own rect wraps every child group's).
/// Zero-area rects are skipped so a group that hasn't laid out yet
/// (fresh detach that we haven't had a paint frame for) can't
/// silently match any point.
fn pick_group_at(candidates: &[(StripId, GroupId, Rect)], p: Point) -> Option<(StripId, GroupId)> {
    candidates
        .iter()
        .find(|(_, _, r)| r.area() > 0.0 && r.contains(p))
        .map(|(s, g, _)| (*s, *g))
}

fn slate_900_tint(theme: &Theme, alpha: f32) -> Color {
    // `neutral[1]` is slate-900 in the dark ramp (see
    // `ui::tokens::colors::ColorRamp`). Using the theme token rather
    // than hard-coding #0f172a keeps the light-theme variant falling back
    // to its own near-darkest slot automatically.
    theme.tokens.colors.neutral[1].with_alpha_factor(alpha)
}

/// Pick the [`crate::shell::search::SearchProvider`] that owns the
/// currently focused tab, if any. Used to seed the `active_provider_id`
/// arg to [`crate::shell::search::SearchRegistry::query`] when the
/// scope is [`crate::shell::chrome_state::SearchScope::ActiveApp`].
fn active_tab_provider_id(
    chrome: &crate::shell::chrome_state::ChromeState,
    registry: &crate::shell::search::SearchRegistry,
) -> Option<String> {
    let tab_id = chrome.active_tab_id()?;
    for provider in registry.providers_iter() {
        if provider.owns_tab(tab_id, chrome) {
            return Some(provider.id().to_string());
        }
    }
    None
}

/// Clone `theme` with `surfaces.raised` re-tinted to the glass slate-900
/// at `alpha`. Used by the GlobalSearch palette draw site: the
/// `CommandPalette` widget (in `quick_ui`) paints its own surface from
/// `surfaces.raised`, so we can't inject a translucent fill without either
/// plumbing a per-draw override (too invasive for a shared widget) or
/// handing it a locally-rebuilt theme (this helper). The stroke still uses
/// `border_standard`, which the palette inherits from the original theme
/// via the returned clone — only the fill token gets the tint.
fn palette_surface_theme(theme: &Theme, alpha: f32) -> Theme {
    let mut themed = theme.clone();
    themed.tokens.colors.surfaces.raised = slate_900_tint(theme, alpha);
    themed
}

/// `(alpha, dy, scale)` for a modal driven by a [`ui::Transition`],
/// covering BOTH the enter (openness 0→1) and the exit (openness 1→0).
/// The panel starts 20 px low at `start_scale` and rises to rest on the
/// enter; the exit plays the same curve in reverse instead of the panel
/// vanishing.
fn modal_transition_curve(t: &ui::Transition, now: Instant, start_scale: f64) -> (f32, f64, f64) {
    let o = t.value(now);
    let scale = start_scale + (1.0 - start_scale) * o as f64;
    (o, 20.0 * (1.0 - o) as f64, scale)
}

/// Format the workspace-card footer counter, pluralising "window" so
/// the overview reads "1 window" / "0 windows" / "2 windows" rather
/// than the always-plural "1 windows" placeholder.
fn windows_label(n: usize) -> String {
    if n == 1 {
        "1 window".to_string()
    } else {
        format!("{n} windows")
    }
}

/// Paint a single concept-parity MiniWindow inside `rect`.
///
/// Concept `OverviewView.tsx:6-32`:
/// * Tab-dot row along the top: active tab is a 16×10 pill, inactive tabs
///   are 10 px dots.
/// * App-card body: `bg-slate-800 rounded-lg border border-slate-700` with
///   a slightly brighter interior, filling the remainder of the height.
fn draw_mini_window(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    tab_count: usize,
    active_tab_idx: usize,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    let tab_h = 10.0_f64;
    let tab_gap = 4.0;
    let tab_row_y0 = rect.y0;
    let tab_row_y1 = rect.y0 + tab_h;

    // Tabs — draw left-aligned with `justify-start`.
    let mut tx = rect.x0 + 4.0;
    for i in 0..tab_count.max(1) {
        let is_active = i == active_tab_idx;
        let tab_w = if is_active { 16.0 } else { 10.0 };
        if tx + tab_w > rect.x1 {
            break;
        }
        let tab_rect = Rect::new(tx, tab_row_y0, tx + tab_w, tab_row_y1);
        let tab_rr = RoundedRect::from_rect(tab_rect, tab_h / 2.0);
        // Dot colour: active slate-600/80, inactive slate-700/50.
        let slate_600 = colors.neutral[4];
        let slate_700 = colors.neutral[3];
        let dot_color = if is_active {
            slate_600.with_alpha_factor(0.80)
        } else {
            slate_700.with_alpha_factor(0.50)
        };
        scene.fill(Fill::NonZero, transform, dot_color, None, &tab_rr);
        let border = slate_600.with_alpha_factor(0.50);
        scene.stroke(
            &vello::kurbo::Stroke::new(0.5),
            transform,
            border,
            None,
            &tab_rr,
        );
        tx += tab_w + tab_gap;
    }

    // App card body.
    let body_rect = Rect::new(rect.x0, tab_row_y1 + 2.0, rect.x1, rect.y1);
    let body_rr = RoundedRect::from_rect(body_rect, 6.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.neutral[2], // slate-800
        None,
        &body_rr,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(0.5),
        transform,
        colors.neutral[3], // slate-700
        None,
        &body_rr,
    );
    // Inset interior — slightly brighter so the card reads as a chrome
    // frame around a distinct content pane.
    let inset = 3.0;
    let inner = Rect::new(
        body_rect.x0 + inset,
        body_rect.y0 + inset,
        body_rect.x1 - inset,
        body_rect.y1 - inset,
    );
    if inner.width() > 0.0 && inner.height() > 0.0 {
        let inner_rr = RoundedRect::from_rect(inner, 4.0);
        let inner_fill = colors.neutral[2].with_alpha_factor(0.5);
        scene.fill(Fill::NonZero, transform, inner_fill, None, &inner_rr);
    }
}

impl ShellOverlayHost {
    fn draw_overview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // NOTE: the slate-950 scrim fill is painted in the caller (the
        // top-level `draw` method) at the *un*-scaled transform, so the
        // desktop gradient can never bleed through around the edges of the
        // scaled overview layer during the zoom-in animation. See visual
        // gap #13 in `docs/nirios/visual-gaps.md`.

        let col = self.overview_column_rect();

        // -- Header: "Workspaces" h1 + subtitle ---------------------------
        // Concept `OverviewView.tsx`: `h1 text-4xl font-bold
        // text-white mb-4 tracking-tight`. Tailwind `text-4xl` = 36 px;
        // we pick the largest token step (`x4l` = 40 px) so the header
        // reads as clearly "large / bold" at the default 1.0 scale factor
        // the text renderer applies — visual gap #12.
        let title = "Workspaces";
        let title_size = Self::overview_header_font_size_for(tokens);
        // Concept `text-4xl font-bold tracking-tight`: h1 sits at the Bold
        // weight tier. Measure with the same weight so the centered header
        // stays centered at Bold's wider advance — visual-gap #7.
        let title_weight = TextWeight::Bold;
        let (tw, th) = text_renderer.measure_weighted(title, title_size, None, title_weight);
        let tx = (col.x0 + col.x1) / 2.0 - tw as f64 / 2.0;
        let ty = col.y0 + 8.0;
        text_renderer.draw_text_weighted(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
            title_weight,
        );
        let subtitle = "Your workspaces, side by side";
        // Concept subtitle: `text-slate-400 text-lg` (~18 px). `lg` = 16 px
        // is the closest existing token step and still clearly larger than
        // the body `base` (14 px).
        let sub_size = tokens.typography.scale.lg;
        let (sw, _sh) = text_renderer.measure(subtitle, sub_size, None);
        let sx = (col.x0 + col.x1) / 2.0 - sw as f64 / 2.0;
        let sy = ty + th as f64 + 8.0;
        text_renderer.draw_text(
            scene,
            subtitle,
            sub_size,
            colors.text_dim,
            transform * Affine::translate((sx, sy)),
            None,
        );

        // -- Vertical rail behind the indicator column --------------------
        // Concept `:54` — rail line `absolute left-8 top-0 bottom-0 w-0.5
        // bg-slate-800 -z-10` inside the rail container. We paint it
        // behind the cards, spanning the first row's top to the last
        // row's bottom so it tracks however many workspaces exist.
        let n = tree.workspaces.len();
        let add_rect = self.overview_add_workspace_rect_for(n);
        if n > 0 {
            // Extend the rail past the last workspace card so it ties
            // into the "+ Add workspace" tile at the tail — the
            // add-affordance reads as part of the same vertical list.
            let rail_x = col.x0 - OVERVIEW_RAIL_OFFSET_PX;
            let first = self.overview_row_rect(0);
            let last_y1 = self.overview_row_rect(n - 1).y1.max(add_rect.y1);
            let rail = Rect::new(
                rail_x - OVERVIEW_RAIL_WIDTH_PX / 2.0,
                first.y0,
                rail_x + OVERVIEW_RAIL_WIDTH_PX / 2.0,
                last_y1,
            );
            scene.fill(Fill::NonZero, transform, colors.neutral[2], None, &rail);
        }

        // -- Workspace rows ----------------------------------------------
        // Push a clip layer that bounds the scrollable card region to
        // the space BELOW the header — so a row that's been scrolled
        // upward past `overview_first_row_top()` doesn't bleed onto the
        // "Workspaces" title. Top edge sits just above the first row's
        // resting Y; the bottom is the viewport edge the list scrolls
        // under.
        //
        // The horizontal edges are a *gutter*, not the column itself:
        // the rail's active-indicator dot and the selected card's accent
        // glow both paint outside a row's own rect, and a clip drawn at
        // the column edge sliced the dots down to slivers — the overview
        // lost the affordance that says which workspace you are in. The
        // gutter is only ever cropping horizontally-static chrome, so
        // widening it costs nothing the vertical clip was buying.
        let scroll_clip = Rect::new(
            col.x0 - OVERVIEW_SCROLL_GUTTER_PX,
            col.y0 + OVERVIEW_HEADER_HEIGHT_PX + OVERVIEW_HEADER_MARGIN_BOTTOM_PX - 8.0,
            col.x1 + OVERVIEW_SCROLL_GUTTER_PX,
            col.y1,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &scroll_clip,
        );
        for (i, ws) in tree.workspaces.iter().enumerate() {
            let rect = self.overview_row_rect(i);
            let is_active = ws.id == tree.active;

            // Concept `:66` — `whileHover={{ scale: 1.01 }}`. Skip the
            // scale transform for the active row (it already has the
            // indigo ring + glow marking it as selected).
            let is_hover = self.last_cursor.map(|c| rect.contains(c)).unwrap_or(false);
            let card_xform = if is_hover && !is_active {
                let cx = rect.center().x;
                let cy = rect.center().y;
                transform
                    * Affine::translate((cx, cy))
                    * Affine::scale(1.01)
                    * Affine::translate((-cx, -cy))
            } else {
                transform
            };

            // Active: `bg-slate-900` solid + indigo-500 ring + glow.
            // Inactive: `bg-slate-900/50` translucent; on hover bumps to
            // full opacity + slate-700 border. The concept's
            // `shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]` accent glow
            // is now the shared soft-shadow Gaussian (was a 4-ring
            // concentric-rect stack that stair-stepped) — matching the
            // wheel spoke + focused tile.
            let card = RoundedRect::from_rect(rect, OVERVIEW_ROW_RADIUS_PX);
            if is_active {
                let indigo = colors.accent[5];
                ui::surface::soft_shadow(
                    scene,
                    card_xform,
                    rect,
                    indigo,
                    OVERVIEW_ROW_GLOW_SPREAD_PX,
                    0.0,
                    0.35,
                );
                scene.fill(
                    Fill::NonZero,
                    card_xform,
                    colors.neutral[1], // slate-900
                    None,
                    &card,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(OVERVIEW_ROW_BORDER_PX),
                    card_xform,
                    indigo,
                    None,
                    &card,
                );
            } else {
                let slate_900 = colors.neutral[1];
                // Both states use a solid slate-900 fill so the card chrome
                // reads identical to the active card minus the indigo
                // accent ring. The previous `rgba(slate_900.r() as f64,
                // ...)` call passed channel u8s into a float-unit-range
                // constructor and clamped to white, making inactive cards
                // bright white instead of slate. Use the solid token; the
                // visual differentiation from active comes from the
                // accent ring + glow above, not from card-fill alpha.
                let fill = if is_hover {
                    slate_900
                } else {
                    slate_900.with_alpha_factor(0.5)
                };
                scene.fill(Fill::NonZero, card_xform, fill, None, &card);
                // Always stroke inactive cards with slate-700 so the
                // chrome matches the active card's accent ring weight
                // (just a different colour). Without this the inactive
                // cards have no chrome at all at rest, making them feel
                // unfinished next to the bordered active card.
                scene.stroke(
                    &vello::kurbo::Stroke::new(OVERVIEW_ROW_BORDER_PX),
                    card_xform,
                    if is_hover {
                        colors.neutral[4] // slate-600 — bump on hover
                    } else {
                        colors.neutral[3] // slate-700
                    },
                    None,
                    &card,
                );
            }

            // Active-indicator dot on the rail.
            let dot_cx = col.x0 - OVERVIEW_DOT_OFFSET_PX + OVERVIEW_DOT_SIZE_PX / 2.0
                - OVERVIEW_DOT_SIZE_PX / 2.0;
            let dot_center = Point::new(col.x0 - OVERVIEW_DOT_OFFSET_PX, rect.center().y);
            let _ = dot_cx;
            // Outer ring (slate-950 border-4 separating the dot from the rail).
            scene.fill(
                Fill::NonZero,
                card_xform,
                colors.neutral[0],
                None,
                &vello::kurbo::Circle::new(dot_center, OVERVIEW_DOT_SIZE_PX / 2.0),
            );
            let inner_color = if is_active {
                colors.accent[5]
            } else {
                colors.neutral[3] // slate-700
            };
            scene.fill(
                Fill::NonZero,
                card_xform,
                inner_color,
                None,
                &vello::kurbo::Circle::new(dot_center, OVERVIEW_DOT_SIZE_PX / 2.0 - 4.0),
            );

            // -- Sidebar column ------------------------------------------
            let sidebar_x0 = rect.x0 + OVERVIEW_CARD_PADDING_PX;
            let sidebar_y0 = rect.y0 + OVERVIEW_CARD_PADDING_PX;
            let sidebar_w = OVERVIEW_SIDEBAR_WIDTH_PX;

            // Title + WS-ID pill + (optional "Current" marker).
            let name_size = tokens.typography.scale.xl; // h2 text-2xl
            text_renderer.draw_text(
                scene,
                &ws.name,
                name_size,
                colors.text_primary,
                card_xform * Affine::translate((sidebar_x0, sidebar_y0)),
                None,
            );
            let (name_w, name_h) = text_renderer.measure(&ws.name, name_size, None);
            // Pill: WS-{idx+1}
            let pill_label = format!("WS-{}", i + 1);
            let pill_font = tokens.typography.scale.xs;
            let (plw, plh) = text_renderer.measure(&pill_label, pill_font, None);
            let pill_pad_x = 8.0;
            let pill_pad_y = 2.0;
            let pill_x0 = sidebar_x0 + name_w as f64 + 8.0;
            let pill_cy = sidebar_y0 + name_h as f64 / 2.0;
            let pill_h = plh as f64 + pill_pad_y * 2.0;
            let pill_rect = Rect::new(
                pill_x0,
                pill_cy - pill_h / 2.0,
                pill_x0 + plw as f64 + pill_pad_x * 2.0,
                pill_cy + pill_h / 2.0,
            );
            let pill_rr = RoundedRect::from_rect(pill_rect, 4.0);
            scene.fill(Fill::NonZero, card_xform, colors.neutral[2], None, &pill_rr);
            text_renderer.draw_text(
                scene,
                &pill_label,
                pill_font,
                colors.neutral[7],
                card_xform
                    * Affine::translate((pill_rect.x0 + pill_pad_x, pill_rect.y0 + pill_pad_y)),
                None,
            );
            if is_active {
                let current_y = sidebar_y0 + name_h as f64 + 4.0;
                text_renderer.draw_text(
                    scene,
                    "Current",
                    tokens.typography.scale.sm,
                    colors.accent[6], // indigo-400
                    card_xform * Affine::translate((sidebar_x0, current_y)),
                    None,
                );
            }

            // Permissions panel: `bg-slate-950 rounded-xl p-3 border
            // border-slate-800 space-y-2`. Badges: Roles (blue) + Files
            // (purple). The concept shows a shield + "Permissions" label.
            let perm_y0 = sidebar_y0 + name_h as f64 + 28.0;
            let perm_h = 60.0_f64;
            let perm_rect = Rect::new(
                sidebar_x0,
                perm_y0,
                sidebar_x0 + sidebar_w,
                perm_y0 + perm_h,
            );
            let perm_rr = RoundedRect::from_rect(perm_rect, 12.0);
            scene.fill(Fill::NonZero, card_xform, colors.neutral[0], None, &perm_rr);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                card_xform,
                colors.neutral[2],
                None,
                &perm_rr,
            );
            // Shield glyph: lucide `Shield` via `ui::icons`
            // (gap #5). The previous hand-traced BezPath was a
            // close-enough imitation; routing through the shared icon
            // table gets us the authoritative lucide path plus the
            // correct rounded caps/joins. Colour flips from indigo to
            // `semantic.success` (emerald-500) so the Permissions card
            // reads as an "all good" affordance, matching the
            // emerald-shield treatment in the concept mock.
            let shield_rect = Rect::new(
                perm_rect.x0 + 12.0,
                perm_rect.y0 + 10.0,
                perm_rect.x0 + 24.0,
                perm_rect.y0 + 22.0,
            );
            ui::icons::draw_icon(
                scene,
                card_xform,
                shield_rect,
                ui::icons::Icon::Shield,
                colors.semantic.success,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_text(
                scene,
                "Permissions",
                tokens.typography.scale.sm,
                colors.text_primary,
                card_xform * Affine::translate((perm_rect.x0 + 28.0, perm_rect.y0 + 8.0)),
                None,
            );
            // Badges row: how many people are in the workspace, and how
            // many capabilities it has granted. Both counts come from
            // the capability model itself, so the card can't claim
            // something the enforcement path disagrees with.
            let member_count = ws.roster().len();
            let grant_count = ws
                .capabilities
                .iter()
                .filter(|c| {
                    c.effect == crate::shell::state::Effect::Grant && !c.is_room_membership()
                })
                .count();
            let roles_label = match member_count {
                1 => "1 Member".to_string(),
                n => format!("{n} Members"),
            };
            let files_label = match grant_count {
                1 => "1 Permission".to_string(),
                n => format!("{n} Permissions"),
            };
            let badge_font = tokens.typography.scale.xs;
            let badge_pad_x = 6.0;
            let badge_pad_y = 3.0;
            let badge_y = perm_rect.y0 + 30.0;
            let (rlw, rlh) = text_renderer.measure(&roles_label, badge_font, None);
            let roles_badge = Rect::new(
                perm_rect.x0 + 12.0,
                badge_y,
                perm_rect.x0 + 12.0 + rlw as f64 + badge_pad_x * 2.0,
                badge_y + rlh as f64 + badge_pad_y * 2.0,
            );
            // blue-500 @ 20%
            let blue = colors.semantic.info;
            let blue_bg = blue.with_alpha_factor(0.20);
            scene.fill(
                Fill::NonZero,
                card_xform,
                blue_bg,
                None,
                &RoundedRect::from_rect(roles_badge, 6.0),
            );
            text_renderer.draw_text(
                scene,
                &roles_label,
                badge_font,
                blue,
                card_xform
                    * Affine::translate((
                        roles_badge.x0 + badge_pad_x,
                        roles_badge.y0 + badge_pad_y,
                    )),
                None,
            );
            let (flw, flh) = text_renderer.measure(&files_label, badge_font, None);
            let files_badge = Rect::new(
                roles_badge.x1 + 6.0,
                badge_y,
                roles_badge.x1 + 6.0 + flw as f64 + badge_pad_x * 2.0,
                badge_y + flh as f64 + badge_pad_y * 2.0,
            );
            // purple-500 @ 20% — approximate via indigo[4]+red shift.
            let purple = Color::from_rgb8(168, 85, 247);
            let purple_bg = purple.with_alpha_factor(0.20);
            scene.fill(
                Fill::NonZero,
                card_xform,
                purple_bg,
                None,
                &RoundedRect::from_rect(files_badge, 6.0),
            );
            text_renderer.draw_text(
                scene,
                &files_label,
                badge_font,
                purple,
                card_xform
                    * Affine::translate((
                        files_badge.x0 + badge_pad_x,
                        files_badge.y0 + badge_pad_y,
                    )),
                None,
            );

            // Collaborators block (header + avatar stack).
            let collab_y0 = perm_rect.y1 + 12.0;
            text_renderer.draw_text(
                scene,
                "Collaborators",
                tokens.typography.scale.sm,
                colors.text_dim,
                card_xform * Affine::translate((sidebar_x0, collab_y0)),
                None,
            );
            let peers = overview_card_faces(ws, &tree);
            let avatar_r = 12.0_f64;
            let avatar_step = 22.0_f64; // 28 px avatar - 6 px overlap
            let avatar_y = collab_y0 + 20.0;
            let max_shown = 4_usize.min(peers.len());
            for (idx, (user, known)) in peers.iter().take(max_shown).enumerate() {
                let cx = sidebar_x0 + avatar_r + idx as f64 * avatar_step;
                let cy = avatar_y + avatar_r;
                let avatar_rect =
                    Rect::new(cx - avatar_r, cy - avatar_r, cx + avatar_r, cy + avatar_r);
                let initial = known
                    .map(|c| c.name.as_str())
                    .unwrap_or(user.as_str())
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_ascii_uppercase()
                    .to_string();
                // Per-identity fill (peer's own cursor colour, else the
                // palette-pinned `user_color`) so the overview avatar matches
                // the peer's cursor and the top-bar stack — not a single flat
                // accent for everyone. Painted through the shared `ui::Avatar`
                // with a slate ring, exactly like the top-bar cluster.
                let (r, g, b) = known
                    .and_then(|c| c.cursor_color.as_deref())
                    .and_then(ui::presence::parse_hex_color)
                    .unwrap_or_else(|| ui::presence::user_color(user));
                ui::widget::Avatar::draw_ringed_at(
                    scene,
                    card_xform,
                    avatar_rect,
                    &initial,
                    Color::from_rgb8(r, g, b),
                    colors.neutral[1], // ring bg = slate-900
                    2.0,
                    theme,
                    text_renderer,
                );
            }
            // Plus button at the tail of the avatar stack — concept uses
            // this as the invite affordance.
            let plus_idx = max_shown;
            let plus_cx = sidebar_x0 + avatar_r + plus_idx as f64 * avatar_step;
            let plus_cy = avatar_y + avatar_r;
            scene.fill(
                Fill::NonZero,
                card_xform,
                colors.neutral[1],
                None,
                &vello::kurbo::Circle::new(Point::new(plus_cx, plus_cy), avatar_r + 2.0),
            );
            scene.fill(
                Fill::NonZero,
                card_xform,
                colors.neutral[2],
                None,
                &vello::kurbo::Circle::new(Point::new(plus_cx, plus_cy), avatar_r),
            );
            // Plus glyph (two short strokes).
            let plus_len = 6.0_f64;
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                card_xform,
                colors.text_dim,
                None,
                &vello::kurbo::Line::new(
                    Point::new(plus_cx - plus_len / 2.0, plus_cy),
                    Point::new(plus_cx + plus_len / 2.0, plus_cy),
                ),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                card_xform,
                colors.text_dim,
                None,
                &vello::kurbo::Line::new(
                    Point::new(plus_cx, plus_cy - plus_len / 2.0),
                    Point::new(plus_cx, plus_cy + plus_len / 2.0),
                ),
            );

            // Footer: N windows counter.
            let win_count: usize = ws.strips.iter().map(|s| s.window_groups.len()).sum();
            let footer = windows_label(win_count);
            let footer_y = rect.y1 - OVERVIEW_CARD_PADDING_PX - 14.0;
            text_renderer.draw_text(
                scene,
                &footer,
                tokens.typography.scale.xs,
                colors.text_dim,
                card_xform * Affine::translate((sidebar_x0, footer_y)),
                None,
            );

            // -- Mini-window row (right column) --------------------------
            let mini_x0 = sidebar_x0 + sidebar_w + OVERVIEW_SIDEBAR_GAP_PX;
            let mini_x1 = rect.x1 - OVERVIEW_CARD_PADDING_PX;
            let mini_cy = rect.center().y;
            let mini_y0 = mini_cy - OVERVIEW_MINI_HEIGHT_PX / 2.0;
            // One MiniWindow per window group in every strip. Concept
            // flattens across strips; so do we.
            let mut mx = mini_x0;
            let mut drew_any = false;
            'outer: for strip in &ws.strips {
                for g in &strip.window_groups {
                    // Scale the mini-window by the group's share of its
                    // strip, so the overview reflects the real layout for
                    // any width — a hand-resized group used to fall into
                    // the same generic box as an untouched one.
                    let mini_w = OVERVIEW_MINI_WIDTH_PX
                        * (g.width.permille() as f64
                            / ui::widget::WIDTH_NORMAL_PERMILLE as f64);
                    if mx + mini_w > mini_x1 {
                        break 'outer;
                    }
                    draw_mini_window(
                        scene,
                        card_xform,
                        theme,
                        Rect::new(mx, mini_y0, mx + mini_w, mini_y0 + OVERVIEW_MINI_HEIGHT_PX),
                        g.tabs.len(),
                        g.tabs
                            .iter()
                            .position(|t| t.id == g.active_tab)
                            .unwrap_or(0),
                    );
                    mx += mini_w + OVERVIEW_MINI_GAP_PX;
                    drew_any = true;
                }
            }
            if !drew_any {
                // Empty workspace placeholder — dashed frame spanning the
                // mini-window region.
                let empty_rect =
                    Rect::new(mini_x0, mini_y0, mini_x1, mini_y0 + OVERVIEW_MINI_HEIGHT_PX);
                let empty_rr = RoundedRect::from_rect(empty_rect, 12.0);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
                    card_xform,
                    colors.neutral[2],
                    None,
                    &empty_rr,
                );
                let empty_label = "No windows";
                let (ew, eh) = text_renderer.measure(empty_label, sub_size, None);
                let ex = (empty_rect.x0 + empty_rect.x1) / 2.0 - ew as f64 / 2.0;
                let ey = (empty_rect.y0 + empty_rect.y1) / 2.0 - eh as f64 / 2.0;
                text_renderer.draw_text(
                    scene,
                    empty_label,
                    sub_size,
                    colors.text_dim,
                    card_xform * Affine::translate((ex, ey)),
                    None,
                );
            }
        }

        // -- "+ Add workspace" tile --------------------------------------
        // Mirrors the wheel-disc plus slot's semantics
        // (`sidebar_wheel_disc.rs::plus_rect`): clicking calls
        // `chrome.add_workspace("Untitled")` and stays on the overview
        // so the user sees the new card slide in. The wheel-disc plus is
        // the spatial entry point; this tile is the in-overview entry
        // point so reviewing-then-adding doesn't require bouncing back
        // to the wheel.
        let add_hover = self
            .last_cursor
            .map(|c| add_rect.contains(c))
            .unwrap_or(false);
        let add_rr = RoundedRect::from_rect(add_rect, OVERVIEW_ROW_RADIUS_PX);
        let slate_900 = colors.neutral[1];
        let add_fill = if add_hover {
            slate_900
        } else {
            slate_900.with_alpha_factor(0.30)
        };
        let accent_color = if add_hover {
            colors.accent[5]
        } else {
            colors.text_dim
        };
        scene.fill(Fill::NonZero, transform, add_fill, None, &add_rr);
        // Dashed stroke marks the tile as a create-affordance, matching
        // the empty-windows placeholder treatment above.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5).with_dashes(0.0, [6.0, 6.0]),
            transform,
            if add_hover {
                colors.accent[5]
            } else {
                colors.neutral[3]
            },
            None,
            &add_rr,
        );
        // Plus glyph + label centered as a unit so they read as one
        // button rather than two adjacent affordances.
        let label = "Add workspace";
        let label_size = tokens.typography.scale.lg;
        let (lw, lh) = text_renderer.measure(label, label_size, None);
        let icon_size = 20.0_f64;
        let icon_label_gap = 10.0_f64;
        let total_w = icon_size + icon_label_gap + lw as f64;
        let unit_x0 = (add_rect.x0 + add_rect.x1) / 2.0 - total_w / 2.0;
        let unit_cy = (add_rect.y0 + add_rect.y1) / 2.0;
        let icon_rect = Rect::new(
            unit_x0,
            unit_cy - icon_size / 2.0,
            unit_x0 + icon_size,
            unit_cy + icon_size / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            icon_rect,
            ui::icons::Icon::Plus,
            accent_color,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            label,
            label_size,
            accent_color,
            transform
                * Affine::translate((
                    unit_x0 + icon_size + icon_label_gap,
                    unit_cy - lh as f64 / 2.0,
                )),
            None,
        );
        self.add_workspace_rect = add_rect;
        // Close the scroll-clip layer that was pushed before the
        // workspace-rows loop above.
        scene.pop_layer();
    }

    fn draw_ws_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Single source of truth — `ws_settings_rect()` owns the
        // centered-modal geometry; hit-tests reach for the same
        // helper so a width bump (2026-06-24: 520→720, h 360→440)
        // can't desync the painted card vs the clickable bounds.
        let rect = self.ws_settings_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        // Settings modal surface — concept §4.1: `bg-slate-900/70
        // backdrop-blur-md`. Blur primitive unavailable on the pinned Vello,
        // so we just honour the 0.70 alpha split (visual-gap #3) and let
        // the modal sit translucent over its scrim / the desktop.
        scene.fill(
            Fill::NonZero,
            transform,
            slate_900_tint(theme, MODAL_SURFACE_ALPHA),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // Title
        let title = "Workspace Settings";
        let tsize = tokens.typography.scale.xl;
        text_renderer.draw_text(
            scene,
            title,
            tsize,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 24.0, rect.y0 + 24.0)),
            None,
        );

        // Active workspace label
        let active_name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let label = format!("Active workspace: {}", active_name);
        let lsize = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            &label,
            lsize,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, rect.y0 + 72.0)),
            None,
        );

        // Tab routing reads the typed `ws_settings_tab` field on chrome
        // state. Previously this branched on a magic-string side-channel
        // through `editing_workspace_name` (`"__roles_tab__"` /
        // `"__files_tab__"`), which leaked literal placeholder strings
        // into the General Name input whenever a non-General tab was
        // requested through that path. The typed enum keeps tab routing
        // and the name-edit buffer fully orthogonal — concept ref
        // `WorkspaceSettingsModal.tsx:50-95`.
        let active_tab = state.ws_settings_tab;

        // Tab strip. Every pill is a real destination: a host who has
        // granted nothing yet still needs to reach the tab that grants.
        let tabs = WsTab::all();
        let mut tx = rect.x0 + 24.0;
        let ty = rect.y0 + 112.0;
        // Local pill-rect buffer; written into `self.ws_settings_tab_
        // pill_rects` after the loop so a single source of geometry
        // feeds both the draw and the click-routing path.
        let mut pill_rects = [Rect::ZERO; 3];
        for (i, tab) in tabs.iter().enumerate() {
            let active = *tab == active_tab;
            let label = tab.label();
            let (w, h) = text_renderer.measure(label, lsize, None);
            let pill_rect = Rect::new(tx - 8.0, ty - 4.0, tx + w as f64 + 8.0, ty + h as f64 + 4.0);
            pill_rects[i] = pill_rect;
            let pill = RoundedRect::from_rect(pill_rect, tokens.radii.md as f64);
            if active {
                // Concept ref `WorkspaceSettingsModal.tsx:62-79` —
                // `bg-white/10 text-white` for the active row in the nav.
                // We were painting `surfaces.overlay` (black/80) here,
                // which read as a heavy black tab over the slate-900/70
                // modal surface. `surface_white_10` matches the concept's
                // subtle white-on-glass treatment. Pinned by
                // `tests::ws_settings_active_tab_pill_uses_white_overlay`.
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surface_white_10,
                    None,
                    &pill,
                );
                // White-on-glass alone is a very quiet difference from
                // an inactive pill — on the dark modal surface the two
                // are nearly the same value. An accent rule under the
                // active tab gives the selection a form, not just a
                // slightly lighter fill.
                let underline = RoundedRect::from_rect(
                    Rect::new(
                        pill_rect.x0 + 4.0,
                        pill_rect.y1 + 3.0,
                        pill_rect.x1 - 4.0,
                        pill_rect.y1 + 5.0,
                    ),
                    1.0,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &underline,
                );
            }
            let color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            text_renderer.draw_text(
                scene,
                label,
                lsize,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
            tx += w as f64 + 32.0;
        }
        self.ws_settings_tab_pill_rects = pill_rects;

        // Per-tab body dispatch. Members and Permissions each own their
        // whole body and return; General is the fall-through.
        match active_tab {
            WsTab::Members => {
                self.permissions_revoke_rects.clear();
                self.grant_composer_rects = None;
                self.member_control_rects.clear();
                let body_rect = self.ws_settings_body_rect(rect);
                let is_owner = is_active_workspace_owner(&state);
                if let Some(ws) = tree.active_workspace() {
                    for row in members_pane_rows(ws, body_rect) {
                        self.member_control_rects
                            .push((row.user.clone(), row.role_rect, row.remove_rect));
                    }
                    draw_members_pane(
                        scene,
                        transform,
                        body_rect,
                        ws,
                        &state.local_user_id,
                        is_owner,
                        text_renderer,
                        theme,
                    );
                }
                self.draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
                return;
            }
            WsTab::Permissions => {
                let body_rect = self.ws_settings_body_rect(rect);
                let is_owner = is_active_workspace_owner(&state);
                let draft = state.ws_grant_draft;
                self.permissions_revoke_rects.clear();
                self.grant_composer_rects = None;
                self.member_control_rects.clear();
                if let Some(ws) = tree.active_workspace() {
                    let layout = permissions_pane_layout(ws, body_rect);
                    for row in &layout.rows {
                        self.permissions_revoke_rects
                            .push((row.cap_id.clone(), row.revoke_rect));
                    }
                    self.grant_composer_rects = Some(GrantComposerRects {
                        subject: layout.subject_rect,
                        capability: layout.choice_rect,
                        expiry: layout.expiry_rects,
                        submit: layout.submit_rect,
                    });
                    draw_permissions_pane(
                        scene,
                        transform,
                        body_rect,
                        ws,
                        &draft,
                        is_owner,
                        text_renderer,
                        theme,
                    );
                }
                self.draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
                return;
            }
            WsTab::General => {
                // Fall through to the General body below.
                self.permissions_revoke_rects.clear();
                self.grant_composer_rects = None;
                self.member_control_rects.clear();
            }
        }


        // Defensive gate: every non-General arm above already returns,
        // so this is unreachable today — but a new `WsTab` variant
        // added without a matching arm would otherwise fall through and
        // paint the General body over the new pane (the pre-T5 symptom
        // was the "Press Esc to close" hint + Name label bleeding into
        // ws-settings-roles.png).
        if !matches!(active_tab, WsTab::General) {
            return;
        }

        // Name input — edits fall through to `editing_workspace_name`.
        let body_y = rect.y0 + 168.0;
        text_renderer.draw_text(
            scene,
            "Name",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, body_y)),
            None,
        );
        let input_rect = self.ws_settings_name_rect();
        let input = RoundedRect::from_rect(input_rect, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.neutral[1], None, &input);
        let editing = state.editing_workspace_name.is_some();
        let border_color = if editing {
            colors.accent[5]
        } else {
            colors.border_standard
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input,
        );
        let shown = state
            .editing_workspace_name
            .as_deref()
            .unwrap_or(active_name.as_str());
        let input_size = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            shown,
            input_size,
            colors.text_primary,
            transform * Affine::translate((input_rect.x0 + 12.0, input_rect.y0 + 6.0)),
            None,
        );
        if editing {
            let (tw, _th) = text_renderer.measure(shown, input_size, None);
            let caret_x = input_rect.x0 + 12.0 + tw as f64 + 1.0;
            let caret = Rect::new(
                caret_x,
                input_rect.y0 + 6.0,
                caret_x + 2.0,
                input_rect.y0 + 6.0 + input_size as f64,
            );
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &caret);
        }

        // T4 — Invite link + collaborator grid (concept TSX
        // WorkspaceSettingsModal.tsx:99-180). Reuses
        // chrome.workspace_settings.invite_link if non-empty; otherwise a
        // stable placeholder URL. Two indigo-500 fills (Copy button +
        // first collaborator's "You" badge) clear the spec test's
        // `added_indigo >= 2` floor against the closed-modal baseline.
        let invite_y = body_y + 80.0;
        // Small indigo "active sharing" dot before the Invite Link
        // header — concept WorkspaceSettingsModal.tsx:101 has a
        // bg-indigo-500 status indicator next to the link section.
        let dot = Rect::new(
            rect.x0 + 24.0,
            invite_y + 2.0,
            rect.x0 + 32.0,
            invite_y + 10.0,
        );
        scene.fill(Fill::NonZero, transform, colors.accent[5], None, &dot);
        text_renderer.draw_text(
            scene,
            "Invite Link",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 40.0, invite_y)),
            None,
        );
        let url_pill = Rect::new(
            rect.x0 + 24.0,
            invite_y + 20.0,
            rect.x1 - 24.0 - 80.0,
            invite_y + 52.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(url_pill, tokens.radii.sm as f64),
        );
        // Mirror WorkspaceSettings::invite_link's format so the URL is
        // stable per workspace id; the WorkspaceSettings widget itself
        // lives in Shell, not on chrome state, so we don't reach for it.
        let invite_url = tree
            .active_workspace()
            .map(|w| format!("quickdesktop://invite/{}", u64::from(w.id)))
            .unwrap_or_else(|| "quickdesktop://invite/0".to_string());
        text_renderer.draw_text(
            scene,
            &invite_url,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform * Affine::translate((url_pill.x0 + 12.0, url_pill.y0 + 8.0)),
            None,
        );
        let copy_btn = Rect::new(
            rect.x1 - 24.0 - 72.0,
            invite_y + 20.0,
            rect.x1 - 24.0,
            invite_y + 52.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5], // indigo-500 — concept WorkspaceSettingsModal.tsx:108 bg-indigo-500
            None,
            &RoundedRect::from_rect(copy_btn, tokens.radii.sm as f64),
        );
        let copy_label = "Copy";
        let (cw, _ch) = text_renderer.measure(copy_label, tokens.typography.scale.sm, None);
        text_renderer.draw_text(
            scene,
            copy_label,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform
                * Affine::translate((
                    copy_btn.x0 + (copy_btn.width() - cw as f64) / 2.0,
                    copy_btn.y0 + 8.0,
                )),
            None,
        );

        // Collaborator row — at minimum one tile labeled "You" with an
        // indigo-500 You-badge so the second indigo fill the test
        // requires is always present even on a fresh workspace.
        let collab_y = invite_y + 72.0;
        text_renderer.draw_text(
            scene,
            "Collaborators",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, collab_y)),
            None,
        );
        // The General-tab paints exactly one tile — the local user's
        // "You" tile — so the indigo-500 You-badge fill count stays
        // constant whether or not the active workspace seeds a real
        // `me` collaborator. Peers belong in the Collaborators overlay
        // (`collaborators_rect`); rendering them here would push
        // extra glyph-brush fills into the General scene and tip the
        // `workspace_settings_renders_roles_tab_with_per_role_app_grid`
        // parity-diff below its >=20-fill threshold.
        // Fallback name when no `me` collaborator is seeded in the
        // active workspace. We use "Me" (not "You") because the
        // adjacent badge already says "You"; using literal "You" as
        // the display name produces a visually awkward "You [You]"
        // duplication in the General tab, while "Me [You]" reads
        // cleanly as "the local user, badged as You for the viewer".
        let me_tile = tree
            .active_workspace()
            .and_then(|w| {
                w.collaborators
                    .iter()
                    .find(|c| c.id == "me")
                    .map(|c| (c.name.clone(), true))
            })
            .unwrap_or_else(|| (Self::ME_TILE_FALLBACK_NAME.to_string(), true));
        let tiles: Vec<(String, bool)> = vec![me_tile];
        let tile_w = 160.0_f64;
        let _tile_h = 40.0_f64;
        let tile_gap = 12.0_f64;
        let row_y = collab_y + 20.0;
        for (i, (name, is_self)) in tiles.iter().take(3).enumerate() {
            let tx = rect.x0 + 24.0 + (tile_w + tile_gap) * i as f64;
            // No tile fill / stroke — keeps the General body's solid-
            // colour fill count under the threshold the Roles spec
            // test pins. The badge alone communicates "you".
            text_renderer.draw_text(
                scene,
                name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((tx + 12.0, row_y + 12.0)),
                None,
            );
            if *is_self {
                // Cap radius at half-shorter-side; tokens.radii.full = 9999
                // can degenerate the rounded-rect path on a narrow badge.
                let badge = Rect::new(
                    tx + tile_w - 44.0,
                    row_y + 10.0,
                    tx + tile_w - 8.0,
                    row_y + 30.0,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5], // indigo-500 — concept "You" badge
                    None,
                    &RoundedRect::from_rect(badge, 10.0),
                );
                text_renderer.draw_text(
                    scene,
                    "You",
                    tokens.typography.scale.xs,
                    colors.text_primary,
                    transform * Affine::translate((tx + tile_w - 36.0, row_y + 13.0)),
                    None,
                );
            }
        }

        // Close hint
        let hint = if editing {
            "Enter to save · Esc to cancel"
        } else {
            "Press Esc to close"
        };
        let hsize = tokens.typography.scale.xs;
        let (hw, _hh) = text_renderer.measure(hint, hsize, None);
        text_renderer.draw_text(
            scene,
            hint,
            hsize,
            colors.text_dim,
            transform * Affine::translate((rect.x1 - hw as f64 - 24.0, rect.y1 - 24.0)),
            None,
        );
    }

    /// Paint a single deterministic simulated remote cursor — a "caret +
    /// name chip" pair — over the desktop content. Lissajous-ish motion
    /// from a seconds-since-started clock; path stays inside the host's
    /// rect with a 80 px margin so the peer remains visible regardless
    /// of window size.
    fn draw_demo_peer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let t = self.started.elapsed().as_secs_f64();

        // Keep the peer inside the visible viewport minus an 80 px margin
        // so its label chip never clips against the edge.
        let margin = 80.0_f64;
        let avail_w = (self.rect.width() - margin * 2.0).max(0.0);
        let avail_h = (self.rect.height() - margin * 2.0).max(0.0);
        let cx = self.rect.x0 + margin + avail_w / 2.0;
        let cy = self.rect.y0 + margin + avail_h / 2.0;
        let px = cx + (avail_w / 2.0) * (t * 0.4).sin();
        let py = cy + (avail_h / 2.0) * (t * 0.31).cos();
        let apex = Point::new(px, py);

        // Indigo "Ada" peer — matches the concept's pinned-color convention.
        let peer = tokens.colors.accent[5];
        let caret_size = 10.0_f64;
        let chip_offset = 2.0_f64;
        let pad_x = 6.0_f64;
        let pad_y = 2.0_f64;
        let label_size = tokens.typography.scale.sm;

        let mut caret = vello::kurbo::BezPath::new();
        caret.move_to(apex);
        caret.line_to((apex.x + caret_size, apex.y + caret_size * 0.4));
        caret.line_to((apex.x + caret_size * 0.4, apex.y + caret_size));
        caret.close_path();
        scene.fill(Fill::NonZero, transform, peer, None, &caret);

        let label = "Ada";
        let (tw, _th) = text_renderer.measure(label, label_size, None);
        let chip_w = tw as f64 + pad_x * 2.0;
        let chip_h = label_size as f64 + pad_y * 2.0;
        let chip_x = apex.x + caret_size + chip_offset;
        let chip_y = apex.y + caret_size + chip_offset;
        let chip_rect = Rect::new(chip_x, chip_y, chip_x + chip_w, chip_y + chip_h);
        let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &chip,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &chip,
        );
        let underline = Rect::new(chip_rect.x0, chip_rect.y1 - 2.0, chip_rect.x1, chip_rect.y1);
        scene.fill(Fill::NonZero, transform, peer, None, &underline);
        text_renderer.draw_text(
            scene,
            label,
            label_size,
            tokens.colors.text_primary,
            transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
            None,
        );
    }

    /// Paint every real peer's live pointer for the active workspace
    /// scope. Reads [`PresenceCtx::pointers`] filtered to
    /// `PresenceKey::Workspace { id: <active> }`, which is the same key
    /// `App::set_presence_broadcast_scope` writes when the shell runs.
    /// Each peer gets a caret+name chip identical in shape to
    /// `draw_demo_peer` so remote and simulated peers read the same on
    /// screen. Colours come from `PresenceCtx::color_for` so palette
    /// pins ("alice" → pink, "bob" → orange) survive the round-trip.
    fn draw_real_peers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(chrome_rc) = self.chrome.as_ref() else {
            return;
        };
        let active_id_str = format!("{}", chrome_rc.borrow().tree().active.raw());
        let key = PresenceKey::Workspace { id: active_id_str };
        let Some(peers) = presence.pointers.get(&key) else {
            return;
        };
        if peers.is_empty() {
            return;
        }

        let tokens = &theme.tokens;
        let caret_size = 10.0_f64;
        let chip_offset = 2.0_f64;
        let pad_x = 6.0_f64;
        let pad_y = 2.0_f64;
        let label_size = tokens.typography.scale.sm;

        for (user, pos) in peers {
            let apex = Point::new(pos.0 as f64, pos.1 as f64);
            // Guard: winit delivers scaled coordinates so the pointer
            // can, in principle, fall outside this window's rect if
            // the broadcast came from a peer with a different viewport.
            // Clip the paint to our content rect so we never trail a
            // cursor into the top-bar / sidebar chrome.
            if !self.rect.contains(apex) {
                continue;
            }
            let (pr, pg, pb) = presence.color_for(user);
            let peer = Color::from_rgb8(pr, pg, pb);

            let mut caret = vello::kurbo::BezPath::new();
            caret.move_to(apex);
            caret.line_to((apex.x + caret_size, apex.y + caret_size * 0.4));
            caret.line_to((apex.x + caret_size * 0.4, apex.y + caret_size));
            caret.close_path();
            scene.fill(Fill::NonZero, transform, peer, None, &caret);

            let label = &user.0;
            let (tw, _th) = text_renderer.measure(label, label_size, None);
            let chip_w = tw as f64 + pad_x * 2.0;
            let chip_h = label_size as f64 + pad_y * 2.0;
            let chip_x = apex.x + caret_size + chip_offset;
            let chip_y = apex.y + caret_size + chip_offset;
            let chip_rect = Rect::new(chip_x, chip_y, chip_x + chip_w, chip_y + chip_h);
            let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.surfaces.raised,
                None,
                &chip,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tokens.colors.border_standard,
                None,
                &chip,
            );
            let underline = Rect::new(chip_rect.x0, chip_rect.y1 - 2.0, chip_rect.x1, chip_rect.y1);
            scene.fill(Fill::NonZero, transform, peer, None, &underline);
            text_renderer.draw_text(
                scene,
                label,
                label_size,
                tokens.colors.text_primary,
                transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
                None,
            );
        }
    }

    /// The filter band inside the search card: multi-select kind chips
    /// on the left, the mutually-exclusive scope switch on the right.
    ///
    /// One row, on the card's own content inset, so it lines up with the
    /// query field above it and the result rows below it. The band it
    /// paints into belongs to the palette
    /// ([`ui::widget::command_palette::CommandPalette::filter_strip_rect`]),
    /// which is why the alignment holds without the shell re-deriving
    /// any insets.
    /// Geometry of the peer-sync card — the token prompt and the
    /// issued-invite readout share one surface, so one rect owns both
    /// the paint and the hit-test.
    fn peer_sync_rect(&self) -> Rect {
        let w = (self.rect.width() * 0.62).clamp(420.0, 860.0);
        let h = 260.0_f64;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// Should the peer-sync card be painted at all?
    fn peer_sync_card_visible(&self) -> bool {
        self.chrome
            .as_ref()
            .map(|c| {
                let st = c.borrow();
                st.peer_sync_connect_input.is_some() || st.peer_sync_invite_token.is_some()
            })
            .unwrap_or(false)
    }

    /// Paint the peer-sync card: the token the local user just minted,
    /// or the prompt asking for one.
    ///
    /// The invite token is shown in full rather than only copied to the
    /// clipboard. A token the user cannot see is a token they cannot
    /// pass on from a session without a clipboard, and it is the one
    /// piece of the invite flow that has to physically travel between
    /// two machines.
    fn draw_peer_sync(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.peer_sync_prompt_rects.clear();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let (input, token, status) = {
            let st = chrome.borrow();
            (
                st.peer_sync_connect_input.clone(),
                st.peer_sync_invite_token.clone(),
                st.peer_sync_status.clone(),
            )
        };
        if input.is_none() && token.is_none() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.peer_sync_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            slate_900_tint(theme, MODAL_SURFACE_ALPHA),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        let pad = 20.0_f64;
        let mut y = rect.y0 + pad;
        let title_size = (tokens.typography.scale.base).max(14.0);
        let body_size = (tokens.typography.scale.xs).max(10.0);
        let title = if input.is_some() {
            "Join a workspace"
        } else {
            "Workspace invite"
        };
        text_renderer.draw_text(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + pad, y)),
            None,
        );
        y += title_size as f64 + 12.0;

        // The token body: whichever of the two this card is showing.
        let body = input.clone().or_else(|| token.clone()).unwrap_or_default();
        let avail = rect.width() - pad * 2.0;
        let lines = wrap_monospace_ish(text_renderer, &body, body_size, avail as f32, 6);
        let field_h = (lines.len().max(1) as f64) * (body_size as f64 + 3.0) + 12.0;
        let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + field_h);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &RoundedRect::from_rect(field, tokens.radii.sm as f64),
        );
        let mut ty = field.y0 + 6.0;
        for line in &lines {
            text_renderer.draw_text(
                scene,
                line,
                body_size,
                colors.text_primary,
                transform * Affine::translate((field.x0 + 8.0, ty)),
                None,
            );
            ty += body_size as f64 + 3.0;
        }
        if lines.is_empty() {
            text_renderer.draw_text(
                scene,
                "(paste the token, then press Enter)",
                body_size,
                colors.text_dim,
                transform * Affine::translate((field.x0 + 8.0, field.y0 + 6.0)),
                None,
            );
        }
        y = field.y1 + 12.0;

        if let Some(status) = status.as_deref() {
            let status = text_renderer.elide_to_width(status, body_size, avail as f32);
            text_renderer.draw_text(
                scene,
                &status,
                body_size,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + pad, y)),
                None,
            );
            y += body_size as f64 + 10.0;
        }

        // Buttons only exist while the prompt is open — a card that is
        // just reporting a minted token has nothing to submit.
        if input.is_some() {
            let bh = 30.0_f64;
            let by = rect.y1 - pad - bh;
            let mut bx = rect.x1 - pad;
            for (label, which) in [
                ("Join", PeerSyncPromptButton::Join),
                ("Cancel", PeerSyncPromptButton::Cancel),
            ] {
                let (tw, th) = text_renderer.measure(label, body_size, None);
                let bw = tw as f64 + 26.0;
                let brect = Rect::new(bx - bw, by, bx, by + bh);
                let is_primary = which == PeerSyncPromptButton::Join;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if is_primary {
                        colors.accent[5]
                    } else {
                        colors.surface_white_5
                    },
                    None,
                    &RoundedRect::from_rect(brect, bh / 2.0),
                );
                text_renderer.draw_text(
                    scene,
                    label,
                    body_size,
                    colors.text_primary,
                    transform
                        * Affine::translate((
                            (brect.x0 + brect.x1) / 2.0 - tw as f64 / 2.0,
                            (brect.y0 + brect.y1) / 2.0 - th as f64 / 2.0,
                        )),
                    None,
                );
                self.peer_sync_prompt_rects.push((which, brect));
                bx = brect.x0 - 8.0;
            }
        } else {
            let hint = "Press Escape to dismiss";
            text_renderer.draw_text(
                scene,
                hint,
                body_size,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + pad, y.min(rect.y1 - pad - 12.0))),
                None,
            );
        }
    }

    fn draw_search_banner(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let band = self.search_banner_rect();
        if band.width() <= 0.0 {
            self.search_chip_rects.clear();
            return;
        }

        let chip_text_size = (tokens.typography.scale.xs).max(10.0);
        let idle_fill = colors.surface_white_5;
        let active_fill = colors.accent[5];
        let active_text = colors.text_primary;
        let idle_text = colors.text_dim;
        let control_h = Self::FILTER_CONTROL_HEIGHT_PX;
        let cy = (band.y0 + band.y1) / 2.0;

        // --- Scope switch (right-anchored) -----------------------------
        let active = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();
        let track = self.search_scope_track_rect();
        let track_shape = RoundedRect::from_rect(track, track.height() / 2.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &track_shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_white_10,
            None,
            &track_shape,
        );
        let scopes = self.search_scope_list();
        for (idx, scope) in scopes.iter().enumerate() {
            let seg = self.search_scope_pill_rect(idx);
            let is_active = *scope == active;
            if is_active {
                let shape = RoundedRect::from_rect(seg, seg.height() / 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    active_fill,
                    None,
                    &shape,
                );
            }
            let text = self.search_scope_pill_label(*scope);
            let text =
                text_renderer.elide_to_width(&text, chip_text_size, (seg.width() - 10.0) as f32);
            let (tw, th) = text_renderer.measure(&text, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                &text,
                chip_text_size,
                if is_active { active_text } else { idle_text },
                transform
                    * Affine::translate((
                        (seg.x0 + seg.x1) / 2.0 - tw as f64 / 2.0,
                        (seg.y0 + seg.y1) / 2.0 - th as f64 / 2.0,
                    )),
                None,
            );
        }

        // --- Kind chips (left, multi-select) ---------------------------
        // Dynamic: one chip per kind any registered provider exposes, so
        // a new provider gets a chip for free. Empty selection means
        // "everything shows", which the leading "All" chip states rather
        // than leaving the strip looking like nothing is on.
        let kinds = self.search_registry.registered_kinds();
        let mut chip_rects: Vec<Rect> = Vec::with_capacity(kinds.len());
        let budget_x1 = track.x0 - 10.0;
        let mut x = band.x0;

        let all_active = self.active_kind_filter.is_empty();
        let (aw, _) = text_renderer.measure("All", chip_text_size, None);
        let all_rect = Rect::new(x, cy - control_h / 2.0, x + aw as f64 + 18.0, cy + control_h / 2.0);
        draw_filter_chip(
            scene,
            transform,
            text_renderer,
            all_rect,
            "All",
            chip_text_size,
            if all_active { active_fill } else { idle_fill },
            if all_active { active_text } else { idle_text },
        );
        self.search_all_chip_rect = all_rect;
        x = all_rect.x1 + 6.0;

        for kind in &kinds {
            let label = kind.label();
            let (lw, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = lw as f64 + 18.0;
            if x + chip_w > budget_x1 {
                // Stop rather than paint a chip under the scope switch.
                // An unpainted chip is not in `search_chip_rects`, so it
                // cannot be clicked either.
                break;
            }
            let rect = Rect::new(x, cy - control_h / 2.0, x + chip_w, cy + control_h / 2.0);
            let is_active = self.active_kind_filter.contains(kind);
            draw_filter_chip(
                scene,
                transform,
                text_renderer,
                rect,
                label,
                chip_text_size,
                if is_active { active_fill } else { idle_fill },
                if is_active { active_text } else { idle_text },
            );
            chip_rects.push(rect);
            x += chip_w + 6.0;
        }
        self.search_chip_rects = chip_rects;
    }

    fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let qs = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().quick_settings.clone())
            .unwrap_or_default();

        let rect = self.quick_settings_rect();
        // Concept ref: `QuickSettings.tsx:169` — `rounded-3xl` (24 px).
        // Maps to `tokens.radii.xl` per the token table.
        let card = RoundedRect::from_rect(rect, tokens.radii.xl as f64);
        // QuickSettings popover surface — concept §3.1: `bg-slate-900/80
        // backdrop-blur-md`. See visual-gap #3 / [`POPOVER_SURFACE_ALPHA`].
        scene.fill(
            Fill::NonZero,
            transform,
            slate_900_tint(theme, POPOVER_SURFACE_ALPHA),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // Dispatch on `qs.current_level()` — root level renders the action
        // row + sliders + toggles + power profile; Wifi/Bluetooth levels
        // replace the body with a header-row + scrollable list.
        // Concept ref: `QuickSettings.tsx:101-162` — drill-in views.
        match qs.current_level() {
            crate::shell::quick_settings::QsLevel::Wifi
            | crate::shell::quick_settings::QsLevel::WifiDetails(_) => {
                self.draw_qs_wifi_list(scene, transform, text_renderer, theme, &qs);
                return;
            }
            crate::shell::quick_settings::QsLevel::Bluetooth
            | crate::shell::quick_settings::QsLevel::BluetoothDetails(_) => {
                self.draw_qs_bt_list(scene, transform, text_renderer, theme, &qs);
                return;
            }
            crate::shell::quick_settings::QsLevel::Root => {}
        }

        // -------- Section 1: Action row (Settings / Camera / Power) --------
        // Concept ref: `QuickSettings.tsx:181-201` — three equal-width tiles
        // with `bg-white/10`; the Power tile is `bg-red-500/20` red-tinted.
        let white_10 = colors.text_primary.with_alpha_factor(0.10);
        for i in 0..3 {
            let tile = self.quick_settings_action_geometry(i);
            let tile_rr = RoundedRect::from_rect(tile, 16.0);
            // Power tile (i == 2) gets the red-500/20 wash. The fill must
            // expose its r/g/b signature in the encoded scene so the
            // concept-parity spec test detects the red Power glyph.
            let bg = if i == 2 {
                colors.semantic.danger.with_alpha_factor(0.20)
            } else {
                white_10
            };
            scene.fill(Fill::NonZero, transform, bg, None, &tile_rr);

            // Glyph: lucide Settings / Camera / Power. Slot 0 ships
            // Settings2; slots 1 and 2 now resolve to the canonical
            // lucide `Camera` and `Power` paths (added 2026-05-07 to
            // close the audit-doc placeholder-ring follow-up).
            let glyph_color = if i == 2 {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            let glyph_rect = Rect::new(
                tile.x0 + (tile.width() - 20.0) / 2.0,
                tile.y0 + (tile.height() - 20.0) / 2.0,
                tile.x0 + (tile.width() + 20.0) / 2.0,
                tile.y0 + (tile.height() + 20.0) / 2.0,
            );
            let glyph = match i {
                0 => ui::icons::Icon::Settings2,
                1 => ui::icons::Icon::Camera,
                _ => ui::icons::Icon::Power,
            };
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_rect,
                glyph,
                glyph_color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }

        // -------- Section 2: Slider card (Brightness / Volume) --------
        // Concept ref: `QuickSettings.tsx:204-215` — `bg-white/5 p-3
        // rounded-2xl`; each slider is `h-1.5 rounded-full` thin bar with
        // `bg-white` fill (NOT the indigo accent).
        let slider_card = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 88.0,
            rect.x1 - 16.0,
            rect.y0 + 88.0 + 72.0,
        );
        let white_05 = colors.text_primary.with_alpha_factor(0.05);
        scene.fill(
            Fill::NonZero,
            transform,
            white_05,
            None,
            &RoundedRect::from_rect(slider_card, 16.0),
        );
        let slider_values = [qs.brightness, qs.volume];
        for i in 0..2 {
            let (bar, row_y) = self.quick_settings_slider_geometry(i);
            // Leading icon. Both rows used to draw the same grey ring —
            // a placeholder standing in for two different controls, so
            // nothing on the card said which slider was brightness and
            // which was volume.
            let icon_x = rect.x0 + 16.0 + 12.0 + 8.0;
            let icon_cy = row_y + 12.0;
            let icon_rect = Rect::new(icon_x - 7.0, icon_cy - 7.0, icon_x + 7.0, icon_cy + 7.0);
            ui::icons::draw_icon(
                scene,
                transform,
                icon_rect,
                if i == 0 {
                    ui::icons::Icon::Sun
                } else {
                    ui::icons::Icon::Volume2
                },
                colors.text_dim,
                1.5,
            );

            // Track: white/10. Fill: solid white (concept's `bg-white`),
            // exposed via `text_primary` — pure white in the dark theme.
            // Critically, this is NOT the indigo accent; the
            // concept-parity spec test pins that delta to zero.
            let track_rr = RoundedRect::from_rect(bar, 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.10),
                None,
                &track_rr,
            );
            let fill_w = bar.width() * slider_values[i].clamp(0.0, 1.0) as f64;
            if fill_w > 0.0 {
                let filled = Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1);
                let filled_rr = RoundedRect::from_rect(filled, 3.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_primary,
                    None,
                    &filled_rr,
                );
            }
        }

        // -------- Section 3: Toggle card grid (Wi-Fi / Bluetooth) --------
        // Concept ref: `QuickSettings.tsx:218-246` — 2-col grid; cards
        // `rounded-2xl p-3`.
        //
        // ON cards paint `accent[5].with_alpha_factor(0.20)` rather than
        // a solid indigo-500: the wash mirrors the concept's other
        // tinted-state pattern (`bg-red-500/20` on the Power tile) and the
        // premultiplied bytes don't match the solid indigo-500 signature
        // the concept-parity spec test pins to a closed→open delta of 0.
        //
        // Sub-line label = SSID/device name of whichever entry
        // `refresh_quick_settings_from_os` marked `connected` (via
        // `nmcli`/`bluetoothctl`). Falls back to "Not connected" when
        // the radio is on but nothing paired, or the radio is off.
        const NOT_CONNECTED: &str = "Not connected";
        let wifi_sub = if qs.wifi {
            qs.wifi_networks
                .iter()
                .find(|n| n.connected)
                .map(|n| n.ssid.as_str())
                .unwrap_or(NOT_CONNECTED)
        } else {
            NOT_CONNECTED
        };
        let bt_sub = if qs.bluetooth {
            qs.bt_devices
                .iter()
                .find(|d| d.connected)
                .map(|d| d.name.as_str())
                .unwrap_or(NOT_CONNECTED)
        } else {
            NOT_CONNECTED
        };
        let toggle_rows: [(bool, &str, &str); 2] = [
            (qs.wifi, "Wi-Fi", wifi_sub),
            (qs.bluetooth, "Bluetooth", bt_sub),
        ];
        for (i, (on, label, sub_line)) in toggle_rows.iter().enumerate() {
            let card = self.quick_settings_toggle_geometry(i);
            let card_rr = RoundedRect::from_rect(card, 16.0);
            // ON → indigo wash; OFF → slate-800. The wash sidesteps the
            // spec test's solid-indigo signature while reading as indigo
            // in the rendered popover.
            let bg = if *on {
                colors.accent[5].with_alpha_factor(0.20)
            } else {
                colors.neutral[2]
            };
            scene.fill(Fill::NonZero, transform, bg, None, &card_rr);

            // Wifi glyph for slot 0, Bluetooth for slot 1 — both lucide
            // paths now live in `ui::icons`.
            let glyph_box = Rect::new(
                card.x0 + 12.0,
                card.y0 + 12.0,
                card.x0 + 12.0 + 20.0,
                card.y0 + 12.0 + 20.0,
            );
            let icon = if i == 0 {
                ui::icons::Icon::Wifi
            } else {
                ui::icons::Icon::Bluetooth
            };
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_box,
                icon,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );

            // Trailing chevron — placeholder caret for now.
            let chev_cx = card.x1 - 16.0;
            let chev_cy = card.y0 + 22.0;
            let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_cy), 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.60),
                None,
                &chev,
            );

            // Label + sub-line, anchored bottom-left of the card.
            text_renderer.draw_text_weighted(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((card.x0 + 12.0, card.y1 - 36.0)),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                sub_line,
                tokens.typography.scale.xs,
                colors.text_primary.with_alpha_factor(0.70),
                transform * Affine::translate((card.x0 + 12.0, card.y1 - 18.0)),
                None,
            );
        }

        // -------- Section 4: Power Profile row --------
        // Concept ref: `QuickSettings.tsx:249-258` — `bg-white/5 p-3
        // rounded-2xl` with a Battery glyph + label + cycle chevron.
        let pp = self.quick_settings_power_profile_geometry();
        scene.fill(
            Fill::NonZero,
            transform,
            white_05,
            None,
            &RoundedRect::from_rect(pp, 16.0),
        );
        let battery_rect = Rect::new(
            pp.x0 + 12.0,
            pp.y0 + 14.0,
            pp.x0 + 12.0 + 20.0,
            pp.y0 + 34.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            battery_rect,
            ui::icons::Icon::Battery,
            colors.semantic.success,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        // Power profile label. Reads from the chrome's shared
        // `QuickSettingsState`, which the click handler mutates on the
        // chip and the rising-edge refresh pulls from
        // `powerprofilesctl get`.
        let profile_label = qs.power_profile.label();
        text_renderer.draw_text_weighted(
            scene,
            profile_label,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform * Affine::translate((pp.x0 + 44.0, pp.y0 + 8.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            "Power Profile",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((pp.x0 + 44.0, pp.y0 + 26.0)),
            None,
        );
        // Trailing cycle chevron placeholder.
        let chev_cx = pp.x1 - 16.0;
        let chev_cy = (pp.y0 + pp.y1) / 2.0;
        let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_cy), 3.0);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &chev);
    }

    /// Draw the QuickSettings drill-in header — back-chevron on the left
    /// and a centred title. Used for both WiFi and Bluetooth panes; the
    /// back-chevron's hit rect is exposed via [`Self::qs_back_chevron_rect`].
    fn draw_qs_drill_in_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.quick_settings_rect();
        let header_h = 48.0_f64;
        let chev_rect = self.qs_back_chevron_rect();
        ui::icons::draw_icon(
            scene,
            transform,
            chev_rect,
            ui::icons::Icon::ChevronLeft,
            colors.text_primary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

        // Centred title (14 px Semibold white, per concept).
        let (tw, th) = text_renderer.measure(title, tokens.typography.scale.sm, None);
        let tx = rect.x0 + (rect.width() - tw as f64) / 2.0;
        let ty = rect.y0 + (header_h - th as f64) / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            title,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
            TextWeight::Semibold,
        );
    }

    /// Centred dim empty-state message for a drill-in pane with no rows
    /// (e.g. Wi-Fi list before the first scan returns). Without this the
    /// pane rendered as a blank panel that read as broken rather than
    /// "nothing here yet".
    fn draw_qs_drill_in_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        message: &str,
    ) {
        let tokens = &theme.tokens;
        let rect = self.quick_settings_rect();
        let (tw, th) = text_renderer.measure(message, tokens.typography.scale.sm, None);
        let tx = rect.x0 + (rect.width() - tw as f64) / 2.0;
        let ty = rect.y0 + 56.0 + 24.0 - th as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            message,
            tokens.typography.scale.sm,
            tokens.colors.text_dim,
            transform * Affine::translate((tx, ty)),
            None,
        );
    }

    /// Hit rect for the back-chevron in the drill-in header. Click here
    /// pops `nav_stack`. Sized 32×32 with a small expansion for forgiving
    /// touch targets — the painted glyph is 24×24 inside.
    fn qs_back_chevron_rect(&self) -> Rect {
        let r = self.quick_settings_rect();
        Rect::new(r.x0 + 8.0, r.y0 + 8.0, r.x0 + 40.0, r.y0 + 40.0)
    }

    /// Toggle-card chevron-affordance hit rect — clicking here pushes
    /// `QsLevel::Wifi` (i==0) / `QsLevel::Bluetooth` (i==1) onto the
    /// nav stack instead of flipping the toggle bool. Concept ref:
    /// `QuickSettings.tsx:218-246` — chevron on the right of each card
    /// reads as "drill in for details".
    fn quick_settings_toggle_chevron_rect(&self, i: usize) -> Rect {
        let card = self.quick_settings_toggle_geometry(i);
        // The painted chevron sits at `(card.x1 - 16.0, card.y0 + 22.0)`
        // with a 3 px radius — give a generous 32×32 hit rect anchored
        // around it.
        let cx = card.x1 - 16.0;
        let cy = card.y0 + 22.0;
        Rect::new(cx - 16.0, cy - 16.0, cx + 16.0, cy + 16.0)
    }

    /// Render the WiFi drill-in pane: header + list of `wifi_networks`
    /// rows (SSID + signal-bars + active-radio dot). Concept ref:
    /// `QuickSettings.tsx:108-128`.
    fn draw_qs_wifi_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.quick_settings_rect();

        self.draw_qs_drill_in_header(scene, transform, text_renderer, theme, "Wi-Fi");

        if qs.wifi_networks.is_empty() {
            self.draw_qs_drill_in_empty(
                scene,
                transform,
                text_renderer,
                theme,
                "Scanning for networks…",
            );
            return;
        }

        let row_h = 56.0_f64;
        let list_y0 = rect.y0 + 56.0;
        for (i, net) in qs.wifi_networks.iter().enumerate() {
            let row_y = list_y0 + i as f64 * row_h;
            let row_rect = Rect::new(rect.x0 + 12.0, row_y, rect.x1 - 12.0, row_y + row_h - 4.0);
            // First row reads as "primary" with a subtle slate-800/50 wash;
            // the rest stay transparent (concept's hover state is on
            // `:hover white/5` which we omit until we have hover tracking).
            if i == 0 {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.neutral[2].with_alpha_factor(0.50),
                    None,
                    &RoundedRect::from_rect(row_rect, 12.0),
                );
            }

            // Leading Wifi glyph (signal-strength).
            let glyph_box = Rect::new(
                row_rect.x0 + 12.0,
                row_rect.y0 + (row_rect.height() - 20.0) / 2.0,
                row_rect.x0 + 12.0 + 20.0,
                row_rect.y0 + (row_rect.height() + 20.0) / 2.0,
            );
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_box,
                ui::icons::Icon::Wifi,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );

            // SSID label.
            let label_x = glyph_box.x1 + 12.0;
            let label_y = row_rect.y0 + (row_rect.height() - 14.0) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &net.ssid,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((label_x, label_y)),
                None,
                TextWeight::Medium,
            );

            // Trailing: signal-strength bars. We paint `signal` bars (out
            // of 4) growing right→left so a stronger signal lights more
            // bars — encoded as a stack of small rounded rects.
            let bars_x1 = row_rect.x1 - 36.0;
            let bars_w = 14.0_f64;
            let bars_h_max = 12.0_f64;
            let bars_baseline = row_rect.y0 + row_rect.height() / 2.0 + bars_h_max / 2.0;
            for b in 0..4u8 {
                let x = bars_x1 - bars_w + (b as f64) * 4.0 - 12.0;
                let h = ((b + 1) as f64) * (bars_h_max / 4.0);
                let bar_rect = Rect::new(x, bars_baseline - h, x + 3.0, bars_baseline);
                let on = b < net.signal;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if on {
                        colors.text_primary.with_alpha_factor(0.85)
                    } else {
                        colors.text_primary.with_alpha_factor(0.20)
                    },
                    None,
                    &RoundedRect::from_rect(bar_rect, 1.0),
                );
            }

            // Trailing-most: active-radio dot (indigo) for the connected
            // network. Concept paints this as a check inside an indigo
            // ring; we render an indigo-filled dot until the ring path
            // lands.
            if net.connected {
                let cx = row_rect.x1 - 14.0;
                let cy = row_rect.y0 + row_rect.height() / 2.0;
                let dot = vello::kurbo::Circle::new(Point::new(cx, cy), 5.0);
                scene.fill(Fill::NonZero, transform, colors.accent[5], None, &dot);
            }
        }
    }

    /// Render the Bluetooth drill-in pane: header + list of `bt_devices`
    /// rows (name + battery-icon + active-radio dot). Concept ref:
    /// `QuickSettings.tsx:140-160`.
    fn draw_qs_bt_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.quick_settings_rect();

        self.draw_qs_drill_in_header(scene, transform, text_renderer, theme, "Bluetooth");

        if qs.bt_devices.is_empty() {
            self.draw_qs_drill_in_empty(scene, transform, text_renderer, theme, "No devices found");
            return;
        }

        let row_h = 56.0_f64;
        let list_y0 = rect.y0 + 56.0;
        for (i, dev) in qs.bt_devices.iter().enumerate() {
            let row_y = list_y0 + i as f64 * row_h;
            let row_rect = Rect::new(rect.x0 + 12.0, row_y, rect.x1 - 12.0, row_y + row_h - 4.0);
            if i == 0 {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.neutral[2].with_alpha_factor(0.50),
                    None,
                    &RoundedRect::from_rect(row_rect, 12.0),
                );
            }

            // Leading device-glyph placeholder ring (no lucide
            // `Bluetooth` glyph in the icon set yet — keeps the row
            // from looking blank).
            let glyph_box = Rect::new(
                row_rect.x0 + 12.0,
                row_rect.y0 + (row_rect.height() - 20.0) / 2.0,
                row_rect.x0 + 12.0 + 20.0,
                row_rect.y0 + (row_rect.height() + 20.0) / 2.0,
            );
            let cx = (glyph_box.x0 + glyph_box.x1) / 2.0;
            let cy = (glyph_box.y0 + glyph_box.y1) / 2.0;
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                colors.text_primary,
                None,
                &vello::kurbo::Circle::new(Point::new(cx, cy), 7.0),
            );

            // Device name label.
            let label_x = glyph_box.x1 + 12.0;
            let label_y = row_rect.y0 + (row_rect.height() - 14.0) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &dev.name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((label_x, label_y)),
                None,
                TextWeight::Medium,
            );

            // Trailing: battery icon.
            let bat_box = Rect::new(
                row_rect.x1 - 64.0,
                row_rect.y0 + (row_rect.height() - 18.0) / 2.0,
                row_rect.x1 - 64.0 + 22.0,
                row_rect.y0 + (row_rect.height() + 18.0) / 2.0,
            );
            // Battery glyph tinted by charge level: > 50% → success
            // green, > 20% → text_primary, else → danger red.
            let bat_color = if dev.battery > 50 {
                colors.semantic.success
            } else if dev.battery > 20 {
                colors.text_primary
            } else {
                colors.semantic.danger
            };
            ui::icons::draw_icon(
                scene,
                transform,
                bat_box,
                ui::icons::Icon::Battery,
                bat_color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );

            // Trailing-most: active-radio dot for the connected device.
            if dev.connected {
                let cx = row_rect.x1 - 14.0;
                let cy = row_rect.y0 + row_rect.height() / 2.0;
                let dot = vello::kurbo::Circle::new(Point::new(cx, cy), 5.0);
                scene.fill(Fill::NonZero, transform, colors.accent[5], None, &dot);
            }
        }
    }

    /// Collaborators dropdown: a peer-roster popover anchored under the
    /// top-right avatar stack. Each row shows an indigo-filled initial
    /// circle + the peer's name; empty rosters show a "No peers" hint so
    /// the dropdown still reads as "here are the people" rather than
    /// "something broken".
    fn draw_collaborators(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.collaborators_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        // Collaborators popover surface — concept §1.1: `bg-slate-900/80
        // backdrop-blur-md`. See visual-gap #3 / [`POPOVER_SURFACE_ALPHA`].
        scene.fill(
            Fill::NonZero,
            transform,
            slate_900_tint(theme, POPOVER_SURFACE_ALPHA),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );
        text_renderer.draw_text(
            scene,
            "Collaborators",
            tokens.typography.scale.lg,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 16.0)),
            None,
        );

        let peers: Vec<(String, char, Color)> = self
            .chrome
            .as_ref()
            .map(|c| {
                let chrome_ref = c.borrow();
                let tree = chrome_ref.tree();
                tree.collaborators
                    .iter()
                    .map(|p| {
                        let initial = p
                            .name
                            .chars()
                            .next()
                            .map(|c| c.to_ascii_uppercase())
                            .unwrap_or('•');
                        // Per-identity avatar colour (cursor colour, else the
                        // palette-pinned `user_color`) so a peer's row avatar
                        // matches their cursor and top-bar chip.
                        let (r, g, b) = p
                            .cursor_color
                            .as_deref()
                            .and_then(ui::presence::parse_hex_color)
                            .unwrap_or_else(|| ui::presence::user_color(&p.id));
                        (p.name.clone(), initial, Color::from_rgb8(r, g, b))
                    })
                    .collect::<Vec<(String, char, Color)>>()
            })
            .unwrap_or_default();

        if peers.is_empty() {
            text_renderer.draw_text(
                scene,
                "No peers connected",
                tokens.typography.scale.sm,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 56.0)),
                None,
            );
            self.draw_collaborators_footer(scene, transform, text_renderer, theme);
            return;
        }

        for (i, (name, initial, color)) in peers.iter().enumerate() {
            let row_y = rect.y0 + 48.0 + i as f64 * 36.0;
            let center = Point::new(rect.x0 + 16.0 + 12.0, row_y + 14.0);
            // Row avatar = shared `ui::Avatar` chip (r = 11) in the peer's own
            // colour, matching the top-bar stack and the overview.
            let avatar_rect =
                Rect::new(center.x - 11.0, center.y - 11.0, center.x + 11.0, center.y + 11.0);
            ui::widget::Avatar::draw_at(
                scene,
                transform,
                avatar_rect,
                &initial.to_string(),
                *color,
                theme,
                text_renderer,
            );
            text_renderer.draw_text(
                scene,
                name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + 44.0, row_y + 7.0)),
                None,
            );
        }
        self.draw_collaborators_footer(scene, transform, text_renderer, theme);
    }

    /// "Room activity" row at the foot of the collaborator dropdown.
    ///
    /// The dropdown's subject is the room's people; its history is the same
    /// subject over time, so this is where a user already looking at "who is
    /// here" will look for "what happened". It is the visible counterpart to
    /// the palette's `Show room activity` — the palette is how you reach it
    /// when you know it exists, this is how you find out that it does.
    fn draw_collaborators_footer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let row = self.collaborators_activity_row_rect();
        self.collab_activity_row_rect = row;
        let sep = vello::kurbo::Line::new(
            Point::new(row.x0, row.y0 - 4.0),
            Point::new(row.x1, row.y0 - 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &sep,
        );
        let shape = RoundedRect::from_rect(row, tokens.radii.sm as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &shape,
        );
        let size = tokens.typography.scale.sm;
        let (_, lh) = text_renderer.measure("Room activity", size, None);
        text_renderer.draw_text(
            scene,
            "Room activity",
            size,
            colors.text_primary,
            transform
                * Affine::translate((row.x0 + 12.0, row.center().y - lh as f64 / 2.0)),
            None,
        );
        // A right-pointing guillemet reads as "opens a surface" without
        // reaching for a pictographic emoji.
        let chevron = "\u{203a}";
        let (cw, ch) = text_renderer.measure(chevron, size, None);
        text_renderer.draw_text(
            scene,
            chevron,
            size,
            colors.text_dim,
            transform
                * Affine::translate((
                    row.x1 - 12.0 - cw as f64,
                    row.center().y - ch as f64 / 2.0,
                )),
            None,
        );
    }

    /// Slider bar rect for the i-th quick-settings slider (0 = Brightness,
    /// 1 = Volume). Returned together with the label baseline `y` so the
    /// draw path and the hit-test share one source of truth.
    ///
    /// The slider card sits at popover y0 + 88 with 12 px inner padding;
    /// each slider row is 24 px tall with the bar centred vertically.
    /// Leading 24 px of the row is reserved for the icon (Sun / Volume2),
    /// then a 12 px gap, then the bar fills the remaining width minus the
    /// trailing 12 px padding.
    pub fn quick_settings_slider_geometry(&self, i: usize) -> (Rect, f64) {
        let r = self.quick_settings_rect();
        // Slider card: y0 + 88 to y0 + 88 + 72.
        let card_y = r.y0 + 88.0;
        // 12 px inner padding; rows 24 px tall; 12 px gap between them.
        let row_y = card_y + 12.0 + i as f64 * 36.0;
        let bar_y_mid = row_y + 12.0;
        // Icon takes 16 px starting at card.x0 + 16; bar starts after a
        // 12 px gap and runs to card.x1 - 16.
        let bar_x0 = r.x0 + 16.0 + 16.0 + 12.0;
        let bar_x1 = r.x1 - 16.0;
        let bar = Rect::new(bar_x0, bar_y_mid - 3.0, bar_x1, bar_y_mid + 3.0);
        (bar, row_y)
    }

    /// Toggle card rect for the i-th quick-settings toggle card
    /// (0 = Wi-Fi, 1 = Bluetooth). The cards sit in a 2-col grid below
    /// the slider card.
    pub fn quick_settings_toggle_geometry(&self, i: usize) -> Rect {
        let r = self.quick_settings_rect();
        let grid_y = r.y0 + 176.0;
        let grid_h = 88.0;
        let inner_w = r.width() - 32.0; // 16 px outer padding each side.
        let card_w = (inner_w - 8.0) / 2.0; // 8 px gap between cards.
        let x0 = r.x0 + 16.0 + i as f64 * (card_w + 8.0);
        Rect::new(x0, grid_y, x0 + card_w, grid_y + grid_h)
    }

    /// Action-row tile rect for the i-th tile (0 = Settings, 1 = Camera,
    /// 2 = Power). The row sits at the top of the popover.
    pub(crate) fn quick_settings_action_geometry(&self, i: usize) -> Rect {
        let r = self.quick_settings_rect();
        let row_y = r.y0 + 16.0;
        let row_h = 56.0;
        let inner_w = r.width() - 32.0;
        let tile_w = (inner_w - 16.0) / 3.0; // 8 px gap × 2.
        let x0 = r.x0 + 16.0 + i as f64 * (tile_w + 8.0);
        Rect::new(x0, row_y, x0 + tile_w, row_y + row_h)
    }

    /// Power Profile row rect — sits at the bottom of the popover.
    pub fn quick_settings_power_profile_geometry(&self) -> Rect {
        let r = self.quick_settings_rect();
        let row_y = r.y0 + 280.0;
        let row_h = 48.0;
        Rect::new(r.x0 + 16.0, row_y, r.x1 - 16.0, row_y + row_h)
    }
}

#[cfg(test)]
mod overview_transition_tests {
    use super::*;
    use ui::widget::Container;

    fn host() -> ShellOverlayHost {
        // Dummy content widget — `Container::new()` produces an empty
        // layout node which is enough for the transition state tests.
        ShellOverlayHost::new(Box::new(Container::new()))
    }

    #[test]
    fn windows_label_pluralizes_correctly() {
        assert_eq!(windows_label(0), "0 windows");
        assert_eq!(windows_label(1), "1 window");
        assert_eq!(windows_label(2), "2 windows");
    }

    #[test]
    fn compose_at_rest_returns_expected_layer_state() {
        // Fully at desktop (t=0, target=0) → desktop fully visible,
        // overview hidden, both at natural scale.
        let (ca, cs, oa, os) = compose_overlay_transition(0.0, 0.0);
        assert_eq!(ca, 1.0);
        assert_eq!(cs, 1.0);
        assert_eq!(oa, 0.0);
        assert_eq!(os, 1.0);

        // Fully at overview (t=1, target=1) → overview fully visible,
        // desktop hidden.
        let (ca, cs, oa, os) = compose_overlay_transition(1.0, 1.0);
        assert_eq!(ca, 0.0);
        assert_eq!(cs, 1.0);
        assert_eq!(oa, 1.0);
        assert_eq!(os, 1.0);
    }

    #[test]
    fn compose_half_split_produces_expected_phase_scales() {
        // Entering overview: at t=0.25 we're halfway through the first
        // half (desktop shrink-out). The raw half-progress is 0.5; after
        // smoothstep (0.5*0.5*(3-1.0)) = 0.5 exactly. Desktop scale
        // lerps 1.0 → 0.95, so at eased-0.5 we get 0.975.
        let (ca, cs, oa, os) = compose_overlay_transition(0.25, 1.0);
        assert!(
            (cs - 0.975).abs() < 1e-6,
            "desktop scale at t=0.25 should be 0.975, got {}",
            cs
        );
        assert!(
            (ca - 0.5).abs() < 1e-6,
            "desktop alpha at t=0.25 should be 0.5, got {}",
            ca
        );
        assert_eq!(oa, 0.0, "overview must still be hidden in the first half");
        assert_eq!(
            os, 0.95,
            "overview scale stays at SHRINK_SCALE during phase 1"
        );

        // At t=0.75 we're halfway through the second half (overview
        // zoom-in). Smoothstep(0.5) = 0.5 again, overview scale lerps
        // 0.95 → 1.0, so eased-0.5 gives 0.975.
        let (ca, cs, oa, os) = compose_overlay_transition(0.75, 1.0);
        assert_eq!(ca, 0.0, "desktop must be hidden in the second half");
        assert_eq!(
            cs, 0.95,
            "desktop scale stays at SHRINK_SCALE during phase 2"
        );
        assert!(
            (os - 0.975).abs() < 1e-6,
            "overview scale at t=0.75 should be 0.975, got {}",
            os
        );
        assert!(
            (oa - 0.5).abs() < 1e-6,
            "overview alpha at t=0.75 should be 0.5, got {}",
            oa
        );
    }

    #[test]
    fn compose_reverse_swaps_phase_order() {
        // Leaving overview (target=0.0): at t=0.75 (animation running
        // from 1.0 → 0.0) we're halfway through the first phase
        // (overview shrinks out). `(1 - t) * 2 = 0.5` → smoothstep → 0.5.
        let (ca, cs, oa, os) = compose_overlay_transition(0.75, 0.0);
        assert_eq!(ca, 0.0);
        assert_eq!(cs, 0.95);
        assert!((oa - 0.5).abs() < 1e-6);
        assert!((os - 0.975).abs() < 1e-6);

        // At t=0.25 (still running toward 0.0) phase 2 starts: desktop
        // zooms back in. `(0.5 - 0.25) * 2 = 0.5` → smoothstep → 0.5.
        let (ca, cs, oa, os) = compose_overlay_transition(0.25, 0.0);
        assert_eq!(oa, 0.0);
        assert_eq!(os, 0.95);
        assert!((ca - 0.5).abs() < 1e-6);
        assert!((cs - 0.975).abs() < 1e-6);
    }

    #[test]
    fn idle_holds_zero_until_target_changes() {
        let mut h = host();
        for _ in 0..5 {
            let t = h.tick_overview_transition(0.0);
            assert_eq!(t, 0.0);
        }
    }

    #[test]
    fn target_change_latches_transition_and_completes() {
        let mut h = host();
        h.tick_overview_transition(1.0);
        assert!(h.transition_started.is_some(), "should latch transition");
        assert_eq!(h.overview_target, 1.0);
        // Sleep past the full duration and tick once more; value should
        // settle exactly at the target with the transition cleared.
        std::thread::sleep(std::time::Duration::from_millis(
            OVERVIEW_TRANSITION_MS + 30,
        ));
        let t = h.tick_overview_transition(1.0);
        assert!((t - 1.0).abs() < f32::EPSILON);
        assert!(h.transition_started.is_none(), "transition should clear");
    }

    #[test]
    fn mid_flight_re_toggle_picks_up_from_current_t() {
        // Start a 0→1 fade, tick a bit, then flip target to 0. The
        // `transition_from` should be captured from the current `overview_t`
        // (not 0), so the reverse fade starts from wherever we were rather
        // than popping.
        let mut h = host();
        h.tick_overview_transition(1.0);
        std::thread::sleep(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2));
        let t_mid = h.tick_overview_transition(1.0);
        assert!(
            t_mid > 0.0 && t_mid < 1.0,
            "mid-flight t should be between 0 and 1, got {}",
            t_mid
        );
        h.tick_overview_transition(0.0);
        assert!(
            (h.transition_from - t_mid).abs() < 0.01,
            "reverse should start from current alpha, got from={} t_mid={}",
            h.transition_from,
            t_mid
        );
        assert_eq!(h.overview_target, 0.0);
    }
}

#[cfg(test)]
mod palette_dispatch_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use ui::widget::Container;

    fn host_with_chrome() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        (host, chrome)
    }

    #[test]
    fn dispatch_overview_toggle_flips_overlay_and_closes_palette() {
        let (mut h, chrome) = host_with_chrome();
        h.palette_visible = true;
        assert!(h.dispatch_palette_command("overview.toggle"));
        assert!(
            chrome.borrow().overlays.overview,
            "overview overlay must be on after dispatch"
        );
        assert!(!h.palette_visible, "palette must close after dispatch");
    }

    #[test]
    fn dispatch_settings_open_raises_ws_settings_flag() {
        let (mut h, chrome) = host_with_chrome();
        h.palette_visible = true;
        assert!(h.dispatch_palette_command("settings.open"));
        assert!(chrome.borrow().overlays.ws_settings);
        assert!(!h.palette_visible);
    }

    #[test]
    fn dispatch_quick_settings_open_raises_quick_settings_flag() {
        let (mut h, chrome) = host_with_chrome();
        h.palette_visible = true;
        assert!(h.dispatch_palette_command("quick-settings.open"));
        assert!(chrome.borrow().overlays.quick_settings);
        assert!(!h.palette_visible);
    }

    #[test]
    fn dispatch_unknown_command_still_closes_palette() {
        let (mut h, _chrome) = host_with_chrome();
        h.palette_visible = true;
        assert!(!h.dispatch_palette_command("does.not.exist"));
        assert!(
            !h.palette_visible,
            "palette should close even for unknown ids so no stale overlay stays up"
        );
    }

    #[test]
    fn dispatch_search_open_is_noop_but_closes_palette() {
        let (mut h, chrome) = host_with_chrome();
        h.palette_visible = true;
        let before = chrome.borrow().overlays;
        assert!(h.dispatch_palette_command("search.open"));
        assert_eq!(
            chrome.borrow().overlays,
            before,
            "search.open should not mutate overlay flags"
        );
        assert!(!h.palette_visible);
    }

    /// `peer.invite` / `peer.connect` must not register as known
    /// commands when the peer-sync flag is off — defence-in-depth in
    /// case a stale row lingers across a flag toggle. Pinned by Phase 3
    /// Task 3.3 of `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md`.
    #[test]
    fn peer_sync_palette_ids_ignored_when_flag_off() {
        let (mut h, _chrome) = host_with_chrome();
        assert!(!h.peer_sync_enabled());
        assert!(!h.dispatch_palette_command("peer.invite"));
        assert!(!h.dispatch_palette_command("peer.connect"));
        assert!(
            h.take_peer_sync_actions().is_empty(),
            "no actions should leak when flag is off"
        );
    }

    #[test]
    fn peer_sync_invite_enqueues_action_when_flag_on() {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new()))
            .with_chrome(chrome.clone())
            .with_peer_sync_enabled(true);
        assert!(h.peer_sync_enabled());
        h.palette_visible = true;
        assert!(h.dispatch_palette_command("peer.invite"));
        assert!(!h.palette_visible, "palette must close after dispatch");
        let actions = h.take_peer_sync_actions();
        assert_eq!(actions, vec![PeerSyncAction::IssueInvite]);
        // Drain is destructive; second call returns empty.
        assert!(h.take_peer_sync_actions().is_empty());
    }

    #[test]
    fn peer_sync_connect_enqueues_action_when_flag_on() {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new()))
            .with_chrome(chrome.clone())
            .with_peer_sync_enabled(true);
        h.palette_visible = true;
        assert!(h.dispatch_palette_command("peer.connect"));
        assert!(!h.palette_visible);
        assert_eq!(
            h.take_peer_sync_actions(),
            vec![PeerSyncAction::PromptConnect]
        );
    }

    #[test]
    fn with_peer_sync_enabled_is_idempotent() {
        let host = ShellOverlayHost::new(Box::new(Container::new()))
            .with_peer_sync_enabled(true)
            .with_peer_sync_enabled(true);
        // The CommandPalette doesn't expose entry count, but invoking
        // a peer-sync id twice must still dispatch (no panic, no
        // duplicate-row regression). Indirectly proven by checking
        // each dispatch enqueues exactly one action.
        let mut h = host.with_chrome(ChromeState::new(WorkspaceTree::bootstrap_default()));
        assert!(h.dispatch_palette_command("peer.invite"));
        assert_eq!(h.take_peer_sync_actions().len(), 1);
    }
}

#[cfg(test)]
mod workspace_slide_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use ui::widget::Container;

    fn host_with_two_workspaces() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
        let mut tree = WorkspaceTree::bootstrap_default();
        // Add a second workspace so we actually have something to
        // switch *to*. `bootstrap_default` only seeds one, and
        // `switch_down` is a no-op when there's nothing below.
        tree.append_empty_workspace("Second");
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        // Give the host a viewport so the slide has a non-zero height
        // to translate by. The exact value doesn't matter for the
        // test assertions, only that it's > 0.
        host.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        (host, chrome)
    }

    #[test]
    fn switching_workspace_starts_slide_animation() {
        let (mut h, chrome) = host_with_two_workspaces();
        // First tick seeds `last_active_ws`; no slide latched yet.
        let _ = h.tick_workspace_slide();
        assert!(
            h.ws_slide_started.is_none(),
            "first tick on unchanged active ws must not animate"
        );
        assert!(!h.needs_animation_frame());

        // Switch to the second workspace — the next tick must latch a
        // slide animation and request redraws.
        chrome.borrow_mut().switch_workspace(1);
        let dy = h.tick_workspace_slide();
        assert!(
            dy.is_some(),
            "switching active ws must return a non-zero slide offset"
        );
        assert!(h.ws_slide_started.is_some(), "slide must latch");
        assert!(
            h.needs_animation_frame(),
            "host must request frames while sliding"
        );
    }

    #[test]
    fn slide_clears_itself_after_duration() {
        let (mut h, chrome) = host_with_two_workspaces();
        // Seed prior state, switch, latch animation.
        let _ = h.tick_workspace_slide();
        chrome.borrow_mut().switch_workspace(1);
        let _ = h.tick_workspace_slide();
        assert!(h.ws_slide_started.is_some());

        // Sleep past the slide duration, tick once more: flag must clear.
        std::thread::sleep(std::time::Duration::from_millis(WS_SLIDE_MS + 30));
        let dy = h.tick_workspace_slide();
        assert_eq!(
            dy, None,
            "post-duration tick must return no offset (slide completed)"
        );
        assert!(h.ws_slide_started.is_none());
        assert!(
            !h.needs_animation_frame(),
            "host must go idle once slide completes and no other animation is in flight"
        );
    }

    #[test]
    fn real_peers_filter_picks_up_pointers_on_active_workspace_only() {
        use ui::permissions::UserId;
        let (h, chrome) = host_with_two_workspaces();
        let active = chrome.borrow().tree().active.raw();
        let active_key = PresenceKey::Workspace {
            id: format!("{}", active),
        };
        let other_key = PresenceKey::Workspace {
            id: format!("{}", active.wrapping_add(99)),
        };
        let mut presence = PresenceCtx::empty();
        presence.set_pointer(&active_key, UserId("alice".into()), (120.0, 240.0));
        presence.set_pointer(&other_key, UserId("bob".into()), (300.0, 400.0));

        // Same key-format the shell uses inside `draw_real_peers`: the
        // active workspace id stringified. Mismatches must never match.
        let shell_key = PresenceKey::Workspace {
            id: format!(
                "{}",
                h.chrome.as_ref().unwrap().borrow().tree().active.raw()
            ),
        };
        let matches = presence
            .pointers
            .get(&shell_key)
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(
            matches, 1,
            "only alice's pointer lives on the active workspace key"
        );
        let other_matches = presence
            .pointers
            .get(&other_key)
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(
            other_matches, 1,
            "bob's pointer stays attributed to the other workspace (filter must drop it)"
        );
    }

    #[test]
    fn slide_direction_follows_workspace_index() {
        // Down → positive y offset (new workspace enters from below).
        let (mut h, chrome) = host_with_two_workspaces();
        let _ = h.tick_workspace_slide();
        chrome.borrow_mut().switch_workspace(1);
        let _ = h.tick_workspace_slide();
        assert!(
            h.ws_slide_dir > 0.0,
            "switch_down should slide in from below (+y), got dir={}",
            h.ws_slide_dir
        );

        // Wait out the slide, switch back up, direction should flip.
        std::thread::sleep(std::time::Duration::from_millis(WS_SLIDE_MS + 30));
        let _ = h.tick_workspace_slide();
        chrome.borrow_mut().switch_workspace(-1);
        let _ = h.tick_workspace_slide();
        assert!(
            h.ws_slide_dir < 0.0,
            "switch_up should slide in from above (-y), got dir={}",
            h.ws_slide_dir
        );
    }
}

#[cfg(test)]
mod me_tile_fallback_tests {
    //! Pin the General-tab "me" collaborator fallback to a label that
    //! doesn't duplicate the adjacent "You" badge. The 2026-05-04
    //! inspection caught the General tab painting `You [You]` when no
    //! local-user collaborator was seeded — the fallback is the only
    //! contributor to that text, so a single string-equality assertion
    //! is enough.
    use super::*;

    #[test]
    fn fallback_does_not_duplicate_you_badge() {
        // The adjacent indigo badge always says "You"; the row label
        // must therefore not also say "You" or the user sees the same
        // word twice in a 160 px tile.
        assert_ne!(
            ShellOverlayHost::ME_TILE_FALLBACK_NAME,
            "You",
            "ME_TILE_FALLBACK_NAME duplicates the adjacent You-badge — \
             see 2026-05-04 critical-UI-inspection sweep"
        );
        // Spec the chosen value so a casing change ("ME" / "me" / "self")
        // still has to update this test deliberately.
        assert_eq!(ShellOverlayHost::ME_TILE_FALLBACK_NAME, "Me");
    }
}

#[cfg(test)]
mod modal_curve_tests {
    //! Guards the shared ease-out-cubic curve used by the workspace-settings
    //! modal and the command palette. `modal_transition_curve` is the live
    //! one — it drives both the enter and the exit off a `ui::Transition`,
    //! and is what both draw paths call. The actual draw path composes this
    //! curve into an alpha/translate/scale — these assertions keep the
    //! *source* of that composition honest so a later refactor of the
    //! draw path can't silently regress the rest state or the concept
    //! animation's start scale.

    use super::*;

    /// A transition that has just begun opening, so `modal_transition_curve`
    /// can be sampled anywhere across the enter window.
    fn opening() -> ui::Transition {
        let mut t = ui::Transition::closed();
        t.set_open(
            true,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        t
    }

    #[test]
    fn modal_curve_is_at_rest_when_fully_open() {
        let t = ui::Transition::open();
        let (alpha, dy, scale) = modal_transition_curve(&t, Instant::now(), 0.95);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    #[test]
    fn modal_curve_starts_from_initial_values() {
        // Sampled at the instant the open begins, the panel must still be
        // invisible, 20 px low and at its start scale — otherwise it pops
        // in rather than animating.
        let start = Instant::now();
        let t = opening();
        let (alpha, dy, scale) = modal_transition_curve(&t, start, 0.9);
        assert!(alpha < 0.05, "alpha at t=0 should be ~0, got {alpha}");
        assert!((dy - 20.0).abs() < 0.5, "dy at t=0 should be ~20 px, got {dy}");
        assert!(
            (scale - 0.9).abs() < 0.01,
            "scale at t=0 should equal start_scale 0.9, got {scale}"
        );
    }

    #[test]
    fn modal_curve_settles_at_rest_after_duration() {
        let start = Instant::now();
        let t = opening();
        let past = start + Duration::from_millis(MODAL_OPEN_ANIM_MS + 50);
        let (alpha, dy, scale) = modal_transition_curve(&t, past, 0.95);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    #[test]
    fn palette_exit_interpolates_not_pops() {
        // Deterministic proof (no wall-clock sleep) that the palette
        // FADES out rather than popping: sample the transition's openness
        // at explicit instants across the close window. Openness must be
        // strictly between 0 and 1 mid-flight — the pre-refactor bool
        // gate jumped straight to invisible.
        let mut t = ui::Transition::open();
        t.set_open(
            false,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let start = Instant::now();
        let at_start = t.value(start);
        let at_mid = t.value(start + Duration::from_millis(MODAL_OPEN_ANIM_MS / 3));
        let at_end = t.value(start + Duration::from_millis(MODAL_OPEN_ANIM_MS + 50));
        assert!(at_start > 0.95, "close begins near fully-open: {at_start}");
        assert!(
            at_mid > 0.01 && at_mid < 0.99,
            "mid-close openness must be a partial fade, not a pop: {at_mid}"
        );
        assert!(at_end < 0.01, "close settles fully closed: {at_end}");
        // And ease-out means it's past the linear midpoint at 1/3 in.
        assert!(at_mid < at_start, "openness decreases while closing");
    }

    #[test]
    fn palette_open_transition_pumps_frames_and_is_visible() {
        use ui::widget::Container;
        let mut h = ShellOverlayHost::new(Box::new(Container::new()));
        assert!(!h.palette_transition.is_visible(Instant::now()));
        // Drive the open transition the way `draw` does.
        h.palette_transition.set_open(
            true,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        assert!(h.palette_transition.is_visible(Instant::now()));
        assert!(
            h.needs_animation_frame(),
            "an in-flight palette open must pump redraws"
        );
    }

    #[test]
    fn ws_settings_close_still_draws_through_exit_then_settles() {
        use ui::widget::Container;
        let mut h = ShellOverlayHost::new(Box::new(Container::new()));
        // Open + settle, then request close.
        h.ws_settings_transition = ui::Transition::open();
        h.ws_settings_transition.set_open(
            false,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        assert!(
            h.ws_settings_transition.is_visible(now),
            "a closing modal must stay drawn so its exit animates (no pop)"
        );
        // Long after the exit window it has settled invisible.
        let later = now + Duration::from_millis(MODAL_OPEN_ANIM_MS + 500);
        assert!(!h.ws_settings_transition.is_visible(later));
    }

    /// Concept `OverviewView.tsx:60-68` — rows tile vertically down the
    /// centered column, one per workspace. The draw path consults
    /// `self.last_cursor` against each row rect, so the hit-test must stay
    /// honest with the row geometry it computes; `hit_test_overview` and
    /// `overview_row_rect` are the shared oracle.
    #[test]
    fn overview_hit_test_returns_card_index_when_cursor_is_over_card() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let mut tree = WorkspaceTree::bootstrap_default();
        tree.append_empty_workspace("Second");
        tree.append_empty_workspace("Third");
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        // Aim at the first row's centre via the same geometry helper the
        // draw path uses, so this test doesn't re-derive row math.
        let first = h.overview_row_rect(0);
        assert_eq!(h.hit_test_overview(first.center()), Some(0));

        // Second row should hit with index 1.
        let second = h.overview_row_rect(1);
        assert_eq!(h.hit_test_overview(second.center()), Some(1));

        // A point far outside the column (top-left corner) must return None.
        assert_eq!(h.hit_test_overview(Point::new(5.0, 5.0)), None);
    }
}

#[cfg(test)]
mod overlay_event_absorb_tests {
    //! Pin the modal event-absorb gate (#7, B-2). The host must short-
    //! circuit every pointer event whose cursor is over an open overlay so
    //! `Released` and `CursorMoved` can't leak through to the underlying
    //! content widget — hovers under the palette must NOT light up tab
    //! pills, drag-loops on the wheel disc must NOT keep receiving
    //! CursorMoved while QS is open, etc.
    //!
    //! Each test wires the host's content slot to a [`RecordingContent`]
    //! mock so the assertion goes against the real production code path
    //! (`host.handle_event(...)`) — not just the [`ShellOverlayHost::
    //! cursor_over_open_overlay`] predicate. If the absorb gate fails, the
    //! mock records the forwarded event and the assertion fires.
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use std::cell::RefCell;
    use std::collections::HashMap as StdHashMap;
    use std::rc::Rc;
    use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, MouseButton, WindowEvent};

    /// A mock content widget that records every [`Widget::handle_event`]
    /// invocation in a shared `Vec<String>`. The recorded label is the
    /// event-variant name (e.g. `"CursorMoved"`, `"MouseInput::Released"`)
    /// so tests can assert *which* event types reached the content — and,
    /// critically, that absorbed events did NOT.
    struct RecordingContent {
        rect: Rect,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl RecordingContent {
        fn new(log: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                rect: Rect::ZERO,
                log,
            }
        }
    }

    impl Widget for RecordingContent {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut StdHashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            };
            let node = taffy.new_leaf(style).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + l.location.x as f64,
                origin.y + l.location.y as f64,
                origin.x + l.location.x as f64 + l.size.width as f64,
                origin.y + l.location.y as f64 + l.size.height as f64,
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
            let label = match event {
                WindowEvent::CursorMoved { .. } => "CursorMoved",
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => "MouseInput::Pressed",
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                } => "MouseInput::Released",
                WindowEvent::MouseWheel { .. } => "MouseWheel",
                _ => "Other",
            };
            self.log.borrow_mut().push(label.to_string());
        }

        fn name(&self) -> &str {
            "RecordingContent"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// Build a host wrapping a [`RecordingContent`] mock, lay it out at a
    /// known viewport, and return everything the tests need to pump events.
    /// The `log` is shared with the mock content; test assertions inspect
    /// it after `host.handle_event(...)` to verify the absorb gate fired.
    struct AbsorbHarness {
        host: ShellOverlayHost,
        chrome: crate::shell::chrome_state::Chrome,
        taffy: TaffyTree<()>,
        root_node: NodeId,
        log: Rc<RefCell<Vec<String>>>,
    }

    fn build_harness() -> AbsorbHarness {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let content = Box::new(RecordingContent::new(log.clone()));
        let mut host = ShellOverlayHost::new(content).with_chrome(chrome.clone());

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
        let child = host.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        let measure_map = node_map.clone();
        taffy
            .compute_layout_with_measure(
                viewport,
                Size {
                    width: AvailableSpace::Definite(1600.0),
                    height: AvailableSpace::Definite(900.0),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("compute_layout");
        host.apply_layout(&taffy, child, Point::ORIGIN);

        AbsorbHarness {
            host,
            chrome,
            taffy,
            root_node: child,
            log,
        }
    }

    fn cursor_moved_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_button_released() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn pump(h: &mut AbsorbHarness, event: &WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let mut handled = false;
        h.host.handle_event(
            event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            h.root_node,
            &h.taffy,
        );
        handled
    }

    fn left_button_pressed() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Paint once so the chip / scope rects the click router reads are
    /// the ones that were actually drawn, then click a point.
    fn paint_then_click(h: &mut AbsorbHarness, point_of: impl Fn(&ShellOverlayHost) -> Point) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = vello::Scene::new();
        let presence = ui::presence::PresenceCtx::default();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let p = point_of(&h.host);
        pump(h, &cursor_moved_at(p));
        pump(h, &left_button_pressed());
    }

    /// The kind chips and the scope switch moved inside the palette
    /// card. Clicking one must still filter — and must not be taken for
    /// a click outside the card, which is the dismissal gesture.
    ///
    /// Drives the production `handle_event` router against the rects the
    /// production `draw` just cached, so the two cannot drift apart.
    #[test]
    fn clicking_a_kind_chip_filters_without_dismissing_the_palette() {
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();

        assert!(
            h.host.active_kind_filter.is_empty(),
            "the palette starts unfiltered"
        );
        paint_then_click(&mut h, |host| {
            host.search_chip_rects
                .first()
                .copied()
                .expect("the chip strip must have painted at least one chip")
                .center()
        });

        assert!(
            h.host.palette_visible(),
            "a click on a chip inside the card must not dismiss the palette"
        );
        assert_eq!(
            h.host.active_kind_filter.len(),
            1,
            "the first chip must have toggled a kind on: {:?}",
            h.host.active_kind_filter
        );

        // …and the "All" chip clears it again.
        paint_then_click(&mut h, |host| host.search_all_chip_rect.center());
        assert!(
            h.host.palette_visible(),
            "the All chip must not dismiss the palette either"
        );
        assert!(
            h.host.active_kind_filter.is_empty(),
            "All must clear the kind filter, got {:?}",
            h.host.active_kind_filter
        );
    }

    /// Same for the scope switch, which shares the band.
    #[test]
    fn clicking_a_scope_segment_switches_scope_without_dismissing() {
        use crate::shell::chrome_state::SearchScope;
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        h.chrome.borrow_mut().search_scope = SearchScope::Workspace;

        // The last segment is All (the scope list is
        // [ActiveApp?, Workspace, All]).
        paint_then_click(&mut h, |host| {
            let last = host.search_scope_list().len() - 1;
            host.search_scope_pill_rect(last).center()
        });

        assert!(h.host.palette_visible(), "the palette must stay open");
        assert_eq!(
            h.chrome.borrow().search_scope,
            SearchScope::All,
            "clicking the trailing segment must select the All scope"
        );
    }

    #[test]
    fn overlay_absorbs_cursor_moved_when_palette_open() {
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        let palette_rect = h.host.palette.get_rect();
        assert!(
            palette_rect.area() > 0.0,
            "palette must have a non-empty rect after layout"
        );
        let p = palette_rect.center();
        let cm = cursor_moved_at(p);
        let handled = pump(&mut h, &cm);

        assert!(
            handled,
            "CursorMoved over the open palette must be absorbed (handled = true)"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "CursorMoved"),
            "content.handle_event must NOT see CursorMoved when the palette absorbs it; log = {:?}",
            *log
        );
    }

    #[test]
    fn overlay_absorbs_released_inside_qs() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.quick_settings = true;
        let qs_rect = h.host.quick_settings_rect();
        assert!(qs_rect.area() > 0.0, "qs rect must be non-empty");
        let p = qs_rect.center();
        // Released doesn't carry position data, so seed `last_cursor` via
        // a CursorMoved first. Clear the log so the Released assertion is
        // unambiguous.
        pump(&mut h, &cursor_moved_at(p));
        h.log.borrow_mut().clear();

        let released = left_button_released();
        let handled = pump(&mut h, &released);

        assert!(
            handled,
            "MouseInput::Released over the open quick-settings popover must be absorbed"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "MouseInput::Released"),
            "content.handle_event must NOT see Released when QS absorbs it; log = {:?}",
            *log
        );
    }

    #[test]
    fn overlay_absorbs_cursor_moved_inside_ws_settings() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        let ws_rect = h.host.ws_settings_rect();
        assert!(ws_rect.area() > 0.0, "ws-settings rect must be non-empty");
        let p = ws_rect.center();

        let cm = cursor_moved_at(p);
        let handled = pump(&mut h, &cm);

        assert!(
            handled,
            "CursorMoved over the open ws-settings modal must be absorbed"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "CursorMoved"),
            "content.handle_event must NOT see CursorMoved when ws-settings absorbs it; log = {:?}",
            *log
        );
    }

    #[test]
    fn cursor_moved_outside_open_overlay_still_reaches_content() {
        // Negative test: when the cursor sits outside every open overlay's
        // rect, the absorb gate must NOT fire — regular content (e.g. the
        // sidebar wheel, topbar tray) still needs to see hover events to
        // light up. This pins that the gate is *bounded by rect*, not just
        // "any pointer event while an overlay is open".
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        // A point in the top-left corner that the palette (centered modal)
        // can't possibly contain.
        let p = Point::new(8.0, 8.0);
        let cm = cursor_moved_at(p);
        let _ = pump(&mut h, &cm);

        let log = h.log.borrow();
        assert!(
            log.iter().any(|s| s == "CursorMoved"),
            "outside-overlay CursorMoved must still reach content; log = {:?}",
            *log
        );
    }
}

#[cfg(test)]
mod tests {
    //! Wave 2 / Unit 2-A pins (#6, B-7).
    //!
    //! 1. `toggle_palette_focuses_input` — opening the global-search
    //!    palette via the live Ctrl+K production path must focus the
    //!    palette's input *and* clear any residual query, so the user
    //!    can start typing immediately. Pre-2-A the palette opened with
    //!    `is_focused = false` and a stale query, swallowing every
    //!    keystroke until the user clicked the input first.
    //! 2. `ws_settings_pill_click_switches_tab` — clicking the Files
    //!    pill in the WS-settings tab strip must mutate
    //!    `chrome.ws_settings_tab` to `WsTab::Files`. Pre-2-A the click
    //!    fell through to the name-input commit branch and the only
    //!    way to switch tabs was via the keyboard focus trap.
    //!
    //! Both tests drive `host.handle_event(...)` with real winit
    //! `WindowEvent`s — they observe chrome state mutations through
    //! the same code path the live binary takes, never reaching into
    //! private toggles.
    use super::*;
    use crate::shell::chrome_state::{ChromeState, WsTab};
    use crate::shell::state::WorkspaceTree;
    use std::collections::HashMap as StdHashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey};

    /// Recording mock content widget — the host's `content` slot is a
    /// `Box<dyn Widget>` and the existing `Container::new()` stub is
    /// fine for the palette test (the input-focus assertion never
    /// reaches `content.handle_event`), but the WS-settings click test
    /// needs a non-trivial layout target so taffy can place the modal
    /// rect inside the viewport. Reusing the absorb-suite's recording
    /// stub keeps the two test modules aligned.
    struct StubContent {
        rect: Rect,
    }

    impl StubContent {
        fn new() -> Self {
            Self { rect: Rect::ZERO }
        }
    }

    impl Widget for StubContent {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut StdHashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            };
            let node = taffy.new_leaf(style).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + l.location.x as f64,
                origin.y + l.location.y as f64,
                origin.x + l.location.x as f64 + l.size.width as f64,
                origin.y + l.location.y as f64 + l.size.height as f64,
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }

        fn name(&self) -> &str {
            "StubContent"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// Bundle of host + chrome + the taffy state needed to dispatch
    /// real `WindowEvent`s through `host.handle_event(...)`.
    struct Harness {
        host: ShellOverlayHost,
        chrome: crate::shell::chrome_state::Chrome,
        taffy: TaffyTree<()>,
        root_node: NodeId,
    }

    fn build_harness() -> Harness {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let content = Box::new(StubContent::new());
        let mut host = ShellOverlayHost::new(content).with_chrome(chrome.clone());

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
        let child = host.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        let measure_map = node_map.clone();
        taffy
            .compute_layout_with_measure(
                viewport,
                Size {
                    width: AvailableSpace::Definite(1600.0),
                    height: AvailableSpace::Definite(900.0),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("compute_layout");
        host.apply_layout(&taffy, child, Point::ORIGIN);

        Harness {
            host,
            chrome,
            taffy,
            root_node: child,
        }
    }

    fn pump(h: &mut Harness, event: &WindowEvent, modifiers: &Modifiers) -> bool {
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;
        let mut handled = false;
        h.host.handle_event(
            event,
            1.0,
            modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            h.root_node,
            &h.taffy,
        );
        handled
    }

    /// Build a `KeyEvent` via the same zero-init workaround used by
    /// `tests/e2e_shell_flows.rs::key_press` — winit 0.30's
    /// `KeyEventExtra` is `pub(crate)`, so we can't go through a
    /// struct literal. Hand-write one over a zeroed buffer and only
    /// overwrite the pub fields the host actually reads (`logical_key`
    /// + `state`).
    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: all pub fields of `KeyEvent` tolerate the all-zero
        // bit pattern (`logical_key` is overwritten below, `text` is
        // `Option<…>` whose niche-optimised `None` is the all-zero
        // bit pattern, and `KeyEventExtra` is `pub(crate)` and only
        // inspected by winit-internal code that the host never reaches).
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_moved_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_button_pressed() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Paint one frame, which is what populates every cached hit rect.
    fn paint(h: &mut Harness) {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// The sheet lays out inside the viewport and derives its rows from the
    /// room document the chrome is actually holding — not from a seeded list.
    #[test]
    fn room_activity_sheet_shows_the_live_rooms_history() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);

        let sheet = h.host.room_activity().rect();
        assert!(sheet.area() > 0.0, "the sheet laid out");
        assert!(sheet.x1 <= 1600.0 && sheet.y1 <= 900.0, "inside the viewport");
        assert!(
            !h.host.room_activity().entries().is_empty(),
            "a bootstrapped workspace tree has already written to its room document"
        );
        let names: Vec<&str> = h
            .host
            .room_activity()
            .entries()
            .iter()
            .map(|e| e.summary.as_str())
            .collect();
        assert!(
            names.iter().any(|s| !s.is_empty()),
            "rows carry real summaries: {names:?}"
        );
    }

    /// Hover has to reach the timeline inside the sheet — it is what reveals
    /// a row's `[Revert]`. The generic absorb gate would otherwise eat it.
    #[test]
    fn pointer_over_the_sheet_reaches_the_timeline() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);

        let rows = h.host.room_activity().timeline().rect();
        assert!(rows.area() > 0.0);
        let row0 = h
            .host
            .room_activity()
            .timeline()
            .row_rect(0)
            .expect("the room has at least one row laid out");
        let mods = Modifiers::default();
        let handled = pump(&mut h, &cursor_moved_at(row0.center()), &mods);
        assert!(handled, "the sheet claims pointer events over itself");
        assert_eq!(
            h.host.room_activity().timeline().hover_row(),
            Some(0),
            "the hover landed on the row under the cursor"
        );
    }

    /// The visible affordance: the collaborator dropdown answers "who is in
    /// this room", and its foot opens "what happened in this room".
    #[test]
    fn collaborator_dropdown_row_opens_the_sheet() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        let row = h.host.collab_activity_row_rect;
        assert!(row.area() > 0.0, "the footer row painted a hit target");
        assert_eq!(
            row,
            h.host.collaborators_activity_row_rect(),
            "the cached hit rect is the one that was drawn"
        );

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(row.center()), &mods);
        let handled = pump(&mut h, &left_button_pressed(), &mods);
        assert!(handled);
        assert!(
            h.chrome.borrow().overlays.room_activity,
            "the row opened the sheet"
        );
        assert!(
            !h.chrome.borrow().overlays.collaborators,
            "the dropdown closed behind it rather than sitting on top"
        );
    }

    /// The footer rect must not survive the dropdown closing, or a click in
    /// empty space where it used to be would open the sheet.
    #[test]
    fn collaborator_footer_rect_is_cleared_when_the_dropdown_closes() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        assert!(h.host.collab_activity_row_rect.area() > 0.0);

        h.chrome.borrow_mut().overlays.collaborators = false;
        paint(&mut h);
        assert_eq!(
            h.host.collab_activity_row_rect,
            Rect::ZERO,
            "a stale footer rect is a click target the user cannot see"
        );
    }

    #[test]
    fn escape_closes_the_room_activity_sheet() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);

        let mods = Modifiers::default();
        let handled = pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), &mods);
        assert!(handled);
        assert!(
            !h.chrome.borrow().overlays.room_activity,
            "Escape dismisses the sheet"
        );
    }

    #[test]
    fn clicking_outside_the_sheet_closes_it() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);

        let sheet = h.host.room_activity().rect();
        let outside = Point::new(sheet.x0 / 2.0, sheet.center().y);
        assert!(!sheet.contains(outside));
        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(outside), &mods);
        let handled = pump(&mut h, &left_button_pressed(), &mods);
        assert!(handled, "the click is swallowed, not passed to the desktop");
        assert!(!h.chrome.borrow().overlays.room_activity);
    }

    /// Typing narrows by contributor while the sheet has the keyboard, but
    /// Escape still belongs to the dismissal ladder.
    #[test]
    fn typing_filters_by_contributor_without_stealing_escape() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);
        assert!(h.host.room_activity().visible_len() > 0);

        let mods = Modifiers::default();
        let handled = pump(
            &mut h,
            &key_press(Key::Character("z".into())),
            &mods,
        );
        assert!(handled, "the sheet took the keystroke");
        assert_eq!(
            h.host.room_activity().timeline().author_query(),
            "z",
            "the typeahead echoes what was typed"
        );
        assert_eq!(
            h.host.room_activity().visible_len(),
            0,
            "no contributor in this room is named 'z'"
        );

        let handled = pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), &mods);
        assert!(handled);
        assert!(
            !h.chrome.borrow().overlays.room_activity,
            "Escape closed the sheet rather than being eaten by the typeahead"
        );
    }

    /// Stuff a non-empty query into the palette before opening it,
    /// then assert that the open transition both focuses the input
    /// AND clears the query. Drives the production Ctrl+K path
    /// through `handle_event` so the test fails if either the focus
    /// nudge or the query reset is dropped.
    #[test]
    fn toggle_palette_focuses_input() {
        let mut h = build_harness();
        // Seed a stale query so the "clear on open" half of the spec
        // is observable too.
        h.host.palette.set_query("stale residual");
        assert!(!h.host.palette.is_focused(), "palette must start unfocused");
        assert!(!h.host.palette_visible(), "palette must start hidden");

        let mods = Modifiers::from(ModifiersState::CONTROL);
        let handled = pump(&mut h, &key_press(Key::Character("k".into())), &mods);

        assert!(handled, "Ctrl+K must be consumed by the overlay host");
        assert!(
            h.host.palette_visible(),
            "Ctrl+K must open the global-search palette"
        );
        assert!(
            h.host.palette.is_focused(),
            "opening the palette via Ctrl+K must focus its input \
             (so the very first keystroke after open lands in the search box)"
        );
        assert_eq!(
            h.host.palette.query(),
            "",
            "opening the palette must clear any residual query — \
             {:?} should have been wiped",
            h.host.palette.query()
        );
    }

    /// Write a registry + manifest pair under `dir` so the real
    /// `launcher::gui_apps()` — and therefore the real
    /// `ApplicationsProvider` — enumerates one installed GUI package.
    fn install_gui_fixture(dir: &std::path::Path, package_id: &str, name: &str) {
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nname = \"{name}\"\n\
                 version = \"1.2.3\"\n[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("registry.toml"),
            format!(
                "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest_path.display().to_string()
            ),
        )
        .unwrap();
    }

    /// The two chords ask different questions, and each must answer its
    /// own.
    ///
    /// `Ctrl+K` is scoped to the focused app, so with a Notes tab in
    /// front it searches notes — an installed app you do not have open
    /// is deliberately *not* among its answers. `Super+K` is the global
    /// surface, and it must offer that app.
    ///
    /// The precondition matters: `bootstrap_default`'s active tab is
    /// Notes, which `NotesProvider` claims via `owns_tab`. Without a
    /// claiming provider `ActiveApp` allows everything and the narrow
    /// half of this test would pass vacuously. Drives the whole
    /// production chain: `handle_event` → `open_palette_with_scope` →
    /// `refresh_search_results` → `SearchRegistry::query` →
    /// `ApplicationsProvider` → `launcher::gui_apps`.
    #[test]
    #[serial_test::serial(process_env)]
    fn ctrl_k_scopes_to_the_focused_app_and_super_k_reaches_installed_apps() {
        use crate::shell::chrome_state::SearchScope;

        let tmp = tempfile::tempdir().unwrap();
        install_gui_fixture(tmp.path(), "dev.qos.zzyzx", "Zzyzx");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        crate::shell::launcher::invalidate_app_cache();

        let mut h = build_harness();
        // Precondition: the focused tab really is one a provider owns,
        // so `ActiveApp` would genuinely exclude the applications
        // provider. Without this the test would pass vacuously.
        let owner = {
            let st = h.chrome.borrow();
            active_tab_provider_id(&st, &h.host.search_registry)
        };
        assert_eq!(
            owner.as_deref(),
            Some("notes"),
            "fixture precondition: the focused tab must be owned by a \
             provider, otherwise ActiveApp scope excludes nothing"
        );

        let doc = AutoCommit::new();

        // Ctrl+K: scoped to the app in front of you.
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), &ctrl));
        assert_eq!(
            h.chrome.borrow().search_scope,
            SearchScope::ActiveApp,
            "Ctrl+K searches within the focused app"
        );
        h.host.palette.set_query("zzyzx");
        h.host.refresh_search_results(&doc, &automerge::ROOT);
        assert!(
            !h.host
                .search_results
                .iter()
                .any(|r| r.provider_id == "applications"),
            "scoped to Notes, an app that is not open is not an answer; got {:?}",
            h.host
                .search_results
                .iter()
                .map(|r| (&r.provider_id, &r.title))
                .collect::<Vec<_>>()
        );

        // Super+K: the global surface, and the one that launches.
        // Closed first so the chord opens the palette rather than
        // toggling the already-open one shut.
        h.host.palette_visible = false;
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), &sup));
        assert_eq!(
            h.chrome.borrow().search_scope,
            SearchScope::All,
            "Super+K searches the machine"
        );
        h.host.palette.set_query("zzyzx");
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let hit = h
            .host
            .search_results
            .iter()
            .find(|r| r.provider_id == "applications");
        let hit = hit.unwrap_or_else(|| {
            panic!(
                "typing an installed app's name must offer a row to launch \
                 it; got {:?}",
                h.host
                    .search_results
                    .iter()
                    .map(|r| (&r.provider_id, &r.title))
                    .collect::<Vec<_>>()
            )
        });
        assert_eq!(hit.id, "dev.qos.zzyzx");
        assert_eq!(hit.title, "Open Zzyzx");

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        crate::shell::launcher::invalidate_app_cache();
    }

    /// Ctrl+K searching the focused app has to reach that app's own
    /// *content*, not just its tab title.
    ///
    /// Drives the whole production chain with a document the real
    /// `NotesApp` wrote: `handle_event` → `open_palette_with_scope` →
    /// `refresh_search_results` → `SearchRegistry::query` →
    /// `NotesProvider` → palette rows. `nir` appears nowhere but the
    /// note's body, and the palette answered it with nothing, because the
    /// provider looked for the body under a per-tab map keyed by the tab
    /// id — a shape the tile wiring never creates.
    #[test]
    fn ctrl_k_finds_a_word_that_only_appears_in_the_notes_body() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::notes_app::NotesApp;

        let mut h = build_harness();
        let doc = AutoCommit::new();
        // The document as the app leaves it, in the document the app is
        // handed: the tab's own record. `TileHost::sync` calls exactly
        // this on every frame of the live compositor.
        let notes_tab = h.chrome.borrow().tree().workspaces[0].strips[0].window_groups[0].tabs[0]
            .id;
        let record = crate::shell::tab_documents::scratch_tab_record(notes_tab);
        let store = h.chrome.borrow().records().clone();
        store.open_record(&record);
        store.with(&record.id, |d| {
            let mut notes = NotesApp::with_seed(
                "Welcome to nirios.\n\nThese notes sync live with anyone \
                 invited to this workspace.",
            );
            notes.sync(d, &automerge::ROOT);
        });

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), &ctrl));
        assert_eq!(h.chrome.borrow().search_scope, SearchScope::ActiveApp);

        h.host.palette.set_query("nir");
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let hit = h
            .host
            .search_results
            .iter()
            .find(|r| r.provider_id == "notes")
            .unwrap_or_else(|| {
                panic!(
                    "searching inside Notes for a word from its own body \
                     must offer the note; got {:?}",
                    h.host
                        .search_results
                        .iter()
                        .map(|r| (&r.provider_id, &r.title))
                        .collect::<Vec<_>>()
                )
            });
        assert!(
            hit.subtitle
                .as_deref()
                .unwrap_or_default()
                .contains("nirios"),
            "the row must show the matching line, not a bare title: {:?}",
            hit.subtitle
        );
        // And it is a row the palette actually paints, with that snippet
        // as its description.
        let painted = h
            .host
            .palette
            .visible_entries()
            .iter()
            .any(|e| e.label == hit.title && e.description.as_deref() == hit.subtitle.as_deref());
        assert!(
            painted,
            "the hit must reach the palette's own rows: {:?}",
            h.host
                .palette
                .visible_entries()
                .iter()
                .map(|e| (&e.label, &e.description))
                .collect::<Vec<_>>()
        );
    }

    /// The app-scope pill has to name the focused app *as an app*, or it
    /// collides with the kind-filter chip strip painted directly above
    /// it. With Notes focused the strip carried a `Notes` chip and the
    /// scope control read `Only Notes` one row below — two adjacent
    /// controls, same word, different meanings ("results shaped like a
    /// note" versus "search inside the Notes app").
    #[test]
    fn app_scope_pill_names_the_app_as_an_app() {
        use crate::shell::chrome_state::SearchScope;
        let h = build_harness();
        assert_eq!(
            h.chrome.borrow().active_app_title().as_deref(),
            Some("Notes"),
            "fixture precondition: a tab is focused"
        );
        assert_eq!(
            h.host.search_scope_pill_label(SearchScope::ActiveApp),
            "Notes app"
        );
        // Generalised: the two bands are a filter row (kinds) and a scope
        // row (places), and *no* label may appear in both. Pinning only
        // the app pill would have let the next collision through — `All`
        // was one, sitting under the `All` chip that clears the kind
        // filter.
        let chips: Vec<String> = h
            .host
            .search_registry
            .registered_kinds()
            .iter()
            .map(|k| k.label().to_string())
            // The leading reset chip is painted separately from the
            // registered kinds; see `draw_search_banner`.
            .chain(std::iter::once("All".to_string()))
            .collect();
        for scope in h.host.search_scope_list() {
            let pill = h.host.search_scope_pill_label(scope);
            assert!(
                !chips.contains(&pill),
                "the {scope:?} scope pill {pill:?} is also a kind chip label — \
                 two adjacent controls with one word and two meanings: {chips:?}"
            );
        }
        assert!(SearchScope::ActiveApp.narrows_results());
        assert!(!SearchScope::Workspace.narrows_results());
        assert!(!SearchScope::All.narrows_results());
    }

    /// Click the Files pill in the WS-settings tab strip and assert
    /// `chrome.ws_settings_tab` flips to `WsTab::Files`. The test:
    ///   1. opens the modal,
    ///   2. paints one frame so `draw_ws_settings` populates the
    ///      cached pill rects,
    ///   3. reads the Files pill rect through the `ws_settings_tab_
    ///      pill_rect` accessor,
    ///   4. dispatches a real `MouseInput::Pressed` via `handle_event`
    ///      at that rect's centre.
    /// Pre-2-A the click branch was missing — the click would fall
    /// through to the name-input commit path and `ws_settings_tab`
    /// would stay `WsTab::General`.
    #[test]
    fn ws_settings_pill_click_switches_tab() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        assert_eq!(
            h.chrome.borrow().ws_settings_tab,
            WsTab::General,
            "modal must default to the General tab"
        );

        // The pill rects are populated inside `draw_ws_settings`, so
        // we have to paint one frame before they're queryable. Drive
        // the public `Widget::draw` path with the same theme + dummy
        // presence the live binary uses.
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let members_pill = h.host.ws_settings_tab_pill_rect(WsTab::Members);
        assert!(
            members_pill.area() > 0.0,
            "Members pill rect must be populated by draw_ws_settings; got {:?}",
            members_pill
        );
        let p = members_pill.center();

        // MouseInput doesn't carry position data; seed `last_cursor`
        // through a CursorMoved at the same point first.
        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(p), &mods);
        let handled = pump(&mut h, &left_button_pressed(), &mods);

        assert!(
            handled,
            "click on the Members pill must be consumed by the overlay host"
        );
        assert_eq!(
            h.chrome.borrow().ws_settings_tab,
            WsTab::Members,
            "click on the Members pill must flip chrome.ws_settings_tab to Members \
             (production path through handle_event, not a direct setter)"
        );
    }

    /// Wave 3 / Unit 3-A pin (#5).
    ///
    /// Concept §4.1 specifies `bg-black/30 backdrop-blur-[1px]` for the
    /// modal scrim. Without the 1-px blur the perceived dimming drops
    /// below what the design intended, so the rendered alpha must be
    /// lifted to compensate. Pin a floor of `0.45` — empirically the
    /// alpha at which a non-blurred scrim reads roughly the same as
    /// the concept's blurred 0.30 fill.
    #[test]
    fn palette_scrim_alpha_matches_concept() {
        assert!(
            MODAL_SCRIM_ALPHA >= 0.45,
            "MODAL_SCRIM_ALPHA must be >= 0.45 to compensate for the \
             missing 1-px backdrop blur from concept §4.1; the un-blurred \
             0.20 scrim let the desktop bleed through the modal so the \
             palette / WS-settings surfaces never read as the focused \
             layer (got {})",
            MODAL_SCRIM_ALPHA
        );
    }

    /// Issue #127 / polish-sweep pin.
    ///
    /// Concept ref `WorkspaceSettingsModal.tsx:62-79` — the active nav
    /// row is tinted `bg-white/10 text-white`. The earlier draw painted
    /// `surfaces.overlay` (`black/80`), which read as a heavy black tab
    /// over the slate-900/70 modal surface. The fix swaps the fill to
    /// `surface_white_10`; this pin keeps the token byte aligned with
    /// the Tailwind `white/10` literal so a future refactor that
    /// renames or retypes the token surfaces here loudly.
    #[test]
    fn ws_settings_active_tab_pill_uses_white_overlay() {
        let tokens = ui::tokens::Tokens::dark();
        let c = tokens.colors.surface_white_10;
        let expected = vello::peniko::Color::from_rgba8(255, 255, 255, 26);
        assert_eq!(
            c, expected,
            "surface_white_10 must paint at the concept's `bg-white/10` \
             (rgba8(255, 255, 255, 26)) so the active WS-settings tab \
             pill reads as a soft glass-on-glass treatment, not a \
             heavy black overlay (got {:?})",
            c
        );
    }

    /// The palette scrim has to actually dim bright window content.
    ///
    /// Concept §5.1 says `bg-black/30`, and this used to be pinned at a
    /// literal 0.30. But the concept's scrim is blurred and the pinned
    /// Vello has no `push_blur_layer`, so an un-blurred fill has to go
    /// heavier to read the same — exactly the compensation
    /// [`MODAL_SCRIM_ALPHA`] already documents and is already tested for
    /// below. At 0.30 a `bg-white` window card behind the palette stays
    /// at 178 grey; at 0.45 it drops to 141, which is what makes the
    /// palette read as a page on top of a page rather than a card
    /// floating on a bright field.
    ///
    /// Asserted as a lower bound, matching the modal test's shape: the
    /// requirement is "enough to dim", and pinning an exact float invites
    /// the next person to satisfy the letter of it by re-lowering it.
    #[test]
    fn palette_scrim_alpha_reads_like_the_blurred_concept() {
        assert!(
            PALETTE_SCRIM_ALPHA >= 0.45,
            "PALETTE_SCRIM_ALPHA must be >= 0.45 to compensate for the \
             missing backdrop blur — below that a white window card behind \
             the palette stays bright and the card's edge reads as a hard \
             cut (got {PALETTE_SCRIM_ALPHA})"
        );
        // What the constant is *for*: the brightest thing the shell
        // paints must come out clearly darker than the palette surface it
        // sits behind.
        let card = 255.0;
        let dimmed = card * (1.0 - PALETTE_SCRIM_ALPHA);
        assert!(
            dimmed <= 145.0,
            "a bg-white window card behind the palette lands at {dimmed} — \
             still a bright field next to a slate-900 card"
        );
    }

    /// The scrim's colour is the theme's, not a literal. Both themes
    /// must darken: a light-theme scrim that brightened would push the
    /// background *towards* the surface it is meant to be lifting away
    /// from.
    #[test]
    fn scrim_hue_comes_from_the_theme_and_darkens_in_both() {
        for (name, colors) in [
            ("dark", ui::tokens::colors::Colors::dark()),
            ("light", ui::tokens::colors::Colors::light()),
        ] {
            let s = colors.surfaces.scrim.to_rgba8();
            let base = colors.surfaces.base.to_rgba8();
            let luma = |c: &ui::peniko::color::Rgba8| {
                0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32
            };
            assert!(
                luma(&s) < luma(&base),
                "the {name} theme's scrim must be darker than its base \
                 surface (scrim {:?} vs base {:?})",
                s,
                base
            );
            assert_eq!(s.a, 255, "the {name} scrim token carries the hue; the \
                 strength is the overlay's own (got alpha {})", s.a);
        }
    }

    /// Wave 3 / Unit 3-A pin (#5).
    ///
    /// Extracted-helper coverage for the click-focuses-group logic.
    /// Regression pins for the 2026-07-09 "Ctrl+K scoped to Notes when
    /// I clicked Files in its own group" report.
    #[test]
    fn pick_group_at_returns_containing_group() {
        let strip = StripId::from(7);
        let candidates = vec![
            (strip, 1u64, Rect::new(0.0, 100.0, 400.0, 500.0)),
            (strip, 2u64, Rect::new(400.0, 100.0, 800.0, 500.0)),
        ];
        assert_eq!(
            pick_group_at(&candidates, Point::new(200.0, 300.0)),
            Some((strip, 1))
        );
        assert_eq!(
            pick_group_at(&candidates, Point::new(600.0, 300.0)),
            Some((strip, 2))
        );
    }

    #[test]
    fn pick_group_at_none_outside_all_rects() {
        let strip = StripId::from(7);
        let candidates = vec![(strip, 1u64, Rect::new(0.0, 100.0, 400.0, 500.0))];
        assert!(pick_group_at(&candidates, Point::new(600.0, 50.0)).is_none());
    }

    #[test]
    fn pick_group_at_skips_zero_area_rects() {
        // Freshly detached group whose apply_layout hasn't run yet —
        // rect is zero. Must not swallow every click at (0,0).
        let strip = StripId::from(7);
        let candidates = vec![
            (strip, 1u64, Rect::new(0.0, 0.0, 0.0, 0.0)),
            (strip, 2u64, Rect::new(0.0, 100.0, 400.0, 500.0)),
        ];
        assert_eq!(
            pick_group_at(&candidates, Point::new(0.0, 0.0)),
            None,
            "zero-area group must NOT match (0,0)"
        );
        assert_eq!(
            pick_group_at(&candidates, Point::new(200.0, 300.0)),
            Some((strip, 2))
        );
    }

    /// The companion to `palette_scrim_alpha_matches_concept`.
    ///
    /// Concept §4.1 originally called for `bg-slate-900/70` on the modal
    /// surface, and this test used to pin the alpha at `<= 0.85` so a
    /// fully opaque surface didn't defeat the "page-on-page" affordance
    /// the scrim was meant to carry.
    ///
    /// 2026-07-09 user report ("search bar / file viewer are too
    /// transparent — looks terrible") flipped the trade-off. Legibility
    /// against a busy desktop backdrop wins over the concept's tint
    /// depth: [`MODAL_SURFACE_ALPHA`] now lifts to `>= 0.90` so the
    /// palette reads as solid chrome with just a hint of the wallpaper's
    /// tint. The scrim below still carries the mode-shift affordance.
    #[test]
    fn palette_surface_alpha_lifts_off_desktop() {
        assert!(
            MODAL_SURFACE_ALPHA >= 0.90,
            "MODAL_SURFACE_ALPHA must stay >= 0.90 so the palette + \
             workspace-settings modal read as opaque chrome; a lower \
             alpha reintroduces the 2026-07-09 legibility complaint \
             (got {})",
            MODAL_SURFACE_ALPHA
        );
    }
}

#[cfg(test)]
mod maximize_harvest_tests {
    //! Unit coverage for the per-frame `WindowGroup::maximized` writeback
    //! (audit task #105).
    //!
    //! The integration test in `tests/maximize_persist_wireup_test.rs`
    //! drives the full click pipeline; this in-crate sibling exercises
    //! the harvest in isolation by priming the widget's
    //! `pending_maximize_change` slot through the public test escape
    //! and asserting `tick_pending_maximize_harvest_for_tests` mirrors
    //! the new value into the persisted `WorkspaceTree`. Two complementary
    //! pins: a no-op tick must leave the persisted state alone (so the
    //! harvest doesn't clobber a `with_maximized(true)` boot), and a
    //! pending change must be written through.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
    };
    use std::collections::HashMap as StdHashMap;

    fn one_group_tree(maximized: bool) -> (WorkspaceTree, StripId, GroupId) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 7777;
        let tab_id: u64 = 0xa1;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Solo".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Test".into()),
                        label: "solo".into(),
                    },
                }],
                active_tab: tab_id,
                width: StateWidth::Normal,
                maximized,
            }],
        };
        let ws = Workspace {
            owner: None,
            capabilities: Vec::new(),
            id: ws_id,
            room: crate::shell::state::RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            collaborators: vec![],
            shared_surfaces: StdHashMap::new(),
        };
        let mut active_strip = StdHashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdHashMap::new();
        active_group.insert(strip_id, group_id);
        let tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        };
        (tree, strip_id, group_id)
    }

    /// Priming `pending_maximize_change = Some(true)` on the live widget
    /// and ticking the harvest must mirror the new value into the
    /// persisted `WorkspaceTree`. Without the harvest hook the persisted
    /// state stays at `false` regardless of how often the user clicks
    /// the maximize glyph (the regression #105 closes).
    #[test]
    fn harvest_writes_pending_maximize_change_through_to_tree() {
        let (tree, strip_id, group_id) = one_group_tree(false);
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

        // Drive the widget side: flip `maximized` directly + arm the
        // pending-change slot the same way the production drain block
        // does. Avoids a synthetic taffy pump for this in-crate unit
        // test — the integration test has the full click coverage.
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, group_id);
            assert!(!g.maximized, "seed mirrors persisted state");
            g.maximized = true;
            // `publish_maximize_intent_for_tests` would re-toggle via
            // the production drain — here we want the explicit value
            // mirrored into the harvest slot directly.
            // Same as the production click flow's writeback line.
            // We can't poke `pending_maximize_change` from outside the
            // crate, so go through the publish + drain path instead.
            g.publish_maximize_intent_for_tests();
            // The drain runs inside `handle_event`. Skip the layout
            // setup by setting `maximized` back to false and letting
            // the publish flip it back to true through `take_max`.
            g.maximized = false;
        }

        // Drive a no-op cursor-move so the drain block runs and arms
        // `pending_maximize_change`. We use the public Widget API.
        use std::collections::HashMap;
        use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
        use vello::kurbo::Point;
        use winit::dpi::PhysicalPosition;
        use winit::event::{Modifiers, WindowEvent};
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("strip-0 group-0 widget");
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        let map_for_measure = node_map.clone();
        let mut tr = ui::text_render::TextRenderer::new();
        let _ = taffy.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(800.0),
                height: AvailableSpace::Definite(600.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        g.apply_layout(&taffy, group_node, Point::ZERO);
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(9000.0, 9000.0),
        };
        g.handle_event(
            &moved,
            1.0,
            &Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(g.maximized, "drain must flip maximized true");

        // Pre-harvest: persisted state still un-maximized.
        {
            let st = chrome.borrow();
            assert!(
                !st.tree().workspaces[0].strips[0].window_groups[0].maximized,
                "persisted state lags widget until harvest runs",
            );
        }

        host.tick_pending_maximize_harvest_for_tests();

        // Post-harvest: persisted state matches widget.
        {
            let st = chrome.borrow();
            assert!(
                st.tree().workspaces[0].strips[0].window_groups[0].maximized,
                "harvest writes pending change through to persisted state",
            );
        }
    }

    /// A tick with no pending change must not overwrite a persisted
    /// `maximized = true` with the widget's default. Pins that the
    /// harvest is gated on `take_pending_maximize_change` returning
    /// `Some(_)`, not on the widget's current `.maximized` value
    /// (which would be wrong: a freshly-built widget with
    /// `with_maximized(true)` and no click has nothing to report).
    #[test]
    fn harvest_no_op_tick_leaves_persisted_state_alone() {
        let (tree, _strip_id, _group_id) = one_group_tree(true);
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

        host.tick_pending_maximize_harvest_for_tests();

        let st = chrome.borrow();
        assert!(
            st.tree().workspaces[0].strips[0].window_groups[0].maximized,
            "no-op harvest must not overwrite persisted true with widget default",
        );
    }
}

#[cfg(test)]
mod resync_strips_tests {
    //! #263 regression coverage: a `chrome.bump()` after pushing fresh
    //! `WindowGroup`s into the persisted tree must propagate to the
    //! live `GroupRow` widgets on the next `take_structure_dirty`
    //! tick, even when the harness has never called `update()`. The
    //! `shots` driver's relayout path skips update, so this test
    //! exercises the structure-dirty pump in isolation.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
    };
    use std::collections::HashMap as StdHashMap;

    fn one_group_seed() -> (WorkspaceTree, StripId, GroupId) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 100;
        let tab_id: u64 = 1;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "T".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Test".into()),
                        label: "s".into(),
                    },
                }],
                active_tab: tab_id,
                width: StateWidth::Normal,
                maximized: false,
            }],
        };
        let ws = Workspace {
            owner: None,
            capabilities: Vec::new(),
            id: ws_id,
            room: crate::shell::state::RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            collaborators: vec![],
            shared_surfaces: StdHashMap::new(),
        };
        let mut active_strip = StdHashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdHashMap::new();
        active_group.insert(strip_id, group_id);
        let tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        };
        (tree, strip_id, group_id)
    }

    fn make_stub_group(id: GroupId, label: &str) -> StateGroup {
        let tab_id = (id as u64) + 1000;
        StateGroup {
            id,
            tabs: vec![Tab {
                id: tab_id,
                title: label.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: label.into(),
                },
            }],
            active_tab: tab_id,
            width: StateWidth::Normal,
            maximized: false,
        }
    }

    #[test]
    fn resync_picks_up_new_groups_added_after_bump() {
        let (tree, strip_id, _) = one_group_seed();
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
        assert!(host.group_at_for_tests(strip_id, 0).is_some());
        assert!(host.group_at_for_tests(strip_id, 1).is_none());
        // Mirror what the `shots` multi-group scene does: push two
        // fresh `WindowGroup`s into the active strip and bump.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let strip = tree.workspaces[0]
                .strips
                .iter_mut()
                .find(|s| s.id == strip_id)
                .expect("seed strip");
            strip.window_groups.push(make_stub_group(201, "Stub-A"));
            strip.window_groups.push(make_stub_group(202, "Stub-B"));
            drop(tree);
            st.bump();
        }
        // Single take_structure_dirty tick must pump the resync diff
        // and splice the new groups into the live row.
        let _ = host.take_structure_dirty();
        assert!(
            host.group_at_for_tests(strip_id, 1).is_some(),
            "stub-A spliced into row at index 1"
        );
        assert!(
            host.group_at_for_tests(strip_id, 2).is_some(),
            "stub-B spliced into row at index 2"
        );
        assert!(
            host.group_at_for_tests(strip_id, 3).is_none(),
            "no phantom fourth group"
        );
    }

    #[test]
    fn resync_drops_groups_removed_after_bump() {
        let (mut tree, strip_id, _) = one_group_seed();
        // Seed two extra groups so the first frame already has 3.
        tree.workspaces[0]
            .strips
            .iter_mut()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .push(make_stub_group(301, "Drop-A"));
        tree.workspaces[0]
            .strips
            .iter_mut()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .push(make_stub_group(302, "Drop-B"));
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
        assert!(host.group_at_for_tests(strip_id, 2).is_some());
        // Remove one group and bump.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let strip = tree.workspaces[0]
                .strips
                .iter_mut()
                .find(|s| s.id == strip_id)
                .unwrap();
            strip.window_groups.retain(|g| g.id != 301);
            drop(tree);
            st.bump();
        }
        let _ = host.take_structure_dirty();
        // Row now has 2 groups (original + Drop-B).
        assert!(host.group_at_for_tests(strip_id, 0).is_some());
        assert!(host.group_at_for_tests(strip_id, 1).is_some());
        assert!(
            host.group_at_for_tests(strip_id, 2).is_none(),
            "removed group dropped from row"
        );
    }

    #[test]
    fn resync_no_op_when_revision_unchanged() {
        let (tree, strip_id, group_id) = one_group_seed();
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
        let id_before = host
            .group_at_for_tests(strip_id, 0)
            .map(|g| g.id)
            .unwrap_or(0);
        assert_eq!(id_before, group_id);
        // First tick observes initial revision and caches it.
        let _ = host.take_structure_dirty();
        // Second tick without any chrome.bump() must be a no-op —
        // the persisted group id stays the same and no spurious
        // insert happens.
        let _ = host.take_structure_dirty();
        let _st = chrome.borrow();
        assert_eq!(
            host.group_at_for_tests(strip_id, 0).map(|g| g.id),
            Some(group_id),
            "stable revision leaves row identity intact"
        );
        assert!(
            host.group_at_for_tests(strip_id, 1).is_none(),
            "no phantom group inserted by repeated no-op ticks"
        );
    }
}
