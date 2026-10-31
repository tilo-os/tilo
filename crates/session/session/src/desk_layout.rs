//! The desk as it was left: its workspaces, what each strip is called, every
//! window group with its tabs in order, how wide each group is, which group
//! and tab are in front, and which display each group is shown on. Kept in
//! this device's state directory, so the next session starts there instead of
//! on one empty desk.
//!
//! Every window is kept in the form that opens it again, never as it is
//! running: a live window's surface id names a buffer belonging to the
//! compositor that mapped it, and after a restart the same number is another
//! window or none. So a running app's tab is written as the app
//! ([`tilo_desktop_model::TileContent::LocalApp`]), which the next session
//! launches into the tab it was in; a tab whose content already opens again —
//! a local app, a placeholder, a stub, a window hosted on another device — is
//! written as it is.
//!
//! A desk this build cannot read is never written over: the session starts on
//! an empty one, keeps nothing, and the person is told where their own desk
//! still is ([`StateNotice`]).

use std::path::{Path, PathBuf};

use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION, TileContent};

use crate::i18n::{t, t_args};

/// The file beside the rest of this device's state.
const FILE: &str = "desk.json";

/// Where the desk lives under `state`, this device's state directory.
fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// What became of the desk this device saved.
pub(crate) enum Kept {
    /// The desk as it was left, ready to be put back. Boxed: the whole desk
    /// is many times the size of the other two answers, and every call that
    /// returns one pays for the largest.
    Desk(Box<DesktopTree>),
    /// This device kept no desk, or one with nothing on it: a first start.
    None,
    /// The desk is on disk and this build will not open it, with what the
    /// person is told about it.
    Refused(StateNotice),
}

/// Why the desk on disk was left closed. It is still there and untouched:
/// this session starts on an empty desk instead, and the person has to be
/// told, because a desk that is not theirs looks exactly like theirs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StateNotice {
    /// The file was written by a newer Tilo than this build understands.
    NewerSchema {
        path: PathBuf,
        found: u32,
        understood: u32,
    },
    /// The file exists and could not be read at all.
    Unreadable { path: PathBuf, error: String },
}

impl StateNotice {
    /// What happened, in one line.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::NewerSchema { .. } => t("desk-not-opened-newer-title"),
            Self::Unreadable { .. } => t("desk-not-opened-unreadable-title"),
        }
    }

    /// Where the person's desk is and that nothing was written over it.
    ///
    /// The layout versions go in as text: they are names of on-disk shapes,
    /// not quantities, and a group separator in the middle of one reads as a
    /// different number.
    pub(crate) fn explains(&self) -> String {
        match self {
            Self::NewerSchema {
                path,
                found,
                understood,
            } => t_args(
                "desk-not-opened-newer-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("found", found.to_string().into()),
                    ("understood", understood.to_string().into()),
                ],
            ),
            Self::Unreadable { path, error } => t_args(
                "desk-not-opened-unreadable-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.clone().into()),
                ],
            ),
        }
    }

    /// Both lines of it, as the person reads them.
    fn whole(&self) -> String {
        format!("{}\n\n{}", self.title(), self.explains())
    }
}

/// The desk as it was saved under `state`.
pub(crate) fn load_at(state: &Path) -> Kept {
    let path = path(state);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Kept::None,
        Err(error) => {
            return refuse(StateNotice::Unreadable {
                path,
                error: error.to_string(),
            });
        }
    };
    let tree: DesktopTree = match serde_json::from_str(&text) {
        Ok(tree) => tree,
        Err(error) => {
            // A newer layout means the desk is intact and this build is the
            // wrong one; a file that does not parse at all may be damage.
            return refuse(match version_in(&text) {
                Some(found) if found > STATE_SCHEMA_VERSION => StateNotice::NewerSchema {
                    path,
                    found,
                    understood: STATE_SCHEMA_VERSION,
                },
                _ => StateNotice::Unreadable {
                    path,
                    error: error.to_string(),
                },
            });
        }
    };
    if tree.schema_version > STATE_SCHEMA_VERSION {
        return refuse(StateNotice::NewerSchema {
            path,
            found: tree.schema_version,
            understood: STATE_SCHEMA_VERSION,
        });
    }
    // A desk an older build wrote has the shape this one reads, and the next
    // save brings it up to date. One with no workspace in it holds nothing to
    // come back to.
    if tree.workspaces.is_empty() {
        return Kept::None;
    }
    Kept::Desk(Box::new(shape(tree)))
}

/// The version `text` says it was written by, read without the desk having to
/// be deserialized: what tells a newer build's desk from a damaged one.
fn version_in(text: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    u32::try_from(value.get("schema_version")?.as_u64()?).ok()
}

/// Leave `notice` with the desk file it is about, for anyone not looking at
/// the screen when it happened: repair mode, support, the next login.
fn refuse(notice: StateNotice) -> Kept {
    let path = match &notice {
        StateNotice::NewerSchema { path, .. } | StateNotice::Unreadable { path, .. } => path,
    };
    tracing::error!(path = %path.display(), "{}", notice.explains());
    let beside = PathBuf::from(format!("{}.not-opened", path.display()));
    let body = format!(
        "{}\n\nWritten by Tilo when it declined to open the file next to this \
         one. Delete this file once the situation is resolved; nothing reads \
         it.\n",
        notice.whole()
    );
    if let Err(error) = std::fs::write(&beside, body) {
        tracing::warn!(
            path = %beside.display(),
            %error,
            "could not leave the explanation beside the desk file"
        );
    }
    Kept::Refused(notice)
}

/// The desk `tree` describes, as it is kept: what a save writes, what a
/// reader compares, and what the workspace's history records.
#[must_use]
pub(crate) fn value(tree: &DesktopTree) -> Option<serde_json::Value> {
    serde_json::to_value(shape(tree.clone())).ok()
}

/// Write `text`, the desk as [`value`] made it, under `state`, replacing the
/// one before.
///
/// # Errors
/// The state directory could not be written.
pub(crate) fn save_at(state: &Path, text: &str) -> std::io::Result<()> {
    let path = path(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside and moved into place: a session that dies mid-write
    // leaves the desk it had.
    let beside = path.with_extension("json.tmp");
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, &path)
}

/// `tree` as it is kept: the whole desk, except that every window stands in
/// the form that opens it again.
fn shape(mut tree: DesktopTree) -> DesktopTree {
    tree.schema_version = STATE_SCHEMA_VERSION;
    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    *tab = tab.relaunchable();
                }
                // A window whose app could not be told apart from any other —
                // the compositor named none — is kept as a tab with no app to
                // open it: nothing would ever come back into it, and the tab
                // would wait on the desk for good. What cannot open again is
                // not kept.
                group.tabs.retain(|tab| {
                    !matches!(&tab.tile, TileContent::LocalApp { app_id, .. }
                        if app_id.trim().is_empty())
                });
                if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                    && let Some(first) = group.tabs.first()
                {
                    group.active_tab = first.id;
                }
                let open = &group.tabs;
                group
                    .focus_history
                    .retain(|tab| open.iter().any(|candidate| candidate.id == *tab));
            }
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{
        Arrangement, ArrangementKind, CanvasPlace, Display, GroupId, OutputPartition, Strip,
        StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    /// The surface id of the terminal in the desk below: the compositor's own
    /// number for it, which nothing after this process can mean.
    const SURFACE: u64 = 4242;

    fn tab(id: TabId, title: &str, content: TileContent) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: content,
            app_title: None,
        }
    }

    fn local(app_id: &str, label: &str) -> TileContent {
        TileContent::LocalApp {
            app_id: app_id.into(),
            label: label.into(),
            note: String::new(),
        }
    }

    fn group(id: GroupId, tabs: Vec<Tab>, active_tab: TabId, width: WindowWidth) -> WindowGroup {
        WindowGroup {
            id,
            tabs,
            active_tab,
            width,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A desk as a person leaves it: two named strips, a window they named
    /// themselves, a running terminal, a window of another device's, widths
    /// they chose, and a canvas arrangement.
    fn desk() -> DesktopTree {
        let notes = tab(100, "Notes", local("so.tilo.notes", "Notes"));
        // The terminal the person calls "Build"; its own title stands beside
        // the name, so a cleared name goes back to it.
        let mut terminal = tab(
            101,
            "~/src",
            TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: SURFACE,
            },
        );
        assert!(terminal.rename("Build"));
        let files = tab(
            110,
            "Files",
            TileContent::Stub {
                kind: tilo_desktop_model::AppKind::Terminal,
                label: "Files".into(),
            },
        );
        let hosted = tab(
            200,
            "Mail",
            TileContent::HostedWindow {
                app_id: "thunderbird".into(),
                label: "Mail".into(),
                host: "ab12".into(),
                host_label: "Desk".into(),
            },
        );

        let mut writing = Strip::new(StripId(1), "Writing");
        writing.window_groups = vec![
            // The person's own width, not one of the tiers.
            group(10, vec![notes, terminal], 101, WindowWidth::Fraction(480)),
            group(11, vec![files], 110, WindowWidth::Narrow),
        ];
        writing.window_groups[1].maximized = true;
        let mut reference = Strip::new(StripId(2), "Reference");
        reference.window_groups = vec![group(20, vec![hosted], 200, WindowWidth::Normal)];

        let mut home = Workspace::new(WorkspaceId(1), "Home");
        home.strips = vec![writing, reference];
        home.arrangement = Arrangement {
            kind: ArrangementKind::Canvas,
            places: std::collections::BTreeMap::from([(20, CanvasPlace::new(1280, 0, 900, 700))]),
        };
        let mut away = Workspace::new(WorkspaceId(2), "Away");
        away.strips = vec![Strip::new(StripId(3), "Later")];

        let mut outputs = OutputPartition {
            displays: vec![
                Display {
                    id: 0,
                    monitor: "edid:Acme|Panel|1".into(),
                    x: 0.0,
                    y: 0.0,
                    width: 1280.0,
                    height: 800.0,
                    primary: false,
                },
                Display {
                    id: 1,
                    monitor: "edid:Dell|U2720Q|CN0".into(),
                    x: 1280.0,
                    y: 0.0,
                    width: 2560.0,
                    height: 1440.0,
                    primary: false,
                },
            ],
            ..OutputPartition::default()
        };
        outputs.home.insert(20, "edid:Dell|U2720Q|CN0".into());
        outputs.home.insert(10, "edid:Acme|Panel|1".into());

        let mut tree = DesktopTree {
            active: home.id,
            workspaces: vec![home, away],
            outputs,
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceId(1), StripId(1));
        tree.set_active_group(StripId(1), 11);
        // The window the person took across their display.
        assert!(tree.set_fullscreen(StripId(1), 11, true));
        tree
    }

    #[test]
    fn the_desk_comes_back_as_it_was_left_and_its_windows_open_again() {
        let state = tempfile::tempdir().expect("a state directory");
        let saved = desk();
        let text = serde_json::to_string(&value(&saved).expect("the desk is written"))
            .expect("the desk as text");
        save_at(state.path(), &text).expect("the desk is saved");
        assert!(
            !text.contains("surface_id"),
            "a live surface id names nothing after a restart: {text}"
        );

        let back = match load_at(state.path()) {
            Kept::Desk(back) => *back,
            Kept::None => panic!("the desk is read"),
            Kept::Refused(notice) => panic!("the desk is refused: {}", notice.explains()),
        };
        assert_eq!(back.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(back.active, saved.active);
        assert_eq!(back.active_strip, saved.active_strip);
        assert_eq!(
            back.active_group_for(StripId(1)),
            Some(11),
            "the group in front"
        );
        assert_eq!(back.outputs, saved.outputs, "the display each group is on");

        let home = &back.workspaces[0];
        assert_eq!((home.name.as_str(), back.workspaces.len()), ("Home", 2));
        assert_eq!(home.arrangement, saved.workspaces[0].arrangement);
        let names: Vec<&str> = home
            .strips
            .iter()
            .map(|strip| strip.name.as_str())
            .collect();
        assert_eq!(names, ["Writing", "Reference"], "the strips, in order");
        assert_eq!(back.workspaces[1].strips[0].name, "Later");

        let writing = &home.strips[0];
        let shapes: Vec<(GroupId, u32)> = writing
            .window_groups
            .iter()
            .map(|group| (group.id, group.width.permille()))
            .collect();
        assert_eq!(shapes, [(10, 480), (11, 333)], "each group's own width");
        assert!(
            writing.window_groups[1].maximized,
            "a maximized group stays maximized"
        );
        assert_eq!(
            back.fullscreen,
            Some(11),
            "so does the window taken across the display"
        );

        let windows = &writing.window_groups[0];
        assert_eq!(windows.active_tab, 101, "the tab in front");
        let ids: Vec<TabId> = windows.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(ids, [100, 101], "the tabs, in order");
        assert_eq!(windows.tabs[0].title, "Notes");
        assert_eq!(windows.tabs[0].tile, local("so.tilo.notes", "Notes"));
        // The person's name and the window's own title both survive.
        let terminal = &windows.tabs[1];
        assert_eq!(terminal.title, "Build");
        assert_eq!(terminal.app_title.as_deref(), Some("~/src"));
        assert_eq!(
            terminal.tile,
            local("Alacritty", "Build"),
            "the running terminal is kept as the app that opens it again"
        );

        let reference = &home.strips[1].window_groups[0];
        assert_eq!(reference.width, WindowWidth::Normal);
        assert_eq!(
            reference.tabs[0].tile,
            TileContent::HostedWindow {
                app_id: "thunderbird".into(),
                label: "Mail".into(),
                host: "ab12".into(),
                host_label: "Desk".into(),
            },
            "a window hosted on another device names nothing local and is kept"
        );
    }

    /// A window whose app the compositor named none of opens again in no
    /// way, so the tab standing for it is not kept, and the group falls back
    /// to the tab in front of the ones that are.
    #[test]
    fn a_window_with_no_app_to_open_it_is_not_kept() {
        let mut windows = group(
            10,
            vec![
                tab(
                    300,
                    "Untitled",
                    TileContent::Foreign {
                        app_id: String::new(),
                        surface_id: 7,
                    },
                ),
                tab(
                    301,
                    "Editor",
                    TileContent::Foreign {
                        app_id: "org.example.Editor".into(),
                        surface_id: 8,
                    },
                ),
            ],
            300,
            WindowWidth::Normal,
        );
        windows.focus_history = vec![301];
        let mut strip = Strip::new(StripId(1), "Windows");
        strip.window_groups = vec![windows];
        let mut tree = DesktopTree {
            active: WorkspaceId(1),
            workspaces: vec![Workspace::new(WorkspaceId(1), "Home")],
            ..DesktopTree::default()
        };
        tree.workspaces[0].strips = vec![strip];

        let written = serde_json::to_string(&value(&tree).expect("the desk is written"))
            .expect("the desk is written");
        let kept: DesktopTree = serde_json::from_str(&written).expect("a desk");
        let windows = &kept.workspaces[0].strips[0].window_groups[0];
        let ids: Vec<TabId> = windows.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(ids, [301], "only the window that can open again is kept");
        assert_eq!(
            windows.active_tab, 301,
            "the group shows the one that is left"
        );
        assert!(windows.focus_history.is_empty(), "with nothing behind it");
    }

    /// A desk from a newer build is intact, and this one is the wrong build
    /// to open it: it stays exactly as it is, and an explanation is left
    /// beside it.
    #[test]
    fn a_desk_this_build_does_not_understand_is_left_alone_and_explained() {
        let state = tempfile::tempdir().expect("a state directory");
        let newer = serde_json::to_string(&value(&desk()).expect("the desk is written"))
            .expect("the desk is written")
            .replace(
                &format!("\"schema_version\":{STATE_SCHEMA_VERSION}"),
                "\"schema_version\":9999",
            );
        let path = state.path().join("tilo").join(FILE);
        std::fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the directory");
        std::fs::write(&path, &newer).expect("the desk is saved");

        let Kept::Refused(notice) = load_at(state.path()) else {
            panic!("a desk from a newer build is not opened");
        };
        assert_eq!(
            notice,
            StateNotice::NewerSchema {
                path: path.clone(),
                found: 9999,
                understood: STATE_SCHEMA_VERSION,
            }
        );
        assert!(
            notice.explains().contains("9999"),
            "the person is told which build wrote it: {}",
            notice.explains()
        );
        let beside = PathBuf::from(format!("{}.not-opened", path.display()));
        let left = std::fs::read_to_string(&beside).expect("the explanation is beside the desk");
        assert!(left.contains(&notice.title()), "{left}");
        assert!(
            left.contains(&path.display().to_string()),
            "it names the file it is about: {left}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the desk is still there"),
            newer,
            "nothing was written over it"
        );
    }

    /// A file that is not a desk at all is damage rather than another
    /// build's, and is explained the same way.
    #[test]
    fn a_desk_that_cannot_be_read_is_damage_and_says_so() {
        let state = tempfile::tempdir().expect("a state directory");
        let path = state.path().join("tilo").join(FILE);
        std::fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the directory");
        std::fs::write(&path, "{ not a desk").expect("the file is there");

        match load_at(state.path()) {
            Kept::Refused(StateNotice::Unreadable { path: named, error }) => {
                assert_eq!(named, path);
                assert!(!error.is_empty());
            }
            Kept::Refused(other) => panic!("a damaged file is not another build's: {other:?}"),
            Kept::Desk(_) | Kept::None => panic!("a damaged file is not opened and not ignored"),
        }
        assert!(
            state
                .path()
                .join("tilo")
                .join("desk.json.not-opened")
                .exists(),
            "the explanation is left beside the desk"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file is still there"),
            "{ not a desk",
            "nothing was written over it"
        );
    }
}
