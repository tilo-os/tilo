//! The shell the chrome's tests stand on.

use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use tilo_shell_surface::WidgetSurface;
use ui::headless::FrameHarness;
use ui::kurbo::Affine;
use ui::synthetic_input::{char_key, key_press};
use ui::widget::DrawChild;
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// A window of `app` , titled `title` .
pub fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// One workspace with one strip, and Notes in front in it.
pub fn layout_tree() -> Layout {
    let tabs = vec![window(20, "Notes", "so.tilo.notes")];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The shell with `tree` on it and the chrome that carries the desktop's chords over it: the shell
/// and the chrome are the two layers the session hosts, both driving the one desktop.
pub fn shell_and_chrome(tree: &Layout) -> (FrameHarness, FrameHarness, DesktopHandle) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut shell = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    shell.relayout();
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    (shell, frame, handle)
}

pub fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    let active_tab = tabs[0].id;
    WindowGroup {
        id,
        tabs,
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// The windows of group `group` , in the order the strip shows them.
pub fn windows_of(handle: &DesktopHandle, group: &str) -> Vec<String> {
    handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .filter(|entry| entry.route.group == group)
        .map(|entry| entry.route.tab)
        .collect()
}

/// What the shell has in front: the active window of the active strip.
pub fn in_front(handle: &DesktopHandle) -> Option<u64> {
    handle
        .snapshot("g", vec![])
        .active
        .and_then(|route| route.tab.parse().ok())
}

pub fn press(frame: &mut FrameHarness, key: Key) {
    frame.send_host_event(&key_press(key, None));
}

pub fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

/// Run `visit` on the chrome the frame mounts, as the session's chrome layer holds it.
pub fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// Draw one frame of the chrome, as the session's chrome layer does.
pub fn draw_chrome(frame: &mut FrameHarness) {
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::default();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = ui::vello::Scene::new();
    with_chrome(frame, |chrome| {
        chrome.draw_child(&mut scene, Affine::IDENTITY, &mut text, &theme,&presence.draw_env());
    });
}

pub fn type_text(frame: &mut FrameHarness, text: &str) {
    for character in text.chars() {
        frame.send_host_event(&char_key(&character.to_string()));
    }
}

pub fn send_chord(host: &mut WidgetSurface) {
    host.host_mut()
        .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::SUPER,
        )));
    host.host_mut()
        .send_host_event(&ui::synthetic_input::key_press(
            Key::Character("k".into()),
            None,
        ));
    host.host_mut()
        .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::empty(),
        )));
}
