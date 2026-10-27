//! The windows the shell lends the apps it mounts in its own process (a
//! Terminal tab): the slots an app declares on its window stand in that
//! tab's bar, what the person chose there goes back on the window, and an
//! app that asks closes its tab, the window with it when it was the only
//! one.

use automerge::{AutoCommit, ROOT};

use crate::shell::frame::FrameBuilder;
use crate::shell::state::{GroupId, Tab, TabId, TileContent, WindowGroup, WindowWidth, next_id};
use ui::hosted_window::HostedWindow;
use ui::window_toolbar::{LocationChoice, ToolbarEvent, ToolbarLocation, WindowToolbar};

/// A terminal's slots: where its shell runs, as the window's title, and the
/// places it can switch to.
fn terminal_slots() -> WindowToolbar {
    let choice = |label: &str, value: &str, current: bool, separated: bool| LocationChoice {
        label: label.into(),
        value: value.into(),
        current,
        separated,
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Your shell on this device".into(),
            choices: vec![
                choice("Default environment", "open:default", false, false),
                choice("Your shell on this device", "open:host", true, false),
                choice("Manage environments…", "manage", false, true),
            ],
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }
}

struct Desk {
    fb: FrameBuilder,
    group: GroupId,
    first: TabId,
    second: TabId,
}

impl Desk {
    /// The only window, focused, holds two tabs of apps the shell shows
    /// itself.
    fn with_two_tabs() -> Self {
        let mut fb = FrameBuilder::new((1600, 900));
        let (group, first, second) = (next_id(), next_id(), next_id());
        fb.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
            let strip = workspace.strips.first_mut().expect("a strip");
            let tab = |id: TabId, title: &str, surface_id| Tab {
                id,
                title: title.into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.terminal".into(),
                    surface_id,
                },
                app_title: None,
            };
            // Alone in the strip, so its bar is on screen.
            strip.window_groups.clear();
            strip.window_groups.push(WindowGroup {
                id: group,
                tabs: vec![
                    tab(first, "Terminal", 9_201),
                    tab(second, "Terminal", 9_202),
                ],
                active_tab: first,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
            let strip_id = strip.id;
            tree.desktop.set_active_group(strip_id, group);
            drop(tree);
            // The desktop rebuilds its windows from the changed tree.
            st.bump();
        });
        let mut desk = Self {
            fb,
            group,
            first,
            second,
        };
        desk.settle();
        desk
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    /// The window the shell lends tab `tab`'s app.
    fn window(&self, tab: TabId) -> HostedWindow {
        let chrome = self.fb.chrome();
        let apps = chrome
            .hosted_inputs
            .in_process()
            .expect("the shell mounts apps in its own process")
            .clone();
        drop(chrome);
        apps.borrow_mut().hosted_window(tab)
    }

    /// The tabs of the window, or `None` once it is gone.
    fn tabs(&self) -> Option<Vec<TabId>> {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.id == self.group)
            .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
    }

    fn named(&self, name: &str) -> Option<vello::kurbo::Rect> {
        self.fb
            .widget_registry()
            .get(name)
            .copied()
            .filter(|rect| rect.area() > 0.0)
    }
}

#[test]
fn exiting_the_shell_closes_its_tab_and_the_window_with_its_last_tab() {
    let mut desk = Desk::with_two_tabs();
    let first = desk.window(desk.first);
    let second = desk.window(desk.second);

    // The shell of the first tab exits, from the thread reading it.
    std::thread::spawn(move || first.close())
        .join()
        .expect("the reader thread");
    desk.settle();
    assert_eq!(
        desk.tabs(),
        Some(vec![desk.second]),
        "only the tab whose shell exited closes"
    );

    second.close();
    desk.settle();
    assert_eq!(desk.tabs(), None, "the window closes with its last tab");
    drop(desk);
}

#[test]
fn the_target_an_app_declares_on_its_window_is_the_title_in_its_bar() {
    let mut desk = Desk::with_two_tabs();
    let window = desk.window(desk.first);
    window.declare(Some(terminal_slots()));
    desk.settle();
    let shown = desk.fb.host().window_toolbars.toolbar(desk.first).cloned();
    assert_eq!(shown, Some(terminal_slots().bounded()));
    assert!(
        desk.named("window_location").is_some(),
        "the bar shows where the shell runs: {:?}",
        desk.fb
            .widget_registry()
            .keys()
            .filter(|name| name.starts_with("window_") || name.starts_with("tab:"))
            .collect::<Vec<_>>()
    );

    // What the person chose in the bar's picker goes back on the window.
    let location = terminal_slots().location.expect("a title");
    let host = location.choices.get(1).expect("the device's shell");
    assert!(desk.fb.host_mut().window_toolbars.route(
        desk.first,
        ToolbarEvent::Commit {
            text: host.value.clone()
        }
    ));
    desk.settle();
    assert_eq!(
        window.take_events(),
        vec![ToolbarEvent::Commit {
            text: "open:host".into()
        }]
    );
    assert!(
        desk.fb.host_mut().take_toolbar_outbox().is_empty(),
        "nothing goes to a connection: the app has none"
    );
    drop(desk);
}
