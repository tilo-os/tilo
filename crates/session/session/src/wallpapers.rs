//! The desk's wallpaper: the picture the person chose, on each workspace.
//!
//! The choice is the `appearance.wallpaper` setting: one of the pictures
//! the image ships, or the person's own ([`tilo_wallpapers::CUSTOM`]),
//! which the Wallpaper portal keeps in this device's private state.
//! `appearance.wallpaper_scope` says whether a change covers the workspace
//! the person is on or every workspace, and `appearance.wallpaper_target`
//! whether it shows on the desk, the lock screen or both.
//!
//! Which picture a workspace shows is this device's own record, kept beside
//! the desk's ([`crate::desk_layout`]): a change that covers one workspace
//! leaves the others the pictures they had, and the next session starts
//! with the same. The settings document itself is the person's on every
//! device, so it carries the choice, not what each workspace here shows.
//!
//! The paint path only reads memory. A picture that is not decoded yet is
//! asked for and the compositor woken when it arrives; only the first,
//! which has nothing to replace, is decoded on the spot, so the desk never
//! shows its gradient before the person's picture.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;
use ui::peniko::ImageData;

/// The file beside the rest of this device's state.
const FILE: &str = "wallpapers.json";

/// The record format this build reads.
const SCHEMA: u32 = 1;

/// The largest edge a picture of the person's own is decoded to: a 4K
/// panel's picture covers the widest desk without holding more pixels.
const CUSTOM_EDGE: u32 = 3840;

fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// What each workspace on this device shows: its own
/// `appearance.wallpaper` value, else this device's default; no value at
/// all is the picture the image ships.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Records {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspaces: BTreeMap<String, String>,
}

impl Records {
    /// The `appearance.wallpaper` value workspace `id` shows.
    fn value_for(&self, id: &str) -> Option<&str> {
        self.workspaces
            .get(id)
            .map(String::as_str)
            .or(self.default.as_deref())
    }

    /// Apply the person's `value` the way `scope` and `target` say, with
    /// `active` the workspace they are on; whether that changed anything.
    ///
    /// A picture for the lock screen alone leaves the desk as it is. "Every
    /// workspace" replaces the default and forgets what workspaces had of
    /// their own; "this workspace" records the one the person is on and
    /// leaves the others.
    fn apply(
        &mut self,
        value: Option<&str>,
        scope: &str,
        target: &str,
        active: Option<&str>,
    ) -> bool {
        if target == "lockscreen" {
            return false;
        }
        if scope == "all" {
            let changed = self.default.as_deref() != value || !self.workspaces.is_empty();
            self.default = value.map(str::to_owned);
            self.workspaces.clear();
            return changed;
        }
        let Some(active) = active else {
            return false;
        };
        match value {
            Some(value) => {
                let before = self.workspaces.insert(active.to_owned(), value.to_owned());
                before.as_deref() != Some(value)
            }
            None => self.workspaces.remove(active).is_some(),
        }
    }
}

/// The records as the file holds them.
#[derive(Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    schema: u32,
    #[serde(flatten)]
    records: Records,
}

fn load_at(path: &Path) -> Records {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Records::default();
    };
    match serde_json::from_str::<Kept>(&text) {
        Ok(kept) if kept.schema == SCHEMA => kept.records,
        // A file a newer build wrote describes pictures this one cannot
        // place.
        _ => {
            tracing::debug!("the kept wallpapers are another build's");
            Records::default()
        }
    }
}

fn save_at(path: &Path, records: &Records) {
    let written = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let kept = Kept {
            schema: SCHEMA,
            records: records.clone(),
        };
        // Written beside and moved into place: a session that dies mid-write
        // leaves the wallpapers it had.
        let beside = path.with_extension("json.tmp");
        std::fs::write(&beside, serde_json::to_vec(&kept)?)?;
        std::fs::rename(&beside, path)
    })();
    if let Err(error) = written {
        tracing::debug!(%error, "the wallpapers are not kept");
    }
}

/// This device's wallpapers: the picture each workspace shows, decoded once
/// and kept while a workspace still shows it.
pub(crate) struct Wallpapers {
    /// Where the records are kept, if this session keeps any.
    root: Option<PathBuf>,
    /// Wakes the compositor when a picture decoded off its thread.
    wake: Arc<dyn Fn() + Send + Sync>,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    records: Records,
    /// The workspace in front, which a change covering one workspace lands
    /// on.
    active: Option<String>,
    /// Decoded pictures by the value they show.
    images: HashMap<Option<String>, ImageData>,
    /// Values decoding off this thread.
    decoding: HashSet<Option<String>>,
    /// Values that could not be decoded: the image does not carry the
    /// picture (a source checkout) or the person's own is gone, until the
    /// records name another.
    unreadable: HashSet<Option<String>>,
    /// The settings as last read: a change applies once, and a session
    /// start applies nothing, so a restart keeps what the workspaces had.
    intent: Option<(Option<String>, String, String)>,
}

impl Wallpapers {
    /// The wallpapers this device kept under `state`, remembering no picture
    /// yet; without a state directory, nothing is read or kept.
    pub(crate) fn new(root: Option<PathBuf>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                records: root
                    .as_deref()
                    .map_or_else(Records::default, |root| load_at(&path(root))),
                ..State::default()
            })),
            root,
            wake,
        }
    }

    /// Write `records` as this device's, replacing the ones before.
    fn save(&self, records: &Records) {
        if let Some(root) = &self.root {
            save_at(&path(root), records);
        }
    }

    /// The picture workspace `id` shows, if it is decoded: a frame draws
    /// what is ready and asks for the rest.
    pub(crate) fn picture(&self, id: &str) -> Option<ImageData> {
        let mut state = self.state.lock_recover();
        let value = state.records.value_for(id).map(str::to_owned);
        if let Some(image) = state.images.get(&value) {
            return Some(image.clone());
        }
        if state.unreadable.contains(&value) || state.decoding.contains(&value) {
            return None;
        }
        // The first picture is decoded here, where there is nothing to keep
        // up, so the desk's first frame is the person's wallpaper. One that
        // replaces a picture shown decodes off this thread.
        if state.images.is_empty() && state.decoding.is_empty() {
            let decoded = decode(value.as_deref());
            return if let Some(image) = decoded {
                state.images.insert(value, image.clone());
                Some(image)
            } else {
                state.unreadable.insert(value);
                None
            };
        }
        state.decoding.insert(value.clone());
        drop(state);
        let state = Arc::clone(&self.state);
        let wake = Arc::clone(&self.wake);
        let asked = value.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-wallpaper".into())
            .spawn(move || {
                let image = decode(asked.as_deref());
                let mut state = state.lock_recover();
                state.decoding.remove(&asked);
                match image {
                    Some(image) => {
                        state.images.insert(asked, image);
                    }
                    None => {
                        state.unreadable.insert(asked);
                    }
                }
                drop(state);
                wake();
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "a wallpaper cannot be decoded off this thread");
            self.state.lock_recover().decoding.remove(&value);
        }
        None
    }

    /// Note which workspace the person is on: the one a change covering
    /// "this workspace" lands on.
    pub(crate) fn set_active(&self, id: Option<String>) {
        self.state.lock_recover().active = id;
    }

    /// Start from `document`, the person's settings. A device that kept no
    /// picture yet takes the one chosen as its default; one that has them
    /// keeps each workspace's, as [`Self::follow`] does.
    pub(crate) fn start(&self, document: &Map<String, Value>) {
        let taken = {
            let mut state = self.state.lock_recover();
            if state.records.default.is_none() && state.records.workspaces.is_empty() {
                settings_store::appearance::WALLPAPER
                    .from_document(document)
                    .inspect(|value| state.records.default = Some(value.clone()))
            } else {
                None
            }
        };
        if taken.is_some() {
            let records = self.state.lock_recover().records.clone();
            self.save(&records);
        }
        self.follow(document);
    }

    /// Follow `document`, the person's settings: a changed picture, where it
    /// shows or which workspaces it covers is applied to the desk. The
    /// first read only notes it, as the session's own start read does.
    pub(crate) fn follow(&self, document: &Map<String, Value>) {
        let value = settings_store::appearance::WALLPAPER.from_document(document);
        let appearance = document.get("appearance").and_then(Value::as_object);
        let leaf = |name: &str, default: &str| {
            appearance
                .and_then(|section| section.get(name))
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_owned()
        };
        let intent = (
            value,
            leaf("wallpaper_scope", "workspace"),
            leaf("wallpaper_target", "background"),
        );
        let mut state = self.state.lock_recover();
        let active = state.active.clone();
        let (value, scope, target) = &intent;
        // A change that covers one workspace waits for the one in front
        // rather than landing on none.
        if scope != "all" && target != "lockscreen" && active.is_none() {
            return;
        }
        // The first read is the session's own start read: what the person
        // chose is noted, and the workspaces keep the pictures this device
        // kept for them.
        let Some(last) = state.intent.replace(intent.clone()) else {
            return;
        };
        if last == intent {
            return;
        }
        if !state
            .records
            .apply(value.as_deref(), scope, target, active.as_deref())
        {
            return;
        }
        let records = state.records.clone();
        // The pictures no workspace shows any more are let go: each one is
        // the whole screen in pixels.
        let (default, workspaces) = (&records.default, &records.workspaces);
        state.images.retain(|value, _| {
            value == default || workspaces.values().any(|own| value.as_deref() == Some(own))
        });
        state.unreadable.clear();
        drop(state);
        self.save(&records);
    }
}

/// The picture `value` names, decoded.
fn decode(value: Option<&str>) -> Option<ImageData> {
    match tilo_wallpapers::choice(value)? {
        tilo_wallpapers::Choice::Bundled(wallpaper) => wallpaper.decode_picture(),
        tilo_wallpapers::Choice::Custom => tilo_wallpapers::decode_custom_picture(CUSTOM_EDGE),
    }
    .map(|decoded| ui::raster::rgba8(decoded.rgba, decoded.width, decoded.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn appearance(section: Value) -> Map<String, Value> {
        Map::from_iter([("appearance".to_owned(), section)])
    }

    #[test]
    fn a_workspace_keeps_the_picture_the_person_gave_it() {
        let mut records = Records::default();
        assert!(records.apply(Some("meadow"), "workspace", "background", Some("1")));
        assert!(records.apply(Some("dunes"), "workspace", "background", Some("2")));
        assert_eq!(records.value_for("1"), Some("meadow"));
        assert_eq!(records.value_for("2"), Some("dunes"));
        // One the person never chose shows the picture the image ships.
        assert_eq!(records.value_for("3"), None);
        assert!(records.apply(None, "workspace", "background", Some("2")));
        assert_eq!(records.value_for("2"), None);
        assert_eq!(records.value_for("1"), Some("meadow"));
    }

    #[test]
    fn every_workspace_takes_the_change_and_its_own_pictures_are_forgotten() {
        let mut records = Records::default();
        assert!(records.apply(Some("meadow"), "workspace", "background", Some("1")));
        assert!(records.apply(Some("dunes"), "all", "background", Some("1")));
        assert_eq!(records.value_for("1"), Some("dunes"));
        assert_eq!(records.value_for("2"), Some("dunes"));
        // The same choice again changes nothing.
        assert!(!records.apply(Some("dunes"), "all", "background", None));
    }

    #[test]
    fn a_picture_for_the_lock_screen_leaves_the_desk_as_it_is() {
        let mut records = Records::default();
        assert!(records.apply(Some("meadow"), "workspace", "background", Some("1")));
        assert!(!records.apply(Some("dunes"), "all", "lockscreen", Some("1")));
        assert_eq!(records.value_for("1"), Some("meadow"));
        // Both places shows it on the desk too.
        assert!(records.apply(Some("dunes"), "all", "both", Some("1")));
        assert_eq!(records.value_for("2"), Some("dunes"));
    }

    #[test]
    fn a_change_lands_on_the_workspace_in_front_after_the_first_read_notes_it() {
        let wallpapers = Wallpapers::new(None, Arc::new(|| {}));
        wallpapers.set_active(Some("2".into()));
        // The session's own start read applies nothing: the workspaces keep
        // the pictures this device kept for them.
        wallpapers.follow(&appearance(json!({ "wallpaper": "meadow" })));
        assert_eq!(wallpapers.state.lock_recover().records.value_for("2"), None);
        wallpapers.follow(&appearance(json!({ "wallpaper": "dunes" })));
        let state = wallpapers.state.lock_recover();
        assert_eq!(state.records.value_for("2"), Some("dunes"));
        assert_eq!(state.records.value_for("1"), None);
        drop(state);
    }

    #[test]
    fn the_wallpapers_this_device_kept_come_back_with_the_next_session() {
        let state = tempfile::tempdir().expect("a state directory");
        let kept = Records {
            default: Some("dunes".into()),
            workspaces: BTreeMap::from([("1".to_owned(), "meadow".to_owned())]),
        };
        let wallpapers = Wallpapers::new(Some(state.path().to_path_buf()), Arc::new(|| {}));
        wallpapers.save(&kept);
        let again = Wallpapers::new(Some(state.path().to_path_buf()), Arc::new(|| {}));
        assert_eq!(again.state.lock_recover().records, kept);
    }
}
