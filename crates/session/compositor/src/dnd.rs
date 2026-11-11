//! Drag-and-drop between Wayland clients, X11 clients and hosted surface
//! apps, files (`text/uri-list`) included.
//!
//! | from \ to | Wayland | X11 | hosted |
//! |---|---|---|---|
//! | Wayland | smithay's data device | XDND, the compositor as source | read from the client's source |
//! | X11 | a compositor drag, data read through `XdndSelection` | XDND among X11 clients | read through `XdndSelection` |
//! | hosted | a compositor drag of the app's bytes | XDND, the compositor as source | the app's bytes |
//!
//! While a drag runs under a pointer grab (a client's drag or one the
//! compositor started), pointer motion first finds what is under the
//! pointer: a Wayland surface gets the drag from smithay's data device, a
//! hosted surface is asked whether it takes the formats
//! ([`SurfaceHost::drop_offered`]), an X11 window is spoken to in XDND
//! through [`XdndBridge`]. An X11 client's drag runs inside Xwayland until
//! it leaves the X11 windows; then the bridge's proxy window takes it (see
//! [`crate::xdnd_bridge`]).
//!
//! A Wayland source is told its drag was cancelled as the button comes up
//! over anything that is not a Wayland client (smithay validates drops
//! only through its own offers), so the data a hosted surface or an X11
//! window gets is asked for before the button reaches the grab: the
//! request goes out first, and sources answer requests in order.
//!
//! A drag follows the pointer or, when it started with a touch, that
//! finger; its icon (the client's icon surface, or the image a hosted app
//! gave) is drawn under it, in front of everything but the idle dim.
//!
//! [`SurfaceHost::drop_offered`]: tilo_session_core::surface::SurfaceHost::drop_offered

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportDma, ImportMem, Renderer};
use smithay::input::Seat;
use smithay::input::pointer::{ButtonEvent, GrabStartData, MotionEvent};
use smithay::input::touch::{
    GrabStartData as TouchGrabStartData, MotionEvent as TouchMotionEvent, UpEvent,
};
use smithay::output::Output;
use smithay::reexports::calloop::channel::{self, Sender};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER, Scale, Serial};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, ServerDndGrabHandler, SourceMetadata, start_dnd, with_source_metadata,
};
use smithay::xwayland::X11Surface;
use smithay::xwayland::xwm::XwmId;

use crate::render::OutputRenderElement;
use crate::selection::{MAX_SELECTION_BYTES, host_offer, normalized_mimes, serve, value_for};
use crate::state::State;
use crate::xdnd::{self, Action};
use crate::xdnd_bridge::{BridgeEvent, Request, XdndBridge};

/// The left button, which starts a hosted app's drag.
const BTN_LEFT: u32 = 0x110;

/// Drag-and-drop state.
#[derive(Default)]
pub(crate) struct Dnd {
    /// The drag the pointer is carrying, while a grab runs it.
    drag: Option<Drag>,
    /// What a compositor-started drag serves Wayland clients, kept past the drop until the client
    /// finished reading.
    served: Option<Served>,
    /// An X11 client's drag that reached the proxy.
    incoming: Option<Incoming>,
    /// The data our drag serves the X11 window it was dropped on.
    x_served: Option<XServed>,
    /// The XDND connections, one per workspace display with an X11 window to drop on.
    bridged: Vec<Bridged>,
    /// Bytes read off the loop, for the loop.
    deliveries: Option<Sender<Delivery>>,
    /// The drag grab of an X11 client's drag ended: Xwayland must see the button come up (done once
    /// the pointer call returns).
    release_x11_source: bool,
    /// What the drag shows under the pointer or finger.
    icon: Option<DragIcon>,
    /// A Wayland source whose drop a hosted surface or an X11 window took: told `dnd_finished` once
    /// the target has the data.
    finishing: Option<WlDataSource>,
    /// Where the drag is, in the logical layout.
    at: Point<f64, Logical>,
}

/// A drag's icon.
pub(crate) enum DragIcon {
    /// A client's icon surface, offset from the drag's point by what its commits moved it
    /// (`wl_surface.offset`).
    Surface {
        surface: WlSurface,
        offset: Point<i32, Logical>,
    },
    /// A hosted app's image, its hotspot (logical) on the drag's point.
    Image {
        frame: tilo_session_core::surface::HostFrame,
        hotspot: Point<i32, Logical>,
    },
}

/// A drag the pointer carries under a grab.
struct Drag {
    source: Source,
    mimes: Vec<String>,
    actions: DndAction,
    target: Target,
    /// How the drop went: set by our own drop (hosted and X11 targets) or by smithay's grab
    /// (Wayland targets).
    outcome: Option<bool>,
    /// The action a hosted or X11 target takes it with: the source's preferred one, or what an X11
    /// window accepted (a move deletes the item at the source once the target has it).
    chosen: Action,
}

/// Where a dragged item comes from.
enum Source {
    /// A Wayland client's data source.
    Client(WlDataSource),
    /// A hosted app's bytes; `origin` is the app.
    Host {
        values: Arc<BTreeMap<String, Vec<u8>>>,
        origin: usize,
    },
    /// An X11 client's drag ([`Dnd::incoming`]).
    X11,
}

/// What the drag is over.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Nothing,
    /// A Wayland surface: smithay's grab offers it the drag.
    Client,
    /// A hosted surface, and the format it takes there.
    Host {
        index: usize,
        mime: Option<String>,
    },
    /// An X11 window of `xwm` 's display; `aware` when it takes XDND drops.
    X11 {
        xwm: XwmId,
        window: u32,
        aware: bool,
    },
}

impl Target {
    /// Whether the drag stays on the same target over `under` .
    fn is(&self, under: &Under) -> bool {
        match (self, under) {
            (Self::Nothing, Under::Nothing) | (Self::Client, Under::Client(..)) => true,
            (Self::Host { index, .. }, Under::Host(new)) => index == new,
            (
                Self::X11 { xwm, window, .. },
                Under::X11 {
                    xwm: under_xwm,
                    window: under_window,
                },
            ) => xwm == under_xwm && window == under_window,
            _ => false,
        }
    }
}

/// What is under a point, for a drag.
enum Under {
    Nothing,
    Client(WlSurface, Point<f64, Logical>),
    Host(usize),
    X11 { xwm: XwmId, window: u32 },
}

/// What a compositor-started Wayland drag serves.
enum Served {
    Host(Arc<BTreeMap<String, Vec<u8>>>),
    /// The X11 drag's types, read through the bridge of `xwm` 's display.
    X11 {
        xwm: XwmId,
        types: Vec<String>,
    },
}

/// An X11 client's drag over the proxy.
struct Incoming {
    /// The display the drag came from.
    xwm: XwmId,
    types: Vec<String>,
    mimes: Vec<String>,
    /// The hosted surface it is over while Xwayland still runs the drag.
    host: Option<(usize, Option<String>)>,
    /// A compositor drag took it over for Wayland clients.
    grabbed: bool,
    /// How the Wayland (or hosted) side took the drop.
    outcome: Option<bool>,
    /// The X11 source dropped it on the proxy.
    dropped: bool,
}

/// Data our XDND drag serves an X11 window.
struct XServed {
    /// The display of the window it is dropped on.
    xwm: XwmId,
    values: BTreeMap<String, Vec<u8>>,
    /// Formats still being read from the Wayland source.
    loading: Vec<String>,
    /// Requests for formats still loading.
    waiting: Vec<Request>,
}

/// The XDND connection to one workspace's display.
struct Bridged {
    xwm: XwmId,
    bridge: XdndBridge,
    /// The proxy window, as the window manager sees it.
    proxy: Option<X11Surface>,
}

impl Dnd {
    /// The XDND connection of `xwm` 's display.
    fn bridged(&self, xwm: XwmId) -> Option<&Bridged> {
        self.bridged.iter().find(|bridged| bridged.xwm == xwm)
    }

    fn bridged_mut(&mut self, xwm: XwmId) -> Option<&mut Bridged> {
        self.bridged.iter_mut().find(|bridged| bridged.xwm == xwm)
    }
}

/// Bytes read off the loop.
enum Delivery {
    /// For a hosted surface's drop; `finish_x11` when an X11 source waits.
    Host {
        index: usize,
        mime: String,
        bytes: Vec<u8>,
        finish_x11: bool,
    },
    /// Read ahead for an X11 drop target.
    ReadAhead { mime: String, bytes: Vec<u8> },
}

impl State {
    /// Start a hosted surface app's drag of `offer` (bytes per format),
    /// showing `icon` under it, while a finger holds the app (the drag
    /// follows that finger) or else while the pointer is pressed on it;
    /// false when there is nothing to drag or no hosted surface there.
    pub(crate) fn start_host_drag(
        &mut self,
        offer: Vec<(String, Vec<u8>)>,
        icon: Option<(tilo_session_core::surface::HostFrame, Point<i32, Logical>)>,
    ) -> bool {
        let Some(values) = host_offer(offer) else {
            return false;
        };
        let touch = self.touch.take_host_finger();
        let origin = if let Some((_, index, _)) = touch {
            index
        } else {
            let Some(origin) = self.host_under(self.pointer) else {
                return false;
            };
            if self
                .seat
                .get_pointer()
                .is_none_or(|pointer| pointer.is_grabbed())
            {
                return false;
            }
            origin
        };
        let values = Arc::new(values);
        let mimes = normalized_mimes(values.keys().cloned());
        self.dnd.served = Some(Served::Host(Arc::clone(&values)));
        self.dnd.drag = Some(Drag {
            source: Source::Host { values, origin },
            mimes: mimes.clone(),
            actions: DndAction::Copy,
            target: Target::Nothing,
            outcome: None,
            chosen: Action::Copy,
        });
        self.dnd.icon = icon.map(|(frame, hotspot)| DragIcon::Image { frame, hotspot });
        let metadata = SourceMetadata {
            mime_types: mimes,
            dnd_action: DndAction::Copy,
        };
        let display = self.display.clone();
        let seat = self.seat.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        if let Some((slot, _, at)) = touch {
            let start = TouchGrabStartData {
                focus: None,
                slot: Some(slot).into(),
                location: at,
            };
            start_dnd(&display, &seat, self, serial, None, Some(start), metadata);
            self.drag_touch_motion(slot, at, time);
        } else {
            let start = GrabStartData {
                focus: None,
                button: BTN_LEFT,
                location: self.pointer,
            };
            start_dnd(&display, &seat, self, serial, Some(start), None, metadata);
            self.drag_motion(self.pointer, SERIAL_COUNTER.next_serial(), time);
        }
        true
    }

    /// The finger that carries the drag, when a touch carries it.
    pub(crate) fn drag_finger(&self) -> Option<u32> {
        self.dnd.drag.as_ref()?;
        let touch = self.seat.get_touch()?;
        let start = touch.grab_start_data()?;
        u32::try_from(i32::from(start.slot)).ok()
    }

    /// The finger carrying a drag moved: find the target and move the grab.
    pub(crate) fn drag_touch_motion(&mut self, slot: u32, at: Point<f64, Logical>, time: u32) {
        let Some(drag) = &self.dnd.drag else {
            return;
        };
        self.dnd.at = at;
        let under = self.drag_under(at, matches!(drag.source, Source::X11));
        self.retarget(&under, at);
        let focus = match under {
            Under::Client(surface, origin) => Some((surface, origin)),
            _ => None,
        };
        if let Some(touch) = self.seat.get_touch() {
            touch.motion(
                self,
                focus,
                &TouchMotionEvent {
                    slot: Some(slot).into(),
                    location: at,
                    time,
                },
            );
            touch.frame(self);
        }
        self.x11_dnd_events();
    }

    /// The finger carrying a drag lifted: drop on the target.
    pub(crate) fn drag_touch_up(&mut self, slot: u32, serial: Serial, time: u32) {
        self.drop_on_target();
        if let Some(touch) = self.seat.get_touch() {
            touch.up(
                self,
                &UpEvent {
                    slot: Some(slot).into(),
                    serial,
                    time,
                },
            );
        }
        let ended = self
            .seat
            .get_touch()
            .is_none_or(|touch| !touch.is_grabbed());
        if ended && let Some(drag) = self.dnd.drag.take() {
            self.drag_ended(&drag, BTN_LEFT);
        }
        self.x11_dnd_events();
    }

    /// The touch carrying a drag was taken back: the drag ends unaccepted.
    pub(crate) fn drag_cancelled(&mut self) {
        if let Some(mut drag) = self.dnd.drag.take() {
            if let Target::Host { index, .. } = drag.target
                && let Some(host) = self.hosts.get_mut(index)
            {
                host.drop_left();
            }
            drag.outcome = Some(false);
            self.drag_ended(&drag, BTN_LEFT);
        }
    }

    /// The drag icon's elements on `output` , front first; none without a drag or an icon.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "smithay geometry: logical and physical positions on outputs, far from i32 limits"
    )]
    pub(crate) fn drag_icon_elements<R>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem + ImportDma,
        R::TextureId: Clone + Send + 'static,
    {
        let (Some(_), Some(icon)) = (&self.dnd.drag, &self.dnd.icon) else {
            return Vec::new();
        };
        let Some(geometry) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = output.current_scale().fractional_scale();
        let at = self.dnd.at - geometry.loc.to_f64();
        match icon {
            DragIcon::Surface { surface, offset } => {
                let origin: Point<i32, Physical> =
                    (at + offset.to_f64()).to_physical_precise_round(scale);
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    origin,
                    Scale::from(scale),
                    1.0,
                    Kind::Unspecified,
                )
            }
            DragIcon::Image { frame, hotspot } => {
                let origin: Point<f64, Physical> = (at - hotspot.to_f64()).to_physical(scale);
                let placed = crate::uploads::Placed {
                    at: origin,
                    src: None,
                    size: None,
                };
                match self.uploads.element(renderer, frame, placed) {
                    Ok(element) => element.into_iter().collect(),
                    Err(error) => {
                        tracing::warn!(?error, "drag icon not imported");
                        Vec::new()
                    }
                }
            }
        }
    }

    /// A client committed `surface` : when it is the drag icon, follow what the commit moved it by.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "smithay geometry: logical and physical positions on outputs, far from i32 limits"
    )]
    pub(crate) fn drag_icon_commit(&mut self, surface: &WlSurface) {
        let Some(DragIcon::Surface {
            surface: icon,
            offset,
        }) = self.dnd.icon.as_mut()
        else {
            return;
        };
        if icon != surface {
            return;
        }
        let delta = smithay::wayland::compositor::with_states(surface, |states| {
            states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .buffer_delta
                .take()
        });
        if let Some(delta) = delta {
            *offset += delta;
        }
    }

    /// What is under `at` for a drag; X11 windows are no target of an X11 client's drag that left
    /// them.
    fn drag_under(&self, at: Point<f64, Logical>, x11_source: bool) -> Under {
        if self.locked() {
            return Under::Nothing;
        }
        if let Some(index) = self.host_under(at) {
            return Under::Host(index);
        }
        match self.surface_under(at) {
            Some((window, surface, origin)) => match window.x11_surface() {
                Some(_) if x11_source => Under::Nothing,
                Some(x11) => match x11.xwm_id() {
                    Some(xwm) => Under::X11 {
                        xwm,
                        window: x11.window_id(),
                    },
                    None => Under::Nothing,
                },
                None => Under::Client(surface, origin),
            },
            None => Under::Nothing,
        }
    }

    /// The pointer moved while a drag grab runs: find the target and move the grab. False when no
    /// drag grab runs.
    pub(crate) fn drag_motion(
        &mut self,
        location: Point<f64, Logical>,
        serial: Serial,
        time: u32,
    ) -> bool {
        let Some(drag) = &self.dnd.drag else {
            return false;
        };
        self.dnd.at = location;
        let under = self.drag_under(location, matches!(drag.source, Source::X11));
        self.retarget(&under, location);
        let focus = match under {
            Under::Client(surface, origin) => Some((surface, origin)),
            _ => None,
        };
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.motion(
                self,
                focus,
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
        }
        self.x11_dnd_events();
        true
    }

    /// Leave the old target and enter (or move over) the new one.
    fn retarget(&mut self, under: &Under, at: Point<f64, Logical>) {
        let Some(drag) = &self.dnd.drag else {
            return;
        };
        let old = drag.target.clone();
        let mimes = drag.mimes.clone();
        let action = Action::preferred(drag.actions);
        let same = old.is(under);
        if !same {
            match old {
                Target::Host { index, .. } => {
                    if let Some(host) = self.hosts.get_mut(index) {
                        host.drop_left();
                    }
                }
                Target::X11 { xwm, .. } => {
                    if let Some(bridged) = self.dnd.bridged_mut(xwm) {
                        bridged.bridge.source_leave();
                    }
                }
                Target::Client | Target::Nothing => {}
            }
            self.source_feedback(None, Action::None);
        }
        let target = match *under {
            Under::Nothing => Target::Nothing,
            Under::Client(..) => Target::Client,
            Under::Host(index) => {
                let mime = self
                    .hosts
                    .get_mut(index)
                    .and_then(|host| host.drop_offered(&mimes, at.x, at.y));
                self.source_feedback(mime.as_deref(), action);
                Target::Host { index, mime }
            }
            Under::X11 { xwm, window } => {
                let aware = match old {
                    Target::X11 {
                        xwm: was_xwm,
                        window: was,
                        aware,
                    } if same && was == window && was_xwm == xwm => aware,
                    _ => self.dnd.bridged_mut(xwm).is_some_and(|bridged| {
                        bridged
                            .bridge
                            .source_enter(window, &xdnd::targets_of_mimes(&mimes))
                    }),
                };
                if aware && let Some(bridged) = self.dnd.bridged(xwm) {
                    bridged.bridge.source_position(at.x, at.y, action);
                }
                Target::X11 { xwm, window, aware }
            }
        };
        if let Some(drag) = self.dnd.drag.as_mut() {
            drag.target = target;
        }
    }

    /// Tell a Wayland source whether the target takes it (smithay does for Wayland targets; this is
    /// for hosted and X11 ones).
    fn source_feedback(&self, mime: Option<&str>, action: Action) {
        let Some(Drag {
            source: Source::Client(source),
            ..
        }) = &self.dnd.drag
        else {
            return;
        };
        source.target(mime.map(str::to_owned));
        if source.version() >= 3 {
            source.action(if mime.is_some() {
                action.dnd()
            } else {
                DndAction::None
            });
        }
    }

    /// A button while a drag grab runs: a release drops on the target. False when no drag grab
    /// runs.
    pub(crate) fn drag_button(
        &mut self,
        button: u32,
        pressed: bool,
        serial: Serial,
        time: u32,
    ) -> bool {
        if self.dnd.drag.is_none() {
            return false;
        }
        if !pressed {
            self.drop_on_target();
        }
        if let Some(pointer) = self.seat.get_pointer() {
            let state = if pressed {
                smithay::backend::input::ButtonState::Pressed
            } else {
                smithay::backend::input::ButtonState::Released
            };
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
        }
        let ended = self
            .seat
            .get_pointer()
            .is_none_or(|pointer| !pointer.is_grabbed());
        if ended && let Some(drag) = self.dnd.drag.take() {
            self.drag_ended(&drag, button);
        }
        self.x11_dnd_events();
        true
    }

    /// The grab ended: tell the hosted app that started the drag, let an X11 source see its button
    /// come up, and route the pointer anew.
    fn drag_ended(&mut self, drag: &Drag, button: u32) {
        self.dnd.icon = None;
        if let Source::Host { origin, .. } = drag.source
            && let Some(host) = self.hosts.get_mut(origin)
        {
            // The press was the app's; so is the release.
            host.pointer_button(button, false);
            // smithay cancels every drop on a client it does not validate,
            // so a client drop that was not cancelled was taken.
            host.drag_ended(drag.outcome.unwrap_or(true));
        }
        if std::mem::take(&mut self.dnd.release_x11_source) {
            self.release_x11_source();
        } else {
            self.refocus_pointer();
        }
    }

    /// The target of a Wayland source's drop has the data: the source may finish (and delete the
    /// item after a move).
    fn finish_source(&mut self) {
        if let Some(source) = self.dnd.finishing.take()
            && source.is_alive()
            && source.version() >= 3
        {
            source.dnd_finished();
        }
    }

    /// Route the pointer by what is under it now.
    fn refocus_pointer(&mut self) {
        let time = self.now_ms();
        self.pointer_motion(self.pointer, SERIAL_COUNTER.next_serial(), time);
    }

    /// The button came up over a hosted surface or an X11 window: hand it the drop before the grab
    /// tells the source.
    fn drop_on_target(&mut self) {
        let Some(drag) = &self.dnd.drag else {
            return;
        };
        let outcome = match drag.target.clone() {
            Target::Host {
                index,
                mime: Some(mime),
            } => {
                self.drop_on_host(index, &mime);
                Some(true)
            }
            Target::Host { index, mime: None } => {
                if let Some(host) = self.hosts.get_mut(index) {
                    host.drop_left();
                }
                Some(false)
            }
            Target::X11 {
                aware: true, xwm, ..
            } => Some(self.drop_on_x11(xwm)),
            Target::X11 { .. } => Some(false),
            Target::Client | Target::Nothing => None,
        };
        if let Some(outcome) = outcome {
            if let Some(drag) = self.dnd.drag.as_mut() {
                drag.outcome = Some(outcome);
            }
            if let Some(incoming) = self.dnd.incoming.as_mut() {
                incoming.outcome = Some(outcome);
            }
        }
    }

    fn drop_on_host(&mut self, index: usize, mime: &str) {
        let Some(drag) = &self.dnd.drag else {
            return;
        };
        match &drag.source {
            Source::Client(source) => {
                let source = source.clone();
                self.read_for_host(index, mime, false, |fd| {
                    source.send(mime.to_owned(), fd.as_fd());
                });
            }
            Source::Host { values, .. } => {
                let bytes = value_for(values, mime).map(<[u8]>::to_vec);
                if let (Some(bytes), Some(host)) = (bytes, self.hosts.get_mut(index)) {
                    host.drop_data(mime, bytes);
                }
            }
            Source::X11 => self.read_x11_for_host(index, mime, false),
        }
    }

    /// Read the X11 drag's `mime` for hosted surface `index` .
    fn read_x11_for_host(&mut self, index: usize, mime: &str, finish_x11: bool) {
        let Some(target) = self
            .dnd
            .incoming
            .as_ref()
            .and_then(|incoming| xdnd::pick_format(mime, &incoming.types))
        else {
            return;
        };
        let Some(xwm) = self.dnd.incoming.as_ref().map(|incoming| incoming.xwm) else {
            return;
        };
        let mut writer = None;
        self.read_for_host(index, mime, finish_x11, |fd| writer = Some(fd));
        if let (Some(fd), Some(bridged)) = (writer, self.dnd.bridged_mut(xwm)) {
            bridged.bridge.convert(target, fd);
        }
    }

    /// Read what `request` writes into a pipe, off the loop, then hand it to hosted surface `index`
    /// .
    fn read_for_host(
        &mut self,
        index: usize,
        mime: &str,
        finish_x11: bool,
        request: impl FnOnce(OwnedFd),
    ) {
        let mime = mime.to_owned();
        self.read_off_loop(request, move |bytes| Delivery::Host {
            index,
            mime,
            bytes,
            finish_x11,
        });
    }

    /// Hand `request` a pipe's writing end and read the other off the loop, up to
    /// [`MAX_SELECTION_BYTES`]; `deliver` makes the message.
    fn read_off_loop(
        &mut self,
        request: impl FnOnce(OwnedFd),
        deliver: impl FnOnce(Vec<u8>) -> Delivery + Send + 'static,
    ) {
        let Some(sender) = self.deliveries() else {
            return;
        };
        let (reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(error) => {
                tracing::warn!(%error, "no pipe for dropped data");
                return;
            }
        };
        request(writer.into());
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let limit = u64::try_from(MAX_SELECTION_BYTES).unwrap_or(u64::MAX);
            if let Err(error) = reader.take(limit).read_to_end(&mut bytes) {
                tracing::debug!(%error, "dropped data not read");
            }
            // The loop may have gone; then nobody wants the bytes.
            let _ = sender.send(deliver(bytes));
        });
    }

    /// The channel bytes read off the loop come back on.
    fn deliveries(&mut self) -> Option<Sender<Delivery>> {
        if self.dnd.deliveries.is_none() {
            let handle = self.loop_handle.clone()?;
            let (sender, receiver) = channel::channel();
            let inserted = handle.insert_source(receiver, |event, (), data| {
                if let channel::Event::Msg(delivery) = event {
                    data.state.deliver(delivery);
                }
            });
            if let Err(error) = inserted {
                tracing::warn!(%error, "dropped data channel not added");
                return None;
            }
            self.dnd.deliveries = Some(sender);
        }
        self.dnd.deliveries.clone()
    }

    fn deliver(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Host {
                index,
                mime,
                bytes,
                finish_x11,
            } => {
                if let Some(host) = self.hosts.get_mut(index) {
                    host.drop_data(&mime, bytes);
                }
                if finish_x11 {
                    self.finish_incoming(true);
                }
                self.finish_source();
            }
            Delivery::ReadAhead { mime, bytes } => {
                let Some(xwm) = self.dnd.x_served.as_ref().map(|served| served.xwm) else {
                    return;
                };
                let Some(served) = self.dnd.x_served.as_mut() else {
                    return;
                };
                served.loading.retain(|loading| *loading != mime);
                served.values.insert(mime, bytes);
                let waiting = std::mem::take(&mut served.waiting);
                for request in waiting {
                    self.answer_x11(xwm, request);
                }
            }
        }
        self.x11_dnd_events();
    }

    /// Drop on the X11 window of `xwm` 's display under the drag: what it may ask for is read ahead
    /// (a Wayland source) or at hand (a hosted one).
    fn drop_on_x11(&mut self, xwm: XwmId) -> bool {
        let Some(drag) = &self.dnd.drag else {
            return false;
        };
        if !self
            .dnd
            .bridged(xwm)
            .is_some_and(|bridged| bridged.bridge.source_accepted())
        {
            if let Some(bridged) = self.dnd.bridged_mut(xwm) {
                bridged.bridge.source_leave();
            }
            return false;
        }
        match &drag.source {
            Source::Client(source) => {
                let source = source.clone();
                let loading = xdnd::read_ahead(&drag.mimes);
                self.dnd.x_served = Some(XServed {
                    xwm,
                    values: BTreeMap::new(),
                    loading: loading.clone(),
                    waiting: Vec::new(),
                });
                for mime in loading {
                    let asked = mime.clone();
                    self.read_off_loop(
                        |fd| source.send(asked, fd.as_fd()),
                        move |bytes| Delivery::ReadAhead { mime, bytes },
                    );
                }
            }
            Source::Host { values, .. } => {
                self.dnd.x_served = Some(XServed {
                    xwm,
                    values: values.as_ref().clone(),
                    loading: Vec::new(),
                    waiting: Vec::new(),
                });
            }
            // An X11 drag over an X11 window is Xwayland's own.
            Source::X11 => return false,
        }
        self.dnd
            .bridged_mut(xwm)
            .is_some_and(|bridged| bridged.bridge.source_drop())
    }

    /// Answer an X11 window of `xwm` 's display asking for our drag's data.
    fn answer_x11(&mut self, xwm: XwmId, request: Request) {
        let answer: Option<Vec<u8>> = match self
            .dnd
            .x_served
            .as_mut()
            .filter(|served| served.xwm == xwm)
        {
            None => None,
            Some(served) => {
                let available: Vec<String> = served
                    .values
                    .keys()
                    .chain(served.loading.iter())
                    .cloned()
                    .collect();
                match xdnd::pick_format(&request.target, &available) {
                    Some(mime) if served.loading.contains(&mime) => {
                        // The bytes are still being read: answered when they arrive.
                        served.waiting.push(request);
                        return;
                    }
                    Some(mime) => served.values.get(&mime).cloned(),
                    None => None,
                }
            }
        };
        if let Some(bridged) = self.dnd.bridged_mut(xwm) {
            bridged.bridge.answer(&request, answer.as_deref());
        }
    }

    // X11 clients' drags.

    /// Connect an XDND bridge to the display of `xwm` 's server.
    pub(crate) fn start_xdnd(&mut self, display: u32, xwm: XwmId) {
        let bridge = match XdndBridge::connect(display) {
            Ok(bridge) => bridge,
            Err(error) => {
                tracing::warn!(%error, "no drag-and-drop with X11 windows");
                return;
            }
        };
        let (Some(handle), Ok(fd)) = (self.loop_handle.clone(), bridge.fd()) else {
            return;
        };
        let inserted = handle.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, _, data| {
                data.state.x11_dnd_events();
                Ok(PostAction::Continue)
            },
        );
        if let Err(error) = inserted {
            tracing::warn!(%error, "no drag-and-drop with X11 windows");
            return;
        }
        self.dnd.bridged.retain(|bridged| bridged.xwm != xwm);
        self.dnd.bridged.push(Bridged {
            xwm,
            bridge,
            proxy: None,
        });
    }

    /// Whether `surface` is the proxy of `xwm` 's display; the window manager keeps it out of the
    /// scene.
    pub(crate) fn dnd_proxy(&mut self, xwm: XwmId, surface: &X11Surface) -> bool {
        let proxy = self
            .dnd
            .bridged_mut(xwm)
            .is_some_and(|bridged| bridged.bridge.proxy() == surface.window_id());
        if proxy && let Some(bridged) = self.dnd.bridged_mut(xwm) {
            bridged.proxy = Some(surface.clone());
        }
        proxy
    }

    /// The display whose X11 window holds the pointer's press or a finger (Xwayland runs any X11
    /// drag under either).
    fn x11_held(&self) -> Option<XwmId> {
        let xwm_of = |surface: &WlSurface| {
            self.space
                .elements()
                .find(|window| crate::xwayland::x11_wl_surface(window).as_ref() == Some(surface))
                .and_then(|window| window.x11_surface())
                .and_then(X11Surface::xwm_id)
        };
        let pressed = self
            .seat
            .get_pointer()
            .and_then(|pointer| pointer.grab_start_data())
            .and_then(|start| start.focus)
            .and_then(|(surface, _)| xwm_of(&surface));
        pressed.or_else(|| {
            self.touch
                .clients
                .iter()
                .filter_map(|finger| finger.focus.as_ref())
                .find_map(|(surface, _)| xwm_of(surface))
        })
    }

    /// Raise the proxy of the display an X11 client may be dragging from — under the pointer or a
    /// finger — over something that is not an X11 window; take the other proxies down.
    pub(crate) fn x11_drag_motion(&mut self, at: Point<f64, Logical>) {
        let held = self.x11_held();
        let over_x11 = matches!(self.drag_under(at, false), Under::X11 { .. });
        let dragging = self.dnd.drag.is_none();
        let taken_over = self
            .dnd
            .incoming
            .as_ref()
            .filter(|incoming| incoming.grabbed)
            .map(|incoming| incoming.xwm);
        for bridged in &mut self.dnd.bridged {
            let wanted =
                Some(bridged.xwm) == held && dragging && !over_x11 && bridged.bridge.x11_may_drag();
            let keep = taken_over == Some(bridged.xwm);
            bridged.bridge.show_proxy(wanted || keep);
        }
    }

    /// Handle what the X11 side sent.
    pub(crate) fn x11_dnd_events(&mut self) {
        let mut events = Vec::new();
        for bridged in &mut self.dnd.bridged {
            for event in bridged.bridge.events() {
                events.push((bridged.xwm, event));
            }
        }
        for (xwm, event) in events {
            self.x11_dnd_event(xwm, event);
        }
    }

    fn x11_dnd_event(&mut self, xwm: XwmId, event: BridgeEvent) {
        match event {
            BridgeEvent::Enter { types } => {
                let mimes = xdnd::mimes_of_targets(&types);
                self.dnd.incoming = Some(Incoming {
                    xwm,
                    types,
                    mimes,
                    host: None,
                    grabbed: false,
                    outcome: None,
                    dropped: false,
                });
            }
            BridgeEvent::Position { x, y } => {
                self.incoming_position(Point::from((f64::from(x), f64::from(y))));
            }
            BridgeEvent::Leave => {
                if let Some(incoming) = self.dnd.incoming.take_if(|incoming| !incoming.grabbed)
                    && let Some((index, _)) = incoming.host
                    && let Some(host) = self.hosts.get_mut(index)
                {
                    host.drop_left();
                }
            }
            BridgeEvent::Drop => self.incoming_drop(),
            BridgeEvent::Status { accept, action } => {
                if accept && let Some(drag) = self.dnd.drag.as_mut() {
                    drag.chosen = action;
                }
                let mime = self
                    .dnd
                    .drag
                    .as_ref()
                    .and_then(|drag| drag.mimes.first().cloned());
                self.source_feedback(mime.as_deref().filter(|_| accept), action);
            }
            BridgeEvent::Finished => {
                self.dnd.x_served = None;
                self.finish_source();
            }
            BridgeEvent::Request(request) => self.answer_x11(xwm, request),
        }
    }

    /// The X11 drag is at `at` over the proxy, Xwayland still running it.
    fn incoming_position(&mut self, at: Point<f64, Logical>) {
        let Some(xwm) = self.dnd.incoming.as_ref().map(|incoming| incoming.xwm) else {
            return;
        };
        let Some(incoming) = &self.dnd.incoming else {
            return;
        };
        if incoming.grabbed {
            // The compositor drag took over; the drop decides.
            self.x11_status(xwm, true);
            return;
        }
        let mimes = incoming.mimes.clone();
        let old_host = incoming.host.as_ref().map(|(index, _)| *index);
        let under = self.drag_under(at, true);
        if let Some(old) = old_host
            && !matches!(under, Under::Host(index) if index == old)
            && let Some(host) = self.hosts.get_mut(old)
        {
            host.drop_left();
        }
        match under {
            Under::Host(index) => {
                let mime = self
                    .hosts
                    .get_mut(index)
                    .and_then(|host| host.drop_offered(&mimes, at.x, at.y));
                let accept = mime.is_some();
                if let Some(incoming) = self.dnd.incoming.as_mut() {
                    incoming.host = Some((index, mime));
                }
                self.x11_status(xwm, accept);
            }
            Under::Client(surface, origin) => {
                if let Some(incoming) = self.dnd.incoming.as_mut() {
                    incoming.host = None;
                }
                self.take_over_x11_drag(surface, origin, mimes);
            }
            Under::Nothing | Under::X11 { .. } => {
                if let Some(incoming) = self.dnd.incoming.as_mut() {
                    incoming.host = None;
                }
                self.x11_status(xwm, false);
            }
        }
    }

    fn x11_status(&self, xwm: XwmId, accept: bool) {
        if let Some(bridged) = self.dnd.bridged(xwm) {
            bridged.bridge.status(accept, Action::Copy);
        }
    }

    /// The X11 drag reached a Wayland surface: a compositor drag carries it on, so Wayland clients
    /// get it from the data device. Xwayland loses the pointer until the drop.
    fn take_over_x11_drag(
        &mut self,
        surface: WlSurface,
        origin: Point<f64, Logical>,
        mimes: Vec<String>,
    ) {
        let Some(xwm) = self.dnd.incoming.as_ref().map(|incoming| incoming.xwm) else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some(start) = pointer.grab_start_data() else {
            self.x11_status(xwm, false);
            return;
        };
        let Some(incoming) = self.dnd.incoming.as_mut() else {
            return;
        };
        incoming.grabbed = true;
        self.dnd.served = Some(Served::X11 {
            xwm,
            types: incoming.types.clone(),
        });
        self.dnd.drag = Some(Drag {
            source: Source::X11,
            mimes: mimes.clone(),
            actions: DndAction::Copy,
            target: Target::Client,
            outcome: None,
            chosen: Action::Copy,
        });
        let metadata = SourceMetadata {
            mime_types: mimes,
            dnd_action: DndAction::Copy,
        };
        let display = self.display.clone();
        let seat = self.seat.clone();
        start_dnd(
            &display,
            &seat,
            self,
            SERIAL_COUNTER.next_serial(),
            Some(start),
            None,
            metadata,
        );
        let time = self.now_ms();
        pointer.motion(
            self,
            Some((surface, origin)),
            &MotionEvent {
                location: self.pointer,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        self.x11_status(xwm, true);
    }

    /// The X11 source dropped on the proxy.
    fn incoming_drop(&mut self) {
        let Some(incoming) = self.dnd.incoming.as_mut() else {
            return;
        };
        incoming.dropped = true;
        if incoming.grabbed {
            if let Some(outcome) = incoming.outcome {
                self.finish_incoming(outcome);
            }
            return;
        }
        match incoming.host.clone() {
            Some((index, Some(mime))) => self.read_x11_for_host(index, &mime, true),
            _ => self.finish_incoming(false),
        }
    }

    /// Tell the X11 source its drop is done and route the pointer anew (it was on the proxy if a
    /// compositor drag took over).
    fn finish_incoming(&mut self, success: bool) {
        let Some(incoming) = self.dnd.incoming.take() else {
            return;
        };
        if let Some(bridged) = self.dnd.bridged_mut(incoming.xwm) {
            bridged.bridge.finish(success, Action::Copy);
        }
        if incoming.grabbed {
            self.refocus_pointer();
        }
    }

    /// The compositor drag that carried an X11 drag ended: put the pointer on the proxy so Xwayland
    /// sees the button up (it releases every button a returning pointer holds) and the source drops
    /// there.
    fn release_x11_source(&mut self) {
        let Some(xwm) = self.dnd.incoming.as_ref().map(|incoming| incoming.xwm) else {
            self.finish_incoming(false);
            return;
        };
        let Some(surface) = self
            .dnd
            .bridged(xwm)
            .and_then(|bridged| bridged.proxy.as_ref())
            .and_then(X11Surface::wl_surface)
        else {
            self.finish_incoming(false);
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.now_ms();
        pointer.motion(
            self,
            Some((surface, Point::from((0.0, 0.0)))),
            &MotionEvent {
                location: self.pointer,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// The display of `xwm` 's server is gone: its bridge, its windows' drags and what they served
    /// go with it.
    pub(crate) fn forget_x11_dnd(&mut self, xwm: XwmId) {
        self.dnd.bridged.retain(|bridged| bridged.xwm != xwm);
        if self
            .dnd
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.xwm == xwm)
        {
            self.dnd.incoming = None;
            if self
                .dnd
                .drag
                .as_ref()
                .is_some_and(|drag| matches!(drag.source, Source::X11))
            {
                self.dnd.drag = None;
                self.dnd.release_x11_source = false;
            }
        }
        if self
            .dnd
            .x_served
            .as_ref()
            .is_some_and(|served| served.xwm == xwm)
        {
            self.dnd.x_served = None;
        }
        if self.dnd.served.as_ref().is_some_and(
            |served| matches!(served, Served::X11 { xwm: served, .. } if *served == xwm),
        ) {
            self.dnd.served = None;
        }
    }
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dnd.icon = icon.map(|surface| DragIcon::Surface {
            surface,
            offset: Point::default(),
        });
        self.dnd.at = self.pointer;
        // A drag without a source stays within its client.
        self.dnd.drag = source.map(|source| {
            let (mimes, actions) =
                with_source_metadata(&source, |meta| (meta.mime_types.clone(), meta.dnd_action))
                    .unwrap_or_else(|_| (Vec::new(), DndAction::empty()));
            Drag {
                source: Source::Client(source),
                mimes,
                actions,
                target: Target::Nothing,
                outcome: None,
                chosen: Action::preferred(actions),
            }
        });
    }

    /// A hosted surface or an X11 window took the drop (handed it before the button reached the
    /// grab): the source hears it was performed with the action taken, not that it was cancelled.
    fn drop_target_action(&mut self, _seat: &Seat<Self>) -> Option<DndAction> {
        let drag = self.dnd.drag.as_ref()?;
        let Source::Client(source) = &drag.source else {
            return None;
        };
        if drag.outcome != Some(true) || matches!(drag.target, Target::Client | Target::Nothing) {
            return None;
        }
        self.dnd.finishing = Some(source.clone());
        Some(drag.chosen.dnd())
    }

    fn dropped(&mut self, _target: Option<WlSurface>, validated: bool, _seat: Seat<Self>) {
        if let Some(drag) = self.dnd.drag.as_mut() {
            drag.outcome.get_or_insert(validated);
        }
    }
}

impl ServerDndGrabHandler for State {
    fn send(&mut self, mime_type: String, fd: OwnedFd, _seat: Seat<Self>) {
        let x11 = match &self.dnd.served {
            Some(Served::X11 { xwm, types }) => Some((*xwm, xdnd::pick_format(&mime_type, types))),
            _ => None,
        };
        if let Some((xwm, target)) = x11 {
            if let (Some(target), Some(bridged)) = (target, self.dnd.bridged_mut(xwm)) {
                bridged.bridge.convert(target, fd);
            }
            return;
        }
        let Some(Served::Host(values)) = &self.dnd.served else {
            return;
        };
        if let Some(bytes) = value_for(values, &mime_type) {
            serve(fd, bytes.to_vec());
        } else {
            tracing::debug!(mime_type, "drag asked for a format it does not offer");
        }
    }

    fn dropped(&mut self, _seat: Seat<Self>) {
        if self
            .dnd
            .drag
            .as_ref()
            .is_some_and(|drag| matches!(drag.source, Source::X11))
        {
            self.dnd.release_x11_source = true;
        }
    }

    fn cancelled(&mut self, _seat: Seat<Self>) {
        if let Some(drag) = self.dnd.drag.as_mut() {
            drag.outcome.get_or_insert(false);
        }
        if let Some(incoming) = self.dnd.incoming.as_mut() {
            incoming.outcome.get_or_insert(false);
        }
        self.dnd.served = None;
    }

    fn finished(&mut self, _seat: Seat<Self>) {
        self.dnd.served = None;
        if let Some(drag) = self.dnd.drag.as_mut() {
            drag.outcome = Some(true);
        }
        let Some(incoming) = self.dnd.incoming.as_mut() else {
            return;
        };
        incoming.outcome = Some(true);
        if incoming.dropped {
            self.finish_incoming(true);
        }
    }
}
