//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.
//! Reclaiming space is a [`StorageEffect`] the [`StorageModel`] runs.
//!
//! What each device holds of the person's workspaces, and what of it is
//! only there, is [`residency`]: this device's storage summary is the
//! `storage-summary` effect, which another of the person's devices asks
//! for too, and the other devices' are theirs, asked of each through the
//! host ([`other_devices`]).

mod other_devices;
mod residency;

pub use residency::{DeviceResidency, Fleet};

use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use runtime_fs::storage::{
    CachePolicy, DEFAULT_STORAGE_SOCKET, EvictionReport, EvictionSelection, ResidencySummary,
    StorageClient, SyncStorageSnapshot,
};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected};
use ui::i18n::ArgValue;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Widget};

use crate::effects::effect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{SettingsSection, card, card_with_action, note, rows, value_row};

fn category_label(category: StorageCategory) -> String {
    t(match category {
        StorageCategory::InstalledApps => "settings-storage-cat-apps",
        StorageCategory::AppData => "settings-storage-cat-data",
        StorageCategory::Cache => "settings-storage-cat-cache",
        StorageCategory::SyncedFiles => "settings-storage-cat-synced",
    })
}

/// How much space fetched workspace files may keep on this device: the
/// daemon's default share of the disk, or a fixed size in gigabytes.
const CACHE_SIZES: &[&str] = &["Automatic", "5 GB", "10 GB", "25 GB", "50 GB", "100 GB"];

/// The bytes `size` (one of [`CACHE_SIZES`]) keeps; `Ok(None)` is the
/// daemon's default.
fn cache_budget(size: &str) -> Result<Option<u64>, String> {
    if size == "Automatic" {
        return Ok(None);
    }
    size.strip_suffix(" GB")
        .filter(|_| CACHE_SIZES.contains(&size))
        .and_then(|gigabytes| gigabytes.parse::<u64>().ok())
        .map(|gigabytes| Some(gigabytes.saturating_mul(1_000_000_000)))
        .ok_or_else(|| {
            t_args(
                "settings-storage-cache-size-unknown",
                &[("size", size.into())],
            )
        })
}

/// The entry of [`CACHE_SIZES`] for `budget`, if it is one of them.
fn cache_size(budget: Option<u64>) -> Option<&'static str> {
    CACHE_SIZES
        .iter()
        .copied()
        .find(|size| cache_budget(size).ok() == Some(budget))
}

fn cache_size_shown(size: &str) -> String {
    match size.strip_suffix(" GB") {
        Some(gigabytes) => t_args(
            "settings-storage-cache-size-gb",
            &[("size", gigabytes.into())],
        ),
        None => t("settings-storage-cache-size-auto"),
    }
}

fn cache_size_from_label(label: &str) -> String {
    CACHE_SIZES
        .iter()
        .find(|size| **size == label || cache_size_shown(size) == label)
        .map_or_else(|| label.to_string(), |size| (*size).to_string())
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
    /// Fetch the workspace files you are likely to open next before you
    /// open them, or only when opened.
    #[effect(job, idempotent)]
    SetPrefetch { on: bool },
    /// Set how much space fetched workspace files may use on this device.
    #[effect(job, idempotent)]
    SetCacheSize {
        #[param("The space to keep", choices = CACHE_SIZES.iter().copied())]
        size: String,
    },
    /// What this device holds of each workspace.
    ///
    /// Answers, per workspace, the bytes kept here by choice, also held by
    /// another device, held only here, and cache. Names no file.
    #[effect(job, returns, idempotent, kind = "read")]
    StorageSummary,
    /// Free a workspace's cache.
    ///
    /// Drops what this device fetched from another one; it downloads again
    /// when opened.
    #[effect(job, returns)]
    FreeCache {
        #[param("The workspace, as the storage summary names it")]
        workspace: String,
    },
    /// Keep a whole workspace on this device, or stop keeping it.
    #[effect(job, idempotent)]
    KeepHere {
        #[param("The workspace, as the storage summary names it")]
        workspace: String,
        keep: bool,
    },
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
    /// The window's client for the action door, which asks the person's
    /// other devices for their summaries.
    remote: ui::RemoteActions,
}

impl StorageArea {
    pub fn new(cx: &PaneCx, remote: ui::RemoteActions) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            remote,
        }
    }

    /// The model, created on first use: a capture's scans nothing and asks
    /// no storage service and no other device.
    pub fn model(&self) -> &Shared<StorageModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                let mut model = StorageModel::new(self.cx.clone());
                model.ask_other_devices(other_devices::OtherDevices::new(self.remote.clone()));
                model
            } else {
                let mut model = StorageModel::with_roots(
                    self.cx.clone(),
                    StorageRoots {
                        app_payloads: None,
                        room_data: None,
                        app_cache: None,
                        sync_replica: None,
                    },
                );
                // A capture shows its stand-in devices.
                for device in self
                    .cx
                    .scene()
                    .map(|scene| scene.storage_devices.clone())
                    .unwrap_or_default()
                {
                    model.fleet.adopt(device);
                }
                model
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
    snapshot_result: Option<std::sync::mpsc::Receiver<SyncRead>>,
    /// The daemon's cache policy, once asked; `None` while unknown.
    cache_policy: Option<CachePolicy>,
    /// The policy being read, or changed by the effect named.
    policy_result: Option<PolicyAnswer>,
    /// What each of the person's devices holds of their workspaces.
    fleet: Fleet,
    /// The person's other devices, asked for their summaries; `None` where
    /// no device is reached.
    others: Option<other_devices::OtherDevices>,
    /// The residency effect running against the storage service, and how
    /// it ends.
    residency_job: Option<(
        &'static str,
        std::sync::mpsc::Receiver<Result<String, String>>,
    )>,
    /// Asked for this device's summary, answered once the next read ends.
    summary_waiting: bool,
    replies: Replies,
    changed: bool,
}

/// One read of the storage service: the synced-file accounting, and this
/// device's storage summary from a service that answers it.
struct SyncRead {
    snapshot: Result<SyncStorageSnapshot, String>,
    residency: Option<DeviceResidency>,
}

impl SyncRead {
    fn now() -> Self {
        let mut client = match StorageClient::connect(DEFAULT_STORAGE_SOCKET) {
            Ok(client) => client,
            Err(error) => {
                return Self {
                    snapshot: Err(error.to_string()),
                    residency: None,
                };
            }
        };
        let snapshot = client.snapshot().map_err(|error| error.to_string());
        // A service that predates the summary closes the connection. This
        // device's card is titled "This device"; its key and name come from
        // the host's device list, which the other devices' summaries need.
        let residency = client.residency().ok().map(|summary| DeviceResidency {
            key: String::new(),
            label: String::new(),
            current: true,
            summary,
        });
        Self {
            snapshot,
            residency,
        }
    }
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
            cache_policy: None,
            policy_result: None,
            fleet: Fleet::default(),
            others: None,
            residency_job: None,
            summary_waiting: false,
            replies: Replies::default(),
            changed: false,
        }
    }

    /// Ask the person's other devices for their summaries through
    /// `others`, now and whenever the figures are read again.
    fn ask_other_devices(&mut self, mut others: other_devices::OtherDevices) {
        others.refresh();
        self.others = Some(others);
    }

    /// Take in the other devices' answers that arrived; whether any did.
    fn poll_other_devices(&mut self) -> bool {
        let Some(others) = self.others.as_mut() else {
            return false;
        };
        let arrived = others.poll();
        let changed = arrived.listed.is_some() || !arrived.summaries.is_empty();
        if let Some(listed) = &arrived.listed {
            self.fleet.keep_listed(listed);
        }
        for device in arrived.summaries {
            self.fleet.adopt(device);
        }
        changed
    }

    fn refresh_sync(&mut self) {
        if let Some(others) = self.others.as_mut() {
            others.refresh();
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(SyncRead::now());
        });
        if self.sync_snapshot.is_none() {
            self.sync_message = Some(t("settings-storage-loading"));
        }
        self.snapshot_result = Some(receiver);
        if self.policy_result.is_none() {
            self.change_policy(None, |_| Ok(()));
        }
    }

    /// Ask the daemon for its cache policy and, for the effect `effect`,
    /// replace it with what `change` makes of it.
    fn change_policy(
        &mut self,
        effect: Option<&'static str>,
        change: impl FnOnce(&mut CachePolicy) -> Result<(), String> + Send + 'static,
    ) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let read_only = effect.is_none();
        std::thread::spawn(move || {
            let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                .and_then(|mut client| {
                    let mut policy = client.cache_policy()?;
                    if read_only {
                        return Ok(Ok(policy));
                    }
                    if let Err(error) = change(&mut policy) {
                        return Ok(Err(error));
                    }
                    client.set_cache_policy(policy).map(Ok)
                })
                .map_err(|error| error.to_string())
                .and_then(|changed| changed);
            let _ = sender.send(result);
        });
        self.policy_result = Some(PolicyAnswer { effect, receiver });
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
            StorageEffect::SetPrefetch { on } => {
                let on = *on;
                self.change_policy(Some(id), move |policy| {
                    policy.prefetch = on;
                    Ok(())
                });
            }
            StorageEffect::SetCacheSize { size } => match cache_budget(size) {
                Ok(budget) => self.change_policy(Some(id), move |policy| {
                    policy.budget_bytes = budget;
                    Ok(())
                }),
                Err(error) => self.replies.finish(id, Err(error)),
            },
            StorageEffect::StorageSummary => self.summarize(),
            StorageEffect::FreeCache { workspace } => {
                let workspace = workspace.clone();
                self.run_residency_job(id, move |client| {
                    client.free_cache(&workspace).map(|freed| {
                        t_args(
                            "settings-storage-cache-freed",
                            &[
                                ("size", ui::format::bytes_si(freed.freed_bytes).into()),
                                ("files", ArgValue::Number(freed.files as f64)),
                            ],
                        )
                    })
                });
            }
            StorageEffect::KeepHere { workspace, keep } => {
                let (workspace, keep) = (workspace.clone(), *keep);
                self.run_residency_job(id, move |client| {
                    client.keep_here(&workspace, keep).map(|()| {
                        t(if keep {
                            "settings-storage-now-kept"
                        } else {
                            "settings-storage-no-longer-kept"
                        })
                    })
                });
            }
        }
    }

    /// Answer this device's storage summary: from the next read of the
    /// storage service, or, in a capture, its stand-in.
    fn summarize(&mut self) {
        if !self.cx.reaches_devices() {
            let summary = self
                .fleet
                .this_device()
                .map(|device| device.summary.clone())
                .unwrap_or_default();
            self.answer_summary(&summary);
            return;
        }
        self.summary_waiting = true;
        if self.snapshot_result.is_none() {
            self.refresh_sync();
        }
    }

    fn answer_summary(&mut self, summary: &ResidencySummary) {
        self.summary_waiting = false;
        match serde_json::to_value(summary) {
            Ok(value) => self
                .replies
                .answer(StorageEffect::STORAGE_SUMMARY.id, value),
            Err(error) => self
                .replies
                .fail(StorageEffect::STORAGE_SUMMARY.id, error.to_string()),
        }
    }

    /// Run `job` against the storage service off the UI thread, one at a
    /// time; the effect `id` is told how it ends, and the figures are read
    /// again.
    fn run_residency_job(
        &mut self,
        id: &'static str,
        job: impl FnOnce(&mut StorageClient) -> std::io::Result<String> + Send + 'static,
    ) {
        if self.residency_job.is_some() {
            self.replies.fail(id, t("settings-storage-residency-busy"));
            return;
        }
        if !self.cx.reaches_devices() {
            self.replies
                .fail(id, t("settings-storage-accounting-unavailable"));
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                .and_then(|mut client| job(&mut client))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.residency_job = Some((id, receiver));
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
        let mut sections = vec![self.overview_card()];
        sections.extend(residency::cards(
            &self.cx,
            &self.fleet,
            self.residency_job.is_some(),
        ));
        sections.extend([
            self.synced_card(),
            self.cache_card(),
            self.apps_card(),
            self.cleanup_card(),
            self.tools_card(),
        ]);
        crate::widgets::cards(sections)
    }

    fn overview_card(&self) -> SettingsSection {
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

    fn synced_card(&self) -> SettingsSection {
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
                sync_rows.push(note(message));
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
                sync_rows.push(note(&t(if self.sync_busy {
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
                rows(vec![note(
                    self.sync_message.as_deref().unwrap_or(&fallback),
                )]),
            )
        }
    }

    /// Prediction and the space fetched workspace files may use.
    fn cache_card(&self) -> SettingsSection {
        let prefetch_label = t("settings-storage-prefetch");
        let size_label = t("settings-storage-cache-size");
        let Some(policy) = self.cache_policy else {
            let unavailable = t("settings-storage-unavailable");
            return card(
                &t("settings-storage-cache-card"),
                rows(vec![
                    value_row(&prefetch_label, &unavailable),
                    value_row(&size_label, &unavailable),
                ]),
            );
        };
        let size_row = match cache_size(policy.budget_bytes) {
            Some(size) => self
                .cx
                .row(size_label)
                .id("storage.cache-size")
                .choice_effect(
                    CACHE_SIZES.iter().map(|size| cache_size_shown(size)),
                    &LocalState::new(cache_size_shown(size)),
                    effect(StorageEffect::SET_CACHE_SIZE)
                        .map(|shown: String| cache_size_from_label(&shown)),
                ),
            // A size set some other way is shown, not rounded to a choice.
            None => value_row(
                &size_label,
                &ui::format::bytes_si(policy.budget_bytes.unwrap_or_default()),
            ),
        };
        card(
            &t("settings-storage-cache-card"),
            rows(vec![
                self.cx
                    .row(prefetch_label)
                    .described(t("settings-storage-prefetch-desc"))
                    .id("storage.prefetch")
                    .toggle_effect(
                        &LocalState::new(policy.prefetch),
                        effect(StorageEffect::SET_PREFETCH),
                    ),
                size_row,
                note(&t("settings-storage-cache-caption")),
            ]),
        )
    }

    fn apps_card(&self) -> SettingsSection {
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
                rows(vec![note(&t("settings-storage-no-apps"))])
            } else {
                rows(largest_rows)
            },
        )
    }

    fn cleanup_card(&self) -> SettingsSection {
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
                let mut preview_rows = vec![note(&t("settings-storage-cleanup-caption"))];
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
                rows(vec![note(&t("settings-storage-cleanup-empty"))]),
            ),
        }
    }

    fn tools_card(&self) -> SettingsSection {
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
                note(&t("settings-storage-tools-caption")),
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
        let residency_finished = self.poll_residency_job();
        if let Some(receiver) = &self.snapshot_result {
            match receiver.try_recv() {
                Ok(read) => {
                    self.snapshot_result = None;
                    self.take_read(read);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some(t("settings-storage-service-ended"));
                    if self.summary_waiting {
                        self.summary_waiting = false;
                        self.replies.fail(
                            StorageEffect::STORAGE_SUMMARY.id,
                            t("settings-storage-service-ended"),
                        );
                    }
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
        let policy_changed = self.poll_policy();
        let others_answered = self.poll_other_devices();
        let finished = (waiting.0 && self.snapshot_result.is_none())
            || (waiting.1 && self.sync_result.is_none())
            || residency_finished
            || policy_changed
            || others_answered;
        std::mem::take(&mut self.changed) || finished
    }

    fn name(&self) -> &'static str {
        "StoragePane"
    }

    fn busy(&self) -> bool {
        self.sync_busy
            || self.snapshot_result.is_some()
            || self.policy_result.is_some()
            || self.residency_job.is_some()
    }
}

/// The daemon's answer about its cache policy, for the effect that asked.
struct PolicyAnswer {
    effect: Option<&'static str>,
    receiver: std::sync::mpsc::Receiver<Result<CachePolicy, String>>,
}

impl StorageModel {
    /// Take in the cache policy's answer, if it came; whether it did.
    fn poll_policy(&mut self) -> bool {
        let Some(answer) = &self.policy_result else {
            return false;
        };
        let result = match answer.receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(t("settings-storage-service-ended"))
            }
        };
        let effect = answer.effect;
        self.policy_result = None;
        match result {
            Ok(policy) => {
                self.cache_policy = Some(policy);
                if let Some(effect) = effect {
                    self.replies
                        .finish(effect, Ok(t("settings-storage-cache-saved")));
                }
            }
            Err(error) => {
                if let Some(effect) = effect {
                    self.replies.finish(
                        effect,
                        Err(t_args(
                            "settings-storage-cache-failed",
                            &[("error", error.into())],
                        )),
                    );
                }
            }
        }
        true
    }

    /// Take in how the running residency effect ended, if it did; whether
    /// it did. Its figures are read again.
    fn poll_residency_job(&mut self) -> bool {
        let Some((id, receiver)) = &self.residency_job else {
            return false;
        };
        let id = *id;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(t("settings-storage-service-ended"))
            }
        };
        self.residency_job = None;
        self.replies.finish(id, result);
        self.refresh_sync();
        true
    }

    /// Take in a finished read of the storage service.
    fn take_read(&mut self, read: SyncRead) {
        match read.snapshot {
            Ok(snapshot) => {
                self.sync_snapshot = Some(snapshot);
                if self.sync_message.as_deref() == Some(t("settings-storage-loading").as_str()) {
                    self.sync_message = None;
                }
            }
            Err(error) => {
                self.sync_snapshot = None;
                self.sync_message = Some(t_args(
                    "settings-storage-service-unavailable",
                    &[("error", error.into())],
                ));
            }
        }
        match read.residency {
            Some(device) => {
                let summary = device.summary.clone();
                self.fleet.adopt(device);
                if self.summary_waiting {
                    self.answer_summary(&summary);
                }
            }
            None if self.summary_waiting => {
                self.summary_waiting = false;
                self.replies.fail(
                    StorageEffect::STORAGE_SUMMARY.id,
                    t("settings-storage-accounting-unavailable"),
                );
            }
            None => {}
        }
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

    /// Each device's card names its workspaces and what of them no other
    /// device holds; only this device's rows offer to free its cache and
    /// keep a workspace here.
    #[test]
    fn the_pane_shows_what_only_one_device_holds() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = StorageModel::with_roots(
            cx,
            StorageRoots {
                app_payloads: None,
                room_data: None,
                app_cache: None,
                sync_replica: None,
            },
        );
        for device in crate::scenarios::storage_fixture() {
            pane.fleet.adopt(device);
        }
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        let says = |needle: &str| text.iter().any(|line| line.contains(needle));
        let at_risk = t_args(
            "settings-storage-only-here-mark",
            &[("size", ui::format::bytes_si(48_000_000).into())],
        );
        assert!(
            says(&t_args(
                "settings-storage-this-device",
                &[("device", "Laptop".into())]
            )),
            "{text:#?}"
        );
        assert!(says("Home server") && says("Desktop"), "{text:#?}");
        assert!(says(&at_risk), "{text:#?}");
        assert_eq!(
            text.iter().filter(|line| line.contains(&at_risk)).count(),
            1,
            "only the laptop holds work no other device has: {text:#?}"
        );
        let free = t_args(
            "settings-storage-free-cache",
            &[("size", ui::format::bytes_si(1_260_000_000).into())],
        );
        assert!(says(&free), "{text:#?}");
        assert_eq!(
            text.iter()
                .filter(|line| line.contains(&t("settings-storage-keep-here"))
                    || line.contains(&t("settings-storage-stop-keeping")))
                .count(),
            2,
            "one keep control per workspace of this device, none for the others: {text:#?}"
        );
    }

    /// The summary another device asks for carries counts per workspace and
    /// no file name.
    #[test]
    fn the_storage_summary_names_no_file() {
        let summary = &crate::scenarios::storage_fixture()[0].summary;
        let value = serde_json::to_value(summary).unwrap();
        let workspaces = value["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 2);
        for workspace in workspaces {
            let mut keys: Vec<_> = workspace.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["kept_here", "split", "workspace"]);
            assert!(
                workspace["split"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(serde_json::Value::is_u64),
                "only counts: {workspace}"
            );
        }
    }
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

    /// Every cache size an agent may choose maps to a budget the daemon
    /// accepts and back, shown or by id; anything else is refused.
    #[test]
    fn cache_sizes_round_trip_and_stay_above_the_daemons_minimum() {
        assert_eq!(cache_budget("Automatic"), Ok(None));
        assert_eq!(cache_budget("25 GB"), Ok(Some(25_000_000_000)));
        for size in CACHE_SIZES {
            let budget = cache_budget(size).unwrap();
            assert!(
                budget.is_none_or(|bytes| bytes >= runtime_fs::storage::MIN_CACHE_BUDGET_BYTES)
            );
            assert_eq!(cache_size(budget), Some(*size));
            assert_eq!(cache_size_from_label(&cache_size_shown(size)), *size);
            assert_eq!(cache_size_from_label(size), *size);
        }
        assert!(cache_budget("7 GB").is_err());
        assert!(cache_budget("lots").is_err());
        assert_eq!(cache_size(Some(123)), None);
    }
}
