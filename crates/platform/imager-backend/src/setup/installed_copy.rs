//! Installed-copy continuation receives authority only through the existing
//! pinned Shell / exact root-helper gate. No setup labels or paths mint a grant.
mod dispatcher;
pub use dispatcher::{CopyCommand, CopyEvent};
mod receiver;
use super::{flatpak_transfer::portable, migration::PinnedDirectory, portable_home_media};
use launcher::{
    owned_service::ServiceCancellation,
    retained_view::{RetainedLaunchGate, installed_copy::ReceivedInstalledCopy},
};
use std::{
    fs::File,
    io,
    os::{
        fd::FromRawFd,
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool},
    time::Duration,
};
use zeroize::Zeroizing;
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyPayload {
    pub attempt: String,
    pub home: Option<portable_home_media::Expectation>,
    pub applications: Option<portable::Expectation>,
    pub applications_selected: bool,
}
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn native_home(uid: u32) -> io::Result<PathBuf> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // Native NSS fixes the actual target account independently of request paths
    // or HOME environment variables. A truncated/unavailable result refuses.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if found.is_null() || record.pw_uid != uid || record.pw_dir.is_null() {
        return Err(refused());
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }.to_bytes();
    if bytes.is_empty() || bytes.len() > 4095 {
        return Err(refused());
    }
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    if !path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(refused());
    }
    let pin = PinnedDirectory::open_absolute(&path)?;
    pin.validate_destination()?;
    Ok(path)
}
/// Nonclone receive authority, minted by mutual real root QPE and the pinned
/// Shell's actual owned root helper. Secret/frame/worker custody remain joined.
pub(super) struct AuthenticatedCopyReceive {
    channel: Mutex<ReceivedInstalledCopy>,
    payload: CopyPayload,
    operation: String,
    setup_binding: String,
    target_uid: u32,
    home: PathBuf,
    home_pin: std::sync::Arc<PinnedDirectory>,
    home_package: Option<PathBuf>,
    secret: Zeroizing<Vec<u8>>,
    home_taken: bool,
    applications_taken: bool,
}
impl AuthenticatedCopyReceive {
    fn receive(gate: RetainedLaunchGate, cancellation: ServiceCancellation) -> io::Result<Self> {
        let channel = gate.receive_installed_copy(cancellation)?;
        channel.check()?;
        let declaration = channel.declaration();
        let payload: CopyPayload =
            serde_json::from_slice(&declaration.payload).map_err(io::Error::other)?;
        let attempt = uuid::Uuid::parse_str(&payload.attempt).map_err(io::Error::other)?;
        if attempt.is_nil() || attempt.to_string() != payload.attempt {
            return Err(refused());
        }
        let operation = declaration.operation.clone();
        let setup_binding = declaration.setup_binding.clone();
        let target_uid = declaration.target_uid;
        if let Some(home) = &payload.home {
            home.validate()?;
            if home.operation != operation || home.setup_binding != setup_binding {
                return Err(refused());
            }
        }
        if let Some(applications) = &payload.applications {
            applications.validate()?;
            if applications.setup_binding != setup_binding {
                return Err(refused());
            }
        }
        if payload.home.is_none() && payload.applications.is_none() {
            return Err(refused());
        }
        // Descriptor presence is independently checked on this authenticated
        // channel; a payload cannot claim a file the root never delivered.
        if channel.clone_home()?.is_some() != payload.home.is_some()
            || channel.clone_applications()?.is_some() != payload.applications.is_some()
        {
            return Err(refused());
        }
        let home = native_home(target_uid)?;
        let home_pin = PinnedDirectory::open_absolute(&home)?;
        home_pin.validate_destination()?;
        let home_package = payload.home.as_ref().map(|expected| {
            Path::new("/var/lib/tilo-imager/home-capture/snapshots")
                .join(format!("installed-copy-{operation}-{}", payload.attempt))
                .join("artifacts")
                .join(&expected.filename)
        });
        let secret_file = channel.secret()?;
        let mut secret = Zeroizing::new(vec![
            0u8;
            usize::try_from(secret_file.metadata()?.len())
                .map_err(io::Error::other)?
        ]);
        std::os::unix::fs::FileExt::read_exact_at(&secret_file, &mut secret, 0)?;
        channel.check()?;
        let received = Self {
            channel: Mutex::new(channel),
            payload,
            operation,
            setup_binding,
            target_uid,
            home,
            home_pin,
            home_package,
            secret,
            home_taken: false,
            applications_taken: false,
        };
        received.check()?;
        Ok(received)
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != self.target_uid
            || rustix::process::geteuid().as_raw() != self.target_uid
        {
            return Err(refused());
        }
        self.home_pin.validate_destination()?;
        if native_home(self.target_uid)? != self.home {
            return Err(refused());
        }
        self.channel.lock().map_err(|_| refused())?.check()
    }
    fn stop_fence(
        &self,
    ) -> io::Result<launcher::retained_view::installed_copy::InstalledCopyStopFence> {
        self.check()?;
        self.channel.lock().map_err(|_| refused())?.stop_fence()
    }
    pub(super) fn operation(&self) -> &str {
        &self.operation
    }
    pub(super) fn setup_binding(&self) -> &str {
        &self.setup_binding
    }
    pub(super) fn target_uid(&self) -> u32 {
        self.target_uid
    }
    pub(super) fn home(&self) -> &Path {
        &self.home
    }
    pub(super) fn home_expectation(&self) -> Option<&portable_home_media::Expectation> {
        self.payload.home.as_ref()
    }
    pub(super) fn applications_expectation(&self) -> Option<&portable::Expectation> {
        self.payload.applications.as_ref()
    }
    pub(super) fn applications_selected(&self) -> bool {
        self.payload.applications_selected
    }
    pub(super) fn take_home_file(&mut self) -> io::Result<Option<File>> {
        self.check()?;
        if self.home_taken {
            return Err(refused());
        }
        self.home_taken = true;
        self.channel.lock().map_err(|_| refused())?.clone_home()
    }
    pub(super) fn take_applications_file(&mut self) -> io::Result<Option<File>> {
        self.check()?;
        if self.applications_taken {
            return Err(refused());
        }
        self.applications_taken = true;
        self.channel
            .lock()
            .map_err(|_| refused())?
            .clone_applications()
    }
    pub(super) fn secret(&self) -> &[u8] {
        &self.secret
    }
    pub(super) fn home_package_path(&self) -> io::Result<&Path> {
        self.home_package.as_deref().ok_or_else(refused)
    }
    pub(super) fn receive_command(
        &self,
        budget: Duration,
    ) -> io::Result<Option<dispatcher::CopyCommand>> {
        self.check()?;
        self.channel.lock().map_err(|_| refused())?.receive(budget)
    }
    pub(super) fn send_event(&self, event: &dispatcher::CopyEvent) -> io::Result<()> {
        self.check()?;
        self.channel.lock().map_err(|_| refused())?.send(event)
    }
}
/// Fixed nonroot helper entry on inherited private sockets only. It exposes no
/// source/mount/identity declarations and creates no new public socket lane.
pub fn run_receiver(arguments: impl Iterator<Item = std::ffi::OsString>) -> io::Result<()> {
    let mut arguments = arguments;
    fn descriptor(argument: Option<std::ffi::OsString>) -> io::Result<i32> {
        let argument = argument.ok_or_else(refused)?;
        let text = argument.to_str().ok_or_else(refused)?;
        let fd = text.parse::<i32>().map_err(io::Error::other)?;
        if !(3..=4095).contains(&fd) || fd.to_string() != text {
            return Err(refused());
        }
        Ok(fd)
    }
    let shell = descriptor(arguments.next())?;
    let root = descriptor(arguments.next())?;
    if shell == root || arguments.next().is_some() {
        return Err(refused());
    }
    if rustix::process::getuid().as_raw() == 0 || rustix::process::geteuid().as_raw() == 0 {
        return Err(refused());
    }
    // The handles select transport only; all authority is constructed by the
    // canonical mutual QPE/gate. Both are consumed exactly once by this entry.
    fn socket(fd: i32) -> io::Result<UnixStream> {
        let mut kind = 0i32;
        let mut size = u32::try_from(std::mem::size_of::<i32>()).map_err(io::Error::other)?;
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut size,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if kind != libc::SOCK_STREAM || size != std::mem::size_of::<i32>() as u32 {
            return Err(refused());
        }
        // Valid inherited descriptor; consumed once only after kernel socket
        // type measurement. QPE then authenticates peer and path custody.
        let socket = unsafe { UnixStream::from_raw_fd(fd) };
        socket.peer_addr()?;
        Ok(socket)
    }
    let shell = socket(shell)?;
    let root = socket(root)?;
    let cancellation = ServiceCancellation::default();
    let gate = RetainedLaunchGate::receive(shell, root, &cancellation)?;
    let grant = AuthenticatedCopyReceive::receive(gate, cancellation)?;
    let fence = grant.stop_fence()?;
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let watch_cancel = cancel.clone();
    let watch_stop = stop.clone();
    let watcher = std::thread::Builder::new()
        .name("original-copy-receiver-fence".into())
        .spawn(move || {
            while !watch_stop.load(std::sync::atomic::Ordering::Acquire) {
                if !matches!(fence.cancelled(), Ok(false)) {
                    watch_cancel.store(true, std::sync::atomic::Ordering::Release);
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(50));
            }
        })?;
    let result = dispatcher::run(grant, cancel);
    stop.store(true, std::sync::atomic::Ordering::Release);
    // Join the actual fence before helper exit. Each observation is bounded;
    // a stopped event or disconnected channel cannot stand in for this join.
    while !watcher.is_finished() {
        std::thread::park_timeout(Duration::from_millis(50));
    }
    watcher
        .join()
        .map_err(|_| io::Error::other("Installed copy receiver fence failed"))?;
    result
}
