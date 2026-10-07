//! Real native proxy progress while the production presentation closure is backpressured.
//! This has no user pixels, driver timing claim, or fabricated broker identity.
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop as NativeLoop};
use winit::platform::wayland::{EventLoopBuilderExtWayland, WindowExtWayland};
use winit::window::Window;

struct Server {
    stop: Arc<AtomicBool>,
    frames: mpsc::Sender<(bool, mpsc::Sender<(u64, u64)>)>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    fn new() -> (Self, UnixStream) {
        let (server, client) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (ready, initialized) = mpsc::channel();
        let (frames, frame_requests) = mpsc::channel::<(bool, mpsc::Sender<(u64, u64)>)>();
        let thread = thread::spawn(move || {
            let event_loop: EventLoop<'static, shell::compositor::state::State> =
                EventLoop::try_new().unwrap();
            let mut display: Display<shell::compositor::state::State> = Display::new().unwrap();
            let mut state =
                shell::compositor::state::State::new(event_loop.handle(), display.handle());
            display
                .handle()
                .insert_client(
                    server,
                    Arc::new(shell::compositor::state::ClientState::default()),
                )
                .unwrap();
            ready.send(()).unwrap();
            while !stopped.load(Ordering::Acquire) {
                display.dispatch_clients(&mut state).unwrap();
                shell::compositor::foreign::flush_configures(&mut state);
                if let Ok((complete, reply)) = frame_requests.try_recv() {
                    let captured = state
                        .workspace_tree
                        .read()
                        .desktop
                        .workspaces
                        .iter()
                        .flat_map(|w| &w.strips)
                        .flat_map(|s| &s.window_groups)
                        .flat_map(|g| &g.tabs)
                        .filter_map(|t| match &t.tile {
                            shell::shell::state::TileContent::Foreign { surface_id, .. } => {
                                Some(*surface_id)
                            }
                            _ => None,
                        })
                        .find_map(|id| {
                            shell::shell::foreign::store()
                                .presentation_pixels(id)
                                .map(|pixels| (id, pixels))
                        });
                    if let Some((id, pixels)) = captured {
                        if complete {
                            let scene = super::remote_typing_wire::draw_presentation_pixels(id);
                            assert!(!scene.encoding().is_empty());
                            let frame = state.presentation_feedback.composed(
                                shell::shell::foreign::store(),
                                Some(super::remote_typing_wire::presentation_output()),
                            );
                            state.presentation_feedback.submitted(73, frame);
                            state.presentation_feedback.completed(
                                72,
                                Some(shell::shell::present::PhysicalPresentation {
                                    timestamp: Duration::from_secs(987),
                                    sequence: 1234,
                                    refresh: None,
                                }),
                            );
                            // Another serial must not credit this buffer. Exact73 does.
                            state.presentation_feedback.completed(
                                73,
                                Some(shell::shell::present::PhysicalPresentation {
                                    timestamp: Duration::from_secs(987),
                                    sequence: 1234,
                                    refresh: None,
                                }),
                            );
                        }
                        reply.send(pixels).unwrap();
                    } else {
                        reply.send((0, 0)).unwrap();
                    }
                }
                display.flush_clients().unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });
        let server = Self {
            stop,
            frames,
            thread: Some(thread),
        };
        initialized.recv_timeout(Duration::from_secs(5)).unwrap();
        (server, client)
    }
    fn captured(&self, after: u64, complete: bool) -> (u64, u64) {
        let deadline = std::time::Instant::now()
            .checked_add(Duration::from_secs(5))
            .unwrap();
        loop {
            let (tx, rx) = mpsc::channel();
            self.frames.send((complete, tx)).unwrap();
            let pixels = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if pixels.1 > after {
                return pixels;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "actual buffer capture unavailable"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

enum Message {
    Probe,
    Exit,
}
struct Handler {
    request_only: bool,
    driver_calls: Arc<std::sync::atomic::AtomicUsize>,
    ready: Option<mpsc::Sender<()>>,
    release: Option<mpsc::Receiver<()>>,
    echoed: mpsc::Sender<()>,
    worker: Option<JoinHandle<bool>>,
    competing_worker: Option<JoinHandle<bool>>,
    competing_entered: Arc<AtomicBool>,
    window: Option<Arc<Window>>,
}
impl ApplicationHandler<Message> for Handler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(ready) = self.ready.take() else {
            return;
        };
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes())
                .unwrap(),
        );
        eprintln!("present-progress: actual native window configured");
        let present_window = window.clone();
        let release = self.release.take().unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let request_only = self.request_only;
        let calls = self.driver_calls.clone();
        self.worker = Some(thread::spawn(move || {
            let present = || {
                calls.fetch_add(1, Ordering::AcqRel);
                entered.send(()).unwrap();
                let _ = release.recv_timeout(Duration::from_secs(5));
            };
            if request_only {
                // Same request-only Winit entry used by the SDK's sole
                // buffer owner; the driver operation is independently invoked.
                let accepted = present_window.observe_next_buffer(Box::new(|_| {}));
                present();
                accepted
            } else {
                present_window.present_with_feedback(Box::new(|_| {}), present)
            }
        }));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let competing_window = window.clone();
        let competing_entered = self.competing_entered.clone();
        if !request_only {
            self.competing_worker = Some(thread::spawn(move || {
                competing_window.present_with_feedback(Box::new(|_| {}), || {
                    competing_entered.store(true, Ordering::Release);
                })
            }));
        }
        eprintln!("present-progress: actual presentation closure entered");
        self.window = Some(window);
        ready.send(()).unwrap();
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Message) {
        match event {
            Message::Probe => {
                let window = self.window.as_ref().unwrap();
                assert!(window.inner_size().width > 0);
                window.set_title("native progress fixture");
                self.echoed.send(()).unwrap();
            }
            Message::Exit => event_loop.exit(),
        }
    }
    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        _: winit::event::WindowEvent,
    ) {
    }
}

#[test]
fn native_proxy_progresses_while_present_is_backpressured() {
    run_progress(false);
}

#[test]
fn request_only_feedback_preserves_native_progress_and_calls_driver_once() {
    run_progress(true);
}

fn run_progress(request_only: bool) {
    let (_server, socket) = Server::new();
    let mut socket_environment = tilo_test_env::ScopedEnv::new();
    // The isolated nextest process transfers this actual FD to Wayland.
    socket_environment.set("WAYLAND_SOCKET", socket.into_raw_fd().to_string());
    let mut builder = NativeLoop::<Message>::with_user_event();
    builder.with_wayland().with_any_thread(true);
    let event_loop = builder.build().unwrap();
    let proxy = event_loop.create_proxy();
    let (ready, ready_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (echoed, echoed_rx) = mpsc::channel();
    let competing_entered = Arc::new(AtomicBool::new(false));
    let competing_probe = competing_entered.clone();
    let coordinator = thread::spawn(move || {
        let started = ready_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        let sent = started && proxy.send_event(Message::Probe).is_ok();
        let before_release = sent && echoed_rx.recv_timeout(Duration::from_millis(500)).is_ok();
        let competing_before_release = competing_probe.load(Ordering::Acquire);
        // Always release, even on failed observation. No native worker leaks.
        let _ = release.send(());
        let eventually =
            before_release || (sent && echoed_rx.recv_timeout(Duration::from_secs(5)).is_ok());
        let _ = proxy.send_event(Message::Exit);
        (
            started,
            sent,
            before_release,
            eventually,
            competing_before_release,
        )
    });
    let driver_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handler = Handler {
        request_only,
        driver_calls: driver_calls.clone(),
        ready: Some(ready),
        release: Some(release_rx),
        echoed,
        worker: None,
        competing_worker: None,
        competing_entered,
        window: None,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop.run_app(&mut handler)
    }));
    let observation = coordinator.join().unwrap();
    let supported = handler.worker.take().map(|worker| worker.join().unwrap());
    let competing_supported = handler
        .competing_worker
        .take()
        .map(|worker| worker.join().unwrap());
    let result = match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    result.unwrap();
    eprintln!(
        "present-progress: started={} sent={} before_release={} eventually={}",
        observation.0, observation.1, observation.2, observation.3
    );
    assert_eq!(
        supported,
        Some(true),
        "actual presentation protocol is required"
    );
    assert_eq!(
        driver_calls.load(Ordering::Acquire),
        1,
        "driver operation must occur exactly once"
    );
    if request_only {
        assert_eq!(
            competing_supported, None,
            "request-only contract has one buffer presenter"
        );
    } else {
        assert_eq!(competing_supported, Some(true));
        assert!(handler.competing_entered.load(Ordering::Acquire));
    }
    assert!(
        !observation.4,
        "competing presentation stole the pending commit"
    );
    assert!(
        observation.0 && observation.1 && observation.3,
        "actual native proxy route must be observed"
    );
    assert!(
        observation.2,
        "native owner proxy was starved until presentation returned"
    );
}

#[derive(Default)]
struct BufferClient {
    shm: Option<wayland_client::protocol::wl_shm::WlShm>,
}
impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()>
    for BufferClient
{
    fn event(
        state: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        (): &(),
        _: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_shm"
        {
            state.shm = Some(registry.bind(name, version.min(1), qh, ()));
        }
    }
}
wayland_client::delegate_noop!(BufferClient: ignore wayland_client::protocol::wl_shm::WlShm);
wayland_client::delegate_noop!(BufferClient: ignore wayland_client::protocol::wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(BufferClient: ignore wayland_client::protocol::wl_buffer::WlBuffer);

struct WireHandler {
    ready: Option<mpsc::Sender<Arc<Window>>>,
    window: Option<Arc<Window>>,
}
impl ApplicationHandler<Message> for WireHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(ready) = self.ready.take() {
            let window = Arc::new(
                event_loop
                    .create_window(Window::default_attributes())
                    .unwrap(),
            );
            ready.send(window.clone()).unwrap();
            self.window = Some(window);
        }
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Message) {
        if matches!(event, Message::Exit) {
            event_loop.exit();
        }
    }
    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        _: winit::event::WindowEvent,
    ) {
    }
}

fn verify_native_buffers(window: &Window, server: &Server) {
    use std::io::Write;
    use std::os::fd::AsFd;
    use wayland_client::protocol::{wl_shm, wl_surface};
    use wayland_client::{Connection, Proxy};
    use wgpu::rwh::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use winit::platform::wayland::WaylandPresentation;
    let RawDisplayHandle::Wayland(display) = window.display_handle().unwrap().as_raw() else {
        panic!("actual Wayland display required");
    };
    let RawWindowHandle::Wayland(native) = window.window_handle().unwrap().as_raw() else {
        panic!("actual Wayland surface required");
    };
    // SAFETY: `window` pins Winit's live display and surface until all
    // borrowed proxies/guest connection below have been dropped. Guest
    // mode does not close the owner's display or destroy its surface.
    let backend = unsafe {
        wayland_client::backend::Backend::from_foreign_display(display.display.as_ptr().cast())
    };
    let connection = Connection::from_backend(backend);
    // SAFETY: this pointer and interface are from the matching live
    // Winit Wayland handle. We send attach/commit only and never destroy it.
    let id = unsafe {
        wayland_client::backend::ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            native.surface.as_ptr().cast(),
        )
    }
    .unwrap();
    let surface = wl_surface::WlSurface::from_id(&connection, id).unwrap();
    let mut queue = connection.new_event_queue::<BufferClient>();
    let qh = queue.handle();
    let mut client = BufferClient::default();
    let registry = connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut client).unwrap();
    let shm = client
        .shm
        .take()
        .expect("actual server shared-memory buffers");
    let mut pixels = tempfile::tempfile().unwrap();
    pixels.write_all(&[0, 0, 255, 255, 0, 255, 0, 255]).unwrap();
    let pool = shm.create_pool(pixels.as_fd(), 8, &qh, ());
    let old = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, &qh, ());
    let new = pool.create_buffer(4, 1, 1, 4, wl_shm::Format::Argb8888, &qh, ());
    let (seen, events) = mpsc::channel();
    let first = seen.clone();
    assert!(window.observe_next_buffer(Box::new(move |feedback| {
        first.send((0, feedback)).unwrap();
    })));
    surface.attach(Some(&old), 0, 0);
    surface.commit();
    connection.flush().unwrap();
    let before = server.captured(0, false);
    assert!(
        events.try_recv().is_err(),
        "submission alone must not certify pixels"
    );
    assert!(window.observe_next_buffer(Box::new(move |feedback| {
        seen.send((1, feedback)).unwrap();
    })));
    surface.attach(Some(&new), 0, 0);
    surface.commit();
    connection.flush().unwrap();
    let after = server.captured(before.1, false);
    assert!(
        after.0 > before.0 && after.1 > before.1,
        "distinct actual buffer capture required"
    );
    server.captured(before.1, true);
    let mut outcomes = std::collections::BTreeMap::new();
    for _ in 0..2 {
        let (tag, result) = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            outcomes.insert(tag, result).is_none(),
            "one-shot buffer feedback"
        );
    }
    assert_eq!(outcomes.get(&0), Some(&WaylandPresentation::Discarded));
    assert_eq!(
        outcomes.get(&1),
        Some(&WaylandPresentation::Presented {
            clock_id: u32::try_from(libc::CLOCK_MONOTONIC).unwrap(),
            timestamp: Duration::from_secs(987),
            refresh: Duration::ZERO,
            sequence: 1234,
            flags: 7,
        })
    );
    assert!(events.try_recv().is_err(), "no duplicate callback");
    old.destroy();
    new.destroy();
    pool.destroy();
    drop(registry);
}

#[test]
fn request_only_feedback_belongs_to_actual_native_buffers_and_discards_superseded_pixels() {
    let (server, socket) = Server::new();
    let server = Arc::new(server);
    let worker_server = server.clone();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("WAYLAND_SOCKET", socket.into_raw_fd().to_string());
    let mut builder = NativeLoop::<Message>::with_user_event();
    builder.with_wayland().with_any_thread(true);
    let event_loop = builder.build().unwrap();
    let proxy = event_loop.create_proxy();
    let (ready, ready_rx) = mpsc::channel::<Arc<Window>>();
    let worker = thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let window = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            verify_native_buffers(&window, &worker_server);
        }));
        let _ = proxy.send_event(Message::Exit);
        result
    });
    let mut handler = WireHandler {
        ready: Some(ready),
        window: None,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop.run_app(&mut handler)
    }));
    let wire = worker.join().unwrap();
    drop(handler);
    drop(server);
    if let Err(panic) = wire {
        std::panic::resume_unwind(panic);
    }
    match result {
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
