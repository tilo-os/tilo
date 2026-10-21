//! Daily printer/scanner control: printers through the shared typed CUPS
//! service, scanners through SANE.
//!
//! Settings owns discovery, adding a driverless queue, defaults, queue health
//! and recovery. A future Print Centre may offer long job history, finishing
//! workflows and diagnostics, but must consume the same `PrintService` rather
//! than maintaining another printer database. Every change is a
//! [`PrinterEffect`], offered on the queue, nearby printer or job it names.

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;

use os_actions::printing::{DiscoveredPrinter, JobState, PrintJob, PrinterQueue, QueueState};
use tilo_capture::scanner::Scanner;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::tilo_resource::{self, Effect};
use ui::time::{Duration, Instant};
use ui::widget::{Button, ChipStyle, ControlSize, Row, RowMenu, Widget};

use crate::channel;
use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Body, Item, SettingsSection, StatusTone, block, error_caption, note, row, rows, section,
    section_action, thing_row,
};

crate::effects::effect_family!(PrinterEffect => answer);

crate::area::pane_area! {
    PrintersArea;
    fn poll(&self) {
        PrintersArea::poll(self);
    }
}

/// The window's [`PrintersArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&PrintersArea> {
    app.areas().find()
}

/// Which of the last refresh's lists could not be read, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrinterReadErrors {
    printers: Option<String>,
    jobs: Option<String>,
    scanners: Option<String>,
}

/// Keep a list that was read; answer why one was not, clearing what was
/// shown so a stale list is not mistaken for the current one.
fn adopt<T>(read: Result<Vec<T>, String>, shown: &mut Vec<T>) -> Option<String> {
    match read {
        Ok(value) => {
            *shown = value;
            None
        }
        Err(error) => {
            shown.clear();
            Some(error)
        }
    }
}

/// The most queues, nearby printers and jobs published as rows.
const MAX_ROWS: usize = 64;

/// Changes to this device's printers and print jobs.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PrinterEffect {
    /// Read the printers, jobs and scanners again.
    #[effect(job, idempotent)]
    RefreshPrinters,
    /// Look for driverless printers on the network.
    #[effect(job, idempotent)]
    FindPrinters,
    /// Add a nearby driverless printer as a queue.
    #[effect(job, row, label = "Add printer")]
    AddPrinter {
        #[param("The printer's address, as found nearby")]
        uri: String,
    },
    /// Make a printer the one apps print to unless told otherwise.
    #[effect(job, row, idempotent, label = "Make default")]
    SetDefaultPrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Pause a printer's queue, or let it print again.
    #[effect(job, row, idempotent)]
    SetPrinterPaused {
        #[param("The printer's queue name")]
        printer: String,
        #[param("Whether the queue holds its jobs")]
        paused: bool,
    },
    /// Remove a printer's queue from this device.
    #[effect(job, row, destructive, label = "Remove printer")]
    RemovePrinter {
        #[param("The printer's queue name")]
        printer: String,
    },
    /// Cancel a print job.
    #[effect(job, row, destructive, label = "Cancel job")]
    CancelPrintJob {
        #[param("The job's id")]
        job: String,
    },
    /// Send a stopped or failed print job again.
    #[effect(job, row, label = "Retry job")]
    RetryPrintJob {
        #[param("The job's id")]
        job: String,
    },
}

/// Answer `effect`: reads start on the model; changes run on a worker and
/// then read the printers again.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: PrinterEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = crate::panes::printers::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    match effect {
        PrinterEffect::RefreshPrinters => {
            model.borrow_mut().request_refresh();
            Ok(Effected::done())
        }
        PrinterEffect::FindPrinters => {
            model.borrow_mut().request_discovery();
            Ok(Effected::done())
        }
        effect => {
            let discovered = model.borrow().discovered.clone();
            let service = Arc::clone(&model.borrow().service);
            Ok(crate::device_read::job(
                cx,
                move || carry_out(service.as_ref(), &discovered, &effect),
                |app: &mut SettingsApp, outcome: Result<(), String>, _| {
                    let model = crate::panes::printers::area(app)
                        .ok_or_else(crate::area::missing)?
                        .model();
                    let mut model = model.borrow_mut();
                    model.error = outcome.as_ref().err().cloned();
                    model.changed = true;
                    model.request_refresh();
                    outcome.map(|()| Effected::done())
                },
            ))
        }
    }
}

/// Carry out a change on `service`. Never called on the UI thread.
fn carry_out(
    service: &dyn PrintService,
    discovered: &[DiscoveredPrinter],
    effect: &PrinterEffect,
) -> Result<(), String> {
    match effect {
        PrinterEffect::RefreshPrinters | PrinterEffect::FindPrinters => Ok(()),
        PrinterEffect::AddPrinter { uri } => {
            let printer = discovered
                .iter()
                .find(|printer| &printer.uri == uri)
                .ok_or_else(|| format!("no printer at `{uri}` was found nearby"))?;
            service.add(printer)
        }
        PrinterEffect::SetDefaultPrinter { printer } => service.set_default(printer),
        PrinterEffect::SetPrinterPaused { printer, paused } => {
            service.set_enabled(printer, !paused)
        }
        PrinterEffect::RemovePrinter { printer } => service.remove(printer),
        PrinterEffect::CancelPrintJob { job } => service.cancel(job),
        PrinterEffect::RetryPrintJob { job } => service.retry(job),
    }
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
        tilo_capture::scanner::scanners()
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

/// What the printers area keeps for the window: the model, read when
/// first needed.
pub struct PrintersArea {
    cx: PaneCx,
    model: OnceCell<Shared<PrintersModel>>,
}

impl PrintersArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: OnceCell::new(),
        }
    }

    /// The model, created on first use over CUPS, or over sample printers
    /// for a screenshot.
    pub fn model(&self) -> &Shared<PrintersModel> {
        self.model.get_or_init(|| {
            let service: Arc<dyn PrintService> = if self.cx.reaches_devices() {
                Arc::new(CupsService)
            } else {
                Arc::new(SamplePrintService)
            };
            let mut model =
                PrintersModel::new(self.cx.clone(), service, !self.cx.reaches_devices());
            model.request_refresh();
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Take in finished reads, if the model exists. Only an open pane
    /// keeps reading the queues again.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_background();
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per queue, nearby printer and job, offering what can be done
    /// to it now.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let printers = model.printers.iter().take(MAX_ROWS).map(printer_live_row);
        let nearby = model.discovered.iter().take(MAX_ROWS).map(nearby_live_row);
        let jobs = model.jobs.iter().take(MAX_ROWS).map(job_live_row);
        printers.chain(nearby).chain(jobs).collect()
    }
}

/// A queue as a row of the published surface, offering what can be done to
/// it now; the pane's controls for it bind to it.
fn printer_live_row(printer: &PrinterQueue) -> LiveRow {
    let name = printer.name.clone();
    let paused = printer.state == QueueState::Paused;
    let mut row = LiveRow::new(
        format!("settings.printer:{name}"),
        "settings.printer",
        name.clone(),
    )
    .operation(
        SettingsEffect::from(PrinterEffect::SetPrinterPaused {
            printer: name.clone(),
            paused: !paused,
        })
        .bound(),
    )
    .operation(
        SettingsEffect::from(PrinterEffect::RemovePrinter {
            printer: name.clone(),
        })
        .bound(),
    );
    if !printer.default {
        row = row.operation(
            SettingsEffect::from(PrinterEffect::SetDefaultPrinter { printer: name }).bound(),
        );
    }
    row
}

/// A printer found nearby as a row of the published surface.
fn nearby_live_row(printer: &DiscoveredPrinter) -> LiveRow {
    LiveRow::new(
        format!("settings.nearby-printer:{}", printer.uri),
        "settings.nearby-printer",
        printer.name.clone(),
    )
    .operation(
        SettingsEffect::from(PrinterEffect::AddPrinter {
            uri: printer.uri.clone(),
        })
        .bound(),
    )
}

/// A print job as a row of the published surface.
fn job_live_row(job: &PrintJob) -> LiveRow {
    let mut row = LiveRow::new(
        format!("settings.print-job:{}", job.id),
        "settings.print-job",
        job.title.clone().unwrap_or_else(|| job.id.clone()),
    );
    if retryable(job) {
        row = row.operation(
            SettingsEffect::from(PrinterEffect::RetryPrintJob {
                job: job.id.clone(),
            })
            .bound(),
        );
    }
    if cancellable(job) {
        row = row.operation(
            SettingsEffect::from(PrinterEffect::CancelPrintJob {
                job: job.id.clone(),
            })
            .bound(),
        );
    }
    row
}

fn retryable(job: &PrintJob) -> bool {
    matches!(
        job.state,
        Some(JobState::Stopped | JobState::Canceled | JobState::Aborted)
    )
}

fn cancellable(job: &PrintJob) -> bool {
    matches!(
        job.state,
        None | Some(JobState::Pending | JobState::Held | JobState::Processing | JobState::Stopped)
    )
}

pub struct PrintersModel {
    cx: PaneCx,
    service: Arc<dyn PrintService>,
    sample: bool,
    printers: Vec<PrinterQueue>,
    jobs: Vec<PrintJob>,
    discovered: Vec<DiscoveredPrinter>,
    scanners: Vec<Scanner>,
    refresh_rx: Option<channel::Receiver<PrinterSnapshot>>,
    discover_rx: Option<channel::Receiver<Result<Vec<DiscoveredPrinter>, String>>>,
    last_refresh: Instant,
    error: Option<String>,
    /// Why each list could not be read in the last refresh. A list that
    /// could not be read is shown as that failure, not as empty, and stays
    /// so until a later refresh reads it, whatever else succeeds meanwhile.
    read_errors: PrinterReadErrors,
    changed: bool,
}

struct PrinterSnapshot {
    printers: Result<Vec<PrinterQueue>, String>,
    jobs: Result<Vec<PrintJob>, String>,
    scanners: Result<Vec<Scanner>, String>,
}

impl PrintersModel {
    fn new(cx: PaneCx, service: Arc<dyn PrintService>, sample: bool) -> Self {
        Self {
            cx,
            service,
            sample,
            printers: Vec::new(),
            jobs: Vec::new(),
            discovered: Vec::new(),
            scanners: Vec::new(),
            refresh_rx: None,
            discover_rx: None,
            last_refresh: ui::time::now(),
            error: None,
            read_errors: PrinterReadErrors::default(),
            changed: false,
        }
    }

    pub(crate) fn request_refresh(&mut self) {
        if self.refresh_rx.is_some() {
            return;
        }
        // Capture and test snapshots are already in memory. Publishing their
        // rows must not depend on whether an OS worker has been scheduled.
        if self.sample {
            self.adopt_snapshot(PrinterSnapshot {
                printers: self.service.queues(),
                jobs: self.service.jobs(),
                scanners: self.service.scanners(),
            });
            return;
        }
        let service = self.service.clone();
        let (tx, rx) = channel::channel();
        self.refresh_rx = Some(rx);

        if let Err(error) = crate::device_read::thread(
            "tilo-printer-status",
            Box::new(move || {
                let _ = tx.send(PrinterSnapshot {
                    printers: service.queues(),
                    jobs: service.jobs(),
                    scanners: service.scanners(),
                });
            }),
        ) {
            self.refresh_rx = None;

            self.error = Some(t_args(
                "settings-printers-worker-failed",
                &[("error", ArgValue::Text(error.to_string()))],
            ));
        }
    }

    pub(crate) fn request_discovery(&mut self) {
        if self.discover_rx.is_some() {
            return;
        }
        let service = self.service.clone();
        let (tx, rx) = channel::channel();
        self.discover_rx = Some(rx);

        if let Err(error) = crate::device_read::thread(
            "tilo-printer-discovery",
            Box::new(move || {
                let _ = tx.send(service.discover());
            }),
        ) {
            self.discover_rx = None;

            self.error = Some(t_args(
                "settings-printers-discover-failed",
                &[("error", ArgValue::Text(error.to_string()))],
            ));
        }
    }

    fn adopt_snapshot(&mut self, result: PrinterSnapshot) {
        self.last_refresh = ui::time::now();
        self.read_errors = PrinterReadErrors {
            printers: adopt(result.printers, &mut self.printers),
            jobs: adopt(result.jobs, &mut self.jobs),
            scanners: adopt(result.scanners, &mut self.scanners),
        };
        self.changed = true;
    }

    fn poll_background(&mut self) {
        if let Some(result) = self.refresh_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.refresh_rx = None;

            self.adopt_snapshot(result);
        }
        if let Some(result) = self.discover_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.discover_rx = None;

            match result {
                Ok(value) => {
                    self.discovered = value;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
            self.changed = true;
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut sections: Vec<SettingsSection> = Vec::new();
        if let Some(error) = &self.error {
            sections.push(section(
                &t("settings-printers-attention"),
                rows(vec![block(error_caption(error))]),
            ));
        }

        // The queues are read again every few seconds: no refresh.
        let printers = if let Some(error) = &self.read_errors.printers {
            rows(vec![block(error_caption(&t_args(
                "settings-printers-read-error",
                &[("error", error.as_str().into())],
            )))])
        } else if self.printers.is_empty() {
            rows(vec![note(&t("settings-printers-empty"))])
        } else {
            rows(self.printers.iter().map(|p| self.printer_row(p)).collect())
        };
        sections.push(section(&t("settings-printers-card"), printers));

        let find_label = if self.discover_rx.is_some() {
            t("settings-printers-searching")
        } else {
            t("settings-printers-find")
        };
        let find = self.cx.bind_invocation(
            Button::new("settings.printers.discover", &find_label),
            PrinterEffect::FindPrinters,
        );
        let discovery = if self.discovered.is_empty() {
            rows(vec![note(&t(if self.discover_rx.is_some() {
                "settings-printers-discovering"
            } else {
                "settings-printers-discover-empty"
            }))])
        } else {
            rows(
                self.discovered
                    .iter()
                    .map(|p| self.discovered_row(p))
                    .collect(),
            )
        };
        sections.push(section_action(
            &t("settings-printers-nearby"),
            find,
            discovery,
        ));

        let jobs = if let Some(error) = &self.read_errors.jobs {
            rows(vec![block(error_caption(&t_args(
                "settings-printers-jobs-read-error",
                &[("error", error.as_str().into())],
            )))])
        } else if self.jobs.is_empty() {
            rows(vec![note(&t("settings-printers-no-jobs"))])
        } else {
            rows(self.jobs.iter().map(|job| self.job_row(job)).collect())
        };
        sections.push(section(&t("settings-printers-activity"), jobs));

        let scanners = self.scanner_rows();
        sections.push(section(&t("settings-printers-scanners"), scanners));
        crate::widgets::page(sections)
    }

    fn scanner_rows(&self) -> Body {
        if let Some(error) = &self.read_errors.scanners {
            rows(vec![block(error_caption(&t_args(
                "settings-printers-scanners-read-error",
                &[("error", error.as_str().into())],
            )))])
        } else if self.scanners.is_empty() {
            rows(vec![note(&t("settings-printers-no-scanners"))])
        } else {
            rows(
                self.scanners
                    .iter()
                    .map(|s| {
                        row(Box::new(
                            thing_row(
                                &format!("settings.printers.scanner.{}", slug(&s.device)),
                                &s.description,
                                std::slice::from_ref(&s.device),
                                None,
                            )
                            .icon(Icon::ScanLine),
                        ))
                    })
                    .collect(),
            )
        }
    }

    /// A printer: its name and state, what it is doing and its supplies,
    /// and its menu (make default, pause or resume, remove).
    fn printer_row(&self, printer: &PrinterQueue) -> Item {
        let name = printer.name.clone();
        let live = printer_live_row(printer).into_resource();
        let mut menu = RowMenu::new(
            format!("settings.printers.more.{}", slug(&name)),
            &t_args("settings-row-more", &[("name", name.clone().into())]),
        );
        if !printer.default {
            menu = menu.action(
                t("settings-printers-make-default"),
                self.cx.row_invocation(
                    &live,
                    PrinterEffect::SetDefaultPrinter {
                        printer: name.clone(),
                    },
                ),
            );
        }
        let paused = printer.state == QueueState::Paused;
        menu = menu
            .action(
                t(if paused {
                    "settings-printers-resume"
                } else {
                    "settings-printers-pause"
                }),
                self.cx.row_invocation(
                    &live,
                    PrinterEffect::SetPrinterPaused {
                        printer: name.clone(),
                        paused: !paused,
                    },
                ),
            )
            .destructive(
                t("settings-printers-remove"),
                self.cx.row_invocation(
                    &live,
                    PrinterEffect::RemovePrinter {
                        printer: name.clone(),
                    },
                ),
            );
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
                    .join("\n");
                t_args("settings-printers-supplies-list", &[("list", list.into())])
            }
            None if printer.telemetry.available => t("settings-printers-supplies-unreported"),
            None => t("settings-printers-ipp-unavailable"),
        };
        let state = state_label(printer.state);
        row(Box::new(
            thing_row(
                &format!("settings.printers.printer.{}", slug(&name)),
                &printer.name,
                &[detail, supplies],
                Some(Box::new(menu)),
            )
            .icon(Icon::Printer)
            .tags(&[(state.as_str(), ChipStyle::Tinted(state_tone(printer.state)))]),
        ))
    }

    fn discovered_row(&self, p: &DiscoveredPrinter) -> Item {
        let key = format!("settings.printers.add.{}", slug(&p.uri));
        let add = t("settings-printers-add");
        let location = p
            .location
            .clone()
            .unwrap_or_else(|| t("settings-printers-driverless"));
        let add_button = self.cx.bind_row(
            Button::new(&key, &add).control_size(ControlSize::Small),
            &nearby_live_row(p).into_resource(),
            PrinterEffect::AddPrinter { uri: p.uri.clone() },
        );
        row(Box::new(
            thing_row(
                &format!("settings.printers.nearby.{}", slug(&p.uri)),
                &p.name,
                &[location],
                Some(Box::new(add_button)),
            )
            .icon(Icon::Printer),
        ))
    }

    /// A print job: what is printing, where and for whom, how far it got,
    /// and the one thing to do with it (cancel, or retry a stopped one,
    /// whose cancel is in its menu).
    fn job_row(&self, job: &PrintJob) -> Item {
        let live = job_live_row(job).into_resource();
        let size = ui::format::size(job.size_bytes);
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
        let retry = || {
            self.cx.bind_row(
                Button::new(
                    &format!("settings.printers.job.retry.{}", slug(&job.id)),
                    &t("settings-printers-retry"),
                )
                .control_size(ControlSize::Small),
                &live,
                PrinterEffect::RetryPrintJob {
                    job: job.id.clone(),
                },
            )
        };
        let cancel_effect = PrinterEffect::CancelPrintJob {
            job: job.id.clone(),
        };
        let trailing: Option<Box<dyn Widget>> = match (retryable(job), cancellable(job)) {
            (true, true) => Some(Box::new(
                Row::new()
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .push(Box::new(retry()))
                    .push(Box::new(
                        RowMenu::new(
                            format!("settings.printers.job.more.{}", slug(&job.id)),
                            &t_args("settings-row-more", &[("name", job.id.clone().into())]),
                        )
                        .destructive(
                            t("settings-printers-cancel"),
                            self.cx.row_invocation(&live, cancel_effect),
                        ),
                    )),
            )),
            (true, false) => Some(Box::new(retry())),
            (false, true) => Some(Box::new(
                self.cx.bind_row(
                    Button::new(
                        &format!("settings.printers.job.cancel.{}", slug(&job.id)),
                        &t("settings-printers-cancel"),
                    )
                    .control_size(ControlSize::Small),
                    &live,
                    cancel_effect,
                ),
            )),
            (false, false) => None,
        };
        let title = job.title.clone().unwrap_or_else(|| job.id.clone());
        let sent = t_args(
            "settings-printers-job-sent",
            &[
                ("destination", job.destination.clone().into()),
                ("owner", job.owner.clone().into()),
                ("size", size.into()),
            ],
        );
        let details = vec![sent, progress];
        row(Box::new(
            thing_row(
                &format!("settings.printers.job.{}", slug(&job.id)),
                &title,
                &details,
                trailing,
            )
            .icon(Icon::FileText)
            .tags(&[(state.as_str(), ChipStyle::Tinted(StatusTone::Neutral))]),
        ))
    }
}

impl PaneModel for PrintersModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_background();
        if self.last_refresh.elapsed() >= Duration::from_secs(3) {
            self.request_refresh();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "PrintersPane"
    }

    fn busy(&self) -> bool {
        self.refresh_rx.is_some() || self.discover_rx.is_some()
    }
}

/// The printers a screenshot and the crate's tests show: one office queue
/// printing a report, one more found nearby.
struct SamplePrintService;

impl PrintService for SamplePrintService {
    fn queues(&self) -> Result<Vec<PrinterQueue>, String> {
        Ok(vec![PrinterQueue {
            name: "Office".into(),
            state: QueueState::Ready,
            default: true,
            accepting_jobs: true,
            reason: None,
            telemetry: os_actions::printing::PrinterTelemetry::default(),
        }])
    }
    fn jobs(&self) -> Result<Vec<PrintJob>, String> {
        Ok(vec![PrintJob {
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
        }])
    }
    fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String> {
        Ok(vec![DiscoveredPrinter {
            uri: "ipps://studio.local/ipp/print".into(),
            name: "Studio".into(),
            location: Some("Room 2".into()),
        }])
    }
    fn scanners(&self) -> Result<Vec<Scanner>, String> {
        Ok(Vec::new())
    }
    fn add(&self, _: &DiscoveredPrinter) -> Result<(), String> {
        Ok(())
    }
    fn set_default(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_enabled(&self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn cancel(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn retry(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn sample_area_rows_do_not_wait_for_workers() {
        let profile = tempfile::tempdir().unwrap();
        let area = PrintersArea::new(&PaneCx::in_file(profile.path().join("settings.json")));
        assert_eq!(
            area.rows().len(),
            2,
            "printer and job rows publish on the first read"
        );
        let model = area.model().borrow();
        assert!(
            model.refresh_rx.is_none(),
            "sample rows are ready when the area reads"
        );
        assert_eq!(model.printers.len(), 1);
        assert_eq!(model.jobs.len(), 1);
    }

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

    /// Each change reaches the one typed backend, for the printer or job it
    /// names; a nearby printer is added by its address.
    #[test]
    fn changes_reach_one_typed_backend() {
        let service = RecordingService::new();
        let nearby = service.discover().unwrap();
        for effect in [
            PrinterEffect::SetDefaultPrinter {
                printer: "Office".into(),
            },
            PrinterEffect::CancelPrintJob {
                job: "Office-42".into(),
            },
            PrinterEffect::SetPrinterPaused {
                printer: "Office".into(),
                paused: true,
            },
            PrinterEffect::AddPrinter {
                uri: "ipps://office/ipp/print".into(),
            },
        ] {
            carry_out(&service, &nearby, &effect).unwrap();
        }
        assert!(
            carry_out(
                &service,
                &nearby,
                &PrinterEffect::AddPrinter {
                    uri: "ipps://elsewhere".into()
                }
            )
            .is_err()
        );
        assert_eq!(
            service.calls.lock().unwrap().as_slice(),
            [
                "default:Office",
                "cancel:Office-42",
                "enabled:Office:false",
                "add:ipps://office/ipp/print"
            ]
        );
    }

    /// Queues and jobs are rows offering their changes; removing a queue
    /// asks first.
    #[test]
    fn printers_and_jobs_are_rows() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(PrinterEffect::RefreshPrinters))
            .unwrap();
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| {
                harness
                    .resources()
                    .iter()
                    .any(|resource| tilo_actions_has(resource, "settings.print-job:Office-42"))
            });
        app.effect_on(
            "settings.print-job:Office-42",
            &SettingsEffect::from(PrinterEffect::CancelPrintJob {
                job: "Office-42".into(),
            }),
        )
        .expect_err("cancelling a job asks first");
        app.effect_on(
            "settings.printer:Office",
            &SettingsEffect::from(PrinterEffect::SetPrinterPaused {
                printer: "Office".into(),
                paused: true,
            }),
        )
        .expect("the queue is paused");
    }

    fn tilo_actions_has(resource: &ui::tilo_actions::Resource, id: &str) -> bool {
        resource.id.as_str() == id
            || resource
                .children
                .iter()
                .any(|child| tilo_actions_has(child, id))
    }
}
