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
            app_title: None,
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
    let mut ws = Workspace::in_room(ws_id, RoomId::parse(ROOM).expect("room id"), "Desk");
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![group(ON_PANEL), group(ON_MONITOR)],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_peer_salt(SALT);
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
        recent: Vec::new(),
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

/// A file chooser is a system sheet for the window that asked, on whichever
/// display that window is: centred on that display at the sheet's own size,
/// and wholly on it.
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
            content,
            display: output,
            focused,
            ..
        } = desk.fb.host().file_picker_place()
        else {
            panic!("the chooser on {name} is a sheet on the window that asked");
        };
        assert!(focused, "the asking window is the one in use");
        assert!(
            display.contains(content.center()),
            "the sheet is for {name}'s window"
        );
        let sheet = desk.sheet();
        assert_on(display, sheet, "the file chooser", name);
        assert!(
            (sheet.center() - output.center()).hypot() < 1.0,
            "the sheet {sheet:?} centres on {name} ({output:?}), not on its window"
        );
        assert_eq!(
            sheet,
            crate::shell::file_picker_overlay::file_picker_rect(output),
            "the sheet is the system sheet's size on {name}"
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
            recent: Vec::new(),
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

/// Escape takes the chooser down as Cancel does, also when the window that
/// asked is the only one, which shows no focus accent.
#[test]
fn escape_cancels_the_file_chooser_of_a_lone_window() {
    use crate::shell::file_picker_overlay::PickerPlace;

    let folder = tempfile::tempdir().expect("tempdir");
    let mut tree = app_tree();
    tree.desktop.workspaces[0].strips[0]
        .window_groups
        .retain(|group| group.id == ON_MONITOR);
    let (mut desk, pickers) = Desk::with_queue(tree);
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_MONITOR, tab_of(ON_MONITOR));
    desk.settle();
    let mut answer = ask(&pickers, folder.path(), None);
    assert!(desk.fb.host_mut().pump_file_picker());
    desk.settle();
    assert!(
        matches!(
            desk.fb.host().file_picker_place(),
            PickerPlace::Window { focused: true, .. }
        ),
        "the lone window that asked is the one in use"
    );
    for pressed in [true, false] {
        desk.dispatch(&crate::shell::test_control::keyboard_input_event(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            pressed,
        ));
    }
    let _ = desk.fb.host_mut().pump_file_picker();
    desk.settle();
    assert!(desk.fb.host().file_picker().is_none(), "Escape closed it");
    assert_eq!(
        answer.try_recv().ok(),
        Some(None),
        "and the app hears it was cancelled"
    );
}

/// A second window on the asking window's display, so the pointer reaching
/// it does not move the display in use.
const BESIDE: GroupId = id(1005);

/// A chooser up on the focused window that asked, with [`BESIDE`] showing
/// on the same display: the desk, the request's answer, the waiting
/// window, and a point on `BESIDE` clear of the sheet and that window.
fn chooser_beside_another_window() -> (
    Desk,
    tempfile::TempDir,
    tokio::sync::oneshot::Receiver<Option<Vec<std::path::PathBuf>>>,
    Rect,
    Point,
) {
    use crate::shell::file_picker_overlay::PickerPlace;

    let folder = tempfile::tempdir().expect("tempdir");
    let mut tree = app_tree();
    let mut beside = group(BESIDE);
    beside.tabs[0].tile = TileContent::Foreign {
        app_id: "org.tilo.other".into(),
        surface_id: tab_of(BESIDE),
    };
    tree.desktop.workspaces[0].strips[0]
        .window_groups
        .push(beside);
    tree.desktop.outputs.home.insert(BESIDE, MONITOR_KEY.into());
    let (mut desk, pickers) = Desk::with_queue(tree);
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_MONITOR, tab_of(ON_MONITOR));
    desk.settle();
    let answer = ask(&pickers, folder.path(), None);
    assert!(desk.fb.host_mut().pump_file_picker());
    desk.settle();
    let PickerPlace::Window {
        window,
        display,
        focused: true,
        ..
    } = desk.fb.host().file_picker_place()
    else {
        panic!("the sheet is on the focused window that asked");
    };
    let other = desk
        .fb
        .host()
        .active_tile_rect(BESIDE)
        .expect("the other window is laid out")
        .intersect(display);
    assert!(other.area() > 0.0, "the other window shows on that display");
    let other = other.center();
    assert!(!window.contains(other), "the other window is elsewhere");
    assert!(!desk.sheet().contains(other), "and clear of the sheet");
    (desk, folder, answer, window, other)
}

/// With the window that asked focused, its sheet types, but the pointer
/// over another window is that window's, as with no chooser up: the
/// press that moves focus there reaches its app. The waiting window and
/// the sheet still take the pointer.
#[test]
fn a_click_on_another_window_reaches_it_while_the_asking_window_has_the_keys() {
    use crate::shell::input_policy::{
        chrome_captures_input, chrome_captures_pointer, chrome_captures_touch_at,
    };

    let (mut desk, _folder, _answer, window, other) = chooser_beside_another_window();
    desk.dispatch(&ui::synthetic_input::cursor_moved(other.x, other.y));
    assert!(
        chrome_captures_input(&desk.fb),
        "the keys stay with the sheet"
    );
    assert!(
        !chrome_captures_pointer(&desk.fb),
        "the pointer over another window is that window's"
    );
    assert!(
        !chrome_captures_touch_at(&desk.fb, other),
        "and so is a finger landing there"
    );

    let centre = window.center();
    desk.dispatch(&ui::synthetic_input::cursor_moved(centre.x, centre.y));
    assert!(
        chrome_captures_pointer(&desk.fb),
        "the waiting window stays closed to the pointer"
    );
    assert!(chrome_captures_touch_at(&desk.fb, desk.sheet().center()));
}

/// Input from another device follows the same decision as local input: a
/// click on another window reaches it while the chooser waits on the
/// focused window that asked, the waiting window stays closed to it, and
/// no key from that device gets past the sheet.
#[test]
fn a_click_from_another_device_reaches_another_window_while_a_chooser_waits() {
    use crate::network::input_firewall::{Denial, RemoteInputFirewall, ShellView};
    use crate::network::streaming::InputEvent;
    use crate::shell::input_policy::{foreign_tiles, input_capture};

    const PEER: crate::network::protocol::PeerId = 9;
    let (desk, _folder, _answer, _window, other) = chooser_beside_another_window();
    let tiles = {
        let chrome = desk.fb.chrome();
        let tree = chrome.tree();
        foreign_tiles(&tree, desk.fb.host())
    };
    let tile = |group: GroupId| {
        tiles
            .iter()
            .find(|hit| hit.group == group)
            .map(|hit| (hit.surface_id, hit.rect))
            .expect("laid out")
    };
    let (asking, asking_rect) = tile(ON_MONITOR);
    let (beside, beside_rect) = tile(BESIDE);
    let mut firewall = RemoteInputFirewall::new();
    firewall.publish_view(ShellView::new(
        tiles.iter().map(|hit| (hit.surface_id, hit.rect)).collect(),
        Some(asking),
        input_capture(&desk.fb),
        false,
    ));
    let now = std::time::Instant::now();

    let press = InputEvent::PointerButton {
        button: 0x110,
        pressed: true,
    };
    let at = (other.x - beside_rect.x0, other.y - beside_rect.y0);
    assert_eq!(
        firewall.review(
            PEER,
            beside,
            &InputEvent::PointerMotion { x: at.0, y: at.1 },
            now
        ),
        Ok(InputEvent::PointerMotion {
            x: other.x,
            y: other.y
        }),
        "the pointer from another device reaches the other window"
    );
    assert_eq!(
        firewall.review(PEER, beside, &press, now),
        Ok(press.clone()),
        "and so does its click"
    );

    let centre = (asking_rect.width() / 2.0, asking_rect.height() / 2.0);
    assert_eq!(
        firewall.review(
            PEER,
            asking,
            &InputEvent::PointerMotion {
                x: centre.0,
                y: centre.1
            },
            now
        ),
        Err(Denial::Occluded),
        "the waiting window stays closed to it"
    );
    assert_eq!(
        firewall.review(
            PEER,
            asking,
            &InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0
            },
            now
        ),
        Err(Denial::FocusMuted),
        "keys from another device do not get past the sheet"
    );
}

/// While another window has the keyboard, the compositor lets its keys
/// and its pointer through to that window; the chooser holds the keyboard
/// only while the window that asked is focused, and the pointer only over
/// that window and its sheet. A chooser held back every key wherever the
/// person had gone, and ignored them itself.
#[test]
fn a_file_chooser_holds_back_input_only_for_the_window_it_blocks() {
    use crate::shell::file_picker_overlay::PickerPlace;
    use crate::shell::input_policy::{chrome_captures_input, chrome_captures_touch_at};

    let folder = tempfile::tempdir().expect("tempdir");
    let (mut desk, pickers) = Desk::with_queue(app_tree());
    let _ = desk
        .fb
        .host_mut()
        .focus_window_on_strip(STRIP, ON_MONITOR, tab_of(ON_MONITOR));
    desk.settle();
    let _answer = ask(&pickers, folder.path(), None);
    assert!(desk.fb.host_mut().pump_file_picker());
    desk.settle();
    assert!(
        chrome_captures_input(&desk.fb),
        "the asking window is focused: its sheet has the keys"
    );

    let other = desk.display(0).center();
    desk.click(other);
    let PickerPlace::Window {
        window,
        focused: false,
        ..
    } = desk.fb.host().file_picker_place()
    else {
        panic!("the other window took the keyboard");
    };
    assert!(
        !chrome_captures_input(&desk.fb),
        "the focused window gets its keys while the sheet waits on another"
    );
    assert!(
        !chrome_captures_touch_at(&desk.fb, other),
        "and the pointer over it"
    );
    assert!(
        chrome_captures_touch_at(&desk.fb, window.center()),
        "the waiting window stays closed to the pointer"
    );
    assert!(
        chrome_captures_touch_at(&desk.fb, desk.sheet().center()),
        "and its sheet takes it"
    );

    desk.click(window.center());
    assert!(
        chrome_captures_input(&desk.fb),
        "a click back on the waiting window gives its sheet the keys again"
    );
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
    let centre = window.center();
    let beside_the_sheet = [
        Point::new(centre.x, f64::midpoint(sheet.y1, window.y1)),
        Point::new(centre.x, f64::midpoint(window.y0, sheet.y0)),
        Point::new(f64::midpoint(window.x0, sheet.x0), centre.y),
        Point::new(f64::midpoint(sheet.x1, window.x1), centre.y),
    ]
    .into_iter()
    .find(|at| window.contains(*at) && !sheet.contains(*at))
    .expect("the waiting window shows beside its sheet");
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

impl Desk {
    /// Work on the window on display `index`, with one frame painted.
    fn work_on(&mut self, index: usize) {
        let group = if index == 0 { ON_PANEL } else { ON_MONITOR };
        // Whether focus moved: false when that window has it already.
        let _ = self
            .fb
            .host_mut()
            .focus_window_on_strip(STRIP, group, tab_of(group));
        // Focus reaches the display partition on the next update, as it
        // does after any input.
        self.settle();
        assert_eq!(
            self.focused_display(),
            Some(self.outputs[index].id.0),
            "working on display {index}"
        );
    }

    fn paint_once(&mut self) {
        self.fb.relayout();
        let _ = self.fb.paint();
    }
}

#[test]
fn a_readout_never_jumps_displays_while_it_is_up() {
    use crate::shell::osd::Osd;
    let mut desk = Desk::new("laptop-beside-1080p");
    let (panel, monitor) = (desk.display(0), desk.display(1));
    desk.work_on(0);
    desk.fb
        .with_chrome(|chrome| chrome.show_osd(Osd::volume(0.4, false)));
    desk.paint_once();
    let pill = desk.fb.host().osd_rect().expect("the readout is painted");
    assert_on(panel, pill, "the volume readout", "the panel");

    desk.work_on(1);
    let pill = desk.fb.host().osd_rect().expect("the readout is still up");
    assert_on(panel, pill, "the readout raised on the panel", "the panel");

    desk.fb
        .with_chrome(|chrome| chrome.show_osd(Osd::volume(0.45, false)));
    desk.paint_once();
    let pill = desk.fb.host().osd_rect().expect("the readout is painted");
    assert_on(monitor, pill, "the next press's readout", "the monitor");
}

/// What the brightness keys record: which display was set to what.
#[derive(Default)]
struct BrightnessRecorder {
    calls: std::cell::RefCell<Vec<String>>,
}

impl crate::shell::quick_settings::actions::QsActions for BrightnessRecorder {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("brightness={pct:.0}"));
        Ok(())
    }
    fn set_display_brightness(&self, connector: &str, pct: f32) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("{connector}={pct:.0}"));
        Ok(())
    }
    fn set_volume(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_wifi(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_bluetooth(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
        Ok(())
    }
    fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
        Ok(())
    }
}

fn dimmable() -> Vec<crate::shell::chrome_state::DisplayBrightness> {
    vec![
        crate::shell::chrome_state::DisplayBrightness {
            connector: PANEL.0.into(),
            name: "Built-in display".into(),
            level: 0.5,
        },
        crate::shell::chrome_state::DisplayBrightness {
            connector: MONITOR.0.into(),
            name: "Test Monitor".into(),
            level: 0.3,
        },
    ]
}

#[test]
fn brightness_keys_adjust_the_focused_display() {
    use crate::shell::media_keys::{BrightnessKeys, Context, DispatcherAudio, MediaKey, Response};
    let mut desk = Desk::new("laptop-beside-1080p");
    let displays = dimmable();
    for (index, connector, expected) in [(1, MONITOR.0, "DP-1=40"), (0, PANEL.0, "eDP-1=60")] {
        desk.work_on(index);
        let focused = desk.fb.host().focused_connector();
        assert_eq!(
            focused.as_deref(),
            Some(connector),
            "display {index} is in use"
        );
        let rec = BrightnessRecorder::default();
        let (transport_osd, _mailbox) = crate::session_runtime::mailbox(std::sync::Arc::new(|| {}));
        let response = crate::shell::media_keys::apply(
            MediaKey::BrightnessUp,
            &Context {
                actions: &rec,
                audio: &DispatcherAudio(&rec),
                status: None,
                transport_osd: &transport_osd,
                airplane_mode: false,
                brightness: BrightnessKeys {
                    displays: &displays,
                    focused: focused.as_deref(),
                    all: false,
                },
            },
        );
        assert_eq!(*rec.calls.borrow(), [expected]);
        let Some(Response::Osd(osd)) = response else {
            panic!("a readout, not {response:?}");
        };
        let name = &displays
            .iter()
            .find(|d| d.connector == connector)
            .expect("dimmable")
            .name;
        assert!(
            osd.caption().ends_with(name.as_str()),
            "with two dimmable displays the readout names {name}: {}",
            osd.caption()
        );
        desk.fb.with_chrome(|chrome| chrome.show_osd(osd));
        desk.paint_once();
        let pill = desk.fb.host().osd_rect().expect("the readout is painted");
        assert_on(
            desk.display(index),
            pill,
            "the brightness readout",
            "the display in use",
        );
    }
}

#[test]
fn quick_settings_lists_the_focused_display_first() {
    use crate::shell::quick_settings::refresh::FieldReading;
    let mut desk = Desk::new("laptop-beside-1080p");
    for (index, first) in [(1, MONITOR.0), (0, PANEL.0)] {
        desk.work_on(index);
        let chrome = desk.fb.chrome_handle();
        {
            let mut st = chrome.borrow_mut();
            desk.fb
                .host()
                .apply_qs_field_reading(&mut st, FieldReading::Brightness(None, dimmable()));
            let order: Vec<&str> = st
                .quick_settings
                .displays
                .iter()
                .map(|display| display.connector.as_str())
                .collect();
            assert_eq!(
                order.first().copied(),
                Some(first),
                "display {index} first: {order:?}"
            );
            assert_eq!(order.len(), 2, "one slider per dimmable display");
        }
    }
}

#[test]
fn machine_wide_banners_show_on_every_display_and_a_call_edges_the_others() {
    use tilo_desktop::notifications::{Addressed, Notification};
    let mut desk = Desk::new("laptop-beside-1080p");
    let (panel, monitor) = (desk.display(0), desk.display(1));
    desk.work_on(0);
    desk.fb.with_chrome(|chrome| {
        chrome.push_notification_full(Notification::new("Build finished", "main is green"));
    });
    desk.paint_once();
    assert!(
        desk.fb.host().machine_banner_rects().is_empty(),
        "an ordinary banner stays on the display in use"
    );
    desk.fb.with_chrome(|chrome| {
        chrome.push_notification_full(
            Notification::new("Battery critical", "Plug in now").machine_wide(),
        );
    });
    desk.paint_once();
    let banners = desk.fb.host().machine_banner_rects().to_vec();
    assert_eq!(banners.len(), 1, "repeated once, on the other display");
    assert_on(monitor, banners[0], "the critical banner", "the monitor");
    assert!(desk.fb.host().call_edge_rects().is_empty(), "no call rings");

    desk.fb.with_chrome(|chrome| {
        chrome.push_notification_full(
            Notification::new("Jonas is calling", "")
                .with_from("Jonas", "jonas")
                .with_addressed(Addressed::IncomingCall),
        );
    });
    desk.work_on(1);
    assert_eq!(
        desk.fb.host().call_edge_rects(),
        [panel],
        "the display not in use gets the quiet edge"
    );
    let banners = desk.fb.host().machine_banner_rects().to_vec();
    assert_eq!(banners.len(), 1);
    assert_on(panel, banners[0], "the critical banner", "the panel");
}

/// Someone already in the workspace, for the arrival line.
fn ada() -> crate::shell::arrival_line::Present {
    crate::shell::arrival_line::Present {
        name: "Ada".into(),
        id: "ada@peer".into(),
        colour: None,
        is_agent: false,
    }
}

/// The arrival line (the workspace's name and the faces of who is here)
/// goes up under the top bar of the display in use, centred on it and
/// wholly on it: never centred on the whole desktop, where it straddled the
/// seam between the panel and the monitor.
#[test]
fn the_arrival_line_sits_wholly_on_the_display_in_use() {
    let mut desk = Desk::new("laptop-beside-1080p");
    desk.on_each_display(|desk, display, name| {
        desk.fb
            .host_mut()
            .arrival_line
            .show("Desk", vec![ada()], std::time::Instant::now());
        desk.paint_once();
        let line = desk.fb.host().arrival_line.rect();
        assert_on(display, line, "the arrival line", name);
        assert!(
            (line.center().x - display.center().x).abs() < 1.0,
            "the arrival line {line:?} centres on {name} {display:?}"
        );
        desk.fb.host_mut().arrival_line.dismiss();
        desk.paint_once();
    });
}

/// Quick Settings opening over the arrival line takes the line away: the
/// line says what the top bar says, so nothing of it stays beside or under
/// the panel, and a press where its "Change" was goes to the panel. The
/// line belongs to the desktop sections, beneath every overlay: nothing of
/// it is painted in the sections the overlays are drawn into.
#[test]
fn overlays_stack_above_the_arrival_line_and_take_it_away() {
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::kurbo::Affine;

    let mut desk = Desk::new("laptop-beside-1080p");
    desk.work_on(0);
    let sections = |desk: &mut Desk| {
        desk.fb.host_mut().draw_scene_sections(
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        )
    };
    let bare = sections(&mut desk);
    let again = sections(&mut desk);
    assert!(
        bare.above_quick_settings
            .encoding_eq(&again.above_quick_settings),
        "the fixture paints the overlay sections the same twice"
    );

    let host = desk.fb.host_mut();
    host.arrival_line
        .show("Desk", vec![ada()], std::time::Instant::now());
    host.arrival_line
        .tell("People here see that you are here".into());
    host.arrival_line.settle_for_test();
    let with_line = sections(&mut desk);
    let line = desk.fb.host().arrival_line.rect();
    assert!(!line.is_zero_area(), "the arrival line is up");
    assert!(
        !(bare.workspace.encoding_eq(&with_line.workspace)
            && bare
                .workspace_overlays
                .encoding_eq(&with_line.workspace_overlays)),
        "the arrival line is painted with the desktop"
    );
    assert!(
        bare.above_quick_settings
            .encoding_eq(&with_line.above_quick_settings),
        "nothing of the arrival line is painted in the overlay sections"
    );

    // Its "Change", which takes a press while nothing covers it.
    let change = (0..=40)
        .flat_map(|i| (0..=40).map(move |j| (i, j)))
        .map(|(i, j)| {
            Point::new(
                line.x0 + line.width() * f64::from(i) / 40.0,
                line.y0 + line.height() * f64::from(j) / 40.0,
            )
        })
        .find(|&p| desk.fb.host().arrival_line.change_at(p))
        .expect("the line offers Change");

    desk.fb
        .with_chrome(|chrome| chrome.overlays.quick_settings = true);
    desk.fb.host_mut().force_quick_settings_settled();
    // Before any frame: the press is the panel's, not the line's.
    assert!(
        !desk.fb.host_mut().press_presence_notice(change),
        "a press where Change was belongs to the open overlay"
    );
    assert!(
        !desk.fb.host().arrival_line.is_open(),
        "Quick Settings opening takes the arrival line away"
    );
    desk.paint_once();
    assert_eq!(desk.fb.host().arrival_line.rect(), Rect::ZERO);
    assert!(!desk.fb.host().presence_notice_captures_pointer());
}
