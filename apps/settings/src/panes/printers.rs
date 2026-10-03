#![allow(
    clippy::struct_excessive_bools,
    reason = "loading, discovery, local-document isolation, and dirty states are independent"
)]

//! Daily printer/scanner control backed by the shared typed CUPS/SANE service.
//!
//! Settings owns discovery, adding a driverless queue, defaults, queue health
//! and recovery. A future Print Centre may offer long job history, finishing
//! workflows and diagnostics, but must consume the same `PrintService` rather
//! than maintaining another printer database.

use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::printing::{
    DiscoveredPrinter, JobState, PrintJob, PrinterQueue, QueueState, Scanner,
};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::i18n::ArgValue;

use crate::i18n::{t, t_args};
use crate::widgets::{El, StatusTone, caption, card, card_with_action, rows, semantic_status_pill};

pub(crate) const REFRESH_KEY: &str = "settings.printers.refresh";
pub(crate) const SCAN_KEY: &str = "settings.printers.discover";

pub(crate) fn queue_action_key(action: &str, name: &str) -> String {
    format!("settings.printers.{action}.{}", slug(name))
}

pub(crate) fn job_action_key(action: &str, id: &str) -> String {
    format!("settings.printers.job.{action}.{}", slug(id))
}

pub trait PrintService: Send + Sync {
    /// # Errors
    ///
    /// Returns `Err` if the CUPS backend cannot be reached or reports a fault
    /// while enumerating queues.
    fn queues(&self) -> Result<Vec<PrinterQueue>, String>;

    /// # Errors
    ///
    /// Returns `Err` if the CUPS backend cannot be reached or reports a fault
    /// while enumerating the job list.
    fn jobs(&self) -> Result<Vec<PrintJob>, String>;

    /// # Errors
    ///
    /// Returns `Err` if the driverless discovery scan (DNS-SD/mDNS) fails to
    /// start or is interrupted.
    fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String>;

    /// # Errors
    ///
    /// Returns `Err` if the SANE backend cannot be reached or reports a fault
    /// while enumerating scanners.
    fn scanners(&self) -> Result<Vec<Scanner>, String>;

    /// # Errors
    ///
    /// Returns `Err` if the discovered printer cannot be added as a
    /// driverless queue (unreachable URI, or CUPS rejects the queue).
    fn add(&self, printer: &DiscoveredPrinter) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns `Err` if `name` does not name an existing queue, or CUPS
    /// refuses to change the system default.
    fn set_default(&self, name: &str) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns `Err` if `name` does not name an existing queue, or CUPS
    /// refuses to change its enabled/paused state.
    fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns `Err` if `name` does not name an existing queue, or CUPS
    /// refuses to delete it.
    fn remove(&self, name: &str) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns `Err` if `id` does not name an active job, or CUPS refuses to
    /// cancel it.
    fn cancel(&self, id: &str) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns `Err` if `id` does not name a stopped/canceled/aborted job, or
    /// CUPS refuses to resubmit it.
    fn retry(&self, id: &str) -> Result<(), String>;
}

struct CupsService;
impl PrintService for CupsService {
    fn queues(&self) -> Result<Vec<PrinterQueue>, String> {
        os_actions::printing::queues()
    }
    fn jobs(&self) -> Result<Vec<PrintJob>, String> {
        os_actions::printing::jobs()
    }
    fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String> {
        os_actions::printing::discover_driverless()
    }
    fn scanners(&self) -> Result<Vec<Scanner>, String> {
        os_actions::printing::scanners()
    }
    fn add(&self, p: &DiscoveredPrinter) -> Result<(), String> {
        os_actions::printing::add_driverless(&p.uri, &p.name)
    }
    fn set_default(&self, name: &str) -> Result<(), String> {
        os_actions::printing::set_default(name)
    }
    fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        os_actions::printing::set_enabled(name, enabled)
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        os_actions::printing::remove_queue(name)
    }
    fn cancel(&self, id: &str) -> Result<(), String> {
        os_actions::printing::cancel_job(id)
    }
    fn retry(&self, id: &str) -> Result<(), String> {
        os_actions::printing::retry_job(id)
    }
}

pub struct PrintersPane {
    service: Arc<dyn PrintService>,
    printers: Vec<PrinterQueue>,
    jobs: Vec<PrintJob>,
    discovered: Vec<DiscoveredPrinter>,
    scanners: Vec<Scanner>,
    refresh_rx: Option<mpsc::Receiver<PrinterSnapshot>>,
    discover_rx: Option<mpsc::Receiver<Result<Vec<DiscoveredPrinter>, String>>>,
    last_refresh: Instant,
    loading: bool,
    discovering: bool,
    confirm_remove: Option<String>,
    error: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

struct PrinterSnapshot {
    printers: Result<Vec<PrinterQueue>, String>,
    jobs: Result<Vec<PrintJob>, String>,
    scanners: Result<Vec<Scanner>, String>,
}

impl Default for PrintersPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintersPane {
    pub fn new() -> Self {
        let mut pane = Self::empty(Arc::new(CupsService));
        pane.request_refresh();
        pane
    }

    fn empty(service: Arc<dyn PrintService>) -> Self {
        Self {
            service,
            printers: Vec::new(),
            jobs: Vec::new(),
            discovered: Vec::new(),
            scanners: Vec::new(),
            refresh_rx: None,
            discover_rx: None,
            last_refresh: ui::time::now(),
            loading: false,
            discovering: false,
            confirm_remove: None,
            error: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
    fn from_service(service: Arc<dyn PrintService>) -> Self {
        let printers = service.queues().unwrap_or_default();
        let jobs = service.jobs().unwrap_or_default();
        let scanners = service.scanners().unwrap_or_default();
        Self {
            service,
            printers,
            jobs,
            discovered: Vec::new(),
            scanners,
            refresh_rx: None,
            discover_rx: None,
            last_refresh: ui::time::now(),
            loading: false,
            discovering: false,
            confirm_remove: None,
            error: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
    fn with_service(service: Arc<dyn PrintService>) -> Self {
        Self::from_service(service)
    }

    fn request_refresh(&mut self) {
        if self.refresh_rx.is_some() {
            return;
        }
        let service = self.service.clone();
        let (tx, rx) = mpsc::channel();
        self.refresh_rx = Some(rx);
        self.loading = true;
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-printer-status".into())
            .spawn(move || {
                let _ = tx.send(PrinterSnapshot {
                    printers: service.queues(),
                    jobs: service.jobs(),
                    scanners: service.scanners(),
                });
            })
        {
            self.refresh_rx = None;
            self.loading = false;
            self.error = Some(t_args(
                "settings-printers-worker-failed",
                &[("error", ArgValue::Text(error.to_string()))],
            ));
        }
    }

    fn request_discovery(&mut self) {
        if self.discover_rx.is_some() {
            return;
        }
        let service = self.service.clone();
        let (tx, rx) = mpsc::channel();
        self.discover_rx = Some(rx);
        self.discovering = true;
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-printer-discovery".into())
            .spawn(move || {
                let _ = tx.send(service.discover());
            })
        {
            self.discover_rx = None;
            self.discovering = false;
            self.error = Some(t_args(
                "settings-printers-discover-failed",
                &[("error", ArgValue::Text(error.to_string()))],
            ));
        }
    }

    fn poll_background(&mut self) {
        if let Some(result) = self.refresh_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.refresh_rx = None;
            self.loading = false;
            self.last_refresh = ui::time::now();
            let mut errors = Vec::new();
            match result.printers {
                Ok(value) => self.printers = value,
                Err(error) => errors.push(t_args(
                    "settings-printers-error-printers",
                    &[("error", error.into())],
                )),
            }
            match result.jobs {
                Ok(value) => self.jobs = value,
                Err(error) => errors.push(t_args(
                    "settings-printers-error-jobs",
                    &[("error", error.into())],
                )),
            }
            match result.scanners {
                Ok(value) => self.scanners = value,
                Err(error) => errors.push(t_args(
                    "settings-printers-error-scanners",
                    &[("error", error.into())],
                )),
            }
            self.error = (!errors.is_empty()).then(|| errors.join(" · "));
            self.structure_dirty = true;
        }
        if let Some(result) = self.discover_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.discover_rx = None;
            self.discovering = false;
            match result {
                Ok(value) => {
                    self.discovered = value;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
            self.structure_dirty = true;
        }
    }

    fn apply(&mut self, result: Result<(), String>) {
        if let crate::actions::LocalOperationStatus::Running { token } = self.operation_status.get()
            && token.starts_with(crate::actions::PRINTER_OPERATION_TYPE)
        {
            self.operation_status.set(match &result {
                Ok(()) => crate::actions::LocalOperationStatus::Succeeded {
                    token,
                    message: t("settings-printers-op-done"),
                },
                Err(error) => crate::actions::LocalOperationStatus::Failed {
                    token,
                    message: error.clone(),
                },
            });
        }
        self.error = result.err();
        self.request_refresh();
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }
    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards: Vec<El> = Vec::new();
        if let Some(error) = &self.error {
            cards.push(card(
                &t("settings-printers-attention"),
                rows(vec![caption(error)]),
            ));
        }

        let refresh_label = if self.loading {
            t("settings-printers-refreshing")
        } else {
            t("settings-printers-refresh")
        };
        let refresh = Box::new(
            Button::new(
                "settings.printers.refresh",
                &refresh_label,
                Some(REFRESH_KEY),
            )
            .content_sized(),
        );
        let printers = if self.printers.is_empty() {
            rows(vec![caption(&t("settings-printers-empty"))])
        } else {
            rows(self.printers.iter().map(|p| self.printer_row(p)).collect())
        };
        cards.push(card_with_action(
            &t("settings-printers-card"),
            refresh,
            printers,
        ));

        let find_label = if self.discovering {
            t("settings-printers-searching")
        } else {
            t("settings-printers-find")
        };
        let find = Box::new(
            Button::new("settings.printers.discover", &find_label, Some(SCAN_KEY)).content_sized(),
        );
        let discovery = if self.discovered.is_empty() {
            rows(vec![caption(&t(if self.discovering {
                "settings-printers-discovering"
            } else {
                "settings-printers-discover-empty"
            }))])
        } else {
            rows(self.discovered.iter().map(|p| discovered_row(p)).collect())
        };
        cards.push(card_with_action(
            &t("settings-printers-nearby"),
            find,
            discovery,
        ));

        let jobs = if self.jobs.is_empty() {
            rows(vec![caption(&t("settings-printers-no-jobs"))])
        } else {
            rows(self.jobs.iter().map(job_row).collect())
        };
        cards.push(card(&t("settings-printers-activity"), jobs));

        let scanners = if self.scanners.is_empty() {
            rows(vec![caption(&t("settings-printers-no-scanners"))])
        } else {
            rows(
                self.scanners
                    .iter()
                    .map(|s| {
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(Box::new(Text::new(&s.description).with_size(13.0)))
                                .push(Box::new(Text::new(&s.device).with_size(11.0).dimmed())),
                        ) as El
                    })
                    .collect(),
            )
        };
        cards.push(card(&t("settings-printers-scanners"), scanners));
        crate::widgets::cards(cards)
    }

    fn printer_row(&self, printer: &PrinterQueue) -> El {
        let default_key = format!("settings.printers.default.{}", slug(&printer.name));
        let enabled_key = format!("settings.printers.enabled.{}", slug(&printer.name));
        let remove_key = format!("settings.printers.remove.{}", slug(&printer.name));
        let confirm_key = format!("settings.printers.confirm_remove.{}", slug(&printer.name));
        let state = state_label(printer.state);
        let mut top = Row::new()
            .push(Box::new(Text::new(&printer.name).with_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(semantic_status_pill(&state, state_tone(printer.state)));
        if !printer.default {
            let make_default = t("settings-printers-make-default");
            top = top.push(Box::new(
                Button::new(&default_key, &make_default, Some(&default_key)).content_sized(),
            ));
        }
        let pause_label = t(if printer.state == QueueState::Paused {
            "settings-printers-resume"
        } else {
            "settings-printers-pause"
        });
        top = top.push(Box::new(
            Button::new(&enabled_key, &pause_label, Some(&enabled_key)).content_sized(),
        ));
        let remove_label = t(
            if self.confirm_remove.as_deref() == Some(printer.name.as_str()) {
                "settings-printers-remove-confirm"
            } else {
                "settings-printers-remove"
            },
        );
        let remove_action = if self.confirm_remove.as_deref() == Some(printer.name.as_str()) {
            confirm_key
        } else {
            remove_key
        };
        top = top.push(Box::new(
            Button::new(&remove_action, &remove_label, Some(&remove_action)).content_sized(),
        ));
        let role = t(if printer.default {
            "settings-printers-role-default"
        } else {
            "settings-printers-role-configured"
        });
        let ipp_reason = printer.telemetry.state_reasons.first();
        let detail = if let Some(reason) = printer.reason.as_ref().or(ipp_reason) {
            t_args(
                "settings-printers-detail-reason",
                &[("role", role.into()), ("reason", reason.clone().into())],
            )
        } else if !printer.accepting_jobs {
            t("settings-printers-not-accepting")
        } else if printer.default {
            t("settings-printers-default-accepting")
        } else {
            t("settings-printers-accepting")
        };
        let supplies = match &printer.telemetry.supplies {
            Some(supplies) if supplies.is_empty() => t("settings-printers-supplies-none"),
            Some(supplies) => {
                let list = supplies
                    .iter()
                    .map(|supply| match supply.level_percent {
                        Some(level) => t_args(
                            "settings-printers-supply-level",
                            &[
                                ("name", supply.name.clone().into()),
                                ("level", ArgValue::Number(f64::from(level))),
                            ],
                        ),
                        None => t_args(
                            "settings-printers-supply-unknown",
                            &[("name", supply.name.clone().into())],
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                t_args("settings-printers-supplies-list", &[("list", list.into())])
            }
            None if printer.telemetry.available => t("settings-printers-supplies-unreported"),
            None => t("settings-printers-ipp-unavailable"),
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(top))
                .push(Box::new(Text::new(&detail).with_size(11.0).dimmed()))
                .push(Box::new(Text::new(&supplies).with_size(11.0).dimmed())),
        )
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PrintersPane::new())
}

fn state_label(state: QueueState) -> String {
    t(match state {
        QueueState::Ready => "settings-printers-state-ready",
        QueueState::Printing => "settings-printers-state-printing",
        QueueState::Paused => "settings-printers-state-paused",
        QueueState::Offline => "settings-printers-state-offline",
    })
}

fn state_tone(state: QueueState) -> StatusTone {
    match state {
        QueueState::Ready | QueueState::Printing => StatusTone::Success,
        QueueState::Paused | QueueState::Offline => StatusTone::Neutral,
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn discovered_row(p: &DiscoveredPrinter) -> El {
    let key = format!("settings.printers.add.{}", slug(&p.uri));
    let add = t("settings-printers-add");
    let location = p
        .location
        .clone()
        .unwrap_or_else(|| t("settings-printers-driverless"));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&p.name).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Button::new(&key, &add, Some(&key)).content_sized(),
                    )),
            ))
            .push(Box::new(Text::new(&location).with_size(11.0).dimmed())),
    )
}

fn job_row(job: &PrintJob) -> El {
    let cancel = format!("settings.printers.job.cancel.{}", slug(&job.id));
    let retry = format!("settings.printers.job.retry.{}", slug(&job.id));
    let size = ui::format::bytes_si(job.size_bytes);
    let state = job
        .state
        .map_or_else(|| t("settings-printers-job-state-missing"), job_state_label);
    let progress = match (job.impressions_completed, job.impressions_total) {
        (Some(done), Some(total)) if total > 0 => t_args(
            "settings-printers-job-progress",
            &[
                ("done", ArgValue::Number(f64::from(done))),
                ("total", ArgValue::Number(f64::from(total))),
                (
                    "percent",
                    ArgValue::Number(f64::from(job.progress_percent().unwrap_or(0))),
                ),
            ],
        ),
        (Some(done), _) => t_args(
            "settings-printers-job-done",
            &[("done", ArgValue::Number(f64::from(done)))],
        ),
        _ => t("settings-printers-job-progress-unknown"),
    };
    let reason = job
        .state_reasons
        .first()
        .map_or(String::new(), |reason| format!(" · {reason}"));
    let retry_label = t("settings-printers-retry");
    let cancel_label = t("settings-printers-cancel");
    let mut action_row = Row::new()
        .push(Box::new(
            Text::new(&format!("{} → {}", job.id, job.destination)).with_size(13.0),
        ))
        .push(Box::new(Spacer::new()));
    if matches!(
        job.state,
        Some(JobState::Stopped | JobState::Canceled | JobState::Aborted)
    ) {
        action_row = action_row.push(Box::new(
            Button::new(&retry, &retry_label, Some(&retry)).content_sized(),
        ));
    }
    if matches!(
        job.state,
        None | Some(JobState::Pending | JobState::Held | JobState::Processing | JobState::Stopped)
    ) {
        action_row = action_row.push(Box::new(
            Button::new(&cancel, &cancel_label, Some(&cancel)).content_sized(),
        ));
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(action_row))
            .push(Box::new(
                Text::new(&format!("{} · {} · {}", job.owner, size, job.submitted))
                    .with_size(11.0)
                    .dimmed(),
            ))
            .push(Box::new(
                Text::new(&format!("{state} · {progress}{reason}"))
                    .with_size(11.0)
                    .dimmed(),
            )),
    )
}

fn job_state_label(state: JobState) -> String {
    t(match state {
        JobState::Pending => "settings-printers-job-waiting",
        JobState::Held => "settings-printers-job-held",
        JobState::Processing => "settings-printers-job-printing",
        JobState::Stopped => "settings-printers-job-stopped",
        JobState::Canceled => "settings-printers-job-canceled",
        JobState::Aborted => "settings-printers-job-failed",
        JobState::Completed => "settings-printers-job-completed",
        JobState::Unknown => "settings-printers-job-unknown",
    })
}

fn take_bool(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
    let pressed = matches!(state.get(root, key), Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), automerge::ScalarValue::Boolean(true)));
    if pressed {
        let _ = state.put(root, key, false);
    }
    pressed
}

impl Widget for PrintersPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        if let Some(crate::actions::LocalPaneCommand::Printer(operation)) = self.command_state.get()
        {
            self.command_state.set(None);
            let key = match operation {
                crate::actions::PrinterOperation::Refresh => REFRESH_KEY.into(),
                crate::actions::PrinterOperation::Discover => SCAN_KEY.into(),
                crate::actions::PrinterOperation::SetDefault { name } => {
                    queue_action_key("default", &name)
                }
                crate::actions::PrinterOperation::SetEnabled { name, .. } => {
                    queue_action_key("enabled", &name)
                }
                crate::actions::PrinterOperation::Remove { name } => {
                    queue_action_key("remove", &name)
                }
                crate::actions::PrinterOperation::CancelJob { id } => job_action_key("cancel", &id),
                crate::actions::PrinterOperation::RetryJob { id } => job_action_key("retry", &id),
            };
            let _ = state.put(&root, key, true);
        }
        {
            let state = &mut state;
            let root = &root;
            self.poll_background();
            if self.last_refresh.elapsed() >= Duration::from_secs(3) {
                self.request_refresh();
            }
            if take_bool(state, root, REFRESH_KEY) {
                self.request_refresh();
            }
            if take_bool(state, root, SCAN_KEY) {
                self.request_discovery();
            }
            for p in self.discovered.clone() {
                let key = format!("settings.printers.add.{}", slug(&p.uri));
                if take_bool(state, root, &key) {
                    let result = self.service.add(&p);
                    self.apply(result);
                }
            }
            for p in self.printers.clone() {
                let default = queue_action_key("default", &p.name);
                let enabled = queue_action_key("enabled", &p.name);
                let remove = queue_action_key("remove", &p.name);
                let confirm = format!("settings.printers.confirm_remove.{}", slug(&p.name));
                if take_bool(state, root, &default) {
                    let result = self.service.set_default(&p.name);
                    self.apply(result);
                }
                if take_bool(state, root, &enabled) {
                    let result = self
                        .service
                        .set_enabled(&p.name, p.state == QueueState::Paused);
                    self.apply(result);
                }
                if take_bool(state, root, &remove) {
                    self.confirm_remove = Some(p.name.clone());
                    self.structure_dirty = true;
                }
                if take_bool(state, root, &confirm) {
                    self.confirm_remove = None;
                    let result = self.service.remove(&p.name);
                    self.apply(result);
                }
            }
            for job in self.jobs.clone() {
                let cancel = job_action_key("cancel", &job.id);
                let retry = job_action_key("retry", &job.id);
                if take_bool(state, root, &cancel) {
                    let result = self.service.cancel(&job.id);
                    self.apply(result);
                }
                if take_bool(state, root, &retry) {
                    let result = self.service.retry(&job.id);
                    self.apply(result);
                }
            }
            if let Some(c) = &mut self.composed {
                c.sync(state, root);
            }
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "child was just created in this same tree"
        )]
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(ch) = taffy.children(node)
            && let Some(&child) = ch.first()
        {
            c.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(ch) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = ch.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn name(&self) -> &'static str {
        "PrintersPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingService {
        calls: Mutex<Vec<String>>,
    }
    impl RecordingService {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    fn queue() -> PrinterQueue {
        PrinterQueue {
            name: "Office".into(),
            state: QueueState::Ready,
            default: false,
            accepting_jobs: true,
            reason: None,
            telemetry: os_actions::printing::PrinterTelemetry::default(),
        }
    }
    fn job() -> PrintJob {
        PrintJob {
            id: "Office-42".into(),
            title: Some("Quarterly report.pdf".into()),
            owner: "ada".into(),
            size_bytes: 2048,
            submitted: "now".into(),
            destination: "Office".into(),
            state: Some(JobState::Processing),
            state_reasons: vec!["job-printing".into()],
            impressions_completed: Some(1),
            impressions_total: Some(4),
        }
    }
    impl PrintService for RecordingService {
        fn queues(&self) -> Result<Vec<PrinterQueue>, String> {
            Ok(vec![queue()])
        }
        fn jobs(&self) -> Result<Vec<PrintJob>, String> {
            Ok(vec![job()])
        }
        fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String> {
            Ok(vec![DiscoveredPrinter {
                uri: "ipps://office/ipp/print".into(),
                name: "Office".into(),
                location: Some("Room 2".into()),
            }])
        }
        fn scanners(&self) -> Result<Vec<Scanner>, String> {
            Ok(vec![])
        }
        fn add(&self, p: &DiscoveredPrinter) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("add:{}", p.uri));
            Ok(())
        }
        fn set_default(&self, n: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("default:{n}"));
            Ok(())
        }
        fn set_enabled(&self, n: &str, e: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("enabled:{n}:{e}"));
            Ok(())
        }
        fn remove(&self, n: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("remove:{n}"));
            Ok(())
        }
        fn cancel(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("cancel:{id}"));
            Ok(())
        }
        fn retry(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("retry:{id}"));
            Ok(())
        }
    }

    /// Printer actions reach one typed backend and removal needs a second action.
    #[test]
    fn printer_actions_use_one_backend_and_confirm_removal() {
        // Case `actions_reach_one_typed_backend`.
        {
            let service = Arc::new(RecordingService::new());
            let mut pane = PrintersPane::with_service(service.clone());
            let mut doc = AutoCommit::new();
            doc.put(&automerge::ROOT, "settings.printers.default.office", true)
                .unwrap();
            doc.put(
                &automerge::ROOT,
                "settings.printers.job.cancel.office_42",
                true,
            )
            .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                service.calls.lock().unwrap().as_slice(),
                ["default:Office", "cancel:Office-42"]
            );
        }

        // Case `remove_requires_a_second_explicit_action`.
        {
            let service = Arc::new(RecordingService::new());
            let mut pane = PrintersPane::with_service(service.clone());
            let mut doc = AutoCommit::new();
            doc.put(&automerge::ROOT, "settings.printers.remove.office", true)
                .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(service.calls.lock().unwrap().is_empty());
            doc.put(
                &automerge::ROOT,
                "settings.printers.confirm_remove.office",
                true,
            )
            .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(service.calls.lock().unwrap().as_slice(), ["remove:Office"]);
        }
    }
}
