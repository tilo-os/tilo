//! Status-notifier ("tray") icons of running applications.
//!
//! Discord, Steam, Nextcloud and `KeePassXC` put an icon in the panel and
//! keep running there when their window is closed; without a host for
//! that icon they are unreachable. The session owns
//! `org.kde.StatusNotifierWatcher` on the session bus ([`watcher`]),
//! reads every registered `org.kde.StatusNotifierItem` and its
//! `com.canonical.dbusmenu` menu ([`item`]), and keeps what it learnt in
//! one [`Tray`].
//!
//! The host draws each item's icon and forwards clicks as
//! `Activate`/`ContextMenu`; the palette, the control socket, agents and
//! screen readers reach the same items — and every entry of their menus —
//! as actions the host builds from [`Tray::snapshot`]. An icon that only
//! answers a pointer would be invisible to all of them.
//!
//! The one thing the host supplies is an [`IconRasteriser`] for icons an
//! item names rather than sends, so this crate needs no icon theme or
//! image decoder.
//!
//! Bus conversations run on the session runtime; nothing here blocks the
//! compositor loop, which only reads [`Tray::snapshot`].

pub mod icon;
pub mod item;
pub mod watcher;

#[cfg(test)]
mod copy_lint_tests;

use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover as _;

pub use icon::{IconRasteriser, TrayIcon};

/// Well-known name of the watcher the shell owns.
pub const WATCHER_BUS_NAME: &str = "org.kde.StatusNotifierWatcher";
/// Object path the watcher is served at.
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// What an item says about itself (`Status`): `Passive` items are kept
/// out of the bar, `NeedsAttention` ones are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    Passive,
    Active,
    NeedsAttention,
}

impl ItemStatus {
    pub fn parse(status: &str) -> Self {
        match status {
            "Passive" => Self::Passive,
            "NeedsAttention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Active => "active",
            Self::NeedsAttention => "needs-attention",
        }
    }
}

/// One entry of an item's menu that does something when chosen: submenus
/// and separators are flattened away, their labels kept in `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// The dbusmenu item id the `Event` call names.
    pub id: i32,
    /// Human label with mnemonics removed; a submenu entry reads
    /// `"Parent › Child"`.
    pub label: String,
    pub enabled: bool,
    /// `Some(state)` for check and radio entries.
    pub toggled: Option<bool>,
}

/// Where on the bus an item lives, and the connection that reaches it.
#[derive(Clone)]
pub struct ItemAddress {
    pub connection: zbus::Connection,
    /// The item's bus name; `None` on a peer-to-peer connection, which
    /// has no names.
    pub destination: Option<String>,
    pub path: String,
    /// The item's `com.canonical.dbusmenu` object, when it exports one.
    pub menu_path: Option<String>,
}

impl std::fmt::Debug for ItemAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemAddress")
            .field("destination", &self.destination)
            .field("path", &self.path)
            .field("menu_path", &self.menu_path)
            .finish_non_exhaustive()
    }
}

/// One registered item as the shell last read it.
#[derive(Debug, Clone)]
pub struct TrayItem {
    /// Stable key: bus name (if any) followed by object path. This is the
    /// string `RegisteredStatusNotifierItems` reports.
    pub key: String,
    /// The application's own id for the item (`Id`).
    pub id: String,
    /// What to call it: `Title`, else `Id`.
    pub title: String,
    pub status: ItemStatus,
    pub icon: TrayIcon,
    /// `ItemIsMenu`: the item has no primary action; activating it opens
    /// its menu.
    pub item_is_menu: bool,
    pub menu: Vec<MenuEntry>,
    pub address: ItemAddress,
}

/// A request from the desktop to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayRequest {
    /// Primary action (left click): usually shows or hides the window.
    Activate { x: i32, y: i32 },
    /// Secondary action (middle click).
    SecondaryActivate { x: i32, y: i32 },
    /// Ask the item to show its own context menu.
    ContextMenu { x: i32, y: i32 },
    /// Choose a menu entry by dbusmenu id.
    Menu { id: i32 },
}

#[derive(Default)]
struct TrayState {
    items: Vec<TrayItem>,
    revision: u64,
}

/// Every status-notifier item the shell knows about.
pub struct Tray {
    state: Mutex<TrayState>,
    runtime: Mutex<Option<tokio::runtime::Handle>>,
    rasteriser: IconRasteriser,
}

impl Tray {
    /// An empty tray that turns named icons into pixels with `rasteriser`.
    pub fn new(rasteriser: IconRasteriser) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            runtime: Mutex::default(),
            rasteriser,
        })
    }

    /// What turns an item's named icon into pixels.
    pub fn rasteriser(&self) -> &IconRasteriser {
        &self.rasteriser
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TrayState> {
        self.state.lock_recover()
    }

    /// Where requests are sent from; set by whoever serves the watcher.
    pub fn attach_runtime(&self, runtime: tokio::runtime::Handle) {
        *self.runtime.lock_recover() = Some(runtime);
    }

    /// Bumped on every change, so a painter can skip an unchanged tray.
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// Items in registration order.
    pub fn snapshot(&self) -> Vec<TrayItem> {
        self.lock().items.clone()
    }

    pub fn item(&self, key: &str) -> Option<TrayItem> {
        self.lock()
            .items
            .iter()
            .find(|item| item.key == key)
            .cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        self.lock()
            .items
            .iter()
            .map(|item| item.key.clone())
            .collect()
    }

    /// Insert or replace the item with `item.key`.
    pub fn upsert(&self, item: TrayItem) {
        let mut state = self.lock();
        if let Some(existing) = state.items.iter_mut().find(|known| known.key == item.key) {
            *existing = item;
        } else {
            state.items.push(item);
        }
        state.revision = state.revision.wrapping_add(1);
    }

    /// Forget `key`; `true` when it was known.
    pub fn remove(&self, key: &str) -> bool {
        let mut state = self.lock();
        let before = state.items.len();
        state.items.retain(|item| item.key != key);
        let removed = state.items.len() != before;
        if removed {
            state.revision = state.revision.wrapping_add(1);
        }
        removed
    }

    /// Send `request` to the item at `key` without waiting for it.
    ///
    /// # Errors
    ///
    /// When the item is unknown, a menu id is not in its menu, or no
    /// runtime is serving the tray.
    pub fn request(&self, key: &str, request: TrayRequest) -> Result<(), String> {
        let item = self
            .item(key)
            .ok_or_else(|| format!("no tray item {key}"))?;
        if let TrayRequest::Menu { id } = request
            && !item
                .menu
                .iter()
                .any(|entry| entry.id == id && entry.enabled)
        {
            return Err(format!("{} has no enabled menu entry {id}", item.title));
        }
        let runtime = self
            .runtime
            .lock_recover()
            .clone()
            .ok_or_else(|| "the tray is not being served".to_string())?;
        let address = item.address;
        drop(runtime.spawn(async move {
            if let Err(error) = item::send(&address, request).await {
                tracing::debug!(target: "shell::tray", ?address, ?request, %error, "tray request failed");
            }
        }));
        Ok(())
    }

    /// The tray as the control socket's state dump reports it.
    pub fn state_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.snapshot()
                .into_iter()
                .map(|item| {
                    serde_json::json!({
                        "key": item.key,
                        "id": item.id,
                        "title": item.title,
                        "status": item.status.label(),
                        "icon": item.icon.kind(),
                        "item_is_menu": item.item_is_menu,
                        "menu": item.menu.iter().map(|entry| serde_json::json!({
                            "id": entry.id,
                            "label": entry.label,
                            "enabled": entry.enabled,
                            "toggled": entry.toggled,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }
}
