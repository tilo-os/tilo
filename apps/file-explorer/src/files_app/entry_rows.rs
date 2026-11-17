//! The rows a Files window publishes for the entries it lists, each kept while
//! what it shows holds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ui::collaboration::NearbyTransferTarget;
use ui::framework::LiveRow;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{AbsolutePath, Effect as _};

use super::open_with_recent::{OpenWithFacts, open_with_actions};
use super::{FilesState, MAX_EAGER_ENTRY_RESOURCES, hex_prefix};
use crate::i18n::{arg, t, t_args};
use crate::model::Entry;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowContext {
    recent: bool,
    trash: bool,
    import_mounts: Vec<PathBuf>,
    native_trash: Option<PathBuf>,
    airdrop: bool,
    nearby: Vec<NearbyTransferTarget>,
    locale: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EntryRowFacts {
    converted: Option<String>,
    open_with: Option<OpenWithFacts>,
    airdrop_file: bool,
    app_removal: Option<runtime_portal::tools::AppRemoval>,
}

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

pub(super) fn in_native_trash(path: &Path, trash: Option<&Path>) -> bool {
    trash.is_some_and(|root| path.parent() == Some(root))
}

impl FilesState {
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

    pub(super) fn entry_row_facts(&self, context: &RowContext, entry: &Entry) -> EntryRowFacts {
        if crate::applications::is_app(&entry.path) {
            return EntryRowFacts {
                converted: None,
                open_with: None,
                airdrop_file: false,
                app_removal: Some(self.app_removal(&entry.path)),
            };
        }
        EntryRowFacts {
            converted: self.converted_description(&entry.path),
            open_with: self.open_with_facts(entry),
            airdrop_file: context.airdrop && entry.path.is_file(),
            app_removal: None,
        }
    }

    pub(super) fn entry_resource(&self, entry: &Entry) -> LiveRow {
        let context = self.row_context();
        let facts = self.entry_row_facts(&context, entry);
        entry_row(&context, &facts, entry)
    }

    pub(super) fn live_entry_rows(&self) -> Vec<LiveRow> {
        let context = self.row_context();
        let mut kept = self.entry_rows.borrow_mut();
        let mut last = std::mem::take(&mut kept.rows);
        if kept.context.as_ref() != Some(&context) {
            last.clear();
        }
        let mut rows = Vec::with_capacity(self.entries.len().min(MAX_EAGER_ENTRY_RESOURCES));
        for entry in self.entries.iter().take(MAX_EAGER_ENTRY_RESOURCES) {
            let key = (entry.path.clone(), entry.document_id);
            let facts = self.entry_row_facts(&context, entry);
            let row = match last.remove(&key) {
                Some(row) if row.entry == *entry && row.facts == facts => row,
                _ => KeptRow {
                    row: entry_row(&context, &facts, entry),
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

fn app_row(row: LiveRow, resource_id: &str, removal: runtime_portal::tools::AppRemoval) -> LiveRow {
    use crate::effects::{FilesEffect, row_action};
    use runtime_portal::tools::AppRemoval;
    let row = row.operation(row_action(
        &FilesEffect::OpenItem {
            item: resource_id.to_owned(),
        },
        "open",
        t("files-menu-open"),
    ));
    let (id, label) = match removal {
        AppRemoval::UninstallWithData | AppRemoval::Uninstall => {
            ("uninstall", t("files-menu-uninstall"))
        }
        AppRemoval::RemoveFromApps => ("remove-from-apps", t("files-menu-remove-from-apps")),
        AppRemoval::None => return row,
    };
    row.operation(row_action(
        &FilesEffect::UninstallApp {
            app: resource_id.to_owned(),
        },
        id,
        label,
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "an entry's complete canonical action contract stays together so lazy and eager resource resolution cannot diverge by dispatch route"
)]
fn entry_row(context: &RowContext, facts: &EntryRowFacts, entry: &Entry) -> LiveRow {
    use crate::effects::{FilesEffect, row_action};
    let resource_id = FilesState::entry_resource_id(entry);
    let id = || resource_id.clone();
    let document_id = entry.document_id.map(uuid::Uuid::from_bytes);
    let mut resource = LiveRow::new(
        resource_id.clone(),
        if document_id.is_some() {
            "files.record"
        } else {
            "files.entry"
        },
        entry.name.clone(),
    );
    resource = resource.operation(
        row_action(
            &FilesEffect::ReadEntry { entry: id() },
            "read",
            t("files-action-read"),
        )
        .reads_document(),
    );
    if let Some(removal) = facts.app_removal {
        return app_row(resource, &resource_id, removal);
    }
    if context.recent {
        resource = resource
            .operation(row_action(
                &FilesEffect::RevealRecent { entry: id() },
                crate::search_provider::REVEAL_ACTION,
                t("files-recent-reveal"),
            ))
            .operation(row_action(
                &FilesEffect::ForgetRecent { entry: id() },
                "forget-recent",
                t("files-recent-forget"),
            ));
    } else if let Some(document_id) = document_id {
        resource = resource.operation(row_action(
            &FilesEffect::Reveal {
                record: document_id.to_string(),
            },
            crate::search_provider::REVEAL_ACTION,
            t("files-action-reveal"),
        ));
    }
    let mut open = row_action(
        &FilesEffect::OpenItem { item: id() },
        "open",
        if facts.converted.is_some() {
            t("files-mounted-open")
        } else {
            t("files-menu-open")
        },
    );
    // A file whose bytes no device delivered says so where an agent and a
    // screen reader look, before anyone opens it and waits for nothing.
    let presentation = if let Some(description) = facts.converted.clone() {
        open.type_mut().description = Some(description.clone());
        Some(ResourcePresentation {
            detail: Some(description.clone()),
            badge: Some(t("files-mounted-readonly")),
            preview: Some(format!(
                "{} {}",
                description,
                t("files-mounted-independent")
            )),
        })
    } else if entry
        .availability
        .as_ref()
        .is_some_and(fs_protocol::AvailabilityProjectionV1::unavailable)
    {
        let detail = t("files-unavailable-offline");
        open.type_mut().description = Some(detail.clone());
        Some(ResourcePresentation {
            detail: Some(detail),
            badge: Some(t("files-unavailable")),
            preview: None,
        })
    } else {
        None
    };
    resource = resource.operation(open);
    if let Some(presentation) = presentation {
        resource = resource.presentation(presentation);
    }
    if facts.converted.is_some() {
        resource = resource.operation(row_action(
            &FilesEffect::ImportConverted { entry: id() },
            "import-converted",
            t("files-mounted-copy"),
        ));
    }
    if let Some(open_with) = &facts.open_with {
        for action in open_with_actions(open_with, &resource_id) {
            resource = resource.operation(action);
        }
    }
    if !context.trash {
        if let Ok(here) = AbsolutePath::new(entry.path.parent().unwrap_or(entry.path.as_path())) {
            let mut copy_to = FilesEffect::CopyTo {
                entry: id(),
                folder: here.clone(),
            }
            .bound_asking(&["folder"]);
            copy_to.type_mut().label = t("files-action-copy-to-folder");
            resource = resource.operation(copy_to);
            if importable_under(&entry.path, &context.import_mounts) {
                let mut import = FilesEffect::ImportOnce {
                    entry: id(),
                    folder: here,
                }
                .bound_asking(&["folder"]);
                import.type_mut().label = t("files-import-once");
                resource = resource.operation(import);
            }
        }
        resource = resource.operation(row_action(
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
            resource = resource.operation(action);
        }
        for target in &context.nearby {
            resource = resource.operation(row_action(
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
        resource = resource.operation(row_action(&effect, action, label));
    }
    if let Some(availability) = entry.availability.as_ref() {
        resource = resource.operation(row_action(
            &FilesEffect::Availability { entry: id() },
            "availability",
            t("files-availability-title-short"),
        ));
        if availability.policy == fs_protocol::AvailabilityPolicy::Automatic {
            resource = resource.operation(row_action(
                &FilesEffect::KeepOffline { entry: id() },
                "keep-offline",
                t("files-keep-offline"),
            ));
        }
        if availability.policy == fs_protocol::AvailabilityPolicy::Pinned
            && !availability.policy_inherited
        {
            resource = resource.operation(row_action(
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
            resource = resource.operation(row_action(
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
            resource = resource.operation(row_action(
                &FilesEffect::RetryDownload { entry: id() },
                "retry-download",
                t("files-retry-download"),
            ));
        }
    }
    if entry.document_id.is_some() {
        resource = resource.operation(row_action(
            &FilesEffect::Share { entry: id() },
            "share",
            t("files-share"),
        ));
    }
    resource
}
