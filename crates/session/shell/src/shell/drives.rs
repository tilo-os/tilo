//! NTFS drives in the session: the shell's half of the consent-gated NTFS
//! mount service (`tilo-ntfs-mount`).
//!
//! The service opens a drive only for the shell, and the shell asks it to
//! only after the person answered the shell's own sheet, which no app can
//! draw ([`crate::shell::portal_dialog::DriveConsentRequest`]). The one
//! exception is a drive inside this computer the person chose to remember:
//! it opens when they open it in Files, without asking again, and never
//! before.
//!
//! - A worker watches udev's database for block devices coming and going.
//!   Only while one of them holds NTFS does it ask the service about drives,
//!   so a machine that never sees NTFS never starts the service.
//! - An external drive is asked about as it arrives (or, when it was
//!   already attached, once per session); an internal one only when the
//!   person opens it.
//! - Files lists drives and asks to open one through the tools service
//!   ([`os_actions::ToolIntent::OpenDrive`]); the answer arrives here.
//! - An opened drive opens in Files, which is granted the drive for this
//!   session through the workspace's own grants, like the home folder.
//! - Files does not ask on a timer. It waits (the broker's `drivesChanged`)
//!   on a revision that moves whenever the list it would be given changes,
//!   and then asks for the list once. A machine whose drives stay put never
//!   wakes Files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::watch;

use ntfs_mount::protocol::{
    self, Consent, Drive, Mode, Mounted, Placement, ReadOnlyReason, RememberOffer, Request,
    Response,
};
use os_actions::{DriveEntry, MAX_LISTED_DRIVES};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::portal_dialog::{DriveConsentDecision, DriveConsentRequest, PortalDialogQueue};

/// udev's database: one file per device, rewritten when it changes.
const UDEV_DATA: &str = "/run/udev/data";
/// How long a question to the service may take.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// Opening checks the volume first, in a confined helper.
const OPEN_TIMEOUT: Duration = Duration::from_secs(150);
/// Changes that arrive together (a disk and its partitions) are handled once.
const SETTLE: Duration = Duration::from_millis(400);

/// The ways to open a drive, in the order the sheet lists them.
const CHOICES: [Mode; 2] = [Mode::OpenSafely, Mode::Open];

enum Command {
    Refresh,
    Open {
        drive: String,
        /// Ask even when a remembered choice would open it without asking.
        ask: bool,
    },
}

#[derive(Default)]
struct Shared {
    drives: Mutex<Vec<Drive>>,
    reveals: Mutex<Vec<PathBuf>>,
    notices: Mutex<Vec<Notification>>,
    commands: Mutex<Option<mpsc::Sender<Command>>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(Shared::default)
}

/// Start the worker, once per process. `wake` brings the compositor loop
/// round to show a sheet, post a notice or open Files.
pub(crate) fn start(wake: crate::session_runtime::Wake) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let (commands, receiver) = mpsc::channel();
        *shared().commands.lock_recover() = Some(commands.clone());
        let worker = std::thread::Builder::new()
            .name("ntfs-drives".into())
            .spawn(move || Worker::new(wake).run(&receiver));
        if let Err(error) = worker {
            tracing::warn!(%error, "NTFS drives are unavailable");
            return;
        }
        let watcher = std::thread::Builder::new()
            .name("ntfs-drives-udev".into())
            .spawn(move || watch_udev(Path::new(UDEV_DATA), &commands));
        if let Err(error) = watcher {
            tracing::warn!(%error, "NTFS drives will not notice new disks");
        }
    });
}

/// The drives as Files lists them.
pub fn listed() -> Vec<DriveEntry> {
    entries(&shared().drives.lock_recover())
}

/// `drives` as Files is given them.
fn entries(drives: &[Drive]) -> Vec<DriveEntry> {
    drives
        .iter()
        .take(MAX_LISTED_DRIVES)
        .map(|drive| DriveEntry {
            id: drive.id.clone(),
            label: drive.label.clone(),
            size_bytes: drive.size_bytes,
            internal: drive.placement == Placement::Internal,
            open_at: drive.mounted.as_ref().map(|mounted| mounted.path.clone()),
        })
        .collect()
}

/// When the list Files is given may have changed: a revision that names
/// nothing.
struct ListRevision {
    /// Starts at 1, so a first wait (from 0) answers at once and Files
    /// fetches its first list.
    changes: watch::Sender<u64>,
}

impl ListRevision {
    fn new() -> Self {
        Self {
            changes: watch::Sender::new(1),
        }
    }

    /// Put `drives` in `slot`, and move the revision when the list Files
    /// would be given differs from the one before. A refresh that finds
    /// the same drives moves nothing, so nobody wakes.
    fn replace(&self, slot: &Mutex<Vec<Drive>>, drives: Vec<Drive>) {
        let changed = {
            let mut held = slot.lock_recover();
            let changed = entries(&held) != entries(&drives);
            *held = drives;
            changed
        };
        if changed {
            self.changes
                .send_modify(|revision| *revision = revision.wrapping_add(1).max(1));
        }
    }

    /// Resolves once the revision moves past `after`, with the revision to
    /// wait from next. Subscribed now, not when first polled, so a change
    /// in between is still seen.
    fn wait(&self, after: u64) -> impl std::future::Future<Output = u64> + Send + 'static {
        let mut changes = self.changes.subscribe();
        async move {
            loop {
                let current = *changes.borrow_and_update();
                if current != after {
                    return current;
                }
                if changes.changed().await.is_err() {
                    // The revision outlives every wait and never closes.
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

/// The revision Files' waits follow.
fn revision() -> &'static ListRevision {
    static REVISION: OnceLock<ListRevision> = OnceLock::new();
    REVISION.get_or_init(ListRevision::new)
}

/// Resolves once the list [`listed`] gives may have changed since `after`
/// (0 at first, then what the previous wait returned), with the value to
/// wait from next. While no drive comes, goes, opens or closes it stays
/// pending and costs no wakeup.
pub fn changed(after: u64) -> impl std::future::Future<Output = u64> + Send + 'static {
    revision().wait(after)
}

/// Open `drive`, asking the person first unless a remembered choice
/// applies and the request comes `from_files`: Files asks when the person
/// clicks the drive there, which is the only moment a remembered drive opens
/// without a question. Anything else asking is answered by the sheet.
pub fn request_open(drive: &str, from_files: bool) -> Result<(), String> {
    if !shared()
        .drives
        .lock_recover()
        .iter()
        .any(|known| known.id == drive)
    {
        return Err(t("drives-gone"));
    }
    let commands = shared().commands.lock_recover().clone();
    commands
        .ok_or_else(|| t("drives-unavailable"))?
        .send(Command::Open {
            drive: drive.to_owned(),
            ask: !from_files,
        })
        .map_err(|_| t("drives-unavailable"))
}

/// Notices queued since the last call.
pub fn drain_notices() -> Vec<Notification> {
    std::mem::take(&mut *shared().notices.lock_recover())
}

/// Open each drive opened since the last call in Files, on the compositor
/// thread, where the workspace's grants live. Returns whether anything was
/// opened.
pub(crate) fn pump_reveals(chrome: &Chrome) -> bool {
    let reveals = std::mem::take(&mut *shared().reveals.lock_recover());
    for path in &reveals {
        if let Err(error) = reveal(chrome, path) {
            tracing::warn!(%error, path = %path.display(), "could not show an opened drive");
            let notice = Notification::new(t("drives-app-name"), error)
                .with_severity(NotificationSeverity::Warning)
                .with_app_name(t("drives-app-name"));
            chrome.borrow_mut().push_notification_full(notice);
        }
    }
    !reveals.is_empty()
}

/// Give Files the drive for this session and open it there.
fn reveal(chrome: &Chrome, path: &Path) -> Result<(), String> {
    use crate::shell::search::providers::applications::{
        SYSTEM_FILE_MANAGER, grant_file_manager_path,
    };

    let app = crate::shell::launcher::installed_apps()
        .ok()
        .and_then(|apps| {
            apps.into_iter()
                .find(|app| app.package_id == SYSTEM_FILE_MANAGER)
        })
        .ok_or_else(|| t("drives-no-files"))?;
    let workspace = {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let active = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id)
            .ok_or_else(|| t("drives-no-workspace"))?;
        grant_file_manager_path(&mut tree, active, path.to_path_buf());
        let workspace = tree.workspace_after_opening_app(SYSTEM_FILE_MANAGER);
        tree.queue_save();
        workspace
    };
    let workspace = workspace.ok_or_else(|| t("drives-no-workspace"))?;
    chrome.borrow_mut().bump();
    crate::shell::launcher::queue_document_in_workspace(app, &workspace, path.to_path_buf(), || {})
        .map_err(|error| error.user_message())
}

/// Whether udev knows a block device holding NTFS.
fn ntfs_attached(udev_data: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(udev_data) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name().as_encoded_bytes().first() == Some(&b'b')
            && std::fs::read_to_string(entry.path())
                .is_ok_and(|record| record.lines().any(|line| line == "E:ID_FS_TYPE=ntfs"))
    })
}

/// Ask for a refresh whenever a block device's record in udev's database
/// changes. The worker settles bursts.
fn watch_udev(udev_data: &Path, commands: &mpsc::Sender<Command>) {
    use rustix::fs::inotify;
    let Ok(fd) = inotify::init(inotify::CreateFlags::CLOEXEC) else {
        return;
    };
    let watching = inotify::add_watch(
        &fd,
        udev_data,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::DELETE,
    );
    if watching.is_err() {
        return;
    }
    let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(&fd, &mut buffer);
    loop {
        match reader.next() {
            Ok(event) => {
                let block_device = event
                    .file_name()
                    .is_some_and(|name| name.to_bytes().first() == Some(&b'b'));
                if block_device && commands.send(Command::Refresh).is_err() {
                    return;
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

/// What the person sees about a drive: its label, or what it is.
fn drive_name(drive: &Drive) -> String {
    if drive.label.trim().is_empty() {
        match drive.placement {
            Placement::Internal => t("drives-unnamed-internal"),
            Placement::External => t("drives-unnamed-external"),
        }
    } else {
        drive.label.clone()
    }
}

fn size_text(bytes: u64) -> String {
    ui::format::size(bytes)
}

/// The sheet for `drive`.
pub fn consent_request(drive: &Drive, handle: String) -> DriveConsentRequest {
    let name = drive_name(drive);
    let size = size_text(drive.size_bytes);
    let where_ = match drive.placement {
        Placement::Internal => t_args("drives-consent-internal", &[("size", size.into())]),
        Placement::External => t_args("drives-consent-external", &[("size", size.into())]),
    };
    let remember = match drive.can_remember {
        RememberOffer::AnyMode => Some(t("drives-consent-remember")),
        RememberOffer::OpenSafely => Some(t("drives-consent-remember-safely")),
        RememberOffer::Nothing => None,
    };
    DriveConsentRequest {
        handle,
        title: match (drive.label.trim().is_empty(), drive.placement) {
            (true, Placement::Internal) => t("drives-consent-title-unnamed-internal"),
            (true, Placement::External) => t("drives-consent-title-unnamed-external"),
            (false, _) => t_args("drives-consent-title", &[("drive", name.into())]),
        },
        drive: where_,
        detail: if drive.remembered.is_some() && drive.placement == Placement::External {
            t("drives-consent-detail-known")
        } else {
            t("drives-consent-detail")
        },
        choices: CHOICES
            .iter()
            .map(|mode| match mode {
                Mode::OpenSafely => t("drives-consent-open-safely"),
                Mode::Open => t("drives-consent-open"),
            })
            .collect(),
        preselect: CHOICES
            .iter()
            .position(|mode| *mode == drive.preselect)
            .unwrap_or(0),
        remember,
        confirm_label: t("drives-consent-confirm"),
        cancel_label: t("drives-consent-not-now"),
    }
}

/// The person's answer as the service takes it.
pub(crate) fn consent_from(decision: DriveConsentDecision) -> Option<Consent> {
    match decision {
        DriveConsentDecision::Chosen { choice, remember } => {
            CHOICES.get(choice).map(|mode| Consent {
                mode: *mode,
                remember,
            })
        }
        DriveConsentDecision::NotNow => None,
    }
}

fn read_only_notice(name: &str, reason: ReadOnlyReason) -> Notification {
    let why = match reason {
        ReadOnlyReason::WindowsHibernated => t("drives-read-only-hibernated"),
        ReadOnlyReason::WindowsNotShutDown => t("drives-read-only-not-shut-down"),
        ReadOnlyReason::NeedsCheck => t("drives-read-only-check"),
    };
    Notification::new(
        t_args("drives-read-only-title", &[("drive", name.into())]),
        why,
    )
    .with_severity(NotificationSeverity::Warning)
    .with_app_name(t("drives-app-name"))
}

struct Worker {
    wake: crate::session_runtime::Wake,
    socket: PathBuf,
    udev_data: PathBuf,
    queue: PortalDialogQueue,
    /// External drives already asked about in this session.
    asked: HashSet<String>,
    next_sheet: u64,
}

impl Worker {
    fn new(wake: crate::session_runtime::Wake) -> Self {
        Self {
            wake,
            socket: PathBuf::from(protocol::SOCKET_PATH),
            udev_data: PathBuf::from(UDEV_DATA),
            queue: PortalDialogQueue::global(),
            asked: HashSet::new(),
            next_sheet: 0,
        }
    }

    fn run(mut self, commands: &mpsc::Receiver<Command>) {
        self.refresh();
        while let Ok(command) = commands.recv() {
            let mut opens = Vec::new();
            match command {
                Command::Refresh => {
                    // A disk and its partitions arrive as a burst of records.
                    std::thread::sleep(SETTLE);
                    while let Ok(next) = commands.try_recv() {
                        if let Command::Open { drive, ask } = next {
                            opens.push((drive, ask));
                        }
                    }
                    self.refresh();
                }
                Command::Open { drive, ask } => opens.push((drive, ask)),
            }
            for (drive, ask) in opens {
                self.open_requested(&drive, ask);
                self.refresh();
            }
        }
    }

    fn call(&self, request: Request, timeout: Duration) -> Result<Response, String> {
        protocol::call(&self.socket, request, timeout).map_err(|error| {
            tracing::warn!(%error, "the NTFS mount service did not answer");
            t("drives-unavailable")
        })
    }

    fn notify(&self, notice: Notification) {
        shared().notices.lock_recover().push(notice);
        (self.wake)();
    }

    fn failed(&self, drive: &str, message: &str) {
        self.notify(
            Notification::new(
                t_args("drives-failed-title", &[("drive", drive.into())]),
                message,
            )
            .with_severity(NotificationSeverity::Warning)
            .with_app_name(t("drives-app-name")),
        );
    }

    /// Learn the drives attached now, and ask about external ones that
    /// arrived.
    fn refresh(&mut self) {
        let drives = if ntfs_attached(&self.udev_data) {
            match self.call(Request::Drives, ASK_TIMEOUT) {
                Ok(Response::Drives { drives }) => drives,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let arrived: Vec<Drive> = drives
            .iter()
            .filter(|drive| {
                drive.placement == Placement::External
                    && drive.mounted.is_none()
                    && !self.asked.contains(&drive.id)
            })
            .cloned()
            .collect();
        self.asked
            .retain(|id| drives.iter().any(|drive| drive.id == *id));
        revision().replace(&shared().drives, drives);
        for drive in arrived {
            self.asked.insert(drive.id.clone());
            if let Some(consent) = self.ask(&drive) {
                self.open(&drive.id, Some(consent));
            }
        }
    }

    /// Show the sheet for `drive` and wait for the person.
    fn ask(&mut self, drive: &Drive) -> Option<Consent> {
        self.next_sheet = self.next_sheet.wrapping_add(1);
        let handle = format!("tilo-drive-{}-{}", self.next_sheet, drive.id);
        let answer = self
            .queue
            .enqueue_drive_consent(consent_request(drive, handle))
            .ok()?;
        (self.wake)();
        answer.blocking_recv().ok().and_then(consent_from)
    }

    fn open_requested(&mut self, drive: &str, ask: bool) {
        if !ask {
            return self.open(drive, None);
        }
        let known = shared()
            .drives
            .lock_recover()
            .iter()
            .find(|known| known.id == drive)
            .cloned();
        match known {
            Some(known) if known.mounted.is_none() => {
                if let Some(consent) = self.ask(&known) {
                    self.open(drive, Some(consent));
                }
            }
            // Already open: show it again, without asking.
            Some(_) => self.open(drive, None),
            None => self.failed(&t("drives-unnamed-external"), &t("drives-gone")),
        }
    }

    fn open(&mut self, drive: &str, consent: Option<Consent>) {
        let response = self.call(
            Request::Open {
                drive: drive.to_owned(),
                consent,
            },
            OPEN_TIMEOUT,
        );
        let name = shared()
            .drives
            .lock_recover()
            .iter()
            .find(|known| known.id == drive)
            .map_or_else(|| t("drives-unnamed-external"), drive_name);
        match response {
            Ok(Response::Opened { mounted }) => self.opened(&name, &mounted),
            Ok(Response::NeedsConsent { drive: asked }) if consent.is_none() => {
                if let Some(consent) = self.ask(&asked) {
                    self.open(drive, Some(consent));
                }
            }
            Ok(Response::Failed { message } | Response::Refused { reason: message }) => {
                self.failed(&name, &message);
            }
            Ok(_) => self.failed(&name, &t("drives-unavailable")),
            Err(message) => self.failed(&name, &message),
        }
    }

    fn opened(&self, name: &str, mounted: &Mounted) {
        if let Some(reason) = mounted.read_only {
            shared()
                .notices
                .lock_recover()
                .push(read_only_notice(name, reason));
        }
        shared()
            .reveals
            .lock_recover()
            .push(PathBuf::from(&mounted.path));
        (self.wake)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(placement: Placement, preselect: Mode) -> Drive {
        Drive {
            id: "sdb1:4".into(),
            label: String::new(),
            size_bytes: 64_000_000_000,
            placement,
            hot_added: placement == Placement::External,
            remembered: None,
            needs_consent: true,
            preselect,
            can_remember: if placement == Placement::Internal {
                RememberOffer::AnyMode
            } else {
                RememberOffer::OpenSafely
            },
            mounted: None,
        }
    }

    #[test]
    fn the_sheet_starts_on_the_suggested_way_and_answers_with_the_chosen_one() {
        let stick = consent_request(&drive(Placement::External, Mode::OpenSafely), "h".into());
        assert_eq!(CHOICES[stick.preselect], Mode::OpenSafely);
        assert_eq!(stick.choices.len(), CHOICES.len());
        assert!(stick.remember.is_some());
        let windows = consent_request(&drive(Placement::Internal, Mode::Open), "h".into());
        assert_eq!(CHOICES[windows.preselect], Mode::Open);
        assert_ne!(
            windows.remember, stick.remember,
            "an external drive remembers only Open safely"
        );

        assert_eq!(
            consent_from(DriveConsentDecision::Chosen {
                choice: 1,
                remember: true
            }),
            Some(Consent {
                mode: Mode::Open,
                remember: true
            })
        );
        assert_eq!(consent_from(DriveConsentDecision::NotNow), None);
        assert_eq!(
            consent_from(DriveConsentDecision::Chosen {
                choice: 7,
                remember: false
            }),
            None,
            "a choice the sheet never offered opens nothing"
        );
    }

    #[test]
    fn a_drive_nothing_may_be_remembered_for_offers_no_remember_row() {
        let mut anonymous = drive(Placement::External, Mode::OpenSafely);
        anonymous.can_remember = RememberOffer::Nothing;
        assert_eq!(consent_request(&anonymous, "h".into()).remember, None);
    }

    #[test]
    fn only_a_block_device_holding_ntfs_wakes_the_service() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b8:1"), "E:ID_FS_TYPE=vfat\n").unwrap();
        std::fs::write(dir.path().join("c13:64"), "E:ID_FS_TYPE=ntfs\n").unwrap();
        assert!(!ntfs_attached(dir.path()));
        std::fs::write(
            dir.path().join("b8:17"),
            "S:disk/by-label/W\nE:ID_FS_TYPE=ntfs\n",
        )
        .unwrap();
        assert!(ntfs_attached(dir.path()));
        assert!(!ntfs_attached(&dir.path().join("missing")));
    }

    /// A mount service with one drive, served on a real socket by the
    /// service's own loop; it records what it was asked to mount.
    mod service {
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        use ntfs_mount::grants::GrantStore;
        use ntfs_mount::identity::Identity;
        use ntfs_mount::policy::DriveFacts;
        use ntfs_mount::protocol::Mode;
        use ntfs_mount::service::{
            Backend, Caller, Candidate, Health, MountRequest, Service, server,
        };
        use ntfs_mount::topology::{Controller, Topology};

        pub struct Fake {
            pub mounted: Arc<Mutex<Vec<Mode>>>,
        }

        impl Backend for Fake {
            type Mount = ();
            fn drives(&mut self) -> Result<Vec<Candidate>, String> {
                Ok(vec![Candidate {
                    id: "nvme0n1p3:1".into(),
                    name: "nvme0n1p3".into(),
                    label: "Windows".into(),
                    size_bytes: 1 << 39,
                    facts: DriveFacts {
                        topology: Topology {
                            device_path: "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1/nvme0n1p3".into(),
                            disk: "nvme0n1".into(),
                            controller: Some(Controller::NvmeRootPort),
                            hotplug_link: None,
                            removable: false,
                            virtual_device: false,
                        },
                        identity: Identity {
                            drive: Some("wwn:eui.1".into()),
                            partition: Some("guid".into()),
                            volume: Some(1),
                        },
                        present_at_boot: true,
                        on_install_disk: true,
                    },
                }])
            }
            fn preflight(&mut self, _: &Candidate) -> Result<Health, String> {
                Ok(Health { read_only: None })
            }
            fn mount(&mut self, request: &MountRequest<'_>) -> Result<(PathBuf, ()), String> {
                self.mounted.lock().unwrap().push(request.mode);
                Ok((PathBuf::from("/run/media/person/Windows"), ()))
            }
            fn unmount(&mut self, _: &std::path::Path, (): &mut (), _: bool) -> Result<(), String> {
                Ok(())
            }
            fn alive(&mut self, (): &mut ()) -> bool {
                true
            }
            fn owner_present(&self, _: u32) -> bool {
                true
            }
            fn new_grant_id(&mut self) -> String {
                "g".into()
            }
        }

        /// Serve on `socket` until `stop`, treating every caller as the
        /// person's shell.
        pub fn serve(
            socket: &std::path::Path,
            grants: PathBuf,
            stop: Arc<AtomicBool>,
        ) -> Arc<Mutex<Vec<Mode>>> {
            let mounted = Arc::new(Mutex::new(Vec::new()));
            let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
            let fake = Fake {
                mounted: Arc::clone(&mounted),
            };
            std::thread::spawn(move || {
                let mut service = Service::new(fake, GrantStore::new(grants));
                server::serve(
                    &listener,
                    &mut service,
                    |_| Ok(Caller::Shell { uid: 1000 }),
                    server::Pacing {
                        housekeeping: std::time::Duration::from_secs(60),
                        idle_exit: None,
                    },
                    &stop,
                );
            });
            mounted
        }
    }

    /// Answer the next sheet on `queue` from another thread with `answer`.
    fn answer_next(
        queue: PortalDialogQueue,
        answer: impl FnOnce(&mut crate::shell::portal_dialog::PortalDialogController) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut sheet = crate::shell::portal_dialog::PortalDialogController::new(queue);
            while !sheet.pump() {
                std::thread::sleep(Duration::from_millis(5));
            }
            answer(&mut sheet);
        })
    }

    /// Run `open` on the worker, failing instead of hanging when it waits
    /// on a sheet nobody answers.
    fn without_a_sheet(worker: Worker, open: impl FnOnce(&mut Worker) + Send + 'static) -> Worker {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let mut worker = worker;
            open(&mut worker);
            let _ = done.send(worker);
        });
        tilo_test_env::recv("the drive to open without asking", &finished)
    }

    #[test]
    fn a_drive_is_asked_about_once_then_files_opens_it_without_the_sheet() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let stop = Arc::new(AtomicBool::new(false));
        let mounted = service::serve(&socket, dir.path().join("grants"), Arc::clone(&stop));
        let udev = dir.path().join("udev");
        std::fs::create_dir_all(&udev).unwrap();
        std::fs::write(udev.join("b259:3"), "E:ID_FS_TYPE=ntfs\n").unwrap();
        let queue = PortalDialogQueue::default();
        let mut worker = Worker {
            wake: Arc::new(|| {}),
            socket: socket.clone(),
            udev_data: udev,
            queue: queue.clone(),
            asked: HashSet::new(),
            next_sheet: 0,
        };
        let first = poll_once(&mut Box::pin(changed(0))).expect("a first wait answers at once");
        let mut plugged = Box::pin(changed(first));
        worker.refresh();
        assert!(
            listed()
                .iter()
                .any(|drive| drive.id == "nvme0n1p3:1" && drive.internal)
        );
        let seen = poll_once(&mut plugged).expect("a drive found tells a waiting Files");
        let mut opened = Box::pin(changed(seen));
        assert!(
            !crate::shell::portal_dialog::PortalDialogController::new(queue.clone()).pump(),
            "an internal drive is not asked about until someone opens it"
        );

        // First click in Files: the sheet comes up on Open; the person
        // remembers the drive.
        let answered = answer_next(queue.clone(), |sheet| {
            assert_eq!(CHOICES[sheet.selected()], Mode::Open);
            assert!(sheet.toggle_remember());
            assert!(sheet.confirm());
        });
        worker.open_requested("nvme0n1p3:1", false);
        answered.join().unwrap();
        assert_eq!(*mounted.lock().unwrap(), vec![Mode::Open]);
        // The worker looks again after each open, as `run` does.
        worker.refresh();
        let open = poll_once(&mut opened).expect("an opened drive tells a waiting Files");
        let mut closed = Box::pin(changed(open));
        assert!(drain_reveal_paths().contains(&PathBuf::from("/run/media/person/Windows")));

        // Closed, then clicked in Files again: opens lazily, no sheet.
        let close = Request::Close {
            drive: "nvme0n1p3:1".into(),
        };
        assert_eq!(
            protocol::call(&socket, close.clone(), ASK_TIMEOUT).unwrap(),
            Response::Done
        );
        worker.refresh();
        assert!(
            poll_once(&mut closed).is_some(),
            "a closed drive tells a waiting Files"
        );
        let mut worker = without_a_sheet(worker, |worker| {
            worker.open_requested("nvme0n1p3:1", false);
        });
        assert_eq!(*mounted.lock().unwrap(), vec![Mode::Open, Mode::Open]);

        // Any other app asking gets the sheet, remembered or not; Not now
        // opens nothing.
        assert_eq!(
            protocol::call(&socket, close, ASK_TIMEOUT).unwrap(),
            Response::Done
        );
        let declined = answer_next(queue, |sheet| assert!(sheet.cancel()));
        worker.open_requested("nvme0n1p3:1", true);
        declined.join().unwrap();
        assert_eq!(mounted.lock().unwrap().len(), 2);
        stop.store(true, Ordering::Release);
    }

    fn drain_reveal_paths() -> Vec<PathBuf> {
        std::mem::take(&mut *shared().reveals.lock_recover())
    }

    /// Poll `wait` once: its answer if it has one now.
    fn poll_once(
        wait: &mut std::pin::Pin<Box<impl std::future::Future<Output = u64>>>,
    ) -> Option<u64> {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(wait.as_mut(), &mut cx) {
            std::task::Poll::Ready(change) => Some(change),
            std::task::Poll::Pending => None,
        }
    }

    #[test]
    fn files_hears_once_per_change_to_its_list_and_never_for_a_refresh_alone() {
        type Edit = fn(&mut Vec<Drive>);
        let revision = ListRevision::new();
        let slot = Mutex::new(Vec::new());
        let mut after =
            poll_once(&mut Box::pin(revision.wait(0))).expect("a first wait answers at once");

        let mut wait = Box::pin(revision.wait(after));
        revision.replace(&slot, Vec::new());
        assert_eq!(poll_once(&mut wait), None, "no drives, still none");

        let changes: [(&str, Edit); 5] = [
            ("a stick is plugged in", |drives| {
                drives.push(drive(Placement::External, Mode::OpenSafely));
            }),
            ("it opens", |drives| {
                drives[0].mounted = Some(Mounted {
                    path: "/run/media/person/Stick".into(),
                    mode: Mode::OpenSafely,
                    read_only: None,
                });
            }),
            ("it is renamed", |drives| drives[0].label = "Photos".into()),
            ("it closes", |drives| drives[0].mounted = None),
            ("it is taken out", Vec::clear),
        ];
        let mut drives = Vec::new();
        for (what, edit) in changes {
            edit(&mut drives);
            revision.replace(&slot, drives.clone());
            let change = poll_once(&mut wait).unwrap_or_else(|| panic!("{what}: Files hears"));
            assert_ne!(change, after, "{what}");
            assert_ne!(change, 0, "{what}: never reads as a first wait");
            after = change;
            wait = Box::pin(revision.wait(after));
            revision.replace(&slot, drives.clone());
            assert_eq!(poll_once(&mut wait), None, "{what}: heard once");
        }

        // What Files is not shown moves nothing.
        drives.push(drive(Placement::External, Mode::OpenSafely));
        revision.replace(&slot, drives.clone());
        after = poll_once(&mut wait).expect("plugged in again");
        let mut wait = Box::pin(revision.wait(after));
        drives[0].needs_consent = false;
        drives[0].preselect = Mode::Open;
        revision.replace(&slot, drives);
        assert_eq!(
            poll_once(&mut wait),
            None,
            "how the sheet would ask is not in Files' list"
        );
    }

    #[test]
    fn sizes_read_the_way_drives_are_sold() {
        assert_eq!(size_text(64_000_000_000), "64 GB");
        assert_eq!(size_text(4_700_000_000), "4.7 GB");
    }
}
