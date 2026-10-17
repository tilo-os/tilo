//! Remembered drives: the NTFS drives the person chose to remember when the
//! shell asked whether, and how, to open them (Windows partitions next to
//! Tilo, external drives).
//!
//! The choices live with the consent-gated NTFS mount service, root-owned,
//! because a remembered choice stands in for the person's answer. Settings
//! asks the service for them over its socket, which the image binds into
//! Settings alone, and may do no more there than the person at the seat:
//! forget a drive, which closes it if it is open, and switch a drive inside
//! this computer between opening safely and the faster kernel driver. It
//! cannot open a drive; only the shell's own prompt can.
//!
//! A change is a device-local [`DrivesEffect`]; its buttons and the Ctrl+K
//! entries build the same value and both reach [`DrivesModel::start`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ntfs_mount::protocol::{self, Mode, Placement, RememberedDrive, Request, Response};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Widget};

use crate::channel;
use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, StatusTone, card, error_caption, item_row, note, row, rows, status_pill,
};

/// How long the service may take to answer; forgetting an open drive waits
/// for its driver to write everything out.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A grant id as the service mints it: hex.
const MAX_GRANT_BYTES: usize = 64;

/// Forget a remembered drive, or change how it opens.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DrivesEffect {
    /// Read the remembered drives again.
    #[effect(job, idempotent, device_only)]
    RefreshDrives,
    /// Forget a remembered drive: it is closed if open, and asked about
    /// again the next time.
    #[effect(job, row, device_only, label = "Forget")]
    ForgetDrive {
        #[param("The remembered drive, as the drive list names it")]
        grant: String,
    },
    /// Open a drive inside this computer safely (a confined driver, a bit
    /// slower) or with the faster kernel driver, from its next opening on.
    #[effect(job, row, idempotent, device_only)]
    SetDriveOpensSafely {
        #[param("The remembered drive, as the drive list names it")]
        grant: String,
        safely: bool,
    },
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "each area receives its owned variant from the shared Settings effect vocabulary"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    operation: DrivesEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.drives().model();
    let mut model = model.borrow_mut();
    if model.busy() {
        return Err(t("settings-drives-busy"));
    }
    model.start(&operation, Some(cx.reply()));
    Ok(Effected::Pending)
}

/// What Settings asks of the mount service, behind a seam for tests and
/// captures.
pub trait DriveService: Send + Sync {
    /// The person's remembered drives.
    ///
    /// # Errors
    /// When the service is unreachable or refuses.
    fn remembered(&self) -> Result<Vec<RememberedDrive>, Unreachable>;

    /// Carry out one change.
    ///
    /// # Errors
    /// The service's reason, for a change it did not make.
    fn change(&self, request: Request) -> Result<(), String>;
}

/// The service could not be asked, or answered something else: on a
/// machine that has never seen an NTFS drive this is the normal state, and
/// the pane shows nothing about drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreachable(pub String);

/// The real service, over its control socket.
pub struct SocketService {
    pub socket: PathBuf,
}

impl SocketService {
    fn call(&self, request: Request) -> Result<Response, String> {
        crate::device_read::off_ui_thread();
        protocol::call(&self.socket, request, CALL_TIMEOUT).map_err(|error| error.to_string())
    }
}

impl DriveService for SocketService {
    fn remembered(&self) -> Result<Vec<RememberedDrive>, Unreachable> {
        match self.call(Request::Remembered).map_err(Unreachable)? {
            Response::Remembered { drives } => Ok(drives),
            Response::Failed { message } | Response::Refused { reason: message } => {
                Err(Unreachable(message))
            }
            _ => Err(Unreachable(t("settings-drives-unexpected"))),
        }
    }

    fn change(&self, request: Request) -> Result<(), String> {
        match self.call(request)? {
            Response::Done => Ok(()),
            Response::Failed { message } | Response::Refused { reason: message } => Err(message),
            _ => Err(t("settings-drives-unexpected")),
        }
    }
}

/// A capture's: the scene's sample drives, and no change ever made.
struct CaptureService(Vec<RememberedDrive>);

impl DriveService for CaptureService {
    fn remembered(&self) -> Result<Vec<RememberedDrive>, Unreachable> {
        Ok(self.0.clone())
    }
    fn change(&self, _: Request) -> Result<(), String> {
        Err("drives are not changed outside a live device host".into())
    }
}

fn valid_grant(grant: &str) -> bool {
    !grant.is_empty()
        && grant.len() <= MAX_GRANT_BYTES
        && grant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The request `operation` makes of the service, and what to say once done.
fn request_for(operation: &DrivesEffect) -> Result<Option<(Request, String)>, String> {
    match operation {
        DrivesEffect::RefreshDrives => Ok(None),
        DrivesEffect::ForgetDrive { grant } if valid_grant(grant) => Ok(Some((
            Request::Forget {
                grant: grant.clone(),
            },
            t("settings-drives-forgotten"),
        ))),
        DrivesEffect::SetDriveOpensSafely { grant, safely } if valid_grant(grant) => Ok(Some((
            Request::SetMode {
                grant: grant.clone(),
                mode: if *safely {
                    Mode::OpenSafely
                } else {
                    Mode::Open
                },
            },
            t(if *safely {
                "settings-drives-now-safely"
            } else {
                "settings-drives-now-fast"
            }),
        ))),
        _ => Err(t("settings-drives-bad-request")),
    }
}

/// The host keeps the drives model when the Storage pane is closed.
pub struct DrivesArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<DrivesModel>>,
}

impl DrivesArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    pub fn model(&self) -> &Shared<DrivesModel> {
        self.model.get_or_init(|| {
            let service: Arc<dyn DriveService> = if self.cx.reaches_devices() {
                Arc::new(SocketService {
                    socket: PathBuf::from(protocol::SOCKET_PATH),
                })
            } else {
                Arc::new(CaptureService(
                    self.cx
                        .scene()
                        .map(|scene| scene.remembered_drives.clone())
                        .unwrap_or_default(),
                ))
            };
            std::rc::Rc::new(std::cell::RefCell::new(DrivesModel::over(
                self.cx.clone(),
                service,
            )))
        })
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll();
        }
    }

    /// One row per remembered drive the pane has read, offering what its
    /// buttons do.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let Some(model) = self.model.get() else {
            return Vec::new();
        };
        let model = model.borrow();
        model
            .shown()
            .iter()
            .map(|drive| {
                let mut row = LiveRow::new(
                    format!("settings.drive:{}", drive.grant),
                    "settings.drive",
                    drive_name(drive),
                )
                .operation(
                    SettingsEffect::from(DrivesEffect::ForgetDrive {
                        grant: drive.grant.clone(),
                    })
                    .bound(),
                );
                if drive.placement == Placement::Internal {
                    row = row.operation(
                        SettingsEffect::from(DrivesEffect::SetDriveOpensSafely {
                            grant: drive.grant.clone(),
                            safely: drive.mode != Mode::OpenSafely,
                        })
                        .bound(),
                    );
                }
                row
            })
            .collect()
    }
}

fn drive_name(drive: &RememberedDrive) -> String {
    if drive.label.trim().is_empty() {
        t(match drive.placement {
            Placement::Internal => "settings-drives-unnamed-internal",
            Placement::External => "settings-drives-unnamed-external",
        })
    } else {
        drive.label.clone()
    }
}

/// The Drives section of Storage.
pub struct DrivesModel {
    cx: PaneCx,
    service: Arc<dyn DriveService>,
    /// `None` until the first read answers.
    drives: Option<Result<Vec<RememberedDrive>, Unreachable>>,
    read_rx: Option<channel::Receiver<Result<Vec<RememberedDrive>, Unreachable>>>,
    change_rx: Option<channel::Receiver<(DrivesEffect, Result<String, String>)>>,
    /// The last change's outcome.
    outcome: Option<Result<String, String>>,
    replies: Replies,
    structure_dirty: bool,
}

impl DrivesModel {
    /// A model over `service`, reading it once.
    pub fn over(cx: PaneCx, service: Arc<dyn DriveService>) -> Self {
        let mut model = Self {
            cx,
            service,
            drives: None,
            read_rx: None,
            change_rx: None,
            outcome: None,
            replies: Replies::default(),
            structure_dirty: false,
        };
        model.request_read();
        model
    }

    fn request_read(&mut self) {
        if self.read_rx.is_some() {
            return;
        }
        let service = Arc::clone(&self.service);
        let (tx, rx) = channel::channel();
        match crate::device_read::thread(
            "tilo-drives-read",
            Box::new(move || {
                let _ = tx.send(service.remembered());
            }),
        ) {
            Ok(()) => self.read_rx = Some(rx),
            Err(error) => self.drives = Some(Err(Unreachable(error.to_string()))),
        }
    }

    /// Start `operation`: what a button and a Ctrl+K entry both reach.
    pub fn start(&mut self, operation: &DrivesEffect, reply: Option<EffectReply>) {
        if let Some(reply) = reply {
            self.replies.wait(operation.id(), reply);
        }
        if self.change_rx.is_some() {
            self.finish(operation, Err(t("settings-drives-busy")));
            return;
        }
        let (request, done) = match request_for(operation) {
            Ok(Some(request)) => request,
            Ok(None) => {
                self.request_read();
                self.finish(operation, Ok(t("settings-drives-refreshed")));
                return;
            }
            Err(error) => {
                self.finish(operation, Err(error));
                return;
            }
        };
        let service = Arc::clone(&self.service);
        let (tx, rx) = channel::channel();
        let sent = operation.clone();
        match crate::device_read::thread(
            "tilo-drives-change",
            Box::new(move || {
                let result = service.change(request).map(|()| done);
                let _ = tx.send((sent, result));
            }),
        ) {
            Ok(()) => {
                self.change_rx = Some(rx);
                self.outcome = None;
                self.structure_dirty = true;
            }
            Err(error) => self.finish(operation, Err(error.to_string())),
        }
    }

    fn finish(&mut self, operation: &DrivesEffect, result: Result<String, String>) {
        self.replies.finish(operation.id(), result.clone());
        if *operation != DrivesEffect::RefreshDrives {
            self.outcome = Some(result);
        }
        self.structure_dirty = true;
    }

    /// Wait for the background work started so far (tests).
    #[cfg(test)]
    fn settle(&mut self) {
        if let Some(rx) = self.change_rx.take()
            && let Ok((operation, result)) = rx.recv()
        {
            self.finish(&operation, result);
            self.request_read();
        }
        if let Some(rx) = self.read_rx.take()
            && let Ok(drives) = rx.recv()
        {
            self.drives = Some(drives);
            self.structure_dirty = true;
        }
    }

    fn poll(&mut self) {
        if let Some(drives) = self.read_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.read_rx = None;
            self.drives = Some(drives);
            self.structure_dirty = true;
        }
        if let Some((operation, result)) = self.change_rx.as_ref().and_then(|rx| rx.try_recv().ok())
        {
            self.change_rx = None;
            self.finish(&operation, result);
            self.request_read();
        }
    }

    fn shown(&self) -> &[RememberedDrive] {
        match &self.drives {
            Some(Ok(drives)) => drives,
            _ => &[],
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // A machine that never remembered a drive, or has no mount service,
        // has nothing to say about drives.
        if self.shown().is_empty() && self.outcome.is_none() {
            return crate::widgets::cards(Vec::new());
        }
        let busy = self.change_rx.is_some();
        let mut body: Vec<Item> = vec![note(&t("settings-drives-intro"))];
        for drive in self.shown() {
            body.extend(self.drive_rows(drive, busy));
        }
        if busy {
            body.push(note(&t("settings-drives-changing")));
        }
        match &self.outcome {
            Some(Ok(message)) => body.push(note(message)),
            Some(Err(message)) => body.push(row(error_caption(&t_args(
                "settings-drives-change-error",
                &[("error", message.as_str().into())],
            )))),
            None => {}
        }
        let sections: Vec<SettingsSection> = vec![card(&t("settings-drives-title"), rows(body))];
        crate::widgets::cards(sections)
    }

    fn drive_rows(&self, drive: &RememberedDrive, busy: bool) -> Vec<Item> {
        let name = drive_name(drive);
        let mut marks = vec![status_pill(
            &t(match drive.placement {
                Placement::Internal => "settings-drives-internal",
                Placement::External => "settings-drives-external",
            }),
            StatusTone::Neutral,
        )];
        if drive.mounted.is_some() {
            marks.push(status_pill(
                &t("settings-drives-open-now"),
                StatusTone::Info,
            ));
        }
        let detail = t(match (drive.placement, drive.mode) {
            (Placement::External, _) => "settings-drives-external-detail",
            (Placement::Internal, Mode::OpenSafely) => "settings-drives-internal-safely-detail",
            (Placement::Internal, Mode::Open) => "settings-drives-internal-fast-detail",
        });
        let forget = self.cx.bind_invocation(
            Button::new(
                &format!("settings.drives.forget.{}", drive.grant),
                &t("settings-drives-forget"),
            )
            .with_enabled(!busy),
            DrivesEffect::ForgetDrive {
                grant: drive.grant.clone(),
            },
        );
        let mut items = vec![item_row(&name, marks, &[detail], vec![forget])];
        if drive.placement == Placement::Internal {
            let grant = drive.grant.clone();
            items.push(
                self.cx
                    .row(t_args(
                        "settings-drives-open-safely",
                        &[("drive", name.into())],
                    ))
                    .described(t("settings-drives-open-safely-desc"))
                    .id(format!("settings.drives.safely.{}", drive.grant))
                    .toggle_effect(
                        &LocalState::new(drive.mode == Mode::OpenSafely),
                        effect(DrivesEffect::SET_DRIVE_OPENS_SAFELY)
                            .map(move |safely: bool| (grant.clone(), safely)),
                    ),
            );
        }
        items
    }
}

impl PaneModel for DrivesModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }
    fn poll(&mut self) -> bool {
        DrivesModel::poll(self);
        std::mem::take(&mut self.structure_dirty)
    }
    fn name(&self) -> &'static str {
        "DrivesCard"
    }
    fn busy(&self) -> bool {
        self.read_rx.is_some() || self.change_rx.is_some()
    }
}

/// A capture's remembered drives: the Windows partition beside Tilo, open
/// now, and a USB drive opened safely before.
pub(crate) fn capture_fixture() -> Vec<RememberedDrive> {
    vec![
        RememberedDrive {
            grant: "fixture-windows".into(),
            label: "Windows".into(),
            placement: Placement::Internal,
            mode: Mode::Open,
            mounted: Some("/run/media/person/Windows".into()),
        },
        RememberedDrive {
            grant: "fixture-stick".into(),
            label: String::new(),
            placement: Placement::External,
            mode: Mode::OpenSafely,
            mounted: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ntfs_mount::grants::GrantStore;
    use ntfs_mount::identity::Identity;
    use ntfs_mount::policy::{Binding, Grant};
    use ntfs_mount::service::{Backend, Caller, Candidate, Health, MountRequest, Service, server};

    const UID: u32 = 1000;

    /// No drives attached; the grants are what the test remembered.
    struct NoDrives;

    impl Backend for NoDrives {
        type Mount = ();
        fn drives(&mut self) -> Result<Vec<Candidate>, String> {
            Ok(Vec::new())
        }
        fn preflight(&mut self, _: &Candidate) -> Result<Health, String> {
            Err("no drives here".into())
        }
        fn mount(&mut self, _: &MountRequest<'_>) -> Result<(std::path::PathBuf, ()), String> {
            Err("no drives here".into())
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
            "unused".into()
        }
    }

    /// The real service over a real socket, as Settings reaches it: a
    /// sandboxed process of the person, so a [`Caller::Person`].
    struct Running {
        socket: PathBuf,
        grants: GrantStore,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        _dir: tempfile::TempDir,
    }

    impl Running {
        fn start(remembered: &[Grant]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("control.sock");
            let grants = GrantStore::new(dir.path().join("grants"));
            grants.save(UID, remembered).unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let halt = Arc::clone(&stop);
            let mut service = Service::new(NoDrives, grants.clone());
            let thread = std::thread::spawn(move || {
                server::serve(
                    &listener,
                    &mut service,
                    |_| Ok(Caller::Person { uid: UID }),
                    server::Pacing {
                        housekeeping: Duration::from_secs(60),
                        idle_exit: None,
                    },
                    &halt,
                );
            });
            Self {
                socket,
                grants,
                stop,
                thread: Some(thread),
                _dir: dir,
            }
        }

        fn service(&self) -> Arc<dyn DriveService> {
            Arc::new(SocketService {
                socket: self.socket.clone(),
            })
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn grant(id: &str, label: &str, internal: bool, mode: Mode) -> Grant {
        let identity = Identity {
            drive: Some(format!("serial:{id}")),
            partition: Some(format!("part-{id}")),
            volume: Some(7),
        };
        Grant {
            id: id.into(),
            label: label.into(),
            mode,
            binding: if internal {
                Binding::Internal {
                    device_path: "devices/pci0000:00/0000:00:17.0/ata1/block/sda/sda3".into(),
                    identity,
                }
            } else {
                Binding::External { identity }
            },
        }
    }

    fn test_cx() -> PaneCx {
        PaneCx::in_file(std::env::temp_dir().join("tilo-drives-test-settings.json"))
    }

    fn texts(widget: &dyn Widget) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![widget];
        while let Some(next) = stack.pop() {
            if let Some(node) = next.accessibility() {
                out.extend(node.name);
                out.extend(node.value);
            }
            stack.extend(next.children());
        }
        out
    }

    #[test]
    fn the_card_lists_what_the_service_remembers() {
        let running = Running::start(&[
            grant("a1", "Windows", true, Mode::Open),
            grant("b2", "", false, Mode::OpenSafely),
        ]);
        let mut model = DrivesModel::over(test_cx(), running.service());
        model.settle();
        let shown = texts(model.compose().as_ref()).join("\n");
        assert!(shown.contains("Windows"), "{shown}");
        assert!(
            shown.contains(&t("settings-drives-unnamed-external")),
            "{shown}"
        );
        assert!(
            shown.contains(&t_args(
                "settings-drives-open-safely",
                &[("drive", "Windows".into())]
            )),
            "an internal drive offers opening safely: {shown}"
        );
        assert!(
            !shown.contains(&t_args(
                "settings-drives-open-safely",
                &[("drive", t("settings-drives-unnamed-external").into())]
            )),
            "an external drive always opens safely: {shown}"
        );
    }

    #[test]
    fn forgetting_and_switching_reach_the_service() {
        let running = Running::start(&[
            grant("a1", "Windows", true, Mode::Open),
            grant("b2", "Stick", false, Mode::OpenSafely),
        ]);
        let mut model = DrivesModel::over(test_cx(), running.service());
        model.settle();

        model.start(
            &DrivesEffect::SetDriveOpensSafely {
                grant: "a1".into(),
                safely: true,
            },
            None,
        );
        model.settle();
        assert_eq!(model.outcome, Some(Ok(t("settings-drives-now-safely"))));
        let stored = running.grants.load(UID).unwrap();
        assert_eq!(stored[0].mode, Mode::OpenSafely);
        assert_eq!(model.shown()[0].mode, Mode::OpenSafely, "read again after");

        // The service refuses the fast driver for an external drive, and
        // the card says so instead of pretending.
        model.start(
            &DrivesEffect::SetDriveOpensSafely {
                grant: "b2".into(),
                safely: false,
            },
            None,
        );
        model.settle();
        assert!(
            matches!(&model.outcome, Some(Err(_))),
            "{:?}",
            model.outcome
        );
        assert_eq!(running.grants.load(UID).unwrap()[1].mode, Mode::OpenSafely);

        model.start(&DrivesEffect::ForgetDrive { grant: "a1".into() }, None);
        model.settle();
        assert_eq!(model.outcome, Some(Ok(t("settings-drives-forgotten"))));
        let left: Vec<String> = running
            .grants
            .load(UID)
            .unwrap()
            .into_iter()
            .map(|grant| grant.id)
            .collect();
        assert_eq!(left, ["b2"]);
        assert_eq!(model.shown().len(), 1);
    }

    #[test]
    fn a_malformed_grant_never_reaches_the_service() {
        assert_eq!(
            request_for(&DrivesEffect::ForgetDrive {
                grant: "../../x".into()
            }),
            Err(t("settings-drives-bad-request"))
        );
        assert!(
            request_for(&DrivesEffect::ForgetDrive {
                grant: String::new()
            })
            .is_err()
        );
    }

    #[test]
    fn no_service_means_nothing_about_drives() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = DrivesModel::over(
            test_cx(),
            Arc::new(SocketService {
                socket: dir.path().join("absent.sock"),
            }),
        );
        model.settle();
        assert!(matches!(model.drives, Some(Err(_))));
        assert!(texts(model.compose().as_ref()).is_empty());
    }

    #[test]
    fn rows_offer_forget_everywhere_and_the_switch_inside_this_computer() {
        let mut app = ui::prelude::TestApp::<crate::SettingsShell>::new().unwrap();
        let running = Running::start(&[
            grant("a1", "Windows", true, Mode::Open),
            grant("b2", "Stick", false, Mode::OpenSafely),
        ]);
        let service = running.service();
        app.surface::<SettingsApp, _>(|app| {
            assert!(
                app.drives().rows().is_empty(),
                "nothing read, nothing offered"
            );
            let mut model = DrivesModel::over(app.host().clone(), Arc::clone(&service));
            model.settle();
            *app.drives().model().borrow_mut() = model;
            let rows = app.drives().rows();
            assert_eq!(rows.len(), 2);
        })
        .unwrap();
        app.effect_by(
            "remote-collaborator",
            &SettingsEffect::from(DrivesEffect::SetDriveOpensSafely {
                grant: "a1".into(),
                safely: true,
            }),
        )
        .expect_err("a remote collaborator changes no drive of this device");
        assert_eq!(running.grants.load(UID).unwrap()[0].mode, Mode::Open);
        app.effect_on(
            "settings.drive:a1",
            &SettingsEffect::from(DrivesEffect::ForgetDrive { grant: "a1".into() }),
        )
        .unwrap();
        app.surface::<SettingsApp, _>(|app| app.drives().model().borrow_mut().settle())
            .unwrap();
        let left = running.grants.load(UID).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "b2");
    }
}
