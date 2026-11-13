//! Whether the workspace an app's window is in has synced with its host, as the shell
//! tells the app ([`crate::floating_surface::FloatingSurfaceHostMessage::Appearance`]).
//! An empty workspace and a workspace this device has not reached yet look the same to an
//! app: no content. Saying "Nothing has been said here yet" about a workspace that never
//! synced is a lie the person acts on.

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};

use crate::arriving::{Arrival, Arriving, Source};
use crate::time::Instant;

/// Where this device stands with the host of the workspace a window is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSyncState {
    Connecting,
    Unreachable,
    HostNeedsUpdate,
    DeviceNeedsUpdate,
    Synced,
}

/// The workspace a window is in, and where it stands with its host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSync {
    pub state: WorkspaceSyncState,
    #[serde(default)]
    pub workspace: String,
    /// Whether this app may write the workspace now: the workspace's grants let the
    /// person change it and make the app one of its authors, the same decision that
    /// opened it writable or read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    /// This device's writes to the workspace that are not yet synced: held on this device
    /// because no host of the workspace carries them now, or because the host did not
    /// keep them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unsynced_writes: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cannot_keep_writes: bool,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
const fn is_zero(count: &u32) -> bool {
    *count == 0
}

impl WorkspaceSync {
    /// Whether what the workspace holds may not be here yet: no sync completed.
    #[must_use]
    pub const fn before_first_sync(&self) -> bool {
        !matches!(self.state, WorkspaceSyncState::Synced)
    }
}

struct Told {
    sync: WorkspaceSync,
    since: Instant,
}

/// Why what the workspace holds cannot arrive: nothing syncs until one side updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceUnreachable {
    HostNeedsUpdate,
    DeviceNeedsUpdate,
    ShellGone,
}

impl std::fmt::Display for WorkspaceUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::I18nManager::lookup_plain(match self {
            Self::HostNeedsUpdate => "workspace-sync-host-needs-update",
            Self::DeviceNeedsUpdate => "workspace-sync-device-needs-update",
            Self::ShellGone => "workspace-sync-shell-gone",
        }))
    }
}

thread_local! {
    static TOLD: RefCell<Option<Told>> = const { RefCell::new(None) };
    static SHELL: Cell<bool> = const { Cell::new(false) };
    static HEARD: Cell<bool> = const { Cell::new(false) };
    static LOST: Cell<bool> = const { Cell::new(false) };
    static HOLDINGS: RefCell<Option<Arriving<(), WorkspaceUnreachable>>> =
        const { RefCell::new(None) };
    static FOLLOWERS: RefCell<Vec<crate::permissions::WeakPermissions>> =
        const { RefCell::new(Vec::new()) };
    /// The spinner step an empty state painted last, and where (scene pixels) a new pose
    /// was painted that no presented frame has damaged yet.
    static SPINNER: Cell<(Option<u64>, Option<vello::kurbo::Rect>)> =
        const { Cell::new((None, None)) };
    /// When the turning spinner the last drawn frame painted takes its next pose: the
    /// window redraws then, and only then.
    static NEXT_POSE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// What the shell says now about the workspace this app's window is in; `None`: the
/// window's workspace syncs with no host (or the shell said nothing).
pub fn set(sync: Option<WorkspaceSync>) -> bool {
    LOST.with(|lost| lost.set(false));
    let first = !HEARD.with(|heard| heard.replace(true));
    let changed = tell(sync);
    let settled = (first || changed) && settle_holdings();
    changed || settled
}

/// A shell hosts this window: what its workspace holds waits for the shell to say where
/// the workspace stands.
pub fn hosted_by_shell() {
    if !SHELL.with(|shell| shell.replace(true)) {
        let _ = settle_holdings();
    }
}

/// The shell hosting this window went away. A shell says where the workspace stands in
/// its first message to a window (every shell does: the app and the shell speak one
/// shell-ui version, or do not connect), so a window still waiting for that word never
/// gets it: what the workspace holds cannot arrive, and the window says so
/// ([`WorkspaceUnreachable::ShellGone`]).
pub fn shell_lost() -> bool {
    if !SHELL.with(Cell::get) || HEARD.with(Cell::get) {
        return false;
    }
    LOST.with(|lost| lost.set(true));
    settle_holdings()
}

/// What the workspace this window is in holds, as a read: pending (from this device,
/// briefly) until the shell hosting the window said where the workspace stands, then from
/// the workspace's host until the workspace's first sync completed; failed while one side
/// has to update first; ready once it synced, in a workspace with no host, or while its
/// host cannot be reached (the person works on what this device has).
#[must_use]
pub fn holdings() -> Arriving<(), WorkspaceUnreachable> {
    HOLDINGS.with(|holdings| {
        holdings
            .borrow_mut()
            .get_or_insert_with(|| {
                let arriving = Arriving::pending(Source::Local);
                arriving.stand(standing());
                arriving
            })
            .clone()
    })
}

fn standing() -> Arrival<(), WorkspaceUnreachable> {
    if SHELL.with(Cell::get) && !HEARD.with(Cell::get) {
        if LOST.with(Cell::get) {
            return Arrival::Failed(WorkspaceUnreachable::ShellGone);
        }
        return Arrival::Pending {
            from: Source::Local,
        };
    }
    match current() {
        None => Arrival::Ready(()),
        Some(sync) => match sync.state {
            WorkspaceSyncState::Connecting => Arrival::Pending {
                from: Source::host(sync.workspace.trim()),
            },
            WorkspaceSyncState::Unreachable | WorkspaceSyncState::Synced => Arrival::Ready(()),
            WorkspaceSyncState::HostNeedsUpdate => {
                Arrival::Failed(WorkspaceUnreachable::HostNeedsUpdate)
            }
            WorkspaceSyncState::DeviceNeedsUpdate => {
                Arrival::Failed(WorkspaceUnreachable::DeviceNeedsUpdate)
            }
        },
    }
}

fn settle_holdings() -> bool {
    let Some(holdings) = HOLDINGS.with(|holdings| holdings.borrow().clone()) else {
        return false;
    };
    let next = standing();
    let moved = holdings.with(|now| *now != next);
    if moved {
        holdings.stand(next);
    }
    moved
}

/// Where what this window's workspace holds and this device lacks comes from: the
/// workspace's host, by the workspace's name, when the workspace has one; otherwise the
/// workspace's other devices, which no one names here.
#[must_use]
pub fn source() -> Source {
    match current() {
        Some(sync) => Source::host(sync.workspace.trim()),
        None => Source::peer(""),
    }
}

fn tell(sync: Option<WorkspaceSync>) -> bool {
    TOLD.with(|told| {
        let mut told = told.borrow_mut();
        if told.as_ref().map(|told| &told.sync) == sync.as_ref() {
            return false;
        }
        let since = told
            .as_ref()
            .filter(|told| told.sync.before_first_sync())
            .map_or_else(crate::time::now, |told| told.since);
        let writable = sync.as_ref().and_then(|sync| sync.writable);
        *told = sync.map(|sync| Told { sync, since });
        drop(told);
        if let Some(writable) = writable {
            follow(writable);
        }
        SPINNER.with(|spinner| spinner.set((None, None)));
        true
    })
}

/// Let `permissions`, a workspace launch's, follow the workspace's grants: when the shell
/// says the app may write the workspace now, or may not any more, they become an author's
/// or a viewer's at once ([`WorkspaceSync::writable`]).
pub fn follow_participation(permissions: &crate::permissions::SharedPermissions) {
    FOLLOWERS.with(|followers| {
        let mut followers = followers.borrow_mut();
        followers.retain(crate::permissions::WeakPermissions::is_shared);
        followers.push(permissions.downgrade());
    });
    if let Some(writable) = current().and_then(|sync| sync.writable) {
        rebase(permissions, writable);
    }
}

fn follow(writable: bool) {
    let live: Vec<_> = FOLLOWERS.with(|followers| {
        let mut followers = followers.borrow_mut();
        followers.retain(crate::permissions::WeakPermissions::is_shared);
        followers
            .iter()
            .filter_map(crate::permissions::WeakPermissions::upgrade)
            .collect()
    });
    for permissions in live {
        rebase(&permissions, writable);
    }
}

fn rebase(permissions: &crate::permissions::SharedPermissions, writable: bool) {
    let next = permissions.borrow().with_participation(writable);
    if let Some(next) = next {
        permissions.set(next);
    }
}

/// What the shell last said about this window's workspace.
#[must_use]
pub fn current() -> Option<WorkspaceSync> {
    TOLD.with(|told| told.borrow().as_ref().map(|told| told.sync.clone()))
}

/// What the window says while this device holds writes to its workspace that are not yet
/// synced ([`WorkspaceSync::unsynced_writes`]), or cannot keep them on disk
/// ([`WorkspaceSync::cannot_keep_writes`]); `None` while neither.
#[must_use]
pub fn unsynced_status() -> Option<String> {
    let sync = current().filter(|sync| sync.unsynced_writes > 0 || sync.cannot_keep_writes)?;
    let workspace = sync.workspace.trim();
    let key = match (sync.cannot_keep_writes, workspace.is_empty()) {
        (true, true) => {
            return Some(crate::i18n::I18nManager::lookup_plain(
                "workspace-sync-cannot-keep-unnamed",
            ));
        }
        (true, false) => "workspace-sync-cannot-keep",
        (false, true) => {
            return Some(crate::i18n::I18nManager::lookup_plain(
                "workspace-sync-not-yet-synced-unnamed",
            ));
        }
        (false, false) => "workspace-sync-not-yet-synced",
    };
    let mut args = fluent::FluentArgs::new();
    args.set("workspace", workspace.to_owned());
    Some(crate::i18n::I18nManager::lookup(key, Some(&args)))
}

/// Whether the window's workspace has not synced yet: its content may be missing, not
/// absent.
#[must_use]
pub fn before_first_sync() -> bool {
    current().is_some_and(|sync| sync.before_first_sync())
}

/// What an empty state shows in place of the app's own while the workspace has not
/// synced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub title: String,
    pub message: Option<String>,
    pub spinner_since: Option<Instant>,
    pub icon: crate::icons::Icon,
    pub keeps_action: bool,
}

impl Waiting {
    pub(crate) const fn keeps_action(&self) -> bool {
        self.keeps_action
    }
}

/// What an empty state shows instead of the app's own, while the workspace has not
/// synced; `None` once it has (or outside a workspace that syncs).
pub(crate) fn waiting() -> Option<Waiting> {
    TOLD.with(|told| {
        let told = told.borrow();
        let told = told.as_ref().filter(|told| told.sync.before_first_sync())?;
        let workspace = told.sync.workspace.trim();
        Some(match told.sync.state {
            WorkspaceSyncState::Connecting => Waiting {
                title: if workspace.is_empty() {
                    crate::i18n::I18nManager::lookup_plain("workspace-sync-connecting-unnamed")
                } else {
                    let mut args = fluent::FluentArgs::new();
                    args.set("workspace", workspace.to_owned());
                    crate::i18n::I18nManager::lookup("workspace-sync-connecting", Some(&args))
                },
                message: None,
                spinner_since: Some(told.since),
                icon: crate::icons::Icon::Info,
                keeps_action: false,
            },
            WorkspaceSyncState::Unreachable => Waiting {
                title: if workspace.is_empty() {
                    crate::i18n::I18nManager::lookup_plain("workspace-sync-unreachable-unnamed")
                } else {
                    let mut args = fluent::FluentArgs::new();
                    args.set("workspace", workspace.to_owned());
                    crate::i18n::I18nManager::lookup("workspace-sync-unreachable", Some(&args))
                },
                message: Some(crate::i18n::I18nManager::lookup_plain(
                    "workspace-sync-unreachable-body",
                )),
                spinner_since: None,
                icon: crate::icons::Icon::WifiOff,
                keeps_action: true,
            },
            WorkspaceSyncState::HostNeedsUpdate => Waiting {
                title: crate::i18n::I18nManager::lookup_plain("workspace-sync-host-needs-update"),
                message: Some(crate::i18n::I18nManager::lookup_plain(
                    "workspace-sync-host-needs-update-body",
                )),
                spinner_since: None,
                icon: crate::icons::Icon::Download,
                keeps_action: false,
            },
            WorkspaceSyncState::DeviceNeedsUpdate => Waiting {
                title: crate::i18n::I18nManager::lookup_plain("workspace-sync-device-needs-update"),
                message: Some(crate::i18n::I18nManager::lookup_plain(
                    "workspace-sync-device-needs-update-body",
                )),
                spinner_since: None,
                icon: crate::icons::Icon::Download,
                keeps_action: false,
            },
            WorkspaceSyncState::Synced => return None,
        })
    })
}

/// An empty state painted the waiting spinner at `step` (its pose since the workspace
/// began waiting) over `scene_rect`, its slot in scene (physical) pixels, and it takes
/// its next pose at `next` while it turns: the window redraws then ([`next_pose`]), and
/// the new pose damages that slot alone. A turning spinner costs one small frame per
/// pose, never a frame per refresh, a sync or a layout.
pub(crate) fn spinner_painted(step: u64, next: Option<Instant>, scene_rect: vello::kurbo::Rect) {
    SPINNER.with(|spinner| {
        let (last, pending) = spinner.get();
        let pending = if last == Some(step) {
            pending
        } else {
            Some(pending.map_or(scene_rect, |earlier| earlier.union(scene_rect)))
        };
        spinner.set((Some(step), pending));
    });
    if let Some(next) = next {
        NEXT_POSE.with(|pose| pose.set(Some(pose.get().map_or(next, |earlier| earlier.min(next)))));
    }
}

/// A frame starts drawing: what it paints says again whether a spinner turns
/// ([`spinner_painted`]).
#[cfg(any(test, feature = "native"))]
pub(crate) fn frame_drawing() {
    NEXT_POSE.with(|pose| pose.set(None));
}

/// When the turning spinner the last drawn frame painted takes its next pose; `None` when
/// no spinner turns.
#[cfg(any(test, feature = "native"))]
pub(crate) fn next_pose() -> Option<Instant> {
    NEXT_POSE.with(Cell::get)
}

/// Where the waiting spinner painted a new pose since the last frame, in scene (physical)
/// pixels: it paints outside any widget's box, so the frame damages that slot.
#[cfg(any(test, feature = "native"))]
pub(crate) fn take_spinner_damage() -> Option<vello::kurbo::Rect> {
    SPINNER.with(|spinner| {
        let (last, pending) = spinner.get();
        spinner.set((last, None));
        pending
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn told(state: WorkspaceSyncState) -> Option<WorkspaceSync> {
        Some(WorkspaceSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        })
    }

    #[test]
    fn a_workspace_that_never_synced_says_so_instead_of_looking_empty() {
        set(None);
        assert!(
            waiting().is_none(),
            "a workspace without a host is the app's"
        );
        assert!(set(told(WorkspaceSyncState::Connecting)));
        let connecting = waiting().unwrap();
        assert!(connecting.title.contains("Lounge"), "{}", connecting.title);
        assert!(connecting.spinner_since.is_some());
        assert!(!set(told(WorkspaceSyncState::Connecting)), "said once");

        assert!(
            !connecting.keeps_action(),
            "what the workspace holds is on its way"
        );

        assert!(set(told(WorkspaceSyncState::Unreachable)));
        let unreachable = waiting().unwrap();
        assert!(
            unreachable.title.contains("Lounge"),
            "{}",
            unreachable.title
        );
        assert!(unreachable.spinner_since.is_none(), "nothing is on its way");
        assert!(unreachable.message.is_some(), "it says changes stay here");
        assert!(
            unreachable.keeps_action(),
            "the person works on what is here"
        );

        assert!(set(told(WorkspaceSyncState::HostNeedsUpdate)));
        let behind = waiting().unwrap();
        assert!(behind.spinner_since.is_none());
        assert_eq!(
            behind.title,
            crate::i18n::I18nManager::lookup_plain("workspace-sync-host-needs-update")
        );

        assert!(set(told(WorkspaceSyncState::Synced)));
        assert!(waiting().is_none(), "only a synced workspace is empty");
        assert!(!before_first_sync());
        set(None);
    }

    #[test]
    fn what_the_workspace_holds_arrives_from_its_host_until_its_first_sync() {
        let held = holdings();
        hosted_by_shell();
        assert_eq!(
            held.get(),
            Arrival::local(),
            "this device asks the shell where the workspace stands"
        );
        set(told(WorkspaceSyncState::Connecting));
        assert_eq!(
            held.get(),
            Arrival::Pending {
                from: Source::host("Lounge")
            },
            "then waits on the workspace's host"
        );
        assert_eq!(
            holdings().get(),
            held.get(),
            "every view shares the one read"
        );
        set(told(WorkspaceSyncState::Unreachable));
        assert_eq!(
            held.get(),
            Arrival::Ready(()),
            "with its host out of reach, the person works on what is here"
        );
        set(told(WorkspaceSyncState::DeviceNeedsUpdate));
        assert_eq!(
            held.get(),
            Arrival::Failed(WorkspaceUnreachable::DeviceNeedsUpdate)
        );
        set(told(WorkspaceSyncState::Synced));
        assert_eq!(held.get(), Arrival::Ready(()));
        set(None);
        assert_eq!(
            held.get(),
            Arrival::Ready(()),
            "what is here is all there is"
        );
    }

    #[test]
    fn a_window_whose_shell_goes_before_its_first_word_says_its_workspace_cannot_arrive() {
        let held = holdings();
        hosted_by_shell();
        assert!(held.get().is_pending());
        assert!(shell_lost(), "the window shows the change");
        assert_eq!(
            held.get(),
            Arrival::Failed(WorkspaceUnreachable::ShellGone),
            "a word that never comes ends the wait"
        );
        set(told(WorkspaceSyncState::Synced));
        assert_eq!(held.get(), Arrival::Ready(()), "a shell's word still lands");
        assert!(!shell_lost(), "a shell gone after it spoke changes nothing");
        assert_eq!(held.get(), Arrival::Ready(()));
    }

    #[test]
    fn the_shells_first_word_shows_what_a_window_waited_on() {
        let held = holdings();
        hosted_by_shell();
        assert!(held.get().is_pending());
        assert!(set(None), "no workspace: what is here shows now");
        assert_eq!(held.get(), Arrival::Ready(()));
    }

    #[test]
    fn a_window_no_shell_hosts_has_what_this_device_has() {
        assert_eq!(holdings().get(), Arrival::Ready(()));
        assert_eq!(source(), Source::peer(""), "no host to name");
    }

    #[test]
    fn the_spinner_damages_only_its_slot_and_only_when_its_pose_moves() {
        let slot = vello::kurbo::Rect::new(100.0, 80.0, 148.0, 128.0);
        set(told(WorkspaceSyncState::Connecting));
        let _ = take_spinner_damage();
        spinner_painted(3, None, slot);
        assert_eq!(
            take_spinner_damage(),
            Some(slot),
            "its slot, not the window"
        );
        spinner_painted(3, None, slot);
        assert_eq!(take_spinner_damage(), None, "the same pose damages nothing");
        spinner_painted(4, None, slot);
        assert_eq!(take_spinner_damage(), Some(slot));
        set(None);
    }

    #[test]
    fn a_turning_spinner_asks_for_its_next_pose_and_nothing_once_it_is_gone() {
        let slot = vello::kurbo::Rect::new(100.0, 80.0, 148.0, 128.0);
        let since = crate::time::now();
        frame_drawing();
        assert_eq!(next_pose(), None, "nothing turns");
        let next = crate::animation::next_indicator_step(since, since);
        spinner_painted(0, Some(next), slot);
        assert_eq!(
            next_pose(),
            Some(next),
            "the window redraws at the next pose"
        );
        frame_drawing();
        assert_eq!(
            next_pose(),
            None,
            "a frame that paints no spinner asks for no more frames"
        );
        let _ = take_spinner_damage();
    }

    #[test]
    fn a_launch_opened_read_only_becomes_writable_when_its_authorship_arrives_and_back() {
        use crate::permissions::{
            ActionKind, PermissionContext, PermissionScope, Role, SharedPermissions,
        };
        let workspace = "3f2b1c00-0000-4000-8000-000000000001";
        let writes = |permissions: &SharedPermissions| {
            permissions
                .borrow()
                .check(&PermissionScope::workspace(ActionKind::Undoable))
        };
        set(None);
        let launch = crate::permissions::SharedPermissions::new(PermissionContext::member_of(
            workspace,
            "person-a",
            Role::Viewer,
        ));
        let own = crate::permissions::SharedPermissions::new(PermissionContext::member_of(
            workspace,
            "person-a",
            Role::Owner,
        ));
        follow_participation(&launch);
        follow_participation(&own);
        assert!(!writes(&launch), "opened read-only");
        let told = |writable| {
            Some(WorkspaceSync {
                state: WorkspaceSyncState::Synced,
                workspace: "Lounge".into(),
                writable,
                unsynced_writes: 0,
                cannot_keep_writes: false,
            })
        };
        set(told(Some(true)));
        assert!(writes(&launch), "writable the moment authorship arrives");
        set(told(Some(false)));
        assert!(!writes(&launch), "read-only again when it is withdrawn");
        assert!(
            writes(&own),
            "an owner's own launch is no participation to follow"
        );
        set(told(Some(true)));
        set(told(None));
        assert!(writes(&launch));
        set(None);
    }

    #[test]
    fn writes_held_for_the_workspaces_hosts_say_they_are_not_yet_synced_until_kept() {
        let told = |unsynced_writes| {
            Some(WorkspaceSync {
                state: WorkspaceSyncState::Synced,
                workspace: "Lounge".into(),
                writable: None,
                unsynced_writes,
                cannot_keep_writes: false,
            })
        };
        set(told(0));
        assert_eq!(unsynced_status(), None, "nothing held, nothing to say");
        assert!(set(told(2)), "held writes change what the window says");
        let status = unsynced_status().expect("held writes are not yet synced");
        assert!(status.contains("Lounge"), "{status}");
        assert!(
            !before_first_sync(),
            "a synced workspace with held writes still shows what it holds"
        );
        assert!(set(told(0)), "a host kept them");
        assert_eq!(unsynced_status(), None);
        set(told(1));
        let waiting = unsynced_status();
        assert!(set(Some(WorkspaceSync {
            cannot_keep_writes: true,
            ..told(1).unwrap()
        })));
        let full = unsynced_status().expect("a device that cannot keep its writes says so");
        assert_ne!(
            Some(full),
            waiting,
            "it says it cannot sync, not only that the writes wait"
        );
        set(None);
    }

    #[test]
    fn the_wire_names_each_state() {
        let sync = WorkspaceSync {
            state: WorkspaceSyncState::HostNeedsUpdate,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        };
        let json = serde_json::to_value(&sync).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "host_needs_update", "workspace": "Lounge"})
        );
        assert_eq!(serde_json::from_value::<WorkspaceSync>(json).unwrap(), sync);
    }
}
