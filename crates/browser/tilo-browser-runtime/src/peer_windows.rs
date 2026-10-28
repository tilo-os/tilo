//! Where the desktop shows the legacy windows other members stream.
//!
//! A stream names the layout's tab holding its window
//! (`legacy_window::Stream::tab`), the same tab on every device of the
//! workspace. The desktop shows such a stream in that tab, where the layout
//! places it, so the window is there once and moves with the layout; while
//! the layout lacks the tab the stream is shown nowhere, as a native device
//! shows it. A stream naming no tab opens a window of its own.

use std::collections::HashMap;

use tilo_desktop_model::{DesktopTree, Tab, TabId, TileContent};

/// Launch name of the window showing the legacy window `key`.
pub(crate) const PEER_WINDOW_LAUNCH: &str = "tilo.peer-window:";

/// Another member's legacy window, as the page streams it: its key (the
/// page's name for the stream), app id, title, codec and the layout's tab
/// holding it, 16 hex digits or empty.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct PeerWindow {
    pub key: String,
    pub app: String,
    pub title: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub tab: String,
    /// Shown in the layout's tab it names, not in a window of its own.
    #[serde(skip)]
    pub placed: bool,
}

impl PeerWindow {
    /// The layout's tab the stream names, if it names one.
    pub fn layout_tab(&self) -> Option<TabId> {
        u64::from_str_radix(&self.tab, 16)
            .ok()
            .filter(|tab| *tab != 0)
    }
}

/// What showing the streamed legacy windows asks of the desktop: the
/// launch names of the windows to open, the tabs of windows to close, and
/// the layout tabs whose content changed (a stream placed in them, or gone).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerWindowChanges {
    pub launch: Vec<String>,
    pub close: Vec<TabId>,
    pub rebuild: Vec<TabId>,
}

/// Place `wanted`, the streams to show, in `tree`, with `open` the windows
/// showing streams by tab: layout tabs take the streams naming them at once
/// (in `open`), and the changes say which windows of their own to open
/// (with the streams they show, by key) and close.
pub(crate) fn place(
    tree: &DesktopTree,
    open: &mut HashMap<TabId, PeerWindow>,
    wanted: Vec<PeerWindow>,
) -> (PeerWindowChanges, Vec<PeerWindow>) {
    let tabs: HashMap<TabId, &Tab> = tree
        .workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab))
        .collect();
    let in_layout = |tab: TabId| {
        tabs.get(&tab)
            .is_some_and(|tab| !matches!(tab.tile, TileContent::PeerWindow { .. }))
    };
    let wanted: Vec<PeerWindow> = wanted
        .into_iter()
        .filter_map(|mut window| {
            match window.layout_tab() {
                None => window.placed = false,
                Some(tab) if in_layout(tab) => window.placed = true,
                Some(_) => return None,
            }
            Some(window)
        })
        .collect();
    let mut changes = PeerWindowChanges::default();
    // Layout tabs: each shows the stream naming it, and nothing else.
    let placed: HashMap<TabId, &PeerWindow> = wanted
        .iter()
        .filter(|window| window.placed)
        .filter_map(|window| Some((window.layout_tab()?, window)))
        .collect();
    open.retain(|tab, window| {
        if !window.placed {
            return true;
        }
        let kept = placed.get(tab).is_some_and(|wanted| *wanted == window);
        if !kept {
            changes.rebuild.push(*tab);
        }
        kept
    });
    for (tab, window) in &placed {
        if !open.contains_key(tab) {
            open.insert(*tab, (*window).clone());
            if !changes.rebuild.contains(tab) {
                changes.rebuild.push(*tab);
            }
        }
    }
    // Windows of their own: closed once their stream is gone or names a
    // layout tab.
    for tab in tabs
        .values()
        .filter(|tab| matches!(tab.tile, TileContent::PeerWindow { .. }))
    {
        if open.get(&tab.id).is_none_or(|window| {
            !wanted
                .iter()
                .any(|wanted| !wanted.placed && wanted.key == window.key)
        }) {
            changes.close.push(tab.id);
        }
    }
    changes.close.sort_unstable();
    changes.rebuild.sort_unstable();
    let mut pending = Vec::new();
    for window in wanted.into_iter().filter(|window| !window.placed) {
        if open
            .iter()
            .any(|(tab, open)| open.key == window.key && !changes.close.contains(tab))
        {
            continue;
        }
        changes
            .launch
            .push(format!("{PEER_WINDOW_LAUNCH}{}", window.key));
        pending.push(window);
    }
    (changes, pending)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tilo_desktop_model::{
        DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceId,
    };

    use super::{PEER_WINDOW_LAUNCH, PeerWindow, PeerWindowChanges, place};

    const HOSTED: u64 = 0xab;

    fn tab(id: u64, tile: TileContent) -> Tab {
        Tab {
            id,
            title: "Chromium".into(),
            tile,
            app_title: None,
        }
    }

    fn hosted() -> TileContent {
        TileContent::HostedWindow {
            app_id: "org.chromium.Chromium".into(),
            label: "Chromium".into(),
            host: "a1".repeat(32),
            host_label: "laptop".into(),
        }
    }

    fn own_window() -> TileContent {
        TileContent::PeerWindow {
            app_id: "org.chromium.Chromium".into(),
            label: "Chromium".into(),
            note: String::new(),
            missing: None,
        }
    }

    /// A desktop whose one group holds `tabs`.
    fn tree(tabs: Vec<Tab>) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId::default(), "Room");
        let mut strip = Strip::new(StripId(5), "Main");
        strip.window_groups.push(WindowGroup {
            id: 7,
            active_tab: tabs.first().map_or(0, |tab| tab.id),
            tabs,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips = vec![strip];
        DesktopTree {
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    fn stream(tab: &str) -> PeerWindow {
        serde_json::from_value(serde_json::json!({
            "key": "shell:0000000000000007",
            "app": "org.chromium.Chromium",
            "title": "Chromium",
            "codec": "h264",
            "tab": tab,
        }))
        .unwrap()
    }

    /// A native device's legacy window is the layout's tab on every device:
    /// its stream is shown in that tab, once, and no window of its own opens.
    #[test]
    fn a_stream_naming_its_layout_tab_is_shown_there_and_nowhere_else() {
        let desktop = tree(vec![tab(HOSTED, hosted())]);
        let mut open = HashMap::new();
        let (changes, pending) = place(&desktop, &mut open, vec![stream("00000000000000ab")]);
        assert_eq!(
            changes,
            PeerWindowChanges {
                rebuild: vec![HOSTED],
                ..PeerWindowChanges::default()
            }
        );
        assert!(pending.is_empty(), "no window of its own");
        assert_eq!(open.keys().copied().collect::<Vec<_>>(), [HOSTED]);
        assert!(open[&HOSTED].placed);

        let (again, _) = place(&desktop, &mut open, vec![stream("00000000000000ab")]);
        assert_eq!(again, PeerWindowChanges::default(), "nothing to redo");

        let (gone, _) = place(&desktop, &mut open, Vec::new());
        assert_eq!(gone.rebuild, [HOSTED], "the tab no longer shows the stream");
        assert!(gone.close.is_empty(), "the layout's tab stays");
        assert!(open.is_empty());
    }

    /// While the layout lacks the tab a stream names, the stream waits for
    /// it rather than opening a window that would show it twice later.
    #[test]
    fn a_stream_waits_for_the_tab_it_names() {
        let mut open = HashMap::new();
        let (changes, pending) = place(
            &tree(Vec::new()),
            &mut open,
            vec![stream("00000000000000ab")],
        );
        assert_eq!(changes, PeerWindowChanges::default());
        assert!(pending.is_empty() && open.is_empty());
    }

    /// A stream naming no tab opens a window of its own, which closes once
    /// the stream names a layout tab.
    #[test]
    fn a_stream_naming_no_tab_opens_its_own_window_until_it_names_one() {
        let mut open = HashMap::new();
        let (changes, pending) = place(&tree(Vec::new()), &mut open, vec![stream("")]);
        assert_eq!(
            changes.launch,
            [format!("{PEER_WINDOW_LAUNCH}shell:0000000000000007")]
        );
        assert_eq!(pending, [stream("")]);

        open.insert(0x51, stream(""));
        let desktop = tree(vec![tab(HOSTED, hosted()), tab(0x51, own_window())]);
        let (changes, pending) = place(&desktop, &mut open, vec![stream("00000000000000ab")]);
        assert_eq!(
            changes,
            PeerWindowChanges {
                launch: Vec::new(),
                close: vec![0x51],
                rebuild: vec![HOSTED],
            }
        );
        assert!(pending.is_empty());
    }
}
