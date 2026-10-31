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

use std::path::Path;

use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION};

/// The file beside the rest of this device's state.
const FILE: &str = "desk.json";

/// Where the desk lives under `state`, this device's state directory.
fn path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join(FILE)
}

/// The desk as it was saved under `state`, if there is one this build
/// understands.
pub(crate) fn load_at(state: &Path) -> Option<DesktopTree> {
    let text = std::fs::read_to_string(path(state)).ok()?;
    let tree: DesktopTree = serde_json::from_str(&text)
        .map_err(|error| tracing::debug!(%error, "the saved desk is not this build's"))
        .ok()?;
    // A file a newer build wrote describes a desk this one cannot place.
    if tree.schema_version != STATE_SCHEMA_VERSION {
        tracing::debug!(
            saved = tree.schema_version,
            this = STATE_SCHEMA_VERSION,
            "the saved desk is another build's"
        );
        return None;
    }
    if tree.workspaces.is_empty() {
        return None;
    }
    Some(shape(tree))
}

/// The desk `tree` describes, as it is kept: what a save writes and a
/// reader compares.
#[must_use]
pub(crate) fn text(tree: &DesktopTree) -> Option<String> {
    serde_json::to_string(&shape(tree.clone())).ok()
}

/// Write `text`, the desk as [`text`] made it, under `state`, replacing the
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
                },
                Display {
                    id: 1,
                    monitor: "edid:Dell|U2720Q|CN0".into(),
                    x: 1280.0,
                    y: 0.0,
                    width: 2560.0,
                    height: 1440.0,
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
        let text = text(&saved).expect("the desk is written");
        save_at(state.path(), &text).expect("the desk is saved");
        assert!(
            !text.contains("surface_id"),
            "a live surface id names nothing after a restart: {text}"
        );

        let back = load_at(state.path()).expect("the desk is read");
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
}
