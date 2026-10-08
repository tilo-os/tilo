//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.
//! Reclaiming space is a [`StorageEffect`] the [`StorageModel`] runs.

use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use runtime_fs::storage::{
    DEFAULT_STORAGE_SOCKET, EvictionReport, EvictionSelection, StorageClient, SyncStorageSnapshot,
};
use std::sync::Arc;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected};
use ui::i18n::ArgValue;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Widget};

use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{El, caption, card, card_with_action, rows, value_row};

fn category_label(category: StorageCategory) -> String {
    t(match category {
        StorageCategory::InstalledApps => "settings-storage-cat-apps",
        StorageCategory::AppData => "settings-storage-cat-data",
        StorageCategory::Cache => "settings-storage-cat-cache",
        StorageCategory::SyncedFiles => "settings-storage-cat-synced",
    })
}

/// Reclaiming this device's space, and its storage tools.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum StorageEffect {
    /// Remove the apps' caches.
    #[effect(job, destructive)]
    CleanUpCaches,
    /// Free the local copies of synced files that are safe on the server.
    #[effect(job, destructive)]
    FreeSyncedFiles,
    /// Open Disks.
    #[effect(job)]
    OpenDisks,
    /// Open Backups.
    #[effect(job)]
    OpenBackups,
}

/// Answer `effect` on the area's model.
#[expect(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: StorageEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let reply = cx.reply();
    app.storage().model().borrow_mut().run(&effect, reply);
    Ok(Effected::Pending)
}

/// What the storage area keeps for the window: the model, scanned when
/// first needed.
pub struct StorageArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<StorageModel>>,
    reader: Option<Arc<dyn StorageReadSource>>,
    device: bool,
}

impl StorageArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            reader: None,
            device: cx.reaches_devices(),
        }
    }

    /// The model, created on first use: a capture's scans nothing and asks
    /// no storage service.
    pub fn model(&self) -> &Shared<StorageModel> {
        self.model.get_or_init(|| {
            let model = if self.device {
                self.reader.as_ref().map_or_else(
                    || StorageModel::new(self.cx.clone()),
                    |reader| StorageModel::with_reader(self.cx.clone(), Arc::clone(reader)),
                )
            } else {
                StorageModel::with_roots(
                    self.cx.clone(),
                    StorageRoots {
                        app_payloads: None,
                        room_data: None,
                        app_cache: None,
                        sync_replica: None,
                    },
                )
            };
            std::rc::Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Take in finished background work, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            let mut model = model.borrow_mut();
            if model.poll() {
                model.changed = true;
            }
        }
    }
}

struct StorageRead {
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
}

trait StorageReadSource: Send + Sync {
    fn read(&self) -> Result<StorageRead, String>;
}

struct DeviceStorageRead;

impl StorageReadSource for DeviceStorageRead {
    fn read(&self) -> Result<StorageRead, String> {
        Ok(read_roots(StorageRoots::discover()))
    }
}

fn read_roots(roots: StorageRoots) -> StorageRead {
    let snapshot = desktop_policy::scan_storage(&roots);
    let cleanup = roots
        .app_cache
        .as_deref()
        .and_then(|path| desktop_policy::preview_cache_cleanup(path).ok());
    StorageRead {
        roots,
        snapshot,
        cleanup,
    }
}

trait StorageCleanupSource: Send + Sync {
    fn clean(&self, preview: CleanupPreview, roots: StorageRoots) -> Result<StorageRead, String>;
}

struct DeviceStorageCleanup;

impl StorageCleanupSource for DeviceStorageCleanup {
    fn clean(&self, preview: CleanupPreview, roots: StorageRoots) -> Result<StorageRead, String> {
        let selected: Vec<usize> = (0..preview.items.len()).collect();
        desktop_policy::apply_cleanup(&preview, &selected).map_err(|error| error.to_string())?;
        Ok(read_roots(roots))
    }
}

struct CleanupJob {
    result: std::sync::mpsc::Receiver<Result<StorageRead, String>>,
    reply: EffectReply,
}

enum InitialStorageRead {
    Pending(std::sync::mpsc::Receiver<Result<StorageRead, String>>),
    Ready,
    Failed(String),
}

pub struct StorageModel {
    cx: PaneCx,
    initial: InitialStorageRead,
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
    cleanup_source: Arc<dyn StorageCleanupSource>,
    cleanup_job: Option<CleanupJob>,
    cleanup_notice: Option<String>,
    storage_socket: std::path::PathBuf,
    sync_snapshot: Option<SyncStorageSnapshot>,
    sync_message: Option<String>,
    sync_busy: bool,
    sync_result: Option<std::sync::mpsc::Receiver<Result<EvictionReport, String>>>,
    sync_reply: Option<EffectReply>,
    snapshot_result: Option<std::sync::mpsc::Receiver<Result<SyncStorageSnapshot, String>>>,
    replies: Replies,
    changed: bool,
}

impl StorageModel {
    /// Start this device's scan off the UI thread, independently of synced
    /// storage accounting.
    pub fn new(cx: PaneCx) -> Self {
        Self::with_reader(cx, Arc::new(DeviceStorageRead))
    }

    fn with_reader(cx: PaneCx, reader: Arc<dyn StorageReadSource>) -> Self {
        let mut model = Self::from_read(
            cx,
            StorageRead {
                roots: StorageRoots {
                    app_payloads: None,
                    room_data: None,
                    app_cache: None,
                    sync_replica: None,
                },
                snapshot: StorageSnapshot::default(),
                cleanup: None,
            },
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        model.initial =
            match ui::background::Background::current().spawn("storage_snapshot", move || {
                if sender.send(reader.read()).is_ok() {
                    wake.wake();
                }
            }) {
                Ok(()) => InitialStorageRead::Pending(receiver),
                Err(error) => InitialStorageRead::Failed(error.to_string()),
            };
        model.refresh_sync();
        model
    }

    pub fn with_roots(cx: PaneCx, roots: StorageRoots) -> Self {
        Self::from_read(cx, read_roots(roots))
    }

    fn from_read(cx: PaneCx, read: StorageRead) -> Self {
        let StorageRead {
            roots,
            snapshot,
            cleanup,
        } = read;
        Self {
            cx,
            initial: InitialStorageRead::Ready,
            roots,
            snapshot,
            cleanup,
            cleanup_source: Arc::new(DeviceStorageCleanup),
            cleanup_job: None,
            cleanup_notice: None,
            storage_socket: DEFAULT_STORAGE_SOCKET.into(),
            sync_snapshot: None,
            sync_message: None,
            sync_busy: false,
            sync_result: None,
            sync_reply: None,
            snapshot_result: None,
            replies: Replies::default(),
            changed: false,
        }
    }

    fn initial_message(&self) -> Option<String> {
        match &self.initial {
            InitialStorageRead::Ready => None,
            InitialStorageRead::Pending(_) => Some(t("settings-storage-scan-loading")),
            InitialStorageRead::Failed(error) => Some(t_args(
                "settings-storage-scan-failed",
                &[("error", error.clone().into())],
            )),
        }
    }

    fn poll_initial(&mut self) {
        let outcome = match &self.initial {
            InitialStorageRead::Pending(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(t("settings-storage-service-ended")))
                }
            },
            _ => None,
        };
        if let Some(outcome) = outcome {
            self.initial = match outcome {
                Ok(read) => {
                    self.roots = read.roots;
                    self.snapshot = read.snapshot;
                    self.cleanup = read.cleanup;
                    InitialStorageRead::Ready
                }
                Err(error) => InitialStorageRead::Failed(error),
            };
            self.changed = true;
        }
    }

    fn refresh_sync(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let socket = self.storage_socket.clone();
        std::thread::spawn(move || {
            let result = StorageClient::connect(socket)
                .and_then(|mut client| client.snapshot())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        if self.sync_snapshot.is_none() {
            self.sync_message = Some(t("settings-storage-loading"));
        }
        self.snapshot_result = Some(receiver);
    }

    /// Start `effect`; `reply` is told how it ends.
    pub(crate) fn run(&mut self, effect: &StorageEffect, reply: EffectReply) {
        if matches!(effect, StorageEffect::CleanUpCaches) {
            self.clean_up_caches(reply);
            return;
        }
        if matches!(effect, StorageEffect::FreeSyncedFiles) {
            self.begin_sync_eviction(reply);
            return;
        }
        let id = effect.id();
        self.replies.wait(id, reply);
        self.changed = true;
        match effect {
            StorageEffect::CleanUpCaches | StorageEffect::FreeSyncedFiles => {}
            StorageEffect::OpenDisks => {
                let result = os_actions::open_tool(ToolIntent::Disks {
                    route: DisksRoute::Overview,
                });
                self.replies
                    .finish(id, result.map(|()| t("settings-storage-opened-disks")));
            }
            StorageEffect::OpenBackups => {
                let result = os_actions::open_tool(ToolIntent::Backups {
                    route: BackupsRoute::Overview,
                });
                self.replies
                    .finish(id, result.map(|()| t("settings-storage-opened-backups")));
            }
        }
    }

    fn clean_up_caches(&mut self, reply: EffectReply) {
        if let Some(message) = self.initial_message() {
            reply.finish(Err(message));
            return;
        }
        if self.cleanup_job.is_some() {
            reply.finish(Err(t("settings-storage-cleaning")));
            return;
        }
        let Some(preview) = self.cleanup.clone() else {
            reply.finish(Err(t("settings-storage-no-cache")));
            return;
        };
        let source = Arc::clone(&self.cleanup_source);
        let roots = self.roots.clone();
        let (sender, result) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        match ui::background::Background::current().spawn("storage_cleanup", move || {
            if sender.send(source.clean(preview, roots)).is_ok() {
                wake.wake();
            }
        }) {
            Ok(()) => {
                self.cleanup_job = Some(CleanupJob { result, reply });
                self.cleanup_notice = Some(t("settings-storage-cleaning"));
            }
            Err(error) => {
                let message = t_args(
                    "settings-storage-clean-failed",
                    &[("error", error.to_string().into())],
                );
                self.cleanup_notice = Some(message.clone());
                reply.finish(Err(message));
            }
        }
        self.changed = true;
    }

    fn poll_cleanup(&mut self) {
        let outcome = self
            .cleanup_job
            .as_ref()
            .and_then(|job| match job.result.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(t("settings-storage-clean-worker-ended")))
                }
            });
        if let Some(outcome) = outcome {
            let Some(job) = self.cleanup_job.take() else {
                return;
            };
            let result = match outcome {
                Ok(read) => {
                    self.roots = read.roots;
                    self.snapshot = read.snapshot;
                    self.cleanup = read.cleanup;
                    self.cleanup_notice = Some(t("settings-storage-cleaned"));
                    Ok(ui::tilo_actions::ActionOutput {
                        value: None,
                        resources: Vec::new(),
                    })
                }
                Err(error) => {
                    let message =
                        t_args("settings-storage-clean-failed", &[("error", error.into())]);
                    self.cleanup_notice = Some(message.clone());
                    Err(message)
                }
            };
            job.reply.finish(result);
            self.changed = true;
        }
    }

    fn begin_sync_eviction(&mut self, reply: EffectReply) {
        if let Some(message) = self.initial_message() {
            reply.finish(Err(message));
            return;
        }
        if self.sync_busy {
            reply.finish(Err(t("settings-storage-reclaiming")));
            return;
        }
        self.sync_reply = Some(reply);
        self.changed = true;
        self.free_synced_files();
    }

    fn finish_sync_eviction(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.sync_reply.take() {
            reply.finish(result.map(|()| ui::tilo_actions::ActionOutput {
                value: None,
                resources: Vec::new(),
            }));
        }
    }

    fn free_synced_files(&mut self) {
        if !self.sync_busy
            && let Some(snapshot) = &self.sync_snapshot
        {
            let selected = reclaimable_selections(snapshot);
            let (sender, receiver) = std::sync::mpsc::channel();
            let socket = self.storage_socket.clone();
            std::thread::spawn(move || {
                let result = StorageClient::connect(socket)
                    .and_then(|mut client| client.evict(selected))
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
            self.sync_busy = true;
            self.sync_result = Some(receiver);
        } else {
            self.finish_sync_eviction(Err(t("settings-storage-no-reclaimable")));
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        crate::widgets::cards(vec![
            self.overview_card(),
            self.synced_card(),
            self.apps_card(),
            self.cleanup_card(),
            self.tools_card(),
        ])
    }

    fn overview_card(&self) -> El {
        if let Some(message) = self.initial_message() {
            return card(&t("settings-storage-use"), rows(vec![caption(&message)]));
        }
        let totals = self.snapshot.totals();
        card(
            &t("settings-storage-use"),
            rows(
                [
                    StorageCategory::InstalledApps,
                    StorageCategory::AppData,
                    StorageCategory::Cache,
                    StorageCategory::SyncedFiles,
                ]
                .into_iter()
                .map(|category| {
                    let value = if category == StorageCategory::SyncedFiles {
                        self.sync_snapshot.as_ref().map_or_else(
                            || t("settings-storage-unavailable"),
                            |snapshot| {
                                ui::format::bytes_si(
                                    snapshot.workspaces.iter().map(|w| w.logical_bytes).sum(),
                                )
                            },
                        )
                    } else if self.snapshot.unavailable.contains(&category) {
                        t("settings-storage-unavailable")
                    } else {
                        ui::format::bytes_si(*totals.get(&category).unwrap_or(&0))
                    };
                    value_row(&category_label(category), &value)
                })
                .collect(),
            ),
        )
    }

    fn synced_card(&self) -> El {
        if let Some(snapshot) = &self.sync_snapshot {
            let local: u64 = snapshot.workspaces.iter().map(|w| w.local_bytes).sum();
            let remote: u64 = snapshot
                .workspaces
                .iter()
                .map(|w| w.remote_only_bytes)
                .sum();
            let reclaimable: u64 = snapshot
                .workspaces
                .iter()
                .map(|w| w.reclaimable_bytes)
                .sum();
            let mut sync_rows = vec![
                value_row(
                    &t("settings-storage-on-device"),
                    &ui::format::bytes_si(local),
                ),
                value_row(
                    &t("settings-storage-remote-only"),
                    &ui::format::bytes_si(remote),
                ),
                value_row(
                    &t("settings-storage-safe-to-free"),
                    &ui::format::bytes_si(reclaimable),
                ),
            ];
            if let Some(message) = &self.sync_message {
                sync_rows.push(caption(message));
            }
            sync_rows.extend(
                snapshot
                    .files
                    .iter()
                    .filter(|f| f.reclaimable_bytes > 0)
                    .take(8)
                    .map(|f| {
                        value_row(&f.logical_path, &ui::format::bytes_si(f.reclaimable_bytes))
                    }),
            );
            if reclaimable > 0 && !self.sync_busy {
                let free = t_args(
                    "settings-storage-free",
                    &[("size", ui::format::bytes_si(reclaimable).into())],
                );
                card_with_action(
                    &t("settings-storage-synced-card"),
                    Box::new(self.cx.bind_invocation(
                        Button::new("storage.sync.free", &free),
                        StorageEffect::FreeSyncedFiles,
                    )),
                    rows(sync_rows),
                )
            } else {
                sync_rows.push(caption(&t(if self.sync_busy {
                    "settings-storage-reclaiming"
                } else if snapshot.online {
                    "settings-storage-nothing-reclaimable"
                } else {
                    "settings-storage-offline"
                })));
                card(&t("settings-storage-synced-card"), rows(sync_rows))
            }
        } else {
            let fallback = t("settings-storage-accounting-unavailable");
            card(
                &t("settings-storage-synced-card"),
                rows(vec![caption(
                    self.sync_message.as_deref().unwrap_or(&fallback),
                )]),
            )
        }
    }

    fn apps_card(&self) -> El {
        if let Some(message) = self.initial_message() {
            return card(
                &t("settings-storage-largest"),
                rows(vec![caption(&message)]),
            );
        }
        let mut largest = self.snapshot.entries.clone();
        largest.sort_by_key(|e| std::cmp::Reverse(e.bytes));
        let largest_rows = largest
            .into_iter()
            .take(8)
            .map(|e| {
                value_row(
                    &e.owner,
                    &t_args(
                        "settings-storage-item-meta",
                        &[
                            ("size", ui::format::bytes_si(e.bytes).into()),
                            ("category", category_label(e.category).into()),
                        ],
                    ),
                )
            })
            .collect();
        card(
            &t("settings-storage-largest"),
            if self.snapshot.entries.is_empty() {
                rows(vec![caption(&t("settings-storage-no-apps"))])
            } else {
                rows(largest_rows)
            },
        )
    }

    fn cleanup_card(&self) -> El {
        if let Some(message) = self.initial_message() {
            return card(
                &t("settings-storage-cleanup"),
                rows(vec![caption(&message)]),
            );
        }
        if self.cleanup_job.is_some() {
            return card(
                &t("settings-storage-cleanup"),
                rows(vec![caption(&t("settings-storage-cleaning"))]),
            );
        }
        match &self.cleanup {
            Some(preview) if !preview.items.is_empty() => {
                let remove = t_args(
                    "settings-storage-remove",
                    &[("size", ui::format::bytes_si(preview.total_bytes).into())],
                );
                let action = Box::new(self.cx.bind_invocation(
                    Button::new("storage.clean", &remove),
                    StorageEffect::CleanUpCaches,
                ));
                let mut preview_rows = vec![caption(&t("settings-storage-cleanup-caption"))];
                if let Some(message) = &self.cleanup_notice {
                    preview_rows.push(caption(message));
                }
                let cache_item = t("settings-storage-cache-item");
                preview_rows.extend(preview.items.iter().take(8).map(|i| {
                    value_row(
                        i.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(cache_item.as_str()),
                        &ui::format::bytes_si(i.bytes),
                    )
                }));
                card_with_action(
                    &t("settings-storage-cleanup-preview"),
                    action,
                    rows(preview_rows),
                )
            }
            _ => card(
                &t("settings-storage-cleanup"),
                rows(vec![caption(
                    self.cleanup_notice
                        .as_deref()
                        .unwrap_or(&t("settings-storage-cleanup-empty")),
                )]),
            ),
        }
    }

    fn tools_card(&self) -> El {
        let open_disks = t("settings-storage-open-disks");
        let open_backups = t("settings-storage-open-backups");
        card(
            &t("settings-storage-tools"),
            rows(vec![
                crate::widgets::setting_row_described(
                    &t("settings-storage-disks"),
                    &t("settings-storage-disks-desc"),
                    Box::new(self.cx.bind_invocation(
                        Button::new("storage.disks", &open_disks),
                        StorageEffect::OpenDisks,
                    )),
                ),
                crate::widgets::setting_row_described(
                    &t("settings-storage-backups"),
                    &t("settings-storage-backups-desc"),
                    Box::new(self.cx.bind_invocation(
                        Button::new("storage.backups", &open_backups),
                        StorageEffect::OpenBackups,
                    )),
                ),
                caption(&t("settings-storage-tools-caption")),
            ]),
        )
    }
}

#[cfg(test)]
#[path = "storage_sync_receipt_tests.rs"]
mod sync_receipt_tests;

#[cfg(test)]
#[path = "storage_job_tests.rs"]
mod job_tests;

#[cfg(test)]
#[path = "storage_startup_tests.rs"]
mod startup_tests;

impl PaneModel for StorageModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_initial();
        self.poll_cleanup();
        let waiting = (self.snapshot_result.is_some(), self.sync_result.is_some());
        if let Some(receiver) = &self.snapshot_result {
            match receiver.try_recv() {
                Ok(Ok(snapshot)) => {
                    self.sync_snapshot = Some(snapshot);
                    self.snapshot_result = None;
                    if self.sync_message.as_deref() == Some(t("settings-storage-loading").as_str())
                    {
                        self.sync_message = None;
                    }
                }
                Ok(Err(error)) => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some(t_args(
                        "settings-storage-service-unavailable",
                        &[("error", error.into())],
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some(t("settings-storage-service-ended"));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(receiver) = &self.sync_result {
            match receiver.try_recv() {
                Ok(Ok(report)) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    self.refresh_sync();
                    self.sync_message =
                        Some(if report.queued_files > 0 && report.evicted_files > 0 {
                            t_args(
                                "settings-storage-freed-queued",
                                &[
                                    ("size", ui::format::bytes_si(report.freed_bytes).into()),
                                    ("evicted", ArgValue::Number(report.evicted_files as f64)),
                                    ("queued", ArgValue::Number(report.queued_files as f64)),
                                ],
                            )
                        } else if report.queued_files > 0 {
                            t_args(
                                "settings-storage-queued-only",
                                &[("queued", ArgValue::Number(report.queued_files as f64))],
                            )
                        } else {
                            t_args(
                                "settings-storage-freed",
                                &[
                                    ("size", ui::format::bytes_si(report.freed_bytes).into()),
                                    ("evicted", ArgValue::Number(report.evicted_files as f64)),
                                ],
                            )
                        });
                    self.finish_sync_eviction(Ok(()));
                }
                Ok(Err(error)) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    let message = t_args(
                        "settings-storage-nothing-removed",
                        &[("error", error.into())],
                    );
                    self.sync_message = Some(message.clone());
                    self.finish_sync_eviction(Err(message));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    self.sync_message = Some(t("settings-storage-request-ended"));
                    self.finish_sync_eviction(Err(t("settings-storage-service-ended")));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let finished = (waiting.0 && self.snapshot_result.is_none())
            || (waiting.1 && self.sync_result.is_none());
        std::mem::take(&mut self.changed) || finished
    }

    fn name(&self) -> &'static str {
        "StoragePane"
    }

    fn busy(&self) -> bool {
        self.sync_busy
            || self.cleanup_job.is_some()
            || self.snapshot_result.is_some()
            || matches!(self.initial, InitialStorageRead::Pending(_))
    }
}

fn reclaimable_selections(snapshot: &SyncStorageSnapshot) -> Vec<EvictionSelection> {
    snapshot
        .files
        .iter()
        .filter(|file| file.reclaimable_bytes > 0)
        .map(EvictionSelection::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Only real roots are reported and only authority-approved files are reclaimed.
    #[test]
    fn storage_pane_reports_real_roots_and_reclaims_only_approved_files() {
        // Case `pane_reports_only_real_roots`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir_all(cache.join("so.tilo.browser")).unwrap();
            std::fs::write(cache.join("so.tilo.browser/http"), vec![0u8; 24]).unwrap();
            let profile = tempfile::tempdir().unwrap();
            let cx = PaneCx::in_file(profile.path().join("settings.json"));
            let pane = StorageModel::with_roots(
                cx,
                StorageRoots {
                    app_payloads: None,
                    room_data: None,
                    app_cache: Some(cache),
                    sync_replica: None,
                },
            );
            assert_eq!(pane.snapshot.reclaimable_bytes(), 24);
            assert_eq!(pane.cleanup.as_ref().unwrap().total_bytes, 24);
        }

        // Case `settings_submits_only_authority_approved_reclaimable_files`.
        {
            let file = |identity: &str, reclaimable_bytes| runtime_fs::storage::SyncedFileState {
                workspace: "work".into(),
                app: None,
                logical_path: format!("/{identity}"),
                identity: identity.into(),
                inode: 1,
                version: "v".into(),
                logical_bytes: 10,
                hydrated_bytes: 10,
                remote_only: false,
                dirty: reclaimable_bytes == 0,
                pinned: false,
                in_flight: false,
                remotely_durable: reclaimable_bytes > 0,
                reclaimable_bytes,
            };
            let snapshot = SyncStorageSnapshot {
                online: true,
                files: vec![file("clean", 10), file("dirty", 0)],
                ..SyncStorageSnapshot::default()
            };
            let selected = reclaimable_selections(&snapshot);
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].identity, "clean");
        }
    }
}
