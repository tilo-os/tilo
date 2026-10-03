//! `StoreApp` — the root widget.
//!
//! The Store has four pages, each one column on the window ground, composed
//! in [`view`] from the toolkit's content widgets:
//!
//! ```text
//! catalogue         6 apps from 2 sources            [search] [remotes] [trust] [refresh]
//!                   Updates (1)        ┌ card ┐
//!                   On this device (2) ┌ card ┐ ┌ card ┐
//!                   Discover (3)       ┌ card ┐ ┌ card ┐ ┌ card ┐
//! details           ← Back to apps
//!                   [icon] Name, its badges, its actions and description,
//!                   then Details (one property list) and Trust
//! flatpak remotes   the add form and the configured remotes
//! trust             report drafts and how decisions are made
//! ```
//!
//! People find apps through the platform search (the magnifier, Ctrl+K): the
//! Store only provides it, with source, category and status as facets (see
//! [`crate::search_provider`]). The catalogue itself has no search field.
//! An operation's progress and failures read in a banner in the page; its
//! result reads once in the transient status bubble.
//!
//! The widget is intentionally read-mostly: the source-of-truth for the item
//! list is refreshed by an outside "refresh" call ([`StoreApp::set_items`])
//! rather than on every frame, so a capture or a test can inject a
//! deterministic catalogue without spinning up an HTTP server.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::tilo_actions::{
    ActionAddress, ActionArgs, ActionBinding, ActionContract, ActionParam, ActionSpec,
    Availability, ConfirmationPolicy, ControlInvocation, EncodedAction, ExecutionLocus,
    ExecutionMode, OperationKind, ParamType, Resource, ResourcePresentation, ResultSpec,
    RetryPolicy, RiskLevel,
};
use ui::widget::Widget;
use ui::{Theme, permissions::PermissionContext, presence::PresenceCtx, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::model::{ActionKind, SourceFilter, StoreItem, StoreSource};
use crate::sources::distinct_categories;
mod view;

/// Build a taffy node from `children` already built in the same tree.
/// `new_with_children` only fails when a child id does not resolve in this
/// tree, which cannot happen for children this same call just built.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

/// The sole document-backed Store keys are collaborative moderation records.
/// Browse focus, filters, pagination and all control activation stay local;
/// package, remote and moderation mutations enter through typed actions.
const REPORT_RESOURCE_KEY_PREFIX: &str = crate::doc_actions::REPORT_RESOURCE_KEY_PREFIX;
const REPORT_CASE_KEY_PREFIX: &str = "store.report_case.";
const REPORT_NOTE_KEY_PREFIX: &str = "store.report_note.";
const PAGE_SIZE: usize = 48;
/// Bound CRDT-supplied configuration before it is rendered, serialized, or
/// forwarded to the Flatpak worker. These limits are deliberately generous
/// relative to remote names and HTTPS URLs, but prevent a hostile collaboration
/// document from allocating an unbounded action payload or layout paragraph.
const MAX_REMOTE_NAME_CHARS: usize = 128;
const MAX_REMOTE_URL_BYTES: usize = 2_048;
/// The worker status can include a remote or host error. Keep the exact value
/// in the operation record and live region, but bound the presentation copy
/// so an untrusted server cannot turn the masthead into an unbounded layout.
const MAX_VISIBLE_OPERATION_STATUS_CHARS: usize = 96;
const UNKNOWN_OPERATION_STATUS: &str = "No additional details were provided.";
/// Search/history tiles need a stable, bounded summary even when an upstream
/// catalogue description is malformed or unexpectedly huge.
const MAX_RESOURCE_DETAIL_BYTES: usize = 140;
/// The contextual action snapshot deliberately stays small. The retained
/// global catalogue and `Widget::resource` resolve every omitted stable app
/// on demand, so this never hides a catalogue entry from agents or Ctrl+K.
const RESOURCE_SNAPSHOT_LIMIT: usize = 200;
const REPORT_SNAPSHOT_LIMIT: usize = 128;
const MAX_REPORT_STATUS_CHARS: usize = 128;
const MAX_REPORT_NOTE_CHARS: usize = 4_096;
const MAX_CASE_ID_CHARS: usize = 128;
const MAX_PENDING_EXTERNAL_OPERATIONS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRecord {
    pub id: String,
    pub app_id: String,
    pub status: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ReportEnvelope {
    pub format: String,
    pub app_id: String,
    pub status: String,
    pub publisher_fingerprint: Option<String>,
    pub catalogue_trust_claim: String,
    pub evidence_note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteIntent {
    Refresh,
    Add { name: String, url: String },
    Remove(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModerationIntent {
    Submit {
        registry_name: String,
        envelope: ReportEnvelope,
    },
    Refresh {
        registry_name: String,
        case_id: String,
    },
    Appeal {
        registry_name: String,
        case_id: String,
        reason: String,
    },
}

fn action_code(action: ActionKind) -> &'static str {
    match action {
        ActionKind::Open => "open",
        ActionKind::Remove => "remove",
        ActionKind::Update => "update",
        ActionKind::MarkInstalled => "pin",
        ActionKind::Unmark => "unpin",
        ActionKind::FlatpakInstall => "flatpak-install",
        ActionKind::FlatpakUpdate => "flatpak-update",
        ActionKind::FlatpakOpen => "flatpak-open",
        ActionKind::FlatpakRemove => "flatpak-remove",
        ActionKind::NeedsFlatpakHost => "flatpak-setup",
    }
}

fn action_from_code(code: &str) -> Option<ActionKind> {
    Some(match code {
        "open" => ActionKind::Open,
        "remove" => ActionKind::Remove,
        "update" => ActionKind::Update,
        "pin" => ActionKind::MarkInstalled,
        "unpin" => ActionKind::Unmark,
        "flatpak-install" => ActionKind::FlatpakInstall,
        "flatpak-update" => ActionKind::FlatpakUpdate,
        "flatpak-open" => ActionKind::FlatpakOpen,
        "flatpak-remove" => ActionKind::FlatpakRemove,
        "flatpak-setup" => ActionKind::NeedsFlatpakHost,
        _ => return None,
    })
}

fn short_publisher(item: &StoreItem) -> String {
    item.publisher_pubkey_hex.as_deref().map_or_else(
        || match item.source {
            crate::StoreSource::Flathub => "Flathub upstream metadata".into(),
            crate::StoreSource::Tilo { .. } => "Not disclosed".into(),
        },
        |key| {
            let prefix = key.chars().take(12).collect::<String>();
            format!("ed25519:{prefix}…")
        },
    )
}

fn full_publisher(item: &StoreItem) -> String {
    item.publisher_pubkey_hex.as_deref().map_or_else(
        || match item.source {
            crate::StoreSource::Flathub => "Flathub upstream metadata".into(),
            crate::StoreSource::Tilo { .. } => "Not disclosed".into(),
        },
        |key| format!("ed25519:{key}"),
    )
}

fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "Size not published".into();
    }
    ui::format::bytes_si(bytes)
}

/// A compact, printable status detail for the visible progress/error surface.
///
/// The worker's raw status is intentionally still retained for the operation
/// history and assistive live region. This display copy collapses unusual
/// whitespace and has a finite reading measure, while preserving enough of a
/// useful error (for example, a failed signature verification) to make the
/// next action clear without sending people to a separate task surface.
fn visible_operation_status(status: &str) -> String {
    let compact = status.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return UNKNOWN_OPERATION_STATUS.to_string();
    }
    let mut visible = compact
        .chars()
        .take(MAX_VISIBLE_OPERATION_STATUS_CHARS)
        .collect::<String>();
    if compact.chars().count() > MAX_VISIBLE_OPERATION_STATUS_CHARS {
        visible.push('…');
    }
    visible
}

/// Collapse arbitrary catalogue copy into a small, printable resource detail
/// without first allocating a normalized version of the entire input. This is
/// used in system-search and history presentation, where a remote listing
/// must never turn one resource summary into a large allocation.
fn resource_detail_excerpt(value: &str) -> String {
    let mut result = String::with_capacity(MAX_RESOURCE_DETAIL_BYTES);
    let mut pending_space = false;
    let mut truncated = false;
    for ch in value.chars() {
        if ch.is_control() {
            // Newlines and tabs are both control characters *and* word
            // separators. Keep that boundary rather than running words
            // together when we sanitise untrusted catalogue prose.
            pending_space |= !result.is_empty() && ch.is_whitespace();
            continue;
        }
        if ch.is_whitespace() {
            pending_space |= !result.is_empty();
            continue;
        }
        if pending_space {
            if result.len().saturating_add(1 + '…'.len_utf8()) > MAX_RESOURCE_DETAIL_BYTES {
                truncated = true;
                break;
            }
            result.push(' ');
            pending_space = false;
        }
        if result.len().saturating_add(ch.len_utf8() + '…'.len_utf8()) > MAX_RESOURCE_DETAIL_BYTES
        {
            truncated = true;
            break;
        }
        result.push(ch);
    }
    if truncated {
        result.push('…');
    }
    result
}

fn report_note_key(app_id: &str) -> String {
    format!("{REPORT_NOTE_KEY_PREFIX}{app_id}")
}

pub(crate) fn moderation_status_label(
    status: registry_types::ModerationCaseStatus,
) -> &'static str {
    use registry_types::ModerationCaseStatus;
    match status {
        ModerationCaseStatus::Open => "Open",
        ModerationCaseStatus::NeedsInformation => "Needs information",
        ModerationCaseStatus::Appealed => "Appealed",
        ModerationCaseStatus::ClosedNoAction => "Closed, no action",
        ModerationCaseStatus::Quarantined => "Quarantined",
        ModerationCaseStatus::Restored => "Restored",
    }
}

fn report_envelope(report: &ReportRecord, item: Option<&StoreItem>, note: &str) -> ReportEnvelope {
    ReportEnvelope {
        format: "tilo.store.report-envelope.v1".into(),
        app_id: report.app_id.clone(),
        status: report.status.clone(),
        publisher_fingerprint: item.and_then(|item| item.publisher_pubkey_hex.clone()),
        catalogue_trust_claim: item.map_or_else(
            || "Catalogue item no longer available".into(),
            |item| item.trust.label().into(),
        ),
        evidence_note: note.chars().take(16_384).collect(),
    }
}

fn report_export_spec(envelope: &ReportEnvelope) -> Option<ActionSpec> {
    ActionSpec::operation(
        "export-evidence",
        "Export evidence envelope",
        OperationKind::Effect,
        "store.report-export",
        envelope,
    )
    .ok()
    .map(|spec| {
        spec.execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job)
    })
}

fn report_export_binding(
    report: &ReportRecord,
    item: Option<&StoreItem>,
    note: &str,
) -> Option<ActionBinding<()>> {
    let envelope = report_envelope(report, item, note);
    let spec = report_export_spec(&envelope)?;
    let resource =
        Resource::new(report.id.clone(), "store.report", report.app_id.clone()).with_action(spec);
    ActionBinding::unit_for(&resource, "export-evidence").ok()
}

fn report_submit_spec(envelope: &ReportEnvelope, registry_name: &str) -> Option<ActionSpec> {
    ActionSpec::operation(
        "submit-report",
        "Submit signed report",
        OperationKind::Effect,
        "store.moderation",
        &(registry_name, envelope),
    )
    .ok()
    .map(|spec| {
        spec.execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Resource {
                resource_kind: "store.moderation-case".into(),
            })
    })
}

fn report_submit_binding(
    report: &ReportRecord,
    item: &StoreItem,
    note: &str,
    registry_name: &str,
) -> Option<ActionBinding<()>> {
    let envelope = report_envelope(report, Some(item), note);
    let spec = report_submit_spec(&envelope, registry_name)?;
    let resource =
        Resource::new(report.id.clone(), "store.report", report.app_id.clone()).with_action(spec);
    ActionBinding::unit_for(&resource, "submit-report").ok()
}

fn moderation_case_spec(
    action: &'static str,
    label: &'static str,
    type_id: &'static str,
    payload: &(String, String, String),
) -> Option<ActionSpec> {
    ActionSpec::operation(action, label, OperationKind::Effect, type_id, payload)
        .ok()
        .map(|spec| {
            spec.execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Resource {
                    resource_kind: "store.moderation-case".into(),
                })
        })
}

fn validate_remote_url(value: &str) -> Result<(), String> {
    if value.len() > MAX_REMOTE_URL_BYTES {
        return Err("repository URL is too long".into());
    }
    let url = reqwest::Url::parse(value).map_err(|_| "repository URL is invalid".to_string())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err("repository URL must be credential-free HTTPS".into());
    }
    Ok(())
}

fn validate_remote_name(value: &str) -> Result<(), String> {
    let name = value.trim();
    if name.is_empty()
        || name.chars().count() > MAX_REMOTE_NAME_CHARS
        || name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err("remote name must be a non-empty, short identifier".into());
    }
    Ok(())
}

fn valid_case_id(value: &str) -> bool {
    value.starts_with("case-")
        && value.chars().count() <= MAX_CASE_ID_CHARS
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn store_app_resource(item: &StoreItem, flatpak_available: bool) -> Resource {
    let action = item.action_kind(flatpak_available);
    let code = action_code(action);
    let payload = vec![item.id.clone(), code.to_string()];
    let source = match &item.source {
        StoreSource::Tilo { .. } => "Tilo verified",
        StoreSource::Flathub => "Flathub",
    };
    // The resource directory feeds system search, history, and assistive
    // clients which do not see the card layout.  Keep its summary short and
    // useful rather than exporting a second, unbounded copy of catalogue
    // metadata.  The canonical detail route still resolves the full item.
    let detail = resource_detail_excerpt(&item.description);
    let latest = item.latest_version();
    let badge = if latest.is_empty() {
        source.to_string()
    } else {
        format!("{source}, v{latest}")
    };
    let mut resource = Resource::new(format!("app:{}", item.id), "store.app", item.name.clone())
        .with_presentation(ResourcePresentation {
            detail: (!detail.is_empty()).then_some(detail),
            badge: Some(badge),
            preview: None,
        });
    if let Ok(show) = ActionSpec::operation(
        "show-details",
        "Show app details",
        OperationKind::View,
        "store.view",
        &item.id,
    ) {
        resource.actions.push(show);
    }
    add_store_item_action(
        &mut resource,
        code,
        action.label(),
        action,
        &payload,
        flatpak_available,
    );
    if let Some(pin) = item.pin_action() {
        add_store_item_action(
            &mut resource,
            action_code(pin),
            pin.label(),
            pin,
            &[item.id.clone(), action_code(pin).to_string()],
            flatpak_available,
        );
    }
    if let Some(remove) = item.remove_action() {
        add_store_item_action(
            &mut resource,
            // Keep the published action address identical to the serialized
            // action code. In particular, a Flatpak removal is not the Tilo
            // `remove` operation: using a generic id here made the card's
            // typed binding point at an action that the live resource tree
            // did not publish, so the host quite correctly rejected it.
            action_code(remove),
            "Remove",
            remove,
            &[item.id.clone(), action_code(remove).to_string()],
            flatpak_available,
        );
    }
    for version in item.versions.iter().take(32) {
        let label = if version.version.is_empty() {
            "Version metadata unavailable".into()
        } else {
            format!("Version {}", version.version)
        };
        resource.children.push(Resource::new(
            format!("app:{}:version:{}", item.id, version.version),
            "store.version",
            label,
        ));
    }
    let review_label = match item.trust {
        crate::StoreTrust::PublisherSigned => "Signature verified, no independent review",
        crate::StoreTrust::Flathub => "Flathub distribution, no Tilo review",
    };
    resource.children.push(Resource::new(
        format!("app:{}:review", item.id),
        "store.review",
        review_label,
    ));
    let report_action = crate::doc_actions::StoreDocumentAction::CreateReportDraft {
        app_id: item.id.clone(),
    };
    if let Ok(report) = ActionSpec::immediate(
        "draft-report",
        "Create report draft",
        crate::doc_actions::STORE_DOCUMENT_ACTION_TYPE,
        &report_action,
    ) {
        resource.actions.push(report.with_contract(ActionContract {
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: RetryPolicy::Idempotent,
            result: ResultSpec::None,
            ..ActionContract::default()
        }));
    }
    resource
}

/// Return an owned canonical resource from the bounded contextual snapshot.
///
/// `Widget::resource` is also used for direct links from history, agents, and
/// collaboration.  Keeping this traversal here makes roots, remotes, reports,
/// and moderation cases resolvable by their published IDs, while app resources
/// beyond the snapshot retain the lazy resolver below.
fn find_snapshot_resource(resource: Resource, id: &str) -> Option<Resource> {
    if resource.id.0 == id {
        return Some(resource);
    }
    resource
        .children
        .into_iter()
        .find_map(|child| find_snapshot_resource(child, id))
}

fn add_store_item_action(
    resource: &mut Resource,
    id: &str,
    label: &str,
    action: ActionKind,
    payload: &[String],
    flatpak_available: bool,
) {
    let Ok(spec) = ActionSpec::operation(id, label, OperationKind::Effect, "store.app", &payload)
    else {
        return;
    };
    let destructive = matches!(action, ActionKind::Remove | ActionKind::FlatpakRemove);
    resource.actions.push(spec.with_contract(ActionContract {
        availability: if action.requires_flatpak_host() && !flatpak_available {
            Availability::Unavailable {
                code: "flatpak_host_unavailable".into(),
                reason: "Flatpak is not installed on this host; install it before using Flatpak catalog actions".into(),
            }
        } else {
            Availability::Available
        },
        risk: if destructive {
            RiskLevel::Destructive
        } else {
            RiskLevel::Low
        },
        confirmation: if destructive {
            ConfirmationPolicy::Confirm
        } else {
            ConfirmationPolicy::None
        },
        retry: if matches!(
            action,
            ActionKind::Open | ActionKind::FlatpakOpen | ActionKind::NeedsFlatpakHost
        ) {
            RetryPolicy::AtMostOnce
        } else {
            RetryPolicy::Idempotent
        },
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Job,
        ..ActionContract::default()
    }));
}

fn item_action_binding(item: &StoreItem, action: ActionKind) -> Option<ActionBinding<()>> {
    let code = action_code(action);
    let payload = vec![item.id.clone(), code.to_string()];
    let spec = ActionSpec::operation(
        code,
        action.label(),
        OperationKind::Effect,
        "store.app",
        &payload,
    )
    .ok()?;
    let resource =
        Resource::new(format!("app:{}", item.id), "store.app", item.name.clone()).with_action(spec);
    ActionBinding::unit_for(&resource, code).ok()
}

fn report_binding(item: &StoreItem) -> Option<ActionBinding<()>> {
    let action = crate::doc_actions::StoreDocumentAction::CreateReportDraft {
        app_id: item.id.clone(),
    };
    let spec = ActionSpec::immediate(
        "draft-report",
        "Create report draft",
        crate::doc_actions::STORE_DOCUMENT_ACTION_TYPE,
        &action,
    )
    .ok()?;
    let resource =
        Resource::new(format!("app:{}", item.id), "store.app", item.name.clone()).with_action(spec);
    ActionBinding::unit_for(&resource, "draft-report").ok()
}

fn refresh_binding() -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        "refresh",
        "Refresh catalog",
        OperationKind::Effect,
        "store.refresh",
        &"",
    )
    .ok()?;
    let resource = Resource::new(crate::resource::CATALOGUE_ID, "store.catalog", "App Store")
        .with_action(spec);
    ActionBinding::unit_for(&resource, "refresh").ok()
}

/// Clear the query and every filter of this window's catalogue view.
fn clear_filters_binding() -> Option<ActionBinding<()>> {
    ActionBinding::unit_for(&crate::browse_actions::resource(&[]), "clear").ok()
}

fn detail_binding(item: &StoreItem) -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        "show-details",
        "Show app details",
        OperationKind::View,
        "store.view",
        &item.id,
    )
    .ok()?;
    let resource =
        Resource::new(format!("app:{}", item.id), "store.app", item.name.clone()).with_action(spec);
    ActionBinding::unit_for(&resource, "show-details").ok()
}

fn open_surface_binding(
    resource_id: &str,
    resource_kind: &str,
    resource_label: &str,
    action_id: &str,
    action_label: &str,
    type_id: &str,
) -> Option<ActionBinding<()>> {
    let spec =
        ActionSpec::operation(action_id, action_label, OperationKind::View, type_id, &()).ok()?;
    let resource = Resource::new(resource_id, resource_kind, resource_label).with_action(spec);
    ActionBinding::unit_for(&resource, action_id).ok()
}

/// A local route change is still a first-class, discoverable action.  Keeping
/// this out of Automerge is intentional: opening a detail page, returning to
/// the catalogue, and paging a result set are presentation choices for this
/// window, not events collaborators should receive in their history.
fn navigation_binding(
    action_id: &str,
    action_label: &str,
    target: &str,
) -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        action_id,
        action_label,
        OperationKind::View,
        "store.navigation",
        &target,
    )
    .ok()?;
    let resource =
        Resource::new("store:navigation", "store.navigation", "Store navigation").with_action(spec);
    ActionBinding::unit_for(&resource, action_id).ok()
}

/// Stage the values currently held in the device-local remote form through a
/// view action.  The handler validates them and emits the canonical typed
/// `store.remote/add` effect; the editable fields themselves never become
/// collaboration-document click state.
fn remote_form_binding() -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        "stage-add",
        "Add Flatpak remote",
        OperationKind::View,
        "store.remote-form",
        &(),
    )
    .ok()?;
    let resource =
        Resource::new("store:remotes", "store.remote-list", "Flatpak remotes").with_action(spec);
    ActionBinding::unit_for(&resource, "stage-add").ok()
}

fn remote_refresh_binding() -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        "refresh",
        "Refresh remotes",
        OperationKind::Effect,
        "store.remote",
        &vec!["refresh".to_string()],
    )
    .ok()?;
    let resource =
        Resource::new("store:remotes", "store.remote-list", "Flatpak remotes").with_action(spec);
    ActionBinding::unit_for(&resource, "refresh").ok()
}

fn remote_remove_binding(remote: &crate::flathub_client::Remote) -> Option<ActionBinding<()>> {
    let spec = ActionSpec::operation(
        "remove",
        "Remove remote",
        OperationKind::Effect,
        "store.remote",
        &vec!["remove".to_string(), remote.name.clone()],
    )
    .ok()?;
    let resource = Resource::new(
        format!("remote:{}", remote.name),
        "store.remote",
        remote.title.clone(),
    )
    .with_action(spec);
    ActionBinding::unit_for(&resource, "remove").ok()
}

/// Root widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (host flatpak \
              availability, a layout-rebuild gate, a pending worker action, \
              the remotes view's open/closed state, and a --shots-only seam); \
              grouping them into enums would not express any real invariant \
              between them and would touch sync/draw/handle_event call sites \
              across this file"
)]
pub struct StoreApp {
    /// Document actions: local host standalone, framework dispatcher when hosted.
    actions: crate::resource::ActionSink,
    /// Merged item feed.
    items: Vec<StoreItem>,
    /// Retained global-search projection. It is rebuilt only after a changed
    /// worker snapshot and resolves palette addresses against live rows.
    search_index: crate::search_provider::StoreSearchIndex,
    /// The items' names and descriptions, indexed while they stay the same.
    item_index: ui::search::ListIndex,
    /// Active source-side filter (`All | Tilo | Flatpak | Installed`). Kept
    /// locally: browsing one catalogue must not navigate collaborators.
    active_filter: SourceFilter,
    /// Active category or `None` for "all categories".
    active_category: Option<String>,
    /// Current catalogue query, applied to the merged catalogue. It is set by
    /// the browse actions only: people search through the platform search,
    /// which `search_index` serves, and a query here never makes a
    /// colleague's catalogue jump around.
    query: String,
    /// An unsubmitted remote form is device-local. Only the canonical Add
    /// remote effect crosses into machine-scoped package configuration.
    remote_name_state: ui::LocalState<String>,
    remote_url_state: ui::LocalState<String>,
    /// Rolling status line the worker overwrites while an action runs. It
    /// reads in the operation notice while busy, and once, in the status
    /// bubble, when the operation settles.
    status_line: String,
    worker_busy: bool,
    /// The one app mutation currently accepted by the serialized worker.
    /// Keeping its stable app id and offered action locally lets the matching
    /// card communicate progress and reject a second activation; it is a
    /// backend observation, not shared document state.
    active_operation: Option<(String, ActionKind)>,
    operation_error: Option<String>,
    /// Whether `flatpak` is available on the host — drives whether
    /// Flatpak items show "Install" or the "install flatpak first"
    /// helper.
    flatpak_available: bool,
    /// Which item the user has expanded in the responsive detail sheet.
    /// Keyed by [`StoreItem::id`] and retained across window rebuilds so the
    /// document-backed route does not disappear during a responsive reflow.
    focused_detail: Option<String>,
    /// Marked dirty when a chip / category selection flips so the
    /// host re-runs `build_layout`.
    structure_dirty: bool,
    /// The rect the toplevel container resolves into during
    /// `apply_layout` — needed for `get_rect`.
    rect: Rect,
    /// Root of the built layout so events can be routed to the
    /// composed sub-tree.
    root: Option<Box<dyn Widget>>,
    pending_lifecycle_actions: Vec<(String, ActionKind, tilo_actions::InvocationId)>,
    pending_lifecycle_refresh: Option<tilo_actions::InvocationId>,
    pending_lifecycle_remote: Vec<(RemoteIntent, tilo_actions::InvocationId)>,
    completed_invocations: Vec<tilo_actions::InvocationUpdate>,
    pending_search: Option<String>,
    pending_detail: Option<String>,
    remotes: Vec<crate::flathub_client::Remote>,
    remotes_open: bool,
    safety_open: bool,
    reports: Vec<ReportRecord>,
    /// Becomes true after the first shared report resource is observed. Until
    /// then an in-memory presentation fixture (including `--shots`) is not
    /// mistaken for a collaborator deleting every draft from a fresh doc.
    reports_hydrated: bool,
    report_notes: HashMap<String, String>,
    pending_control_invocation: Option<ControlInvocation>,
    report_export: Option<(
        tilo_actions::InvocationId,
        ui::portal::PortalChannel,
        Vec<u8>,
    )>,
    pending_moderation: Vec<(ModerationIntent, tilo_actions::InvocationId)>,
    /// List position survives the composed widget tree being rebuilt after a
    /// filter, search result, or catalogue update.
    scroll_offset: Rc<Cell<(f32, f32)>>,
    catalog_page: usize,
    /// The transient status bubble: an operation's result, shown once when
    /// it changes.
    status_bubble: ui::StatusBubble,
    /// Last pointer position (window coords), tracked from `CursorMoved`
    /// so the bubble can stay open while the pointer rests on it.
    last_cursor: Point,
    /// Shared async, bounded decoded-artwork cache. `None` for hermetic shots.
    images: Option<ui::ImageStore>,
    /// Actions a list row asked for (its closure cannot reach the host):
    /// returned through [`ActionControls::take_control_invocation`].
    queued_invocations: Rc<RefCell<Vec<ControlInvocation>>>,
    /// Concern text a capture staged: written to the shared document on the
    /// next sync, which is where the concern field reads it.
    staged_notes: Vec<(String, String)>,
}

impl Default for StoreApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreApp {
    pub fn new() -> Self {
        Self {
            actions: crate::resource::ActionSink::default(),
            items: Vec::new(),
            search_index: crate::search_provider::StoreSearchIndex::default(),
            item_index: ui::search::ListIndex::default(),
            active_filter: SourceFilter::All,
            active_category: None,
            query: String::new(),
            remote_name_state: ui::LocalState::new(String::new()),
            remote_url_state: ui::LocalState::new(String::new()),
            status_line: String::new(),
            worker_busy: false,
            active_operation: None,
            operation_error: None,
            flatpak_available: crate::flathub_client::flatpak_available(),
            focused_detail: None,
            structure_dirty: true,
            rect: Rect::ZERO,
            root: None,
            pending_lifecycle_actions: Vec::new(),
            pending_lifecycle_refresh: None,
            pending_lifecycle_remote: Vec::new(),
            completed_invocations: Vec::new(),
            pending_search: None,
            pending_detail: None,
            remotes: Vec::new(),
            remotes_open: false,
            safety_open: false,
            reports: Vec::new(),
            reports_hydrated: false,
            report_notes: HashMap::new(),
            pending_control_invocation: None,
            report_export: None,
            pending_moderation: Vec::new(),
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            catalog_page: 0,
            status_bubble: ui::StatusBubble::new(),
            last_cursor: Point::ZERO,
            images: None,
            queued_invocations: Rc::default(),
            staged_notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_image_store(mut self, images: ui::ImageStore) -> Self {
        self.images = Some(images);
        self
    }

    /// Replace the artwork cache: `None` shows every app with its generated
    /// icon and no previews. A capture passes a store that serves bundled
    /// artwork, so it never reaches for the network.
    pub fn set_image_store(&mut self, images: Option<ui::ImageStore>) {
        self.images = images;
        self.structure_dirty = true;
    }

    /// Share the retained action/search catalogue with the serialized worker.
    /// The worker updates it before publishing a UI snapshot, which keeps
    /// refresh indexing out of the render/input path.
    #[must_use]
    pub fn with_search_index(
        mut self,
        search_index: crate::search_provider::StoreSearchIndex,
    ) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn take_lifecycle_actions(
        &mut self,
    ) -> Vec<(String, ActionKind, tilo_actions::InvocationId)> {
        std::mem::take(&mut self.pending_lifecycle_actions)
    }

    pub fn take_lifecycle_refresh(&mut self) -> Option<tilo_actions::InvocationId> {
        self.pending_lifecycle_refresh.take()
    }

    pub fn take_lifecycle_remote(&mut self) -> Vec<(RemoteIntent, tilo_actions::InvocationId)> {
        std::mem::take(&mut self.pending_lifecycle_remote)
    }

    pub fn complete_lifecycle_invocation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        error: Option<String>,
    ) {
        self.completed_invocations.push(match error {
            Some(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "store.operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
            None => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        });
    }

    pub fn take_search_requested(&mut self) -> Option<String> {
        self.pending_search.take()
    }

    pub fn take_detail_requested(&mut self) -> Option<String> {
        self.pending_detail.take()
    }

    pub fn reports(&self) -> &[ReportRecord] {
        &self.reports
    }

    pub fn take_moderation_requests(
        &mut self,
    ) -> Vec<(ModerationIntent, tilo_actions::InvocationId)> {
        std::mem::take(&mut self.pending_moderation)
    }

    pub fn set_moderation_case(
        &mut self,
        state: &mut AutoCommit,
        _path: &ObjId,
        case: &registry_types::ModerationCase,
    ) {
        if !valid_case_id(&case.case_id) {
            self.status_line = "Registry returned an invalid moderation case identifier.".into();
            self.operation_error = Some(self.status_line.clone());
            self.structure_dirty = true;
            return;
        }
        if let Some(report) = self
            .reports
            .iter_mut()
            .find(|report| report.app_id == case.package_id)
        {
            report.id.clone_from(&case.case_id);
            report.status = moderation_status_label(case.status).into();
        }
        self.perform_document_action(
            state,
            &crate::doc_actions::StoreDocumentAction::RecordModerationCase {
                app_id: case.package_id.clone(),
                case_id: case.case_id.clone(),
                status: moderation_status_label(case.status).into(),
            },
        );
        self.status_line = format!(
            "Moderation case {} is {}.",
            case.case_id,
            moderation_status_label(case.status)
        );
        self.structure_dirty = true;
    }

    pub fn focus_detail_for_shot(&mut self, id: &str) {
        self.focused_detail = Some(id.to_string());
        self.structure_dirty = true;
    }

    /// Open a stable catalogue identity supplied by the shell's `tilo-store:`
    /// URI route. This is deliberately presentation-only: the user still
    /// sees provenance and chooses the current, revalidated install action.
    pub fn open_catalogue_item(&mut self, id: String) {
        self.focused_detail = Some(id.clone());
        self.pending_detail = Some(id);
        self.status_line = "Opening app details…".into();
        self.structure_dirty = true;
    }

    pub fn show_safety_for_shot(&mut self) {
        self.safety_open = true;
        self.reports.push(ReportRecord {
            id: "report:so.tilo.notes".into(),
            app_id: "so.tilo.notes".into(),
            status: "Not submitted".into(),
        });
        self.structure_dirty = true;
    }

    pub fn show_moderation_case_for_shot(&mut self, status: &str) {
        self.show_safety_for_shot();
        self.staged_notes.push((
            "so.tilo.notes".into(),
            "The app contacted a third-party host while its offline mode was enabled. The exported trace contains the exact destination and time.".into(),
        ));
        if let Some(report) = self.reports.first_mut() {
            report.id = "case-8d10fce04f126f0c2f293c16".into();
            report.status = status.into();
        }
        self.structure_dirty = true;
    }

    /// Populate the machine-scoped remote management view for a deterministic
    /// compact-layout screenshot without querying the real Flatpak host.
    pub fn show_remotes_for_shot(&mut self) {
        self.remotes_open = true;
        self.remotes = vec![crate::flathub_client::Remote {
            name: "flathub".into(),
            url: "https://dl.flathub.org/repo/".into(),
            title: "Flathub".into(),
            disabled: false,
        }];
        self.structure_dirty = true;
    }

    pub fn clear_catalogue_for_shot(&mut self) {
        self.items.clear();
        self.structure_dirty = true;
    }

    pub fn set_remotes(&mut self, remotes: Vec<crate::flathub_client::Remote>) {
        if self.remotes != remotes {
            self.remotes = remotes;
            self.structure_dirty = true;
        }
    }

    /// Merge an enriched `AppStream` record into the current list and keep the
    /// detail view focused on it.
    pub fn set_detail(&mut self, detail: StoreItem) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == detail.id) {
            *item = detail.clone();
        } else {
            self.items.push(detail.clone());
        }
        self.focused_detail = Some(detail.id);
        self.search_index.sync(&self.items);
        self.structure_dirty = true;
    }

    /// Apply a detail snapshot whose retained search projection was already
    /// prepared by the serialized Store worker.  Catalogue-wide indexing is
    /// intentionally never part of a frame-time snapshot drain.
    pub fn set_detail_from_worker(&mut self, detail: StoreItem) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == detail.id) {
            *item = detail.clone();
        } else {
            self.items.push(detail.clone());
        }
        self.focused_detail = Some(detail.id);
        self.structure_dirty = true;
    }

    /// Replace the item feed. Idempotent — the widget re-derives its
    /// category chip row from `items` on the next `build_layout`.
    pub fn set_items(&mut self, items: Vec<StoreItem>) {
        self.items = items;
        self.search_index.sync(&self.items);
        self.structure_dirty = true;
    }

    /// Apply a worker snapshot without rebuilding the retained search
    /// directory on the UI thread. [`Self::set_items`] remains the
    /// synchronous, self-contained constructor/test path.
    pub fn set_items_from_worker(&mut self, items: Vec<StoreItem>) {
        self.items = items;
        self.structure_dirty = true;
    }

    pub fn items(&self) -> &[StoreItem] {
        &self.items
    }

    /// Adopt collaborative report drafts from the framework resource.
    /// An empty projection does not wipe a non-empty local catalogue feed, and
    /// does not clear presentation-only report seeds before the first shared
    /// draft has been observed.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::StoreState) {
        if state.reports.is_empty() && !self.reports_hydrated && !self.reports.is_empty() {
            return;
        }
        let reports: Vec<ReportRecord> = state
            .reports
            .iter()
            .map(|report| ReportRecord {
                id: report.id.clone(),
                app_id: report.app_id.clone(),
                status: report.status.clone(),
            })
            .collect();
        if reports != self.reports {
            self.reports = reports;
            self.structure_dirty = true;
        }
        self.reports_hydrated |= !state.reports.is_empty();
        for report in &state.reports {
            self.report_notes
                .insert(report.app_id.clone(), report.note.clone());
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ui::framework::ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Store>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Apply a document action through the local host or framework dispatcher.
    pub(crate) fn perform_document_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::doc_actions::StoreDocumentAction,
    ) {
        let context =
            ui::tilo_actions::ActionContext::for_action("so.tilo.store", "App Store", action);
        self.actions.dispatch_with_context(
            doc,
            crate::doc_actions::STORE_DOCUMENT_ACTION_TYPE,
            action,
            context,
        );
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Override the flatpak-availability probe result. Tests use
    /// this so they don't have to depend on the host's `flatpak`
    /// binary.
    pub fn set_flatpak_available(&mut self, available: bool) {
        self.flatpak_available = available;
        self.structure_dirty = true;
    }

    pub fn active_filter(&self) -> SourceFilter {
        self.active_filter
    }

    pub fn set_active_filter(&mut self, f: SourceFilter) {
        if self.active_filter != f {
            self.active_filter = f;
            self.catalog_page = 0;
            self.scroll_offset.set((0.0, 0.0));
            self.structure_dirty = true;
        }
    }

    fn set_active_filter_from_label(&mut self, value: &str) {
        if let Some(filter) = SourceFilter::ALL
            .iter()
            .copied()
            .find(|filter| filter.label() == value)
        {
            self.set_active_filter(filter);
        }
    }

    /// Whether the catalogue is narrowed by a query, a source or a category.
    pub(crate) fn filters_active(&self) -> bool {
        !self.query.trim().is_empty()
            || self.active_filter != SourceFilter::All
            || self.active_category.is_some()
    }

    pub fn active_category(&self) -> Option<&str> {
        self.active_category.as_deref()
    }

    pub fn set_active_category(&mut self, c: Option<String>) {
        if self.active_category != c {
            self.active_category = c;
            self.catalog_page = 0;
            self.scroll_offset.set((0.0, 0.0));
            self.structure_dirty = true;
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, q: String) {
        if self.query != q {
            self.query = q;
            self.catalog_page = 0;
            self.scroll_offset.set((0.0, 0.0));
            self.pending_search = Some(self.query.clone());
            self.structure_dirty = true;
        }
    }

    pub fn set_status_line(&mut self, s: impl Into<String>) {
        self.status_line = s.into();
    }

    pub fn set_worker_busy(&mut self, busy: bool) {
        if self.worker_busy != busy || (!busy && self.active_operation.is_some()) {
            self.worker_busy = busy;
            if !busy {
                self.active_operation = None;
            }
            self.structure_dirty = true;
        }
    }

    /// Deterministic screenshot seam for the active-card progress treatment.
    pub fn set_active_operation_for_shot(&mut self, id: &str, action: ActionKind) {
        self.active_operation = Some((id.to_string(), action));
        self.structure_dirty = true;
    }

    pub fn set_operation_error(&mut self, error: Option<String>) {
        if self.operation_error != error {
            self.operation_error = error;
            self.structure_dirty = true;
        }
    }

    pub fn status_line(&self) -> &str {
        &self.status_line
    }

    /// The visible-after-filters item list. Kept public because
    /// tests + the detail-sheet reducer both need it.
    pub fn visible_items(&self) -> Vec<&StoreItem> {
        let cat = self.active_category.as_deref();
        let hits: Vec<&StoreItem> = self
            .items
            .iter()
            .filter(|i| self.active_filter.accepts(i))
            .filter(|i| match cat {
                Some(c) => i.categories.iter().any(|k| k == c),
                None => true,
            })
            .collect();
        let ranks = crate::model::relevance(&self.item_index, &self.query, &hits);
        let mut hits: Vec<(&StoreItem, i32)> = hits
            .into_iter()
            .zip(ranks)
            .filter_map(|(item, rank)| Some((item, rank?)))
            .collect();
        // Ranking: when idle, marked-installed (pinned) apps sort
        // first, then merely-cached, then everything else —
        // alphabetised within each band so the list is stable across
        // refreshes. When a query is active the best fuzzy match wins
        // regardless of install state.
        if self.query.trim().is_empty() {
            hits.sort_by_key(|(i, _)| {
                (
                    std::cmp::Reverse(i.install_rank()),
                    i.name.to_ascii_lowercase(),
                )
            });
        } else {
            hits.sort_by_key(|(_, rank)| std::cmp::Reverse(*rank));
        }
        hits.into_iter().map(|(item, _)| item).collect()
    }

    /// Category chips: `All` first, then every distinct category
    /// derived from the item feed.
    fn category_labels(&self) -> Vec<String> {
        let mut all = vec!["All".to_string()];
        all.extend(distinct_categories(&self.items));
        all
    }

    /// Report what an operation left behind, once, when it settles. While an
    /// operation runs or has failed the page itself says so (the operation
    /// notice), so the bubble stays quiet.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        if !self.worker_busy && self.operation_error.is_none() && !self.status_line.is_empty() {
            self.status_bubble.show(&self.status_line, now);
        }
        self.status_bubble
            .pointer_moved(self.rect, self.last_cursor, tr, theme, now);
        self.status_bubble
            .paint(scene, transform, tr, theme, self.rect, now);
    }

    fn sync_report_resources(&mut self, state: &AutoCommit, path: &ObjId) {
        let has_shared_reports = state
            .keys(path)
            .any(|key| key.starts_with(REPORT_RESOURCE_KEY_PREFIX));
        let mut reports = state
            .keys(path)
            .filter_map(|key| {
                let app_id = key.strip_prefix(REPORT_RESOURCE_KEY_PREFIX)?.to_string();
                if app_id.is_empty()
                    || app_id.len() > 255
                    || !app_id
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
                {
                    return None;
                }
                let status = state
                    .get(path, &key)
                    .ok()
                    .flatten()?
                    .0
                    .as_str()?
                    .chars()
                    .take(MAX_REPORT_STATUS_CHARS)
                    .collect();
                Some(ReportRecord {
                    id: state
                        .get(path, format!("{REPORT_CASE_KEY_PREFIX}{app_id}"))
                        .ok()
                        .flatten()
                        .and_then(|(value, _)| value.as_str().map(str::to_owned))
                        .filter(|value| valid_case_id(value))
                        .unwrap_or_else(|| format!("report:{app_id}")),
                    app_id,
                    status,
                })
            })
            .take(REPORT_SNAPSHOT_LIMIT)
            .collect::<Vec<_>>();
        reports.sort_by(|a, b| a.app_id.cmp(&b.app_id));
        // A peer can deliberately delete every shared draft. Once this app
        // has observed a document-backed report, adopt that empty state too:
        // retaining a removed local record would leave a stale moderation
        // target visible and could make an old action look valid. Before the
        // first shared report, retain a presentation-only seed (used by
        // headless scenarios) rather than treating a new empty document as a
        // deletion.
        if (has_shared_reports || self.reports_hydrated) && reports != self.reports {
            self.reports = reports;
            self.structure_dirty = true;
        }
        self.reports_hydrated |= has_shared_reports;
    }

    fn drain_report_export(&mut self) {
        let outcome = self
            .report_export
            .as_ref()
            .and_then(|(_, receiver, _)| receiver.try_recv().ok());
        let Some(outcome) = outcome else { return };
        let Some((invocation, _receiver, bytes)) = self.report_export.take() else {
            return;
        };
        let update = match outcome {
            ui::portal::PortalOutcome::Opened(path) => {
                match tilo_files::native::atomic::write(&path, &bytes) {
                    Ok(()) => {
                        self.status_line =
                            format!("Exported report evidence to {}.", path.display());
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    }
                    Err(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "store.report_export_failed".into(),
                            message: format!("Could not export report evidence: {error}"),
                            details: None,
                            retryable: true,
                        },
                    },
                }
            }
            ui::portal::PortalOutcome::Cancelled => tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Export cancelled".into()),
            },
            ui::portal::PortalOutcome::Failed(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "store.report_export_failed".into(),
                    message,
                    details: None,
                    retryable: true,
                },
            },
        };
        self.completed_invocations.push(update);
        self.structure_dirty = true;
    }
}

fn text_at(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    ui::doc::read_text(state, root, key)
        .unwrap_or_default()
        .chars()
        .take(MAX_REPORT_NOTE_CHARS)
        .collect()
}

impl Widget for StoreApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::browse_actions::STORE_BROWSE_OPERATION_TYPE,
            "store.refresh",
            "store.app",
            "store.remote",
            "store.remote-form",
            "store.navigation",
            "store.view",
            "store.view-remotes",
            "store.view-trust",
            "store.report-export",
            "store.moderation",
            "store.moderation-status",
            "store.moderation-appeal",
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let index = self
            .completed_invocations
            .iter()
            .position(|update| update.invocation() == invocation)?;
        Some(self.completed_invocations.remove(index))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.drain_report_export();
        self.sync_report_resources(state, path);
        let missing_notes: Vec<String> = self
            .reports
            .iter()
            .filter(|report| {
                !matches!(
                    state.get(path, report_note_key(&report.app_id)),
                    Ok(Some((Value::Object(ObjType::Text), _)))
                )
            })
            .map(|report| report.app_id.clone())
            .collect();
        for app_id in missing_notes {
            self.perform_document_action(
                state,
                &crate::doc_actions::StoreDocumentAction::EnsureReportNote { app_id },
            );
        }
        for (app_id, note) in std::mem::take(&mut self.staged_notes) {
            self.perform_document_action(
                state,
                &crate::doc_actions::StoreDocumentAction::SetReportNote { app_id, note },
            );
        }
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        self.report_notes = self
            .reports
            .iter()
            .map(|report| {
                (
                    report.app_id.clone(),
                    text_at(state, path, &report_note_key(&report.app_id)),
                )
            })
            .collect();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.structure_dirty = false;
        let mut root = self.compose(theme);
        let child = root.build_layout(taffy, theme, node_map);
        self.root = Some(root);
        // A column, so the page's scroller can absorb the window's height.
        let node = parent_node(
            taffy,
            Style {
                flex_direction: taffy::FlexDirection::Column,
                flex_grow: 1.0,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[child],
        );
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
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node))
            && let Some(&child_node) = children.first()
        {
            root.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(root) = self.root.as_mut() {
            root.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        if let Some(root) = self.root.as_mut() {
            root.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Child overlays first, then the transient status bubble on top of
        // the content.
        if let Some(root) = self.root.as_mut() {
            root.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.draw_status_bubble(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // Track the pointer so the bubble stays open while it rests on it
        // (coords are window-space, unscaled).
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let (Some(root), Ok(children)) = (self.root.as_mut(), ctx.taffy.children(ctx.node_id))
            && let Some(&child_node) = children.first()
        {
            root.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root
            .as_ref()
            .map(|r| r.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root
            .as_mut()
            .map(|r| r.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "StoreApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The composed child tree supplies individual control semantics; the
    /// canvas-free root still names the application for assistive technology.
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window)
            .with_name("App Store")
            .with_bounds(self.rect);
        if !self.status_line.is_empty() {
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name(self.status_line.clone())
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.root.as_mut().is_some_and(|r| r.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|r| r.needs_animation_frame())
            || self.status_bubble.needs_frame(ui::time::now())
            || self
                .images
                .as_ref()
                .is_some_and(|images| images.pending() > 0)
            || self.report_export.is_some()
    }
}

impl ui::search::capability::SearchSource for StoreApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl StoreApp {
    /// Standalone / `--shots` / unit-test tree: framework publish plus live
    /// catalogue, remotes and install progress overlaid on one root.
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let state = crate::doc_actions::state_from_doc(doc);
        let mut roots = crate::resource::provide_resources(&state);
        let Some(root) = roots.first_mut() else {
            return roots;
        };
        self.overlay_live_catalogue(root, doc);
        roots
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.publish_resources(doc)
            .into_iter()
            .find_map(|resource| find_snapshot_resource(resource, id))
    }

    /// Hosted: what the framework's root cannot publish because it is not
    /// document state: the live catalogue's apps and publishers, the browse
    /// and navigation views, the Flatpak remotes, in-flight status, and the
    /// moderation actions that need the catalogue. These are siblings of the
    /// framework's root, never a second `store.catalog`: document operations
    /// stay on [`tilo_resource::publish`]. Every control the pages draw is
    /// bound to an address in this set, so a control whose address is missing
    /// here is disabled.
    pub(crate) fn hosted_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut resources = self.live_catalogue_children(doc);
        resources.push(self.live_moderation_resource(doc));
        resources
    }

    fn live_catalogue_children(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut children = Vec::new();
        children.push(crate::browse_actions::resource(&self.category_labels()));
        let mut navigation =
            Resource::new("store:navigation", "store.navigation", "Store navigation");
        for (id, label, target) in [
            ("show-catalogue", "Back to apps", "catalogue"),
            ("previous-page", "Previous catalogue page", "previous-page"),
            ("next-page", "Next catalogue page", "next-page"),
        ] {
            if let Ok(action) =
                ActionSpec::operation(id, label, OperationKind::View, "store.navigation", &target)
            {
                navigation.actions.push(action);
            }
        }
        children.push(navigation);

        let mut publishers: HashMap<String, Resource> = HashMap::new();
        let mut snapshot_items = self.items.iter().collect::<Vec<_>>();
        snapshot_items.sort_by(|left, right| left.id.cmp(&right.id));
        for item in snapshot_items.into_iter().take(RESOURCE_SNAPSHOT_LIMIT) {
            let resource = store_app_resource(item, self.flatpak_available);
            let publisher_id = item
                .publisher_pubkey_hex
                .clone()
                .unwrap_or_else(|| "flathub".into());
            let publisher_label = if publisher_id == "flathub" {
                "Flathub distribution".into()
            } else {
                format!("Publisher {}", short_publisher(item))
            };
            publishers
                .entry(publisher_id.clone())
                .or_insert_with(|| {
                    Resource::new(
                        format!("publisher:{publisher_id}"),
                        "store.publisher",
                        publisher_label,
                    )
                })
                .children
                .push(resource);
        }
        let mut publisher_resources = publishers.into_values().collect::<Vec<_>>();
        publisher_resources.sort_by(|a, b| a.label.cmp(&b.label));
        children.extend(publisher_resources);
        if self.items.len() > RESOURCE_SNAPSHOT_LIMIT {
            children.push(Resource::new(
                "store:catalogue-snapshot",
                "store.catalogue-snapshot",
                format!(
                    "{} additional apps are available through App Store search",
                    self.items.len().saturating_sub(RESOURCE_SNAPSHOT_LIMIT)
                ),
            ));
        }
        children.push(self.live_remotes_resource());
        if !self.status_line.is_empty() {
            children.push(Resource::new(
                "store:latest-job",
                "store.install-job",
                self.status_line.clone(),
            ));
        }
        children
    }

    fn live_remotes_resource(&self) -> Resource {
        let mut remotes = Resource::new("store:remotes", "store.remote-list", "Flatpak remotes");
        if let Ok(show) = ActionSpec::operation(
            "show",
            "Show Flatpak remotes",
            OperationKind::View,
            "store.view-remotes",
            &(),
        ) {
            remotes.actions.push(show);
        }
        if let Ok(stage_add) = ActionSpec::operation(
            "stage-add",
            "Add Flatpak remote",
            OperationKind::View,
            "store.remote-form",
            &(),
        ) {
            remotes.actions.push(stage_add);
        }
        if let Ok(refresh) = ActionSpec::operation(
            "refresh",
            "Refresh remotes",
            OperationKind::Effect,
            "store.remote",
            &vec!["refresh"],
        ) {
            remotes.actions.push(
                refresh
                    .execution(ExecutionMode::Background { cancellable: false })
                    .returns(ResultSpec::Job),
            );
        }
        remotes.actions.push(
            ActionSpec::typed_operation(
                "add",
                "Add Flatpak remote",
                "Remote name and HTTPS repository URL",
                OperationKind::Effect,
                "store.remote",
                vec![
                    ActionParam::required("name", ParamType::Text, "Unique remote name"),
                    ActionParam::required("url", ParamType::Text, "HTTPS repository URL"),
                ],
                |args: &ActionArgs| {
                    EncodedAction::encode_operation(
                        "store.remote",
                        &vec![
                            "add".to_string(),
                            args.text("name").unwrap_or("").to_string(),
                            args.text("url").unwrap_or("").to_string(),
                        ],
                    )
                    .unwrap_or_else(|_| EncodedAction {
                        type_id: "store.remote".into(),
                        payload: Vec::new(),
                    })
                },
            )
            .requiring(ui::tilo_actions::PermissionRequirement::Admin)
            .privileged()
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        );
        for remote in &self.remotes {
            let mut resource = Resource::new(
                format!("remote:{}", remote.name),
                "store.remote",
                remote.title.clone(),
            );
            if let Ok(remove) = ActionSpec::operation(
                "remove",
                "Remove remote",
                OperationKind::Effect,
                "store.remote",
                &vec!["remove".to_string(), remote.name.clone()],
            ) {
                resource.actions.push(remove.with_contract(ActionContract {
                    permission: ui::tilo_actions::PermissionRequirement::Admin,
                    risk: RiskLevel::Destructive,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::Idempotent,
                    execution: ExecutionMode::Background { cancellable: false },
                    result: ResultSpec::Job,
                    ..ActionContract::default()
                }));
            }
            remotes.children.push(resource);
        }
        remotes
    }

    fn live_moderation_resource(&self, doc: &AutoCommit) -> Resource {
        let mut moderation = Resource::new(
            "store:moderation",
            "store.moderation",
            "Trust and moderation",
        );
        if let Ok(show) = ActionSpec::operation(
            "show",
            "Show trust and moderation",
            OperationKind::View,
            "store.view-trust",
            &(),
        ) {
            moderation.actions.push(show);
        }
        for report in &self.reports {
            let mut report_resource = Resource::new(
                report.id.clone(),
                "store.report",
                format!("{}: {}", report.app_id, report.status),
            );
            let note = text_at(doc, &automerge::ROOT, &report_note_key(&report.app_id));
            let note_app_id = report.app_id.clone();
            report_resource.actions.push(ActionSpec::prompted(
                "set-note",
                "Set report concern",
                "Concern and supporting evidence",
                "Describe what happened and how it can be verified",
                ExecutionLocus::ClientLocal,
                move |note| {
                    EncodedAction::encode(
                        crate::doc_actions::STORE_DOCUMENT_ACTION_TYPE,
                        &crate::doc_actions::StoreDocumentAction::SetReportNote {
                            app_id: note_app_id.clone(),
                            note: note.to_owned(),
                        },
                    )
                    .unwrap_or_else(|_| EncodedAction {
                        type_id: crate::doc_actions::STORE_DOCUMENT_ACTION_TYPE.into(),
                        payload: Vec::new(),
                    })
                },
            ));
            if !note.trim().is_empty() {
                report_resource.children.push(Resource::new(
                    format!("{}:note", report.id),
                    "store.report-note",
                    note.clone(),
                ));
            }
            if let Some(export) = report_export_spec(&report_envelope(
                report,
                self.items.iter().find(|item| item.id == report.app_id),
                &note,
            )) {
                report_resource.actions.push(export);
            }
            if let Some(item) = self.items.iter().find(|item| item.id == report.app_id)
                && let StoreSource::Tilo { registry_name } = &item.source
            {
                if report.id.starts_with("case-") {
                    for (action, label, type_id, reason) in [
                        (
                            "refresh-status",
                            "Refresh status",
                            "store.moderation-status",
                            "",
                        ),
                        (
                            "appeal",
                            "Appeal decision",
                            "store.moderation-appeal",
                            note.as_str(),
                        ),
                    ] {
                        if let Some(spec) = moderation_case_spec(
                            action,
                            label,
                            type_id,
                            &(registry_name.clone(), report.id.clone(), reason.to_string()),
                        ) {
                            report_resource.actions.push(spec);
                        }
                    }
                } else if let Some(submit) =
                    report_submit_spec(&report_envelope(report, Some(item), &note), registry_name)
                {
                    report_resource.actions.push(submit);
                }
            }
            moderation.children.push(report_resource);
        }
        moderation
    }

    fn overlay_live_catalogue(&self, root: &mut Resource, doc: &AutoCommit) {
        // The document moderation child stays. Live reports that are not in
        // the document are added beside it. Catalogue rows stay live.
        let live = self.live_moderation_resource(doc);
        if let Some(published) = root
            .children
            .iter_mut()
            .find(|child| child.kind == "store.moderation")
        {
            for action in live.actions {
                if !published.actions.iter().any(|have| have.id == action.id) {
                    published.actions.push(action);
                }
            }
            for child in live.children {
                if !published.children.iter().any(|have| have.id == child.id) {
                    published.children.push(child);
                }
            }
        } else {
            root.children.push(live);
        }
        root.children.extend(self.live_catalogue_children(doc));
    }
}

impl ui::undo::CommittedActions for StoreApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ui::resources::PublishesResources for StoreApp {
    /// Standalone and unit tests publish the full tree. When the framework
    /// hosts the surface, [`tilo_resource::publish`] already owns the
    /// catalogue door: publish the live resources beside it.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return self.hosted_resources(doc);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        // The framework root already owns the catalogue door.
        if self.is_hosted() && id == crate::resource::CATALOGUE_ID {
            return None;
        }
        // Resolve every ID the published catalogue already lists before
        // falling back to an app outside that page. Hosted lookup uses the
        // same catalogue, not only the live install-progress extra.
        if let Some(resource) = self
            .publish_resources(doc)
            .into_iter()
            .find_map(|resource| find_snapshot_resource(resource, id))
        {
            return Some(resource);
        }
        if let Some(rest) = id.strip_prefix("app:") {
            if let Some((app_id, version)) = rest.split_once(":version:") {
                let item = self.items.iter().find(|item| item.id == app_id)?;
                let version = item
                    .versions
                    .iter()
                    .find(|entry| entry.version == version)?;
                let label = if version.version.is_empty() {
                    "Version metadata unavailable".into()
                } else {
                    format!("Version {}", version.version)
                };
                return Some(Resource::new(id, "store.version", label));
            }
            if let Some(app_id) = rest.strip_suffix(":review") {
                let item = self.items.iter().find(|item| item.id == app_id)?;
                let label = match item.trust {
                    crate::StoreTrust::PublisherSigned => {
                        "Signature verified, no independent review"
                    }
                    crate::StoreTrust::Flathub => "Flathub distribution, no Tilo review",
                };
                return Some(Resource::new(id, "store.review", label));
            }
            return self
                .items
                .iter()
                .find(|item| item.id == rest)
                .map(|item| store_app_resource(item, self.flatpak_available));
        }
        if let Some(publisher_id) = id.strip_prefix("publisher:") {
            let item = self.items.iter().find(|item| {
                item.publisher_pubkey_hex.as_deref().unwrap_or("flathub") == publisher_id
            })?;
            let label = if publisher_id == "flathub" {
                "Flathub distribution".into()
            } else {
                format!("Publisher {}", short_publisher(item))
            };
            return Some(Resource::new(id, "store.publisher", label));
        }
        if id == "store:catalogue-snapshot" && self.items.len() > RESOURCE_SNAPSHOT_LIMIT {
            return Some(Resource::new(
                id,
                "store.catalogue-snapshot",
                format!(
                    "{} apps omitted from the contextual snapshot",
                    self.items.len() - RESOURCE_SNAPSHOT_LIMIT
                ),
            ));
        }
        None
    }
}

impl ui::widget::controls::ActionControls for StoreApp {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.root
            .as_mut()
            .and_then(|root| ui::widget::controls::take_control_invocation(root.as_mut()))
            .or_else(|| {
                let mut queue = self.queued_invocations.borrow_mut();
                (!queue.is_empty()).then(|| queue.remove(0))
            })
            .or_else(|| self.pending_control_invocation.take())
    }
}

impl StoreApp {
    #[allow(
        clippy::too_many_lines,
        reason = "the Store's synchronous action boundary keeps every local view route and typed operation refusal together so capability checks cannot split from resource dispatch"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == OperationKind::View && type_id == "store.navigation" {
            let target: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            match target.as_str() {
                "catalogue" => {
                    self.focused_detail = None;
                    self.remotes_open = false;
                    self.safety_open = false;
                }
                "previous-page" => {
                    self.catalog_page = self.catalog_page.saturating_sub(1);
                    self.scroll_offset.set((0.0, 0.0));
                }
                "next-page" => {
                    let pages = self.visible_items().len().div_ceil(PAGE_SIZE).max(1);
                    self.catalog_page = (self.catalog_page + 1).min(pages.saturating_sub(1));
                    self.scroll_offset.set((0.0, 0.0));
                }
                _ => return Err("unknown Store navigation target".into()),
            }
            self.structure_dirty = true;
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "store.remote-form" {
            let (): () = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            let name = self.remote_name_state.with(Clone::clone);
            let url = self.remote_url_state.with(Clone::clone);
            if validate_remote_name(&name).is_ok() && validate_remote_url(&url).is_ok() {
                self.pending_control_invocation = Some(ControlInvocation {
                    address: ActionAddress::new("store:remotes", "add"),
                    arguments: serde_json::Map::from_iter([
                        ("name".into(), serde_json::Value::String(name)),
                        ("url".into(), serde_json::Value::String(url)),
                    ]),
                });
            } else {
                self.status_line =
                    "A remote name and credential-free HTTPS repository URL are required.".into();
                self.structure_dirty = true;
            }
            return Ok(true);
        }
        if kind == OperationKind::View
            && let Some(operation) = crate::browse_actions::decode(type_id, payload)?
        {
            match operation {
                crate::browse_actions::BrowseOperation::SetSearch { query } => {
                    if query.chars().count() > 512 {
                        return Err("search query is too long".into());
                    }
                    self.set_query(query);
                }
                crate::browse_actions::BrowseOperation::SetSource { source } => {
                    self.set_active_filter_from_label(&source);
                    if self.active_filter.label() != source {
                        return Err("unknown Store source filter".into());
                    }
                }
                crate::browse_actions::BrowseOperation::SetCategory { category } => {
                    let category = (category != "All").then_some(category);
                    if category.as_ref().is_some_and(|value| {
                        value.chars().count() > 128
                            || !self.category_labels().iter().any(|known| known == value)
                    }) {
                        return Err("unknown Store category".into());
                    }
                    self.set_active_category(category);
                }
                crate::browse_actions::BrowseOperation::Clear => {
                    self.set_query(String::new());
                    self.set_active_filter(SourceFilter::All);
                    self.set_active_category(None);
                }
            }
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "store.view-remotes" {
            let (): () = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            self.remotes_open = true;
            self.safety_open = false;
            self.focused_detail = None;
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new("store:remotes", "refresh"),
                arguments: serde_json::Map::new(),
            });
            self.structure_dirty = true;
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "store.view-trust" {
            let (): () = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            self.safety_open = true;
            self.remotes_open = false;
            self.focused_detail = None;
            self.structure_dirty = true;
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "store.view" {
            let app_id: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            let item = self
                .items
                .iter()
                .find(|item| item.id == app_id)
                .ok_or_else(|| "app is no longer present in the catalogue".to_string())?;
            self.focused_detail = Some(app_id.clone());
            self.pending_detail = Some(app_id);
            self.status_line = format!("Loading details for {}…", item.name);
            self.structure_dirty = true;
            return Ok(true);
        }
        if kind != OperationKind::Effect {
            return Ok(false);
        }
        match type_id {
            "store.refresh" | "store.app" => {
                Err("store operations require the lifecycle-capable operation host".into())
            }
            _ => Ok(false),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single typed effect-dispatch boundary for Store lifecycle, export, remote, and moderation actions"
    )]
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != OperationKind::Effect {
            return Ok(None);
        }
        if type_id == "store.refresh" {
            if self.pending_lifecycle_refresh.is_some() {
                return Err("a catalog refresh is already queued".into());
            }
            self.pending_lifecycle_refresh = Some(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if type_id == "store.report-export" {
            if self.report_export.is_some() {
                return Err("a report export is already in progress".into());
            }
            let envelope: ReportEnvelope =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            if envelope.format != "tilo.store.report-envelope.v1"
                || envelope.app_id.trim().is_empty()
                || envelope.evidence_note.len() > 65_536
            {
                return Err("invalid report evidence envelope".into());
            }
            let bytes = serde_json::to_vec_pretty(&envelope).map_err(|e| e.to_string())?;
            let suggested = format!(
                "{}-report.json",
                envelope.app_id.replace(
                    |c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-',
                    "-"
                )
            );
            let receiver =
                ui::portal::PickRequest::save("Export report evidence", "Export", &suggested)
                    .filter(ui::portal::FileFilter::new("JSON evidence envelope").glob("*.json"))
                    .thread_name("store-report-export")
                    .send();
            self.report_export = Some((invocation, receiver, bytes));
            self.status_line = "Choose where to export the report evidence…".into();
            self.structure_dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if type_id == "store.moderation" {
            if self.pending_moderation.len() >= MAX_PENDING_EXTERNAL_OPERATIONS {
                return Err("too many moderation operations are already queued".into());
            }
            let (registry_name, envelope): (String, ReportEnvelope) =
                serde_json::from_slice(payload)
                    .map_err(|_| "invalid moderation request".to_string())?;
            if registry_name.trim().is_empty()
                || envelope.format != "tilo.store.report-envelope.v1"
                || envelope.app_id.trim().is_empty()
                || envelope.evidence_note.trim().chars().count() < 8
                || envelope.evidence_note.chars().count() > 4_096
            {
                return Err("Add a concern of 8–4096 characters before submitting".into());
            }
            let item = self
                .items
                .iter()
                .find(|item| item.id == envelope.app_id)
                .ok_or_else(|| "the reported app is no longer in this catalogue".to_string())?;
            let StoreSource::Tilo {
                registry_name: current_registry,
            } = &item.source
            else {
                return Err("reports can only be submitted to the app's Tilo registry".into());
            };
            if current_registry != &registry_name {
                return Err("the report target no longer matches this app's registry".into());
            }
            let report = self
                .reports
                .iter()
                .find(|report| report.app_id == envelope.app_id && !report.id.starts_with("case-"))
                .ok_or_else(|| "the current report draft is no longer available".to_string())?;
            // A published action is a precise snapshot, not a bearer token
            // for any app id that happens to be in the request. Rebuild the
            // expected envelope from the collaborative document and reject a
            // stale or substituted submission before it reaches the worker.
            let note = self
                .report_notes
                .get(&report.app_id)
                .map_or("", String::as_str);
            if report_envelope(report, Some(item), note) != envelope {
                return Err(
                    "the report draft changed; review it and submit the current version".into(),
                );
            }
            self.pending_moderation.push((
                ModerationIntent::Submit {
                    registry_name,
                    envelope,
                },
                invocation,
            ));
            self.status_line = "Signing report with your workspace identity…".into();
            self.structure_dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if matches!(
            type_id,
            "store.moderation-status" | "store.moderation-appeal"
        ) {
            let (registry_name, case_id, reason): (String, String, String) =
                serde_json::from_slice(payload)
                    .map_err(|_| "invalid moderation case request".to_string())?;
            if self.pending_moderation.len() >= MAX_PENDING_EXTERNAL_OPERATIONS {
                return Err("too many moderation operations are already queued".into());
            }
            if registry_name.trim().is_empty() || !valid_case_id(&case_id) {
                return Err("moderation case is no longer available".into());
            }
            let report = self
                .reports
                .iter()
                .find(|report| report.id == case_id)
                .ok_or_else(|| "moderation case is no longer available".to_string())?;
            let item = self
                .items
                .iter()
                .find(|item| item.id == report.app_id)
                .ok_or_else(|| "the moderated app is no longer in this catalogue".to_string())?;
            let StoreSource::Tilo {
                registry_name: current_registry,
            } = &item.source
            else {
                return Err("this moderation case no longer has a Tilo registry target".into());
            };
            if current_registry != &registry_name {
                return Err("the moderation case no longer matches this app's registry".into());
            }
            let intent = if type_id == "store.moderation-status" {
                ModerationIntent::Refresh {
                    registry_name,
                    case_id,
                }
            } else {
                if reason.trim().chars().count() < 8 || reason.chars().count() > 8_192 {
                    return Err("Add an appeal reason of 8–8192 characters".into());
                }
                ModerationIntent::Appeal {
                    registry_name,
                    case_id,
                    reason,
                }
            };
            self.pending_moderation.push((intent, invocation));
            self.status_line = "Contacting the authoritative registry…".into();
            self.structure_dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if type_id == "store.remote" {
            if self.pending_lifecycle_remote.len() >= MAX_PENDING_EXTERNAL_OPERATIONS {
                return Err("too many remote operations are already queued".into());
            }
            let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            let intent = match args.first().map(String::as_str) {
                Some("refresh") => RemoteIntent::Refresh,
                Some("remove") => {
                    let name = args
                        .get(1)
                        .ok_or_else(|| "missing remote name".to_string())?;
                    validate_remote_name(name)?;
                    RemoteIntent::Remove(name.clone())
                }
                Some("add") => {
                    let name = args
                        .get(1)
                        .ok_or_else(|| "missing remote name".to_string())?
                        .clone();
                    let url = args
                        .get(2)
                        .ok_or_else(|| "missing repository URL".to_string())?
                        .clone();
                    validate_remote_name(&name)?;
                    validate_remote_url(&url)?;
                    RemoteIntent::Add { name, url }
                }
                _ => return Err("unknown remote action".into()),
            };
            self.pending_lifecycle_remote.push((intent, invocation));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if type_id != "store.app" {
            return Ok(None);
        }
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let id = args
            .first()
            .ok_or_else(|| "missing app id".to_string())?
            .clone();
        let action = action_from_code(args.get(1).map_or("", String::as_str))
            .ok_or_else(|| "unknown store action".to_string())?;
        let item = self
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| "app is no longer present in the catalog".to_string())?;
        if !item.allows_action(action, self.flatpak_available) {
            return Err("this action is no longer available for the current app state".into());
        }
        if action.requires_flatpak_host() && !self.flatpak_available {
            return Err(
                "Flatpak is not installed on this host; no install operation can run".into(),
            );
        }
        if self.active_operation.is_some() {
            return Err("a package operation is already in progress".into());
        }
        self.active_operation = Some((id.clone(), action));
        self.operation_error = None;
        self.status_line = format!("{} {}…", action.progress_label(), item.name);
        self.structure_dirty = true;
        self.pending_lifecycle_actions
            .push((id, action, invocation));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};
    use crate::widgets::{CardAction, CardDetails};
    use ui::search::SearchProvider;
    use ui::widget::CardGrid;

    fn item(name: &str, source: StoreSource, cat: &str, installed: InstalledState) -> StoreItem {
        let trust = match &source {
            StoreSource::Tilo { .. } => StoreTrust::PublisherSigned,
            StoreSource::Flathub => StoreTrust::Flathub,
        };
        StoreItem {
            id: format!("id.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: format!("{name} description"),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![cat.into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source,
            trust,
            versions: vec![StoreItemVersion {
                version: "1".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    fn seeded_app() -> StoreApp {
        let mut app = StoreApp::new();
        app.set_flatpak_available(true);
        app.set_items(vec![
            item(
                "Notes",
                StoreSource::Tilo {
                    registry_name: "Tilo".into(),
                },
                "Productivity",
                InstalledState::Not,
            ),
            item(
                "Terminal",
                StoreSource::Tilo {
                    registry_name: "Tilo".into(),
                },
                "System",
                InstalledState::Installed {
                    version: "1".into(),
                    available: "1".into(),
                    pinned: false,
                    user_managed: true,
                },
            ),
            item("Chess", StoreSource::Flathub, "Games", InstalledState::Not),
        ]);
        app
    }

    /// Source, installed and category filters, search ranking and the category list.
    #[test]
    fn catalogue_filters_and_search_rank_visible_items() {
        // Case `visible_items_show_everything_when_filter_is_all`.
        {
            let app = seeded_app();
            assert_eq!(app.visible_items().len(), 3);
        }

        // Case `filter_tilo_hides_flatpak`.
        {
            let mut app = seeded_app();
            app.set_active_filter(SourceFilter::Tilo);
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert!(names.contains(&"Notes"));
            assert!(names.contains(&"Terminal"));
            assert!(!names.contains(&"Chess"));
        }

        // Case `filter_installed_shows_only_installed`.
        {
            let mut app = seeded_app();
            app.set_active_filter(SourceFilter::Installed);
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(names, vec!["Terminal"]);
        }

        // Case `category_filter_gates_the_list`.
        {
            let mut app = seeded_app();
            app.set_active_category(Some("Games".into()));
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(names, vec!["Chess"]);
        }

        // Case `search_narrows_and_ranks_by_score`.
        {
            let mut app = seeded_app();
            // "Term" only matches Terminal — Notes contains "te" but not
            // "term", so the substring gate drops it.
            app.set_query("Term".into());
            let visible = app.visible_items();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].name, "Terminal");
        }

        // Case `search_ranks_name_prefix_hits_first`.
        {
            let mut app = seeded_app();
            // Both "Notes" (name contains "te" mid-string) and
            // "Terminal" (name starts with "Te") match "te".
            app.set_query("te".into());
            let visible = app.visible_items();
            assert!(visible.len() >= 2);
            assert_eq!(visible[0].name, "Terminal", "prefix hit ranks first");
        }

        // Case `categories_include_all_plus_distinct`.
        {
            let app = seeded_app();
            let cats = app.category_labels();
            assert_eq!(cats[0], "All");
            assert!(cats.iter().any(|c| c == "Productivity"));
            assert!(cats.iter().any(|c| c == "Games"));
            assert!(cats.iter().any(|c| c == "System"));
        }
    }

    /// Resource snapshots come from the worker, stay bounded, and resolve by canonical id.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn resource_snapshots_are_bounded_and_resolve_canonically() {
        // Case `worker_snapshot_does_not_reindex_on_the_ui_thread`.
        {
            let index = crate::search_provider::StoreSearchIndex::default();
            let mut app = StoreApp::new().with_search_index(index.clone());
            let indexed = item(
                "Indexed catalogue row",
                StoreSource::Flathub,
                "Utilities",
                InstalledState::Not,
            );
            let rendered = item(
                "Rendered worker snapshot",
                StoreSource::Flathub,
                "Utilities",
                InstalledState::Not,
            );

            // The worker publishes the retained directory before its snapshot.
            // If `set_items_from_worker` ever silently rebuilt it, this assertion
            // would instead find the rendered row and reintroduce refresh work to
            // the frame path.
            index.sync(&[indexed]);
            app.set_items_from_worker(vec![rendered]);

            assert_eq!(app.items()[0].name, "Rendered worker snapshot");
            let hits = index.provider().search("Indexed catalogue row");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].title, "Indexed catalogue row");
        }

        // Case `contextual_resource_snapshot_is_bounded_and_omitted_apps_resolve_lazily`.
        {
            let mut app = StoreApp::new();
            let mut items = Vec::new();
            for index in 0..(RESOURCE_SNAPSHOT_LIMIT + 17) {
                let mut entry = item(
                    "Utility",
                    StoreSource::Tilo {
                        registry_name: "Tilo".into(),
                    },
                    "Utilities",
                    InstalledState::Not,
                );
                entry.id = format!("dev.example.utility{index:04}");
                entry.name = format!("Utility {index:04}");
                items.push(entry);
            }
            app.set_items(items);
            let doc = AutoCommit::new();
            let root = ui::resources::resources(&app, &doc).pop().unwrap();
            let snapshot_count = root
                .children
                .iter()
                .filter(|resource| resource.kind == "store.publisher")
                .flat_map(|publisher| publisher.children.iter())
                .count();
            assert_eq!(snapshot_count, RESOURCE_SNAPSHOT_LIMIT);
            let eager = "app:dev.example.utility0000";
            let on_tree = root
                .children
                .iter()
                .filter(|resource| resource.kind == "store.publisher")
                .flat_map(|publisher| publisher.children.iter())
                .find(|resource| resource.id.as_str() == eager)
                .expect("the first app is on the published page");
            let looked_up = ui::resources::resource(&app, &doc, eager).unwrap();
            assert_eq!(
                looked_up
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                on_tree
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                "an eager app is the published row"
            );
            app.mark_hosted_for_test();
            let hosted = ui::resources::resource(&app, &doc, eager).unwrap();
            assert_eq!(
                hosted
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                on_tree
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                "a hosted app is the published row"
            );
            assert!(ui::resources::resource(&app, &doc, crate::resource::CATALOGUE_ID).is_none());
            let omitted = "app:dev.example.utility0216";
            let resolved = ui::resources::resource(&app, &doc, omitted).unwrap();
            assert_eq!(resolved.id.0, omitted);
            assert!(
                resolved
                    .actions
                    .iter()
                    .any(|action| action.id == "show-details")
            );
        }

        // Case `every_top_level_remote_and_moderation_resource_resolves_by_canonical_id`.
        {
            let mut app = seeded_app();
            app.set_remotes(vec![crate::flathub_client::Remote {
                name: "flathub".into(),
                url: "https://dl.flathub.org/repo/".into(),
                title: "Flathub".into(),
                disabled: false,
            }]);
            let doc = AutoCommit::new();

            for id in [
                crate::resource::CATALOGUE_ID,
                crate::browse_actions::BROWSE_RESOURCE_ID,
                "store:remotes",
                "remote:flathub",
                "store:moderation",
            ] {
                assert!(
                    ui::resources::resource(&app, &doc, id).is_some(),
                    "published resource {id} must remain directly resolvable"
                );
            }
            let remote = ui::resources::resource(&app, &doc, "remote:flathub").unwrap();
            assert!(remote.actions.iter().any(|action| action.id == "remove"));
            let browse =
                ui::resources::resource(&app, &doc, crate::browse_actions::BROWSE_RESOURCE_ID)
                    .unwrap();
            assert!(
                browse
                    .actions
                    .iter()
                    .all(|action| action.kind == OperationKind::View)
            );
            assert!(
                browse
                    .actions
                    .iter()
                    .any(|action| action.id == "set-search")
            );
        }

        // Case `app_resources_include_bounded_catalogue_context_for_nonvisual_clients`.
        {
            let app = seeded_app();
            let item = app.items.iter().find(|item| item.id == "id.notes").unwrap();
            let resource = store_app_resource(item, true);
            let presentation = resource
                .presentation
                .expect("app resource has a search and history summary");
            assert_eq!(presentation.badge.as_deref(), Some("Tilo verified, v1"));
            assert!(
                presentation
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("Notes description"))
            );
        }
    }

    /// Remote configuration rejects hostile input and unsubmitted drafts stay on their device.
    #[test]
    fn remote_configuration_is_validated_and_drafts_stay_local() {
        // Case `remote_configuration_rejects_hostile_sizes_and_control_characters`.
        {
            assert!(validate_remote_name("flathub").is_ok());
            assert!(validate_remote_name(&"r".repeat(MAX_REMOTE_NAME_CHARS + 1)).is_err());
            assert!(validate_remote_name("bad\nname").is_err());
            assert!(validate_remote_url("https://dl.flathub.org/repo/").is_ok());
            assert!(
                validate_remote_url(&format!(
                    "https://example.invalid/{}",
                    "a".repeat(MAX_REMOTE_URL_BYTES)
                ))
                .is_err()
            );
            assert!(valid_case_id("case-authoritative_1"));
            assert!(!valid_case_id("case-../../escape"));
            assert!(!valid_case_id(&format!(
                "case-{}",
                "x".repeat(MAX_CASE_ID_CHARS)
            )));
        }

        // Case `unsubmitted_remote_draft_stays_on_its_device`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            app.remote_name_state.set("studio-tools".into());
            app.remote_url_state
                .set("https://apps.tilo.so/artifacts/repo".into());
            app.sync(&mut doc, &automerge::ROOT);
            assert!(doc.keys(&automerge::ROOT).all(|key| {
                !key.starts_with("store.remote_name") && !key.starts_with("store.remote_url")
            }));
        }
    }

    /// Device, heading and source-summary copy tell the truth about what is loaded.
    #[test]
    fn catalogue_copy_is_honest_about_sources_and_queries() {
        // Case `device_copy_distinguishes_tilo_on_demand_from_flatpak_installation`.
        {
            let tilo = item(
                "Notes",
                StoreSource::Tilo {
                    registry_name: "Tilo".into(),
                },
                "Productivity",
                InstalledState::Not,
            );
            let flatpak = item("Chess", StoreSource::Flathub, "Games", InstalledState::Not);
            assert_eq!(
                view::device_properties(&tilo),
                vec![
                    ("On this device", "Available on demand".to_string()),
                    (
                        "Storage",
                        "Fetched when opened, not kept on this device".to_string()
                    ),
                ]
            );
            assert_eq!(
                view::device_properties(&flatpak),
                vec![("On this device", "Not installed".to_string())]
            );
            assert_eq!(tilo.action_kind(true).label(), "Open");
            assert_eq!(flatpak.action_kind(true).label(), "Install");
        }

        // Case `availability_groups_give_way_to_one_ranked_list_while_searching`.
        {
            let mut app = seeded_app();
            assert!(app.group_catalogue_by_availability());
            app.set_query("notes".into());
            assert!(
                !app.group_catalogue_by_availability(),
                "search results remain a single relevance-ranked sequence"
            );
        }

        // Case `catalogue_source_summary_is_honest_about_loaded_sources`.
        {
            let app = seeded_app();
            assert_eq!(app.catalogue_source_summary(3), "3 apps from 2 sources");

            let empty = StoreApp::new();
            assert_eq!(empty.catalogue_source_summary(0), "No apps loaded yet");

            let mut filtered = seeded_app();
            filtered.set_active_filter(SourceFilter::Tilo);
            assert_eq!(filtered.catalogue_source_summary(2), "Showing 2 of 3 apps");
        }
    }

    /// Availability groups, narrow cards, medium summaries and the narrow discover section.
    #[test]
    fn catalogue_grids_keep_cards_apart_at_narrow_and_medium_widths() {
        // Case `narrow_cards_do_not_cover_each_others_actions`.
        {
            fn check(widget: &dyn Widget) {
                if widget.name() == "Card" {
                    let mut buttons = Vec::new();
                    named_rects(widget, "Button", &mut buttons);
                    for button in buttons {
                        assert!(
                            button.y1 <= widget.get_rect().y1 - 8.0,
                            "action {button:?} outside {:?}",
                            widget.get_rect()
                        );
                    }
                }
                for child in widget.children() {
                    check(child);
                }
            }
            let mut app = seeded_app();
            app.items.extend(app.items.clone());
            for (i, item) in app.items.iter_mut().enumerate() {
                item.id = format!("test-{i}");
                item.description =
                    "Useful software with a description that wraps onto another line.".into();
            }
            let list = frame_composition(app.compose(&Theme::dark()), 480.0);
            check(list.root_widget());
        }

        // Case `catalogue_states_its_summary_once_and_never_titles_the_window`.
        {
            let app = seeded_app();
            let tree = app.compose(&Theme::dark());
            let summary = app.catalogue_source_summary(app.visible_items().len());
            assert_eq!(accessible_name_count(tree.as_ref(), &summary), 1);
            assert_eq!(
                accessible_name_count(tree.as_ref(), "App Store"),
                0,
                "the operating system names the window, not the page"
            );
        }

        // Case `first_empty_busy_catalogue_is_real_chrome_not_a_flat_fill`.
        {
            let mut app = StoreApp::new();
            app.set_worker_busy(true);
            let tree = app.compose(&Theme::dark());
            assert_eq!(accessible_name_count(tree.as_ref(), "Loading apps"), 1);
        }

        // Case `full_narrow_catalogue_keeps_discover_section_below_ready_cards`.
        {
            let mut app = seeded_app();
            app.set_items(crate::shots::fixture_catalog());
            let frame = frame_composition(app.compose(&Theme::dark()), 480.0);
            let tree = frame.root_widget();

            let discover =
                accessible_rect(tree, "Discover").expect("discover heading in full catalogue");
            let mut grids = Vec::new();
            card_grid_extents(tree, &mut grids);
            assert_eq!(grids.len(), 3, "updates, on this device and discover grids");
            assert!(
                grids[1].1 <= discover.y0,
                "device descendants end at {}, over discover heading {:?}; allocated grid {:?}",
                grids[1].1,
                discover,
                grids[1].0
            );
            assert!(
                discover.y1 <= grids[2].0.y0,
                "discover heading {:?} must precede discover grid {:?}",
                discover,
                grids[2].0
            );
        }
    }

    /// A catalogue longer than one page offers Previous and Next only where they lead somewhere.
    #[test]
    fn a_long_catalogue_pages_with_previous_and_next() {
        let mut app = StoreApp::new();
        let items = (0..(PAGE_SIZE * 2 + 5))
            .map(|index| {
                let mut entry = item(
                    "Utility",
                    StoreSource::Flathub,
                    "Utilities",
                    InstalledState::Not,
                );
                entry.id = format!("dev.example.utility{index:03}");
                entry.name = format!("Utility {index:03}");
                entry
            })
            .collect();
        app.set_items(items);
        let has = |app: &StoreApp, id: &str| {
            let frame = frame_composition(app.compose(&Theme::dark()), 800.0);
            has_test_id(frame.root_widget(), id)
        };
        assert!(has(&app, "store.page.next"));
        assert!(!has(&app, "store.page.previous"));
        app.catalog_page = 1;
        assert!(has(&app, "store.page.next") && has(&app, "store.page.previous"));
        app.catalog_page = 2;
        assert!(!has(&app, "store.page.next") && has(&app, "store.page.previous"));
    }

    fn has_test_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id)
            || widget
                .children()
                .into_iter()
                .any(|child| has_test_id(child, id))
    }

    /// Mount a composed tree and run one frame at `width` × 900.
    fn frame_composition(widget: Box<dyn Widget>, width: f32) -> ui::headless::FrameHarness {
        ui::headless::FrameHarness::new(
            ui::State::new(),
            widget,
            ui::headless::viewport((width, 900.0)),
        )
    }

    /// The `CardGrid` control test drains `take_control_invocation`, which the
    /// harness's control custody would take first, so it lays out by hand.
    fn layout_card_grid(widget: &mut CardGrid, width: f32) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::new();
        let mut map = HashMap::new();
        let node = widget.build_layout(&mut tree, &Theme::dark(), &mut map);
        let root = tree
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(width),
                        height: taffy::Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let mut text = TextRenderer::new();
        tree.compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(900.0),
            },
            |known, available, id, _, _| {
                map.get(&id).map_or(taffy::Size::ZERO, |ptr| unsafe {
                    (**ptr).measure(known, available, &mut text)
                })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, node, Point::ZERO);
        (tree, node)
    }

    fn named_rects(widget: &dyn Widget, name: &str, out: &mut Vec<Rect>) {
        if widget.name() == name {
            out.push(widget.get_rect());
        }
        for child in widget.children() {
            named_rects(child, name, out);
        }
    }

    fn accessible_rect(widget: &dyn Widget, name: &str) -> Option<Rect> {
        if widget
            .accessibility()
            .as_ref()
            .and_then(|node| node.name.as_deref())
            == Some(name)
        {
            return Some(widget.get_rect());
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| accessible_rect(child, name))
    }

    fn accessible_name_count(widget: &dyn Widget, name: &str) -> usize {
        let own = usize::from(
            widget
                .accessibility()
                .as_ref()
                .and_then(|node| node.name.as_deref())
                == Some(name),
        );
        own + widget
            .children()
            .into_iter()
            .map(|child| accessible_name_count(child, name))
            .sum::<usize>()
    }

    fn descendant_bottom(widget: &dyn Widget) -> f64 {
        widget
            .children()
            .into_iter()
            .map(descendant_bottom)
            .fold(widget.get_rect().y1, f64::max)
    }

    fn card_grid_extents(widget: &dyn Widget, out: &mut Vec<(Rect, f64)>) {
        if widget.name() == "CardGrid" {
            out.push((widget.get_rect(), descendant_bottom(widget)));
            return;
        }
        for child in widget.children() {
            card_grid_extents(child, out);
        }
    }

    fn assert_finite_rects(widget: &dyn Widget) {
        let rect = widget.get_rect();
        assert!(
            [rect.x0, rect.y0, rect.x1, rect.y1]
                .into_iter()
                .all(f64::is_finite),
            "{} has non-finite geometry: {rect:?}",
            widget.name()
        );
        for child in widget.children() {
            assert_finite_rects(child);
        }
    }

    fn control_mut<'a>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        for child in widget.children_mut() {
            if let Some(found) = control_mut(child, id) {
                return Some(found);
            }
        }
        None
    }

    /// Card pointer/primary activation and detail/remote controls use typed local actions.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn cards_and_detail_controls_keep_typed_local_bindings() {
        // Case `card_pointer_details_and_primary_activation_keep_typed_bindings`.
        {
            use winit::event::WindowEvent;
            for width in [480.0, 1200.0] {
                let app = seeded_app();
                let item = &app.items[1];
                let action = item.action_kind(true);
                for busy in [false, true] {
                    let card = crate::widgets::store_card(
                        item,
                        CardAction {
                            kind: action,
                            key: "primary",
                            binding: item_action_binding(item, action),
                            in_progress: busy,
                        },
                        CardDetails {
                            key: "select",
                            binding: detail_binding(item),
                        },
                        None,
                        &Theme::dark(),
                    );
                    let mut grid = CardGrid::new(vec![card]);
                    let (tree, node) = layout_card_grid(&mut grid, width);
                    let center = control_mut(&mut grid, "select")
                        .unwrap()
                        .get_rect()
                        .center();
                    let mut doc = AutoCommit::new();
                    let mut focus = None;
                    let cursor = WindowEvent::CursorMoved {
                        device_id: unsafe { std::mem::zeroed() },
                        position: winit::dpi::PhysicalPosition::new(center.x, center.y),
                    };
                    let events = [
                        cursor,
                        WindowEvent::MouseInput {
                            device_id: unsafe { std::mem::zeroed() },
                            state: winit::event::ElementState::Pressed,
                            button: winit::event::MouseButton::Left,
                        },
                        WindowEvent::MouseInput {
                            device_id: unsafe { std::mem::zeroed() },
                            state: winit::event::ElementState::Released,
                            button: winit::event::MouseButton::Left,
                        },
                    ];
                    for event in events {
                        grid.handle_event(
                            &event,
                            &mut ui::widget::EventCtx {
                                scale_factor: 1.0,
                                modifiers: &winit::event::Modifiers::default(),
                                state: &mut doc,
                                path: &automerge::ROOT,
                                handled: &mut false,
                                focus_target: &mut focus,
                                node_id: node,
                                taffy: &tree,
                            },
                        );
                    }
                    let details = ui::widget::controls::take_control_invocation(&mut grid)
                        .expect("pressing the card shows its details");
                    assert_eq!(details.address.resource, format!("app:{}", item.id));
                    assert_eq!(details.address.action, "show-details");
                    let primary = control_mut(&mut grid, "primary").unwrap();
                    assert_eq!(primary.is_enabled(), !busy);
                    assert_eq!(
                        primary.perform_action(
                            &ui::a11y::Action::Activate,
                            &mut doc,
                            &automerge::ROOT
                        ),
                        !busy
                    );
                    let invoked = ui::widget::controls::take_control_invocation(&mut grid);
                    if busy {
                        assert!(invoked.is_none());
                    } else {
                        assert_eq!(invoked.unwrap().address.action, action_code(action));
                    }
                }
            }
        }

        // Case `detail_and_remote_controls_use_typed_local_actions`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::View,
                    "store.view",
                    &serde_json::to_vec(&"id.chess").unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
            assert_eq!(app.take_detail_requested().as_deref(), Some("id.chess"));
            assert!(app.take_detail_requested().is_none());

            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::View,
                    "store.view-remotes",
                    &serde_json::to_vec(&()).unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
            let intent = ui::widget::controls::take_control_invocation(&mut app)
                .expect("refresh action intent");
            assert_eq!(intent.address.resource, "store:remotes");
            assert_eq!(intent.address.action, "refresh");
        }
    }

    /// Stacked geometry, contained actions, the three-column measure and the large-text layout.
    #[test]
    fn catalogue_measures_stay_finite_contained_and_stacked_under_large_text() {
        // Case `full_catalogue_geometry_stays_finite_at_every_width_and_text_scale`.
        {
            for (width, scale) in [(480.0, 1.0), (1_280.0, 2.0)] {
                let mut app = seeded_app();
                app.set_items(crate::shots::fixture_catalog());
                let frame =
                    frame_composition(app.compose(&Theme::dark().with_text_scale(scale)), width);
                assert_finite_rects(frame.root_widget());
            }
        }

        // Case `catalogue_cards_contain_actions_and_align_long_descriptions`.
        {
            fn check(widget: &dyn Widget, found: &mut usize) {
                if widget.name() == "CardGrid" {
                    let cards = widget.children();
                    let mut bottoms = Vec::new();
                    for card in cards {
                        let mut buttons = Vec::new();
                        named_rects(card, "Button", &mut buttons);
                        for button in &buttons {
                            assert!(
                                button.y1 <= card.get_rect().y1 - 8.0,
                                "button {button:?} must fit padded card {:?}",
                                card.get_rect()
                            );
                            assert!(
                                button.x0 >= card.get_rect().x0 && button.x1 <= card.get_rect().x1
                            );
                        }
                        bottoms.push(buttons.last().unwrap().y1);
                        *found += 1;
                    }
                    for bottom in &bottoms {
                        assert!((bottom - bottoms[0]).abs() < 1.0);
                    }
                }
                for child in widget.children() {
                    check(child, found);
                }
            }
            let mut app = seeded_app();
            app.items[0].description = "A useful app with a longer description. ".repeat(7);
            let list = frame_composition(app.build_list(&Theme::dark()), 1200.0);
            let mut found = 0;
            check(list.root_widget(), &mut found);
            assert_eq!(found, 3);
        }
    }

    /// Operation notices, root accessibility, trust summary and bounded, printable live status.
    #[test]
    fn status_and_accessibility_surfaces_are_named_bounded_live_regions() {
        // Case `operation_notice_uses_shared_live_banner_semantics`.
        {
            fn banner(widget: &dyn Widget) -> Option<AccessNode> {
                if let Some(node) = widget.accessibility()
                    && node.name.as_deref() == Some("Working in background")
                {
                    return Some(node);
                }
                widget.children().into_iter().find_map(banner)
            }
            let mut app = seeded_app();
            assert!(app.operation_notice(&Theme::dark()).is_none());

            app.set_status_line("Installing Notes…");
            app.set_worker_busy(true);
            let busy_notice = app
                .operation_notice(&Theme::dark())
                .expect("busy work has a semantic notice");
            let busy = banner(&*busy_notice)
                .expect("busy banner remains a live child of the progress notice");
            assert_eq!(busy.live, Some(Politeness::Polite));
            assert_eq!(busy.description.as_deref(), Some("Installing Notes…"));

            app.set_worker_busy(false);
            app.set_operation_error(Some("signature mismatch".into()));
            let error = app
                .operation_notice(&Theme::dark())
                .and_then(|notice| notice.accessibility())
                .expect("failure has a semantic notice");
            assert_eq!(error.name.as_deref(), Some("The last operation failed"));
            assert_eq!(error.live, Some(Politeness::Assertive));
        }

        // Case `root_accessibility_is_a_named_window_not_an_empty_generic`.
        {
            let mut app = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            let node = app.accessibility().expect("store exports a root node");
            assert_eq!(node.role, Role::Window);
            assert_eq!(node.name.as_deref(), Some("App Store"));
            assert!(node.bounds.is_some());
        }

        // Case `publisher_identity_reads_as_a_short_fingerprint_with_the_full_key_kept`.
        {
            let mut signed = item(
                "Signed",
                StoreSource::Tilo {
                    registry_name: "Tilo".into(),
                },
                "Utilities",
                InstalledState::Not,
            );
            signed.publisher_pubkey_hex = Some("ab".repeat(32));
            assert_eq!(short_publisher(&signed), "ed25519:abababababab…");
            assert_eq!(
                full_publisher(&signed),
                format!("ed25519:{}", "ab".repeat(32))
            );
        }

        // Case `visible_operation_status_is_printable_and_bounded`.
        {
            assert_eq!(visible_operation_status("\n\t"), UNKNOWN_OPERATION_STATUS);
            assert_eq!(
                visible_operation_status("Install failed\n\tpackage signature did not match"),
                "Install failed package signature did not match"
            );
            let long = "x".repeat(MAX_VISIBLE_OPERATION_STATUS_CHARS + 1);
            let visible = visible_operation_status(&long);
            assert_eq!(
                visible.chars().count(),
                MAX_VISIBLE_OPERATION_STATUS_CHARS + 1
            );
            assert!(visible.ends_with('…'));
        }

        // Case `resource_detail_excerpt_is_printable_and_byte_bounded`.
        {
            assert_eq!(resource_detail_excerpt(" one\n\ttwo  "), "one two");
            let detail = resource_detail_excerpt(&format!("{}\nignored", "界".repeat(200)));
            assert!(detail.len() <= MAX_RESOURCE_DETAIL_BYTES);
            assert!(detail.ends_with('…'));
            assert!(!detail.contains('\n'));
        }

        // Case `operation_status_is_exposed_as_a_polite_live_region`.
        {
            let mut app = seeded_app();
            app.set_status_line("Installing Notes…");
            let node = app.accessibility().expect("store exports a root node");
            let status = node.children.first().expect("status live region");
            assert_eq!(status.name.as_deref(), Some("Installing Notes…"));
            assert_eq!(status.live, Some(Politeness::Polite));
        }
    }

    /// Update priority, refresh and app operations run as lifecycle-scoped background jobs.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn refresh_and_operations_are_lifecycle_scoped_worker_jobs() {
        // Case `update_priority_follows_the_current_catalogue_snapshot`.
        {
            let mut app = seeded_app();
            let mut files = item(
                "Files",
                StoreSource::Tilo {
                    registry_name: "Tilo".into(),
                },
                "System",
                InstalledState::Installed {
                    version: "1.0.0".into(),
                    available: "1.1.0".into(),
                    pinned: false,
                    user_managed: true,
                },
            );
            files.versions[0].version = "1.1.0".into();
            app.set_items({
                let mut items = app.items().to_vec();
                items.push(files);
                items
            });
            assert!(
                app.visible_items()
                    .iter()
                    .any(|item| item.installed.update_available()),
                "the current catalogue exposes the verified update candidate"
            );

            app.set_active_filter(SourceFilter::Flatpak);
            assert!(
                app.visible_items()
                    .iter()
                    .all(|item| !item.installed.update_available()),
                "the update shelf must be derived from the active visible catalogue"
            );
        }

        // Case `refresh_action_is_lifecycle_scoped_and_does_not_write_the_document`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let heads = doc.get_heads();
            let invocation = tilo_actions::InvocationId::new();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        "store.refresh",
                        &serde_json::to_vec(&"").unwrap()
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert_eq!(app.take_lifecycle_refresh(), Some(invocation));
            assert_eq!(doc.get_heads(), heads);
        }

        // Case `exported_refresh_is_accepted_until_worker_completion`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let invocation = tilo_actions::InvocationId::new();
            let started = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        "store.refresh",
                        &serde_json::to_vec(&"").unwrap(),
                    ),
                    &mut doc,
                    &automerge::ROOT,
                )
                .unwrap();
            assert_eq!(started, Some(ui::widget::ResourceOperationStart::Accepted));
            assert_eq!(app.take_lifecycle_refresh(), Some(invocation));
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none()
            );
            app.complete_lifecycle_invocation(invocation, None);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // Case `app_operation_locks_the_target_card_until_the_worker_is_idle`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let payload = serde_json::to_vec(&vec!["id.notes", "open"]).unwrap();
            let first = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "store.app",
                        &payload,
                    ),
                    &mut doc,
                    &automerge::ROOT,
                )
                .unwrap();
            assert_eq!(first, Some(ui::widget::ResourceOperationStart::Accepted));
            assert_eq!(
                app.active_operation,
                Some(("id.notes".into(), ActionKind::Open))
            );
            assert_eq!(app.status_line(), "Starting Notes…");
            assert_eq!(app.take_lifecycle_actions().len(), 1);
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "store.app",
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err(),
                "the card action cannot double-submit while the worker owns it"
            );
            app.set_worker_busy(true);
            app.set_worker_busy(false);
            assert!(app.active_operation.is_none());
        }

        // Case `exported_store_effects_are_background_jobs`.
        {
            let app = seeded_app();
            let doc = AutoCommit::new();
            for action in ui::resources::resources(&app, &doc)
                .iter()
                .flat_map(|resource| {
                    resource.actions.iter().chain(
                        resource
                            .children
                            .iter()
                            .flat_map(|child| child.actions.iter()),
                    )
                })
            {
                if action.kind != OperationKind::Effect {
                    continue;
                }
                assert!(matches!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: false }
                ));
                assert!(matches!(
                    action.contract.result,
                    ResultSpec::Job | ResultSpec::Resource { .. }
                ));
            }
        }
    }

    /// The live store starts empty and its search/source/category controls stay local to the window.
    #[test]
    fn local_browse_controls_filter_without_document_writes() {
        // Case `live_constructor_starts_empty`.
        {
            // The live app must show a real (empty) catalog until the
            // refresh worker streams one — never a fabricated one.
            let app = StoreApp::new();
            assert!(
                app.items().is_empty(),
                "live StoreApp must not fabricate a catalog"
            );
            assert_eq!(app.active_filter(), SourceFilter::All);
            assert!(app.query().is_empty());
        }

        // Case `search_query_filters_the_catalogue_and_reaches_the_worker`.
        {
            let mut app = seeded_app();
            app.set_query("Term".into());
            assert_eq!(app.query(), "Term");
            assert_eq!(app.take_search_requested().as_deref(), Some("Term"));
            assert!(app.take_search_requested().is_none());
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(names, vec!["Terminal"]);
        }

        // Case `source_filter_re_filters_the_catalogue`.
        {
            let mut app = seeded_app();
            app.set_active_filter_from_label("Tilo");
            assert_eq!(app.active_filter(), SourceFilter::Tilo);
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert!(names.contains(&"Notes"));
            assert!(!names.contains(&"Chess"), "Tilo filter hides Flatpak items");
        }

        // Case `category_selection_is_semantic_and_resets_the_result_position`.
        {
            let mut app = seeded_app();
            app.scroll_offset.set((0.0, 240.0));
            app.set_active_category(Some("Games".into()));
            assert_eq!(app.active_category(), Some("Games"));
            assert_eq!(app.scroll_offset.get(), (0.0, 0.0));
            assert_eq!(app.visible_items()[0].name, "Chess");
        }

        // Case `one_window_cannot_navigate_another_with_document_filters`.
        {
            let mut first = seeded_app();
            let mut second = seeded_app();
            let mut doc = AutoCommit::new();
            first.set_active_filter(SourceFilter::Tilo);
            first.set_active_category(Some("Games".into()));
            first.sync(&mut doc, &automerge::ROOT);
            second.sync(&mut doc, &automerge::ROOT);
            assert_eq!(first.active_filter(), SourceFilter::Tilo);
            assert_eq!(first.active_category(), Some("Games"));
            assert_eq!(second.active_filter(), SourceFilter::All);
            assert_eq!(second.active_category(), None);
            assert!(
                doc.keys(&automerge::ROOT)
                    .all(|key| !key.starts_with("store.search")
                        && !key.starts_with("store.source_filter")
                        && !key.starts_with("store.category_filter"))
            );
        }

        // Case `agent_browse_operations_match_local_controls_without_document_mutation`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let heads = doc.get_heads();
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::browse_actions::STORE_BROWSE_OPERATION_TYPE,
                &crate::browse_actions::BrowseOperation::SetSource {
                    source: "Tilo".into(),
                },
            )
            .unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.active_filter(), SourceFilter::Tilo);
            assert!(
                app.visible_items()
                    .iter()
                    .all(|item| matches!(item.source, StoreSource::Tilo { .. }))
            );
            assert_eq!(
                doc.get_heads(),
                heads,
                "local browse must not change document heads"
            );
        }
    }

    /// Stale or host-less Flatpak actions are unavailable/refused and removal uses its exact address.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn flatpak_and_stale_actions_are_refused_without_escalation() {
        // Case `stale_resource_action_cannot_escalate_to_an_unoffered_mutation`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let result = app.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    "store.app",
                    &serde_json::to_vec(&vec!["id.notes", "remove"]).unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            );
            assert!(
                result.is_err(),
                "an uninstalled app publishes Open, not an arbitrary stale Remove effect"
            );
            assert!(app.take_lifecycle_actions().is_empty());
        }

        // Case `flatpak_actions_are_explicitly_unavailable_without_a_flatpak_host`.
        {
            let mut app = seeded_app();
            app.set_flatpak_available(false);
            let doc = AutoCommit::new();
            let root = ui::resources::resources(&app, &doc).pop().unwrap();
            let chess = root
                .children
                .iter()
                .flat_map(|publisher| publisher.children.iter())
                .find(|resource| resource.id.0 == "app:id.chess")
                .unwrap();
            let install = chess
                .actions
                .iter()
                .find(|action| action.id == "flatpak-setup")
                .unwrap();
            assert!(matches!(
                install.contract.availability,
                Availability::Unavailable { ref code, .. } if code == "flatpak_host_unavailable"
            ));
        }

        // Case `installed_flatpak_remove_is_unavailable_and_refused_without_host`.
        {
            let mut app = seeded_app();
            app.items
                .iter_mut()
                .find(|item| item.id == "id.chess")
                .unwrap()
                .installed = InstalledState::Installed {
                version: "1".into(),
                available: "1".into(),
                pinned: false,
                user_managed: true,
            };
            app.set_flatpak_available(false);
            let mut doc = AutoCommit::new();
            let root = ui::resources::resources(&app, &doc).pop().unwrap();
            let chess = root
                .children
                .iter()
                .flat_map(|publisher| publisher.children.iter())
                .find(|resource| resource.id.0 == "app:id.chess")
                .unwrap();
            for id in ["flatpak-setup", "flatpak-remove"] {
                let action = chess.actions.iter().find(|action| action.id == id).unwrap();
                assert!(matches!(
                    action.contract.availability,
                    Availability::Unavailable { ref code, .. } if code == "flatpak_host_unavailable"
                ));
            }
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "store.app",
                        &serde_json::to_vec(&vec!["id.chess", "flatpak-remove"]).unwrap()
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err(),
                "direct/stale invocation cannot bypass the unavailable resource contract"
            );
        }

        // Case `flatpak_remove_uses_its_exact_published_action_address`.
        {
            let mut app = seeded_app();
            app.items
                .iter_mut()
                .find(|item| item.id == "id.chess")
                .unwrap()
                .installed = InstalledState::Installed {
                version: "1".into(),
                available: "1".into(),
                pinned: false,
                user_managed: true,
            };
            let resource = store_app_resource(
                app.items.iter().find(|item| item.id == "id.chess").unwrap(),
                true,
            );
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "flatpak-remove")
                .expect("Flatpak remove action is published at its concrete address");
            assert_eq!(remove.contract.confirmation, ConfirmationPolicy::Confirm);
            assert!(
                item_action_binding(
                    app.items.iter().find(|item| item.id == "id.chess").unwrap(),
                    ActionKind::FlatpakRemove,
                )
                .is_some()
            );
        }
    }

    /// Moderation effects wait for authoritative cases, stay bounded and refuse stale targets.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn moderation_effects_are_authoritative_bounded_and_target_checked() {
        // Case `moderation_effect_waits_for_authoritative_case_before_materializing_status`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let invocation = tilo_actions::InvocationId::new();
            app.reports.push(ReportRecord {
                id: "report:id.notes".into(),
                app_id: "id.notes".into(),
                status: "Not submitted".into(),
            });
            app.report_notes.insert(
                "id.notes".into(),
                "The app connected to a remote host while offline mode was active.".into(),
            );
            let note = app.report_notes.get("id.notes").unwrap();
            let envelope = report_envelope(
                app.reports.first().unwrap(),
                app.items.iter().find(|item| item.id == "id.notes"),
                note,
            );
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        "store.moderation",
                        &serde_json::to_vec(&("Tilo", &envelope)).unwrap()
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            let pending = app.take_moderation_requests();
            assert!(
                matches!(pending.as_slice(), [(ModerationIntent::Submit { .. }, id)] if *id == invocation)
            );
            assert!(
                doc.get(&automerge::ROOT, "store.report_case.id.notes")
                    .unwrap()
                    .is_none()
            );

            let case = registry_types::ModerationCase {
                case_id: "case-authoritative".into(),
                package_id: "id.notes".into(),
                publisher_pubkey_hex: "aa".repeat(32),
                status: registry_types::ModerationCaseStatus::Open,
                opened_at_unix: 10,
                updated_at_unix: 10,
                latest_sequence: 1,
            };
            app.set_moderation_case(&mut doc, &automerge::ROOT, &case);
            assert_eq!(
                doc.get(&automerge::ROOT, "store.report_case.id.notes")
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)),
                Some("case-authoritative".into())
            );
        }

        // Case `invalid_authoritative_case_id_never_enters_the_document`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let case = registry_types::ModerationCase {
                case_id: "case-../../outside".into(),
                package_id: "id.notes".into(),
                publisher_pubkey_hex: "aa".repeat(32),
                status: registry_types::ModerationCaseStatus::Open,
                opened_at_unix: 10,
                updated_at_unix: 10,
                latest_sequence: 1,
            };

            app.set_moderation_case(&mut doc, &automerge::ROOT, &case);

            assert!(
                doc.get(&automerge::ROOT, "store.report_case.id.notes")
                    .unwrap()
                    .is_none()
            );
            assert!(app.operation_error.is_some());
        }

        // Case `remote_effect_queue_is_bounded`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let payload = serde_json::to_vec(&vec!["refresh".to_string()]).unwrap();
            for _ in 0..MAX_PENDING_EXTERNAL_OPERATIONS {
                assert!(
                    app.start_operation(
                        &ui::widget::ResourceOperation::local(
                            tilo_actions::InvocationId::new(),
                            OperationKind::Effect,
                            "store.remote",
                            &payload
                        ),
                        &mut doc,
                        &automerge::ROOT
                    )
                    .is_ok()
                );
            }
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "store.remote",
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
        }

        // Case `moderation_submission_refuses_a_stale_or_substituted_report_target`.
        {
            let mut app = seeded_app();
            app.reports.push(ReportRecord {
                id: "report:id.notes".into(),
                app_id: "id.notes".into(),
                status: "Not submitted".into(),
            });
            app.report_notes.insert(
                "id.notes".into(),
                "The listing omitted a network dependency.".into(),
            );
            let note = app.report_notes.get("id.notes").unwrap();
            let current = report_envelope(
                app.reports.first().unwrap(),
                app.items.iter().find(|item| item.id == "id.notes"),
                note,
            );
            let mut doc = AutoCommit::new();
            let accepted = app.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    "store.moderation",
                    &serde_json::to_vec(&("Tilo", &current)).unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            );
            assert!(accepted.is_ok());
            app.take_moderation_requests();

            let mut stale = current;
            stale.evidence_note = "Different text that was never saved in this draft.".into();
            let rejected = app.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    "store.moderation",
                    &serde_json::to_vec(&("Tilo", stale)).unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            );
            assert!(rejected.is_err());
            assert!(app.take_moderation_requests().is_empty());
        }

        // Case `removed_collaborative_report_is_not_kept_as_a_stale_local_target`.
        {
            use automerge::transaction::Transactable;

            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            let key = format!("{REPORT_RESOURCE_KEY_PREFIX}id.notes");
            doc.put(&automerge::ROOT, &key, "Not submitted").unwrap();
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.reports().len(), 1);
            // A peer deletes the final shared draft. Once the report collection
            // has been hydrated from the document, the empty state is authoritative.
            doc.delete(&automerge::ROOT, &key).unwrap();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.reports().is_empty());
        }

        // Case `every_published_moderation_effect_has_a_local_operation_handler`.
        {
            let app = seeded_app();
            let handlers = Widget::local_resource_operation_handlers(&app);
            for type_id in [
                "store.moderation",
                "store.moderation-status",
                "store.moderation-appeal",
            ] {
                assert!(
                    handlers.contains(&type_id),
                    "published {type_id} action must be callable by agents and collaboration"
                );
            }
        }
    }
}
