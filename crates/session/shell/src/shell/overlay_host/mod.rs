//! Shell overlay host — wraps the root content widget and draws modal
//! overlays (command palette, overview, quick settings) on top.
//!
//! `build_tilo_root` returns this widget so the shell has exactly one
//! place to route global keyboard events and one place to render the
//! overlay stack. Overlays own their own state; the host gates their
//! visibility and paints the scrim.
//!
//! `handle_event` consumes the palette-toggle and Esc before propagating
//! to the wrapped content, and an open palette gets first crack at every
//! event so typing and Enter work without the desktop stealing focus.

mod accessibility;
mod activity_served;
mod agent_actions;
mod agent_markers;
mod app_chooser;
mod ask_agent;
mod axis_navigation;
mod bar_popovers;
mod cursor_chat;
mod feedback_point;
pub(crate) use cursor_chat::ChatOut;
mod arrival;
mod person_markers;
mod presence_self;
mod qs_motion;
mod voice;
pub(crate) use agent_markers::AccessibleAgentMarker;

use crate::shell::state::WorkspaceAuthority as _;
use std::time::{Duration, Instant};
use std::{collections::HashMap, sync::Arc};
use ui::num::{f32_of_f64, f64_of_usize, isize_of_usize, usize_of_f64, usize_of_isize};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use settings_store::magnifier::MagnifierStep;
use taffy::{
    Dimension, LengthPercentageAuto, NodeId, Position, Rect as TaffyRect, Size, Style, TaffyTree,
};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::RoundedRect;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::shell::quick_settings::actions::QsActions as _;
use crate::shell::quick_settings::refresh::QsField;
use ui::TextRole;

mod controller;
mod desk;
mod desktop_control_chips;
pub use desk::SuperSurface;
mod device_fabric;
pub(crate) mod dictation_layer;
mod displays;
mod emoji_layer;
mod history;
pub(crate) mod layers;
mod now_playing;
mod overview_grid;
mod palette_flow;
mod parked_workspaces;
mod peer_sync_dispatch;
mod sheet_accessibility;
mod shortcut_steps;
mod shortcut_triggers;
mod shortcuts;
mod strip_switcher;
mod switcher_layer;
mod tab_menu;
mod task_import;
mod top_bar_reveal;
mod transient_zone;
mod widget;
mod window_toolbars;
mod workspace_slide;

pub use controller::{ControllerDelivery, ControllerKey};
use layers::{
    Band, LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, OverlayStack, ShellLayer as _,
};
use tab_menu::{MenuSubject, TabMenuTarget};

pub(crate) use device_fabric::remote_capture_kind_key;
pub use device_fabric::{
    BluetoothHandoffAuthorizationView, DeviceFabricBridge, DeviceFabricUiAction, DeviceFabricView,
    FabricDeviceView, FabricMediaView, FabricTransferStatus, FabricTransferView,
    GamepadAuthorizationView, LocalMediaInputView, LocalMediaOutputView, RealtimeContinuityBridge,
    RealtimeContinuityUiAction,
};
use device_fabric::{
    DeviceFabricPaneRow, bluetooth_handoff_failure_key, device_fabric_pane_rows,
    media_handoff_failure_key, sentence_case, speaker_group_summary,
};
use history::{
    HistoryTargetAddress, clipboard_history_matches_query, clipboard_history_relative_time,
    clipboard_history_source_label, history_target_address,
};
#[cfg(test)]
use shortcuts::configured_shortcut_matches;
pub(crate) use shortcuts::shortcut_chord_matches;
use shortcuts::{
    configured_shortcut_action, shortcut_action_label, shortcut_action_rows,
    shortcut_action_summary, shortcut_app_tab_note, shortcut_chord_from_event,
    shortcut_chord_keycaps, shortcut_guide_chord, shortcut_guide_content_height,
    shortcut_navigation_columns, shortcut_navigation_height, shortcut_navigation_step,
    shortcut_note_lines, shortcut_reference_columns, shortcut_reference_group_height,
    shortcut_reference_groups, shortcut_reference_rows, shortcut_reference_step,
};
#[cfg(test)]
use std::path::PathBuf;

use ui::presence::{PresenceCtx, PresenceKey};
use ui::search::system::filters::{
    FilterStripSlot, FilterText, KindChip, ScopeSegment, SearchFilters, draw_filter_chip,
    toggle_kind,
};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, CrossStripDetach, FloatingWindow, GroupRow, OutputSpan, PROMPT_HEIGHT,
    PermissionPrompt, PromptChoice, SecretPrompt, TextInput, Widget, WindowGroup as UiWindowGroup,
};
use ui::{CommandEntry, CommandPalette};

#[cfg(test)]
use crate::shell::capture_surface::CaptureSurfaceKind;
use crate::shell::capture_surface::{
    CaptureMirrorImage, CaptureSurfaceRequest, PreparedCaptureSurface, capture_mirror_initial_size,
    capture_mirror_origin, visible_capture_spawn,
};
use crate::shell::chrome_state::{Chrome, SearchScopeText as _, WsTab};
use crate::shell::file_picker_overlay::FilePickerOverlay;
pub use crate::shell::file_picker_overlay::file_picker_rect;
use crate::shell::i18n::{t, t_args};
use crate::shell::notification::ChromeNotifications;
pub use crate::shell::peer_sync_overlay::{
    InvitationTerms, InviteRecipientPath, PeerSyncRequest, PeerSyncResponse, PeerSyncSnapshot,
    PendingInvitationSummary, RememberedInvitee,
};
use crate::shell::peer_sync_overlay::{InviteContext, PeerSyncOverlay};
use crate::shell::portal_dialog::PortalDialogController;
use crate::shell::power_menu::PowerMenu;
#[cfg(test)]
use crate::shell::power_menu::PowerMenuClick;
use crate::shell::projection_overlay::ProjectionOverlay;
use crate::shell::quick_settings::root::{
    BluetoothIntent, QuickSettingsRoot, RootAction, SliderCommit, WifiIntent,
};
use crate::shell::search::ResourceKindText as _;
use crate::shell::state::{Capability, GroupId, StripId, WorkspaceId};
use crate::shell::window_menu::WindowMenuAction;
pub use peer_sync_dispatch::{PeerSyncCall, PeerSyncJob, PeerSyncRunner, PeerSyncWorker};
pub use tilo_desktop::collaborators::RosterRow;
use tilo_desktop::collaborators::{CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay};
use tilo_desktop::drill_in as qs;
use tilo_desktop::drill_in::DrillInList;
use tilo_desktop::notifications::{
    CenterLayout, CenterMetrics, CenterOutcome, NotificationCenter, NotificationHost as _,
    NotificationStack,
};
use tilo_desktop::workspace_settings::EXPIRY_CHOICES;
pub use tilo_desktop::workspace_settings::overlay::GrantComposerRects;
use tilo_desktop::workspace_settings::overlay::{
    AgentAccessKey, EventOutcome as WorkspaceSettingsOutcome, MemberSheetKey,
    WorkspaceSettingsOverlay,
};
use ui::widget::ButtonRole;

/// The window an open file chooser is a sheet on, as laid out this frame.
struct AskingWindow {
    strip: StripId,
    group: GroupId,
    /// The whole window group, tab decoration included.
    window: Rect,
    /// The app area; its centre names the display the sheet centres on.
    content: Rect,
    focused: bool,
}

#[derive(Debug)]
struct SessionActionResult {
    action: os_actions::SessionAction,
    result: Result<(), String>,
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "clipboard_manager_tests.rs"]
mod clipboard_manager_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "device_fabric_ui_tests.rs"]
mod device_fabric_ui_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test reads one host and chrome borrow through to its last assertion"
)]
#[path = "launcher_action_coverage_tests.rs"]
mod launcher_action_coverage_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "idle_io_animation_tests.rs"]
mod idle_io_animation_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test reads one chrome borrow through to its last assertion"
)]
#[path = "qs_device_state_tests.rs"]
mod qs_device_state_tests;

mod app_sheets;
mod capture;
mod chords;
mod collaborators;
mod content_drop;
mod follow;
mod foreign_surfaces;
mod keyboard;
mod launches;
mod overlay_routing;
mod overview_input;
mod overview_layout;
mod overview_paint;
mod paint_desktop;
mod paint_top;
mod palette_routing;
mod palette_run;
mod palette_search;
use palette_search::LaunchpadMark;
mod nested_shortcuts;
mod peer_sync;
mod portal_prompts;
mod projection;
mod qs_device_input;
mod qs_device_rows;
mod qs_input;
mod qs_paint;
mod qs_state;
#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test reads one chrome borrow through to its last assertion"
)]
#[path = "qs_wifi_radio_tests.rs"]
mod qs_wifi_radio_tests;
mod queries;
mod room_activity;
mod scene;
mod services;
mod setup;
mod shell_clicks;
mod shortcut_config;
mod shortcut_help;
mod strip_rail;
mod tab_menu_host;
mod tab_rename;
mod thing_menu;
mod top_bar;
mod tree_harvest;
mod wifi_password;
mod window_focus;
mod workspace_content;
mod ws_settings;

use capture::{instant_screen_capture, instant_window_capture, magnifier_step};
use collaborators::{CollaborationPreferences, collaborators_screen_exposure};
#[cfg(test)]
use overview_layout::OVERVIEW_PANEL_START_SCALE;
use overview_layout::{
    OVERVIEW_FACE_AWAY_ALPHA, OVERVIEW_FACES_MAX, OVERVIEW_PANEL_RISE_PX, OVERVIEW_SHARES_MAX_ROWS,
    OverviewMetrics, overview_panel_curve,
};
use overview_paint::OverviewPanelCache;
pub(crate) use portal_prompts::{prompt_choice_to_outcome, prompt_label};
use qs_state::{
    QS_CLIPBOARD_ROWS, QS_WIFI_RESCAN, QsDeviceSource, QsPanelMetrics,
    focus_quick_settings_accessibility, quick_settings_settled_bounds, radio_field,
    scanned_wifi_security, sound_outputs_of,
};
use room_activity::ActivityRefreshGate;
#[cfg(test)]
use room_activity::{
    ACTIVITY_AWARENESS_FALLBACK, enter_activity_workspace, grounded_intent_fallback,
    prepare_status_evidence, should_reveal_activity_on_entry, take_pending_activity_reveal,
};
use shortcut_config::{DesktopControlsSource, SHORTCUT_GUIDE_ROWS, ShortcutGuideFocus};

/// Side-effect dispatcher for a peer-sync request (`/invite`, `/connect`),
/// bridging the transport-agnostic widget layer to `ShellNetwork`.
type PeerSyncBridge = std::rc::Rc<dyn Fn(PeerSyncRequest) -> Result<PeerSyncResponse, String>>;
type FollowBridge = std::rc::Rc<dyn Fn(crate::shell::state::RoomId, Option<String>)>;
/// Optional network presence announcement for the *visual* follow
/// relationship. Kept separate from [`FollowBridge`]: the latter is private
/// local app control, while this one only paints a room-scoped cue remotely.
type FollowPresenceBridge = std::rc::Rc<dyn Fn(crate::shell::state::RoomId, Option<String>)>;
type StatusPresenceBridge =
    std::rc::Rc<dyn Fn(crate::shell::state::RoomId, Option<ui::LiveStatus>)>;
type ActiveTabPresenceBridge =
    std::rc::Rc<dyn Fn(crate::shell::state::RoomId, crate::shell::state::TabId, bool)>;

/// Privileged clipboard-manager operation. Payload bytes never cross this UI
/// bridge; only the compositor can activate a retained item as the live
/// selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardManagerAction {
    Snapshot,
    Activate([u8; 32]),
    SetPinned { id: [u8; 32], pinned: bool },
    Remove([u8; 32]),
    ClearUnpinned,
}

pub type ClipboardManagerBridge = std::rc::Rc<
    dyn Fn(ClipboardManagerAction) -> Vec<crate::compositor::selection::ClipboardHistorySnapshot>,
>;

/// Secret operations stay behind a narrow bridge to the certified personal
/// fabric. This enum intentionally has no `Debug` implementation so a future
/// diagnostic cannot accidentally format a passphrase.
///
/// Each carries the network's authentication mode as the last scan listed
/// it, so the bridge never scans again on the compositor thread.
pub enum WifiSecretAction {
    Lookup {
        ssid: String,
        security: os_actions::WifiSecurity,
    },
    Share {
        ssid: String,
        security: os_actions::WifiSecurity,
        password: String,
    },
}

pub type WifiSecretBridge = std::rc::Rc<dyn Fn(WifiSecretAction) -> Result<Option<String>, String>>;

struct OverlayInputs {
    search_registry: crate::shell::search::SearchRegistry,
    app_launcher: AppLauncher,
    room_records: std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
    collaboration_preferences: CollaborationPreferences,
    shortcut_config: settings_store::hardware::ShortcutConfig,
    shortcut_triggers: crate::shell::automation_triggers::TriggerHost,
    desktop_controls: DesktopControlsSource,
    qs_root: QuickSettingsRoot,
    qs_refresh: crate::shell::quick_settings::refresh::QsRefresh,
    qs_actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions>,
    qs_action_slot: crate::shell::search::providers::actions::SharedQsActions,
    room_activity: crate::shell::room_activity::RoomActivityPane,
    projection_overlay: ProjectionOverlay,
    history_narrator: Option<crate::shell::history_narrative::LocalHistoryNarrator>,
    status_narrator: crate::shell::history_narrative::LocalHistoryNarrator,
    history_describer: crate::shell::history_narrative::HistoryDescriber,
    portal_dialogs: PortalDialogController,
    session_action_policy: crate::shell::search::providers::SessionActionPolicy,
    now_playing: std::rc::Rc<dyn crate::shell::now_playing::NowPlayingSource>,
}

/// How long a launchpad app card waits for the Applications index to take
/// in the installed apps after boot before falling back to the launcher.
/// Whether the focused window is a Wayland client's (a paste reaches it
/// through the seat) rather than an app running inside the shell.
fn focused_tab_is_wayland_window(state: &crate::shell::chrome_state::ChromeState) -> bool {
    let Some(tab) = crate::shell::window_actions::active_tab_id(state) else {
        return true;
    };
    let tree = state.tree();
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|candidate| candidate.id == tab)
        .is_none_or(|tab| {
            !matches!(
                tab.tile,
                crate::shell::state::TileContent::AppPlaceholder { .. }
            )
        })
}

/// Narrowest the permission prompt is drawn over the window that asks: its
/// label and four answers in one row.
const PROMPT_MIN_WIDTH: f64 = 640.0;

const LAUNCHPAD_APP_RESOLVE_WINDOW: Duration = Duration::from_secs(5);
/// How often a waiting launchpad app card asks the index again.
const LAUNCHPAD_APP_RETRY: Duration = Duration::from_millis(25);

/// A launchpad app card chosen before its app resolved.
struct PendingLaunchpadApp {
    app_id: &'static str,
    display_name: &'static str,
    give_up_at: Instant,
}

/// How often a launchpad card's app action, delivered once its app is up,
/// is asked whether it was admitted.
const LAUNCHPAD_ACTION_POLL: Duration = Duration::from_millis(100);

/// A launchpad card's app action awaiting admission: the app may be
/// launching first. The card's title names it if it is refused.
struct PendingLaunchpadAction {
    title: String,
    admission: tilo_actions::remote::Pending<
        Result<
            (u64, tilo_actions::remote::InvocationReceipt),
            tilo_actions::remote::ActionHostError,
        >,
    >,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-toggled, documented piece of overlay/transition \
              state (palette visibility, quick-settings open edge, peer-sync enablement, ...); \
              folding them into an enum or bitflags would not group any of them meaningfully \
              and would ripple through this whole file's field accesses"
)]
pub struct ShellOverlayHost {
    rect: Rect,
    /// Last-used modality plus devices the host has actually discovered.
    /// The shortcut overlay reads this; connection alone never selects a
    /// controller-facing view.
    interaction: ui::InteractionEnvironment,
    /// Direct-touch arena for shell-owned three-finger navigation and pinch
    /// overview. One-finger streams continue to the ordinary widget tree.
    touch_gestures: ui::gesture::TouchGestureArena,
    /// Geometry painted by the keyboard guide, reused by pointer dispatch so
    /// the visible keycaps themselves are the edit targets.
    shortcut_binding_rects: Vec<(settings_store::hardware::ShortcutAction, Rect)>,
    shortcut_reset_rects: Vec<(
        settings_store::hardware::ShortcutAction,
        settings_store::hardware::ShortcutAction,
        Rect,
    )>,
    /// The action waiting for its next pressed chord. Escape clears this
    /// without touching the persisted binding.
    shortcut_rebind: Option<settings_store::hardware::ShortcutAction>,
    shortcut_rebind_message: Option<String>,
    shortcut_guide_focus: Option<ShortcutGuideFocus>,
    /// How far the keyboard guide's body is scrolled, in logical pixels.
    /// Only non-zero when the window is shorter than the guide's content.
    shortcut_guide_scroll: f64,
    /// The scroll range measured by the last paint, so the wheel clamps to
    /// what is actually there.
    shortcut_guide_max_scroll: f64,
    /// The guide's Done button as wide as its label, measured by the last
    /// paint (the label is localised and the type scale is the person's),
    /// so the button never elides it and hit-testing matches what is drawn.
    shortcut_help_done_width: f64,
    desktop_controls: DesktopControlsSource,
    shortcut_help_drawn_rect: Rect,
    /// Saved preferences, kept separate from windowed-session defaults so
    /// editing an unrelated binding never writes those defaults to disk.
    shortcut_config: settings_store::hardware::ShortcutConfig,
    /// Windowed-only effective bindings, shared by routing and shortcut hints.
    nested_shortcuts: Option<settings_store::hardware::ShortcutConfig>,
    /// What starts saved shortcuts from a chord or on schedule.
    shortcut_triggers: shortcut_triggers::ShortcutTriggerState,
    content: Box<dyn Widget>,
    /// Recovery reports of embedded components settled when a workspace
    /// rebuild replaced `content`; handed on by
    /// [`Widget::take_component_close_reports`] so a rebuild cannot lose them.
    desktop_close_reports: Vec<ui::application::ComponentCloseReport>,
    /// The content of workspaces shown recently, kept while another one is
    /// `content` so a switch back puts it back instead of building it again.
    parked_workspaces: parked_workspaces::ParkedWorkspaces,
    /// The pointer-made workspace switch moving between the content kept
    /// for the workspace shown before and the content shown now.
    workspace_slide: Option<workspace_slide::WorkspaceSlide>,
    /// Trusted first-login chrome. While present it is the entire root
    /// surface: desktop content, launch affordances, and every other overlay
    /// stay unreachable until a real identity has been provisioned and the
    /// user dismisses the result screen.
    onboarding: Option<crate::onboarding_view::OnboardingView>,
    /// Layout node for the modal setup/tour surface. A reopened desktop tour
    /// shares the normal desktop tree; first-login setup still owns the root.
    onboarding_node: Option<NodeId>,
    /// One-shot relayout edge raised when onboarding releases the desktop.
    onboarding_released: bool,
    /// One-shot focus edge paired with [`Self::onboarding_released`]. Kept
    /// separate because relayout can consume its edge before the next input
    /// dispatch clears the compositor's cached widget node.
    onboarding_focus_reset: bool,
    /// The strip ribbon (grown into the strip switcher on intent) and the
    /// strip rail; see [`strip_switcher`].
    strip_navigator: tilo_desktop::strip_navigator::StripNavigator,
    /// Chrome revision the switcher's model was built from.
    strip_switcher_revision: Option<u64>,
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
    /// The live application directory, kept to narrow search to one part of
    /// an app when the app asks (`AppHostIntent::OpenSearch`).
    app_actions: Option<std::rc::Rc<crate::shell::search::providers::LiveAppActionsProvider>>,
    /// Starts an installed desktop app through the session launcher. Kept as
    /// an explicit service so fixture hosts can remain process-free.
    app_launcher: AppLauncher,
    /// Backs the current palette entries, indexed in lock-step with
    /// `palette.entries` so Enter/click resolves by ordinal.
    search_results: Vec<crate::shell::search::SearchResult>,
    /// A parameterized result whose arguments the palette is collecting,
    /// one step per parameter. Keeping the inert result (not a pre-bound
    /// callback) lets the last step perform the same strict
    /// `ActionArgs::bind_json` as scripts.
    param_flow: Option<palette_flow::ParamFlow>,
    /// A system action an app prepared for the person (the Store's Add to
    /// this workspace), with the label the palette was opened on: picking
    /// that row starts it with the app's arguments, and the person answers
    /// the rest, its confirmation among them.
    palette_offer: Option<(ui::search::SearchOffer, String)>,
    /// A file row whose other actions the palette is listing (Alt+Enter),
    /// with the query to give back.
    secondary_flow: Option<(crate::shell::search::SearchResult, String)>,
    /// Kinds whose whole group the person asked to see ("Show all").
    expanded_kinds: Vec<crate::shell::search::ResourceKind>,
    /// The kind each "Show all" row stands for, by the number in its id.
    more_rows: Vec<crate::shell::search::ResourceKind>,
    /// Kinds the unfiltered results hold, most hits first: the only kinds
    /// the filter band offers.
    present_kinds: Vec<crate::shell::search::ResourceKind>,
    /// The chrome revision the open palette's rows were built against: a
    /// state the rows show (a switch's ON/OFF) changing moves it, and the
    /// rows are rebuilt.
    search_chrome_revision: u64,
    /// An index the open palette's query reaches was still being built at
    /// the last tick: its rows are owed, and a still capture waits for them.
    search_indexes_settling: bool,
    /// The artwork generation the open palette's rows were built against.
    search_artwork: u64,
    /// Atomic remote-directory admissions are not assumed accepted when the
    /// user presses Enter. Their tickets are polled at frame refresh without
    /// ever blocking the compositor on a broker RPC.
    pending_action_admissions: Vec<(String, crate::shell::search::PendingInvocation)>,
    /// A launchpad app card awaiting the Applications index; see
    /// [`Self::retry_pending_launchpad_app`].
    pending_launchpad_app: Option<PendingLaunchpadApp>,
    /// Where the widget document stood when the launchpad harvest last found
    /// no request in it; see [`Self::settle_launchpad`].
    launchpad_quiet_at: Option<LaunchpadMark>,
    /// Launchpad cards' app actions awaiting admission; see
    /// [`Self::poll_pending_launchpad_actions`].
    pending_launchpad_actions: Vec<PendingLaunchpadAction>,
    /// `palette.query()` after the last fan-out; refresh is
    /// short-circuited when this still matches.
    last_search_query: String,
    /// Provider-index revision paired with `last_search_query`. A live
    /// resource/action change must repaint an already-open palette even when
    /// the person has not changed its text.
    last_search_revision: String,
    /// The row the last refresh selected by itself (its best result), while
    /// the person hasn't moved the selection since; `None` once they have.
    /// A slower provider's rows then re-select the new best result, and
    /// only a choice the person made is kept across them.
    auto_selected_row: Option<String>,
    /// Kinds the chip strip has toggled on. Empty means unfiltered.
    active_kind_filter: Vec<crate::shell::search::ResourceKind>,
    /// The app chooser an app asked for, while the palette shows it.
    app_chooser: Option<app_chooser::PendingAppChooser>,
    /// The palette's filter band: chips, scope switch, their pointer
    /// targets and semantics, laid out and cached at draw.
    search_filters: SearchFilters,
    /// What [`Self::search_filters`] was last configured with, so an
    /// unchanged band is not rebuilt every frame.
    search_filter_inputs: Option<SearchFilterInputs>,
    /// Records each room holds, shared with the records search provider.
    room_records: std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
    /// Set whenever [`Self::refresh_search_results`] rewrites the
    /// palette's entries, so `take_structure_dirty` rebuilds the taffy
    /// node with the new row count — without this only ~1 result shows.
    palette_layout_dirty: bool,
    /// Host rect and display area the palette's taffy style was last
    /// computed against. See the sizing comment in `build_layout`.
    palette_layout_host: Option<(Rect, Rect)>,
    content_node: Option<NodeId>,
    palette_node: Option<NodeId>,
    /// Shared chrome state; drives whether overview / quick-settings /
    /// ws-settings draw on top of the content.
    chrome: Option<Chrome>,
    /// Last known cursor position, for hit-testing overlay clicks.
    last_cursor: Option<Point>,
    /// The overview's zoom (`ui::motion::Zoom::OVERVIEW`): how far every
    /// window has flown from its place on its strip into the overview's
    /// picture of the workspace. A re-toggle mid-flight reverses from where
    /// the windows are drawn.
    overview_zoom: ui::motion::Presence,
    /// A strip's name being typed in the overview.
    overview_naming: Option<overview_grid::OverviewNaming>,
    /// The overview's live pictures of shell-drawn windows, painted once and
    /// kept while nothing they show changes, so every frame of the zoom
    /// draws the same paint instead of painting each window again.
    overview_thumbnails: Option<(u64, crate::shell::window_previews::LiveThumbnails)>,
    /// Scroll offset (px) for the overview's room list, clamped each
    /// frame so a workspace deletion can't strand it past the tail.
    overview_scroll_y: f64,
    /// Keyboard target in the overview switcher: arrows walk it, Enter
    /// goes there, Esc leaves it alone. `None` while closed; re-seeded
    /// from the current room each time it opens.
    overview_selection: Option<usize>,
    /// The window in the current room's preview row the keyboard is on:
    /// Left and Right walk it, Enter goes there. `None` while the keyboard
    /// is on the room list.
    overview_window_selection: Option<crate::shell::state::TabId>,
    /// One interrupted activation is consumed after its target disappears.
    overview_window_interrupted: bool,
    /// A press on the overview until it is let go: a click or tap on a
    /// window preview goes to that window, a drag carries it to another
    /// room.
    overview_press: Option<tilo_desktop::overview::OverviewPress>,
    /// Settled overview panel recording. Animation frames reuse it and only
    /// change the transform, instead of re-encoding every room row.
    overview_panel_cache: Option<OverviewPanelCache>,
    /// How many times the panel recording was built. Tests use this to prove
    /// a transition does not rebuild it per frame.
    overview_panel_cache_fills: u32,
    /// Overview panel density for the current theme, refreshed each
    /// layout pass — see [`OverviewMetrics`]. Cached so the geometry
    /// helpers hit-testing calls (no theme in hand) read the same numbers
    /// paint does.
    overview_metrics: OverviewMetrics,
    /// Layout-only theme snapshot for the shared compact-modal rule.
    overview_layout_theme: Theme,
    /// The keyboard guide's navigation row step for the theme of the last
    /// layout pass, so the guide's hit-testing and paint share one answer.
    shortcut_navigation_step: f64,
    overview_layout_viewport: Rect,
    /// Quick Settings' appear, drill and height motion (`ui::motion`).
    qs_motion: qs_motion::QsMotion,
    /// The input the person last used, which the next surface that opens
    /// or closes follows: a shortcut lands at once, a click animates.
    input_source: ui::tokens::MotionSource,
    /// A workspace switch was asked for and no frame has been painted since:
    /// the switch owes the one frame that shows the other workspace. The
    /// switch itself does not move (the content is swapped in that frame),
    /// so nothing past that frame is owed.
    workspace_switch_owed: bool,
    /// The input the switch [`Self::workspace_switch_owed`] was asked by: a
    /// chord announces its switch before the content is swapped.
    workspace_switch_source: ui::tokens::MotionSource,
    /// Times each switch from its chord to its first and resting frames.
    workspace_switch_latency: crate::compositor::switch_latency::SwitchLatency,
    /// Previous-frame quick-settings visibility, to detect the open edge.
    qs_was_open: bool,
    /// The notification centre's viewer-local state: scroll position,
    /// keyboard selection and folded groups. The notifications themselves
    /// stay owned by `ChromeState`.
    notification_center: NotificationCenter,
    /// The resolved caption (`xs`) and label (`sm`) sizes the Quick Settings
    /// pages paint with, captured from the theme at the top of each frame so
    /// the notification-history geometry — resolved before a frame has a
    /// theme, like the root's — measures the type it actually draws, with
    /// the ui scale and the text scale both included.
    qs_type_lines: CenterMetrics,
    /// The quick settings panel's width and window margin, from the theme
    /// at the top of each frame (HIG 9.6: a utility Panel is
    /// `panel_width_utility`, grown with the text; it stays the floating
    /// Panel margin inside its display).
    qs_panel: QsPanelMetrics,
    /// Offset into the separate clipboard history. Clipboard and notification
    /// panes retain independent positions when the person switches between
    /// them.
    qs_clipboard_history_start: usize,
    qs_clipboard_wheel_remainder: f64,
    /// Stable keyboard target for clipboard history. The content hash is
    /// retained instead of an index so concurrent fabric merges cannot make
    /// Enter activate a different item than the highlighted one.
    qs_selected_clipboard: Option<[u8; 32]>,
    /// Ephemeral, metadata-only clipboard manager filter. It is never synced
    /// and never causes a retained payload to become the live clipboard.
    qs_clipboard_query: String,
    /// Offset into the complete media/output/device list. Unlike the initial
    /// compact implementation, no enrolled device disappears after row seven.
    qs_device_fabric_start: usize,
    qs_device_fabric_wheel_remainder: f64,
    /// The local source the next device click routes to. This is private
    /// view state, never convergent fabric state.
    qs_selected_source: Option<QsDeviceSource>,
    /// The overlays that are self-contained [`layers::ShellLayer`]s, and the
    /// ordering, dismissal and release bookkeeping they share. The tab menu
    /// is opened by `harvest_pending_tab_context_menus` from the tab bar's
    /// request; the power flyout by its chord and the Quick Settings tile.
    layers: OverlayStack,
    /// Results from session helpers run off the frame thread. Failures must
    /// return to chrome instead of disappearing into the journal.
    session_action_channel: (
        std::sync::mpsc::Sender<SessionActionResult>,
        std::sync::mpsc::Receiver<SessionActionResult>,
    ),
    /// The compositor power service is the single suspend actuator: unlike a
    /// generic `systemctl suspend` worker, it coordinates the lock frame and
    /// reports an eventual logind failure back through `PowerSignal`.
    power_bridge: Option<tilo_power_mgmt::PowerBridge>,
    /// Portal session-operation admission shared by the power menu and the
    /// typed session actions, backed by the registry serving
    /// `org.freedesktop.portal.Inhibit`. A logout/user switch must pass it
    /// before any irreversible OS action is spawned, whichever door it came
    /// through.
    session_action_policy: crate::shell::search::providers::SessionActionPolicy,
    /// When the top bar the person hides shows over the windows.
    top_bar_reveal: top_bar_reveal::TopBarReveal,
    /// A fullscreen window's bar, shown over it when reached for.
    fullscreen_bar_reveal: top_bar_reveal::FullscreenBarReveal,
    /// A finger that came down on a display's top edge while something
    /// hides there, and where: a swipe down from it reveals that.
    top_edge_swipe: Option<(u64, Point)>,
    /// A direct strip pan (touchpad or touch) is in progress or settling.
    /// Its frames, and the frame it comes to rest on, are composited from
    /// layers drawn before it began, so the chrome they hold (the ribbon and
    /// the rail) must not change under it: the position it leaves becomes
    /// the indicators' resting point without showing them. The strip moving
    /// under the fingers is the feedback there.
    strip_pan_active: bool,
    /// Off-thread reader for the OS state quick settings displays.
    /// See [`crate::shell::quick_settings::refresh`] for why the reads left the frame.
    qs_refresh: crate::shell::quick_settings::refresh::QsRefresh,
    /// Workspace-settings transition, geometry, painting chrome, and typed
    /// hit targets. Policy outcomes are applied by this host.
    workspace_settings_overlay: WorkspaceSettingsOverlay,
    /// A rename submit owns its held Enter repeats and release after the draft closes.
    workspace_rename_submit_held: bool,
    palette_was_visible: bool,
    /// Interruptible open/close driver for the command palette.
    palette_transition: ui::motion::Presence,
    /// The checkboxes the shell's own sheets paint, filling as a click
    /// checks them.
    faces: ui::widget::FaceMotions,
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
    /// The step strip pans are drawn on ([`Self::set_strip_pan_quantum`]).
    strip_pan_quantum: Option<f64>,
    /// Workspace id the current `content` was built for.
    /// [`Self::resync_workspace_content`] rebuilds when this disagrees
    /// with `tree.active`. `None` means unbuilt/test stub: the
    /// reconciler only latches the id without rebuilding, so hosts
    /// constructed around hand-made content keep it.
    content_built_for_ws: Option<u64>,
    /// Whether `content` includes the first-use empty state. Crossing the
    /// zero/one-window boundary requires a rebuild even though the strip set
    /// itself did not change.
    content_built_empty: Option<bool>,
    /// A workspace-root swap happened during `update`; the following relayout must rebuild its
    /// Taffy nodes before reading geometry from the replacement widgets.
    workspace_content_layout_dirty: bool,
    /// The focused `(strip, group)` last recorded in the display partition,
    /// so a focus change is noted once.
    last_noted_focus: Option<(StripId, GroupId)>,
    axis_gather_pending: Option<axis_navigation::Gather>,
    /// Hit rect for the "+ Add workspace" affordance at the tail of the
    /// overview's list. `Rect::ZERO` when not painted.
    add_workspace_rect: Rect,
    /// Top of the band below the top bar that sheets, the permission prompt
    /// and the notification stack lay out under. Defaults to
    /// [`CHROME_TOP_OFFSET_PX`]; `with_chrome_top` overrides it.
    chrome_top_y: f32,
    /// Live permission-prompt widget, populated by
    /// `pump_permission_prompt` from the front of
    /// `chrome.pending_grants`. `None` when the queue is empty.
    permission_prompt: Option<PermissionPrompt>,
    permission_prompt_id: Option<uuid::Uuid>,
    /// The app the latched prompt asks for, so it opens over that app's
    /// window ([`Self::permission_prompt_surface`]).
    permission_prompt_app: Option<String>,
    /// The portal's prompt queue, when the compositor serves portals.
    /// `drain_portal_prompts` empties it onto `chrome.pending_grants`.
    portal_prompts: Option<crate::portal::PromptQueueHandle>,
    /// Terminal tabs waiting for the person's answer about the device's own
    /// shell.
    host_shell_allows: Vec<crate::shell::host_shell_allow::PendingHostShellAllow>,
    terminal_device_shell:
        Option<crate::shell::search::providers::terminal_device_shell::TerminalDeviceShellProvider>,
    /// Owns the portal file chooser's queue, modal state, rendering and
    /// exclusive input routing. Keeping those together prevents the host
    /// from growing another set of loosely-coupled modal fields.
    file_picker_overlay: FilePickerOverlay,
    /// Compositor-owned sheets for portal app choices and consent/review
    /// flows. A chooser is a concrete user selection, not a broad grant.
    portal_dialogs: PortalDialogController,
    /// Whether peer-sync palette commands (`/invite`, `/connect`) are
    /// surfaced. It follows the live session transport and can be enabled
    /// when first-login provisioning completes.
    peer_sync_enabled: bool,
    /// Carries peer-sync requests out against `ShellNetwork`, keeping the
    /// widget layer transport-agnostic: on the network's worker in a
    /// session, so a relay round trip never holds a frame. Neither bridge
    /// nor worker in tests and in sessions booted without peer-sync, where
    /// `/invite` and `/connect` report that rather than doing nothing.
    peer_sync: peer_sync_dispatch::PeerSyncDispatch,
    /// Until when (Unix-epoch milliseconds) a pairing link of this
    /// computer's admits a device, so its host is asked for one.
    pairing_until_ms: u64,
    /// When the host was last asked for the devices asking.
    pairing_polled_at: Option<std::time::Instant>,
    /// The last change of the devices another of the person's devices asked
    /// about here, as the sheet has read them.
    certification_seen: u64,
    /// Rooms a screenshot scene stands in as held through the web.
    site_rooms_for_shots: Option<Vec<crate::shell::network::SiteRoom>>,
    /// What each device asking calls itself, for the answer's notice.
    device_labels: std::collections::HashMap<[u8; 32], String>,
    /// Privileged in-process access to the compositor's bounded clipboard
    /// history. Absent in screenshots and embedders.
    clipboard_manager_bridge: Option<ClipboardManagerBridge>,
    /// Certified peer and media-output actions. Remote actions stay on the
    /// exact-target personal lane; local sinks stay inside `PipeWire`.
    device_fabric_bridge: Option<DeviceFabricBridge>,
    realtime_continuity_bridge: Option<RealtimeContinuityBridge>,
    wifi_secret_bridge: Option<WifiSecretBridge>,
    /// Session-local control path into the typed app broker. Follow never
    /// enters the replicated workspace document or crosses the network.
    follow_bridge: Option<FollowBridge>,
    /// Authenticated ephemeral relationship cue for the people already in
    /// this room. It is neither durable workspace state nor a control path.
    follow_presence_bridge: Option<FollowPresenceBridge>,
    status_presence_bridge: Option<StatusPresenceBridge>,
    active_tab_presence_bridge: Option<ActiveTabPresenceBridge>,
    published_active_tab: Option<(crate::shell::state::RoomId, crate::shell::state::TabId)>,
    local_status: Option<String>,
    local_status_origin: ui::StatusOrigin,
    status_sharing: ui::StatusSharing,
    /// Owns the invite/connect prompt state, painting, hit-testing and input.
    peer_sync_overlay: PeerSyncOverlay,
    /// "Join a workspace…": the public workspaces, as a sheet in place.
    directory_sheet: crate::shell::directory_sheet::DirectorySheet,
    /// The faces the bar pulses for an arrival, until the frame bounces
    /// them (`arrival::say_arrived`).
    arrival_pulses: Vec<String>,
    /// Top-right notification stack, above content but below the
    /// permission prompt and modals.
    notification_stack: NotificationStack,
    /// The theme's reduced-motion choice at the last paint, so wakes and
    /// animation frames for the acknowledgement skip a fade it won't show.
    acknowledgement_reduce_motion: std::cell::Cell<bool>,
    /// Where the hardware-key readout was last painted, `None` while none is.
    osd_painted: std::cell::Cell<Option<Rect>>,
    /// The display the readout was raised on, kept while it is up
    /// ([`crate::shell::osd::pin_display`]).
    osd_pin: std::cell::Cell<Option<crate::shell::osd::OsdPin>>,
    /// Where the last paint repeated banners about the whole machine on the
    /// displays other than the one in use.
    machine_banners: Vec<Rect>,
    /// The displays the last paint edged for a ringing call, every display
    /// but the one whose banner shows the call.
    call_edges: Vec<Rect>,
    /// The motion the chrome may play as last drawn (`Theme::motion`), for
    /// transitions started outside a draw.
    motion: std::cell::Cell<ui::tokens::EffectiveMotion>,
    /// Last `chrome.revision` seen by [`Self::resync_strips_from_tree`];
    /// a disagreement means the tree changed shape and live
    /// [`GroupRow`] widgets need the new strip contents. `u64::MAX` at
    /// construction so the first poll always rebuilds.
    last_chrome_revision: u64,
    /// The last [`crate::shell::tab_binding::BroughtForward`] serial whose
    /// window the view has been moved to ([`Self::follow_brought_forward`]).
    brought_forward_shown: u64,
    /// The strip of that window, to frame it on the next navigation pass
    /// even if it already had focus and the strip was panned away.
    brought_forward_reveal: Option<StripId>,
    /// Session-local bindings from `(owner, surface)` to the peer-window tab
    /// whose runtime content was replaced by a `PeerView`. These never enter
    /// `ChromeState` or its replicated workspace document.
    peer_stream_tabs: std::collections::HashMap<(u64, u64), u64>,
    /// Wakes the compositor loop when a peer stream's picture finishes
    /// decoding; handed to every peer view this host mounts.
    peer_picture_wake: Option<media::realtime::PictureWake>,
    /// Quick-settings root-page presentation, geometry and input state.
    qs_root: QuickSettingsRoot,
    // Navigation can shrink the panel before the matching release arrives.
    qs_navigation_release_pending: bool,
    /// The finger holding a Quick Settings slider, read as the pointer held
    /// down by the toolkit's [`ui::widget::SliderTouch`].
    qs_slider_touch: ui::widget::SliderTouch,
    /// Where the now-playing card beside the root page reads players and
    /// sends transport commands.
    now_playing_source: std::rc::Rc<dyn crate::shell::now_playing::NowPlayingSource>,
    /// The now-playing card's focus, pickers and artwork cache.
    now_playing_card: crate::shell::quick_settings::now_playing::NowPlayingCard,
    /// The player the person picked on the card, while it is still running.
    now_playing_preferred: Option<String>,
    /// When the visible card next repaints to advance its playhead.
    now_playing_tick: std::cell::Cell<Option<Instant>>,
    /// The desk widgets and the Super surface.
    desk: desk::DeskState,
    /// Side-effect dispatcher for quick-settings actions. Production
    /// uses [`crate::shell::quick_settings::actions::RealQsActions`]; tests swap in a
    /// recording impl via [`Self::with_qs_actions`].
    qs_actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions>,
    /// Mutable provider handle shared with the shell action catalogue. Replacing
    /// a test dispatcher updates the existing provider rather than registering
    /// a duplicate action source.
    qs_action_slot: crate::shell::search::providers::actions::SharedQsActions,
    /// Shared drill-in chrome for host-owned pages (Bluetooth details).
    qs_aux_drill_in: DrillInList,
    /// When the open Wi-Fi section last asked for a scan: it scans again
    /// while it stays open, so the list follows the room.
    qs_wifi_scanned_at: Option<Instant>,
    /// The section whose host-described rows were last read, so opening the
    /// Clipboard afresh starts at the newest copy with no search.
    qs_hosted_section: Option<crate::shell::quick_settings::QsSection>,
    /// Live battery/network/volume readings, or `None` when nothing
    /// polls — `--shots` and headless tests have no poller so their
    /// pixels don't depend on host hardware. See
    /// [`crate::shell::status`].
    status_bridge: Option<crate::shell::status::StatusBridge>,
    /// The bridge's status as last copied into the chrome, so a frame copies
    /// only what the bridge changed and a slider being dragged keeps the
    /// level it shows until it commits.
    status_copied: std::cell::RefCell<crate::shell::status::SystemStatus>,
    /// Single session-lifetime owner for persistent balance/mono transforms.
    /// Present only in compositor sessions; omitted from shots and unit tests.
    session_audio_policy: Option<crate::shell::session_audio_policy::SessionAudioPolicyOwner>,
    /// Room-activity sheet. Held as a `Widget` with its own hover, scroll, and
    /// filter state, placed by hand like the notification stack.
    room_activity: crate::shell::room_activity::RoomActivityPane,
    /// Background, local-only prose renderer. Deterministic summaries remain
    /// installed until a cited model result passes validation.
    history_narrator: Option<crate::shell::history_narrative::LocalHistoryNarrator>,
    status_narrator: crate::shell::history_narrative::LocalHistoryNarrator,
    /// Describes apps' changes for their History, with the model on this device.
    history_describer: crate::shell::history_narrative::HistoryDescriber,
    status_narrative_request: Option<ui::NarrativeRequest>,
    /// Workspace whose passive activity was last presented. Entering a
    /// different workspace with unseen work opens Catch up once, so the user
    /// does not have to notice and decode a tiny unread badge after time away.
    activity_awareness_workspace: Option<u64>,
    /// What the passive Activity awareness last refreshed from, and when.
    /// A frame whose key is unchanged skips the refresh (and its tree write
    /// lock and record reads) until
    /// [`room_activity::ACTIVITY_AWARENESS_FALLBACK`] passes.
    activity_refresh: ActivityRefreshGate,
    /// What the activity model served apps when they last read it
    /// ([`tilo_desktop::activity::RoomActivityPane::served_revision`]).
    activity_served: Option<u64>,
    /// Unseen work discovered during a workspace transition waits until
    /// Overview/People/another modal releases attention. Cleared as soon as
    /// Catch up is shown or opened manually, so dismissal remains sticky.
    activity_pending_reveal: Option<u64>,
    /// Collaborator roster popover, including its painted geometry and hit
    /// targets. The host only applies its semantic actions to chrome state.
    collaborators_overlay: CollaboratorsOverlay,
    /// Who is here arriving from the bar and leaving (`motion::Appear`).
    collaborators_appearing: ui::motion::Appearing,
    /// The roster as it was last painted at rest, and its panel: what is
    /// drawn arriving, and leaving once it is closed.
    collaborators_painted: Scene,
    collaborators_painted_panel: Rect,
    /// Roster id whose recent summary People shows. Local view state: the
    /// same person stays expanded as the roster reorders.
    expanded_collaborator: Option<String>,
    /// Peer whose live tab focus this session mirrors. Local-only and
    /// ephemeral: follow is a viewing mode, never shared workspace state.
    followed_peer: Option<String>,
    /// The session's calls: who is in which voice channel, and the one this
    /// device is in ([`Self::set_native_media`]).
    native_media: Option<crate::shell::native_media::NativeMediaService>,
    followed_tab: Option<u64>,
    followed_session: Option<u64>,
    /// Room the active Follow command was sent to. Keeping this separate
    /// from the live workspace prevents a later workspace switch from
    /// releasing the wrong set of local app views.
    followed_room: Option<crate::shell::state::RoomId>,
    /// The "Following Ada" banner the browser paints too: it arrives and
    /// leaves gracefully instead of snapping between states, and only its
    /// Stop is a target.
    follow_banner: tilo_desktop::follow_banner::FollowBanner,
    /// The next end of following is the person's own input taking their
    /// view back: the banner says "Stopped following" a moment.
    follow_ended_by_own_input: bool,
    /// Hit targets over live peer cursors. Rebuilt from room presence every
    /// paint; a click is an explicit spatial request to follow that person.
    follow_cursor_targets: Vec<(String, Rect)>,
    agent_markers: agent_markers::AgentMarkers,
    person_markers: tilo_desktop::person_cursor::PersonCursors,
    person_quick_composer: crate::shell::person_quick_composer::PersonQuickComposer,
    /// This person's cursor chat: `/` over a shared window.
    cursor_chat: cursor_chat::CursorChatComposer,
    /// You, as the people in each workspace see you.
    self_presence: presence_self::SelfPresence,
    /// Who was here when the bar last said someone arrived.
    presence_notices: arrival::PresenceNotices,
    /// A capture shows the workspace on screen as public.
    #[cfg(feature = "hosted-test-control")]
    public_for_shots: bool,
    person_quick_composer_node: Option<NodeId>,
    /// The invite, join and pairing sheet's node while it is up.
    peer_sync_node: Option<NodeId>,
    /// The question about one of the person's apps on screen, "Uninstall
    /// \<name\>?" or "Add \<name\> to apps?" ([`app_sheets`]).
    app_sheet: Option<app_sheets::AppSheet>,
    /// The question just answered, drawn leaving until it has gone; it
    /// takes no input.
    app_sheet_leaving: Option<app_sheets::AppSheet>,
    /// The sheet's window node and its own, while it is laid out.
    app_sheet_nodes: Option<(NodeId, NodeId)>,
    /// The sheet opened or closed since the tree was last built.
    app_sheet_changed: bool,
    agent_actions:
        HashMap<crate::shell::agent_quick_composer::ComposerOwner, agent_actions::AgentActions>,
    /// The rooms' trackers, where the composer follows the runs it asked for.
    tracker: Option<std::sync::Arc<crate::shell::broker_services::NativeTrackerRuns>>,
    /// The rooms' old tasks, moved into their trackers.
    task_imports: task_import::TaskImports,
    agent_quick_composer: crate::shell::agent_quick_composer::AgentQuickComposer,
    agent_quick_composer_node: Option<NodeId>,
    /// The workspace menu and the profile card the top bar opens.
    bar_popovers: bar_popovers::BarPopovers,
    /// The ask-agent chord's progress and the apps that answer it.
    selection_requests: crate::shell::selection_agent::SelectionRequests,
    /// A click that picked what a feedback report is about, waiting for the
    /// app under it to name the control there.
    feedback_point: crate::shell::feedback_entry::FeedbackPoint,
    /// The slots Tilo apps declared for their windows' bars.
    window_toolbars: window_toolbars::WindowToolbars,
    /// The tree revision the windows lent to apps the shell mounts were
    /// last matched against the tabs at ([`Self::sync_hosted_windows`]).
    hosted_windows_revision: Option<u64>,
    /// The tabs whose bar says "Controls your desktop", and the
    /// generations they were read at ([`desktop_control_chips`]).
    desktop_control: desktop_control_chips::DesktopControlMarks,
    /// The tooltip naming the window bar's button under the pointer.
    bar_tooltip: ui::widget::tooltip::TooltipHost,
    /// The tiles shown and the pointer, kept current for a presence pump
    /// that runs outside this tree (the hosted shell's). `None` unless
    /// [`Self::share_desk`] asked; the native frame loop reads them directly.
    shared_desk: Option<crate::shell::peer_presence::SharedDesk>,
    /// The Super+P display sheet, including controller, editor, animation,
    /// painted geometry, and event routing.
    projection_overlay: ProjectionOverlay,
    /// The Wi-Fi join prompt — its passphrase field is the only place the
    /// passphrase lives — and the prompt (`WifiPasswordPrompt::id`) it is
    /// typed for.
    wifi_prompt: SecretPrompt,
    wifi_password_prompt: Option<u64>,
    /// Numbered cards painted on every display when Settings asks which
    /// screen is which.
    display_identify: crate::shell::display_identify::DisplayIdentify,
    /// Live window previews the switcher and the overview show, rendered
    /// by the compositor only while one of them is up.
    window_previews: crate::shell::window_previews::WindowPreviews,
    /// Each window the overview's row last drew, in order, and whether its
    /// well showed its live picture: what the panel on screen holds.
    overview_row_drawn: Vec<(u64, tilo_desktop::window_switcher::PreviewState)>,
    /// The focused text field's caret in desktop coordinates, as the
    /// compositor last reported it; the emoji picker and clipboard history
    /// open against it.
    text_caret: Option<Rect>,
    /// Clipboard history items chosen to paste since the compositor last
    /// asked ([`Self::take_clipboard_pastes`]).
    clipboard_pastes: Vec<[u8; 32]>,
    /// Clipboard history choices to paste into an app running inside the
    /// shell: queued this frame, and ready since the last one
    /// ([`Self::take_in_process_pastes`]).
    in_process_pastes: (usize, usize),
    /// A Bluetooth pairing question from the compositor's agent, modal
    /// above the desktop while it waits for the person.
    pairing: crate::bluetooth_prompt::PairingOverlay,
    /// Shared toolkit capture selector. The shell only hosts it and drains
    /// semantic intents; compositor readback/encoding stays below this layer.
    capture_studio: ui::widget::CaptureStudio,
    /// Shift/Alt+Print captures, which skip the chooser and wait here for
    /// the compositor's next clean scene alongside the chooser's intents.
    instant_captures: Vec<ui::widget::CaptureIntent>,
    /// Set when a shortcut changed the magnifier, so the frame re-reads it at once instead of
    /// on its next settings poll.
    magnifier_changed: bool,
    capture_node: Option<NodeId>,
    /// Native exact-pixel screenshot editor. It lives in the shared UI crate;
    /// the shell supplies only artifact bytes and drains semantic commands.
    screenshot_markup: Option<ui::widget::ScreenshotMarkup>,
    screenshot_markup_node: Option<NodeId>,
    /// Live arbitrary-region `PiP` windows. Pixels are refreshed by the
    /// compositor capture executor; placement/input/chrome stay in the shared
    /// widget layer so they behave exactly like other floating surfaces.
    capture_mirrors: Vec<(String, FloatingWindow<CaptureMirrorImage>)>,
    capture_mirror_nodes: Vec<NodeId>,
    capture_mirrors_structure_dirty: bool,
    closed_capture_mirrors: Vec<String>,
    /// App-originated call/navigation/PiP detach requests waiting for the
    /// compositor's floating-surface host. Requests bubble through the normal
    /// widget tree instead of reaching into individual app implementations.
    pending_floating_surfaces: Vec<ui::floating_surface::FloatingSurfaceRequest>,
    pending_floating_surface_updates: Vec<(String, Rect)>,
}

/// Ordered shell scene recordings used by the compositor's retained-damage path.
///
/// `TopBar` and Quick Settings have bounded visual footprints. The backdrop is deliberately
/// opaque to damage selection: callers may use it only after exact encoded-scene equality proves
/// it did not change. The workspace, its overlays and the sections above Quick Settings are
/// opaque except inside their keyed parts ([`ui::scene_parts`]): outside every part
/// they too must be proven equal. Quick Settings is bounded as a whole and has parts too
/// (the panel, the strip ribbon, the strip rail), which narrow its damage to what changed.
pub struct ShellSceneSections {
    pub backdrop: Scene,
    /// OS chrome: its normal controls stay inside the reserved top-bar
    /// band, while anchored surfaces such as the center widget editor stay
    /// above app tabs and client content.
    pub topbar: Scene,
    pub workspace: Scene,
    /// Desktop content over the strips that stays where it is on screen
    /// (notices, the permission prompt, the overview) while the strips are
    /// drawn in canvas coordinates ([`ui::retained_present::unbake_scroll`]);
    /// in `workspace` otherwise. Drawn right after `workspace`.
    pub workspace_overlays: Scene,
    pub topbar_bounds: Option<Rect>,
    pub quick_settings: Scene,
    pub above_quick_settings: Scene,
    pub quick_settings_bounds: Option<Rect>,
    /// What is drawn apart from `workspace` while the strips are drawn in
    /// canvas coordinates; empty otherwise.
    pub panned: PannedSections,
}

/// The parts of the workspace a presenter pans on their own, drawn apart
/// while the strips are drawn in canvas coordinates
/// ([`ui::retained_present::unbake_scroll`]). Each strip draws over its own
/// destination, so two strips whose drawings meet (each draws its shadows
/// past its row) can pan apart.
#[derive(Default)]
pub struct PannedSections {
    /// Every strip with a row on screen, top to bottom, each drawn on its
    /// own in canvas coordinates, with the surfaces floating over it.
    pub strips: Vec<(StripId, Scene)>,
    /// The damage parts each of `strips` marked, in the same order: each
    /// window group is one ([`ui::widget::GroupRow::draw_apart`]),
    /// so a presenter keeping a strip's pixels rasterises only where a
    /// group's drawing changed. `None` for a strip whose parts were not
    /// well nested.
    pub strip_parts: Vec<Option<ui::scene_parts::SectionParts>>,
    /// A strip's floating surfaces drew something. On screen they are drawn
    /// above every strip, which the panned strips cannot express.
    pub floating_drawn: bool,
}

/// Backdrop layers ([`ui::backdrop::layer_scene`]) of the sections
/// [`ShellOverlayHost::draw_scene_sections`] draws, one per section. The
/// sections are composed in this order, so a floating surface's layer names
/// which of them lie below it.
mod backdrop_layer {

    pub const BACKDROP: u32 = 0;
    pub const WORKSPACE: u32 = 1;
    pub const WORKSPACE_OVERLAYS: u32 = 2;
    pub const TOPBAR: u32 = 3;
    pub const QUICK_SETTINGS: u32 = 4;
    pub const ABOVE_QUICK_SETTINGS: u32 = 5;
}

impl ShellSceneSections {
    /// Name each section's backdrop layer for the active recording. The
    /// sections must stay where they are while floating surfaces are drawn
    /// into them.
    pub fn register_backdrop_layers(&self) {
        ui::backdrop::layer_scene(&self.backdrop, backdrop_layer::BACKDROP);
        ui::backdrop::layer_scene(&self.workspace, backdrop_layer::WORKSPACE);
        ui::backdrop::layer_scene(&self.workspace_overlays, backdrop_layer::WORKSPACE_OVERLAYS);
        ui::backdrop::layer_scene(&self.topbar, backdrop_layer::TOPBAR);
        ui::backdrop::layer_scene(&self.quick_settings, backdrop_layer::QUICK_SETTINGS);
        ui::backdrop::layer_scene(
            &self.above_quick_settings,
            backdrop_layer::ABOVE_QUICK_SETTINGS,
        );
    }

    /// Split the sections whose damage is proven by their parts
    /// ([`ui::scene_parts`]) for the active recording. The sections
    /// must stay where they are while anything is drawn into them.
    pub fn track_damage_parts(&self) {
        ui::scene_parts::track(&self.workspace);
        ui::scene_parts::track(&self.workspace_overlays);
        ui::scene_parts::track(&self.topbar);
        ui::scene_parts::track(&self.quick_settings);
        ui::scene_parts::track(&self.above_quick_settings);
    }

    fn in_order(&self) -> [&Scene; 6] {
        [
            &self.backdrop,
            &self.workspace,
            &self.workspace_overlays,
            &self.topbar,
            &self.quick_settings,
            &self.above_quick_settings,
        ]
    }

    /// Append the sections to `scene` in the order they are presented.
    pub fn compose_into(&self, scene: &mut Scene) {
        for section in self.in_order() {
            scene.append(section, None);
        }
    }

    /// The sections composed below a floating surface drawn into backdrop
    /// `layer`'s section, bottom to top.
    pub fn below_layer(&self, layer: u32) -> Option<Vec<&Scene>> {
        self.in_order()
            .get(..usize::try_from(layer).ok()?)
            .map(<[&Scene]>::to_vec)
    }
}

/// Action emitted by a search-provider callback into `ChromeState` when the
/// user picks a peer-sync command. The overlay host drains that typed queue
/// into its `ShellNetwork` bridge so Iroh side-effects stay on the runtime
/// thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncAction {
    /// A palette invocation owns this request until the overlay has applied
    /// the inner presentation action and publishes its terminal result.
    Tracked {
        request_id: u64,
        action: Box<PeerSyncAction>,
    },
    /// Mint an invite for `name`, copying it to the clipboard when
    /// `copy`. The name is written into the room document once the
    /// invitee turns up — it never rides inside the token itself.
    IssueInviteFor { name: String, copy: bool },
    /// Open the invite composer, before anything is minted. What the
    /// collaborators panel's Invite button emits.
    PromptInvite,
    /// Leave the site's workspace `room` (the overview's Leave).
    LeaveSite { room: crate::shell::state::RoomId },
    /// Try again to join `room`, whose workspace says it was not joined or
    /// needs joining again (the workspace menu's Join again).
    RetryJoin { room: crate::shell::state::RoomId },
    /// Open "Join a workspace…", the directory of public workspaces.
    BrowseDirectory,
    /// Report the public workspace `room` to the people who run it.
    ReportWorkspace {
        room: crate::shell::state::RoomId,
        reason: String,
    },
    /// Make the workspace `room` public with `listing`, or private again.
    Publish {
        room: crate::shell::state::RoomId,
        listing: Option<tilo_relay_protocol::directory::Listing>,
    },
    /// Ask whether the workspace `room` is public, for its settings.
    ReadPublication { room: crate::shell::state::RoomId },
    /// Ask the workspace `room` for a bot, or take the request back.
    AskForBot {
        room: crate::shell::state::RoomId,
        ask: crate::shell::network::BotAsk,
    },
    /// Open "Pair a new device" and make its link.
    PairDevice,
    /// User invoked `/connect`. Opens the token-input prompt
    /// (`ShellOverlayHost::draw_peer_sync`); submitting it sends a
    /// [`PeerSyncRequest::AcceptInvite`] through the bridge.
    PromptConnect,
    /// Run one typed network request for an invitation action and return
    /// its JSON output to the tracked caller. Nothing is shown or copied on
    /// this screen: the caller may be a script, an agent or an assistive
    /// tool, and an invitation token is a credential. `workspace` is the
    /// active workspace the action was authorized in; the request fails
    /// rather than act on a different one.
    Network {
        workspace: crate::shell::state::WorkspaceId,
        request: PeerSyncRequest,
    },
}

/// The Store's list of bots, which room settings' Add a bot opens.
const STORE_BOTS_LINK: &str = "tilo-store:bots";

/// Row id of one of a file's other actions (Alt+Enter).
const SECONDARY_PREFIX: &str = "secondary:";
/// A secondary action its row's provider gave, by position:
/// `secondary:provided:<n>`.
const PROVIDED_SLUG: &str = "provided:";

/// The share of the display's height the palette's result list may take
/// before it scrolls: enough to see several groups at once, while the card
/// still leaves the desktop around it visible.
const PALETTE_LIST_SHARE: f64 = 0.55;

/// The tallest the list grows on a large display, in logical pixels at
/// 100 % text: twelve two-line rows. Past that it scrolls; a card standing
/// over most of a 4K screen would cover the desktop the search is about.
const PALETTE_LIST_MAX: f64 = 504.0;

/// How often the host is asked for the devices asking under a pairing link.
const PAIRING_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Glyph that stands for a result kind in the palette. Giving each kind
/// a picture is what replaced the bracketed kind literal the shell used
/// to prepend to every subtitle.
/// The file manager Super+E opens.
const FILES_PACKAGE: (&str, &str) = ("so.tilo.file-explorer", "Files");

// -- Lossy numeric casts, named and scoped one lint at a time --------
//
// This file's layout math is all in `f64` (to match `kurbo`), but
// `taffy`/`vello`/`cosmic-text` want `f32`, and row/slot/count indices
// are `usize`. Every value that flows through these conversions is a
// screen-space pixel, row count, or millisecond duration — nowhere near
// the magnitude where the narrowing would actually lose a representable
// value. Convention: name the conversion, scope the `#[allow]` to it alone, and
// use the named function everywhere instead of a bare `as`.

/// W16 wants ≥ 8 painted frames per open/close. Host demand still
/// clears when the crossfade settles; the compositor consumes this
/// many paints on its own.
const TRANSITION_PAINT_RUN: u32 = 8;

/// The top of the band the room-activity sheet, the permission prompt and
/// the notification stack lay out under. The production builder passes
/// `TOPBAR_HEIGHT_PX` through `with_chrome_top`; this default is what a bare
/// `ShellOverlayHost::new` gets, and it is the same value so a fixture
/// observes the geometry the desktop paints.
const CHROME_TOP_OFFSET_PX: f32 = crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX;

/// Wallpaper gutter at both ends of a strip, the one every desktop keeps
/// ([`ui::widget::STRIP_EDGE_GUTTER_PX`]): without it a full-width group's
/// border, corners and shadow fall off screen at the bezel. It is also the
/// pointer floor the strip ribbon's band on the trailing edge needs. The
/// leading one is part of the strip's canvas and pans away with it.
pub const STRIP_EDGE_GUTTER_PX: f64 = ui::widget::STRIP_EDGE_GUTTER_PX;

/// The trailing gutter, where the strip ribbon sits.
const STRIP_TRAILING_GUTTER_PX: f64 = STRIP_EDGE_GUTTER_PX;

/// One display the shell lays out across, in the logical desktop. A window
/// group's width is resolved against its own display, not the combined
/// desktop.
pub use crate::shell::output_scene::OutputGeometry;

/// What the palette's filter band shows: each registered kind with its
/// label, the chosen kinds, and each scope with its label.
#[derive(Clone, Debug, PartialEq)]
struct SearchFilterInputs {
    kinds: Vec<(crate::shell::search::ResourceKind, String)>,
    selected: Vec<crate::shell::search::ResourceKind>,
    scopes: Vec<(crate::shell::chrome_state::SearchScope, String)>,
    active_scope: crate::shell::chrome_state::SearchScope,
}

/// `(strip_id, groups)` where each group is `(group_id, tab_ids)` — the
/// per-strip snapshot [`ShellOverlayHost::groups_per_strip`] returns for
/// tests.
type GroupsPerStrip = Vec<(StripId, Vec<(u64, Vec<u64>)>)>;

type AppLauncher = std::rc::Rc<dyn Fn(&Chrome, &str, &str)>;

/// A key or button pressed: the person acting, not a pointer passing over.
fn is_press(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput {
            event: winit::event::KeyEvent {
                state: winit::event::ElementState::Pressed,
                ..
            },
            ..
        } | WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            ..
        }
    )
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

/// An unmodified `/` press (Shift allowed: some layouts type `/` with it).
fn is_plain_slash(event: &WindowEvent, modifiers: winit::keyboard::ModifiersState) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput {
            event: winit::event::KeyEvent {
                logical_key: winit::keyboard::Key::Character(character),
                state: ElementState::Pressed,
                ..
            },
            ..
        } if character.as_str() == "/"
    ) && !(modifiers.control_key() || modifiers.alt_key() || modifiers.super_key())
}

// Overview layout constants. Module-level so the draw path,
// `hit_test_overview` and the unit tests share one geometry oracle.

/// The semantic target of the open palette's query field, as the palette
/// widget publishes it.
const PALETTE_QUERY_TARGET: &str = "command-palette:query";

/// Where one `update` of the host spent its time, reported as
/// `SLOW-HOST-UPDATE` when the whole of it took long enough to hold up input:
/// a stalled pump then names its step in the session log.
struct UpdateSteps {
    started: Instant,
    last: Instant,
    steps: [(&'static str, Duration); 20],
    len: usize,
}

/// An update this long delays the frame behind it by a visible amount.
const SLOW_HOST_UPDATE: Duration = Duration::from_millis(50);

impl UpdateSteps {
    fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            steps: [("", Duration::ZERO); 20],
            len: 0,
        }
    }

    fn mark(&mut self, step: &'static str) {
        let now = Instant::now();
        let took = now.saturating_duration_since(self.last);
        crate::compositor::metrics::profile::record_phase(step, took, None);
        if let Some(slot) = self.steps.get_mut(self.len) {
            *slot = (step, took);
            self.len = self.len.saturating_add(1);
        }
        self.last = now;
    }

    /// The `SLOW-HOST-UPDATE` line, when the update was slow.
    fn report(&self, total: Duration) -> Option<String> {
        if total < SLOW_HOST_UPDATE {
            return None;
        }
        let steps = self
            .steps
            .iter()
            .take(self.len)
            .map(|(step, took)| format!("{step}={took:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!("SLOW-HOST-UPDATE total={total:?} ({steps})"))
    }
}

impl Drop for UpdateSteps {
    fn drop(&mut self) {
        if let Some(line) = self.report(self.started.elapsed()) {
            eprintln!("{line}");
        }
    }
}

/// Is the local user the owner of the active workspace? Gates the
/// permission prompt's "Always" button and the Permissions tab's Revoke
/// buttons, via the workspace's capability model (recorded owner, or
/// anyone given `Admin`). No recorded owner means no administrators
/// (fail-closed) — [`crate::shell::state::WorkspaceTree::adopt_ownerless_workspaces`]
/// keeps the local user's own workspaces out of that state.
pub(crate) fn is_active_workspace_owner(state: &crate::shell::chrome_state::ChromeState) -> bool {
    if let Some(forced) = state.test_only_owner_override {
        return forced;
    }
    let user = state.active_local_user();
    let tree = state.tree();
    tree.desktop
        .active_workspace()
        .is_some_and(|ws| tree.governs_room(ws.room()) && ws.may_administer(&user))
}

/// The kind chip a picker opened by "give me a new window" starts lit: Apps,
/// every kind of app the person can start.
fn app_kind_filter() -> Vec<crate::shell::search::ResourceKind> {
    use crate::shell::search::ResourceKind;
    vec![ResourceKind::Application]
}

/// The modal scrim, scaled by `openness` in `0.0..=1.0` so it fades in
/// with the modal opening and out with it closing.
/// Draw one overlay of a damage-tracked section as a keyed part
/// ([`ui::scene_parts`]) that is there every frame, so opening, animating
/// or closing the overlay changes that part alone. `draw` paints the
/// overlay and returns the bounds, in scene coordinates, of everything it
/// painted (shadows and antialiasing included), or `None` when it cannot
/// bound them, which claims all of `desktop`. A draw that painted nothing
/// claims nothing.
pub(crate) fn overlay_part(
    scene: &mut Scene,
    kind: &'static str,
    desktop: Rect,
    draw: impl FnOnce(&mut Scene) -> Option<Vec<Rect>>,
) {
    ui::scene_parts::part_drawn(scene, ui::scene_parts::PartKey::new(kind, ()), |scene| {
        let before = scene.encoding().draw_tags.len();
        let bounds = draw(scene);
        if scene.encoding().draw_tags.len() == before {
            Vec::new()
        } else {
            bounds.unwrap_or_else(|| vec![desktop])
        }
    });
}

fn draw_modal_scrim_at(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    openness: f32,
    theme: &Theme,
) {
    ui::overlay::scrim::modal(scene, transform, rect, openness, theme);
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
    use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup as StateGroup};
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
                app_title: None,
            }
        })
        .collect();
    let active_tab = widget.active_tab_id();
    let width = widget.width();
    StateGroup {
        id: widget.id,
        tabs,
        active_tab,
        width,
        maximized: false,
        focus_history: Vec::new(),
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

/// One row of a review sheet's multi-select: a real checkbox, its label and
/// an optional detail in secondary ink, the row lifted while focused.
struct CheckRow<'a> {
    checked: bool,
    /// Where the box's fill and mark are this frame.
    pose: ui::widget::CheckPose,
    label: &'a str,
    detail: Option<&'a str>,
    focused: bool,
}

fn draw_check_row(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    row: &CheckRow<'_>,
) {
    let radius = f64::from(theme.tokens.radii.item);
    ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(row.focused))
        .paint(scene, transform, rect, radius);
    let pad = f64::from(theme.tokens.spacing.rhythm.sibling);
    let side = 18.0;
    let cy = rect.center().y;
    let check = Rect::new(
        rect.x0 + pad,
        cy - side / 2.0,
        rect.x0 + pad + side,
        cy + side / 2.0,
    );
    ui::widget::Checkbox::paint(
        scene,
        transform,
        theme,
        &ui::widget::CheckboxFace::new(check, row.checked).pose(row.pose),
    );
    let style = theme.text(TextRole::Body);
    let x = check.x1 + pad;
    let room = (rect.x1 - pad - x).max(0.0);
    let label = text_renderer.elide_to_width_style(row.label, ui::num::f32_of_f64(room), &style);
    let (w, h) = text_renderer.measure_style(&label, None, &style);
    let y = cy - f64::from(h) / 2.0;
    text_renderer.draw_style(
        scene,
        &label,
        theme.tokens.colors.text_primary,
        transform.pre_translate((x, y).into()),
        None,
        &style,
    );
    if let Some(detail) = row.detail {
        let dx = x + f64::from(w) + pad;
        let room = (rect.x1 - pad - dx).max(0.0);
        let detail = text_renderer.elide_to_width_style(detail, ui::num::f32_of_f64(room), &style);
        text_renderer.draw_style(
            scene,
            &detail,
            theme.tokens.colors.text_dim,
            transform.pre_translate((dx, y).into()),
            None,
            &style,
        );
    }
}

/// Clones `theme` with `surfaces.raised` set to the panel ground, so the
/// fields and wells `CommandPalette` paints from `surfaces.raised` sit on
/// its panel rather than reading as raised cards. The palette's own body is
/// the floating material.
fn palette_surface_theme(theme: &Theme) -> Theme {
    let mut themed = theme.clone();
    themed.tokens.colors.surfaces.raised = ui::chrome::shell::panel_ground(theme);
    themed
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "tab_context_menu_tests.rs"]
mod tab_context_menu_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "tab_rename_tests.rs"]
mod tab_rename_tests;

#[cfg(test)]
#[path = "empty_workspace_tests.rs"]
mod empty_workspace_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact overview layout geometry is the behavior under test"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "overview_panel_tests.rs"]
mod overview_panel_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact transition endpoints are the behavior under test"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "overview_transition_tests.rs"]
mod overview_transition_tests;

#[cfg(test)]
#[path = "wifi_password_tests.rs"]
mod wifi_password_tests;

#[cfg(test)]
#[path = "open_window_rows_tests.rs"]
mod open_window_rows_tests;
#[cfg(test)]
#[path = "search_launcher_tests.rs"]
mod search_launcher_tests;
#[cfg(test)]
#[path = "store_index_arrival_tests.rs"]
mod store_index_arrival_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "palette_dispatch_tests.rs"]
mod palette_dispatch_tests;

#[cfg(test)]
#[path = "peer_sync_worker_tests.rs"]
mod peer_sync_worker_tests;

#[cfg(test)]
#[path = "two_workspace_host_tests.rs"]
mod two_workspace_host_tests;

#[cfg(test)]
#[path = "parked_workspaces_tests.rs"]
mod parked_workspaces_tests;

#[cfg(test)]
#[path = "capture_tool_tests.rs"]
mod capture_tool_tests;
#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod people_motion_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact modal curve endpoints are the behavior under test"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "modal_curve_tests.rs"]
mod modal_curve_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact overlay hit-test geometry is the behavior under test"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "overlay_event_absorb_tests.rs"]
mod overlay_event_absorb_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic host geometry fixtures use exactly representable values"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "overlay_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "maximize_harvest_tests.rs"]
mod maximize_harvest_tests;

#[cfg(test)]
#[path = "resync_strips_tests.rs"]
mod resync_strips_tests;

#[cfg(test)]
#[path = "peer_cursor_tests.rs"]
mod peer_cursor_tests;

#[cfg(test)]
#[path = "multi_display_tests.rs"]
mod multi_display_tests;

#[cfg(test)]
#[path = "transient_zone_tests.rs"]
mod transient_zone_tests;

#[cfg(test)]
#[path = "window_bar_location_tests.rs"]
mod window_bar_location_tests;

#[cfg(test)]
#[path = "hosted_window_tests.rs"]
mod hosted_window_tests;

#[cfg(test)]
#[path = "room_window_stream_tests.rs"]
mod room_window_stream_tests;

/// The collaborators dropdown lists live people and agents together, and
/// says which is which. Dormant membership stays in workspace settings:
/// immediate presence never invents an idle session.
#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "roster_rows_tests.rs"]
mod roster_rows;

#[cfg(test)]
pub(crate) fn install_fixture_search_provider(
    host: &mut ShellOverlayHost,
    provider: std::rc::Rc<dyn crate::shell::search::SearchProvider>,
) {
    host.search_registry.register_or_replace(provider);
}
