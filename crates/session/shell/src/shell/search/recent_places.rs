//! Where the person's windows have been: each window's location as its app
//! declares it to the window bar ([`ui::window_toolbar::ToolbarLocation`]),
//! with the view's anchor when the app gives one. What the launcher's
//! Continue lists, newest first.
//!
//! One private file in the user's state directory, written when a window's
//! location changes. A place is the same place again when its app, its
//! workspace and what it names (its anchor, else its address, else its
//! label) are: coming back moves it to the front.

use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

/// How many places are kept: the newest, far more than Continue shows, so
/// those of other workspaces leave each workspace its own.
pub const KEPT: usize = 64;

/// One place a window was, as its app declared it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The app showing it.
    pub app_id: String,
    /// The room of the workspace it was in.
    pub room: String,
    /// The window it was in, while that window lasts.
    pub tab: u64,
    /// What the window bar showed: the document, the page, the folder.
    pub label: String,
    /// Its whole address, when the app gave one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// The view's anchor, as the app's `open` takes it: where reopening
    /// shows it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// When the window was last there, in seconds since the epoch.
    pub at: u64,
}

impl Place {
    fn names(&self) -> &str {
        self.anchor
            .as_deref()
            .filter(|anchor| !anchor.is_empty())
            .or_else(|| (!self.value.is_empty()).then_some(self.value.as_str()))
            .unwrap_or(&self.label)
    }

    fn same_as(&self, other: &Self) -> bool {
        self.app_id == other.app_id && self.room == other.room && self.names() == other.names()
    }

    /// When the window was last there; `None` for a record whose time no
    /// clock can hold.
    #[must_use]
    pub fn when(&self) -> Option<SystemTime> {
        UNIX_EPOCH.checked_add(Duration::from_secs(self.at))
    }
}

struct Places {
    path: Option<PathBuf>,
    /// Newest first, at most [`KEPT`].
    places: Vec<Place>,
}

#[cfg(not(test))]
static PLACES: OnceLock<Mutex<Places>> = OnceLock::new();

/// `$XDG_STATE_HOME/tilo/recent-places.json`. Unit tests keep it in memory:
/// a test must not read or write the developer's own history.
#[cfg(not(test))]
fn persistence_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo").join("recent-places.json"))
}

#[cfg(test)]
fn persistence_path() -> Option<PathBuf> {
    None
}

fn load() -> Mutex<Places> {
    let path = persistence_path();
    let mut places: Vec<Place> = path
        .as_ref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    places.truncate(KEPT);
    Mutex::new(Places { path, places })
}

#[cfg(not(test))]
fn places() -> MutexGuard<'static, Places> {
    PLACES.get_or_init(load).lock_recover()
}

// Each test runs on a thread of its own with places of its own.
#[cfg(test)]
thread_local! {
    static TEST_PLACES: &'static Mutex<Places> = Box::leak(Box::new(load()));
}

#[cfg(test)]
fn places() -> MutexGuard<'static, Places> {
    TEST_PLACES.with(|places| *places).lock_recover()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Window `tab` of `app_id`, in the workspace of `room`, is at `label`
/// (with its address `value` and its view's `anchor`, when the app gave
/// them) as of now.
pub fn visit(app_id: &str, room: &str, tab: u64, label: &str, value: &str, anchor: Option<&str>) {
    visit_at(app_id, room, tab, label, value, anchor, now());
}

/// [`visit`], `ago` before now: a history for tests and screenshots to
/// start from.
#[cfg(any(test, feature = "hosted-test-control"))]
pub fn visit_ago(
    app_id: &str,
    room: &str,
    tab: u64,
    label: &str,
    anchor: Option<&str>,
    ago: Duration,
) {
    visit_at(
        app_id,
        room,
        tab,
        label,
        "",
        anchor,
        now().saturating_sub(ago.as_secs()),
    );
}

fn visit_at(
    app_id: &str,
    room: &str,
    tab: u64,
    label: &str,
    value: &str,
    anchor: Option<&str>,
    at: u64,
) {
    let label = label.trim();
    if label.is_empty() || app_id.is_empty() {
        return;
    }
    let place = Place {
        app_id: app_id.to_owned(),
        room: room.to_owned(),
        tab,
        label: label.to_owned(),
        value: value.to_owned(),
        anchor: anchor
            .map(str::trim)
            .filter(|anchor| !anchor.is_empty())
            .map(str::to_owned),
        at,
    };
    let mut held = places();
    if held.places.first() == Some(&place) {
        return;
    }
    held.places.retain(|kept| !kept.same_as(&place));
    let index = held
        .places
        .iter()
        .position(|kept| kept.at <= place.at)
        .unwrap_or(held.places.len());
    held.places.insert(index, place);
    held.places.truncate(KEPT);
    let Some(path) = held.path.clone() else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&held.places) else {
        return;
    };
    drop(held);
    crate::shell::durable_writer::global().submit(path.clone(), move || {
        if let Err(error) = write_private(&path, &bytes) {
            tracing::warn!(%error, "could not save where the windows have been");
        }
    });
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600)
}

/// The places kept, newest first.
#[must_use]
pub fn recent() -> Vec<Place> {
    places().places.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coming_back_to_a_place_moves_it_to_the_front_once() {
        visit("so.tilo.notes", "home", 1, "Pitch", "", Some("pages/p1"));
        visit_ago(
            "so.tilo.notes",
            "home",
            1,
            "Plan",
            Some("pages/p2"),
            Duration::from_secs(60),
        );
        visit(
            "so.tilo.notes",
            "home",
            2,
            "Pitch deck",
            "",
            Some("pages/p1"),
        );
        let kept: Vec<(String, u64)> = recent()
            .into_iter()
            .map(|place| (place.label, place.tab))
            .collect();
        assert_eq!(
            kept,
            [("Pitch deck".to_owned(), 2), ("Plan".to_owned(), 1)],
            "the same note under a new title, in another window, is one place"
        );
        visit("so.tilo.notes", "home", 1, "  ", "", None);
        assert_eq!(recent().len(), 2, "a window with no location is nowhere");
    }
}
