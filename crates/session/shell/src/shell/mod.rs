//! The desktop shell: workspace tree, chrome, input policy and the services the
//! compositor hosts.
//!
//! To see it, drive a running compositor rather than eyeballing a checklist:
//! `cargo run --bin shell -- --backend headless` plus `tilo-control`
//! (`scripts/headless-examples/`, `docs/headless-testing.md`), or render stills
//! and recordings with `cargo run --bin shots`. `--backend winit` (the default)
//! runs the compositor in a window inside an existing X11/Wayland session.

pub mod agent_admission;
pub(crate) mod animation_diagnostics;
pub mod app_action_access;
pub mod app_action_consent;
pub mod app_close;
pub mod app_notice_card;
pub mod app_tabs;
pub(crate) mod audio_output;
pub(crate) mod audio_writer;
pub mod automation_folders;
pub mod automation_host;
pub mod automation_records;
pub mod automation_triggers;
pub(crate) mod broker_services;
pub use broker_services::decide_receipt_against_surfaces;
pub(crate) mod agent_quick_composer;
pub mod capture_protection;
pub(crate) mod capture_surface;
pub mod catalogue;
mod certified_action_session;
pub mod channel_encryption;
pub mod channel_policy;
pub mod chrome_state;
pub mod closed_tabs;
pub mod codec_pack_notice;
pub(crate) mod cold_record_reads;
pub mod collab_broker;
pub mod collab_broker_capnp;
pub mod collab_core;
pub mod collab_history;
pub mod collab_outbound;
pub mod collab_presence;
pub mod color_pipeline;
pub mod continuity_policy;
pub mod continuity_suggestion;
pub mod desk_widgets;
pub mod desktop_backdrop;
pub(crate) mod desktop_commands;
#[cfg(target_os = "linux")]
pub(crate) mod desktop_continuation;
pub mod device_fabric;
pub mod device_fabric_runtime;
pub mod device_fabric_secrets;
pub(crate) mod device_pairing;
pub mod device_unlock;
pub mod device_unlock_runtime;
pub mod device_wake;
#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) mod disk_sync_guard;
pub mod display_bars;
pub mod display_confirm;
pub(crate) mod display_control;
pub mod display_identify;
pub(crate) mod display_persistence;
pub mod doc_open;
pub mod document_import;
pub mod durable_writer;
pub mod emoji_recents;
pub mod fabric_clock;
mod file_picker_overlay;
pub mod fixed_chords;
pub mod fixture_inputs;
pub mod floating;
pub mod foreign;
mod foreign_damage;
pub mod frame;
pub mod fs_authority;
pub mod gpu_frames;
pub mod hdr_display;
pub mod hdr_surface;
pub mod hdr_tonemap;
pub mod heap_trim;
pub mod history_narrative;
pub mod host_apps;
pub mod host_shell_allow;
pub mod hosted_inputs;
pub mod i18n;
pub(crate) mod identity_admin;
pub mod identity_provisioning;
pub mod in_process;
pub mod input_policy;
pub(crate) mod invite_policy;
pub mod keyboard_shortcuts;
pub mod launch_gate;
pub(crate) mod launch_readiness;
pub mod launcher;
pub mod link_open;
pub mod live_activities;
pub mod live_presence;
pub(crate) mod local_audio_graph;
mod local_person;
pub mod magnifier;
pub mod media_keys;
pub(crate) mod membership;
pub mod memory_report;
pub mod mls_vault;
pub mod mpris;
pub(crate) mod native_media;
pub(crate) mod native_workspace_owner;
pub mod nearby_transfer;
pub mod network;
pub mod notification;
pub mod notification_history;
pub mod now_playing;
pub mod opening_card;
pub mod osd;
pub mod output_scene;
pub mod overlay_host;
pub mod overview;
pub mod pad_map;
pub mod peer_actions;
pub mod peer_presence;
pub mod peer_sync_overlay;
pub mod peer_view;
pub mod peer_windows;
pub mod persist;
pub mod personal_continuity;
pub mod personal_continuity_registry;
pub mod personal_tethering;
pub mod personal_tethering_runtime;
pub mod personal_workspaces;
pub mod physical_gamepad;
pub mod placement;
pub mod portal_dialog;
mod power_menu;
pub mod present;
pub mod projection;
mod projection_overlay;
pub mod quick_settings;
pub mod realtime_session;
pub mod recent_files;
pub mod record_content;
pub mod record_hydration;
pub mod remote_action_link;
pub mod remote_actions;
pub mod remote_audio;
pub mod remote_capture;
pub mod remote_use;
pub mod render;
pub mod room_activity;
pub mod room_records;
pub mod scanout_candidate;
pub mod screenshot_send;
pub mod search;
pub(crate) mod selection_agent;
pub mod service_members;
pub mod session_audio_policy;
pub mod session_policy;
pub mod settings_failures;
pub(crate) mod shared_display_actions;
pub mod shell_narrator;
pub mod spatial_calibration;
pub mod speaker_calibration;
pub mod speaker_group;
pub mod speaker_group_audio;
pub mod speaker_group_runtime;
pub mod speaker_settings;
pub mod state;
pub mod status;
pub mod storage_notice;
pub mod system_sounds;
pub mod tab_binding;
pub mod tab_documents;
pub(crate) mod task_review;
pub mod test_control;
#[cfg(unix)]
pub mod test_control_socket;
pub mod test_support;
pub mod tool_intents;
pub mod topbar_chrome;
pub mod trusted_ui;
pub mod uri_open;
pub mod window_actions;
pub mod window_appearance;
pub mod window_previews;
pub mod window_transfer;
#[cfg(target_os = "linux")]
pub mod windows_compat;
pub mod workspace_drop;
pub mod workspace_governance;
pub mod workspace_history;

use state::{AppKind as ShellKind, TileContent as ShellTile, WorkspaceTree};
use ui::widget::context_menu::MenuItem;
use ui::widget::{
    AppKind as UiKind, GroupRow, TabDescriptor, TileContent as UiTile, Widget,
    WindowGroup as UiGroup, context_menu::ContextMenu,
};

pub use tilo_desktop::launchpad::{
    EMPTY_WORKSPACE_ACTIVITY, EMPTY_WORKSPACE_CHAT, EMPTY_WORKSPACE_INVITE, EMPTY_WORKSPACE_MEET,
    EMPTY_WORKSPACE_NOTES, EMPTY_WORKSPACE_TASKS,
};

/// One action a tab's right-click menu offers. The
/// [`overlay_host::ShellOverlayHost`] builds its menu from
/// [`tab_context_menu_items`], and every action here is reachable by pointer, keyboard (arrows + Enter in
/// the open menu) and by its [`Self::semantic_key`] through accessibility,
/// scripts and the bench — a menu row is not a capability on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabMenuAction {
    /// Close this tab through the same durable lifecycle the `✕` and the
    /// keyboard chord use (`ShellOverlayHost::close_tab_in_group`).
    CloseTab,
    /// Detach the tab into a new window group immediately to the right of
    /// its current group — the drag-out gesture, without the drag.
    SplitRight,
    /// Detach the tab into a new window group at the end of the strip.
    MoveToNewGroup,
}

impl TabMenuAction {
    /// User-facing label, from the shell's catalogue.
    pub fn label(self) -> String {
        use crate::shell::i18n::t;
        match self {
            Self::CloseTab => t("shell-tab-menu-close"),
            Self::SplitRight => t("shell-tab-menu-split-right"),
            Self::MoveToNewGroup => t("shell-tab-menu-new-group"),
        }
    }

    /// The glyph leading the row, so the menu reads at a glance like every
    /// other menu with icons (HIG 9.3): close, a second column, a window of
    /// its own.
    pub fn icon(self) -> ui::icons::Icon {
        match self {
            Self::CloseTab => ui::icons::Icon::X,
            Self::SplitRight => ui::icons::Icon::Columns2,
            Self::MoveToNewGroup => ui::icons::Icon::AppWindow,
        }
    }

    /// Stable semantic address of the menu row (`tabmenu:<slug>`): the
    /// `semantic:` target a script or the bench names, and the
    /// `perform_semantic_action` target an assistive technology activates.
    pub fn semantic_key(self) -> &'static str {
        match self {
            Self::CloseTab => "tabmenu:close",
            Self::SplitRight => "tabmenu:split-right",
            Self::MoveToNewGroup => "tabmenu:new-group",
        }
    }

    /// The action a semantic target names, if it is one of ours.
    pub fn from_semantic_key(key: &str) -> Option<Self> {
        [Self::CloseTab, Self::SplitRight, Self::MoveToNewGroup]
            .into_iter()
            .find(|action| action.semantic_key() == key)
    }
}

/// The tab menu's rows in order; `None` is a separator. A group with a
/// single tab is not offered the two split actions — detaching its only
/// tab would dissolve the group and re-create it one slot over, a no-op
/// that reads as a glitch.
pub fn tab_context_menu_items(tab_count: usize) -> Vec<Option<TabMenuAction>> {
    let mut items = vec![Some(TabMenuAction::CloseTab)];
    if tab_count >= 2 {
        items.push(None);
        items.push(Some(TabMenuAction::SplitRight));
        items.push(Some(TabMenuAction::MoveToNewGroup));
    }
    items
}

/// A closed [`ContextMenu`] holding [`tab_context_menu_items`] for a group
/// of `tab_count` tabs, plus the index-aligned action list the host reads a
/// selection back through (`ContextMenu::take_selection` yields the row
/// index; the `Arc<dyn Fn()>` callbacks are deliberately inert because the
/// host has to mutate itself and the persisted tree).
pub fn build_tab_context_menu(tab_count: usize) -> (ContextMenu, Vec<Option<TabMenuAction>>) {
    let items = tab_context_menu_items(tab_count);
    let mut menu = ContextMenu::new();
    for item in &items {
        match item {
            Some(action) => {
                menu.push(MenuItem::icon_action(action.icon(), action.label(), || {}));
            }
            None => menu.push(MenuItem::separator()),
        }
    }
    (menu, items)
}

#[cfg(test)]
mod tab_menu_item_tests {
    use super::*;

    #[test]
    fn tab_menu_items_depend_on_group_size_and_keys_round_trip() {
        fn a_single_tab_group_is_offered_only_close() {
            let items = tab_context_menu_items(1);
            assert_eq!(items, vec![Some(TabMenuAction::CloseTab)]);
            let (menu, _) = build_tab_context_menu(1);
            assert_eq!(menu.items().len(), 1);
        }

        fn a_multi_tab_group_gets_close_then_the_split_pair_after_a_separator() {
            let items = tab_context_menu_items(3);
            assert_eq!(
                items,
                vec![
                    Some(TabMenuAction::CloseTab),
                    None,
                    Some(TabMenuAction::SplitRight),
                    Some(TabMenuAction::MoveToNewGroup),
                ]
            );
            let (menu, actions) = build_tab_context_menu(3);
            assert_eq!(menu.items().len(), actions.len(), "one menu row per item");
            let labels: Vec<_> = menu.items().iter().filter_map(|i| i.label()).collect();
            assert_eq!(labels, ["Close tab", "Split right", "Move to new group"]);
        }

        fn semantic_keys_round_trip_and_are_unique() {
            for action in [
                TabMenuAction::CloseTab,
                TabMenuAction::SplitRight,
                TabMenuAction::MoveToNewGroup,
            ] {
                assert_eq!(
                    TabMenuAction::from_semantic_key(action.semantic_key()),
                    Some(action)
                );
                assert!(action.semantic_key().starts_with("tabmenu:"));
            }
            assert_eq!(TabMenuAction::from_semantic_key("tabmenu:nope"), None);
        }

        a_single_tab_group_is_offered_only_close();
        a_multi_tab_group_gets_close_then_the_split_pair_after_a_separator();
        semantic_keys_round_trip_and_are_unique();
    }
}

fn to_ui_kind(k: &ShellKind) -> UiKind {
    // `ui::AppKind` has only five variants; the shell's own `AppKind` still
    // carries `FileExplorer` and `Other(String)` for v1 compatibility.
    match k {
        ShellKind::Terminal => UiKind::Terminal,
        ShellKind::FileExplorer => UiKind::Files,
        ShellKind::Browser => UiKind::Browser,
        // Apps hosted under `Other` are all text editors; without this
        // they wore the unknown-kind settings cog instead of the editor glyph.
        ShellKind::Other(s) if is_editor_label(s) => UiKind::Editor,
        ShellKind::Settings | ShellKind::Other(_) => UiKind::Settings,
    }
}

/// Apps the shell hosts in-process under an `AppKind::Other` label, as
/// `(tab title, label)`. Two columns because the persisted tree names an
/// app by label, while a group rebuilt from widgets (a tab dragged into its
/// own window) has only the tab's title to recover it from.
const HOSTED_OTHER_APPS: &[(&str, &str)] = &[
    ("Notes", "Notes"),
    ("Editor", "Editor"),
    ("Document", "Word"),
];

fn is_editor_label(s: &str) -> bool {
    HOSTED_OTHER_APPS
        .iter()
        .any(|(_, label)| label.eq_ignore_ascii_case(s))
}

/// Falls through to `title` itself for anything the shell does not host.
pub(crate) fn hosted_label_for_title(title: &str) -> String {
    HOSTED_OTHER_APPS
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(title))
        .map_or_else(|| title.to_string(), |(_, label)| (*label).to_string())
}

/// How the shell resolves `t`, for tests that don't want the full `UiTile`
/// type (and its boxed `AppWidget`) on the public surface.
#[cfg(any(test, debug_assertions))]
/// # Panics
/// Panics if its explicitly temporary fixture directory cannot be created.
#[allow(
    clippy::expect_used,
    reason = "These infallible scratch/demo entry points require a private temporary Files fixture."
)]
pub fn resolve_tile_shape_for_test(t: &ShellTile) -> &'static str {
    let inputs = hosted_inputs::HostedTileInputs::fixture().expect("tile-shape fixture");
    // As the shell resolves a tab: an app it mounts in-process first.
    let tab = state::Tab {
        id: 1,
        title: String::new(),
        tile: t.clone(),
    };
    let request = tilo_desktop::resolver::TileRequest {
        room: state::RoomId::new(),
        tab: &tab,
    };
    match mount_in_process(&request, &inputs) {
        Some(tilo_desktop::resolver::TileResolution::App(..)) => return "App",
        Some(_) => return "Notice",
        None => {}
    }
    match to_ui_tile_with_inputs(t, &inputs) {
        UiTile::App(_) => "App",
        UiTile::Stub { .. } => "Stub",
        UiTile::Notice { .. } => "Notice",
    }
}

/// Shared by the initial tree→widget build and by
/// [`overlay_host::ShellOverlayHost`]'s per-revision resync.
pub(crate) fn tile_app_kind(t: &ShellTile) -> UiKind {
    match t {
        ShellTile::Stub { kind, .. } | ShellTile::AppPlaceholder { kind, .. } => to_ui_kind(kind),
        // No `AppKind` describes "arbitrary external client", so both a
        // foreign surface and a peer window wear the catch-all `Settings`
        // glyph — and keep wearing it when a `PeerWindow` binds in and
        // becomes `Foreign`, so the pill doesn't change under the user.
        // The device-local activity tile has no app of its own either.
        ShellTile::WorkspaceActivity
        | ShellTile::Foreign { .. }
        | ShellTile::PeerWindow { .. }
        | ShellTile::HostedWindow { .. }
        | ShellTile::LocalApp { .. } => UiKind::Settings,
    }
}

pub fn to_ui_tile(t: &ShellTile) -> UiTile {
    to_ui_tile_with_inputs(t, &hosted_inputs::HostedTileInputs::default())
}

pub(crate) fn to_ui_tile_with_inputs(
    t: &ShellTile,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiTile {
    match try_to_ui_tile(t, inputs) {
        Ok(tile) => tile,
        Err(error) => UiTile::Notice {
            title: crate::shell::i18n::t("shell-app-unavailable"),
            detail: error.to_string(),
        },
    }
}

pub(crate) fn to_ui_tile_for_tab_with_inputs(
    t: &ShellTile,
    tab: state::TabId,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiTile {
    try_to_ui_tile_for_tab(t, inputs, Some(tab)).unwrap_or_else(|error| UiTile::Notice {
        title: crate::shell::i18n::t("shell-app-unavailable"),
        detail: error.to_string(),
    })
}

/// The content of a tile the shell shows itself rather than mounting an
/// app's component for it ([`mount_in_process`]).
/// # Errors
/// A required service must exist before the content is constructed.
pub fn try_to_ui_tile(
    t: &ShellTile,
    inputs: &hosted_inputs::HostedTileInputs,
) -> Result<UiTile, hosted_inputs::HostedTileError> {
    try_to_ui_tile_for_tab(t, inputs, None)
}

#[expect(
    clippy::too_many_lines,
    reason = "one match over every tile kind, each building its hosted tile with localized fallbacks"
)]
fn try_to_ui_tile_for_tab(
    t: &ShellTile,
    inputs: &hosted_inputs::HostedTileInputs,
    tab: Option<state::TabId>,
) -> Result<UiTile, hosted_inputs::HostedTileError> {
    Ok(match t {
        // The retained activity controller is painted into this ordinary
        // tile by `ShellOverlayHost`, which also owns the workspace-history
        // index. The neutral body exists only for the first reconciliation
        // frame before that controller has its content rect.
        ShellTile::WorkspaceActivity => UiTile::Notice {
            title: crate::shell::i18n::t("shell-activity-title"),
            detail: crate::shell::i18n::t("shell-activity-loading"),
        },
        ShellTile::Stub { kind, label } => UiTile::Stub {
            kind: to_ui_kind(kind),
            label: label.clone(),
        },
        // Pulls the client's most recent buffer out of
        // `compositor::foreign`'s frame store and also publishes the tab's
        // rect back so the compositor can configure the client to size.
        ShellTile::Foreign { app_id, surface_id } => UiTile::App(Box::new(
            crate::shell::foreign::ForeignSurfaceWidget::new(*surface_id, app_id.clone()),
        )),
        // A window someone else has open that this machine hasn't got its
        // own instance of yet; becomes a `Foreign` above once it arrives.
        ShellTile::PeerWindow {
            label,
            note,
            missing: Some(tilo_desktop_model::MissingApp::Installable { display_name }),
            ..
        } if tab.is_some() => UiTile::App(Box::new(app_notice_card::AppNoticeCard::install(
            tab.unwrap_or_default(),
            label,
            display_name,
            note,
        ))),
        ShellTile::PeerWindow {
            label,
            missing: Some(tilo_desktop_model::MissingApp::Unavailable),
            ..
        } => UiTile::Notice {
            title: label.clone(),
            detail: crate::shell::i18n::t_args(
                "peer-window-unavailable",
                &[("app", label.clone().into())],
            ),
        },
        // Its instance here is starting, or being installed first: the
        // place it will open in, moving until it does.
        ShellTile::PeerWindow {
            label,
            note,
            missing: None,
            ..
        } if note == peer_windows::OPENING
            || tab.is_some_and(crate::shell::peer_windows::installing) =>
        {
            let wait = match tab.and_then(crate::shell::peer_windows::installing_package) {
                Some((package_id, app)) => opening_card::Wait::Installing { package_id, app },
                None => opening_card::Wait::Opening,
            };
            UiTile::App(Box::new(opening_card::OpeningCard::new(label, wait)))
        }
        // A legacy window runs only on its host; until a stream of it is
        // attached here, the tab says where it is.
        ShellTile::HostedWindow {
            label,
            host,
            host_label,
            ..
        } => UiTile::Notice {
            title: label.clone(),
            detail: crate::shell::i18n::t_args(
                "hosted-window-running-on",
                &[(
                    "device",
                    if host_label.trim().is_empty() {
                        host.chars().take(8).collect::<String>()
                    } else {
                        host_label.clone()
                    }
                    .into(),
                )],
            ),
        },
        ShellTile::LocalApp {
            app_id,
            label,
            note,
        } if tab.is_some() && !note.is_empty() && note != peer_windows::OPENING => {
            UiTile::App(Box::new(app_notice_card::AppNoticeCard::retry(
                tab.unwrap_or_default(),
                app_id,
                label,
                note,
            )))
        }
        // Asked for (a launch, a restore) and not yet arrived: the place
        // it will open in, moving until it does.
        ShellTile::LocalApp { label, note, .. }
            if note.is_empty() || note == peer_windows::OPENING =>
        {
            let wait = if note.is_empty() {
                opening_card::Wait::Restoring
            } else {
                opening_card::Wait::Opening
            };
            UiTile::App(Box::new(opening_card::OpeningCard::new(label, wait)))
        }
        ShellTile::PeerWindow { label, note, .. } | ShellTile::LocalApp { label, note, .. } => {
            UiTile::Notice {
                title: label.clone(),
                detail: note.clone(),
            }
        }
        ShellTile::AppPlaceholder { kind, label } => match kind {
            // These are their apps' own components, mounted in this process
            // by `mount_in_process`; reaching here, the host mounts none.
            ShellKind::FileExplorer
                if inputs.files.is_none()
                    && tab.and_then(|tab| inputs.files_for_tab(tab)).is_none()
                    && (inputs.is_fixture() || !cfg!(target_os = "linux")) =>
            {
                return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                    "Files startup",
                ));
            }
            ShellKind::Terminal | ShellKind::FileExplorer | ShellKind::Browser => {
                return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                    "in-process apps",
                ));
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Editor") => {
                match tab.and_then(|tab| inputs.document(tab)) {
                    Some(hosted_inputs::HostedDocumentState::Unavailable {
                        path, reason, ..
                    }) => UiTile::Notice {
                        title: path.file_name().map_or_else(
                            || crate::shell::i18n::t("shell-document-unavailable"),
                            |name| {
                                crate::shell::i18n::t_args(
                                    "shell-named-document-unavailable",
                                    &[("name", name.to_string_lossy().into_owned().into())],
                                )
                            },
                        ),
                        detail: reason,
                    },
                    _ => {
                        return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                            "in-process apps",
                        ));
                    }
                }
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Notes") => {
                return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                    "in-process apps",
                ));
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Word") => {
                UiTile::App(Box::new(word_processor::WordProcessorWidget::new()))
            }
            _ => UiTile::Stub {
                kind: to_ui_kind(kind),
                label: label.clone(),
            },
        },
    })
}

/// v1 renders the active workspace's active strip as a `StripView` overlay
/// above the active `WorkspaceView`, with placeholder groups laid out
/// horizontally.
pub fn build_tilo_root(tree: &WorkspaceTree) -> Box<dyn Widget> {
    let (root, _chrome) = build_tilo_root_with_chrome(tree);
    root
}

/// Also returns the shared chrome handle, so integration tests can observe
/// overlay toggles without drilling through the widget tree.
pub fn build_tilo_root_with_chrome(
    tree: &WorkspaceTree,
) -> (Box<dyn Widget>, chrome_state::Chrome) {
    let (host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    (Box::new(host) as Box<dyn Widget>, chrome)
}

/// Returns the concrete [`overlay_host::ShellOverlayHost`] instead of a
/// boxed trait object, so tests can assert on host-local state like
/// `palette_visible()`. Production wiring goes through `build_tilo_root`.
pub fn build_tilo_overlay_host_with_chrome(
    tree: &WorkspaceTree,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_tilo_overlay_host_with_options(tree, false)
}

pub fn build_tilo_overlay_host_with_options(
    tree: &WorkspaceTree,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_tilo_overlay_host_with_handle(tree.clone().shared(), enable_peer_sync)
}

/// Extracted so `ShellOverlayHost::resync_strips_from_tree` can reach for
/// the same conversion when splicing freshly-added groups into a live
/// [`GroupRow`].
pub fn to_ui_window_group(g: &state::WindowGroup, records: &ui::record::RecordStore) -> UiGroup {
    to_ui_window_group_with_inputs(g, records, &hosted_inputs::HostedTileInputs::default())
}

pub fn to_ui_window_group_with_inputs(
    g: &state::WindowGroup,
    records: &ui::record::RecordStore,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiGroup {
    let tabs_with_content: Vec<(TabDescriptor, UiTile)> = g
        .tabs
        .iter()
        .map(|t| {
            let kind = tile_app_kind(&t.tile);
            (
                TabDescriptor {
                    id: t.id,
                    title: t.title.clone(),
                    app_kind: Some(kind),
                    icon: crate::app_icons::for_tile(&t.tile),
                },
                to_ui_tile_for_tab_with_inputs(&t.tile, t.id, inputs),
            )
        })
        .collect();
    let mut group =
        UiGroup::new(g.id, tabs_with_content, g.active_tab, g.width).with_maximized(g.maximized);
    // Every tab gets its own document before it is ever synced, so no
    // hosted app writes a keystroke into the shell's own state.
    tab_documents::bind_group_records(&mut group, records);
    group
}

/// Wires the hosted file explorer's document activation to
/// [`doc_open::open_in_workspace`] — the explorer asks, the compositor
/// answers. Named so a test can install it against its own chrome too.
pub fn install_document_opener(chrome: &chrome_state::Chrome) {
    file_explorer::open_with::set_open_with_route(std::rc::Rc::new(ShellOpenWithRoute {
        chrome: chrome.clone(),
    }));
    let chrome = chrome.clone();
    file_explorer::files_app::set_document_opener(move |path| {
        open_legacy_document_from_files(&chrome, path)
    });
}

/// Preserve the standalone synchronous opener for legacy embeddings. It runs
/// inside the Files event path, where an admission preflight would re-enter
/// the tile's record loan. Hosted native Files use the deferred owner route
/// below instead.
fn open_legacy_document_from_files(
    chrome: &chrome_state::Chrome,
    path: &std::path::Path,
) -> Result<(), String> {
    let native_app = { chrome.borrow().hosted_inputs.native_app_for_path(path) };
    if let Some(app) = native_app {
        let path = path.canonicalize().map_err(|error| {
            crate::shell::i18n::t_args(
                "shell-path-open-failed",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.to_string().into()),
                ],
            )
        })?;
        // The editor reads the file itself; read here, a file on the
        // workspace mount would hydrate on the compositor loop.
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        window_actions::open_hosted_after_setup(
            &mut chrome.borrow_mut(),
            app.kind,
            title,
            window_actions::Destination::Here,
            |state, tab| {
                {
                    let mut tree = state.tree_mut();
                    tree.native_document_tabs.insert(
                        tab,
                        state::NativeDocumentReference {
                            provider_id: app.package_id.to_string(),
                            path: path.clone(),
                        },
                    );
                    tree.queue_save();
                }
                state.hosted_inputs.bind_document(
                    tab,
                    hosted_inputs::HostedDocument {
                        provider_id: app.package_id,
                        path,
                    },
                );
            },
        );
        return Ok(());
    }
    doc_open::open_in_workspace(path, chrome)
        .map(|_| ())
        .map_err(|error| error.user_message())
}

/// Admit an exact native document record before publishing its tab/input pair.
/// This is called only by the owning Files open route after its record loan has
/// ended, so `TileHost` can retain its existing-only record-follow contract.
pub(crate) fn open_document_from_files(
    chrome: &chrome_state::Chrome,
    path: &std::path::Path,
) -> Result<(), String> {
    let native_app = { chrome.borrow().hosted_inputs.native_app_for_path(path) };
    if let Some(app) = native_app {
        let path = path
            .canonicalize()
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        // The editor reads the file itself; read here, a file on the
        // workspace mount would hydrate on the compositor loop.
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file(&path),
            ui::record::RecordKind::new("text-document"),
        );
        let records = { chrome.borrow().records().clone() };
        match records.try_open_record(&checkout) {
            Ok(_) => {}
            Err(ui::record::RecordOpenError::MissingRecord) => {
                records.create_record(&checkout).map_err(|error| {
                    format!(
                        "cannot open {}: document storage unavailable: {error}",
                        path.display()
                    )
                })?;
                records.try_open_record(&checkout).map_err(|error| {
                    format!(
                        "cannot open {}: document storage unavailable: {error}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot open {}: document storage unavailable: {error}",
                    path.display()
                ));
            }
        }
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        window_actions::open_hosted_after_setup(
            &mut chrome.borrow_mut(),
            app.kind,
            title,
            window_actions::Destination::Here,
            |state, tab| {
                {
                    let mut tree = state.tree_mut();
                    tree.native_document_tabs.insert(
                        tab,
                        state::NativeDocumentReference {
                            provider_id: app.package_id.to_string(),
                            path: path.clone(),
                        },
                    );
                    tree.queue_save();
                }
                state.hosted_inputs.bind_document(
                    tab,
                    hosted_inputs::HostedDocument {
                        provider_id: app.package_id,
                        path,
                    },
                );
            },
        );
        return Ok(());
    }
    doc_open::open_in_workspace(path, chrome)
        .map(|_| ())
        .map_err(|error| error.user_message())
}

/// Files' "Open With": candidates, launches and defaults all come from
/// [`doc_open`], under the active workspace's policy and the local person.
struct ShellOpenWithRoute {
    chrome: chrome_state::Chrome,
}

impl file_explorer::open_with::OpenWithRoute for ShellOpenWithRoute {
    fn candidates(&self, path: &std::path::Path) -> file_explorer::open_with::OpenWithCandidates {
        let apps = {
            let state = self.chrome.borrow();
            let tree = state.tree();
            doc_open::open_with_choices(
                path,
                &state.local_user_id,
                tree.desktop
                    .active_workspace()
                    .map(|workspace| &workspace.default_apps),
            )
        };
        file_explorer::open_with::OpenWithCandidates {
            type_label: doc_open::type_label(path),
            apps: apps
                .into_iter()
                .map(|choice| file_explorer::open_with::OpenWithApp {
                    id: choice.id,
                    name: choice.name,
                    is_default: choice.is_default,
                })
                .collect(),
        }
    }

    fn open_with(&self, path: &std::path::Path, app_id: &str) -> Result<(), String> {
        doc_open::open_with_app(path, &self.chrome, app_id)
            .map(|_| ())
            .map_err(|error| error.user_message())
    }

    fn set_default(&self, path: &std::path::Path, app_id: &str) -> Result<(), String> {
        doc_open::set_personal_default(path, app_id)
    }
}

/// The production entry point: threads the *shared* [`WorkspaceTreeHandle`](crate::shell::state::WorkspaceTreeHandle)
/// (the same handle the portal's `GrantStoreHandle` and
/// peer-sync's `WorkspaceTreeReplica` hold) so chrome mutations land on the
/// tree other subsystems read from. Reads it once for the initial widget
/// tree; later rebuilds run `sync_from_tree` on each chrome widget.
/// # Panics
/// Panics if its explicitly temporary fixture directory cannot be created.
#[allow(
    clippy::expect_used,
    reason = "These infallible scratch/demo entry points require a private temporary Files fixture."
)]
pub fn build_tilo_overlay_host_with_handle(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    // In-memory records: a host built without being told where to keep them
    // is a test, a probe or a `--shots` run, none of which may leave a
    // user's tabs on disk. A session passes its own store through
    // [`build_tilo_overlay_host_with_records`].
    build_tilo_overlay_host_with_inputs(
        tree_handle,
        enable_peer_sync,
        ui::record::RecordStore::in_memory(),
        hosted_inputs::HostedTileInputs::fixture().expect("scratch Files fixture"),
    )
}

/// A scratch tab belongs to the compositor, not to any installed app, so
/// its records live under this package in the compositor's own state dir.
pub const SHELL_RECORDS_PACKAGE: &str = "org.tilo.Shell";

pub fn session_record_store() -> ui::record::RecordStore {
    ui::record::RecordStore::open(SHELL_RECORDS_PACKAGE)
}

/// Host services for the shared desktop surface: native tile content, the
/// session's record-creation refusals, and foreign-surface readiness.
pub(crate) struct NativeTileResolver<'a> {
    pub inputs: &'a hosted_inputs::HostedTileInputs,
    pub failures: &'a std::collections::BTreeMap<u64, String>,
}

impl tilo_desktop::resolver::TileResolver for NativeTileResolver<'_> {
    fn resolve(
        &mut self,
        request: tilo_desktop::resolver::TileRequest<'_>,
    ) -> tilo_desktop::resolver::TileResolution {
        if let Some(mounted) = mount_in_process(&request, self.inputs) {
            return mounted;
        }
        tilo_desktop::resolver::TileResolution::Content(to_ui_tile_for_tab_with_inputs(
            &request.tab.tile,
            request.tab.id,
            self.inputs,
        ))
    }
    fn creation_refusal(&self, tab: &state::Tab) -> Option<String> {
        self.failures.get(&tab.id).cloned()
    }
    fn tab_icon(&self, tab: &state::Tab) -> Option<ui::icons::AppImage> {
        crate::app_icons::for_tile(&tab.tile)
    }
}

fn mount_private_feedback(
    apps: &mut in_process::InProcessApps,
    room: &str,
    tab: u64,
) -> Result<ui::application::AppComponent, ui::launch::LaunchError> {
    apps.mount(
        tilo_desktop::feedback::definition(),
        room,
        tab,
        ui::record::RecordId::for_tab(tab),
        |_| {},
    )
}

/// The tab as the framework app's own component, when the host mounts that
/// app in its own process: the terminal on its tab's record, opening
/// sessions through the session's process supervisor; Files on the room's
/// record of places, starting where the shell says and opening documents
/// through the shell; the browser on the room's record of its bookmarks and
/// settings.
pub fn mount_in_process(
    request: &tilo_desktop::resolver::TileRequest<'_>,
    inputs: &hosted_inputs::HostedTileInputs,
) -> Option<tilo_desktop::resolver::TileResolution> {
    use tilo_desktop::resolver::TileResolution;
    let apps = inputs.in_process()?;
    let ShellTile::AppPlaceholder { kind, .. } = &request.tab.tile else {
        return None;
    };
    let room = request.room.to_string();
    let tab = request.tab.id;
    let mounted = match kind {
        ShellKind::Other(name) if name.eq_ignore_ascii_case("Feedback") => {
            mount_private_feedback(&mut apps.borrow_mut(), &room, tab)
        }
        ShellKind::Terminal => apps.borrow_mut().mount(
            terminal::definition(),
            &room,
            tab,
            ui::record::RecordId::for_tab(tab),
            |context| {
                // Only a real session's terminals open sessions; a test's or
                // a capture's show a terminal with none.
                if inputs.opens_sessions() {
                    // Discovery and connect belong to the open worker, too.
                    context.lend(terminal::HostedSessionSource);
                } else {
                    context.lend(terminal::HostedProcessServices::unavailable());
                }
            },
        ),
        ShellKind::FileExplorer => {
            let startup = inputs.files_for_tab(tab).or_else(|| inputs.files.clone());
            // Without a starting place the shell names, only a Linux
            // session lets Files start at the person's home.
            if startup.is_none() && (inputs.is_fixture() || !cfg!(target_os = "linux")) {
                return None;
            }
            let services = inputs.file_services().cloned();
            apps.borrow_mut().mount(
                file_explorer::definition(),
                &room,
                tab,
                ui::record::RecordId::room_default(),
                |context| {
                    if let Some(startup) = startup {
                        context.lend(startup);
                    }
                    if let Some(services) = services {
                        context.lend(services);
                    }
                },
            )
        }
        ShellKind::Browser => apps.borrow_mut().mount(
            browser::definition(),
            &room,
            tab,
            ui::record::RecordId::room_default(),
            |_| {},
        ),
        ShellKind::Other(name) if name.eq_ignore_ascii_case("Messages") => apps.borrow_mut().mount(
            messages::definition(),
            &room,
            tab,
            ui::record::RecordId::room_default(),
            |_| {},
        ),
        ShellKind::Other(name) if name.eq_ignore_ascii_case("Passwords") => {
            apps.borrow_mut().mount(
                passwords::definition(),
                &room,
                tab,
                ui::record::RecordId::room_default(),
                |_| {},
            )
        }
        // Every Notes tab is a notebook of its own, on the tab's record.
        ShellKind::Other(name) if name.eq_ignore_ascii_case("Notes") => apps.borrow_mut().mount(
            notes::definition(),
            &room,
            tab,
            ui::record::RecordId::for_tab(tab),
            |_| {},
        ),
        // An Editor tab opened on a file is the editor's project of that
        // file, which the editor reads from disk itself; one opened on
        // nothing is a project of the tab's own.
        ShellKind::Other(name) if name.eq_ignore_ascii_case("Editor") => {
            let (record, arguments) = match inputs.document(tab) {
                Some(hosted_inputs::HostedDocumentState::Ready(document)) => (
                    code_editor::launch_record(&document.path)
                        .unwrap_or_else(|| ui::record::RecordId::for_tab(tab)),
                    vec![document.path.display().to_string()],
                ),
                // The tile says why the file is not there.
                Some(hosted_inputs::HostedDocumentState::Unavailable { .. }) => return None,
                None => (ui::record::RecordId::for_tab(tab), Vec::new()),
            };
            apps.borrow_mut()
                .mount(code_editor::definition(), &room, tab, record, |context| {
                    context.provide_launch_arguments(arguments);
                })
        }
        _ => return None,
    };
    Some(match mounted {
        Ok(component) => {
            let collaboration = apps.borrow().collaboration(&component, &room);
            TileResolution::App(component, collaboration)
        }
        Err(error) => TileResolution::Failed(error),
    })
}

/// Spacing of the strip column under the top bar. Nothing between the bar
/// and the first strip: the tab row above every window already carries
/// `TAB_PILL_INSET_Y` of air over its pills, and the bar keeps `s1` under
/// its controls, so a margin on top of both was an empty band (24 physical
/// px at scale 2) between the bar and the windows' tab row. The windows
/// keep a margin over the bottom edge, which nothing else provides.
pub(crate) const WORK_AREA: tilo_desktop::composition::WorkArea =
    tilo_desktop::composition::WorkArea::Flush {
        top: 0.0,
        bottom: 12.0,
    };

/// The native chrome the shared desktop surface is built with: the session's
/// top bar and wallpaper, the collaboration launchpad for an empty
/// workspace, and a leading inset for the sidebar wheel.
pub(crate) fn workspace_surface_chrome(
    desktop: &state::DesktopTree,
    chrome: &chrome_state::Chrome,
) -> tilo_desktop::composition::SurfaceChrome {
    let local = chrome
        .borrow()
        .explicit_local_user()
        .map_or_else(crate::shell::state::local_user_id, str::to_owned);
    let topbar = topbar_chrome::TopBarChrome::from_tree_with_local_user(desktop, local)
        .with_chrome(topbar_chrome::native_source(chrome.clone()));
    // Full-screen layer under the translucent top bar, so the bar picks up
    // the gradient tint behind it.
    let backdrop: Box<dyn Widget> = desktop.active_workspace().map_or_else(
        || {
            Box::new(desktop_backdrop::shipped_backdrop(
                desktop_backdrop::DesktopBackdrop::new(),
            )) as Box<dyn Widget>
        },
        |workspace| {
            let workspace_id = workspace.id.0.to_string();
            // Scratch scenes and captures host their own files and read no
            // private wallpaper; they show the shipped one.
            let backdrop = if chrome.borrow().hosted_inputs.file_services().is_some() {
                desktop_backdrop::shipped_backdrop(
                    desktop_backdrop::DesktopBackdrop::for_workspace(workspace_id),
                )
            } else {
                desktop_backdrop::native_backdrop(workspace_id)
            };
            Box::new(backdrop) as Box<dyn Widget>
        },
    );
    tilo_desktop::composition::SurfaceChrome {
        navigation: tilo_desktop::composition::NavigationChrome::InSurface,
        // Window surfaces meet the strip boundary: moving apps inward to keep
        // a clipped shadow would leave a visible shell-owned gutter.
        work_area: WORK_AREA,
        topbar: Box::new(topbar),
        backdrop,
        empty_workspace: Some(Box::new(
            tilo_desktop::launchpad::CollaborationLaunchpad::new(
                overlay_host::workspace_rail_clearance_for_viewport,
            ),
        )),
        // Compact top bars of the displays other than the primary.
        display_chrome: Some(Box::new(display_bars::DisplayBars::new(chrome.clone()))),
        // Reserves canvas-x [0, SIDEBAR_RESERVE_PX] for the wheel disc, which
        // lives in the overlay layer, so wheel and strip pan together.
        leading_inset: f64::from(tilo_desktop::sidebar_wheel_disc::SIDEBAR_RESERVE_PX),
        // An application mounted in this process does its asynchronous work
        // (an action directory question, an invocation, a collaborator's
        // edit) on the compositor's own thread, which parks while nothing
        // stirs: without this, what such an app is asked from outside waits
        // for some unrelated frame, and on an idle desktop never gets an
        // answer.
        notify: Some(std::sync::Arc::new(|| {
            if let Some(wake) = COMPONENT_WAKE.get() {
                wake();
            }
        })),
    }
}

/// What wakes the compositor loop when an application it hosts wants a frame
/// ([`install_component_wake`]).
static COMPONENT_WAKE: std::sync::OnceLock<crate::session_runtime::Wake> =
    std::sync::OnceLock::new();

/// Wake the compositor loop from any thread, for work finished off it that
/// changes what the desktop shows (a wallpaper decoded on a worker).
pub(crate) fn wake_compositor() {
    if let Some(wake) = COMPONENT_WAKE.get() {
        wake();
    }
}

/// Let the applications this session hosts wake its loop. Set once, by the
/// compositor, before any such application is asked anything.
pub(crate) fn install_component_wake(wake: crate::session_runtime::Wake) {
    let _ = COMPONENT_WAKE.set(wake);
}

/// The shared desktop surface for the tree's active workspace, plus the
/// per-strip row pointers the host's reconcilers harvest through. The host
/// rebuilds both together when the active workspace changes.
pub fn build_workspace_root(
    tree: &WorkspaceTree,
    records: &ui::record::RecordStore,
    chrome: &chrome_state::Chrome,
) -> (
    Box<dyn Widget>,
    std::collections::HashMap<state::StripId, *mut GroupRow>,
) {
    let failures = chrome.borrow().record_creation_errors(tree);
    build_desktop_root(&tree.desktop, records, chrome, &failures)
}

/// [`build_workspace_root`] for a desktop alone, with the tabs whose record
/// could not be created already found: all a rebuild needs, so it never
/// copies the tree's replication documents.
pub(crate) fn build_desktop_root(
    desktop: &state::DesktopTree,
    records: &ui::record::RecordStore,
    chrome: &chrome_state::Chrome,
    failures: &std::collections::BTreeMap<u64, String>,
) -> (
    Box<dyn Widget>,
    std::collections::HashMap<state::StripId, *mut GroupRow>,
) {
    let surface_chrome = workspace_surface_chrome(desktop, chrome);
    let state = chrome.borrow();
    let mut resolver = NativeTileResolver {
        inputs: &state.hosted_inputs,
        failures,
    };
    let mut surface = Box::new(tilo_desktop::composition::DesktopSurface::build(
        desktop,
        records,
        &mut resolver,
        surface_chrome,
    ));
    for id in surface.strip_ids() {
        if let Some(row) = surface.row_mut(id) {
            for group in &mut row.groups {
                tab_documents::apply_creation_failures(group, failures);
            }
        }
    }
    let rows = surface_row_ptrs(&mut surface);
    (surface, rows)
}

/// Stable addresses of the surface's strip rows. Each row is boxed inside
/// the surface, so its address holds until the surface is rebuilt; the host
/// replaces this map whenever that happens.
pub(crate) fn surface_row_ptrs(
    surface: &mut tilo_desktop::composition::DesktopSurface,
) -> std::collections::HashMap<state::StripId, *mut GroupRow> {
    surface
        .strip_ids()
        .into_iter()
        .filter_map(|id| surface.row_mut(id).map(|row| (id, std::ptr::from_mut(row))))
        .collect()
}

/// As [`build_tilo_overlay_host_with_handle`], but with the store the
/// open tabs' documents live in named by the caller.
///
/// # Panics
/// In unit tests, panics if the isolated Files fixture cannot be created.
pub fn build_tilo_overlay_host_with_records(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
    records: ui::record::RecordStore,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    #[cfg(test)]
    let inputs = hosted_inputs::HostedTileInputs::fixture().expect("test Files fixture");
    #[cfg(not(test))]
    let inputs = {
        let mut inputs = hosted_inputs::HostedTileInputs::default();
        // The session mounts the apps it hosts itself as their own
        // components, on records it keeps beside its scratch tabs'.
        match in_process::InProcessApps::native(&state::local_user_id()) {
            Ok(apps) => inputs.mount_apps_in_process(apps),
            Err(error) => {
                tracing::warn!(%error, "apps hosted in the session keep no records");
            }
        }
        inputs
    };
    build_tilo_overlay_host_with_inputs(tree_handle, enable_peer_sync, records, inputs)
}

/// Build the shell tree with host-provided app startup data, retained for tile rebuilds.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Keep the shared-tree construction signature aligned with the existing owned-handle public entry points."
)]
pub fn build_tilo_overlay_host_with_inputs(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
    records: ui::record::RecordStore,
    inputs: hosted_inputs::HostedTileInputs,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    let chrome = chrome_state::ChromeState::new_with_handle(tree_handle.clone());
    chrome.borrow_mut().hosted_inputs = inputs;
    chrome.borrow_mut().set_records(records);
    // Points every hosted file explorer at the desktop's one open path, so
    // activating a row resolves the owning app and focuses an already-open
    // document exactly as a palette hit does. Installed once here and
    // picked up by explorers this host creates later without threading a
    // handle through each constructor.
    install_document_opener(&chrome);
    // Snapshot to walk without holding the read lock across widget
    // construction; feeds only the *initial* layout, the live chrome
    // handle drives subsequent updates.
    let tree: WorkspaceTree = tree_handle.read().clone();
    launcher::action_authority::Export::of(&tree).publish();
    launcher::action_authority::follow_tree(&tree_handle);
    for room in tree.rooms() {
        launcher::set_trusted_identity_projection(room, &tree.trusted_identity_projection(room));
    }

    build_overlay_host_from_chrome(&tree, chrome, enable_peer_sync, false)
}

pub(crate) fn build_tilo_overlay_host_with_fixture_inputs(
    tree: state::WorkspaceTreeHandle,
    inputs: hosted_inputs::HostedTileInputs,
) -> Result<(overlay_host::ShellOverlayHost, chrome_state::Chrome), fixture_inputs::ShellFixtureError>
{
    fixture_inputs::validate(&tree.read(), &inputs)?;
    let snapshot = tree.read().clone();
    let chrome = chrome_state::ChromeState::new_with_fixture_handle(tree, inputs);
    Ok(build_overlay_host_from_chrome(
        &snapshot, chrome, false, true,
    ))
}

fn build_overlay_host_from_chrome(
    tree: &WorkspaceTree,
    chrome: chrome_state::Chrome,
    enable_peer_sync: bool,
    fixture: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    let records = chrome.borrow().records().clone();
    let (root, group_row_ptrs) = build_workspace_root(tree, &records, &chrome);
    // Each workspace is a spoke; dragging rotates them, releasing snaps to
    // the nearest one.
    let spokes: Vec<tilo_desktop::sidebar_wheel_disc::SpokeLabel> = tree
        .desktop
        .workspaces
        .iter()
        .map(|w| tilo_desktop::sidebar_wheel_disc::SpokeLabel {
            text: w.name.clone(),
            workspace_id: Some(w.id.0),
        })
        .collect();
    let active_idx = tree
        .desktop
        .workspaces
        .iter()
        .position(|w| w.id == tree.desktop.active)
        .unwrap_or(0);
    let sidebar: Box<dyn Widget> = Box::new(
        tilo_desktop::sidebar_wheel_disc::SidebarWheelDisc::new(spokes)
            .with_active_idx(active_idx)
            .with_host(std::rc::Rc::new(chrome_state::ChromeWheel(chrome.clone()))),
    );

    // The slot's top is pinned below the topbar so the sidebar's
    // click-guard doesn't overlap the overview-toggle icon. With padding
    // off on the outer Container/Column the topbar lands at y=0, so its
    // bottom is exactly `TOPBAR_HEIGHT_PX`.
    let sidebar_top = topbar_chrome::TOPBAR_HEIGHT_PX;
    let host = if fixture {
        overlay_host::ShellOverlayHost::with_fixture_content(root)
    } else {
        overlay_host::ShellOverlayHost::new(root)
    }
    .with_chrome(chrome.clone())
    .with_sidebar(sidebar)
    .with_sidebar_top(sidebar_top)
    .with_group_row_ptrs(group_row_ptrs)
    .with_content_workspace(tree.desktop.active.raw())
    .with_peer_sync_enabled(enable_peer_sync);
    (host, chrome)
}

/// Registers each `Collaborator` with `active_tab: Some(tab_id)` under
/// `PresenceKey::Tab { id: format!("{:x}", tab_id) }`, the key
/// `TabBar::draw` reads to paint presence dots beneath a tab pill.
/// Additive — call once per frame before draw; reset `PresenceCtx::here`
/// first if the roster needs clearing.
pub fn sync_tab_presence_from_tree(tree: &WorkspaceTree, ctx: &mut ui::presence::PresenceCtx) {
    use ui::permissions::UserId;
    use ui::presence::PresenceKey;
    if let Some(ws) = tree.desktop.active_workspace() {
        for c in &ws.collaborators {
            // Colour overrides first: later write wins for the same user
            // id, but ordering doesn't matter since this only walks the
            // active workspace.
            if let Some(hex) = &c.cursor_color {
                ctx.set_color_override(UserId(c.id.clone()), hex);
            }
            if let Some(tab_id) = c.active_tab {
                let key = PresenceKey::Tab {
                    id: format!("{tab_id:x}"),
                };
                ctx.register_presence(&key, UserId(c.id.clone()));
            }
        }
    }
    // Tree-level roster too (top-bar avatar stack); only the colour
    // override applies since tab attribution lives on the workspace.
    for c in &tree.desktop.collaborators {
        if let Some(hex) = &c.cursor_color {
            ctx.set_color_override(UserId(c.id.clone()), hex);
        }
    }
}

#[cfg(test)]
mod hosted_app_tests {
    use super::*;

    #[test]
    fn hosted_app_titles_round_trip_into_live_apps_or_pass_through() {
        /// Drag a tab into its own window and the group rebuilds from widgets,
        /// which knows only the tab's title — if the recovered label isn't
        /// what the hydration match arms name, the tab comes back as a
        /// "coming soon" card. This is the failure this table exists to prevent.
        fn every_hosted_app_round_trips_from_its_tab_title_into_a_live_app() {
            for (title, label) in HOSTED_OTHER_APPS {
                let recovered = hosted_label_for_title(title);
                assert_eq!(
                    &recovered, label,
                    "a tab titled {title:?} must recover the label {label:?}",
                );

                let tile = ShellTile::AppPlaceholder {
                    kind: ShellKind::Other(recovered.clone()),
                    label: (*title).to_string(),
                };
                assert_eq!(
                    resolve_tile_shape_for_test(&tile),
                    "App",
                    "{title:?} hydrated into a placeholder card instead of the \
                     app it names",
                );
                assert_eq!(
                    to_ui_kind(&ShellKind::Other(recovered)),
                    UiKind::Editor,
                    "{title:?} must wear an editor glyph, not the unknown-kind \
                     settings cog",
                );
            }
        }

        /// A title the shell does not host carries through untouched, so a
        /// foreign or future app is not silently renamed into one of ours.
        fn an_unhosted_title_is_carried_through_unchanged() {
            assert_eq!(hosted_label_for_title("Inkscape"), "Inkscape");
            assert_eq!(hosted_label_for_title("report.odt"), "report.odt");
        }

        every_hosted_app_round_trips_from_its_tab_title_into_a_live_app();
        an_unhosted_title_is_carried_through_unchanged();
    }
}

#[cfg(test)]
mod legacy_files_opener_regressions {
    use super::*;

    struct ClearDocumentOpener;

    impl Drop for ClearDocumentOpener {
        fn drop(&mut self) {
            file_explorer::files_app::clear_document_opener();
        }
    }

    fn legacy_chrome(
        root: &std::path::Path,
        records: ui::record::RecordStore,
    ) -> chrome_state::Chrome {
        let tree =
            crate::shell::state::WorkspaceTreeHandle::new(state::WorkspaceTree::bootstrap_demo());
        let chrome = chrome_state::ChromeState::new_with_handle(tree);
        chrome.borrow_mut().hosted_inputs =
            hosted_inputs::HostedTileInputs::for_host_directory_privately(root)
                .expect("owned Files root");
        chrome.borrow_mut().set_records(records);
        install_document_opener(&chrome);
        chrome
    }

    #[test]
    #[serial_test::serial]
    fn deferred_native_file_admission_preserves_existing_file_record_heads() {
        use automerge::transaction::Transactable;

        let directory = tempfile::tempdir().expect("temporary Files root");
        let document = directory.path().join("existing.txt");
        std::fs::write(&document, "existing contents\n").expect("fixture document");
        let document = document.canonicalize().expect("canonical fixture document");
        let mut owners = ui::record::RecordOwners::new(directory.path().join("profile"));
        let records = owners
            .open(SHELL_RECORDS_PACKAGE, "deferred-file-admission")
            .expect("canonical profile record store");
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file(&document),
            ui::record::RecordKind::new("text-document"),
        );
        records
            .create_record(&checkout)
            .expect("existing document record");
        let before = records
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, "body", "preserved existing body")
                    .expect("seed existing document");
                doc.get_heads()
            })
            .expect("existing document writable");
        let chrome = legacy_chrome(directory.path(), records.clone());
        let _clear = ClearDocumentOpener;

        open_document_from_files(&chrome, &document).expect("deferred native admission");

        assert_eq!(
            records
                .with(&checkout.id, automerge::AutoCommit::get_heads)
                .expect("existing document remains available"),
            before,
            "deferred admission must not replace or mutate an already-open canonical file record"
        );
    }
}

/// Install immutable trusted project check configuration before session admission.
///
/// # Errors
/// Refuses malformed, ambiguous, non-private or unsupported host definitions.
#[cfg(target_os = "linux")]
pub fn install_approved_checks_from_args(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    broker_services::approved_checks::install_from_args(arguments)
}

pub(crate) mod morning_sources;
