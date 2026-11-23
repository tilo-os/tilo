//! The EIS servers the input portals hand apps a socket of.
//!
//! `RemoteDesktop.ConnectToEIS` gives a sender context: the app emulates
//! devices and its events reach the seat as [`RemoteInput`], for the
//! devices the person allowed and only while the session lives.
//! `InputCapture.ConnectToEIS` gives a receiver context: while a capture
//! is active, the session's input is sent to the app instead of the
//! desktop. Each server runs on a thread of its own and owns only its
//! socket; what it may do is the portal session's shared state, checked
//! for every event.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calloop::PostAction;
use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
use reis::enumflags2::{BitFlag as _, BitFlags};
use reis::request::{Connection, Device, DeviceCapability, EisRequest, Seat};

use crate::input_capture::CapturedInput;
use crate::remote_desktop::{InputInjector, RemoteInput, device_type};

/// The capabilities `devices` (`RemoteDesktop` device bits) emulate.
fn capabilities(devices: u32) -> BitFlags<DeviceCapability> {
    let mut capabilities = DeviceCapability::empty();
    if devices & device_type::KEYBOARD != 0 {
        capabilities |= DeviceCapability::Keyboard;
    }
    if devices & device_type::POINTER != 0 {
        capabilities |=
            DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll;
    }
    capabilities
}

/// Serve a sender context for a remote desktop allowed `devices`; the
/// app's end of the socket. Its events reach `injector` while `live`.
///
/// # Errors
/// The socket or its thread could not be made.
pub(crate) fn serve_sender(
    live: Arc<AtomicBool>,
    devices: u32,
    injector: Arc<dyn InputInjector>,
) -> std::io::Result<OwnedFd> {
    let (server, client) = UnixStream::pair()?;
    std::thread::Builder::new()
        .name("portal-remote-eis".into())
        .spawn(move || run_sender(server, &live, devices, injector))?;
    Ok(OwnedFd::from(client))
}

fn run_sender(
    server: UnixStream,
    live: &Arc<AtomicBool>,
    devices: u32,
    injector: Arc<dyn InputInjector>,
) {
    let Ok(context) = reis::eis::Context::new(server) else {
        return;
    };
    let Ok(mut event_loop) = calloop::EventLoop::<()>::try_new() else {
        return;
    };
    let allowed = capabilities(devices);
    let still = Arc::clone(live);
    let installed = event_loop.handle().insert_source(
        EisRequestSource::new(context, 1),
        move |event, connection, ()| {
            match event {
                Ok(EisRequestSourceEvent::Connected) => {
                    let _seat = connection.add_seat(Some("tilo"), allowed);
                    let _ = connection.flush();
                }
                Ok(EisRequestSourceEvent::Request(request)) => {
                    if !still.load(Ordering::Acquire) {
                        return Ok(PostAction::Remove);
                    }
                    if let EisRequest::Bind(bind) = request {
                        let device = bind.seat.add_device(
                            Some("tilo remote input"),
                            reis::eis::device::DeviceType::Virtual,
                            bind.capabilities & allowed,
                            |_| {},
                        );
                        device.resumed();
                        let _ = connection.flush();
                    } else if let Some(input) = remote_input(request)
                        && devices & input.device() != 0
                    {
                        injector.inject(input);
                    }
                }
                Err(_) => return Ok(PostAction::Remove),
            }
            Ok(PostAction::Continue)
        },
    );
    if installed.is_err() {
        return;
    }
    while live.load(Ordering::Acquire)
        && event_loop.dispatch(Duration::from_secs(1), &mut ()).is_ok()
    {}
}

/// The seat event an app's emulated event is, if it is one.
fn remote_input(request: EisRequest) -> Option<RemoteInput> {
    Some(match request {
        EisRequest::PointerMotion(event) => RemoteInput::PointerMotion {
            dx: f64::from(event.dx),
            dy: f64::from(event.dy),
        },
        EisRequest::PointerMotionAbsolute(event) => RemoteInput::PointerMotionAbsolute {
            stream: 0,
            x: f64::from(event.dx_absolute),
            y: f64::from(event.dy_absolute),
        },
        EisRequest::Button(event) => RemoteInput::PointerButton {
            button: i32::try_from(event.button).ok()?,
            pressed: matches!(event.state, reis::eis::button::ButtonState::Press),
        },
        EisRequest::ScrollDelta(event) => RemoteInput::PointerAxis {
            dx: f64::from(event.dx),
            dy: f64::from(event.dy),
        },
        EisRequest::ScrollDiscrete(event) => RemoteInput::PointerAxisDiscrete {
            axis: 0,
            steps: event.discrete_dy,
        },
        EisRequest::KeyboardKey(event) => RemoteInput::KeyboardKeycode {
            keycode: i32::try_from(event.key).ok()?,
            pressed: matches!(event.state, reis::eis::keyboard::KeyState::Press),
        },
        _ => return None,
    })
}

/// What an input capture tells its receiver context.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReceiverCommand {
    /// Start emulating, as activation `u32`.
    Activate(u32),
    Deactivate,
    Input(CapturedInput),
    Close,
}

/// Where an input capture's commands go.
#[derive(Clone)]
pub(crate) struct Receiver {
    commands: calloop::channel::Sender<ReceiverCommand>,
}

impl Receiver {
    pub(crate) fn send(&self, command: ReceiverCommand) {
        let _ = self.commands.send(command);
    }
}

/// Serve a receiver context for an input capture; where its commands go
/// and the app's end of the socket.
///
/// # Errors
/// The socket or its thread could not be made.
pub(crate) fn serve_receiver() -> std::io::Result<(Receiver, OwnedFd)> {
    let (server, client) = UnixStream::pair()?;
    let (commands, channel) = calloop::channel::channel();
    std::thread::Builder::new()
        .name("portal-capture-eis".into())
        .spawn(move || run_receiver(server, channel))?;
    Ok((Receiver { commands }, OwnedFd::from(client)))
}

#[derive(Default)]
struct ReceiverState {
    connection: Option<Bound>,
    active: bool,
    running: bool,
    sequence: u32,
    started: Option<std::time::Instant>,
}

struct Bound {
    connection: Connection,
    keyboard: Option<Device>,
    pointer: Option<Device>,
}

fn run_receiver(server: UnixStream, commands: calloop::channel::Channel<ReceiverCommand>) {
    let Ok(context) = reis::eis::Context::new(server) else {
        return;
    };
    let Ok(mut event_loop) = calloop::EventLoop::<ReceiverState>::try_new() else {
        return;
    };
    let handle = event_loop.handle();
    let requests = handle.insert_source(
        EisRequestSource::new(context, 1),
        |event, connection, state: &mut ReceiverState| {
            match event {
                Ok(EisRequestSourceEvent::Connected) => {
                    // A receiver binds nothing until the seat says what it
                    // offers.
                    let _seat = connection.add_seat(Some("tilo"), capabilities(device_type::ALL));
                    state.connection = Some(Bound {
                        connection: connection.clone(),
                        keyboard: None,
                        pointer: None,
                    });
                    let _ = connection.flush();
                }
                Ok(EisRequestSourceEvent::Request(EisRequest::Bind(bind))) => {
                    let sequence = state.sequence;
                    if let Some(bound) = &mut state.connection {
                        if bound.keyboard.is_none()
                            && bind.capabilities.contains(DeviceCapability::Keyboard)
                        {
                            bound.keyboard = Some(add_device(
                                &bind.seat,
                                "tilo keyboard",
                                DeviceCapability::Keyboard.into(),
                                sequence,
                            ));
                        }
                        if bound.pointer.is_none()
                            && bind.capabilities.contains(DeviceCapability::Pointer)
                        {
                            bound.pointer = Some(add_device(
                                &bind.seat,
                                "tilo pointer",
                                capabilities(device_type::POINTER),
                                sequence,
                            ));
                        }
                        let _ = bound.connection.flush();
                    }
                }
                Ok(EisRequestSourceEvent::Request(EisRequest::Disconnect)) | Err(_) => {
                    return Ok(PostAction::Remove);
                }
                Ok(EisRequestSourceEvent::Request(_)) => {}
            }
            Ok(PostAction::Continue)
        },
    );
    let commanded = handle.insert_source(commands, |event, (), state| {
        let calloop::channel::Event::Msg(command) = event else {
            state.running = false;
            return;
        };
        match command {
            ReceiverCommand::Activate(sequence) => {
                state.active = true;
                state.sequence = sequence;
                if let Some(bound) = &state.connection {
                    for device in [&bound.keyboard, &bound.pointer].into_iter().flatten() {
                        device.start_emulating(sequence);
                    }
                    let _ = bound.connection.flush();
                }
            }
            ReceiverCommand::Deactivate => {
                state.active = false;
                if let Some(bound) = &state.connection {
                    for device in [&bound.keyboard, &bound.pointer].into_iter().flatten() {
                        device.stop_emulating();
                    }
                    let _ = bound.connection.flush();
                }
            }
            ReceiverCommand::Input(input) if state.active => send_input(state, input),
            ReceiverCommand::Input(_) => {}
            ReceiverCommand::Close => state.running = false,
        }
    });
    if requests.is_err() || commanded.is_err() {
        return;
    }
    let mut state = ReceiverState {
        running: true,
        started: Some(std::time::Instant::now()),
        ..ReceiverState::default()
    };
    while state.running
        && event_loop
            .dispatch(Duration::from_secs(1), &mut state)
            .is_ok()
    {}
}

fn add_device(
    seat: &Seat,
    name: &str,
    capabilities: BitFlags<DeviceCapability>,
    sequence: u32,
) -> Device {
    let device = seat.add_device(
        Some(name),
        reis::eis::device::DeviceType::Virtual,
        capabilities,
        |_| {},
    );
    device.resumed();
    if sequence != 0 {
        device.start_emulating(sequence);
    }
    device
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "EIS carries pointer deltas as f32; a delta is far inside its range"
)]
fn send_input(state: &ReceiverState, input: CapturedInput) {
    let Some(bound) = &state.connection else {
        return;
    };
    // Microseconds since the server started: the timestamps only order
    // frames for the receiver.
    let time = state.started.map_or(0, |started| {
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
    });
    match input {
        CapturedInput::PointerMotion { dx, dy, .. } => {
            if let Some(device) = &bound.pointer
                && let Some(pointer) = device.interface::<reis::eis::Pointer>()
            {
                pointer.motion_relative(dx as f32, dy as f32);
                device.frame(time);
            }
        }
        CapturedInput::PointerButton { button, pressed } => {
            if let Some(device) = &bound.pointer
                && let Some(buttons) = device.interface::<reis::eis::Button>()
            {
                buttons.button(
                    button,
                    if pressed {
                        reis::eis::button::ButtonState::Press
                    } else {
                        reis::eis::button::ButtonState::Released
                    },
                );
                device.frame(time);
            }
        }
        CapturedInput::Scroll { dx, dy } => {
            if let Some(device) = &bound.pointer
                && let Some(scroll) = device.interface::<reis::eis::Scroll>()
            {
                scroll.scroll(dx, dy);
                device.frame(time);
            }
        }
        CapturedInput::Keyboard { keycode, pressed } => {
            if let Some(device) = &bound.keyboard
                && let Some(keyboard) = device.interface::<reis::eis::Keyboard>()
            {
                keyboard.key(
                    keycode,
                    if pressed {
                        reis::eis::keyboard::KeyState::Press
                    } else {
                        reis::eis::keyboard::KeyState::Released
                    },
                );
                device.frame(time);
            }
        }
    }
    let _ = bound.connection.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_desktop_emulates_only_the_devices_allowed() {
        assert_eq!(
            capabilities(device_type::KEYBOARD),
            BitFlags::from(DeviceCapability::Keyboard)
        );
        let pointer = capabilities(device_type::POINTER);
        assert!(pointer.contains(DeviceCapability::Scroll));
        assert!(!pointer.contains(DeviceCapability::Keyboard));
    }
}
