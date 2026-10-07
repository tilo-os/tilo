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
    thread: Option<JoinHandle<()>>,
}
impl Server {
    fn new() -> (Self, UnixStream) {
        let (server, client) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (ready, initialized) = mpsc::channel();
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
                display.flush_clients().unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });
        let server = Self {
            stop,
            thread: Some(thread),
        };
        initialized.recv_timeout(Duration::from_secs(5)).unwrap();
        (server, client)
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
        self.worker = Some(thread::spawn(move || {
            present_window.present_with_feedback(Box::new(|_| {}), || {
                entered.send(()).unwrap();
                // A bounded explicit substitute for a backpressured driver;
                // the actual production method owns the window-state guard.
                let _ = release.recv_timeout(Duration::from_secs(5));
            })
        }));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let competing_window = window.clone();
        let competing_entered = self.competing_entered.clone();
        self.competing_worker = Some(thread::spawn(move || {
            competing_window.present_with_feedback(Box::new(|_| {}), || {
                competing_entered.store(true, Ordering::Release);
            })
        }));
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
    let mut handler = Handler {
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
    assert_eq!(competing_supported, Some(true));
    assert!(handler.competing_entered.load(Ordering::Acquire));
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
