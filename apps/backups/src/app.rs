//! Retained unified backup views over the resource-native device runtime.
use crate::actions::{BackupsEffect, Blocked};
use crate::i18n;
use crate::model::MAX_KEPT;
use crate::model::PolicyField;
use crate::model::{Catalog, Filter, RetentionPolicy};
use crate::resource::{BackupLocation, Backups, DemoRepository};
use crate::view::{self, Intent, Intents, JobKind, Order, Page, Setup};
use chrono::NaiveDateTime;
use std::cell::Cell;

/// How often a running job's progress line is read.
const PROGRESS_READ: ui::time::Duration = ui::time::Duration::from_millis(250);
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::LocalState;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Setting, SurfaceCx,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{AppModel, Column, ModalDialog, ModelPath, NavShell, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
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

pub struct BackupsApp {
    shell: NavShell,
    intents: Intents,
    catalog: Catalog,
    catalog_rev: u64,
    catalog_key: String,
    page: Page,
    selected: Option<String>,
    policy: RetentionPolicy,
    retention: [LocalState<f64>; 4],
    order: Order,
    restore: Option<ModalDialog<Column>>,
    restore_changed: bool,
    failure: Option<(String, String)>,
    status_bubble: ui::StatusBubble,
    cursor: Option<Point>,
    scroll: Rc<Cell<(f32, f32)>>,
    built: Option<ViewKey>,
    dirty: bool,
    open_drawer: bool,
    source_path: Option<PathBuf>,
    destination_path: Option<PathBuf>,
    repository_error: Option<String>,
    dispatcher: Option<Dispatcher<Backups>>,
    location: Option<Setting<BackupLocation>>,
    loaded: BackupLocation,
    work: Work,
    job_progress: JobProgress,
    operation_status: Option<String>,
    job_status: Option<view::Job>,
    job_kind: Option<JobKind>,
    ui: UiCache,
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
impl Default for BackupsApp {
    fn default() -> Self {
        Self::new()
    }
}
fn now() -> NaiveDateTime {
    ui::clock::local_now().naive_local()
}
impl BackupsApp {
    pub fn new() -> Self {
        Self::with_catalog(Catalog::empty())
    }
    fn with_catalog(catalog: Catalog) -> Self {
        let policy = RetentionPolicy::default();
        Self {
            shell: NavShell::new()
                .content_header("", Vec::new())
                .inspector_header("Snapshot", Vec::new())
                .with_id("backups-shell"),
            intents: Intents::default(),
            catalog,
            catalog_rev: 0,
            catalog_key: String::new(),
            restore_changed: false,
            dispatcher: None,
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
            restore: None,
            failure: None,
            status_bubble: ui::StatusBubble::new(),
            cursor: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            dirty: true,
            open_drawer: false,
            source_path: None,
            destination_path: None,
            repository_error: None,
            job_status: None,
        }
    }
    pub(crate) fn resource_catalog(&self) -> &Catalog {
        &self.catalog
    }
    pub(crate) fn is_configured(&self) -> bool {
        self.source_path.is_some()
            && self.destination_path.is_some()
            && self.repository_error.is_none()
    }

    /// Take part in the resource: edits and effects go through the
    /// dispatcher, and the repository shown is this device's (a live
    /// window) or the scenario's (a capture).
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Backups>) {
        self.dispatcher = Some(cx.dispatcher());
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
        self.shell.toast(msg, ToastKind::Success);
    }

    /// Errors must read as errors — a green success toast after a rejected
    /// restore is actively misleading and can make recovery unsafe.
    fn toast_error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.failure = Some(("Backup paused".into(), msg.clone()));
        self.shell.toast(msg, ToastKind::Error);
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
        cx: &mut EffectCx<Backups, Self>,
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
                    Ok(self.choose_destination(cx))
                } else {
                    Ok(self.choose_source(cx))
                }
            }
            BackupsEffect::BackupNow => {
                if let Some(blocked) = self.blocked() {
                    self.toast_error(blocked.reason.clone());
                    return Err(blocked.reason);
                }
                Ok(self.start_backup(cx))
            }
            BackupsEffect::Verify { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.ui.sel_snapshot = Some(snapshot_id.clone());
                Ok(self.start_verify(snapshot_id, cx))
            }
            BackupsEffect::RestoreSnapshot { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.ui.sel_snapshot = Some(snapshot_id.clone());
                Ok(self.choose_restore_folder(snapshot_id, cx))
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
            BackupsEffect::ShowSnapshot { snapshot_id } => {
                if !self.has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.ui.sel_snapshot = Some(snapshot_id);
                self.ui.view = View::Timeline;
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

    fn choose_source(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Choose a folder to protect", "Protect folder"),
            |app: &mut Self, picked, cx| {
                app.work = Work::Idle;
                match picked {
                    Picked::Opened(path) => {
                        app.source_path = Some(path);
                        Ok(app.choose_destination(cx))
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

    fn choose_destination(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Choose a backup location", "Use this location"),
            |app: &mut Self, picked, cx| {
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
                Ok(app.start_backup(cx))
            },
        )
    }

    fn start_job(&mut self, status: &str) -> JobProgress {
        self.work = Work::Running;
        self.job_progress = JobProgress::new(Mutex::new(Some(status.to_owned())));
        self.operation_status = Some(status.to_owned());
        Arc::clone(&self.job_progress)
    }

    fn finish_job(&mut self) {
        self.work = Work::Idle;
        self.operation_status = None;
    }

    fn start_backup(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return Effected::done();
        };
        let policy = cx.live().get().policy;
        let shown = self.start_job("Preparing files…");
        cx.thread(
            move |progress| {
                let created = crate::engine::create_snapshot(&source, &destination, |step| {
                    report(
                        &shown,
                        format!("{} files totaling {}", step.files, ui::format::bytes(step.bytes)),
                    );
                    progress.report("copying", Some(step.files), None);
                })
                .map_err(|error| error.to_string())?;
                progress.report("pruning", None, None);
                let pruned = crate::engine::prune_snapshots(&destination, &policy)
                    .map_err(|error| error.to_string());
                Ok::<_, String>((created, pruned))
            },
            |app: &mut Self, result, _| {
                app.finish_job();
                let (manifest, pruned) = result.inspect_err(|error| {
                    app.toast_error(error.clone());
                })?;
                if let Err(error) = app.reload_repository() {
                    app.toast_error(
                        "Backup finished, but its history could not be reopened. Choose the location again before another operation.",
                    );
                    return Err(error);
                }
                match &pruned {
                    Ok(_) => app.toast_now(format!(
                        "Backup complete: {} files totaling {}.",
                        manifest.files,
                        ui::format::bytes(manifest.bytes)
                    )),
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

    fn start_verify(&mut self, id: String, cx: &mut EffectCx<Backups, Self>) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        self.start_job("Checking every file…");
        cx.thread(
            move |_| crate::engine::verify_snapshot(&destination, &id),
            |app: &mut Self, result, _| {
                app.finish_job();
                let result = result.map_err(|error| {
                    app.toast_error(error.to_string());
                    error.to_string()
                })?;
                app.toast_now(format!(
                    "Verified {} files totaling {}.",
                    result.files,
                    ui::format::bytes(result.bytes)
                ));
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
    }

    fn choose_restore_folder(&mut self, id: String, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Restore into an empty folder", "Restore here"),
            |app: &mut Self, picked, cx| {
                app.work = Work::Idle;
                match picked {
                    // The sheet promises a destination chooser, not a
                    // background restore: it closes only once a folder was
                    // chosen, and the engine never overwrites one.
                    Picked::Opened(path) => {
                        app.restore = None;
                        app.restore_changed = true;
                        Ok(app.start_restore(id, path, cx))
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
        cx: &mut EffectCx<Backups, Self>,
    ) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        let shown = self.start_job("Restoring and checking files…");
        cx.thread(
            move |progress| {
                crate::engine::restore_snapshot(&destination, &id, &restore_to, false, |step| {
                    report(
                        &shown,
                        format!(
                            "{} files totaling {}",
                            step.files,
                            ui::format::bytes(step.bytes)
                        ),
                    );
                    progress.report("restoring", Some(step.files), None);
                })
            },
            |app: &mut Self, result, _| {
                app.finish_job();
                let result = result.map_err(|error| {
                    app.toast_error(error.to_string());
                    error.to_string()
                })?;
                app.toast_now(format!(
                    "Restored {} files totaling {}.",
                    result.files,
                    ui::format::bytes(result.bytes)
                ));
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
    }

    // ---- semantic colour mapping (all sourced from the Theme) -----------
}
impl BackupsApp {
    pub(crate) fn answer_effect(
        &mut self,
        effect: BackupsEffect,
        cx: &mut EffectCx<Backups, Self>,
    ) -> EffectAnswer {
        self.job_kind = match &effect {
            BackupsEffect::BackupNow => Some(JobKind::Backup),
            BackupsEffect::Verify { .. } => Some(JobKind::Verify),
            BackupsEffect::RestoreSnapshot { .. } => Some(JobKind::Restore),
            _ => self.job_kind,
        };
        let result = self.answer_backend(effect, cx);
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
        self.refresh_view();
        result
    }
    fn refresh_runtime(&mut self) {
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
                self.status_bubble.poke(ui::time::now());
            }
            "progress" => {
                self.operation_status = Some("642 files totaling 3.8 GB verified so far".into());
                self.job_kind = Some(JobKind::Verify);
            }
            "failure" => self.toast_error(
                "Backup paused: the destination is unavailable. Reconnect it, then retry.",
            ),
            "restore" => {
                self.keep_selection();
                self.open_restore();
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
        self.refresh_view();
    }
    fn open_restore(&mut self) {
        let Some(snapshot) = self
            .selected
            .as_deref()
            .and_then(|id| self.catalog.snapshot(id))
        else {
            return;
        };
        self.restore_changed = true;
        self.restore = Some(
            view::restore_dialog(snapshot, &self.catalog, now(), &self.intents)
                .with_primary_enabled(self.blocked().is_none()),
        );
        self.dirty = true;
    }

    // ---- intents ------------------------------------------------------------

    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            self.dirty = true;
            match intent {
                Intent::Navigate(page) => {
                    self.page = page;
                    self.selected = None;
                    self.scroll.set((0.0, 0.0));
                }
                Intent::SelectSnapshot(id) => {
                    self.selected = Some(id);
                    if !self.shell.is_inspector_open() {
                        self.shell.toggle_inspector();
                    }
                }
                Intent::BackUpNow => self.invoke(&BackupsEffect::BackupNow),
                Intent::Verify => {
                    if let Some(id) = &self.selected {
                        self.invoke(&BackupsEffect::Verify {
                            snapshot_id: id.clone(),
                        });
                    }
                }
                Intent::OpenRestore => self.open_restore(),
                Intent::ChooseRestoreFolder => {
                    if let Some(id) = &self.selected {
                        self.invoke(&BackupsEffect::RestoreSnapshot {
                            snapshot_id: id.clone(),
                        });
                    }
                }
                Intent::CloseRestore => {
                    self.restore = None;
                    self.restore_changed = true;
                }
                Intent::Setup => self.invoke(&BackupsEffect::Setup),
                Intent::DismissFailure => self.failure = None,
                Intent::SetOrder(order) => self.order = order,
            }
        }
        for ((field, current), retained) in [
            (PolicyField::Hourly, self.policy.hourly),
            (PolicyField::Daily, self.policy.daily),
            (PolicyField::Weekly, self.policy.weekly),
            (PolicyField::Monthly, self.policy.monthly),
        ]
        .into_iter()
        .zip(self.retention.clone())
        {
            let value = retained.get().clamp(0.0, 999.0) as i64;
            if value != i64::from(current) {
                self.bump_retention(field, value - i64::from(current));
                retained.set(f64::from(current));
            }
        }
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

    // ---- the shell's content ----------------------------------------------

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

    fn set_content_header(&mut self, title: String, actions: Vec<ui::widget::Element>) {
        self.shell.set_content_header(title, actions);
    }

    /// Rebuild the columns when what they show changed.
    fn refresh_view(&mut self) {
        self.keep_selection();
        let key = self.view_key();
        if self.built.as_ref() == Some(&key) {
            return;
        }
        let sidebar_stale = self
            .built
            .as_ref()
            .is_none_or(|built| built.catalog != key.catalog || built.setup != key.setup);
        self.built = Some(key.clone());
        self.dirty = true;
        if let Some(setup) = &key.setup {
            let state = match setup {
                SetupKey::Start => Setup::Start,
                SetupKey::ChooseDestination => Setup::ChooseDestination,
                SetupKey::Running(line) => Setup::Running(line),
                SetupKey::Unavailable(error) => Setup::Unavailable(error),
            };
            self.shell.set_sidebar(None);
            self.shell.set_inspector(None);
            self.set_content_header(String::new(), Vec::new());
            self.shell.set_content(view::setup(&state, &self.intents));
            return;
        }
        if sidebar_stale {
            self.shell.set_sidebar(Some(Box::new(view::sidebar(
                &self.catalog,
                &self.page,
                &self.intents,
            ))));
        } else if let Some(sidebar) = self.shell.sidebar_mut::<ui::widget::NavList>() {
            let selected = self.page.nav_id();
            if sidebar.selected_id().as_deref() != Some(selected.as_str()) {
                sidebar.set_selected(Some(&selected));
            }
        }
        let actions = view::header_actions(key.job, key.can_act, &self.intents);
        self.set_content_header(view::page_title(&self.catalog, &self.page), actions);
        let now = now();
        let model = view::Model {
            catalog: &self.catalog,
            page: &self.page,
            selected: self.selected.as_deref(),
            now,
            order: self.order,
            job: self.job_status.as_ref(),
            failure: self
                .failure
                .as_ref()
                .map(|(title, message)| (title.as_str(), message.as_str())),
        };
        let content = view::content(&model, &self.intents, &self.scroll, &self.retention);
        self.shell.set_content(content);
        let details = self
            .selected
            .as_deref()
            .filter(|_| self.page.lists_snapshots())
            .and_then(|id| self.catalog.snapshot(id))
            .map(|snapshot| {
                view::snapshot_details(
                    snapshot,
                    &self.catalog,
                    now,
                    key.can_act && key.job.is_none(),
                    &self.intents,
                )
            });
        self.shell.set_inspector(details);
    }

    /// "All backups healthy", or how many need a look.
    fn health_line(&self) -> String {
        let failed = self.catalog.failed(Filter::All) as u64;
        if failed == 0 {
            i18n::t("backups-health-ok")
        } else {
            i18n::counted("backups-health-attention", failed, "")
        }
    }

    // ---- overlays -----------------------------------------------------------

    fn paint_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        if self.setup_key().is_some() {
            return;
        }
        let now = ui::time::now();
        let line = self.health_line();
        self.status_bubble.show(&line, now);
        // Resting the pointer on the bubble's corner holds it open.
        if let Some(cursor) = self.cursor {
            self.status_bubble
                .pointer_moved(window, cursor, text, theme, now);
        }
        self.status_bubble
            .paint(scene, transform, text, theme, window, now);
    }
}
impl ui::widget::Decorator for BackupsApp {
    type Inner = NavShell;

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        self.shell.provide(request);
    }
    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        self.shell.provide_mut(request);
    }
    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh_runtime();
        self.drain_intents();
        self.refresh_view();
        if self.open_drawer && self.shell.geometry().sidebar == ui::widget::SidebarPlacement::Drawer
        {
            self.open_drawer = false;
            self.shell.open_sidebar();
            self.shell.snap();
        }
        self.shell.sync(state, path);
        if let Some(dialog) = &mut self.restore {
            dialog.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // A running job's progress line, read on its wake deadline.
        let progressed = self.work == Work::Running
            && self
                .job_progress
                .lock()
                .is_ok_and(|shown| *shown != self.operation_status);
        self.drain_intents();
        self.refresh_view();
        self.shell.update(state, path) || self.dirty || progressed
    }

    /// A running job reports progress through a shared line; the app reads
    /// it a few times a second on a deadline. Choosing a folder and the job's
    /// end wake the app themselves (framework effects).
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let progress = (self.work == Work::Running)
            .then(|| ui::time::now().checked_add(PROGRESS_READ))
            .flatten();
        progress
            .into_iter()
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
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(ctx.logical(position));
        }
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
        let window = presence
            .viewport
            .get()
            .unwrap_or_else(|| self.shell.get_rect());
        self.paint_status(scene, transform, text, theme, window);
        if let Some(dialog) = &mut self.restore {
            dialog.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.status_bubble.needs_frame(ui::time::now()) || self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.restore.is_some() || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.restore.as_ref().and_then(Widget::modal_capture)
    }

    fn traps_focus(&self) -> bool {
        self.restore.is_some()
    }

    fn name(&self) -> &'static str {
        "BackupsApp"
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
    fn demo() -> BackupsApp {
        BackupsApp::with_catalog(Catalog::sample(now()))
    }
    fn frame(app: BackupsApp, width: f64, height: f64, scale: f32) -> Framed<BackupsApp> {
        let mut theme = Theme::dark();
        theme.text_scale = scale;
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((width, height)),
            theme,
        )
    }
    fn target(app: &Framed<BackupsApp>, id: &str) -> Rect {
        *Framed::harness(app)
            .targets()
            .get(id)
            .unwrap_or_else(|| panic!("missing target {id}"))
    }
    fn click(app: &mut Framed<BackupsApp>, at: Point) {
        Framed::harness_mut(app).click_at(at.x, at.y);
        Framed::harness_mut(app).relayout();
    }
    fn place(app: &mut Framed<BackupsApp>, id: &str) {
        if app.shell.geometry().sidebar == SidebarPlacement::Drawer {
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
        let app = BackupsApp::with_catalog(Catalog::empty());
        assert!(app.catalog.sources.is_empty(), "no fabricated sources");
        assert!(app.catalog.snapshots.is_empty(), "no fabricated snapshots");
        assert!(
            app.catalog.destinations.is_empty(),
            "no fabricated destinations"
        );
    }
    fn unreadable_saved_repository_blocks_effects_until_reselected() {
        let mut app = BackupsApp::with_catalog(Catalog::empty());
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
        // Every snapshot row offers its effects as unavailable too.
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
        let app = demo();
        assert!(!app.catalog.sources.is_empty());
        assert!(!app.catalog.snapshots.is_empty());
        assert!(!app.catalog.destinations.is_empty());
        let app = frame(BackupsApp::new(), 1280.0, 800.0, 1.0);
        assert!(!app.shell.has_sidebar());
        assert!(
            Framed::harness(&app)
                .targets()
                .keys()
                .any(|id| id.ends_with("EmptyState"))
        );
        let mut app = BackupsApp::new();
        app.source_path = Some("/documents".into());
        app.destination_path = Some("/backup".into());
        let app = frame(app, 1280.0, 800.0, 1.0);
        assert!(app.is_configured());
        assert!(app.selected.is_none());
        let text = Framed::harness(&app).inspection_json().unwrap();
        assert!(!text.contains("backups-restore"));
    }
    #[test]
    fn rail_paints_and_scrolls_only_when_needed() {
        let mut narrow = frame(demo(), 390.0, 844.0, 1.0);
        assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Drawer);
        assert!(!narrow.shell.is_sidebar_open());
        narrow.shell.open_sidebar();
        narrow.shell.snap();
        Framed::harness_mut(&mut narrow).relayout();
        assert!(narrow.shell.is_sidebar_open());
        let mut short = frame(demo(), 1280.0, 480.0, 1.0);
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
                let primary = target(&app, "backups-back-up-now");
                assert!(primary.width() > 0.0);
                assert!(primary.x0 >= 0.0 && primary.x1 <= width);
                app.shell.open_sidebar();
                app.shell.snap();
                Framed::harness_mut(&mut app).relayout();
                let rows = app
                    .catalog
                    .sources
                    .iter()
                    .map(|s| format!("source:{}", s.id))
                    .chain(
                        app.catalog
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
        assert_eq!(app.page, Page::Source("etc".into()));
        let snapshot = app
            .catalog
            .snapshot(app.selected.as_deref().unwrap())
            .unwrap();
        assert_eq!(snapshot.source_id, "etc");
        place(&mut app, "retention");
        assert_eq!(app.page, Page::Retention);
        let at = app
            .shell
            .sidebar_mut::<NavList>()
            .unwrap()
            .row_rect("all")
            .unwrap();
        click(&mut app, at.center());
        Framed::harness_mut(&mut app).key("ArrowDown");
        Framed::harness_mut(&mut app).key("Enter");
        assert_ne!(app.page, Page::All);
        let owner = "backups-places";
        let inspection = Framed::harness(&app).inspection();

        let semantic = target_in(&inspection.root).unwrap();
        Framed::harness_mut(&mut app)
            .perform_semantic_named(owner, Some(&semantic), &Action::Activate)
            .unwrap();
        assert_eq!(app.page, Page::Retention);
        app.intents.push(Intent::Navigate(Page::All));
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
            let app = BackupsApp::with_catalog(catalog.clone());
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
            let mut unavailable = BackupsApp::new();
            unavailable.repository_error=Some("The saved backup location is unavailable. Reconnect it or choose another location.".into());
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
        let before = app.page.clone();
        click(&mut app, behind.center());
        assert_eq!(app.page, before);
        assert!(
            app.restore.is_none(),
            "a scrim click dismisses without navigating behind it"
        );
        app.open_restore();
        Framed::harness_mut(&mut app).relayout();
        let cancel = app.restore.as_ref().unwrap().secondary_rect();
        click(&mut app, cancel.center());
        assert!(app.restore.is_none());
        let rows = crate::actions::live_rows(app.resource_catalog(), None)
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
            ["open", "verify", "restore"]
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
        let app = frame(BackupsApp::new(), 1280.0, 800.0, 1.0);
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
