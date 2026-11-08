//! The desk the shell this session replaced kept, brought over once.
//!
//! The shell kept a person's workspaces, strips, windows and tabs in
//! `~/.config/tilo/state.json`: a versioned file — 3 through 6 — whose body is
//! the portable desktop this session draws, wrapped in the shell's own
//! bookkeeping. A device that used the shell comes up here with no desk of its
//! own, so the first start reads the file through the steps that bring each
//! older version up ([`STATE_FORMAT`]), writes the desk it held as this
//! device's one document ([`crate::desk_doc`]), and moves the file aside as
//! `state.json.imported`, never deleting it. A desk already on this device is
//! never replaced by it.
//!
//! A newer file is not read, and a file that cannot be read is not written
//! over: it stays where it is, untouched, and the person is told where their
//! old desk still is ([`StateNotice::OldDeskNewer`],
//! [`StateNotice::OldDeskUnreadable`]) — a desk brought over wrongly is worse
//! than a desk still on disk.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use automerge::transaction::CommitOptions;
use tilo_desktop_model::{DesktopTree, desktop_doc};
use tilo_files::native::migration::Format;

use crate::desk_doc::StateNotice;

/// The version of the file the shell last wrote.
pub(crate) const VERSION: u32 = 6;

/// The suffix the file is moved to once its desk is this device's: what was there is kept for good,
/// and nothing reads it again.
const IMPORTED: &str = "imported";

/// Largest desk file this build reads. A desk is a few hundred windows at most; anything past this
/// is not one, and reading it would cost the session's memory to find out.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// `state.json` as a versioned format: a file of an older version comes up to
/// [`VERSION`] when it is imported, after a copy of it is kept beside it
/// (docs/architecture/compatibility.md). A change to the file's shape bumps
/// the version and adds a step from the one before; `oldest` never rises, so
/// a machine that missed shell updates still comes over.
pub(crate) const STATE_FORMAT: Format = Format {
    name: "desktop state",
    version: VERSION,
    oldest: 3,
    steps: &[
        drop_untouched_lounges,
        activity_becomes_an_app,
        owner_record_leaves_the_file,
    ],
};

/// The file the shell kept this device's desk in, where a device that ran it still has one.
/// `TILO_STATE_PATH` overrides it, as it did for the shell: a harness that kept its desk elsewhere
/// wrote it elsewhere.
pub fn shell_state_path() -> Option<PathBuf> {
    if let Some(set) = std::env::var_os("TILO_STATE_PATH").filter(|set| !set.is_empty()) {
        return Some(PathBuf::from(set));
    }
    tilo_paths::config_dir().map(|config| config.join("tilo").join("state.json"))
}

/// What became of the desk the shell kept.
pub(crate) enum Imported {
    /// Nothing came over: this device has a desk already, or the shell kept none here.
    Nothing,
    /// The desk the shell kept is this device's desk now.
    Desk,
    /// The desk is on disk and this build did not open it, with what the person is told about it.
    /// The file was not changed.
    Refused(StateNotice),
}

/// Bring the desk `file` holds over into `doc` , the desk's one document at `document` , written by
/// `actor` , unless this device has a desk already. The file is moved aside once the document is
/// written: a session that dies before the write lands imports again.
pub(crate) fn bring_over(
    file: &Path,
    document: &Path,
    doc: &mut AutoCommit,
    actor: automerge::ActorId,
) -> Imported {
    // A desk this device used is never replaced by one an older program kept: the person has worked
    // here since.
    if desktop_doc::workspaces(doc) > 0 {
        return Imported::Nothing;
    }
    let text = match read(file) {
        Read::Text(text) => text,
        Read::None => return Imported::Nothing,
        Read::Refused(error) => return Imported::Refused(unreadable(file, error)),
    };
    let stored = match serde_json::from_str::<StoredVersion>(&text) {
        Ok(stored) => stored.schema_version,
        Err(error) => return Imported::Refused(unreadable(file, error.to_string())),
    };
    // An older file comes up to date first, a copy of it kept; a newer one is refused untouched.
    let bytes = match STATE_FORMAT.bring_up(file, text.into_bytes(), stored) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Imported::Refused(match error {
                tilo_files::native::migration::MigrationError::Newer {
                    stored, version, ..
                } => StateNotice::OldDeskNewer {
                    path: file.to_path_buf(),
                    found: stored,
                    understood: version,
                },
                error => unreadable(file, error.to_string()),
            });
        }
    };
    let tree: DesktopTree = match serde_json::from_slice(&bytes) {
        Ok(tree) => tree,
        Err(error) => return Imported::Refused(unreadable(file, error.to_string())),
    };
    if tree.workspaces.is_empty() {
        // A file the shell wrote before its person had a workspace: nothing in it to bring over,
        // and no reason to read it again.
        set_aside(file);
        return Imported::Nothing;
    }
    doc.set_actor(actor);
    crate::desk_doc::stamp_schema(doc);
    desktop_doc::seed(doc, &crate::desk_doc::relaunchable(&tree));
    doc.commit_with(CommitOptions::default().with_time(crate::activity::now_seconds()));
    let bytes = doc.save();
    if let Err(error) = tilo_files::native::atomic::write_private(document, &bytes) {
        tracing::warn!(%error, "the desk is not kept");
        return Imported::Desk;
    }
    set_aside(file);
    Imported::Desk
}

/// The file as the shell left it.
enum Read {
    Text(String),
    /// The shell kept no desk on this device.
    None,
    Refused(String),
}

fn read(file: &Path) -> Read {
    let handle = match std::fs::File::open(file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Read::None,
        Err(error) => return Read::Refused(error.to_string()),
    };
    let mut text = String::new();
    if let Err(error) = handle
        .take(MAX_BYTES.saturating_add(1))
        .read_to_string(&mut text)
    {
        return Read::Refused(error.to_string());
    }
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Read::Refused("larger than a desk file can be".into());
    }
    Read::Text(text)
}

fn unreadable(file: &Path, error: String) -> StateNotice {
    StateNotice::OldDeskUnreadable {
        path: file.to_path_buf(),
        error,
    }
}

/// Keep the file the desk came from, where nothing reads it again.
fn set_aside(file: &Path) {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(IMPORTED);
    let beside = file.with_file_name(name);
    if let Err(error) = std::fs::rename(file, &beside) {
        tracing::warn!(%error, path = %beside.display(), "the old desk file stays");
    }
}

/// The version a desk file says it is, read before anything else of it.
#[derive(serde::Deserialize)]
struct StoredVersion {
    #[serde(default)]
    schema_version: u32,
}

/// Version 3 to 4: workspaces no longer get a Lounge. Version 3 gave every workspace one, a strip
/// and its first windows (chat, activity, notes) under ids in the [`RESERVED_SALT`] namespace,
/// which nobody but that code minted in. A Lounge the person never put a window or tab of their own
/// into (every window and tab id of it in that namespace) goes; one they used stays as it is, and
/// so does one that is a workspace's only strip. Focus on a dropped strip moves to the workspace's
/// first strip.
///
/// [`RESERVED_SALT`]: tilo_desktop_model::ids::RESERVED_SALT
#[allow(
    clippy::needless_pass_by_value,
    reason = "a migration step: every step of a file's migrations takes its bytes and returns them"
)]
fn drop_untouched_lounges(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use serde_json::Value;
    use tilo_desktop_model::ids::{RESERVED_SALT, peer_salt_of};
    fn reserved(id: Option<&Value>) -> bool {
        id.and_then(Value::as_u64)
            .is_some_and(|id| peer_salt_of(id) == RESERVED_SALT)
    }
    fn untouched(strip: &Value) -> bool {
        reserved(strip.get("id"))
            && strip
                .get("window_groups")
                .and_then(Value::as_array)
                .is_none_or(|groups| {
                    groups.iter().all(|group| {
                        reserved(group.get("id"))
                            && group
                                .get("tabs")
                                .and_then(Value::as_array)
                                .is_none_or(|tabs| tabs.iter().all(|tab| reserved(tab.get("id"))))
                    })
                })
    }
    let mut file: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut dropped: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    // Workspace id, as the focus maps key it, to its first strip left.
    let mut first_left: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for workspace in file
        .get_mut("workspaces")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let key = workspace
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string());
        let Some(strips) = workspace.get_mut("strips").and_then(Value::as_array_mut) else {
            continue;
        };
        if strips.iter().all(untouched) {
            continue;
        }
        strips.retain(|strip| {
            if !untouched(strip) {
                return true;
            }
            if let Some(id) = strip.get("id").and_then(Value::as_u64) {
                dropped.insert(id);
            }
            false
        });
        if let (Some(key), Some(first)) = (
            key,
            strips
                .first()
                .and_then(|strip| strip.get("id"))
                .and_then(Value::as_u64),
        ) {
            first_left.insert(key, first);
        }
    }
    if let Some(active) = file.get_mut("active_strip").and_then(Value::as_object_mut) {
        for (workspace, strip) in active.iter_mut() {
            if strip.as_u64().is_some_and(|id| dropped.contains(&id))
                && let Some(first) = first_left.get(workspace)
            {
                *strip = Value::from(*first);
            }
        }
    }
    if let Some(groups) = file.get_mut("active_group").and_then(Value::as_object_mut) {
        groups.retain(|strip, _| !strip.parse::<u64>().is_ok_and(|id| dropped.contains(&id)));
    }
    if let Some(retained) = file
        .get_mut("strips_to_retain")
        .and_then(Value::as_array_mut)
    {
        retained.retain(|strip| strip.as_u64().is_none_or(|id| !dropped.contains(&id)));
    }
    let Some(fields) = file.as_object_mut() else {
        return Err("a desk file is a JSON object".into());
    };
    fields.insert("schema_version".into(), Value::from(4_u32));
    serde_json::to_vec(&file).map_err(|error| error.to_string())
}

/// Version 4 to 5: Activity is an app. Version 4 kept the Activity window as a tile of its own
/// kind; it becomes the Activity app's window, in the same place under the same tab id, and opens
/// the app when the session starts like any app's window.
#[allow(
    clippy::needless_pass_by_value,
    reason = "a migration step: every step of a file's migrations takes its bytes and returns them"
)]
fn activity_becomes_an_app(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use serde_json::Value;
    let mut file: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let tabs = file
        .get_mut("workspaces")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace.get_mut("strips").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(|strip| strip.get_mut("window_groups").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(|group| group.get_mut("tabs").and_then(Value::as_array_mut))
        .flatten();
    for tab in tabs {
        if tab.get("tile").and_then(Value::as_str) == Some("WorkspaceActivity") {
            tab["tile"] = serde_json::json!({
                "LocalApp": {
                    "app_id": tilo_actions::remote::workspace_activity::APP_ID,
                    // The name the shell titled it by; the app's own name is the same word, and
                    // this record is what a window's chrome shows before the app answers.
                    "label": "Activity",
                    "note": "",
                }
            });
        }
    }
    let Some(fields) = file.as_object_mut() else {
        return Err("a desk file is a JSON object".into());
    };
    fields.insert("schema_version".into(), Value::from(5_u32));
    serde_json::to_vec(&file).map_err(|error| error.to_string())
}

/// Version 5 to 6: the device's record of the rooms it held was part of the desk file; it is not
/// the desk. The shell moved it to a file of its own as these rooms became workspace documents, so
/// it is dropped here with the step that dropped it there — a record that could not be read stays
/// in the copy kept beside the file.
#[allow(
    clippy::needless_pass_by_value,
    reason = "a migration step: every step of a file's migrations takes its bytes and returns them"
)]
fn owner_record_leaves_the_file(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut file: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let Some(fields) = file.as_object_mut() else {
        return Err("a desk file is a JSON object".into());
    };
    fields.remove("native_authority");
    fields.insert("schema_version".into(), serde_json::Value::from(6_u32));
    serde_json::to_vec(&file).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    //! A desk the shell kept at any version it wrote comes over as this device's desk, and comes
    //! over once.

    use super::*;
    use crate::desk_doc::{DeskDoc, Kept};
    use tilo_desktop_model::ids::RESERVED_SALT;
    use tilo_desktop_model::{
        IdMinter, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceSlot, compose_id,
    };

    /// The two workspaces a device that used the shell left.
    const PLANS: &str = "Plans";
    const RESEARCH: &str = "Research";
    /// The id the Activity window a version 4 file kept.
    const ACTIVITY: u64 = 4242;
    /// The salt a second device of the person minted under.
    const OTHER_DEVICE: u16 = 0x1234;

    /// The desk a person left: two workspaces, each with a Main and a Side strip and a window per
    /// tab.
    fn desk() -> DesktopTree {
        let mut minter = IdMinter::new(7);
        let mut tree = DesktopTree::default();
        for name in [PLANS, RESEARCH] {
            let mut workspace = Workspace::new(WorkspaceSlot(minter.mint()), name);
            workspace.slot = WorkspaceSlot::of_workspace(workspace.id());
            workspace.strips = [("Main", &["Notes", "Files"][..]), ("Side", &["Search"][..])]
                .into_iter()
                .map(|(strip_name, tabs)| {
                    let mut strip = Strip::new(StripId(minter.mint()), strip_name);
                    strip.window_groups =
                        tabs.iter().map(|title| group(&mut minter, title)).collect();
                    strip
                })
                .collect();
            tree.workspaces.push(workspace);
        }
        tree.active = tree.workspaces[0].slot;
        let first_strip = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, first_strip);
        tree
    }

    fn tab(title: &str, id: u64) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn group(minter: &mut IdMinter, title: &str) -> WindowGroup {
        let tab = tab(title, minter.mint());
        WindowGroup {
            id: minter.mint(),
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A Lounge as version 3 made it: a strip and three windows under ids in the reserved
    /// namespace, which the person never put a window into.
    fn lounge() -> Strip {
        let mut strip = Strip::new(StripId(compose_id(RESERVED_SALT, 1)), "Lounge");
        for (counter, title) in [(2, "Chat"), (3, "Activity"), (4, "Notes")] {
            let id = compose_id(RESERVED_SALT, counter);
            strip.window_groups.push(WindowGroup {
                id,
                active_tab: id,
                tabs: vec![Tab {
                    id,
                    title: title.into(),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".into(),
                        label: title.into(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        strip
    }

    /// `desk` as the shell wrote it in `version` : the file's own version, and what only that
    /// version's file carried.
    fn shell_file(desk: &DesktopTree, version: u32) -> Vec<u8> {
        let mut file = serde_json::to_value(desk).expect("a desk is json");
        if version <= 4 {
            // The Activity window as a kind of tile of its own.
            let groups = file["workspaces"][0]["strips"][0]["window_groups"]
                .as_array_mut()
                .expect("the first strip's windows");
            let mut activity = groups[0].clone();
            activity["id"] = serde_json::json!(ACTIVITY);
            activity["active_tab"] = serde_json::json!(ACTIVITY);
            activity["tabs"] = serde_json::json!([{
                "id": ACTIVITY,
                "title": "Activity",
                "tile": "WorkspaceActivity",
            }]);
            groups.push(activity);
        }
        if version <= 3 {
            // Every workspace got a Lounge, and the person's focus was on it.
            for workspace in file["workspaces"]
                .as_array_mut()
                .expect("the desk's workspaces")
            {
                let strip = serde_json::to_value(lounge()).expect("a strip is json");
                if let Some(strips) = workspace["strips"].as_array_mut() {
                    strips.insert(0, strip);
                }
            }
            let first = desk.workspaces[0].slot.raw().to_string();
            if let Some(active) = file["active_strip"].as_object_mut() {
                active.insert(first, serde_json::json!(compose_id(RESERVED_SALT, 1)));
            }
        }
        if version == 5 {
            // The device's record of the rooms it held, inside the file.
            file["native_authority"] =
                serde_json::json!({"revision": 0, "rooms": {}, "pending": {}});
        }
        file["schema_version"] = serde_json::json!(version);
        serde_json::to_vec(&file).expect("a desk is json")
    }

    /// What a desk holds, as a person would list it: workspaces, strips,
    /// windows and tabs, by id and title.
    type Shape = Vec<(u64, Vec<(u64, Vec<(u64, Vec<(u64, String)>)>)>)>;

    fn shape(tree: &DesktopTree) -> Shape {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.slot.raw(),
                    workspace
                        .strips
                        .iter()
                        .map(|strip| {
                            (
                                strip.id.raw(),
                                strip
                                    .window_groups
                                    .iter()
                                    .map(|group| {
                                        (
                                            group.id,
                                            group
                                                .tabs
                                                .iter()
                                                .map(|tab| (tab.id, tab.title.clone()))
                                                .collect(),
                                        )
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    /// The file the shell's desk is moved to once it is this device's.
    fn imported_path(file: &Path) -> PathBuf {
        PathBuf::from(format!("{}.{IMPORTED}", file.display()))
    }

    /// A device that ran the shell, with its desk file at `file` .
    fn shell_device() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let state = tempfile::tempdir().expect("a state directory");
        let config = tempfile::tempdir().expect("a config directory");
        let file = config.path().join("tilo").join("state.json");
        std::fs::create_dir_all(file.parent().expect("the config directory"))
            .expect("the config directory");
        (state, config, file)
    }

    /// A desk the shell wrote in any version it wrote over comes over whole: the same workspaces,
    /// strips, windows and tabs, on the ids they had; the file it came from moves aside, kept, and
    /// the older file it was is kept beside it.
    #[test]
    fn the_shells_desk_comes_over_at_every_version_it_wrote() {
        let left = desk();
        for version in [3, 4, 5, 6] {
            let (state, _config, file) = shell_device();
            let written = shell_file(&left, version);
            std::fs::write(&file, &written).expect("the shell's desk is on disk");

            let opened = DeskDoc::load(Some(state.path()), Some(&file), "ada")
                .tree()
                .expect("the shell's desk opens");

            let mut wanted = left.clone();
            if version <= 4 {
                wanted.workspaces[0].strips[0]
                    .window_groups
                    .push(activity_window());
            }
            assert_eq!(shape(&opened), shape(&wanted), "version {version} whole");

            assert!(!file.exists(), "version {version}: the file moved aside");
            assert!(
                imported_path(&file).exists(),
                "version {version}: the file is kept as it was read"
            );
            assert_eq!(
                tilo_files::native::migration::Format::snapshot_path(&file, version).exists(),
                version < 6,
                "version {version}: the file as it was is kept beside it"
            );
            assert!(
                crate::desk_doc::path(state.path()).exists(),
                "version {version}: the desk is this device's document now"
            );
        }
    }

    fn activity_window() -> WindowGroup {
        WindowGroup {
            id: ACTIVITY,
            active_tab: ACTIVITY,
            tabs: vec![Tab {
                id: ACTIVITY,
                title: "Activity".into(),
                tile: TileContent::LocalApp {
                    app_id: tilo_actions::remote::workspace_activity::APP_ID.into(),
                    label: "Activity".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A desk from before workspaces stopped getting a Lounge comes up without the ones nobody
    /// used, and with the one the person put a window into; focus on a dropped Lounge moves to the
    /// workspace's first strip left.
    #[test]
    fn a_v3_desk_keeps_the_lounges_someone_used_and_drops_the_rest() {
        let (state, _config, file) = shell_device();
        let left = desk();
        let mut written: serde_json::Value =
            serde_json::from_slice(&shell_file(&left, 3)).expect("a desk file is json");
        // One Lounge the person put a window of their own into.
        let research = written["workspaces"]
            .as_array_mut()
            .expect("the desk's workspaces")
            .iter_mut()
            .find(|workspace| workspace["name"] == serde_json::json!(RESEARCH))
            .expect("the workspace the person used");
        let sketch = compose_id(OTHER_DEVICE, 7);
        let lounge = research["strips"][0]["window_groups"]
            .as_array_mut()
            .expect("the Lounge's windows");
        let mut touched = lounge[0].clone();
        touched["id"] = serde_json::json!(sketch);
        touched["active_tab"] = serde_json::json!(sketch);
        touched["tabs"] = serde_json::json!([{
            "id": sketch,
            "title": "Sketch",
            "tile": {"LocalApp": {
                "app_id": "so.tilo.paint",
                "label": "Sketch",
                "note": "",
            }},
        }]);
        lounge.push(touched);
        std::fs::write(&file, serde_json::to_vec(&written).expect("json"))
            .expect("the shell's desk is on disk");
        let plans = left.workspaces[0].slot;
        let research = left.workspaces[1].slot;

        let desk = DeskDoc::load(Some(state.path()), Some(&file), "ada");
        let opened = desk.tree().expect("the shell's desk opens");
        let strips = |id: WorkspaceSlot| {
            opened
                .workspaces
                .iter()
                .find(|workspace| workspace.slot == id)
                .expect("the workspace came over")
                .strips
                .iter()
                .map(|strip| strip.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strips(plans),
            [
                left.workspaces[0].strips[0].id,
                left.workspaces[0].strips[1].id
            ],
            "the Lounge nobody used is gone"
        );
        assert_eq!(
            strips(research).len(),
            3,
            "the Lounge someone put a window into stays"
        );
        assert_eq!(
            strips(research)[0],
            StripId(compose_id(RESERVED_SALT, 1)),
            "and it is where it was"
        );
        assert_eq!(
            opened.active_strip.get(&plans),
            Some(&left.workspaces[0].strips[0].id),
            "focus on the dropped Lounge moved to the first strip left"
        );
    }

    /// The desk is imported once: a device that has one keeps it, whatever is on disk afterwards.
    #[test]
    fn a_device_that_has_a_desk_does_not_import_again() {
        let (state, _config, file) = shell_device();
        let left = desk();
        std::fs::write(&file, shell_file(&left, 6)).expect("the shell's desk is on disk");
        let first = DeskDoc::load(Some(state.path()), Some(&file), "ada");
        let opened = first.tree().expect("the shell's desk opens");

        let mut other = serde_json::to_value(&left).expect("a desk is json");
        other["workspaces"][0]["name"] = serde_json::json!("Somewhere else");
        std::fs::write(&file, serde_json::to_vec(&other).expect("json"))
            .expect("a second desk is on disk");
        let next = DeskDoc::load(Some(state.path()), Some(&file), "ada");
        let again = next.tree().expect("the desk opens");
        assert_eq!(shape(&again), shape(&opened), "the desk it had is kept");
        assert!(file.exists(), "nothing imported the second file");
    }

    /// A desk this build does not open is not written over and not moved: the person is told where
    /// it still is, and no desk takes its place.
    #[test]
    fn a_desk_this_build_does_not_open_is_left_where_it_is() {
        let (state, _config, file) = shell_device();
        let mut newer = serde_json::to_value(desk()).expect("a desk is json");
        newer["schema_version"] = serde_json::json!(VERSION.saturating_add(1));
        let newer = serde_json::to_vec(&newer).expect("json");
        std::fs::write(&file, &newer).expect("the shell's desk is on disk");
        let desk = DeskDoc::load(Some(state.path()), Some(&file), "ada");
        match desk.opened() {
            Kept::Refused(StateNotice::OldDeskNewer { found, .. }) => {
                assert_eq!(
                    found,
                    VERSION.saturating_add(1),
                    "the build that wrote it is named"
                );
            }
            Kept::Refused(other) => panic!("{other:?}"),
            Kept::Desk(_) | Kept::None => panic!("a desk from a newer build is not opened"),
        }
        assert_eq!(
            std::fs::read(&file).expect("the desk is still there"),
            newer,
            "nothing was written over it"
        );
        assert!(
            PathBuf::from(format!("{}.not-opened", file.display())).exists(),
            "the reason is left beside the file"
        );
        assert!(
            !crate::desk_doc::path(state.path()).exists(),
            "and no desk took its place"
        );

        std::fs::write(&file, b"{not a desk").expect("a damaged desk is on disk");
        let desk = DeskDoc::load(Some(state.path()), Some(&file), "ada");
        assert!(
            matches!(
                desk.opened(),
                Kept::Refused(StateNotice::OldDeskUnreadable { .. })
            ),
            "a damaged desk is reported"
        );
        assert_eq!(
            std::fs::read(&file).expect("the desk is still there"),
            b"{not a desk",
            "and left exactly as it was"
        );
    }

    /// The chain keeps every version the shell wrote loadable: the step from the version before the
    /// current one reads the file that version wrote.
    #[test]
    fn every_version_the_shell_wrote_comes_up_to_the_one_this_build_reads() {
        assert!(STATE_FORMAT.is_whole());
        tilo_files::native::migration::assert_step_reads_previous(
            &STATE_FORMAT,
            &shell_file(&desk(), VERSION.saturating_sub(1)),
            |current| {
                let file: serde_json::Value =
                    serde_json::from_slice(current).expect("a desk file is json");
                assert!(
                    file.get("native_authority").is_none(),
                    "the device's record of its rooms leaves the desk"
                );
                let desk: DesktopTree = serde_json::from_slice(current)
                    .expect("the step's output reads as the current desk");
                assert!(!desk.workspaces.is_empty());
            },
        );
    }
}
