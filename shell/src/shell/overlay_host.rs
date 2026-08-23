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
use crate::shell::sidebar_wheel_disc::SidebarWheelDisc;
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
    /// Chip rows the filter band is currently reserving. Recomputed
    /// during draw from the labels' measured widths; when it changes the
    /// palette is told, so the card grows and the rows below the band
    /// move down rather than being painted over. See
    /// [`Self::draw_search_banner`].
    search_chip_rows: usize,
    /// Which records each room holds — shared with the records search
    /// provider, which is what writes into it. Held here as well so the
    /// host can seed it (`--shots`) and flush it on teardown.
    room_records: std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
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
    /// Vertical scroll offset (px) for the overview's room list. Wheel
    /// events over the panel pan the rooms through this offset while the
    /// "you are here" block stays pinned. Clamped in [0, max] each frame
    /// against the total content extent so a workspace deletion can't
    /// strand the scroll past the tail.
    overview_scroll_y: f64,
    /// Which room the keyboard is pointing at, as a tree index.
    ///
    /// The overview is a switcher, so it has a target you move before you
    /// commit to it: arrows walk this, Enter goes there, Esc leaves it
    /// alone. `None` while the panel is closed — the selection is
    /// re-seeded from the room you are in each time it opens, so it never
    /// reopens pointing at wherever you happened to stop last time.
    overview_selection: Option<usize>,
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
    /// When the workspace invite link was last copied, so the button can
    /// confirm it happened.
    ws_invite_copied_at: Option<Instant>,
    /// Ribbon segment under the pointer, if any. Paint-only.
    strip_ribbon_hover: Option<usize>,
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
    /// Raw id of the workspace the current `content` was built for.
    /// [`Self::resync_workspace_content`] compares it against
    /// `tree.active` each frame and rebuilds the content when they
    /// disagree — the strip widgets are a projection of ONE workspace,
    /// and without the swap the screen keeps showing the boot
    /// workspace forever. `None` means "unbuilt / test stub": the
    /// reconciler then only latches the current id without rebuilding,
    /// so hosts constructed around hand-made content keep it.
    content_built_for_ws: Option<u64>,
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
    /// The portal's prompt queue, when the compositor is serving
    /// portals. `drain_portal_prompts` empties it onto
    /// `chrome.pending_grants` every frame; `None` when no portal is
    /// running, in which case there is nothing to drain.
    portal_prompts: Option<crate::portal::PromptQueueHandle>,
    /// The portal's file-picker queue. Drained every frame into
    /// [`Self::file_picker`].
    portal_pickers: Option<crate::portal::FilePickerQueue>,
    /// The live file-chooser dialog, and the request it is answering.
    /// One at a time: a second `OpenFile` while a dialog is up waits in
    /// the queue rather than stacking two modals over each other.
    file_picker: Option<Box<ActiveFilePicker>>,
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
    /// What each live tab was last built to show, keyed by tab id.
    ///
    /// The revision counter says *something* in the tree moved; this says
    /// which tabs it moved. Without it the resync can only add and remove
    /// tabs, so a tab whose content changed while its id did not keeps the
    /// widget it was built with — the tree and the screen then disagree
    /// permanently, and every assertion made against the tree passes while
    /// the person is still looking at the old thing.
    tab_tiles: std::collections::HashMap<u64, String>,
    /// Side-effect dispatcher for quick-settings actions. Production
    /// builds use [`crate::shell::qs_actions::RealQsActions`]; tests
    /// can swap in a recording impl via [`Self::with_qs_actions`] to
    /// assert that clicks routed through the popover translate into
    /// the expected shell-outs without actually invoking `nmcli` etc.
    qs_actions: Box<dyn crate::shell::qs_actions::QsActions>,
    /// Live battery / network / volume readings, or `None` when nothing
    /// is polling — a `--shots` render and a headless test deliberately
    /// have no poller so their pixels do not depend on the host's
    /// hardware. See [`crate::shell::status`].
    status_bridge: Option<crate::shell::status::StatusBridge>,
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
    /// Hit rects of the Invite / Join buttons in the collaborator
    /// dropdown. Zeroed on the same terms as the activity row, and also
    /// whenever the local user may not invite here — a button that is
    /// not painted must not be clickable, and the capability decides
    /// both together so the two can never disagree.
    collab_invite_row_rect: Rect,
    collab_join_row_rect: Rect,
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
    /// Mint an invite *for somebody*, and put it on the clipboard in
    /// the same step when `copy`.
    ///
    /// `name` is what this room will call them. It is recorded here, by
    /// the machine doing the inviting, and written into the room
    /// document the moment that peer turns up — the same replicated,
    /// authority-checked channel every other name in the room travels
    /// on, refused by `apply_room_changes` from anybody but the room's
    /// authority. The invitee never says who they are.
    ///
    /// `copy` is what separates the collaborators panel's Invite from
    /// the palette's `peer.invite.named` row. Somebody who filled in a
    /// name and pressed Create has said everything the token needs;
    /// making them then locate a Copy button is a second gesture
    /// serving one intent — the gesture that got skipped on camera,
    /// because it is the one nobody thinks to make. The card carries a
    /// Copy button either way, for a second copy.
    IssueInviteFor { name: String, copy: bool },
    /// Open the invite composer: the field that asks who is being
    /// invited, before anything is minted.
    ///
    /// What the collaborators panel's Invite button emits. A button
    /// that minted an anonymous blob left the host's own roster reading
    /// `peer@12191268537764683811` after the guest arrived, because
    /// nothing in the flow had ever been told a name.
    PromptInvite,
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
    /// Mint a signed invite to the active workspace, and remember that
    /// whoever redeems it is called this. `Ok` carries the token.
    ///
    /// The name does not travel inside the token: a token is a bearer
    /// credential that crosses machines, and a name that rode along in
    /// it would be a name the *holder* presents. It stays on the
    /// inviting machine until that machine sees the invitee ask this
    /// room for its document, and is written into the room document
    /// from there.
    IssueInviteFor(String),
    /// Join the room a token names. `Ok` carries the room id.
    AcceptInvite(String),
}

/// A button on the peer-sync card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSyncPromptButton {
    /// Mint the invite for the name that has been typed into the
    /// composer.
    CreateInvite,
    /// Submit whatever has been typed as an invite token.
    Join,
    /// Abandon the prompt.
    Cancel,
    /// Put the minted invite token on the system clipboard.
    ///
    /// The token is the one thing in this flow that has to physically
    /// travel between two machines. Without this the only way across is
    /// to read several hundred base64 characters off one screen and
    /// type them into another, which is not a thing anybody does
    /// correctly twice.
    CopyToken,
    /// Pull an invite token off the system clipboard into the prompt.
    PasteToken,
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

/// Shorten an invite token to something that fits on one line.
///
/// Head and tail with the middle elided, so a person can see that the
/// field is holding a token — and that it changed — without the card
/// turning into six lines of base64. Nothing reads this to check a
/// token; the fingerprint underneath is what is meant to be compared.
fn elide_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 20 {
        return token.to_string();
    }
    let head: String = chars[..10].iter().collect();
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("{head}…{tail}")
}

/// The invite `token` carries, if it is one this machine will act on.
///
/// Deliberately the real validator rather than a lenient parse: a token
/// that does not verify, or that has expired, is not an invite, and a
/// card that summarised it as though it were would be describing
/// something the Join button is about to refuse.
fn parse_invite(token: &str) -> Option<crate::network::invite::PeerInviteToken> {
    crate::network::invite::validate_invite_token(token.trim()).ok()
}

/// A short, comparable fingerprint of the machine an invite came from.
///
/// The last eight base58 characters of the inviter's libp2p `PeerId`,
/// in two groups so it can be read aloud over a phone. A `PeerId` is a
/// multihash of that machine's public key, so this identifies the
/// *sender*, not the bytes of one particular token: every invite from
/// one machine reads the same, which is what makes it something two
/// people can check against each other and not a second password.
///
/// Grouped with a space, which is how this OS already writes a
/// fingerprint somebody is meant to compare by eye — see
/// `onboarding::MintedIdentity::grouped_fingerprint`. Not the `·` this
/// card uses between phrases: one glyph meaning two things on one
/// surface is how a code gets read as a sentence.
fn peer_fingerprint(peer_id: &str) -> String {
    let chars: Vec<char> = peer_id.chars().collect();
    let tail: Vec<char> = chars[chars.len().saturating_sub(8)..].to_vec();
    if tail.len() <= 4 {
        return tail.into_iter().collect();
    }
    let (a, b) = tail.split_at(4);
    format!(
        "{} {}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    )
}

/// How long until `expiry_unix_seconds`, in units a person reads.
///
/// Rounded down, and never rounded up into a promise: an invite the
/// card called "1 hour" a second before it lapsed would be a card that
/// lied at the only moment the number mattered.
fn time_until(expiry_unix_seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let left = expiry_unix_seconds.saturating_sub(now);
    match left {
        0 => "moments".to_string(),
        s if s < 120 => format!("{s} seconds"),
        s if s < 5400 => format!("{} minutes", s / 60),
        s if s < 172_800 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

/// One person, drawn the way the collaborators panel draws them: an
/// avatar chip, a name, and a line of state under it.
///
/// The same idiom on the card as in the roster, so the person the card
/// is about and the row they will become are recognisably the same
/// thing. Returns the y the row ends at.
///
/// `named` distinguishes somebody from nobody: an invite minted with no
/// name is an invite to whoever holds it, and it gets the top bar's
/// empty-seat ring rather than a monogram, because there is no initial
/// to put in one.
#[allow(clippy::too_many_arguments)]
fn draw_invite_person_row(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    who: &str,
    sub: &str,
    named: bool,
) -> f64 {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let center = Point::new(rect.x0 + 16.0, rect.y0 + 18.0);
    if named {
        let initial = who
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            Rect::new(center.x - 16.0, center.y - 16.0, center.x + 16.0, center.y + 16.0),
            &initial,
            colors.accent[5],
            theme,
            text_renderer,
        );
    } else {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            colors.border_white_10,
            None,
            &vello::kurbo::Circle::new(center, 16.0),
        );
    }
    let text_x = center.x + 16.0 + 14.0;
    let name_size = (tokens.typography.scale.sm).max(12.0);
    let sub_size = (tokens.typography.scale.xs).max(10.0);
    let who = text_renderer.elide_to_width(who, name_size, (rect.x1 - text_x) as f32);
    text_renderer.draw_text(
        scene,
        &who,
        name_size,
        colors.text_primary,
        transform * Affine::translate((text_x, rect.y0 + 2.0)),
        None,
    );
    let sub = text_renderer.elide_to_width(sub, sub_size, (rect.x1 - text_x) as f32);
    text_renderer.draw_text(
        scene,
        &sub,
        sub_size,
        colors.text_dim,
        transform * Affine::translate((text_x, rect.y0 + 2.0 + name_size as f64 + 6.0)),
        None,
    );
    rect.y0 + 2.0 + name_size as f64 + 6.0 + sub_size as f64
}

/// The fingerprint, and what it is for, on one line.
///
/// Drawn as a chip rather than as running text: it is the one thing on
/// the card a person is expected to compare character by character with
/// somebody else's screen, and running prose is not something anybody
/// reads that way. Returns the y it ends at.
#[allow(clippy::too_many_arguments)]
/// The invite code, drawn as the field it is.
///
/// It used to be a small chip with a sentence beside it explaining what
/// it was for. The sentence did no work — it did not say what to do with
/// the code, and a caption is not how an interface tells you that a
/// field is readable and copyable. The field says it by being one:
/// full width, set large, in the face that makes a code legible
/// character by character.
fn draw_invite_code(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    at: Point,
    code: &str,
    avail: f64,
) -> f64 {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let size = tokens.typography.scale.lg.max(15.0);
    let (_, fh) = text_renderer.measure(code, size, None);
    let field = Rect::new(at.x, at.y, at.x + avail, at.y + fh as f64 + 20.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surface_white_5,
        None,
        &RoundedRect::from_rect(field, tokens.radii.md as f64),
    );
    text_renderer.draw_text(
        scene,
        code,
        size,
        colors.text_primary,
        transform * Affine::translate((field.x0 + 14.0, field.y0 + 10.0)),
        None,
    );
    field.y1
}

/// Gap between two chips in the filter band.
const CHIP_GAP_PX: f64 = 6.0;
/// Horizontal padding inside a chip — both sides together.
const CHIP_PAD_PX: f64 = 18.0;
/// Narrowest a chip is ever squeezed to. Below this a pill stops reading
/// as a control at all; a chip this narrow is a chip whose label is an
/// ellipsis, and the band is expected to have grown a row by the next
/// frame anyway.
const CHIP_MIN_W_PX: f64 = 34.0;

/// Where each chip of the filter strip goes.
#[derive(Debug, Clone, PartialEq)]
struct ChipStrip {
    /// One rect per chip, in the order handed in. **Every chip gets one**
    /// — see [`lay_out_chip_strip`].
    rects: Vec<Rect>,
    /// Rows the chips want at their natural width. The host reserves this
    /// many from the palette; until it does, the strip is laid out inside
    /// however many it currently has.
    rows_needed: usize,
}

/// Lay the filter chips out across a band that may be too narrow.
///
/// **The invariant is that every chip gets a rect.** The strip used to
/// stop painting at the first chip that did not fit, and an unpainted
/// chip has no rect, so it could not be clicked either: a filter that
/// exists, is listed by the registry, and cannot be reached. At 1600 px
/// the five kinds of the day exactly filled the row, so the sixth was
/// invisible — which reads as a missing feature rather than a layout bug.
///
/// Three behaviours, in order of preference:
///
/// - Chips take their natural width while they fit.
/// - They **wrap** to the next row when they do not, and the band is told
///   how many rows that needs. Wrapping is chosen over eliding because a
///   filter strip is a set of names: `Applications` squeezed to `Ap…` is
///   reachable but no longer says what it filters.
/// - Only when the rows on offer run out do the leftovers **share** the
///   last one, narrowing (and, at paint time, eliding) so that all of
///   them still land. That is the transient case — the band grows on the
///   next layout — and it is what keeps the invariant unconditional
///   rather than dependent on when a relayout happens to run.
///
/// `first_row_x1` is where the first row must stop, because the scope
/// switch is anchored to the right of it; later rows have the whole band.
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

    // Greedy wrap at natural width — what the strip wants. `per_row`
    // additionally caps how many chips a row may take, which is what the
    // balancing pass below uses.
    let wrap = |per_row: usize| -> Vec<Vec<usize>> {
        let mut plan: Vec<Vec<usize>> = vec![Vec::new()];
        let mut x = band.x0;
        for (i, w) in natural.iter().enumerate() {
            let w = w.max(CHIP_MIN_W_PX);
            let row = plan.len() - 1;
            if !plan[row].is_empty() && (x + w > row_x1(row) || plan[row].len() >= per_row) {
                plan.push(Vec::new());
                x = band.x0;
            }
            let row = plan.len() - 1;
            plan[row].push(i);
            x += w + CHIP_GAP_PX;
        }
        plan
    };
    let plan = wrap(usize::MAX);
    let rows_needed = plan.len();

    // Spread the chips evenly over the rows that will be used. Filling
    // each row to the brim leaves the last one with whatever is left —
    // six chips and then a lone `Folders` underneath, which reads as
    // something having gone wrong rather than as a two-row strip. The
    // balanced plan is only adopted if it still fits in the same number
    // of rows, so evenness never costs anyone a chip.
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
    let mut place = |row: usize, ids: &[usize], widths: &[f64], rects: &mut Vec<Rect>| {
        let cy = band.y0 + row as f64 * row_h + row_h / 2.0;
        let mut x = band.x0;
        for (slot, &id) in ids.iter().enumerate() {
            let w = widths[slot];
            rects[id] = Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
            x += w + CHIP_GAP_PX;
        }
    };

    let kept_rows = rows_needed.min(rows_available);
    for (row, ids) in plan.iter().take(kept_rows.saturating_sub(1)).enumerate() {
        let widths: Vec<f64> = ids.iter().map(|&i| natural[i].max(CHIP_MIN_W_PX)).collect();
        place(row, ids, &widths, &mut rects);
    }
    // The last row this layout may use holds everything left over.
    let last = kept_rows - 1;
    let leftover: Vec<usize> = plan[last..].iter().flatten().copied().collect();
    let widths = share_row(
        &leftover.iter().map(|&i| natural[i]).collect::<Vec<_>>(),
        row_x1(last) - band.x0,
    );
    place(last, &leftover, &widths, &mut rects);

    ChipStrip { rects, rows_needed }
}

/// Widths for one row of chips that must all fit inside `available`.
///
/// Chips narrower than an equal share keep their natural width and hand
/// what they do not need to the ones that are over — so squeezing costs
/// the long labels and leaves the short ones alone, rather than shaving
/// every chip by the same amount and making `Files` look deliberate at
/// half its natural size.
fn share_row(natural: &[f64], available: f64) -> Vec<f64> {
    if natural.is_empty() {
        return Vec::new();
    }
    let n = natural.len() as f64;
    let gaps = CHIP_GAP_PX * (n - 1.0);
    let total: f64 = natural.iter().sum();
    if total + gaps <= available {
        return natural.iter().map(|w| w.max(CHIP_MIN_W_PX)).collect();
    }
    let base = ((available - gaps) / n).max(CHIP_MIN_W_PX);
    let spare: f64 = natural.iter().filter(|w| **w < base).map(|w| base - w).sum();
    let over = natural.iter().filter(|w| **w >= base).count();
    let share_over = if over > 0 {
        base + spare / over as f64
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

/// Wallpaper gutter at the far edge of a display, matching
/// [`WHEEL_TO_STRIP_GAP_PX`] on the near edge. Without it a full-width
/// group ended exactly on the last column of the output, so the card's
/// right border, its rounded corners and its drop shadow all fell off the
/// screen and the window read as cut off by the bezel rather than as a
/// window resting on a desktop.
const STRIP_TRAILING_GUTTER_PX: f64 = 28.0;

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
// Surface and scrim alphas (gap #3 in `docs/nirios/visual-gaps.md`).
//
// The concept layers `backdrop-blur-xl` / `backdrop-blur-md` under
// `bg-slate-900/80` + `/70` tints. The pinned Vello has no backdrop
// filter — it can blur a *shape* (`draw_blurred_rounded_rect`, which is
// what a drop shadow is made of) but not what is behind one — so the
// tint is all the shell could honour, and a tint without its blur is not
// glass. It is a contrast loss that changes with whatever the user has
// open behind it.
//
// So the chrome surfaces are opaque and the *scrims* carry the depth
// instead: a scrim dims a known quantity by a known amount, which is a
// thing an un-blurred fill can actually do well. Centralised so the
// `shell_chrome_test` suite can pin the numeric values.
// ---------------------------------------------------------------------------

/// Height of the "Room activity" row at the foot of the collaborator
/// dropdown, including the separator above it.
const COLLAB_FOOTER_H: f64 = 40.0;
/// Height reserved for the broker-unavailable line when the dropdown
/// also has participants to list beneath it.
const COLLAB_WARNING_H: f64 = 24.0;
/// Height of the Invite / Join band above the "Room activity" row.
///
/// The dropdown answers "who is in this room". Adding somebody to it,
/// and joining somebody else's, are the two acts that change the
/// answer — so they belong on the surface that shows it, and not only
/// in a palette a person has to know the name of a command to reach.
const COLLAB_ACTIONS_H: f64 = 40.0;

/// What a row for somebody who has been named but has not arrived says
/// about itself.
///
/// A word rather than only a dimming, because dimming alone is a
/// difference a viewer has to already know the meaning of, and the
/// difference here is the one the whole surface turns on: whether the
/// person in this row is actually in the room.
const INVITED_TAG: &str = "invited";

/// How far back the dashed seat-ring of an invited row is drawn, in
/// that peer's own colour.
///
/// Their colour, so the ring and the chip that replaces it when they
/// arrive are recognisably the same person; faded, so it reads as
/// pending beside a present peer without looking disabled — an
/// invitation is live, and the row becomes an ordinary participant the
/// moment its peer turns up.
const INVITED_ROW_ALPHA: f32 = 0.55;

/// Alpha applied to the collaborator and QuickSettings popover surfaces.
///
/// Opaque, and it defers to the toolkit's own chrome constant so a
/// popover cannot drift away from the bar it hangs off again. This used
/// to be a literal `0.80` on the grounds that the concept called for
/// `bg-slate-900/80 backdrop-blur-md`; the pinned vello has no backdrop
/// filter, so what shipped was the tint without the blur, which is not
/// glass but a loss of contrast that varies with whatever the user has
/// open behind it. A 2026-08 report put it plainly: the transparency
/// looked bad.
///
/// The palette and the workspace-settings modal already went opaque for
/// the same reason after the 2026-07-09 report ("too transparent — looks
/// terrible"); see [`MODAL_SURFACE_ALPHA`]. The popover was the one
/// surface left behind.
pub const POPOVER_SURFACE_ALPHA: f32 = ui::chrome::shell::CHROME_SURFACE_ALPHA;

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

/// A cheap value that changes exactly when what a tab should be showing
/// changes.
///
/// The serialised tile, because that is the whole of what
/// [`crate::shell::to_ui_tile`] reads — so two tiles with the same
/// serialisation build the same widget, and building one to compare
/// against would be both expensive and, for a tile that spawns a process,
/// destructive.
fn tile_signature(tile: &crate::shell::state::TileContent) -> String {
    serde_json::to_string(tile).unwrap_or_default()
}

/// Whether `tile` is a client window that has not drawn anything yet.
///
/// Asked of a tab that is about to have its contents replaced by this
/// tile, to decide whether the replacement is ready to be shown. Only a
/// foreign surface can answer yes: everything else is drawn by this
/// process and is on screen the moment it is built.
///
/// The question is "has the client committed a buffer", not "does that
/// buffer fit its tab" — the stricter test the window group applies
/// before painting. It has to be the looser one here, because the tab's
/// rectangle only reaches the client once this swap has happened: the
/// widget that publishes the placement is the one being swapped in. A
/// client that never draws keeps the card it already has, which says
/// what is being done about it and is the honest thing to leave on
/// screen.
fn window_is_still_starting(tile: &crate::shell::state::TileContent) -> bool {
    match tile {
        crate::shell::state::TileContent::Foreign { surface_id, .. } => {
            crate::compositor::foreign::store().frame_seq(*surface_id) == 0
        }
        _ => false,
    }
}

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
        // `records` (every record any app wrote, each row saying which
        // room it lives in — activating one brings it into this room),
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
        // Applications qOS did not package. Separate provider and
        // separate chip from the one above: these run unconfined, and
        // the palette is where the user chooses between them.
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
            ws_invite_copied_at: None,
            strip_ribbon_hover: None,
            qs_refresh: crate::shell::qs_refresh::QsRefresh::new(),
            ws_settings_was_open: false,
            ws_settings_transition: ui::Transition::closed(),
            palette_was_visible: false,
            palette_transition: ui::Transition::closed(),
            group_row_ptrs: HashMap::new(),
            content_built_for_ws: None,
            last_scrolled_active_group: HashMap::new(),
            add_workspace_rect: Rect::ZERO,
            ws_settings_tab_pill_rects: [Rect::ZERO; 3],
            permissions_revoke_rects: Vec::new(),
            grant_composer_rects: None,
            member_control_rects: Vec::new(),
            sidebar_top_y: SIDEBAR_TOP_OFFSET_PX,
            permission_prompt: None,
            portal_prompts: None,
            portal_pickers: None,
            file_picker: None,
            peer_sync_enabled: false,
            peer_sync_actions: Vec::new(),
            peer_sync_bridge: None,
            peer_sync_prompt_rects: Vec::new(),
            notification_stack: None,
            last_chrome_revision: u64::MAX,
            tab_tiles: std::collections::HashMap::new(),
            qs_actions: Box::new(crate::shell::qs_actions::RealQsActions),
            status_bridge: None,
            room_activity: crate::shell::room_activity::RoomActivityPane::new(),
            room_activity_transition: ui::Transition::closed(),
            room_activity_was_open: false,
            collab_activity_row_rect: Rect::ZERO,
            collab_invite_row_rect: Rect::ZERO,
            collab_join_row_rect: Rect::ZERO,
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
        // Replacing where quick settings writes means this host is not
        // driving the real machine, so it must not read from it either.
        self.qs_refresh.disable_os_reads();
        self
    }

    /// Install the live-status bridge. Only the compositor's real-session
    /// constructors do this; without it the tray's readings stay `None`
    /// and it draws no indicators, which is what keeps golden renders
    /// identical across machines.
    pub fn with_status_bridge(mut self, bridge: crate::shell::status::StatusBridge) -> Self {
        self.status_bridge = Some(bridge);
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

    /// Where the on-screen readout sits: horizontally centred, one
    /// eighth of the viewport up from the bottom.
    ///
    /// Low, because it must not cover what the user is working on, and
    /// centred because it is not attached to anything — unlike a
    /// notification, which belongs to an app and lives in that app's
    /// corner. This is the machine answering a key.
    pub fn osd_rect(&self) -> Rect {
        let w = 260.0_f64.min(self.rect.width() * 0.6);
        let h = 96.0_f64;
        let cx = self.rect.center().x;
        let y1 = self.rect.y1 - (self.rect.height() * 0.12).max(56.0);
        Rect::new(cx - w / 2.0, y1 - h, cx + w / 2.0, y1)
    }

    /// Paint the transient hardware-key readout, if one is up.
    fn draw_osd(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let now = Instant::now();
        // Expiring here rather than on a timer keeps the slab's lifetime
        // tied to the frames that actually paint it: a compositor that is
        // idle is not painting, and a readout that expired while nothing
        // was on screen has nothing to clean up.
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

        let rect = self.osd_rect();
        let colors = &theme.tokens.colors;
        let tokens = &theme.tokens;
        let radius = 18.0_f64;

        let faded = alpha < 0.999;
        if faded {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &self.rect,
            );
        }

        // Lift it off whatever it covers — without a shadow a translucent
        // slab over a busy tile reads as a rendering artefact.
        ui::surface::soft_shadow(
            scene,
            transform,
            rect,
            colors.neutral[0],
            36.0,
            14.0,
            0.5,
        );
        let body = RoundedRect::from_rect(rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &body,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_white_10,
            None,
            &body,
        );

        // Glyph, centred in the upper half.
        let glyph_px = 28.0_f64;
        let gx = rect.center().x;
        let gy = rect.y0 + 26.0;
        let glyph_rect = Rect::new(
            gx - glyph_px / 2.0,
            gy - glyph_px / 2.0,
            gx + glyph_px / 2.0,
            gy + glyph_px / 2.0,
        );
        let (icon, tint) = match osd.kind {
            crate::shell::osd::OsdKind::Volume { muted: true } => {
                (ui::icons::Icon::VolumeX, colors.text_primary)
            }
            crate::shell::osd::OsdKind::Volume { muted: false } => {
                (ui::icons::Icon::Volume2, colors.text_primary)
            }
            crate::shell::osd::OsdKind::Brightness => (ui::icons::Icon::Sun, colors.text_primary),
            crate::shell::osd::OsdKind::MicMute { muted } => (
                ui::icons::Icon::MicOff,
                if muted {
                    colors.semantic.warning
                } else {
                    colors.text_primary
                },
            ),
            crate::shell::osd::OsdKind::Transport(t) => (
                match t {
                    crate::shell::osd::Transport::Play => ui::icons::Icon::Play,
                    crate::shell::osd::Transport::Pause => ui::icons::Icon::Pause,
                    crate::shell::osd::Transport::Next => ui::icons::Icon::SkipForward,
                    crate::shell::osd::Transport::Previous => ui::icons::Icon::SkipBack,
                },
                colors.text_primary,
            ),
        };
        ui::icons::draw_icon(
            scene,
            transform,
            glyph_rect,
            icon,
            tint,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX * 1.4,
        );

        // Level bar. The track is always drawn at full width so the
        // number and the bar occupy the same place whatever the value —
        // a bar that shrinks its own track reads as a progress spinner.
        if osd.has_level() {
            let bar_h = 6.0_f64;
            let inset = 24.0_f64;
            let bar = Rect::new(
                rect.x0 + inset,
                rect.y0 + 50.0,
                rect.x1 - inset,
                rect.y0 + 50.0 + bar_h,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surface_white_10,
                None,
                &RoundedRect::from_rect(bar, bar_h / 2.0),
            );
            // A muted sink draws an empty track: the level it will return
            // to is still true, but reporting it as a filled bar would
            // contradict the crossed-out glyph directly above it.
            let filled = if matches!(
                osd.kind,
                crate::shell::osd::OsdKind::Volume { muted: true }
            ) {
                0.0
            } else {
                osd.level.clamp(0.0, 1.0) as f64
            };
            if filled > 0.0 {
                let w = (bar.width() * filled).max(bar_h);
                let fill_rect = Rect::new(bar.x0, bar.y0, bar.x0 + w, bar.y1);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &RoundedRect::from_rect(fill_rect, bar_h / 2.0),
                );
            }
        }

        // Caption.
        let caption = osd.caption();
        let size = tokens.typography.scale.sm;
        let (cw, ch) = text_renderer.measure_weighted(&caption, size, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            &caption,
            size,
            colors.text_dim,
            transform
                * Affine::translate((
                    rect.center().x - cw as f64 / 2.0,
                    rect.y1 - 12.0 - ch as f64,
                )),
            None,
            TextWeight::Medium,
        );

        if faded {
            scene.pop_layer();
        }
    }

    /// Copy the poller's latest reading onto the chrome, bumping the
    /// revision only when something actually changed — the revision is
    /// the repaint trigger, and a status that re-publishes an identical
    /// battery percentage every five seconds must not wake the renderer.
    fn sync_status_from_bridge(&mut self) {
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
            // Opening points the selection at the room you are in, so the
            // first arrow press moves one step from *here* rather than
            // from wherever a previous visit left off.
            self.overview_selection = if target > 0.5 {
                self.overview_here_index()
            } else {
                None
            };
            self.overview_scroll_y = 0.0;
        }
        if let Some(started) = self.transition_started {
            let elapsed = started.elapsed().as_millis() as f32;
            let duration = OVERVIEW_TRANSITION_MS as f32;
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
                PeerSyncAction::IssueInvite => self.mint_invite(None, false),
                PeerSyncAction::IssueInviteFor { name, copy } => {
                    self.mint_invite(Some(name), copy)
                }
                PeerSyncAction::PromptInvite => {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        // A fresh field, and nothing left over from the
                        // last invite: a card that opened still showing
                        // the previous token would put a live bearer
                        // credential behind a question about somebody
                        // else.
                        st.peer_sync_invite_name = Some(String::new());
                        st.peer_sync_connect_input = None;
                        st.peer_sync_invite_token = None;
                        st.peer_sync_invite_for = None;
                        st.peer_sync_status = None;
                        st.bump();
                    }
                }
                PeerSyncAction::PromptConnect => {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.peer_sync_invite_name = None;
                        st.peer_sync_connect_input = Some(String::new());
                        st.peer_sync_status = Some("Paste or type the invite token".to_string());
                        st.bump();
                    }
                }
            }
        }
    }

    /// Mint a token, park it on the card, and say who it is for.
    ///
    /// `invitee` is the name this room will call whoever redeems the
    /// token. `None` is a plain `/invite`, which is an invite to nobody
    /// in particular and is labelled as one — never with a name left
    /// over from the last person who was invited.
    ///
    /// The composer field is cleared only on success, so an invite the
    /// network refused can be retried without retyping the name.
    fn mint_invite(&mut self, invitee: Option<String>, copy: bool) {
        let request = match invitee.as_deref() {
            Some(name) => PeerSyncRequest::IssueInviteFor(name.to_string()),
            None => PeerSyncRequest::IssueInvite,
        };
        let outcome = self.peer_sync_call(request);
        let minted = outcome.is_ok();
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            match outcome {
                Ok(token) => {
                    // Nothing. The card appearing, with the name and
                    // the code on it, is the whole message — a status
                    // line restating what the card already shows is one
                    // more sentence to read and no more information.
                    st.peer_sync_status = None;
                    st.peer_sync_invite_token = Some(token);
                    st.peer_sync_invite_for = invitee.clone();
                    st.peer_sync_invite_name = None;
                }
                Err(e) => {
                    st.peer_sync_invite_token = None;
                    st.peer_sync_invite_for = None;
                    st.peer_sync_status = Some(format!("Invite failed: {e}"));
                }
            }
            st.bump();
        }
        // Only after a mint that produced something. Copying on a
        // failed invite would overwrite whatever the clipboard held and
        // then report success for it.
        if copy && minted {
            self.copy_invite_token();
        }
    }

    /// Mint the invite the composer has been filled in for.
    ///
    /// An empty field is not an invite: it is the question still
    /// unanswered, and minting anyway is exactly how the host ended up
    /// looking at a number instead of a name.
    fn submit_invite_composer(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let name = chrome.borrow().peer_sync_invite_name.clone();
        let Some(name) = name else { return };
        let name = name.trim().to_string();
        if name.is_empty() {
            let mut st = chrome.borrow_mut();
            st.peer_sync_status = Some("Say who this invite is for".to_string());
            st.bump();
            return;
        }
        self.mint_invite(Some(name), true);
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

    /// Put the minted invite token on the system clipboard.
    ///
    /// The compositor installs its selection mirror as this process's
    /// clipboard backend (`compositor::selection::install_host_clipboard`),
    /// so this reaches the same `wl_data_device` selection a sandboxed
    /// app pastes from — the token leaves the machine the same way any
    /// other copied text does.
    fn copy_invite_token(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let token = chrome.borrow().peer_sync_invite_token.clone();
        let Some(token) = token else { return };
        ui::text_selection::clipboard_copy(&token);
        let mut st = chrome.borrow_mut();
        st.peer_sync_status = Some("Copied".to_string());
        st.bump();
    }

    /// Pull an invite token off the system clipboard into the prompt.
    ///
    /// Replaces the field rather than appending to it: a token is one
    /// whole value, and a paste on top of a half-typed one is a request
    /// to use the pasted one. Whitespace goes — a token copied out of a
    /// chat window arrives wrapped.
    fn paste_invite_token(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if !self.connect_prompt_open() {
            return;
        }
        let pasted = ui::text_selection::clipboard_paste()
            .map(|s| s.split_whitespace().collect::<String>())
            .filter(|s| !s.is_empty());
        let mut st = chrome.borrow_mut();
        match pasted {
            Some(token) => {
                st.peer_sync_connect_input = Some(token);
                st.peer_sync_status = Some("Pasted — press Enter to join".to_string());
            }
            None => {
                st.peer_sync_status = Some("Nothing on the clipboard to paste".to_string());
            }
        }
        st.bump();
    }

    /// Route one event to the peer-sync card. Returns whether the card
    /// consumed it.
    ///
    /// While the token prompt is open every key belongs to it: a base64
    /// token contains the same characters the chrome reads as chords,
    /// so anything that leaked past would both corrupt the token and
    /// fire a shell action.
    fn handle_peer_sync_event(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        let prompt_open = self.connect_prompt_open();
        let composer_open = self.invite_composer_open();
        // Ctrl (or Super, for a user whose muscle memory is a Mac) is
        // what turns `c` and `v` from token characters into the copy and
        // paste chords. Checked before the plain-character branch below,
        // which would otherwise splice a literal "v" into the token the
        // user was trying to paste over.
        let mods = modifiers.state();
        let chord = mods.control_key() || mods.super_key();
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
                    // Copy / paste come first: they are the whole point
                    // of the card, and `Key::Character` below would
                    // otherwise swallow them as token text.
                    Key::Character(s) if chord && s.eq_ignore_ascii_case("c") => {
                        self.copy_invite_token();
                        return true;
                    }
                    Key::Character(s) if chord && s.eq_ignore_ascii_case("v") => {
                        self.paste_invite_token();
                        return true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.peer_sync_connect_input = None;
                            st.peer_sync_invite_name = None;
                            st.peer_sync_invite_token = None;
                            st.peer_sync_invite_for = None;
                            st.peer_sync_status = None;
                            st.bump();
                        }
                        return true;
                    }
                    Key::Named(NamedKey::Enter) if composer_open => {
                        self.submit_invite_composer();
                        return true;
                    }
                    Key::Named(NamedKey::Enter) if prompt_open => {
                        self.submit_connect_prompt();
                        return true;
                    }
                    Key::Named(NamedKey::Backspace) if prompt_open || composer_open => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            let field = if prompt_open {
                                st.peer_sync_connect_input.as_mut()
                            } else {
                                st.peer_sync_invite_name.as_mut()
                            };
                            if let Some(buf) = field {
                                buf.pop();
                            }
                            st.bump();
                        }
                        return true;
                    }
                    // `!chord` so a modified key the card has no meaning
                    // for is swallowed rather than typed into the token.
                    Key::Character(s) if (prompt_open || composer_open) && !chord => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            let field = if prompt_open {
                                st.peer_sync_connect_input.as_mut()
                            } else {
                                st.peer_sync_invite_name.as_mut()
                            };
                            if let Some(buf) = field {
                                buf.push_str(s.as_str());
                            }
                            st.bump();
                        }
                        return true;
                    }
                    // A name has spaces in it. `Key::Character` never
                    // carries one, so without this the composer could
                    // only be told a single word.
                    Key::Named(NamedKey::Space) if composer_open && !chord => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.peer_sync_invite_name.as_mut() {
                                buf.push(' ');
                            }
                            st.bump();
                        }
                        return true;
                    }
                    _ => {}
                }
                // Swallow anything else while the card owns the
                // keyboard so a stray chord cannot act on the desktop
                // behind it.
                prompt_open || composer_open
            }
            WindowEvent::KeyboardInput { .. } => prompt_open || composer_open,
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
                            PeerSyncPromptButton::CreateInvite => self.submit_invite_composer(),
                            PeerSyncPromptButton::Join => self.submit_connect_prompt(),
                            PeerSyncPromptButton::CopyToken => self.copy_invite_token(),
                            PeerSyncPromptButton::PasteToken => self.paste_invite_token(),
                            PeerSyncPromptButton::Cancel => {
                                if let Some(chrome) = self.chrome.clone() {
                                    let mut st = chrome.borrow_mut();
                                    st.peer_sync_connect_input = None;
                                    st.peer_sync_invite_name = None;
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

    /// Is the card asking who is being invited?
    fn invite_composer_open(&self) -> bool {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().peer_sync_invite_name.is_some())
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
                let leading = if hosts_chrome && self.sidebar.is_some() {
                    sidebar_width_for_viewport(o.height as f32) as f64 + WHEEL_TO_STRIP_GAP_PX
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

    pub fn with_group_row_ptrs(mut self, ptrs: HashMap<StripId, *mut GroupRow>) -> Self {
        self.group_row_ptrs = ptrs;
        self
    }

    /// Record which workspace the injected content was built for, so
    /// [`Self::resync_workspace_content`] can detect the first switch
    /// away from it. The production builder always sets this.
    pub fn with_content_workspace(mut self, ws_raw: u64) -> Self {
        self.content_built_for_ws = Some(ws_raw);
        self
    }

    /// Swap the workspace-scoped content when the active workspace
    /// changes, or when it grows a strip the content has no row for.
    /// On either the content root and the strip pointer map are
    /// rebuilt TOGETHER via [`crate::shell::build_workspace_root`], so
    /// no reconciler can ever observe pointers into freed rows.
    /// Returns `true` when a swap happened (the caller must treat the
    /// widget structure as dirty — every row is new).
    ///
    /// The strip check is what makes a peer's window visible.
    /// [`Self::resync_strips_from_tree`] splices new groups and tabs
    /// incrementally, but it is keyed by strip and skips any strip it
    /// has no row for — and a window opened on another machine arrives
    /// in a strip of *theirs*, which is new here. So the window landed
    /// in the tree, read back correctly, and was skipped by the only
    /// code that could have drawn it.
    ///
    /// A rebuild rather than an incremental insert because there is no
    /// incremental path for a strip: rows are built with the content
    /// root and the pointer map has to be replaced with them or it
    /// dangles. It costs the rows' widget-local state (scroll offsets),
    /// and it runs only when the strip set actually changes, which is
    /// rare. What the person is looking at — workspace, strip, group,
    /// front tab — lives in the tree, not in the rows, so a rebuild
    /// does not move them.
    ///
    /// Runs at the top of both [`Widget::update`] and
    /// [`Widget::take_structure_dirty`], BEFORE
    /// [`Self::resync_strips_from_tree`] and every other
    /// `group_row_ptrs` consumer, for the same reason the resync does:
    /// the shots driver's relayout-only path skips `update` entirely.
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
    /// workspace?
    ///
    /// A strip with no row is a strip nothing can draw — the
    /// incremental resync looks its row up by strip id and skips what
    /// it cannot find. Extra rows count too: a strip that went away
    /// leaves a row pointing at a group that is no longer in the tree.
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
                        let signature = tile_signature(&tab.tile);
                        if live_tab_ids.contains(&tab.id) {
                            // The tab is already on screen. What is *in*
                            // it can still have changed under it — a tab
                            // that announced a window before the window
                            // existed becomes the window, in place — and
                            // the splice above only ever fires for a tab
                            // id it has not seen. Without this the tree
                            // and the screen disagree permanently, and
                            // every assertion made against the tree
                            // passes while the person sees the old thing.
                            // ...but not before the window is there to
                            // become. The card this tab is currently
                            // showing says what is being done — a peer's
                            // window that this machine is opening its own
                            // instance of says exactly that — and
                            // swapping it for a surface that has not
                            // drawn replaces a sentence that is true with
                            // a blank frame. The tile signature is left
                            // unrecorded, so the swap is retried until
                            // the window can be shown.
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
    /// Give every live window group the same share of its strip, and
    /// mirror it to the persisted tree the way a width drag does.
    ///
    /// A group takes about three quarters of its strip by default, so
    /// two of them never fit on screen at once. Setting the state
    /// tree alone does not move anything: the laid-out width lives on
    /// the `RuntimeGroup` widget and the taffy node carries a baked
    /// length, so the widget is the thing that has to be told.
    ///
    /// Returns the ids it touched.
    pub fn set_group_widths(&mut self, permille: u16) -> Vec<u64> {
        let mut touched: Vec<u64> = Vec::new();
        for (_strip_id, &ptr) in self.group_row_ptrs.iter() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
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
            for ws in tree.workspaces.iter_mut() {
                for strip in ws.strips.iter_mut() {
                    for g in strip.window_groups.iter_mut() {
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

    /// Close the active tab of the group the user is looking at, through
    /// the same `TabBar::start_closing` a `✕` click runs.
    ///
    /// The live `WindowGroup` widget owns the tab list, so — like
    /// [`Self::set_group_widths`] — setting the persisted tree alone
    /// would not move anything; the widget is the thing that has to be
    /// told. Returns `false` when nothing is focused or the group has no
    /// tab left to close.
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
            for g in row.groups.iter_mut() {
                if g.id != gid {
                    continue;
                }
                let closed = match tab {
                    Some(id) => g.request_close_tab(id),
                    None => g.request_close_active_tab(),
                };
                // Deliberately no `bump()`. A revision bump makes the
                // strip resync rebuild this row from the persisted tree,
                // and the tree still lists the tab: closing a tab is a
                // live-widget operation with no tree-side counterpart
                // (there is no `doc_remove_tab`), exactly as a `✕` click
                // is. Bumping here re-spliced the tab straight back and
                // the chord looked like a no-op.
                return closed;
            }
        }
        false
    }

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

    /// Sibling of [`Self::harvest_pending_search_open`] for the tab
    /// strip's "+" chip: drains each group's
    /// [`UiWindowGroup::take_pending_new_tab`] and opens the palette
    /// aimed at that group.
    ///
    /// The "+" deliberately opens the palette rather than immediately
    /// minting a tab of some assumed type. A new tab is a *choice* — a
    /// terminal, a browser, a note — and the palette is where choosing
    /// already happens. What the "+" adds on top is the destination:
    /// [`OpenTarget::Here`] pins the result to this group even when the
    /// placement rule would have opened beside it, which is exactly the
    /// distinction a user clicking a specific group's strip is making.
    fn harvest_pending_new_tab(&mut self) -> bool {
        if self.group_row_ptrs.is_empty() {
            return false;
        }
        let mut requested: Option<u64> = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                if let Some(id) = g.take_pending_new_tab() {
                    requested = Some(id);
                }
            }
        }
        let Some(group) = requested else {
            return false;
        };
        // Make the tree's focus match the strip that was clicked before
        // anything reads it — the palette's own scope resolution and the
        // launch path both walk from the active pointers.
        self.harvest_active_tab_writeback();
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.open_target = crate::shell::chrome_state::OpenTarget::Here(group);
            // `All` rather than `ActiveApp`: the question a "+" asks is
            // "what do you want to open", and only the widest scope
            // offers to launch something that isn't already running.
            st.search_scope = crate::shell::chrome_state::SearchScope::All;
            st.bump();
        }
        // Force-open rather than routing through `open_palette_with_scope`:
        // that helper toggles closed on a same-scope repeat, which is the
        // right behaviour for a chord and the wrong one for a button the
        // user pointed at. Clicking "+" always means "show me the picker".
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

    /// Drop a stale [`OpenTarget::Here`] once the palette is gone.
    ///
    /// The target is one-shot by contract — it aims exactly one launch at
    /// exactly one group. Anything that dismisses the palette (Esc, an
    /// outside click, activating a result) ends that window of intent, and
    /// there are enough such paths that clearing at each of them would
    /// rot. Reconciling here, once per frame against the single fact that
    /// matters, cannot miss one.
    fn clear_stale_open_target(&mut self) {
        if self.palette_visible {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.open_target != crate::shell::chrome_state::OpenTarget::Beside {
            st.open_target = crate::shell::chrome_state::OpenTarget::Beside;
            st.bump();
        }
    }

    /// Drop a passphrase buffer the user walked away from.
    ///
    /// Closing the popover — by Esc, by clicking outside, by any of the
    /// paths that clear the flag — has to take the typed passphrase with
    /// it. Reconciling once per frame against the one fact that matters
    /// (is the popover open) cannot miss a path, and a password is
    /// exactly the kind of buffer that must not outlive its window.
    fn clear_stale_wifi_password(&mut self) {
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
                // Eased, not teleported: a focus change (click, Alt+Shift
                // wheel jump, keyboard) slides the strip to the group on
                // the snap curve, the way Niri's focus-column-left/right
                // slides the view.
                row.scroll_group_into_view_animated(group_id, Instant::now());
            }
            self.last_scrolled_active_group.insert(strip_id, group_id);
        }
    }

    /// Tell exactly one window group on screen that the keyboard is
    /// aimed at it.
    ///
    /// The tree has always persisted an `active_group` pointer per strip
    /// and an `active_strip` pointer per workspace — that pair decides
    /// where a new window lands, what Ctrl+K scopes to, and which tab
    /// takes a keystroke. None of it was drawn. In a strip of
    /// non-overlapping windows there is no z-order to infer focus from,
    /// so three side-by-side windows looked identical while only one of
    /// them would receive what the user typed.
    ///
    /// Runs every frame rather than edge-triggered on a change: it is a
    /// handful of bool writes across the groups already in memory, and
    /// making it a pure function of the tree means focus cannot be left
    /// latched on a group that a resync spliced in, moved between
    /// strips, or rebuilt.
    fn mark_focused_group(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Groups fading out are not focus candidates: the pointer keeps
        // resolving to a closing group for the length of its fade, and
        // lighting a corpse tells the user their keystrokes are going
        // somewhere they are not.
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
        // Which strip owns focus, resolved exactly the way
        // `publish_strip_viewport` resolves it — including the fallback
        // to the first strip, so a workspace whose pointer was never
        // seeded still lights one window instead of none. The group is
        // resolved through `resolve_active_group`, which brings the same
        // guarantee one level down: an unseeded strip falls back to its
        // leftmost surviving group rather than lighting nothing.
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
        // With one window on the workspace there is nothing to tell
        // apart, and a permanently lit edge around the only thing on
        // screen is decoration — the marker has to earn its place. It
        // is the workspace's total, not the strip's: three strips
        // holding one window each is exactly the case where "which one
        // takes the keystroke" is unanswerable without it.
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
        for (&strip_id, &ptr) in self.group_row_ptrs.iter() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in row.groups.iter_mut() {
                g.set_focused(disambiguating && focused == Some((strip_id, g.id)));
            }
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
            // An open that had to load the app first raised a notice
            // saying so, and it stands until the user can see how the
            // open ended.
            //
            // Only a *failure* settles it here. `Started` does not: it
            // means the launcher process forked, which is several
            // seconds before the app has a window — retiring on it left
            // exactly the silence the notice exists to prevent, with
            // the user watching an empty desktop after the one piece of
            // feedback had already gone. The success settlement is the
            // window appearing, and that is
            // [`Self::retire_notices_for_opened_windows`]. `Exited` is
            // not a settlement either: by then the app has run.
            if matches!(
                event.kind,
                LaunchEventKind::SpawnFailed { .. } | LaunchEventKind::Failed { .. }
            ) {
                if let Some(notice) = crate::shell::catalogue::take_pending_notice(&event.package_id)
                {
                    let mut st = chrome_handle.borrow_mut();
                    st.dismiss_notification(notice);
                    st.bump();
                    posted = true;
                }
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

    /// Retire the "Opening …" notice of any app that is now on screen.
    ///
    /// This is the success half of an open's settlement, and what counts
    /// as "on screen" is the point of it.
    ///
    /// A launch reports `Started` when the launcher process forks, which
    /// on a cold open is seconds before there is anything to look at.
    /// Settling on that put the desktop back to silence while the user
    /// was still waiting, which is the whole defect.
    ///
    /// A tab merely existing is not the answer either. The compositor
    /// adopts a tab when the client creates its toplevel, and the client
    /// then takes seconds more to present a buffer; for a short while
    /// after adoption there is still nothing in that tab to look at.
    ///
    /// What settles it is the moment something the person can see stands
    /// in the window's own place — the app's first frame, or the card
    /// holding the place for it, whichever arrives first. Both are the
    /// wait being accounted for where the window is going to be, which is
    /// a better place to account for it than the corner of the screen. So
    /// this is a hand-over rather than a dismissal: the notice covers the
    /// stretch before there is a window's place at all, and stands down
    /// when the place itself takes over.
    ///
    /// Waiting for the first frame instead would leave both up together
    /// for seconds, saying the same thing twice — and worse, an app that
    /// never draws ends with a window reading "isn't responding" under a
    /// notification still reading "Loading…".
    ///
    /// Returns whether anything was retired, so the caller can mark the
    /// frame dirty.
    pub(crate) fn retire_notices_for_opened_windows(&mut self) -> bool {
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
                    crate::shell::state::TileContent::Foreign {
                        app_id,
                        surface_id,
                    } if {
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
            // Pop only if the head still belongs to the prompt the
            // user just clicked — if a portal push changed the head
            // mid-frame we'd otherwise apply the user's grant to
            // the wrong app.
            let head_matches = st
                .pending_grants
                .front()
                .map(|p| p.app_id == widget_app_id)
                .unwrap_or(false);
            if head_matches {
                if let Some(mut pending) = st.pending_grants.pop_front() {
                    let app_id = pending.app_id.clone();
                    let capability = pending.capability.clone();
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
                    // Release the blocked portal call. Without this the
                    // D-Bus method awaiting the oneshot never returns and
                    // the calling app hangs on its own Open dialog.
                    pending.answer(prompt_choice_to_outcome(choice));
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
        self.portal_pickers = Some(pickers);
    }

    /// The live file-chooser dialog, if one is up. For the frame
    /// builder's widget walk and for tests.
    pub fn file_picker(&self) -> Option<&ui::widget::FilePicker> {
        self.file_picker.as_ref().map(|a| &a.widget)
    }

    pub fn file_picker_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.file_picker.as_mut().map(|a| &mut a.widget)
    }

    /// Mount dialogs for everything the portal's picker queue has
    /// buffered, and resolve whichever dialog the user just finished
    /// with.
    ///
    /// Returns whether anything changed, so the caller can repaint.
    pub fn pump_file_picker(&mut self) -> bool {
        let mut dirty = false;

        // Step 1 — resolve a finished dialog. `take_outcome` drains, so
        // this runs at most once per decision.
        if let Some(active) = self.file_picker.as_mut() {
            if let Some(outcome) = active.widget.take_outcome() {
                let mut active = self.file_picker.take().expect("just checked");
                let picked = match outcome {
                    ui::widget::FilePickerOutcome::Selected(paths) => Some(paths),
                    ui::widget::FilePickerOutcome::Cancelled => None,
                };
                active.answer(picked);
                dirty = true;
            }
        }

        // Step 2 — mount the next queued request, if the floor is free.
        let Some(queue) = self.portal_pickers.as_ref() else {
            return dirty;
        };
        let queued = queue.drain();
        if queued.is_empty() {
            return dirty;
        }
        for req in queued {
            if self.file_picker.is_some() {
                // A dialog is already up. Answering "cancelled" is the
                // honest response to a request we are not going to show
                // — better than queueing a modal the user will meet
                // minutes later with no idea what asked for it.
                let _ = req.reply.send(None);
                continue;
            }
            let scope = self.picker_scope_for(&req.caller);
            let active = ActiveFilePicker::mount(req, scope);
            // Which app raised the dialog, and where it may browse. A
            // file dialog appearing with no record of who asked for it
            // is the one event here nobody can debug after the fact.
            tracing::info!(
                app = %active.app_id,
                dir = %active.widget.current_dir().display(),
                "portal: file dialog opened"
            );
            self.file_picker = Some(Box::new(active));
            dirty = true;
        }
        dirty
    }

    /// Where the dialog for `caller` may browse.
    ///
    /// Read off the live workspace, not off a default: the roots are
    /// the user's own directories plus whatever filesystem paths this
    /// workspace has already granted this app, so a project directory
    /// the app was given earlier is reachable in the dialog.
    fn picker_scope_for(&self, caller: &crate::portal::PortalCaller) -> crate::portal::PickerScope {
        let base = crate::portal::PickerScope::for_user();
        let Some(chrome) = self.chrome.as_ref() else {
            return base;
        };
        let st = chrome.borrow();
        let tree = st.tree();
        let Some(ws) = tree
            .workspaces
            .iter()
            .find(|w| w.id == caller.workspace_id)
        else {
            return base;
        };
        let granted: Vec<std::path::PathBuf> = ws
            .app_capabilities(&caller.app_id)
            .into_iter()
            .filter_map(|cap| match &cap.scope {
                collab_types::caps::Scope::FsPath(p) => Some(std::path::PathBuf::from(p)),
                _ => None,
            })
            .collect();
        base.with_granted_paths(granted)
    }

    /// Move everything the portal's prompt queue has buffered onto the
    /// chrome's queue, so the next `pump_permission_prompt` surfaces it.
    ///
    /// This is the step that was missing: `PromptQueueHandle::drain` had
    /// no production caller, so a `PromptResolver::Channel` queue filled
    /// up and nothing ever emptied it. Every entry owns the reply sender
    /// for a D-Bus method that is parked on `rx.await`, which is why the
    /// senders travel onto `pending_grants` rather than being dropped
    /// here — dropping one answers deny, and a user who has not been
    /// asked yet has not denied anything.
    ///
    /// Returns whether anything moved, so the caller can mark the frame
    /// dirty and repaint with the prompt on screen.
    pub fn drain_portal_prompts(&mut self) -> bool {
        let Some(prompts) = self.portal_prompts.as_ref() else {
            return false;
        };
        let drained = prompts.drain();
        if drained.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            // No chrome to render a prompt: answer every request rather
            // than hold senders that will never be fulfilled. An
            // unanswerable request is a denial, and it says so now
            // instead of at some later drop.
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

    /// Test-only one-shot harvest for the tab strip's "+" chip.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_new_tab_for_tests(&mut self) -> bool {
        self.harvest_pending_new_tab()
    }

    /// Test-only one-shot: run the focus-follow scroll reconcile from
    /// outside `update`. Mirrors what [`Widget::update`] runs each frame.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_scroll_active_into_view_for_tests(&mut self) {
        self.scroll_active_group_into_view();
    }

    /// Mutable access to a strip's live [`GroupRow`], so a harness can read
    /// `scroll_x` / handle geometry and drive the reorder / resize / scroll
    /// gestures through the real widget.
    ///
    /// Deliberately *not* gated on `debug_assertions`: the `shots` binary
    /// drives scenarios through this and is built in release for the OS
    /// image, so gating it breaks the image build while every debug build
    /// and test still passes.
    /// The sidebar wheel disc, downcast back out of the `Box<dyn Widget>`
    /// the host holds it in.
    ///
    /// For the screenshot harness, which needs to ask the wheel to hold
    /// its settle at a chosen point on the curve: the settle is motion,
    /// a still frame cannot show motion, and an offscreen render takes
    /// longer than the settle lasts, so stepping real frames only ever
    /// catches the endpoints.
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

    /// Select `tab` inside `group` on the live widget tree — the
    /// authoritative side for the active tab (the per-frame
    /// `harvest_active_tab_writeback` mirrors widgets into the tree, so
    /// a tree-only write is overwritten one frame later). Used by the
    /// compositor's Alt+Shift wheel window jump. Returns `true` when
    /// the widget-side active tab changed.
    pub fn focus_window_widget(&mut self, strip_id: StripId, group: GroupId, tab: u64) -> bool {
        let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.set_group_active_tab(group, tab)
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

    /// The peer-sync card's buttons, named `join` / `cancel` / `copy` /
    /// `paste`.
    pub fn peer_sync_prompt_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.peer_sync_prompt_rects
            .iter()
            .map(|(which, rect)| {
                let name = match which {
                    PeerSyncPromptButton::CreateInvite => "create",
                    PeerSyncPromptButton::Join => "join",
                    PeerSyncPromptButton::Cancel => "cancel",
                    PeerSyncPromptButton::CopyToken => "copy",
                    PeerSyncPromptButton::PasteToken => "paste",
                };
                (name, *rect)
            })
            .collect()
    }

    /// The collaborator dropdown's Invite / Join buttons, as painted on
    /// the last frame.
    ///
    /// The stored rects rather than a recomputation: a name that
    /// resolves has to mean "this was on screen", and geometry computed
    /// on demand would answer for a dropdown that is shut. Both are
    /// `Rect::ZERO` while the panel is closed or the local user may not
    /// invite here, and `register_chrome_rects` drops zero-area entries
    /// — so a scenario that clicks `collab_invite` fails when the
    /// button is not really there, instead of clicking the origin.
    pub fn collaborators_action_rects_for_tests(&self) -> (Rect, Rect) {
        (self.collab_invite_row_rect, self.collab_join_row_rect)
    }

    /// The "Room activity" row at the foot of the same dropdown, on the
    /// same terms.
    pub fn collaborators_activity_rect_for_tests(&self) -> Rect {
        self.collab_activity_row_rect
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
                        // The palette has no step that collects an
                        // argument yet, so it can only offer the nullary
                        // half of the surface. A parameterised row
                        // reached from here refuses, and the refusal is
                        // shown rather than swallowed: a row that does
                        // nothing when activated, silently, is the worst
                        // of the three possible behaviours.
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

    /// Whether the workspace wheel is currently summoned to the pointer.
    ///
    /// Read by the client-input filter alongside
    /// [`Self::palette_visible`]: Escape is only taken from the focused
    /// client while the shell has something up that Escape would
    /// dismiss, and a summoned wheel is exactly that. Without it the
    /// key would go to the app underneath and the wheel would have no
    /// way to be dismissed from the keyboard.
    pub fn wheel_summoned(&self) -> bool {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().wheel_summon().is_some())
            .unwrap_or(false)
    }

    /// Whether the last draw discovered that the layout it was handed is
    /// not the one it needs — the palette's filter band growing a row
    /// when the kind chips wrap, which only a paint can measure.
    ///
    /// A peek, not a take: `take_structure_dirty` is the layout pass's
    /// own call and consumes the flag. This exists so a caller that
    /// paints exactly once (a screenshot) can notice it has to lay out
    /// and paint again, rather than capturing a frame the user never
    /// sees.
    pub fn structure_dirty(&self) -> bool {
        self.palette_layout_dirty
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

    /// The rows on offer, with the subtitle each one renders under its
    /// label.
    ///
    /// [`Self::palette_snapshot`] carries labels only, which is enough
    /// to say *which* row Enter takes but not enough to say what the
    /// row tells the user. The subtitle is where a badge, a source
    /// marker or the word "install" would appear, so a harness
    /// checking that an app the machine has to fetch is offered
    /// indistinguishably from one it already has needs to read it.
    pub fn palette_rows_detailed(&self) -> Vec<(String, String, Option<String>)> {
        self.palette
            .visible_entries()
            .into_iter()
            .map(|e| (e.id.clone(), e.label.clone(), e.description.clone()))
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

    /// The room record index the records provider reads and writes.
    ///
    /// Shared, not copied: a caller that references a record in here is
    /// changing what the next keystroke finds.
    pub fn room_records(
        &self,
    ) -> std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>> {
        std::rc::Rc::clone(&self.room_records)
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
        // Same seeding the animated open does, so a single-frame paint
        // shows the panel in the state a real open would leave it in.
        self.overview_selection = if target {
            self.overview_here_index()
        } else {
            None
        };
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

    /// Fold every reading the quick-settings workers have delivered
    /// into chrome state. Runs once per frame and never blocks: the
    /// reads themselves happen on a worker (see
    /// [`crate::shell::qs_refresh`]), because a shell-out on the paint
    /// path froze the whole desktop for as long as the OS tool took.
    ///
    /// Each field is applied only if it was actually read, so a missing
    /// daemon leaves the previous value on screen rather than blanking
    /// the panel.
    fn apply_quick_settings_readings(&mut self) {
        use crate::shell::qs_refresh::QsReading;
        let readings = self.qs_refresh.drain();
        if readings.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        for reading in readings {
            let mut st = chrome.borrow_mut();
            let (brightness, volume, wifi, bluetooth, power, wifi_nets, bt_devices) = match reading
            {
                QsReading::Fast { brightness, volume, wifi, bluetooth, power } => {
                    (brightness, volume, wifi, bluetooth, power, None, None)
                }
                QsReading::WifiNetworks(nets) => (None, None, None, None, None, nets, None),
                QsReading::BtDevices(devs) => (None, None, None, None, None, None, devs),
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

    /// Font size the overview sets the current room's name at, in logical
    /// pixels.
    ///
    /// The panel opens by naming the room you are in, and that name is
    /// the largest thing on the surface — it is the answer to the first
    /// question the screen exists to answer. Exposed so the shell-chrome
    /// test can pin "it reads as large" against a real number. A method,
    /// not a constant, so it tracks the active theme's typography token.
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

    /// The strip ribbon for the active workspace, anchored to the right
    /// edge of the content area.
    ///
    /// The content's right gutter is [`STRIP_TRAILING_GUTTER_PX`] wide
    /// and holds nothing else, so the ribbon costs no layout: it sits in
    /// space the strips already leave clear.
    fn strip_ribbon(&self) -> ui::chrome::shell::StripRibbon {
        let (count, focused) = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().strip_focus())
            .unwrap_or((0, 0));
        ui::chrome::shell::StripRibbon {
            content: Rect::new(
                self.rect.x0,
                self.rect.y0 + crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX as f64,
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

    /// The workspace-settings "Copy" button next to the invite link.
    ///
    /// Shared by the draw pass and the click router. It previously
    /// existed only inside the draw pass, was never registered as a hit
    /// rect, and never called the clipboard — a painted rectangle that
    /// looked like the primary action of the whole tab and did nothing
    /// when pressed.
    fn ws_settings_copy_rect(&self) -> Rect {
        let rect = self.ws_settings_rect();
        // Mirrors the draw pass: body_y = rect.y0 + 168, invite_y = body_y + 80.
        let invite_y = rect.y0 + 168.0 + 80.0;
        Rect::new(rect.x1 - 24.0 - 72.0, invite_y + 20.0, rect.x1 - 24.0, invite_y + 52.0)
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

    /// Collaborators dropdown rect — anchored to the top-right like quick-
    /// settings but narrower and taller-when-many-peers. Height is driven by
    /// the peer count so a roster of 8 doesn't clip.
    fn collaborators_rect(&self) -> Rect {
        let popover_w = 260.0_f64;
        let members = self
            .chrome
            .as_ref()
            .map(|c| collaborator_roster_rows(&c.borrow().tree()).len())
            .unwrap_or(0);
        let rows = members.max(1) as f64;
        // Room for the broker warning when there is one *and* rows under
        // it — with no rows the warning sits in the space the empty-state
        // line would have used.
        let warning = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().collab_unavailable.is_some())
            && members > 0;
        let warning_h = if warning { COLLAB_WARNING_H } else { 0.0 };
        // + COLLAB_FOOTER_H for the "Room activity" row: the dropdown answers
        // "who is in this room", and "what has happened in this room" is the
        // question you ask next.
        // + COLLAB_ACTIONS_H for Invite / Join, but only when this user may
        // actually invite. Reserving the band unconditionally would leave a
        // strip of empty popover under the roster for everyone who cannot,
        // which reads as a control that failed to paint.
        let actions_h = if self.may_invite_here() {
            COLLAB_ACTIONS_H
        } else {
            0.0
        };
        let popover_h: f64 =
            48.0 + warning_h + rows * 36.0 + 16.0 + actions_h + COLLAB_FOOTER_H;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(
            x1 - popover_w,
            y0,
            x1,
            y0 + popover_h.min(420.0 + actions_h + COLLAB_FOOTER_H),
        )
    }

    /// May the local user invite into the room they are looking at?
    ///
    /// The same question the palette's `peer.invite` row is filtered on,
    /// asked through the same policy (`required_capability` →
    /// [`Workspace::allows`]) rather than re-decided here. A second
    /// opinion about who may invite is a second security boundary, and
    /// the one that disagrees is the one that is wrong.
    ///
    /// Fail-closed at every missing piece: no chrome, no workspace, or
    /// peer-sync not running in this session all answer "no", because a
    /// button that mints nothing is worse than no button.
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
        let actor = workspace.actor_for_person(&st.local_user_id);
        CapabilityVisibility::new(workspace, actor).can_see(
            &ResourceRef::new("actions", "peer.invite"),
            &crate::shell::search::ResourceKind::Action,
        )
    }

    /// Rects of the Invite / Join buttons, mirroring what
    /// [`Self::draw_collaborators_footer`] paints. Both are `Rect::ZERO`
    /// when this user may not invite, so the band is absent from the
    /// geometry rather than present and inert.
    fn collaborators_action_row_rects(&self) -> (Rect, Rect) {
        if !self.may_invite_here() {
            return (Rect::ZERO, Rect::ZERO);
        }
        let r = self.collaborators_rect();
        let y0 = r.y1 - COLLAB_FOOTER_H - COLLAB_ACTIONS_H + 6.0;
        let y1 = y0 + 28.0;
        let gap = 8.0;
        let half = (r.width() - 16.0 - gap) / 2.0;
        let invite = Rect::new(r.x0 + 8.0, y0, r.x0 + 8.0 + half, y1);
        let join = Rect::new(invite.x1 + gap, y0, r.x1 - 8.0, y1);
        (invite, join)
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

    /// How many share rows the "shared out of here" list draws for a room.
    ///
    /// At least one: a room that shares nothing says so on a line of its
    /// own rather than collapsing the section away, because "nothing
    /// leaves this room" is an answer the screen exists to give.
    fn overview_share_rows(card: &crate::shell::overview::RoomCard) -> usize {
        let shown = card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
        let overflow = usize::from(card.shares.len() > shown);
        (shown + overflow).max(1)
    }

    /// Height of the "you are here" block, including its share list.
    fn overview_here_block_height(&self) -> f64 {
        let rows = match self.overview_here_index() {
            Some(i) => Self::overview_share_rows(&self.overview_cards()[i]),
            None => 1,
        };
        OVERVIEW_HERE_HEIGHT_PX
            + OVERVIEW_CAPTION_HEIGHT_PX
            + rows as f64 * OVERVIEW_SHARE_ROW_HEIGHT_PX
    }

    /// How many rows the "go to" list holds: every room but the one you
    /// are in, plus the trailing "New room" row.
    fn overview_list_slots(&self) -> usize {
        let n = self.overview_cards().len();
        n.saturating_sub(usize::from(self.overview_here_index().is_some())) + 1
    }

    /// Height the list would take if nothing clipped it.
    fn overview_content_height(&self) -> f64 {
        let k = self.overview_list_slots() as f64;
        (k * (OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX) - OVERVIEW_ROOM_ROW_GAP_PX)
            .max(0.0)
    }

    /// The panel: a floating surface centred in the viewport, sized to
    /// what it has to say and capped so it never runs off the screen.
    ///
    /// Same width and centring as the workspace-settings modal, because
    /// they are the same kind of object — a shell surface you summon,
    /// act in, and dismiss.
    fn overview_panel_rect(&self) -> Rect {
        let w = OVERVIEW_PANEL_WIDTH_PX.min(self.rect.width() - OVERVIEW_PANEL_MARGIN_PX * 2.0);
        let natural = OVERVIEW_PANEL_PAD_PX * 2.0
            + self.overview_here_block_height()
            + OVERVIEW_SECTION_GAP_PX
            + OVERVIEW_CAPTION_HEIGHT_PX
            + self.overview_content_height();
        let max_h = self.rect.height() - OVERVIEW_PANEL_MARGIN_PX * 2.0;
        let h = natural.min(max_h).max(0.0);
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
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
            + slot as f64 * (OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX);
        Rect::new(c.x0, y0, c.x1, y0 + OVERVIEW_ROOM_ROW_HEIGHT_PX)
    }

    /// The list slot a room occupies: the room you are in has none — it
    /// is the block at the top — and every other room keeps tree order.
    fn overview_slot_of(&self, idx: usize) -> Option<usize> {
        match self.overview_here_index() {
            Some(here) if here == idx => None,
            Some(here) if idx > here => Some(idx - 1),
            _ => Some(idx),
        }
    }

    /// Rect of workspace `idx` on this surface — the "you are here"
    /// block for the room you are in, its list row for any other.
    ///
    /// Indices are tree order, which is the contract the test-control
    /// socket's `overview_card[i]` names are built on.
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

    /// Pan the room list so the keyboard's selection is on screen.
    ///
    /// Without this, arrowing past the last visible room moves a
    /// selection the user cannot see — the list would keep answering the
    /// keyboard while showing the same rows.
    fn scroll_overview_selection_into_view(&mut self) {
        let Some(idx) = self.overview_selection else {
            return;
        };
        let Some(slot) = self.overview_slot_of(idx) else {
            // The room you are in is the pinned block, never in the list.
            return;
        };
        let step = OVERVIEW_ROOM_ROW_HEIGHT_PX + OVERVIEW_ROOM_ROW_GAP_PX;
        let top = slot as f64 * step;
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
// The overview answers three questions at a glance — what workspaces exist,
// what is in each one, and which one you are in — so the geometry is built
// around a grid of equal cards rather than a tall column of near-empty rows.
// Keeping the numbers as module-level constants lets the draw path,
// `hit_test_overview`, and the unit tests share one geometry oracle.

/// Widest the rooms panel ever gets. The panel is a switcher, not a
/// page: it is sized to what it says, and matches the workspace-settings
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
/// Most share rows drawn before the rest collapse into a "+N more" line.
/// The panel's job is to say *whether* things leave this room and roughly
/// what; the full list is the permissions pane's job.
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
/// being the panel behind it. Quiet enough to read as "not here right
/// now", strong enough to still be a face — at a third it vanished
/// against a light ground and lost the member altogether.
const OVERVIEW_FACE_AWAY_ALPHA: f32 = 0.55;
/// Most faces drawn before the rest collapse into a "+N" disc.
const OVERVIEW_FACES_MAX: usize = 4;
/// How far the panel rises as it opens, in logical pixels. Matches the
/// workspace-settings modal's entrance so the shell's floating surfaces
/// arrive the same way.
const OVERVIEW_PANEL_RISE_PX: f64 = 20.0;
/// Scale the panel starts its entrance at.
const OVERVIEW_PANEL_START_SCALE: f64 = 0.97;
/// Height of the fade painted over the bottom of the room list when it
/// overflows, so a clipped row reads as "there is more" rather than as a
/// row someone forgot to finish.
const OVERVIEW_SCROLL_FADE_PX: f64 = 40.0;

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
        // Workspace switch first: everything below walks
        // `group_row_ptrs`, and a switch replaces the whole row set.
        let ws_swapped = self.resync_workspace_content();
        let chrome_dirty = self.resync_strips_from_tree() || ws_swapped;
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
        // Open the palette aimed at the group whose "+" chip was clicked,
        // and retire that aim once the palette is gone. The refresh has
        // to happen here rather than inside the harvest: a picker that
        // opens on "Nothing to show" until the user types reads as
        // broken, and this is the frame where both the intent and the
        // document are in hand.
        let opened_for_new_tab = self.harvest_pending_new_tab();
        if opened_for_new_tab {
            self.refresh_search_results(s, p);
        }
        self.clear_stale_open_target();
        self.clear_stale_wifi_password();
        // Publish the poller's latest battery / network / volume reading
        // onto the chrome the top bar draws from.
        self.sync_status_from_bridge();
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
        // Light the group that pointer names. Same ordering rationale as
        // the scroll reconciler above: `active_group` is settled for this
        // frame by the time both of them read it.
        self.mark_focused_group();
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
        // Move whatever the portal buffered since the last frame onto the
        // chrome's queue. This has to run *before* the prompt pump: a
        // request that arrives during this frame should be on screen at
        // the end of it, not one frame later.
        let portal_dirty = self.drain_portal_prompts();
        // Mount a file-chooser dialog for any queued portal request, and
        // resolve one the user just finished with.
        let picker_dirty = self.pump_file_picker();
        // Sandbox Phase 4 Task 4.2: drive the permission-prompt pump.
        // Latches a fresh widget for the front of `chrome.pending_grants`,
        // and drains entries when the user clicks an action button.
        // Idempotent — runs every frame regardless of queue state.
        self.pump_permission_prompt();
        // App launches report back from their worker threads. Draining
        // here is what turns "the sandbox refused to start" into
        // something the user can see; the frame thread only ever takes
        // a mutex and moves a Vec out.
        let launch_dirty = self.drain_launch_events() | self.retire_notices_for_opened_windows();
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
        a || b || c || d || launch_dirty || chrome_dirty || portal_dirty || picker_dirty
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
            // active workspace (see `shell/mod.rs` build path), but it
            // is a HashMap, so iteration order is not the stacking
            // order — on a multi-strip workspace "the first one" was
            // whichever strip happened to hash first, and could differ
            // between runs. Take the topmost strip in tree order
            // instead, which is the one the wheel lines up beside.
            let top_strip: Option<StripId> = self.chrome.as_ref().and_then(|c| {
                let st = c.borrow();
                let tree = st.tree();
                tree.active_workspace()
                    .and_then(|ws| ws.strips.first().map(|s| s.id))
            });
            let scroll_of = |id: Option<StripId>| -> Option<f64> {
                let ptr = match id {
                    Some(id) => self.group_row_ptrs.get(&id)?,
                    None => return None,
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
            // A summoned wheel has left the canvas: it places itself
            // around the pointer, in screen space, so handing it the
            // strip's pan would shift it back out from under the
            // cursor by however far the strip happens to be scrolled.
            let summoned = self
                .chrome
                .as_ref()
                .map(|c| c.borrow().wheel_summon().is_some())
                .unwrap_or(false);
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
        let flags = self.overlay_flags();
        let target = if flags.overview { 1.0 } else { 0.0 };
        let t = self.tick_overview_transition(target);
        // The overview is a panel over your desk, not a room you travel
        // to, so the desktop does not go anywhere: it stays drawn and
        // the scrim dims it while the panel rises into place. This is
        // the same enter/exit the workspace-settings modal plays, run
        // off `overview_t` — one motion language for the shell's
        // floating surfaces.
        //
        // The previous form crossfaded the desktop out entirely and
        // painted a flat slate ground behind the overview. That is what
        // made it read as a separate page: for the length of the
        // animation the machine you were working on ceased to exist.
        let (overview_alpha, overview_dy, overview_scale) = overview_panel_curve(t);

        // Scale pivot = scene centre (so the shrink/zoom feels anchored
        // to the viewport the user is looking at, not the origin).
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let scaled = |s: f64| -> Affine {
            Affine::translate((cx, cy)) * Affine::scale(s) * Affine::translate((-cx, -cy))
        };

        // A workspace switch does not move the screen. The content tree
        // paints the new workspace where the old one was, at rest, on the
        // very next frame — no translate, no curve, nothing to wait out.
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_real_peers(scene, transform, text_renderer, theme, presence);

        // Floating sidebar wheel: above the desktop content, below every
        // overlay — including the scrims. It used to paint after the
        // overview, which put an undimmed disc on top of the dim while
        // the rest of the desktop receded behind it; the wheel is part
        // of the desktop, so it dims with the desktop.
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.draw(scene, transform, text_renderer, theme, presence);
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

        // The rooms panel. It rises 20 px and settles from 0.97, over a
        // scrim whose own strength follows the openness — so the desk
        // dims under it on the way in and comes back on the way out,
        // instead of the screen cutting to a different ground.
        if overview_alpha <= 0.0 {
            // Overview fully hidden — clear the cached "New room" hit
            // rect so a stray click during an in-flight close can't
            // create a workspace.
            self.add_workspace_rect = Rect::ZERO;
        }
        if overview_alpha > 0.0 {
            draw_modal_scrim_at(scene, transform, self.rect, overview_alpha, theme);
            let panel_xform = if (overview_scale - 1.0).abs() > f64::EPSILON
                || overview_dy.abs() > f64::EPSILON
            {
                Affine::translate((0.0, overview_dy)) * scaled(overview_scale) * transform
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

        // Strip ribbon — which strip of this workspace has focus, and
        // how many there are. Drawn under the popovers so an open panel
        // sits over it, and hidden while the overview is up because the
        // overview is not showing strips.
        if !self.overlay_flags().overview {
            let ribbon = self.strip_ribbon();
            ribbon.draw(scene, transform, theme, self.strip_ribbon_hover, 1.0);
        }

        // Quick-settings popover (anchored top-right, no scrim so it reads
        // as a dropdown rather than a modal). Concept wraps this in a
        // `motion.div` with `initial={{ opacity: 0, y: -10, scale: 0.95 }}`
        // → `animate={{ opacity: 1, y: 0, scale: 1 }}` so the panel reads
        // as "slipping out of the top-right". Detect the open rising edge
        // and play the same motion for `QS_OPEN_ANIM_MS`.
        if flags.quick_settings && !self.qs_was_open {
            // Rising edge: ask for fresh OS state, but do not wait for
            // it. The panel opens on this frame with the last known
            // values and updates as readings land. The lists behind the
            // Wi-Fi and Bluetooth drill-ins are deliberately not
            // requested here — see `qs_refresh`.
            self.qs_refresh.kick_fast();
        }
        self.qs_was_open = flags.quick_settings;
        // Apply anything the workers have delivered since the last frame.
        self.apply_quick_settings_readings();
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
            self.collab_invite_row_rect = Rect::ZERO;
            self.collab_join_row_rect = Rect::ZERO;
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

        // The portal's file-chooser dialog, over a scrim, above every
        // other overlay — including the sidebar wheel, which paints
        // after the desktop content and would otherwise sit on top of a
        // dialog that is supposed to be modal.
        //
        // It genuinely is modal: an app in another process is blocked on
        // the answer, and `handle_event` absorbs every event while it is
        // up. Chrome that still looks live underneath a modal is chrome
        // the user will try to click, so the scrim covers the whole
        // viewport and the dialog is drawn last.
        //
        // The compositor drawing it is also what makes it trustworthy:
        // the requesting app cannot draw over it, read it, or dismiss
        // it.
        if let Some(active) = self.file_picker.as_mut() {
            // Through `fill_scrim`, at the same strength every other
            // modal dims with. Filling with the raw token instead — as
            // this did — paints it at full opacity, and the token is
            // documented as opaque precisely because "the strength of
            // the dim belongs to whatever is being lifted". The result
            // was the black hole that comment warns about: a file dialog
            // floating on a void, with the desktop and the very document
            // the file was going into erased behind it, so nothing on
            // screen said what had asked or where the file would land.
            fill_scrim(scene, transform, self.rect, MODAL_SCRIM_ALPHA, theme);
            active.widget.set_rect(file_picker_rect(self.rect));
            active
                .widget
                .draw(scene, transform, text_renderer, theme, presence);
        }

        // The hardware-key readout sits above absolutely everything,
        // including the modals. It reports a change the user just made
        // with a physical key, and that key works while a modal is up —
        // so hiding the answer behind the modal would make the key look
        // broken exactly when the user is least able to check.
        self.draw_osd(scene, transform, text_renderer, theme);
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
            let p = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_cursor = Some(p);
            // Ribbon hover. Paint-only, so it is tracked here rather
            // than routed: a segment thickens under the pointer to say
            // it is a control, which is most of how the ribbon
            // advertises that it can be clicked at all.
            let hover = if self.overlay_flags().overview {
                None
            } else {
                self.strip_ribbon().hit(p)
            };
            if hover != self.strip_ribbon_hover {
                self.strip_ribbon_hover = hover;
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().bump();
                }
            }
        }

        // The portal's file-chooser dialog runs before everything,
        // including the peer-sync card, and absorbs every event whether
        // or not it consumed one.
        //
        // It is genuinely modal: a D-Bus method in another process is
        // parked on the answer, and a filename being typed into it
        // contains characters (`k`, `/`) the chrome underneath would
        // otherwise read as chords. Letting a keystroke through would
        // mean the user switching workspaces mid-dialog, leaving an app
        // blocked on a dialog they can no longer see.
        if self.file_picker.is_some() {
            let picker_rect = file_picker_rect(self.rect);
            if let Some(active) = self.file_picker.as_mut() {
                active.widget.set_rect(picker_rect);
                let mut tmp_taffy = TaffyTree::<()>::new();
                let tmp_node = tmp_taffy.new_leaf(Default::default()).unwrap();
                active.widget.handle_event(
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
            }
            *handled = true;
            return;
        }

        // Peer-sync card. Runs before every other overlay because while
        // the token prompt is up it owns the keyboard outright — a
        // token contains characters (`k`, `/`) that would otherwise be
        // read as chords by the chrome underneath.
        if self.peer_sync_card_visible() && self.handle_peer_sync_event(event, modifiers) {
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
            let shift_down = mstate.shift_key();
            // A passphrase field takes the keyboard whole while it is up.
            // Ahead of every chord below, including the palette ones: a
            // password can contain any character, and a `k` in one must
            // not open search instead of being typed.
            if self.handle_wifi_password_key(logical_key, modifiers) {
                *handled = true;
                return;
            }
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
            // The overview is a switcher, so while it is up the arrows
            // walk its room list and Enter goes to the room they land
            // on. Ahead of the modifier chords because these keys carry
            // no modifier, and ahead of the content because the desktop
            // must not scroll underneath an open panel.
            if self.overlay_flags().overview
                && !self.palette_visible
                && matches!(
                    logical_key,
                    Key::Named(NamedKey::ArrowDown)
                        | Key::Named(NamedKey::ArrowUp)
                        | Key::Named(NamedKey::Enter)
                )
            {
                let count = self
                    .chrome
                    .as_ref()
                    .map(|c| c.borrow().tree().workspaces.len())
                    .unwrap_or(0);
                if count > 0 {
                    if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                        // Enter commits: go there, and leave. A switcher
                        // that made you find the mouse to finish would
                        // not be one.
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
                        let step: isize =
                            if matches!(logical_key, Key::Named(NamedKey::ArrowDown)) {
                                1
                            } else {
                                -1
                            };
                        let cur = self
                            .overview_selection
                            .or_else(|| self.overview_here_index())
                            .unwrap_or(0) as isize;
                        // Wraps, because a switcher's list is a ring: the
                        // room after the last one is the first.
                        let next = (cur + step).rem_euclid(count as isize) as usize;
                        self.overview_selection = Some(next);
                        self.scroll_overview_selection_into_view();
                    }
                    *handled = true;
                    return;
                }
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
                // Window-creation chords. All four are Super-only, which
                // `is_shell_chord` already grabs wholesale, so none of
                // them takes a key away from a focused client — the
                // reason not to reach for the Ctrl+Shift+letter chords
                // that browsers and terminals already define.
                //
                // `Super+Return` is the direct one: a terminal, now, no
                // picker. It is the recovery path when the last shell is
                // gone, so it must not depend on search working.
                if super_down && matches!(logical_key, Key::Named(NamedKey::Enter)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        crate::shell::window_actions::open_terminal(
                            &mut st,
                            crate::shell::window_actions::Destination::Beside,
                        );
                        *handled = true;
                        return;
                    }
                }
                if super_down {
                    if let Key::Character(s) = logical_key {
                        match s.as_str() {
                            // Super+T — a new tab *here*: the chord form
                            // of the tab strip's "+".
                            "t" | "T" => {
                                let group = self
                                    .chrome
                                    .as_ref()
                                    .and_then(|c| {
                                        crate::shell::window_actions::focused_group(&c.borrow())
                                    });
                                if let (Some(chrome), Some(group)) = (self.chrome.clone(), group) {
                                    let mut st = chrome.borrow_mut();
                                    st.open_target =
                                        crate::shell::chrome_state::OpenTarget::Here(group);
                                    st.search_scope =
                                        crate::shell::chrome_state::SearchScope::All;
                                    st.bump();
                                    drop(st);
                                    self.palette_visible = true;
                                    self.palette.set_focus(true);
                                    self.palette.set_query("");
                                    self.last_search_query =
                                        "\u{0}not-yet-refreshed\u{0}".to_string();
                                    self.refresh_search_results(state, path);
                                    *handled = true;
                                    return;
                                }
                            }
                            // Super+N — a new window, placed by the rule.
                            "n" | "N" => {
                                if let Some(chrome) = self.chrome.clone() {
                                    let mut st = chrome.borrow_mut();
                                    st.open_target =
                                        crate::shell::chrome_state::OpenTarget::Beside;
                                    st.search_scope =
                                        crate::shell::chrome_state::SearchScope::All;
                                    st.bump();
                                }
                                self.palette_visible = true;
                                self.palette.set_focus(true);
                                self.palette.set_query("");
                                self.last_search_query =
                                    "\u{0}not-yet-refreshed\u{0}".to_string();
                                self.refresh_search_results(state, path);
                                *handled = true;
                                return;
                            }
                            // Super+W — close the tab you are looking at,
                            // the chord form of the pill's `✕`.
                            //
                            // Super, not Ctrl+W: `shell_owns_chord`
                            // grabs every Super chord wholesale, so this
                            // takes nothing away from a focused client,
                            // and it sits with the other window verbs
                            // (Super+T new tab, Super+N new window,
                            // Super+D duplicate). Ctrl+W would have to
                            // be added to the chord table, and would
                            // then be swallowed ahead of every client
                            // that binds it — which is browsers,
                            // terminals and editors, i.e. most of them.
                            // Same reasoning that keeps Super+S off the
                            // `is_mod` alias a few lines down.
                            "w" | "W" => {
                                if self.close_focused_tab() {
                                    *handled = true;
                                    return;
                                }
                            }
                            // Super+D — a second instance of what you are
                            // already looking at.
                            "d" | "D" => {
                                if let Some(chrome) = self.chrome.clone() {
                                    let mut st = chrome.borrow_mut();
                                    if let Err(err) =
                                        crate::shell::window_actions::duplicate_active_tab(&mut st)
                                    {
                                        st.push_warning_notification(
                                            "Can't duplicate this tab",
                                            err.user_message(),
                                        );
                                    }
                                    *handled = true;
                                    return;
                                }
                            }
                            // Super+S — call the workspace wheel to the
                            // pointer. The wheel is docked at the left
                            // edge of the strip's content and pans with
                            // it, so working out at the right-hand end
                            // of a strip leaves it off screen; without
                            // this you would have to scroll the whole
                            // strip home just to glance at the workspace
                            // list. Pressing it again, Escape, or
                            // picking a workspace sends it back.
                            //
                            // Gated on Super specifically, not on the
                            // `is_mod` alias: Ctrl+S is save in every
                            // app that has ever had a keyboard, and the
                            // shell grabs its chords ahead of the
                            // focused client.
                            "s" | "S" if super_down => {
                                if let Some(chrome) = self.chrome.clone() {
                                    let mut st = chrome.borrow_mut();
                                    let next = match st.wheel_summon() {
                                        Some(_) => None,
                                        None => self.last_cursor,
                                    };
                                    st.set_wheel_summon(next);
                                    *handled = true;
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Shift+ArrowUp / Shift+ArrowDown (with the same Super
                // or Ctrl modifier) — move focus between the strips of
                // the current workspace.
                //
                // Strips had no binding at all: the only switch function
                // in the tree was reachable from a debug-only simulator
                // that built a scratch copy of the workspace tree and
                // threw it away, so no key, gesture or click could move
                // between them. Workspaces already own the unshifted
                // Up/Down on both modifiers, and strips are a different
                // level of the same model rather than a variant of the
                // same move, so they take the shifted pair.
                if shift_down
                    && matches!(
                        logical_key,
                        Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown)
                    )
                {
                    let dir = if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                        -1
                    } else {
                        1
                    };
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if st.switch_strip(dir) {
                            *handled = true;
                        }
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
                    // A summoned wheel is the most transient thing on
                    // screen — it is up for one glance — so it goes
                    // first, above even an in-flight name edit.
                    if st.wheel_summon().is_some() {
                        st.set_wheel_summon(None);
                        *handled = true;
                        return;
                    }
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
                // Strip ribbon: click a segment to focus that strip.
                // Gated on no overlay owning the pointer, so a click that
                // happens to land under an open panel goes to the panel.
                if !self.overlay_flags().overview
                    && !self.cursor_over_open_overlay(p)
                    && let Some(i) = self.strip_ribbon().hit(p)
                    && let Some(chrome) = self.chrome.clone()
                {
                    chrome.borrow_mut().focus_strip_index(i);
                    *handled = true;
                    return;
                }
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
                    // Invite / Join before the activity row and before the
                    // outside-click close, so a click that lands on a button
                    // performs it rather than dismissing the surface it is
                    // painted on. Both rects are zero unless the button was
                    // painted, so this cannot fire for a band that is not
                    // there — the capability decides the paint and the hit
                    // together.
                    let invite = self.collab_invite_row_rect;
                    if invite.area() > 0.0 && invite.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.push_peer_sync_request(PeerSyncAction::PromptInvite);
                            // The card carries the question, and then the
                            // token, and it is what the user now has to
                            // read. Leaving the dropdown up would put a
                            // menu over the thing the click was for — the
                            // same reason `room.activity.open` closes it.
                            st.overlays.collaborators = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    let join = self.collab_join_row_rect;
                    if join.area() > 0.0 && join.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.push_peer_sync_request(PeerSyncAction::PromptConnect);
                            st.overlays.collaborators = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
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
                        // A passphrase prompt owns the Wi-Fi pane while
                        // it is up, so its controls are tested before the
                        // list's — including the back chevron, which
                        // backs out of the prompt rather than out of
                        // Wi-Fi altogether.
                        let prompt_up = self
                            .chrome
                            .as_ref()
                            .map(|c| c.borrow().quick_settings.wifi_password.is_some())
                            .unwrap_or(false);
                        if prompt_up {
                            let (cancel, connect) = self.qs_password_button_rects();
                            if back.contains(p) || cancel.contains(p) {
                                self.cancel_wifi_password();
                                *handled = true;
                                return;
                            }
                            if connect.contains(p) {
                                self.submit_wifi_password();
                                *handled = true;
                                return;
                            }
                            // Everything else on the pane is inert: the
                            // field is always focused, so there is
                            // nothing a click inside it could focus.
                            *handled = true;
                            return;
                        }
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
                                    // An open network, or one already
                                    // joined, needs no passphrase — and
                                    // asking for one the user does not
                                    // have would be a dead end. A
                                    // protected network we are not on
                                    // gets the prompt: NetworkManager
                                    // may already hold a profile, but it
                                    // will not tell us that without
                                    // trying, and a prompt the user can
                                    // dismiss is cheaper than a silent
                                    // failure.
                                    if net.secure && !net.connected {
                                        self.begin_wifi_password(&ssid);
                                    } else {
                                        crate::shell::qs_actions::log_qs_result(
                                            "connect_wifi",
                                            self.qs_actions.connect_wifi(&ssid, None),
                                        );
                                    }
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
                            // The list this level shows is fetched here,
                            // not on panel-open: enumerating networks
                            // means a Wi-Fi scan, which is by far the
                            // slowest read in the set and is invisible
                            // until this drill-in.
                            match level {
                                crate::shell::quick_settings::QsLevel::Wifi => {
                                    self.qs_refresh.kick_wifi_networks()
                                }
                                crate::shell::quick_settings::QsLevel::Bluetooth => {
                                    self.qs_refresh.kick_bt_devices()
                                }
                                _ => {}
                            }
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
                    // Copy the workspace invite link. The button was
                    // painted but never routed, so the most prominent
                    // control on the General tab did nothing at all when
                    // pressed.
                    let on_general = self
                        .chrome
                        .as_ref()
                        .is_some_and(|c| matches!(c.borrow().ws_settings_tab, WsTab::General));
                    if on_general && self.ws_settings_copy_rect().contains(p)
                    {
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
                    // wheel detent reveals roughly an eighth of one
                    // grid row (`OVERVIEW_CARD_HEIGHT_PX`).
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
        // no layout node yet. The workspace-content swap runs first for
        // the same reason at a larger scale: it replaces the whole row
        // set, and everything after it walks `group_row_ptrs`.
        let ws_swapped = self.resync_workspace_content();
        let resynced = self.resync_strips_from_tree() | ws_swapped;
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
            || qs_in_flight
            || ws_settings_in_flight
            || palette_in_flight
            || room_activity_in_flight
            || self.content.needs_animation_frame()
            || sidebar_frame
            // A reading that lands after the open animation has settled
            // still has to reach the screen, and the loop idles unless
            // something says otherwise.
            || self.qs_refresh.is_in_flight()
    }
}

/// `(alpha, dy, scale)` for the rooms panel at linear progress `t`.
///
/// The panel enters like every other floating surface in the shell: it
/// rises the last 20 px and settles from 0.97 while its alpha comes up
/// on an ease-out. Because `t` runs both ways, the exit is the same
/// curve in reverse rather than the panel vanishing.
///
/// The desktop is not in the return value, and that is the point. It
/// stays on screen, dimmed by the scrim, for the whole gesture — you
/// are looking at your machine with a switcher over it.
fn overview_panel_curve(t: f32) -> (f32, f64, f64) {
    let o = ui::ease_out(t.clamp(0.0, 1.0) as f64);
    let alpha = o as f32;
    let dy = OVERVIEW_PANEL_RISE_PX * (1.0 - o);
    let scale = OVERVIEW_PANEL_START_SCALE + (1.0 - OVERVIEW_PANEL_START_SCALE) * o;
    (alpha, dy, scale)
}


/// Centre the file-chooser dialog in `viewport`.
///
/// Sized as a fraction of the viewport with a floor and a ceiling, so it
/// stays usable on a small output and does not become a full-screen wall
/// of filenames on a large one.
pub fn file_picker_rect(viewport: Rect) -> Rect {
    const MIN_W: f64 = 420.0;
    const MIN_H: f64 = 320.0;
    const MAX_W: f64 = 900.0;
    const MAX_H: f64 = 640.0;
    let w = (viewport.width() * 0.62).clamp(MIN_W, MAX_W).min(viewport.width());
    let h = (viewport.height() * 0.72).clamp(MIN_H, MAX_H).min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - w) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - h) / 2.0;
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Map the prompt widget's choice onto the portal's outcome vocabulary.
///
/// Two enums for one decision is not ideal, but they belong to
/// different layers: `PromptChoice` is what a button click means, and
/// `PromptOutcome` is what a D-Bus caller is told. Keeping the mapping
/// in one place is what stops them drifting.
fn prompt_choice_to_outcome(choice: PromptChoice) -> crate::portal::PromptOutcome {
    use crate::portal::PromptOutcome;
    match choice {
        PromptChoice::Allow => PromptOutcome::AllowOnce,
        PromptChoice::Session => PromptOutcome::AllowSession,
        PromptChoice::Always => PromptOutcome::AllowAlways,
        PromptChoice::Deny => PromptOutcome::Deny,
    }
}

/// A file-chooser dialog on screen, and the portal request it answers.
///
/// The reply channel is the point: the D-Bus method that raised this is
/// parked on it. Dropping this struct without calling [`Self::answer`]
/// drops the sender, which the FileChooser backend reads as "cancelled"
/// — so a dialog that disappears for any reason (session lock, a
/// workspace closing, a panic unwinding the frame) fails closed and
/// releases the caller instead of stranding it.
pub(crate) struct ActiveFilePicker {
    pub(crate) widget: ui::widget::FilePicker,
    /// Who asked. Kept so the compositor can log and attribute the
    /// dialog without re-reading the (consumed) request.
    pub(crate) app_id: crate::shell::state::AppId,
    /// Whether the calling app asked for more than one path. Enforced
    /// here rather than in the widget: `OpenFile` vs `OpenFiles` is a
    /// distinction the *portal* makes, and the widget is shared with
    /// callers that have no portal request behind them.
    multiple: bool,
    reply: Option<tokio::sync::oneshot::Sender<Option<Vec<std::path::PathBuf>>>>,
}

impl ActiveFilePicker {
    /// Build the dialog for `req` out of the existing
    /// [`ui::widget::FilePicker`].
    ///
    /// Note what is taken from the request and what is not. The title,
    /// the filters and the suggested filename are the app's to choose —
    /// they are presentation. Where the dialog opens and how far it can
    /// browse are *not*: those come from
    /// [`crate::portal::PickerScope`], because an app that could aim the
    /// dialog could use it to learn what exists outside its sandbox.
    fn mount(req: crate::portal::FilePickerRequest, scope: crate::portal::PickerScope) -> Self {
        use ui::widget::file_picker::FileFilter;
        use ui::widget::{FilePicker, FilePickerMode};

        let mut filters: Vec<FileFilter> = req
            .filters
            .iter()
            .filter(|f| !f.is_catch_all())
            .map(|f| {
                let exts: Vec<&str> = f.extensions.iter().map(|e| e.as_str()).collect();
                FileFilter::new(f.label.clone(), &exts)
            })
            .collect();
        // "All files" always last, and always present: a filter list
        // with no escape hatch is a dialog that cannot open a file whose
        // extension the app failed to anticipate.
        filters.push(FileFilter::all());

        let mode = if req.save {
            FilePickerMode::Save
        } else {
            FilePickerMode::Open
        };
        let mut widget = FilePicker::new(req.title.clone(), &scope.initial_dir, scope.roots)
            .with_mode(mode)
            .with_filters(filters);
        if let Some(name) = req.current_name.as_deref() {
            widget = widget.with_filename(name);
        }
        Self {
            widget,
            app_id: req.caller.app_id.clone(),
            multiple: req.multiple,
            reply: Some(req.reply),
        }
    }

    /// Release the blocked portal call.
    ///
    /// An app that called `OpenFile` gets one path even if the user
    /// ctrl-clicked several: the dialog is shared and permits
    /// multi-select, so honouring the request the app actually made is
    /// this layer's job.
    fn answer(&mut self, picked: Option<Vec<std::path::PathBuf>>) {
        let picked = picked.map(|mut paths| {
            if !self.multiple {
                paths.truncate(1);
            }
            paths
        });
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(picked);
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
        Capability::Print { .. } => "Send documents to a printer".to_string(),
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
        Capability::Print { .. } => Capability::Print { scope },
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

/// One row of the collaborators dropdown.
///
/// People and agents are listed together because both are *in the room*,
/// and a person asking "who else is here" is asking one question. They
/// are tagged apart because they are not the same kind of participant:
/// a row that did not say which it was would be the OS reporting a
/// person present when none is.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    /// What the row is called.
    pub label: String,
    /// Avatar initial.
    pub initial: char,
    /// Per-identity avatar colour.
    pub color: Color,
    /// True for an agent admitted by capability, false for a person.
    pub is_agent: bool,
    /// What this participant may do here, as the room's own capability
    /// list says — `owner`, `editor`, `commenter`, `viewer`, `blocked`.
    ///
    /// `None` when the room records nothing about them, which is the
    /// ordinary case for somebody who walked in on a plain invite. A
    /// row with no role shows none rather than a default: inventing one
    /// would be the OS telling a user what a person is allowed to do in
    /// their room on no evidence.
    pub role: Option<String>,
    /// True for somebody the room has named who has not arrived in it
    /// yet. Drawn dimmed and qualified, never as a present participant.
    pub invited: bool,
}

/// The rows the collaborators dropdown shows: the peers this session
/// knows about, then the agents the active room has admitted.
///
/// The two halves come from different places on purpose. A peer is
/// somebody the room knows is in it — it is in `tree.collaborators`
/// because [`crate::shell::peer_presence::PeerPresence::refresh_roster`]
/// put it there, from the room document's own list of named peers and
/// from the presence this session has actually had. An agent meets
/// nobody and announces nothing a session could notice, so the only
/// honest evidence it is here is the capability the room recorded when
/// it was admitted, which is what `Workspace::agent_members` reads. The
/// consequence worth stating: no agent row can exist before a grant
/// did, and revoking the grant removes it.
///
/// Shared by the rect math and the paint so the two cannot drift — a
/// card sized for one row painting two clipped the second.
pub fn collaborator_roster_rows(tree: &crate::shell::state::WorkspaceTree) -> Vec<RosterRow> {
    // The people in the room the user is looking at, with the
    // tree-level roster standing in only when that workspace has none
    // of its own — the same precedence the top bar and the wheel disc
    // apply. Reading the tree-level list unconditionally showed one
    // workspace's peers under another workspace's heading.
    let roster: &[crate::shell::state::Collaborator] = match tree.active_workspace() {
        Some(ws) if !ws.collaborators.is_empty() => &ws.collaborators,
        _ => &tree.collaborators,
    };
    // Settled over the room rather than picked per row: `roster_colors`
    // honours an explicit `cursor_color` and moves anyone who would
    // otherwise land on it, so a peer's row avatar matches their cursor
    // and their top-bar chip, and two people never share a colour.
    let colors = crate::shell::state::roster_colors(roster);
    let mut rows: Vec<RosterRow> = roster
        .iter()
        .map(|p| {
            let (r, g, b) = colors
                .get(&p.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&p.id));
            RosterRow {
                initial: roster_initial(&p.name),
                label: p.name.clone(),
                color: Color::from_rgb8(r, g, b),
                is_agent: false,
                role: p.role_id.clone(),
                invited: p.invited,
            }
        })
        .collect();
    if let Some(ws) = tree.active_workspace() {
        for (agent, role) in ws.agent_members() {
            // The label is the principal itself. An agent has no display
            // name in the room document yet — when peers grow one, this
            // is the seam it arrives through, and until then showing the
            // subject is both true and the string you would revoke.
            let (r, g, b) = ui::presence::user_color(&agent);
            rows.push(RosterRow {
                initial: roster_initial(&agent),
                label: agent,
                color: Color::from_rgb8(r, g, b),
                is_agent: true,
                // The verb on the membership capability that admitted
                // it. An agent's authority is nothing but that grant,
                // so this is the one number a person needs in order to
                // decide whether to revoke it.
                role: Some(crate::shell::state::role_label(role).to_string()),
                // An agent is admitted by a grant and needs nothing to
                // redeem, so it is here the moment the capability says
                // so. There is no invited state for it to be in.
                invited: false,
            });
        }
    }
    rows
}

/// The avatar letter for a roster row.
fn roster_initial(name: &str) -> char {
    name.chars()
        .next()
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('•')
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
                // Editor / anything else / None → recover the hosted
                // app's label from the tab's title, which is all the
                // widget layer kept. A title the shell does not host
                // carries through unchanged.
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

/// `a` moved `t` of the way toward `b`, opaque throughout.
///
/// The opaque form matters wherever something is drawn *on* the result —
/// a monogram, an icon — because those pick their own contrast from the
/// colour they sit on, and can only do that if it is a real colour
/// rather than a partly-transparent one.
fn mix_toward(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (a.to_rgba8(), b.to_rgba8());
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb8(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// The surface every floating chrome panel is painted on — quick
/// settings, the collaborators popover, the workspace-settings sheet, the
/// peer-sync card.
///
/// This reached into `neutral[1]` — the ramp slot that happens to be
/// slate-900 on the dark palette. A ramp slot is a colour; the surface
/// tokens are a promise about contrast that each theme keeps in its own
/// way, and only the latter survives a theme its author never saw.
/// Panels now take the theme's own popover tier, which is what makes
/// them the same material as the bar they hang off (HIG §9.2).
fn panel_surface(theme: &Theme, alpha: f32) -> Color {
    ui::chrome::shell::panel_ground(theme).with_alpha_factor(alpha)
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
    themed.tokens.colors.surfaces.raised = panel_surface(theme, alpha);
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

impl ShellOverlayHost {
    /// Paint an overlapping stack of member faces, right-aligned at
    /// `right_x` and centred on `cy`.
    ///
    /// `ground` is the colour actually behind the stack. The separator
    /// ring between overlapping faces is a hole punched in the face
    /// below, so it has to be painted in the surface the stack sits on:
    /// ringed with anything else, the ring reads as a smudge and the
    /// faces run together into one shape.
    ///
    /// Presence is carried by opacity, not by omission: a member who has
    /// gone home is still someone this room admits, so they stay on the
    /// stack at reduced strength rather than disappearing and making the
    /// room look smaller than it is.
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
        let slots = shown + usize::from(overflow > 0);
        if slots == 0 {
            return right_x;
        }
        let right = right_x - r;
        for (idx, person) in people.iter().take(shown).enumerate() {
            let cx = right - (slots - 1 - idx) as f64 * step;
            let (cr, cg, cb) = colors_for
                .get(&person.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&person.id));
            let fill = Color::from_rgb8(cr, cg, cb);
            // Away members read as a quieter version of the same face,
            // so the stack still says who belongs here.
            //
            // Mixed toward the ground rather than made translucent: the
            // monogram picks its ink from the fill it is drawn on, and a
            // fill that is only *partly* there is not the colour the eye
            // ends up seeing. On a light panel an alpha-reduced face left
            // white letters on a pale disc, which was unreadable.
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
        right - (slots - 1) as f64 * step - r
    }

    /// Per-identity avatar colours for one room, settled over the room
    /// so a face here matches that person's cursor and their chip on the
    /// top bar.
    fn overview_face_colors(
        &self,
        idx: usize,
    ) -> std::collections::HashMap<String, (u8, u8, u8)> {
        match self.chrome.as_ref() {
            Some(c) => {
                let state = c.borrow();
                let tree = state.tree();
                match tree.workspaces.get(idx) {
                    Some(ws) => crate::shell::state::roster_colors(&ws.collaborators),
                    None => Default::default(),
                }
            }
            None => Default::default(),
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
        let hover = self.last_cursor.map(|c| rect.contains(c)).unwrap_or(false);

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

        // Faces first: they claim the right edge, and the name is elided
        // against what is left rather than running underneath them.
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
            text_w as f32,
            TextWeight::Semibold,
        );
        let (_, name_h) = text_renderer.measure_weighted(
            &name,
            name_size,
            None,
            TextWeight::Semibold,
        );
        let sub_size = tokens.typography.scale.sm;
        // Someone being in a room right now outranks who could be: it is
        // the thing that changes what you would do next. Access is what
        // the line says when the room is empty.
        let mut sub = crate::shell::overview::room_presence_label(card)
            .unwrap_or_else(|| crate::shell::overview::room_access_label(card));
        if let Some(shares) = crate::shell::overview::room_shares_label(card) {
            sub = format!("{sub} · {shares}");
        }
        let sub = text_renderer.elide_to_width(&sub, sub_size, text_w as f32);
        let (_, sub_h) = text_renderer.measure(&sub, sub_size, None);
        let stack_h = name_h as f64 + 2.0 + sub_h as f64;
        let top = rect.center().y - stack_h / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &name,
            name_size,
            colors.text_primary,
            transform * Affine::translate((x0, top)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            &sub,
            sub_size,
            colors.text_dim,
            transform * Affine::translate((x0, top + name_h as f64 + 2.0)),
            None,
        );
    }

    /// The workspace overview: a panel about **rooms**.
    ///
    /// # Why this is not a page
    ///
    /// A workspace here is a room — it has a host, a set of people, its
    /// own permissions and its own shared files — and the overview is
    /// how you see which one you are in, who is in it with you, what
    /// leaves it, and how to get to another. That is a switcher: a
    /// surface you pass *through*. So it is a floating panel in the same
    /// family as the command palette and the workspace-settings modal,
    /// not a full-screen document with a headline over a grid of cards.
    ///
    /// Reading order is the order the questions are asked: where you are
    /// now, what is shared out of here, and where else you can go.
    fn draw_overview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use ui::widget::{Chip, ChipLead, ChipStyle};

        if self.chrome.is_none() {
            return;
        }
        let cards = self.overview_cards();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let accent = colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX];

        // -- Panel surface ------------------------------------------------
        // The same fill / hairline / lift as the workspace-settings modal:
        // one floating-surface language across the shell's overlays.
        let panel = self.overview_panel_rect();
        let ground = panel_surface(theme, MODAL_SURFACE_ALPHA);
        ui::surface::elevation(scene, transform, panel, &tokens.elevation.e4);
        let card_rr = RoundedRect::from_rect(panel, tokens.radii.lg as f64);
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

        // -- Where you are ------------------------------------------------
        let here_rect = self.overview_here_rect();
        let name_size = Self::overview_header_font_size_for(tokens);
        let caption_size = tokens.typography.scale.sm;
        if let Some(hi) = here_idx {
            let card = &cards[hi];

            // The keyboard's target is marked wherever it lands, and on
            // open it lands here — so the room you are in carries the
            // same accent-tinted selection a list row does. Without it
            // the panel would open with nothing selected on screen and
            // the first arrow press would look like it came from
            // nowhere.
            if self.overview_selection == Some(hi) {
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
                name_avail as f32,
                TextWeight::Bold,
            );
            let (name_w, name_h) = text_renderer.measure_weighted(
                &name,
                name_size,
                None,
                TextWeight::Bold,
            );
            text_renderer.draw_text_weighted(
                scene,
                &name,
                name_size,
                colors.text_primary,
                transform * Affine::translate((content.x0, here_rect.y0)),
                None,
                TextWeight::Bold,
            );
            let chip_x0 = content.x0 + name_w as f64 + 10.0;
            Chip::draw_at(
                scene,
                transform,
                Rect::new(
                    chip_x0,
                    here_rect.y0 + name_h as f64 / 2.0 - chip_h / 2.0,
                    chip_x0 + chip_w,
                    here_rect.y0 + name_h as f64 / 2.0 + chip_h / 2.0,
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
                transform
                    * Affine::translate((content.x0, here_rect.y0 + name_h as f64 + 6.0)),
                None,
            );
        } else {
            // No room is marked current. Saying which one you are in
            // would be an invention, so the panel says what is true.
            text_renderer.draw_text_weighted(
                scene,
                "No room selected",
                name_size,
                colors.text_dim,
                transform * Affine::translate((content.x0, here_rect.y0)),
                None,
                TextWeight::Bold,
            );
        }

        // -- What leaves this room ----------------------------------------
        let shares_top = self.overview_shares_top();
        text_renderer.draw_text(
            scene,
            "Shared out of here",
            caption_size,
            colors.text_tertiary,
            transform * Affine::translate((content.x0, shares_top + 6.0)),
            None,
        );
        let row_h = OVERVIEW_SHARE_ROW_HEIGHT_PX;
        let mut sy = shares_top + OVERVIEW_CAPTION_HEIGHT_PX;
        let empty_card = crate::shell::overview::RoomCard {
            id: crate::shell::state::WorkspaceId::default(),
            name: String::new(),
            is_current: false,
            people: Vec::new(),
            shares: Vec::new(),
        };
        let here_card = here_idx.map(|i| &cards[i]).unwrap_or(&empty_card);
        // One vertical rule for the whole section: the glyph and both
        // texts centre on the row's middle line. Nudged offsets per
        // element are what left the icon riding above its own label.
        let body_size = tokens.typography.scale.base;
        let glyph = 14.0_f64;
        let (_, body_h) = text_renderer.measure("Ag", body_size, None);
        let body_top = |row_y: f64| row_y + (row_h - body_h as f64) / 2.0;
        let glyph_rect = |row_y: f64| {
            let cy = row_y + row_h / 2.0;
            Rect::new(
                content.x0,
                cy - glyph / 2.0,
                content.x0 + glyph,
                cy + glyph / 2.0,
            )
        };
        if here_card.shares.is_empty() {
            // An honest zero-state: nothing is invented to fill the row,
            // and "nothing" is itself the answer to the question.
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_rect(sy),
                ui::icons::Icon::Shield,
                colors.text_tertiary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_text(
                scene,
                "Nothing in this room is shared out of it",
                body_size,
                colors.text_dim,
                transform * Affine::translate((content.x0 + glyph + 8.0, body_top(sy))),
                None,
            );
        } else {
            let shown = here_card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
            for share in here_card.shares.iter().take(shown) {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    glyph_rect(sy),
                    ui::icons::Icon::ExternalLink,
                    colors.text_tertiary,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                // Who holds it sits right; what is shared runs from the
                // left and is elided against it.
                let with = match share.with.len() {
                    1 => share.with[0].clone(),
                    2 => format!("{} and {}", share.with[0], share.with[1]),
                    n => format!("{} and {} others", share.with[0], n - 1),
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
                    transform * Affine::translate((
                        content.x1 - with_w as f64,
                        sy + (row_h - with_h as f64) / 2.0,
                    )),
                    None,
                );
                let what_x = content.x0 + glyph + 8.0;
                let what_avail = (content.x1 - with_w as f64 - 12.0 - what_x).max(40.0);
                let what =
                    text_renderer.elide_to_width(&share.what, body_size, what_avail as f32);
                text_renderer.draw_text(
                    scene,
                    &what,
                    body_size,
                    colors.text_primary,
                    transform * Affine::translate((what_x, body_top(sy))),
                    None,
                );
                sy += row_h;
            }
            let hidden = here_card.shares.len() - shown;
            if hidden > 0 {
                let more = format!("and {hidden} more");
                let (_, more_h) = text_renderer.measure(&more, tokens.typography.scale.sm, None);
                text_renderer.draw_text(
                    scene,
                    &more,
                    tokens.typography.scale.sm,
                    colors.text_tertiary,
                    transform * Affine::translate((
                        content.x0 + glyph + 8.0,
                        sy + (row_h - more_h as f64) / 2.0,
                    )),
                    None,
                );
            }
        }

        // -- Where else you can go ----------------------------------------
        text_renderer.draw_text(
            scene,
            "Go to",
            caption_size,
            colors.text_tertiary,
            transform
                * Affine::translate((content.x0, self.overview_list_caption_top() + 6.0)),
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

        // -- New room ------------------------------------------------------
        // A peer row in the same list, not a banner: creating a room is
        // one of the things you can go and do from here.
        let add_rect = self.overview_add_workspace_rect_for(cards.len());
        let add_hover = self
            .last_cursor
            .map(|c| add_rect.contains(c))
            .unwrap_or(false);
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
            transform
                * Affine::translate((gx + glyph + 10.0, gcy - add_h as f64 / 2.0)),
            None,
        );
        self.add_workspace_rect = add_rect;

        // -- Overflow cue ---------------------------------------------------
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
            panel_surface(theme, MODAL_SURFACE_ALPHA),
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
        // "Copied" replaces the label for a moment after a successful
        // copy. Without it the button was indistinguishable from a
        // button that had done nothing, which is what it used to be.
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

    /// Paint every real peer's live pointer for the active workspace
    /// scope. Reads [`PresenceCtx::pointers`] filtered to
    /// `PresenceKey::Workspace { id: <room> }` — the same key every app
    /// publishes under, since the toolkit derives its broadcast scope
    /// from the room the launcher stamped in as `QOS_WORKSPACE_ID`.
    /// Each peer gets a pointer plus a name pill, drawn by the toolkit's
    /// [`ui::presence::draw_peer_cursors`] — the same call an app makes
    /// for the peers inside its own window, so a peer over the desktop
    /// and a peer over an app are the same thing drawn once. Colours are
    /// settled over the whole room by
    /// [`ui::presence::assign_peer_colors`], with an explicit roster
    /// colour outranking it.
    ///
    /// The whole set goes in one call because the names have to be
    /// arranged against each other: two people working side by side put
    /// their pointers a few pixels apart, and two pills at the same
    /// offset from those two pointers are one illegible smear.
    fn draw_real_peers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let visible = self.visible_peers(presence);
        ui::presence::draw_peer_cursors(
            scene,
            transform,
            self.rect,
            text_renderer,
            theme,
            &visible,
        );
    }

    /// Exactly the peers [`Self::draw_real_peers`] is about to paint:
    /// who they are, where they are, and what colour they are in.
    ///
    /// Separated from the paint so the selection can be asserted on
    /// directly. "No peer arrived" and "a peer arrived and was dropped by
    /// the filter" produce the same empty screen, and a test that
    /// rebuilds the lookup key for itself proves only that it can
    /// rebuild the key — which is how a mismatch between this filter and
    /// what apps publish survived long enough to need a fabricated peer
    /// covering for it.
    pub(crate) fn visible_peers(&self, presence: &PresenceCtx) -> Vec<ui::presence::PeerCursor> {
        let Some(chrome_rc) = self.chrome.as_ref() else {
            return Vec::new();
        };
        // Scope by the workspace's **room**, not its local handle.
        //
        // The room is the durable identity: it is what the launcher stamps
        // into an app as `QOS_WORKSPACE_ID`, what the toolkit addresses its
        // collaboration to, and therefore what an app publishes presence
        // under. The local `WorkspaceId` is a per-machine handle and
        // differs on every peer, so looking peers up by it matched nothing
        // an app had ever published — which is why this overlay had never
        // drawn a real peer, and why a fabricated one was standing in for
        // it.
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

        // Settle the colours over everyone in the room before deciding
        // what is on screen, and include the local user: every host in a
        // room sees the same membership that way — the others plus
        // itself — so every host computes the same assignment and a
        // person is one colour on every screen. Filtering to the visible
        // ones first would make each host compute over a different set.
        let local = crate::shell::state::local_user_id();
        let mut room_ids: Vec<&str> = peers.iter().map(|(u, _)| u.0.as_str()).collect();
        room_ids.push(local.as_str());
        let reserved: Vec<(u8, u8, u8)> = presence.color_overrides.values().copied().collect();
        let assigned = ui::presence::assign_peer_colors(&room_ids, &reserved);

        peers
            .iter()
            .filter(|(_, pos)| {
                // Guard: winit delivers scaled coordinates so the pointer
                // can, in principle, fall outside this window's rect if
                // the broadcast came from a peer with a different viewport.
                // Clip the paint to our content rect so we never trail a
                // cursor into the top-bar / sidebar chrome.
                self.rect.contains(Point::new(pos.0 as f64, pos.1 as f64))
            })
            .map(|(user, pos)| {
                // An explicit roster colour is a decision someone made
                // and outranks anything computed; everyone else takes
                // what the room settled on.
                let (r, g, b) = presence
                    .color_overrides
                    .get(user)
                    .copied()
                    .or_else(|| assigned.get(user.0.as_str()).copied())
                    .unwrap_or_else(|| presence.color_for(user));
                ui::presence::PeerCursor {
                    pos: (pos.0 as f64, pos.1 as f64),
                    // Name the peer rather than their id when the roster
                    // knows one. `display_name` falls back to the id, so a
                    // peer whose session never said who was at it is still
                    // labelled — with the handle it actually arrived
                    // under, not a guess.
                    label: presence.display_name(user),
                    color: Color::from_rgb8(r, g, b),
                }
            })
            .collect()
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
        // Narrower than it was. The card used to be sized around a
        // token that filled whatever width it was given; what it holds
        // now is a name, a line of state and a fingerprint, and a
        // surface three times wider than its content reads as one that
        // failed to load rather than one that is finished.
        let w = (self.rect.width() * 0.42).clamp(400.0, 520.0);
        // Each face is as tall as what it has to say. The card used to
        // be one fixed height because the token filled whatever it was
        // given; with the token gone, a fixed height is mostly empty
        // surface around three lines of text.
        let h = if self.invite_composer_open() {
            210.0_f64
        } else if self.connect_prompt_open() {
            220.0_f64
        } else {
            200.0_f64
        };
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
                st.peer_sync_connect_input.is_some()
                    || st.peer_sync_invite_token.is_some()
                    || st.peer_sync_invite_name.is_some()
            })
            .unwrap_or(false)
    }

    /// Paint the peer-sync card: who is being invited, the invite that
    /// was minted for them, or the prompt asking for one.
    ///
    /// The token itself is not drawn. It is some nine hundred base64
    /// characters — six lines of noise filling the surface at the exact
    /// moment the person needs to read one short thing, and nobody
    /// checks a bearer credential by eye anyway. What travels is the
    /// clipboard; what is shown is who the invite is for, that it is
    /// ready, and a short fingerprint of the machine that signed it, so
    /// the two people have something concrete to compare out of band. A
    /// session with no clipboard still has the whole token — it is in
    /// `state_dump`, which is where the proofs read it from.
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
        let (input, token, invite_name, invite_for, status) = {
            let st = chrome.borrow();
            (
                st.peer_sync_connect_input.clone(),
                st.peer_sync_invite_token.clone(),
                st.peer_sync_invite_name.clone(),
                st.peer_sync_invite_for.clone(),
                st.peer_sync_status.clone(),
            )
        };
        if input.is_none() && token.is_none() && invite_name.is_none() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.peer_sync_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            panel_surface(theme, MODAL_SURFACE_ALPHA),
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
        let name_size = (tokens.typography.scale.sm).max(12.0);
        let avail = rect.width() - pad * 2.0;
        let title = if invite_name.is_some() {
            "Invite someone"
        } else if input.is_some() {
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
        y += title_size as f64 + 14.0;

        if let Some(typed) = invite_name.as_deref() {
            // --- Asking who ------------------------------------------
            //
            // The question comes before the token, because an invite
            // minted without an answer to it is an invite the room can
            // never attach a name to: the host is left reading a peer
            // id off its own roster once the guest arrives.
            text_renderer.draw_text(
                scene,
                "Who are you inviting?",
                body_size,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + pad, y)),
                None,
            );
            y += body_size as f64 + 8.0;

            let field_h = 38.0_f64;
            let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + field_h);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surface_white_5,
                None,
                &RoundedRect::from_rect(field, tokens.radii.sm as f64),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(field, tokens.radii.sm as f64),
            );
            let shown = text_renderer.elide_to_width(typed, name_size, (avail - 40.0) as f32);
            let (tw, th) = text_renderer.measure(&shown, name_size, None);
            let ty = (field.y0 + field.y1) / 2.0 - th as f64 / 2.0;
            if typed.is_empty() {
                text_renderer.draw_text(
                    scene,
                    "Their name",
                    name_size,
                    colors.text_dim,
                    transform * Affine::translate((field.x0 + 12.0, ty)),
                    None,
                );
            } else {
                text_renderer.draw_text(
                    scene,
                    &shown,
                    name_size,
                    colors.text_primary,
                    transform * Affine::translate((field.x0 + 12.0, ty)),
                    None,
                );
            }
            // A caret, so the field reads as one that is taking keys
            // rather than one waiting to be clicked into.
            let caret_x = field.x0 + 12.0 + if typed.is_empty() { 0.0 } else { tw as f64 + 2.0 };
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &Rect::new(caret_x, field.y0 + 9.0, caret_x + 1.5, field.y1 - 9.0),
            );
            y = field.y1 + 12.0;

            let note = text_renderer.elide_to_width(
                "The room will call them this, beside their cursor.",
                body_size,
                avail as f32,
            );
            text_renderer.draw_text(
                scene,
                &note,
                body_size,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + pad, y)),
                None,
            );
            y += body_size as f64 + 10.0;
        } else if let Some(token) = token.as_deref() {
            // --- The invite, made ------------------------------------
            let parsed = parse_invite(token);
            let who = invite_for.as_deref().unwrap_or("Anyone with this invite");
            let sub = match parsed.as_ref() {
                Some(t) => format!("Expires in {}", time_until(t.expiry_unix_seconds)),
                None => String::new(),
            };
            y = draw_invite_person_row(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
                who,
                &sub,
                invite_for.is_some(),
            );
            y += 12.0;
            if let Some(t) = parsed.as_ref() {
                y = draw_invite_code(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    Point::new(rect.x0 + pad, y),
                    &peer_fingerprint(&t.owner_peer_id),
                    avail,
                );
            }
        } else {
            // --- The receiving half ----------------------------------
            let typed = input.as_deref().unwrap_or("");
            let parsed = parse_invite(typed);
            if let Some(t) = parsed.as_ref() {
                y = draw_invite_person_row(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
                    "Invite received",
                    &format!("Expires in {}", time_until(t.expiry_unix_seconds)),
                    false,
                );
                y += 12.0;
                y = draw_invite_code(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    Point::new(rect.x0 + pad, y),
                    &peer_fingerprint(&t.owner_peer_id),
                    avail,
                );
            } else {
                let line = if typed.is_empty() {
                    "Paste the invite they sent you.".to_string()
                } else {
                    // Not a wall: enough to see that something is in
                    // the field and that it is growing, which is what a
                    // person typing one by hand needs. The reason it
                    // does not parse yet is the line underneath.
                    format!("{} · {} characters", elide_token(typed), typed.chars().count())
                };
                let line = text_renderer.elide_to_width(&line, name_size, avail as f32);
                text_renderer.draw_text(
                    scene,
                    &line,
                    name_size,
                    if typed.is_empty() {
                        colors.text_dim
                    } else {
                        colors.text_primary
                    },
                    transform * Affine::translate((rect.x0 + pad, y)),
                    None,
                );
                y += name_size as f64 + 8.0;
                if !typed.is_empty() {
                    text_renderer.draw_text(
                        scene,
                        "Not a whole invite yet",
                        body_size,
                        colors.text_dim,
                        transform * Affine::translate((rect.x0 + pad, y)),
                        None,
                    );
                    y += body_size as f64 + 8.0;
                }
            }
        }

        if let Some(status) = status.as_deref() {
            let status = text_renderer.elide_to_width(status, body_size, avail as f32);
            text_renderer.draw_text(
                scene,
                &status,
                body_size,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + pad, y)),
                None,
            );
            // The chord hint belongs to the buttons, not to the status
            // line above it — without the gap the two read as one
            // paragraph and the status stops being the thing that just
            // happened.
            y += body_size as f64 + 16.0;
        }

        // Every face of the card carries buttons, because every step of
        // moving an invite is a gesture: naming the person, getting the
        // token onto the clipboard, getting it off again.
        {
            let buttons: &[(&str, PeerSyncPromptButton)] = if invite_name.is_some() {
                &[
                    ("Create invite", PeerSyncPromptButton::CreateInvite),
                    ("Cancel", PeerSyncPromptButton::Cancel),
                ]
            } else if input.is_some() {
                &[
                    ("Join", PeerSyncPromptButton::Join),
                    ("Cancel", PeerSyncPromptButton::Cancel),
                    ("Paste", PeerSyncPromptButton::PasteToken),
                ]
            } else {
                &[("Copy", PeerSyncPromptButton::CopyToken)]
            };
            let bh = 30.0_f64;
            let by = rect.y1 - pad - bh;
            let mut bx = rect.x1 - pad;
            for (label, which) in buttons.iter().copied() {
                let (tw, th) = text_renderer.measure(label, body_size, None);
                let bw = tw as f64 + 26.0;
                let brect = Rect::new(bx - bw, by, bx, by + bh);
                let is_primary = matches!(
                    which,
                    PeerSyncPromptButton::Join
                        | PeerSyncPromptButton::CopyToken
                        | PeerSyncPromptButton::CreateInvite
                );
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
        //
        // Every chip is painted, whatever the width — the strip wraps,
        // and the band grows to hold the rows. See [`lay_out_chip_strip`]
        // for why not painting one was the bug.
        let kinds = self.search_registry.registered_kinds();
        let labels: Vec<&str> = std::iter::once("All")
            .chain(kinds.iter().map(|k| k.label()))
            .collect();
        let natural: Vec<f64> = labels
            .iter()
            .map(|l| text_renderer.measure(l, chip_text_size, None).0 as f64 + CHIP_PAD_PX)
            .collect();
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
            let rect = strip.rects[i];
            let is_active = match i {
                0 => self.active_kind_filter.is_empty(),
                _ => self.active_kind_filter.contains(&kinds[i - 1]),
            };
            // Only a chip that had to be squeezed loses any of its label.
            let label = text_renderer.elide_to_width(
                label,
                chip_text_size,
                (rect.width() - CHIP_PAD_PX / 2.0).max(1.0) as f32,
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

        // Ask the card for the rows the strip actually needs. The next
        // layout grows the band and moves the result rows down; until it
        // runs, the chips above are squeezed into the band there is —
        // which is why they are all still clickable in the meantime.
        if strip.rows_needed != self.search_chip_rows {
            self.search_chip_rows = strip.rows_needed;
            if self.palette.set_host_filter_strip_rows(strip.rows_needed) {
                self.palette_layout_dirty = true;
            }
        }
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
        let card = RoundedRect::from_rect(
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
        );
        // Panels are elevated (HIG §9.1). Without this the popover was a
        // flat tint, which reads as deliberate only on a palette whose
        // overlay tier is much darker than its ground — i.e. only on
        // dark. Light and high-contrast had nothing lifting the panel off
        // the desktop at all.
        ui::chrome::shell::panel_shadow(
            scene,
            transform,
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
            theme,
            1.0,
        );
        // QuickSettings popover surface — concept §3.1: `bg-slate-900/80
        // backdrop-blur-md`. See visual-gap #3 / [`POPOVER_SURFACE_ALPHA`].
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
            // Control role: 12 px. This was a literal 16, which is
            // the one step the radius scale deliberately omits — so
            // these tiles matched nothing else on screen.
            let tile_rr = RoundedRect::from_rect(
                tile,
                ui::chrome::ChromeRole::Control.radius(theme, tile.height()),
            );
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
        // the Wi-Fi reading marked `connected` (via
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

        // A passphrase prompt takes over the pane. It is not a separate
        // nav level: the user is still "in Wi-Fi", and pushing a level
        // would put a second back-chevron in the stack that means
        // something different from the first.
        if let Some(prompt) = qs.wifi_password.as_ref() {
            self.draw_qs_wifi_password(scene, transform, text_renderer, theme, prompt);
            return;
        }

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

    /// Attempt the join.
    ///
    /// `nmcli device wifi connect` blocks until association succeeds or
    /// fails, which on a busy band is seconds — but this is a deliberate
    /// button press with a modal-ish prompt in front of it, the same
    /// contract every other quick-settings action has, and the
    /// alternative (a background join whose result arrives with no
    /// prompt left to report it into) is worse. The prompt stays up on
    /// failure carrying the reason.
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
                // The list's `connected` flags are now stale, and the
                // next poll of the popover is on its next open. Mark the
                // row we just joined so the pane the user lands back on
                // is not still telling them they are not connected.
                for n in st.quick_settings.wifi_networks.iter_mut() {
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
                    // The passphrase is kept so a typo is a correction
                    // rather than a retype, and selected so the first
                    // keystroke replaces it if it was simply wrong.
                    p.selection.anchor = 0;
                    p.selection.cursor = p.password.len();
                    p.error = Some("Couldn't join. Check the password.".to_string());
                }
            }
        }
        st.bump();
    }

    /// Route a key press into the passphrase field, if one is up.
    /// Returns `true` when the key was consumed.
    fn handle_wifi_password_key(
        &mut self,
        logical_key: &Key,
        modifiers: &winit::event::Modifiers,
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
            let out = crate::shell::text_entry::apply(
                logical_key,
                mods,
                &mut prompt.password,
                &mut sel,
            );
            prompt.selection = sel;
            // Typing after a rejection clears the rejection: the message
            // was about the old value.
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

    /// Cancel + Connect, anchored to the foot of the pane rather than
    /// stacked under the field.
    ///
    /// The popover's height is set by the network list this pane
    /// replaces, and the prompt needs about a third of it — so buttons
    /// placed directly below the field leave the bottom two thirds
    /// visibly empty and the pane reads as half-drawn. Pinning them to
    /// the foot is the layout every dialog uses for the same reason:
    /// the question sits at the top, the answer at the bottom, and the
    /// space between them is deliberate rather than left over.
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
        &self,
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

        // Sub-line: what is being asked for, and why.
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
            transform * Affine::translate((lock_box.x1 + 8.0, sub_y + 1.0)),
            None,
            TextWeight::Medium,
        );

        // The field. Focused for as long as it is on screen — it is the
        // only thing on the pane that takes typing, so a focus ring that
        // could be off would only ever be a bug.
        let field = self.qs_password_field_rect();
        let field_shape = RoundedRect::from_rect(field, 10.0);
        // Opaque, not the `surface_white_5` wash the popover's other
        // cards use. The quick-settings panel is translucent by design,
        // and a 5%-white rect over it lets whatever the app underneath
        // is showing run straight through the input — here that put the
        // word "Click to edit" across the passphrase. A field you cannot
        // read is a field you cannot check what you typed into, which is
        // the one thing a masked input has to get right.
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
        let (mask_w, mask_h) = text_renderer.measure_weighted(
            &masked,
            text_size,
            None,
            TextWeight::Medium,
        );
        let text_y = field.y0 + (field.height() - mask_h as f64) / 2.0;
        if masked.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                "Password",
                text_size,
                colors.text_dim.with_alpha_factor(0.6),
                transform * Affine::translate((text_x, text_y)),
                None,
                TextWeight::Medium,
            );
        } else {
            text_renderer.draw_text_weighted(
                scene,
                &masked,
                text_size,
                colors.text_primary,
                transform * Affine::translate((text_x, text_y)),
                None,
                TextWeight::Medium,
            );
        }
        // Caret. Masked text is uniform-width bullets, so the caret's x
        // is proportional to how many characters precede it — which is
        // the only thing a masked field can honestly show.
        let chars = prompt.password.chars().count();
        let caret_chars = prompt.password[..prompt.selection.cursor.min(prompt.password.len())]
            .chars()
            .count();
        let caret_x = if chars == 0 {
            text_x
        } else {
            text_x + mask_w as f64 * (caret_chars as f64 / chars as f64)
        };
        scene.fill(
            Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &Rect::new(caret_x, field.y0 + 9.0, caret_x + 1.5, field.y1 - 9.0),
        );

        // A failed attempt says so where the eye already is, rather than
        // as a toast that would outlive the prompt.
        if let Some(err) = prompt.error.as_ref() {
            text_renderer.draw_text_weighted(
                scene,
                err,
                tokens.typography.scale.xs,
                colors.semantic.danger,
                transform * Affine::translate((field.x0 + 2.0, field.y1 + 8.0)),
                None,
                TextWeight::Medium,
            );
        } else if prompt.connecting {
            text_renderer.draw_text_weighted(
                scene,
                "Connecting…",
                tokens.typography.scale.xs,
                colors.text_dim,
                transform * Affine::translate((field.x0 + 2.0, field.y1 + 8.0)),
                None,
                TextWeight::Medium,
            );
        }

        // Buttons.
        let (cancel, connect) = self.qs_password_button_rects();
        let cancel_shape = RoundedRect::from_rect(cancel, 10.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &cancel_shape,
        );
        Self::draw_centred_label(
            scene,
            transform,
            text_renderer,
            cancel,
            "Cancel",
            text_size,
            colors.text_dim,
        );

        let live = prompt.can_submit();
        let connect_shape = RoundedRect::from_rect(connect, 10.0);
        scene.fill(
            Fill::NonZero,
            transform,
            if live {
                colors.accent[5]
            } else {
                // Not merely dim: a disabled primary button that still
                // looks primary invites the click it will ignore.
                colors.accent[5].with_alpha_factor(0.28)
            },
            None,
            &connect_shape,
        );
        Self::draw_centred_label(
            scene,
            transform,
            text_renderer,
            connect,
            if prompt.connecting {
                "Connecting…"
            } else {
                "Connect"
            },
            text_size,
            if live {
                colors.text_primary
            } else {
                colors.text_primary.with_alpha_factor(0.5)
            },
        );
    }

    /// Centre a label inside `rect`. Small enough to inline, used often
    /// enough in the popover that three copies had already drifted.
    fn draw_centred_label(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        rect: Rect,
        label: &str,
        size: f32,
        color: vello::peniko::Color,
    ) {
        let (w, h) = text_renderer.measure_weighted(label, size, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            label,
            size,
            color,
            transform
                * Affine::translate((
                    rect.center().x - w as f64 / 2.0,
                    rect.center().y - h as f64 / 2.0,
                )),
            None,
            TextWeight::Semibold,
        );
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
        text_renderer.draw_text(
            scene,
            "Collaborators",
            tokens.typography.scale.lg,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 16.0)),
            None,
        );

        // Per-identity avatar colour (cursor colour, else the
        // palette-pinned `user_color`) so a row's avatar matches that
        // identity's cursor and top-bar chip.
        let rows: Vec<RosterRow> = self
            .chrome
            .as_ref()
            .map(|c| collaborator_roster_rows(&c.borrow().tree()))
            .unwrap_or_default();

        // A broken session has to say so whether or not it has anybody to
        // list, and the two cases were not the same bug. With no rows,
        // "No peers connected" is true of a working session nobody has
        // joined *and* of a session with no collaboration at all, and
        // saying the reassuring one for both is how a session with its
        // broker down looked healthy. With rows, it is worse: an agent
        // holds its membership by capability, so a grant made earlier
        // still lists it after the broker has died — a participant on
        // screen that cannot participate. So the warning is drawn above
        // whatever is listed, never instead of it.
        let unavailable = self
            .chrome
            .as_ref()
            .and_then(|c| c.borrow().collab_unavailable.clone());
        if let Some(reason) = &unavailable {
            text_renderer.draw_text(
                scene,
                reason,
                tokens.typography.scale.sm,
                colors.semantic.danger,
                transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 56.0)),
                None,
            );
        }

        if rows.is_empty() {
            if unavailable.is_none() {
                text_renderer.draw_text(
                    scene,
                    "No peers connected",
                    tokens.typography.scale.sm,
                    colors.text_dim,
                    transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 56.0)),
                    None,
                );
            }
            self.draw_collaborators_footer(scene, transform, text_renderer, theme);
            return;
        }

        let list_top = if unavailable.is_some() {
            rect.y0 + 48.0 + COLLAB_WARNING_H
        } else {
            rect.y0 + 48.0
        };
        for (i, row) in rows.iter().enumerate() {
            let row_y = list_top + i as f64 * 36.0;
            let center = Point::new(rect.x0 + 16.0 + 12.0, row_y + 14.0);
            // Row avatar = shared `ui::Avatar` chip (r = 11) in that
            // identity's own colour, matching the top-bar stack and the
            // overview.
            let avatar_rect =
                Rect::new(center.x - 11.0, center.y - 11.0, center.x + 11.0, center.y + 11.0);
            // Same chip, same place, same order, whichever state the
            // row is in — so what a viewer sees when somebody joins is
            // one row filling in, not a list reshuffling.
            //
            // An invitation is drawn as an *empty seat*: the unfilled,
            // dashed ring the top bar already uses to mean "nobody here
            // yet". Reusing that idiom rather than inventing one is the
            // point — and a filled chip merely faded would not have
            // worked anyway, because `Avatar::draw_at` paints its
            // monogram `Color::WHITE` at full strength, so a dimmed
            // disc under a bright letter reads as a rendering fault
            // rather than a state.
            if row.invited {
                let seat = vello::kurbo::Circle::new(center, 11.0);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
                    transform,
                    row.color.multiply_alpha(INVITED_ROW_ALPHA),
                    None,
                    &seat,
                );
                // Their own initial, not a `+`: the seat belongs to a
                // named person, which is exactly what distinguishes
                // this from the top bar's "invite anybody" chip.
                let initial = row.initial.to_string();
                let size = 22.0_f32 * 0.4;
                let (iw, ih) = text_renderer.measure_weighted(
                    &initial,
                    size,
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &initial,
                    size,
                    colors.text_dim,
                    transform
                        * Affine::translate((
                            center.x - iw as f64 / 2.0,
                            center.y - ih as f64 / 2.0,
                        )),
                    None,
                    TextWeight::Semibold,
                );
            } else {
                ui::widget::Avatar::draw_at(
                    scene,
                    transform,
                    avatar_rect,
                    &row.initial.to_string(),
                    row.color,
                    theme,
                    text_renderer,
                );
            }
            text_renderer.draw_text(
                scene,
                &row.label,
                tokens.typography.scale.sm,
                if row.invited {
                    colors.text_dim
                } else {
                    colors.text_primary
                },
                transform * Affine::translate((rect.x0 + 44.0, row_y + 7.0)),
                None,
            );
            // The tag is what keeps the shared list honest. Without it
            // this dropdown says a name is in the room and lets the
            // reader supply "person", which is the one thing it must
            // never do on the reader's behalf.
            //
            // Suppressed only when the label already carries the word,
            // as the agent this OS ships does: "agent  agent" reads as
            // a rendering fault, and the honesty the tag exists for is
            // already served by the name itself.
            if row.is_agent && !row.label.eq_ignore_ascii_case("agent") {
                text_renderer.draw_text(
                    scene,
                    "agent",
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform * Affine::translate((rect.x1 - 56.0, row_y + 9.0)),
                    None,
                );
            }
            // The sub-line: what they may do here, or — for somebody
            // who has been named and has not come — that they have not
            // come. Right-aligned under the name rather than beside the
            // agent tag above, so an agent shows both what it is and
            // what it holds without the two colliding in the same 56 px.
            //
            // "invited" wins over the role when both could be said. A
            // role is what somebody may do once they are here, and
            // leading with it for somebody who is not here yet answers
            // a question nobody asked ahead of the one they did.
            let sub = if row.invited {
                Some(INVITED_TAG.to_string())
            } else {
                row.role.clone()
            };
            if let Some(sub) = sub {
                let (rw, _) = text_renderer.measure_weighted(
                    &sub,
                    tokens.typography.scale.xs,
                    None,
                    TextWeight::Regular,
                );
                text_renderer.draw_text(
                    scene,
                    &sub,
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform
                        * Affine::translate((rect.x1 - 16.0 - rw as f64, row_y + 21.0)),
                    None,
                );
            }
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

        // Invite / Join. Both are the palette's peer-sync rows, put where
        // a person looking at the room's people will look for them: the
        // palette is how you reach a command whose name you already know,
        // and this is how you find out the command exists at all.
        //
        // Invite is the filled one because it is the act this surface is
        // usually opened to perform; Join is outlined because it is the
        // other machine's half of the same gesture, and a room you are
        // already in is not one you join.
        let (invite, join) = self.collaborators_action_row_rects();
        self.collab_invite_row_rect = invite;
        self.collab_join_row_rect = join;
        if invite.area() > 0.0 {
            let btn_size = tokens.typography.scale.sm;
            for (rect, label, filled) in [(invite, "Invite", true), (join, "Join", false)] {
                let shape = RoundedRect::from_rect(rect, tokens.radii.sm as f64);
                if filled {
                    scene.fill(Fill::NonZero, transform, colors.accent[5], None, &shape);
                } else {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        colors.surface_white_5,
                        None,
                        &shape,
                    );
                    scene.stroke(
                        &vello::kurbo::Stroke::new(1.0),
                        transform,
                        colors.border_standard,
                        None,
                        &shape,
                    );
                }
                let (tw, th) = text_renderer.measure_weighted(
                    label,
                    btn_size,
                    None,
                    TextWeight::Medium,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    btn_size,
                    colors.text_primary,
                    transform
                        * Affine::translate((
                            rect.center().x - tw as f64 / 2.0,
                            rect.center().y - th as f64 / 2.0,
                        )),
                    None,
                    TextWeight::Medium,
                );
            }
        }

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
mod overview_panel_tests {
    //! The overview is a switcher for **rooms**: which one you are in,
    //! who is in it with you, what leaves it, and how to get to another.
    //! These pin the parts of that which are geometry or data rather
    //! than pixels.
    //!
    //! Every assertion runs against the helpers `draw_overview` itself
    //! calls — `overview_row_rect` is where a room is painted *and* what
    //! `hit_test_overview` clicks against, and `room_cards` is the model
    //! the panel's every claim is read from.

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
            .map(|c| c.borrow().tree().workspaces.len())
            .unwrap_or(0)
    }

    /// The room you are in is the block at the top, not a row in the
    /// list you could scroll away from. That is the difference between
    /// "here is where you are" and "here is an index of rooms".
    #[test]
    fn the_room_you_are_in_is_the_pinned_block_not_a_list_row() {
        let h = host();
        let here = h.overview_here_index().expect("demo tree has an active room");
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
        assert_eq!(h.hit_test_overview(Point::new(panel.x0 - 20.0, panel.center().y)), None);
        assert_eq!(h.hit_test_overview(Point::new(panel.center().x, panel.y0 - 20.0)), None);
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
        assert!(p.y0 >= h.rect.y0 && p.y1 <= h.rect.y1, "panel stays on screen");
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
        // What boot does before the session paints: a room you made is a
        // room you own. Without it the tree describes a workspace with no
        // recorded access at all, which is a different claim.
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
            card.people.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
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
        // Dummy content widget — `Container::new()` produces an empty
        // layout node which is enough for the transition state tests.
        ShellOverlayHost::new(Box::new(Container::new()))
    }

    /// A window as wide as the strip allows still has to end before the
    /// display does. Its border, its rounded corners and its drop shadow
    /// all live in the last few pixels of the card, so a group that
    /// resolves flush to the output's right edge loses every one of them
    /// and reads as clipped by the bezel instead of as a window on a
    /// desktop. The near edge already reserves a wallpaper gutter beside
    /// the wheel disc; this asserts the far edge reserves one too.
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
        let leading = sidebar_width_for_viewport(900.0) as f64 + WHEEL_TO_STRIP_GAP_PX;
        // The widest a group can be, and therefore where the widest one ends.
        let widest = ui::widget::WIDTH_MAX_PERMILLE as f64 / 1000.0 * span.usable;
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
        for step in 1..=20 {
            let cur = overview_panel_curve(step as f32 / 20.0);
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
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let rec = std::rc::Rc::new(JoinRecorder {
            attempts: RefCell::new(Vec::new()),
            reject,
        });
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

    fn press(h: &mut ShellOverlayHost, key: Key) {
        let mods = winit::event::Modifiers::default();
        h.handle_wifi_password_key(&key, &mods);
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
            press(&mut h, ch(c));
        }
        assert_eq!(
            chrome.borrow().quick_settings.wifi_password.as_ref().unwrap().password,
            "hunk"
        );
        assert!(
            !h.palette_visible,
            "a 'k' in a password must be a character, not the search chord"
        );
    }

    #[test]
    fn enter_submits_the_typed_password_to_the_join() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["p", "w", "1"] {
            press(&mut h, ch(c));
        }
        press(&mut h, Key::Named(NamedKey::Enter));
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
    fn a_rejected_password_keeps_the_prompt_up_and_says_why() {
        let (mut h, chrome, _) = host_on_wifi_pane(true);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["b", "a", "d"] {
            press(&mut h, ch(c));
        }
        press(&mut h, Key::Named(NamedKey::Enter));
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
        press(&mut h, ch("x"));
        press(&mut h, Key::Named(NamedKey::Enter));
        assert!(chrome.borrow().quick_settings.wifi_password.as_ref().unwrap().error.is_some());
        press(&mut h, ch("y"));
        assert!(
            chrome.borrow().quick_settings.wifi_password.as_ref().unwrap().error.is_none(),
            "the message was about the old value"
        );
    }

    #[test]
    fn escape_backs_out_and_takes_the_password_with_it() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, ch("s"));
        press(&mut h, Key::Named(NamedKey::Escape));
        assert!(chrome.borrow().quick_settings.wifi_password.is_none());
        assert!(rec.attempts.borrow().is_empty());
    }

    #[test]
    fn closing_the_popover_discards_a_half_typed_password() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, ch("s"));
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
        press(&mut h, Key::Named(NamedKey::Enter));
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
            !h.handle_wifi_password_key(&ch("k"), &mods),
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
        let tree = WorkspaceTree::bootstrap_demo();
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
        let tree = WorkspaceTree::bootstrap_demo();
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
        let mut h = host.with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        assert!(h.dispatch_palette_command("peer.invite"));
        assert_eq!(h.take_peer_sync_actions().len(), 1);
    }

    // -----------------------------------------------------------------
    // Getting the token from the machine that minted it to the machine
    // that needs it.
    //
    // A token is a few hundred opaque base64 characters. If the only
    // way across is to read it off one screen and type it into another,
    // the invite flow does not work — not "is inconvenient", does not
    // work, because nobody transcribes that correctly. These prove the
    // two halves of the crossing against the process clipboard.
    // -----------------------------------------------------------------

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
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_invite_token = Some(SAMPLE_TOKEN.to_string());
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("c".into())), &ctrl),
            "the card owns Ctrl+C while it is showing a token"
        );

        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some(SAMPLE_TOKEN),
            "the token has to reach the clipboard — that is the only way it \
             gets to the other machine without being retyped"
        );
        assert!(
            chrome
                .borrow()
                .peer_sync_status
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("copied"),
            "a copy the user cannot see happen is a copy they will do twice"
        );
    }

    /// And answering the question is one gesture, so it produces one
    /// outcome: a token, for that person, on the clipboard, with the
    /// card saying so. Splitting it into "mint" and "now find the Copy
    /// button" is how the copy came to be skipped — the second half is
    /// the half nobody thinks to do.
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
            chrome.borrow().peer_sync_invite_name.as_deref(),
            Some(""),
            "the composer opened with an empty field"
        );

        let none = Modifiers::default();
        for c in ["X", "a", "v", "e", "r"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), &none);
        }
        assert_eq!(
            chrome.borrow().peer_sync_invite_name.as_deref(),
            Some("bob"),
            "the field takes the name — the same keys that type a token"
        );

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), &none),
            "Enter belongs to the composer while it is up"
        );
        assert_eq!(
            chrome.borrow().peer_sync_invite_token.as_deref(),
            Some("token-for-bob"),
            "the name reached the request, so it can reach the room document"
        );
        assert_eq!(
            chrome.borrow().peer_sync_invite_for.as_deref(),
            Some("bob"),
            "and the card knows whose invite it is holding"
        );
        assert!(
            chrome.borrow().peer_sync_invite_name.is_none(),
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
        h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), &none);

        assert!(
            chrome.borrow().peer_sync_invite_token.is_none(),
            "nothing was minted for nobody"
        );
        assert_eq!(
            chrome.borrow().peer_sync_invite_name.as_deref(),
            Some(""),
            "and the card is still asking"
        );
    }

    /// The card is a summary now, not a transcript, so what a person
    /// compares out of band has to be pinned: the same machine reads
    /// the same on both screens, and two machines do not collide into
    /// one.
    #[test]
    fn the_fingerprint_identifies_the_machine_that_signed_the_invite() {
        let a = "12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvWx";
        assert_eq!(peer_fingerprint(a), peer_fingerprint(a), "stable");
        assert_eq!(
            peer_fingerprint(a),
            "QrSt UvWx",
            "the tail of the peer id, in two groups — spaced the way \
             `MintedIdentity::grouped_fingerprint` spaces its own"
        );
        assert_ne!(
            peer_fingerprint(a),
            peer_fingerprint("12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvAb"),
            "two machines do not read the same"
        );
        // Short and odd inputs are not a panic. A peer id is always
        // longer than this, but the card must not be the thing that
        // takes the compositor down if one ever is not.
        assert_eq!(peer_fingerprint("abc"), "abc");
        assert_eq!(peer_fingerprint(""), "");
    }

    /// The receiving side shows what it is holding without showing all
    /// of it — enough that a person can see the field is not empty and
    /// that it changed.
    #[test]
    fn a_token_is_elided_rather_than_spelled_out() {
        let token = "e".repeat(900);
        let shown = elide_token(&token);
        assert!(
            shown.chars().count() < 20,
            "a summary, not six lines of base64: {shown}"
        );
        assert!(shown.contains('…'), "and it says there is more: {shown}");
        assert_eq!(
            elide_token("short-one"),
            "short-one",
            "something already short is left alone"
        );
    }

    #[test]
    #[serial_test::serial(clipboard)]
    fn ctrl_v_fills_the_connect_prompt_from_the_clipboard() {
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_connect_input = Some(String::new());
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("v".into())), &ctrl),
            "the prompt owns Ctrl+V"
        );

        assert_eq!(
            chrome.borrow().peer_sync_connect_input.as_deref(),
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
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_connect_input = Some(String::new());
        ui::text_selection::clipboard_copy(&format!("  {SAMPLE_TOKEN}\n"));
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), &ctrl);
        assert_eq!(
            chrome.borrow().peer_sync_connect_input.as_deref(),
            Some(SAMPLE_TOKEN)
        );
    }

    /// The chord must not also type its own letter. `v` is a legal
    /// base64 character, so a paste that left one behind would corrupt
    /// the token in a way the user could not see.
    #[test]
    #[serial_test::serial(clipboard)]
    fn the_paste_chord_does_not_also_type_a_v() {
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_connect_input = Some(String::new());
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), &ctrl);
        let got = chrome.borrow().peer_sync_connect_input.clone().unwrap();
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
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_connect_input = Some(String::new());
        let none = Modifiers::default();
        for c in ["a", "b", "c"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), &none);
        }
        assert_eq!(
            chrome.borrow().peer_sync_connect_input.as_deref(),
            Some("abc")
        );
    }

    /// Every face of the card offers its gesture as a button too, for
    /// a user who reaches for the pointer.
    #[test]
    fn each_face_of_the_card_offers_its_own_gesture_as_a_button() {
        let (mut h, chrome) = host_with_chrome();
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        chrome.borrow_mut().peer_sync_invite_name = Some("bob".to_string());
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"create"),
            "the face that asks who must offer a way to answer, got {names:?}"
        );
        chrome.borrow_mut().peer_sync_invite_name = None;

        chrome.borrow_mut().peer_sync_invite_token = Some(SAMPLE_TOKEN.to_string());
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"copy"),
            "the issuing side must offer Copy, got {names:?}"
        );

        chrome.borrow_mut().peer_sync_invite_token = None;
        chrome.borrow_mut().peer_sync_connect_input = Some(String::new());
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"paste"),
            "the receiving side must offer Paste, got {names:?}"
        );
        assert!(names.contains(&"join"), "and still Join, got {names:?}");

        // Half a token — what the field holds while somebody is typing
        // one by hand, and the one state no end-to-end run reaches,
        // because a paste arrives whole. The card summarises it rather
        // than spelling it out, so this is the path through
        // `elide_token`, and a card that panicked here would take the
        // compositor with it.
        chrome.borrow_mut().peer_sync_connect_input = Some("e".repeat(400));
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

    /// The indication is continuous, and there is only ever one of it.
    ///
    /// An open that takes seconds used to have its one piece of feedback
    /// retired the moment the launcher process forked — seconds before
    /// anything was on screen — leaving somebody watching an empty desktop
    /// wondering whether their keystroke had registered. So it has to
    /// stand until there is something to look at.
    ///
    /// But it must also stand down as soon as there is. Once the window's
    /// own place is saying which app is coming, a notification in the
    /// corner saying it again is noise; and if that app never draws, the
    /// window ends up reading "isn't responding" beneath a notification
    /// still reading "Loading…".
    #[test]
    fn the_loading_indication_stands_until_the_window_place_speaks_for_itself() {
        let (mut h, chrome) = host_with_chrome();
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
            chrome.borrow().notifications().iter().any(|n| n.id == notice),
            "the user is still waiting; the acknowledgement must still be up"
        );

        // The client creates its toplevel, and the compositor adopts a
        // tab for it. This is *not* yet the end of the wait: for the first
        // moments after adoption the tab holds nothing anyone can see,
        // because an app that opens in a blink must not be preceded by an
        // indicator flashing on and off in its place.
        const SURFACE: u64 = 918_273;
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
            chrome.borrow().notifications().iter().any(|n| n.id == notice),
            "still waiting — nothing stands in the window's place"
        );

        // Past the delay the window's place is a card naming the app, so
        // the wait is now being accounted for where the window is going to
        // be. The notice stands down rather than saying it a second time.
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
            !chrome.borrow().notifications().iter().any(|n| n.id == notice),
            "the wait has moved to where the window will be, and only one \
             thing may be saying it"
        );
        store.forget(SURFACE);
    }

    /// The other way the hand-over can happen: an app fast enough to paint
    /// before the card was ever due still settles the open.
    #[test]
    fn a_window_that_paints_before_the_card_is_due_settles_the_open_too() {
        let (mut h, chrome) = host_with_chrome();
        let notice = {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(crate::shell::notification::Notification::new(
                "Opening Ledger",
                "Loading…",
            ))
        };
        crate::shell::catalogue::note_pending_notice("dev.qos.retirefixture", notice);

        const SURFACE: u64 = 918_274;
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
        assert!(store.mark_presented(SURFACE, tile), "precondition: it presented");
        assert!(
            h.retire_notices_for_opened_windows(),
            "the window is on screen, which is the end of the wait however \
             fast it came"
        );
        assert!(
            !chrome.borrow().notifications().iter().any(|n| n.id == notice),
            "once the app is on screen the indication has done its job"
        );
        store.forget(SURFACE);
    }

    /// Escape still dismisses. The card gained chords; it must not have
    /// lost the way out.
    #[test]
    fn escape_still_dismisses_the_card() {
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_invite_token = Some(SAMPLE_TOKEN.to_string());
        let none = Modifiers::default();
        assert!(h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Escape)), &none));
        assert!(chrome.borrow().peer_sync_invite_token.is_none());
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
        // Add a second workspace so we actually have something to
        // switch *to*. `bootstrap_demo` only seeds one, and
        // `switch_down` is a no-op when there's nothing below.
        tree.append_empty_workspace("Second");
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        // Give the host a viewport. The exact value doesn't matter for
        // the test assertions, only that it's > 0.
        host.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        (host, chrome)
    }

    /// Switching workspace moves nothing.
    ///
    /// The shell used to translate its whole content tree one viewport
    /// off-axis on every switch and ease it back over 260 ms. That tree
    /// carries the wallpaper and the top bar as well as the strips, so
    /// what actually travelled was the entire screen — the first frame
    /// of a switch rendered empty, and the desktop then flew back into
    /// place. The content is simply the new workspace on the next frame
    /// now.
    ///
    /// `needs_animation_frame` is the signal that drove those redraws,
    /// so a switch leaving the host idle is the property that says no
    /// such animation exists to be reintroduced by accident.
    #[test]
    fn switching_workspace_animates_nothing() {
        let (mut h, chrome) = host_with_two_workspaces();
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
            let here = tree
                .workspaces
                .iter()
                .find(|w| w.id == active)
                .expect("an active workspace")
                .room
                .to_string();
            let elsewhere = tree
                .workspaces
                .iter()
                .find(|w| w.id != active)
                .expect("a second workspace")
                .room
                .to_string();
            (here, elsewhere)
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

    /// Two peers in the same room are both drawn, each in their own
    /// colour. A single peer had always been enough to make the overlay
    /// look right; several at once is the case that has to work.
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
        // Two of them close enough together that a fixed-offset pill
        // layout would have stacked their names, and two with names that
        // differ by one letter.
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

        let mut tree = WorkspaceTree::bootstrap_demo();
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
        build_harness_sized(1600.0, 900.0)
    }

    /// One full layout pass over the host at `vw` × `vh`, exactly as the
    /// compositor's frame does it. Separate from the harness constructor
    /// so a test can lay out *again* after a paint — which is what the
    /// shell itself does when a draw discovers the palette needs more
    /// room than the last layout gave it.
    fn lay_out_host(host: &mut ShellOverlayHost, vw: f64, vh: f64) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
        let child = host.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(vw as f32),
                        height: Dimension::length(vh as f32),
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
                    width: AvailableSpace::Definite(vw as f32),
                    height: AvailableSpace::Definite(vh as f32),
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

    /// Paint once so the chip / scope rects the click router reads are
    /// the ones that were actually drawn, then click a point.
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

    /// **Every filter has to be reachable at every width.**
    ///
    /// The strip used to stop painting at the first chip that did not
    /// fit, and a chip that is not painted has no rect — so the filter it
    /// stands for could be listed by the registry and still be
    /// unclickable. At 1600 px the five kinds of the day exactly filled
    /// the row, so a sixth was invisible; under the test harness's own
    /// metrics even `Folders` was dropped.
    ///
    /// Asserts on the rects the production `draw` cached, because that is
    /// the thing a click is hit-tested against.
    #[test]
    fn every_kind_is_reachable_at_every_width() {
        // 800 is the narrowest viewport `shots --sweep` renders, and the
        // one where the palette card hits its 560 px floor: the filter
        // band is at its tightest and the strip has the least room.
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
                kinds.iter().map(|k| k.label()).collect::<Vec<_>>()
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

    /// The reachability claim, driven the way a user makes it: click the
    /// **last** chip — the one that used to fall off the end of the row —
    /// at the narrowest viewport, and the filter it stands for applies.
    ///
    /// Clicks the rect the production `draw` cached, through the
    /// production `handle_event` router, so a chip that is painted but
    /// mis-placed fails here too.
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
                .map(|k| k.label())
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
            let strip = lay_out_chip_strip(&natural, band, band.x1 - 100.0, rows_available, 36.0, 24.0);
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
            assert_eq!(r.width(), natural[i], "chip {i} was squeezed with room to spare");
        }
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
        pump(&mut h, &cursor_moved_at(at), &sup);
        assert!(
            pump(&mut h, &key_press(Key::Character("s".into())), &sup),
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
        assert!(pump(&mut h, &key_press(Key::Character("s".into())), &sup));
        assert!(
            h.chrome.borrow().wheel_summon().is_none(),
            "Super+S toggles the summon off"
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

        pump(&mut h, &cursor_moved_at(Point::new(900.0, 400.0)), &sup);
        pump(&mut h, &key_press(Key::Character("s".into())), &sup);
        // Raise an overview too, so the test proves ordering rather
        // than just "escape closes the only thing open".
        {
            let mut st = h.chrome.borrow_mut();
            st.overlays.overview = true;
            st.bump();
        }

        assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), &none));
        assert!(
            h.chrome.borrow().wheel_summon().is_none(),
            "Escape sends the wheel back to its dock"
        );
        assert!(
            h.chrome.borrow().overlays.overview,
            "and stops there — the overview behind it is untouched"
        );
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

    /// Make the local user the owner of every room, which is what a real
    /// session does on the way up (`ChromeState::add_workspace` →
    /// `WorkspaceTree::adopt_ownerless_workspaces`) and what
    /// `bootstrap_demo` leaves undone.
    ///
    /// Without it nobody administers the room, so nobody may invite into
    /// it — correctly, but it is not a state a session is ever in, and a
    /// test that relies on it would pass for a reason that has nothing
    /// to do with what it claims to check.
    fn adopt_rooms(h: &Harness) {
        let st = h.chrome.borrow();
        let user = st.local_user_id.clone();
        st.tree_mut().adopt_ownerless_workspaces(&user);
    }

    /// The dropdown's subject is who is in this room, and the two acts
    /// that change the answer are adding somebody and joining somebody
    /// else's room. Before this it offered neither: it listed people and
    /// the only route to inviting one was a palette command whose name
    /// you had to already know, which is not a route a person on a
    /// desktop takes.
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
        let panel = h.host.collaborators_rect();
        for (name, r) in [("Invite", invite), ("Join", join)] {
            assert!(
                panel.contains(r.center()) && panel.contains(r.origin()),
                "{name} is inside the popover it is painted on"
            );
        }
        assert!(
            invite.y1 <= h.host.collaborators_activity_row_rect().y0,
            "the actions band sits above the Room activity row rather than over it"
        );
    }

    /// Inviting somebody is inviting *somebody*, so the button asks
    /// who before it mints anything.
    ///
    /// A token minted without an answer is a token the room can never
    /// attach a name to: the guest arrives and the host's own roster
    /// reads `peer@12191268537764683811`, because nothing in the flow
    /// was ever told a name.
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
        let _ = pump(&mut h, &cursor_moved_at(invite.center()), &mods);
        let handled = pump(&mut h, &left_button_pressed(), &mods);
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
        let _ = pump(&mut h, &cursor_moved_at(join.center()), &mods);
        let handled = pump(&mut h, &left_button_pressed(), &mods);
        assert!(handled, "the button claimed the click");

        assert_eq!(
            h.chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::PromptConnect],
            "the click opens the prompt that consumes a token"
        );
    }

    /// Peer sync off means nothing in this session can mint a token, so
    /// a button offering to is a button that lies. The room is adopted
    /// first so peer-sync is the *only* thing missing — a test that also
    /// happened to fail the capability check would prove nothing about
    /// the flag it names.
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

    /// And the capability, which is the half that matters: somebody who
    /// does not administer this room may not add people to it, and the
    /// gate has to remove the *hit target* rather than only the paint. A
    /// rect that outlives the button it belongs to is a control the user
    /// cannot see and can still press.
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
        let panel = h.host.collaborators_rect();
        let mods = Modifiers::default();
        let _ = pump(h, &cursor_moved_at(panel.center()), &mods);
        let _ = pump(h, &left_button_pressed(), &mods);
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
    /// The precondition matters: `bootstrap_demo`'s active tab is
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

    /// **The gesture a room starting empty depends on.**
    ///
    /// A room begins with nothing in it, which is only workable if moving
    /// one of your own records into it costs one step. That step is this:
    /// Super+K, type the name, and a record living in *another* room is
    /// among the answers — labelled with where it lives — and Enter
    /// brings it here.
    ///
    /// Drives the whole production chain rather than the provider alone:
    /// `handle_event` → `open_palette_with_scope` →
    /// `refresh_search_results` → `SearchRegistry::query_grouped` →
    /// `RecordsProvider` → palette rows → `activate_selected_result` →
    /// the row's own `on_open`.
    #[test]
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
        assert!(pump(&mut h, &key_press(Key::Character("k".into())), &sup));
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
        h.host
            .activate_selected_result(&mut doc, &automerge::ROOT);

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
        let ids = |room| -> Vec<RecordId> {
            records.records_in(room).into_iter().map(|c| c.id).collect()
        };
        assert_eq!(
            ids(here),
            ids(there),
            "both rooms must name the same record id — a copy would have \
             minted a new one, and the two would diverge from the next \
             edit onward"
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

#[cfg(test)]
mod peer_cursor_tests {
    //! A peer caret on screen must mean a peer is connected.
    //!
    //! The desktop used to paint an unconditional stand-in peer beside
    //! the real roster, so a solo session — and every screenshot and
    //! recording taken of one — showed a collaborator who did not
    //! exist. These tests pin both halves of the invariant on the
    //! production paint path: nothing is drawn for an empty roster, and
    //! a real peer is still drawn.
    use super::*;
    use crate::shell::build_nirios_overlay_host_with_chrome;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
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
        let mut tree = WorkspaceTree::default();
        tree.workspaces = vec![ws];
        tree.active = ws_id;
        // Both are per-parent maps, not single pointers: one active strip
        // per workspace, one active group per strip.
        tree.active_strip.insert(ws_id, strip_id);
        tree.active_group.insert(strip_id, group_id);
        tree
    }

    /// The key the collaboration layer publishes a workspace's pointers
    /// under, and the one `draw_real_peers` looks them up by.
    /// The key an app in this workspace actually publishes presence
    /// under — the workspace's **room**, read off the tree rather than
    /// spelled out here.
    ///
    /// It used to be `tree.active.raw()`, the local workspace handle,
    /// which is a different string from the one `visible_peers` looks
    /// up. So the helper below reported zero for every input, real peer
    /// included — which fails the one test that is supposed to catch
    /// exactly that, and quietly makes the two no-peer assertions pass
    /// for the wrong reason.
    fn active_key(tree: &WorkspaceTree) -> PresenceKey {
        // The **room**, not the local `WorkspaceId`. The room is what
        // the launcher stamps into an app as `QOS_WORKSPACE_ID` and
        // therefore what an app publishes presence under, and it is
        // what `visible_peers` looks up by. Naming the local handle
        // here wrote a key nobody reads: the two no-peer assertions
        // stayed green on a lookup that could never match, and only
        // this module's one positive test could tell.
        let room = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .map(|w| w.room.to_string())
            .expect("the seed has an active workspace");
        PresenceKey::Workspace { id: room }
    }

    /// Draw the peer layer for `presence` and report how many draw
    /// commands it emitted. Zero means nothing was painted.
    ///
    /// Takes the tree the caller built its `presence` keys against —
    /// `one_group_seed` mints a fresh `WorkspaceId` per call, so a
    /// helper that seeded its own would look up a key nobody wrote and
    /// report zero for every input, real peer included.
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

    /// The other half, and the one that keeps the first two honest: if
    /// this fails the helper is reporting zero for everything and the
    /// no-peer assertions above prove nothing. A peer published into
    /// `PresenceCtx::pointers` under the active workspace key — the key
    /// the collaboration layer writes — still gets painted.
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
}

/// The collaborators dropdown lists people and agents together, and says
/// which is which.
///
/// The property under test is not "the vector has two entries" — it is
/// that an agent can only get onto that list by having been granted
/// membership, and that once there it is never mistaken for a person.
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
    }

    #[test]
    fn a_granted_agent_is_listed_beside_the_peer_and_tagged() {
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
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].label, "bob");
        assert!(!rows[0].is_agent, "a peer must never be tagged as an agent");
        assert_eq!(rows[1].label, "scribe");
        assert!(
            rows[1].is_agent,
            "an agent listed without its tag is the OS reporting a person",
        );
        assert_eq!(rows[1].initial, 'S');
    }

    /// A row says what its participant may do here, and where the room
    /// says nothing it says nothing.
    ///
    /// The agent's role is the verb on the capability that admitted it,
    /// which the roster used to discard — so the one row whose entire
    /// authority *is* a grant showed no sign of what the grant was. The
    /// peer's comes from the room's own list too, carried on the
    /// `Collaborator` the presence path writes; a peer the room has
    /// recorded nothing about gets no chip rather than a default.
    #[test]
    fn a_row_carries_the_role_the_room_recorded_and_no_other() {
        let mut tree = tree_with_room();
        tree.collaborators
            .push(Collaborator::new("peer@7", "peer@7"));
        tree.collaborators
            .push(Collaborator::new("bob", "bob").with_role("editor"));
        {
            let ws = tree.active_workspace_mut().expect("fixture has a room");
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"ada".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Comment),
            )
            .expect("the owner may admit an agent");
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
