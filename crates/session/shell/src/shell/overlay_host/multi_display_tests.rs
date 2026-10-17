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
        let mut fb = FrameBuilder::with_tree(tree(), combined_geometry(&outputs));
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

/// A file chooser an app asks for opens over the display in use, centred
/// there rather than on the seam.
#[test]
fn the_file_chooser_opens_on_the_focused_display() {
    use crate::portal::FilePickerSpawner as _;

    let folder = tempfile::tempdir().expect("tempdir");
    let mut desk = Desk::new("laptop-beside-1080p");
    let pickers = crate::portal::FilePickerQueue::new();
    desk.fb.host_mut().set_portal_queues(
        crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoAllow),
        pickers.clone(),
    );
    let mut parked = Vec::new();
    desk.on_each_display(|desk, display, name| {
        let (reply, answer) = tokio::sync::oneshot::channel();
        parked.push(answer);
        pickers.spawn(crate::portal::FilePickerRequest {
            caller: crate::portal::PortalCaller::new(WorkspaceId(7), "org.tilo.audit"),
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
        let picker =
            ui::widget::Widget::get_rect(desk.fb.host().file_picker().expect("the chooser is up"));
        assert_on(display, picker, "the file chooser", name);
        assert!(
            (picker.center().x - display.center().x).abs() < 1.0,
            "the chooser {picker:?} centres on {name}"
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
