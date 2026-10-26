//! The rows a Files window publishes for the entries it lists, each kept
//! while what it shows holds.
//!
//! A row is a pure function of its entry, the facts about that entry the
//! window holds (a converted file's description, the apps that open it)
//! and what every row shares (the place shown, the devices nearby, the
//! locale): [`entry_row`] reads nothing else, so a row whose inputs are the
//! ones it was made from is that very row again. A publication made every
//! row afresh, each with a dozen actions, on every click.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ui::collaboration::NearbyTransferTarget;
use ui::framework::LiveRow;
use ui::tilo_actions::Resource;
use ui::tilo_resource::Effect as _;

use super::open_with_recent::{OpenWithFacts, append_open_with_actions};
use super::{FileExplorerWidget, MAX_EAGER_ENTRY_RESOURCES, hex_prefix};
use crate::i18n::{arg, t, t_args};
use crate::model::Entry;

/// What every entry row of the listing shows besides its entry.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowContext {
    /// The place shown is Recent.
    recent: bool,
    /// The place shown is a Trash.
    trash: bool,
    /// Where the accounts a file can be imported from are mounted.
    import_mounts: Vec<PathBuf>,
    /// The host's Trash, when its native Trash is in use.
    native_trash: Option<PathBuf>,
    /// Whether `AirDrop` can send.
    airdrop: bool,
    /// The devices nearby that are online.
    nearby: Vec<NearbyTransferTarget>,
    /// The copy's [`ui::i18n::I18nManager::revision`].
    locale: u64,
}

/// What one entry's row shows that the window holds about the entry.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct EntryRowFacts {
    /// A converted cloud file's description.
    converted: Option<String>,
    /// What its open-with actions are made from.
    open_with: Option<OpenWithFacts>,
    /// `AirDrop` can send it: a file, with `AirDrop` available.
    airdrop_file: bool,
}

/// The entry rows last published, by entry, under the context they share.
#[derive(Default)]
pub(super) struct EntryRows {
    context: Option<RowContext>,
    rows: HashMap<(PathBuf, Option<[u8; 16]>), KeptRow>,
}

struct KeptRow {
    entry: Entry,
    facts: EntryRowFacts,
    row: LiveRow,
}

/// Whether a file at `path` can be imported once from an account mounted
/// at one of `mounts`. Legacy native-document placeholders hold a browser
/// link, not editable bytes: copying one cannot fulfil an import.
pub(super) fn importable_under(path: &Path, mounts: &[PathBuf]) -> bool {
    let placeholder = path.extension().is_some_and(|extension| {
        [
            "gdoc", "gsheet", "gslides", "gdraw", "gform", "gsite", "url",
        ]
        .iter()
        .any(|suffix| extension.eq_ignore_ascii_case(suffix))
    });
    !placeholder && mounts.iter().any(|mount| path.starts_with(mount))
}

/// Whether `path` is an entry of the host's Trash `trash`.
pub(super) fn in_native_trash(path: &Path, trash: Option<&Path>) -> bool {
    trash.is_some_and(|root| path.parent() == Some(root))
}

impl FileExplorerWidget {
    /// What every row of the listing shares, now.
    pub(super) fn row_context(&self) -> RowContext {
        RowContext {
            recent: self.is_recent_view(),
            trash: self.is_trash_view(),
            import_mounts: self.import_mounts(),
            native_trash: self.native_trash_dir(),
            airdrop: self.nearby_send.airdrop_available(),
            nearby: self
                .nearby_send
                .targets()
                .iter()
                .filter(|target| target.online)
                .cloned()
                .collect(),
            locale: ui::i18n::I18nManager::revision(),
        }
    }

    /// What `entry`'s row shows that the window holds about it, now.
    pub(super) fn entry_row_facts(&self, context: &RowContext, entry: &Entry) -> EntryRowFacts {
        EntryRowFacts {
            converted: self.converted_description(&entry.path),
            open_with: self.open_with_facts(entry),
            airdrop_file: context.airdrop && entry.path.is_file(),
        }
    }

    /// The complete action contract of one listed entry's row, as an exact
    /// search, history or agent address resolves it.
    pub(super) fn entry_resource(&self, entry: &Entry) -> Resource {
        let context = self.row_context();
        let facts = self.entry_row_facts(&context, entry);
        entry_row(&context, &facts, entry)
    }

    /// The rows of the first entries of the listing, those kept whose
    /// entry, facts and shared context are the ones they were made from.
    pub(super) fn live_entry_rows(&self) -> Vec<LiveRow> {
        let context = self.row_context();
        let mut kept = self.entry_rows.borrow_mut();
        let mut last = std::mem::take(&mut kept.rows);
        if kept.context.as_ref() != Some(&context) {
            last.clear();
        }
        let mut rows = Vec::with_capacity(self.entries.len().min(MAX_EAGER_ENTRY_RESOURCES));
        for entry in self.entries.iter().take(MAX_EAGER_ENTRY_RESOURCES) {
            let key = (entry.path.clone(), entry.record_id);
            let facts = self.entry_row_facts(&context, entry);
            let row = match last.remove(&key) {
                Some(row) if row.entry == *entry && row.facts == facts => row,
                _ => KeptRow {
                    row: LiveRow::from_resource(entry_row(&context, &facts, entry)),
                    entry: entry.clone(),
                    facts,
                },
            };
            rows.push(row.row.clone());
            kept.rows.insert(key, row);
        }
        kept.context = Some(context);
        rows
    }
}

/// Construct the complete action contract for one already-projected row.
/// This is intentionally separate from `resources`: a folder can have far
/// more rows than an action directory should enumerate, while an exact
/// search/history/agent address must still resolve to the same canonical
/// actions as a visible context menu.
#[allow(
    clippy::too_many_lines,
    reason = "an entry's complete canonical action contract stays together so lazy and eager resource resolution cannot diverge by dispatch route"
)]
fn entry_row(context: &RowContext, facts: &EntryRowFacts, entry: &Entry) -> Resource {
    use crate::effects::{FilesEffect, row_action};
    let resource_id = FileExplorerWidget::entry_resource_id(entry);
    let id = || resource_id.clone();
    let record_id = entry.record_id.map(uuid::Uuid::from_bytes);
    let mut resource = Resource::new(
        resource_id.clone(),
        if record_id.is_some() {
            "files.record"
        } else {
            "files.entry"
        },
        entry.name.clone(),
    );
    if context.recent {
        resource.actions.push(row_action(
            &FilesEffect::RevealRecent { entry: id() },
            crate::search_provider::REVEAL_ACTION,
            t("files-recent-reveal"),
        ));
        resource.actions.push(row_action(
            &FilesEffect::ForgetRecent { entry: id() },
            "forget-recent",
            t("files-recent-forget"),
        ));
    } else if let Some(record_id) = record_id {
        resource.actions.push(row_action(
            &FilesEffect::Reveal {
                record: record_id.to_string(),
            },
            crate::search_provider::REVEAL_ACTION,
            t("files-action-reveal"),
        ));
    }
    resource.actions.push(row_action(
        &FilesEffect::OpenItem { item: id() },
        "open",
        if facts.converted.is_some() {
            t("files-mounted-open")
        } else {
            t("files-menu-open")
        },
    ));
    if let Some(description) = facts.converted.clone() {
        resource.presentation = Some(ui::tilo_actions::ResourcePresentation {
            detail: Some(description.clone()),
            badge: Some(t("files-mounted-readonly")),
            preview: Some(format!(
                "{} {}",
                description,
                t("files-mounted-independent")
            )),
        });
        if let Some(open) = resource.actions.last_mut() {
            open.type_mut().description = Some(description);
        }
        resource.actions.push(row_action(
            &FilesEffect::ImportConverted { entry: id() },
            "import-converted",
            t("files-mounted-copy"),
        ));
    }
    if let Some(open_with) = &facts.open_with {
        append_open_with_actions(open_with, &resource_id, &mut resource);
    }
    if !context.trash {
        let mut copy_to = FilesEffect::CopyTo {
            entry: id(),
            folder: String::new(),
        }
        .bound_asking(&["folder"]);
        copy_to.type_mut().label = "Copy to folder…".into();
        resource.actions.push(copy_to);
        if importable_under(&entry.path, &context.import_mounts) {
            let mut import = FilesEffect::ImportOnce {
                entry: id(),
                folder: String::new(),
            }
            .bound_asking(&["folder"]);
            import.type_mut().label = t("files-import-once");
            resource.actions.push(import);
        }
        resource.actions.push(row_action(
            &FilesEffect::SendNearby { entry: id() },
            "send-nearby",
            t("files-menu-send-nearby"),
        ));
        if facts.airdrop_file {
            let effect = FilesEffect::SendAirDrop {
                entry: id(),
                address: String::new(),
                port: 0,
            };
            let mut params = effect.bound_asking(&["address", "port"]).params().to_vec();
            for param in &mut params {
                let key = if &*param.name == "address" {
                    "files-airdrop-address"
                } else {
                    "files-airdrop-port"
                };
                param.description = t(key).into();
                param.schema = Some(
                    (if &*param.name == "port" {
                        serde_json::json!({"minimum": 1, "maximum": 65535})
                    } else {
                        serde_json::json!({"minLength": 1, "maxLength": 64})
                    })
                    .into(),
                );
            }
            let mut action = effect.bound_asking_params(params);
            action.type_mut().description = Some(t("files-airdrop-action-detail"));
            action.type_mut().id = "send-airdrop".into();
            action.type_mut().label = t("files-action-send-airdrop");
            resource.actions.push(action);
        }
        for target in &context.nearby {
            resource.actions.push(row_action(
                &FilesEffect::SendTo {
                    entry: id(),
                    target: target.handle,
                },
                format!("send-nearby-{}", hex_prefix(target.handle.as_bytes(), 6)),
                t_args(
                    "files-action-send-nearby-device",
                    &[arg("device", target.label.clone())],
                ),
            ));
        }
    }
    let mut entry_effects: Vec<(FilesEffect, &str, String)> = if context.trash {
        vec![
            (
                FilesEffect::Restore { entry: id() },
                "restore",
                t("files-menu-restore"),
            ),
            (
                FilesEffect::DeletePermanently { entry: id() },
                "delete-permanently",
                t("files-menu-delete-permanently"),
            ),
            (
                FilesEffect::Properties { entry: id() },
                "properties",
                t("files-menu-properties"),
            ),
        ]
    } else {
        vec![
            (
                FilesEffect::Copy { entry: id() },
                "copy",
                t("files-menu-copy"),
            ),
            (FilesEffect::Cut { entry: id() }, "cut", t("files-menu-cut")),
            (
                FilesEffect::Properties { entry: id() },
                "properties",
                t("files-menu-properties"),
            ),
            (
                FilesEffect::Sharing { entry: id() },
                "sharing",
                t("files-sharing-access"),
            ),
            (
                FilesEffect::History { entry: id() },
                "history",
                t("files-version-history"),
            ),
        ]
    };
    if in_native_trash(&entry.path, context.native_trash.as_deref()) {
        entry_effects
            .retain(|(effect, _, _)| !matches!(effect, FilesEffect::DeletePermanently { .. }));
    }
    for (effect, action, label) in entry_effects {
        resource.actions.push(row_action(&effect, action, label));
    }
    if let Some(availability) = entry.availability.as_ref() {
        resource.actions.push(row_action(
            &FilesEffect::Availability { entry: id() },
            "availability",
            t("files-availability-title-short"),
        ));
        if availability.policy == fs_protocol::AvailabilityPolicy::Automatic {
            resource.actions.push(row_action(
                &FilesEffect::KeepOffline { entry: id() },
                "keep-offline",
                t("files-keep-offline"),
            ));
        }
        if availability.policy == fs_protocol::AvailabilityPolicy::Pinned
            && !availability.policy_inherited
        {
            resource.actions.push(row_action(
                &FilesEffect::AutomaticStorage { entry: id() },
                "automatic-storage",
                t("files-use-automatic-storage"),
            ));
        }
        let releasable = !entry.is_dir
            && availability.policy == fs_protocol::AvailabilityPolicy::Automatic
            && !availability.policy_inherited
            && availability.transfer.phase == fs_protocol::TransferPhase::Idle
            && matches!(
                availability.residency,
                fs_protocol::Residency::Partial { .. } | fs_protocol::Residency::Resident { .. }
            );
        if releasable {
            resource.actions.push(row_action(
                &FilesEffect::FreeLocalSpace { entry: id() },
                "free-local-space",
                t("files-free-local-space"),
            ));
        }
        let retryable = !matches!(
            availability.residency,
            fs_protocol::Residency::Resident { .. }
        ) && availability.transfer.phase == fs_protocol::TransferPhase::Idle;
        if retryable {
            resource.actions.push(row_action(
                &FilesEffect::RetryDownload { entry: id() },
                "retry-download",
                t("files-retry-download"),
            ));
        }
    }
    if entry.record_id.is_some() {
        resource.actions.push(row_action(
            &FilesEffect::ShareLink { entry: id() },
            "share-link",
            "Create one-time share link",
        ));
    }
    resource
}
