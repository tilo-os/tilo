//! Whether the room an app's window is in has synced with its host, as the
//! shell tells the app ([`crate::floating_surface::FloatingSurfaceHostMessage::Appearance`]).
//!
//! An empty room and a room this device has not reached yet look the same
//! to an app: no content. Saying "Nothing has been said here yet" about a
//! room that never synced is a lie the person acts on. So until the room's
//! first sync completes, every empty state the framework paints
//! ([`crate::widget::EmptyState`]) says what is really going on instead:
//! "Connecting to {workspace}…" with a spinner, or that the workspace's host
//! (or this device) has to update before anything syncs. An app's own empty
//! state shows only once the room has synced, or in a window whose
//! workspace syncs with no host at all (the shell says nothing then).

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};

use crate::time::Instant;

/// Where this device stands with the host of the room a window is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomSyncState {
    /// This device has not reached the room's host since it started, and is
    /// not dialling it now.
    NeverSynced,
    /// Reaching the room's host; no sync with it has completed yet.
    Connecting,
    /// The host speaks an older peer protocol than this device: nothing
    /// syncs until the host updates.
    HostNeedsUpdate,
    /// The host speaks a newer peer protocol: this device updates.
    DeviceNeedsUpdate,
    /// A sync with the room's host completed: what the room held then is
    /// here.
    Synced,
}

/// The room a window is in, and where it stands with its host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSync {
    /// Where the room stands.
    pub state: RoomSyncState,
    /// The workspace's name, as the person knows it.
    #[serde(default)]
    pub workspace: String,
    /// Whether this app may write the room now: the room's grants let the
    /// person change it and make the app one of its authors, the same
    /// decision that opened it writable or read-only. It follows the room's
    /// grants as they change, so an app opened read-only becomes writable
    /// the moment its authorship arrives, and the other way round. Absent
    /// when the shell has no answer; the launch's stands then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
}

impl RoomSync {
    /// Whether what the room holds may not be here yet: no sync completed.
    #[must_use]
    pub const fn before_first_sync(&self) -> bool {
        !matches!(self.state, RoomSyncState::Synced)
    }
}

/// What the shell last said, and since when the room has been waiting.
struct Told {
    sync: RoomSync,
    /// When the room began waiting for its first sync: the spinner's phase.
    since: Instant,
}

thread_local! {
    static TOLD: RefCell<Option<Told>> = const { RefCell::new(None) };
    /// The permissions of the components a workspace launch hosts, which
    /// follow [`RoomSync::writable`] (`follow_participation`).
    static FOLLOWERS: RefCell<Vec<std::rc::Weak<RefCell<crate::permissions::PermissionContext>>>> =
        const { RefCell::new(Vec::new()) };
    /// The spinner step an empty state painted last, and where (scene
    /// pixels) a new pose was painted that no presented frame has damaged
    /// yet.
    static SPINNER: Cell<(Option<u64>, Option<vello::kurbo::Rect>)> =
        const { Cell::new((None, None)) };
}

/// What the shell says now about the room this app's window is in; `None`:
/// the window's workspace syncs with no host (or the shell said nothing).
/// Answers whether that changed what the app shows.
pub fn set(sync: Option<RoomSync>) -> bool {
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
        // What the window shows changes with it: it is laid out again.
        SPINNER.with(|spinner| spinner.set((None, None)));
        true
    })
}

/// Let `permissions`, a workspace launch's, follow the room's grants: when
/// the shell says the app may write the room now, or may not any more, they
/// become an author's or a viewer's at once ([`RoomSync::writable`]).
pub fn follow_participation(
    permissions: &std::rc::Rc<RefCell<crate::permissions::PermissionContext>>,
) {
    FOLLOWERS.with(|followers| {
        let mut followers = followers.borrow_mut();
        followers.retain(|follower| follower.strong_count() > 0);
        followers.push(std::rc::Rc::downgrade(permissions));
    });
    if let Some(writable) = current().and_then(|sync| sync.writable) {
        rebase(permissions, writable);
    }
}

/// Make every following launch an author's (`writable`) or a viewer's.
fn follow(writable: bool) {
    let live: Vec<_> = FOLLOWERS.with(|followers| {
        let mut followers = followers.borrow_mut();
        followers.retain(|follower| follower.strong_count() > 0);
        followers
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect()
    });
    for permissions in live {
        rebase(&permissions, writable);
    }
}

fn rebase(permissions: &RefCell<crate::permissions::PermissionContext>, writable: bool) {
    let next = permissions.borrow().with_participation(writable);
    if let Some(next) = next {
        *permissions.borrow_mut() = next;
    }
}

/// What the shell last said about this window's room.
#[must_use]
pub fn current() -> Option<RoomSync> {
    TOLD.with(|told| told.borrow().as_ref().map(|told| told.sync.clone()))
}

/// Whether the window's room has not synced yet: its content may be
/// missing, not absent.
#[must_use]
pub fn before_first_sync() -> bool {
    current().is_some_and(|sync| sync.before_first_sync())
}

/// What an empty state shows in place of the app's own while the room has
/// not synced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub title: String,
    pub message: Option<String>,
    /// Since when the spinner turns, when the room is on its way.
    pub spinner_since: Option<Instant>,
}

/// What an empty state shows instead of the app's own, while the room has
/// not synced; `None` once it has (or outside a room that syncs).
pub(crate) fn waiting() -> Option<Waiting> {
    TOLD.with(|told| {
        let told = told.borrow();
        let told = told.as_ref().filter(|told| told.sync.before_first_sync())?;
        let workspace = told.sync.workspace.trim();
        Some(match told.sync.state {
            RoomSyncState::NeverSynced | RoomSyncState::Connecting => Waiting {
                title: if workspace.is_empty() {
                    crate::i18n::I18nManager::lookup_plain("room-sync-connecting-unnamed")
                } else {
                    let mut args = fluent::FluentArgs::new();
                    args.set("workspace", workspace.to_owned());
                    crate::i18n::I18nManager::lookup("room-sync-connecting", Some(&args))
                },
                message: None,
                spinner_since: Some(told.since),
            },
            RoomSyncState::HostNeedsUpdate => Waiting {
                title: crate::i18n::I18nManager::lookup_plain("room-sync-host-needs-update"),
                message: Some(crate::i18n::I18nManager::lookup_plain(
                    "room-sync-host-needs-update-body",
                )),
                spinner_since: None,
            },
            RoomSyncState::DeviceNeedsUpdate => Waiting {
                title: crate::i18n::I18nManager::lookup_plain("room-sync-device-needs-update"),
                message: Some(crate::i18n::I18nManager::lookup_plain(
                    "room-sync-device-needs-update-body",
                )),
                spinner_since: None,
            },
            RoomSyncState::Synced => return None,
        })
    })
}

/// An empty state painted the waiting spinner at `step` (its pose since the
/// room began waiting) over `scene_rect`, its slot in scene (physical)
/// pixels: the frame pump keeps frames coming while it turns, and a new pose
/// damages that slot alone.
pub(crate) fn spinner_painted(step: u64, turning: bool, scene_rect: vello::kurbo::Rect) {
    SPINNER.with(|spinner| {
        let (last, pending) = spinner.get();
        let pending = if last == Some(step) {
            pending
        } else {
            Some(pending.map_or(scene_rect, |earlier| earlier.union(scene_rect)))
        };
        spinner.set((Some(step), pending));
    });
    if turning {
        crate::motion::want_frames_until(
            crate::time::now() + crate::animation::indicator::INDICATOR_STEP,
        );
    }
}

/// Where the waiting spinner painted a new pose since the last frame, in
/// scene (physical) pixels: it paints outside any widget's box, so the
/// frame damages that slot. `None` when its pose did not move. Only the
/// native runner presents frames with damage.
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

    fn told(state: RoomSyncState) -> Option<RoomSync> {
        Some(RoomSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
        })
    }

    // USER REQUIREMENT (2026-10-19): "If a v6 client meets the v5 relay, the user must see a clear "update needed" state, never a silent empty room."
    #[test]
    fn a_room_that_never_synced_says_so_instead_of_looking_empty() {
        set(None);
        assert!(waiting().is_none(), "a room without a host is the app's");
        assert!(set(told(RoomSyncState::Connecting)));
        let connecting = waiting().unwrap();
        assert!(connecting.title.contains("Lounge"), "{}", connecting.title);
        assert!(connecting.spinner_since.is_some());
        assert!(!set(told(RoomSyncState::Connecting)), "said once");

        assert!(set(told(RoomSyncState::HostNeedsUpdate)));
        let behind = waiting().unwrap();
        assert!(behind.spinner_since.is_none());
        assert_eq!(
            behind.title,
            crate::i18n::I18nManager::lookup_plain("room-sync-host-needs-update")
        );

        assert!(set(told(RoomSyncState::Synced)));
        assert!(waiting().is_none(), "only a synced room is empty");
        assert!(!before_first_sync());
        set(None);
    }

    #[test]
    fn the_spinner_damages_only_its_slot_and_only_when_its_pose_moves() {
        let slot = vello::kurbo::Rect::new(100.0, 80.0, 148.0, 128.0);
        set(told(RoomSyncState::Connecting));
        let _ = take_spinner_damage();
        spinner_painted(3, false, slot);
        assert_eq!(
            take_spinner_damage(),
            Some(slot),
            "its slot, not the window"
        );
        spinner_painted(3, false, slot);
        assert_eq!(take_spinner_damage(), None, "the same pose damages nothing");
        spinner_painted(4, false, slot);
        assert_eq!(take_spinner_damage(), Some(slot));
        set(None);
    }

    // USER REQUIREMENT (2026-10-19): "An app's write permission must follow grants live. When the room's grants change (authorship granted or withdrawn, role changed), open apps switch between read-only and writable without a restart"
    #[test]
    fn a_launch_opened_read_only_becomes_writable_when_its_authorship_arrives_and_back() {
        use crate::permissions::{ActionKind, PermissionContext, PermissionScope, Role};
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let writes = |permissions: &std::rc::Rc<RefCell<PermissionContext>>| {
            permissions
                .borrow()
                .check(&PermissionScope::workspace(ActionKind::Undoable))
        };
        set(None);
        let launch = std::rc::Rc::new(RefCell::new(PermissionContext::member_of(
            room,
            "person-a",
            Role::Viewer,
        )));
        let own = std::rc::Rc::new(RefCell::new(PermissionContext::member_of(
            room,
            "person-a",
            Role::Owner,
        )));
        follow_participation(&launch);
        follow_participation(&own);
        assert!(!writes(&launch), "opened read-only");
        let told = |writable| {
            Some(RoomSync {
                state: RoomSyncState::Synced,
                workspace: "Lounge".into(),
                writable,
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
        // No answer from the shell leaves the launch as it is.
        set(told(Some(true)));
        set(told(None));
        assert!(writes(&launch));
        set(None);
    }

    #[test]
    fn the_wire_names_each_state() {
        let sync = RoomSync {
            state: RoomSyncState::HostNeedsUpdate,
            workspace: "Lounge".into(),
            writable: None,
        };
        let json = serde_json::to_value(&sync).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "host_needs_update", "workspace": "Lounge"})
        );
        assert_eq!(serde_json::from_value::<RoomSync>(json).unwrap(), sync);
    }
}
