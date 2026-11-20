//! The rows a Files window publishes for the entries it lists, each kept while
//! what it shows holds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ui::collaboration::NearbyTransferTarget;
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::RowBindings;

use super::open_with_recent::OpenWithFacts;
use super::{FilesState, MAX_EAGER_ENTRY_RESOURCES};
use crate::effects::FilesEffect;
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

/// `entry` as a row of this window: the type it is an item of, and the
/// effects the window offers on it now, each bound to what it acts on. An
/// app's row is a file's, with its way out of the person's apps in place of
/// the file work.
fn entry_row(context: &RowContext, facts: &EntryRowFacts, entry: &Entry) -> LiveRow {
    let resource_id = FilesState::entry_resource_id(entry);
    let bindings = entry_bindings(context, facts, entry);
    let label = entry.name.clone();
    let row = if entry.document_id.is_some() {
        LiveRow::typed_with::<crate::row_types::Document>(resource_id, label, &bindings)
    } else {
        LiveRow::typed_with::<crate::row_types::Entry>(resource_id, label, &bindings)
    };
    match entry_presentation(facts, entry) {
        Some(presentation) => row.presentation(presentation),
        None => row,
    }
}

/// What the window offers on `entry` now, each effect with the values it acts
/// on bound: an effect the entry cannot take is left out, not offered and
/// refused later.
fn entry_bindings(context: &RowContext, facts: &EntryRowFacts, entry: &Entry) -> RowBindings {
    let resource_id = FilesState::entry_resource_id(entry);
    let id = || serde_json::Value::String(resource_id.clone());
    let mut bindings = RowBindings::new()
        .bind(FilesEffect::READ, [("entry", id())])
        .bind(FilesEffect::OPEN, [("item", id())]);
    if facts.app_removal.is_some() {
        return bindings.bind(FilesEffect::UNINSTALL, [("app", id())]);
    }
    if context.recent {
        bindings = bindings
            .bind(FilesEffect::REVEAL_RECENT, [("entry", id())])
            .bind(FilesEffect::FORGET_RECENT, [("entry", id())]);
    } else if let Some(document_id) = entry.document_id {
        bindings = bindings.bind(
            FilesEffect::REVEAL,
            [(
                "record",
                serde_json::Value::String(uuid::Uuid::from_bytes(document_id).to_string()),
            )],
        );
    }
    if facts.converted.is_some() {
        bindings = bindings.bind(FilesEffect::IMPORT_CONVERTED, [("entry", id())]);
    }
    if facts.open_with.is_some() {
        bindings = bindings.bind(FilesEffect::OPEN_WITH, [("entry", id())]);
    }
    if context.trash {
        bindings = bindings
            .bind(FilesEffect::RESTORE, [("entry", id())])
            .bind(FilesEffect::PROPERTIES, [("entry", id())]);
        if !in_native_trash(&entry.path, context.native_trash.as_deref()) {
            bindings = bindings.bind(FilesEffect::DELETE_PERMANENTLY, [("entry", id())]);
        }
    } else {
        bindings = bindings
            .bind(FilesEffect::COPY, [("entry", id())])
            .bind(FilesEffect::CUT, [("entry", id())])
            .bind(FilesEffect::PROPERTIES, [("entry", id())])
            .bind(FilesEffect::HISTORY, [("entry", id())])
            .bind(FilesEffect::COPY_TO, [("entry", id())]);
        if importable_under(&entry.path, &context.import_mounts) {
            bindings = bindings.bind(FilesEffect::IMPORT_ONCE, [("entry", id())]);
        }
        if facts.airdrop_file {
            bindings = bindings.bind(FilesEffect::SEND_AIR_DROP, [("entry", id())]);
        }
        // Sending to a nearby device is one offer: its target chosen from the
        // devices online now, or, with none of them near, the picker that says so.
        bindings = if context.nearby.is_empty() {
            bindings.bind(FilesEffect::SEND_NEARBY, [("entry", id())])
        } else {
            bindings.bind(FilesEffect::SEND_TO, [("entry", id())])
        };
    }
    if let Some(availability) = entry.availability.as_ref() {
        bindings = bindings.bind(FilesEffect::AVAILABILITY, [("entry", id())]);
        if availability.policy == fs_protocol::AvailabilityPolicy::Automatic {
            bindings = bindings.bind(FilesEffect::KEEP_OFFLINE, [("entry", id())]);
        }
        if availability.policy == fs_protocol::AvailabilityPolicy::Pinned
            && !availability.policy_inherited
        {
            bindings = bindings.bind(FilesEffect::AUTOMATIC_STORAGE, [("entry", id())]);
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
            bindings = bindings.bind(FilesEffect::FREE_LOCAL_SPACE, [("entry", id())]);
        }
        let retryable = !matches!(
            availability.residency,
            fs_protocol::Residency::Resident { .. }
        ) && availability.transfer.phase == fs_protocol::TransferPhase::Idle;
        if retryable {
            bindings = bindings.bind(FilesEffect::RETRY_DOWNLOAD, [("entry", id())]);
        }
    }
    if entry.document_id.is_some() {
        bindings = bindings.bind(FilesEffect::SHARE, [("entry", id())]);
    }
    bindings
}

/// What the row's hand-off says of itself, and the badge a file whose bytes no
/// device delivered wears: a conversion that reads alone, or content that is
/// not here.
fn entry_presentation(facts: &EntryRowFacts, entry: &Entry) -> Option<ResourcePresentation> {
    if let Some(description) = facts.converted.clone() {
        return Some(ResourcePresentation {
            detail: Some(description.clone()),
            badge: Some(t("files-mounted-readonly")),
            preview: Some(format!(
                "{} {}",
                description,
                t("files-mounted-independent")
            )),
        });
    }
    entry
        .availability
        .as_ref()
        .is_some_and(fs_protocol::AvailabilityProjectionV1::unavailable)
        .then(|| ResourcePresentation {
            detail: Some(t("files-unavailable-offline")),
            badge: Some(t("files-unavailable")),
            preview: None,
        })
}

/// Sending `id` to a nearby device, as its row offers it: each device online now
/// a choice read by its name, or, with none of them near, the picker greyed
/// with why.
fn send_nearby_status(context: &RowContext, id: &str) -> EffectStatus {
    if context.nearby.is_empty() {
        return EffectStatus::unavailable(
            FilesEffect::SEND_NEARBY,
            "no-nearby-device",
            t("files-nearby-no-devices"),
        )
        .with_label(t("files-menu-send-nearby"))
        .on(id);
    }
    EffectStatus::labelled(FilesEffect::SEND_TO, t("files-menu-send-nearby"))
        .with_labelled_param_choices(
            "target",
            context
                .nearby
                .iter()
                .map(|target| (hex::encode(target.handle.as_bytes()), target.label.clone())),
        )
        .on(id)
}

/// What each effect `entry` offers says on its row now: the words this entry
/// is opened, read, kept or sent by, the apps that open it, the devices
/// nearby, and what a conversion or an absent byte stream means here.
pub(super) fn entry_row_status(
    context: &RowContext,
    facts: &EntryRowFacts,
    entry: &Entry,
) -> Vec<EffectStatus> {
    let resource_id = FilesState::entry_resource_id(entry);
    let id = resource_id.as_str();
    let mut statuses =
        vec![EffectStatus::labelled(FilesEffect::READ, t("files-action-read")).on(id)];
    if let Some(description) = facts.converted.clone() {
        statuses.push(
            EffectStatus::labelled(FilesEffect::OPEN, t("files-mounted-open"))
                .with_description(description)
                .on(id),
        );
        statuses.push(
            EffectStatus::labelled(FilesEffect::IMPORT_CONVERTED, t("files-mounted-copy")).on(id),
        );
    } else if entry
        .availability
        .as_ref()
        .is_some_and(fs_protocol::AvailabilityProjectionV1::unavailable)
    {
        statuses.push(
            EffectStatus::described(FilesEffect::OPEN, t("files-unavailable-offline")).on(id),
        );
    }
    if facts.app_removal == Some(runtime_portal::tools::AppRemoval::RemoveFromApps) {
        statuses.push(
            EffectStatus::labelled(FilesEffect::UNINSTALL, t("files-menu-remove-from-apps")).on(id),
        );
    }
    if let Some(open_with) = &facts.open_with {
        let mut status = EffectStatus::labelled(FilesEffect::OPEN_WITH, t("files-menu-open-with"))
            .with_labelled_param_choices(
                "app",
                open_with.candidates().iter().map(|app| {
                    let label = if app.is_default {
                        t_args("files-open-with-default", &[arg("app", app.name.clone())])
                    } else {
                        app.name.clone()
                    };
                    (app.id.clone(), label)
                }),
            )
            .on(id);
        if let Some(default) = open_with.default_app() {
            status = status.with_param_schema("app", serde_json::json!({ "default": default.id }));
        }
        statuses.push(status);
    }
    if !context.trash {
        statuses.push(send_nearby_status(context, id));
        if facts.airdrop_file {
            statuses.push(
                EffectStatus::labelled(FilesEffect::SEND_AIR_DROP, t("files-action-send-airdrop"))
                    .with_param_description("address", t("files-airdrop-address"))
                    .with_param_description("port", t("files-airdrop-port"))
                    .with_param_schema(
                        "address",
                        serde_json::json!({"minLength": 1, "maxLength": 64}),
                    )
                    .with_param_schema("port", serde_json::json!({"minimum": 1, "maximum": 65535}))
                    .on(id),
            );
        }
    }
    if entry.document_id.is_some() && !context.recent {
        statuses.push(EffectStatus::labelled(FilesEffect::REVEAL, t("files-reveal")).on(id));
    }
    if entry.document_id.is_some() && !context.trash {
        statuses.push(EffectStatus::labelled(FilesEffect::SHARE, t("files-share")).on(id));
    }
    statuses
        .push(EffectStatus::labelled(FilesEffect::PROPERTIES, t("files-menu-properties")).on(id));
    if entry.availability.is_some() {
        statuses.push(
            EffectStatus::labelled(
                FilesEffect::AVAILABILITY,
                t("files-availability-title-short"),
            )
            .on(id),
        );
    }
    statuses
}
