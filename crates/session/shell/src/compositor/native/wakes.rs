//! The sources that wake the compositor's event loop. Calloop drives the
//! whole compositor; every one of these only ends a park, and the loop turn
//! that follows does the work they announce.

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::Arc;

use smithay::reexports::calloop::{self, Interest, LoopHandle, Mode, PostAction, generic::Generic};
use smithay::reexports::wayland_server::Display;

use crate::compositor::backend::Backend;
use crate::compositor::session_termination::SessionTermination;
use crate::compositor::state::State;
use crate::shell::frame::FrameBuilder;
use crate::shell::present;

type Handle = LoopHandle<'static, State>;

/// Ordered teardown on SIGTERM/SIGINT/SIGHUP: the signal only wakes the loop,
/// which then leaves its turn and unwinds through `run`'s shutdown.
#[allow(
    clippy::exit,
    reason = "only called from `run`'s entry-point setup; a session that cannot observe its \
              termination signals has always ended here"
)]
pub(super) fn session_termination(handle: &Handle) -> SessionTermination {
    let (termination, termination_source) = SessionTermination::source().unwrap_or_else(|error| {
        eprintln!("tilo compositor: cannot establish ordered signal teardown: {error}");
        std::process::exit(1);
    });
    handle
        .insert_source(termination_source, |_, fd, _| {
            let mut value = [0_u8; 8];
            // SAFETY: the buffer is writable for its stated length; fd is the
            // live nonblocking eventfd owned by this event source.
            unsafe {
                libc::read(fd.as_raw_fd(), value.as_mut_ptr().cast(), value.len());
            }
            Ok(PostAction::Continue)
        })
        .unwrap_or_else(|error| {
            eprintln!("tilo compositor: cannot observe termination signals: {error}");
            std::process::exit(1);
        });
    termination
}

/// The `Display` owns the wayland-server protocol state. Its poll fd is an
/// epoll instance containing the individual client sockets. Register a
/// duplicate as an edge-triggered *wake token*, while `run` retains
/// ownership and does all actual protocol dispatch. The edge is safe
/// because `dispatch_clients` drains the inner epoll fully; level-triggered
/// epoll nesting can consume outer readiness without draining the inner
/// client event and then strand a request. If registration is unavailable,
/// retain the bounded 4 ms service fallback rather than breaking clients.
pub(super) fn wayland_client_wake(handle: &Handle, display: &Display<State>) -> bool {
    let duplicate = unsafe { libc::fcntl(display.as_fd().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "could not duplicate Wayland display wake fd; using bounded polling fallback"
        );
        false
    } else {
        // SAFETY: F_DUPFD_CLOEXEC returned a new owned descriptor.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
        match handle.insert_source(Generic::new(fd, Interest::READ, Mode::Edge), |_, _, _| {
            Ok(PostAction::Continue)
        }) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not register Wayland display wake fd; using bounded polling fallback"
                );
                false
            }
        }
    }
}

/// The session's one settings document: read once per change on its own
/// thread and shared, by generation, with every subsystem that follows a
/// setting (input devices, switch control, rotation lock, automatic
/// brightness, input methods, continuity policy). A change wakes an idle
/// loop through this ping; nothing here reads the file between changes.
pub(super) fn live_settings(handle: &Handle) -> settings_store::live::LiveSettings {
    let wake = match calloop::ping::make_ping() {
        Ok((ping, source)) => match handle.insert_source(source, |(), (), _| {}) {
            Ok(_) => Some(ping),
            Err(error) => {
                tracing::warn!(%error, "could not register the settings wake source; an idle desktop applies Settings changes on its next wake");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the settings wake source; an idle desktop applies Settings changes on its next wake");
            None
        }
    };
    let live_settings = settings_store::live::LiveSettings::start(move || {
        if let Some(ping) = &wake {
            ping.ping();
        }
    });
    if let Some(reason) = live_settings.degraded() {
        tracing::warn!(%reason, "settings changes may arrive late");
    }
    live_settings
}

/// DRM owns a nested calloop for libinput/libseat/udev. Register its poll
/// fd in the compositor loop so hardware input wakes us immediately rather
/// than waiting for a periodic backend poll (which made the cursor visibly
/// lag by roughly the old 50 ms interval).
pub(super) fn backend_event_wake(handle: &Handle, backend: &dyn Backend) -> bool {
    if let Some(fd) = backend.event_fd() {
        match handle
            // `event_fd` is the poll fd of the DRM backend's nested calloop.
            // Treat it as a wake *edge*, then let `Backend::poll_events`
            // drain that inner loop at the start of the next iteration.  A
            // level-triggered registration can remain readable after the
            // inner dispatch consumed its source, making the outer calloop
            // return immediately forever.  The resulting idle storm was
            // measured at ~12,000 iterations per two seconds while no frame
            // was dirty and the presenter counters did not move.
            .insert_source(Generic::new(fd, Interest::READ, Mode::Edge), |_, _, _| {
                Ok(PostAction::Continue)
            }) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(%error, "could not register backend event wake fd");
                false
            }
        }
    } else {
        !backend.needs_idle_poll()
    }
}

/// Move presentation onto its own thread where the backend supports it.
/// Headless declines: its bytes are read back synchronously by `snapshot`.
/// Spawned as soon as the backend exists: the render thread builds its
/// renderer while the rest of the compositor starts, not when the first
/// frame arrives.
#[allow(
    clippy::exit,
    reason = "only called from `run`'s entry-point setup; a presenter that cannot wake the loop \
              has always ended the process here"
)]
pub(super) fn present_pipe(
    handle: &Handle,
    backend: &mut dyn Backend,
) -> Option<present::PresentPipe> {
    backend.take_presenter().map(|p| {
        eprintln!("tilo compositor: presenting on a dedicated render thread");
        // Wake the loop when a frame retires — otherwise a tick that declined to
        // paint for lack of pipe capacity would park out the whole interval, halving throughput.
        let (ping, ping_source) = match calloop::ping::make_ping() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("tilo compositor: create present-retire ping failed: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = handle.insert_source(ping_source, |(), (), _| {
            // Waking is the whole effect: the loop re-evaluates its paint gate next iteration and now finds room.
        }) {
            eprintln!("tilo compositor: insert present-retire ping source failed: {e}");
            std::process::exit(1);
        }
        present::PresentPipe::spawn(p, Box::new(move || ping.ping()))
    })
}

/// Foreign capture workers finish off the loop and wake it through this
/// ping. The returned handle is held for the session: the source stays
/// registered only while a ping handle lives.
#[allow(
    clippy::exit,
    reason = "only called from `run`'s entry-point setup; capture completion has always been \
              fatal to miss here"
)]
pub(super) fn capture_completion_wake(handle: &Handle) -> calloop::ping::Ping {
    let (capture_ping, capture_ping_source) = match calloop::ping::make_ping() {
        Ok(pair) => pair,
        Err(error) => {
            tracing::error!(%error, "could not create foreign capture completion wake source");
            std::process::exit(1);
        }
    };
    if let Err(error) = handle.insert_source(capture_ping_source, |(), (), _| {}) {
        tracing::error!(%error, "could not register foreign capture completion wake source");
        std::process::exit(1);
    }
    crate::shell::foreign::install_completion_ping(&capture_ping);
    capture_ping
}

/// Convertibles: the accelerometer turns the built-in panel. The ping
/// only wakes the loop; the change itself is drained by the loop.
pub(super) fn device_rotation(
    handle: &Handle,
    live_settings: &settings_store::live::LiveSettings,
) -> Option<std::sync::mpsc::Receiver<settings_store::hardware::OutputTransform>> {
    match calloop::ping::make_ping() {
        Ok((ping, source)) => match handle.insert_source(source, |(), (), _| {}) {
            Ok(_) => crate::compositor::device_rotation::spawn(
                move || ping.ping(),
                live_settings.clone(),
            ),
            Err(error) => {
                tracing::warn!(%error, "could not register the device rotation wake source");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the device rotation wake source");
            None
        }
    }
}

/// Work the desktop's hosted views finish on other threads: a peer stream's
/// decoded picture and an app's History descriptions.
pub(super) fn desktop_view_wakes(handle: &Handle, frame_builder: &mut FrameBuilder) {
    // A peer stream's picture finishes on a media thread after its frame's
    // push returned (the loop never waits for a decode); this wakes the loop
    // to show it, with no polling while a stream plays.
    match calloop::ping::make_ping() {
        Ok((ping, source)) => match handle.insert_source(source, |(), (), _| {}) {
            Ok(_) => frame_builder
                .host_mut()
                .set_peer_picture_wake(Arc::new(move || ping.ping())),
            Err(error) => {
                tracing::warn!(%error, "could not register the peer picture wake source; peer streams poll for decoded pictures");
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the peer picture wake source; peer streams poll for decoded pictures");
        }
    }
    // An app's History descriptions come from a worker thread; this wakes
    // the loop to send them.
    match calloop::ping::make_ping() {
        Ok((ping, source)) => match handle.insert_source(source, |(), (), _| {}) {
            Ok(_) => frame_builder
                .host_mut()
                .set_history_describer_wake(Arc::new(move || ping.ping())),
            Err(error) => {
                tracing::warn!(%error, "could not register the history description wake source; descriptions are sent on the next wake");
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the history description wake source; descriptions are sent on the next wake");
        }
    }
}

/// Compositor chrome has no application window for `accesskit_winit` to
/// attach to; its AT-SPI provider's callbacks run on AccessKit's thread, so
/// its mailbox is paired with this ping. An idle compositor must wake for a
/// reader's first tree request or action instead of waiting for unrelated
/// Wayland input.
#[allow(
    clippy::exit,
    reason = "only called from `run`'s entry-point setup; a missing AT-SPI wake has always \
              ended the process here"
)]
pub(super) fn accessibility_wake(handle: &Handle) -> calloop::ping::Ping {
    let (a11y_ping, a11y_ping_source) = match calloop::ping::make_ping() {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("tilo compositor: create AT-SPI wake source failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = handle.insert_source(a11y_ping_source, |(), (), _| {}) {
        eprintln!("tilo compositor: insert AT-SPI wake source failed: {error}");
        std::process::exit(1);
    }
    a11y_ping
}

/// Work handed to the session runtime from this loop (the network start
/// after first-login setup, media-player replies) reports back through
/// mailboxes; this ping wakes an idle loop when one is filled.
#[allow(
    clippy::exit,
    reason = "only called from `run`'s entry-point setup; a missing session wake has always \
              ended the process here"
)]
pub(super) fn session_wake(handle: &Handle) -> crate::session_runtime::Wake {
    match calloop::ping::make_ping() {
        Ok((ping, source)) => {
            if let Err(error) = handle.insert_source(source, |(), (), _| {}) {
                eprintln!("tilo compositor: insert session wake source failed: {error}");
                std::process::exit(1);
            }
            Arc::new(move || ping.ping())
        }
        Err(error) => {
            eprintln!("tilo compositor: create session wake source failed: {error}");
            std::process::exit(1);
        }
    }
}

/// Hand the session wake to every service that finishes work off the loop.
/// The returned guard keeps control-client invocations waking the loop.
pub(super) fn share_session_wake(
    session_wake: &crate::session_runtime::Wake,
    state: &mut State,
    frame_builder: &mut FrameBuilder,
    notif_bridge: &crate::notifications::NotificationBridge,
) -> crate::shell::search::invocation_wake::Installed {
    state.output_management.set_wake(Arc::clone(session_wake));
    state.set_share_encode_wake(Arc::clone(session_wake));
    crate::shell::display_persistence::set_wake(Arc::clone(session_wake));
    crate::shell::storage_notice::start(Arc::clone(session_wake));
    crate::shell::crash_notice::start(Arc::clone(session_wake));
    // NTFS drives open only through the shell's own sheet; see shell::drives.
    crate::shell::drives::start(Arc::clone(session_wake));
    crate::shell::app_list::set_wake(Arc::clone(session_wake));
    crate::shell::downloaded_apps::set_wake(Arc::clone(session_wake));
    crate::shell::downloaded_apps::follow_moves();
    crate::app_icons::set_wake(Arc::clone(session_wake));
    crate::shell::mpris::set_now_playing_wake(Arc::clone(session_wake));
    crate::shell::install_component_wake(Arc::clone(session_wake));
    // An invocation a control client waits on wakes the loop when it is
    // admitted or its job changes, not on the next unrelated wake.
    let invocation_wake = crate::shell::search::invocation_wake::install(Arc::clone(session_wake));
    notif_bridge.set_wake(Arc::clone(session_wake));
    frame_builder
        .host_mut()
        .set_quick_settings_wake(Arc::clone(session_wake));
    // The desk's System monitor is woken by each reading the session's
    // system information service sends it.
    frame_builder
        .host_mut()
        .set_desk_wake(Arc::clone(session_wake));
    // The network, once the loop installs it, takes the same wake and
    // binds the hosted apps that use it.
    // What the broker asks of the person's records the session holds
    // wakes the loop, which runs it.
    crate::shell::settings_records::wake_with(Arc::clone(session_wake));
    // So does what a room's other devices write to its layout record.
    crate::shell::workspace_layouts::wake_with(Arc::clone(session_wake));
    // So do the reads and writes apps ask of the workspace activity model.
    crate::shell::activity_provider::wake_with(Arc::clone(session_wake));
    // So do the layout changes agents ask of the desktop.
    crate::shell::desktop_door::wake_with(Arc::clone(session_wake));
    frame_builder.with_chrome(|chrome| {
        // From here on the loop never waits on the disk to open a tab's
        // record: the record store's thread opens it and wakes the loop.
        chrome.open_records_without_waiting(Arc::clone(session_wake));
        if let Some(apps) = chrome.hosted_inputs.in_process() {
            apps.borrow_mut()
                .bind_session_wake(Arc::clone(session_wake));
        }
    });
    invocation_wake
}
