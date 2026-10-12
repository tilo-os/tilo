//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.
//! Reclaiming space is a [`StorageEffect`] the [`StorageModel`] runs.

use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use runtime_fs::storage::{
    DEFAULT_STORAGE_SOCKET, EvictionReport, EvictionSelection, StorageClient, SyncStorageSnapshot,
};
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
}

impl StorageArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model, created on first use: a capture's scans nothing and asks
    /// no storage service.
    pub fn model(&self) -> &Shared<StorageModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                StorageModel::new(self.cx.clone())
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
            let _ = model.borrow_mut().poll();
        }
    }
}

pub struct StorageModel {
    cx: PaneCx,
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
    sync_snapshot: Option<SyncStorageSnapshot>,
    sync_message: Option<String>,
    sync_busy: bool,
    sync_result: Option<std::sync::mpsc::Receiver<Result<EvictionReport, String>>>,
    snapshot_result: Option<std::sync::mpsc::Receiver<Result<SyncStorageSnapshot, String>>>,
    replies: Replies,
    changed: bool,
}

impl StorageModel {
    /// This device's storage, scanned now, with the synced-file accounting
    /// asked of the storage service.
    pub fn new(cx: PaneCx) -> Self {
        let roots = StorageRoots::discover();
        let mut model = Self::with_roots(cx, roots);
        model.refresh_sync();
        model
    }

    pub fn with_roots(cx: PaneCx, roots: StorageRoots) -> Self {
        let snapshot = desktop_policy::scan_storage(&roots);
        let cleanup = roots
            .app_cache
            .as_deref()
            .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok());
        Self {
            cx,
            roots,
            snapshot,
            cleanup,
            sync_snapshot: None,
            sync_message: None,
            sync_busy: false,
            sync_result: None,
            snapshot_result: None,
            replies: Replies::default(),
            changed: false,
        }
    }

    fn refresh_sync(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
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
        let id = effect.id();
        self.replies.wait(id, reply);
        self.changed = true;
        match effect {
            StorageEffect::CleanUpCaches => self.clean_up_caches(),
            StorageEffect::FreeSyncedFiles => self.free_synced_files(),
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

    fn clean_up_caches(&mut self) {
        if let Some(preview) = &self.cleanup {
            let selected: Vec<usize> = (0..preview.items.len()).collect();
            match desktop_policy::apply_cleanup(preview, &selected) {
                Ok(_) => {
                    self.snapshot = desktop_policy::scan_storage(&self.roots);
                    self.cleanup = self
                        .roots
                        .app_cache
                        .as_deref()
                        .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok());
                    self.replies.finish(
                        StorageEffect::CLEAN_UP_CACHES.id,
                        Ok(t("settings-storage-cleaned")),
                    );
                }
                Err(error) => self.replies.finish(
                    StorageEffect::CLEAN_UP_CACHES.id,
                    Err(t_args(
                        "settings-storage-clean-failed",
                        &[("error", error.to_string().into())],
                    )),
                ),
            }
        } else {
            self.replies.finish(
                StorageEffect::CLEAN_UP_CACHES.id,
                Err(t("settings-storage-no-cache")),
            );
        }
    }

    fn free_synced_files(&mut self) {
        if !self.sync_busy
            && let Some(snapshot) = &self.sync_snapshot
        {
            let selected = reclaimable_selections(snapshot);
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                    .and_then(|mut client| client.evict(selected))
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
            self.sync_busy = true;
            self.sync_result = Some(receiver);
        } else {
            self.replies.finish(
                StorageEffect::FREE_SYNCED_FILES.id,
                Err(t("settings-storage-no-reclaimable")),
            );
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
                rows(vec![caption(&t("settings-storage-cleanup-empty"))]),
            ),
        }
    }

    fn tools_card(&self) -> El {
        let open_disks = t("settings-storage-open-disks");
        let open_backups = t("settings-storage-open-backups");
        card(
            &t("settings-storage-tools"),
            rows(vec![
                self.cx
                    .row(t("settings-storage-disks"))
                    .described(t("settings-storage-disks-desc"))
                    .button("storage.disks", &open_disks, StorageEffect::OpenDisks),
                self.cx
                    .row(t("settings-storage-backups"))
                    .described(t("settings-storage-backups-desc"))
                    .button("storage.backups", &open_backups, StorageEffect::OpenBackups),
                caption(&t("settings-storage-tools-caption")),
            ]),
        )
    }
}

impl PaneModel for StorageModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
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
                    self.replies.finish(
                        StorageEffect::FREE_SYNCED_FILES.id,
                        Ok(t("settings-storage-reclaim-done")),
                    );
                }
                Ok(Err(error)) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    let message = t_args(
                        "settings-storage-nothing-removed",
                        &[("error", error.into())],
                    );
                    self.sync_message = Some(message.clone());
                    self.replies
                        .finish(StorageEffect::FREE_SYNCED_FILES.id, Err(message));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    self.sync_message = Some(t("settings-storage-request-ended"));
                    self.replies.finish(
                        StorageEffect::FREE_SYNCED_FILES.id,
                        Err(t("settings-storage-service-ended")),
                    );
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
        self.sync_busy || self.snapshot_result.is_some()
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
