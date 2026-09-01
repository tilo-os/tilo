//! Shell overlay host — wraps the root content widget and draws modal
//! overlays (command palette, overview, quick settings) on top.
//!
//! `build_nirios_root` returns this widget so the shell has exactly one
//! place to route global keyboard events and one place to render the
//! overlay stack. Overlays own their own state; the host gates their
//! visibility and paints the scrim.
//!
//! `handle_event` consumes the palette-toggle and Esc before propagating
//! to the wrapped content, and an open palette gets first crack at every
//! event so typing and Enter work without the desktop stealing focus.

use std::collections::HashMap;
use std::path::PathBuf;
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
    Button, ButtonFace, CrossStripDetach, GroupRow, OutputSpan, PROMPT_HEIGHT, PermissionPrompt,
    PromptChoice, Widget, WindowGroup as UiWindowGroup,
};
use ui::{CommandEntry, CommandPalette};

use crate::shell::chrome_state::{Chrome, WsTab};
use crate::shell::collaborators_overlay::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay,
};
pub use crate::shell::collaborators_overlay::{RosterRow, collaborator_roster_rows};
use crate::shell::file_picker_overlay::FilePickerOverlay;
pub use crate::shell::file_picker_overlay::file_picker_rect;
use crate::shell::notification::NotificationStack;
use crate::shell::peer_sync_overlay::{EventOutcome as PeerSyncEventOutcome, PeerSyncOverlay};
pub use crate::shell::peer_sync_overlay::{PeerSyncRequest, PeerSyncSnapshot};
use crate::shell::power_menu::{PowerMenu, PowerMenuClick};
use crate::shell::projection_overlay::ProjectionOverlay;
use crate::shell::quick_settings_bluetooth::QuickSettingsBluetoothPane;
use crate::shell::quick_settings_drill_in::{DrillInList, DrillInOutcome};
use crate::shell::quick_settings_vpn::QuickSettingsVpnPane;
use crate::shell::quick_settings_wifi::QuickSettingsWifiPane;
use crate::shell::sidebar_wheel_disc::SidebarWheelDisc;
use crate::shell::state::{
    Capability, GRANT_ALWAYS, GRANT_SESSION, GrantScope, GroupId, StripId, WorkspaceId,
    WorkspaceTree,
};
use crate::shell::workspace_settings::{
    CapEdit, DefaultAppsCtx, EXPIRY_CHOICES, default_app_rows, draw_default_apps_pane,
    draw_members_pane, draw_permissions_pane, members_pane_rows, permissions_pane_layout,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum HistoryTargetAddress {
    File(PathBuf),
    Uri(String),
    App,
}

/// Resolve only addresses the action explicitly declared. Semantic resource
/// ids such as `notes.page:abc` are labels within an app, not record ids or
/// URIs; treating them as either would make History's Open control navigate
/// somewhere other than the selected action.
fn history_target_address(target: &ui::HistoryResourceTarget) -> HistoryTargetAddress {
    if let Some(path) = target.id.strip_prefix("file:") {
        return HistoryTargetAddress::File(PathBuf::from(path));
    }
    if target.id.contains("://") && crate::shell::uri_open::resource_for_uri(&target.id).is_ok() {
        return HistoryTargetAddress::Uri(target.id.clone());
    }
    HistoryTargetAddress::App
}

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

/// Side-effect dispatcher for a peer-sync request (`/invite`, `/connect`),
/// bridging the transport-agnostic widget layer to `ShellNetwork`.
type PeerSyncBridge = std::rc::Rc<dyn Fn(PeerSyncRequest) -> Result<String, String>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-toggled, documented piece of overlay/transition \
              state (palette visibility, quick-settings open edge, peer-sync enablement, ...); \
              folding them into an enum or bitflags would not group any of them meaningfully \
              and would ripple through this whole file's field accesses"
)]
pub struct ShellOverlayHost {
    rect: Rect,
    content: Box<dyn Widget>,
    /// Trusted first-login chrome. While present it is the entire root
    /// surface: desktop content, launch affordances, and every other overlay
    /// stay unreachable until a real identity has been provisioned and the
    /// user dismisses the result screen.
    onboarding: Option<crate::onboarding_view::OnboardingView>,
    /// One-shot relayout edge raised when onboarding releases the desktop.
    onboarding_released: bool,
    /// One-shot focus edge paired with [`Self::onboarding_released`]. Kept
    /// separate because relayout can consume its edge before the next input
    /// dispatch clears the compositor's cached widget node.
    onboarding_focus_reset: bool,
    /// Floating sidebar wheel. Optional so tests can build a bare host
    /// without one; production always provides it.
    sidebar: Option<Box<dyn Widget>>,
    /// Displays the compositor is painting across, pushed by `FrameBuilder`
    /// before layout. Empty in a bare `ui::App` host, where
    /// [`Self::strip_output_spans`] treats the window as the one display.
    outputs: Vec<OutputGeometry>,
    palette: CommandPalette,
    palette_visible: bool,
    /// Search providers the palette fans out to. Built once at
    /// construction — providers are stateful (own background indexers),
    /// so re-registration would leak worker threads.
    search_registry: crate::shell::search::SearchRegistry,
    /// Backs the current palette entries, indexed in lock-step with
    /// `palette.entries` so Enter/click resolves by ordinal.
    search_results: Vec<crate::shell::search::SearchResult>,
    /// `palette.query()` after the last fan-out; refresh is
    /// short-circuited when this still matches.
    last_search_query: String,
    /// Kinds the chip strip has toggled on. Empty means unfiltered.
    active_kind_filter: Vec<crate::shell::search::ResourceKind>,
    /// Painted chip rects this frame, indexed like
    /// [`Self::search_registry`]'s `registered_kinds`. Cached from draw
    /// so `handle_event` hit-tests without recomputing geometry.
    search_chip_rects: Vec<Rect>,
    /// Rect of the leading "All" chip, cached from draw for hit-testing.
    search_all_chip_rect: Rect,
    /// Chip rows the filter band is reserving. Changing it tells the
    /// palette to grow so lower rows aren't painted over. See
    /// [`Self::draw_search_banner`].
    search_chip_rows: usize,
    /// Records each room holds, shared with the records search provider.
    room_records: std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
    /// Set whenever [`Self::refresh_search_results`] rewrites the
    /// palette's entries, so `take_structure_dirty` rebuilds the taffy
    /// node with the new row count — without this only ~1 result shows.
    palette_layout_dirty: bool,
    /// Host width the palette's taffy style was last computed against.
    /// See the sizing comment in `build_layout`.
    palette_layout_host_w: f64,
    content_node: Option<NodeId>,
    palette_node: Option<NodeId>,
    sidebar_node: Option<NodeId>,
    /// Shared chrome state; drives whether overview / quick-settings /
    /// ws-settings draw on top of the content.
    chrome: Option<Chrome>,
    /// Last known cursor position, for hit-testing overlay clicks.
    last_cursor: Option<Point>,
    /// Overview crossfade: `overview_t` ∈ [0, 1], animating toward
    /// `overview_target` (latched on toggle). `transition_started`/
    /// `transition_from` let a re-toggle mid-fade continue from the
    /// current alpha instead of popping to 0 or 1.
    overview_t: f32,
    overview_target: f32,
    transition_started: Option<Instant>,
    transition_from: f32,
    /// Scroll offset (px) for the overview's room list, clamped each
    /// frame so a workspace deletion can't strand it past the tail.
    overview_scroll_y: f64,
    /// Keyboard target in the overview switcher: arrows walk it, Enter
    /// goes there, Esc leaves it alone. `None` while closed; re-seeded
    /// from the current room each time it opens.
    overview_selection: Option<usize>,
    /// Interruptible open/close driver for quick-settings, so it fades +
    /// slides out on close instead of vanishing.
    qs_transition: ui::Transition,
    /// Previous-frame quick-settings visibility, to detect the open edge.
    qs_was_open: bool,
    /// Quick-settings slider being dragged (0 = brightness, 1 = volume).
    /// The `os_action` shell-out fires only on Released so a drag doesn't
    /// spawn a subprocess per pixel of motion.
    qs_slider_drag: Option<usize>,
    /// Session action modal, including destructive-action arming and its
    /// own open/close transition.
    power_menu: PowerMenu,
    /// When the workspace invite link was last copied, to confirm it.
    ws_invite_copied_at: Option<Instant>,
    /// Ribbon segment under the pointer, if any. Paint-only.
    strip_ribbon_hover: Option<usize>,
    /// Same as [`Self::strip_ribbon_hover`] for the strip rail.
    strip_rail_hover: Option<usize>,
    /// Off-thread reader for the OS state quick settings displays.
    /// See [`crate::shell::qs_refresh`] for why the reads left the frame.
    qs_refresh: crate::shell::qs_refresh::QsRefresh,
    ws_settings_was_open: bool,
    /// Interruptible open/close driver for the workspace-settings modal
    /// — like `qs_transition` but for the modal, so it fades + scales
    /// OUT on close instead of vanishing.
    ws_settings_transition: ui::Transition,
    palette_was_visible: bool,
    /// Interruptible open/close driver for the command palette.
    palette_transition: ui::Transition,
    /// Per-strip raw pointers to the [`GroupRow`] widgets inside
    /// `content`, so the per-frame reconcilers reach them without
    /// downcasting `&dyn Widget`. Empty until
    /// [`Self::with_group_row_ptrs`] is called.
    ///
    /// SAFETY: each pointer is the heap address of a `GroupRow` owned by
    /// the chrome's content tree. `Box<T>` -> `Box<dyn Widget>` unsizing
    /// keeps the heap allocation at the same address, and the host owns
    /// the content for the rest of the process lifetime, so the pointer
    /// remains valid as long as `&mut self` does.
    group_row_ptrs: HashMap<StripId, *mut GroupRow>,
    /// Workspace id the current `content` was built for.
    /// [`Self::resync_workspace_content`] rebuilds when this disagrees
    /// with `tree.active`. `None` means unbuilt/test stub: the
    /// reconciler only latches the id without rebuilding, so hosts
    /// constructed around hand-made content keep it.
    content_built_for_ws: Option<u64>,
    /// Per-strip last active-group id that
    /// [`Self::scroll_active_group_into_view`] scrolled into view, so the
    /// focus-follow scroll is edge-triggered and doesn't fight a user who
    /// wheels away.
    last_scrolled_active_group: HashMap<StripId, GroupId>,
    /// Hit rect for the "+ Add workspace" affordance at the tail of the
    /// overview's list. `Rect::ZERO` when not painted.
    add_workspace_rect: Rect,
    /// Hit rects for the WS-settings tab pills, in [`WsTab::all`] order.
    /// `Rect::ZERO` when not painted, so a stale click during a
    /// crossfade can't flip tabs after the modal is gone.
    ws_settings_tab_pill_rects: [Rect; 4],
    /// Apps & Files rows: resource, workspace-policy control, personal
    /// workspace-choice control. Geometry is painted and hit-tested from the
    /// same cache.
    default_app_rects: Vec<(desktop_policy::ResourceKind, Rect, Rect)>,
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
    /// Vertical inset at which the floating sidebar wheel starts.
    /// Defaults to [`SIDEBAR_TOP_OFFSET_PX`]; `with_sidebar_top`
    /// overrides it so the real shell's padded topbar doesn't overlap.
    sidebar_top_y: f32,
    /// Live permission-prompt widget, populated by
    /// `pump_permission_prompt` from the front of
    /// `chrome.pending_grants`. `None` when the queue is empty.
    permission_prompt: Option<PermissionPrompt>,
    /// The portal's prompt queue, when the compositor serves portals.
    /// `drain_portal_prompts` empties it onto `chrome.pending_grants`.
    portal_prompts: Option<crate::portal::PromptQueueHandle>,
    /// Owns the portal file chooser's queue, modal state, rendering and
    /// exclusive input routing. Keeping those together prevents the host
    /// from growing another set of loosely-coupled modal fields.
    file_picker_overlay: FilePickerOverlay,
    /// Whether peer-sync palette commands (`/invite`, `/connect`) are
    /// surfaced. Off by default; set via `--enable-peer-sync` or
    /// [`Self::with_peer_sync_enabled`].
    peer_sync_enabled: bool,
    /// Queue of peer-sync actions from [`Self::dispatch_palette_command`],
    /// drained by the compositor into its `ShellNetwork` handle.
    peer_sync_actions: Vec<PeerSyncAction>,
    /// Closure that carries out a peer-sync request against
    /// `ShellNetwork`, keeping the widget layer transport-agnostic.
    /// `None` in tests and in sessions booted without peer-sync, where
    /// `/invite` and `/connect` report that rather than doing nothing.
    peer_sync_bridge: Option<PeerSyncBridge>,
    /// Owns the invite/connect prompt state, painting, hit-testing and input.
    peer_sync_overlay: PeerSyncOverlay,
    /// Top-right notification stack, above content but below the
    /// permission prompt and modals. `None` until [`Self::with_chrome`]
    /// runs.
    notification_stack: Option<NotificationStack>,
    /// Last `chrome.revision` seen by [`Self::resync_strips_from_tree`];
    /// a disagreement means the tree changed shape and live
    /// [`GroupRow`] widgets need the new strip contents. `u64::MAX` at
    /// construction so the first poll always rebuilds.
    last_chrome_revision: u64,
    /// What each live tab was last built to show, keyed by tab id — the
    /// revision counter says something moved, this says which tabs, so a
    /// tab whose content changed without its id changing still gets
    /// rebuilt.
    tab_tiles: std::collections::HashMap<u64, String>,
    /// Side-effect dispatcher for quick-settings actions. Production
    /// uses [`crate::shell::qs_actions::RealQsActions`]; tests swap in a
    /// recording impl via [`Self::with_qs_actions`].
    qs_actions: Box<dyn crate::shell::qs_actions::QsActions>,
    /// Wi-Fi network-list layout, painting and join hit-testing. The
    /// specialised ephemeral password editor remains host-owned.
    qs_wifi_pane: QuickSettingsWifiPane,
    /// Bluetooth discovery list layout, painting and back hit-testing.
    qs_bluetooth_pane: QuickSettingsBluetoothPane,
    /// Shared drill-in chrome for host-owned notification/password panes.
    qs_aux_drill_in: DrillInList,
    /// VPN drill-in geometry, shared toggle painting, click routing and OS
    /// dispatch. Chrome remains the source of the profile snapshot.
    qs_vpn_pane: QuickSettingsVpnPane,
    /// Live battery/network/volume readings, or `None` when nothing
    /// polls — `--shots` and headless tests have no poller so their
    /// pixels don't depend on host hardware. See
    /// [`crate::shell::status`].
    status_bridge: Option<crate::shell::status::StatusBridge>,
    /// Single session-lifetime owner for persistent balance/mono transforms.
    /// Present only in real compositor sessions, never shots or unit tests.
    session_audio_policy: Option<crate::shell::session_audio_policy::SessionAudioPolicyOwner>,
    /// Room-activity sheet. Held rather than drawn immediate-mode
    /// because it is a real `Widget` with its own hover/scroll/filter
    /// state, placed by hand like the notification stack.
    room_activity: crate::shell::room_activity::RoomActivityPane,
    /// Open/close curve for the room-activity sheet.
    room_activity_transition: ui::Transition,
    /// Whether the sheet was open last frame, to invalidate derived rows
    /// on the rising edge.
    room_activity_was_open: bool,
    /// Collaborator roster popover, including its painted geometry and hit
    /// targets. The host only applies its semantic actions to chrome state.
    collaborators_overlay: CollaboratorsOverlay,
    /// Peer whose live tab focus this session mirrors. Local-only and
    /// ephemeral: follow is a viewing mode, never shared workspace state.
    followed_peer: Option<String>,
    /// Persistent follow-mode banner painted while following.
    follow_chip_rect: Rect,
    /// Explicit stop target within [`Self::follow_chip_rect`]. The rest of
    /// the banner is informative, not a surprising destructive target.
    follow_stop_rect: Rect,
    /// Complete wireless projection picker, including controller, editor,
    /// animation, painted geometry, and event routing.
    projection_overlay: ProjectionOverlay,
}

/// Action emitted from the palette into [`ShellOverlayHost::peer_sync_actions`]
/// when the user picks a peer-sync command; the compositor drains these
/// into its `ShellNetwork` handle so Iroh side-effects stay on the
/// runtime thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncAction {
    /// User invoked `/invite`. The compositor calls
    /// `ShellNetwork::issue_invite` and puts the token on the clipboard.
    IssueInvite,
    /// Mint an invite for `name`, copying it to the clipboard when
    /// `copy`. The name is written into the room document once the
    /// invitee turns up — it never rides inside the token itself.
    IssueInviteFor { name: String, copy: bool },
    /// Open the invite composer, before anything is minted. What the
    /// collaborators panel's Invite button emits.
    PromptInvite,
    /// User invoked `/connect`. Opens the token-input prompt
    /// ([`ShellOverlayHost::draw_peer_sync`]); submitting it sends a
    /// [`PeerSyncRequest::AcceptInvite`] through the bridge.
    PromptConnect,
}

/// Rows the palette asks the registry for, shared round-robin between the
/// matching kinds. Must equal what the card actually paints — asking for
/// 16 let a three-group answer's third header land off the bottom of the
/// card. Eight keeps every group's header on screen, each showing the
/// total it's holding back, so clicking its chip is how you see the rest.
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
        // Deliberately *not* the applications glyph. A host app is not
        // one of qOS's — it runs unconfined, outside everything the
        // grid stands for — and the row has to look different at a
        // glance, not only read differently.
        K::HostApplication => Icon::ExternalLink,
        K::Note => Icon::FileText,
        // A record is one document several rooms can reach — the stacked
        // plates say "this is the same thing in more than one place",
        // which is exactly what a reference is.
        K::Record => Icon::Layers,
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

// -- Lossy numeric casts, named and scoped one lint at a time --------
//
// This file's layout math is all in `f64` (to match `kurbo`), but
// `taffy`/`vello`/`cosmic-text` want `f32`, and row/slot/count indices
// are `usize`. Every value that flows through these conversions is a
// screen-space pixel, row count, or millisecond duration — nowhere near
// the magnitude where the narrowing would actually lose a representable
// value. Same convention as `sidebar_wheel_disc.rs`'s lossy-cast
// helpers: name the conversion, scope the `#[allow]` to it alone, and
// use the named function everywhere instead of a bare `as`.

/// Lossy but bounded: UI row/slot/element counts never approach f64's
/// 53-bit exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    reason = "row/slot/element counts stay far below f64's exact-integer range"
)]
fn usize_to_f64_lossy(n: usize) -> f64 {
    n as f64
}

/// Narrows an `f64` pixel/width/alpha value to the `f32` that
/// `taffy`/`vello`/`cosmic-text`'s own APIs take. Screen-space geometry
/// in this UI never approaches a magnitude where this loses meaningful
/// precision.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space geometry stays well within f32's precision for this UI"
)]
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

/// Same rationale as [`usize_to_f64_lossy`], for the millisecond
/// durations `Instant::elapsed().as_millis()` and this file's transition
/// constants use.
#[allow(
    clippy::cast_precision_loss,
    reason = "transition durations are always tiny millisecond counts"
)]
fn millis_to_f32_lossy(ms: u128) -> f32 {
    ms as f32
}

/// A workspace/overview-selection index, `usize -> isize`, for the
/// ring-wraparound arithmetic in [`ShellOverlayHost::handle_event`]. The
/// tree never holds anywhere near `isize::MAX` workspaces.
#[allow(
    clippy::cast_possible_wrap,
    reason = "workspace counts stay far below isize::MAX"
)]
fn usize_to_isize_lossy(n: usize) -> isize {
    n as isize
}

/// The inverse of [`usize_to_isize_lossy`], used only on the result of
/// `isize::rem_euclid` with a positive divisor — which is defined to
/// always return a non-negative value, so this never actually discards a
/// sign.
#[allow(
    clippy::cast_sign_loss,
    reason = "only called on rem_euclid's result with a positive divisor, which is never negative"
)]
fn isize_to_usize_lossy(n: isize) -> usize {
    n as usize
}

/// Clamps an alpha/colour-channel value into `u8`'s range before the
/// (otherwise saturating but silently-wrong-looking) narrowing cast.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "explicitly clamped into u8's range on the line above"
)]
fn f32_to_u8_saturating(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

/// Floors a row-count computation into `usize`. Rust's `f64 as usize`
/// cast already saturates a negative input to `0` rather than wrapping
/// or panicking (defined since Rust 1.45), so this is memory-safe for
/// any `v`; callers additionally `.max(1)` the result.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "float-to-usize casts saturate; a negative input already floors to 0"
)]
fn f64_floor_to_usize_saturating(v: f64) -> usize {
    v.floor() as usize
}

/// Gap between two chips in the filter band.
const CHIP_GAP_PX: f64 = 6.0;
/// Horizontal padding inside a chip — both sides together.
const CHIP_PAD_PX: f64 = 18.0;
/// Narrowest a chip is ever squeezed to before its label is just an
/// ellipsis.
const CHIP_MIN_W_PX: f64 = 34.0;

/// Where each chip of the filter strip goes.
#[derive(Debug, Clone, PartialEq)]
struct ChipStrip {
    /// One rect per chip, in the order handed in. Every chip gets one —
    /// see [`lay_out_chip_strip`].
    rects: Vec<Rect>,
    /// Rows the chips want at their natural width; the host reserves
    /// this many from the palette.
    rows_needed: usize,
}

/// Lays the filter chips out across a band that may be too narrow.
///
/// Invariant: every chip gets a rect — an unpainted chip could never be
/// clicked. Three behaviours in preference order: natural width while it
/// fits; wrap to a new row (over eliding — `Applications` → `Ap…` stops
/// saying what it filters); only once rows run out do leftovers share and
/// narrow the last row, a transient case the next layout fixes.
///
/// `first_row_x1` bounds the first row, since the scope switch anchors to
/// its right; later rows have the whole band.
fn lay_out_chip_strip(
    natural: &[f64],
    band: Rect,
    first_row_x1: f64,
    rows_available: usize,
    row_h: f64,
    control_h: f64,
) -> ChipStrip {
    if natural.is_empty() || band.width() <= 0.0 {
        return ChipStrip {
            rects: Vec::new(),
            rows_needed: 1,
        };
    }
    let rows_available = rows_available.max(1);
    let row_x1 = |row: usize| if row == 0 { first_row_x1 } else { band.x1 };

    // Greedy wrap at natural width; `per_row` caps chips per row, used
    // by the balancing pass below.
    let wrap = |per_row: usize| -> Vec<Vec<usize>> {
        let mut plan: Vec<Vec<usize>> = vec![Vec::new()];
        let mut x = band.x0;
        for (i, w) in natural.iter().enumerate() {
            let w = w.max(CHIP_MIN_W_PX);
            // `plan` always starts with one row and is only ever pushed
            // to, never cleared, so `len() - 1` cannot underflow — spelled
            // with saturating_sub so the invariant doesn't need re-proving
            // at every call site.
            let row = plan.len().saturating_sub(1);
            let row_full = plan
                .get(row)
                .is_some_and(|r| !r.is_empty() && (x + w > row_x1(row) || r.len() >= per_row));
            if row_full {
                plan.push(Vec::new());
                x = band.x0;
            }
            let row = plan.len().saturating_sub(1);
            if let Some(r) = plan.get_mut(row) {
                r.push(i);
            }
            x += w + CHIP_GAP_PX;
        }
        plan
    };
    let plan = wrap(usize::MAX);
    let rows_needed = plan.len();

    // Spread chips evenly over the rows used, rather than filling each
    // to the brim and leaving a lone chip on the last row. Only adopted
    // if it still fits in the same row count, so evenness never costs a
    // chip.
    let plan = if rows_needed > 1 {
        let per_row = natural.len().div_ceil(rows_needed);
        let balanced = wrap(per_row);
        if balanced.len() == rows_needed {
            balanced
        } else {
            plan
        }
    } else {
        plan
    };

    let mut rects = vec![Rect::ZERO; natural.len()];
    let place = |row: usize, ids: &[usize], widths: &[f64], rects: &mut Vec<Rect>| {
        let cy = band.y0 + usize_to_f64_lossy(row) * row_h + row_h / 2.0;
        let mut x = band.x0;
        for (slot, &id) in ids.iter().enumerate() {
            // `widths` is built alongside `ids` by every caller below, so
            // it always has one entry per id — `.get()` keeps a shape
            // mismatch from panicking instead of proving it can't happen.
            let Some(&w) = widths.get(slot) else {
                continue;
            };
            let Some(rect) = rects.get_mut(id) else {
                continue;
            };
            *rect = Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
            x += w + CHIP_GAP_PX;
        }
    };

    let kept_rows = rows_needed.min(rows_available);
    for (row, ids) in plan.iter().take(kept_rows.saturating_sub(1)).enumerate() {
        let widths: Vec<f64> = ids
            .iter()
            .map(|&i| {
                natural
                    .get(i)
                    .copied()
                    .unwrap_or(CHIP_MIN_W_PX)
                    .max(CHIP_MIN_W_PX)
            })
            .collect();
        place(row, ids, &widths, &mut rects);
    }
    let last = kept_rows.saturating_sub(1);
    let leftover: Vec<usize> = plan
        .get(last..)
        .map(|rows| rows.iter().flatten().copied().collect())
        .unwrap_or_default();
    let widths = share_row(
        &leftover
            .iter()
            .map(|&i| natural.get(i).copied().unwrap_or(CHIP_MIN_W_PX))
            .collect::<Vec<_>>(),
        row_x1(last) - band.x0,
    );
    place(last, &leftover, &widths, &mut rects);

    ChipStrip { rects, rows_needed }
}

/// Widths for one row of chips that must all fit inside `available`.
/// Chips narrower than an equal share keep their natural width and hand
/// the rest to the ones that are over, so squeezing costs the long
/// labels and leaves short ones alone.
fn share_row(natural: &[f64], available: f64) -> Vec<f64> {
    if natural.is_empty() {
        return Vec::new();
    }
    let n = usize_to_f64_lossy(natural.len());
    let gaps = CHIP_GAP_PX * (n - 1.0);
    let total: f64 = natural.iter().sum();
    if total + gaps <= available {
        return natural.iter().map(|w| w.max(CHIP_MIN_W_PX)).collect();
    }
    let base = ((available - gaps) / n).max(CHIP_MIN_W_PX);
    let spare: f64 = natural
        .iter()
        .filter(|w| **w < base)
        .map(|w| base - w)
        .sum();
    let over = natural.iter().filter(|w| **w >= base).count();
    let share_over = if over > 0 {
        base + spare / usize_to_f64_lossy(over)
    } else {
        base
    };
    natural
        .iter()
        .map(|w| {
            if *w < base {
                w.max(CHIP_MIN_W_PX)
            } else {
                share_over.min(*w).max(CHIP_MIN_W_PX)
            }
        })
        .collect()
}

/// One pill in the search card's filter band, shared by the kind chips
/// and the "All" reset chip.
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
        transform.pre_translate(
            (
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            )
                .into(),
        ),
        None,
    );
}

const OVERVIEW_TRANSITION_MS: u64 = 400;

/// Vertical inset below the top bar where the floating sidebar wheel
/// starts. Matches `TOPBAR_HEIGHT_PX`; kept local to avoid a
/// cross-module import for one f32.
const SIDEBAR_TOP_OFFSET_PX: f32 = 40.0;

/// Clamp range for the viewport-driven sidebar width, so the disc's
/// `apply_layout` keeps the visible-arc curvature identical across
/// widths. `SIDEBAR_WIDTH_MAX_PX` also caps the extra left padding the
/// content column reserves (`sidebar_wheel_chrome::SIDEBAR_WIDTH_PX`).
const SIDEBAR_WIDTH_MIN_PX: f32 = 220.0;
const SIDEBAR_WIDTH_MAX_PX: f32 = 320.0;
const SIDEBAR_WIDTH_VIEWPORT_FRACTION: f32 = 0.26;

/// Sidebar width for a given viewport height: `0.26 * h`, clamped to
/// [220, 320]. First-frame callers pass `0.0` and get the lower bound,
/// which reflows once `ShellOverlayHost::rect` is known.
fn sidebar_width_for_viewport(viewport_h: f32) -> f32 {
    (viewport_h * SIDEBAR_WIDTH_VIEWPORT_FRACTION).clamp(SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_MAX_PX)
}

/// Wallpaper gutter between the wheel disc's right edge and the first
/// window, so the radial switcher reads as separated chrome rather than
/// butting into content.
const WHEEL_TO_STRIP_GAP_PX: f64 = 28.0;

/// Wallpaper gutter at the far edge of a display, matching
/// [`WHEEL_TO_STRIP_GAP_PX`] — without it a full-width group's border,
/// corners and shadow fall off screen at the bezel.
const STRIP_TRAILING_GUTTER_PX: f64 = 28.0;

/// One display the compositor is painting across, in its combined
/// coordinate space. The compositor renders one scene spanning every
/// output's bounding box, so a window group's width must be resolved
/// against its own display, not the combined desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputGeometry {
    /// Left edge in combined coordinates.
    pub x: f64,
    pub width: f64,
    pub height: f64,
}

/// Duration of the quick-settings popover entry animation: short enough
/// to feel snappy, long enough to register as motion.
const QS_OPEN_ANIM_MS: u64 = 180;

/// Duration of the modal-style entry animation for workspace-settings
/// and global-search. Slightly longer than the dropdown variant since
/// the travel distance is larger.
pub(crate) const MODAL_OPEN_ANIM_MS: u64 = 220;

// Surface and scrim alphas. The pinned Vello has no backdrop filter, so
// chrome surfaces are opaque and scrims carry the depth instead.
// Centralised so `shell_chrome_test` can pin the numeric values.

/// Alpha of the collaborator and `QuickSettings` popover surfaces.
/// Opaque, deferring to the toolkit's chrome constant so a popover can't
/// drift from the bar it hangs off — the pinned Vello has no backdrop
/// filter, so a translucent tint here would just lose contrast.
pub const POPOVER_SURFACE_ALPHA: f32 = ui::chrome::shell::CHROME_SURFACE_ALPHA;

/// Alpha of the `GlobalSearch` palette and Workspace-Settings modal
/// surfaces. Paints solid rather than the design's translucent tint,
/// since translucency drops legibility of long rows; the scrim beneath
/// sells the depth instead.
pub const MODAL_SURFACE_ALPHA: f32 = 1.0;

/// Alpha of the scrim beneath the `WorkspaceSettings` modal. The design
/// calls for a blurred `0.30`; the pinned Vello has no blur layer, so
/// `0.45` is the empirical alpha at which an un-blurred scrim reads the
/// same. Pinned by `tests::palette_scrim_alpha_matches_concept`.
pub const MODAL_SCRIM_ALPHA: f32 = 0.45;

/// Alpha of the scrim beneath the `GlobalSearch` palette and Overview
/// view. Same value and same reasoning as [`MODAL_SCRIM_ALPHA`]. Pinned
/// by `tests::palette_scrim_alpha_reads_like_the_blurred_concept`.
pub const PALETTE_SCRIM_ALPHA: f32 = 0.45;

/// A cheap value that changes exactly when a tab's content changes: the
/// serialised tile, since that's the whole of what
/// [`crate::shell::to_ui_tile`] reads.
fn tile_signature(tile: &crate::shell::state::TileContent) -> String {
    serde_json::to_string(tile).unwrap_or_default()
}

/// Whether `tile`, about to replace a tab's contents, is a client window
/// that hasn't drawn anything yet — only a foreign surface can answer
/// yes, since everything else is on screen the moment it's built. Looser
/// than the window group's own paint-time check, because the tab's rect
/// only reaches the client once this swap has happened.
fn window_is_still_starting(tile: &crate::shell::state::TileContent) -> bool {
    match tile {
        crate::shell::state::TileContent::Foreign { surface_id, .. } => {
            crate::compositor::foreign::store().frame_seq(*surface_id) == 0
        }
        _ => false,
    }
}

/// `(strip_id, groups)` where each group is `(group_id, tab_ids)` — the
/// per-strip snapshot [`ShellOverlayHost::groups_per_strip`] returns for
/// tests.
type GroupsPerStrip = Vec<(StripId, Vec<(u64, Vec<u64>)>)>;

/// `(brightness, volume, wifi, bluetooth, power, wired, battery)` — the
/// scalar fields [`ShellOverlayHost::apply_fast_qs_reading`] pulls out of a
/// [`crate::shell::qs_refresh::FastReading`].
type QsFastFields = (
    Option<f32>,
    Option<f32>,
    Option<bool>,
    Option<bool>,
    Option<crate::shell::qs_actions::PowerProfile>,
    Option<crate::shell::qs_actions::WiredInfo>,
    Option<crate::shell::qs_actions::BatteryInfo>,
);

impl ShellOverlayHost {
    /// Registers every built-in search provider and hands back the shared
    /// records index alongside the registry. Split out of [`Self::new`],
    /// which was otherwise dominated by this one flat setup block.
    fn build_search_registry() -> (
        crate::shell::search::SearchRegistry,
        std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
    ) {
        // Start the remote-app refresh with the shell, not with the user's
        // first Win+K query. A persisted metadata snapshot makes search
        // useful immediately; this refresh quietly makes it current.
        let _ = crate::shell::catalogue::available_apps();
        let mut search_registry = crate::shell::search::SearchRegistry::new();
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ActionsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ApplicationsProvider::new(),
        ));
        // Applications qOS did not package: unconfined, separate chip
        // from the one above.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::HostApplicationsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::NotesProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::EditorProvider::new(),
        ));
        let records = crate::shell::search::providers::RecordsProvider::new();
        let room_records = records.index();
        search_registry.register(std::rc::Rc::new(records));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::FilesProvider::new(),
        ));
        (search_registry, room_records)
    }

    pub fn new(content: Box<dyn Widget>) -> Self {
        // Palette entries come from the search registry on every
        // keystroke. New providers slot in via `SearchRegistry::register`;
        // the palette picks up their resource-kind chips automatically.
        let mut palette = CommandPalette::new();
        palette.set_bypass_fuzzy(true);
        // The shell paints scope + kind controls into the palette's own
        // reserved filter band, inside the card.
        palette.set_host_filter_strip(true);
        let (search_registry, room_records) = Self::build_search_registry();
        Self {
            rect: Rect::ZERO,
            content,
            onboarding: None,
            onboarding_released: false,
            onboarding_focus_reset: false,
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
            search_chip_rows: 1,
            room_records,
            palette_layout_dirty: false,
            palette_layout_host_w: f64::NAN,
            content_node: None,
            palette_node: None,
            sidebar_node: None,
            chrome: None,
            last_cursor: None,
            overview_t: 0.0,
            overview_target: 0.0,
            overview_scroll_y: 0.0,
            overview_selection: None,
            transition_started: None,
            transition_from: 0.0,
            qs_transition: ui::Transition::closed(),
            qs_was_open: false,
            qs_slider_drag: None,
            power_menu: PowerMenu::default(),
            ws_invite_copied_at: None,
            strip_ribbon_hover: None,
            strip_rail_hover: None,
            qs_refresh: crate::shell::qs_refresh::QsRefresh::new(),
            ws_settings_was_open: false,
            ws_settings_transition: ui::Transition::closed(),
            palette_was_visible: false,
            palette_transition: ui::Transition::closed(),
            group_row_ptrs: HashMap::new(),
            content_built_for_ws: None,
            last_scrolled_active_group: HashMap::new(),
            add_workspace_rect: Rect::ZERO,
            ws_settings_tab_pill_rects: [Rect::ZERO; 4],
            default_app_rects: Vec::new(),
            permissions_revoke_rects: Vec::new(),
            grant_composer_rects: None,
            member_control_rects: Vec::new(),
            sidebar_top_y: SIDEBAR_TOP_OFFSET_PX,
            permission_prompt: None,
            portal_prompts: None,
            file_picker_overlay: FilePickerOverlay::default(),
            peer_sync_enabled: false,
            peer_sync_actions: Vec::new(),
            peer_sync_bridge: None,
            peer_sync_overlay: PeerSyncOverlay::default(),
            notification_stack: None,
            last_chrome_revision: u64::MAX,
            tab_tiles: std::collections::HashMap::new(),
            qs_actions: Box::new(crate::shell::qs_actions::RealQsActions),
            qs_wifi_pane: QuickSettingsWifiPane::default(),
            qs_bluetooth_pane: QuickSettingsBluetoothPane::default(),
            qs_aux_drill_in: DrillInList::default(),
            qs_vpn_pane: QuickSettingsVpnPane::default(),
            status_bridge: None,
            session_audio_policy: None,
            room_activity: crate::shell::room_activity::RoomActivityPane::new_persistent(),
            room_activity_transition: ui::Transition::closed(),
            room_activity_was_open: false,
            collaborators_overlay: CollaboratorsOverlay::default(),
            projection_overlay: ProjectionOverlay::default(),
            followed_peer: None,
            follow_chip_rect: Rect::ZERO,
            follow_stop_rect: Rect::ZERO,
        }
    }

    /// Gate this host behind the shell-owned identity ceremony.
    ///
    /// This lives on the compositor root rather than in a pre-session winit
    /// window: on a fresh machine there is no display server before `shell`,
    /// and an application-shaped setup window could be imitated by an app.
    #[must_use]
    pub fn with_onboarding(mut self, identity_dir: std::path::PathBuf) -> Self {
        self.onboarding = Some(crate::onboarding_view::OnboardingView::new(identity_dir));
        self
    }

    /// Activate first-login chrome on an already constructed compositor
    /// host. Production uses this after resolving the canonical XDG path.
    pub fn enable_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.onboarding = Some(crate::onboarding_view::OnboardingView::new(identity_dir));
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Whether first-login chrome is still withholding the desktop.
    pub fn onboarding_active(&self) -> bool {
        self.onboarding.is_some()
    }

    /// Replace the real protocol broker. Headless tests use this to drive
    /// discovery, connection and failure states without receiver hardware.
    #[must_use]
    pub fn with_projection_backend(
        mut self,
        backend: Box<dyn crate::shell::projection::ProjectionBackend>,
    ) -> Self {
        self.projection_overlay = ProjectionOverlay::with_backend(backend);
        self
    }

    pub fn projection(&self) -> &crate::shell::projection::ProjectionController {
        self.projection_overlay.controller()
    }

    /// Whether a receiver session is being established or is live. The
    /// compositor uses this to turn the frame encoder on only while needed.
    pub fn projection_capture_requested(&self) -> bool {
        self.projection_overlay.capture_requested()
    }

    /// Replace the QS action dispatcher. Tests use this to swap in a
    /// recording impl that captures every `set_*` call instead of
    /// shelling out to `nmcli` / `brightnessctl` / `bluetoothctl` /
    /// `wpctl` / `powerprofilesctl`. Production builds keep the
    /// default [`crate::shell::qs_actions::RealQsActions`].
    #[must_use]
    pub fn with_qs_actions(
        mut self,
        actions: Box<dyn crate::shell::qs_actions::QsActions>,
    ) -> Self {
        self.qs_actions = actions;
        // Replacing where quick settings writes means this host is not
        // driving the real machine, so it must not read from it either.
        self.qs_refresh.disable_os_reads();
        self
    }

    /// Install the live-status bridge. Only the compositor's real-session
    /// constructors do this; without it the tray's readings stay `None`
    /// and it draws no indicators, which is what keeps golden renders
    /// identical across machines.
    #[must_use]
    pub fn with_status_bridge(mut self, bridge: crate::shell::status::StatusBridge) -> Self {
        self.status_bridge = Some(bridge);
        // A spawn failure means resource exhaustion on the host; the session
        // still runs, just without persistent audio-balance/mono transforms.
        self.session_audio_policy =
            match crate::shell::session_audio_policy::SessionAudioPolicyOwner::spawn() {
                Ok(owner) => Some(owner),
                Err(error) => {
                    tracing::warn!(
                        target: "qos::audio_policy",
                        %error,
                        "failed to spawn the session audio policy worker"
                    );
                    None
                }
            };
        self
    }

    /// The QS action dispatcher, for callers that actuate hardware
    /// outside the popover — the media keys, which drive the same
    /// volume and brightness the sliders do and must go through the same
    /// dispatcher so a test can record them.
    pub fn qs_actions(&self) -> &dyn crate::shell::qs_actions::QsActions {
        self.qs_actions.as_ref()
    }

    /// Handle on the status bridge, for the paths that publish a reading
    /// rather than consume one (the volume keys, which must move the
    /// indicator in the frame they were pressed rather than up to a poll
    /// interval later).
    pub fn status_bridge(&self) -> Option<&crate::shell::status::StatusBridge> {
        self.status_bridge.as_ref()
    }

    /// Paint the transient hardware-key readout, if one is up.
    fn draw_osd(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let now = Instant::now();
        // Expiring here rather than on a timer ties the slab's lifetime
        // to frames that actually paint it, not an idle compositor.
        {
            let mut st = chrome.borrow_mut();
            st.expire_osd(now);
        }
        let Some(osd) = chrome.borrow().osd else {
            return;
        };
        let alpha = osd.alpha_at(now);
        if alpha <= 0.0 {
            return;
        }

        osd.draw(scene, transform, text_renderer, theme, self.rect, alpha);
    }

    /// Copy the poller's latest reading onto the chrome, bumping the
    /// repaint-triggering revision only when it actually changed.
    fn sync_status_from_bridge(&self) {
        let Some(bridge) = self.status_bridge.as_ref() else {
            return;
        };
        let next = bridge.snapshot();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.status != next {
            st.status = next;
            st.bump();
        }
    }

    /// Advance the overview crossfade toward `overview_target`, latching a
    /// new transition if the target changed. Returns *linear* progress —
    /// the draw path splits it into its own two-phase, smoothstep-eased
    /// animation, so this must stay linear.
    fn tick_overview_transition(&mut self, target: f32) -> f32 {
        if (target - self.overview_target).abs() > f32::EPSILON {
            self.transition_from = self.overview_t;
            self.overview_target = target;
            self.transition_started = Some(Instant::now());
            // Reseed from the current room so the first arrow press moves
            // one step from here, not from a stale previous visit.
            self.overview_selection = if target > 0.5 {
                self.overview_here_index()
            } else {
                None
            };
            self.overview_scroll_y = 0.0;
        }
        if let Some(started) = self.transition_started {
            let elapsed = millis_to_f32_lossy(started.elapsed().as_millis());
            let duration = millis_to_f32_lossy(u128::from(OVERVIEW_TRANSITION_MS));
            if elapsed >= duration {
                self.overview_t = self.overview_target;
                self.transition_started = None;
            } else {
                let p = (elapsed / duration).clamp(0.0, 1.0);
                // Linear progress; `overview_panel_curve` does the
                // easing, so the shape of the motion lives in one place.
                self.overview_t =
                    self.transition_from + (self.overview_target - self.transition_from) * p;
            }
        }
        self.overview_t
    }

    #[must_use]
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

    /// Toggle the peer-sync palette commands (`/invite`, `/connect`). Off
    /// by default; flipped on by `--enable-peer-sync`. Idempotent: the
    /// rows are appended once and can't be removed by toggling back off.
    #[must_use]
    pub fn with_peer_sync_enabled(mut self, enabled: bool) -> Self {
        // `ActionsProvider` reads the flag off chrome, not this struct.
        self.peer_sync_enabled = enabled;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().peer_sync_enabled = enabled;
        }
        self
    }

    /// Whether peer-sync palette entries are registered.
    pub fn peer_sync_enabled(&self) -> bool {
        self.peer_sync_enabled
    }

    /// Drain pending [`PeerSyncAction`]s since the last call, FIFO. Pumped
    /// each frame by the compositor into its `ShellNetwork` handle. No
    /// de-dup or coalescing, so repeated `/invite` all surface.
    pub fn take_peer_sync_actions(&mut self) -> Vec<PeerSyncAction> {
        std::mem::take(&mut self.peer_sync_actions)
    }

    /// Install the closure that carries out [`PeerSyncRequest`]s. Without
    /// one, `/invite` and `/connect` push to a queue nothing drains.
    #[must_use]
    pub fn with_peer_sync_bridge(mut self, bridge: PeerSyncBridge) -> Self {
        self.set_peer_sync_bridge(bridge);
        self
    }

    /// [`Self::with_peer_sync_bridge`] for a host that is already
    /// built — the native compositor constructs its host inside a
    /// `FrameBuilder` before it has a network handle to bridge to.
    pub fn set_peer_sync_bridge(&mut self, bridge: PeerSyncBridge) {
        self.peer_sync_bridge = Some(bridge);
    }

    /// Run one peer-sync request through the installed bridge.
    fn peer_sync_call(&self, req: PeerSyncRequest) -> Result<String, String> {
        match self.peer_sync_bridge.as_ref() {
            Some(bridge) => bridge(req),
            None => Err("peer-sync is not running in this session".to_string()),
        }
    }

    /// Drain the palette's peer-sync intents and act on them, once per
    /// frame from [`Widget::update`]. `/invite` mints a token; `/connect`
    /// opens the token prompt. Both write their outcome into chrome.
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
                    let request = self.peer_sync_overlay.issue_invite_request();
                    self.run_peer_sync_request(&request, false);
                }
                PeerSyncAction::IssueInviteFor { name, copy } => {
                    self.run_peer_sync_request(&PeerSyncRequest::IssueInviteFor(name), copy);
                }
                PeerSyncAction::PromptInvite => self.peer_sync_overlay.prompt_invite(),
                PeerSyncAction::PromptConnect => self.peer_sync_overlay.prompt_connect(),
            }
        }
    }

    fn run_peer_sync_request(&mut self, request: &PeerSyncRequest, copy_result: bool) {
        let outcome = self.peer_sync_call(request.clone());
        self.peer_sync_overlay
            .finish_request(request, outcome, copy_result);
    }

    /// Route one event to the peer-sync card, returning whether it was
    /// consumed. While a prompt is open every key belongs to it — a
    /// base64 token shares characters with shell chords, so anything
    /// that leaked past would both corrupt the token and fire an action.
    fn handle_peer_sync_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let PeerSyncEventOutcome {
            consumed,
            request,
            copy_result,
        } = self
            .peer_sync_overlay
            .handle_event(event, modifiers, self.last_cursor);
        if let Some(request) = request {
            self.run_peer_sync_request(&request, copy_result);
        }
        consumed
    }

    /// Register the floating sidebar wheel as an absolutely-positioned
    /// overlay, so it hovers over the wallpaper rather than occupying a
    /// column.
    #[must_use]
    pub fn with_sidebar(mut self, sidebar: Box<dyn Widget>) -> Self {
        self.sidebar = Some(sidebar);
        self
    }

    /// Override the sidebar's top y-offset. The 40px default assumes an
    /// unpadded topbar; without this on the real shell's padded topbar,
    /// the sidebar overlaps it and swallows overview-toggle clicks.
    #[must_use]
    pub fn with_sidebar_top(mut self, y: f32) -> Self {
        self.sidebar_top_y = y;
        self
    }

    /// Tell the host which displays it is painting across, so
    /// [`GroupRow`]s can size against per-display strip widths.
    pub fn set_output_geometry(&mut self, outputs: &[OutputGeometry]) {
        if self.outputs != outputs {
            self.outputs = outputs.to_vec();
        }
    }

    /// The displays the host was last told about.
    pub fn output_geometry(&self) -> &[OutputGeometry] {
        &self.outputs
    }

    /// Per-display strip widths: full extent plus width actually
    /// available to window groups, with the leftmost display giving up
    /// the wheel-disc slot and its gutter. Falls back to the host's own
    /// rect when the compositor hasn't pushed geometry yet (`--mode
    /// hosted`).
    fn strip_output_spans(&self) -> Vec<OutputSpan> {
        let outputs: Vec<OutputGeometry> = if self.outputs.is_empty() {
            // `self.rect` is zero on a first frame (only set in
            // apply_layout, this runs during build_layout); no spans
            // lets `GroupRow::basis_for` fall back to its own chain.
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
        let strip_left = 0.0_f64;
        outputs
            .iter()
            .filter(|o| o.width > 0.0)
            .map(|o| {
                let hosts_chrome = strip_left >= o.x && strip_left < o.x + o.width;
                let leading = if hosts_chrome && self.sidebar.is_some() {
                    f64::from(sidebar_width_for_viewport(f64_to_f32_lossy(o.height)))
                        + WHEEL_TO_STRIP_GAP_PX
                } else {
                    0.0
                };
                OutputSpan {
                    x: o.x,
                    width: o.width,
                    usable: (o.width - leading - STRIP_TRAILING_GUTTER_PX).max(1.0),
                }
            })
            .collect()
    }

    /// Register the per-strip [`GroupRow`] raw pointers harvested at
    /// build time, used each frame to feed the reconciler without
    /// re-walking the widget tree.
    ///
    /// SAFETY: every pointer in `ptrs` must address a `GroupRow` owned by
    /// the content subtree this host wraps. The caller (currently
    /// [`build_nirios_overlay_host_with_chrome`]) is responsible for
    /// upholding that invariant by capturing the pointer immediately after
    /// `Box::new(strip_row)` and passing the same `Box` into the content
    /// tree — the `Box<T>` -> `Box<dyn Widget>` unsizing coercion keeps
    /// the heap address stable.
    #[must_use]
    pub fn with_group_row_ptrs(mut self, ptrs: HashMap<StripId, *mut GroupRow>) -> Self {
        self.group_row_ptrs = ptrs;
        self
    }

    /// Record which workspace the injected content was built for, so
    /// [`Self::resync_workspace_content`] can detect the first switch away.
    #[must_use]
    pub fn with_content_workspace(mut self, ws_raw: u64) -> Self {
        self.content_built_for_ws = Some(ws_raw);
        self
    }

    /// Swaps the workspace-scoped content when the active workspace changes
    /// or grows a strip with no row. Content root and pointer map rebuild
    /// TOGETHER via [`crate::shell::build_workspace_root`], so no
    /// reconciler ever observes a pointer into a freed row. A new strip has
    /// no incremental path — [`Self::resync_strips_from_tree`] splices by
    /// strip id and skips strips it has no row for — so a rebuild (costing
    /// scroll offsets, not focus, which lives in the tree) is the only
    /// option, and it's rare. Returns `true` on a swap. Runs at the top of
    /// both [`Widget::update`] and [`Widget::take_structure_dirty`] — the
    /// shots driver's relayout-only path skips `update`.
    fn resync_workspace_content(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let snapshot = {
            let st = chrome.borrow();
            let tree = st.tree();
            let active_raw = tree.active.raw();
            match self.content_built_for_ws {
                // Hand-made test content: latch, never rebuild over it.
                None => {
                    drop(tree);
                    self.content_built_for_ws = Some(active_raw);
                    return false;
                }
                Some(cur) if cur == active_raw && self.rows_cover_active_strips(&tree) => {
                    return false;
                }
                Some(_) => {}
            }
            tree.clone()
        };
        let records = chrome.borrow().records().clone();
        let (root, ptrs) = crate::shell::build_workspace_root(&snapshot, &records, &chrome);
        // Replace content and pointer map in one step: dropping the old
        // root frees every old GroupRow, and the map must never outlive
        // the rows it points into.
        self.content = root;
        self.group_row_ptrs = ptrs;
        self.content_built_for_ws = Some(snapshot.active.raw());
        // Per-strip memos of the old workspace die with its rows.
        self.last_scrolled_active_group.clear();
        true
    }

    /// Does the content tree have a row for every strip of the active
    /// workspace? A strip with no row can't be drawn (the incremental
    /// resync looks it up by id); extra rows point at strips that went away.
    fn rows_cover_active_strips(&self, tree: &crate::shell::state::WorkspaceTree) -> bool {
        let Some(ws) = tree.active_workspace() else {
            return true;
        };
        ws.strips.len() == self.group_row_ptrs.len()
            && ws
                .strips
                .iter()
                .all(|s| self.group_row_ptrs.contains_key(&s.id))
    }

    /// Test hook for the workspace-content reconcile — mirrors the
    /// per-frame call the update/layout paths make.
    pub fn tick_workspace_content_for_tests(&mut self) -> bool {
        self.resync_workspace_content()
    }

    /// On-screen rect of the tile hosting foreign wayland surface
    /// `surface_id`, or `None` when no *visible* tile hosts it. Walks the
    /// live widget tree, not persisted state, so the answer is what's
    /// actually painted — what the compositor needs to size the client at.
    pub fn foreign_surface_rect(&mut self, surface_id: u64) -> Option<Rect> {
        let wanted = format!("foreign-surface-{surface_id}");
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — the pointer comes from a `Box<GroupRow>` owned by
            // the content tree, the host outlives the content, and
            // `&mut self` gives us unaliased access for this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
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

    /// Whether any tab on screen is still holding a place for an app that
    /// hasn't drawn yet (see [`ui::widget::WindowGroup::any_tile_opening`]).
    /// A group in this state requests animation frames for as long as it
    /// waits — deliberately indistinguishable from a wedged animation from
    /// the outside; this method is how a caller waiting for the tree to go
    /// quiet tells them apart.
    pub fn any_tile_opening(&mut self) -> bool {
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — the pointer comes from a `Box<GroupRow>` owned by
            // the content tree, the host outlives the content, and
            // `&mut self` gives us unaliased access for this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            if row
                .groups
                .iter()
                .any(ui::widget::WindowGroup::any_tile_opening)
            {
                return true;
            }
        }
        false
    }

    /// Diffs the live `WorkspaceTree` against every `GroupRow`'s children,
    /// splicing via `insert_group`/`remove_group_by_id` to preserve
    /// untouched rows' host state (scroll, tab selection). Without this
    /// poll, rows would keep painting the boot-time snapshot as the
    /// persisted tree grows. Cheap on the no-op path — one u64 compare
    /// against `last_chrome_revision`.
    fn resync_strips_from_tree(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let current_rev = chrome.borrow().revision;
        if current_rev == self.last_chrome_revision {
            return false;
        }
        let mut any_change = false;
        // Moved out for the walk below: the rows are reached through raw
        // pointers taken from `&mut self`, so a field borrowed across them
        // would alias. Put back before returning.
        let mut tab_tiles = std::mem::take(&mut self.tab_tiles);
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
                // Splice in new groups without rebuilding the vector, so
                // existing rows keep their host state.
                for (idx, g) in strip.window_groups.iter().enumerate() {
                    if !live_ids.contains(&g.id) {
                        let mut fresh = crate::shell::to_ui_window_group(g, &records);
                        // Same spawn fade the row's own splice paths use,
                        // so an opened window arrives the same way.
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
                // Reconcile tabs inside groups that already exist — the
                // group-level splice above only fires for whole new
                // groups, so a tab adopted into an existing one (an
                // external toplevel) needs its own path here.
                for g in &strip.window_groups {
                    let Some(live) = row.groups.iter_mut().find(|lg| lg.id == g.id) else {
                        continue;
                    };
                    let live_tab_ids: std::collections::HashSet<u64> =
                        live.tabs().iter().map(|t| t.id).collect();
                    let tree_tab_ids: std::collections::HashSet<u64> =
                        g.tabs.iter().map(|t| t.id).collect();
                    for (idx, tab) in g.tabs.iter().enumerate() {
                        let signature = tile_signature(&tab.tile);
                        if live_tab_ids.contains(&tab.id) {
                            // A placeholder becoming the real window: held
                            // off by `window_is_still_starting` until the
                            // surface has drawn, so a swap doesn't replace
                            // an "opening" card with a blank frame — the
                            // signature is left unrecorded so it retries.
                            if tab_tiles.get(&tab.id) != Some(&signature)
                                && !window_is_still_starting(&tab.tile)
                                && let Some(host) = live.host_for_tab_mut(tab.id)
                            {
                                host.replace_content(crate::shell::to_ui_tile(&tab.tile));
                                tab_tiles.insert(tab.id, signature);
                                any_change = true;
                            }
                            continue;
                        }
                        let desc = ui::widget::TabDescriptor {
                            id: tab.id,
                            title: tab.title.clone(),
                            app_kind: Some(crate::shell::tile_app_kind(&tab.tile)),
                        };
                        // Gets its own document too, same as any tab
                        // present at boot.
                        let mut host =
                            ui::widget::TileHost::new(crate::shell::to_ui_tile(&tab.tile));
                        host.bind_record(
                            records.clone(),
                            crate::shell::tab_documents::scratch_tab_record(tab.id),
                        );
                        live.insert_detached_tab(idx.min(live.tab_count()), desc, host);
                        tab_tiles.insert(tab.id, signature);
                        any_change = true;
                    }
                    for stale in live_tab_ids.difference(&tree_tab_ids) {
                        if live.detach_tab(*stale).is_some() {
                            tab_tiles.remove(stale);
                            any_change = true;
                        }
                    }
                }
            }
        }
        drop(tree);
        drop(st);
        self.tab_tiles = tab_tiles;
        self.last_chrome_revision = current_rev;
        any_change
    }

    /// Pushes the compositor's transient fullscreen state onto every
    /// strip's [`GroupRow`] so the owning strip promotes while others
    /// collapse. Idempotent. Called from [`Widget::update`],
    /// [`Widget::build_layout`], and `FrameBuilder::relayout` *before* the
    /// structure-dirty check, so a promotion/restore triggers the rebuild.
    pub fn sync_fullscreen_from_chrome(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Client `xdg_toplevel.set_fullscreen` wins over a user-expanded
        // group if both are set.
        let fs_group = {
            let st = chrome.borrow();
            st.fullscreen_group().or_else(|| st.maximized_group())
        };
        // The live client tree and this rendered tree aren't yet unified,
        // so a fullscreen id we don't host must count as "no fullscreen"
        // — otherwise every strip hides around a group that isn't here.
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

    fn reconcile_active_group_off_closing(&self) {
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
            // host outlives the content, and this is a read-only deref,
            // so the pointer is valid regardless of aliasing.
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

    /// Drains every group's [`UiWindowGroup::take_pending_maximize_change`]
    /// one-shot and mirrors it into the persisted
    /// [`crate::shell::state::WorkspaceTree`] — without this the toggle
    /// lives only on the widget and resets on the next restart. Idempotent;
    /// the chrome borrow is only taken when a flip needs writing.
    fn harvest_pending_maximize_changes(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Snapshot first so the mutable widget borrow drops before the
        // chrome borrow.
        let mut changes: Vec<(StripId, GroupId, bool)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and reconcile sweeps
            // run sequentially so no other path is aliasing this row.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
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
            // Parks the group id in chrome's user-maximize slot so
            // `is_fullscreen()` suppresses the top bar + wheel. Cleared
            // only by the group that owns the slot.
            if *new_value {
                st.set_maximized_group(Some(*group_id));
            } else if st.maximized_group() == Some(*group_id) {
                st.set_maximized_group(None);
            }
        }
        for (_strip_id, group_id, new_value) in changes {
            // Flat scan is correct since group ids are tree-unique.
            let mut tree = st.tree_mut();
            for ws in &mut tree.workspaces {
                for strip in &mut ws.strips {
                    if let Some(g) = strip.window_groups.iter_mut().find(|g| g.id == group_id) {
                        g.maximized = new_value;
                    }
                }
            }
            tree.doc_set_maximized(group_id, new_value);
        }
    }

    /// Mirrors the widget-side `TabBar::active_id` back into the persisted
    /// tree's `group.active_tab`/`active_group`/`active_strip` — without
    /// it, a tab click flips visible content but leaves search-scope
    /// helpers pointing at the group's initial tab. Runs every frame; the
    /// O(groups × tabs) walk stays cheap enough to skip a dirty bit.
    fn harvest_active_tab_writeback(&self) {
        // Widgets and chrome can't be borrowed at once, so collect the
        // tuples first and apply under one chrome borrow.
        struct Writeback {
            strip: StripId,
            group: GroupId,
            tab: crate::shell::state::TabId,
        }
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut writes: Vec<Writeback> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content; this is a
            // read-only deref.
            let row: &GroupRow = unsafe { &*ptr };
            for g in &row.groups {
                writes.push(Writeback {
                    strip: strip_id,
                    group: g.id,
                    tab: g.active_tab_id(),
                });
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        // A divergence between `bar.active_id()` and the tree means a tab
        // pill was clicked; stamp `active_strip`/`active_group` after the
        // writeback, or Ctrl+K's scope stays on the old group.
        let mut focus_writes: Vec<(WorkspaceId, StripId, GroupId)> = Vec::new();
        {
            let mut tree = st.tree_mut();
            for w in &writes {
                for ws in &mut tree.workspaces {
                    for strip in &mut ws.strips {
                        if strip.id != w.strip {
                            continue;
                        }
                        if let Some(group) =
                            strip.window_groups.iter_mut().find(|g| g.id == w.group)
                            && group.active_tab != w.tab
                        {
                            // A stale `bar.active_id` (mid-detach
                            // frame) would write a phantom active-tab
                            // id and silently drop the group.
                            let tab_exists = group.tabs.iter().any(|t| t.id == w.tab);
                            if !tab_exists {
                                continue;
                            }
                            group.active_tab = w.tab;
                            focus_writes.push((ws.id, w.strip, w.group));
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

    /// Give every live window group the same share of its strip, and
    /// mirror it to the persisted tree the way a width drag does. Setting
    /// the tree alone wouldn't move anything — the laid-out width lives
    /// on the widget. Returns the ids touched.
    pub fn set_group_widths(&mut self, permille: u16) -> Vec<u64> {
        let mut touched: Vec<u64> = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                g.set_width(ui::widget::WindowWidth::Fraction(permille));
                touched.push(g.id);
            }
        }
        if touched.is_empty() {
            return touched;
        }
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            for ws in &mut tree.workspaces {
                for strip in &mut ws.strips {
                    for g in &mut strip.window_groups {
                        if touched.contains(&g.id) {
                            g.width = crate::shell::state::WindowWidth::Fraction(permille);
                        }
                    }
                }
            }
            for id in &touched {
                tree.doc_set_width(*id, permille);
            }
        }
        // `RuntimeGroup::set_width` flags its own style dirty; the
        // frame loop's structure sweep is what rebuilds the taffy node
        // whose baked length carries the old width.
        touched
    }

    /// Close the active tab of the focused group, through the same
    /// `TabBar::start_closing` a `✕` click runs. `false` when nothing is
    /// focused or the group has no tab left to close.
    pub fn close_focused_tab(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let focused = crate::shell::window_actions::focused_group(&chrome.borrow());
        let Some(gid) = focused else {
            return false;
        };
        self.close_tab_in_group(gid, None)
    }

    /// Close `tab` (or the active tab, when `tab` is `None`) in the group
    /// with id `gid`. Shared by the chord and the tab context menu.
    pub fn close_tab_in_group(&mut self, gid: GroupId, tab: Option<u64>) -> bool {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if g.id != gid {
                    continue;
                }
                let closed = match tab {
                    Some(id) => g.request_close_tab(id),
                    None => g.request_close_active_tab(),
                };
                // No `bump()`: closing a tab has no tree-side counterpart
                // (no `doc_remove_tab`), so a revision bump would resync
                // this row from the tree and splice the tab straight back.
                return closed;
            }
        }
        false
    }

    fn harvest_pending_width_changes(&self) {
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
            for g in &mut row.groups {
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
            for ws in &mut tree.workspaces {
                for strip in &mut ws.strips {
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

    /// Mirrors a completed whole-group reorder drag into the persisted
    /// tree via [`WorkspaceTree::reorder_group_within_strip`] (which also
    /// mirrors into the automerge doc for peer-sync) — without this a
    /// group-drag resets to the persisted order on restart. Mirrors
    /// [`Self::harvest_pending_width_changes`] in shape.
    fn harvest_pending_group_reorder(&self) {
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

    /// Same as [`Self::harvest_pending_group_reorder`], for an
    /// *intra-group* tab reorder via [`WorkspaceTree::reorder_tab_within_group`].
    fn harvest_pending_tab_reorder(&self) {
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
            for g in &mut row.groups {
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

    /// Drains each group's [`UiWindowGroup::take_pending_search_open`]
    /// one-shot and opens the search bar scoped to the active app — the
    /// same entry point Ctrl+K uses. The palette lives on the host, not
    /// the group.
    fn harvest_pending_search_open(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let mut requested = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
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

    /// Sibling of [`Self::harvest_pending_search_open`] for the "+" chip:
    /// opens the palette aimed at that group rather than minting an
    /// assumed tab type. [`OpenTarget::Here`] pins the result to this
    /// group even when the placement rule would open beside it.
    fn harvest_pending_new_tab(&mut self) -> bool {
        if self.group_row_ptrs.is_empty() {
            return false;
        }
        let mut requested: Option<u64> = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if let Some(id) = g.take_pending_new_tab() {
                    requested = Some(id);
                }
            }
        }
        let Some(group) = requested else {
            return false;
        };
        // Must run before the palette's scope resolution and launch path
        // read the active pointers.
        self.harvest_active_tab_writeback();
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.open_target = crate::shell::chrome_state::OpenTarget::Here(group);
            // `All`, not `ActiveApp`: a "+" asks what to open, and only the
            // widest scope offers something not already running.
            st.search_scope = crate::shell::chrome_state::SearchScope::All;
            st.bump();
        }
        // Narrowed to kinds that can *be* a tab, so results aren't buried
        // under notes/files/chrome actions the user has to read past.
        self.active_kind_filter = app_kind_filter();
        // Force-open, not `open_palette_with_scope`: that helper toggles
        // closed on a same-scope repeat, wrong for a "+" click.
        if !self.palette_visible {
            self.palette_visible = true;
            self.palette.set_focus(true);
            self.palette.set_query("");
        }
        // Re-run the query even when the palette was already up: the rows
        // carry a placement hint that just changed.
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        true
    }

    /// Drops a stale `OpenTarget::Here` and kind filter once the palette
    /// closes — both are one-shot aims for a single launch, and a leaked
    /// filter would silently narrow the next unrelated query.
    fn clear_stale_open_target(&mut self) {
        if self.palette_visible {
            return;
        }
        self.active_kind_filter.clear();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.open_target != crate::shell::chrome_state::OpenTarget::Beside {
            st.open_target = crate::shell::chrome_state::OpenTarget::Beside;
            st.bump();
        }
    }

    /// Test-only one-shot: retire a dismissed palette's aim and filter.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_clear_stale_open_target_for_tests(&mut self) {
        self.clear_stale_open_target();
    }

    /// Clears a typed wifi passphrase once the quick-settings popover
    /// closes, so it doesn't outlive the window it was entered in.
    fn clear_stale_wifi_password(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let open = chrome.borrow().overlays.quick_settings;
        if open {
            return;
        }
        let mut st = chrome.borrow_mut();
        if st.quick_settings.wifi_password.take().is_some() {
            st.bump();
        }
    }

    /// Pans the focused group into view, edge-triggered on the persisted
    /// `active_group` pointer so free wheel-scrolling isn't fought by a
    /// per-frame snap-back.
    fn scroll_active_group_into_view(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
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
                // A group focused the same frame it appeared has no rect yet
                // (no layout pass); leave the memo and retry once it does.
                if !row.group_is_laid_out(group_id) {
                    continue;
                }
                // Eased, not teleported — matches Niri's focus-column slide.
                row.scroll_group_into_view_animated(group_id, Instant::now());
            }
            self.last_scrolled_active_group.insert(strip_id, group_id);
        }
    }

    /// Lights exactly one on-screen window group as keyboard-focused, since
    /// non-overlapping windows have no z-order to infer it from. Runs every
    /// frame as a pure function of the tree (not edge-triggered) so focus
    /// can't stay latched on a group a resync moved or removed.
    fn mark_focused_group(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Closing groups aren't focus candidates — lighting one mid-fade
        // would tell the user their keystrokes go somewhere gone.
        let closing: std::collections::HashSet<GroupId> = self
            .group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups
                    .iter()
                    .filter(|g| g.is_closing())
                    .map(|g| g.id)
                    .collect::<Vec<_>>()
            })
            .collect();
        // Mirrors `publish_strip_viewport`'s resolution, including the
        // fallback to the first strip/leftmost group for an unseeded pointer.
        let focused: Option<(StripId, GroupId)> = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.active_workspace()
                .and_then(|ws| {
                    tree.active_strip
                        .get(&ws.id)
                        .copied()
                        .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                        .or_else(|| ws.strips.first().map(|s| s.id))
                })
                .and_then(|sid| {
                    tree.resolve_active_group(sid, |gid| closing.contains(&gid))
                        .map(|gid| (sid, gid))
                })
        };
        // A lone window needs no marker; it's a workspace-wide count so
        // three strips holding one window each still disambiguate.
        let disambiguating = self
            .group_row_ptrs
            .values()
            .map(|&ptr| {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups.len()
            })
            .sum::<usize>()
            > 1;
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                g.set_focused(disambiguating && focused == Some((strip_id, g.id)));
            }
        }
    }

    /// Publishes the active strip's laid-out geometry for
    /// [`crate::shell::placement`] to consult. Doesn't bump the chrome
    /// revision — this reports layout output, not a mutation to resync against.
    fn publish_strip_viewport(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Same resolution the placement rule performs.
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

    /// Routes a cross-strip tab drop: hit-tests the release point against
    /// other strips' rects and delivers via
    /// [`GroupRow::accept_cross_strip_drop`], or returns the tab to its
    /// source strip when none matches — returning to source avoids
    /// mutating the persisted tree outside the explicit strip-creation UI.
    ///
    /// SAFETY: each pointer addresses a `GroupRow` owned by the content
    /// tree, the host outlives the content, and each row is only ever
    /// touched by one reconcile sweep at a time — the frame loop runs
    /// them sequentially, never re-entrantly.
    fn dispatch_cross_strip_detaches(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        // Drain sources first: a later `accept_cross_strip_drop` borrows
        // the destination row mutably, conflicting with a held source
        // payload reference.
        let mut detaches: Vec<(StripId, CrossStripDetach)> = Vec::new();
        let mut strip_rects: Vec<(StripId, Rect)> = Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and reconcile sweeps
            // run sequentially so no other path is aliasing.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            strip_rects.push((strip_id, row.get_rect()));
            if let Some(payload) = row.take_pending_cross_strip_detach() {
                detaches.push((strip_id, payload));
            }
        }
        if detaches.is_empty() {
            return;
        }
        // Hit-test release.y only: strips span the full content width, so
        // the destination row resolves the gap by x itself.
        for (src_strip_id, payload) in detaches {
            let release = payload.release_point;
            // Not a mismatched-field typo: `release.y` is deliberately
            // checked against the *same* rect's paired `y0`/`y1` bounds to
            // test membership in its vertical band (see the hit-test note
            // above), not against a second rect's fields.
            #[allow(
                clippy::suspicious_operation_groupings,
                reason = "release.y >= rect.y0 && release.y <= rect.y1 is a deliberate \
                          single-field range check against one rect's paired bounds, not a \
                          copy-pasted comparison that meant to swap in a different field"
            )]
            let dest_strip_id = strip_rects
                .iter()
                .find(|(sid, rect)| {
                    *sid != src_strip_id && release.y >= rect.y0 && release.y <= rect.y1
                })
                .map_or(src_strip_id, |(sid, _)| *sid);
            // SAFETY: same contract as the drain above. We're mutating a
            // single row at a time and never aliasing across rows.
            let Some(&dest_ptr) = self.group_row_ptrs.get(&dest_strip_id) else {
                continue;
            };
            let dest_row: &mut GroupRow = unsafe { &mut *dest_ptr };
            dest_row.accept_cross_strip_drop(payload.desc, payload.host, release);
            // Mirror into the persisted tree so a restart sees the tab
            // in its new strip.
            if dest_strip_id != src_strip_id
                && let Some(chrome) = self.chrome.clone()
            {
                let st = chrome.borrow_mut();
                st.tree_mut()
                    .move_tab_across_strips(src_strip_id, dest_strip_id, payload.tab_id);
            }
        }
    }

    /// Turns app-launcher worker-thread failures into notifications (a clean
    /// launch/exit is silent — the window is the feedback). Returns whether
    /// anything was posted, so the caller can mark the frame dirty.
    pub(crate) fn drain_launch_events(&self) -> bool {
        use crate::shell::launcher::LaunchEventKind;
        let events = crate::shell::launcher::drain_events();
        if events.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            return false;
        };
        let mut posted = false;
        for event in events {
            // Only a failure settles the "Opening…" notice here. `Started`
            // doesn't — the window is still seconds away — that's
            // [`Self::retire_notices_for_opened_windows`]'s job; `Exited`
            // doesn't either, by then the app has run.
            if matches!(
                event.kind,
                LaunchEventKind::SpawnFailed { .. } | LaunchEventKind::Failed { .. }
            ) && let Some(notice) =
                crate::shell::catalogue::take_pending_notice(&event.package_id)
            {
                let mut st = chrome_handle.borrow_mut();
                st.dismiss_notification(notice);
                st.bump();
                posted = true;
            }
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

    /// Retires the "Opening…" notice of any app now on screen. Neither
    /// `Started` (forked, no window yet) nor an adopted-but-not-presenting
    /// tab counts — what settles it is the app's first frame or placeholder
    /// card, whichever shows first, handing over to the window's own place.
    pub(crate) fn retire_notices_for_opened_windows(&self) -> bool {
        let pending = crate::shell::catalogue::pending_notices();
        if pending.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            return false;
        };
        let painted: Vec<String> = {
            let st = chrome_handle.borrow();
            st.tree()
                .workspaces
                .iter()
                .flat_map(|w| w.strips.iter())
                .flat_map(|s| s.window_groups.iter())
                .flat_map(|g| g.tabs.iter())
                .filter_map(|t| match &t.tile {
                    crate::shell::state::TileContent::Foreign { app_id, surface_id }
                        if {
                            let store = crate::compositor::foreign::store();
                            store.is_presented(*surface_id) || store.is_opening(*surface_id)
                        } =>
                    {
                        Some(app_id.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        if painted.is_empty() {
            return false;
        }
        let mut retired = false;
        for (package_id, notice) in pending {
            let showing = painted
                .iter()
                .any(|id| crate::shell::launcher::app_id_matches_package(id, &package_id));
            if !showing {
                continue;
            }
            if crate::shell::catalogue::take_pending_notice(&package_id).is_some() {
                let mut st = chrome_handle.borrow_mut();
                st.dismiss_notification(notice);
                st.bump();
                retired = true;
            }
        }
        retired
    }

    pub fn pump_permission_prompt(&mut self) {
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
            // Pop only if the head still belongs to the prompt clicked —
            // a mid-frame portal push could otherwise apply the grant to
            // the wrong app.
            let head_matches = st
                .pending_grants
                .front()
                .is_some_and(|p| p.app_id == widget_app_id);
            if head_matches && let Some(mut pending) = st.pending_grants.pop_front() {
                let app_id = pending.app_id.clone();
                let capability = pending.capability.clone();
                match choice {
                    // `Allow` is once-only (treated as satisfied for this
                    // single call, not persisted) and `Deny` persists
                    // nothing (absence of a grant reads as deny) — neither
                    // writes a grant, so they share this no-op arm.
                    PromptChoice::Allow | PromptChoice::Deny => {}
                    PromptChoice::Session => {
                        let cap = with_grant_scope(capability, GRANT_SESSION);
                        record_app_grant(&st, &app_id, &cap);
                    }
                    PromptChoice::Always => {
                        let cap = with_grant_scope(capability, GRANT_ALWAYS);
                        record_app_grant(&st, &app_id, &cap);
                    }
                }
                // Release the blocked portal call — otherwise the D-Bus
                // method awaiting the oneshot never returns and the app
                // hangs on its own Open dialog.
                pending.answer(prompt_choice_to_outcome(choice));
                st.bump();
            }
            // Discharged either way — drop it so the next frame latches
            // fresh or paints nothing.
            self.permission_prompt = None;
        }

        // Step 2 — latch a new prompt when the queue's front needs
        // surfacing and we don't already hold a widget.
        if self.permission_prompt.is_none() {
            let st = chrome_handle.borrow();
            if let Some(pending) = st.pending_grants.front() {
                let label = capability_label(&pending.capability);
                let is_owner = is_active_workspace_owner(&st);
                self.permission_prompt = Some(PermissionPrompt::new(
                    pending.app_id.clone(),
                    label,
                    is_owner,
                ));
            }
        }
    }

    /// Attach the portal's queues so the per-frame pumps have something
    /// to drain. Called by the compositor once, at startup, after
    /// `portal::serve` succeeds.
    pub fn set_portal_queues(
        &mut self,
        prompts: crate::portal::PromptQueueHandle,
        pickers: crate::portal::FilePickerQueue,
    ) {
        self.portal_prompts = Some(prompts);
        self.file_picker_overlay.set_queue(pickers);
    }

    /// The live file-chooser dialog, if one is up. For the frame
    /// builder's widget walk and for tests.
    pub fn file_picker(&self) -> Option<&ui::widget::FilePicker> {
        self.file_picker_overlay.widget()
    }

    pub fn file_picker_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.file_picker_overlay.widget_mut()
    }

    /// Mounts queued file-chooser dialogs and resolves whichever one the
    /// user just finished. Returns whether anything changed, so the caller
    /// can repaint.
    pub fn pump_file_picker(&mut self) -> bool {
        self.file_picker_overlay.pump(self.chrome.as_ref())
    }

    /// Moves buffered portal prompts onto the chrome's queue for
    /// `pump_permission_prompt`. Senders travel with them rather than being
    /// dropped — dropping one would answer deny before the user was asked.
    pub fn drain_portal_prompts(&mut self) -> bool {
        let Some(prompts) = self.portal_prompts.as_ref() else {
            return false;
        };
        let drained = prompts.drain();
        if drained.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            // No chrome to render a prompt — answer deny now rather than
            // hold senders that would never be fulfilled.
            for pending in drained {
                let _ = pending.reply.send(crate::portal::PromptOutcome::Deny);
            }
            return false;
        };
        let mut st = chrome_handle.borrow_mut();
        for pending in drained {
            st.pending_grants
                .push_back(crate::shell::chrome_state::PendingPrompt::with_reply(
                    pending.caller.app_id.clone(),
                    pending.capability,
                    pending.reply,
                ));
        }
        st.bump();
        true
    }

    /// Test-only: runs the per-frame permission-prompt pump outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_permission_prompt_for_tests(&mut self) {
        self.pump_permission_prompt();
    }

    /// Test-only accessor for the held [`PermissionPrompt`].
    #[cfg(any(test, debug_assertions))]
    pub fn permission_prompt_mut_for_tests(&mut self) -> Option<&mut PermissionPrompt> {
        self.permission_prompt.as_mut()
    }

    /// Test-only: mutable access to the [`UiWindowGroup`] at
    /// `(strip_id, group_idx)`.
    ///
    /// SAFETY mirrors [`Self::with_group_row_ptrs`]: the pointer is valid
    /// for the host's lifetime, and the `&mut self` borrow on this method
    /// keeps any other reconcile path from aliasing.
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

    /// Test-only: runs the per-frame active-group reconcile outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_active_group_reconcile_for_tests(&mut self) {
        self.reconcile_active_group_off_closing();
    }

    /// Test-only: runs the per-frame maximize-change writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_maximize_harvest_for_tests(&mut self) {
        self.harvest_pending_maximize_changes();
    }

    /// Test-only: runs the per-frame width-change writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_width_harvest_for_tests(&mut self) {
        self.harvest_pending_width_changes();
    }

    /// Test-only: runs the per-frame cross-strip drag-drop router outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_cross_strip_dispatch_for_tests(&mut self) {
        self.dispatch_cross_strip_detaches();
    }

    /// Test-only: runs the per-frame whole-group reorder writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_group_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_group_reorder();
    }

    /// Test-only accessor for the sidebar wheel's laid-out rect. `None` means
    /// no sidebar; zero-area means suppressed by a fullscreen window.
    #[cfg(any(test, debug_assertions))]
    pub fn sidebar_rect_for_tests(&self) -> Option<vello::kurbo::Rect> {
        self.sidebar.as_ref().map(|s| s.get_rect())
    }

    /// Test-only: runs the per-frame intra-group tab reorder writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_tab_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_tab_reorder();
    }

    /// Test-only: runs the per-frame decoration-search-chip handling outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_search_open_for_tests(&mut self) {
        self.harvest_pending_search_open();
    }

    /// Test-only harvest for the tab strip's "+" chip.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_new_tab_for_tests(&mut self) -> bool {
        self.harvest_pending_new_tab()
    }

    /// Test-only: mirrors the widget-side active tab into the tree. Must run
    /// before the focus-follow scroll, or the tree reads one frame behind
    /// the clicks that just happened.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_active_tab_writeback_for_tests(&mut self) {
        self.harvest_active_tab_writeback();
    }

    /// Test-only: runs the focus-follow scroll reconcile outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_scroll_active_into_view_for_tests(&mut self) {
        self.scroll_active_group_into_view();
    }

    /// The sidebar wheel disc, downcast from the `Box<dyn Widget>`. For the
    /// screenshot harness to hold the settle animation at a chosen point —
    /// a still frame can't show motion, and offscreen rendering outruns it.
    pub fn sidebar_wheel_mut_for_tests(&mut self) -> Option<&mut SidebarWheelDisc> {
        self.sidebar
            .as_mut()?
            .as_any_mut()?
            .downcast_mut::<SidebarWheelDisc>()
    }

    pub fn group_row_mut_for_tests(&mut self, strip_id: StripId) -> Option<&mut GroupRow> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        Some(unsafe { &mut *ptr })
    }

    /// Selects `tab` inside `group` on the live widget tree (the
    /// authoritative side — `harvest_active_tab_writeback` mirrors it into
    /// the persisted tree each frame). Used by the compositor's Alt+Shift
    /// wheel jump.
    pub fn focus_window_widget(&mut self, strip_id: StripId, group: GroupId, tab: u64) -> bool {
        let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.set_group_active_tab(group, tab)
    }

    /// Moves focus one or more windows along the active strip — the single
    /// implementation behind keyboard, scroll, touchpad and modifier-wheel
    /// navigation, so they can't drift into models that disagree at group
    /// boundaries. A "window" is a tab in one flat sequence across groups
    /// (stepping past a group's last tab continues into the next rather
    /// than wrapping), clamped at the strip's ends. Widget written before
    /// tree, per [`Self::focus_window_widget`].
    pub fn step_focus_along_strip(&mut self, steps: i32) -> bool {
        if steps == 0 {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = {
            let st = chrome.borrow();
            let tree = st.tree();
            crate::compositor::client_input::step_active_window(&tree, steps)
        };
        let Some((strip, group, tab)) = target else {
            return false;
        };
        self.focus_window_on_strip(strip, group, tab)
    }

    /// Focuses one window by identity, then pans the strip to it (unlike a
    /// click on the window itself) since the rail addresses windows that may
    /// be off screen.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "st's only use is producing tree via tree_mut(); the write-guard's \
                  lifetime is tied to st's borrow, so it can't be dropped before tree's \
                  last use, and the block already ends right there"
    )]
    pub fn focus_window_on_strip(&mut self, strip: StripId, group: GroupId, tab: u64) -> bool {
        self.focus_window_widget(strip, group, tab);
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut changed = false;
        {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            if let Some(g) = tree
                .active_workspace_mut()
                .and_then(|ws| ws.strips.iter_mut().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter_mut().find(|g| g.id == group))
                && g.active_tab != tab
            {
                g.active_tab = tab;
                changed = true;
            }
            if tree.active_group_for(strip) != Some(group) {
                tree.set_active_group(strip, group);
                changed = true;
            }
        }
        if changed {
            chrome.borrow_mut().bump();
        }
        changed
    }

    /// Focus the first window of `group` — what clicking a rail segment
    /// means, since a segment stands for a whole group.
    pub fn focus_group_on_strip(&mut self, strip: StripId, group: GroupId) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let tab = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.active_workspace()
                .and_then(|ws| ws.strips.iter().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter().find(|g| g.id == group))
                .map(|g| g.active_tab)
        };
        let Some(tab) = tab else { return false };
        self.focus_window_on_strip(strip, group, tab)
    }

    /// Keep local focus attached to the followed participant's current tab.
    /// Missing location is not guessed: the existing view stays put until a
    /// fresh live target arrives.
    fn reconcile_followed_peer(&mut self) -> bool {
        let Some(peer_id) = self.followed_peer.as_deref() else {
            return false;
        };
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = {
            let state = chrome.borrow();
            let tree = state.tree();
            let roster: &[crate::shell::state::Collaborator] = match tree.active_workspace() {
                Some(workspace) if !workspace.collaborators.is_empty() => &workspace.collaborators,
                _ => &tree.collaborators,
            };
            let tab = roster
                .iter()
                .find(|participant| participant.id == peer_id && !participant.invited)
                .and_then(|participant| participant.active_tab);
            tab.and_then(|tab| {
                tree.active_workspace()?.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .any(|candidate| candidate.id == tab)
                            .then_some((strip.id, group.id, tab))
                    })
                })
            })
        };
        let Some((strip, group, tab)) = target else {
            return false;
        };
        self.focus_window_on_strip(strip, group, tab)
    }

    fn followed_roster_row(&self) -> Option<RosterRow> {
        let peer_id = self.followed_peer.as_deref()?;
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        collaborator_roster_rows(&state.tree())
            .into_iter()
            .find(|row| row.id == peer_id && row.followable)
    }

    /// Cached "+ Add workspace" tile rect, `Rect::ZERO` while the overview
    /// is hidden. Lets tests assert it's on screen and click its centre.
    pub fn add_workspace_rect_for_tests(&self) -> Rect {
        self.add_workspace_rect
    }

    /// Test-only: a strip's `GroupRow` rect in screen-space. `None` if no
    /// row is registered for the strip, `Rect::ZERO` if not yet laid out.
    /// Used by cross-strip drag tests to pick a release point in another
    /// strip's band without re-walking the taffy tree. Mirrors the
    /// SAFETY contract on [`Self::groups_per_strip`].
    #[cfg(any(test, debug_assertions))]
    pub fn group_row_rect_for_tests(&self, strip_id: StripId) -> Option<Rect> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &GroupRow = unsafe { &*ptr };
        Some(row.get_rect())
    }

    /// Screen-space rect of the content area (below the tab bar) of
    /// window group `group_id`, or `None` if no live row hosts it yet.
    /// The compositor's input router uses this to translate a cursor
    /// position into a foreign toplevel's surface-local coordinates,
    /// since the group's active tile rect *is* the surface's rect.
    ///
    /// SAFETY mirrors [`Self::groups_per_strip`].
    pub fn active_tile_rect(&self, group_id: u64) -> Option<Rect> {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            if let Some(group) = row.groups.iter().find(|g| g.id == group_id) {
                return group.active_host().map(ui::Widget::get_rect);
            }
        }
        None
    }

    /// SAFETY mirrors the rest of the `group_row_ptrs` accessors: pointers
    /// were captured from a `Box<GroupRow>` we own through the content
    /// tree, the host outlives the content, and `&self` here keeps the
    /// reconcile sweeps from racing the read.
    pub fn groups_per_strip(&self) -> GroupsPerStrip {
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

    /// Cached WS-settings tab-pill rect for `tab`, populated while the
    /// modal is visible. Every [`WsTab`] has a real pill, so every tab has
    /// a rect.
    pub fn ws_settings_tab_pill_rect(&self, tab: WsTab) -> Rect {
        let idx = match tab {
            WsTab::General => 0,
            WsTab::Members => 1,
            WsTab::Permissions => 2,
            WsTab::Apps => 3,
        };
        self.ws_settings_tab_pill_rects
            .get(idx)
            .copied()
            .unwrap_or(Rect::ZERO)
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

    /// The peer-sync card's buttons, named `join` / `cancel` / `copy` /
    /// `paste`.
    pub fn peer_sync_prompt_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.peer_sync_overlay.button_rects_for_tests()
    }

    pub fn peer_sync_snapshot(&self) -> PeerSyncSnapshot {
        self.peer_sync_overlay.snapshot()
    }

    pub fn peer_sync_card_visible(&self) -> bool {
        self.peer_sync_overlay.visible()
    }

    #[cfg(test)]
    pub fn set_peer_sync_snapshot_for_tests(&mut self, snapshot: PeerSyncSnapshot) {
        self.peer_sync_overlay.set_snapshot_for_tests(snapshot);
    }

    /// The collaborator dropdown's Invite / Join buttons, as painted last
    /// frame (not recomputed) — `Rect::ZERO` when closed or not permitted,
    /// so a test click on a missing button fails instead of hitting the origin.
    pub fn collaborators_action_rects_for_tests(&self) -> (Rect, Rect) {
        self.collaborators_overlay.action_rects()
    }

    /// The "Room activity" row at the foot of the same dropdown, on the
    /// same terms.
    pub fn collaborators_activity_rect_for_tests(&self) -> Rect {
        self.collaborators_overlay.activity_rect()
    }

    /// Revoke-button rects painted by [`Self::draw_ws_settings`] while
    /// the Permissions tab is active, each keyed by the capability id it
    /// revokes. Empty whenever the modal is closed or another tab is
    /// active.
    pub fn permissions_revoke_rects(&self) -> &[(String, Rect)] {
        &self.permissions_revoke_rects
    }

    /// Grant-composer control rects painted while the Permissions tab is
    /// active. `None` when the composer isn't on screen.
    pub fn grant_composer_rects(&self) -> Option<&GrantComposerRects> {
        self.grant_composer_rects.as_ref()
    }

    /// Per-member control rects painted while the Members tab is active:
    /// `(member, role chip, Remove button)`.
    pub fn member_control_rects(&self) -> &[(crate::shell::state::UserId, Rect, Rect)] {
        &self.member_control_rects
    }

    /// Open the palette in `scope`, or close it if the same scope fires
    /// twice (mirrors the old second-press-dismisses chord behaviour). A
    /// different scope while open just swaps scope without losing the
    /// typed query. `ActiveApp` degrades to `All` when no tab is focused.
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
            // Empty query still runs through the registry, so the palette
            // opens with an "everything" list rather than blank.
            self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        }
    }

    /// Refreshes the palette's rows by fanning the query out to every
    /// registered [`crate::shell::search::SearchProvider`]. Cheap on a warm
    /// cache; first-boot latency comes from the tantivy walker's initial pass.
    fn refresh_search_results(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let current_query = self.palette.query().to_string();
        // Short-circuits no-op refreshes from peer-state polling or palette re-entry.
        if current_query == self.last_search_query {
            return;
        }
        self.last_search_query.clone_from(&current_query);
        let (scope, active_provider) = {
            let st = chrome.borrow();
            let scope = st.search_scope;
            let active_provider = active_tab_provider_id(&st, &self.search_registry);
            (scope, active_provider)
        };
        let groups = {
            let st = chrome.borrow();
            self.search_registry.query_grouped(
                &crate::shell::search::QueryArgs {
                    needle: &current_query,
                    scope,
                    active_provider_id: active_provider.as_deref(),
                    wanted_kinds: &self.active_kind_filter,
                    chrome: &st,
                    doc,
                    doc_root: root,
                },
                PALETTE_ROW_BUDGET,
            )
        };

        // Kind travels as the row's section and glyph, not a `[Files]`
        // literal on the subtitle, which pushed the distinguishing part
        // of a path off the right edge.
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

        // Peer-sync rows are ordinary `ActionsProvider` results, ranked
        // against everything else rather than appended below every hit.
        self.palette.set_entries(entries);
        self.search_results = results;
        // New entry count needs a new palette node height on the next
        // rebuild, or the palette stays pinned at its first-paint size.
        self.palette_layout_dirty = true;
    }

    /// Mirrors any widget-side group missing from the persisted tree into
    /// it — closes the writeback gap for
    /// [`ui::widget::GroupRow::split_tab_into_new_group`], whose drag
    /// handler creates a widget-side group with no automerge access.
    /// Without this, [`Self::resync_strips_from_tree`] would see "widget
    /// has X, tree doesn't" and evict the dragged-out group.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "both guards already end at the block that binds them, right where \
                  `strip_tree_ids` (built from them) is handed back; nothing to tighten \
                  without cloning the tree out for no real gain"
    )]
    fn harvest_widget_only_groups_into_tree(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Skip if the chrome revision moved since our snapshot (tree just
        // changed) — `resync_strips_from_tree` will catch up, and running
        // here would re-add a legitimately-removed group. Widget-side
        // splits don't bump, so they stay visible to this harvest.
        {
            let cur_rev = chrome.borrow().revision;
            if cur_rev != self.last_chrome_revision {
                return;
            }
        }
        // Collect under an immutable borrow, apply under the mutable
        // one — same shape as the other harvesters.
        let mut writes: Vec<(StripId, usize, crate::shell::state::WindowGroup)> = Vec::new();
        // Snapshot each strip's known tab ids under one read borrow, then
        // drop it — the unsafe row-pointer walk below doesn't touch the
        // tree, so there's no reason to hold the lock across it.
        let strip_tree_ids: Vec<(StripId, std::collections::HashSet<u64>)> = {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(ws) = tree.workspaces.iter().find(|w| w.id == tree.active) else {
                return;
            };
            ws.strips
                .iter()
                .map(|strip| (strip.id, strip.window_groups.iter().map(|g| g.id).collect()))
                .collect()
        };
        for (strip_id, tree_ids) in strip_tree_ids {
            let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
                continue;
            };
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor.
            let row: &GroupRow = unsafe { &*ptr };
            for (idx, wg) in row.groups.iter().enumerate() {
                if !tree_ids.contains(&wg.id) {
                    writes.push((strip_id, idx, build_state_group_from_widget(wg)));
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        for (strip_id, idx, group) in writes {
            for ws in &mut tree.workspaces {
                if let Some(strip) = ws.strips.iter_mut().find(|s| s.id == strip_id) {
                    let insert_at = idx.min(strip.window_groups.len());
                    strip.window_groups.insert(insert_at, group.clone());
                }
            }
        }
    }

    /// `(strip_id, group_id, rect)` triples from the live per-strip
    /// [`GroupRow`] pointers, in traversal order. Shared by the
    /// click-focus stamp and the file-drop router so both resolve a
    /// pointer coordinate through the same post-layout geometry.
    fn group_hit_candidates(&self) -> Vec<(StripId, GroupId, Rect)> {
        let mut candidates: Vec<(StripId, GroupId, Rect)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content, and the `&self`
            // borrow keeps any reconcile sweep from aliasing the read.
            let row: &GroupRow = unsafe { &*ptr };
            for g in &row.groups {
                candidates.push((strip_id, g.id, g.get_rect()));
            }
        }
        candidates
    }

    /// The window group directly under `p`, via the click-focus
    /// hit-test, so a file drop lands on exactly the group a click there
    /// would focus. `None` over empty strip/desktop space.
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
        let candidates = self.group_hit_candidates();
        let Some((strip_id, group_id)) = pick_group_at(&candidates, p) else {
            return;
        };
        // Mirror any widget-only group into the tree first: the harvest
        // reconciler runs at frame boundaries, after this dispatch, and
        // an `active_group` the tree doesn't know leaves `active_tab_id`
        // as None.
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
        drop(tree);
        drop(st);
        // Marks this strip at rest so a click doesn't also pan: the
        // target was visible to be clicked, and panning would drag it
        // out from under the cursor. Keyboard/wheel keep the pan, since
        // they can target off-screen windows.
        self.last_scrolled_active_group.insert(strip_id, group_id);
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

    /// Dispatch the selected palette row through the provider callback
    /// (`result:N`) or the legacy chrome-command dispatcher (`peer:*`,
    /// a bridge until peer commands get their own provider).
    fn activate_selected_result(&mut self, doc: &mut automerge::AutoCommit, root: &ObjId) {
        let Some(id) = self
            .palette
            .selected_id()
            .map(std::string::ToString::to_string)
        else {
            return;
        };
        if let Some(idx) = id.strip_prefix("result:")
            && let Ok(idx) = idx.parse::<usize>()
            && let Some(result) = self.search_results.get(idx).cloned()
        {
            if let Some(chrome) = self.chrome.clone() {
                // No argument-collection step yet, so a
                // parameterised row refuses visibly rather than
                // doing nothing.
                let args = crate::shell::search::action_api::ActionArgs::none();
                if let Err(err) = (result.on_open)(doc, root, &chrome, &args) {
                    chrome.borrow_mut().push_warning_notification(
                        format!("Can't run “{}”", result.title),
                        err.to_string(),
                    );
                }
            }
            self.palette_visible = false;
            return;
        }
        // Legacy id-based dispatch for peer-sync rows, which enqueue an
        // action drained by `take_peer_sync_actions`.
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

    /// The command-palette visibility flag, for tests that only hold the
    /// host through `Box<dyn Widget>`.
    pub fn palette_visible(&self) -> bool {
        self.palette_visible
    }

    /// Last pointer position this host knows about, or `None` before
    /// anything has moved a pointer through the session.
    pub fn pointer_position(&self) -> Option<Point> {
        self.last_cursor
    }

    /// Record pointer position for a motion this widget tree never sees:
    /// a `CursorMoved` over a foreign client goes straight to it, so
    /// `last_cursor` (which hit-tests the position-less `MouseInput`)
    /// would otherwise go stale inside that window.
    pub fn note_pointer(&mut self, p: Point) {
        self.last_cursor = Some(p);
    }

    /// Whether the workspace wheel is summoned. Read alongside
    /// [`Self::palette_visible`] so Escape is taken from the focused
    /// client only when the shell has something up to dismiss with it.
    pub fn wheel_summoned(&self) -> bool {
        self.chrome
            .as_ref()
            .is_some_and(|c| c.borrow().wheel_summon().is_some())
    }

    /// Whether the last draw found the layout it was handed isn't what
    /// it needs (the filter band growing a row is only measurable by
    /// painting). A peek, not a take, so a caller that paints once (a
    /// screenshot) can notice it must lay out and paint again.
    pub fn structure_dirty(&self) -> bool {
        self.palette_layout_dirty
    }

    /// The palette's query, the id Enter would activate, and the rows on
    /// offer — otherwise a keystroke-driven harness can't tell which row
    /// is selected without "press Enter and see what changed".
    pub fn palette_snapshot(&self) -> (bool, String, Option<String>, Vec<(String, String)>) {
        (
            self.palette_visible,
            self.palette.query().to_string(),
            self.palette
                .selected_id()
                .map(std::string::ToString::to_string),
            self.palette
                .visible_entries()
                .into_iter()
                .map(|e| (e.id.clone(), e.label.clone()))
                .collect(),
        )
    }

    /// Result kinds the palette is filtered to, as chip labels. Empty
    /// means every kind is on offer — otherwise "filtered to apps" and
    /// "a query only apps matched" produce the same row list.
    pub fn palette_kind_filter(&self) -> Vec<String> {
        self.active_kind_filter
            .iter()
            .map(|k| k.label().to_string())
            .collect()
    }

    /// Every strip's horizontal pan, keyed by strip id, paired with its
    /// max — on a strip whose content fits the display the pan is pinned
    /// at zero, so "the viewport didn't move" needs the bound to mean
    /// anything.
    pub fn strip_scroll_offsets(&self) -> Vec<(u64, f64, f64)> {
        let mut out: Vec<(u64, f64, f64)> = self
            .group_row_ptrs
            .iter()
            .map(|(sid, &ptr)| {
                // SAFETY: same contract as every other `group_row_ptrs`
                // accessor — the pointer is captured from a `Box<GroupRow>`
                // owned through the content tree, and the host outlives it.
                let row: &GroupRow = unsafe { &*ptr };
                (sid.0, row.scroll_x(), row.max_scroll_x())
            })
            .collect();
        out.sort_by_key(|(sid, _, _)| *sid);
        out
    }

    /// The rows on offer with their subtitles — [`Self::palette_snapshot`]
    /// carries labels only, not the badge/source-marker text a harness
    /// needs to tell "already have this" from "fetches this" apart.
    pub fn palette_rows_detailed(&self) -> Vec<(String, String, Option<String>)> {
        self.palette
            .visible_entries()
            .into_iter()
            .map(|e| (e.id.clone(), e.label.clone(), e.description.clone()))
            .collect()
    }

    /// Focus the palette's query field. `open_palette_with_scope` does
    /// this; harnesses that force `palette_visible` need it too or they
    /// capture the unfocused field.
    pub fn set_palette_focus(&mut self, focused: bool) {
        self.palette.set_focus(focused);
    }

    /// Force-set the palette visibility, so headless harnesses can drive
    /// the palette scene without a synthetic Ctrl+K.
    pub fn set_palette_visible(&mut self, v: bool) {
        self.palette_visible = v;
    }

    /// The room record index the records provider reads and writes.
    /// Shared, not copied — a caller referencing a record here changes
    /// what the next keystroke finds.
    pub fn room_records(
        &self,
    ) -> std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>> {
        std::rc::Rc::clone(&self.room_records)
    }

    /// Runs the search fan-out directly, for `bin/shots` after forcing
    /// the palette open — scenes bypass the per-keystroke refresh.
    pub fn refresh_search_results_for_shots(
        &mut self,
        doc: &mut automerge::AutoCommit,
        root: &ObjId,
    ) {
        // Drop the no-op short-circuit, or an empty-query scene matches
        // the initially-empty `last_search_query` and runs no provider.
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        self.refresh_search_results(doc, root);
    }

    /// Builds a headless [`ActionCatalog`](crate::shell::search::action_api::ActionCatalog)
    /// over the same provider registry that feeds the Ctrl+K palette, so a
    /// script or LLM navigator can drive it with no scene. `visibility` is
    /// the permission seam — a restricted caller only sees what it may.
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

    /// The command-palette query buffer, for the `shots` harness to
    /// assert a synthetic `Type("…")` landed.
    pub fn palette_query(&self) -> &str {
        self.palette.query()
    }

    /// Force-set the query buffer, for `shots` to seed a query while the
    /// palette is closed without round-tripping synthetic key events.
    pub fn set_palette_query(&mut self, q: &str) {
        self.palette.set_query(q);
    }

    /// Fast-forwards the overview crossfade so a headless single-frame
    /// render sees the settled layer, not the alpha-zero start.
    pub fn force_overview_settled(&mut self, target: bool) {
        let t = if target { 1.0 } else { 0.0 };
        self.overview_target = t;
        self.overview_t = t;
        self.transition_from = t;
        self.transition_started = None;
        // Same seeding the animated open does, so a single-frame paint
        // matches what a real open leaves behind.
        self.overview_selection = if target {
            self.overview_here_index()
        } else {
            None
        };
    }

    /// Single-frame settle for the palette's entry animation, so a
    /// one-frame headless scene doesn't land at alpha 0 — which is
    /// indistinguishable from no overlay at all. Test-only.
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

    /// Folds every reading the quick-settings workers delivered into
    /// chrome state. Never blocks — the reads happen on a worker, since
    /// a shell-out on the paint path froze the desktop. Each field
    /// applies only if actually read, so a missing daemon leaves the
    /// previous value rather than blanking it.
    fn apply_quick_settings_readings(&mut self) {
        let readings = self.qs_refresh.drain();
        if readings.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        for reading in readings {
            let mut st = chrome.borrow_mut();
            Self::apply_one_qs_reading(&mut st, reading);
        }
    }

    /// Unpacks a [`QsReading::Fast`] payload, writing its non-scalar fields
    /// (radios, VPNs, privacy) straight to chrome state and returning the
    /// scalar ones for [`Self::apply_one_qs_reading`]'s shared apply chain
    /// — split out to keep that function under the line-count ceiling.
    fn apply_fast_qs_reading(
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: Box<crate::shell::qs_refresh::FastReading>,
    ) -> QsFastFields {
        use crate::shell::qs_refresh::FastReading;
        let FastReading {
            brightness,
            volume,
            wifi,
            bluetooth,
            power,
            wired,
            battery,
            radios,
            privacy,
            vpns,
        } = *reading;
        if let Some(radios) = radios {
            st.quick_settings.radios = radios;
        }
        if let Some(vpns) = vpns {
            st.quick_settings.vpns = vpns;
        }
        st.quick_settings.privacy = privacy;
        (
            brightness,
            volume,
            wifi,
            bluetooth,
            power,
            wired,
            Some(battery),
        )
    }

    /// Applies a single quick-settings worker reading to chrome state and
    /// bumps the revision — the per-iteration body of
    /// [`Self::apply_quick_settings_readings`], split out to keep that
    /// function under the line-count ceiling.
    fn apply_one_qs_reading(
        st: &mut crate::shell::chrome_state::ChromeState,
        reading: crate::shell::qs_refresh::QsReading,
    ) {
        use crate::shell::qs_refresh::QsReading;
        let (brightness, volume, wifi, bluetooth, power, wired, battery, wifi_nets, bt_devices) =
            match reading {
                QsReading::Fast(reading) => {
                    let (brightness, volume, wifi, bluetooth, power, wired, battery) =
                        Self::apply_fast_qs_reading(st, reading);
                    (
                        brightness, volume, wifi, bluetooth, power, wired, battery, None, None,
                    )
                }
                QsReading::WifiNetworks(nets) => {
                    (None, None, None, None, None, None, None, nets, None)
                }
                QsReading::BtDevices(devs) => {
                    (None, None, None, None, None, None, None, None, devs)
                }
                QsReading::Firmware(updates) => {
                    st.quick_settings.firmware_updates = updates.map(|v| v.len());
                    st.bump();
                    return;
                }
            };
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
        if let Some(v) = wired {
            st.quick_settings.wired = Some(v);
        }
        if let Some(v) = battery {
            st.quick_settings.battery = v;
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

    /// See [`Self::force_palette_settled`].
    pub fn force_ws_settings_settled(&mut self) {
        if !self.overlay_flags().ws_settings {
            return;
        }
        self.ws_settings_was_open = true;
        self.ws_settings_transition = ui::Transition::open();
    }

    /// Font size the overview sets the current room's name at — the
    /// largest thing on the panel, since naming where you are is the
    /// first question it answers. A method, not a constant, so it tracks
    /// the active theme's typography token.
    pub fn overview_header_font_size(&self, theme: &Theme) -> f32 {
        Self::overview_header_font_size_for(&theme.tokens)
    }

    /// Dispatches the shell action for a palette command id, then closes
    /// the palette (even on an unknown id, to avoid a stale overlay).
    // No production caller routes a selection here, so `/invite` and
    // `/connect` are inert even under --enable-peer-sync. Kept as the
    // only definition of what those commands do; tests pin the contract.
    #[allow(dead_code)]
    pub(crate) fn dispatch_palette_command(&mut self, id: &str) -> bool {
        // Peer-sync rows enqueue an action for the compositor to drain
        // (Iroh stays on the runtime thread) rather than mutating
        // chrome directly. Ignored when the flag is off, in case a stale
        // palette row survives a toggle.
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
                "projection.open" => {
                    st.overlays.projection = true;
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
        // Largest step in the type scale — currently 40 px.
        tokens.typography.scale.x4l
    }

    fn overlay_flags(&self) -> crate::shell::chrome_state::OverlayFlags {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().overlays)
            .unwrap_or_default()
    }

    /// Mirror of the geometry in `draw_quick_settings` — the popover rect
    /// anchored under the top-right tray pill. Width matches the concept's
    /// `w-80` (320px); height sums the section bands in
    /// [`Self::draw_quick_settings`], including three rows of system tiles.
    fn quick_settings_rect(&self) -> Rect {
        let popover_w = 320.0_f64;
        let popover_h = 552.0_f64;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(x1 - popover_w, y0, x1, y0 + popover_h)
    }

    /// The strip ribbon for the active workspace, anchored to the content
    /// area's right edge. Costs no layout — it sits in
    /// [`STRIP_TRAILING_GUTTER_PX`], which the strips already leave clear.
    fn strip_ribbon(&self) -> ui::chrome::shell::StripRibbon {
        let (count, focused) = self
            .chrome
            .as_ref()
            .map_or((0, 0), |c| c.borrow().strip_focus());
        ui::chrome::shell::StripRibbon {
            content: Rect::new(
                self.rect.x0,
                self.rect.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX),
                self.rect.x1,
                self.rect.y1,
            ),
            count,
            focused,
        }
    }

    /// Ribbon geometry for integration tests, so a scenario can click a
    /// strip segment without duplicating the anchoring maths.
    pub fn strip_ribbon_for_tests(&self) -> ui::chrome::shell::StripRibbon {
        self.strip_ribbon()
    }

    /// The rail that maps the active strip: one segment per window group,
    /// the focused one marked, the ones on screen lit. Reads the live
    /// [`GroupRow`] rather than the tree — visibility is a layout question,
    /// and the tree knows only order.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "sid and order are each fully chained from tree (sid resolves the active \
                  strip, order re-reads that strip's groups); nothing to drop earlier \
                  without cloning the tree data out for no real gain, and the block already \
                  ends right after both are computed"
    )]
    fn strip_rail(&self) -> ui::chrome::shell::StripRail {
        let content = Rect::new(
            self.rect.x0,
            self.rect.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX),
            self.rect.x1,
            self.rect.y1,
        );
        let empty = ui::chrome::shell::StripRail {
            content,
            count: 0,
            focused: 0,
            visible: Vec::new(),
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return empty;
        };
        let (strip_id, order) = {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(sid) = tree.active_workspace().and_then(|ws| {
                tree.active_strip
                    .get(&ws.id)
                    .copied()
                    .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                    .or_else(|| ws.strips.first().map(|s| s.id))
            }) else {
                return empty;
            };
            let order: Vec<GroupId> = tree
                .active_workspace()
                .and_then(|ws| ws.strips.iter().find(|s| s.id == sid))
                .map(|s| s.window_groups.iter().map(|g| g.id).collect())
                .unwrap_or_default();
            (sid, order)
        };
        if order.is_empty() {
            return empty;
        }
        let focused = chrome
            .borrow()
            .tree()
            .active_group_for(strip_id)
            .and_then(|g| order.iter().position(|&id| id == g))
            .unwrap_or(0);
        let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
            return ui::chrome::shell::StripRail {
                content,
                count: order.len(),
                focused,
                visible: Vec::new(),
            };
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &GroupRow = unsafe { &*ptr };
        let onscreen = row.onscreen_group_ids();
        let visible = order
            .iter()
            .enumerate()
            .filter(|(_, id)| onscreen.contains(id))
            .map(|(i, _)| i)
            .collect();
        ui::chrome::shell::StripRail {
            content,
            count: order.len(),
            focused,
            visible,
        }
    }

    /// Rail geometry for integration tests, so a scenario can click a
    /// segment without duplicating the anchoring maths.
    pub fn strip_rail_for_tests(&self) -> ui::chrome::shell::StripRail {
        self.strip_rail()
    }

    /// The group a rail segment stands for, and the strip it is on.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "every field this reads is chained from the previous lookup (tree -> ws -> \
                  sid -> group), so the borrow can't be dropped before the last one without \
                  cloning each step early for no real gain"
    )]
    fn strip_rail_target(&self, segment: usize) -> Option<(StripId, GroupId)> {
        let chrome = self.chrome.as_ref()?;
        let st = chrome.borrow();
        let tree = st.tree();
        let ws = tree.active_workspace()?;
        let sid = tree
            .active_strip
            .get(&ws.id)
            .copied()
            .filter(|id| ws.strips.iter().any(|s| s.id == *id))
            .or_else(|| ws.strips.first().map(|s| s.id))?;
        let group = ws
            .strips
            .iter()
            .find(|s| s.id == sid)?
            .window_groups
            .get(segment)?
            .id;
        Some((sid, group))
    }

    /// The workspace-settings "Copy" button next to the invite link.
    ///
    /// Shared by the draw pass and the click router, so the rect that
    /// paints the button is the rect that can be clicked.
    fn ws_settings_copy_rect(&self) -> Rect {
        let body = self.ws_settings_body_rect(self.ws_settings_rect());
        let link_y = body.y0 + 202.0;
        Rect::new(body.x1 - 16.0 - 72.0, link_y, body.x1 - 16.0, link_y + 34.0)
    }

    /// Copy-button geometry for integration tests.
    pub fn ws_settings_copy_rect_for_tests(&self) -> Rect {
        self.ws_settings_copy_rect()
    }

    /// When the invite link was last copied, for integration tests.
    pub fn ws_invite_copied_at_for_tests(&self) -> Option<Instant> {
        self.ws_invite_copied_at
    }

    /// Public alias for [`Self::quick_settings_rect`] so integration
    /// tests can compute drill-in row coordinates without duplicating
    /// the popover-rect math.
    pub fn quick_settings_rect_for_tests(&self) -> Rect {
        self.quick_settings_rect()
    }

    /// May the local user invite into the room they're looking at? Asked
    /// through the same policy the palette's `peer.invite` row is filtered
    /// on, not re-decided here — a second opinion is a second security
    /// boundary. Fail-closed at every missing piece (no chrome, no
    /// workspace, peer-sync off).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "st feeds peer_sync_enabled, then tree(), then local_user_id right through to \
                  the final can_see() call built from workspace/actor, both borrowed from tree; \
                  nothing to drop earlier without cloning the tree data out for no real gain"
    )]
    fn may_invite_here(&self) -> bool {
        use crate::shell::search::action_api::{ActionVisibility, ResourceRef};
        use crate::shell::search::visibility::CapabilityVisibility;

        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let st = chrome.borrow();
        if !st.peer_sync_enabled {
            return false;
        }
        let tree = st.tree();
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let actor_id = st.local_user_for_room(&workspace.room);
        let actor = workspace.actor_for_person(&actor_id);
        CapabilityVisibility::new(workspace, actor).can_see(
            &ResourceRef::new("actions", "peer.invite"),
            &crate::shell::search::ResourceKind::Action,
        )
    }

    /// Lays the room-activity sheet out (by hand, not taffy, like the
    /// other floating overlays) and re-derives its rows from the document.
    fn refresh_room_activity(&mut self) {
        let viewport = Rect::new(
            self.rect.x0,
            self.rect.y0 + f64::from(self.sidebar_top_y),
            self.rect.x1,
            self.rect.y1,
        );
        self.room_activity.layout(viewport);
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow();
            let records = st.records().clone();
            let mut tree = st.tree_mut();
            self.room_activity.refresh(&mut tree);
            self.room_activity.refresh_records(&mut tree, &records);
        }
    }

    /// Keep passive chrome truthful while the sheet is closed. The source's
    /// heads check makes the resting path constant-time; publishing is also a
    /// no-op unless headline, unread count, or latest content id changed.
    fn refresh_room_activity_awareness(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let workspace = {
            let st = chrome.borrow();
            let records = st.records().clone();
            let mut tree = st.tree_mut();
            self.room_activity.refresh(&mut tree);
            self.room_activity.refresh_records(&mut tree, &records);
            tree.active.raw()
        };
        let (headline, unread, pulse) = self.room_activity.glance_signal(workspace);
        let passive_digest = self.room_activity.digest_for_workspace(workspace);
        let focus =
            passive_digest
                .briefs
                .first()
                .map(|brief| crate::shell::chrome_state::ActivityFocus {
                    author: brief.authors.first().map(|author| author.name.clone()),
                    group_id: brief.group_id.clone(),
                    entry_id: None,
                });
        chrome
            .borrow_mut()
            .publish_activity_glance(headline, unread, pulse, focus);
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
    #[allow(
        clippy::unused_self,
        reason = "kept as a method for the self.ws_settings_body_rect(...) call-site sugar \
                  used consistently across every ws-settings tab body's layout and draw pass; \
                  an associated fn would need Self:: at each of those sites for no real gain"
    )]
    fn ws_settings_body_rect(&self, card: Rect) -> Rect {
        Rect::new(
            card.x0 + 16.0,
            card.y0 + 160.0,
            card.x1 - 16.0,
            card.y1 - 16.0,
        )
    }

    /// The "Press Esc to close" hint, bottom-right of the modal card.
    /// Each tab that owns its whole body draws it, so the General body
    /// never has to run just to produce the hint.
    #[allow(
        clippy::unused_self,
        reason = "kept as a method for the self.draw_ws_settings_close_hint(...) call-site \
                  sugar used consistently across every ws-settings tab's draw pass; an \
                  associated fn would need Self:: at each of those sites for no real gain"
    )]
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
            transform.pre_translate((card.x1 - f64::from(w) - 24.0, card.y1 - 24.0).into()),
            None,
        );
    }

    /// Name-input rect inside the ws-settings modal. Matches the drawn
    /// rect in `draw_ws_settings` so hit-test and draw stay in sync.
    fn ws_settings_name_rect(&self) -> Rect {
        let body = self.ws_settings_body_rect(self.ws_settings_rect());
        Rect::new(
            body.x0 + 16.0,
            body.y0 + 62.0,
            body.x1 - 16.0,
            body.y0 + 96.0,
        )
    }

    /// Visible pointer-accessible close control in the identity card.
    fn ws_settings_close_rect(&self) -> Rect {
        let rect = self.ws_settings_rect();
        Rect::new(
            rect.x1 - 58.0,
            rect.y0 + 25.0,
            rect.x1 - 28.0,
            rect.y0 + 55.0,
        )
    }

    /// A contextual lens docked beside the live workspace. Narrow displays
    /// promote the same surface to a nearly full-width sheet.
    fn ws_settings_rect(&self) -> Rect {
        let margin = 12.0_f64;
        let top = self.rect.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX) + margin;
        let bottom = (self.rect.y1 - margin).max(top + 320.0);
        if self.rect.width() < 820.0 {
            return Rect::new(self.rect.x0 + margin, top, self.rect.x1 - margin, bottom);
        }
        let lens_w = (self.rect.width() * 0.46).clamp(620.0, 780.0);
        let right = self.rect.x1 - margin;
        Rect::new(right - lens_w, top, right, bottom)
    }

    /// The band the scope + kind controls paint into — the palette's own
    /// reserved filter band, not a floating surface, which keeps the
    /// controls aligned with the query field and rows below.
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

    /// Label for the i-th scope pill. [`SearchScope::ActiveApp`] names
    /// the focused app (`Notes app`), truncated to `MAX_APP_PILL_CHARS`
    /// — "app" is explicit because the kind-filter strip above uses the
    /// same names for a different axis.
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

    /// Individual scope-segment rect. `idx` indexes into
    /// [`Self::search_scope_list`] (`ActiveApp` is 0 when present). One
    /// segmented control, not loose pills, since the scopes are mutually
    /// exclusive. Hit-test and draw share this helper.
    fn search_scope_pill_rect(&self, idx: usize) -> Rect {
        let b = self.search_banner_rect();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        let count = self.search_scope_list().len().max(1);
        let track = self.search_scope_track_rect();
        let seg_w = (track.width() - 2.0 * SEGMENT_INSET_PX) / usize_to_f64_lossy(count);
        let x0 = track.x0 + SEGMENT_INSET_PX + usize_to_f64_lossy(idx) * seg_w;
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
        let w = (Self::SCOPE_SEGMENT_WIDTH_PX * usize_to_f64_lossy(count) + 2.0 * SEGMENT_INSET_PX)
            .min(b.width() * 0.55);
        let h = Self::FILTER_CONTROL_HEIGHT_PX;
        // Anchored to the *first* row of the band, not to the middle of
        // it: when the kind chips wrap to a second row the switch must
        // stay where it was, beside the row it shares, rather than
        // drifting down into the gap between the two rows of chips.
        let cy = b.y0 + self.palette.filter_strip_row_height() / 2.0;
        Rect::new(b.x1 - w, cy - h / 2.0, b.x1, cy + h / 2.0)
    }

    /// Width of one scope segment. Wide enough to hold a truncated app
    /// title without cramping.
    const SCOPE_SEGMENT_WIDTH_PX: f64 = 88.0;
    /// Shared height for every control in the filter band, so the chips
    /// and the scope switch sit on one baseline.
    const FILTER_CONTROL_HEIGHT_PX: f64 = 24.0;

    /// The overview's model of every room, in tree order.
    ///
    /// Geometry and paint both read it, so the panel can never be sized
    /// for one set of rooms and painted with another.
    fn overview_cards(&self) -> Vec<crate::shell::overview::RoomCard> {
        match self.chrome.as_ref() {
            Some(c) => crate::shell::overview::room_cards(&c.borrow().tree()),
            None => Vec::new(),
        }
    }

    /// Index of the room this session is in, if the tree named one.
    fn overview_here_index(&self) -> Option<usize> {
        self.overview_cards().iter().position(|c| c.is_current)
    }

    /// How many share rows the "shared out of here" list draws — at least
    /// one, so a room sharing nothing says so on its own line rather than
    /// collapsing the section away.
    fn overview_share_rows(card: &crate::shell::overview::RoomCard) -> usize {
        let shown = card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
        let overflow = usize::from(card.shares.len() > shown);
        shown.saturating_add(overflow).max(1)
    }

    /// Height of the "you are here" block, including its share list.
    fn overview_here_block_height(&self) -> f64 {
        let cards = self.overview_cards();
        let rows = match self.overview_here_index().and_then(|i| cards.get(i)) {
            Some(card) => Self::overview_share_rows(card),
            None => 1,
        };
        OVERVIEW_HERE_HEIGHT_PX
            + OVERVIEW_CAPTION_HEIGHT_PX
            + usize_to_f64_lossy(rows) * OVERVIEW_SHARE_ROW_HEIGHT_PX
    }

    /// How many rows the "go to" list holds: every room but the one you
    /// are in, plus the trailing "New room" row.
    fn overview_list_slots(&self) -> usize {
        let n = self.overview_cards().len();
        n.saturating_sub(usize::from(self.overview_here_index().is_some()))
            .saturating_add(1)
    }

    /// Height the list would take if nothing clipped it.
    fn overview_content_height(&self) -> f64 {
        let k = usize_to_f64_lossy(self.overview_list_slots());
        (k * (OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX) - OVERVIEW_ROOM_ROW_GAP_PX)
            .max(0.0)
    }

    /// The panel: a floating surface centred in the viewport, sized to what
    /// it has to say and capped so it never runs off screen. Same width and
    /// centring as the workspace-settings modal — the same kind of object.
    fn overview_panel_rect(&self) -> Rect {
        let w = OVERVIEW_PANEL_WIDTH_PX.min(self.rect.width() - OVERVIEW_PANEL_MARGIN_PX * 2.0);
        let natural = OVERVIEW_PANEL_PAD_PX * 2.0
            + self.overview_here_block_height()
            + OVERVIEW_SECTION_GAP_PX
            + OVERVIEW_CAPTION_HEIGHT_PX
            + self.overview_content_height();
        let max_h = self.rect.height() - OVERVIEW_PANEL_MARGIN_PX * 2.0;
        let h = natural.min(max_h).max(0.0);
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// The band inside the panel that content is laid out in.
    fn overview_content_rect(&self) -> Rect {
        self.overview_panel_rect().inset(-OVERVIEW_PANEL_PAD_PX)
    }

    /// The "you are here" block — the current room's identity, access
    /// and people, with what it shares beneath.
    fn overview_here_rect(&self) -> Rect {
        let c = self.overview_content_rect();
        Rect::new(c.x0, c.y0, c.x1, c.y0 + OVERVIEW_HERE_HEIGHT_PX)
    }

    /// Y of the "Shared out of here" caption.
    fn overview_shares_top(&self) -> f64 {
        self.overview_here_rect().y1
    }

    /// Y of the "Go to" caption above the room list.
    fn overview_list_caption_top(&self) -> f64 {
        self.overview_content_rect().y0
            + self.overview_here_block_height()
            + OVERVIEW_SECTION_GAP_PX
    }

    /// Y the first room row starts at, before scrolling.
    fn overview_list_top(&self) -> f64 {
        self.overview_list_caption_top() + OVERVIEW_CAPTION_HEIGHT_PX
    }

    /// Height of the band the room list scrolls inside.
    fn overview_viewport_height(&self) -> f64 {
        (self.overview_content_rect().y1 - self.overview_list_top()).max(0.0)
    }

    /// Maximum legal `overview_scroll_y` — the list's overflow below the
    /// visible band. Zero when every room fits.
    fn overview_max_scroll_y(&self) -> f64 {
        (self.overview_content_height() - self.overview_viewport_height()).max(0.0)
    }

    /// Rect of list slot `slot`, panned by `overview_scroll_y`.
    fn overview_list_slot_rect(&self, slot: usize) -> Rect {
        let c = self.overview_content_rect();
        let y0 = self.overview_list_top() - self.overview_scroll_y
            + usize_to_f64_lossy(slot) * (OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX);
        Rect::new(c.x0, y0, c.x1, y0 + OVERVIEW_ROOM_ROW_HEIGHT_PX)
    }

    /// The list slot a room occupies: the room you are in has none — it
    /// is the block at the top — and every other room keeps tree order.
    fn overview_slot_of(&self, idx: usize) -> Option<usize> {
        match self.overview_here_index() {
            Some(here) if here == idx => None,
            Some(here) if idx > here => Some(idx.saturating_sub(1)),
            _ => Some(idx),
        }
    }

    /// Rect of workspace `idx` — the "you are here" block for the room
    /// you are in, its list row for any other. Indices are tree order,
    /// the contract the test-control socket's `overview_card[i]` names
    /// are built on.
    fn overview_row_rect(&self, idx: usize) -> Rect {
        match self.overview_slot_of(idx) {
            None => self.overview_here_rect(),
            Some(slot) => self.overview_list_slot_rect(slot),
        }
    }

    /// Rect of the "New room" row — the slot after the last room.
    fn overview_add_workspace_rect_for(&self, ws_count: usize) -> Rect {
        let slot = ws_count.saturating_sub(usize::from(self.overview_here_index().is_some()));
        self.overview_list_slot_rect(slot)
    }

    /// Pan the room list so the keyboard's selection is on screen —
    /// otherwise arrowing past the last visible room moves a selection
    /// nobody can see.
    fn scroll_overview_selection_into_view(&mut self) {
        let Some(idx) = self.overview_selection else {
            return;
        };
        let Some(slot) = self.overview_slot_of(idx) else {
            // The room you are in is the pinned block, never in the list.
            return;
        };
        let step = OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX;
        let top = usize_to_f64_lossy(slot) * step;
        let bottom = top + OVERVIEW_ROOM_ROW_HEIGHT_PX;
        let view = self.overview_viewport_height();
        let max = self.overview_max_scroll_y();
        let mut y = self.overview_scroll_y;
        if top < y {
            y = top;
        } else if bottom > y + view {
            y = bottom - view;
        }
        self.overview_scroll_y = y.clamp(0.0, max);
    }

    /// Mirror of the geometry in `draw_overview`: return the index of the
    /// room at logical position `p`, if any. A row panned out of the
    /// list's band is not hittable, so a click never lands on a room the
    /// user cannot see.
    fn hit_test_overview(&self, p: Point) -> Option<usize> {
        let chrome = self.chrome.as_ref()?;
        let count = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.workspaces.len()
        };
        let band = Rect::new(
            self.overview_content_rect().x0,
            self.overview_list_top(),
            self.overview_content_rect().x1,
            self.overview_content_rect().y1,
        );
        for i in 0..count {
            let r = self.overview_row_rect(i);
            let visible = self.overview_slot_of(i).is_none()
                || (r.y0 >= band.y0 - 0.5 && r.y1 <= band.y1 + 0.5);
            if visible && r.contains(p) {
                return Some(i);
            }
        }
        None
    }

    /// True if `p` is over any open overlay's hit rect, so
    /// [`Widget::handle_event`] absorbs every pointer event and not just
    /// `Pressed` — otherwise `Released`/`CursorMoved` fall through and
    /// light up affordances underneath.
    pub(crate) fn cursor_over_open_overlay(&self, p: Point) -> bool {
        let f = self.overlay_flags();
        (self.palette_visible && self.palette.get_rect().contains(p))
            || (f.collaborators && self.collaborators_overlay.contains(p))
            || (f.quick_settings && self.quick_settings_rect().contains(p))
            || (f.ws_settings && self.ws_settings_rect().contains(p))
            || (f.room_activity && self.room_activity.rect().contains(p))
            || (f.projection && ProjectionOverlay::contains(self.rect, p))
    }

    /// Route the modal component before application content and apply its
    /// visibility/invalidation requests to global chrome state.
    fn handle_projection_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let open = self.overlay_flags().projection;
        let outcome = self.projection_overlay.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            focus_target,
            self.last_cursor,
            self.rect,
            open,
        );
        if outcome.close_requested
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().overlays.projection = false;
        }
        if outcome.invalidate
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().bump();
        }
        outcome.consumed
    }
}

// `handle_event` was one 1591-line function — the single largest
// `too_many_lines` violation in the shell. It reads as a long chain of
// gates ("if this owns the event, act and stop") followed by a few
// larger dispatch stages (global keyboard chords, per-overlay mouse
// clicks, then content). Each gate/stage below is a helper that mirrors
// its slice of the original inline control flow exactly: a `bool`
// return of `true` means "this fully resolved the event, the caller
// must return immediately" — precisely the points where the original
// hit a bare `return` inside `handle_event` itself. `*handled` is
// mutated only where the original set it, so a helper returning `true`
// without touching `handled` reproduces an original early return that
// left the event formally unhandled.
impl ShellOverlayHost {
    /// Tracks the last cursor position and updates strip ribbon/rail
    /// hover, for hit-testing overlay clicks in the absence of position
    /// data on `MouseInput`. First stage of `handle_event`.
    fn track_cursor_position(&mut self, event: &WindowEvent, scale_factor: f64) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_cursor = Some(p);
            // Paint-only: a segment thickens under the pointer to say it's
            // clickable.
            let hover = if self.overlay_flags().overview {
                None
            } else {
                self.strip_ribbon().hit(p)
            };
            let rail_hover = if self.overlay_flags().overview {
                None
            } else {
                self.strip_rail().hit(p)
            };
            if hover != self.strip_ribbon_hover || rail_hover != self.strip_rail_hover {
                self.strip_ribbon_hover = hover;
                self.strip_rail_hover = rail_hover;
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().bump();
                }
            }
        }
    }

    /// Modal overlays that absorb an event outright before anything else
    /// sees it: the file picker (genuinely modal — a D-Bus call is
    /// parked on the answer), an in-flight peer-sync token prompt, and
    /// the projection sheet.
    #[allow(clippy::too_many_arguments)]
    fn route_modal_absorbing_overlays(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        // Runs before everything and absorbs every event: genuinely
        // modal (a D-Bus method in another process is parked on the
        // answer), and a typed filename shares characters with chords.
        if self.file_picker_overlay.handle_event(
            self.rect,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
        ) {
            return true;
        }

        // Before every other overlay: a token prompt owns the keyboard
        // outright, since a token shares characters with chords.
        if self.peer_sync_overlay.visible() && self.handle_peer_sync_event(event, modifiers) {
            *handled = true;
            return true;
        }

        if self.handle_projection_event(event, scale_factor, modifiers, state, path, focus_target) {
            *handled = true;
            return true;
        }

        false
    }

    /// A brightness/volume slider drag in progress owns pointer input
    /// ahead of the modal absorb gate, so it keeps updating even once
    /// the drag leaves the slider bar or the popover — like a native
    /// slider.
    fn route_qs_slider_drag(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        let Some(drag_i) = self.qs_slider_drag else {
            return false;
        };
        match event {
            WindowEvent::CursorMoved { .. } => {
                if let (Some(p), Some(chrome)) = (self.last_cursor, self.chrome.clone()) {
                    let (bar, _) = self.quick_settings_slider_geometry(drag_i);
                    let t = f64_to_f32_lossy(((p.x - bar.x0) / bar.width()).clamp(0.0, 1.0));
                    let mut st = chrome.borrow_mut();
                    if drag_i == 0 {
                        st.quick_settings.brightness = t;
                    } else {
                        st.quick_settings.volume = t;
                    }
                    st.bump();
                }
                *handled = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let value_pct: f32 = self.chrome.as_ref().map_or(0.0, |c| {
                    let st = c.borrow();
                    if drag_i == 0 {
                        st.quick_settings.brightness
                    } else {
                        st.quick_settings.volume
                    }
                }) * 100.0;
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
                true
            }
            _ => false,
        }
    }

    /// The room-activity sheet, ahead of the general absorb gate — it
    /// wants exactly the events that gate would otherwise swallow
    /// (hover, wheel); the gate still catches whatever the sheet
    /// doesn't claim.
    fn route_room_activity_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        if !(self.overlay_flags().room_activity
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::KeyboardInput { .. }
            ))
        {
            return false;
        }
        let sheet = self.room_activity.rect();
        let pointer_inside = self.last_cursor.is_some_and(|p| sheet.contains(p));
        let is_key = matches!(event, WindowEvent::KeyboardInput { .. });
        // Escape belongs to the dismissal ladder below, except while a
        // name field is up — there it abandons the field instead, or
        // closing the sheet would throw away what was typed.
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
                let records = st.records().clone();
                let mut tree = st.tree_mut();
                self.room_activity.handle_event_with_records(
                    event,
                    scale_factor,
                    &modifiers,
                    cursor,
                    &mut tree,
                    Some(&records),
                )
            };
            if outcome.reverted {
                let mut st = chrome.borrow_mut();
                st.bump();
                if outcome.undo_partial {
                    st.push_notification(
                        "Partially undid the selected work",
                        "Later collaborator edits were preserved. History shows which parts were left intact.",
                    );
                } else {
                    st.push_notification(
                        "Reverted a room change",
                        "Written forward: the version you reverted from is still in the history.",
                    );
                }
            }
            if let Some(target) = outcome.open_target {
                let app_id = target.app_id.as_deref();
                let workspace = chrome.borrow().tree().active_workspace().cloned();
                if let (Some(app_id), Some(workspace)) = (app_id, workspace) {
                    let result = match history_target_address(&target) {
                        HistoryTargetAddress::File(path) => {
                            crate::shell::launcher::launch_document_in_workspace(
                                app_id,
                                &workspace,
                                Some(&path),
                            )
                        }
                        HistoryTargetAddress::Uri(uri) => {
                            crate::shell::launcher::launch_uri_in_workspace(
                                app_id, &workspace, &uri,
                            )
                        }
                        HistoryTargetAddress::App => {
                            crate::shell::launcher::launch_in_workspace(app_id, &workspace)
                        }
                    };
                    if let Err(error) = result {
                        chrome.borrow_mut().push_notification(
                            format!("Couldn't open {}", target.label),
                            error.to_string(),
                        );
                    }
                }
            }
            if outcome.compared {
                chrome.borrow_mut().push_notification(
                    "Comparison ready",
                    "The exact causal before/after state is shown in History.",
                );
            }
            if outcome.named {
                let mut st = chrome.borrow_mut();
                st.bump();
                st.push_notification(
                    "Named a version",
                    "Saved in this room's history, so everyone here sees it.",
                );
            }
            // Pointer events over the sheet absorb regardless; keys
            // only when consumed, or an arrow key with the cursor
            // resting over the sheet would vanish.
            if outcome.handled || (pointer_inside && !is_key) {
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Absorbs every non-`Pressed` pointer event over an open overlay so
    /// a hover/drag can't continue underneath it (`Pressed` is excluded
    /// so the per-overlay branches later still get clicks), and a
    /// left-click on a toast dismisses it before the permission prompt
    /// or any other overlay sees the press.
    fn route_absorbable_pointer_and_toast_click(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
    ) -> bool {
        let is_absorbable_pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
        );
        if is_absorbable_pointer
            && let Some(p) = self.last_cursor
            && self.cursor_over_open_overlay(p)
        {
            *handled = true;
            return true;
        }

        if let (
            Some(stack),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            },
        ) = (self.notification_stack.as_mut(), event)
            && let Some(p) = self.last_cursor
            && stack.notify_click_at(p)
        {
            *handled = true;
            return true;
        }
        false
    }

    /// `pump_permission_prompt` drains `pending_choice` next tick.
    /// Gated on the prompt's rect so its hit zone doesn't swallow
    /// clicks outside the bar.
    #[allow(clippy::too_many_arguments)]
    fn route_permission_prompt_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let Some(prompt) = self.permission_prompt.as_mut() else {
            return false;
        };
        let prompt_rect = prompt.get_rect();
        let forward = match event {
            WindowEvent::CursorMoved { .. } => true,
            WindowEvent::MouseInput { .. } => {
                self.last_cursor.is_some_and(|p| prompt_rect.contains(p))
            }
            _ => false,
        };
        if !forward {
            return false;
        }
        let mut tmp_taffy = TaffyTree::<()>::new();
        let Ok(tmp_node) = tmp_taffy.new_leaf(Style::default()) else {
            return false;
        };
        prompt.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            handled,
            focus_target,
            tmp_node,
            &tmp_taffy,
        );
        *handled
    }

    /// `DroppedFile` carries no coordinate, so route by `last_cursor`
    /// through the same hit-test click-focus uses. A drop over empty
    /// space is a no-op, not dumped into an arbitrary window.
    fn route_dropped_file_event(&self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::DroppedFile(path) = event else {
            return false;
        };
        if let Some(chrome) = self.chrome.clone()
            && let Some(p) = self.last_cursor
            && let Some((strip_id, group_id)) = self.resolve_drop_target(p)
        {
            // Mirrors `stamp_active_group_from_click`: fold a
            // widget-only group into the tree before splicing.
            self.harvest_widget_only_groups_into_tree();
            // A file an app declares opens in that app with
            // its read grant; a folder or unclaimed type
            // takes the placeholder-tab drop below.
            let owned = matches!(
                crate::shell::doc_open::resolve(path),
                crate::shell::doc_open::DocumentTarget::App { .. }
            );
            if owned {
                if let Err(e) = crate::shell::doc_open::open_in_group(path, &chrome, group_id) {
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
        *handled = true;
        true
    }

    /// Global shortcuts: `Ctrl+K` opens search scoped to the focused
    /// tab; `Super+K`/`Super+Space`/`Alt+Space` open in global scope
    /// (`Alt+Space` is the nested-Wayland-client substitute, since a
    /// host DE running qOS as a client swallows Super first).
    /// `Ctrl+Tab`/`Super+Tab` toggle the overview; Esc closes the top
    /// overlay. Only called for a `Pressed` `KeyboardInput`.
    fn handle_shell_keyboard_event(
        &mut self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        let mstate = modifiers.state();
        let super_down = mstate.super_key();
        let alt_down = mstate.alt_key();
        let is_mod = super_down || mstate.control_key();
        let shift_down = mstate.shift_key();
        // Ahead of every chord below: a password can contain any
        // character, and a `k` in one must not open search instead.
        if self.handle_wifi_password_key(logical_key, modifiers) {
            *handled = true;
            return true;
        }
        // Captured ahead of the modifier-only branch so Character('K')
        // further down can't also fire.
        let is_space = matches!(logical_key, Key::Named(NamedKey::Space));
        if is_space && (super_down || alt_down) {
            self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
            self.refresh_search_results(state, path);
            *handled = true;
            return true;
        }
        if self.handle_overview_keyboard_nav(logical_key, handled) {
            return true;
        }
        if is_mod
            && self.handle_shell_mod_chords(
                logical_key,
                super_down,
                shift_down,
                state,
                path,
                handled,
            )
        {
            return true;
        }
        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && self.handle_shell_escape_chord(handled)
        {
            return true;
        }
        self.handle_workspace_rename_key(logical_key, modifiers, handled)
    }

    /// Arrow/Enter navigation of the room overview list, unmodified so
    /// it runs ahead of the modifier chords and of the content — the
    /// desktop must not scroll under an open panel. Returns `false`
    /// (falling through to the modifier chords) when the overview isn't
    /// showing, the palette is up, a different key was pressed, or
    /// there are no rooms to navigate.
    fn handle_overview_keyboard_nav(&mut self, logical_key: &Key, handled: &mut bool) -> bool {
        if !(self.overlay_flags().overview
            && !self.palette_visible
            && matches!(
                logical_key,
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Enter)
            ))
        {
            return false;
        }
        let count = self
            .chrome
            .as_ref()
            .map_or(0, |c| c.borrow().tree().workspaces.len());
        if count == 0 {
            return false;
        }
        if matches!(logical_key, Key::Named(NamedKey::Enter)) {
            // Commits: go there and leave, no mouse required.
            let target = self
                .overview_selection
                .or_else(|| self.overview_here_index());
            if let (Some(idx), Some(chrome)) = (target, self.chrome.clone()) {
                let mut st = chrome.borrow_mut();
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.workspaces.get(idx) {
                        let id = ws.id;
                        tree.set_active(id);
                    }
                }
                st.overlays.overview = false;
                st.bump();
            }
        } else {
            let step: isize = if matches!(logical_key, Key::Named(NamedKey::ArrowDown)) {
                1
            } else {
                -1
            };
            let cur = usize_to_isize_lossy(
                self.overview_selection
                    .or_else(|| self.overview_here_index())
                    .unwrap_or(0),
            );
            // A ring: the room after the last one is the first.
            // `count > 0` is checked above, so the divisor is
            // never zero; `wrapping_add` means an (unreachable
            // in practice) isize overflow still lands on some
            // in-range selection instead of panicking.
            let next = isize_to_usize_lossy(
                cur.wrapping_add(step)
                    .rem_euclid(usize_to_isize_lossy(count)),
            );
            self.overview_selection = Some(next);
            self.scroll_overview_selection_into_view();
        }
        *handled = true;
        true
    }

    /// The `is_mod` (Ctrl or Super held) chord set: search, the
    /// projection and overview toggles, window-creation chords, and
    /// focus/workspace stepping.
    fn handle_shell_mod_chords(
        &mut self,
        logical_key: &Key,
        super_down: bool,
        shift_down: bool,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if let Key::Character(s) = logical_key
            && s.as_str().eq_ignore_ascii_case("k")
        {
            use crate::shell::chrome_state::SearchScope;
            // Ctrl+K searches the focused app; Super+K searches
            // the whole machine, including apps not open.
            // `ActiveApp` widens to every provider (or `All`)
            // when nothing claims/has the focused tab.
            let scope = if super_down {
                SearchScope::All
            } else {
                SearchScope::ActiveApp
            };
            self.open_palette_with_scope(scope);
            // So the palette lands populated (chrome actions
            // for an empty query) instead of blank.
            self.refresh_search_results(state, path);
            *handled = true;
            return true;
        }
        // Win+P — wireless display picker. Super specifically, so an
        // application's Ctrl+P remains Print. Handled before the
        // general Super-letter switch below.
        if super_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("p")
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            st.overlays.projection = !st.overlays.projection;
            st.bump();
            *handled = true;
            return true;
        }
        if matches!(logical_key, Key::Named(NamedKey::Tab))
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            st.overlays.overview = !st.overlays.overview;
            st.bump();
            *handled = true;
            return true;
        }
        // Window-creation chords are Super-only (grabbed wholesale,
        // taking nothing from a focused client), not the
        // Ctrl+Shift+letter chords browsers/terminals define.
        // Super+Return is the recovery path when the last shell is
        // gone, so it must not depend on search.
        if super_down
            && matches!(logical_key, Key::Named(NamedKey::Enter))
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            crate::shell::window_actions::open_terminal(
                &mut st,
                crate::shell::window_actions::Destination::Beside,
            );
            *handled = true;
            return true;
        }
        if super_down
            && let Key::Character(s) = logical_key
            && self.handle_shell_window_open_chords(s.as_str(), super_down, state, path, handled)
        {
            return true;
        }
        self.handle_shell_focus_and_strip_chords(logical_key, super_down, shift_down, handled)
    }

    /// Focus/workspace stepping: Super+Left/Right along the strip,
    /// Shift+Up/Down between strips, and unmodified-within-`is_mod`
    /// Up/Down between workspaces. Split out of `handle_shell_mod_chords`.
    fn handle_shell_focus_and_strip_chords(
        &mut self,
        logical_key: &Key,
        super_down: bool,
        shift_down: bool,
        handled: &mut bool,
    ) -> bool {
        // Super+ArrowLeft/Right moves focus one window along the
        // strip — Super only, not the `is_mod` alias the vertical
        // pair uses: Ctrl+Left/Right is word-wise cursor motion in
        // nearly every text field.
        if super_down
            && matches!(
                logical_key,
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
            )
        {
            let dir = if matches!(logical_key, Key::Named(NamedKey::ArrowLeft)) {
                -1
            } else {
                1
            };
            if self.step_focus_along_strip(dir) {
                *handled = true;
            }
            return true;
        }
        // Shift+ArrowUp/Down move focus between strips —
        // workspaces already own the unshifted pair.
        if shift_down
            && matches!(
                logical_key,
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown)
            )
            && let Some(chrome) = self.chrome.clone()
        {
            let dir = if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                -1
            } else {
                1
            };
            let mut st = chrome.borrow_mut();
            if st.switch_strip(dir) {
                *handled = true;
            }
            return true;
        }
        // Cmd+ArrowUp / Cmd+ArrowDown — switch active workspace.
        if matches!(logical_key, Key::Named(NamedKey::ArrowUp))
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            if st.switch_workspace(-1) {
                *handled = true;
            }
            return true;
        }
        if matches!(logical_key, Key::Named(NamedKey::ArrowDown))
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            if st.switch_workspace(1) {
                *handled = true;
            }
            return true;
        }
        false
    }

    /// Super+letter chords that open, close, or duplicate a window: `T`
    /// (chord form of the tab strip's "+"), `N` (new window), `W`
    /// (close focused tab), `D` (duplicate), `S` (summon the workspace
    /// wheel to the pointer). Split out of `handle_shell_mod_chords`,
    /// which was otherwise the single largest chord block.
    fn handle_shell_window_open_chords(
        &mut self,
        s: &str,
        super_down: bool,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        match s {
            // Chord form of the tab strip's "+": same
            // question, same picker.
            "t" | "T" => {
                let group = self
                    .chrome
                    .as_ref()
                    .and_then(|c| crate::shell::window_actions::focused_group(&c.borrow()));
                if let (Some(chrome), Some(group)) = (self.chrome.clone(), group) {
                    let mut st = chrome.borrow_mut();
                    st.open_target = crate::shell::chrome_state::OpenTarget::Here(group);
                    st.search_scope = crate::shell::chrome_state::SearchScope::All;
                    st.bump();
                    drop(st);
                    self.active_kind_filter = app_kind_filter();
                    self.palette_visible = true;
                    self.palette.set_focus(true);
                    self.palette.set_query("");
                    self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
                    self.refresh_search_results(state, path);
                    *handled = true;
                    return true;
                }
                false
            }
            // Super+N — a new window, placed by the rule.
            "n" | "N" => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    st.open_target = crate::shell::chrome_state::OpenTarget::Beside;
                    st.search_scope = crate::shell::chrome_state::SearchScope::All;
                    st.bump();
                }
                self.palette_visible = true;
                self.palette.set_focus(true);
                self.palette.set_query("");
                self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
                self.refresh_search_results(state, path);
                *handled = true;
                true
            }
            // Chord form of the pill's `✕`. Super, not
            // Ctrl+W — the latter is already bound in
            // most browsers/terminals/editors, and the
            // shell would swallow it ahead of the client.
            "w" | "W" => {
                if self.close_focused_tab() {
                    *handled = true;
                    true
                } else {
                    false
                }
            }
            // Super+D — a second instance of what you are
            // already looking at.
            "d" | "D" => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    if let Err(err) = crate::shell::window_actions::duplicate_active_tab(&mut st) {
                        st.push_warning_notification(
                            "Can't duplicate this tab",
                            err.user_message(),
                        );
                    }
                    *handled = true;
                    true
                } else {
                    false
                }
            }
            // Calls the workspace wheel to the pointer —
            // it's docked at the strip's left edge and
            // pans with it, so it can be off screen from
            // the right end. Super specifically, not
            // `is_mod`: Ctrl+S is save in nearly every app.
            "s" | "S" if super_down => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    let next = match st.wheel_summon() {
                        Some(_) => None,
                        None => self.last_cursor,
                    };
                    st.set_wheel_summon(next);
                    *handled = true;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// The Escape dismissal ladder. Priority matches z-order: palette >
    /// overview > ws settings > quick settings.
    fn handle_shell_escape_chord(&mut self, handled: &mut bool) -> bool {
        if self.palette_visible {
            self.palette_visible = false;
            *handled = true;
            return true;
        }
        if self.power_menu.close() {
            *handled = true;
            return true;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut st = chrome.borrow_mut();
        // Most transient thing on screen, so goes first.
        if st.wheel_summon().is_some() {
            st.set_wheel_summon(None);
            *handled = true;
            return true;
        }
        if st.editing_workspace_name.is_some() {
            st.editing_workspace_name = None;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.overview {
            st.overlays.overview = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.ws_settings {
            st.overlays.ws_settings = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.room_activity {
            st.overlays.room_activity = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.projection {
            st.overlays.projection = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.collaborators {
            st.overlays.collaborators = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.quick_settings {
            st.overlays.quick_settings = false;
            st.bump();
            *handled = true;
            return true;
        }
        false
    }

    /// The workspace-rename text field's own key handling: a buffer in
    /// flight consumes character/backspace/enter/navigation/clipboard
    /// keys before anything else gets a chance.
    fn handle_workspace_rename_key(
        &self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let is_editing = chrome.borrow().editing_workspace_name.is_some();
        if !is_editing {
            return false;
        }
        let mods_state = modifiers.state();
        let shift = mods_state.shift_key();
        let ctrl = mods_state.control_key() || mods_state.super_key();
        let mut st = chrome.borrow_mut();
        // Pulled out by value first so it isn't a separate
        // immutable borrow alongside `buf`.
        let sel = st.editing_workspace_name_selection;
        let Some(buf) = st.editing_workspace_name.as_mut() else {
            return true;
        };
        if matches!(logical_key, Key::Named(NamedKey::Enter)) {
            drop(st);
            chrome.borrow_mut().commit_workspace_name();
            *handled = true;
            return true;
        }
        let (consumed, needs_bump, sel) =
            Self::apply_workspace_rename_key(logical_key, ctrl, shift, buf, sel);
        if consumed {
            st.editing_workspace_name_selection = sel;
            if needs_bump {
                st.bump();
            }
            *handled = true;
            return true;
        }
        false
    }

    /// Every workspace-rename chord but `Enter`, applied to `buf`/`sel` in
    /// place and returned as `(consumed, needs_bump, selection)`. `Enter`
    /// is special-cased by the caller, which needs to drop the borrow this
    /// only holds a `&mut` into. Split out of
    /// [`Self::handle_workspace_rename_key`] to keep it under
    /// `too_many_lines`.
    fn apply_workspace_rename_key(
        logical_key: &Key,
        ctrl: bool,
        shift: bool,
        buf: &mut String,
        mut sel: ui::text_selection::Selection,
    ) -> (bool, bool, ui::text_selection::Selection) {
        use ui::text_selection::{
            Selection, next_char_boundary, next_word_boundary, prev_char_boundary,
            prev_word_boundary,
        };
        let mut consumed = true;
        let mut needs_bump = true;
        match logical_key {
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
                sel = Selection::collapsed(r.start.saturating_add(1));
            }
            Key::Character(s) if ctrl => {
                let (c, n, new_sel) = Self::apply_workspace_rename_ctrl_chord(s.as_str(), buf, sel);
                consumed = c;
                needs_bump = n;
                sel = new_sel;
            }
            Key::Character(s) => {
                let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
                if filtered.is_empty() {
                    consumed = false;
                    needs_bump = false;
                } else {
                    let r = sel.range();
                    buf.replace_range(r.clone(), &filtered);
                    sel = Selection::collapsed(r.start.saturating_add(filtered.len()));
                }
            }
            _ => {
                consumed = false;
                needs_bump = false;
            }
        }
        (consumed, needs_bump, sel)
    }

    /// The `Ctrl`-chord half of [`Self::apply_workspace_rename_key`]:
    /// select-all, copy, cut, paste. Split out to keep the caller under
    /// `too_many_lines`.
    fn apply_workspace_rename_ctrl_chord(
        s: &str,
        buf: &mut String,
        mut sel: ui::text_selection::Selection,
    ) -> (bool, bool, ui::text_selection::Selection) {
        use ui::text_selection::{Selection, clipboard_copy, clipboard_paste};
        let mut consumed = true;
        let mut needs_bump = true;
        match s {
            "a" | "A" => {
                sel.anchor = 0;
                sel.cursor = buf.len();
                needs_bump = false;
            }
            "c" | "C" => {
                let r = sel.range();
                if let Some(selected) = buf.get(r) {
                    clipboard_copy(selected);
                }
                needs_bump = false;
            }
            "x" | "X" => {
                let r = sel.range();
                if r.is_empty() {
                    needs_bump = false;
                } else if let Some(selected) = buf.get(r.clone()) {
                    clipboard_copy(selected);
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
                    sel = Selection::collapsed(r.start.saturating_add(paste.len()));
                } else {
                    needs_bump = false;
                }
            }
            _ => {
                consumed = false;
                needs_bump = false;
            }
        }
        (consumed, needs_bump, sel)
    }

    /// Outside-click close for a popover/modal, swallowing the click so
    /// content beneath doesn't react: strip ribbon/rail focus, the
    /// palette's scope pills or outside-dismiss, the power menu, then
    /// (each absorbing every click once open) room activity,
    /// collaborators, quick settings and workspace settings. Overview
    /// has its own dismissal path, so it is skipped here. Was over 700
    /// lines of one `handle_event`; the flag-gated per-overlay handlers
    /// always end in `*handled = true`, so once inside one this always
    /// returns.
    fn handle_shell_mouse_click_event(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(p) = self.last_cursor else {
            return false;
        };
        if !self.cursor_over_open_overlay(p) && self.follow_stop_rect.contains(p) {
            self.followed_peer = None;
            self.follow_chip_rect = Rect::ZERO;
            self.follow_stop_rect = Rect::ZERO;
            *handled = true;
            return true;
        }
        // Following is a viewing mode, not a lock. The first deliberate
        // click back into the workspace hands control to the local user and
        // continues routing that very click to its intended target.
        if self.followed_peer.is_some()
            && !self.cursor_over_open_overlay(p)
            && !self.follow_chip_rect.contains(p)
        {
            self.followed_peer = None;
            self.follow_chip_rect = Rect::ZERO;
            self.follow_stop_rect = Rect::ZERO;
        }
        // Gated on no overlay owning the pointer, so a click
        // under an open panel goes to the panel.
        if !self.overlay_flags().overview
            && !self.cursor_over_open_overlay(p)
            && let Some(i) = self.strip_ribbon().hit(p)
            && let Some(chrome) = self.chrome.clone()
        {
            chrome.borrow_mut().focus_strip_index(i);
            *handled = true;
            return true;
        }
        // The rail addresses windows off screen, so this is the
        // one pointer path that also pans the strip.
        if !self.overlay_flags().overview
            && !self.cursor_over_open_overlay(p)
            && let Some(i) = self.strip_rail().hit(p)
            && let Some((strip, group)) = self.strip_rail_target(i)
        {
            self.focus_group_on_strip(strip, group);
            *handled = true;
            return true;
        }
        if self.palette_visible {
            // Before outside-click so scope can be swapped
            // without dismissing the palette.
            let scopes = self.search_scope_list();
            for (idx, scope) in scopes.iter().enumerate() {
                if self.search_scope_pill_rect(idx).contains(p) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.search_scope = *scope;
                        st.bump();
                    }
                    *handled = true;
                    return true;
                }
            }
            let palette_rect = self.palette.get_rect();
            if !palette_rect.contains(p) {
                self.palette_visible = false;
                *handled = true;
                return true;
            }
        }
        let flags = self.overlay_flags();
        match self.power_menu.click(self.rect, p) {
            PowerMenuClick::Ignored => {}
            PowerMenuClick::Consumed => {
                *handled = true;
                return true;
            }
            PowerMenuClick::Execute(action) => {
                std::thread::spawn(move || {
                    os_actions::log_result(
                        "session_action",
                        os_actions::perform_session_action(action),
                    );
                });
                *handled = true;
                return true;
            }
        }
        if flags.room_activity && !self.room_activity.rect().contains(p) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.overlays.room_activity = false;
                st.bump();
            }
            *handled = true;
            return true;
        }
        if flags.collaborators {
            self.handle_shell_collaborators_click(p, handled);
            return true;
        }
        if flags.quick_settings {
            self.handle_shell_quick_settings_click(p, handled);
            return true;
        }
        if flags.ws_settings {
            if self.ws_settings_rect().contains(p) {
                self.handle_shell_ws_settings_click(p, handled);
                return true;
            }
            if let Some(chrome) = self.chrome.clone() {
                // Commit an in-progress field before the desktop takes focus,
                // but keep the contextual lens open beside the workspace.
                chrome.borrow_mut().commit_workspace_name();
            }
        }
        false
    }

    /// Clicks inside the open collaborators dropdown: invite, join,
    /// jump to room activity, or (falling through the specific rows)
    /// swallow the click so content beneath doesn't grab it. Reached
    /// only once `flags.collaborators` is true, at which point every
    /// path absorbs the click.
    fn handle_shell_collaborators_click(&mut self, p: Point, handled: &mut bool) {
        let hit = self.collaborators_overlay.hit_test(p);
        if let CollaboratorsHit::ToggleFollow(index) = hit {
            let target = self.chrome.as_ref().and_then(|chrome| {
                let state = chrome.borrow();
                collaborator_roster_rows(&state.tree())
                    .get(index)
                    .filter(|row| row.followable)
                    .cloned()
            });
            if let Some(target) = target {
                if self.followed_peer.as_deref() == Some(target.id.as_str()) {
                    self.followed_peer = None;
                } else {
                    self.followed_peer = Some(target.id);
                    self.reconcile_followed_peer();
                }
            }
            *handled = true;
            return;
        }
        if let Some(chrome) = self.chrome.clone() {
            let mut state = chrome.borrow_mut();
            match hit {
                CollaboratorsHit::Invite => {
                    state.push_peer_sync_request(PeerSyncAction::PromptInvite);
                    state.overlays.collaborators = false;
                    state.bump();
                }
                CollaboratorsHit::Join => {
                    state.push_peer_sync_request(PeerSyncAction::PromptConnect);
                    state.overlays.collaborators = false;
                    state.bump();
                }
                CollaboratorsHit::RoomActivity => {
                    state.overlays.room_activity = true;
                    state.overlays.collaborators = false;
                    state.bump();
                }
                CollaboratorsHit::Dismiss => {
                    state.overlays.collaborators = false;
                    state.bump();
                }
                CollaboratorsHit::ToggleFollow(_) => {}
                CollaboratorsHit::Absorb => {}
            }
        }
        *handled = true;
    }

    /// Clicks inside the open quick-settings popover: dismiss outside
    /// the panel, or route to the drill-in sub-page vs. the root page.
    /// Reached only once `flags.quick_settings` is true, at which point
    /// every path absorbs the click.
    fn handle_shell_quick_settings_click(&mut self, p: Point, handled: &mut bool) {
        let qs_rect = self.quick_settings_rect();
        if !qs_rect.contains(p) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.overlays.quick_settings = false;
                // Next open starts at root, not the last drill-in.
                st.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
                st.bump();
            }
            *handled = true;
            return;
        }
        let current_level = self
            .chrome
            .as_ref()
            .map_or(crate::shell::quick_settings::QsLevel::Root, |c| {
                c.borrow().quick_settings.current_level().clone()
            });
        if matches!(current_level, crate::shell::quick_settings::QsLevel::Root) {
            self.handle_shell_quick_settings_root_click(p);
        } else {
            self.handle_shell_quick_settings_drill_in_click(p, &current_level);
        }
        *handled = true;
    }

    /// Clicks on a quick-settings drill-in sub-page (Wi-Fi, VPN,
    /// Bluetooth, notifications): back, the passphrase prompt, or a
    /// row. Split out of `handle_shell_quick_settings_click`, which
    /// always treats the click as absorbed regardless of what this
    /// does.
    fn handle_shell_quick_settings_drill_in_click(
        &mut self,
        p: Point,
        current_level: &crate::shell::quick_settings::QsLevel,
    ) {
        let back = self.qs_back_chevron_rect();
        // A passphrase prompt owns the pane while up, so
        // its controls (including back) are tested first.
        let prompt_up = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().quick_settings.wifi_password.is_some());
        if prompt_up {
            let (cancel, connect) = self.qs_password_button_rects();
            if back.contains(p) || cancel.contains(p) {
                self.cancel_wifi_password();
                return;
            }
            if connect.contains(p) {
                self.submit_wifi_password();
            }
            // The field is always focused; nothing else
            // on the pane can be focused by a click.
            return;
        }
        if back.contains(p)
            && !matches!(
                current_level,
                crate::shell::quick_settings::QsLevel::Wifi
                    | crate::shell::quick_settings::QsLevel::Bluetooth
                    | crate::shell::quick_settings::QsLevel::Vpn
            )
        {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.quick_settings.pop_level();
                st.bump();
            }
            return;
        }
        if matches!(current_level, crate::shell::quick_settings::QsLevel::Wifi) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                let outcome = self.qs_wifi_pane.handle_click(
                    p,
                    self.quick_settings_rect(),
                    &mut st.quick_settings,
                    self.qs_actions.as_ref(),
                );
                match outcome {
                    DrillInOutcome::Back => st.quick_settings.pop_level(),
                    DrillInOutcome::Changed => {}
                    DrillInOutcome::None => return,
                }
                st.bump();
            }
        } else if matches!(
            current_level,
            crate::shell::quick_settings::QsLevel::Bluetooth
        ) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                let outcome = self.qs_bluetooth_pane.handle_click(
                    p,
                    self.quick_settings_rect(),
                    &st.quick_settings.bt_devices,
                );
                match outcome {
                    DrillInOutcome::Back => st.quick_settings.pop_level(),
                    DrillInOutcome::Changed => {}
                    DrillInOutcome::None => return,
                }
                st.bump();
            }
        } else if matches!(current_level, crate::shell::quick_settings::QsLevel::Vpn) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                let outcome = self.qs_vpn_pane.handle_click(
                    p,
                    self.quick_settings_rect(),
                    &mut st.quick_settings.vpns,
                    self.qs_actions.as_ref(),
                );
                match outcome {
                    DrillInOutcome::Back => st.quick_settings.pop_level(),
                    DrillInOutcome::Changed => {}
                    DrillInOutcome::None => return,
                }
                st.bump();
            }
        } else if matches!(
            current_level,
            crate::shell::quick_settings::QsLevel::Notifications
        ) && self.qs_notification_clear_rect().contains(p)
            && let Some(chrome) = self.chrome.clone()
        {
            chrome.borrow_mut().clear_notification_history();
        }
    }

    /// Clicks on the quick-settings root page: a chevron drills into a
    /// sub-page, an action tile fires and closes the popover, otherwise
    /// falls to sliders/toggles/the power-profile chip. Split out of
    /// `handle_shell_quick_settings_click`, which always treats the
    /// click as absorbed regardless of what this does.
    fn handle_shell_quick_settings_root_click(&mut self, p: Point) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Tested before the toggle hit so the chevron
        // region wins over the toggle's full-card rect.
        let mut pushed_level = None;
        for i in [0, 1, 5] {
            let chev = self.quick_settings_toggle_chevron_rect(i);
            if chev.contains(p) {
                pushed_level = Some(match i {
                    0 => crate::shell::quick_settings::QsLevel::Wifi,
                    1 => crate::shell::quick_settings::QsLevel::Bluetooth,
                    _ => crate::shell::quick_settings::QsLevel::Vpn,
                });
                break;
            }
        }
        if let Some(level) = pushed_level {
            // Fetched here, not on panel-open: a Wi-Fi
            // scan is the slowest read in the set.
            match level {
                crate::shell::quick_settings::QsLevel::Wifi => {
                    self.qs_refresh.kick_wifi_networks();
                }
                crate::shell::quick_settings::QsLevel::Bluetooth => {
                    self.qs_refresh.kick_bt_devices();
                }
                _ => {}
            }
            let mut st = chrome.borrow_mut();
            st.quick_settings.push_level(level);
            st.bump();
            return;
        }
        // Each tile closes the popover; the action fires
        // after the chrome borrow drops.
        let mut action_hit: Option<usize> = None;
        for i in 0..4 {
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
                    st.overlays.ws_settings = true;
                }
                1 => {
                    let active = st.quick_settings.privacy.active_media.clone();
                    if active.is_empty() {
                        let privacy = &st.quick_settings.privacy;
                        let camera = format!("camera: {:?}", privacy.camera_kill);
                        let microphone = format!("microphone: {:?}", privacy.microphone_kill);
                        st.push_notification("Privacy", format!("{camera} · {microphone}"));
                    } else {
                        let mut failed = Vec::new();
                        let mut seen = std::collections::BTreeSet::new();
                        for use_ in &active {
                            if seen.insert(use_.client_id)
                                && os_actions::revoke_media_client(use_.client_id).is_err()
                            {
                                failed.push(use_.label.clone());
                            }
                        }
                        if failed.is_empty() {
                            st.quick_settings.privacy.active_media.clear();
                            st.push_notification(
                                "Camera and microphone access stopped",
                                "The active PipeWire clients were disconnected.",
                            );
                        } else {
                            st.push_notification(
                                "Could not stop all access",
                                format!("Still active: {}", failed.join(", ")),
                            );
                        }
                    }
                }
                2 => {
                    st.quick_settings
                        .push_level(crate::shell::quick_settings::QsLevel::Notifications);
                    st.bump();
                    return;
                }
                _ => {
                    self.power_menu.open();
                }
            }
            st.overlays.quick_settings = false;
            st.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
            st.bump();
            return;
        }
        self.handle_shell_quick_settings_toggle_click(&chrome, p);
    }

    /// Sliders, radio/feature toggles and the power-profile chip on the
    /// quick-settings root page. Split out of
    /// `handle_shell_quick_settings_root_click` — actions dispatch
    /// after dropping the chrome borrow to avoid a re-entrant
    /// `borrow_mut`; sliders don't participate, since `Released` owns
    /// their shell-out.
    fn handle_shell_quick_settings_toggle_click(&mut self, chrome: &Chrome, p: Point) {
        let mut pending_wifi: Option<bool> = None;
        let mut pending_bt: Option<bool> = None;
        let mut pending_wired: Option<bool> = None;
        let mut pending_airplane: Option<bool> = None;
        let mut pending_hotspot: Option<bool> = None;
        let mut pending_mobile: Option<bool> = None;
        let mut pending_night_light: Option<bool> = None;
        let mut pending_power: Option<crate::shell::quick_settings::PowerProfile> = None;
        let mut st = chrome.borrow_mut();
        let mut changed = false;
        for i in 0..2 {
            let (bar, _) = self.quick_settings_slider_geometry(i);
            // Extends the hit vertically to the whole row.
            let hit = Rect::new(bar.x0 - 4.0, bar.y0 - 12.0, bar.x1 + 4.0, bar.y1 + 12.0);
            if hit.contains(p) {
                let t = f64_to_f32_lossy(((p.x - bar.x0) / bar.width()).clamp(0.0, 1.0));
                if i == 0 {
                    st.quick_settings.brightness = t;
                } else {
                    st.quick_settings.volume = t;
                }
                // Committed to the OS on Released; only the
                // visual updates during drag, like a
                // physical mixer knob.
                self.qs_slider_drag = Some(i);
                changed = true;
            }
        }
        for i in 0..9 {
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
                } else if i == 1 {
                    st.quick_settings.bluetooth = !st.quick_settings.bluetooth;
                    pending_bt = Some(st.quick_settings.bluetooth);
                } else if i == 2 {
                    if let Some(wired) = st.quick_settings.wired.as_mut() {
                        wired.connected = !wired.connected;
                        pending_wired = Some(wired.connected);
                    }
                } else if i == 3 {
                    st.quick_settings.radios.airplane_mode =
                        !st.quick_settings.radios.airplane_mode;
                    pending_airplane = Some(st.quick_settings.radios.airplane_mode);
                } else if i == 4 {
                    st.quick_settings.radios.hotspot_active =
                        !st.quick_settings.radios.hotspot_active;
                    pending_hotspot = Some(st.quick_settings.radios.hotspot_active);
                } else if i == 5 {
                    st.quick_settings
                        .push_level(crate::shell::quick_settings::QsLevel::Vpn);
                } else if i == 6 {
                    st.quick_settings.night_light = !st.quick_settings.night_light;
                    pending_night_light = Some(st.quick_settings.night_light);
                } else if i == 7 {
                    st.push_notification(
                        "Displays",
                        "Display switching is available in Settings → Displays.",
                    );
                } else if st.quick_settings.radios.wwan_available {
                    st.quick_settings.radios.wwan_enabled = !st.quick_settings.radios.wwan_enabled;
                    pending_mobile = Some(st.quick_settings.radios.wwan_enabled);
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
        if !changed {
            return;
        }
        st.bump();
        drop(st);
        self.dispatch_quick_settings_toggle_actions(
            pending_wifi,
            pending_bt,
            pending_wired,
            pending_airplane,
            pending_hotspot,
            pending_mobile,
            pending_night_light,
            pending_power,
        );
    }

    /// Shells out to the OS for whichever quick-settings toggles changed.
    /// Split out of `handle_shell_quick_settings_toggle_click` — runs
    /// after that function drops its chrome borrow, to avoid a
    /// re-entrant `borrow_mut`.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_quick_settings_toggle_actions(
        &self,
        pending_wifi: Option<bool>,
        pending_bt: Option<bool>,
        pending_wired: Option<bool>,
        pending_airplane: Option<bool>,
        pending_hotspot: Option<bool>,
        pending_mobile: Option<bool>,
        pending_night_light: Option<bool>,
        pending_power: Option<crate::shell::quick_settings::PowerProfile>,
    ) {
        if let Some(v) = pending_wifi {
            crate::shell::qs_actions::log_qs_result("set_wifi", self.qs_actions.set_wifi(v));
        }
        if let Some(v) = pending_bt {
            crate::shell::qs_actions::log_qs_result(
                "set_bluetooth",
                self.qs_actions.set_bluetooth(v),
            );
        }
        if let Some(v) = pending_wired {
            crate::shell::qs_actions::log_qs_result("set_wired", self.qs_actions.set_wired(v));
        }
        if let Some(v) = pending_airplane {
            crate::shell::qs_actions::log_qs_result(
                "set_airplane_mode",
                self.qs_actions.set_airplane_mode(v),
            );
        }
        if let Some(v) = pending_hotspot {
            crate::shell::qs_actions::log_qs_result("set_hotspot", self.qs_actions.set_hotspot(v));
        }
        if let Some(v) = pending_mobile {
            crate::shell::qs_actions::log_qs_result(
                "set_mobile_broadband",
                self.qs_actions.set_mobile_broadband(v),
            );
        }
        if let Some(v) = pending_night_light {
            let mut values = serde_json::Map::new();
            values.insert("night_light".into(), serde_json::Value::Bool(v));
            if let Err(e) = settings_store::update_section("display", values) {
                tracing::warn!(error = %e, "night light preference save failed");
            }
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
    }

    /// Clicks inside the open workspace-settings lens: the pointer close
    /// control comes first, then default-app rects, the capability
    /// grant composer, member role/remove controls and revoke rects
    /// each take priority over the tab pills and name field. Reached
    /// only once `flags.ws_settings` is true, at which point every path
    /// absorbs the click (the trailing tab/name handling sets
    /// `*handled` for the paths that fall all the way through).
    fn handle_shell_ws_settings_click(&mut self, p: Point, handled: &mut bool) {
        if self.ws_settings_close_rect().contains(p) {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.commit_workspace_name();
                st.overlays.ws_settings = false;
                st.bump();
            }
            *handled = true;
            return;
        }
        // Before the tab pills so a Revoke/composer click can't
        // route to a pill above it. Every branch consumes the
        // click even for non-owners, since the control paints
        // disabled but shouldn't commit a half-typed name.
        let is_owner = self
            .chrome
            .as_ref()
            .is_some_and(|c| is_active_workspace_owner(&c.borrow()));

        if self.handle_shell_ws_settings_default_app_click(p, is_owner, handled) {
            return;
        }
        if self.handle_shell_ws_settings_grant_click(p, is_owner, handled) {
            return;
        }
        self.handle_shell_ws_settings_tab_and_name_click(p, handled);
    }

    /// A click on a default-app row (workspace policy or per-user
    /// choice) in the workspace-settings modal. Split out of
    /// `handle_shell_ws_settings_click`.
    fn handle_shell_ws_settings_default_app_click(
        &self,
        p: Point,
        is_owner: bool,
        handled: &mut bool,
    ) -> bool {
        let default_rects = self.default_app_rects.clone();
        for (resource, workspace_rect, user_rect) in default_rects {
            let workspace_hit = workspace_rect.area() > 0.0 && workspace_rect.contains(p);
            let user_hit = user_rect.area() > 0.0 && user_rect.contains(p);
            if !workspace_hit && !user_hit {
                continue;
            }
            if let Some(chrome) = self.chrome.clone() {
                let mut state = chrome.borrow_mut();
                let actor = state.local_user_id.clone();
                let candidates = crate::shell::launcher::default_app_candidates();
                let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
                let mut tree = state.tree_mut();
                if let Some(ws) = tree.active_workspace_mut() {
                    if workspace_hit && is_owner {
                        let next = crate::shell::workspace_settings::next_workspace_default(
                            ws.default_apps.policy.get(&resource),
                            &resource,
                            &candidates,
                        );
                        let _ = ws.set_default_app_policy_as(&actor, resource, next, &resolver);
                    } else if user_hit {
                        let next = crate::shell::workspace_settings::next_user_default(
                            ws.default_apps
                                .user_choices
                                .get(&actor)
                                .and_then(|m| m.get(&resource)),
                            &resource,
                            &candidates,
                        );
                        let _ =
                            ws.set_default_app_choice_as(&actor, &actor, resource, next, &resolver);
                    }
                }
                drop(tree);
                state.bump();
            }
            *handled = true;
            return true;
        }
        false
    }

    /// A click on the capability grant composer, a member's role/remove
    /// control, or a revoke button, in the workspace-settings modal.
    /// Split out of `handle_shell_ws_settings_click`.
    fn handle_shell_ws_settings_grant_click(
        &self,
        p: Point,
        is_owner: bool,
        handled: &mut bool,
    ) -> bool {
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
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    apply_composer_hit(&chrome, hit);
                }
                *handled = true;
                return true;
            }
        }

        let member_rects = self.member_control_rects.clone();
        for (user, role_rect, remove_rect) in &member_rects {
            let on_role = role_rect.area() > 0.0 && role_rect.contains(p);
            let on_remove = remove_rect.area() > 0.0 && remove_rect.contains(p);
            if !on_role && !on_remove {
                continue;
            }
            if is_owner && let Some(chrome) = self.chrome.clone() {
                let user = user.clone();
                edit_active_workspace_caps(&chrome, move |ws, actor| {
                    if on_role {
                        let current = ws
                            .role_of(&user)
                            .unwrap_or(crate::shell::state::CapRole::Viewer);
                        let next = crate::shell::workspace_settings::next_role(current);
                        crate::shell::workspace_settings::set_member_role(ws, actor, &user, next)
                    } else {
                        crate::shell::workspace_settings::remove_member(ws, actor, &user)
                    }
                });
            }
            *handled = true;
            return true;
        }

        let revoke_rects = self.permissions_revoke_rects.clone();
        for (cap_id, rrect) in &revoke_rects {
            if rrect.area() > 0.0 && rrect.contains(p) {
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    let cap_id = cap_id.clone();
                    edit_active_workspace_caps(&chrome, move |ws, actor| {
                        crate::shell::workspace_settings::apply_revoke(ws, actor, &cap_id)
                    });
                }
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Tab pill clicks, the invite-link copy button and the workspace
    /// name field, on the workspace-settings modal. Split out of
    /// `handle_shell_ws_settings_click`, which always treats the click
    /// as absorbed.
    fn handle_shell_ws_settings_tab_and_name_click(&mut self, p: Point, handled: &mut bool) {
        // Before the name-input branch so a pill click never
        // falls through to "commit pending edit".
        let pill_rects = self.ws_settings_tab_pill_rects;
        for (idx, prect) in pill_rects.iter().enumerate() {
            if prect.area() > 0.0 && prect.contains(p) {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
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
        // Copy the workspace invite link.
        let on_general = self
            .chrome
            .as_ref()
            .is_some_and(|c| matches!(c.borrow().ws_settings_tab, WsTab::General));
        if on_general && self.ws_settings_copy_rect().contains(p) {
            let link = self
                .chrome
                .as_ref()
                .and_then(|c| {
                    c.borrow()
                        .tree()
                        .active_workspace()
                        .map(crate::shell::workspace_settings::invite_link)
                })
                .unwrap_or_default();
            if !link.is_empty() {
                ui::text_selection::clipboard_copy(&link);
                self.ws_invite_copied_at = Some(Instant::now());
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().bump();
                }
            }
            *handled = true;
            return;
        }
        // Name rect begins an edit; any other click inside
        // commits the pending one.
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
                    let caret = new_name.as_ref().map_or(0, std::string::String::len);
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
    }

    /// Overview-only pointer input: wheel pans the row list (the column
    /// header stays pinned), and a click either creates a room (the
    /// "New room" slot) or activates the room under it and closes the
    /// overview.
    fn handle_shell_overview_pointer_event(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
    ) -> bool {
        if !self.overlay_flags().overview {
            return false;
        }
        // Pans the overview's row list; the column header stays pinned.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy: f64 = match *delta {
                // Matches the strip's wheel factor (`GroupRow::handle_event`).
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if dy.abs() > f64::EPSILON {
                let max = self.overview_max_scroll_y();
                self.overview_scroll_y = (self.overview_scroll_y + dy).clamp(0.0, max);
                *handled = true;
                return true;
            }
            return false;
        }
        // Click-on-overview-card: consume the click and — if it lands
        // on a workspace card — activate that workspace + close the
        // overview.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
        {
            // Wins the hit-test before the row-card scan so a
            // click on it doesn't fall through to "missed".
            if self.add_workspace_rect.area() > 0.0 && self.add_workspace_rect.contains(p) {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    // Mirrors the wheel-disc slot: both create
                    // affordances seed "Untitled".
                    st.add_workspace("Untitled");
                }
                *handled = true;
                return true;
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
                // Dismisses even on a miss.
                st.overlays.overview = false;
                st.bump();
            }
            *handled = true;
            return true;
        }
        false
    }

    /// Palette-owned input, ahead of the sidebar/content dispatch: kind
    /// chips, a click on a result row, keyboard Enter, then everything
    /// else the palette widget itself wants (typing, arrow selection).
    #[allow(clippy::too_many_arguments)]
    fn route_palette_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        palette_child: Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.palette_visible {
            return false;
        }
        // Before the row hit-test below so a chip click doesn't fall
        // through to the top result.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
        {
            // A direct way to reach "nothing selected", not just
            // untick every chip one at a time.
            if self.search_all_chip_rect.area() > 0.0 && self.search_all_chip_rect.contains(p) {
                if !self.active_kind_filter.is_empty() {
                    self.active_kind_filter.clear();
                    self.last_search_query.clear(); // force a refresh
                    self.refresh_search_results(state, path);
                }
                *handled = true;
                return true;
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
                    return true;
                }
            }
        }
        // Before delegating to the palette, so a single click both
        // selects and fires.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
            && let Some(row) = self.palette.row_at(p)
        {
            self.palette.set_cursor(row);
            self.activate_selected_result(state, path);
            *handled = true;
            return true;
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
            return true;
        }
        // Let the palette own the character/backspace/arrow keys.
        if let Some(palette_child) = palette_child {
            let query_before = self.palette.query().to_string();
            self.palette.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                palette_child,
                taffy,
            );
            // Recompute results when a character/backspace changes the query.
            if self.palette.query() != query_before {
                self.refresh_search_results(state, path);
            }
            if *handled {
                return true;
            }
        }
        false
    }

    /// The tail of `handle_event`: sidebar forwarding, click-to-focus,
    /// content dispatch, then the horizontal-wheel fallback that steps
    /// focus along the strip (the same step Super+Left/Right takes;
    /// 0.5 rejects trackpad noise).
    #[allow(clippy::too_many_arguments)]
    fn dispatch_pointer_and_content_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        children: &[NodeId],
        taffy: &TaffyTree<()>,
    ) {
        // First dibs so wheel-content precedes the desktop underneath.
        // Gated on the sidebar's rect for pointer events — otherwise it can
        // overlap the topbar band and swallow the overview-toggle click.
        if let (Some(sidebar), Some(&sidebar_child)) = (self.sidebar.as_mut(), children.get(2)) {
            let sidebar_rect = sidebar.get_rect();
            let forward_pointer_to_sidebar = match event {
                WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. } => {
                    self.last_cursor.is_some_and(|p| sidebar_rect.contains(p))
                }
                _ => true,
            };
            if forward_pointer_to_sidebar {
                sidebar.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    sidebar_child,
                    taffy,
                );
                if *handled {
                    return;
                }
            }
        }
        // Click-anywhere-in-a-group focuses that group: hit-test before
        // forwarding and stamp `tree.active_strip`/`active_group`, or
        // clicking the window body (not a tab pill) never updates them.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
            && let Some(p) = self.last_cursor
        {
            self.stamp_active_group_from_click(p);
        }
        if let Some(&content_child) = children.first() {
            self.content.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                content_child,
                taffy,
            );
        }

        // Same step as Super+Left/Right, avoiding a park on the window
        // seam that free-panning would allow. 0.5 rejects trackpad noise.
        if !*handled && let WindowEvent::MouseWheel { delta, .. } = event {
            let dx = match *delta {
                winit::event::MouseScrollDelta::LineDelta(x, _) => x,
                winit::event::MouseScrollDelta::PixelDelta(p) => f64_to_f32_lossy(p.x) / 40.0,
            };
            if dx.abs() > 0.5 {
                let dir = if dx > 0.0 { 1 } else { -1 };
                if self.step_focus_along_strip(dir) {
                    *handled = true;
                }
            }
        }
    }
}

// Overview layout constants. Module-level so the draw path,
// `hit_test_overview` and the unit tests share one geometry oracle.

/// Widest the rooms panel ever gets, matching the workspace-settings
/// modal so the shell's floating surfaces are one family.
const OVERVIEW_PANEL_WIDTH_PX: f64 = 720.0;
/// Inset from the viewport edge, and the smallest gap the panel keeps
/// from the top and bottom of the screen.
const OVERVIEW_PANEL_MARGIN_PX: f64 = 48.0;
/// Inner padding on the panel.
const OVERVIEW_PANEL_PAD_PX: f64 = 20.0;
/// Height of the "you are here" block: the current room's name, who can
/// reach it, and the faces of the people in it.
const OVERVIEW_HERE_HEIGHT_PX: f64 = 84.0;
/// Height of the caption above a section ("Shared out of here", "Go to").
const OVERVIEW_CAPTION_HEIGHT_PX: f64 = 26.0;
/// Height of one share row.
const OVERVIEW_SHARE_ROW_HEIGHT_PX: f64 = 28.0;
/// Most share rows drawn before the rest collapse into "+N more" — the
/// panel says whether things leave this room, the permissions pane has
/// the full list.
const OVERVIEW_SHARES_MAX_ROWS: usize = 4;
/// Height of one room row in the "go to" list.
const OVERVIEW_ROOM_ROW_HEIGHT_PX: f64 = 60.0;
/// Gutter between room rows.
const OVERVIEW_ROOM_ROW_GAP_PX: f64 = 6.0;
/// Gap between the "you are here" block and the room list.
const OVERVIEW_SECTION_GAP_PX: f64 = 18.0;
/// Corner radius of one room row.
const OVERVIEW_ROW_RADIUS_PX: f64 = 10.0;
/// Radius of an avatar in a face stack.
const OVERVIEW_FACE_RADIUS_PX: f64 = 12.0;
/// Horizontal step between overlapping faces.
const OVERVIEW_FACE_STEP_PX: f64 = 18.0;
/// How much of their own colour an away member's face keeps, the rest
/// being the panel behind it. At a third it vanished against a light
/// ground and lost the member altogether.
const OVERVIEW_FACE_AWAY_ALPHA: f32 = 0.55;
/// Most faces drawn before the rest collapse into a "+N" disc.
const OVERVIEW_FACES_MAX: usize = 4;
/// How far the panel rises as it opens, matching the workspace-settings
/// modal so the shell's floating surfaces arrive the same way.
const OVERVIEW_PANEL_RISE_PX: f64 = 20.0;
/// Scale the panel starts its entrance at.
const OVERVIEW_PANEL_START_SCALE: f64 = 0.97;
/// Height of the fade over the bottom of an overflowing room list, so a
/// clipped row reads as "there is more" rather than unfinished.
const OVERVIEW_SCROLL_FADE_PX: f64 = 40.0;

impl Widget for ShellOverlayHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        if let Some(onboarding) = self.onboarding.as_mut() {
            onboarding.sync(s, p);
            let finished = onboarding.finished();
            if finished {
                self.onboarding = None;
                self.onboarding_released = true;
                self.onboarding_focus_reset = true;
            }
            return;
        }
        self.content.sync(s, p);
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.sync(s, p);
        }
        self.palette.sync(s, p);
    }

    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
        if let Some(onboarding) = self.onboarding.as_mut() {
            return onboarding.update(doc, path);
        }
        // Runs first so an invite token / prompt is in chrome before draw.
        self.pump_peer_sync();
        // Workspace switch before anything walking `group_row_ptrs` (it
        // replaces the whole row set), then splice the tree's structural
        // changes into the rows — both before the children's own `update`
        // so freshly-spliced groups join this frame's sync pass.
        let ws_swapped = self.resync_workspace_content();
        let chrome_dirty = self.resync_strips_from_tree() || ws_swapped;
        // Before children update, so a promotion/restore this frame shows.
        self.sync_fullscreen_from_chrome();
        let content_dirty = self.content.update(doc, path);
        let palette_dirty = self.palette.update(doc, path);
        let sidebar_dirty = self.sidebar.as_mut().is_some_and(|w| w.update(doc, path));
        // The harvesters run after `content.update` so this frame's
        // gestures are visible to them, mirroring one-shot widget-side
        // changes into the persisted tree so they survive a restart and
        // replicate to peers.
        self.reconcile_active_group_off_closing();
        self.harvest_pending_maximize_changes();
        self.harvest_pending_width_changes();
        self.harvest_pending_group_reorder();
        self.harvest_pending_tab_reorder();
        self.harvest_pending_search_open();
        // Refreshing here (not inside the harvest) means the palette never
        // shows "Nothing to show" before the user's first keystroke.
        let opened_for_new_tab = self.harvest_pending_new_tab();
        if opened_for_new_tab {
            self.refresh_search_results(doc, path);
        }
        self.clear_stale_open_target();
        self.clear_stale_wifi_password();
        self.sync_status_from_bridge();
        self.harvest_active_tab_writeback();
        let follow_dirty = self.reconcile_followed_peer();
        // Both edge-triggered on `active_group`, so they run after the
        // writeback above reflects this frame's focus change.
        self.scroll_active_group_into_view();
        self.mark_focused_group();
        self.dispatch_cross_strip_detaches();
        // Offer every open tab's document to its own writer, debounced and
        // off-thread — without this the store never writes to disk.
        if let Some(chrome) = self.chrome.as_ref() {
            let records = chrome.borrow().records().clone();
            records.tick();
        }
        // Before the prompt pump, so a request arriving this frame is on
        // screen by the end of it, not one frame later.
        let portal_dirty = self.drain_portal_prompts();
        let picker_dirty = self.pump_file_picker();
        self.pump_permission_prompt();
        let launch_dirty = self.drain_launch_events() | self.retire_notices_for_opened_windows();
        // Also driven by the widget's own `update`, but routed here too so
        // chrome stays consistent for tests that never call the widget.
        let notifications_dropped = if let Some(chrome) = self.chrome.as_ref() {
            let dropped = chrome
                .borrow_mut()
                .update_notification_lifecycle(std::time::Instant::now());
            dropped > 0
        } else {
            false
        };
        // Stamp the wall clock on whatever `doc_*` left open, or automerge
        // closes it implicitly with a zero timestamp — undatable later.
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow().tree_mut().commit_room_change();
        }
        content_dirty
            || palette_dirty
            || sidebar_dirty
            || notifications_dropped
            || launch_dirty
            || chrome_dirty
            || portal_dirty
            || picker_dirty
            || follow_dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if let Some(onboarding) = self.onboarding.as_mut() {
            let child = onboarding.build_layout(taffy, theme, node_map);
            let node = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap_or(child);
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        // Both pushed before the content tree is built, so this pass's
        // fullscreen collapse and per-display width resolution are in
        // effect — the latter is what makes a single-frame offscreen
        // render match the live shell's geometry.
        self.sync_fullscreen_from_chrome();
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().is_fullscreen());
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
        // Override the widget's own narrow 360px default so the palette
        // reads as a centered modal sheet, not a top-right popover.
        // `taffy.style` can only fail for a `NodeId` foreign to this tree
        // or already removed — `palette_node` is neither, it was just
        // inserted above — but the override is skippable, so a failure
        // here degrades to the widget's own default layout instead of
        // panicking.
        if let Ok(style) = taffy.style(palette_node) {
            let mut palette_style = style.clone();
            palette_style.position = Position::Absolute;
            // 70% of viewport clamped to [560, 640]; 560 is the minimum
            // that still fits the filter strip's eight pills without the
            // last one bleeding into the close glyph.
            let palette_target_w = (self.rect.width() * 0.7).clamp(560.0, 640.0);
            palette_style.size = Size {
                width: Dimension::length(f64_to_f32_lossy(palette_target_w)),
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
            let _ = taffy.set_style(palette_node, palette_style);
        }
        // `self.rect` is last frame's, so frame 1 sizes off a zero-width
        // host and lands on the 560 floor; remembering what it sized
        // against lets `apply_layout` re-dirty and converge.
        self.palette_layout_host_w = self.rect.width();

        self.content_node = Some(content_node);
        self.palette_node = Some(palette_node);

        // Override the sidebar's own leaf style with absolute positioning
        // so it hovers above the content pane instead of displacing it.
        let sidebar_node = self.sidebar.as_mut().map(|sidebar| {
            let node = sidebar.build_layout(taffy, theme, node_map);
            // See the palette override above: failure here is not
            // reachable for a node this function just created, but the
            // override is skippable rather than fatal.
            if let Ok(style) = taffy.style(node) {
                let mut style = style.clone();
                style.position = Position::Absolute;
                style.inset = TaffyRect {
                    left: LengthPercentageAuto::length(0.0),
                    right: LengthPercentageAuto::auto(),
                    top: LengthPercentageAuto::length(self.sidebar_top_y),
                    bottom: LengthPercentageAuto::length(0.0),
                };
                style.size = Size {
                    width: Dimension::length(sidebar_width_for_viewport(f64_to_f32_lossy(
                        self.rect.height(),
                    ))),
                    height: Dimension::auto(),
                };
                style.flex_grow = 0.0;
                style.flex_shrink = 0.0;
                // A fullscreen surface covers the whole output, so the
                // wheel is suppressed alongside the top bar.
                if fullscreen {
                    style.display = Display::None;
                }
                let _ = taffy.set_style(node, style);
            }
            node
        });
        self.sidebar_node = sidebar_node;

        let children: Vec<NodeId> = match sidebar_node {
            Some(s) => vec![content_node, palette_node, s],
            None => vec![content_node, palette_node],
        };
        // `new_with_children` only errs on a foreign/removed child
        // `NodeId`, and every id in `children` was just created above in
        // this same tree — but rather than assert that, fall back to the
        // content node alone: a host that (impossibly) can't wrap its
        // overlays still shows the desktop instead of dying.
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
            .unwrap_or(content_node);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout for `node` would mean taffy never computed
        // this frame's tree at all — nothing downstream has a sensible
        // rect to work with, so skip the frame rather than panic.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(onboarding) = self.onboarding.as_mut() {
            if let Ok(children) = taffy.children(node)
                && let Some(&child) = children.first()
            {
                onboarding.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
            }
            return;
        }
        // One more pass if the card sized against a stale host width;
        // settles in a single extra frame.
        if (self.palette_layout_host_w - self.rect.width()).abs() > f64::EPSILON {
            self.palette_layout_dirty = true;
        }
        // Wheel-disc slot width plus a gap, reserved so the strip leaves
        // room for the wheel and a wallpaper gutter beside it. No sidebar
        // means no gutter to reserve.
        let wheel_w = if self.sidebar.is_some() {
            f64::from(sidebar_width_for_viewport(f64_to_f32_lossy(
                self.rect.height(),
            ))) + WHEEL_TO_STRIP_GAP_PX
        } else {
            0.0
        };
        // Re-derive strip widths now the host's rect is known: frame 1's
        // `build_layout` had no viewport yet. A no-op once the
        // compositor has pushed real output geometry.
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
        let Ok(children) = taffy.children(node) else {
            return;
        };
        if let Some(&content_child) = children.first() {
            self.content
                .apply_layout(taffy, content_child, Point::new(self.rect.x0, self.rect.y0));
        }
        if let Some(&palette_child) = children.get(1) {
            self.palette
                .apply_layout(taffy, palette_child, Point::new(self.rect.x0, self.rect.y0));
        }
        if let (Some(sidebar), Some(sidebar_child)) = (self.sidebar.as_mut(), children.get(2)) {
            // The wheel disc lives in the overlay layer, not as a sibling
            // of GroupRow, so it pans in lock-step by shifting its origin
            // with the active strip's `scroll_x`. `group_row_ptrs` is a
            // HashMap, so take the topmost strip in tree order rather
            // than whatever "first" iterates to.
            let top_strip: Option<StripId> = self.chrome.as_ref().and_then(|c| {
                let st = c.borrow();
                let tree = st.tree();
                tree.active_workspace()
                    .and_then(|ws| ws.strips.first().map(|s| s.id))
            });
            let scroll_of = |id: Option<StripId>| -> Option<f64> {
                let ptr = {
                    let id = id?;
                    self.group_row_ptrs.get(&id)?
                };
                // SAFETY: same contract as the other group_row_ptrs
                // accessors — pointer captured from a `Box<GroupRow>`
                // we own through the content tree, host outlives
                // content, &mut self → unaliased read.
                Some(unsafe { (&**ptr).scroll_x() })
            };
            let active_scroll_x: f64 = scroll_of(top_strip)
                .or_else(|| {
                    self.group_row_ptrs
                        .values()
                        .next()
                        .map(|&ptr| unsafe { (&*ptr).scroll_x() })
                })
                .unwrap_or(0.0);
            // A summoned wheel places itself around the pointer in screen
            // space; the strip's pan would shift it out from under the cursor.
            let summoned = self
                .chrome
                .as_ref()
                .is_some_and(|c| c.borrow().wheel_summon().is_some());
            let anchor_x = if summoned {
                self.rect.x0
            } else {
                self.rect.x0 - active_scroll_x
            };
            sidebar.apply_layout(taffy, *sidebar_child, Point::new(anchor_x, self.rect.y0));
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
        if let Some(onboarding) = self.onboarding.as_mut() {
            onboarding.draw(scene, transform, text_renderer, theme, presence);
            return;
        }
        // Split into four layer passes — was one 372-line function; see
        // each helper (in the other `impl ShellOverlayHost` block) for
        // what it covers.
        let flags = self.overlay_flags();
        self.draw_desktop_layer(scene, transform, text_renderer, theme, presence);
        self.draw_overview_and_ws_settings_overlays(scene, transform, text_renderer, theme, flags);
        self.draw_dropdowns_and_sheets(scene, transform, text_renderer, theme, presence, flags);
        self.draw_top_overlays(scene, transform, text_renderer, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "event routing stays linear so modal gates retain their security-sensitive order"
    )]
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
        if let Some(onboarding) = self.onboarding.as_mut() {
            let child_node = taffy
                .children(node_id)
                .ok()
                .and_then(|children| children.first().copied())
                .unwrap_or(node_id);
            onboarding.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
            // First-login is a modal machine surface. Even an event the
            // current control does not consume must never fall through to
            // hidden desktop chrome.
            *handled = true;
            return;
        }
        // Split into named gates and dispatch stages — was one
        // 1591-line function. Each helper (in the other `impl
        // ShellOverlayHost` block) covers one gate or stage, called in
        // the same order this ran inline; a `bool` return means "this
        // gate/stage fully resolved the event, stop here", exactly where
        // the original inline code hit a bare `return`.
        self.track_cursor_position(event, scale_factor);

        if self.route_modal_absorbing_overlays(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
        ) {
            return;
        }
        if self.route_qs_slider_drag(event, handled) {
            return;
        }
        if self.route_room_activity_event(event, scale_factor, *modifiers, handled) {
            return;
        }
        if self.route_absorbable_pointer_and_toast_click(event, handled) {
            return;
        }
        if self.route_permission_prompt_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
        ) {
            return;
        }
        if self.route_dropped_file_event(event, handled) {
            return;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.handle_shell_keyboard_event(logical_key, *modifiers, state, path, handled)
        {
            return;
        }
        if self.handle_shell_mouse_click_event(event, handled) {
            return;
        }
        if self.handle_shell_overview_pointer_event(event, handled) {
            return;
        }

        // No children means taffy never laid this node out this frame —
        // there is nothing to route the event to.
        let Ok(children) = taffy.children(node_id) else {
            return;
        };
        if self.route_palette_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            children.get(1).copied(),
            taffy,
        ) {
            return;
        }
        self.dispatch_pointer_and_content_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            &children,
            taffy,
        );
    }

    fn name(&self) -> &'static str {
        "ShellOverlayHost"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};

        let mut root = AccessNode::new(Role::Group).with_name("qOS workspace");
        if self.overlay_flags().room_activity {
            root.children
                .push(self.room_activity.accessibility_snapshot());
        }
        Some(root)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.overlay_flags()
            .room_activity
            .then(|| self.room_activity.accessibility_rect_for_path(path))
            .flatten()
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.overlay_flags().room_activity && self.room_activity.focus_accessibility_path(path)
    }

    /// Expose content + palette + sidebar as direct children so trait
    /// walkers can recurse into the live tree, in `build_layout`'s
    /// child-node order (sidebar last so it paints over the chrome).
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(onboarding) = self.onboarding.as_ref() {
            return vec![onboarding];
        }
        let mut out: Vec<&dyn Widget> = Vec::with_capacity(3);
        out.push(self.content.as_ref());
        out.push(&self.palette);
        if let Some(s) = self.sidebar.as_ref() {
            out.push(s.as_ref());
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(onboarding) = self.onboarding.as_mut() {
            return vec![onboarding];
        }
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(3);
        out.push(self.content.as_mut());
        out.push(&mut self.palette);
        if let Some(s) = self.sidebar.as_mut() {
            out.push(s.as_mut());
        }
        out
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.onboarding_focus_reset)
            | self
                .onboarding
                .as_mut()
                .is_some_and(Widget::take_focus_reset)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Bubbles structural-dirty flags up from the content tree (and
    /// sidebar) so the post-event rebuild picks up new nodes (e.g. a
    /// spliced `WindowGroup`). Without this the default `false` would
    /// shadow every child signal, leaving it at `Rect::ZERO`. Uses `|`,
    /// not `||`, so both children clear their own flags this tick.
    fn take_structure_dirty(&mut self) -> bool {
        if std::mem::take(&mut self.onboarding_released) {
            return true;
        }
        if let Some(onboarding) = self.onboarding.as_mut() {
            return onboarding.take_structure_dirty();
        }
        // Before resync, or it sees "widget has X, tree doesn't" and evicts X.
        self.harvest_widget_only_groups_into_tree();
        // Pumped here too since the shots driver's relayout path skips
        // `update()`; `last_chrome_revision` makes both call sites idempotent.
        let ws_swapped = self.resync_workspace_content();
        let resynced = self.resync_strips_from_tree() | ws_swapped;
        let content = self.content.take_structure_dirty();
        let sidebar = self
            .sidebar
            .as_mut()
            .is_some_and(|s| s.take_structure_dirty());
        let palette = std::mem::take(&mut self.palette_layout_dirty);
        content | sidebar | palette | resynced
    }

    fn needs_animation_frame(&self) -> bool {
        if let Some(onboarding) = self.onboarding.as_ref() {
            return onboarding.needs_animation_frame();
        }
        // Continuous redraws while overview crossfade, quick-settings
        // open/close, or any inner widget's animation is in flight;
        // `tick_*` clear their own flags once elapsed, so this idles.
        let now = Instant::now();
        let qs_in_flight = self.qs_transition.is_animating(now);
        let ws_settings_in_flight = self.ws_settings_transition.is_animating(now);
        let palette_in_flight = self.palette_transition.is_animating(now);
        let power_menu_in_flight = self.power_menu.is_animating(now);
        let room_activity_in_flight = self.room_activity_transition.is_animating(now);
        let projection_needs_frame = self.projection_overlay.needs_animation_frame(now);
        let sidebar_frame = self
            .sidebar
            .as_ref()
            .is_some_and(|s| s.needs_animation_frame());
        self.transition_started.is_some()
            || qs_in_flight
            || ws_settings_in_flight
            || palette_in_flight
            || power_menu_in_flight
            || room_activity_in_flight
            // The projection component includes both its transition and the
            // bounded worker-response frame pump in this signal.
            || projection_needs_frame
            || self.content.needs_animation_frame()
            || sidebar_frame
            // A reading landing after the open animation settles still
            // has to reach the screen.
            || self.qs_refresh.is_in_flight()
    }
}

/// `(alpha, dy, scale)` for the rooms panel at linear progress `t`. Rises
/// the last 20px and settles from 0.97 on an ease-out; since `t` runs
/// both ways, the exit is the same curve in reverse. The desktop stays
/// on screen, dimmed by the scrim, for the whole gesture.
fn overview_panel_curve(t: f32) -> (f32, f64, f64) {
    let o = ui::ease_out(f64::from(t.clamp(0.0, 1.0)));
    let alpha = f64_to_f32_lossy(o);
    let dy = OVERVIEW_PANEL_RISE_PX * (1.0 - o);
    let scale = OVERVIEW_PANEL_START_SCALE + (1.0 - OVERVIEW_PANEL_START_SCALE) * o;
    (alpha, dy, scale)
}

/// Map the prompt widget's choice onto the portal's outcome vocabulary.
/// Two enums for one decision because they belong to different layers;
/// keeping the mapping in one place stops them drifting.
fn prompt_choice_to_outcome(choice: PromptChoice) -> crate::portal::PromptOutcome {
    use crate::portal::PromptOutcome;
    match choice {
        PromptChoice::Allow => PromptOutcome::AllowOnce,
        PromptChoice::Session => PromptOutcome::AllowSession,
        PromptChoice::Always => PromptOutcome::AllowAlways,
        PromptChoice::Deny => PromptOutcome::Deny,
    }
}

/// Human-readable label for the inline permission prompt from a
/// [`Capability`] payload.
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
        Capability::Print { .. } => "Send documents to a printer".to_string(),
        Capability::Network { hosts, .. } => match hosts {
            Some(h) if !h.is_empty() => format!("Network access to {}", h.join(", ")),
            _ => "Network access".to_string(),
        },
    }
}

/// Return a copy of `cap` with its `scope` field replaced. The portal
/// layer pushes a placeholder scope (typically [`GRANT_ONCE`]); the
/// prompt's button choice decides what actually gets persisted.
fn with_grant_scope(cap: Capability, scope: GrantScope) -> Capability {
    match cap {
        Capability::Filesystem { path, mode, .. } => Capability::Filesystem { path, mode, scope },
        Capability::Camera { .. } => Capability::Camera { scope },
        Capability::Microphone { .. } => Capability::Microphone { scope },
        Capability::Screencast { .. } => Capability::Screencast { scope },
        Capability::RemoteDesktop { .. } => Capability::RemoteDesktop { scope },
        Capability::Print { .. } => Capability::Print { scope },
        Capability::Network { hosts, .. } => Capability::Network { hosts, scope },
    }
}

/// Is the local user the owner of the active workspace? Gates the
/// permission prompt's "Always" button and the Permissions tab's Revoke
/// buttons, via the workspace's capability model (recorded owner, or
/// anyone given `Admin`). No recorded owner means no administrators
/// (fail-closed) — [`crate::shell::state::WorkspaceTree::adopt_ownerless_workspaces`]
/// keeps the local user's own workspaces out of that state.
fn is_active_workspace_owner(state: &crate::shell::chrome_state::ChromeState) -> bool {
    if let Some(forced) = state.test_only_owner_override {
        return forced;
    }
    let user = state.active_local_user();
    state
        .tree()
        .active_workspace()
        .is_some_and(|ws| ws.may_administer(&user))
}

/// Records an app's capability grant on the active workspace: written
/// into the capability list, mirrored into the replicated doc, and
/// flushed to disk when its expiry should survive a restart. Everything
/// the prompt hands out flows through here.
fn record_app_grant(
    state: &crate::shell::chrome_state::ChromeState,
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

/// Applies a permission edit to the active workspace, then mirrors it
/// into the replicated doc and flushes — without the mirror, a click
/// would edit only the in-memory cache and the next boot would read the
/// old answer back off disk. `edit` runs with the local user as actor.
#[allow(
    clippy::significant_drop_tightening,
    reason = "st stays borrowed for `actor` up front and `st.bump()` at the end; the inner \
              `tree` guard it derives feeds `outcome`, which is exactly what decides whether \
              that final bump runs"
)]
fn edit_active_workspace_caps<F>(chrome: &Chrome, edit: F) -> bool
where
    F: FnOnce(&mut crate::shell::state::Workspace, &crate::shell::state::UserId) -> CapEdit,
{
    let mut st = chrome.borrow_mut();
    let actor = st.active_local_user();
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
    // A flat match over every variant, rather than peeling `Submit` off
    // first and re-matching the rest — the latter needed an `other =>`
    // catch-all whose own exhaustive sub-match required an `unreachable!`
    // arm for `Submit`, a real panic site in a `panic = "abort"` crate
    // even though it could never fire.
    match hit {
        ComposerHit::Submit => {
            let draft = chrome.borrow().ws_grant_draft;
            edit_active_workspace_caps(chrome, move |ws, actor| {
                crate::shell::workspace_settings::apply_grant_draft(ws, actor, &draft)
            });
        }
        ComposerHit::Subject => {
            let mut st = chrome.borrow_mut();
            let subject_count = st.tree().active_workspace().map_or(0, |ws| {
                crate::shell::workspace_settings::grant_subjects(ws).len()
            });
            st.ws_grant_draft.cycle_subject(subject_count);
            st.bump();
        }
        ComposerHit::Capability => {
            let mut st = chrome.borrow_mut();
            st.ws_grant_draft.cycle_choice();
            st.bump();
        }
        ComposerHit::Expiry(i) => {
            let mut st = chrome.borrow_mut();
            if let Some(expiry) = EXPIRY_CHOICES.get(i) {
                st.ws_grant_draft.expiry = *expiry;
            }
            st.bump();
        }
    }
}

/// The kind chips a picker opened by "give me a new window" starts lit.
///
/// Both kinds are programs the user can start; they are separate chips
/// because one runs sandboxed in the room and the other does not, which
/// is a difference worth seeing — but neither is the odd one out when the
/// question is "what do you want to open".
fn app_kind_filter() -> Vec<crate::shell::search::ResourceKind> {
    use crate::shell::search::ResourceKind;
    vec![ResourceKind::Application, ResourceKind::HostApplication]
}

/// [`draw_scrim`] scaled by `openness` so the palette/overview scrim
/// fades with its surface.
fn draw_scrim_at(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    fill_scrim(
        scene,
        transform,
        rect,
        PALETTE_SCRIM_ALPHA * openness,
        theme,
    );
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

/// Paint `rect` in the theme's scrim hue at `strength`. Uses
/// `surfaces.scrim` rather than a literal black, since a light theme's
/// scrim is a different colour from a dark theme's.
pub(crate) fn fill_scrim(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    strength: f32,
    theme: &Theme,
) {
    let alpha = f32_to_u8_saturating((strength.clamp(0.0, 1.0) * 255.0).round());
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

/// Synthesises a persisted [`crate::shell::state::WindowGroup`] from a
/// widget-side one, for [`ShellOverlayHost::stamp_active_group_from_click`]
/// when the click resolves a group that only exists widget-side (a fresh
/// drag-detach not yet written back). Each tab becomes an
/// `AppPlaceholder`, matching the tree's automerge codec for foreign/stub
/// tabs.
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
            // Carry the widget-side AppKind hint so `owns_tab`
            // predicates can still classify the tab.
            let kind = match &desc.app_kind {
                Some(ui::widget::AppKind::Terminal) => AppKind::Terminal,
                Some(ui::widget::AppKind::Files) => AppKind::FileExplorer,
                Some(ui::widget::AppKind::Browser) => AppKind::Browser,
                Some(ui::widget::AppKind::Settings) => AppKind::Settings,
                // Recover the hosted app's label from the title, which
                // is all the widget layer kept.
                _ => AppKind::Other(crate::shell::hosted_label_for_title(&desc.title)),
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

/// Picks the topmost group whose rect contains `p`. `candidates` is
/// `(strip_id, group_id, rect)` in traversal order — the FIRST match
/// wins, so a wrapping parent doesn't shadow a smaller child. Zero-area
/// (not-yet-laid-out) rects are skipped.
fn pick_group_at(candidates: &[(StripId, GroupId, Rect)], p: Point) -> Option<(StripId, GroupId)> {
    candidates
        .iter()
        .find(|(_, _, r)| r.area() > 0.0 && r.contains(p))
        .map(|(s, g, _)| (*s, *g))
}

/// `a` moved `t` of the way toward `b`, opaque throughout — matters
/// wherever something (a monogram, an icon) is drawn on the result and
/// needs a real colour to pick its own contrast from.
fn mix_toward(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (a.to_rgba8(), b.to_rgba8());
    let m = |x: u8, y: u8| {
        f32_to_u8_saturating((f32::from(x) + (f32::from(y) - f32::from(x)) * t).round())
    };
    Color::from_rgb8(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// The surface every floating chrome panel is painted on — quick
/// settings, the collaborators popover, workspace-settings, peer-sync.
/// Uses the theme's own popover tier (not a raw colour ramp slot) so
/// panels stay the same material as the bar they hang off (HIG §9.2).
fn panel_surface(theme: &Theme, alpha: f32) -> Color {
    ui::chrome::shell::panel_ground(theme).with_alpha_factor(alpha)
}

/// Workspace settings is app-grade content hosted by the shell, so it uses
/// the same base surface as applications rather than the brighter popover tier.
fn workspace_settings_surface(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.base
}

/// Pick the [`crate::shell::search::SearchProvider`] that owns the
/// currently focused tab, if any. Seeds `active_provider_id` for
/// [`crate::shell::search::SearchRegistry::query`] under
/// [`crate::shell::chrome_state::SearchScope::ActiveApp`].
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

/// Clones `theme` with `surfaces.raised` re-tinted to a translucent glass
/// slate-900 — how `CommandPalette` (which paints from `surfaces.raised`)
/// gets a translucent fill without a per-draw override plumbed into a
/// shared widget. The stroke keeps `border_standard` from the original.
fn palette_surface_theme(theme: &Theme, alpha: f32) -> Theme {
    let mut themed = theme.clone();
    themed.tokens.colors.surfaces.raised = panel_surface(theme, alpha);
    themed
}

/// `(alpha, dy, scale)` for a modal driven by a [`ui::Transition`],
/// covering BOTH the enter (openness 0→1) and the exit (openness 1→0).
/// The panel starts 20 px low at `start_scale` and rises to rest on the
/// enter; the exit plays the same curve in reverse instead of the panel
/// vanishing.
pub(crate) fn modal_transition_curve(
    t: &ui::Transition,
    now: Instant,
    start_scale: f64,
) -> (f32, f64, f64) {
    let o = t.value(now);
    let scale = start_scale + (1.0 - start_scale) * f64::from(o);
    (o, 20.0 * f64::from(1.0 - o), scale)
}

impl ShellOverlayHost {
    /// Base desktop layer: window content, remote-peer presence, the
    /// sidebar wheel, the permission prompt bar, the toast stack and the
    /// visual-alert edge. First of four passes split out of `draw`, which
    /// was one 372-line function.
    fn draw_desktop_layer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_real_peers(scene, transform, text_renderer, theme, presence);

        // Sidebar wheel: above desktop content, below every overlay
        // (including scrims) — it's part of the desktop, so it dims with it.
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.draw(scene, transform, text_renderer, theme, presence);
        }

        self.draw_follow_chip(scene, transform, text_renderer, theme);
        // Below the overlay layer, so an open modal occludes it: act on
        // the modal first, then come back to the prompt.
        if let Some(prompt) = self.permission_prompt.as_mut() {
            let bar_top = self.rect.y0 + f64::from(self.sidebar_top_y);
            let bar_left = self.rect.x0 + f64::from(self.sidebar_top_y);
            let bar_right = self.rect.x1 - f64::from(self.sidebar_top_y);
            prompt.set_rect(Rect::new(
                bar_left,
                bar_top,
                bar_right,
                bar_top + PROMPT_HEIGHT,
            ));
            prompt.draw(scene, transform, text_renderer, theme, presence);
        }

        // Above desktop content, below modal overlays and the permission
        // prompt, so a prompt always wins focus. Top inset keeps toasts
        // off the clock/tray.
        if let Some(stack) = self.notification_stack.as_mut() {
            let viewport = Rect::new(
                self.rect.x0,
                self.rect.y0 + f64::from(self.sidebar_top_y),
                self.rect.x1,
                self.rect.y1,
            );
            stack.set_viewport(viewport);
            stack.draw(scene, transform, text_renderer, theme, presence);
        }

        // A visual alert is deliberately an edge treatment rather than a
        // full-screen luminance flash: it stays obvious without obscuring
        // content or creating an unnecessarily harsh photosensitivity risk.
        let visual_alert = self.chrome.as_ref().is_some_and(|chrome| {
            chrome
                .borrow()
                .visual_alert_active(std::time::Instant::now())
        });
        if visual_alert {
            let edge = Rect::new(
                self.rect.x0 + 4.0,
                self.rect.y0 + 4.0,
                self.rect.x1 - 4.0,
                self.rect.y1 - 4.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(7.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &edge,
            );
        }
    }

    /// The two legacy full-panel modals: the room overview panel and the
    /// workspace-settings sheet. Second of four passes split out of
    /// `draw`.
    fn draw_overview_and_ws_settings_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        self.draw_overview_overlay(scene, transform, text_renderer, theme, flags.overview);
        self.draw_ws_settings_overlay(scene, transform, text_renderer, theme, flags.ws_settings);
    }

    /// The room-overview panel: scrim, scale/translate curve, then the
    /// panel itself. First half of what was
    /// `draw_overview_and_ws_settings_overlays`, split out to keep that
    /// under `too_many_lines`.
    fn draw_overview_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        overview_open: bool,
    ) {
        let target = if overview_open { 1.0 } else { 0.0 };
        let t = self.tick_overview_transition(target);
        // The overview is a panel over the desk, not a place you travel
        // to: the desktop stays drawn and the scrim dims it as the panel
        // rises, the same enter/exit the ws-settings modal plays.
        let (overview_alpha, overview_dy, overview_scale) = overview_panel_curve(t);

        // Scale pivot is the scene centre, so the zoom anchors to the
        // viewport rather than the origin.
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let pivot_y = f64::midpoint(self.rect.y0, self.rect.y1);
        let scaled = |s: f64| -> Affine {
            Affine::translate((cx, pivot_y))
                .pre_scale(s)
                .pre_translate((-cx, -pivot_y).into())
        };

        if overview_alpha <= 0.0 {
            // Clear the cached hit rect so a stray click during an
            // in-flight close can't create a workspace.
            self.add_workspace_rect = Rect::ZERO;
        }
        if overview_alpha > 0.0 {
            draw_modal_scrim_at(scene, transform, self.rect, overview_alpha, theme);
            let panel_xform = if (overview_scale - 1.0).abs() > f64::EPSILON
                || overview_dy.abs() > f64::EPSILON
            {
                {
                    // `Affine`'s `Mul` composes transform matrices with plain
                    // float multiply-adds — it cannot overflow or panic —
                    // but kurbo has no non-operator way to compose an
                    // already-built `Affine` (`scaled(...)`) into a chain,
                    // only the canonical-kind `pre_*`/`then_*` combinators.
                    #[allow(
                        clippy::arithmetic_side_effects,
                        reason = "Affine * Affine is a panic-free float matrix multiply; no \
                                  combinator exists to compose an arbitrary already-built Affine"
                    )]
                    {
                        Affine::translate((0.0, overview_dy)) * scaled(overview_scale) * transform
                    }
                }
            } else {
                transform
            };
            if overview_alpha >= 0.999 {
                self.draw_overview(scene, panel_xform, text_renderer, theme);
            } else {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    overview_alpha,
                    transform,
                    &self.rect,
                );
                self.draw_overview(scene, panel_xform, text_renderer, theme);
                scene.pop_layer();
            }
        }
    }

    /// The workspace-settings modal: fly-in from the anchor button, the
    /// modal-open fade, and the settings sheet itself. Second half of
    /// what was `draw_overview_and_ws_settings_overlays`.
    fn draw_ws_settings_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        ws_settings_open: bool,
    ) {
        // `ws_settings_transition` plays the entrance in reverse on close
        // too, so the modal + scrim fade out instead of popping.
        if !ws_settings_open {
            // Clear cached rects so a click during the close fade can't
            // flip tabs, same as `add_workspace_rect` above.
            self.ws_settings_tab_pill_rects = [Rect::ZERO; 4];
            self.default_app_rects.clear();
            self.permissions_revoke_rects.clear();
            self.grant_composer_rects = None;
            self.member_control_rects.clear();
        }
        self.ws_settings_was_open = ws_settings_open;
        self.ws_settings_transition.set_open(
            ws_settings_open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(MODAL_OPEN_ANIM_MS)
            },
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        if self.ws_settings_transition.is_visible(now) {
            let alpha = self.ws_settings_transition.value(now);
            let in_flight = alpha < 1.0;
            let panel_rect = self.ws_settings_rect();

            if !theme.reduce_motion
                && in_flight
                && let Some(anchor) = self
                    .chrome
                    .as_ref()
                    .map(|chrome| chrome.borrow().workspace_settings_anchor)
                    .filter(|anchor| anchor.area() > 0.0)
            {
                let start = anchor.center();
                let end = Point::new(panel_rect.x0 + 28.0, panel_rect.y0 + 24.0);
                let t = f64::from(alpha);
                let x = start.x + (end.x - start.x) * t;
                let y = start.y + (end.y - start.y) * t;
                let pulse_alpha = (std::f32::consts::PI * alpha).sin().max(0.0);
                let mark =
                    RoundedRect::from_rect(Rect::new(x - 14.0, y - 2.0, x + 14.0, y + 2.0), 2.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[5].with_alpha_factor(pulse_alpha),
                    None,
                    &mark,
                );
            }
            if in_flight {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &self.rect,
                );
            }
            let travel = if theme.reduce_motion {
                0.0
            } else {
                48.0 * (1.0 - f64::from(alpha))
            };
            let panel_xform = transform.pre_translate((travel, 0.0).into());
            ui::surface::soft_shadow(
                scene,
                panel_xform,
                panel_rect,
                theme.tokens.colors.neutral[0],
                40.0,
                16.0,
                0.62 * alpha,
            );
            self.draw_ws_settings(scene, panel_xform, text_renderer, theme);
            if in_flight {
                scene.pop_layer();
            }
        }
    }

    /// The strip ribbon/rail and every anchored dropdown or edge sheet:
    /// quick settings, the power menu, collaborators, room activity and
    /// wireless projection. Third of four passes split out of `draw`.
    fn draw_dropdowns_and_sheets(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        // Both drawn under the popovers, hidden under the overview since
        // it isn't showing strips.
        if !self.overlay_flags().overview {
            let ribbon = self.strip_ribbon();
            ribbon.draw(scene, transform, theme, self.strip_ribbon_hover, 1.0);
            let rail = self.strip_rail();
            rail.draw(scene, transform, theme, self.strip_rail_hover, 1.0);
        }

        self.draw_quick_settings_dropdown(scene, transform, text_renderer, theme, flags);

        self.power_menu
            .draw(self.rect, scene, transform, text_renderer, theme);

        if flags.collaborators {
            let (rows, unavailable) = self.chrome.as_ref().map_or_else(
                || (Vec::new(), None),
                |chrome| {
                    let state = chrome.borrow();
                    (
                        collaborator_roster_rows(&state.tree()),
                        state.collab_unavailable.clone(),
                    )
                },
            );
            let may_invite = self.may_invite_here();
            self.collaborators_overlay.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &CollaboratorsModel {
                    host: self.rect,
                    rows: &rows,
                    unavailable: unavailable.as_deref(),
                    followed_id: self.followed_peer.as_deref(),
                    may_invite,
                },
                self.last_cursor,
            );
        } else {
            self.collaborators_overlay.clear();
        }

        // Above the dropdowns, below the palette (which must stay reachable).
        self.refresh_room_activity_awareness();
        if flags.room_activity && !self.room_activity_was_open {
            // Room may have changed while closed — drop stale derived rows.
            self.room_activity.invalidate();
            if let Some(chrome) = self.chrome.as_ref() {
                let (workspace, focus) = {
                    let mut state = chrome.borrow_mut();
                    let workspace = state.tree().active.raw();
                    (workspace, state.take_activity_focus_request())
                };
                self.room_activity.begin_view(workspace);
                if let Some(focus) = focus {
                    self.room_activity.apply_deep_link(focus);
                }
            }
        } else if !flags.room_activity
            && self.room_activity_was_open
            && let Some(chrome) = self.chrome.as_ref()
        {
            let workspace = chrome.borrow().tree().active.raw();
            self.room_activity.finish_view(workspace);
        }
        self.room_activity_was_open = flags.room_activity;
        self.room_activity_transition.set_open(
            flags.room_activity,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(MODAL_OPEN_ANIM_MS)
            },
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
            // Slides in from its anchored edge rather than scaling about
            // its centre — scaling a full-height panel reads as the whole
            // desktop breathing.
            let slide = (1.0 - f64::from(openness)) * (sheet.width() * 0.25);
            let sheet_xform = if slide > 0.0 {
                transform.pre_translate((slide, 0.0).into())
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

        self.draw_projection_sheet(scene, transform, text_renderer, theme, presence, flags);
    }

    /// The quick-settings dropdown: no scrim, so it reads as a dropdown
    /// rather than a modal. Split out of `draw_dropdowns_and_sheets`.
    fn draw_quick_settings_dropdown(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        if flags.quick_settings && !self.qs_was_open {
            // Rising edge: ask for fresh OS state without waiting — the
            // panel opens with last-known values and updates as readings
            // land. Wi-Fi/Bluetooth drill-in lists aren't requested here,
            // see `qs_refresh`.
            self.qs_refresh.kick_fast();
            self.qs_refresh.kick_firmware();
        }
        self.qs_was_open = flags.quick_settings;
        self.apply_quick_settings_readings();
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
            let dy = -10.0 * f64::from(1.0 - openness);
            let scale = 0.95 + 0.05 * f64::from(openness);
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
            let qs_xform = if dy.abs() > f64::EPSILON || (scale - 1.0).abs() > f64::EPSILON {
                let cx = qs_rect.center().x;
                let cy = qs_rect.y0;
                transform
                    .pre_translate((cx, cy + dy).into())
                    .pre_scale(scale)
                    .pre_translate((-cx, -cy).into())
            } else {
                transform
            };
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
    }

    /// Wireless projection is an OS-owned edge sheet. Discovery and
    /// protocol handshakes complete off-thread; polling only moves
    /// already-completed state into this frame. Split out of
    /// `draw_dropdowns_and_sheets`.
    fn draw_projection_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        let changed = self.projection_overlay.draw(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            self.rect,
            flags.projection,
        );
        if changed && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
    }

    /// The overlays that sit above dropdowns and sheets: the command
    /// palette, the peer-sync card, the file-picker overlay and the OSD.
    /// Last of four passes split out of `draw`.
    fn draw_top_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
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
            let palette_xform = if dy.abs() > f64::EPSILON || (scale - 1.0).abs() > f64::EPSILON {
                let cx = palette_rect.center().x;
                let cy = palette_rect.center().y;
                transform
                    .pre_translate((cx, cy + dy).into())
                    .pre_scale(scale)
                    .pre_translate((-cx, -cy).into())
            } else {
                transform
            };
            ui::surface::soft_shadow(
                scene,
                palette_xform,
                palette_rect,
                theme.tokens.colors.neutral[0],
                44.0,
                14.0,
                0.55 * alpha,
            );
            // A cloned theme with `surfaces.raised` pre-tinted, since
            // `CommandPalette` (in `quick-ui`) paints its own surface from
            // there and has no override plumbed in.
            let palette_theme = palette_surface_theme(theme, MODAL_SURFACE_ALPHA);
            self.palette.draw(
                scene,
                palette_xform,
                text_renderer,
                &palette_theme,
                presence,
            );
            self.draw_search_banner(scene, palette_xform, text_renderer, &palette_theme);
            if in_flight {
                scene.pop_layer();
            }
        }

        // Owns the keyboard while up, so it must sit above the palette
        // that opened it.
        self.draw_peer_sync(scene, transform, text_renderer, theme);

        // Above every other overlay, including the sidebar wheel — it's
        // genuinely modal (an app in another process is blocked on the
        // answer, and `handle_event` absorbs every event), and drawn by
        // the compositor rather than the requesting app so it can't be
        // drawn over, read, or dismissed by that app.
        self.file_picker_overlay
            .draw(self.rect, scene, transform, text_renderer, theme, presence);

        // Above everything, including modals — a physical key works
        // while a modal is up, so its readout must too.
        self.draw_osd(scene, transform, text_renderer, theme);
    }

    /// Paints an overlapping stack of member faces, right-aligned at
    /// `right_x`, centred on `cy`. `ground` must match the surface behind
    /// the stack — the separator ring is a hole punched in the face below.
    /// An absent member stays at reduced opacity rather than disappearing.
    #[allow(clippy::too_many_arguments)]
    fn draw_face_stack(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        right_x: f64,
        cy: f64,
        people: &[crate::shell::overview::RoomPerson],
        colors_for: &std::collections::HashMap<String, (u8, u8, u8)>,
        ground: Color,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let r = OVERVIEW_FACE_RADIUS_PX;
        let step = OVERVIEW_FACE_STEP_PX;
        let shown = people.len().min(OVERVIEW_FACES_MAX);
        let overflow = people.len().saturating_sub(shown);
        let slots = shown.saturating_add(usize::from(overflow > 0));
        if slots == 0 {
            return right_x;
        }
        let right = right_x - r;
        for (idx, person) in people.iter().take(shown).enumerate() {
            let cx = right - usize_to_f64_lossy(slots.saturating_sub(1).saturating_sub(idx)) * step;
            let (cr, cg, cb) = colors_for
                .get(&person.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&person.id));
            let fill = Color::from_rgb8(cr, cg, cb);
            // Mixed toward the ground rather than made translucent: the
            // monogram picks its ink from the fill it's drawn on, and on
            // a light panel an alpha-reduced face left white letters on
            // a pale disc.
            let fill = if person.present {
                fill
            } else {
                mix_toward(fill, ground, 1.0 - OVERVIEW_FACE_AWAY_ALPHA)
            };
            let initial = person
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string();
            ui::widget::Avatar::draw_ringed_at(
                scene,
                transform,
                Rect::new(cx - r, cy - r, cx + r, cy + r),
                &initial,
                fill,
                ground,
                2.0,
                theme,
                text_renderer,
            );
        }
        if overflow > 0 {
            let cx = right;
            ui::widget::Avatar::draw_overflow_at(
                scene,
                transform,
                Rect::new(cx - r, cy - r, cx + r, cy + r),
                overflow,
                c.surfaces.raised,
                ground,
                2.0,
                c.text_dim,
                text_renderer,
            );
        }
        right - usize_to_f64_lossy(slots.saturating_sub(1)) * step - r
    }

    /// Per-identity avatar colours for one room, matching that person's
    /// cursor and top-bar chip.
    fn overview_face_colors(&self, idx: usize) -> std::collections::HashMap<String, (u8, u8, u8)> {
        match self.chrome.as_ref() {
            Some(c) => {
                let state = c.borrow();
                let tree = state.tree();
                match tree.workspaces.get(idx) {
                    Some(ws) => crate::shell::state::roster_colors(&ws.collaborators),
                    None => std::collections::HashMap::default(),
                }
            }
            None => std::collections::HashMap::default(),
        }
    }

    /// One room row in the "go to" list.
    #[allow(clippy::too_many_arguments)]
    fn draw_overview_room_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        idx: usize,
        card: &crate::shell::overview::RoomCard,
        selected: bool,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let accent = colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let ground = panel_surface(theme, MODAL_SURFACE_ALPHA);
        let hover = self.last_cursor.is_some_and(|c| rect.contains(c));

        let rr = RoundedRect::from_rect(rect, OVERVIEW_ROW_RADIUS_PX);
        if selected {
            scene.fill(
                Fill::NonZero,
                transform,
                accent.with_alpha_factor(0.16),
                None,
                &rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                accent,
                None,
                &rr,
            );
        } else if hover {
            scene.fill(Fill::NonZero, transform, colors.surface_white_5, None, &rr);
        }

        let pad = 14.0_f64;
        let x0 = rect.x0 + pad;
        let x1 = rect.x1 - pad;

        // Faces first: the name elides against what's left rather than
        // running underneath them.
        let colors_for = self.overview_face_colors(idx);
        let faces_left = Self::draw_face_stack(
            scene,
            transform,
            text_renderer,
            theme,
            x1,
            rect.center().y,
            &card.people,
            &colors_for,
            ground,
        );
        let text_w = (faces_left - 12.0 - x0).max(40.0);

        let name_size = tokens.typography.scale.lg;
        let name = text_renderer.elide_to_width_weighted(
            &card.name,
            name_size,
            f64_to_f32_lossy(text_w),
            TextWeight::Semibold,
        );
        let (_, name_h) =
            text_renderer.measure_weighted(&name, name_size, None, TextWeight::Semibold);
        let sub_size = tokens.typography.scale.sm;
        // Who's in the room now outranks who could be; access is what
        // the line says when it's empty.
        let mut sub = crate::shell::overview::room_presence_label(card)
            .unwrap_or_else(|| crate::shell::overview::room_access_label(card));
        if let Some(shares) = crate::shell::overview::room_shares_label(card) {
            sub = format!("{sub} · {shares}");
        }
        let sub = text_renderer.elide_to_width(&sub, sub_size, f64_to_f32_lossy(text_w));
        let (_, sub_h) = text_renderer.measure(&sub, sub_size, None);
        let stack_h = f64::from(name_h) + 2.0 + f64::from(sub_h);
        let top = rect.center().y - stack_h / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &name,
            name_size,
            colors.text_primary,
            transform.pre_translate((x0, top).into()),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            &sub,
            sub_size,
            colors.text_dim,
            transform.pre_translate((x0, top + f64::from(name_h) + 2.0).into()),
            None,
        );
    }

    /// The workspace overview: a switcher for rooms, so it's a floating
    /// panel like the palette and workspace-settings modal, not a
    /// full-screen grid. Reading order follows the questions asked: where
    /// you are, what's shared, where else to go.
    fn draw_overview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.chrome.is_none() {
            return;
        }
        let cards = self.overview_cards();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Same fill/hairline/lift as the workspace-settings modal: one
        // floating-surface language across the shell's overlays.
        let panel = self.overview_panel_rect();
        let ground = panel_surface(theme, MODAL_SURFACE_ALPHA);
        ui::surface::elevation(scene, transform, panel, &tokens.elevation.e4);
        let card_rr = RoundedRect::from_rect(panel, f64::from(tokens.radii.lg));
        scene.fill(Fill::NonZero, transform, ground, None, &card_rr);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card_rr,
        );

        let content = self.overview_content_rect();
        let here_idx = self.overview_here_index();
        let here_rect = self.overview_here_rect();
        let caption_size = tokens.typography.scale.sm;

        self.draw_overview_here_section(
            scene,
            transform,
            text_renderer,
            theme,
            content,
            here_rect,
            here_idx,
            &cards,
        );
        self.draw_overview_shares_section(
            scene,
            transform,
            text_renderer,
            theme,
            content,
            here_idx,
            &cards,
        );

        // -- Where else you can go ----------------------------------------
        text_renderer.draw_text(
            scene,
            "Go to",
            caption_size,
            colors.text_tertiary,
            transform.pre_translate((content.x0, self.overview_list_caption_top() + 6.0).into()),
            None,
        );

        let band = Rect::new(
            content.x0 - 2.0,
            self.overview_list_top(),
            content.x1 + 2.0,
            content.y1,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &band,
        );

        self.draw_overview_go_to_list(scene, transform, text_renderer, theme, &cards, band);
        self.draw_overview_add_room_row(scene, transform, text_renderer, theme, cards.len());
        self.draw_overview_scroll_fade(scene, transform, theme, band);

        scene.pop_layer();
    }

    /// "Where you are": the current room's face stack, name, "You are
    /// here" chip and access label — or an honest empty state when
    /// nothing is marked current.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching every other \
                  draw_overview_* pane helper in this impl"
    )]
    fn draw_overview_here_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_rect: Rect,
        here_idx: Option<usize>,
        cards: &[crate::shell::overview::RoomCard],
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let name_size = Self::overview_header_font_size_for(tokens);

        if let Some((hi, card)) = here_idx.and_then(|hi| cards.get(hi).map(|card| (hi, card))) {
            self.draw_overview_here_room(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                here_rect,
                hi,
                card,
                name_size,
            );
        } else {
            // No room is marked current. Saying which one you are in
            // would be an invention, so the panel says what is true.
            text_renderer.draw_text_weighted(
                scene,
                "No room selected",
                name_size,
                colors.text_dim,
                transform.pre_translate((content.x0, here_rect.y0).into()),
                None,
                TextWeight::Bold,
            );
        }
    }

    /// The current room's selection highlight, face stack, name, "You
    /// are here" chip and access label.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching every other \
                  draw_overview_* pane helper in this impl"
    )]
    fn draw_overview_here_room(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_rect: Rect,
        hi: usize,
        card: &crate::shell::overview::RoomCard,
        name_size: f32,
    ) {
        use ui::widget::{Chip, ChipLead, ChipStyle};

        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let accent = colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let ground = panel_surface(theme, MODAL_SURFACE_ALPHA);

        self.draw_overview_here_selection(scene, transform, content, here_rect, hi, accent);

        // Faces claim the right edge; the name is elided against the
        // room they leave rather than sliding under them.
        let colors_for = self.overview_face_colors(hi);
        let faces_left = Self::draw_face_stack(
            scene,
            transform,
            text_renderer,
            theme,
            content.x1,
            here_rect.y0 + 22.0,
            &card.people,
            &colors_for,
            ground,
        );

        // "You are here" states which of these rooms you are in, in
        // the same tinted-chip language every other current-state
        // marker in the shell uses.
        let chip_h = 22.0_f64;
        let chip_w =
            Chip::natural_width_at("You are here", chip_h, ChipLead::None, theme, text_renderer);
        let name_avail = (faces_left - 16.0 - content.x0 - chip_w - 10.0).max(60.0);
        let name = text_renderer.elide_to_width_weighted(
            &card.name,
            name_size,
            f64_to_f32_lossy(name_avail),
            TextWeight::Bold,
        );
        let (name_w, name_h) =
            text_renderer.measure_weighted(&name, name_size, None, TextWeight::Bold);
        text_renderer.draw_text_weighted(
            scene,
            &name,
            name_size,
            colors.text_primary,
            transform.pre_translate((content.x0, here_rect.y0).into()),
            None,
            TextWeight::Bold,
        );
        let chip_x0 = content.x0 + f64::from(name_w) + 10.0;
        Chip::draw_at(
            scene,
            transform,
            Rect::new(
                chip_x0,
                here_rect.y0 + f64::from(name_h) / 2.0 - chip_h / 2.0,
                chip_x0 + chip_w,
                here_rect.y0 + f64::from(name_h) / 2.0 + chip_h / 2.0,
            ),
            "You are here",
            ChipLead::None,
            ChipStyle::tinted(accent),
            theme,
            text_renderer,
        );

        let access = crate::shell::overview::room_access_label(card);
        text_renderer.draw_text(
            scene,
            &access,
            tokens.typography.scale.base,
            colors.text_dim,
            transform.pre_translate((content.x0, here_rect.y0 + f64::from(name_h) + 6.0).into()),
            None,
        );
    }

    /// The keyboard target lands here on open, so the current room
    /// carries the same accent selection a list row does — without it
    /// the first arrow press would seem to come from nowhere.
    fn draw_overview_here_selection(
        &self,
        scene: &mut Scene,
        transform: Affine,
        content: Rect,
        here_rect: Rect,
        hi: usize,
        accent: Color,
    ) {
        if self.overview_selection != Some(hi) {
            return;
        }
        let sel = Rect::new(
            content.x0 - 10.0,
            here_rect.y0 - 10.0,
            content.x1 + 10.0,
            here_rect.y1 - 2.0,
        );
        let sel_rr = RoundedRect::from_rect(sel, OVERVIEW_ROW_RADIUS_PX);
        scene.fill(
            Fill::NonZero,
            transform,
            accent.with_alpha_factor(0.10),
            None,
            &sel_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            accent.with_alpha_factor(0.55),
            None,
            &sel_rr,
        );
    }

    /// "What leaves this room": the share list, or an honest zero-state
    /// when nothing in the current room is shared out of it.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching every other \
                  draw_overview_* pane helper in this impl"
    )]
    fn draw_overview_shares_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_idx: Option<usize>,
        cards: &[crate::shell::overview::RoomCard],
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let caption_size = tokens.typography.scale.sm;

        let shares_top = self.overview_shares_top();
        text_renderer.draw_text(
            scene,
            "Shared out of here",
            caption_size,
            colors.text_tertiary,
            transform.pre_translate((content.x0, shares_top + 6.0).into()),
            None,
        );
        let row_h = OVERVIEW_SHARE_ROW_HEIGHT_PX;
        let sy = shares_top + OVERVIEW_CAPTION_HEIGHT_PX;
        let empty_card = crate::shell::overview::RoomCard {
            id: crate::shell::state::WorkspaceId::default(),
            name: String::new(),
            is_current: false,
            people: Vec::new(),
            shares: Vec::new(),
        };
        let here_card = here_idx.and_then(|i| cards.get(i)).unwrap_or(&empty_card);
        if here_card.shares.is_empty() {
            Self::draw_overview_shares_empty(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                sy,
                row_h,
            );
        } else {
            Self::draw_overview_share_rows(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                here_card,
                sy,
                row_h,
            );
        }
    }

    /// An honest zero-state: "nothing" is itself the answer.
    fn draw_overview_shares_empty(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        sy: f64,
        row_h: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body_size = tokens.typography.scale.base;
        let glyph = 14.0_f64;
        let (_, body_h) = text_renderer.measure("Ag", body_size, None);
        let cy = sy + row_h / 2.0;
        let glyph_rect = Rect::new(
            content.x0,
            cy - glyph / 2.0,
            content.x0 + glyph,
            cy + glyph / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            glyph_rect,
            ui::icons::Icon::Shield,
            colors.text_tertiary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            "Nothing in this room is shared out of it",
            body_size,
            colors.text_dim,
            transform.pre_translate(
                (
                    content.x0 + glyph + 8.0,
                    sy + (row_h - f64::from(body_h)) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }

    /// One row per share (up to `OVERVIEW_SHARES_MAX_ROWS`), plus an
    /// "and N more" trailer when there are more than that.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching every other \
                  draw_overview_* pane helper in this impl"
    )]
    fn draw_overview_share_rows(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        here_card: &crate::shell::overview::RoomCard,
        mut sy: f64,
        row_h: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // One vertical rule for the section: glyph and text centre on
        // the row's middle line.
        let glyph = 14.0_f64;
        let shown = here_card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
        for share in here_card.shares.iter().take(shown) {
            Self::draw_overview_share_row(
                scene,
                transform,
                text_renderer,
                theme,
                content,
                share,
                sy,
                row_h,
                glyph,
            );
            sy += row_h;
        }
        let hidden = here_card.shares.len().saturating_sub(shown);
        if hidden > 0 {
            let more = format!("and {hidden} more");
            let (_, more_h) = text_renderer.measure(&more, tokens.typography.scale.sm, None);
            text_renderer.draw_text(
                scene,
                &more,
                tokens.typography.scale.sm,
                colors.text_tertiary,
                transform.pre_translate(
                    (
                        content.x0 + glyph + 8.0,
                        sy + (row_h - f64::from(more_h)) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
        }
    }

    /// One "shared out of here" row: an icon, who holds it, and what.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching every other \
                  draw_overview_* pane helper in this impl"
    )]
    fn draw_overview_share_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        share: &crate::shell::overview::RoomShare,
        sy: f64,
        row_h: f64,
        glyph: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body_size = tokens.typography.scale.base;
        let (_, body_h) = text_renderer.measure("Ag", body_size, None);
        let body_top = sy + (row_h - f64::from(body_h)) / 2.0;
        let cy = sy + row_h / 2.0;
        let glyph_rect = Rect::new(
            content.x0,
            cy - glyph / 2.0,
            content.x0 + glyph,
            cy + glyph / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            glyph_rect,
            ui::icons::Icon::ExternalLink,
            colors.text_tertiary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        // Who holds it sits right; what's shared elides against it.
        let with = match (share.with.first(), share.with.len()) {
            (Some(first), 1) => first.clone(),
            (Some(first), 2) => {
                format!(
                    "{first} and {}",
                    share.with.get(1).map_or("", String::as_str)
                )
            }
            (Some(first), n) => {
                format!("{first} and {} others", n.saturating_sub(1))
            }
            (None, _) => String::new(),
        };
        let with = if share.writable {
            format!("{with} · can edit")
        } else {
            with
        };
        let with_size = tokens.typography.scale.sm;
        let (with_w, with_h) = text_renderer.measure(&with, with_size, None);
        text_renderer.draw_text(
            scene,
            &with,
            with_size,
            colors.text_dim,
            transform.pre_translate(
                (
                    content.x1 - f64::from(with_w),
                    sy + (row_h - f64::from(with_h)) / 2.0,
                )
                    .into(),
            ),
            None,
        );
        let what_x = content.x0 + glyph + 8.0;
        let what_avail = (content.x1 - f64::from(with_w) - 12.0 - what_x).max(40.0);
        let what =
            text_renderer.elide_to_width(&share.what, body_size, f64_to_f32_lossy(what_avail));
        text_renderer.draw_text(
            scene,
            &what,
            body_size,
            colors.text_primary,
            transform.pre_translate((what_x, body_top).into()),
            None,
        );
    }

    /// "Where else you can go": one row per room, clipped to the
    /// scrollable band so off-screen rows are skipped rather than drawn.
    fn draw_overview_go_to_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cards: &[crate::shell::overview::RoomCard],
        band: Rect,
    ) {
        for (idx, card) in cards.iter().enumerate() {
            let Some(_slot) = self.overview_slot_of(idx) else {
                continue;
            };
            let rect = self.overview_row_rect(idx);
            if rect.y1 < band.y0 || rect.y0 > band.y1 {
                continue;
            }
            self.draw_overview_room_row(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                idx,
                card,
                self.overview_selection == Some(idx),
            );
        }
    }

    /// A peer row in the same list, not a banner: creating a room is
    /// one of the things you can go and do from here.
    fn draw_overview_add_room_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cards_len: usize,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let accent = colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX];

        let add_rect = self.overview_add_workspace_rect_for(cards_len);
        let add_hover = self.last_cursor.is_some_and(|c| add_rect.contains(c));
        let add_rr = RoundedRect::from_rect(add_rect, OVERVIEW_ROW_RADIUS_PX);
        if add_hover {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surface_white_5,
                None,
                &add_rr,
            );
        }
        let add_color = if add_hover { accent } else { colors.text_dim };
        let glyph = 18.0_f64;
        let gx = add_rect.x0 + 14.0;
        let gcy = add_rect.center().y;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(gx, gcy - glyph / 2.0, gx + glyph, gcy + glyph / 2.0),
            ui::icons::Icon::Plus,
            add_color,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let add_size = tokens.typography.scale.base;
        let (_, add_h) = text_renderer.measure("New room", add_size, None);
        text_renderer.draw_text(
            scene,
            "New room",
            add_size,
            add_color,
            transform.pre_translate((gx + glyph + 10.0, gcy - f64::from(add_h) / 2.0).into()),
            None,
        );
        self.add_workspace_rect = add_rect;
    }

    /// Overflow cue: a bottom fade over the scrollable band, only when
    /// there is more to scroll to.
    fn draw_overview_scroll_fade(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        band: Rect,
    ) {
        if self.overview_max_scroll_y() > 0.5 {
            let fade_y1 = band.y1;
            let fade_y0 = fade_y1 - OVERVIEW_SCROLL_FADE_PX;
            let ground = panel_surface(theme, MODAL_SURFACE_ALPHA);
            let grad = vello::peniko::Gradient::new_linear(
                Point::new(band.x0, fade_y0),
                Point::new(band.x0, fade_y1),
            )
            .with_extend(vello::peniko::Extend::Pad)
            .with_stops(
                &[
                    vello::peniko::ColorStop {
                        offset: 0.0,
                        color: ground.with_alpha_factor(0.0).into(),
                    },
                    vello::peniko::ColorStop {
                        offset: 1.0,
                        color: ground.into(),
                    },
                ][..],
            );
            scene.fill(
                Fill::NonZero,
                transform,
                &grad,
                None,
                &Rect::new(band.x0, fade_y0, band.x1, fade_y1),
            );
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "tree/state feed the per-tab pane bodies and the General tab's tail; \
                  cloning Workspace to narrow the guard further isn't worth the paint-path allocation"
    )]
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
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // `ws_settings_rect()` is the single source of truth for the
        // centered-modal geometry; hit-tests reach for the same helper
        // so a width bump can't desync the card vs. the clickable bounds.
        let rect = self.ws_settings_rect();
        Self::draw_ws_settings_card_background(scene, transform, theme, rect);

        // Borrowed only once the header's geometry (which needs no chrome
        // state at all) is already painted, and read again below for the
        // per-tab pane bodies and — on the General tab — the invite URL
        // and collaborator tile near the end of the function; those need
        // a live `&Workspace`, so cloning it to shrink the guard further
        // would add an allocation to a hot paint path for no real gain.
        let state = chrome.borrow();
        let tree = state.tree();

        let active_name = self.draw_ws_settings_header_text(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &state,
            &tree,
        );

        // Reads the typed `ws_settings_tab` field on chrome state, kept
        // orthogonal to the name-edit buffer.
        let active_tab = state.ws_settings_tab;
        self.draw_ws_settings_tab_strip(scene, transform, text_renderer, theme, rect, active_tab);

        // Members, Permissions and Apps each own their whole body and
        // paint it here; General is the fall-through into the rest of
        // this function.
        let fully_painted = self.draw_ws_settings_dispatch_tab(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            active_tab,
            &state,
            &tree,
        );
        if fully_painted {
            return;
        }

        // Unreachable today (every non-General arm returns `true` above),
        // but guards against a new `WsTab` variant falling through to
        // paint the General body over its pane.
        if !matches!(active_tab, WsTab::General) {
            return;
        }

        let body_y = rect.y0 + 168.0;
        let editing = self.draw_ws_settings_name_input(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &state,
            active_name.as_str(),
            body_y,
        );
        let invite_y = self.draw_ws_settings_invite_link(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &tree,
            body_y,
        );
        let collab_y = invite_y + 72.0;
        Self::draw_ws_settings_collaborators(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &tree,
            collab_y,
        );

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
            transform.pre_translate((rect.x1 - f64::from(hw) - 24.0, rect.y1 - 24.0).into()),
            None,
        );
    }

    /// The modal's flat translucent card. No blur primitive on the
    /// pinned Vello, so it sits over its scrim rather than glass.
    fn draw_ws_settings_card_background(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let card = RoundedRect::from_rect(rect, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            workspace_settings_surface(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );
    }

    /// Title plus the "Active workspace: …" line; returns the active
    /// workspace's name so the General tab's name input can fall back
    /// to it while nothing is being edited.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the \
                  name and role are read from; matching every other draw_ws_settings_* \
                  helper in this impl"
    )]
    fn draw_ws_settings_header_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) -> String {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (identity, close) =
            self.draw_ws_settings_identity_card_chrome(scene, transform, theme, rect);
        text_renderer.draw_text(
            scene,
            "CURRENT WORKSPACE",
            tokens.typography.scale.xs,
            colors.accent[4],
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 12.0).into()),
            None,
        );
        let active_name = tree
            .active_workspace()
            .map_or_else(|| "Workspace".into(), |w| w.name.clone());
        let tsize = tokens.typography.scale.xl;
        let shown_name = text_renderer.elide_to_width(
            &active_name,
            tsize,
            f64_to_f32_lossy(close.x0 - (identity.x0 + 76.0) - 12.0),
        );
        text_renderer.draw_text(
            scene,
            &shown_name,
            tsize,
            colors.text_primary,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 34.0).into()),
            None,
        );
        let active_role = tree
            .active_workspace()
            .and_then(|workspace| workspace.role_of(&state.local_user_id))
            .map_or("Guest", crate::shell::workspace_settings::role_label);
        let label = format!("{active_role} · Settings and policy stay with this workspace");
        text_renderer.draw_text(
            scene,
            &label,
            tokens.typography.scale.sm,
            colors.text_dim,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 58.0).into()),
            None,
        );
        active_name
    }

    /// The identity card's background, room-icon mark and close button —
    /// the purely-geometric chrome of [`Self::draw_ws_settings_header_text`],
    /// split out to keep that under `too_many_lines`. Returns the
    /// identity card rect and the close-button rect, since the caller's
    /// text placement is relative to both.
    fn draw_ws_settings_identity_card_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) -> (Rect, Rect) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let identity = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 14.0,
            rect.x1 - 16.0,
            rect.y0 + 94.0,
        );
        let identity_rr = RoundedRect::from_rect(identity, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &identity_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &identity_rr,
        );
        let mark = Rect::new(
            identity.x0 + 14.0,
            identity.y0 + 14.0,
            identity.x0 + 62.0,
            identity.y1 - 14.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.18),
            None,
            &RoundedRect::from_rect(mark, f64::from(tokens.radii.lg)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            mark.inset(14.0),
            ui::icons::Icon::LayoutGrid,
            colors.accent[4],
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let close = self.ws_settings_close_rect();
        let close_rr = RoundedRect::from_rect(close, f64::from(tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &close_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &close_rr,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            close.inset(8.0),
            ui::icons::Icon::X,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        (identity, close)
    }

    /// Tab strip. Every pill is a real destination: a host who has
    /// granted nothing yet still needs to reach the tab that grants.
    fn draw_ws_settings_tab_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let lsize = tokens.typography.scale.base;
        let tabs = WsTab::all();
        let nav = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 104.0,
            rect.x1 - 16.0,
            rect.y0 + 146.0,
        );
        let nav_rr = RoundedRect::from_rect(nav, f64::from(tokens.radii.lg));
        scene.fill(Fill::NonZero, transform, colors.neutral[0], None, &nav_rr);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard.with_alpha_factor(0.72),
            None,
            &nav_rr,
        );
        let segment_w = nav.width() / usize_to_f64_lossy(tabs.len());
        // Written into `self.ws_settings_tab_pill_rects` after the loop
        // so one source of geometry feeds draw and click-routing.
        let mut pill_rects = [Rect::ZERO; 4];
        for (i, tab) in tabs.iter().enumerate() {
            let active = *tab == active_tab;
            let label = if rect.width() < 680.0 {
                tab.compact_label()
            } else {
                tab.label()
            };
            let (w, h) = text_renderer.measure(label, lsize, None);
            let ix = usize_to_f64_lossy(i);
            let pill_rect = Rect::new(
                nav.x0 + segment_w * ix + 3.0,
                nav.y0 + 3.0,
                nav.x0 + segment_w * (ix + 1.0) - 3.0,
                nav.y1 - 3.0,
            );
            if let Some(slot) = pill_rects.get_mut(i) {
                *slot = pill_rect;
            }
            let pill = RoundedRect::from_rect(pill_rect, f64::from(tokens.radii.md));
            if active {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &pill,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &pill,
                );
            }
            let color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            let icon = match tab {
                WsTab::General => ui::icons::Icon::Settings2,
                WsTab::Members => ui::icons::Icon::Home,
                WsTab::Permissions => ui::icons::Icon::Shield,
                WsTab::Apps => ui::icons::Icon::LayoutGrid,
            };
            let icon_size = 15.0;
            let content_w = icon_size + 7.0 + f64::from(w);
            let content_x = pill_rect.center().x - content_w / 2.0;
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    content_x,
                    pill_rect.center().y - icon_size / 2.0,
                    content_x + icon_size,
                    pill_rect.center().y + icon_size / 2.0,
                ),
                icon,
                if active { colors.accent[4] } else { color },
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_text(
                scene,
                label,
                lsize,
                color,
                transform.pre_translate(
                    (
                        content_x + icon_size + 7.0,
                        pill_rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
        }
        self.ws_settings_tab_pill_rects = pill_rects;
    }

    /// Dispatches to the Members/Permissions/Apps tab body when one of
    /// those is active and fully paints it; returns `true` when it did,
    /// so the caller knows to stop. Returns `false` on `General`, whose
    /// clearing happens here too, but whose body the caller continues
    /// painting below.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  bodies read from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_dispatch_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) -> bool {
        match active_tab {
            WsTab::Members => {
                self.draw_ws_settings_members_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::Permissions => {
                self.draw_ws_settings_permissions_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::Apps => {
                self.draw_ws_settings_apps_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::General => {
                // Fall through to the General body below.
                self.permissions_revoke_rects.clear();
                self.grant_composer_rects = None;
                self.member_control_rects.clear();
                self.default_app_rects.clear();
                false
            }
        }
    }

    /// The Members tab body: roster rows, then the shared close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_members_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        self.permissions_revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        let body_rect = self.ws_settings_body_rect(rect);
        let is_owner = is_active_workspace_owner(state);
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
    }

    /// The Permissions tab body: the grant/revoke pane, then the shared
    /// close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_permissions_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        let body_rect = self.ws_settings_body_rect(rect);
        let is_owner = is_active_workspace_owner(state);
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
    }

    /// The Apps tab body: default-app rows, then the shared close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_apps_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        self.permissions_revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        self.default_app_rects.clear();
        let body_rect = self.ws_settings_body_rect(rect);
        let candidates = crate::shell::launcher::default_app_candidates();
        if let Some(ws) = tree.active_workspace() {
            self.default_app_rects.extend(
                default_app_rows(body_rect, &candidates)
                    .into_iter()
                    .map(|row| (row.resource, row.workspace_rect, row.user_rect)),
            );
            draw_default_apps_pane(
                scene,
                transform,
                body_rect,
                &DefaultAppsCtx {
                    ws,
                    local_user: &state.local_user_id,
                    candidates: &candidates,
                },
                text_renderer,
                theme,
            );
        }
        self.draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// Name input — edits fall through to `editing_workspace_name`.
    /// Returns whether the name is currently being edited, so the
    /// caller's footer hint can match.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state it's read \
                  from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_name_input(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        active_name: &str,
        _body_y: f64,
    ) -> bool {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = self.ws_settings_body_rect(rect);
        let identity_card = Rect::new(body.x0, body.y0 + 8.0, body.x1, body.y0 + 112.0);
        let identity_rr = RoundedRect::from_rect(identity_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &identity_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &identity_rr,
        );
        text_renderer.draw_text(
            scene,
            "Workspace identity",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate((identity_card.x0 + 16.0, identity_card.y0 + 13.0).into()),
            None,
        );
        text_renderer.draw_text(
            scene,
            "This name appears in the top bar, invites, and shared activity.",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((identity_card.x0 + 16.0, identity_card.y0 + 36.0).into()),
            None,
        );
        let input_rect = self.ws_settings_name_rect();
        let input = RoundedRect::from_rect(input_rect, f64::from(tokens.radii.sm));
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &input);
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
            .unwrap_or(active_name);
        let input_size = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            shown,
            input_size,
            colors.text_primary,
            transform.pre_translate((input_rect.x0 + 12.0, input_rect.y0 + 6.0).into()),
            None,
        );
        if editing {
            let (tw, _th) = text_renderer.measure(shown, input_size, None);
            let caret_x = input_rect.x0 + 12.0 + f64::from(tw) + 1.0;
            let caret = Rect::new(
                caret_x,
                input_rect.y0 + 6.0,
                caret_x + 2.0,
                input_rect.y0 + 6.0 + f64::from(input_size),
            );
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &caret);
        }
        editing
    }

    /// Sharing card with the workspace's stable invitation URI.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the tree the invite \
                  URL is derived from; matching every other draw_ws_settings_* helper \
                  in this impl"
    )]
    fn draw_ws_settings_invite_link(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        tree: &WorkspaceTree,
        _body_y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = self.ws_settings_body_rect(rect);
        let sharing_card =
            Self::draw_ws_settings_invite_link_card(scene, transform, theme, text_renderer, body);
        let invite_y = body.y0 + 202.0;
        let url_pill = Rect::new(
            sharing_card.x0 + 16.0,
            invite_y,
            sharing_card.x1 - 16.0 - 80.0,
            invite_y + 34.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            workspace_settings_surface(theme),
            None,
            &RoundedRect::from_rect(url_pill, f64::from(tokens.radii.sm)),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &RoundedRect::from_rect(url_pill, f64::from(tokens.radii.sm)),
        );
        // Mirrors WorkspaceSettings::invite_link's format so the URL is
        // stable per workspace id.
        let invite_url = tree.active_workspace().map_or_else(
            || "quickdesktop://invite/0".to_string(),
            |w| format!("quickdesktop://invite/{}", u64::from(w.id)),
        );
        text_renderer.draw_text(
            scene,
            &invite_url,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((url_pill.x0 + 12.0, url_pill.y0 + 9.0).into()),
            None,
        );
        let copy_btn = self.ws_settings_copy_rect();
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5],
            None,
            &RoundedRect::from_rect(
                copy_btn,
                ui::chrome::ChromeRole::Control.radius(theme, copy_btn.height()),
            ),
        );
        // "Copied" replaces the label briefly, or the button looks
        // indistinguishable from one that did nothing.
        let copy_label = if self
            .ws_invite_copied_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(1_600))
        {
            "Copied"
        } else {
            "Copy"
        };
        let (cw, _ch) = text_renderer.measure(copy_label, tokens.typography.scale.sm, None);
        text_renderer.draw_text(
            scene,
            copy_label,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate(
                (
                    copy_btn.x0 + (copy_btn.width() - f64::from(cw)) / 2.0,
                    copy_btn.y0 + 9.0,
                )
                    .into(),
            ),
            None,
        );
        invite_y
    }

    /// The sharing card's background, icon and title/subtitle — split out
    /// of [`Self::draw_ws_settings_invite_link`] to keep that under
    /// `too_many_lines`. Returns the card's rect, since the caller's URL
    /// pill and copy button are placed relative to it.
    fn draw_ws_settings_invite_link_card(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        body: Rect,
    ) -> Rect {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let sharing_card = Rect::new(body.x0, body.y0 + 128.0, body.x1, body.y0 + 252.0);
        let sharing_rr = RoundedRect::from_rect(sharing_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &sharing_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &sharing_rr,
        );
        let share_icon = Rect::new(
            sharing_card.x0 + 16.0,
            sharing_card.y0 + 14.0,
            sharing_card.x0 + 48.0,
            sharing_card.y0 + 46.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.16),
            None,
            &RoundedRect::from_rect(share_icon, f64::from(tokens.radii.md)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            share_icon.inset(8.0),
            ui::icons::Icon::Copy,
            colors.accent[4],
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            "Invite people",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate((sharing_card.x0 + 60.0, sharing_card.y0 + 13.0).into()),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Anyone with this link must still pass the workspace's secure join flow.",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((sharing_card.x0 + 60.0, sharing_card.y0 + 36.0).into()),
            None,
        );
        sharing_card
    }

    /// Live workspace summary. Detailed member management belongs to the
    /// Members tab; General stays an honest overview rather than inventing
    /// a placeholder collaborator.
    fn draw_ws_settings_collaborators(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        tree: &WorkspaceTree,
        _collab_y: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 160.0,
            rect.x1 - 16.0,
            rect.y1 - 16.0,
        );
        let summary_card = Rect::new(body.x0, body.y0 + 268.0, body.x1, body.y0 + 374.0);
        let summary_rr = RoundedRect::from_rect(summary_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &summary_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &summary_rr,
        );
        text_renderer.draw_text(
            scene,
            "At a glance",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate((summary_card.x0 + 16.0, summary_card.y0 + 13.0).into()),
            None,
        );
        let (members, grants, app_rules) = tree.active_workspace().map_or((0, 0, 0), |ws| {
            let members = ws.roster().len();
            let grants = ws
                .capabilities
                .iter()
                .filter(|cap| {
                    cap.effect == crate::shell::state::Effect::Grant && !cap.is_room_membership()
                })
                .count();
            (members, grants, ws.default_apps.policy.len())
        });
        let stats = [
            (members.to_string(), "members"),
            (grants.to_string(), "specific grants"),
            (app_rules.to_string(), "app rules"),
        ];
        let stat_gap = 8.0;
        let stat_y0 = summary_card.y0 + 44.0;
        let stat_w = (summary_card.width() - 32.0 - stat_gap * 2.0) / 3.0;
        for (i, (value, label)) in stats.iter().enumerate() {
            let x0 = summary_card.x0 + 16.0 + usize_to_f64_lossy(i) * (stat_w + stat_gap);
            let stat = Rect::new(x0, stat_y0, x0 + stat_w, summary_card.y1 - 14.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.base,
                None,
                &RoundedRect::from_rect(stat, f64::from(tokens.radii.md)),
            );
            text_renderer.draw_text(
                scene,
                value,
                tokens.typography.scale.lg,
                colors.text_primary,
                transform.pre_translate((stat.x0 + 12.0, stat.y0 + 8.0).into()),
                None,
            );
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((stat.x0 + 12.0, stat.y0 + 33.0).into()),
                None,
            );
        }
    }

    /// Persistent local follow-mode banner. It says both what is happening
    /// and how control returns to the local user; an explicit Stop button
    /// avoids making an unlabeled slab destructive.
    fn draw_follow_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(row) = self.followed_roster_row() else {
            self.followed_peer = None;
            self.follow_chip_rect = Rect::ZERO;
            self.follow_stop_rect = Rect::ZERO;
            return;
        };
        let label = format!("Following {}", row.label);
        let detail = "Your view moves with theirs";
        let width = 344.0_f64.min((self.rect.width() - 24.0).max(220.0));
        let center_x = self.rect.center().x;
        let y0 = self.rect.y0 + 54.0;
        self.follow_chip_rect = Rect::new(
            center_x - width / 2.0,
            y0,
            center_x + width / 2.0,
            y0 + 48.0,
        );
        self.follow_stop_rect = Rect::new(
            self.follow_chip_rect.x1 - 64.0,
            self.follow_chip_rect.y0 + 10.0,
            self.follow_chip_rect.x1 - 10.0,
            self.follow_chip_rect.y1 - 10.0,
        );
        let radius =
            ui::chrome::shell::ChromeRole::Selector.radius(theme, self.follow_chip_rect.height());
        let shape = RoundedRect::from_rect(self.follow_chip_rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            row.color.multiply_alpha(0.16),
            None,
            &shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            row.color,
            None,
            &shape,
        );
        let avatar_rect = Rect::new(
            self.follow_chip_rect.x0 + 10.0,
            self.follow_chip_rect.y0 + 10.0,
            self.follow_chip_rect.x0 + 38.0,
            self.follow_chip_rect.y1 - 10.0,
        );
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            avatar_rect,
            &row.initial.to_string(),
            row.color,
            theme,
            text_renderer,
        );
        let label_x = self.follow_chip_rect.x0 + 48.0;
        let label_budget = (self.follow_stop_rect.x0 - label_x - 8.0).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the follow banner is a few hundred logical pixels wide"
        )]
        let label = text_renderer.elide_to_width_weighted(
            &label,
            theme.tokens.typography.scale.sm,
            label_budget as f32,
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            &label,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            transform.pre_translate((label_x, y0 + 8.0).into()),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            detail,
            theme.tokens.typography.scale.xs,
            theme.tokens.colors.text_dim,
            transform.pre_translate((self.follow_chip_rect.x0 + 48.0, y0 + 27.0).into()),
            None,
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.follow_stop_rect, "Stop")
                .radius(f64::from(theme.tokens.radii.sm))
                .hovered(
                    self.last_cursor
                        .is_some_and(|p| self.follow_stop_rect.contains(p)),
                ),
        );
    }

    /// Paints every real peer's live pointer for the active workspace, via
    /// the same [`ui::presence::draw_peer_cursors`] call an app makes for
    /// peers inside its own window. One call for the whole set, since
    /// names have to be arranged against each other.
    fn draw_real_peers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let peers = self.workspace_peers(presence);
        ui::presence::draw_peer_presence(scene, transform, self.rect, text_renderer, theme, &peers);
    }

    /// Exactly the peers [`Self::draw_real_peers`] is about to paint:
    /// who, where, and what colour. Separated from the paint so the
    /// selection can be asserted on directly — "no peer arrived" and "a
    /// peer arrived and was filtered out" must be distinguishable in a
    /// test, not just an empty screen both ways.
    #[cfg(test)]
    pub(crate) fn visible_peers(&self, presence: &PresenceCtx) -> Vec<ui::presence::PeerCursor> {
        self.workspace_peers(presence)
            .into_iter()
            .filter(|peer| self.rect.contains(Point::new(peer.pos.0, peer.pos.1)))
            .collect()
    }

    fn workspace_peers(&self, presence: &PresenceCtx) -> Vec<ui::presence::PeerCursor> {
        let Some(chrome_rc) = self.chrome.as_ref() else {
            return Vec::new();
        };
        // Scope by the workspace's room, not local `WorkspaceId` — the room
        // is the durable identity apps address collaboration to (stamped in
        // as `QOS_WORKSPACE_ID`); the local id differs per machine.
        let room = {
            let chrome = chrome_rc.borrow();
            let tree = chrome.tree();
            tree.workspaces
                .iter()
                .find(|w| w.id == tree.active)
                .map(|w| w.room.to_string())
        };
        let Some(room) = room else {
            return Vec::new();
        };
        let key = PresenceKey::Workspace { id: room };
        let Some(peers) = presence.pointers.get(&key) else {
            return Vec::new();
        };

        // Settle colours over the whole room (including the local user)
        // before filtering to what's on screen, so every host computes
        // the same assignment and a person is one colour everywhere.
        let local = crate::shell::state::local_user_id();
        let mut room_ids: Vec<&str> = peers.iter().map(|(u, _)| u.0.as_str()).collect();
        room_ids.push(local.as_str());
        let reserved: Vec<(u8, u8, u8)> = presence.color_overrides.values().copied().collect();
        let assigned = ui::presence::assign_peer_colors(&room_ids, &reserved);

        peers
            .iter()
            .map(|(user, pos)| {
                // An explicit roster colour outranks anything computed.
                let (r, g, b) = presence
                    .color_overrides
                    .get(user)
                    .copied()
                    .or_else(|| assigned.get(user.0.as_str()).copied())
                    .unwrap_or_else(|| presence.color_for(user));
                ui::presence::PeerCursor {
                    pos: (f64::from(pos.0), f64::from(pos.1)),
                    // Falls back to the id when the roster has no name.
                    label: presence.display_name(user),
                    color: Color::from_rgb8(r, g, b),
                }
            })
            .collect()
    }

    fn draw_peer_sync(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.peer_sync_overlay
            .draw(self.rect, scene, transform, text_renderer, theme);
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

        self.draw_search_banner_scope_switch(
            scene,
            transform,
            text_renderer,
            theme,
            chip_text_size,
        );

        // Dynamic: one chip per kind any registered provider exposes, so
        // a new provider gets a chip for free. Empty selection means
        // "everything shows", stated by the leading "All" chip. Every
        // chip is painted regardless of width; see [`lay_out_chip_strip`].
        let kinds = self.search_registry.registered_kinds();
        let labels: Vec<&str> = std::iter::once("All")
            .chain(kinds.iter().map(super::search::ResourceKind::label))
            .collect();
        let natural: Vec<f64> = labels
            .iter()
            .map(|l| f64::from(text_renderer.measure(l, chip_text_size, None).0) + CHIP_PAD_PX)
            .collect();
        // The scope track's left edge bounds the chip strip. It stayed behind in
        // the other half when this block was split out, so recompute it here —
        // it is a pure geometry helper on `self`.
        let track = self.search_scope_track_rect();
        let strip = lay_out_chip_strip(
            &natural,
            band,
            track.x0 - 10.0,
            self.palette.host_filter_strip_rows(),
            self.palette.filter_strip_row_height(),
            control_h,
        );

        let mut chip_rects: Vec<Rect> = Vec::with_capacity(kinds.len());
        for (i, label) in labels.iter().enumerate() {
            let rect = strip.rects.get(i).copied().unwrap_or(Rect::ZERO);
            let is_active = match i {
                0 => self.active_kind_filter.is_empty(),
                _ => i
                    .checked_sub(1)
                    .and_then(|k| kinds.get(k))
                    .is_some_and(|kind| self.active_kind_filter.contains(kind)),
            };
            // Only a chip that had to be squeezed loses any of its label.
            let label = text_renderer.elide_to_width(
                label,
                chip_text_size,
                f64_to_f32_lossy((rect.width() - CHIP_PAD_PX / 2.0).max(1.0)),
            );
            draw_filter_chip(
                scene,
                transform,
                text_renderer,
                rect,
                &label,
                chip_text_size,
                if is_active { active_fill } else { idle_fill },
                if is_active { active_text } else { idle_text },
            );
            match i {
                0 => self.search_all_chip_rect = rect,
                _ => chip_rects.push(rect),
            }
        }
        self.search_chip_rects = chip_rects;

        // The next layout grows the band and moves result rows down;
        // until it runs, the chips stay squeezed into the band there is
        // but remain clickable.
        if strip.rows_needed != self.search_chip_rows {
            self.search_chip_rows = strip.rows_needed;
            if self.palette.set_host_filter_strip_rows(strip.rows_needed) {
                self.palette_layout_dirty = true;
            }
        }
    }

    /// The right-anchored scope switch (a pill track) in the search
    /// banner — separated from the chip strip below it since the two
    /// are laid out independently.
    fn draw_search_banner_scope_switch(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        chip_text_size: f32,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let active_fill = colors.accent[5];
        let active_text = colors.text_primary;
        let idle_text = colors.text_dim;

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
            let text = text_renderer.elide_to_width(
                &text,
                chip_text_size,
                f64_to_f32_lossy(seg.width() - 10.0),
            );
            let (tw, th) = text_renderer.measure(&text, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                &text,
                chip_text_size,
                if is_active { active_text } else { idle_text },
                transform.pre_translate(
                    (
                        f64::midpoint(seg.x0, seg.x1) - f64::from(tw) / 2.0,
                        f64::midpoint(seg.y0, seg.y1) - f64::from(th) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
        }
    }

    fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Sub-line falls back to "Not connected" when unpaired or off
        // (used by section 3, the toggle card grid, below).
        const NOT_CONNECTED: &str = "Not connected";
        let qs = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().quick_settings.clone())
            .unwrap_or_default();

        let rect = self.quick_settings_rect();
        Self::draw_quick_settings_card_background(scene, transform, theme, rect);

        // Root level renders the action row + sliders + toggles; Wifi/
        // Bluetooth levels replace the body with a header + list.
        if self.draw_quick_settings_drill_in(scene, transform, text_renderer, theme, &qs) {
            return;
        }

        self.draw_quick_settings_action_row(scene, transform, text_renderer, theme, &qs);
        self.draw_quick_settings_slider_card(scene, transform, theme, rect, &qs);
        self.draw_quick_settings_toggle_grid(
            scene,
            transform,
            text_renderer,
            theme,
            &qs,
            NOT_CONNECTED,
        );
        self.draw_quick_settings_power_profile(scene, transform, text_renderer, theme, &qs);
    }

    /// The quick-settings popover's elevated card: shadow, fill and
    /// border. Panels are elevated (HIG §9.1); a flat tint only reads as
    /// deliberate on dark, where the overlay tier is much darker than
    /// its ground — light and high-contrast had nothing lifting it.
    fn draw_quick_settings_card_background(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let card = RoundedRect::from_rect(
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
        );
        ui::chrome::shell::panel_shadow(
            scene,
            transform,
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
            theme,
            1.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            panel_surface(theme, POPOVER_SURFACE_ALPHA),
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
    }

    /// Root level renders the action row + sliders + toggles below;
    /// Wifi/Bluetooth/VPN/Notifications levels replace the whole body
    /// with a header + list and return `true` so the caller stops.
    fn draw_quick_settings_drill_in(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) -> bool {
        match qs.current_level() {
            crate::shell::quick_settings::QsLevel::Wifi
            | crate::shell::quick_settings::QsLevel::WifiDetails(_) => {
                if let Some(prompt) = qs.wifi_password.as_ref() {
                    self.draw_qs_wifi_password(scene, transform, text_renderer, theme, prompt);
                } else {
                    let panel = self.quick_settings_rect();
                    let cursor = self.last_cursor;
                    self.qs_wifi_pane.draw(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        panel,
                        cursor,
                        &qs.wifi_networks,
                    );
                }
                true
            }
            crate::shell::quick_settings::QsLevel::Bluetooth
            | crate::shell::quick_settings::QsLevel::BluetoothDetails(_) => {
                let panel = self.quick_settings_rect();
                let cursor = self.last_cursor;
                self.qs_bluetooth_pane.draw(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    panel,
                    cursor,
                    &qs.bt_devices,
                );
                true
            }
            crate::shell::quick_settings::QsLevel::Vpn => {
                let panel = self.quick_settings_rect();
                let cursor = self.last_cursor;
                self.qs_vpn_pane.draw(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    panel,
                    cursor,
                    &qs.vpns,
                );
                true
            }
            crate::shell::quick_settings::QsLevel::Notifications => {
                self.draw_qs_notification_history(scene, transform, text_renderer, theme);
                true
            }
            crate::shell::quick_settings::QsLevel::Root => false,
        }
    }

    /// Section 1: Action row (Settings / Privacy / Notifications / Power).
    fn draw_quick_settings_action_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let white_10 = colors.text_primary.with_alpha_factor(0.10);
        for i in 0..4 {
            let tile = self.quick_settings_action_geometry(i);
            // Control role: 12px — the radius scale deliberately omits
            // 16, so a literal 16 here would match nothing else on screen.
            let tile_rr = RoundedRect::from_rect(
                tile,
                ui::chrome::ChromeRole::Control.radius(theme, tile.height()),
            );
            // Power tile's red wash must expose its r/g/b signature so
            // the concept-parity spec test detects it.
            let bg = if i == 3 {
                colors.semantic.danger.with_alpha_factor(0.20)
            } else {
                white_10
            };
            scene.fill(Fill::NonZero, transform, bg, None, &tile_rr);

            let glyph_color = if i == 3 {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            let glyph_rect = Rect::new(
                tile.x0 + (tile.width() - 20.0) / 2.0,
                tile.y0 + 7.0,
                tile.x0 + f64::midpoint(tile.width(), 20.0),
                tile.y0 + 27.0,
            );
            let glyph = match i {
                0 => ui::icons::Icon::Settings2,
                1 => ui::icons::Icon::Camera,
                2 => ui::icons::Icon::Bell,
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
            if i == 1 {
                Self::draw_quick_settings_privacy_indicator(scene, transform, tile, colors, qs);
            }
            let action_label = match i {
                0 => "Settings",
                1 if !qs.privacy.active_media.is_empty() => "Stop access",
                1 => "Privacy",
                2 => "History",
                _ => "Power",
            };
            let (label_w, _) = text_renderer.measure_weighted(
                action_label,
                tokens.typography.scale.xs,
                None,
                TextWeight::Medium,
            );
            // `Affine`'s `Mul` composes transform matrices with plain
            // float multiply-adds — it cannot overflow or panic — but
            // `arithmetic_side_effects` can't see that for a
            // non-primitive `Mul` impl.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
            )]
            let label_xform = transform
                * Affine::translate((tile.center().x - f64::from(label_w) / 2.0, tile.y0 + 34.0));
            text_renderer.draw_text_weighted(
                scene,
                action_label,
                tokens.typography.scale.xs,
                glyph_color,
                label_xform,
                None,
                TextWeight::Medium,
            );
        }
    }

    /// The camera/mic privacy overlay on the Privacy action tile (index
    /// 1): a status dot, plus a mic-off glyph when muted or killed.
    fn draw_quick_settings_privacy_indicator(
        scene: &mut Scene,
        transform: Affine,
        tile: Rect,
        colors: &ui::tokens::colors::Colors,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let blocked = !matches!(
            qs.privacy.camera_kill,
            os_actions::KillState::Allowed | os_actions::KillState::Unavailable
        );
        scene.fill(
            Fill::NonZero,
            transform,
            if blocked {
                colors.semantic.danger
            } else if !qs.privacy.active_media.is_empty() {
                colors.semantic.success
            } else {
                colors.text_dim.with_alpha_factor(0.45)
            },
            None,
            &vello::kurbo::Circle::new(Point::new(tile.x1 - 12.0, tile.y0 + 12.0), 4.0),
        );
        if qs.privacy.microphone_muted.unwrap_or(false)
            || !matches!(
                qs.privacy.microphone_kill,
                os_actions::KillState::Allowed | os_actions::KillState::Unavailable
            )
        {
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(tile.x0 + 7.0, tile.y0 + 7.0, tile.x0 + 21.0, tile.y0 + 21.0),
                ui::icons::Icon::MicOff,
                colors.semantic.danger,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    /// Section 2: Slider card (Brightness / Volume).
    fn draw_quick_settings_slider_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
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

            // Solid white via `text_primary`, not indigo — the
            // concept-parity spec pins that delta to zero.
            let track_rr = RoundedRect::from_rect(bar, 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.10),
                None,
                &track_rr,
            );
            let fill_w = bar.width()
                * f64::from(slider_values.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0));
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
    }

    /// The 9 toggle-card rows (on/off, label, sub-line) section 3 draws
    /// one card per. Split out of the drawing loop so each is a plain
    /// data computation.
    fn quick_settings_toggle_rows<'a>(
        qs: &'a crate::shell::chrome_state::QuickSettingsState,
        not_connected: &'a str,
    ) -> [(bool, &'a str, &'a str); 9] {
        let wifi_sub = if qs.wifi {
            qs.wifi_networks
                .iter()
                .find(|n| n.connected)
                .map_or(not_connected, |n| n.ssid.as_str())
        } else {
            not_connected
        };
        let bt_sub = if qs.bluetooth {
            qs.bt_devices
                .iter()
                .find(|d| d.connected)
                .map_or(not_connected, |d| d.name.as_str())
        } else {
            not_connected
        };
        let wired_on = qs.wired.as_ref().is_some_and(|wired| wired.connected);
        let wired_sub = qs
            .wired
            .as_ref()
            .and_then(|wired| wired.device.as_deref())
            .unwrap_or(if wired_on {
                "Connected"
            } else {
                "Not connected"
            });
        let vpn_active = qs.vpns.iter().any(|vpn| vpn.active);
        let vpn_sub = qs
            .vpns
            .iter()
            .find(|vpn| vpn.active)
            .or_else(|| qs.vpns.first())
            .map_or("No profiles", |vpn| vpn.name.as_str());
        let mobile_sub = if qs.radios.wwan_available {
            if qs.radios.wwan_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
        } else {
            "Unavailable"
        };
        [
            (qs.wifi, "WLAN", wifi_sub),
            (qs.bluetooth, "Bluetooth", bt_sub),
            (wired_on, "LAN", wired_sub),
            (qs.radios.airplane_mode, "Airplane mode", "All radios"),
            (qs.radios.hotspot_active, "Hotspot", "qOS Hotspot"),
            (vpn_active, "VPN", vpn_sub),
            (qs.night_light, "Night Light", "Display comfort"),
            (false, "Displays", "Switch output"),
            (qs.radios.wwan_enabled, "Mobile data", mobile_sub),
        ]
    }

    /// Section 3: Toggle card grid. ON cards use an alpha wash rather
    /// than solid indigo-500 — the concept-parity spec pins the
    /// closed→open delta to zero.
    fn draw_quick_settings_toggle_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
        not_connected: &str,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let toggle_rows = Self::quick_settings_toggle_rows(qs, not_connected);
        for (i, (on, label, sub_line)) in toggle_rows.iter().enumerate() {
            let card = self.quick_settings_toggle_geometry(i);
            let card_rr = RoundedRect::from_rect(card, 16.0);
            // ON → indigo wash; OFF → slate-800 — the wash keeps the
            // spec test's solid-indigo signature clear.
            let bg = if *on {
                colors.accent[5].with_alpha_factor(0.20)
            } else {
                colors.neutral[2]
            };
            scene.fill(Fill::NonZero, transform, bg, None, &card_rr);

            // Wifi for slot 0, Bluetooth for slot 1.
            let glyph_box = Rect::new(
                card.x0 + 12.0,
                card.y0 + 12.0,
                card.x0 + 12.0 + 20.0,
                card.y0 + 12.0 + 20.0,
            );
            // Slot 0 (Wifi) and slot 4 (Hotspot) both fall to the
            // catch-all Wifi glyph below, same as anything past slot 7
            // (e.g. Mobile data).
            let icon = match i {
                1 => ui::icons::Icon::Bluetooth,
                2 => ui::icons::Icon::EthernetPort,
                3 => ui::icons::Icon::WifiOff,
                5 => ui::icons::Icon::Shield,
                6 => ui::icons::Icon::Sun,
                7 => ui::icons::Icon::Monitor,
                _ => ui::icons::Icon::Wifi,
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
            let chev_y = card.y0 + 22.0;
            let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_y), 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.60),
                None,
                &chev,
            );

            text_renderer.draw_text_weighted(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((card.x0 + 12.0, card.y1 - 36.0).into()),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                sub_line,
                tokens.typography.scale.xs,
                colors.text_primary.with_alpha_factor(0.70),
                transform.pre_translate((card.x0 + 12.0, card.y1 - 18.0).into()),
                None,
            );
        }
    }

    /// Section 4: Power Profile row.
    fn draw_quick_settings_power_profile(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let white_05 = colors.text_primary.with_alpha_factor(0.05);
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
        let profile_label = qs.power_profile.label();
        text_renderer.draw_text_weighted(
            scene,
            profile_label,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((pp.x0 + 44.0, pp.y0 + 8.0).into()),
            None,
            TextWeight::Semibold,
        );
        let battery_summary = if qs.battery.present {
            match qs.battery.health_percent() {
                Some(health) => format!("{}% battery · {health}% health", qs.battery.capacity),
                None => format!("{}% battery", qs.battery.capacity),
            }
        } else {
            "AC power".to_string()
        };
        let power_detail = match qs.firmware_updates {
            Some(count) if count > 0 => format!("{battery_summary} · {count} firmware update(s)"),
            _ => battery_summary,
        };
        text_renderer.draw_text(
            scene,
            &power_detail,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((pp.x0 + 44.0, pp.y0 + 26.0).into()),
            None,
        );
        let chev_cx = pp.x1 - 16.0;
        let chev_y = f64::midpoint(pp.y0, pp.y1);
        let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_y), 3.0);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &chev);
    }

    /// Draw the `QuickSettings` drill-in header — back-chevron on the left
    /// and a centred title. Used for both `WiFi` and Bluetooth panes; the
    /// back-chevron's hit rect is exposed via [`Self::qs_back_chevron_rect`].
    fn draw_qs_drill_in_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
    ) {
        let panel = self.quick_settings_rect();
        self.qs_aux_drill_in.layout(panel, 0);
        self.qs_aux_drill_in.draw_header(
            scene,
            transform,
            text_renderer,
            theme,
            panel,
            self.last_cursor,
            title,
        );
    }

    /// Centred dim empty-state message for a drill-in pane with no rows —
    /// without it the pane read as broken rather than "nothing here yet".
    fn draw_qs_drill_in_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        message: &str,
    ) {
        DrillInList::draw_empty(
            scene,
            transform,
            text_renderer,
            theme,
            self.quick_settings_rect(),
            message,
        );
    }

    fn qs_notification_clear_rect(&self) -> Rect {
        let r = self.quick_settings_rect();
        Rect::new(r.x1 - 88.0, r.y0 + 8.0, r.x1 - 12.0, r.y0 + 40.0)
    }

    /// Render the shell-owned, bounded session history. History deliberately
    /// lives in chrome rather than Settings: Settings owns delivery policy,
    /// while the shell is the only authority that actually received and
    /// retired these notifications.
    fn draw_qs_notification_history(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_qs_drill_in_header(scene, transform, text_renderer, theme, "Notifications");
        let rect = self.quick_settings_rect();
        let colors = &theme.tokens.colors;
        let history = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().notification_history().to_vec())
            .unwrap_or_default();
        if history.is_empty() {
            self.draw_qs_drill_in_empty(
                scene,
                transform,
                text_renderer,
                theme,
                "No recent notifications",
            );
            return;
        }

        let clear = self.qs_notification_clear_rect();
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &RoundedRect::from_rect(clear, 10.0),
        );
        let (clear_w, clear_h) =
            text_renderer.measure("Clear", theme.tokens.typography.scale.xs, None);
        text_renderer.draw_text(
            scene,
            "Clear",
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    clear.center().x - f64::from(clear_w) / 2.0,
                    clear.center().y - f64::from(clear_h) / 2.0,
                )
                    .into(),
            ),
            None,
        );

        let row_h = 70.0;
        let max_rows = f64_floor_to_usize_saturating((rect.height() - 64.0) / row_h).max(1);
        for (index, notification) in history.iter().rev().take(max_rows).enumerate() {
            let y0 = rect.y0 + 54.0 + usize_to_f64_lossy(index) * row_h;
            let row = Rect::new(rect.x0 + 12.0, y0, rect.x1 - 12.0, y0 + row_h - 6.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[2].with_alpha_factor(0.62),
                None,
                &RoundedRect::from_rect(row, 12.0),
            );
            let source = notification
                .app_name
                .as_deref()
                .or(notification.app_id.as_deref())
                .unwrap_or("System");
            text_renderer.draw_text(
                scene,
                source,
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((row.x0 + 12.0, row.y0 + 7.0).into()),
                Some(f64_to_f32_lossy(row.width() - 24.0)),
            );
            text_renderer.draw_text_weighted(
                scene,
                &notification.title,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((row.x0 + 12.0, row.y0 + 24.0).into()),
                Some(f64_to_f32_lossy(row.width() - 24.0)),
                TextWeight::Semibold,
            );
            if notification.show_preview && !notification.body.is_empty() {
                text_renderer.draw_text(
                    scene,
                    &notification.body,
                    theme.tokens.typography.scale.xs,
                    colors.text_dim,
                    transform.pre_translate((row.x0 + 12.0, row.y0 + 44.0).into()),
                    Some(f64_to_f32_lossy(row.width() - 24.0)),
                );
            }
        }
    }

    /// Hit rect for the back-chevron in the drill-in header. Click here
    /// pops `nav_stack`. Sized 32×32 with a small expansion for forgiving
    /// touch targets — the painted glyph is 24×24 inside.
    fn qs_back_chevron_rect(&self) -> Rect {
        let r = self.quick_settings_rect();
        Rect::new(r.x0 + 8.0, r.y0 + 8.0, r.x0 + 40.0, r.y0 + 40.0)
    }

    /// Toggle-card chevron hit rect — clicking here pushes
    /// `QsLevel::Wifi` (i==0) / `QsLevel::Bluetooth` (i==1) onto the nav
    /// stack instead of flipping the toggle bool.
    fn quick_settings_toggle_chevron_rect(&self, i: usize) -> Rect {
        let card = self.quick_settings_toggle_geometry(i);
        // Generous 32×32 hit rect around the painted 3px chevron.
        let cx = card.x1 - 16.0;
        let cy = card.y0 + 22.0;
        Rect::new(cx - 16.0, cy - 16.0, cx + 16.0, cy + 16.0)
    }

    /// Raise the passphrase prompt for `ssid`.
    pub fn begin_wifi_password(&mut self, ssid: &str) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        st.quick_settings.wifi_password =
            Some(crate::shell::quick_settings::WifiPasswordPrompt::new(ssid));
        st.bump();
    }

    /// Drop the prompt, clearing the passphrase with it. The buffer only
    /// exists while it is being typed; nothing keeps it afterwards.
    pub fn cancel_wifi_password(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.quick_settings.wifi_password.take().is_some() {
            st.bump();
        }
    }

    /// Attempt the join. `nmcli device wifi connect` blocks until it
    /// succeeds or fails, which can be seconds on a busy band — but a
    /// background join with no prompt left to report into would be
    /// worse. The prompt stays up on failure, carrying the reason.
    pub fn submit_wifi_password(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some((ssid, password)) = ({
            let st = chrome.borrow();
            st.quick_settings
                .wifi_password
                .as_ref()
                .filter(|p| p.can_submit())
                .map(|p| (p.ssid.clone(), p.password.clone()))
        }) else {
            return;
        };
        {
            let mut st = chrome.borrow_mut();
            if let Some(p) = st.quick_settings.wifi_password.as_mut() {
                p.connecting = true;
                p.error = None;
            }
            st.bump();
        }
        let result = self.qs_actions.connect_wifi(&ssid, Some(&password));
        let mut st = chrome.borrow_mut();
        match result {
            Ok(()) => {
                st.quick_settings.wifi_password = None;
                // Mark the joined row directly — the next poll is on the
                // popover's next open.
                for n in &mut st.quick_settings.wifi_networks {
                    n.connected = n.ssid == ssid;
                }
                st.quick_settings.wifi = true;
                st.push_notification(
                    format!("Connected to {ssid}"),
                    "This network is saved; the machine will rejoin it automatically.",
                );
            }
            Err(e) => {
                tracing::warn!(target: "qos::os_actions", op = "connect_wifi", error = %e, "wifi join failed");
                if let Some(p) = st.quick_settings.wifi_password.as_mut() {
                    p.connecting = false;
                    // Kept (not cleared) and selected, so a typo is a
                    // correction and a fresh keystroke replaces it.
                    p.selection.anchor = 0;
                    p.selection.cursor = p.password.len();
                    p.error = Some("Couldn't join. Check the password.".to_string());
                }
            }
        }
        st.bump();
    }

    /// Routes a key press into the passphrase field, if one is up.
    fn handle_wifi_password_key(
        &mut self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mods = crate::shell::text_entry::Mods::from_state(modifiers.state());
        let outcome = {
            let mut st = chrome.borrow_mut();
            let Some(prompt) = st.quick_settings.wifi_password.as_mut() else {
                return false;
            };
            let mut sel = prompt.selection;
            let out =
                crate::shell::text_entry::apply(logical_key, mods, &mut prompt.password, &mut sel);
            prompt.selection = sel;
            // Typing clears a rejection message — it was about the old value.
            if matches!(out, crate::shell::text_entry::Edit::Changed) {
                prompt.error = None;
            }
            out
        };
        match outcome {
            crate::shell::text_entry::Edit::Ignored => false,
            crate::shell::text_entry::Edit::ConsumedQuiet => true,
            crate::shell::text_entry::Edit::Changed => {
                chrome.borrow_mut().bump();
                true
            }
            crate::shell::text_entry::Edit::Submit => {
                self.submit_wifi_password();
                true
            }
            crate::shell::text_entry::Edit::Cancel => {
                self.cancel_wifi_password();
                true
            }
        }
    }

    /// Geometry of the passphrase prompt's controls. Derived rather than
    /// stored so the hit tests and the paint cannot drift apart — the
    /// same contract [`Self::qs_back_chevron_rect`] follows.
    fn qs_password_field_rect(&self) -> Rect {
        let r = self.quick_settings_rect();
        let y = r.y0 + 104.0;
        Rect::new(r.x0 + 16.0, y, r.x1 - 16.0, y + 40.0)
    }

    /// Cancel + Connect, anchored to the pane's foot rather than stacked
    /// under the field — the popover's height is set by the network list
    /// this pane replaces, and the prompt only needs about a third of it.
    fn qs_password_button_rects(&self) -> (Rect, Rect) {
        let r = self.quick_settings_rect();
        let h = 36.0_f64;
        let gap = 10.0_f64;
        let y = r.y1 - 16.0 - h;
        let w = (r.width() - 32.0 - gap) / 2.0;
        let cancel = Rect::new(r.x0 + 16.0, y, r.x0 + 16.0 + w, y + h);
        let connect = Rect::new(r.x1 - 16.0 - w, y, r.x1 - 16.0, y + h);
        (cancel, connect)
    }

    /// The "join this protected network" prompt.
    fn draw_qs_wifi_password(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        prompt: &crate::shell::quick_settings::WifiPasswordPrompt,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.quick_settings_rect();

        // The header names the network, because by the time the keyboard
        // has focus the list that named it is gone.
        self.draw_qs_drill_in_header(scene, transform, text_renderer, theme, &prompt.ssid);

        let sub_y = rect.y0 + 66.0;
        let lock_box = Rect::new(rect.x0 + 16.0, sub_y, rect.x0 + 32.0, sub_y + 16.0);
        ui::icons::draw_icon(
            scene,
            transform,
            lock_box,
            ui::icons::Icon::Shield,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text_weighted(
            scene,
            "This network is protected",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((lock_box.x1 + 8.0, sub_y + 1.0).into()),
            None,
            TextWeight::Medium,
        );

        let field =
            self.draw_qs_wifi_password_field(scene, transform, text_renderer, theme, prompt);
        Self::draw_qs_wifi_password_status(scene, transform, text_renderer, theme, prompt, field);
        self.draw_qs_wifi_password_buttons(scene, transform, text_renderer, theme, prompt);
    }

    /// Passphrase field: box, placeholder/typed text, and the caret whose
    /// `x` is proportional to how many characters precede it — the only
    /// thing a masked field can honestly show. Returns the field's rect
    /// so [`Self::draw_qs_wifi_password_status`] can anchor under it.
    fn draw_qs_wifi_password_field(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        prompt: &crate::shell::quick_settings::WifiPasswordPrompt,
    ) -> Rect {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let field = self.qs_password_field_rect();
        let field_shape = RoundedRect::from_rect(field, 10.0);
        // Opaque, not the translucent `surface_white_5` wash the
        // popover's other cards use — that let the app underneath show
        // through the passphrase field.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[1],
            None,
            &field_shape,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &field_shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            if prompt.error.is_some() {
                colors.semantic.danger
            } else {
                colors.border_focus
            },
            None,
            &field_shape,
        );

        let text_size = tokens.typography.scale.sm;
        let masked = crate::shell::text_entry::masked(&prompt.password);
        let text_x = field.x0 + 12.0;
        let (mask_w, mask_h) =
            text_renderer.measure_weighted(&masked, text_size, None, TextWeight::Medium);
        let text_y = field.y0 + (field.height() - f64::from(mask_h)) / 2.0;
        if masked.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                "Password",
                text_size,
                colors.text_dim.with_alpha_factor(0.6),
                transform.pre_translate((text_x, text_y).into()),
                None,
                TextWeight::Medium,
            );
        } else {
            text_renderer.draw_text_weighted(
                scene,
                &masked,
                text_size,
                colors.text_primary,
                transform.pre_translate((text_x, text_y).into()),
                None,
                TextWeight::Medium,
            );
        }
        // Caret. Masked text is uniform-width bullets, so the caret's x
        // is proportional to how many characters precede it — which is
        // the only thing a masked field can honestly show.
        let chars = prompt.password.chars().count();
        // Counts complete characters before the cursor byte offset instead
        // of slicing at it, so a cursor that (impossibly, but not provably
        // so to the compiler) landed mid-character can't panic.
        let caret_chars = prompt
            .password
            .char_indices()
            .filter(|&(i, _)| i < prompt.selection.cursor)
            .count();
        let caret_x = if chars == 0 {
            text_x
        } else {
            text_x
                + f64::from(mask_w) * (usize_to_f64_lossy(caret_chars) / usize_to_f64_lossy(chars))
        };
        scene.fill(
            Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &Rect::new(caret_x, field.y0 + 9.0, caret_x + 1.5, field.y1 - 9.0),
        );
        field
    }

    /// Error/connecting status line under the passphrase field.
    fn draw_qs_wifi_password_status(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        prompt: &crate::shell::quick_settings::WifiPasswordPrompt,
        field: Rect,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // A failed attempt says so where the eye already is, rather than
        // as a toast that would outlive the prompt.
        if let Some(err) = prompt.error.as_ref() {
            text_renderer.draw_text_weighted(
                scene,
                err,
                tokens.typography.scale.xs,
                colors.semantic.danger,
                transform.pre_translate((field.x0 + 2.0, field.y1 + 8.0).into()),
                None,
                TextWeight::Medium,
            );
        } else if prompt.connecting {
            text_renderer.draw_text_weighted(
                scene,
                "Connecting…",
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((field.x0 + 2.0, field.y1 + 8.0).into()),
                None,
                TextWeight::Medium,
            );
        }
    }

    /// Cancel + Connect buttons, anchored to the pane's foot.
    fn draw_qs_wifi_password_buttons(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        prompt: &crate::shell::quick_settings::WifiPasswordPrompt,
    ) {
        let (cancel, connect) = self.qs_password_button_rects();
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(cancel, "Cancel")
                .radius(10.0)
                .hovered(self.last_cursor.is_some_and(|point| cancel.contains(point))),
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(
                connect,
                if prompt.connecting {
                    "Connecting…"
                } else {
                    "Connect"
                },
            )
            .primary()
            .radius(10.0)
            .disabled(!prompt.can_submit())
            .hovered(
                self.last_cursor
                    .is_some_and(|point| connect.contains(point)),
            ),
        );
    }

    /// Collaborators dropdown: a peer-roster popover anchored under the
    /// Slider bar rect for the i-th quick-settings slider (0 = Brightness,
    /// 1 = Volume), with the label baseline `y`, so draw and hit-test
    /// share one source of truth.
    pub fn quick_settings_slider_geometry(&self, i: usize) -> (Rect, f64) {
        let r = self.quick_settings_rect();
        let card_y = r.y0 + 88.0;
        let row_y = card_y + 12.0 + usize_to_f64_lossy(i) * 36.0;
        let bar_y_mid = row_y + 12.0;
        let bar_x0 = r.x0 + 16.0 + 16.0 + 12.0;
        let bar_x1 = r.x1 - 16.0;
        let bar = Rect::new(bar_x0, bar_y_mid - 3.0, bar_x1, bar_y_mid + 3.0);
        (bar, row_y)
    }

    /// Toggle card rect for the i-th quick-settings toggle card
    /// The nine system tiles sit in a 3-column grid below
    /// the slider card.
    pub fn quick_settings_toggle_geometry(&self, i: usize) -> Rect {
        let r = self.quick_settings_rect();
        let grid_y = r.y0 + 176.0;
        let grid_h = 88.0;
        let inner_w = r.width() - 32.0; // 16 px outer padding each side.
        let card_w = (inner_w - 16.0) / 3.0; // 8 px gaps between cards.
        let row = i / 3;
        let col = i % 3;
        let x0 = r.x0 + 16.0 + usize_to_f64_lossy(col) * (card_w + 8.0);
        let y0 = grid_y + usize_to_f64_lossy(row) * (grid_h + 8.0);
        Rect::new(x0, y0, x0 + card_w, y0 + grid_h)
    }

    /// Action-row tile rect for the i-th tile (Settings, Privacy,
    /// Notifications, Power). The row sits at the top of the popover.
    pub(crate) fn quick_settings_action_geometry(&self, i: usize) -> Rect {
        let r = self.quick_settings_rect();
        let row_y = r.y0 + 16.0;
        let row_h = 56.0;
        let inner_w = r.width() - 32.0;
        let tile_w = (inner_w - 24.0) / 4.0; // 8 px gap × 3.
        let x0 = r.x0 + 16.0 + usize_to_f64_lossy(i) * (tile_w + 8.0);
        Rect::new(x0, row_y, x0 + tile_w, row_y + row_h)
    }

    /// Power Profile row rect — sits at the bottom of the popover.
    pub fn quick_settings_power_profile_geometry(&self) -> Rect {
        let r = self.quick_settings_rect();
        let row_y = r.y0 + 472.0;
        let row_h = 48.0;
        Rect::new(r.x0 + 16.0, row_y, r.x1 - 16.0, row_y + row_h)
    }
}

#[cfg(test)]
mod overview_panel_tests {
    //! The overview is a switcher for rooms: which one you're in, who's in
    //! it, what leaves it, how to get to another. These pin geometry/data,
    //! not pixels — every assertion runs against the same helpers
    //! `draw_overview` itself calls (`overview_row_rect`, `room_cards`).

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::overview::{room_access_label, room_cards, room_shares_label};
    use crate::shell::state::{CapOverride, Collaborator, Scope, Subject, Verb, WorkspaceTree};
    use ui::widget::Container;

    /// A host laid out over a desktop-sized viewport with the overview
    /// settled open, matching what the shots harness and the live binary
    /// both produce.
    fn host_with(tree: WorkspaceTree) -> ShellOverlayHost {
        let chrome = ChromeState::new(tree);
        chrome.borrow_mut().overlays.overview = true;
        let mut host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
        host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
        host.force_overview_settled(true);
        host
    }

    /// The panel is about several rooms, so it is built over the
    /// populated fixture. `bootstrap_default` is a single empty
    /// workspace — a first boot invents nothing — which is the right
    /// shape for the boot-path tests and no shape at all for a list.
    fn host() -> ShellOverlayHost {
        host_with(WorkspaceTree::bootstrap_demo())
    }

    fn ws_count(h: &ShellOverlayHost) -> usize {
        h.chrome
            .as_ref()
            .map_or(0, |c| c.borrow().tree().workspaces.len())
    }

    /// The room you are in is the block at the top, not a row in the
    /// list you could scroll away from. That is the difference between
    /// "here is where you are" and "here is an index of rooms".
    #[test]
    fn the_room_you_are_in_is_the_pinned_block_not_a_list_row() {
        let h = host();
        let here = h
            .overview_here_index()
            .expect("demo tree has an active room");
        assert_eq!(
            h.overview_slot_of(here),
            None,
            "the current room occupies no list slot"
        );
        assert_eq!(h.overview_row_rect(here), h.overview_here_rect());
        for i in 0..ws_count(&h) {
            if i == here {
                continue;
            }
            assert!(
                h.overview_slot_of(i).is_some(),
                "every other room is a row you can go to"
            );
        }
    }

    /// Rows advance down the panel and never overlap: two rooms sharing
    /// pixels would make one of them unclickable.
    #[test]
    fn room_rows_advance_downward_without_overlapping() {
        let h = host();
        let here = h.overview_here_index();
        let mut prev: Option<Rect> = None;
        for i in 0..ws_count(&h) {
            if Some(i) == here {
                continue;
            }
            let r = h.overview_row_rect(i);
            if let Some(p) = prev {
                assert!(
                    r.y0 >= p.y1,
                    "row {i} starts at {} but the row above ends at {}",
                    r.y0,
                    p.y1
                );
            }
            prev = Some(r);
        }
        let add = h.overview_add_workspace_rect_for(ws_count(&h));
        let last = prev.expect("demo tree has rooms other than the current one");
        assert!(
            add.y0 >= last.y1,
            "\"New room\" is the slot after the last room, not on top of it"
        );
        assert_eq!(
            add.width(),
            last.width(),
            "\"New room\" is a peer row in the same list, not a banner"
        );
    }

    /// Draw and hit-test share one geometry oracle: clicking the middle
    /// of a room's painted rect selects that room.
    #[test]
    fn clicking_a_room_rect_hits_that_room() {
        let h = host();
        for i in 0..ws_count(&h) {
            let r = h.overview_row_rect(i);
            assert_eq!(
                h.hit_test_overview(r.center()),
                Some(i),
                "the centre of room {i}'s rect must hit room {i}"
            );
        }
    }

    /// A click outside the panel is not a room. The panel is a surface
    /// floating over a scrim, so the scrim has to stay clickable-through
    /// to "dismiss" rather than silently activating an edge row.
    #[test]
    fn clicks_outside_the_panel_hit_no_room() {
        let h = host();
        let panel = h.overview_panel_rect();
        assert_eq!(
            h.hit_test_overview(Point::new(panel.x0 - 20.0, panel.center().y)),
            None
        );
        assert_eq!(
            h.hit_test_overview(Point::new(panel.center().x, panel.y0 - 20.0)),
            None
        );
    }

    /// The panel is sized to what it says and stays on screen.
    #[test]
    fn the_panel_is_content_sized_and_stays_inside_the_viewport() {
        let small = host_with(WorkspaceTree::bootstrap_default());
        let many = host();
        assert!(
            many.overview_panel_rect().height() > small.overview_panel_rect().height(),
            "more rooms means a taller panel — it is not a fixed page"
        );
        for h in [&small, &many] {
            let p = h.overview_panel_rect();
            assert!(
                p.x0 >= h.rect.x0 && p.x1 <= h.rect.x1 && p.y0 >= h.rect.y0 && p.y1 <= h.rect.y1,
                "panel {p:?} escapes the viewport {:?}",
                h.rect
            );
        }
    }

    /// A tree with more rooms than fit is capped by the viewport and
    /// scrolls instead of growing off-screen.
    #[test]
    fn a_long_room_list_is_capped_and_scrolls() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        for i in 0..40 {
            tree.workspaces.push(crate::shell::state::Workspace::new(
                WorkspaceId::new(),
                format!("Room {i}"),
            ));
        }
        let h = host_with(tree);
        let p = h.overview_panel_rect();
        assert!(
            p.y0 >= h.rect.y0 && p.y1 <= h.rect.y1,
            "panel stays on screen"
        );
        assert!(
            h.overview_max_scroll_y() > 0.0,
            "a list taller than its band has somewhere to scroll to"
        );
    }

    /// Everything fitting means nothing to scroll — a panel that could
    /// pan its content out of view for no reason would lose rooms.
    #[test]
    fn a_short_room_list_has_nothing_to_scroll() {
        let h = host();
        assert_eq!(h.overview_max_scroll_y(), 0.0);
    }

    /// Opening the panel points the selection at the room you are in, so
    /// the first arrow press steps from *here*.
    #[test]
    fn opening_seeds_the_selection_from_the_room_you_are_in() {
        let h = host();
        assert_eq!(h.overview_selection, h.overview_here_index());
        assert!(h.overview_selection.is_some());
    }

    /// Closing forgets it, so a later open does not resume pointing at
    /// wherever a previous visit stopped.
    #[test]
    fn closing_clears_the_selection() {
        let mut h = host();
        h.force_overview_settled(false);
        assert_eq!(h.overview_selection, None);
    }

    /// A room only you own reads as private, and says so on its row.
    #[test]
    fn a_private_room_says_so_on_its_row() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // What boot does before the session paints: a room you made is one you own.
        tree.adopt_ownerless_workspaces(&crate::shell::state::local_user_id());
        let cards = room_cards(&tree);
        assert_eq!(cards.len(), 1);
        assert_eq!(room_access_label(&cards[0]), "Private to you");
        assert_eq!(
            room_shares_label(&cards[0]),
            None,
            "a room that shares nothing shows no share count"
        );
    }

    /// The panel's people come from the room's capabilities, not from
    /// whoever this session has happened to hear from. Showing a face on
    /// a room states that person can reach it, which is a claim about
    /// access.
    #[test]
    fn faces_come_from_capabilities_not_from_the_collaborator_list() {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.workspaces[0].owner = None;
        tree.workspaces[0].capabilities.clear();
        tree.workspaces[0].collaborators = vec![Collaborator::new("ada", "Ada")];
        assert!(
            room_cards(&tree)[0].people.is_empty(),
            "a person with no capability here is not on this room"
        );

        let room = tree.workspaces[0].id.raw().to_string();
        tree.workspaces[0].capabilities.push(CapOverride::grant(
            Subject::person("ada"),
            Scope::Room(room),
            Verb::Write,
        ));
        let card = &room_cards(&tree)[0];
        assert_eq!(
            card.people
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ada"],
            "the member who holds a capability is exactly who the room shows"
        );
        assert_eq!(card.people[0].name, "Ada", "the roster supplies the name");
    }

    /// The panel never grows without bound as sharing does: past a few
    /// rows the rest collapse into a count, so the block stays a summary.
    #[test]
    fn the_share_list_collapses_rather_than_growing_without_bound() {
        let mut tree = WorkspaceTree::bootstrap_default();
        for i in 0..12 {
            tree.workspaces[0].capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::FsPath(format!("/var/workspace/doc{i}")),
                Verb::Read,
            ));
        }
        let card = &room_cards(&tree)[0];
        assert_eq!(card.shares.len(), 12, "the model keeps every share");
        assert!(
            ShellOverlayHost::overview_share_rows(card) <= OVERVIEW_SHARES_MAX_ROWS + 1,
            "the panel draws at most the cap plus one overflow line"
        );
    }

    /// A room that shares nothing still gets a line, because "nothing
    /// leaves this room" is an answer the screen exists to give.
    #[test]
    fn a_room_that_shares_nothing_still_gets_a_row() {
        let tree = WorkspaceTree::bootstrap_default();
        let card = &room_cards(&tree)[0];
        assert!(card.shares.is_empty());
        assert_eq!(ShellOverlayHost::overview_share_rows(card), 1);
    }
}

#[cfg(test)]
mod overview_transition_tests {
    use super::*;
    use ui::widget::Container;

    fn host() -> ShellOverlayHost {
        ShellOverlayHost::new(Box::new(Container::new()))
    }

    /// A window at the strip's max width still has to stop short of the
    /// display edge, or its border/corners/shadow — which live in the last
    /// few pixels — get clipped by the bezel. Mirrors the gutter the near
    /// edge already reserves beside the wheel disc.
    #[test]
    fn a_full_width_window_stops_short_of_both_screen_edges() {
        let mut h = host().with_sidebar(Box::new(Container::new()));
        h.set_output_geometry(&[OutputGeometry {
            x: 0.0,
            width: 1600.0,
            height: 900.0,
        }]);

        let spans = h.strip_output_spans();
        assert_eq!(spans.len(), 1, "one output in, one span out");
        let span = spans[0];

        // Where the strip starts placing groups: past the wheel slot.
        let leading = f64::from(sidebar_width_for_viewport(900.0)) + WHEEL_TO_STRIP_GAP_PX;
        // The widest a group can be, and therefore where the widest one ends.
        let widest = f64::from(ui::widget::WIDTH_MAX_PERMILLE) / 1000.0 * span.usable;
        let right_edge = leading + widest;

        assert!(
            right_edge < span.x + span.width,
            "the widest window reaches x={right_edge} on a {}px display — it \
             has to stop short of the edge or its border and corners fall off \
             the screen",
            span.width,
        );
        // And the gap it leaves should match the one on the other side, so
        // the window looks centred in its canvas rather than shoved right.
        let trailing = span.x + span.width - right_edge;
        assert!(
            (trailing - WHEEL_TO_STRIP_GAP_PX).abs() < 1.0,
            "trailing gutter is {trailing}px against a {WHEEL_TO_STRIP_GAP_PX}px \
             gutter beside the wheel; they should read as the same margin",
        );
    }

    /// At rest the panel is either absent or fully arrived — no
    /// half-drawn surface sits on screen when nothing is animating.
    #[test]
    fn the_panel_curve_is_settled_at_both_ends() {
        let (alpha, dy, scale) = overview_panel_curve(0.0);
        assert_eq!(alpha, 0.0);
        assert_eq!(dy, OVERVIEW_PANEL_RISE_PX);
        assert_eq!(scale, OVERVIEW_PANEL_START_SCALE);

        let (alpha, dy, scale) = overview_panel_curve(1.0);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    /// Mid-flight the panel is partly transparent, still low, and still
    /// small — all three moving together, so it reads as one object
    /// arriving rather than three properties animating.
    #[test]
    fn the_panel_curve_moves_alpha_rise_and_scale_together() {
        let (alpha, dy, scale) = overview_panel_curve(0.5);
        assert!(alpha > 0.0 && alpha < 1.0, "alpha mid-flight: {alpha}");
        assert!(
            dy > 0.0 && dy < OVERVIEW_PANEL_RISE_PX,
            "rise mid-flight: {dy}"
        );
        assert!(
            scale > OVERVIEW_PANEL_START_SCALE && scale < 1.0,
            "scale mid-flight: {scale}"
        );
    }

    /// The curve is monotonic, so the panel never backs up on its way in
    /// or stutters on its way out.
    #[test]
    fn the_panel_curve_never_reverses() {
        let mut prev = overview_panel_curve(0.0);
        for step in 1_i16..=20 {
            let cur = overview_panel_curve(f32::from(step) / 20.0);
            assert!(cur.0 >= prev.0, "alpha went backwards at {step}");
            assert!(cur.1 <= prev.1, "rise went backwards at {step}");
            assert!(cur.2 >= prev.2, "scale went backwards at {step}");
            prev = cur;
        }
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
        // Past the full duration, value settles exactly at the target.
        std::thread::sleep(std::time::Duration::from_millis(
            OVERVIEW_TRANSITION_MS + 30,
        ));
        let t = h.tick_overview_transition(1.0);
        assert!((t - 1.0).abs() < f32::EPSILON);
        assert!(h.transition_started.is_none(), "transition should clear");
    }

    #[test]
    fn mid_flight_re_toggle_picks_up_from_current_t() {
        // Flip target mid-fade; `transition_from` should capture the
        // current `overview_t`, not 0, so the reverse starts from there.
        let mut h = host();
        h.tick_overview_transition(1.0);
        std::thread::sleep(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2));
        let t_mid = h.tick_overview_transition(1.0);
        assert!(
            t_mid > 0.0 && t_mid < 1.0,
            "mid-flight t should be between 0 and 1, got {t_mid}"
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
mod wifi_password_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::quick_settings::{QsLevel, WifiNetwork};
    use crate::shell::state::WorkspaceTree;
    use std::cell::RefCell;
    use ui::widget::Container;

    /// Records the join attempts and can be told to reject them, so the
    /// failure branch is exercised without a radio.
    #[derive(Default)]
    struct JoinRecorder {
        attempts: RefCell<Vec<(String, Option<String>)>>,
        reject: bool,
    }

    impl crate::shell::qs_actions::QsActions for JoinRecorder {
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.attempts
                .borrow_mut()
                .push((ssid.to_string(), password.map(str::to_string)));
            if self.reject {
                Err("Secrets were required, but not provided".to_string())
            } else {
                Ok(())
            }
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }

    /// A host sitting on the Wi-Fi drill-in with one protected network
    /// and one open one visible.
    fn host_on_wifi_pane(
        reject: bool,
    ) -> (
        ShellOverlayHost,
        crate::shell::chrome_state::Chrome,
        std::rc::Rc<JoinRecorder>,
    ) {
        struct Shim(std::rc::Rc<JoinRecorder>);
        impl crate::shell::qs_actions::QsActions for Shim {
            fn set_brightness(&self, v: f32) -> Result<(), String> {
                self.0.set_brightness(v)
            }
            fn set_volume(&self, v: f32) -> Result<(), String> {
                self.0.set_volume(v)
            }
            fn set_wifi(&self, v: bool) -> Result<(), String> {
                self.0.set_wifi(v)
            }
            fn set_bluetooth(&self, v: bool) -> Result<(), String> {
                self.0.set_bluetooth(v)
            }
            fn connect_wifi(&self, s: &str, p: Option<&str>) -> Result<(), String> {
                self.0.connect_wifi(s, p)
            }
            fn set_power_profile(&self, p: os_actions::PowerProfile) -> Result<(), String> {
                self.0.set_power_profile(p)
            }
        }

        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let rec = std::rc::Rc::new(JoinRecorder {
            attempts: RefCell::new(Vec::new()),
            reject,
        });
        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.nav_stack = vec![QsLevel::Root, QsLevel::Wifi];
            st.quick_settings.wifi_networks = vec![
                WifiNetwork {
                    ssid: "Cafe WiFi".into(),
                    signal: 3,
                    secure: true,
                    connected: false,
                },
                WifiNetwork {
                    ssid: "Airport Free".into(),
                    signal: 2,
                    secure: false,
                    connected: false,
                },
            ];
        }
        let host = ShellOverlayHost::new(Box::new(Container::new()))
            .with_chrome(chrome.clone())
            .with_qs_actions(Box::new(Shim(rec.clone())));
        (host, chrome, rec)
    }

    fn press(h: &mut ShellOverlayHost, key: &Key) {
        let mods = winit::event::Modifiers::default();
        h.handle_wifi_password_key(key, mods);
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn a_protected_network_asks_for_a_password_instead_of_failing_silently() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        let prompt = chrome.borrow().quick_settings.wifi_password.clone();
        let prompt = prompt.expect("a prompt is up");
        assert_eq!(prompt.ssid, "Cafe WiFi");
        assert!(prompt.password.is_empty());
        assert!(
            rec.attempts.borrow().is_empty(),
            "nothing is attempted until there is a password to attempt with"
        );
        assert!(
            !prompt.can_submit(),
            "Connect stays inert until something is typed"
        );
    }

    #[test]
    fn typing_reaches_the_buffer_and_never_the_palette() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["h", "u", "n", "k"] {
            press(&mut h, &ch(c));
        }
        assert_eq!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .password,
            "hunk"
        );
        assert!(
            !h.palette_visible,
            "a 'k' in a password must be a character, not the search chord"
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow feeds both assertions right after; ending it earlier would mean \
                  cloning test fixture data for no real gain"
    )]
    fn enter_submits_the_typed_password_to_the_join() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["p", "w", "1"] {
            press(&mut h, &ch(c));
        }
        press(&mut h, &Key::Named(NamedKey::Enter));
        assert_eq!(
            rec.attempts.borrow().as_slice(),
            [("Cafe WiFi".to_string(), Some("pw1".to_string()))],
            "the passphrase reaches nmcli — this is the whole point of the field"
        );
        let st = chrome.borrow();
        assert!(
            st.quick_settings.wifi_password.is_none(),
            "a successful join closes the prompt"
        );
        assert!(
            st.quick_settings
                .wifi_networks
                .iter()
                .any(|n| n.ssid == "Cafe WiFi" && n.connected),
            "the list the user lands back on shows the network they just joined"
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow feeds a derived reference (`p`) used through the assertions right \
                  after; ending it earlier would mean cloning test fixture data for no real gain"
    )]
    fn a_rejected_password_keeps_the_prompt_up_and_says_why() {
        let (mut h, chrome, _) = host_on_wifi_pane(true);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["b", "a", "d"] {
            press(&mut h, &ch(c));
        }
        press(&mut h, &Key::Named(NamedKey::Enter));
        let st = chrome.borrow();
        let p = st
            .quick_settings
            .wifi_password
            .as_ref()
            .expect("the prompt survives a rejection");
        assert!(!p.connecting, "the attempt is over");
        assert!(p.error.is_some(), "and the reason is on screen");
        assert_eq!(p.password, "bad", "a typo is a correction, not a retype");
        assert_eq!(
            (p.selection.anchor, p.selection.cursor),
            (0, 3),
            "and it is selected, so the next keystroke replaces it"
        );
    }

    #[test]
    fn typing_after_a_rejection_clears_the_stale_error() {
        let (mut h, chrome, _) = host_on_wifi_pane(true);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("x"));
        press(&mut h, &Key::Named(NamedKey::Enter));
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        press(&mut h, &ch("y"));
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .error
                .is_none(),
            "the message was about the old value"
        );
    }

    #[test]
    fn escape_backs_out_and_takes_the_password_with_it() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("s"));
        press(&mut h, &Key::Named(NamedKey::Escape));
        assert!(chrome.borrow().quick_settings.wifi_password.is_none());
        assert!(rec.attempts.borrow().is_empty());
    }

    #[test]
    fn closing_the_popover_discards_a_half_typed_password() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("s"));
        chrome.borrow_mut().overlays.quick_settings = false;
        h.clear_stale_wifi_password();
        assert!(
            chrome.borrow().quick_settings.wifi_password.is_none(),
            "a password buffer must not outlive the window it was typed into"
        );
    }

    #[test]
    fn an_empty_password_is_never_submitted() {
        let (mut h, _chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &Key::Named(NamedKey::Enter));
        assert!(
            rec.attempts.borrow().is_empty(),
            "every WPA network rejects an empty passphrase; asking wastes a round trip"
        );
    }

    #[test]
    fn no_prompt_means_keys_fall_through_to_the_shell() {
        let (mut h, _chrome, _) = host_on_wifi_pane(false);
        let mods = winit::event::Modifiers::default();
        assert!(
            !h.handle_wifi_password_key(&ch("k"), mods),
            "with no field up, the shell's own chords must still work"
        );
    }
}

#[cfg(test)]
mod palette_dispatch_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use ui::widget::Container;

    fn host_with_chrome() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
        let tree = WorkspaceTree::bootstrap_demo();
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
    /// case a stale row lingers across a flag toggle.
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
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new()))
            .with_chrome(chrome)
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
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new()))
            .with_chrome(chrome)
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
        // No entry-count API, so proven indirectly: invoking a peer-sync id
        // twice must still dispatch exactly one action, no panic.
        let mut h = host.with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        assert!(h.dispatch_palette_command("peer.invite"));
        assert_eq!(h.take_peer_sync_actions().len(), 1);
    }

    // Proves both halves of the invite-token crossing (mint → clipboard
    // → paste) against the process clipboard.

    use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

    fn key_event(key: Key) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
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

    const SAMPLE_TOKEN: &str = "eyJ3b3Jrc3BhY2VfaWQiOjEsInJvb21faWQiOiJhYmMifQ";

    #[test]
    #[serial_test::serial(clipboard)]
    fn ctrl_c_puts_the_minted_invite_token_on_the_clipboard() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("c".into())), ctrl),
            "the card owns Ctrl+C while it is showing a token"
        );

        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some(SAMPLE_TOKEN),
            "the token has to reach the clipboard — that is the only way it \
             gets to the other machine without being retyped"
        );
        assert!(
            h.peer_sync_snapshot()
                .status
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("copied"),
            "a copy the user cannot see happen is a copy they will do twice"
        );
    }

    /// One gesture, one outcome: a token for that person, on the clipboard,
    /// with the card saying so. Splitting mint from Copy is how the copy
    /// gets skipped — nobody thinks to do the second half.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_composer_mints_for_the_name_it_was_given_and_copies() {
        let (mut h, chrome) = host_with_chrome();
        h.set_peer_sync_bridge(std::rc::Rc::new(|req| match req {
            PeerSyncRequest::IssueInviteFor(name) => Ok(format!("token-for-{name}")),
            PeerSyncRequest::IssueInvite => Ok("token-for-nobody".to_string()),
            PeerSyncRequest::AcceptInvite(_) => Err("not here".to_string()),
        }));
        chrome
            .borrow_mut()
            .push_peer_sync_request(PeerSyncAction::PromptInvite);
        h.pump_peer_sync();
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some(""),
            "the composer opened with an empty field"
        );

        let none = Modifiers::default();
        for c in ["X", "a", "v", "e", "r"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), none);
        }
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some("bob"),
            "the field takes the name — the same keys that type a token"
        );

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), none),
            "Enter belongs to the composer while it is up"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_token.as_deref(),
            Some("token-for-bob"),
            "the name reached the request, so it can reach the room document"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_for.as_deref(),
            Some("bob"),
            "and the card knows whose invite it is holding"
        );
        assert!(
            h.peer_sync_snapshot().invite_name.is_none(),
            "the question is answered, so the card has moved on from asking it"
        );
        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some("token-for-bob"),
            "one gesture, one outcome: the token is on the clipboard"
        );
    }

    /// An empty field is the question still unanswered. Minting anyway
    /// is exactly what left the host looking at a number.
    #[test]
    fn the_composer_refuses_to_mint_for_nobody() {
        let (mut h, chrome) = host_with_chrome();
        h.set_peer_sync_bridge(std::rc::Rc::new(|_| Ok("a-token".to_string())));
        chrome
            .borrow_mut()
            .push_peer_sync_request(PeerSyncAction::PromptInvite);
        h.pump_peer_sync();

        let none = Modifiers::default();
        h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), none);

        assert!(
            h.peer_sync_snapshot().invite_token.is_none(),
            "nothing was minted for nobody"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some(""),
            "and the card is still asking"
        );
    }

    #[test]
    #[serial_test::serial(clipboard)]
    fn ctrl_v_fills_the_connect_prompt_from_the_clipboard() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl),
            "the prompt owns Ctrl+V"
        );

        assert_eq!(
            h.peer_sync_snapshot().connect_input.as_deref(),
            Some(SAMPLE_TOKEN),
            "pasting is the receiving half; without it the token still has \
             to be typed in by hand"
        );
    }

    /// A token copied out of a chat window arrives wrapped. The prompt
    /// takes the token, not the line breaks around it.
    #[test]
    #[serial_test::serial(clipboard)]
    fn a_pasted_token_is_stripped_of_the_whitespace_it_travelled_with() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(&format!("  {SAMPLE_TOKEN}\n"));
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl);
        assert_eq!(
            h.peer_sync_snapshot().connect_input.as_deref(),
            Some(SAMPLE_TOKEN)
        );
    }

    /// The chord must not also type its own letter. `v` is a legal
    /// base64 character, so a paste that left one behind would corrupt
    /// the token in a way the user could not see.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_paste_chord_does_not_also_type_a_v() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl);
        let got = h.peer_sync_snapshot().connect_input.unwrap();
        assert!(
            !got.ends_with('v') || SAMPLE_TOKEN.ends_with('v'),
            "Ctrl+V pasted and typed: {got}"
        );
        assert_eq!(got, SAMPLE_TOKEN);
    }

    /// Unmodified characters still type. The chord guard must not have
    /// cost the prompt its ability to accept a token by hand.
    #[test]
    fn plain_characters_still_type_into_the_prompt() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        let none = Modifiers::default();
        for c in ["a", "b", "c"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), none);
        }
        assert_eq!(h.peer_sync_snapshot().connect_input.as_deref(), Some("abc"));
    }

    /// Every face of the card offers its gesture as a button too, for
    /// a user who reaches for the pointer.
    #[test]
    fn each_face_of_the_card_offers_its_own_gesture_as_a_button() {
        let (mut h, _) = host_with_chrome();
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_name: Some("bob".to_string()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"create"),
            "the face that asks who must offer a way to answer, got {names:?}"
        );
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"copy"),
            "the issuing side must offer Copy, got {names:?}"
        );

        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"paste"),
            "the receiving side must offer Paste, got {names:?}"
        );
        assert!(names.contains(&"join"), "and still Join, got {names:?}");

        // Half a token: what the field holds while typed by hand (a paste
        // arrives whole), exercising `elide_token` — a card that panicked
        // here would take the compositor with it.
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some("e".repeat(400)),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"join") && names.contains(&"paste"),
            "a field holding something unfinished still offers both, got {names:?}"
        );
    }

    fn draw_and_collect_peer_sync_buttons(h: &mut ShellOverlayHost) -> Vec<&'static str> {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        h.draw_peer_sync(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        h.peer_sync_prompt_rects_for_tests()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    // -----------------------------------------------------------------
    // An open the user is waiting on must stay acknowledged until they
    // can see how it ended.
    // -----------------------------------------------------------------

    /// The loading notice must outlive the fork (nothing on screen yet) but
    /// retire the moment the window's own place says which app is coming —
    /// otherwise it's noise, or a stale "Loading…" under "isn't responding".
    #[test]
    fn the_loading_indication_stands_until_the_window_place_speaks_for_itself() {
        const SURFACE: u64 = 918_273;
        let (h, chrome) = host_with_chrome();
        let notice = {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(crate::shell::notification::Notification::new(
                "Opening Ledger",
                "Loading…",
            ))
        };
        crate::shell::catalogue::note_pending_notice("dev.qos.retirefixture", notice);

        assert!(
            !h.retire_notices_for_opened_windows(),
            "nothing has appeared yet, so nothing is settled"
        );
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "the user is still waiting; the acknowledgement must still be up"
        );

        // Adoption isn't the end of the wait — for the first moments the
        // tab holds nothing visible, and a blink-fast app must not be
        // preceded by an indicator flashing on and off.
        let store = crate::compositor::foreign::store();
        store.forget(SURFACE);
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("dev.qos.retirefixture".to_string()),
            );
        }
        store.backdate_first_seen(SURFACE, std::time::Duration::ZERO);
        assert!(
            !h.retire_notices_for_opened_windows(),
            "a tab with nothing painted in it yet is not somewhere the wait \
             can be handed to; retiring on it hands the user back an empty \
             desktop"
        );
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "still waiting — nothing stands in the window's place"
        );

        // Past the delay the window's place names the app; the notice
        // stands down rather than repeating it.
        store.backdate_first_seen(
            SURFACE,
            crate::compositor::foreign::OPENING_DELAY + std::time::Duration::from_millis(50),
        );
        assert!(
            h.retire_notices_for_opened_windows(),
            "the window's own place is naming the app; a second indication \
             in the corner is noise"
        );
        assert!(
            !chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "the wait has moved to where the window will be, and only one \
             thing may be saying it"
        );
        store.forget(SURFACE);
    }

    /// The other way the hand-over can happen: an app fast enough to paint
    /// before the card was ever due still settles the open.
    #[test]
    fn a_window_that_paints_before_the_card_is_due_settles_the_open_too() {
        const SURFACE: u64 = 918_274;
        let (h, chrome) = host_with_chrome();
        let notice = {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(crate::shell::notification::Notification::new(
                "Opening Ledger",
                "Loading…",
            ))
        };
        crate::shell::catalogue::note_pending_notice("dev.qos.retirefixture", notice);

        let store = crate::compositor::foreign::store();
        store.forget(SURFACE);
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("dev.qos.retirefixture".to_string()),
            );
        }
        store.backdate_first_seen(SURFACE, std::time::Duration::ZERO);
        assert!(
            !h.retire_notices_for_opened_windows(),
            "precondition: nothing stands in the window's place yet"
        );

        // A frame that fits the tab, inside the delay — so the card was
        // never due and the window is simply there.
        let tile = vello::kurbo::Rect::new(0.0, 0.0, 120.0, 80.0);
        store.publish_rgba(SURFACE, &vec![0; 120 * 80 * 4], 120, 80);
        assert!(
            store.mark_presented(SURFACE, tile),
            "precondition: it presented"
        );
        assert!(
            h.retire_notices_for_opened_windows(),
            "the window is on screen, which is the end of the wait however \
             fast it came"
        );
        assert!(
            !chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "once the app is on screen the indication has done its job"
        );
        store.forget(SURFACE);
    }

    /// Escape still dismisses. The card gained chords; it must not have
    /// lost the way out.
    #[test]
    fn escape_still_dismisses_the_card() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let none = Modifiers::default();
        assert!(h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Escape)), none));
        assert!(h.peer_sync_snapshot().invite_token.is_none());
    }
}

#[cfg(test)]
mod two_workspace_host_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use ui::widget::Container;

    fn host_with_two_workspaces() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        // `bootstrap_demo` only seeds one workspace; `switch_down` is a
        // no-op with nothing below.
        tree.append_empty_workspace("Second");
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        host.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        (host, chrome)
    }

    /// Switching workspace must not turn on `needs_animation_frame` — a
    /// full-tree slide animation must not get reintroduced by accident.
    #[test]
    fn switching_workspace_animates_nothing() {
        let (h, chrome) = host_with_two_workspaces();
        assert!(
            !h.needs_animation_frame(),
            "an idle host must not be asking for frames"
        );

        chrome.borrow_mut().switch_workspace(1);
        assert!(
            !h.needs_animation_frame(),
            "a workspace switch must not put the shell into an animation \
             loop: the content changes, the screen does not move"
        );

        // And again on the way back — a switch in either direction is
        // the same non-event.
        chrome.borrow_mut().switch_workspace(-1);
        assert!(
            !h.needs_animation_frame(),
            "switching back must not animate either"
        );
    }

    /// The room a peer publishes under, put in and read back out through
    /// the host's own selection — not through a key the test rebuilt for
    /// itself, which would pass even if the two disagreed.
    #[test]
    fn real_peers_are_picked_up_by_room_and_only_the_active_one() {
        use ui::permissions::UserId;
        let (h, chrome) = host_with_two_workspaces();
        let (here, elsewhere) = {
            let c = chrome.borrow();
            let tree = c.tree();
            let active = tree.active;
            (
                tree.workspaces
                    .iter()
                    .find(|w| w.id == active)
                    .expect("an active workspace")
                    .room
                    .to_string(),
                tree.workspaces
                    .iter()
                    .find(|w| w.id != active)
                    .expect("a second workspace")
                    .room
                    .to_string(),
            )
        };
        assert_ne!(here, elsewhere, "two workspaces must be two rooms");

        let mut presence = PresenceCtx::empty();
        presence.set_pointer(
            &PresenceKey::Workspace { id: here },
            UserId("alice".into()),
            (120.0, 240.0),
        );
        presence.set_pointer(
            &PresenceKey::Workspace { id: elsewhere },
            UserId("bob".into()),
            (300.0, 400.0),
        );

        let visible = h.visible_peers(&presence);
        assert_eq!(
            visible.len(),
            1,
            "the peer in this room is drawn and the one in another is not, got {:?}",
            visible.iter().map(|p| &p.label).collect::<Vec<_>>(),
        );
        assert_eq!(visible[0].label, "alice");
        assert_eq!(visible[0].pos, (120.0, 240.0));
    }

    /// Two peers in the same room are drawn in distinct colours.
    #[test]
    fn several_peers_in_one_room_are_all_drawn_in_distinct_colours() {
        use ui::permissions::UserId;
        let (h, chrome) = host_with_two_workspaces();
        let room = {
            let c = chrome.borrow();
            let tree = c.tree();
            let active = tree.active;
            tree.workspaces
                .iter()
                .find(|w| w.id == active)
                .expect("an active workspace")
                .room
                .to_string()
        };
        let key = PresenceKey::Workspace { id: room };
        let mut presence = PresenceCtx::empty();
        // Two close enough to have stacked under a fixed-offset layout, and
        // two with names differing by one letter.
        for (who, at) in [
            ("alice", (400.0, 300.0)),
            ("bob", (404.0, 302.0)),
            ("dana", (120.0, 500.0)),
            ("danah", (700.0, 120.0)),
        ] {
            presence.set_pointer(&key, UserId(who.into()), at);
        }

        let visible = h.visible_peers(&presence);
        assert_eq!(visible.len(), 4, "every peer in the room must be drawn");

        for (i, a) in visible.iter().enumerate() {
            for b in &visible[i + 1..] {
                let (ac, bc) = (a.color.to_rgba8(), b.color.to_rgba8());
                let d = ui::presence::color_distance((ac.r, ac.g, ac.b), (bc.r, bc.g, bc.b));
                assert!(
                    d >= 60.0,
                    "{} and {} are drawn {d:.1} apart — too close to tell apart",
                    a.label,
                    b.label,
                );
            }
        }

        // And their names do not land on each other, however close their
        // pointers are.
        let requests: Vec<ui::presence::PillRequest> = visible
            .iter()
            .map(|p| ui::presence::PillRequest {
                at: p.pos,
                size: (60.0, 20.0),
            })
            .collect();
        let placed = ui::presence::layout_peer_pills(h.rect, &requests);
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert!(
                    a.rect.intersect(b.rect).is_zero_area(),
                    "two name pills overlap: {:?} and {:?}",
                    a.rect,
                    b.rect,
                );
            }
            assert!(
                a.rect.x0 >= h.rect.x0
                    && a.rect.y0 >= h.rect.y0
                    && a.rect.x1 <= h.rect.x1
                    && a.rect.y1 <= h.rect.y1,
                "a name pill at {:?} left the window {:?}",
                a.rect,
                h.rect,
            );
        }
    }
}

#[cfg(test)]
mod modal_curve_tests {
    //! Guards the shared ease-out-cubic curve used by the workspace-settings
    //! modal and the command palette (`modal_transition_curve`, which both
    //! draw paths call) — keeps the *source* honest so a later refactor
    //! can't silently regress the rest state or the animation's start scale.

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
        // At t=0 the panel must be invisible, 20px low, and at its start
        // scale — otherwise it pops in rather than animating.
        let start = Instant::now();
        let t = opening();
        let (alpha, dy, scale) = modal_transition_curve(&t, start, 0.9);
        assert!(alpha < 0.05, "alpha at t=0 should be ~0, got {alpha}");
        assert!(
            (dy - 20.0).abs() < 0.5,
            "dy at t=0 should be ~20 px, got {dy}"
        );
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
        // Deterministic proof (no wall-clock sleep) that the palette fades
        // out rather than popping, by sampling openness at explicit instants.
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

    /// The draw path consults `self.last_cursor` against each row rect, so
    /// the hit-test must stay honest with that geometry; `hit_test_overview`
    /// and `overview_row_rect` are the shared oracle.
    #[test]
    fn overview_hit_test_returns_card_index_when_cursor_is_over_card() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.append_empty_workspace("Second");
        tree.append_empty_workspace("Third");
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
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
    //! Pins the modal event-absorb gate: every pointer event over an open
    //! overlay must short-circuit rather than leak to the content widget
    //! underneath. Runs against the real `host.handle_event` path via a
    //! [`RecordingContent`] mock, not just the
    //! [`ShellOverlayHost::cursor_over_open_overlay`] predicate.
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

    /// Mock content widget recording every [`Widget::handle_event`] call by
    /// event-variant name, so tests can assert which events reached it —
    /// and that absorbed ones did not.
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
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
                origin.x + f64::from(l.location.x) + f64::from(l.size.width),
                origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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

        fn name(&self) -> &'static str {
            "RecordingContent"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// Builds a host wrapping [`RecordingContent`], laid out at a known
    /// viewport; `log` is shared with the mock for post-event assertions.
    struct AbsorbHarness {
        host: ShellOverlayHost,
        chrome: crate::shell::chrome_state::Chrome,
        taffy: TaffyTree<()>,
        root_node: NodeId,
        log: Rc<RefCell<Vec<String>>>,
    }

    fn build_harness() -> AbsorbHarness {
        build_harness_sized(1600.0, 900.0)
    }

    /// One full layout pass over the host, as the compositor's frame does
    /// it. Separate from the constructor so a test can lay out *again*
    /// after a paint — what the shell does when a draw finds the palette
    /// needs more room than the last layout gave it.
    fn lay_out_host(host: &mut ShellOverlayHost, vw: f64, vh: f64) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
        let child = host.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(f64_to_f32_lossy(vw)),
                        height: Dimension::length(f64_to_f32_lossy(vh)),
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
                    width: AvailableSpace::Definite(f64_to_f32_lossy(vw)),
                    height: AvailableSpace::Definite(f64_to_f32_lossy(vh)),
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
        (taffy, child)
    }

    /// The harness at an explicit viewport, for the geometry that only
    /// goes wrong at one end of the range — the palette card is a
    /// fraction of the viewport, so its filter band is only as tight as
    /// the screen is narrow.
    fn build_harness_sized(vw: f64, vh: f64) -> AbsorbHarness {
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let content = Box::new(RecordingContent::new(log.clone()));
        let mut host = ShellOverlayHost::new(content).with_chrome(chrome.clone());
        let (taffy, child) = lay_out_host(&mut host, vw, vh);

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
        let root = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let mut handled = false;
        h.host.handle_event(
            event,
            1.0,
            &modifiers,
            &mut doc,
            &root,
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

    /// One production paint, so the chip / scope rects a click router
    /// (or an assertion) reads are the ones that were actually drawn.
    fn paint(h: &mut AbsorbHarness) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = vello::Scene::new();
        let presence = ui::presence::PresenceCtx::default();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// Paints (see [`paint`]), then clicks a point.
    fn paint_then_click(h: &mut AbsorbHarness, point_of: impl Fn(&ShellOverlayHost) -> Point) {
        paint(h);
        let p = point_of(&h.host);
        pump(h, &cursor_moved_at(p));
        pump(h, &left_button_pressed());
    }

    /// Paint, re-lay-out, paint again — the two-frame settle the shell
    /// itself performs when a draw discovers the filter band needs more
    /// rows than the last layout reserved.
    fn settle(h: &mut AbsorbHarness, vw: f64, vh: f64) {
        paint(h);
        let (taffy, child) = lay_out_host(&mut h.host, vw, vh);
        h.taffy = taffy;
        h.root_node = child;
        paint(h);
    }

    /// Every filter must be reachable at every width. An unpainted chip has
    /// no rect, so its filter is unclickable even though the registry lists
    /// it — at 1600px five kinds exactly filled the row, hiding a sixth.
    /// Asserts on the rects production `draw` cached, since that's what a
    /// click is hit-tested against.
    #[test]
    fn every_kind_is_reachable_at_every_width() {
        // 800 is the narrowest viewport `shots --sweep` renders, where the
        // palette card hits its 560px floor and the filter band is tightest.
        for (vw, vh) in [(800.0, 600.0), (1280.0, 800.0), (1600.0, 900.0)] {
            let mut h = build_harness_sized(vw, vh);
            h.host.set_palette_visible(true);
            h.host.force_palette_settled();
            settle(&mut h, vw, vh);

            let kinds = h.host.search_registry.registered_kinds();
            let rects = h.host.search_chip_rects.clone();
            assert_eq!(
                rects.len(),
                kinds.len(),
                "at {vw}x{vh} the strip painted {} of {} kinds — an \
                 unpainted chip has no rect, so the filter it stands for \
                 cannot be clicked at all: {:?}",
                rects.len(),
                kinds.len(),
                kinds
                    .iter()
                    .map(super::super::search::ResourceKind::label)
                    .collect::<Vec<_>>()
            );

            let band = h.host.search_banner_rect();
            let all = h.host.search_all_chip_rect;
            let track = h.host.search_scope_track_rect();
            let mut painted: Vec<(String, Rect)> = vec![("All".to_string(), all)];
            painted.extend(
                kinds
                    .iter()
                    .map(|k| k.label().to_string())
                    .zip(rects.iter().copied()),
            );
            for (label, r) in &painted {
                assert!(
                    r.area() > 0.0,
                    "at {vw}x{vh} the {label} chip has no area: {r:?}"
                );
                assert!(
                    r.y0 >= band.y0 - 0.5 && r.y1 <= band.y1 + 0.5,
                    "at {vw}x{vh} the {label} chip {r:?} is outside the \
                     band {band:?} — it would paint over the results"
                );
                assert!(
                    r.x0 >= band.x0 - 0.5 && r.x1 <= band.x1 + 0.5,
                    "at {vw}x{vh} the {label} chip {r:?} runs past the \
                     band's edges {band:?}"
                );
            }
            // Nothing overlaps anything: two chips sharing pixels means
            // one of them eats the other's clicks.
            for i in 0..painted.len() {
                for j in (i + 1)..painted.len() {
                    let (a, b) = (painted[i].1, painted[j].1);
                    assert!(
                        a.intersect(b).area() <= 0.0,
                        "at {vw}x{vh} the {} and {} chips overlap: {a:?} / {b:?}",
                        painted[i].0,
                        painted[j].0
                    );
                }
                assert!(
                    painted[i].1.intersect(track).area() <= 0.0,
                    "at {vw}x{vh} the {} chip sits under the scope switch: \
                     {:?} / {track:?}",
                    painted[i].0,
                    painted[i].1
                );
            }
        }
    }

    /// Clicking the last chip at the narrowest viewport must apply its
    /// filter — driven through the production `handle_event` router against
    /// the rect `draw` actually cached, so a mis-placed chip fails here too.
    #[test]
    fn the_last_chip_can_be_clicked_at_the_narrowest_width() {
        let (vw, vh) = (800.0, 600.0);
        let mut h = build_harness_sized(vw, vh);
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        settle(&mut h, vw, vh);

        let last_kind = h
            .host
            .search_registry
            .registered_kinds()
            .last()
            .cloned()
            .expect("the registry has kinds");
        assert!(
            h.host.active_kind_filter.is_empty(),
            "the palette starts unfiltered"
        );
        let target = *h
            .host
            .search_chip_rects
            .last()
            .expect("the last kind must have been painted");
        pump(&mut h, &cursor_moved_at(target.center()));
        pump(&mut h, &left_button_pressed());

        assert_eq!(
            h.host.active_kind_filter,
            vec![last_kind.clone()],
            "clicking the last chip at {vw}x{vh} must filter to {:?}; the \
             palette instead holds {:?}",
            last_kind.label(),
            h.host
                .active_kind_filter
                .iter()
                .map(super::super::search::ResourceKind::label)
                .collect::<Vec<_>>()
        );
        assert!(
            h.host.palette_visible(),
            "and the click must not be taken for a dismissal"
        );
    }

    /// The layout rule itself, at widths no card would ever have, so the
    /// invariant is pinned rather than inferred from one viewport: every
    /// chip gets a positive, non-overlapping rect inside the band, even
    /// when the rows on offer cannot hold them at natural width.
    #[test]
    fn the_chip_strip_places_every_chip_however_narrow_the_band() {
        let band = Rect::new(0.0, 0.0, 240.0, 36.0);
        let natural = [40.0, 44.0, 92.0, 50.0, 46.0, 60.0, 58.0];
        for rows_available in [1usize, 2, 3] {
            let strip =
                lay_out_chip_strip(&natural, band, band.x1 - 100.0, rows_available, 36.0, 24.0);
            assert_eq!(
                strip.rects.len(),
                natural.len(),
                "every chip gets a rect at {rows_available} row(s)"
            );
            assert!(
                strip.rows_needed >= 1,
                "the strip always needs at least one row"
            );
            for (i, r) in strip.rects.iter().enumerate() {
                assert!(
                    r.width() > 0.0 && r.height() > 0.0,
                    "chip {i} of {} is not clickable at {rows_available} \
                     row(s): {r:?}",
                    natural.len()
                );
                assert!(
                    r.x0 >= band.x0 - 0.001,
                    "chip {i} starts left of the band: {r:?}"
                );
            }
            for i in 0..strip.rects.len() {
                for j in (i + 1)..strip.rects.len() {
                    assert!(
                        strip.rects[i].intersect(strip.rects[j]).area() <= 0.0,
                        "chips {i} and {j} overlap at {rows_available} \
                         row(s): {:?} / {:?}",
                        strip.rects[i],
                        strip.rects[j]
                    );
                }
            }
        }
        // And when there is room, nothing is squeezed: the chips keep
        // their natural widths.
        let wide = Rect::new(0.0, 0.0, 900.0, 36.0);
        let strip = lay_out_chip_strip(&natural, wide, wide.x1, 1, 36.0, 24.0);
        assert_eq!(strip.rows_needed, 1);
        for (i, r) in strip.rects.iter().enumerate() {
            assert_eq!(
                r.width(),
                natural[i],
                "chip {i} was squeezed with room to spare"
            );
        }
    }

    /// Clicking a kind chip or scope switch must filter, not be mistaken
    /// for a dismissal click outside the card. Drives the production
    /// `handle_event` router against rects `draw` just cached.
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
    fn workspace_lens_leaves_wide_workspace_visible_and_interactive() {
        let mut h = build_harness_sized(1600.0, 900.0);
        h.chrome
            .borrow_mut()
            .open_workspace_settings(WsTab::General);
        let lens = h.host.ws_settings_rect();
        assert_eq!(lens.x1, 1588.0, "lens docks to the right inset");
        assert!(lens.x0 > 700.0, "workspace remains visible beside {lens:?}");

        let workspace_point = Point::new(120.0, 300.0);
        assert!(!lens.contains(workspace_point));
        pump(&mut h, &cursor_moved_at(workspace_point));
        h.log.borrow_mut().clear();
        assert!(!pump(&mut h, &left_button_pressed()));
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Pressed")
        );
        assert!(h.chrome.borrow().overlays.ws_settings);
    }

    #[test]
    fn workspace_lens_promotes_to_one_inset_sheet_on_narrow_outputs() {
        let h = build_harness_sized(600.0, 700.0);
        let lens = h.host.ws_settings_rect();
        assert_eq!(lens.x0, 12.0);
        assert_eq!(lens.x1, 588.0);
        assert_eq!(lens.y1, 688.0);
    }

    #[test]
    fn cursor_moved_outside_open_overlay_still_reaches_content() {
        // Negative test: outside every overlay's rect the absorb gate must
        // NOT fire — the gate is bounded by rect, not "any pointer event
        // while an overlay is open".
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
    //! Drives `host.handle_event` with real winit events, through the
    //! same path the live binary takes.
    use super::*;
    use crate::shell::chrome_state::{ChromeState, WsTab};
    use crate::shell::state::WorkspaceTree;

    fn history_target(id: &str) -> ui::HistoryResourceTarget {
        ui::HistoryResourceTarget {
            app_id: Some("org.qos.Notes".to_string()),
            app_label: Some("Notes".to_string()),
            id: id.to_string(),
            kind: "page".to_string(),
            label: "Launch notes".to_string(),
        }
    }

    #[test]
    fn history_open_distinguishes_files_links_and_semantic_ids() {
        assert_eq!(
            history_target_address(&history_target("file:/workspace/brief.md")),
            HistoryTargetAddress::File(PathBuf::from("/workspace/brief.md"))
        );
        assert_eq!(
            history_target_address(&history_target("tube://watch/launch")),
            HistoryTargetAddress::Uri("tube://watch/launch".to_string())
        );
        assert_eq!(
            history_target_address(&history_target("notes.page:launch")),
            HistoryTargetAddress::App
        );
    }
    use std::collections::HashMap as StdHashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey};

    /// Recording mock content for the host's `content` slot. The palette
    /// test never reaches `content.handle_event`, but the WS-settings click
    /// test needs a real layout target so taffy can place the modal.
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
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
                origin.x + f64::from(l.location.x) + f64::from(l.size.width),
                origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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

        fn name(&self) -> &'static str {
            "StubContent"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    #[test]
    fn onboarding_release_keeps_focus_reset_after_relayout_edge_is_consumed() {
        let mut host = ShellOverlayHost::new(Box::new(StubContent::new()));
        host.onboarding_released = true;
        host.onboarding_focus_reset = true;

        assert!(host.take_structure_dirty());
        assert!(host.take_focus_reset());
        assert!(!host.take_focus_reset(), "focus reset must remain one-shot");
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
        let tree = WorkspaceTree::bootstrap_demo();
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

    /// Super+S calls the wheel to wherever the pointer is, so the
    /// workspace list is reachable from the right-hand end of a strip
    /// without scrolling all the way back to its left edge.
    #[test]
    fn super_s_summons_the_wheel_to_the_pointer_and_toggles_back() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(
            h.chrome.borrow().wheel_summon().is_none(),
            "the wheel starts docked"
        );

        let at = Point::new(1240.0, 610.0);
        pump(&mut h, &cursor_moved_at(at), sup);
        assert!(
            pump(&mut h, &key_press(Key::Character("s".into())), sup),
            "Super+S is handled by the shell"
        );
        assert_eq!(
            h.chrome.borrow().wheel_summon(),
            Some(at),
            "the wheel is summoned to the pointer, not to a fixed spot"
        );
        assert!(
            h.host.wheel_summoned(),
            "the client-input filter has to see the summon so Escape reaches the shell"
        );

        // Pressing it again puts the wheel back.
        assert!(pump(&mut h, &key_press(Key::Character("s".into())), sup));
        assert!(
            h.chrome.borrow().wheel_summon().is_none(),
            "Super+S toggles the summon off"
        );
    }

    #[test]
    fn super_p_opens_and_toggles_the_projection_sheet() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(!h.chrome.borrow().overlays.projection);

        assert!(pump(&mut h, &key_press(Key::Character("p".into())), sup));
        assert!(
            h.chrome.borrow().overlays.projection,
            "Win+P must open the OS-owned projection sheet"
        );

        assert!(pump(&mut h, &key_press(Key::Character("p".into())), sup));
        assert!(
            !h.chrome.borrow().overlays.projection,
            "the same familiar chord closes the sheet again"
        );
    }

    /// Escape dismisses a summoned wheel, and takes priority over the
    /// rest of the dismissal ladder because it is the most transient
    /// thing the shell puts up.
    #[test]
    fn escape_dismisses_a_summoned_wheel_before_anything_else() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);
        let none = Modifiers::default();

        pump(&mut h, &cursor_moved_at(Point::new(900.0, 400.0)), sup);
        pump(&mut h, &key_press(Key::Character("s".into())), sup);
        // Raise an overview too, so the test proves ordering rather
        // than just "escape closes the only thing open".
        {
            let mut st = h.chrome.borrow_mut();
            st.overlays.overview = true;
            st.bump();
        }

        assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), none));
        assert!(
            h.chrome.borrow().wheel_summon().is_none(),
            "Escape sends the wheel back to its dock"
        );
        assert!(
            h.chrome.borrow().overlays.overview,
            "and stops there — the overview behind it is untouched"
        );
    }

    fn pump(h: &mut Harness, event: &WindowEvent, modifiers: Modifiers) -> bool {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;
        let mut handled = false;
        h.host.handle_event(
            event,
            1.0,
            &modifiers,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            h.root_node,
            &h.taffy,
        );
        handled
    }

    /// Builds a `KeyEvent` over a zeroed buffer — winit 0.30's
    /// `KeyEventExtra` is `pub(crate)`, so a struct literal isn't possible.
    /// Only the pub fields the host reads (`logical_key`, `state`) are
    /// overwritten.
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
        assert!(
            sheet.x1 <= 1600.0 && sheet.y1 <= 900.0,
            "inside the viewport"
        );
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
        let first_row = h
            .host
            .room_activity()
            .timeline()
            .row_rect(0)
            .expect("the room has at least one row laid out");
        let mods = Modifiers::default();
        let handled = pump(&mut h, &cursor_moved_at(first_row.center()), mods);
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

        let row = h.host.collaborators_overlay.activity_rect();
        assert!(row.area() > 0.0, "the footer row painted a hit target");
        assert_eq!(
            row,
            h.host.collaborators_overlay.activity_rect(),
            "the cached hit rect is the one that was drawn"
        );

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(row.center()), mods);
        let handled = pump(&mut h, &left_button_pressed(), mods);
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
        assert!(h.host.collaborators_overlay.activity_rect().area() > 0.0);

        h.chrome.borrow_mut().overlays.collaborators = false;
        paint(&mut h);
        assert_eq!(
            h.host.collaborators_overlay.activity_rect(),
            Rect::ZERO,
            "a stale footer rect is a click target the user cannot see"
        );
    }

    /// Makes the local user owner of every room — what a real session does
    /// on the way up but `bootstrap_demo` leaves undone. Without it nobody
    /// may invite, correctly, but that's not a state a real session is
    /// ever in.
    fn adopt_rooms(h: &Harness) {
        let st = h.chrome.borrow();
        let user = st.local_user_id.clone();
        st.tree_mut().adopt_ownerless_workspaces(&user);
    }

    /// The dropdown's subject is who is in this room, so it must offer both
    /// ways that changes: adding somebody, and joining somebody else's room.
    #[test]
    fn collaborator_dropdown_offers_invite_and_join() {
        let mut h = build_harness();
        adopt_rooms(&h);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        let (invite, join) = h.host.collaborators_action_rects_for_tests();
        assert!(invite.area() > 0.0, "Invite painted a hit target");
        assert!(join.area() > 0.0, "Join painted a hit target");
        assert!(
            invite.x1 <= join.x0,
            "the two buttons do not overlap: {invite:?} / {join:?}"
        );
        let panel = h.host.collaborators_overlay.panel_rect();
        for (name, r) in [("Invite", invite), ("Join", join)] {
            assert!(
                panel.contains(r.center()) && panel.contains(r.origin()),
                "{name} is inside the popover it is painted on"
            );
        }
        assert!(
            invite.y1 <= h.host.collaborators_overlay.activity_rect().y0,
            "the actions band sits above the Room activity row rather than over it"
        );
    }

    /// Inviting somebody is inviting *somebody* — the button asks who
    /// before minting, since an unnamed token leaves the host's own roster
    /// reading a bare peer id once the guest arrives.
    #[test]
    fn collaborator_invite_button_asks_who_before_minting() {
        let mut h = build_harness();
        adopt_rooms(&h);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        let (invite, _) = h.host.collaborators_action_rects_for_tests();
        assert!(invite.area() > 0.0, "there is a button to press");

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(invite.center()), mods);
        let handled = pump(&mut h, &left_button_pressed(), mods);
        assert!(handled, "the button claimed the click");

        assert_eq!(
            h.chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::PromptInvite],
            "the click opens the composer rather than minting an unnamed token"
        );
        assert!(
            !h.chrome.borrow().overlays.collaborators,
            "the dropdown closed, so the card is what the user is left looking at"
        );
    }

    /// The other machine's half of the same gesture.
    #[test]
    fn collaborator_join_button_opens_the_connect_prompt() {
        let mut h = build_harness();
        adopt_rooms(&h);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        let (_, join) = h.host.collaborators_action_rects_for_tests();
        assert!(join.area() > 0.0, "there is a button to press");

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(join.center()), mods);
        let handled = pump(&mut h, &left_button_pressed(), mods);
        assert!(handled, "the button claimed the click");

        assert_eq!(
            h.chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::PromptConnect],
            "the click opens the prompt that consumes a token"
        );
    }

    /// Peer sync off means nothing can mint a token, so the button must not
    /// offer to. The room is adopted first so peer-sync is the *only*
    /// capability missing.
    #[test]
    fn collaborator_actions_are_absent_when_peer_sync_is_off() {
        let mut h = build_harness();
        adopt_rooms(&h);
        assert!(!h.chrome.borrow().peer_sync_enabled);
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        assert_eq!(
            h.host.collaborators_action_rects_for_tests(),
            (Rect::ZERO, Rect::ZERO),
            "a session that cannot mint offers no button that says it can"
        );
        assert_no_peer_sync_work_from_a_click_in_the_panel(&mut h);
    }

    /// The capability gate must remove the *hit target*, not just the
    /// paint — a rect that outlives its button is invisible but still
    /// clickable.
    #[test]
    fn collaborator_actions_are_absent_without_the_invite_capability() {
        let mut h = build_harness();
        {
            // Owned by somebody else, and the local user holds no grant
            // here — the ordinary position of a guest in a room.
            let st = h.chrome.borrow();
            let mut tree = st.tree_mut();
            for ws in &mut tree.workspaces {
                ws.set_owner("somebody-else".to_string());
            }
        }
        // Peer sync *is* on, so the transport is not the reason.
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        assert_eq!(
            h.host.collaborators_action_rects_for_tests(),
            (Rect::ZERO, Rect::ZERO),
            "a guest is not offered the room's membership controls"
        );
        assert_no_peer_sync_work_from_a_click_in_the_panel(&mut h);
    }

    /// A click anywhere inside the popover asks the network for nothing.
    fn assert_no_peer_sync_work_from_a_click_in_the_panel(h: &mut Harness) {
        let panel = h.host.collaborators_overlay.panel_rect();
        let mods = Modifiers::default();
        let _ = pump(h, &cursor_moved_at(panel.center()), mods);
        let _ = pump(h, &left_button_pressed(), mods);
        assert!(
            h.chrome.borrow_mut().take_peer_sync_requests().is_empty(),
            "a click in a panel with no buttons queued no peer-sync work"
        );
    }

    /// Same rule as the footer row: the rects must not survive the
    /// dropdown closing, or empty space where a button used to be stays
    /// clickable.
    #[test]
    fn collaborator_action_rects_are_cleared_when_the_dropdown_closes() {
        let mut h = build_harness();
        adopt_rooms(&h);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        let (invite, join) = h.host.collaborators_action_rects_for_tests();
        assert!(invite.area() > 0.0 && join.area() > 0.0);

        h.chrome.borrow_mut().overlays.collaborators = false;
        paint(&mut h);
        assert_eq!(
            h.host.collaborators_action_rects_for_tests(),
            (Rect::ZERO, Rect::ZERO),
            "stale action rects are click targets the user cannot see"
        );
    }

    #[test]
    fn escape_closes_the_room_activity_sheet() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.room_activity = true;
        h.host.force_room_activity_settled();
        paint(&mut h);

        let mods = Modifiers::default();
        let handled = pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), mods);
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
        let _ = pump(&mut h, &cursor_moved_at(outside), mods);
        let handled = pump(&mut h, &left_button_pressed(), mods);
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
        let handled = pump(&mut h, &key_press(Key::Character("z".into())), mods);
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

        let handled = pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), mods);
        assert!(handled);
        assert!(
            !h.chrome.borrow().overlays.room_activity,
            "Escape closed the sheet rather than being eaten by the typeahead"
        );
    }

    /// Opening the palette with a stale query must both focus the input
    /// and clear it. Drives the production Ctrl+K path through
    /// `handle_event`.
    #[test]
    fn toggle_palette_focuses_input() {
        let mut h = build_harness();
        // Seed a stale query so the "clear on open" half of the spec
        // is observable too.
        h.host.palette.set_query("stale residual");
        assert!(!h.host.palette.is_focused(), "palette must start unfocused");
        assert!(!h.host.palette_visible(), "palette must start hidden");

        let mods = Modifiers::from(ModifiersState::CONTROL);
        let handled = pump(&mut h, &key_press(Key::Character("k".into())), mods);

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

    /// `Ctrl+K` scopes to the focused app (Notes, from `bootstrap_demo`,
    /// which `NotesProvider` claims via `owns_tab`) so an installed-but-
    /// unopened app is not among its answers; `Super+K` is the global
    /// surface and must offer it.
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
        // Precondition: the focused tab must be provider-owned, or
        // `ActiveApp` excludes nothing and the test passes vacuously.
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
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), ctrl));
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

        // Super+K: the global surface. Closed first so the chord opens
        // rather than toggles the already-open palette shut.
        h.host.palette_visible = false;
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), sup));
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

    /// Ctrl+K scoped-search must reach the focused app's own *content*,
    /// not just its tab title. `nir` appears only in the note's body, so
    /// a provider looking it up by tab id rather than by document
    /// answers nothing.
    #[test]
    fn ctrl_k_finds_a_word_that_only_appears_in_the_notes_body() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::notes_app::NotesApp;

        let mut h = build_harness();
        let doc = AutoCommit::new();
        // The document as `TileHost::sync` hands it to the app every
        // frame: the tab's own record.
        let notes_tab =
            h.chrome.borrow().tree().workspaces[0].strips[0].window_groups[0].tabs[0].id;
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
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), ctrl));
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
        let painted =
            h.host.palette.visible_entries().iter().any(|e| {
                e.label == hit.title && e.description.as_deref() == hit.subtitle.as_deref()
            });
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

    /// A room starting empty is only workable if moving one of your own
    /// records into it costs one step: Super+K, type the name, and a
    /// record from *another* room is among the answers — labelled with
    /// where it lives — and Enter brings it here.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the `records` borrow feeds the `record_ids` closure used through the \
                  assertions right after; ending it earlier would mean cloning the record \
                  store for no real gain"
    )]
    fn super_k_finds_a_record_from_another_room_and_enter_brings_it_here() {
        use crate::shell::chrome_state::SearchScope;
        use ui::record::{RecordId, RecordKind};

        let mut h = build_harness();
        // Two rooms from the fixture tree: the one in front of the user
        // and one they are not in.
        let (here, there, there_name) = {
            let st = h.chrome.borrow();
            let tree = st.tree();
            let here = tree.active_workspace().expect("an active room").room;
            let other = tree
                .workspaces
                .iter()
                .find(|w| w.room != here)
                .expect("the fixture has more than one room");
            (here, other.room, other.name.clone())
        };

        // A record that lives over there and nowhere else.
        let book = RecordId::minted();
        let kind = RecordKind::new("field-guide");
        h.host
            .room_records()
            .borrow_mut()
            .reference(there, &book, &kind);
        assert!(
            !h.host.room_records().borrow().holds(here, &book),
            "precondition: this room starts without it"
        );

        let mut doc = AutoCommit::new();
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), sup));
        assert_eq!(h.chrome.borrow().search_scope, SearchScope::All);
        h.host.palette.set_query("field-guide");
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let listing = || {
            h.host
                .search_results
                .iter()
                .map(|r| (r.provider_id.clone(), r.title.clone(), r.subtitle.clone()))
                .collect::<Vec<_>>()
        };
        let idx = h
            .host
            .search_results
            .iter()
            .position(|r| r.provider_id == "records")
            .unwrap_or_else(|| {
                panic!(
                    "a record living in another room must be findable from \
                     this one; got {:?}",
                    listing()
                )
            });
        assert_eq!(
            h.host.search_results[idx].subtitle.as_deref(),
            Some(format!("In {there_name} · field-guide").as_str()),
            "the row has to say where the record lives — without it, two \
             rooms' records are indistinguishable rows"
        );
        // The row the palette paints, not just the result behind it.
        let title = h.host.search_results[idx].title.clone();
        let subtitle = h.host.search_results[idx].subtitle.clone();
        assert!(
            h.host
                .palette
                .visible_entries()
                .iter()
                .any(|e| e.label == title && e.description == subtitle),
            "the record must reach the palette's own rows: {:?}",
            h.host
                .palette
                .visible_entries()
                .iter()
                .map(|e| (&e.label, &e.description))
                .collect::<Vec<_>>()
        );

        // Enter on that row.
        h.host.palette.set_cursor(idx);
        h.host.activate_selected_result(&mut doc, &automerge::ROOT);

        let records = h.host.room_records();
        let records = records.borrow();
        assert!(
            records.holds(here, &book),
            "activating the row must bring the record into this room"
        );
        assert!(
            records.holds(there, &book),
            "and leave it in the room it came from: this is a reference, \
             not a move"
        );
        let record_ids = |room| -> Vec<RecordId> {
            records.records_in(room).into_iter().map(|c| c.id).collect()
        };
        assert_eq!(
            record_ids(here),
            record_ids(there),
            "both rooms must name the same record id — a copy would have \
             minted a new one, and the two would diverge from the next \
             edit onward"
        );
    }

    /// The app-scope pill has to name the focused app *as an app*, or it
    /// collides with the kind-filter chip strip above it: two adjacent
    /// controls with one word and different meanings ("results shaped
    /// like a note" versus "search inside the Notes app").
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
        // Generalised: no label may appear in both the filter row (kinds)
        // and the scope row (places) — `All` was one such collision.
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

    /// Click the Members pill and assert `chrome.ws_settings_tab` flips,
    /// through a real `MouseInput::Pressed` dispatched via `handle_event`.
    #[test]
    fn ws_settings_pill_click_switches_tab() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        assert_eq!(
            h.chrome.borrow().ws_settings_tab,
            WsTab::General,
            "modal must default to the General tab"
        );

        // Pill rects are populated inside `draw_ws_settings`, so paint
        // one frame before they're queryable.
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let members_pill = h.host.ws_settings_tab_pill_rect(WsTab::Members);
        assert!(
            members_pill.area() > 0.0,
            "Members pill rect must be populated by draw_ws_settings; got {members_pill:?}"
        );
        let p = members_pill.center();

        // MouseInput carries no position; seed `last_cursor` via CursorMoved first.
        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(p), mods);
        let handled = pump(&mut h, &left_button_pressed(), mods);

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

    #[test]
    fn ws_settings_close_button_is_pointer_accessible() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        let close = h.host.ws_settings_close_rect();
        assert!(close.area() > 0.0);

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(close.center()), mods);
        assert!(pump(&mut h, &left_button_pressed(), mods));
        assert!(!h.chrome.borrow().overlays.ws_settings);
    }

    /// Without the design's 1px backdrop blur, perceived dimming drops
    /// below intent, so the rendered alpha compensates. `0.45` is
    /// empirically where a non-blurred scrim reads like the blurred 0.30.
    #[allow(
        clippy::assertions_on_constants,
        reason = "regression guards on design constants; the messages interpolate f32 values, which cannot be const-evaluated"
    )]
    #[test]
    fn palette_scrim_alpha_matches_concept() {
        // `MODAL_SCRIM_ALPHA` already satisfies this today, so clippy sees
        // a compile-time-constant condition — but the point is to catch a
        // future edit that lowers it back toward the un-blurred 0.20. A
        // `const` assertion can't carry this message: `f32` isn't
        // const-`Display`-able, and the value belongs in the panic text.
        assert!(
            MODAL_SCRIM_ALPHA >= 0.45,
            "MODAL_SCRIM_ALPHA must be >= 0.45 to compensate for the \
             missing 1-px backdrop blur from concept §4.1; the un-blurred \
             0.20 scrim let the desktop bleed through the modal so the \
             palette / WS-settings surfaces never read as the focused \
             layer (got {MODAL_SCRIM_ALPHA})"
        );
    }

    /// Workspace settings follows application surfaces, not the brighter
    /// floating-panel material used by shell popovers.
    #[test]
    fn ws_settings_uses_application_base_surface() {
        let theme = Theme::dark();
        assert_eq!(
            workspace_settings_surface(&theme),
            theme.tokens.colors.surfaces.base
        );
    }

    /// The palette scrim must actually dim bright window content — same
    /// compensation [`MODAL_SCRIM_ALPHA`] documents. At 0.30 a `bg-white`
    /// card behind the palette stays at 178 grey; at 0.45 it drops to
    /// 141. Asserted as a lower bound so the next person can't satisfy
    /// the letter of it by re-lowering an exact pin.
    #[allow(
        clippy::assertions_on_constants,
        reason = "regression guards on design constants; the messages interpolate f32 values, which cannot be const-evaluated"
    )]
    #[test]
    fn palette_scrim_alpha_reads_like_the_blurred_concept() {
        // Same rationale as `palette_scrim_alpha_matches_concept`: a
        // regression guard on a design constant that's currently true by
        // construction, kept as a runtime assert because the panic
        // message interpolates an `f32`.
        assert!(
            PALETTE_SCRIM_ALPHA >= 0.45,
            "PALETTE_SCRIM_ALPHA must be >= 0.45 to compensate for the \
             missing backdrop blur — below that a white window card behind \
             the palette stays bright and the card's edge reads as a hard \
             cut (got {PALETTE_SCRIM_ALPHA})"
        );
        // The brightest thing the shell paints must come out clearly
        // darker than the surface it sits behind.
        let card = 255.0;
        let dimmed = card * (1.0 - PALETTE_SCRIM_ALPHA);
        assert!(
            dimmed <= 145.0,
            "a bg-white window card behind the palette lands at {dimmed} — \
             still a bright field next to a slate-900 card"
        );
    }

    /// The scrim's colour comes from the theme, not a literal — both
    /// themes must darken, or a light-theme scrim would push the
    /// background toward the surface it's meant to lift away from.
    #[test]
    fn scrim_hue_comes_from_the_theme_and_darkens_in_both() {
        for (name, colors) in [
            ("dark", ui::tokens::colors::Colors::dark()),
            ("light", ui::tokens::colors::Colors::light()),
        ] {
            let s = colors.surfaces.scrim.to_rgba8();
            let base = colors.surfaces.base.to_rgba8();
            let luma = |c: &ui::peniko::color::Rgba8| {
                0.2126 * f32::from(c.r) + 0.7152 * f32::from(c.g) + 0.0722 * f32::from(c.b)
            };
            assert!(
                luma(&s) < luma(&base),
                "the {name} theme's scrim must be darker than its base \
                 surface (scrim {s:?} vs base {base:?})"
            );
            assert_eq!(
                s.a, 255,
                "the {name} scrim token carries the hue; the \
                 strength is the overlay's own (got alpha {})",
                s.a
            );
        }
    }

    /// Extracted-helper coverage for the click-focuses-group logic.
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

    /// Legibility against a busy desktop backdrop wins over tint depth:
    /// [`MODAL_SURFACE_ALPHA`] lifts to `>= 0.90` so the palette reads as
    /// solid chrome with just a hint of the wallpaper's tint.
    #[allow(
        clippy::assertions_on_constants,
        reason = "regression guards on design constants; the messages interpolate f32 values, which cannot be const-evaluated"
    )]
    #[test]
    fn palette_surface_alpha_lifts_off_desktop() {
        // Same rationale as `palette_scrim_alpha_matches_concept`: a
        // regression guard on a design constant that's currently true by
        // construction, kept as a runtime assert because the panic
        // message interpolates an `f32`.
        assert!(
            MODAL_SURFACE_ALPHA >= 0.90,
            "MODAL_SURFACE_ALPHA must stay >= 0.90 so the palette + \
             workspace-settings modal read as opaque chrome; a lower \
             alpha reintroduces the 2026-07-09 legibility complaint \
             (got {MODAL_SURFACE_ALPHA})"
        );
    }
}

#[cfg(test)]
mod maximize_harvest_tests {
    //! Unit coverage for the per-frame `WindowGroup::maximized` writeback,
    //! complementing the full-pipeline test in
    //! `tests/maximize_persist_wireup_test.rs`. Two pins: a no-op tick must
    //! leave persisted state alone, and a pending change must write through.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, GroupId, SharedSurfaces, Strip, StripId, Tab, TileContent,
        WindowGroup as StateGroup, WindowWidth as StateWidth, Workspace, WorkspaceId,
        WorkspaceTree,
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
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            id: ws_id,
            room: crate::shell::state::RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            collaborators: vec![],
            shared_surfaces: SharedSurfaces::default(),
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

    /// Priming `pending_maximize_change = Some(true)` and ticking the
    /// harvest must mirror it into the persisted `WorkspaceTree`.
    #[test]
    fn harvest_writes_pending_maximize_change_through_to_tree() {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
        use vello::kurbo::Point;
        use winit::dpi::PhysicalPosition;
        use winit::event::{Modifiers, WindowEvent};
        let (tree, strip_id, group_id) = one_group_tree(false);
        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

        // Flip `maximized` and arm the pending-change slot the way the
        // production drain block does — the integration test covers the click.
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, group_id);
            assert!(!g.maximized, "seed mirrors persisted state");
            g.maximized = true;
            // Can't poke `pending_maximize_change` from outside the crate,
            // so go through the publish + drain path the production click
            // flow uses.
            g.publish_maximize_intent_for_tests();
            // The drain runs inside `handle_event`; reset to false so the
            // publish flips it back to true through `take_max`.
            g.maximized = false;
        }

        // A no-op cursor-move runs the drain block and arms
        // `pending_maximize_change`.
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

    /// A no-op tick must not overwrite a persisted `maximized = true` with
    /// the widget default — gated on `take_pending_maximize_change`
    /// returning `Some(_)`, not on the widget's current value.
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
    //! A `chrome.bump()` after pushing fresh `WindowGroup`s must propagate
    //! to the live `GroupRow` widgets on the next `take_structure_dirty`
    //! tick, even without `update()` — the `shots` driver's relayout path
    //! skips it.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, GroupId, SharedSurfaces, Strip, StripId, Tab, TileContent,
        WindowGroup as StateGroup, WindowWidth as StateWidth, Workspace, WorkspaceId,
        WorkspaceTree,
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
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            id: ws_id,
            room: crate::shell::state::RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            collaborators: vec![],
            shared_surfaces: SharedSurfaces::default(),
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
        let tab_id = id.saturating_add(1000);
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
        let id_before = host.group_at_for_tests(strip_id, 0).map_or(0, |g| g.id);
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

#[cfg(test)]
mod peer_cursor_tests {
    //! A peer caret on screen must mean a peer is connected.
    //!
    //! These tests pin both halves of the invariant on the
    //! production paint path: nothing is drawn for an empty roster, and
    //! a real peer is still drawn.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, Collaborator, GroupId, Strip, StripId, Tab, TileContent,
        WindowGroup as StateGroup, WindowWidth as StateWidth, Workspace, WorkspaceId,
        WorkspaceTree,
    };
    use ui::permissions::UserId;
    use ui::presence::{PresenceCtx, PresenceKey};

    fn one_group_seed() -> WorkspaceTree {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 100;
        let tab_id = 1;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                width: StateWidth::Normal,
                maximized: false,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Terminal".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Terminal,
                        label: "Terminal".into(),
                    },
                }],
                // A group's active tab has to name a tab it has; `0`
                // names nothing, and the resolvers read this.
                active_tab: tab_id,
            }],
        };
        // Through the constructor rather than a struct literal: a field
        // added to `Workspace` should not break this fixture.
        let mut ws = Workspace::test_default(ws_id, "Work");
        ws.strips = vec![strip];
        let mut tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            ..Default::default()
        };
        // Both are per-parent maps, not single pointers: one active strip
        // per workspace, one active group per strip.
        tree.active_strip.insert(ws_id, strip_id);
        tree.active_group.insert(strip_id, group_id);
        tree
    }

    /// The key an app in this workspace actually publishes presence
    /// under — the workspace's **room**, read off the tree rather than
    /// spelled out here.
    fn active_key(tree: &WorkspaceTree) -> PresenceKey {
        // The room is what the launcher stamps into an app as
        // `QOS_WORKSPACE_ID`, not the local `WorkspaceId` — naming the
        // local handle here would write a key nobody reads.
        let room = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .map(|w| w.room.to_string())
            .expect("the seed has an active workspace");
        PresenceKey::Workspace { id: room }
    }

    /// Draws the peer layer for `presence`, returning how many draw
    /// commands it emitted (0 = nothing painted). Takes the tree the
    /// caller built `presence` keys against — `one_group_seed` mints a
    /// fresh `WorkspaceId` per call.
    fn peer_draw_ops(tree: &WorkspaceTree, presence: &PresenceCtx) -> usize {
        let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(tree);
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let theme = Theme::dark();
        let mut tr = ui::text_render::TextRenderer::new();
        let mut scene = Scene::new();
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence);
        scene.encoding().draw_tags.len()
    }

    /// The one that matters. A session with nobody else in it paints
    /// no caret, no name chip, no peer of any kind — so a screenshot
    /// of a solo desktop cannot show a collaborator.
    #[test]
    fn a_solo_session_paints_no_peer() {
        let tree = one_group_seed();
        assert_eq!(
            peer_draw_ops(&tree, &PresenceCtx::default()),
            0,
            "an empty roster must paint nothing"
        );
    }

    /// An empty pointer list for the active workspace is still an
    /// empty roster — the key existing must not conjure a peer.
    #[test]
    fn an_empty_pointer_list_paints_no_peer() {
        let tree = one_group_seed();
        let mut presence = PresenceCtx::default();
        presence.pointers.insert(active_key(&tree), vec![]);
        assert_eq!(
            peer_draw_ops(&tree, &presence),
            0,
            "an empty list is not a peer"
        );
    }

    /// The half that keeps the no-peer assertions honest: if this fails,
    /// the helper reports zero for everything and proves nothing. A peer
    /// published under the active workspace's presence key still paints.
    #[test]
    fn a_real_peer_is_still_painted() {
        let tree = one_group_seed();
        let mut presence = PresenceCtx::default();
        presence.pointers.insert(
            active_key(&tree),
            vec![(UserId("alice".into()), (400.0, 300.0))],
        );
        assert!(
            peer_draw_ops(&tree, &presence) > 0,
            "a connected peer must still draw a cursor"
        );
    }
    #[test]
    fn following_a_peer_tracks_their_live_tab() {
        let mut tree = one_group_seed();
        let strip = tree.workspaces[0].strips[0].id;
        let target_group = 200;
        let target_tab = 201;
        tree.workspaces[0].strips[0].window_groups.push(StateGroup {
            id: target_group,
            width: StateWidth::Normal,
            maximized: false,
            tabs: vec![Tab {
                id: target_tab,
                title: "Plan".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("Plan".into()),
                    label: "Plan".into(),
                },
            }],
            active_tab: target_tab,
        });
        tree.collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab));

        let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
        host.followed_peer = Some("ada".to_string());

        assert!(host.reconcile_followed_peer());
        assert_eq!(
            chrome.borrow().tree().active_group_for(strip),
            Some(target_group),
            "follow must move local focus to the participant's live group",
        );
    }
}

/// The collaborators dropdown lists live people and agents together, and
/// says which is which. Dormant membership stays in workspace settings:
/// immediate presence never invents an idle session.
#[cfg(test)]
mod roster_rows {
    use super::collaborator_roster_rows;
    use crate::shell::state::{
        CapOverride, Collaborator, Subject, Verb, Workspace, WorkspaceId, WorkspaceTree,
    };

    fn tree_with_room() -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut ws = Workspace::test_default(id, "Demo");
        ws.set_owner("ada");
        tree.workspaces.push(ws);
        tree.active = id;
        tree
    }

    #[test]
    fn an_ungranted_room_contributes_no_agent_row() {
        let mut tree = tree_with_room();
        tree.collaborators.push(Collaborator::new("bob", "bob"));
        let rows = collaborator_roster_rows(&tree);
        assert_eq!(rows.len(), 1, "only the peer is here: {rows:?}");
        assert!(!rows[0].is_agent);
        assert_eq!(rows[0].label, "bob");
        assert!(rows[0].followable);
        assert_eq!(rows[0].activity.as_deref(), Some("Here now"));
    }

    #[test]
    fn a_granted_but_absent_agent_is_not_reported_as_live() {
        let mut tree = tree_with_room();
        tree.collaborators.push(Collaborator::new("bob", "bob"));
        {
            let ws = tree.active_workspace_mut().expect("fixture has a room");
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"ada".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .expect("the owner may admit an agent");
        }

        let rows = collaborator_roster_rows(&tree);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "bob");
        assert!(!rows[0].is_agent, "a peer must never be tagged as an agent");
    }

    #[test]
    fn a_connected_agent_is_tagged_and_followable_like_a_collaborator() {
        let mut tree = tree_with_room();
        {
            let ws = tree.active_workspace_mut().expect("fixture has a room");
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"ada".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .expect("the owner may admit an agent");
            ws.collaborators.push(Collaborator::new("peer@9", "scribe"));
        }

        let rows = collaborator_roster_rows(&tree);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "scribe");
        assert!(
            rows[0].is_agent,
            "an agent listed without its tag is the OS reporting a person",
        );
        assert_eq!(rows[0].initial, 'S');
        assert!(rows[0].followable);
        assert_eq!(rows[0].activity.as_deref(), Some("Working now"));
    }

    /// A row shows what its participant may do here, and nothing where
    /// the room says nothing — an agent's role is the verb on the
    /// capability that admitted it; a peer's comes from the room's own
    /// `Collaborator` list, never a default.
    #[test]
    fn a_row_carries_the_role_the_room_recorded_and_no_other() {
        let mut tree = tree_with_room();
        {
            let ws = tree.active_workspace_mut().expect("fixture has a room");
            ws.collaborators.push(Collaborator::new("peer@7", "peer@7"));
            ws.collaborators
                .push(Collaborator::new("bob", "bob").with_role("editor"));
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"ada".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Comment),
            )
            .expect("the owner may admit an agent");
            ws.collaborators.push(Collaborator::new("peer@9", "scribe"));
        }

        let rows = collaborator_roster_rows(&tree);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            rows[0].role, None,
            "a peer the room recorded nothing about must not acquire a role",
        );
        assert_eq!(rows[1].role.as_deref(), Some("editor"));
        assert_eq!(
            rows[2].role.as_deref(),
            Some("commenter"),
            "an agent's row must show the verb its membership grant carries, \
             which is the whole of what revoking it takes back: {rows:?}",
        );
    }
}
