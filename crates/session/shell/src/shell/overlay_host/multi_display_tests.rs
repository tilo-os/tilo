//! Every shell surface opens on the display the person is working on.
//!
//! Each case lays the production `FrameBuilder` out across a 2x laptop
//! panel with a 1x 1080p monitor to its right (the `laptop-beside-1080p`
//! fixture: the desktop's centre and bottom edge are both on the monitor,
//! below the panel's last row), focuses a window on one display and then
//! the other, and checks that the surface family under test sits wholly on
//! the focused display. A surface placed against the host's own rect fails
//! here: it straddles the seam or hangs below the panel.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the frame builder's chrome guards are held for one short assertion each"
)]

use automerge::{AutoCommit, ROOT};
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, Touch, TouchPhase, WindowEvent};

use crate::compositor::backend::headless::output_fixture;
use crate::compositor::backend::{Output, OutputIdentity, combined_geometry, output_geometry};
use crate::shell::frame::FrameBuilder;
use crate::shell::state::{
    AppKind, GroupId, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree, compose_id,
};

const SALT: u16 = 1;
const fn id(counter: u64) -> u64 {
    compose_id(SALT, counter)
}
const STRIP: StripId = StripId(id(77));
const ROOM: &str = "00000000-0000-4000-8000-0000000000a1";
const ON_PANEL: GroupId = id(1001);
const ON_MONITOR: GroupId = id(1004);
const PANEL: (&str, &str, &str, &str) = ("eDP-1", "Tilo", "Test Panel", "P-0001");
const MONITOR: (&str, &str, &str, &str) = ("DP-1", "Tilo", "Test Monitor", "M-0001");
const PANEL_KEY: &str = "edid:Tilo|Test Panel|P-0001";
const MONITOR_KEY: &str = "edid:Tilo|Test Monitor|M-0001";

fn tab_of(group: GroupId) -> u64 {
    group.saturating_add(5000)
}

fn group(id: GroupId) -> WindowGroup {
    let tab = tab_of(id);
    WindowGroup {
        id,
        tabs: vec![Tab {
            id: tab,
            title: format!("W{id}"),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: format!("W{id}"),
            },
        }],
        active_tab: tab,
        width: WindowWidth::Fraction(600),
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// One window on each display, restored from this device's partition.
fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId(7);
    let mut ws = Workspace::new(ws_id, "Desk");
    ws.set_room(RoomId::parse(ROOM).expect("room id"));
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![group(ON_PANEL), group(ON_MONITOR)],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, ON_PANEL);
    tree.desktop.outputs.home.insert(ON_PANEL, PANEL_KEY.into());
    tree.desktop
        .outputs
        .home
        .insert(ON_MONITOR, MONITOR_KEY.into());
    tree
}

struct Desk {
    fb: FrameBuilder,
    outputs: Vec<Output>,
}

impl Desk {
    fn new(fixture: &str) -> Self {
        Self::laid_out(fixture, tree())
    }

    fn laid_out(fixture: &str, tree: WorkspaceTree) -> Self {
        let mut outputs = output_fixture(fixture).expect("fixture");
        for (output, (connector, make, model, serial)) in outputs.iter_mut().zip([PANEL, MONITOR]) {
            output.identity = OutputIdentity {
                connector: connector.into(),
                make: make.into(),
                model: model.into(),
                serial: serial.into(),
                physical_mm: (0, 0),
            };
        }
        let mut fb = FrameBuilder::with_tree(tree, combined_geometry(&outputs));
        fb.set_output_geometry(output_geometry(&outputs));
        let mut desk = Self { fb, outputs };
        desk.settle();
        assert!(desk.fb.host().is_partitioned(), "two displays, two rows");
        desk
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    fn display(&self, index: usize) -> Rect {
        self.outputs[index].logical_rect()
    }

    fn focused_display(&self) -> Option<u32> {
        self.fb.chrome().tree().desktop.outputs.focused_display()
    }

    fn dispatch(&mut self, event: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }

    /// Work on `display`'s window, then run `check` against that display.
    fn on_each_display(&mut self, mut check: impl FnMut(&mut Self, Rect, &str)) {
        for (index, group) in [(1, ON_MONITOR), (0, ON_PANEL), (1, ON_MONITOR)] {
            let _ = self
                .fb
                .host_mut()
                .focus_window_on_strip(STRIP, group, tab_of(group));
            self.settle();
            assert_eq!(
                self.focused_display(),
                Some(self.outputs[index].id.0),
                "working on display {index}"
            );
            let display = self.display(index);
            let name = if index == 0 {
                "the panel"
            } else {
                "the monitor"
            };
            check(self, display, name);
        }
    }
}

fn assert_on(display: Rect, surface: Rect, what: &str, name: &str) {
    assert!(
        surface.width() > 0.0
            && surface.height() > 0.0
            && surface.x0 >= display.x0 - 0.5
            && surface.x1 <= display.x1 + 0.5
            && surface.y0 >= display.y0 - 0.5
            && surface.y1 <= display.y1 + 0.5,
        "{what} {surface:?} is wholly on {name} {display:?}"
    );
}

/// The fixture is what the cases claim: the desktop's bottom centre is on
/// the monitor, below the panel's last row.
#[test]
fn the_fixture_puts_the_desktop_centre_on_no_panel_row() {
    let desk = Desk::new("laptop-beside-1080p");
    let (panel, monitor) = (desk.display(0), desk.display(1));
    assert_eq!(panel, Rect::new(0.0, 0.0, 1472.0, 920.0));
    assert_eq!(monitor, Rect::new(1472.0, 0.0, 3392.0, 1080.0));
    let desktop = desk.fb.host().rect;
    assert!(monitor.contains(Point::new(desktop.center().x, desktop.y1 - 1.0)));
}

/// Volume and brightness keys: the readout comes up on the display in use.
#[test]
fn hardware_key_readouts_open_on_the_focused_display() {
    let mut desk = Desk::new("laptop-beside-1080p");
    desk.on_each_display(|desk, display, name| {
        desk.fb
            .with_chrome(|chrome| chrome.show_osd(crate::shell::osd::Osd::volume(0.4, false)));
        desk.settle();
        let pill = desk.fb.host().osd_rect().expect("the readout is painted");
        assert_on(display, pill, "the volume readout", name);
        assert!(
            (pill.center().x - display.center().x).abs() < 1.0,
            "the readout {pill:?} centres on {name} {display:?}"
        );
    });
}

/// Flyouts and panels: the power menu, Quick Settings, the window switcher
/// and the tab menu.
#[test]
fn menus_and_panels_open_on_the_focused_display() {
    let mut desk = Desk::new("laptop-beside-1080p");
    desk.on_each_display(|desk, display, name| {
        let host = desk.fb.host_mut();
        host.layers.power_menu.open();
        let rows = host.power_menu_semantic_rects();
        assert!(!rows.is_empty(), "the power menu has rows");
        for (row, rect) in rows {
            assert_on(display, rect, &format!("power menu row {row}"), name);
        }
        let _ = host.layers.power_menu.close();

        assert_on(
            display,
            host.quick_settings_rect_for_tests(),
            "Quick Settings",
            name,
        );

        assert!(host.begin_window_switcher(
            tilo_desktop::window_switcher::SwitcherScope::Workspace,
            false
        ));
        assert_on(
            display,
            host.window_switcher().panel_rect(),
            "the window switcher",
            name,
        );
        let _ = host.layers.window_switcher.dismiss();

        assert!(host.open_focused_tab_context_menu(), "the tab menu opens");
        let menu = host.tab_context_menu_rect().expect("the menu is laid out");
        assert_on(display, menu, "the tab menu", name);
        host.layers.tab_menu.close();
    });
}

/// Search, the workspace sheet and the strip indicators.
#[test]
fn search_and_sheets_open_on_the_focused_display() {
    let mut desk = Desk::new("laptop-beside-1080p");
    desk.on_each_display(|desk, display, name| {
        desk.fb.host_mut().set_palette_visible(true);
        desk.fb.host_mut().force_palette_settled();
        desk.settle();
        assert_on(
            display,
            desk.fb.host().palette_rect_for_tests(),
            "search",
            name,
        );
        desk.fb.host_mut().set_palette_visible(false);
        desk.settle();

        let host = desk.fb.host();
        assert_on(display, host.ws_settings_rect(), "workspace settings", name);
        let ribbon = host.strip_ribbon_for_tests().content;
        assert_on(display, ribbon, "the strip ribbon's content area", name);
    });
}

/// The app with a window on each display, for the chooser cases.
const AUDIT: &str = "org.tilo.audit";

/// [`tree`], with both windows the audit app's.
fn app_tree() -> WorkspaceTree {
    let mut tree = tree();
    for group in &mut tree.desktop.workspaces[0].strips[0].window_groups {
        let surface_id = tab_of(group.id);
        group.tabs[0].tile = TileContent::Foreign {
            app_id: AUDIT.into(),
            surface_id,
        };
    }
    tree
}

/// A file chooser `AUDIT` asks for, scoped to `folder`.
fn ask(
    pickers: &crate::portal::FilePickerQueue,
    folder: &std::path::Path,
    current_name: Option<&str>,
) -> tokio::sync::oneshot::Receiver<Option<Vec<std::path::PathBuf>>> {
    use crate::portal::FilePickerSpawner as _;
    let (reply, answer) = tokio::sync::oneshot::channel();
    pickers.spawn(crate::portal::FilePickerRequest {
        caller: crate::portal::PortalCaller::new(WorkspaceId(7), AUDIT),
        parent_window: Some("wayland:audit".into()),
        title: "Open a document".to_string(),
        save: current_name.is_some(),
        directory: false,
        writable_directory: false,
        multiple: false,
        accept_label: None,
        current_name: current_name.map(str::to_string),
        filters: Vec::new(),
        scope: Some(crate::portal::PickerScope {
            initial_dir: folder.to_path_buf(),
            roots: vec![folder.to_path_buf()],
            home: None,
        }),
        reply,
    });
    answer
}

impl Desk {
    fn with_queue(tree: WorkspaceTree) -> (Self, crate::portal::FilePickerQueue) {
        let mut desk = Self::laid_out("laptop-beside-1080p", tree);
        let pickers = crate::portal::FilePickerQueue::new();
        desk.fb.host_mut().set_portal_queues(
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoAllow),
            pickers.clone(),
        );
        (desk, pickers)
    }

    fn sheet(&self) -> Rect {
        ui::widget::Widget::get_rect(self.fb.host().file_picker().expect("the chooser is up"))
    }

    fn click(&mut self, at: Point) {
        self.dispatch(&ui::synthetic_input::cursor_moved(at.x, at.y));
        self.dispatch(&ui::synthetic_input::mouse_press());
        self.dispatch(&ui::synthetic_input::mouse_release());
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let key = winit::keyboard::Key::Character(ch.to_string().into());
            for pressed in [true, false] {
                self.dispatch(&crate::shell::test_control::keyboard_input_event(
                    &key, pressed,
                ));
            }
        }
        self.settle();
    }
}

/// A file chooser is a sheet on the window that asked, on whichever
/// display that window is: hung from the top of the window's app area,
/// centred on it, and wholly on that display.
#[test]
fn the_file_chooser_is_a_sheet_on_the_window_that_asked() {
    use crate::shell::file_picker_overlay::PickerPlace;

    let folder = tempfile::tempdir().expect("tempdir");
    let (mut desk, pickers) = Desk::with_queue(app_tree());
    let mut parked = Vec::new();
    desk.on_each_display(|desk, display, name| {
        parked.push(ask(&pickers, folder.path(), None));
        assert!(desk.fb.host_mut().pump_file_picker());
        desk.settle();
        let PickerPlace::Window {
            window,
            content,
            focused,
            ..
        } = desk.fb.host().file_picker_place()
        else {
            panic!("the chooser on {name} is a sheet on the window that asked");
        };
        assert!(focused, "the asking window is the one in use");
        assert!(
            display.contains(content.center()),
            "the sheet is on {name}'s window"
        );
        let sheet = desk.sheet();
        assert_on(display, sheet, "the file chooser", name);
        assert!(
            (sheet.center().x - content.center().x).abs() < 1.0,
            "the sheet {sheet:?} centres on its window {content:?} on {name}"
        );
        assert!(
            (sheet.y0 - content.y0).abs() < 1.0 && window.contains(sheet.origin()),
            "the sheet {sheet:?} hangs from the top of its window {content:?} on {name}"
        );
        desk.fb
            .host_mut()
            .file_picker_mut()
            .expect("the chooser is up")
            .click_cancel();
        let _ = desk.fb.host_mut().pump_file_picker();
        desk.settle();
        assert!(desk.fb.host().file_picker().is_none(), "cancel closes it");
    });
    assert_eq!(parked.len(), 3, "each request kept its reply open while up");
}

/// With two windows of one app, the sheet goes on the window the request
/// names by its exported handle, not on the one in use; a handle the app
/// did not export for one of its own windows names nothing.
#[test]
fn the_file_chooser_sheet_goes_on_the_window_its_handle_names() {
    use crate::portal::FilePickerSpawner as _;
    use crate::shell::file_picker_overlay::PickerPlace;

    let folder = tempfile::tempdir().expect("tempdir");
    let (mut desk, pickers) = Desk::with_queue(app_tree());
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_MONITOR, tab_of(ON_MONITOR));
    desk.settle();
    let panel_window = crate::compositor::window_handles::export_for_tests(tab_of(ON_PANEL));
    let elsewhere = crate::compositor::window_handles::export_for_tests(0xdead_0000_0001);
    for (parent_window, display, what) in [
        (format!("wayland:{panel_window}"), 0, "the named window"),
        (format!("wayland:{elsewhere}"), 1, "the window in use"),
        ("x11:4a0000c".to_string(), 1, "the window in use"),
    ] {
        let (reply, _answer) = tokio::sync::oneshot::channel();
        pickers.spawn(crate::portal::FilePickerRequest {
            caller: crate::portal::PortalCaller::new(WorkspaceId(7), AUDIT),
            parent_window: Some(parent_window.clone()),
            title: "Open a document".to_string(),
            save: false,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: folder.path().to_path_buf(),
                roots: vec![folder.path().to_path_buf()],
                home: None,
            }),
            reply,
        });
        assert!(desk.fb.host_mut().pump_file_picker());
        desk.settle();
        let PickerPlace::Window { content, .. } = desk.fb.host().file_picker_place() else {
            panic!("{parent_window} asks with a window");
        };
        assert!(
            desk.display(display).contains(content.center()),
            "{parent_window}: the sheet is on {what}"
        );
        desk.fb
            .host_mut()
            .file_picker_mut()
            .expect("the chooser is up")
            .click_cancel();
        let _ = desk.fb.host_mut().pump_file_picker();
        desk.settle();
    }
    crate::compositor::window_handles::forget(tab_of(ON_PANEL));
    crate::compositor::window_handles::forget(0xdead_0000_0001);
}

/// The sheet closes only its own window to input: the other window takes
/// a click and the keys that follow, the sheet's name field stays as it
/// was, and a click back on the waiting window gives the sheet the keys.
/// Closing the asking window answers the request as cancelled.
#[test]
fn the_file_chooser_sheet_blocks_only_its_own_window() {
    use crate::shell::file_picker_overlay::PickerPlace;

    let folder = tempfile::tempdir().expect("tempdir");
    let (mut desk, pickers) = Desk::with_queue(app_tree());
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_MONITOR, tab_of(ON_MONITOR));
    desk.settle();
    let mut answer = ask(&pickers, folder.path(), Some("draft"));
    assert!(desk.fb.host_mut().pump_file_picker());
    desk.settle();
    let filename = |desk: &Desk| {
        desk.fb
            .host()
            .file_picker()
            .map(|picker| picker.filename().to_string())
    };
    desk.type_text("a");
    assert_eq!(
        filename(&desk).as_deref(),
        Some("a"),
        "the sheet on the focused window takes the keys"
    );

    let other = desk.display(0).center();
    desk.click(other);
    assert_eq!(
        desk.focused_display(),
        Some(desk.outputs[0].id.0),
        "the click reached the other window"
    );
    assert!(matches!(
        desk.fb.host().file_picker_place(),
        PickerPlace::Window { focused: false, .. }
    ));
    desk.type_text("b");
    assert_eq!(
        filename(&desk).as_deref(),
        Some("a"),
        "keys for the other window never reach the sheet"
    );

    let PickerPlace::Window { window, .. } = desk.fb.host().file_picker_place() else {
        panic!("the sheet stays on its window");
    };
    let sheet = desk.sheet();
    let beside_the_sheet = Point::new(window.center().x, f64::midpoint(sheet.y1, window.y1));
    assert!(
        window.contains(beside_the_sheet) && !sheet.contains(beside_the_sheet),
        "the waiting window shows below its sheet"
    );
    desk.click(beside_the_sheet);
    assert!(matches!(
        desk.fb.host().file_picker_place(),
        PickerPlace::Window { focused: true, .. }
    ));
    desk.type_text("c");
    assert_eq!(
        filename(&desk).as_deref(),
        Some("ac"),
        "a click on the waiting window gives its sheet the keys again"
    );

    desk.fb.with_chrome(|chrome| {
        let mut tree = chrome.tree_mut();
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id != ON_MONITOR);
    });
    desk.fb.host_mut().pump_file_picker();
    assert!(desk.fb.host().file_picker().is_none());
    assert_eq!(
        answer.try_recv().ok(),
        Some(None),
        "closing the asking window cancels its chooser"
    );
}

/// A finger put down on the other display makes it the display in use, as
/// the pointer arriving there does: an edge swipe the shell keeps for
/// itself never becomes pointer motion.
#[test]
fn a_touch_on_another_display_focuses_it() {
    let mut desk = Desk::new("laptop-beside-1080p");
    let panel = desk.outputs[0].id.0;
    let monitor = desk.outputs[1].id.0;
    for (display, at) in [(monitor, desk.display(1)), (panel, desk.display(0))] {
        let p = at.center();
        for phase in [TouchPhase::Started, TouchPhase::Ended] {
            desk.dispatch(&WindowEvent::Touch(Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: PhysicalPosition::new(p.x, p.y),
                force: None,
                id: 3,
            }));
        }
        desk.settle();
        assert_eq!(desk.focused_display(), Some(display), "touched {at:?}");
    }
}

/// The on-screen keyboard docks at the bottom of the display holding the
/// field it types into — that display's own bottom, where its top is not
/// the desktop's.
#[test]
fn the_on_screen_keyboard_docks_on_its_fields_display() {
    const SURFACE: u64 = 9_900_731;
    let _env_lock = tilo_test_env::lock();
    crate::shell::foreign::store().publish_rgba(SURFACE, &[0, 0, 0, 255], 1, 1);
    // A 1280x800 panel, and the monitor at (1280, 200) beside it.
    let mut desk = Desk::new("mixed-scale-offset");
    for (index, (x, y)) in [(1, (2200, 400)), (0, (600, 100))] {
        let display = desk.display(index);
        desk.fb.set_osk_surface(
            Some(SURFACE),
            None,
            Some(ui::ime::ImeWindowState {
                content: ui::ime::InputContent::default(),
                x,
                y,
                width: 100,
                height: 30,
            }),
            true,
        );
        let (_, keyboard) = desk.fb.osk_surface_rect().expect("the keyboard is up");
        assert_on(display, keyboard, "the on-screen keyboard", "its display");
        assert!(
            display.y1 - keyboard.y1 < 40.0,
            "the keyboard {keyboard:?} docks at the bottom of {display:?}"
        );
    }
    desk.fb.set_osk_surface(None, None, None, false);
    crate::shell::foreign::store().forget(SURFACE);
}

/// A permission prompt opens across the top of the window of the app that
/// asks, on that window's display, not across the top of the other one.
#[test]
fn a_permission_prompt_opens_over_the_window_that_asks() {
    let mut desk = Desk::new("laptop-beside-1080p");
    desk.fb.with_chrome(|chrome| {
        let mut tree = chrome.tree_mut();
        let group = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter_mut()
            .find(|g| g.id == ON_MONITOR)
            .expect("the monitor's window");
        group.tabs[0].tile = TileContent::LocalApp {
            app_id: "org.tilo.audit".into(),
            label: "Audit".into(),
            note: String::new(),
        };
    });
    // The person works on the panel; the monitor's app asks.
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_PANEL, tab_of(ON_PANEL));
    desk.settle();
    let (reply, _answer) = tokio::sync::oneshot::channel();
    desk.fb.with_chrome(|chrome| {
        chrome
            .pending_grants
            .push_back(crate::shell::chrome_state::PendingPrompt::with_reply(
                "org.tilo.audit".into(),
                crate::shell::state::Capability::HostShell {
                    scope: crate::shell::state::GRANT_ALWAYS,
                },
                reply,
            ));
    });
    desk.fb.host_mut().pump_permission_prompt();
    desk.settle();
    let prompt = desk
        .fb
        .host()
        .trusted_frame_targets(&ui::theme::Theme::dark())
        .into_iter()
        .find(|(name, _)| *name == crate::shell::trusted_ui::PERMISSION_PROMPT_TARGET)
        .map(|(_, band)| band)
        .expect("the prompt is up");
    let window = desk
        .fb
        .host_mut()
        .group_at_for_tests(STRIP, 1)
        .map(|group| ui::widget::Widget::get_rect(&*group))
        .expect("the asking window is laid out");
    assert_on(
        desk.display(1),
        prompt,
        "the permission prompt",
        "the monitor",
    );
    assert!(
        prompt.y0 >= window.y0 - 8.0 && prompt.y0 < window.y0 + 120.0,
        "the prompt {prompt:?} sits at the top of the asking window {window:?}"
    );
}
