//! Drag-and-drop between an X11 program of a kept launch and a Wayland app, through the keeper and
//! a real headless desktop. The X11 side is a small client that speaks XDND as GTK and Qt do: as
//! the source it grabs with its press, looks for the topmost `XdndAware` window under the pointer
//! and drops when the button comes up; as the target it answers positions and reads the drop.

use std::time::{Duration, Instant};

use tilo_compositor::{
    Compositor, InputEvent, LayoutPoint, LayoutRect, OutputConfig, WindowInfo, WindowPolicy,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    MapState, PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Window,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::support::App;
use crate::x11::{X, servers};

const BLUE: u32 = 0x0000_00ff;
const GREEN: [u8; 4] = [0, 255, 0, 255];
const TEXT: &str = "text/plain;charset=utf-8";
const FILES: &str = "text/uri-list";
const BUTTON: u32 = 0x110;

/// The X11 client's side of a drag it started.
#[derive(Default)]
struct Source {
    window: Window,
    /// The formats offered and their bytes.
    data: Vec<(String, Vec<u8>)>,
    target: Option<Window>,
    accepted: bool,
    pressed: bool,
    released: bool,
    dropped: bool,
    left: bool,
    finished: Option<bool>,
}

/// The X11 client's side of a drag dropped on it.
#[derive(Default)]
struct Target {
    window: Window,
    /// The format it reads a drop as.
    wanted: String,
    source: Option<Window>,
    types: Vec<String>,
    positions: usize,
    dropped: bool,
    data: Option<Vec<u8>>,
}

/// An X11 program and a Wayland app of the same launch, both on the desktop.
struct Scene {
    x: X,
    app: App,
    source: Source,
    target: Target,
}

fn inside((x, y, w, h): (i32, i32, i32, i32), (px, py): (i32, i32)) -> bool {
    px >= x && py >= y && px < x + w && py < y + h
}

/// A point of `rect` that is not in `other`.
fn point_of(rect: (i32, i32, i32, i32), other: (i32, i32, i32, i32)) -> (i32, i32) {
    let (x, y, w, h) = rect;
    (0..8)
        .flat_map(|i| (0..8).map(move |j| (x + w * (2 * i + 1) / 16, y + h * (2 * j + 1) / 16)))
        .find(|point| !inside(other, *point))
        .expect("the windows do not cover each other entirely")
}

/// The app's window on the right, the X11 program's on the left, apart.
struct Apart;

impl WindowPolicy for Apart {
    fn initial_size(&mut self, _: &WindowInfo, _: &[LayoutRect]) -> Option<(i32, i32)> {
        None
    }

    fn place(&mut self, window: &WindowInfo, _: (i32, i32), _: &[LayoutRect]) -> LayoutPoint {
        if window.app_id.as_deref() == Some("wayland-app") {
            LayoutPoint::from((360, 200))
        } else {
            LayoutPoint::from((20, 20))
        }
    }
}

fn desktop() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(Apart)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor
}

impl Scene {
    fn start() -> Self {
        let mut x = X::start();
        x.rig.drop_desktop();
        x.rig.attach_desktop(desktop());
        let mut app = App::connect(&mut x.rig);
        app.open_window(&mut x.rig, "Wayland", Some("wayland-app"));
        app.show(&mut x.rig, (240, 180), GREEN);
        Self {
            x,
            app,
            source: Source::default(),
            target: Target::default(),
        }
    }

    fn rect(&mut self, class: &str) -> (i32, i32, i32, i32) {
        let mut found = None;
        self.pump(|scene| {
            found = scene
                .x
                .rig
                .windows()
                .into_iter()
                .find(|(app_id, _, _)| app_id.as_deref() == Some(class))
                .map(|(_, _, rect)| rect);
            found.is_some()
        });
        found.expect("shown")
    }

    /// Turn the desktop, the keeper, the app and the X client until `done`.
    fn pump(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            self.x.x11.flush().expect("flush");
            for _ in 0..3 {
                self.x.rig.settle(&mut [&mut self.app]);
            }
            while let Some(event) = self.x.x11.poll_for_event().expect("event") {
                self.event(&event);
            }
            if done(self) {
                return;
            }
        }
        panic!(
            "never got there: source pressed={} target={:?} accepted={} released={} target types={:?}",
            self.source.pressed,
            self.source.target,
            self.source.accepted,
            self.source.released,
            self.target.types
        );
    }

    fn motion(&mut self, (x, y): (i32, i32)) {
        self.x
            .rig
            .desktop()
            .input(InputEvent::PointerMotion {
                x: f64::from(x),
                y: f64::from(y),
            })
            .expect("motion");
        self.pump(|_| true);
    }

    fn button(&mut self, pressed: bool) {
        self.x
            .rig
            .desktop()
            .input(InputEvent::PointerButton {
                button: BUTTON,
                pressed,
            })
            .expect("button");
    }

    /// The app accepts the drag over it and reads it as `mime`, as a Wayland toolkit does.
    fn receive_drag(&mut self, mime: &str) -> Vec<u8> {
        use wayland_client::protocol::wl_data_device_manager::DndAction;
        let (reader, writer) = std::io::pipe().expect("transfer pipe");
        let offer = self.app.heard.drag_offer.clone().expect("a drag offer");
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        offer.accept(self.app.heard.enter_serial, Some(mime.to_owned()));
        self.app.heard.drag_accepted = true;
        offer.receive(mime.to_owned(), std::os::fd::AsFd::as_fd(&writer));
        drop(writer);
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(None));
        let written = std::sync::Arc::clone(&bytes);
        std::thread::spawn(move || {
            let mut read = Vec::new();
            let mut reader = reader;
            let _ = std::io::Read::read_to_end(&mut reader, &mut read);
            *written.lock().unwrap() = Some(read);
        });
        self.pump(|_| bytes.lock().unwrap().is_some());
        bytes.lock().unwrap().take().unwrap()
    }

    /// Keep the pointer moving a little about `at`, as a hand does, until `done`.
    fn wiggle_until(&mut self, at: (i32, i32), mut done: impl FnMut(&mut Self) -> bool) {
        for turn in 0..200 {
            if done(self) {
                return;
            }
            self.motion((at.0 + turn % 2, at.1));
        }
        self.pump(done);
    }

    /// Move the desktop's pointer from `from` to `to` in steps, as a hand does.
    fn sweep(&mut self, from: (i32, i32), to: (i32, i32)) {
        for step in 1..=8 {
            self.motion((
                from.0 + (to.0 - from.0) * step / 8,
                from.1 + (to.1 - from.1) * step / 8,
            ));
        }
    }

    fn send(&self, to: Window, kind: &str, words: [u32; 5]) {
        let event = ClientMessageEvent::new(32, to, self.x.atom(kind), words);
        self.x
            .x11
            .send_event(false, to, EventMask::NO_EVENT, event)
            .expect("send");
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::MotionNotify(motion) if motion.event == self.source.window => {
                self.source_moved(motion.root_x, motion.root_y, motion.time);
            }
            Event::ButtonPress(press) if press.event == self.source.window => {
                self.source.pressed = true;
            }
            Event::ButtonRelease(release) if release.event == self.source.window => {
                self.source.released = true;
                if let Some(target) = self.source.target {
                    if self.source.accepted {
                        self.source.dropped = true;
                        self.send(
                            target,
                            "XdndDrop",
                            [self.source.window, 0, release.time, 0, 0],
                        );
                    } else {
                        self.source.left = true;
                        self.send(target, "XdndLeave", [self.source.window, 0, 0, 0, 0]);
                    }
                }
            }
            Event::SelectionRequest(request) if request.owner == self.source.window => {
                self.serve(request);
            }
            Event::SelectionNotify(notify) if notify.requestor == self.target.window => {
                self.target_read(notify.property);
            }
            Event::ClientMessage(message) => self.message(message),
            _ => {}
        }
    }

    fn message(&mut self, message: &ClientMessageEvent) {
        let words = message.data.as_data32();
        let kind = message.type_;
        if message.window == self.source.window {
            if kind == self.x.atom("XdndStatus") {
                self.source.accepted = words[1] & 1 != 0;
            } else if kind == self.x.atom("XdndFinished") {
                self.source.finished = Some(words[1] & 1 != 0);
            }
            return;
        }
        if message.window != self.target.window {
            return;
        }
        if kind == self.x.atom("XdndEnter") {
            self.target.source = Some(words[0]);
            let atoms: Vec<u32> = if words[1] & 1 != 0 {
                self.x
                    .x11
                    .get_property(
                        false,
                        words[0],
                        self.x.atom("XdndTypeList"),
                        AtomEnum::ATOM,
                        0,
                        64,
                    )
                    .expect("types")
                    .reply()
                    .expect("types")
                    .value32()
                    .expect("atoms")
                    .collect()
            } else {
                words[2..]
                    .iter()
                    .copied()
                    .filter(|atom| *atom != 0)
                    .collect()
            };
            self.target.types = atoms
                .into_iter()
                .map(|atom| {
                    let name = self
                        .x
                        .x11
                        .get_atom_name(atom)
                        .expect("name")
                        .reply()
                        .expect("name");
                    String::from_utf8(name.name).expect("utf-8")
                })
                .collect();
        } else if kind == self.x.atom("XdndPosition") {
            self.target.positions += 1;
            let accept = self.target.types.contains(&self.target.wanted);
            let action = if accept {
                self.x.atom("XdndActionCopy")
            } else {
                0
            };
            self.send(
                words[0],
                "XdndStatus",
                [self.target.window, u32::from(accept) | 2, 0, 0, action],
            );
        } else if kind == self.x.atom("XdndDrop") {
            self.target.dropped = true;
            self.x
                .x11
                .convert_selection(
                    self.target.window,
                    self.x.atom("XdndSelection"),
                    self.x.atom(&self.target.wanted),
                    self.x.atom("DROPPED"),
                    words[2],
                )
                .expect("convert");
        }
    }

    fn target_read(&mut self, property: u32) {
        let data = if property == x11rb::NONE {
            Vec::new()
        } else {
            self.x
                .x11
                .get_property(
                    true,
                    self.target.window,
                    property,
                    AtomEnum::ANY,
                    0,
                    1 << 20,
                )
                .expect("data")
                .reply()
                .expect("data")
                .value
        };
        self.target.data = Some(data);
        if let Some(source) = self.target.source {
            self.send(
                source,
                "XdndFinished",
                [self.target.window, 1, self.x.atom("XdndActionCopy"), 0, 0],
            );
        }
    }

    /// The topmost viewable window under the root position, if it takes XDND drops.
    fn aware_at(&self, x: i16, y: i16) -> Option<Window> {
        let tree = self.x.x11.query_tree(self.x.root).ok()?.reply().ok()?;
        for window in tree.children.iter().rev().copied() {
            let Some(attributes) = self
                .x
                .x11
                .get_window_attributes(window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            else {
                continue;
            };
            if attributes.map_state != MapState::VIEWABLE {
                continue;
            }
            let Some(geometry) = self
                .x
                .x11
                .get_geometry(window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            else {
                continue;
            };
            let contains = x >= geometry.x
                && y >= geometry.y
                && i32::from(x) < i32::from(geometry.x) + i32::from(geometry.width)
                && i32::from(y) < i32::from(geometry.y) + i32::from(geometry.height);
            if !contains {
                continue;
            }
            let aware = self
                .x
                .x11
                .get_property(
                    false,
                    window,
                    self.x.atom("XdndAware"),
                    AtomEnum::ATOM,
                    0,
                    1,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.value_len > 0);
            return aware.then_some(window);
        }
        None
    }

    fn source_moved(&mut self, x: i16, y: i16, time: u32) {
        if self.source.released {
            return;
        }
        let target = self.aware_at(x, y);
        if target != self.source.target {
            if let Some(old) = self.source.target {
                self.send(old, "XdndLeave", [self.source.window, 0, 0, 0, 0]);
            }
            self.source.accepted = false;
            if let Some(new) = target {
                let types: Vec<u32> = self
                    .source
                    .data
                    .iter()
                    .map(|(name, _)| self.x.atom(name))
                    .collect();
                let mut words = [self.source.window, 5 << 24, 0, 0, 0];
                for (word, atom) in words.iter_mut().skip(2).zip(&types) {
                    *word = *atom;
                }
                self.send(new, "XdndEnter", words);
            }
            self.source.target = target;
        }
        if let Some(target) = target {
            #[allow(
                clippy::cast_sign_loss,
                reason = "the halves carry the coordinates' bits"
            )]
            let point = (u32::from(x as u16) << 16) | u32::from(y as u16);
            self.send(
                target,
                "XdndPosition",
                [
                    self.source.window,
                    0,
                    point,
                    time,
                    self.x.atom("XdndActionCopy"),
                ],
            );
        }
    }

    fn serve(&self, request: &SelectionRequestEvent) {
        let found = self
            .source
            .data
            .iter()
            .find(|(name, _)| self.x.atom(name) == request.target);
        if let Some((_, bytes)) = found {
            self.x
                .x11
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    request.target,
                    bytes,
                )
                .expect("data");
        }
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if found.is_some() {
                request.property
            } else {
                x11rb::NONE
            },
        };
        self.x
            .x11
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)
            .expect("notify");
    }

    /// A window of the X11 program that drags `data` out once pressed.
    fn x11_source(&mut self, class: &str, data: &[(&str, &[u8])]) -> (i32, i32, i32, i32) {
        let window = self.x.window(Some(class), (0, 0), (160, 120), BLUE);
        self.x
            .x11
            .change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                ),
            )
            .expect("events");
        self.source = Source {
            window,
            data: data
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect(),
            ..Source::default()
        };
        self.rect(class)
    }

    /// Press on the X11 window at `at` and drag out to `to`, over the app's window.
    fn drag_out(&mut self, at: (i32, i32), to: (i32, i32)) {
        self.motion(at);
        self.button(true);
        let window = self.source.window;
        self.pump(|scene| scene.source.pressed);
        // The press makes the X11 program a drag source, as a toolkit does past its threshold.
        self.x
            .x11
            .set_selection_owner(window, self.x.atom("XdndSelection"), x11rb::CURRENT_TIME)
            .expect("owner");
        self.pump(|_| true);
        self.sweep(at, to);
    }
}

/// An X11 drag of `data` out onto the Wayland app, read there as `mime`.
fn drag_out_and_drop(
    scene: &mut Scene,
    class: &str,
    data: &[(&str, &[u8])],
    mime: &str,
    expected: &[u8],
) {
    let x11 = scene.x11_source(class, data);
    drag_out_and_drop_from(scene, x11, mime, expected);
}

/// The X11 source's drag out of its window `x11` onto the Wayland app, read there as `mime`.
fn drag_out_and_drop_from(
    scene: &mut Scene,
    x11: (i32, i32, i32, i32),
    mime: &str,
    expected: &[u8],
) {
    scene.app.heard.drag_entered = false;
    scene.app.heard.drag_accepted = false;
    scene.source = Source {
        window: scene.source.window,
        data: std::mem::take(&mut scene.source.data),
        ..Source::default()
    };
    let wayland = scene.rect("wayland-app");
    let from = point_of(x11, wayland);
    let to = point_of(wayland, x11);
    scene.drag_out(from, to);
    scene.wiggle_until(to, |scene| scene.app.heard.drag_entered);
    let read = scene.receive_drag(mime);
    assert_eq!(read, expected, "the app reads the X11 drag as {mime}");
    scene.pump(|scene| scene.source.accepted);
    scene.button(false);
    scene.pump(|scene| scene.source.finished.is_some());
    assert!(
        scene.source.dropped,
        "the X11 source dropped when the desktop's button came up"
    );
    assert_eq!(
        scene.source.finished,
        Some(true),
        "and heard the drop succeeded"
    );
}

tilo_test_env::scenario! {
    fn an_x11_program_of_a_kept_launch_drags_and_drops_with_wayland_apps;

    phase text_and_files_drag_out_of_the_x11_program;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut scene = Scene::start();
        drag_out_and_drop(&mut scene, "Dragging-Text", &[("UTF8_STRING", b"dragged in X11")], TEXT, b"dragged in X11");
        let files = b"file:///home/someone/a.txt\r\n";
        drag_out_and_drop(&mut scene, "Dragging-Files", &[(FILES, files), ("UTF8_STRING", b"a.txt")], FILES, files);
    }

    phase text_and_files_drop_into_the_x11_program;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        for (offered, wanted, bytes) in [
            (TEXT, "UTF8_STRING", &b"dragged from Wayland"[..]),
            (FILES, FILES, &b"file:///home/someone/b.txt\r\n"[..]),
        ] {
            let mut scene = Scene::start();
            let window = scene.x.window(Some("Dropping-X11"), (0, 0), (160, 120), BLUE);
            scene
                .x
                .x11
                .change_property32(PropMode::REPLACE, window, scene.x.atom("XdndAware"), AtomEnum::ATOM, &[5])
                .expect("aware");
            scene.target = Target {
                window,
                wanted: wanted.to_owned(),
                ..Target::default()
            };
            let x11 = scene.rect("Dropping-X11");
            let wayland = scene.rect("wayland-app");
            let from = point_of(wayland, x11);
            let to = point_of(x11, wayland);
            scene.motion(from);
            scene.button(true);
            scene.pump(|scene| scene.app.heard.buttons.iter().any(|(_, button, pressed)| *button == BUTTON && *pressed));
            scene.app.start_drag(offered, bytes);
            scene.sweep(from, to);
            scene.wiggle_until(to, |scene| scene.target.positions > 0);
            assert!(
                scene.target.types.iter().any(|name| name == wanted),
                "the X11 window is offered {wanted}: {:?}",
                scene.target.types
            );
            scene.motion((to.0 + 1, to.1 + 1));
            scene.button(false);
            scene.pump(|scene| scene.target.data.is_some());
            assert_eq!(scene.target.data.as_deref(), Some(bytes), "the X11 window reads the drop as {wanted}");
            scene.pump(|scene| scene.app.heard.dnd_finished);
        }
    }

    phase a_drag_open_when_the_desktop_goes_lets_the_x11_program_go;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut scene = Scene::start();
        let x11 = scene.x11_source("Dragging-X11", &[("UTF8_STRING", b"dragged twice")]);
        let wayland = scene.rect("wayland-app");
        let from = point_of(x11, wayland);
        let to = point_of(wayland, x11);
        scene.drag_out(from, to);
        scene.wiggle_until(to, |scene| scene.app.heard.drag_entered);
        scene.x.rig.drop_desktop();
        // A drag the desktop carries holds a handle that keeps the in-process compositor's sockets
        // open, so the session's own detach tells the keeper, as it does before a restart.
        scene.x.rig.keeper.detach();
        scene.pump(|scene| scene.source.released);
        assert!(!scene.source.dropped, "nothing was dropped");

        // The X11 program is out of its grab: the next drag goes as the first did.
        scene.x.rig.attach_desktop(desktop());
        let x11 = scene.rect("Dragging-X11");
        drag_out_and_drop_from(&mut scene, x11, TEXT, b"dragged twice");
    }
}
