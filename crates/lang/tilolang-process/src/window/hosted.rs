//! The window of a tilolang app whose instance a tilolang host runs: presentation alone.
//!
//! The session started this process as the app's window (`TILO_TILOLANG_HOSTED`): the launcher
//! gave it no collaboration connection, so it holds and commits none of the app's data. The
//! app's instance runs in the tilolang host of its workspace and author, on the app's own
//! connection, and sends this window component patches over a patch connection the session
//! hands it over its window link (`ui::window_link::take_patch`). The window shows them through
//! the app window's half (`tilo_app_window::AppWindow`) and sends back what the person did. Its own
//! launch identity stays for its surface and accessibility.
//!
//! Without a session (`tilolang dev`, a capture), there is no host to run the instance: the
//! window runs one in its own process ([`start`]), over the app's document on this device alone,
//! and turns it on its own frames. The instance runs the same either way. On Windows its patch
//! connection is a pipe, carried down to it by the daemon that started it.

use std::cell::RefCell;
#[cfg(unix)]
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};

use crate::TilolangProcess;
use crate::hosted::{Hosted, Spec};
use crate::phrases::Phrases;
use media::playback::OutputState;
use media::playback_bridge::{CollectionOutput, PlaybackBridge};
use media::playback_cast::transport::ProjectorTransport;
use media::playback_sink::SystemSinkFactory;
use tilo_app_window::AppWindow;
use tilolang_tilo::bundle::Bundle;
use ui::widget::DrawChild;
use ui::widget::{AppModel, ModelPath, Widget};

/// Whether this process is the window of an instance a tilolang host runs: a spare window
/// process the session bound to an app ([`super::bound`]), or on a Mac, where there is no spare,
/// a launch of the app's window ([`runtime_portal::process_link::HOSTED_ENV`]).
#[must_use]
pub fn is_hosted() -> bool {
    #[cfg(target_os = "macos")]
    if std::env::var(runtime_portal::process_link::HOSTED_ENV).is_ok_and(|value| value == "1") {
        return true;
    }
    super::bound().is_some()
}

pub use crate::{PatchConnection, patch_connection};

/// The instance of the window being built: the one the shell asked the running process for, or
/// the process's first window's.
#[must_use]
pub fn window_instance() -> Option<u64> {
    let requested = ui::window_link::requested_instance();
    // A Mac launches each window of the app on its own, naming its instance.
    #[cfg(target_os = "macos")]
    let requested = requested.or_else(|| {
        let value = std::env::var("TILO_CONTINUITY_INSTANCE").ok()?;
        value
            .parse()
            .inspect_err(|error| {
                tracing::error!(value, %error, "the window's instance is not a number");
            })
            .ok()
    });
    requested
}

/// Wakes the window whenever its patch connection has something, and only then: a thread waits
/// on the connection, wakes the window once, and waits again only after the window took what
/// arrived. Idle, nothing runs.
///
/// It wakes the component that runs the window now. The window is made by the component shown
/// before the document ([`ui::framework::Opened`]) and taken over by the one the document opens,
/// which closes the first: a wake captured once would go to a closed component, and the window
/// would show what its instance sent only when something else woke it. Every pass of the
/// window points the watch at the component running it ([`Watch::follow`]).
struct Watch {
    /// Marked by the thread when it woke the window; the window resumes it after reading.
    woke: Arc<Woken>,
    /// Set when the instance's end of the connection hung up: what the window waits for will
    /// never come, so it stops waiting for a view that never arrives.
    gone: Arc<std::sync::atomic::AtomicBool>,
    resume: std::sync::mpsc::Sender<()>,
    /// What the thread wakes: the component running the window.
    wake: Arc<Mutex<ui::wake::WakeHandle>>,
    /// Closed when the window goes: the thread stops waiting on the connection, and lets go of
    /// the descriptor it holds, so the instance sees its window close and ends.
    #[cfg(unix)]
    stop: Option<std::io::PipeWriter>,
    /// Set when the window goes: the thread stops waiting on the connection, and lets go of the
    /// pipe it holds, so the instance sees its window close and ends.
    #[cfg(windows)]
    stop: std::sync::Arc<std::os::windows::io::OwnedHandle>,
    /// The thread, joined as the window goes: once it is, the window holds nothing of the
    /// connection, and the instance's next turn finds it closed.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    #[cfg(unix)]
    fn start(fd: OwnedFd, wake: Arc<Mutex<ui::wake::WakeHandle>>) -> Option<Self> {
        let signalled = Arc::new(Woken::default());
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let flag = Arc::clone(&signalled);
        let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hung_up = Arc::clone(&gone);
        let waking = Arc::clone(&wake);
        let (stopped, stop) = std::io::pipe().ok()?;
        let thread = std::thread::Builder::new()
            .name("tilolang-patches".into())
            .spawn(move || {
                loop {
                    let mut poll = [
                        libc::pollfd {
                            fd: stopped.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // SAFETY: two initialized pollfds naming descriptors this thread owns.
                    let ready = unsafe { libc::poll(poll.as_mut_ptr(), 2, -1) };
                    if ready < 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                        {
                            continue;
                        }
                        return;
                    }
                    // The window went: what the thread holds of the connection goes with it.
                    if poll[0].revents != 0 {
                        return;
                    }
                    flag.mark();
                    waking
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .wake();
                    // The host hung up: the window keeps its last frame.
                    if poll[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                        hung_up.store(true, std::sync::atomic::Ordering::Release);
                        return;
                    }
                    if resumed.recv().is_err() {
                        return;
                    }
                }
            })
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    "no thread watches the window's patch connection: it shows what arrives \
                     only when something else wakes it"
                );
            })
            .ok()?;
        Some(Self {
            woke: signalled,
            gone,
            resume,
            wake,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    #[cfg(windows)]
    fn start(pipe: tilo_socket::Stream, wake: Arc<Mutex<ui::wake::WakeHandle>>) -> Option<Self> {
        use std::os::windows::io::{AsHandle as _, FromRawHandle as _};
        let signalled = Arc::new(Woken::default());
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let flag = Arc::clone(&signalled);
        let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hung_up = Arc::clone(&gone);
        let waking = Arc::clone(&wake);
        // SAFETY: no attributes and no name; manual reset, unset.
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
            )
        };
        if event.is_null() {
            tracing::error!(
                error = %std::io::Error::last_os_error(),
                "no event stops the watch of the window's patch connection"
            );
            return None;
        }
        // SAFETY: an event CreateEventW just returned, owned from here.
        let stop = Arc::new(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(event) });
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("tilolang-patches".into())
            .spawn(move || {
                loop {
                    if tilo_socket::link::wait(&[&pipe], &[stopped.as_handle()], None).is_err() {
                        return;
                    }
                    // The window went: what the thread holds of the connection goes with it.
                    if crate::wait::signalled(stopped.as_handle()) {
                        return;
                    }
                    flag.mark();
                    waking
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .wake();
                    // The host hung up: the window keeps its last frame.
                    if pipe.hung_up() {
                        hung_up.store(true, std::sync::atomic::Ordering::Release);
                        return;
                    }
                    if resumed.recv().is_err() {
                        return;
                    }
                }
            })
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    "no thread watches the window's patch connection: it shows what arrives \
                     only when something else wakes it"
                );
            })
            .ok()?;
        Some(Self {
            woke: signalled,
            gone,
            resume,
            wake,
            stop,
            thread: Some(thread),
        })
    }

    /// Whether the instance's end of the connection hung up: nothing more will arrive.
    fn gone(&self) -> bool {
        self.gone.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Wake the component running the window from now on: the one whose pass this is, when it
    /// is another than the one the watch wakes.
    fn follow(&self) {
        let running = ui::wake::WakeHandle::new();
        // A pass outside any component's scope names none to wake.
        if !running.same_open_scope(&running) {
            return;
        }
        let mut wake = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !wake.same_open_scope(&running) {
            *wake = running;
        }
    }

    /// The window took what arrived: wait for more.
    fn took(&self) {
        if self.woke.take() {
            let _ = self.resume.send(());
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        #[cfg(unix)]
        drop(self.stop.take());
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            // SAFETY: the event this watch owns.
            unsafe { windows_sys::Win32::System::Threading::SetEvent(self.stop.as_raw_handle()) };
        }
        // A thread waiting for the window to take what arrived waits no more.
        let _ = self.resume.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What says a host has work for its next turn: its instances' connections and the work they
/// do off the frame wake it through [`Self::waker`], and its runner waits on it.
#[derive(Default)]
pub struct Woken {
    woken: Mutex<bool>,
    signal: Condvar,
}

impl Woken {
    /// What wakes the host: marks it woken.
    pub fn waker(self: &Arc<Self>) -> tilolang_tilo::background::Waker {
        self.waker_and(|| {})
    }

    /// [`Self::waker`], and `then` after each wake: the window the host runs in, woken too.
    pub fn waker_and(
        self: &Arc<Self>,
        then: impl Fn() + Send + Sync + 'static,
    ) -> tilolang_tilo::background::Waker {
        let woken = Arc::clone(self);
        Arc::new(move || {
            woken.mark();
            then();
        })
    }

    fn mark(&self) {
        *self
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.signal.notify_all();
    }

    /// Whether the host was woken since this was last asked.
    pub fn take(&self) -> bool {
        std::mem::take(
            &mut *self
                .woken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Wait until the host is woken, `timeout` at most; without one, until it is.
    pub fn wait(&self, timeout: Option<std::time::Duration>) {
        let woken = self
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match timeout {
            Some(timeout) => drop(
                self.signal
                    .wait_timeout_while(woken, timeout, |woken| !*woken),
            ),
            None => drop(self.signal.wait_while(woken, |woken| !*woken)),
        }
    }
}

/// A tilolang host in this process running the one instance `spec` of `bundle` on `route`, its
/// resources in a store of this device's own kept under `storage` (in memory without), its words
/// read by `phrases`, woken by `wake`. Answers the host
/// and the window's end of the instance's patch connection: what a window shows it through
/// ([`HostedWindow::over`]). The host is the one a session runs, and turns as it does; its
/// instance takes over the later builds `tilolang dev` publishes for it ([`super::updates`]).
///
/// # Errors
/// The app's data cannot be kept, or the patch connection cannot be made.
pub fn start(
    spec: Spec,
    route: crate::connection::Route,
    bundle: Arc<Bundle>,
    phrases: &Rc<Phrases>,
    mut storage: Option<std::path::PathBuf>,
    wake: tilolang_tilo::background::Waker,
) -> Result<(TilolangProcess, PatchConnection), StartError> {
    let port = match storage.take() {
        Some(storage) => {
            tilolang_tilo::resources::LocalPort::at(storage.join(&spec.app).join("resources"))
                .map_err(StartError::Store)?
                .named(spec.app.clone())
        }
        None => tilolang_tilo::resources::LocalPort::in_memory().named(spec.app.clone()),
    };
    let store = tilolang_tilo::resources::ResourceStore::new(Rc::new(port));
    host_over(
        spec,
        (crate::connection::Connection::local(route), store, None),
        bundle,
        phrases,
        wake,
    )
}

/// [`start`] over the workspace's one store: the instance joins `route` on `collaboration`, the
/// connection the device's daemon made for this window and attests as it, signing what it writes
/// as the person `identity` holds, and every record it reads and writes is the workspace's, as
/// an instance a tilolang host runs reads and writes its own. Waits for the workspace to take the
/// join. `identity` must outlive the host.
///
/// # Errors
/// The connection was refused or ended before the workspace took the join, or the patch
/// connection cannot be made.
pub fn start_joined(
    spec: Spec,
    route: crate::connection::Route,
    bundle: Arc<Bundle>,
    phrases: &Rc<Phrases>,
    (collaboration, identity): (
        runtime_portal::process_link::Descriptor,
        &runtime_portal::process_link::Descriptor,
    ),
    content: Option<runtime_portal::process_link::Descriptor>,
    wake: tilolang_tilo::background::Waker,
) -> Result<(TilolangProcess, PatchConnection), StartError> {
    let kind = collab_types::document::ResourceType::new(
        tilolang_tilo::meta::MetaExt::resource_type(&bundle.meta),
    );
    let connection =
        joined(&bundle, &kind, route, (collaboration, identity)).map_err(StartError::Join)?;
    let store = tilolang_tilo::resources::ResourceStore::new(Rc::new(connection.port(None)));
    let content = crate::store_of(content);
    host_over(spec, (connection, store, content), bundle, phrases, wake)
}

/// The connection a window of `bundle`'s app opens: its instance joins `route` as the record
/// kind `kind` on `collaboration`, the connection the device's daemon made for that window and
/// attests as it, signing what it writes as the person `identity` holds. Waits for the workspace
/// to take the join. `identity` must outlive what is opened on it.
///
/// # Errors
/// The app's surface cannot be read, or the connection was refused or ended before the workspace
/// took the join.
pub fn joined(
    bundle: &Bundle,
    kind: &collab_types::document::ResourceType,
    route: crate::connection::Route,
    (collaboration, identity): (
        runtime_portal::process_link::Descriptor,
        &runtime_portal::process_link::Descriptor,
    ),
) -> Result<crate::connection::Connection, String> {
    // Receivers decide each change against the app's surface: its package's own.
    let surface = bundle.surface().map_err(|error| error.to_string())?;
    collab_types::app_surface::register_package(&bundle.meta.app, &surface);
    let manager =
        collaboration::CollaborationManager::join_instance(kind, collaboration, identity, route)
            .map_err(|error| format!("{error:?}"))?
            .finish_join()
            .map_err(|error| error.to_string())?;
    crate::connection::Connection::new(manager)
}

/// The port a device carrying the documents it kept before anyone signed in writes them into the
/// workspace through, as an app's own window would: the window of `bundle`'s app joined to
/// `route` as no one kind, so a record it writes may be of any kind that app's surface declares
/// (the records a window writes are its app's root's). `identity` must outlive the port.
///
/// # Errors
/// As [`joined`].
pub fn joined_port(
    bundle: &Bundle,
    route: crate::connection::Route,
    (collaboration, identity): (
        runtime_portal::process_link::Descriptor,
        &runtime_portal::process_link::Descriptor,
    ),
) -> Result<crate::connection::CallPort, String> {
    let kind = collab_types::document::ResourceType::new(String::new());
    Ok(joined(bundle, &kind, route, (collaboration, identity))?.port(None))
}

/// The host running the one instance `spec` of `bundle` on `connection`, its resources in
/// `store`, its files and its engine's reads in `content`, and the window's end of its patches.
fn host_over(
    spec: Spec,
    (connection, store, content): (
        crate::connection::Connection,
        tilolang_tilo::resources::ResourceStore,
        Option<tilo_content::ContentStore>,
    ),
    bundle: Arc<Bundle>,
    phrases: &Rc<Phrases>,
    wake: tilolang_tilo::background::Waker,
) -> Result<(TilolangProcess, PatchConnection), StartError> {
    let mut host = TilolangProcess::new(Arc::clone(&wake));
    let (ours, theirs) = crate::patch_pair()?;
    let (transport, watched) = crate::patch_transport(ours)?;
    super::updates::start(&spec.app);
    host.follow_builds(&spec.app, super::updates::builds());
    let content = content.unwrap_or_else(crate::hosted::memory_content);
    let hosted = Hosted::new(
        spec,
        connection,
        (store, content.clone()),
        bundle,
        &crate::hosted::Kept {
            content: Some(content),
            ..Default::default()
        },
        transport,
        (phrases, host.formats()),
        wake,
    );
    host.add(hosted, Some(watched));
    Ok((host, theirs))
}

/// Why [`start`] ran no instance.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the app's resources cannot be kept: {0}")]
    Store(String),
    #[error("the app did not join its workspace: {0}")]
    Join(String),
}

/// The longest a capture waits for the instance it runs to show what it stages.
const STAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The host a window runs in its own process ([`start`]), and what wakes it.
struct InProcess {
    host: TilolangProcess,
    /// The app the instance runs.
    app: String,
    woken: Arc<Woken>,
    /// What a search hit or an agent's `open` asked the window to show, for its instance.
    reveals: Rc<RefCell<Vec<String>>>,
    /// The identity connection the instance signs on, when it joined its workspace: it outlives
    /// the host, which is dropped first.
    _identity: Option<runtime_portal::process_link::Descriptor>,
}

impl InProcess {
    /// The host's turns until it has nothing more to take in now. Whether anything changed.
    fn turn(&mut self) -> bool {
        let _phase = ui::tilo_resource::profiling::Phase::start("instance.turn");
        for anchor in self.reveals.borrow_mut().drain(..) {
            if let Some(hosted) = self.host.instances_mut().next() {
                hosted.reveal(anchor);
            }
        }
        let mut changed = false;
        // A turn's work can be what the next one takes in (a change committed, then answered).
        for _ in 0..16 {
            self.woken.take();
            if !self.host.turn() {
                break;
            }
            changed = true;
        }
        changed
    }

    /// The host's turns until it owes nothing: what a capture stages is all shown.
    fn settle(&mut self) {
        let began = std::time::Instant::now();
        while began.elapsed() < STAGE_WAIT {
            if self.turn() {
                continue;
            }
            if !self.host.owes_work() {
                return;
            }
            self.woken
                .wait(Some(STAGE_WAIT.saturating_sub(began.elapsed())));
        }
        tracing::error!(owed = ?self.host.owed(), "the instance did not settle for the capture");
    }

    /// Close the instance as a session's host does when its window goes: its document is written
    /// to disk.
    fn close(mut self) {
        let _phase = ui::tilo_resource::profiling::Phase::start("tl.instance.close");
        let began = std::time::Instant::now();
        while !self.host.is_empty() && began.elapsed() < STAGE_WAIT {
            if !self.host.turn() {
                let _waiting = ui::tilo_resource::profiling::Phase::start("instance.close_wait");
                self.woken.wait(Some(std::time::Duration::from_millis(50)));
            }
        }
    }
}

/// The window, once its patch connection arrived, and what wakes it.
struct Connected {
    view: AppWindow,
    watch: Option<Watch>,
    /// Where the app's playback plays: its `outputs` Collection, applied to the computer's sound
    /// devices and to a television a person casts to.
    playback: PlaybackBridge<SystemSinkFactory, ProjectorTransport>,
    /// What each place is called, by its id, from the outputs the app last sent: a refusal or a
    /// drop is said by the name the person gave it.
    labels: std::collections::BTreeMap<String, String>,
}

/// The person's picker for each instance this process was admitted for, by its instance: the
/// window showing that instance answers its page's file inputs with it. The chooser comes from
/// the session with the instance, so the window cannot open one of its own.
static PAGE_PICKERS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<u64, std::sync::Arc<dyn tilo_app_window::window::PagePicker>>,
    >,
> = std::sync::OnceLock::new();

/// The session's chooser as a window asks it: the files a page's input takes, answered with the
/// person's exact paths, or none when they cancelled.
pub struct SessionPicker(pub std::sync::Arc<dyn tilo_dialogs::FilePicker>);

impl tilo_app_window::window::PagePicker for SessionPicker {
    fn pick(&self, files: &tilo_app_window::window::PageFiles) -> Vec<String> {
        use tilo_dialogs::portal::{FileFilter, MultiOutcome, PickRequest, PortalOutcome};
        /// How long the person may take before the page is told nothing was chosen.
        const WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        let mut request = if files.folder {
            PickRequest::directory(&files.title, "")
        } else if files.several {
            PickRequest::open_multiple(&files.title, "")
        } else {
            PickRequest::open(&files.title, "")
        }
        .thread_name("tilo-page-file-chooser");
        // The input's accepted types narrow what the picker shows; the page still checks what it
        // was given.
        if !files.accept.is_empty() {
            let mut filter = FileFilter::new("Files");
            for accepted in &files.accept {
                filter = filter.mimetype(accepted);
            }
            request = request.filter(filter);
        }
        let paths = if files.several {
            match self.0.pick_multiple(request).recv_timeout(WAIT) {
                Ok(MultiOutcome::Selected(paths)) => paths,
                _ => return Vec::new(),
            }
        } else {
            match self.0.pick(request).recv_timeout(WAIT) {
                Ok(PortalOutcome::Opened(path)) => vec![path],
                _ => return Vec::new(),
            }
        };
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }
}

/// Keep `picker` as the chooser of the instance `instance`: a page its window shows asks the
/// person with this picker.
pub fn keep_page_picker(
    instance: u64,
    picker: std::sync::Arc<dyn tilo_app_window::window::PagePicker>,
) {
    PAGE_PICKERS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(instance, picker);
}

/// The chooser the instance `instance` was admitted with, when it had one.
fn page_picker(
    instance: Option<u64>,
) -> Option<std::sync::Arc<dyn tilo_app_window::window::PagePicker>> {
    PAGE_PICKERS
        .get()?
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&instance?)
        .cloned()
}

/// The window. See the module docs.
pub struct HostedWindow {
    instance: Option<u64>,
    connected: Option<Connected>,
    /// Whether a frame showing the instance's tree was drawn yet.
    tree_drawn: bool,
    /// Whether any frame was drawn yet.
    drawn: bool,
    /// The instance asked for the window to come forward ("Show this window").
    activation: std::cell::Cell<bool>,
    /// Shown until the patch connection arrives: the platform's loading state.
    waiting: ui::widget::Element,
    /// The host running the window's instance in this process, without a session.
    in_process: Option<InProcess>,
    /// The window is opened for whoever works its record (an agent), not for the person: its
    /// page browses as a visitor, with none of the person's cookies, sign-ins or saved logins.
    visitor: bool,
}

impl HostedWindow {
    /// The window of the instance `instance`.
    #[must_use]
    pub fn new(instance: Option<u64>) -> Self {
        if instance.is_none() {
            tracing::error!("a hosted tilolang window has no instance");
        }
        let mut window = Self {
            instance,
            connected: None,
            tree_drawn: false,
            drawn: false,
            activation: std::cell::Cell::new(false),
            waiting: Box::new(ui::widget::Spinner::new()),
            in_process: None,
            visitor: ui::window_link::requested_visitor(),
        };
        window.connect();
        window
    }

    /// Take the patch connection, once the shell sent it.
    fn connect(&mut self) -> bool {
        if self.connected.is_some() {
            return false;
        }
        let Some(instance) = self.instance else {
            return false;
        };
        let Some(patch) = ui::window_link::take_patch(instance, ui::wake::WakeHandle::new()) else {
            return false;
        };
        match patch_connection(patch) {
            Ok(channel) => {
                tilo_startup_metrics::mark("tilolang-patch-connected");
                self.take_channel(channel, Arc::new(Mutex::new(ui::wake::WakeHandle::new())))
            }
            Err(error) => {
                tracing::error!(%error, "a hosted tilolang window's patch connection is unusable");
                false
            }
        }
    }

    /// The window over the patch connection `channel`, whose other end its instance's host
    /// holds: what the session hands it over the window link.
    #[must_use]
    pub fn over(channel: PatchConnection) -> Self {
        let mut window = Self::unconnected();
        window.take_channel(channel, Arc::new(Mutex::new(ui::wake::WakeHandle::new())));
        window
    }

    fn unconnected() -> Self {
        Self {
            instance: None,
            connected: None,
            tree_drawn: false,
            drawn: false,
            activation: std::cell::Cell::new(false),
            waiting: Box::new(ui::widget::Spinner::new()),
            in_process: None,
            visitor: ui::window_link::requested_visitor(),
        }
    }

    /// The window running its instance in this process ([`start`]): `spec` of `bundle` on
    /// `route`, the host keeping the app's data under `storage`. The window turns
    /// the host on its frames, and is woken when the host's work off the frame is ready.
    ///
    /// # Errors
    /// As [`start`].
    pub fn in_process(
        spec: Spec,
        route: crate::connection::Route,
        bundle: Arc<Bundle>,
        storage: Option<std::path::PathBuf>,
    ) -> Result<Self, StartError> {
        let (wake, woken, waker) = Self::wakes();
        let phrases = Rc::new(Phrases::from_env());
        let app = spec.app.clone();
        let (host, channel) = start(spec, route, bundle, &phrases, storage, waker)?;
        Ok(Self::running(host, channel, (app, wake, woken), None))
    }

    /// [`Self::in_process`] over the workspace's one store ([`start_joined`]): the instance joins
    /// `route` on `collaboration`, the connection the device's daemon made for this window, as the
    /// person `identity` holds, so what it keeps is the workspace's and reaches the person's other
    /// devices. What a phone opens once a person's identity has it.
    ///
    /// # Errors
    /// As [`start_joined`].
    pub fn in_process_joined(
        spec: Spec,
        route: crate::connection::Route,
        bundle: Arc<Bundle>,
        (collaboration, identity): (
            runtime_portal::process_link::Descriptor,
            runtime_portal::process_link::Descriptor,
        ),
        content: Option<runtime_portal::process_link::Descriptor>,
    ) -> Result<Self, StartError> {
        let (wake, woken, waker) = Self::wakes();
        let phrases = Rc::new(Phrases::from_env());
        let app = spec.app.clone();
        let (host, channel) = start_joined(
            spec,
            route,
            bundle,
            &phrases,
            (collaboration, &identity),
            content,
            waker,
        )?;
        Ok(Self::running(
            host,
            channel,
            (app, wake, woken),
            Some(identity),
        ))
    }

    /// What wakes a window whose instance runs in this process: the window's handle, what the
    /// host marks, and the waker the host is handed, which does both.
    fn wakes() -> (
        Arc<Mutex<ui::wake::WakeHandle>>,
        Arc<Woken>,
        tilolang_tilo::background::Waker,
    ) {
        let wake = Arc::new(Mutex::new(ui::wake::WakeHandle::new()));
        let woken = Arc::new(Woken::default());
        let waking = Arc::clone(&wake);
        let waker = woken.waker_and(move || {
            waking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .wake();
        });
        (wake, woken, waker)
    }

    /// The window over `host`, which runs its instance in this process and holds the other end of
    /// `channel`; `identity` is the identity connection the instance signs on, kept as long as
    /// the host runs.
    fn running(
        host: TilolangProcess,
        channel: PatchConnection,
        (app, wake, woken): (String, Arc<Mutex<ui::wake::WakeHandle>>, Arc<Woken>),
        identity: Option<runtime_portal::process_link::Descriptor>,
    ) -> Self {
        let mut window = Self::unconnected();
        window.take_channel(channel, wake);
        // The window names the app it runs: the process installs no package, so nothing else here
        // would tell its actions and its surface which app they are.
        if let Some(connected) = &mut window.connected {
            connected.view.set_app_id(app.as_str());
        }
        window.in_process = Some(InProcess {
            host,
            app,
            woken,
            reveals: Rc::default(),
            _identity: identity,
        });
        window
    }

    /// Where a search hit or an agent's `open` asks the window to show what it names: its
    /// instance shows it on the next frame. `None` for a window whose instance runs elsewhere.
    #[must_use]
    pub fn reveals(&self) -> Option<Rc<RefCell<Vec<String>>>> {
        self.in_process
            .as_ref()
            .map(|running| Rc::clone(&running.reveals))
    }

    /// Keep the types of the package with an origin in step with `origin`'s service, in a window
    /// whose instance runs in this process.
    pub fn add_origin(&mut self, origin: tilolang_tilo::origin::SharedOrigin) {
        if let Some(hosted) = self
            .in_process
            .as_mut()
            .and_then(|running| running.host.instances_mut().next())
        {
            hosted.add_origin(origin);
        }
    }

    /// The app the window runs, when it runs one in this process.
    #[must_use]
    pub fn app(&self) -> Option<&str> {
        self.in_process.as_ref().map(|running| running.app.as_str())
    }

    /// The window's half of the app window, once its patch connection arrived.
    #[must_use]
    pub fn view(&self) -> Option<&AppWindow> {
        self.connected.as_ref().map(|connected| &connected.view)
    }

    /// [`Self::view`], to act on: what the person asks of the app at the window.
    pub fn view_mut(&mut self) -> Option<&mut AppWindow> {
        self.connected.as_mut().map(|connected| &mut connected.view)
    }

    /// Send its instance what the window owes it, and wait until the instance sends something the
    /// window has not shown, or hangs up: whether anything watches its connection. An instance
    /// this window runs itself runs on its passes, and is not waited for here.
    #[must_use]
    pub fn wait_for_app(&mut self) -> bool {
        let Some(connected) = self
            .connected
            .as_mut()
            .filter(|_| self.in_process.is_none())
        else {
            return false;
        };
        connected.view.flush();
        let Some(watch) = connected.watch.as_ref() else {
            return false;
        };
        watch.woke.wait(None);
        // The instance hung up without a view: the window says so rather than waiting for one
        // that will never come.
        if watch.gone() {
            connected
                .view
                .app_gone("the app's window closed before it opened");
        }
        true
    }

    /// What the person does from now on serves `call`, the invocation they signed for it as its
    /// JSON, as the member `invoker` names; an empty call ends it
    /// ([`tilo_render_protocol::patch::ToApp::Serving`]). Nothing before the window connected.
    pub fn serve_call(&mut self, call: Vec<u8>, invoker: Option<String>) {
        if let Some(connected) = &mut self.connected {
            connected.view.flush();
            connected
                .view
                .tell(tilo_render_protocol::patch::ToApp::Serving { call, invoker });
            connected.view.flush();
        }
    }

    /// Hand the instance `layout`, the desktop the device shows this window on as its person
    /// reads it there ([`tilo_render_protocol::patch::ToApp::Desktop`]), as a press it answers
    /// once it took it. Nothing before the window connected.
    pub fn hand_desktop(&mut self, layout: String) {
        if let Some(connected) = &mut self.connected {
            connected.view.hand_desktop(layout);
            connected.view.flush();
        }
    }

    /// The layout's actions the instance asked the desktop this window is shown on to carry out
    /// since this was last asked, each `(action, argument)`
    /// ([`tilo_render_protocol::patch::ToUi::Desktop`]).
    pub fn take_desktop_asks(&mut self) -> Vec<(String, String)> {
        self.connected
            .as_mut()
            .map(|connected| connected.view.take_desktop_asks())
            .unwrap_or_default()
    }

    fn take_channel(
        &mut self,
        channel: PatchConnection,
        wake: Arc<Mutex<ui::wake::WakeHandle>>,
    ) -> bool {
        match crate::patch_transport(channel) {
            Ok((transport, watched)) => {
                #[cfg(windows)]
                let watched = match watched.into_stream() {
                    Ok(pipe) => pipe,
                    Err(error) => {
                        tracing::error!(%error, "a hosted tilolang window's patch connection is unusable");
                        return false;
                    }
                };
                let watch = Watch::start(watched, wake);
                let mut view = AppWindow::new(transport, Vec::new());
                if self.visitor {
                    view.browse_as_visitor();
                }
                // A floating surface the view asks for mirrors its app's own player.
                if let Some(app) = super::bound()
                    .map(|bound| bound.app.as_str())
                    .or_else(|| super::installed_bundle().map(|bundle| bundle.meta.app.as_str()))
                {
                    view.set_app_id(app);
                }
                // The page's file inputs ask the person with the chooser the session opened for
                // this instance, the one place a file is chosen and granted. A click in a window an
                // agent works is the agent's, and puts no chooser in front of the person: its page
                // is told nothing was chosen, and files reach it through the page's own action,
                // which names them.
                if !self.visitor
                    && let Some(picker) = page_picker(self.instance)
                {
                    view.set_page_picker(picker);
                }
                self.connected = Some(Connected {
                    view,
                    watch,
                    playback: PlaybackBridge::new(SystemSinkFactory, ProjectorTransport::new()),
                    labels: std::collections::BTreeMap::new(),
                });
                true
            }
            Err(error) => {
                tracing::error!(%error, "a hosted tilolang window's patch connection is unusable");
                false
            }
        }
    }

    /// What arrived, shown; the watch resumed. A request to come forward is kept for the
    /// runner, which brings the window forward.
    fn receive(connected: &mut Connected, activation: &std::cell::Cell<bool>) -> bool {
        if let Some(watch) = &connected.watch {
            watch.follow();
        }
        let received = connected.view.receive();
        // The app's playback changed: its places play where the person put them, on this
        // computer's devices and on a television. A place that refuses is said, and the others
        // keep playing.
        while let Some(outputs) = connected.view.take_outputs() {
            match serde_json::from_slice::<Vec<CollectionOutput>>(&outputs) {
                Ok(outputs) => {
                    connected.labels = outputs
                        .iter()
                        .map(|output| (output.id.clone(), output.label()))
                        .collect();
                    if let Err(failed) = connected.playback.apply(&outputs) {
                        // What the app's view cannot show itself — the system's own doing, not
                        // the app's — is said in the window that plays it.
                        for why in failed {
                            let said = Self::named(connected, &why);
                            connected
                                .view
                                .say_notice(&said, tilo_render_protocol::patch::NoticeKind::Error);
                        }
                    }
                }
                Err(error) => tracing::warn!(%error, "a playback's outputs do not read"),
            }
        }
        // A cast that dropped is started again; one that keeps dropping fails in words.
        let _ = connected.playback.poll();
        for event in connected.playback.events() {
            match (&event.state, &event.detail) {
                (OutputState::Failed, detail) => {
                    let label = Self::named(connected, &event.id);
                    let why = detail.as_deref().unwrap_or("it stopped playing.");
                    let said = format!("{label}: {why}");
                    connected
                        .view
                        .say_notice(&said, tilo_render_protocol::patch::NoticeKind::Error);
                }
                (state, Some(detail)) => {
                    tracing::info!(output = %event.id, ?state, %detail, "a playback output");
                }
                (state, None) => {
                    tracing::info!(output = %event.id, ?state, "a playback output");
                }
            }
        }
        if connected.view.take_activation() {
            activation.set(true);
        }
        if let Some(watch) = &connected.watch {
            watch.took();
        }
        received
    }

    /// What a place is called, as the app last named it: `at` is the place's id, or a failure
    /// of it as `"<id>: why"`, and the reason is kept beside the name.
    fn named(connected: &Connected, at: &str) -> String {
        let (id, why) = at.split_once(": ").unwrap_or((at, ""));
        let label = connected
            .labels
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_owned());
        if why.is_empty() {
            label
        } else {
            format!("{label}: {why}")
        }
    }

    /// Play the sound the window's player decoded since the last frame in every place the
    /// session's outputs name: one stream, every speaker and cast target.
    fn play_sound(connected: &mut Connected) {
        let samples = connected.view.take_audio();
        if !samples.is_empty() {
            connected.playback.write(&samples);
        }
    }

    fn content(&self) -> &(dyn Widget + 'static) {
        match &self.connected {
            Some(connected) => &connected.view,
            None => self.waiting.as_ref(),
        }
    }

    fn content_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.connected {
            Some(connected) => &mut connected.view,
            None => self.waiting.as_mut(),
        }
    }
}

impl Drop for HostedWindow {
    /// A window running its instance in this process closes it as a session's host does: the
    /// patch connection goes first, and the instance ends, its document written to disk.
    fn drop(&mut self) {
        if let Some(running) = self.in_process.take() {
            self.connected = None;
            running.close();
        }
    }
}

/// A window running its instance in this process is the framework's surface of the app: what a
/// capture stages runs in the instance, and is shown once the instance settled.
impl ui::framework::ResourceSurface<super::resource::Workspace> for HostedWindow {
    fn present(&mut self, _state: &Rc<super::resource::WorkspaceState>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        let Some(running) = &mut self.in_process else {
            return;
        };
        // A capture of a scenario that did not run whole pictures nothing it tells: the capture
        // stops on the step that failed, in the words of why ([`take_stage_failure`]).
        if let Some(scenario) = scenario
            && let Some(hosted) = running.host.instances_mut().next()
            && let Err(why) = hosted.stage(scenario)
        {
            STAGE_FAILED.with(|failed| *failed.borrow_mut() = Some(why));
        }
        running.settle();
        // One staged before the device answered ran as the window settled.
        if let Some(why) = running
            .host
            .instances_mut()
            .next()
            .and_then(Hosted::take_stage_failure)
        {
            STAGE_FAILED.with(|failed| *failed.borrow_mut() = Some(why));
        }
    }
}

thread_local! {
    /// Why the scenario a capture staged in the window this thread draws did not run whole,
    /// until the capture takes it. The framework stages a surface on its next frame, with no
    /// answer of its own, and asks the app afterwards whether the scene stands.
    static STAGE_FAILED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Why the scenario the window this thread draws staged did not run whole, taken once: the
/// step that failed, its action and why.
pub(crate) fn take_stage_failure() -> Option<String> {
    STAGE_FAILED.with(|failed| failed.borrow_mut().take())
}

impl ui::component_lifecycle::WindowActivation for HostedWindow {
    fn take_window_activation_intent(&self) -> bool {
        self.activation.replace(false)
    }
}

/// The window's undo and redo (Ctrl+Z, Ctrl+Shift+Z, Edit › Undo) are the document's, and the
/// document is the instance's: they go to the instance, which takes back the person's last step
/// there, typing into a field included, and sends the window what it shows then. The window
/// holds no document, so no undo of its own could take anything back; and a field keeps no
/// history beside the document's.
impl ui::undo::LocalUndo for HostedWindow {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        let Some(connected) = &mut self.connected else {
            // Nothing shown yet: nothing of the person's to take back.
            return false;
        };
        // What the person typed before asking goes first: the undo takes it back too.
        connected.view.flush();
        connected.view.tell(match request {
            ui::undo::UndoRequest::Undo => tilo_render_protocol::patch::ToApp::Undo,
            ui::undo::UndoRequest::Redo => tilo_render_protocol::patch::ToApp::Redo,
        });
        connected.view.flush();
        true
    }
}

/// A change mark in the window asks the shell to show the task run that made the change.
impl ui::component_lifecycle::HostIntents for HostedWindow {
    fn take_process_intent(&mut self) -> Option<ui::widget::AppProcessIntent> {
        let connected = self.connected.as_mut()?;
        let run = connected.view.take_shown_run()?;
        Some(ui::widget::AppProcessIntent::ShowTaskRun { group: run })
    }
}

/// What the instance's connection heard of the people at the app: the window shows it as a window
/// with a connection of its own does.
impl ui::component_lifecycle::HearsElsewhere for HostedWindow {
    fn take_heard(&mut self) -> Vec<ui::component_lifecycle::Heard> {
        self.connected
            .as_mut()
            .map_or_else(Vec::new, |connected| connected.view.take_heard())
    }
}

/// The window is named by what it shows: the item it shows as its page, else the page, the document,
/// the place its instance's composed shell names in its content header. Two windows of the app
/// read apart in the desktop's tabs, the switcher and assistive technology without either being
/// opened.
impl ui::component_lifecycle::WindowTitle for HostedWindow {
    fn window_title(&self) -> Option<&str> {
        self.view().and_then(tilo_app_window::AppWindow::title)
    }
}

/// The window bar's History is the app window's: the instance shows the system's history of what
/// the window shows. The window offers it from the start, so the bar finds it once something shows.
impl ui::widget::WindowHistory for HostedWindow {
    fn set_window_presents_history(&mut self, _tab_presents: bool) {}

    fn window_history_control(&self) -> Option<ui::widget::HistoryControl> {
        self.view()
            .and_then(ui::widget::WindowHistory::window_history_control)
    }

    fn invoke_window_history_control(
        &mut self,
        control: ui::widget::HistoryControl,
    ) -> Result<(), String> {
        let connected = self.connected.as_mut().ok_or("History is unavailable")?;
        ui::widget::WindowHistory::invoke_window_history_control(&mut connected.view, control)?;
        connected.view.flush();
        Ok(())
    }
}

/// A call delivered to this window names one of its app's actions: the app runs it and answers
/// what it made, as it answers the calls a session's broker delivers to it
/// ([`Hosted::run_for_window`]). A person's own choice from the window's palette is their press,
/// which the app runs as it stands, so it keeps the app window's own answer.
impl tilo_ui_base::host_operation::ResourceOperations for HostedWindow {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[tilo_app_window::catalogue::HOSTED_ACTION]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::host_operation::ResourceOperation<'_>,
        doc: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, String> {
        if let Some(started) = self.run_in_app(op)? {
            return Ok(Some(started));
        }
        // Every other operation is the app window's, as it was before the window answered any:
        // its own handler first, then the children it routes to.
        let inner = ui::widget::Decorator::inner_mut(self);
        tilo_ui_base::host_operation::start_operation(&mut *inner, op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &ui::automerge::AutoCommit,
        path: &ui::automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let inner = ui::widget::Decorator::inner_mut(self);
        tilo_ui_base::host_operation::poll_resource_operation(
            &mut *inner,
            invocation,
            cancellation_requested,
            doc,
            path,
        )
    }
}

impl HostedWindow {
    /// What the app makes of a call delivered to this window, when `op` is one of its actions:
    /// `None` for a press the person chose, and for everything the app window answers itself.
    fn run_in_app(
        &mut self,
        op: &tilo_ui_base::host_operation::ResourceOperation<'_>,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, String> {
        let request = op.request();
        if request.type_id != tilo_app_window::catalogue::HOSTED_ACTION
            || collaboration::invocation_scope::delivered_invocation().is_none()
        {
            return Ok(None);
        }
        let chosen: tilo_app_window::catalogue::Chosen = serde_json::from_slice(request.payload)
            .map_err(|error| format!("a delivered action did not read back: {error}"))?;
        // A window whose app runs in a session's host asks that host nothing here: what the app
        // makes of the call is that host's to answer, and this window only shows it.
        let Some(running) = self.in_process.as_mut() else {
            return Ok(None);
        };
        let app = running.app.clone();
        let hosted = running
            .host
            .instances_mut()
            .find(|hosted| hosted.spec().app == app)
            .ok_or_else(|| "the app's instance is not running".to_owned())?;
        let invoker = collaboration::invocation_scope::current_invoker()
            .unwrap_or_else(|| tilo_actions::ActionInvoker::person(hosted.spec().person.as_str()));
        let argument = chosen.argument();
        let address = tilo_actions::ActionAddress::new(chosen.resource, chosen.action);
        let answer = hosted.run_for_window(
            op.invocation(),
            &address,
            argument.as_deref(),
            &invoker,
            None,
        )?;
        Ok(Some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(answer),
        ))
    }
}

/// Until the app's patch connection arrived the window shows its loading state: what the person
/// types waits for the app's first view.
impl ui::widget::AwaitsContent for HostedWindow {
    fn awaits_content(&self) -> bool {
        self.connected
            .as_ref()
            .is_none_or(|connected| ui::widget::AwaitsContent::awaits_content(&connected.view))
    }

    fn awaits_answer(&self) -> bool {
        self.connected
            .as_ref()
            .is_some_and(|connected| ui::widget::AwaitsContent::awaits_answer(&connected.view))
    }
}

/// The window stands for what it shows (the app window's window, or the loading state before it)
/// in the layout and in every walk of the widgets: it adds no node of its own.
impl ui::widget::Decorator for HostedWindow {
    type Inner = dyn Widget;

    // What the window itself answers, and everything else the widget it shows: the app window's
    // window publishes the app's actions for the command palette and does the one the person
    // chooses ([`tilo_app_window::catalogue`]), so a request for either reaches it through here.
    ui::provides!(
        dyn ui::component_lifecycle::WindowActivation,
        dyn ui::component_lifecycle::WindowTitle,
        dyn ui::component_lifecycle::HostIntents,
        dyn ui::undo::LocalUndo,
        dyn ui::component_lifecycle::HearsElsewhere,
        dyn ui::widget::AwaitsContent,
        dyn ui::widget::WindowHistory,
        dyn tilo_ui_base::host_operation::ResourceOperations;
        decorated
    );

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content_mut()
    }

    fn name(&self) -> &'static str {
        "TilolangProcessedWindow"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(connected) = &mut self.connected {
            Self::receive(connected, &self.activation);
            // What the window answered the instance as it took what arrived — keys typed after
            // a press it just heard ran — goes at once, not on a frame that may not come.
            connected.view.flush();
        }
        self.content_mut().sync(state, path);
        if let Some(connected) = &mut self.connected {
            Self::play_sound(connected);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let connected_now = self.connect();
        let received = match &mut self.connected {
            Some(connected) => {
                connected.view.flush();
                // What the person did runs before what it shows is read.
                if let Some(running) = &mut self.in_process {
                    running.turn();
                }
                Self::receive(connected, &self.activation)
            }
            None => false,
        };
        let child = self.content_mut().update(state, path);
        if let Some(connected) = &mut self.connected {
            Self::play_sound(connected);
        }
        // What the window still owes its instance — a range of rows it asked for, keys typed
        // after a press it heard ran — goes on the next pass: the frame that wrote it is not
        // the last one the window needs.
        let owed = self
            .connected
            .as_ref()
            .is_some_and(|connected| connected.view.owes());
        connected_now || received || child || owed
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.content_mut().handle_event(event, ctx);
        // What the person did goes to the instance at once, not on the next frame.
        if let Some(connected) = &mut self.connected {
            connected.view.flush();
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        // What the window's first frames show, for its launch timing: the instance's own
        // tree, or the loading state before it arrived.
        let showing_tree = self
            .connected
            .as_ref()
            .is_some_and(|connected| connected.view.composed().is_some());
        if showing_tree && !self.tree_drawn {
            self.tree_drawn = true;
            tilo_startup_metrics::mark("tilolang-first-tree-drawn");
        } else if !showing_tree && !self.drawn {
            tilo_startup_metrics::mark("tilolang-first-draw-before-tree");
        }
        self.drawn = true;
        self.content_mut()
            .draw_child(scene, transform, text, theme, &env);
    }

    fn needs_idle_sync(&self) -> bool {
        self.in_process
            .as_ref()
            .is_some_and(|running| running.host.owes_work())
            || self.content().needs_idle_sync()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.in_process
            .as_ref()
            .is_some_and(|running| running.host.owes_work())
            || self.content().has_pending_visual_work()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let host = self
            .in_process
            .as_ref()
            .and_then(|running| running.host.wakes_in())
            .and_then(|wait| ui::time::now().checked_add(wait));
        match (host, self.content().next_wake_deadline()) {
            (Some(host), Some(content)) => Some(host.min(content)),
            (host, content) => host.or(content),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        match &mut self.connected {
            Some(connected) => Widget::take_structure_dirty(&mut connected.view),
            None => self.waiting.take_structure_dirty(),
        }
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        match &mut self.connected {
            Some(connected) => Widget::take_structure_change(&mut connected.view),
            None => self.waiting.take_structure_change(),
        }
    }
}

#[cfg(test)]
mod picker_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tilo_dialogs::portal::{
        LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
        OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome,
    };

    use tilo_app_window::window::PagePicker;

    use super::SessionPicker;

    /// A chooser answering what the test says the person chose.
    struct Picks {
        one: Option<PathBuf>,
        many: Vec<PathBuf>,
    }

    impl tilo_dialogs::FilePicker for Picks {
        fn pick(&self, _request: PickRequest) -> PortalChannel {
            let (answers, channel) = std::sync::mpsc::channel();
            let outcome = match &self.one {
                Some(path) => PortalOutcome::Opened(path.clone()),
                None => PortalOutcome::Cancelled,
            };
            let _ = answers.send(outcome);
            channel
        }

        fn pick_multiple(&self, _request: PickRequest) -> MultiChannel {
            let (answers, channel) = std::sync::mpsc::channel();
            let outcome = if self.many.is_empty() {
                MultiOutcome::Cancelled
            } else {
                MultiOutcome::Selected(self.many.clone())
            };
            let _ = answers.send(outcome);
            channel
        }

        fn open_handle(&self, _request: PickRequest) -> OpenFileChannel {
            let (answers, channel) = std::sync::mpsc::channel();
            let _ = answers.send(OpenFileOutcome::Cancelled);
            channel
        }

        fn launch(&self, _request: LaunchRequest) -> LaunchChannel {
            let (answers, channel) = std::sync::mpsc::channel();
            let _ = answers.send(LaunchOutcome::Opened);
            channel
        }
    }

    /// The session's chooser, as a page's input asks it: the person's exact paths, several of
    /// them for a multiple input, and none when they cancelled.
    #[test]
    fn the_sessions_picker_answers_the_pages_files() {
        let one = PathBuf::from("/home/ada/picture.png");
        let picker = SessionPicker(Arc::new(Picks {
            one: Some(one.clone()),
            many: Vec::new(),
        }));
        let files = |several, folder| tilo_app_window::window::PageFiles {
            title: "Upload".to_owned(),
            several,
            folder,
            accept: vec!["image/png".to_owned()],
        };
        assert_eq!(picker.pick(&files(false, false)), ["/home/ada/picture.png"]);
        assert_eq!(picker.pick(&files(false, true)), ["/home/ada/picture.png"]);

        let two = PathBuf::from("/home/ada/second.png");
        let picker = SessionPicker(Arc::new(Picks {
            one: None,
            many: vec![one, two],
        }));
        assert_eq!(
            picker.pick(&files(true, false)),
            ["/home/ada/picture.png", "/home/ada/second.png"]
        );

        let cancelled = SessionPicker(Arc::new(Picks {
            one: None,
            many: Vec::new(),
        }));
        assert!(
            cancelled.pick(&files(false, false)).is_empty(),
            "a cancelled chooser answers no paths"
        );
    }
}

#[cfg(all(test, unix))]
mod watching_tests {
    use super::HostedWindow;

    /// A window whose instance hung up before it sent a view stops waiting for one, and says the
    /// app could not be opened: a page would otherwise wait for a view that never comes.
    #[test]
    fn a_window_whose_instance_hung_up_stops_waiting_and_says_so() {
        let (window_end, instance_end) = crate::patch_pair().expect("a patch connection");
        let mut window = HostedWindow::over(window_end);
        drop(instance_end);
        assert!(window.wait_for_app(), "the window watches its connection");
        assert!(
            !ui::widget::AwaitsContent::awaits_content(&window),
            "the window waits for no view once its instance is gone"
        );
    }
}
