//! The Backups window over the device's repository: the [`Store`] the
//! view shares with its effect handler (the catalog, where it backs up from
//! and to, the work running on it) and the [`BackupsWindow`] that lays it
//! out.
use crate::actions::{BackupsEffect, Blocked};
use crate::i18n;
use crate::model::MAX_KEPT;
use crate::model::PolicyField;
use crate::model::{Catalog, Filter, RetentionPolicy};
use crate::resource::{BackupLocation, Backups, DemoRepository};
use crate::view::{self, Intent, Intents, JobKind, Order, Page, Setup};
use chrono::NaiveDateTime;
use std::cell::{Cell, RefCell};

/// How often a running job's progress line is read.
const PROGRESS_READ: ui::time::Duration = ui::time::Duration::from_millis(250);
/// The window bar's Back up now ([`BackupsWindow::sync_window_toolbar`]).
const BACK_UP_NOW: &str = "back-up-now";

/// Who raises a job or a notice: this app, in the jobs band and in what
/// wants the person.
fn owner() -> ui::attention::Owner {
    ui::attention::Owner::new("so.tilo.backups", "Backups").with_icon(ui::icons::Icon::Archive)
}
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::LocalState;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Live, Publishes, Setting, ViewCx,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{AppModel, Column, Element, ModalDialog, ModelPath, NavShell, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;
type Picked = ui::portal::PortalOutcome;
#[derive(Clone, Copy)]
enum View {
    Timeline,
    Sources,
    Destinations,
    Schedules,
}
/// Which screen the window shows instead of the catalog, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SetupKey {
    Start,
    ChooseDestination,
    Running(String),
    Unavailable(String),
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ViewKey {
    catalog: u64,
    setup: Option<SetupKey>,
    page: Page,
    selected: Option<String>,
    order: Order,
    job: Option<JobKind>,
    can_act: bool,
    failure: Option<(String, String)>,
}

/// The repository a window shows and the work running on it, shared by the
/// window and the view's effect handler.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent flag (redraw, drawer, a sheet to close or show)"
)]
pub(crate) struct Store {
    intents: Intents,
    /// What the jobs band reports back: the person pressing Stop.
    jobs: ui::jobs::Presses,
    /// What the stack of what wants the person reports back.
    needs: ui::attention::Presses,
    catalog: Catalog,
    catalog_rev: u64,
    catalog_key: String,
    page: Page,
    selected: Option<String>,
    policy: RetentionPolicy,
    retention: [LocalState<f64>; 4],
    order: Order,
    failure: Option<(String, String)>,
    /// Said in the window's bar for a while when the backups' health
    /// changes: all healthy again, or how many now need a look.
    health: ui::widget::toast::Transient<(String, ui::widget::toast::ToastKind)>,
    /// How many backups needed a look when last read; `None` before the
    /// first read, which is the window opening, not news.
    failed_seen: Option<u64>,
    scroll: Rc<Cell<(f32, f32)>>,
    dirty: bool,
    open_drawer: bool,
    source_path: Option<PathBuf>,
    destination_path: Option<PathBuf>,
    repository_error: Option<String>,
    dispatcher: Option<Dispatcher<Backups>>,
    /// What the order picker runs, once the window takes part in the
    /// resource.
    order_binding: Option<ui::tilo_actions::ActionBinding<String>>,
    location: Option<Setting<BackupLocation>>,
    loaded: BackupLocation,
    work: Work,
    job_progress: JobProgress,
    operation_status: Option<String>,
    job_status: Option<view::Job>,
    job_kind: Option<JobKind>,
    ui: UiCache,
    /// What the window says in a toast next.
    toasts: Vec<(String, ToastKind)>,
    /// The restore sheet closes: a folder to restore into was chosen.
    close_restore: bool,
    /// A capture shows the restore sheet.
    show_restore: bool,
}

/// The [`Store`] as the window and the effect handler share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

/// The window: the repository in a [`NavShell`], with the restore sheet
/// over it while it is open.
pub struct BackupsWindow {
    repo: Shared,
    /// The policy as the document holds it, shown as it changes; `None` for
    /// a window outside the framework.
    live: Option<Live<Backups>>,
    presented: Option<(u64, u64)>,
    shell: NavShell,
    restore: Option<ModalDialog<Column>>,
    restore_changed: bool,
    built: Option<ViewKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    Idle,
    /// A folder chooser is open.
    Choosing,
    /// A backup, verification or restore is running.
    Running,
}

/// What a running job last reported, written by its worker and shown by the
/// progress banner.
type JobProgress = Arc<Mutex<Option<String>>>;

/// A backup, verification or restore under way: the progress line the page
/// reads, the job the window's band shows (with its Stop), and the flag
/// between them, so pressing Stop ends the work instead of hiding it.
struct Run {
    progress: JobProgress,
    job: ui::jobs::Running,
    stopped: Arc<std::sync::atomic::AtomicBool>,
}

fn report(progress: &JobProgress, status: String) {
    if let Ok(mut shown) = progress.lock() {
        *shown = Some(status);
    }
}

/// Local view state. It is intentionally never read from or written to the
/// collaborative document: another device's active modal or local repository
/// selection must not navigate this device or trigger a restore prompt.
struct UiCache {
    view: View,
    sel_source: Option<String>,
    sel_snapshot: Option<String>,
    sel_dest: Option<String>,
    policy: RetentionPolicy,
}

impl Default for UiCache {
    fn default() -> Self {
        Self {
            view: View::Timeline,
            sel_source: None,
            sel_snapshot: None,
            sel_dest: Some("pool".into()),
            policy: RetentionPolicy::default(),
        }
    }
}

fn pick_folder(title: &str, accept: &str) -> Picked {
    ui::portal::PickRequest::directory(title, accept)
        .thread_name("backups-folder-picker")
        .send()
        .recv()
        .unwrap_or_else(|_| {
            ui::portal::PortalOutcome::Failed("The folder chooser closed unexpectedly".into())
        })
}
fn now() -> NaiveDateTime {
    ui::clock::local_now().naive_local()
}

/// A verification under way, for a capture: the window's band says what it is
/// doing, how far through it is, and offers Stop, exactly as a live one does.
fn sample_job() {
    if ui::jobs::is_active() {
        return;
    }
    ui::jobs::clear();
    let job = ui::jobs::start(
        ui::jobs::Spec::new("Verifying")
            .subject(ui::jobs::Subject::new(owner(), "Workspace".to_owned()))
            .unit(ui::jobs::Unit::Files)
            .phase("Verifying")
            .stoppable(|| {}),
    );
    job.progress(642, 1_502);
}
impl Store {
    fn with_catalog(catalog: Catalog) -> Self {
        let policy = RetentionPolicy::default();
        Self {
            intents: Intents::default(),
            needs: ui::attention::Presses::new(),
            catalog,
            catalog_rev: 0,
            catalog_key: String::new(),
            dispatcher: None,
            order_binding: None,
            location: None,
            loaded: BackupLocation::default(),
            work: Work::Idle,
            job_progress: JobProgress::default(),
            operation_status: None,
            job_kind: None,
            ui: UiCache::default(),
            page: Page::All,
            selected: None,
            policy,
            retention: [
                LocalState::new(f64::from(policy.hourly)),
                LocalState::new(f64::from(policy.daily)),
                LocalState::new(f64::from(policy.weekly)),
                LocalState::new(f64::from(policy.monthly)),
            ],
            order: Order::NewestFirst,
            failure: None,
            health: ui::widget::toast::Transient::new(),
            failed_seen: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            dirty: true,
            open_drawer: false,
            source_path: None,
            destination_path: None,
            repository_error: None,
            job_status: None,
            toasts: Vec::new(),
            close_restore: false,
            show_restore: false,
            jobs: ui::jobs::Presses::new(),
        }
    }
    pub(crate) fn is_configured(&self) -> bool {
        self.source_path.is_some()
            && self.destination_path.is_some()
            && self.repository_error.is_none()
    }

    /// Take part in the resource: edits and effects go through the
    /// dispatcher, and the repository shown is this device's (a live
    /// window) or the scenario's (a capture).
    fn attach(&mut self, cx: &ViewCx<Backups>) {
        self.dispatcher = Some(cx.dispatcher());
        self.order_binding = cx.actions().effect_binding(BackupsEffect::ORDER_SNAPSHOTS);
        if let Some(demo) = cx.fixture::<DemoRepository>() {
            self.catalog = demo.catalog.clone();

            self.source_path.clone_from(&demo.location.source);
            self.destination_path.clone_from(&demo.location.destination);
            self.repository_error.clone_from(&demo.error);
        } else if cx.mode().is_live() {
            let location = cx.settings::<BackupLocation>();
            self.load_location(location.get());
            self.location = Some(location);
        }
    }

    /// Show the retention policy the document holds.
    pub(crate) fn present_policy(&mut self, policy: RetentionPolicy) {
        if self.policy == policy {
            return;
        }
        self.ui.policy = policy;
        self.policy = policy;
        for (state, value) in
            self.retention
                .iter()
                .zip([policy.hourly, policy.daily, policy.weekly, policy.monthly])
        {
            state.set(f64::from(value));
        }
    }

    /// Load the repository at `location`. A saved destination that is gone
    /// (an unplugged drive) is reported, never created: otherwise it could
    /// look like a fresh repository and receive a backup in the wrong place.
    fn load_location(&mut self, location: BackupLocation) {
        self.loaded = location.clone();
        self.source_path = location.source;
        self.destination_path = location.destination;
        self.repository_error = None;
        self.catalog = Catalog::empty();
        if let (Some(source), Some(destination)) = (&self.source_path, &self.destination_path) {
            if destination.is_dir() {
                match crate::engine::list_snapshots(destination) {
                    Ok(manifests) => {
                        self.catalog = Catalog::from_local(source, destination, &manifests);
                    }
                    Err(error) => {
                        self.repository_error =
                            Some(format!("Couldn’t read the saved backup location: {error}"));
                    }
                }
            } else {
                self.repository_error = Some(
                    "Couldn’t read the saved backup location: it is not mounted or is no longer a folder."
                        .into(),
                );
            }
        }
    }

    /// Reload the repository after a job changed it.
    fn reload_repository(&mut self) -> Result<(), String> {
        let (Some(source), Some(destination)) = (&self.source_path, &self.destination_path) else {
            return Ok(());
        };
        let manifests = crate::engine::list_snapshots(destination).map_err(|error| {
            let error = format!("Its repository could not be reloaded: {error}");
            self.repository_error = Some(error.clone());
            error
        })?;
        self.catalog = Catalog::from_local(source, destination, &manifests);

        Ok(())
    }

    /// Adjust one retention class by `delta`, within `[0, MAX_KEPT]`: the
    /// field edit at its path, which both the on-screen +/- buttons and the
    /// keyboard arrow / `+` / `-` handlers submit.
    fn bump_retention(&self, field: PolicyField, delta: i64) {
        let count = (i64::from(field.of(&self.ui.policy)) + delta).clamp(0, i64::from(MAX_KEPT));
        let count = u32::try_from(count).unwrap_or_default();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(field.path(&Backups::paths().policy).set(count));
        }
    }

    /// Invoke `effect` through the door, as the palette and agents do.
    fn invoke(&self, effect: &BackupsEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    fn toast_now(&mut self, msg: impl Into<String>) {
        self.toasts.push((msg.into(), ToastKind::Success));
        self.dirty = true;
    }

    /// Errors must read as errors — a green success toast after a rejected
    /// restore is actively misleading and can make recovery unsafe.
    fn toast_error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.failure = Some(("Backup paused".into(), msg.clone()));
        self.toasts.push((msg, ToastKind::Error));
        self.dirty = true;
    }

    /// Why the repository effects cannot run here now, if they cannot.
    pub(crate) fn blocked(&self) -> Option<Blocked> {
        if self.work != Work::Idle {
            return Some(Blocked {
                code: "busy",
                reason: "A backup operation is already running; wait for it to finish.".into(),
            });
        }
        if let Some(error) = &self.repository_error {
            return Some(Blocked {
                code: "not_configured",
                reason: format!("{error} Choose another backup location to continue."),
            });
        }
        if !self.is_configured() {
            return Some(Blocked {
                code: "not_configured",
                reason: "Choose a source and destination before running a backup".into(),
            });
        }
        None
    }

    /// The effects that cannot run on this device right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if let Some(blocked) = self.blocked() {
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::BACKUP_NOW,
                blocked.code,
                blocked.reason.clone(),
            ));
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::VERIFY,
                blocked.code,
                blocked.reason.clone(),
            ));
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::RESTORE_SNAPSHOT,
                blocked.code,
                blocked.reason,
            ));
        }
        if self.work != Work::Idle {
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::SETUP,
                "busy",
                "A backup operation is already running; wait for it to finish.",
            ));
        }
        statuses
    }

    fn has_snapshot(&self, id: &str) -> bool {
        self.catalog
            .snapshots
            .iter()
            .any(|snapshot| snapshot.id == id)
    }

    /// Answer one effect, from a button, the palette, an agent or a script.
    pub(crate) fn answer_backend(
        &mut self,
        effect: BackupsEffect,
        me: &Shared,
        cx: &mut EffectCx<Backups>,
    ) -> EffectAnswer {
        match effect {
            BackupsEffect::Setup => {
                if self.work != Work::Idle {
                    return Err(
                        "A backup operation is already running; wait for it to finish.".into(),
                    );
                }
                // A saved location that can no longer be read keeps its
                // source: only a new destination is chosen.
                if self.source_path.is_some() && !self.is_configured() {
                    Ok(self.choose_destination(me, cx))
                } else {
                    Ok(self.choose_source(me, cx))
                }
            }
            BackupsEffect::BackupNow => {
                if let Some(blocked) = self.blocked() {
                    self.toast_error(blocked.reason.clone());
                    return Err(blocked.reason);
                }
                Ok(self.start_backup(me, cx))
            }
            BackupsEffect::Verify { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.ui.sel_snapshot = Some(snapshot_id.clone());
                Ok(self.start_verify(snapshot_id, me, cx))
            }
            BackupsEffect::RestoreSnapshot { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.ui.sel_snapshot = Some(snapshot_id.clone());
                Ok(self.choose_restore_folder(snapshot_id, me, cx))
            }
            BackupsEffect::ShowSource { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That source is unavailable on this device.".into());
                }
                self.ui.sel_source = Some(source_id);
                self.ui.sel_snapshot = None;
                self.ui.view = View::Sources;
                Ok(Effected::done())
            }
            BackupsEffect::ShowDestination { destination_id } => {
                if !self
                    .catalog
                    .destinations
                    .iter()
                    .any(|destination| destination.id == destination_id)
                {
                    return Err("That destination is unavailable on this device.".into());
                }
                self.ui.sel_dest = Some(destination_id);
                self.ui.view = View::Destinations;
                Ok(Effected::done())
            }
            BackupsEffect::ReadSnapshot { snapshot_id } => {
                let snapshot = self
                    .catalog
                    .snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == snapshot_id)
                    .ok_or("That snapshot is unavailable in this local repository.")?;
                Ok(Effected::value(serde_json::json!({
                    "id": snapshot.id,
                    "source": snapshot.source_id,
                    "destination": snapshot.dest_id,
                    "taken": snapshot.taken.to_string(),
                    "label": snapshot.label,
                    "kind": snapshot.kind.label(),
                    "size_bytes": snapshot.size_bytes,
                    "delta_bytes": snapshot.delta_bytes,
                    "files_changed": snapshot.files_changed,
                    "status": format!("{:?}", snapshot.status),
                    "failure": snapshot.failure,
                    "files": snapshot.files.len(),
                })))
            }
            BackupsEffect::ShowSnapshot { snapshot_id } => {
                if !self.has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.ui.sel_snapshot = Some(snapshot_id);
                self.ui.view = View::Timeline;
                Ok(Effected::done())
            }
            BackupsEffect::OrderSnapshots { order } => {
                self.order = Order::of(&order);
                self.dirty = true;
                Ok(Effected::done())
            }
            BackupsEffect::ShowSchedule { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That schedule's source is unavailable on this device.".into());
                }
                self.ui.sel_source = Some(source_id);
                self.ui.view = View::Schedules;
                Ok(Effected::done())
            }
        }
    }

    /// Whether a verification or restore of `snapshot_id` may start now.
    fn repository_ready(&mut self, snapshot_id: &str) -> Result<(), String> {
        if let Some(blocked) = self.blocked() {
            self.toast_error(blocked.reason.clone());
            return Err(blocked.reason);
        }
        if !self.has_snapshot(snapshot_id) {
            return Err("That snapshot is unavailable in this local repository.".into());
        }
        Ok(())
    }

    fn choose_source(&mut self, me: &Shared, cx: &mut EffectCx<Backups>) -> Effected {
        self.work = Work::Choosing;
        let me = Rc::clone(me);
        cx.thread(
            |_| pick_folder("Choose a folder to protect", "Protect folder"),
            move |(), picked, cx| {
                let mut app = me.borrow_mut();
                app.work = Work::Idle;
                match picked {
                    Picked::Opened(path) => {
                        app.source_path = Some(path);
                        Ok(app.choose_destination(&me, cx))
                    }
                    Picked::Cancelled => Err("No folder was chosen; nothing was set up.".into()),
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        Err(error)
                    }
                }
            },
        )
    }

    fn choose_destination(&mut self, me: &Shared, cx: &mut EffectCx<Backups>) -> Effected {
        self.work = Work::Choosing;
        let me = Rc::clone(me);
        cx.thread(
            |_| pick_folder("Choose a backup location", "Use this location"),
            move |(), picked, cx| {
                let mut app = me.borrow_mut();
                app.work = Work::Idle;
                let destination = match picked {
                    Picked::Opened(path) => path,
                    Picked::Cancelled => {
                        return Err("No location was chosen; nothing was set up.".into());
                    }
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        return Err(error);
                    }
                };
                let Some(source) = app.source_path.clone() else {
                    return Err("Choose a folder to protect first.".into());
                };
                if let Err(error) = crate::engine::validate_paths(&source, &destination) {
                    app.toast_error(error.to_string());
                    return Err(error.to_string());
                }
                let location = BackupLocation {
                    source: Some(source),
                    destination: Some(destination),
                };
                if let Some(setting) = &app.location
                    && let Err(error) = setting.set(location.clone())
                {
                    let error = format!("Couldn’t save the backup location: {error}");
                    app.toast_error(error.clone());
                    return Err(error);
                }
                app.load_location(location);
                Ok(app.start_backup(&me, cx))
            },
        )
    }

    /// Begin a job: the page's progress row, and the window's band, which
    /// offers Stop. `subject` names what it works on, so Stop and "Show me"
    /// point at the same thing the person chose.
    fn start_job(&mut self, kind: JobKind, subject: &str, status: &str) -> Run {
        self.work = Work::Running;
        self.job_progress = JobProgress::new(Mutex::new(Some(status.to_owned())));
        self.operation_status = Some(status.to_owned());
        self.job_kind = Some(kind);
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let job = ui::jobs::start(
            ui::jobs::Spec::new(kind.title())
                .subject(ui::jobs::Subject::new(owner(), subject.to_owned()))
                .unit(ui::jobs::Unit::Files)
                .phase(kind.phase())
                .stoppable({
                    let stopped = Arc::clone(&stopped);
                    move || stopped.store(true, std::sync::atomic::Ordering::Release)
                }),
        );
        Run {
            progress: Arc::clone(&self.job_progress),
            job,
            stopped,
        }
    }

    fn finish_job(&mut self) {
        self.work = Work::Idle;
        self.operation_status = None;
    }

    /// End `run` the way its result says: stopped by the person, failed with
    /// its reason, or done. A stop is recorded in the ledger as something
    /// that happened, because the person asked for it rather than it failing.
    fn end_job(
        kind: JobKind,
        run: &Run,
        result: &Result<(), crate::engine::BackupError>,
        done: &str,
    ) {
        match result {
            Ok(()) => run.job.done(done.to_owned()),
            Err(error) if error.is_stopped() => {
                run.job.stopped();
                ui::attention::publish(ui::attention::Notice::did(
                    owner(),
                    format!("Stopped {}", kind.title().to_lowercase()),
                ));
            }
            Err(error) => run.job.failed(error.to_string()),
        }
    }

    fn start_backup(&mut self, me: &Shared, cx: &mut EffectCx<Backups>) -> Effected {
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return Effected::done();
        };
        let policy = cx.live().get().policy;
        let subject = self.source_title();
        let run = self.start_job(JobKind::Backup, &subject, "Preparing files…");
        let shown = Arc::clone(&run.progress);
        let job = run.job.clone();
        let stopped = Arc::clone(&run.stopped);
        let me = Rc::clone(me);
        cx.thread(
            move |progress| {
                let asked = || stopped.load(std::sync::atomic::Ordering::Acquire);
                let stop = crate::engine::Stop::on(&asked);
                let created = crate::engine::create_snapshot(&source, &destination, stop, |step| {
                    report(
                        &shown,
                        format!("{} files totaling {}", step.files, ui::format::size(step.bytes)),
                    );
                    job.seen(step.files);
                    progress.report("Copying", Some(step.files), None);
                })?;
                progress.report("Removing snapshots the policy no longer keeps", None, None);
                job.phase("Removing snapshots the policy no longer keeps");
                let pruned = crate::engine::prune_snapshots(&destination, &policy)
                    .map_err(|error| error.to_string());
                Ok::<_, crate::engine::BackupError>((created, pruned))
            },
            move |(), result, _| {
                let mut app = me.borrow_mut();
                app.finish_job();
                let (manifest, pruned) = match result {
                    Ok(both) => both,
                    Err(error) if error.is_stopped() => {
                        // The person stopped it: nothing failed, so nothing
                        // is reported as a failure. The ledger says what
                        // happened instead.
                        Store::end_job(JobKind::Backup, &run, &Err(error), "");
                        return Ok(Effected::done());
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        Store::end_job(JobKind::Backup, &run, &Err(error), "");
                        app.toast_error(reason.clone());
                        return Err(reason);
                    }
                };
                if let Err(error) = app.reload_repository() {
                    // The backup itself succeeded; it is the history that
                    // could not be read back here.
                    Store::end_job(
                        JobKind::Backup,
                        &run,
                        &Err(crate::engine::BackupError::InvalidSnapshot(error.clone())),
                        "",
                    );
                    app.toast_error(
                        "Backup finished, but its history could not be reopened. Choose the location again before another operation.",
                    );
                    return Err(error);
                }
                let done = format!(
                    "Backed up {} files totaling {}.",
                    manifest.files,
                    ui::format::size(manifest.bytes)
                );
                Store::end_job(JobKind::Backup, &run, &Ok(()), &done);
                // Pruning deletes snapshots: say which ones went, so history
                // never shrinks without a word.
                match &pruned {
                    Ok(removed) if removed.is_empty() => app.toast_now(format!(
                        "Backup complete: {} files totaling {}.",
                        manifest.files,
                        ui::format::size(manifest.bytes)
                    )),
                    Ok(removed) => {
                        let dates: Vec<String> = removed
                            .iter()
                            .map(|id| app.snapshot_when(id))
                            .collect();
                        app.toast_now(format!(
                            "Backup complete: {} files totaling {}. Removed {} older snapshots the policy no longer keeps: {}.",
                            manifest.files,
                            ui::format::size(manifest.bytes),
                            removed.len(),
                            dates.join(", ")
                        ));
                    }
                    Err(error) => app.toast_error(format!(
                        "Backup complete, but older snapshots could not be removed: {error}"
                    )),
                }
                Ok(Effected::value(serde_json::json!({
                    "snapshot": manifest.id,
                    "files": manifest.files,
                    "bytes": manifest.bytes,
                    "removed": pruned.unwrap_or_default(),
                })))
            },
        )
    }

    /// The name the person knows this backup's source by, for the job's
    /// subject and its Stop.
    fn source_title(&self) -> String {
        self.source_path.as_ref().map_or_else(
            || "this folder".to_owned(),
            |path| {
                path.file_name().map_or_else(
                    || path.to_string_lossy().into_owned(),
                    |name| name.to_string_lossy().into_owned(),
                )
            },
        )
    }

    /// When the snapshot `id` was taken, as the person's clock reads it: a
    /// snapshot the policy removed is no longer in the catalog, so its id
    /// (the second it was taken) answers instead.
    fn snapshot_when(&self, id: &str) -> String {
        if let Some(snapshot) = self.catalog.snapshots.iter().find(|s| s.id == id) {
            return crate::actions::taken_label(snapshot);
        }
        id.split('-')
            .next()
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .map(|seconds| ui::format::timestamp(&ui::format::UnixSecs(seconds)))
            .filter(|when| !when.is_empty())
            .unwrap_or_else(|| id.to_owned())
    }

    fn start_verify(&mut self, id: String, me: &Shared, cx: &mut EffectCx<Backups>) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        let subject = self.snapshot_when(&id);
        let run = self.start_job(JobKind::Verify, &subject, "Checking every file…");
        let shown = Arc::clone(&run.progress);
        let job = run.job.clone();
        let stopped = Arc::clone(&run.stopped);
        let me = Rc::clone(me);
        cx.thread(
            move |progress| {
                let asked = || stopped.load(std::sync::atomic::Ordering::Acquire);
                let stop = crate::engine::Stop::on(&asked);
                crate::engine::verify_snapshot(&destination, &id, stop, |step| {
                    report(
                        &shown,
                        match step.of {
                            Some(total) => format!(
                                "{} of {} files checked, {}",
                                step.files,
                                total,
                                ui::format::size(step.bytes)
                            ),
                            None => format!(
                                "{} files checked totaling {}",
                                step.files,
                                ui::format::size(step.bytes)
                            ),
                        },
                    );
                    if let Some(total) = step.of {
                        job.progress(step.files, total);
                        progress.report("Verifying", Some(step.files), Some(total));
                    } else {
                        job.seen(step.files);
                        progress.report("Verifying", Some(step.files), None);
                    }
                })
            },
            move |(), result, _| {
                let mut app = me.borrow_mut();
                app.finish_job();
                let result = match result {
                    Ok(result) => result,
                    Err(error) if error.is_stopped() => {
                        Store::end_job(JobKind::Verify, &run, &Err(error), "");
                        return Ok(Effected::done());
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        Store::end_job(JobKind::Verify, &run, &Err(error), "");
                        app.toast_error(reason.clone());
                        return Err(reason);
                    }
                };
                let done = format!(
                    "Verified {} files totaling {}.",
                    result.files,
                    ui::format::size(result.bytes)
                );
                Store::end_job(JobKind::Verify, &run, &Ok(()), &done);
                app.toast_now(done);
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
    }

    fn choose_restore_folder(
        &mut self,
        id: String,
        me: &Shared,
        cx: &mut EffectCx<Backups>,
    ) -> Effected {
        self.work = Work::Choosing;
        let me = Rc::clone(me);
        cx.thread(
            |_| pick_folder("Restore into an empty folder", "Restore here"),
            move |(), picked, cx| {
                let mut app = me.borrow_mut();
                app.work = Work::Idle;
                match picked {
                    // The sheet promises a destination chooser, not a
                    // background restore: it closes only once a folder was
                    // chosen, and the engine never overwrites one.
                    Picked::Opened(path) => {
                        app.close_restore = true;
                        Ok(app.start_restore(id, path, &me, cx))
                    }
                    Picked::Cancelled => Err("No folder was chosen; nothing was restored.".into()),
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        Err(error)
                    }
                }
            },
        )
    }

    fn start_restore(
        &mut self,
        id: String,
        restore_to: PathBuf,
        me: &Shared,
        cx: &mut EffectCx<Backups>,
    ) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        let subject = self.snapshot_when(&id);
        let run = self.start_job(JobKind::Restore, &subject, "Restoring and checking files…");
        let shown = Arc::clone(&run.progress);
        let job = run.job.clone();
        let stopped = Arc::clone(&run.stopped);
        let me = Rc::clone(me);
        cx.thread(
            move |progress| {
                let asked = || stopped.load(std::sync::atomic::Ordering::Acquire);
                let stop = crate::engine::Stop::on(&asked);
                crate::engine::restore_snapshot(
                    &destination,
                    &id,
                    &restore_to,
                    false,
                    stop,
                    |step| {
                        report(
                            &shown,
                            match step.of {
                                Some(total) => format!(
                                    "{} of {} files restored, {}",
                                    step.files,
                                    total,
                                    ui::format::size(step.bytes)
                                ),
                                None => format!(
                                    "{} files totaling {}",
                                    step.files,
                                    ui::format::size(step.bytes)
                                ),
                            },
                        );
                        if let Some(total) = step.of {
                            job.progress(step.files, total);
                            progress.report("Restoring", Some(step.files), Some(total));
                        } else {
                            job.seen(step.files);
                            progress.report("Restoring", Some(step.files), None);
                        }
                    },
                )
            },
            move |(), result, _| {
                let mut app = me.borrow_mut();
                app.finish_job();
                let result = match result {
                    Ok(result) => result,
                    Err(error) if error.is_stopped() => {
                        Store::end_job(JobKind::Restore, &run, &Err(error), "");
                        return Ok(Effected::done());
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        Store::end_job(JobKind::Restore, &run, &Err(error), "");
                        app.toast_error(reason.clone());
                        return Err(reason);
                    }
                };
                let done = format!(
                    "Restored {} files totaling {}.",
                    result.files,
                    ui::format::size(result.bytes)
                );
                Store::end_job(JobKind::Restore, &run, &Ok(()), &done);
                app.toast_now(done);
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
    }
}
impl Store {
    pub(crate) fn answer_effect(
        &mut self,
        effect: BackupsEffect,
        me: &Shared,
        cx: &mut EffectCx<Backups>,
    ) -> EffectAnswer {
        // The order is the window's alone: it shows the page it showed.
        if matches!(effect, BackupsEffect::OrderSnapshots { .. }) {
            return self.answer_backend(effect, me, cx);
        }
        self.job_kind = match &effect {
            BackupsEffect::BackupNow => Some(JobKind::Backup),
            BackupsEffect::Verify { .. } => Some(JobKind::Verify),
            BackupsEffect::RestoreSnapshot { .. } => Some(JobKind::Restore),
            _ => self.job_kind,
        };
        let result = self.answer_backend(effect, me, cx);
        self.page = match self.ui.view {
            View::Sources | View::Schedules => self
                .ui
                .sel_source
                .as_ref()
                .map_or(Page::All, |id| Page::Source(id.clone())),
            View::Destinations => self
                .ui
                .sel_dest
                .as_ref()
                .map_or(Page::All, |id| Page::Destination(id.clone())),
            View::Timeline => Page::All,
        };
        self.selected.clone_from(&self.ui.sel_snapshot);
        self.dirty = true;
        result
    }
    fn refresh_runtime(&mut self) {
        // Stop and "read" are the registry's own; "show me" is already here,
        // because a snapshot job's subject is the page this app is showing.
        for press in self.jobs.take() {
            let _ = ui::jobs::apply(press);
        }
        for press in self.needs.take() {
            let _ = ui::attention::apply(press);
        }
        if let Some(location) = self.location.as_ref().map(Setting::get)
            && location != self.loaded
            && self.work != Work::Running
        {
            self.load_location(location);
        }
        if self.work == Work::Running
            && let Ok(shown) = self.job_progress.lock()
        {
            self.operation_status.clone_from(&shown);
        }
        match (&self.operation_status, self.job_kind) {
            (Some(line), Some(kind)) => {
                if let Some(job) = self.job_status.as_ref().filter(|job| job.kind == kind) {
                    job.line.borrow_mut().clone_from(line);
                } else {
                    self.job_status = Some(view::Job::new(kind, line));
                }
            }
            _ => self.job_status = None,
        }
        let key = format!("{:?}", self.catalog);
        if key != self.catalog_key {
            self.catalog_key = key;
            self.catalog_rev += 1;
        }
    }
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "status" => {
                let failed = self.failed_count();
                self.say_health(failed);
            }
            "progress" => {
                self.operation_status = Some("642 of 1,502 files checked, 3.8 GB".into());
                self.job_kind = Some(JobKind::Verify);
                sample_job();
            }
            "failure" => self.toast_error(
                "Backup paused: the destination is unavailable. Reconnect it, then retry.",
            ),
            "restore" => {
                self.keep_selection();
                self.show_restore = true;
            }
            "schedules" => self.page = Page::Retention,
            "sources" => {
                if let Some(source) = self.catalog.sources.first() {
                    self.page = Page::Source(source.id.clone());
                }
            }
            "destinations" => {
                if let Some(dest) = self.catalog.destinations.first() {
                    self.page = Page::Destination(dest.id.clone());
                }
            }
            "rail" => self.open_drawer = true,
            _ => {}
        }
        self.refresh_runtime();
        self.dirty = true;
    }
    /// The newest snapshot of the page, unless one is chosen there already.
    fn keep_selection(&mut self) {
        if !self.page.lists_snapshots() {
            return;
        }
        let filter = self.page.filter();
        let still_listed = self
            .selected
            .as_deref()
            .and_then(|id| self.catalog.snapshot(id))
            .is_some_and(|snapshot| filter.admits(snapshot));
        if !still_listed {
            self.selected = self
                .catalog
                .timeline(filter)
                .first()
                .map(|snapshot| snapshot.id.clone());
        }
    }

    fn setup_key(&self) -> Option<SetupKey> {
        if let Some(error) = &self.repository_error {
            return Some(SetupKey::Unavailable(error.clone()));
        }
        if self.is_configured()
            || !self.catalog.sources.is_empty()
            || !self.catalog.destinations.is_empty()
        {
            return None;
        }
        if let Some(job) = &self.job_status {
            return Some(SetupKey::Running(job.line.borrow().clone()));
        }
        Some(if self.source_path.is_some() {
            SetupKey::ChooseDestination
        } else {
            SetupKey::Start
        })
    }

    fn can_act(&self) -> bool {
        self.blocked().is_none()
    }

    fn view_key(&self) -> ViewKey {
        ViewKey {
            catalog: self.catalog_rev,
            setup: self.setup_key(),
            page: self.page.clone(),
            selected: self.selected.clone(),
            order: self.order,
            job: self.job_status.as_ref().map(|job| job.kind),
            can_act: self.can_act(),
            failure: self.failure.clone(),
        }
    }

    fn failed_count(&self) -> u64 {
        self.catalog.failed(Filter::All) as u64
    }

    /// "All backups healthy", or how many need a look, in the window's bar
    /// for a while.
    fn say_health(&mut self, failed: u64) {
        // A count of failed snapshots is a measurement; "all healthy" is not
        // a claim this app can make while health is unknown or no source is
        // configured.
        let text = if self.catalog.sources.is_empty() {
            "Backup health is unknown: no source is configured.".to_owned()
        } else if failed == 0 {
            i18n::t("backups-health-ok")
        } else {
            i18n::counted("backups-health-attention", failed, "")
        };
        let kind = if failed == 0 && !self.catalog.sources.is_empty() {
            ui::widget::toast::ToastKind::Success
        } else {
            ui::widget::toast::ToastKind::Warning
        };
        self.health.show((text, kind));
    }

    /// Say the health when it changes, and keep the bar's notice current.
    fn publish_health(&mut self) {
        if self.setup_key().is_none() {
            let failed = self.failed_count();
            if self.failed_seen.is_some_and(|seen| seen != failed) {
                self.say_health(failed);
            }
            self.failed_seen = Some(failed);
        }
        self.health.expire();
        self.health
            .publish(|(text, kind)| ui::window_toolbar::ToolbarNotice::new(text.clone(), *kind));
    }
}
impl BackupsWindow {
    /// A window over `repo`.
    fn new(repo: Store, live: Option<Live<Backups>>) -> Self {
        // What runs, across the window's foot: it stays whatever page the
        // window shows, so a backup outlives the page that started it and its
        // Stop is reachable from every page.
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_window_toolbar()
            .with_id("backups-shell")
            .bottom_band(Box::new(ui::jobs::Band::with_presses(repo.jobs.clone())));
        let mut window = Self {
            repo: Rc::new(RefCell::new(repo)),
            live,
            presented: None,
            shell,
            restore: None,
            restore_changed: false,
            built: None,
        };
        window.follow_document();
        window
    }

    /// A window outside the framework over `catalog`, which changes nothing.
    #[cfg(test)]
    fn with_catalog(catalog: Catalog) -> Self {
        Self::new(Store::with_catalog(catalog), None)
    }

    /// Show the retention policy the document holds when it changed.
    fn follow_document(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let Some(shown) = live.changed_since(&mut self.presented) else {
            return;
        };
        let policy = shown.policy;
        self.repo.borrow_mut().present_policy(policy);
    }

    /// Bring the window in step with the repository: what it asked to say,
    /// the restore sheet it opened or closed, and the columns.
    fn show_repository(&mut self) {
        let (toasts, close, open) = {
            let mut repo = self.repo.borrow_mut();
            (
                std::mem::take(&mut repo.toasts),
                std::mem::take(&mut repo.close_restore),
                std::mem::take(&mut repo.show_restore),
            )
        };
        for (message, kind) in toasts {
            self.shell.toast(message, kind);
        }
        if close {
            self.restore = None;
            self.restore_changed = true;
        }
        if open {
            self.open_restore();
        }
        self.refresh_view();
    }

    fn open_restore(&mut self) {
        let repo = Rc::clone(&self.repo);
        let mut repo = repo.borrow_mut();
        let Some(snapshot) = repo
            .selected
            .as_deref()
            .and_then(|id| repo.catalog.snapshot(id))
        else {
            return;
        };
        self.restore_changed = true;
        self.restore = Some(
            view::restore_dialog(snapshot, &repo.catalog, now(), &repo.intents)
                .with_primary_enabled(repo.blocked().is_none()),
        );
        repo.dirty = true;
    }

    fn drain_intents(&mut self) {
        let repo = Rc::clone(&self.repo);
        let mut repo = repo.borrow_mut();
        let mut open_restore = false;
        for intent in repo.intents.take() {
            repo.dirty = true;
            match intent {
                Intent::Navigate(page) => {
                    repo.page = page;
                    repo.selected = None;
                    repo.scroll.set((0.0, 0.0));
                }
                Intent::SelectSnapshot(id) => {
                    repo.selected = Some(id);
                    if !self.shell.is_inspector_open() {
                        self.shell.toggle_inspector();
                    }
                }
                Intent::BackUpNow => repo.invoke(&BackupsEffect::BackupNow),
                Intent::Verify => {
                    if let Some(id) = &repo.selected {
                        repo.invoke(&BackupsEffect::Verify {
                            snapshot_id: id.clone(),
                        });
                    }
                }
                Intent::OpenRestore => open_restore = true,
                Intent::ChooseRestoreFolder => {
                    if let Some(id) = &repo.selected {
                        repo.invoke(&BackupsEffect::RestoreSnapshot {
                            snapshot_id: id.clone(),
                        });
                    }
                }
                Intent::CloseRestore => {
                    self.restore = None;
                    self.restore_changed = true;
                }
                Intent::Setup => repo.invoke(&BackupsEffect::Setup),
                Intent::DismissFailure => repo.failure = None,
            }
        }
        for ((field, current), retained) in [
            (PolicyField::Hourly, repo.policy.hourly),
            (PolicyField::Daily, repo.policy.daily),
            (PolicyField::Weekly, repo.policy.weekly),
            (PolicyField::Monthly, repo.policy.monthly),
        ]
        .into_iter()
        .zip(repo.retention.clone())
        {
            let value = retained.get().clamp(0.0, 999.0) as i64;
            if value != i64::from(current) {
                repo.bump_retention(field, value - i64::from(current));
                retained.set(f64::from(current));
            }
        }
        drop(repo);
        if open_restore {
            self.open_restore();
        }
    }
    /// Rebuild the columns when what they show changed.
    fn refresh_view(&mut self) {
        let repo = Rc::clone(&self.repo);
        let mut repo = repo.borrow_mut();
        repo.keep_selection();
        let key = repo.view_key();
        if self.built.as_ref() == Some(&key) {
            return;
        }
        let sidebar_stale = self
            .built
            .as_ref()
            .is_none_or(|built| built.catalog != key.catalog || built.setup != key.setup);
        self.built = Some(key.clone());
        repo.dirty = true;
        if let Some(setup) = &key.setup {
            let state = match setup {
                SetupKey::Start => Setup::Start,
                SetupKey::ChooseDestination => Setup::ChooseDestination,
                SetupKey::Running(line) => Setup::Running(line),
                SetupKey::Unavailable(error) => Setup::Unavailable(error),
            };
            self.shell.set_sidebar(None);
            self.shell.set_inspector(None);
            self.shell.set_content(view::setup(&state, &repo.intents));
            return;
        }
        if sidebar_stale {
            self.shell.set_sidebar(Some(Box::new(view::sidebar(
                &repo.catalog,
                &repo.page,
                &repo.intents,
            ))));
        } else if let Some(sidebar) = self.shell.sidebar_mut::<ui::widget::NavList>() {
            let selected = repo.page.nav_id();
            if sidebar.selected_id().as_deref() != Some(selected.as_str()) {
                sidebar.set_selected(Some(&selected));
            }
        }
        let now = now();
        let model = view::Model {
            catalog: &repo.catalog,
            page: &repo.page,
            selected: repo.selected.as_deref(),
            now,
            order: repo.order,
            order_binding: repo.order_binding.as_ref(),
            job: repo.job_status.as_ref(),
            failure: repo
                .failure
                .as_ref()
                .map(|(title, message)| (title.as_str(), message.as_str())),
        };
        let content = view::content(&model, &repo.intents, &repo.scroll, &repo.retention);
        self.shell.set_content(content);
        let details = repo
            .selected
            .as_deref()
            .filter(|_| repo.page.lists_snapshots())
            .and_then(|id| repo.catalog.snapshot(id))
            .map(|snapshot| {
                view::snapshot_details(
                    snapshot,
                    &repo.catalog,
                    now,
                    key.can_act && key.job.is_none(),
                    &repo.intents,
                )
            });
        self.shell.set_inspector(details);
    }

    /// Declare the window bar's slots ([`ui::window_toolbar`]): the shell's
    /// own (the sources list, Back, the snapshot's details), the page as the
    /// window's location and Back up now; and run what the person did there.
    fn sync_window_toolbar(&mut self) {
        let repo = Rc::clone(&self.repo);
        let repo = repo.borrow();
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ui::window_toolbar::ToolbarEvent::Action { id } = &event
                && id == BACK_UP_NOW
            {
                repo.intents.push(Intent::BackUpNow);
            }
        }
        let mut toolbar = self.shell.window_toolbar();
        if repo.setup_key().is_none() {
            toolbar.location = Some(ui::window_toolbar::ToolbarLocation {
                label: view::page_title(&repo.catalog, &repo.page),
                ..ui::window_toolbar::ToolbarLocation::default()
            });
            let job = repo.job_status.as_ref().map(|job| job.kind);
            let back_up = ui::window_toolbar::ToolbarAction::new(
                BACK_UP_NOW,
                "upload",
                if job == Some(JobKind::Backup) {
                    "Backing up…"
                } else {
                    "Back up now"
                },
            )
            .enabled(repo.can_act() && job.is_none());
            NavShell::push_window_actions(&mut toolbar, [back_up]);
        } else {
            // Before anything is set up nothing is open: the window is
            // Backups.
            toolbar.location = Some(ui::window_toolbar::ToolbarLocation {
                label: "Backups".to_owned(),
                ..ui::window_toolbar::ToolbarLocation::default()
            });
        }
        ui::window_toolbar::declare(Some(toolbar));
    }
}

/// The Backups window over this device's repository, answering its effects
/// and publishing its catalog as live rows.
pub(crate) fn window(cx: &ViewCx<Backups>) -> Element {
    let mut repo = Store::with_catalog(Catalog::empty());
    repo.attach(cx);
    let window = BackupsWindow::new(repo, Some(cx.live().clone()));
    let repo = Rc::clone(&window.repo);
    {
        let (answer, status) = (Rc::clone(&repo), Rc::clone(&repo));
        cx.effects(move |effect: BackupsEffect, cx| {
            answer.borrow_mut().answer_effect(effect, &answer, cx)
        })
        .status(move || status.borrow().effect_statuses());
    }
    {
        let repo = Rc::clone(&repo);
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario {
                repo.borrow_mut().stage_scenario(scenario);
            }
        });
    }
    let (rows, row, entries) = (Rc::clone(&repo), Rc::clone(&repo), repo);
    cx.publishing(
        Publishes::new()
            .live_rows(move || {
                let repo = rows.borrow();
                crate::actions::live_rows(&repo.catalog, repo.blocked().as_ref())
            })
            .live_row(move |id| {
                let repo = row.borrow();
                crate::actions::live_row(&repo.catalog, repo.blocked().as_ref(), id)
            })
            // Every loaded recovery point, older than the published page too.
            .search_entries(move || crate::search::snapshot_entries(&entries.borrow().catalog)),
        Box::new(window),
    )
}

impl ui::widget::Decorator for BackupsWindow {
    type Inner = NavShell;

    // While the restore sheet is open it is the window's one live child:
    // what its controls run reaches it, not the shell behind its scrim.
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        match &self.restore {
            Some(dialog) => dialog.provide(request),
            None => self.shell.provide(request),
        }
    }
    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        match &mut self.restore {
            Some(dialog) => dialog.provide_mut(request),
            None => self.shell.provide_mut(request),
        }
    }
    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.repo.borrow_mut().refresh_runtime();
        self.sync_window_toolbar();
        self.drain_intents();
        self.show_repository();
        // A narrow window shows the list as a drawer or as its own screen.
        if self.repo.borrow().open_drawer
            && matches!(
                self.shell.geometry().sidebar,
                ui::widget::SidebarPlacement::Drawer | ui::widget::SidebarPlacement::Screen
            )
        {
            self.repo.borrow_mut().open_drawer = false;
            self.shell.open_sidebar();
            self.shell.snap();
        }
        self.shell.sync(state, path);
        self.repo.borrow_mut().publish_health();
        if let Some(dialog) = &mut self.restore {
            dialog.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // A running job's progress line, read on its wake deadline.
        let (progressed, jobs_moved) = {
            let repo = self.repo.borrow();
            let progressed = repo.work == Work::Running
                && repo
                    .job_progress
                    .lock()
                    .is_ok_and(|shown| *shown != repo.operation_status);
            // A job's rows move when one starts, reports or ends.
            (progressed, repo.jobs.changed())
        };
        self.sync_window_toolbar();
        self.drain_intents();
        self.show_repository();
        self.shell.update(state, path) || self.repo.borrow().dirty || progressed || jobs_moved
    }

    /// A running job reports progress through a shared line; the app reads
    /// it a few times a second on a deadline. Choosing a folder and the job's
    /// end wake the app themselves (framework effects).
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let repo = self.repo.borrow();
        let progress = (repo.work == Work::Running)
            .then(|| ui::time::now().checked_add(PROGRESS_READ))
            .flatten();
        progress
            .into_iter()
            .chain(repo.health.deadline())
            .chain(self.shell.next_wake_deadline())
            .min()
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.shell.build_layout(taffy, theme, node_map);
        // The host reaches the undo bracket through the node map.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(dialog) = &mut self.restore {
            dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
            );
            *ctx.handled = true;
        } else {
            self.shell.handle_event(event, ctx);
        }
        self.drain_intents();
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.restore_changed) | self.shell.take_structure_dirty()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(dialog) = &self.restore {
            vec![dialog]
        } else {
            self.shell.children()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(dialog) = &self.restore {
            visitor(dialog);
        } else {
            self.shell.for_each_child(visitor);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(dialog) = &mut self.restore {
            vec![dialog]
        } else {
            self.shell.children_mut()
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
        if let Some(dialog) = &mut self.restore {
            dialog.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.repo.borrow().dirty || self.restore.is_some() || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.repo.borrow_mut().dirty = false;
        self.shell.mark_drawn();
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.restore.as_ref().and_then(Widget::modal_capture)
    }

    fn traps_focus(&self) -> bool {
        self.restore.is_some()
    }

    fn name(&self) -> &'static str {
        "BackupsWindow"
    }

    fn test_id(&self) -> Option<&str> {
        Some("backups_app")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;
    use ui::a11y::Action;
    use ui::headless::Framed;
    use ui::widget::{NavList, SidebarPlacement};
    use vello::kurbo::Point;
    fn demo() -> BackupsWindow {
        BackupsWindow::with_catalog(Catalog::sample(now()))
    }

    /// The wire between the band's Stop and the work: a backup, verification
    /// or restore starts as a job that offers Stop, and pressing it sets the
    /// flag the engine checks between files.
    #[test]
    fn a_backup_job_offers_stop_and_stop_reaches_the_worker() {
        let mut app = Store::with_catalog(Catalog::sample(now()));
        let run = app.start_job(JobKind::Backup, "Home", "Preparing files…");
        let job = ui::jobs::rows()
            .into_iter()
            .find(|job| job.id() == run.job.id())
            .expect("the job is in the registry");
        assert!(job.can_stop(), "a backup can be stopped");
        assert!(!run.stopped.load(std::sync::atomic::Ordering::Acquire));
        ui::jobs::stop(run.job.id());
        assert!(
            run.stopped.load(std::sync::atomic::Ordering::Acquire),
            "pressing Stop reaches the worker"
        );
        app.finish_job();
    }
    fn frame(app: BackupsWindow, width: f64, height: f64, scale: f32) -> Framed<BackupsWindow> {
        let mut theme = Theme::dark();
        theme.text_scale = scale;
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((width, height)),
            theme,
        )
    }
    fn target(app: &Framed<BackupsWindow>, id: &str) -> Rect {
        *Framed::harness(app)
            .targets()
            .get(id)
            .unwrap_or_else(|| panic!("missing target {id}"))
    }
    fn click(app: &mut Framed<BackupsWindow>, at: Point) {
        Framed::harness_mut(app).click_at(at.x, at.y);
        Framed::harness_mut(app).relayout();
    }
    fn place(app: &mut Framed<BackupsWindow>, id: &str) {
        // Narrow, the list is a screen of its own: go back to it first.
        if app.shell.geometry().sidebar == SidebarPlacement::Screen {
            app.shell.open_sidebar();
            app.shell.snap();
            Framed::harness_mut(app).relayout();
        }
        let at = app
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect(id)
            .unwrap()
            .center();
        click(app, at);
        // Opening and closing drawers animate on the live path. Geometry
        // assertions inspect the settled destination, as the opening above does.
        app.shell.snap();
        Framed::harness_mut(app).relayout();
    }
    fn live_constructor_starts_with_an_empty_catalog() {
        // The live app must not fabricate sources / snapshots / destinations.
        let app = Store::with_catalog(Catalog::empty());
        assert!(app.catalog.sources.is_empty(), "no fabricated sources");
        assert!(app.catalog.snapshots.is_empty(), "no fabricated snapshots");
        assert!(
            app.catalog.destinations.is_empty(),
            "no fabricated destinations"
        );
    }
    fn unreadable_saved_repository_blocks_effects_until_reselected() {
        let mut app = Store::with_catalog(Catalog::empty());
        app.source_path = Some(PathBuf::from("/documents"));
        app.destination_path = Some(PathBuf::from("/backup-drive"));
        app.repository_error = Some("Couldn’t read the saved backup location: unavailable".into());

        assert!(
            !app.is_configured(),
            "paths alone must not advertise a usable local repository"
        );
        let blocked = app
            .blocked()
            .expect("an unreadable repository blocks its effects");
        assert_eq!(blocked.code, "not_configured");
        assert!(blocked.reason.contains("Choose another backup location"));
        assert_eq!(
            app.effect_statuses().len(),
            3,
            "backing up, verifying and restoring are unavailable; setup is not"
        );
        let rows: Vec<_> =
            crate::actions::live_rows(&Catalog::sample(now()), app.blocked().as_ref())
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect();
        let snapshot = &rows[2].children[0];
        for id in ["verify", "restore"] {
            let action = snapshot.actions.iter().find(|a| a.id == id).unwrap();
            assert!(
                matches!(
                    &action.contract.availability,
                    tilo_actions::Availability::Unavailable { reason, .. } if *reason == blocked.reason
                ),
                "{id}"
            );
        }
    }
    fn the_sample_catalog_indexes_every_snapshot() {
        // Demo/shots data is populated; the live catalog stays empty.
        let demo = Catalog::sample(now());
        assert!(
            demo.snapshots.iter().all(|s| !s.files.is_empty()),
            "every sample snapshot carries an indexed listing"
        );
        assert!(
            Catalog::empty().snapshots.is_empty(),
            "the live catalog has no snapshots to fabricate files for"
        );
    }

    #[test]
    fn catalog_and_repository_state_decide_the_offered_controls() {
        live_constructor_starts_with_an_empty_catalog();
        unreadable_saved_repository_blocks_effects_until_reselected();
        the_sample_catalog_indexes_every_snapshot();
        let app = Store::with_catalog(Catalog::sample(now()));
        assert!(!app.catalog.sources.is_empty());
        assert!(!app.catalog.snapshots.is_empty());
        assert!(!app.catalog.destinations.is_empty());
        let app = frame(
            BackupsWindow::with_catalog(Catalog::empty()),
            1280.0,
            800.0,
            1.0,
        );
        assert!(!app.shell.has_sidebar());
        assert!(
            Framed::harness(&app)
                .targets()
                .keys()
                .any(|id| id.ends_with("EmptyState"))
        );
        let app = BackupsWindow::with_catalog(Catalog::empty());
        {
            let mut repo = app.repo.borrow_mut();
            repo.source_path = Some("/documents".into());
            repo.destination_path = Some("/backup".into());
        }
        let app = frame(app, 1280.0, 800.0, 1.0);
        assert!(app.repo.borrow().is_configured());
        assert!(app.repo.borrow().selected.is_none());
        let text = Framed::harness(&app).inspection_json().unwrap();
        assert!(!text.contains("backups-restore"));
    }
    #[test]
    fn rail_paints_and_scrolls_only_when_needed() {
        let mut narrow = frame(demo(), 390.0, 844.0, 1.0);
        assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Screen);
        assert!(
            !narrow.shell.is_sidebar_open(),
            "a new narrow window opens on its list of backups"
        );
        narrow.shell.open_sidebar();
        narrow.shell.snap();
        Framed::harness_mut(&mut narrow).relayout();
        assert!(narrow.shell.is_sidebar_open());
        // Short enough that the list overflows the whole window height: no
        // header row stands above it.
        let mut short = frame(demo(), 1280.0, 420.0, 1.0);
        let sidebar = short.shell.geometry().sidebar_column;
        let before = short
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect("all")
            .unwrap();
        Framed::harness_mut(&mut short).click_at(sidebar.center().x, sidebar.center().y);
        Framed::harness_mut(&mut short).scroll(0.0, -8.0);
        Framed::harness_mut(&mut short).relayout();
        let after = short
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect("all");
        assert!(after.is_none_or(|r| r.y0 < before.y0));
        let mut tall = frame(demo(), 1280.0, 1100.0, 1.0);
        assert!(
            tall.shell
                .sidebar_mut::<NavList>()
                .unwrap()
                .row_rect("retention")
                .is_some()
        );
    }
    #[test]
    fn phone_chrome_keeps_tabs_clear_and_fully_named() {
        for width in [340.0, 390.0] {
            for scale in [1.0, 2.0] {
                let mut app = frame(demo(), width, 844.0, scale);
                let declared =
                    ui::window_toolbar::declared().expect("the window declares its slots");
                assert!(
                    declared
                        .actions
                        .iter()
                        .any(|action| action.id == BACK_UP_NOW),
                    "{declared:?}"
                );
                app.shell.open_sidebar();
                app.shell.snap();
                Framed::harness_mut(&mut app).relayout();
                let repo = Rc::clone(&app.repo);
                let repo = repo.borrow();
                let rows = repo
                    .catalog
                    .sources
                    .iter()
                    .map(|s| format!("source:{}", s.id))
                    .chain(
                        repo.catalog
                            .destinations
                            .iter()
                            .map(|d| format!("destination:{}", d.id)),
                    )
                    .chain(["all".into(), "retention".into()])
                    .collect::<Vec<_>>();
                let mut shown = Vec::new();
                for id in rows {
                    if let Some(r) = app.shell.sidebar_mut::<NavList>().unwrap().row_rect(&id) {
                        assert!(r.width() > 0.0 && r.x1 <= width);
                        shown.push(r);
                    }
                }
                for (index, row) in shown.iter().enumerate() {
                    for other in &shown[index + 1..] {
                        assert!(row.intersect(*other).is_zero_area());
                    }
                }
                let text = Framed::harness(&app).inspection_json().unwrap();
                assert!(text.contains("Retention"));
                assert!(text.contains("Sources"));
                assert!(text.contains("Destinations"));
            }
        }
    }
    #[test]
    fn view_navigation_is_device_local_through_pointer_keyboard_and_accessibility() {
        fn target_in(node: &ui::headless::InspectionNode) -> Option<String> {
            node.semantics
                .as_ref()
                .and_then(find)
                .or_else(|| node.children.iter().find_map(target_in))
        }

        fn find(node: &ui::a11y::AccessNode) -> Option<String> {
            if node.name.as_deref() == Some("Retention") {
                return node.action_target.clone();
            }
            node.children.iter().find_map(find)
        }

        let mut app = frame(demo(), 1280.0, 800.0, 1.0);
        let before = Framed::harness_mut(&mut app).state_mut().doc.get_heads();
        place(&mut app, "source:etc");
        {
            let repo = app.repo.borrow();
            assert_eq!(repo.page, Page::Source("etc".into()));
            let snapshot = repo
                .catalog
                .snapshot(repo.selected.as_deref().unwrap())
                .unwrap();
            assert_eq!(snapshot.source_id, "etc");
        }
        place(&mut app, "retention");
        assert_eq!(app.repo.borrow().page, Page::Retention);
        let at = app
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect("all")
            .unwrap();
        click(&mut app, at.center());
        Framed::harness_mut(&mut app).key("ArrowDown");
        Framed::harness_mut(&mut app).key("Enter");
        assert_ne!(app.repo.borrow().page, Page::All);
        let owner = "backups-places";
        let inspection = Framed::harness(&app).inspection();

        let semantic = target_in(&inspection.root).unwrap();
        Framed::harness_mut(&mut app)
            .perform_semantic_named(owner, Some(&semantic), &Action::Activate)
            .unwrap();
        assert_eq!(app.repo.borrow().page, Page::Retention);
        app.repo.borrow().intents.push(Intent::Navigate(Page::All));
        app.drain_intents();
        app.refresh_view();
        Framed::harness_mut(&mut app).relayout();
        app.open_restore();
        Framed::harness_mut(&mut app).relayout();
        let cancel = app.restore.as_ref().unwrap().secondary_rect();
        click(&mut app, cancel.center());
        assert!(app.restore.is_none());
        let after = Framed::harness_mut(&mut app).state_mut().doc.get_heads();
        assert_eq!(before, after);
    }
    #[test]
    fn retention_steppers_stay_reachable_and_stack_at_large_text() {
        fn label_target(node: &ui::headless::InspectionNode) -> Option<String> {
            fn find(node: &ui::a11y::AccessNode) -> Option<String> {
                if node.name.as_deref() == Some("Retention") {
                    return node.action_target.clone();
                }
                node.children.iter().find_map(find)
            }
            node.semantics
                .as_ref()
                .and_then(find)
                .or_else(|| node.children.iter().find_map(label_target))
        }

        for (width, scale) in [(1280.0, 1.0), (390.0, 1.0), (390.0, 2.0)] {
            let mut app = frame(demo(), width, 844.0, scale);
            place(&mut app, "retention");
            let ids = PolicyField::ALL
                .iter()
                .map(|f| format!("backups-keep-{}", f.key()))
                .collect::<Vec<_>>();
            let rows = ids.iter().map(|id| target(&app, id)).collect::<Vec<_>>();
            for (index, row) in rows.iter().enumerate() {
                assert!(row.width() > 0.0 && row.x1 <= width);
                for other in &rows[index + 1..] {
                    assert!(row.intersect(*other).is_zero_area());
                }
            }
        }
        let mut app =
            ui::framework::testing::TestApp::<crate::BackupsShell>::capture(None).unwrap();

        let target = label_target(&app.harness().inspection().root).unwrap();
        app.harness()
            .perform_semantic_named("backups-places", Some(&target), &Action::Activate)
            .unwrap();
        let before = app.state().policy.daily;
        app.harness()
            .perform_semantic_named("backups-keep-daily", None, &Action::Increment)
            .unwrap();
        app.settle();
        assert_eq!(app.state().policy.daily, before + 1);
        app.invoke(
            ui::tilo_resource::Invoke::edit(&crate::resource::Backups::paths().policy.daily.set(3))
                .unwrap(),
        )
        .unwrap();
        app.settle();
        assert_eq!(app.state().policy.daily, 3);
    }
    #[test]
    fn recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision() {
        let mut catalog = Catalog::sample(now());
        for source in &mut catalog.sources {
            source.path =
                "/an/intentionally/very/long/device/local/path/that/cannot/fit/a/phone/header"
                    .into();
        }
        for scale in [1.0, 2.0] {
            let app = BackupsWindow::with_catalog(catalog.clone());
            let mut app = frame(app, 390.0, 844.0, scale);
            app.open_restore();
            Framed::harness_mut(&mut app).relayout();
            let text = Framed::harness(&app).inspection_json().unwrap();
            assert!(!text.contains("/an/intentionally"));
            let primary = app.restore.as_ref().unwrap().primary_rect();
            let secondary = app.restore.as_ref().unwrap().secondary_rect();
            assert!(primary.x1 <= 390.0);
            assert!(secondary.x1 <= primary.x0);
            assert!(primary.height() > 0.0);
            let unavailable = BackupsWindow::with_catalog(Catalog::empty());
            unavailable.repo.borrow_mut().repository_error = Some(
                "The saved backup location is unavailable. Reconnect it or choose another location."
                    .into(),
            );
            let unavailable = frame(unavailable, 390.0, 844.0, scale);
            let text = Framed::harness(&unavailable).inspection_json().unwrap();
            assert!(text.contains("Reconnect it or choose another location."));
        }
    }
    #[test]
    fn restore_sheet_offers_only_truthful_modal_storage_controls() {
        let mut app = frame(demo(), 1280.0, 800.0, 1.0);
        let behind = app
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect("source:etc")
            .unwrap();
        app.open_restore();
        Framed::harness_mut(&mut app).relayout();
        assert!(Widget::traps_focus(&*app));
        assert!(
            !app.restore.as_ref().unwrap().primary_enabled(),
            "a capture without a repository cannot restore"
        );
        assert!(
            !Framed::harness(&app)
                .targets()
                .contains_key("backups-places"),
            "the modal hides controls behind its scrim from hit testing"
        );
        let primary = app.restore.as_ref().unwrap().primary_rect();
        let secondary = app.restore.as_ref().unwrap().secondary_rect();
        assert!((primary.y0 - secondary.y0).abs() < f64::EPSILON);
        assert!((primary.y1 - secondary.y1).abs() < f64::EPSILON);
        assert!(secondary.x1 <= primary.x0);
        assert!(primary.height() >= 32.0);
        let text = Framed::harness(&app).inspection_json().unwrap();
        assert!(text.contains("Choose folder"));
        assert!(!text.contains("Choose files"));
        let before = app.repo.borrow().page.clone();
        click(&mut app, behind.center());
        assert_eq!(app.repo.borrow().page, before);
        assert!(
            app.restore.is_none(),
            "a scrim click dismisses without navigating behind it"
        );
        app.open_restore();
        Framed::harness_mut(&mut app).relayout();
        let cancel = app.restore.as_ref().unwrap().secondary_rect();
        click(&mut app, cancel.center());
        assert!(app.restore.is_none());
        let rows = crate::actions::live_rows(&app.repo.borrow().catalog, None)
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect::<Vec<_>>();
        let kinds = rows.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "backups.source-set",
                "backups.destination-set",
                "backups.snapshot-set"
            ]
        );
        let snapshot = &rows[2].children[0];
        assert_eq!(
            snapshot
                .actions
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["read", "open", "verify", "restore"]
        );
        assert!(
            snapshot
                .children
                .iter()
                .any(|child| child.kind == "backups.restore-target")
        );
    }
    #[test]
    fn the_window_alone_writes_nothing_to_the_record() {
        let app = frame(
            BackupsWindow::with_catalog(Catalog::empty()),
            1280.0,
            800.0,
            1.0,
        );
        assert!(
            Framed::harness(&app)
                .state()
                .doc
                .get(automerge::ROOT, "policy")
                .unwrap()
                .is_none()
        );
    }
}
