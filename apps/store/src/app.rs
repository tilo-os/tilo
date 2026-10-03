//! `StoreApp` — the root widget.
//!
//! Composition sketch:
//!
//! ```text
//! ┌ Store header (search box + refresh)  ─────────────────────────┐
//! │ [    Search…                       ] [ Refresh ]              │
//! ├───────────────────────────────────────────────────────────────┤
//! │ [ All | Tilo | Flatpak | Installed ]      ← filter chip row    │
//! ├───────────────────────────────────────────────────────────────┤
//! │ Categories: [ All | Games | Productivity | System | Utils ]   │
//! ├───────────────────────────────────────────────────────────────┤
//! │  ┌───────────────────────────────────────────────────────┐    │
//! │  │  [icon]  Notes                v0.1.0 · Tilo main [Add] │    │
//! │  │  [ ⎔  ]  A CRDT notes app                             │    │
//! │  └───────────────────────────────────────────────────────┘    │
//! │  ... more cards, spanning to the window bottom ...            │
//! │                                    ╭─────────────────────────╮ │
//! │                                    │ Installing so.tilo.notes…│ │  ← transient bubble
//! └────────────────────────────────────╰─────────────────────────╯─┘
//! ```
//!
//! There is no permanent footer strip: the catalog counts (`N apps · M
//! shown`) and the running action status surface in a floating
//! [`ui::StatusBubble`], revealed on a change or on hover of the
//! bottom-right band, then faded — so the card list reclaims the height.
//!
//! The widget is intentionally read-mostly: the source-of-truth for
//! the item list is refreshed by an outside "refresh" call
//! ([`StoreApp::set_items`]) rather than on every frame, so a shot
//! or a test can inject a deterministic catalog without spinning
//! up an HTTP server.

use std::cell::Cell;
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
use ui::widget::{
    Badge, BadgeTone, Banner, CardGrid, Column, Container, Dropdown, EmptyState, ProgressBar, Row,
    ScrollAxis, Scrollable, Spacer, Text, TextInput, Widget,
};
use ui::{
    Theme,
    icons::Icon,
    permissions::PermissionContext,
    presence::PresenceCtx,
    text_render::{TextRenderer, TextWeight},
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::model::{ActionKind, SourceFilter, StoreItem, StoreSource};
use crate::sources::distinct_categories;
use crate::widgets::{CardAction, CardDetails, CardLayout};

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
const CATALOGUE_CONTENT_MAX_WIDTH: f32 = 1_120.0;
/// Minimum width (logical px) of one cell in the browse filter band. Wide
/// enough for the longest source/category value; every cell grows equally
/// from this basis so the labels row and the controls row share column edges.
const FILTER_CELL_MIN_W: f32 = 176.0;
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
            crate::StoreSource::Flathub => "Flathub / upstream metadata".into(),
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
            crate::StoreSource::Flathub => "Flathub / upstream metadata".into(),
            crate::StoreSource::Tilo { .. } => "Not disclosed".into(),
        },
        |key| format!("ed25519:{key}"),
    )
}

/// Human-facing device state must distinguish Tilo's content-addressed,
/// fetch-on-open cache from a conventional Flatpak installation.
fn device_state_label(item: &StoreItem) -> String {
    match (&item.source, &item.installed) {
        (crate::StoreSource::Tilo { .. }, crate::InstalledState::Not) => {
            "Available on demand · fetched when opened\nNot kept on this device".into()
        }
        (
            crate::StoreSource::Tilo { .. },
            crate::InstalledState::Installed {
                version,
                available,
                pinned,
                user_managed,
            },
        ) => format!(
            "Cached {version} · available {available}\n{}",
            if !*user_managed {
                "Provided by the system"
            } else if *pinned {
                "Kept on this device"
            } else {
                "Not kept · cache may be reclaimed"
            }
        ),
        (crate::StoreSource::Flathub, crate::InstalledState::Not) => "Not installed".into(),
        (
            crate::StoreSource::Flathub,
            crate::InstalledState::Installed {
                version, available, ..
            },
        ) => format!("Installed {version} · available {available}"),
    }
}

/// Render a readable, stable publisher identity summary. The full identity is
/// retained in the canonical app resource and report envelope; the detail
/// page intentionally avoids turning a human decision surface into a wall of
/// repeated hex characters.
#[cfg(test)]
fn publisher_identity_lines(item: &StoreItem) -> Vec<String> {
    let Some(key) = item.publisher_pubkey_hex.as_deref() else {
        return vec!["Publisher identity: not published by this distributor".into()];
    };
    let prefix: String = key.chars().take(12).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(12)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    vec![format!(
        "Publisher fingerprint · ed25519:{prefix}…{suffix} ({} characters)",
        key.chars().count()
    )]
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

/// Name the result region for what it actually contains.  A responsive
/// large-text layout may choose one catalogue list instead of availability
/// shelves, but that must not turn an untouched catalogue into “Search
/// results” for sighted or screen-reader users.
#[cfg(test)]
fn catalogue_region_heading(grouped_by_availability: bool, query: &str) -> &'static str {
    if grouped_by_availability {
        "Browse apps"
    } else if query.trim().is_empty() {
        "All apps"
    } else {
        "Search results"
    }
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
        ModerationCaseStatus::ClosedNoAction => "Closed · no action",
        ModerationCaseStatus::Quarantined => "Quarantined",
        ModerationCaseStatus::Restored => "Restored",
    }
}

fn detail_section(
    title: &str,
    lines: impl IntoIterator<Item = String>,
    theme: &Theme,
    compact: bool,
    roomy_desktop: bool,
) -> Box<dyn Widget> {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s2)
        .push(Box::new(
            Text::new(title)
                .with_scaled_size(18.0)
                .with_heading_level(2),
        ));
    for line in lines {
        column = column.push(Box::new(
            Text::new(&line)
                .with_scaled_size(13.0)
                .with_color(theme.text_dim),
        ));
    }
    bounded_detail_card(Box::new(column), compact, roomy_desktop)
}

/// The header of a secondary Store page (Flatpak remotes, Trust & moderation):
/// the back affordance, the page title and its one-line explanation.
///
/// Title and explanation hug each other by `s2`; the back control sits one
/// `s3` above the title so it reads as navigation rather than as part of the
/// heading. Both pages share this block so their titles use one size and the
/// return route always lands at the same place.
fn secondary_page_header(
    back: Box<dyn Widget>,
    title: &str,
    subtitle: &str,
    theme: &Theme,
) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(back)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(Box::new(
                        Text::new(title)
                            .with_scaled_size(24.0)
                            .with_heading_level(1),
                    ))
                    .push(Box::new(
                        Text::new(subtitle)
                            .with_scaled_size(13.0)
                            .with_color(theme.text_dim),
                    )),
            )),
    )
}

/// A secondary page: its header one section step (`s6`) above the body, whose
/// own groups are spaced by the body column. The header is always nearer its
/// own title/explanation than to the first card beneath it.
fn secondary_page(
    header: Box<dyn Widget>,
    body: Box<dyn Widget>,
    theme: &Theme,
) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s6)
            .push(header)
            .push(body),
    )
}

/// A page-level reading column owns desktop centring. Individual detail
/// sections only provide the themed surface, so nested sections never widen
/// back to the viewport and create broad, disconnected slabs.
fn bounded_detail_card(
    child: Box<dyn Widget>,
    _compact: bool,
    _roomy_desktop: bool,
) -> Box<dyn Widget> {
    Box::new(Container::new().with_background().with_child(child))
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
        format!("{source} · v{latest}")
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
        crate::StoreTrust::PublisherSigned => "Signature verified · no independent review",
        crate::StoreTrust::Flathub => "Flathub distribution · no Tilo review",
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
    let resource = Resource::new("store", "store.catalog", "App Store").with_action(spec);
    ActionBinding::unit_for(&resource, "refresh").ok()
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

fn safety_back_button() -> ui::widget::Button {
    let mut button = ui::widget::Button::new("store.safety.back", "Back", None);
    if let Some(binding) = navigation_binding("show-catalogue", "Back to apps", "catalogue") {
        button = button.on_action(binding);
    }
    button
        .with_icon(Icon::ArrowLeft)
        .fit_content(40.0)
        .tooltip("Return to the app catalogue")
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
    /// Current search query, applied to the merged catalogue. Search is
    /// intentionally window-local; resources remain globally searchable via
    /// `search_index` without making a colleague's catalogue jump around.
    query: String,
    query_state: ui::LocalState<String>,
    source_filter_state: ui::LocalState<String>,
    category_filter_state: ui::LocalState<String>,
    /// An unsubmitted remote form is device-local. Only the canonical Add
    /// remote effect crosses into machine-scoped package configuration.
    remote_name_state: ui::LocalState<String>,
    remote_url_state: ui::LocalState<String>,
    /// Rolling status line the worker overwrites while an action
    /// runs. Surfaced transiently in the floating status bubble (see
    /// [`draw_status_bubble`](Self::draw_status_bubble)); the catalog
    /// counts stand in while it is empty.
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
    /// Transient floating status chip. The catalog counts / action
    /// status surface here — revealed on a real change or while the
    /// pointer hovers the bottom-left band, then faded — in place of a
    /// permanent full-width footer strip, so the card list reclaims the
    /// height. See [`draw_status_bubble`](Self::draw_status_bubble).
    status_bubble: ui::StatusBubble,
    /// The label the bubble last reflected, so we only poke it (reveal
    /// + restart the linger) when the surfaced text actually changes.
    last_status_label: String,
    /// Last pointer position (window coords), tracked from `CursorMoved`
    /// so the bubble can reveal on hover of its zone.
    last_cursor: Point,
    /// `--shots` forces the bubble fully visible so a single headless
    /// frame captures the floating chip.
    shots_status: bool,
    /// Shared async, bounded decoded-artwork cache. `None` for hermetic shots.
    images: Option<ui::ImageStore>,
    reduce_motion: bool,
    /// Sampled from the shared theme at layout time.  Store keeps its view
    /// state local, but its responsive composition must still react to the
    /// person's global readable-type preference.
    text_scale: f32,
    layout_width_hint: f64,
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
            query_state: ui::LocalState::new(String::new()),
            source_filter_state: ui::LocalState::new("All".into()),
            category_filter_state: ui::LocalState::new("All".into()),
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
            last_status_label: String::new(),
            last_cursor: Point::ZERO,
            shots_status: false,
            images: None,
            reduce_motion: false,
            text_scale: 1.0,
            layout_width_hint: 0.0,
        }
    }

    #[must_use]
    pub fn with_image_store(mut self, images: ui::ImageStore) -> Self {
        self.images = Some(images);
        self
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
            status: "Draft · not submitted".into(),
        });
        self.structure_dirty = true;
    }

    pub fn show_moderation_case_for_shot(&mut self, status: &str) {
        self.show_safety_for_shot();
        self.report_notes.insert("so.tilo.notes".into(), "The app contacted a third-party host while its offline mode was enabled. The exported trace contains the exact destination and time.".into());
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

    pub fn set_layout_width_hint(&mut self, width: f64) {
        self.layout_width_hint = width;
        self.structure_dirty = true;
    }

    fn compact_layout(&self) -> bool {
        let width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            self.layout_width_hint
        };
        width > 0.0 && width < 620.0
    }

    /// Large text needs the same space as a narrow viewport even on a wide
    /// monitor: a three-column fixed-height card grid would otherwise hide
    /// the decision controls below enlarged descriptions.  This is only a
    /// presentation choice; filters, selection, and every canonical resource
    /// address remain unchanged.
    fn readable_type_layout(&self) -> bool {
        self.text_scale > 1.25
    }

    fn stacked_catalogue_layout(&self) -> bool {
        self.compact_layout() || self.readable_type_layout()
    }

    fn card_layout(&self) -> CardLayout {
        if self.stacked_catalogue_layout() {
            CardLayout::Stacked {
                large_text: self.readable_type_layout(),
            }
        } else {
            CardLayout::Wide
        }
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
            self.source_filter_state.set(f.label().into());
            self.catalog_page = 0;
            self.scroll_offset.set((0.0, 0.0));
            self.structure_dirty = true;
        }
    }

    fn set_active_filter_from_local(&mut self, value: &str) {
        if let Some(filter) = SourceFilter::ALL
            .iter()
            .copied()
            .find(|filter| filter.label() == value)
        {
            self.set_active_filter(filter);
        }
    }

    pub fn active_category(&self) -> Option<&str> {
        self.active_category.as_deref()
    }

    pub fn set_active_category(&mut self, c: Option<String>) {
        if self.active_category != c {
            self.active_category = c;
            self.category_filter_state
                .set(self.active_category.clone().unwrap_or_else(|| "All".into()));
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
            self.query_state.set(self.query.clone());
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

    // ---- layout ----

    /// Compose the whole tree. Called from `build_layout`; also used
    /// by tests to inspect the composition graph directly.
    #[allow(
        clippy::too_many_lines,
        reason = "the root owns the catalogue-only masthead composition alongside the shared detail, remote, safety, and list routes"
    )]
    fn compose(&self, theme: &Theme) -> Box<dyn Widget> {
        let header = self.build_header(theme);
        let content = if self.safety_open {
            self.build_safety_center(theme)
        } else if self.remotes_open {
            self.build_remotes(theme)
        } else {
            self.build_detail(theme)
                .unwrap_or_else(|| self.build_list(theme))
        };
        // No permanent footer strip: the catalog counts / action status
        // float as a transient bubble (see `draw_status_bubble`), so the
        // card list spans to the window bottom.
        let mut column = Column::new()
            // The masthead is an intrinsic sibling above a scroll body. Give
            // the root column the remaining-height contract so the busy body
            // cannot receive a second full viewport and paint over header
            // controls during a progress update.
            .with_fill_remaining_height()
            .with_vertical_padding(0.0)
            .with_horizontal_padding(0.0, 0.0)
            .with_gap(0.0);
        if !self.remotes_open && !self.safety_open && self.focused_detail.is_none() {
            // One compact browse bar keeps the filters close to the results.
            // The old labelled band spent a third of a laptop viewport before
            // users saw a single app.
            // The native picker names remain explicit for assistive
            // technology; their visible values carry the context in this
            // compact, scan-friendly band. The stacked (phone / large-text)
            // layout carries the catalogue summary in the header instead.
            let catalogue_controls = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2)
                .push(header)
                .push(self.build_filter_band(theme, !self.stacked_catalogue_layout()));
            column = column.push(
                self.centered_content(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(Box::new(catalogue_controls)),
                )),
            );
        }
        column = column.push(content);
        Box::new(
            Container::new()
                .filling()
                .with_fill(theme.bg_app)
                .with_child(Box::new(column)),
        )
    }

    fn centered_content(&self, child: Box<dyn Widget>) -> Box<dyn Widget> {
        let width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            self.layout_width_hint
        };
        // A fixed desktop reading width should never create a clipped middle
        // breakpoint. Between phone and roomy desktop widths the masthead and
        // pages simply use the available canvas.
        if self.compact_layout() || (width > 0.0 && width < 1_180.0) {
            child
        } else if self.readable_type_layout() {
            // Enlarged type does not turn a wide monitor into an invitation
            // for 1400px one-column cards.  Keep the same calm editorial
            // measure used by detail pages while retaining every desktop
            // control and full touch target.
            Box::new(
                Row::new()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Container::new()
                            .with_width(760.0)
                            .with_no_padding()
                            .with_child(child),
                    ))
                    .push(Box::new(Spacer::new())),
            )
        } else {
            Box::new(
                Row::new()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Container::new()
                            .with_width(CATALOGUE_CONTENT_MAX_WIDTH)
                            .with_no_padding()
                            .with_child(child),
                    ))
                    .push(Box::new(Spacer::new())),
            )
        }
    }

    /// Whether there is enough room for the deliberately bounded desktop
    /// canvas.  Keeping this decision in one place prevents the masthead
    /// from adapting at one breakpoint while the catalogue still forces a
    /// wider, horizontally clipped grid at another.
    fn roomy_desktop(&self) -> bool {
        let width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            self.layout_width_hint
        };
        width >= 1_180.0
    }

    fn desktop_browse_layout(&self) -> bool {
        self.roomy_desktop() && !self.readable_type_layout()
    }

    /// Minimum width for a discovery card at the current layout mode. A wide
    /// Store canvas resolves to three editorial tiles: enough room for real
    /// artwork, a reason-to-care and an action rail, without the sparse
    /// 2+1 shelves that made a catalogue look unfinished.
    #[cfg(test)]
    fn catalogue_card_min_width(&self) -> f64 {
        if self.desktop_browse_layout() {
            348.0
        } else {
            330.0
        }
    }

    /// Idle browsing benefits from the explicit "ready" and "discover"
    /// decision groups when the viewport can show a complete row. At the
    /// medium desktop breakpoint those groups commonly contain an odd count,
    /// leaving a conspicuous empty quadrant; one balanced catalogue grid is
    /// clearer there. Search always preserves its relevance-ranked sequence.
    fn group_catalogue_by_availability(&self) -> bool {
        self.query.trim().is_empty() && (self.compact_layout() || self.roomy_desktop())
    }

    fn catalogue_source_summary(&self) -> String {
        // This line stays about catalogue scope even while an operation is
        // running. Progress and failures receive their own semantic notice
        // below, rather than making this quiet provenance label unexpectedly
        // change meaning or compete with the result count.
        let source_count = self
            .items
            .iter()
            .map(|item| match &item.source {
                StoreSource::Tilo { .. } => "Tilo catalogue",
                StoreSource::Flathub => "Flathub",
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if self.items.is_empty() {
            "No apps loaded from configured sources yet".to_string()
        } else {
            format!(
                "{} apps from {source_count} configured source{}",
                self.items.len(),
                if source_count == 1 { "" } else { "s" }
            )
        }
    }

    /// Persistent operation feedback belongs in document flow, not in a
    /// tiny subtitle or a transient overlay. `Banner` is the shared native
    /// alert treatment: it honours every theme, retains a readable measure at
    /// 200% type, and exposes a status role to assistive technology.
    fn operation_notice(&self) -> Option<Box<dyn Widget>> {
        if self.worker_busy {
            let detail = if self.status_line.is_empty() {
                "Package operation in progress…".to_string()
            } else {
                visible_operation_status(&self.status_line)
            };
            // Worker operations do not publish one trustworthy unit: a
            // launch, catalogue refresh, and Flatpak update cannot honestly
            // share a made-up percentage. The shared indeterminate bar makes
            // activity visible without pretending otherwise, respects
            // reduce-motion, and carries a polite accessible label.
            return Some(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(7.0)
                    .push(Box::new(
                        Banner::info("Working in background").with_message(detail),
                    ))
                    .push(Box::new(
                        ProgressBar::indeterminate().with_label("Operation in progress"),
                    )),
            ));
        }
        self.operation_error.as_ref().map(|_| {
            Box::new(
                Banner::error("The last operation failed")
                    .with_message(visible_operation_status(&self.status_line)),
            ) as Box<dyn Widget>
        })
    }

    /// Detail, remote-management, and moderation views have a much shorter
    /// comfortable reading measure than a discovery grid. Keep their whole
    /// editorial flow—not merely its individual cards—within that measure
    /// from typical laptop widths upward.
    fn bounded_page_content(&self, child: Box<dyn Widget>) -> Box<dyn Widget> {
        let width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            self.layout_width_hint
        };
        if self.compact_layout() || (width > 0.0 && width < 900.0) {
            child
        } else {
            let page_width = if self.roomy_desktop() && !self.readable_type_layout() {
                1120.0
            } else {
                760.0
            };
            Box::new(
                Row::new()
                    .with_fill_remaining_height()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Container::new()
                            .with_width(page_width)
                            .with_no_padding()
                            .with_child(child),
                    ))
                    .push(Box::new(Spacer::new())),
            )
        }
    }

    /// Use a desktop window as a product decision workspace, with narrative
    /// and project facts in the reading column and a visible action/trust rail.
    #[allow(
        clippy::too_many_lines,
        reason = "the desktop product page keeps its paired narrative and decision-rail composition in one reviewable reading order"
    )]
    fn build_desktop_detail(&self, item: &StoreItem, theme: &Theme) -> Box<dyn Widget> {
        let source = match &item.source {
            StoreSource::Tilo { registry_name } => format!("Tilo · {registry_name}"),
            StoreSource::Flathub => "Flatpak · Flathub".to_string(),
        };
        let trust_label = match item.trust {
            crate::StoreTrust::PublisherSigned => "Publisher signature verified",
            crate::StoreTrust::Flathub => "Distributed by Flathub",
        };
        let action = item.action_kind(self.flatpak_available);
        let operation_active = self
            .active_operation
            .as_ref()
            .is_some_and(|(id, active_action)| id == &item.id && *active_action == action);
        let mut primary = ui::widget::Button::new(
            &format!("store.app.{}.{}", item.id, action_code(action)),
            action.label(),
            None,
        )
        .primary()
        .tooltip(format!("{} {}", action.label(), item.name));
        if let Some(binding) = item_action_binding(item, action) {
            primary = primary.on_action(binding);
        }
        let mut device_actions = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(Box::new(primary));
        if let Some(pin_action) = item.pin_action() {
            let mut button = ui::widget::Button::new(
                &format!("store.app.{}.{}", item.id, action_code(pin_action)),
                pin_action.label(),
                None,
            )
            .tooltip(format!("{} {}", pin_action.label(), item.name));
            if let Some(binding) = item_action_binding(item, pin_action) {
                button = button.on_action(binding);
            }
            device_actions = device_actions.push(Box::new(button));
        }
        if let Some(remove_action) = item.remove_action() {
            let mut button =
                ui::widget::Button::new(&format!("store.app.{}.remove", item.id), "Remove", None)
                    .ghost()
                    .tooltip(format!("Remove {}", item.name));
            if let Some(binding) = item_action_binding(item, remove_action) {
                button = button.on_action(binding);
            }
            device_actions = device_actions.push(Box::new(button));
        }
        let installed = device_state_label(item);
        let package_lines = item.versions.first().map_or_else(
            || vec!["No version metadata was published by this source.".to_string()],
            |version| {
                let mut lines = vec![format!("Latest release · {}", version.version)];
                if version.size_bytes > 0 {
                    lines.push(format!(
                        "Download size · {}",
                        format_size(version.size_bytes)
                    ));
                }
                if let Some(hash) = version.root_hash_hex.as_deref() {
                    let prefix = hash.chars().take(16).collect::<String>();
                    lines.push(format!("Package digest · BLAKE3:{prefix}…"));
                }
                if version.download_url.is_some() {
                    lines.push("Package download record published".to_string());
                }
                lines
            },
        );
        let mut back = ui::widget::Button::new("store.detail.back", "Back to apps", None)
            .with_icon(Icon::ArrowLeft)
            .fit_content(40.0)
            .tooltip("Return to the catalogue");
        if let Some(binding) = navigation_binding("show-catalogue", "Back to apps", "catalogue") {
            back = back.on_action(binding);
        }
        let trust_explanation = match item.trust {
            crate::StoreTrust::PublisherSigned => {
                "Identity and package integrity are verified. This is not an independent quality review."
            }
            crate::StoreTrust::Flathub => {
                "This app is distributed by Flathub. Tilo has not independently reviewed it."
            }
        };
        let mut report = ui::widget::Button::new(
            &format!("store.report.create.{}", item.id),
            "Create report draft",
            None,
        )
        .fit_content(40.0)
        .tooltip("Create a local, collaborative report draft");
        if let Some(binding) = report_binding(item) {
            report = report.on_action(binding);
        }
        let mut moderation =
            ui::widget::Button::new("store.safety.detail", "Moderation details", None)
                .fit_content(40.0)
                .tooltip("Review the Store trust and moderation model");
        if let Some(binding) = open_surface_binding(
            "store:moderation",
            "store.moderation",
            "Trust & moderation",
            "show",
            "Show trust and moderation",
            "store.view-trust",
        ) {
            moderation = moderation.on_action(binding);
        }
        let availability = if operation_active {
            Badge::semantic_pill("Working", BadgeTone::Accent)
        } else if item.installed.update_available() {
            Badge::semantic_pill("Update available", BadgeTone::Warning)
        } else if item.installed.is_installed() {
            Badge::semantic_pill("On this device", BadgeTone::Success)
        } else if matches!(item.source, StoreSource::Tilo { .. }) {
            Badge::semantic_pill("On demand", BadgeTone::Accent)
        } else {
            Badge::semantic_pill("Not installed", BadgeTone::Neutral)
        };
        let hero = bounded_detail_card(
            Box::new(
                Container::new()
                    .with_no_padding()
                    .with_height(154.0)
                    .with_child(Box::new(
                        Row::new()
                            .no_wrap()
                            .push(Box::new(crate::widgets::Artwork::new(
                                self.images.clone(),
                                item.icon_url.as_deref(),
                                &item.name,
                                104.0,
                                104.0,
                            )))
                            .push(Box::new(
                                Container::new().with_no_padding().with_width(14.0),
                            ))
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .with_flex_grow()
                                    .with_child(Box::new(
                                        Column::new()
                                            .with_no_padding()
                                            .with_intrinsic_height()
                                            .with_gap(5.0)
                                            .push(Box::new(
                                                Text::new(&item.name)
                                                    .with_scaled_size(32.0)
                                                    .with_weight(TextWeight::Semibold)
                                                    .with_heading_level(1),
                                            ))
                                            .push(Box::new(
                                                Text::new(&format!("{source} · {trust_label}"))
                                                    .with_scaled_size(13.0)
                                                    .with_color(theme.text_dim),
                                            ))
                                            .push(Box::new(
                                                Row::new().no_wrap().push(Box::new(availability)),
                                            ))
                                            .push(Box::new(
                                                Text::new(&item.description)
                                                    .with_scaled_size(16.0)
                                                    .with_color(theme.text_dim),
                                            )),
                                    )),
                            ))
                            .push(Box::new(
                                Container::new()
                                    .with_width(212.0)
                                    .with_no_padding()
                                    .with_child(Box::new(device_actions)),
                            )),
                    )),
            ),
            false,
            true,
        );
        let mut publisher_facts = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(Box::new(
                Text::new("Source & publisher")
                    .with_scaled_size(18.0)
                    .with_weight(TextWeight::Semibold)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new(&source)
                    .with_scaled_size(13.0)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(
                Text::new(trust_label)
                    .with_scaled_size(13.0)
                    .with_weight(TextWeight::Semibold),
            ));
        if let Some(key) = item.publisher_pubkey_hex.as_deref() {
            let full_key = format!("ed25519:{key}");
            let clipboard_key = full_key.clone();
            let key_button = ui::widget::Button::new(
                &format!("store.publisher.copy.{}", item.id),
                &format!("Publisher · {}", short_publisher(item)),
                None,
            )
            .ghost()
            .fit_content(40.0)
            .with_accessible_name(&format!("Copy full publisher key {full_key}"))
            .tooltip(format!("Copy full publisher key {full_key}"))
            .on_effect(move || {
                ui::text_selection::clipboard_copy(&clipboard_key);
            });
            publisher_facts = publisher_facts.push(Box::new(key_button));
        } else {
            publisher_facts = publisher_facts.push(Box::new(
                Text::new(&format!("Publisher · {}", full_publisher(item)))
                    .with_scaled_size(13.0)
                    .with_color(theme.text_dim),
            ));
        }
        let fact_surface = |child: Box<dyn Widget>| {
            bounded_detail_card(
                Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_height(188.0)
                        .with_child(child),
                ),
                false,
                true,
            )
        };
        let package_facts = fact_surface(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(
                    Text::new("Package")
                        .with_scaled_size(18.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ))
                .push(Box::new(
                    Text::new(&format!("App ID · {}", item.id))
                        .with_scaled_size(12.0)
                        .with_color(theme.text_tertiary),
                ))
                .push(Box::new(
                    Text::new(&format!("Categories · {}", item.categories.join(", ")))
                        .with_scaled_size(13.0)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(
                    Text::new(&package_lines.join("\n"))
                        .with_scaled_size(13.0)
                        .with_color(theme.text_dim),
                )),
        ));
        let publisher_facts = fact_surface(Box::new(publisher_facts));
        let device_facts = fact_surface(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(
                    Text::new("On this device")
                        .with_scaled_size(18.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ))
                .push(Box::new(Text::new(&installed).with_scaled_size(14.0)))
                .push(Box::new(
                    Text::new(trust_explanation)
                        .with_scaled_size(13.0)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(report))
                        .push(Box::new(moderation)),
                )),
        ));
        let facts = Row::new()
            .no_wrap()
            .push(Box::new(
                Container::new()
                    .with_width(364.0)
                    .with_no_padding()
                    .with_child(package_facts),
            ))
            .push(Box::new(
                Container::new()
                    .with_width(364.0)
                    .with_no_padding()
                    .with_child(publisher_facts),
            ))
            .push(Box::new(
                Container::new()
                    .with_width(364.0)
                    .with_no_padding()
                    .with_child(device_facts),
            ));
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(16.0)
            // Back is navigation, not a content surface. A no-wrap row
            // retains the compact control width instead of painting an empty
            // full-width slab before the product hero.
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(back))
                    .push(Box::new(Spacer::new())),
            ));
        if let Some(notice) = self.operation_notice() {
            page = page.push(notice);
        }
        let page = page.push(hero).push(Box::new(facts));
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Row::new()
                        .with_fill_remaining_height()
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(
                            Container::new()
                                .with_width(1120.0)
                                .with_no_padding()
                                .with_child(Box::new(page)),
                        ))
                        .push(Box::new(Spacer::new())),
                )),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one cohesive decision sheet keeps trust, package, permission, link, preview, and action sections in display order"
    )]
    fn build_detail(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        let id = self.focused_detail.as_ref()?;
        let item = self.items.iter().find(|item| &item.id == id)?;
        if self.desktop_browse_layout() {
            return Some(self.build_desktop_detail(item, theme));
        }
        let source = match &item.source {
            crate::StoreSource::Tilo { registry_name } => format!("Tilo · {registry_name}"),
            crate::StoreSource::Flathub => "Flatpak · Flathub".to_string(),
        };
        let locally_retained = matches!(
            &item.source,
            crate::StoreSource::Tilo { registry_name } if registry_name == "Installed on this device"
        );
        let trust_summary = if locally_retained {
            "Verified when installed. Current registry metadata is unavailable, so update availability is unknown."
        } else {
            match item.trust {
                crate::StoreTrust::PublisherSigned => {
                    "Publisher identity and package integrity are verified. This is not an independent quality review."
                }
                crate::StoreTrust::Flathub => {
                    "Distributed by Flathub. Tilo has not independently reviewed this app."
                }
            }
        };
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            // Detail content mixes an intrinsic artwork row, paragraph and
            // actions. A real gap prevents the paragraph's baseline from
            // colliding with the first action at larger text metrics.
            .with_gap(theme.tokens.spacing.s3);
        let action = item.action_kind(self.flatpak_available);
        let mut primary = ui::widget::Button::new(
            &format!("store.app.{}.{}", item.id, action_code(action)),
            action.label(),
            None,
        );
        if let Some(binding) = item_action_binding(item, action) {
            primary = primary.on_action(binding);
        }
        let mut action_buttons: Vec<Box<dyn Widget>> = vec![Box::new(
            primary
                .primary()
                .tooltip(format!("{} {}", action.label(), item.name)),
        )];
        if let Some(pin_action) = item.pin_action() {
            let mut button = ui::widget::Button::new(
                &format!("store.app.{}.{}", item.id, action_code(pin_action)),
                pin_action.label(),
                None,
            );
            if let Some(binding) = item_action_binding(item, pin_action) {
                button = button.on_action(binding);
            }
            action_buttons.push(Box::new(button.ghost().tooltip(format!(
                "{} {}",
                pin_action.label(),
                item.name
            ))));
        }
        if let Some(remove_action) = item.remove_action() {
            let mut button =
                ui::widget::Button::new(&format!("store.app.{}.remove", item.id), "Remove", None);
            if let Some(binding) = item_action_binding(item, remove_action) {
                button = button.on_action(binding);
            }
            action_buttons.push(Box::new(
                button.ghost().tooltip(format!("Remove {}", item.name)),
            ));
        }
        let mut report_button = ui::widget::Button::new(
            &format!("store.report.create.{}", item.id),
            "Create report draft",
            None,
        );
        if let Some(binding) = report_binding(item) {
            report_button = report_button.on_action(binding);
        }
        // Report controls live in an intrinsic detail footer.  Reserve their
        // normal touch target explicitly: otherwise Taffy's intrinsic pass
        // can collapse a neutral button to its text baseline and make the
        // footer look like two stray labels rather than usable controls.
        report_button = report_button.fit_content(40.0);
        let trust_lines = if self.compact_layout() {
            vec![
                match item.trust {
                    crate::StoreTrust::PublisherSigned => "Publisher signature verified".into(),
                    crate::StoreTrust::Flathub => "Distributed by Flathub".into(),
                },
                "No independent quality review".into(),
            ]
        } else {
            vec![trust_summary.to_string()]
        };
        let actions: Box<dyn Widget> = if self.compact_layout() {
            let mut column = Column::new().with_no_padding().with_intrinsic_height();
            for button in action_buttons {
                column = column.push(button);
            }
            Box::new(column)
        } else {
            let mut row = Row::new();
            for button in action_buttons {
                row = row.push(button);
            }
            Box::new(row)
        };
        let mut back = ui::widget::Button::new("store.detail.back", "Back to apps", None);
        if let Some(binding) = navigation_binding("show-catalogue", "Back to apps", "catalogue") {
            back = back.on_action(binding);
        }
        let back = Box::new(
            back.with_icon(Icon::ArrowLeft)
                .fit_content(40.0)
                .tooltip("Return to the catalogue"),
        );
        let artwork = Box::new(crate::widgets::Artwork::new(
            self.images.clone(),
            item.icon_url.as_deref(),
            &item.name,
            if self.compact_layout() { 56.0 } else { 88.0 },
            if self.compact_layout() { 56.0 } else { 88.0 },
        ));
        let trust_label = match item.trust {
            crate::StoreTrust::PublisherSigned => "Publisher verified",
            crate::StoreTrust::Flathub => "Flathub distribution",
        };
        let summary: Box<dyn Widget> = if self.compact_layout() {
            // Keep the compact identity stack intrinsic and separate from the
            // artwork. The previous wrapped row could underreport its height
            // and let the description paint across the icon.
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&item.name).with_scaled_size(26.0)))
                    .push(Box::new(
                        Text::new(&source)
                            .with_scaled_size(12.0)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(
                        Text::new(trust_label)
                            .with_scaled_size(12.0)
                            .with_color(theme.text_tertiary),
                    )),
            )
        } else {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_fill_remaining_height()
                    .push(Box::new(
                        Text::new(&item.name)
                            .with_scaled_size(32.0)
                            .with_weight(TextWeight::Semibold),
                    ))
                    .push(Box::new(
                        Text::new(&source)
                            .with_scaled_size(12.0)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(
                        Text::new(trust_label)
                            .with_scaled_size(12.0)
                            .with_color(theme.text_tertiary),
                    )),
            )
        };
        // One section gap (`s6`) separates the back affordance from the
        // identity block and the identity block from the description/actions.
        // An intrinsic column honours that gap itself, so no extra spacer is
        // stacked on top of it: the earlier fixed bands doubled the distance
        // between artwork and title on a phone.
        let hero_gap = theme.tokens.spacing.s6;
        let hero: Box<dyn Widget> = if self.compact_layout() {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(hero_gap)
                    // The back control keeps its own explicit 40 px height, so
                    // the intrinsic column's row already spans the full touch
                    // target — no wrapper band is needed to hold the artwork
                    // below it.
                    .push(back)
                    .push(artwork)
                    .push(summary)
                    .push(Box::new(
                        Text::new(&item.description).with_scaled_size(14.0),
                    ))
                    .push(actions),
            )
        } else {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(hero_gap)
                    .push(back)
                    .push(Box::new(
                        Row::new()
                            .no_wrap()
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .with_flex_grow()
                                    .with_child(Box::new(
                                        Row::new().no_wrap().push(artwork).push(Box::new(
                                            Column::new()
                                                .with_no_padding()
                                                .with_intrinsic_height()
                                                .with_gap(theme.tokens.spacing.s3)
                                                .push(summary)
                                                .push(Box::new(
                                                    Text::new(&item.description)
                                                        .with_scaled_size(15.0)
                                                        .with_color(theme.text_dim),
                                                )),
                                        )),
                                    )),
                            ))
                            .push(Box::new(
                                Container::new()
                                    .with_width(214.0)
                                    .with_no_padding()
                                    .with_child(Box::new(
                                        Column::new()
                                            .with_no_padding()
                                            .with_intrinsic_height()
                                            .with_gap(theme.tokens.spacing.s2)
                                            .push(actions)
                                            .push(Box::new(
                                                Text::new(match item.trust {
                                                    crate::StoreTrust::PublisherSigned => {
                                                        "Publisher verified"
                                                    }
                                                    crate::StoreTrust::Flathub => {
                                                        "Flathub distribution"
                                                    }
                                                })
                                                .with_scaled_size(12.0)
                                                .with_color(theme.text_dim),
                                            )),
                                    )),
                            )),
                    )),
            )
        };
        let hero = bounded_detail_card(hero, self.compact_layout(), self.roomy_desktop());
        column = column.push(hero);

        let version_lines = if item.versions.is_empty() {
            vec!["No version metadata published.".into()]
        } else if self.compact_layout() {
            item.versions
                .iter()
                .map(|version| {
                    format!(
                        "Version {} · {}",
                        version.version,
                        format_size(version.size_bytes),
                    )
                })
                .collect()
        } else {
            item.versions
                .iter()
                .map(|version| {
                    format!(
                        "Version {} · {} · signed package record available",
                        version.version,
                        format_size(version.size_bytes),
                    )
                })
                .collect()
        };
        let installed = device_state_label(item);
        let fact_block = |title: &str, lines: Vec<String>| {
            let mut block = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s1)
                .push(Box::new(
                    Text::new(title)
                        .with_scaled_size(14.0)
                        .with_weight(TextWeight::Semibold),
                ));
            for line in lines {
                block = block.push(Box::new(
                    Text::new(&line)
                        .with_scaled_size(13.0)
                        .with_color(theme.text_dim),
                ));
            }
            // The encompassing decision surface supplies the visual boundary.
            // Keeping facts unboxed avoids a third nested-card treatment and
            // reads as one concise, scannable safety summary.
            Box::new(block) as Box<dyn Widget>
        };
        // The heading and its fact blocks share one `s3` rhythm; each block
        // keeps its own tighter title/line gap.
        let facts = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Text::new("At a glance")
                    .with_scaled_size(20.0)
                    .with_heading_level(2),
            ))
            .push(fact_block("Trust & safety", trust_lines))
            .push(fact_block("Package", version_lines))
            .push(fact_block(
                "Availability",
                vec![
                    installed,
                    "Tilo asks before sensitive capabilities; the package sandbox remains enforced."
                        .into(),
                ],
            ));
        column = column.push(bounded_detail_card(
            Box::new(facts),
            self.compact_layout(),
            self.roomy_desktop(),
        ));

        let links: Vec<String> = [
            item.homepage_url
                .as_deref()
                .map(|url| format!("Homepage · {url}")),
            item.source_url
                .as_deref()
                .map(|url| format!("Source · {url}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !links.is_empty() {
            column = column.push(detail_section(
                "Project links",
                links,
                theme,
                self.compact_layout(),
                self.roomy_desktop(),
            ));
        }

        if !item.screenshot_urls.is_empty() && self.images.is_some() {
            let mut previews: Vec<Box<dyn Widget>> = Vec::new();
            for (index, preview) in item.screenshot_urls.iter().take(3).enumerate() {
                previews.push(Box::new(crate::widgets::Artwork::new(
                    self.images.clone(),
                    Some(preview),
                    &format!("{} preview {}", item.name, index + 1),
                    248.0,
                    140.0,
                )));
            }
            let gallery: Box<dyn Widget> = if self.compact_layout() {
                let mut column = Column::new().with_no_padding();
                for preview in previews {
                    column = column.push(preview);
                }
                Box::new(column)
            } else {
                let mut row = Row::new();
                for preview in previews {
                    row = row.push(preview);
                }
                Box::new(row)
            };
            // Gallery artwork must share the same bounded reading column as
            // the rest of the decision sheet. Leaving the heading and images
            // directly in the outer scroll column made a missing/remote
            // preview look like an unrelated strip at the viewport edge.
            let gallery_section = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s3)
                .push(Box::new(
                    Text::new("Preview gallery")
                        .with_scaled_size(18.0)
                        .with_heading_level(2),
                ))
                .push(gallery);
            column = column.push(bounded_detail_card(
                Box::new(gallery_section),
                self.compact_layout(),
                self.roomy_desktop(),
            ));
        }

        let report_button: Box<dyn Widget> = Box::new(
            report_button
                .primary()
                .tooltip("Create a local, collaborative report draft"),
        );
        let mut moderation_button =
            ui::widget::Button::new("store.safety.detail", "Moderation details", None);
        if let Some(binding) = open_surface_binding(
            "store:moderation",
            "store.moderation",
            "Trust & moderation",
            "show",
            "Show trust and moderation",
            "store.view-trust",
        ) {
            moderation_button = moderation_button.on_action(binding);
        }
        let moderation_button: Box<dyn Widget> = Box::new(
            moderation_button
                .fit_content(40.0)
                .tooltip("Review the Store trust and moderation model"),
        );
        // Keep supporting safety actions in the same bounded surface system
        // as the decision content.  Previously this utility pair sat outside
        // the 1040px cards, which made it appear detached at desktop widths
        // and let its hit targets collide with the last surface on a fresh
        // layout pass.
        let support_actions: Box<dyn Widget> = if self.compact_layout() {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(report_button)
                    .push(moderation_button),
            )
        } else {
            // The two compact, explicitly-sized buttons now fit comfortably
            // in a single desktop action row.  This gives the support area a
            // clear primary action without wasting a card-height on two
            // orphaned text lines.
            Box::new(
                Row::new()
                    .no_wrap()
                    .push(report_button)
                    .push(moderation_button),
            )
        };
        let report_actions = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Text::new("Help keep the catalogue safe")
                    .with_scaled_size(16.0)
                    .with_weight(TextWeight::Semibold)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Report concerns collaboratively, or review how verified decisions and appeals work.")
                    .with_scaled_size(13.0)
                    .with_color(theme.text_dim),
            ))
            .push(support_actions);
        column = column.push(bounded_detail_card(
            Box::new(report_actions),
            self.compact_layout(),
            self.roomy_desktop(),
        ));
        Some(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(self.bounded_page_content(Box::new(column))),
                )),
        ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete responsive moderation surface is composed in one place so draft, case, appeal, and explanatory ordering cannot drift"
    )]
    fn build_safety_center(&self, theme: &Theme) -> Box<dyn Widget> {
        let back = safety_back_button()
            .with_icon(Icon::ArrowLeft)
            .fit_content(40.0)
            .tooltip("Return to the previous Store view");
        let header = secondary_page_header(
            Box::new(back),
            "Trust & moderation",
            "Signed reports, accountable decisions and publisher appeals",
            theme,
        );
        // The body's groups — the note field, report cards and explanations —
        // sit one `s3` apart; the page header is a section step (`s6`) above
        // the first of them.
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3);
        if self.reports.is_empty() {
            column = column.push(Box::new(
                EmptyState::new("No report drafts")
                    .with_message("Open an app and choose Create report draft."),
            ));
        } else {
            for report in &self.reports {
                let item = self.items.iter().find(|item| item.id == report.app_id);
                let note = self
                    .report_notes
                    .get(&report.app_id)
                    .map_or("", String::as_str);
                let mut export = ui::widget::Button::new(
                    &format!("{}.export", report.id),
                    "Export evidence",
                    None,
                );
                if let Some(binding) = report_export_binding(
                    report,
                    self.items.iter().find(|item| item.id == report.app_id),
                    note,
                ) {
                    export = export.on_action(binding);
                }
                let mut action_buttons: Vec<Box<dyn Widget>> = vec![Box::new(export.tooltip(
                    "Save a bounded JSON evidence envelope through the system file picker",
                ))];
                if let Some(item) = item
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
                            ("appeal", "Appeal decision", "store.moderation-appeal", note),
                        ] {
                            let payload =
                                (registry_name.clone(), report.id.clone(), reason.to_string());
                            let key = format!("{}.{}", report.id, action);
                            let mut button = ui::widget::Button::new(&key, label, None);
                            if let Some(spec) =
                                moderation_case_spec(action, label, type_id, &payload)
                            {
                                let resource = Resource::new(
                                    report.id.clone(),
                                    "store.moderation-case",
                                    report.app_id.clone(),
                                )
                                .with_action(spec);
                                if let Ok(binding) = ActionBinding::unit_for(&resource, action) {
                                    button = button.on_action(binding);
                                }
                            }
                            action_buttons.push(Box::new(button));
                        }
                    } else {
                        let key = format!("{}.submit", report.id);
                        let mut submit = ui::widget::Button::new(&key, "Submit report", None)
                            .tooltip("Sign and submit this report to the authoritative registry");
                        if let Some(binding) =
                            report_submit_binding(report, item, note, registry_name)
                        {
                            submit = submit.on_action(binding);
                        }
                        action_buttons.push(Box::new(submit));
                    }
                }
                let actions: Box<dyn Widget> = if self.compact_layout() {
                    let mut column = Column::new().with_no_padding();
                    for button in action_buttons {
                        column = column.push(button);
                    }
                    Box::new(column)
                } else {
                    let mut row = Row::new();
                    for button in action_buttons {
                        row = row.push(button);
                    }
                    Box::new(row)
                };
                let report_title = if report.id.starts_with("case-") {
                    "Moderation case"
                } else {
                    "Report draft"
                };
                let report_card = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(Box::new(Text::new(&report.app_id).with_scaled_size(16.0)))
                    .push(Box::new(
                        Text::new(report_title)
                            .with_scaled_size(20.0)
                            .with_weight(TextWeight::Semibold)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(
                        Text::new(&report.app_id)
                            .with_scaled_size(15.0)
                            .with_weight(TextWeight::Semibold),
                    ))
                    .push(Box::new(
                        Text::new(&format!("Status · {}", report.status))
                            .with_scaled_size(13.0)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(
                        Text::new("What happened?")
                            .with_scaled_size(15.0)
                            .with_weight(TextWeight::Semibold),
                    ))
                    .push(Box::new(
                        Text::new("Describe observable behavior and evidence another reviewer can verify.")
                            .with_scaled_size(12.0)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(
                        TextInput::new(vec![])
                            .with_id(&format!("store.report.note.{}", report.app_id))
                            .with_on_change_path(&report_note_key(&report.app_id))
                            .with_accessible_name("Concern and supporting evidence")
                            .with_placeholder("Describe what happened"),
                    ))
                    .push(actions);
                column = column.push(bounded_detail_card(
                    Box::new(report_card),
                    self.compact_layout(),
                    self.roomy_desktop(),
                ));
            }
        }
        column = column
            .push(detail_section("How reports and appeals work", [
                "Reports are signed by your workspace identity. Publisher replies and appeals must match the package publisher key; reviewer decisions must match the pinned registry owner.".into(),
                "Drafts stay collaborative. A submission receipt does not replace the last verified status: status changes appear only after the Store verifies the complete event chain against an owner-signed snapshot. Offline or refused requests remain drafts and can be retried.".into(),
            ], theme, self.compact_layout(), self.roomy_desktop()))
            .push(detail_section("Verified identity & integrity", [
                "Tilo packages are bound to a publisher key, app id, version, exact size and content hash. A pinned registry owner controls which publishers may list apps.".into(),
            ], theme, self.compact_layout(), self.roomy_desktop()))
            .push(detail_section("Review is a separate claim", [
                "A valid package signature does not mean an app is useful, private or honest. Moderation decisions are separately signed; the Store never invents a review badge.".into(),
                "Public moderation events form a numbered signed hash chain. The Store derives case status itself, checks publisher and reviewer authorization, and requires the final head committed by the registry owner's current snapshot.".into(),
            ], theme, self.compact_layout(), self.roomy_desktop()))
            .push(detail_section("Publishing guidelines", [
                "Listings must accurately describe behavior, identify paid or remote-service dependencies, request only necessary permissions, preserve user data during updates, provide an accessible UI, and avoid impersonation, manipulation and undisclosed tracking.".into(),
            ], theme, self.compact_layout(), self.roomy_desktop()));
        let page = secondary_page(header, Box::new(column), theme);
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(self.bounded_page_content(page)),
                )),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the machine-scoped remote editor and its canonical remove controls are deliberately composed together so their responsive layouts cannot diverge"
    )]
    fn build_remotes(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut back = ui::widget::Button::new("store.remotes.back", "Back to apps", None);
        if let Some(binding) = navigation_binding("show-catalogue", "Back to apps", "catalogue") {
            back = back.on_action(binding);
        }
        let back = Box::new(
            back.with_icon(Icon::ArrowLeft)
                .fit_content(40.0)
                .tooltip("Return to the app catalogue"),
        );
        let header = secondary_page_header(
            back,
            "Flatpak remotes",
            "Repositories are machine-scoped and every change runs through the package worker.",
            theme,
        );
        // The body separates its groups — add form, configured list — by one
        // `s3` step; within a group the heading hugs its cards, and the page
        // header is a section step (`s6`) above the first group.
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3);
        let mut remote_rows: Vec<Box<dyn Widget>> = Vec::new();
        for remote in &self.remotes {
            let mut remove = ui::widget::Button::new(
                &format!("store.remote.{}.remove", remote.name),
                "Remove remote",
                None,
            );
            if let Some(binding) = remote_remove_binding(remote) {
                remove = remove.on_action(binding);
            }
            let remove = Box::new(
                remove
                    .danger()
                    .fit_content(40.0)
                    .tooltip(format!("Remove {} from this machine", remote.title)),
            );
            let remote_row: Box<dyn Widget> = if self.compact_layout() {
                Box::new(
                    Container::new().with_no_padding().with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_gap(theme.tokens.spacing.s1)
                            .push(Box::new(Text::new(&remote.title).with_scaled_size(16.0)))
                            .push(Box::new(
                                Text::new(&remote.url)
                                    .with_scaled_size(13.0)
                                    .with_color(theme.text_dim),
                            ))
                            .push(Box::new(
                                Text::new(if remote.disabled {
                                    "Disabled"
                                } else {
                                    "Enabled"
                                })
                                .with_scaled_size(12.0)
                                .with_color(theme.text_dim),
                            ))
                            .push(remove),
                    )),
                )
            } else {
                Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_flex_grow()
                                .with_child(Box::new(
                                    Column::new()
                                        .with_no_padding()
                                        .with_intrinsic_height()
                                        .with_gap(theme.tokens.spacing.s1)
                                        .push(Box::new(
                                            Text::new(&remote.title).with_scaled_size(16.0),
                                        ))
                                        .push(Box::new(
                                            Text::new(&remote.url)
                                                .with_scaled_size(13.0)
                                                .with_color(theme.text_dim),
                                        ))
                                        .push(Box::new(
                                            Text::new(if remote.disabled {
                                                "Disabled"
                                            } else {
                                                "Enabled"
                                            })
                                            .with_scaled_size(12.0)
                                            .with_color(theme.text_dim),
                                        )),
                                )),
                        ))
                        .push(remove),
                )
            };
            remote_rows.push(remote_row);
        }
        let remote_name: Box<dyn Widget> = Box::new(
            TextInput::from_state(&self.remote_name_state)
                .with_id("store.remote.name")
                .with_kind(ui::widget::TextInputKind::Name)
                .with_placeholder("Remote name"),
        );
        let remote_url: Box<dyn Widget> = Box::new(
            TextInput::from_state(&self.remote_url_state)
                .with_id("store.remote.url")
                .with_kind(ui::widget::TextInputKind::Url)
                .with_placeholder("Repository URL"),
        );
        let mut add = ui::widget::Button::new("store.remote.add", "Add remote", None);
        if let Some(binding) = remote_form_binding() {
            add = add.on_action(binding);
        }
        let add: Box<dyn Widget> = Box::new(add.primary());
        let mut refresh = ui::widget::Button::new("store.remote.refresh", "Refresh", None);
        if let Some(binding) = remote_refresh_binding() {
            refresh = refresh.on_action(binding);
        }
        let refresh: Box<dyn Widget> = Box::new(refresh);
        let actions: Box<dyn Widget> = if self.compact_layout() {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(add)
                    .push(refresh),
            )
        } else {
            Box::new(Row::new().push(add).push(refresh))
        };
        let form = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Text::new("Add a remote")
                    .with_scaled_size(18.0)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Use a descriptive name and an HTTPS repository URL.")
                    .with_scaled_size(13.0)
                    .with_color(theme.text_dim),
            ))
            .push(remote_name)
            .push(remote_url)
            .push(actions);
        column = column.push(bounded_detail_card(
            Box::new(form),
            self.compact_layout(),
            self.roomy_desktop(),
        ));
        // The list heading belongs to the cards beneath it: `s2` between the
        // heading and its first card, `s2` between cards, and the page gap
        // before the whole group. Previously the heading sat flush on the
        // first card while the form above it had a spacer of its own.
        let mut configured = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Text::new("Configured remotes")
                    .with_scaled_size(18.0)
                    .with_heading_level(2),
            ));
        for remote_row in remote_rows {
            configured = configured.push(bounded_detail_card(
                remote_row,
                self.compact_layout(),
                self.roomy_desktop(),
            ));
        }
        column = column.push(Box::new(configured));
        let page = secondary_page(header, Box::new(column), theme);
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(self.bounded_page_content(page)),
                )),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the responsive catalogue header is one cohesive search, navigation, status, and toolbar surface"
    )]
    fn build_header(&self, theme: &Theme) -> Box<dyn Widget> {
        // Use the same actual-width decision as the catalogue. A medium-width
        // short window has room for the title and utility actions on one row;
        // forcing it through a separate arbitrary header breakpoint both
        // displaced results and repeated the source summary beside filters.
        let compact_header = self.stacked_catalogue_layout();
        // Search state belongs to this window. Agents still discover every
        // stable app through the Store search provider; typing never changes
        // a collaborator's view or document history.
        let search = TextInput::from_state(&self.query_state)
            .with_id("store.search")
            .with_kind(ui::widget::TextInputKind::Search)
            .on_action(crate::browse_actions::search_binding(
                &self.category_labels(),
            ))
            .with_placeholder("Search apps, publishers, or categories…");
        // Refresh button writes REFRESH_KEY; the outer host drives
        // the real refetch when it sees the bit go high.
        let mut refresh_btn = ui::widget::Button::new("store.refresh", "Refresh", None);
        if let Some(binding) = refresh_binding() {
            refresh_btn = refresh_btn.on_action(binding);
        }
        let refresh_btn = refresh_btn.tooltip("Refresh configured catalogues");
        let mut remotes_btn = ui::widget::Button::new("store.remotes", "Remotes", None)
            .with_icon(Icon::Globe)
            .fit_content(40.0)
            .tooltip("Manage Flatpak remotes");
        if let Some(binding) = open_surface_binding(
            "store:remotes",
            "store.remote-list",
            "Flatpak remotes",
            "show",
            "Show Flatpak remotes",
            "store.view-remotes",
        ) {
            remotes_btn = remotes_btn.on_action(binding);
        }
        let mut safety_btn = ui::widget::Button::new("store.safety", "Trust", None)
            .with_icon(Icon::Shield)
            .fit_content(40.0)
            .tooltip("Understand signatures, review, reports and appeals");
        if let Some(binding) = open_surface_binding(
            "store:moderation",
            "store.moderation",
            "Trust & moderation",
            "show",
            "Show trust and moderation",
            "store.view-trust",
        ) {
            safety_btn = safety_btn.on_action(binding);
        }
        let refresh_btn = refresh_btn.with_icon(Icon::Refresh).fit_content(40.0);
        if compact_header {
            let mut header = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_horizontal_padding(0.0, 0.0)
                .with_vertical_padding(theme.tokens.spacing.s3)
                .with_gap(theme.tokens.spacing.s2)
                .push(Box::new(
                    Text::new("Apps")
                        .with_scaled_size(28.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(1),
                ))
                .push(Box::new(
                    Text::new("Useful software from sources you choose.")
                        .with_scaled_size(13.0)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(search))
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(safety_btn))
                        .push(Box::new(remotes_btn))
                        .push(Box::new(refresh_btn)),
                ))
                .push(Box::new(
                    Text::new(&self.catalogue_source_summary())
                        .with_scaled_size(13.0)
                        .with_color(theme.text_tertiary),
                ));
            if self.operation_error.is_some() && !self.items.is_empty() {
                header = header.push(Box::new(
                    Container::new().with_background().with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_gap(theme.tokens.spacing.s1)
                            .push(Box::new(
                                Text::new(operation_error_summary())
                                    .with_scaled_size(13.0)
                                    .with_color(theme.text_dim),
                            ))
                            .push(Box::new(
                                Text::new(&format!(
                                    "Status · {}",
                                    visible_operation_status(&self.status_line)
                                ))
                                .with_scaled_size(12.0)
                                .with_color(theme.text_tertiary),
                            )),
                    )),
                ));
            }
            return Box::new(header);
        }

        let controls = Row::new()
            .no_wrap()
            .push(Box::new(safety_btn))
            .push(Box::new(remotes_btn))
            .push(Box::new(refresh_btn));
        let title_row: Box<dyn Widget> = Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(theme.tokens.spacing.s2)
                                .push(Box::new(
                                    Text::new("App Store")
                                        .with_scaled_size(29.0)
                                        .with_weight(TextWeight::Semibold)
                                        .with_heading_level(1),
                                ))
                                .push(Box::new(
                                    Text::new("Useful software, with source and trust details before you act.")
                                        .with_scaled_size(14.0)
                                        .with_color(theme.text_dim),
                                )),
                        )),
                ))
                .push(Box::new(
                    Container::new()
                        .with_width(318.0)
                        .with_no_padding()
                        .with_child(Box::new(controls)),
                )),
        );
        // Title row and search field are one `s3` apart inside an `s6` band.
        let mut header = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_horizontal_padding(0.0, 0.0)
            .with_vertical_padding(theme.tokens.spacing.s6)
            .with_gap(theme.tokens.spacing.s3);
        header = header.push(title_row);
        header = header.push(Box::new(search));
        if self.operation_error.is_some() && !self.items.is_empty() {
            header = header.push(Box::new(
                Container::new().with_background().with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(theme.tokens.spacing.s1)
                        .push(Box::new(
                            Text::new(operation_error_summary())
                                .with_scaled_size(13.0)
                                .with_color(theme.text_dim),
                        ))
                        .push(Box::new(
                            Text::new(&format!(
                                "Status · {}",
                                visible_operation_status(&self.status_line)
                            ))
                            .with_scaled_size(12.0)
                            .with_color(theme.text_tertiary),
                        )),
                )),
            ));
        }
        Box::new(header)
    }

    fn build_source_filter(&self) -> Dropdown {
        let values = SourceFilter::ALL
            .iter()
            .map(|filter| {
                let display = filter.label().to_owned();
                (filter.label().to_owned(), display)
            })
            .collect();
        let mut dropdown = Dropdown::from_state(Vec::new(), &self.source_filter_state)
            .with_id("store.source-filter")
            .with_accessible_name("App source")
            .on_action(crate::browse_actions::source_binding(
                &self.category_labels(),
            ));
        dropdown.set_value_options(values);
        dropdown.set_selected(self.active_filter.label());
        dropdown
    }

    fn build_category_filter(&self) -> Dropdown {
        let labels = self.category_labels();
        let values = labels
            .iter()
            .map(|label| {
                let display = label.clone();
                (label.clone(), display)
            })
            .collect();
        let mut dropdown = Dropdown::from_state(Vec::new(), &self.category_filter_state)
            .with_id("store.category-filter")
            .with_accessible_name("App category")
            .on_action(crate::browse_actions::category_binding(&labels));
        dropdown.set_value_options(values);
        dropdown.set_selected(self.active_category.as_deref().unwrap_or("All"));
        dropdown
    }

    /// The browse band under the search field: the source and category
    /// pickers with their labels, plus — on a wide catalogue — the source
    /// summary in a third column.
    ///
    /// It is composed as two rows with identical cells (labels above,
    /// controls below) rather than one row of label/control stacks, so the
    /// summary text sits on the dropdowns' centre line instead of being
    /// centred against a taller label+control stack and drifting above the
    /// values it accompanies. Every cell shares one flex basis, so the two
    /// rows resolve to the same column edges.
    fn build_filter_band(&self, theme: &Theme, with_summary: bool) -> Box<dyn Widget> {
        let cell = |child: Box<dyn Widget>| -> Box<dyn Widget> {
            Box::new(
                Container::new()
                    .with_no_padding()
                    // Both pickers plus the row gap fit inside the compact
                    // viewport instead of clipping the category affordance at
                    // the trailing edge.
                    .with_width(FILTER_CELL_MIN_W)
                    .with_flex_grow()
                    .with_child(child),
            )
        };
        let label = |text: &str| -> Box<dyn Widget> {
            Box::new(Text::new(text).with_scaled_size(12.0).dimmed())
        };
        let mut labels = Row::new()
            .no_wrap()
            .push(cell(label("Source")))
            .push(cell(label("Category")));
        let mut controls = Row::new()
            .no_wrap()
            .push(cell(Box::new(self.build_source_filter())))
            .push(cell(Box::new(self.build_category_filter())));
        if with_summary {
            // An empty cell keeps the label row's flex structure identical to
            // the control row's, so the summary column starts where both rows
            // agree.
            labels = labels.push(cell(Box::new(Container::new().with_no_padding())));
            controls = controls.push(cell(Box::new(
                Text::new(&self.catalogue_source_summary())
                    .with_scaled_size(13.0)
                    .with_color(theme.text_dim),
            )));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s1)
                .push(Box::new(labels))
                .push(Box::new(controls)),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the catalogue's empty, paging and responsive-card states are one cohesive view"
    )]
    fn build_list(&self, theme: &Theme) -> Box<dyn Widget> {
        let visible = self.visible_items();
        if visible.is_empty() {
            let (title, message) = if self.items.is_empty() {
                if self.worker_busy {
                    (
                        "Loading apps…",
                        "Checking your configured catalogues.".to_string(),
                    )
                } else if self.operation_error.is_some() {
                    (
                        if self.readable_type_layout() {
                            "Offline"
                        } else {
                            "Catalogues offline"
                        },
                        // With no catalogue cards there is no separate error
                        // masthead: this is the sole actionable failure
                        // surface, paired with the Refresh catalogue CTA.
                        // A bounded worker detail is more useful than a
                        // second generic connection warning.
                        if self.readable_type_layout() {
                            "Couldn’t load apps".into()
                        } else {
                            visible_operation_status(&self.status_line)
                        },
                    )
                } else {
                    (
                        "No apps available",
                        "Add or enable a catalogue you trust, then refresh.".to_string(),
                    )
                }
            } else {
                (
                    "No apps found",
                    "Try a different search or category.".to_string(),
                )
            };
            if self.desktop_browse_layout()
                || (self.readable_type_layout() && self.operation_error.is_none())
            {
                let large_text = self.readable_type_layout();
                let mut panel = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(10.0)
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_height(if large_text { 18.0 } else { 28.0 }),
                    ))
                    .push(Box::new(
                        Text::new(title)
                            .with_scaled_size(if large_text { 20.0 } else { 24.0 })
                            .with_weight(TextWeight::Semibold)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(
                        Text::new(&message)
                            .with_scaled_size(14.0)
                            .with_color(theme.text_dim),
                    ));
                if self.items.is_empty() && !self.worker_busy {
                    let mut refresh =
                        ui::widget::Button::new("store.empty.refresh", "Refresh catalog", None)
                            .primary()
                            .fit_content(40.0)
                            .tooltip("Refresh configured catalogues");
                    if let Some(binding) = refresh_binding() {
                        refresh = refresh.on_action(binding);
                    }
                    panel = panel.push(Box::new(refresh));
                }
                if large_text {
                    // Bound the intrinsic measurement of wrapped failure copy
                    // so it cannot displace the fixed masthead before Taffy
                    // resolves the final narrow width.
                    return Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_height(300.0)
                            .with_child(Box::new(panel)),
                    );
                }
                return Box::new(panel);
            }
            let empty = EmptyState::new(title).with_message(&message);
            // An unavailable or genuinely empty catalogue is recoverable by
            // refreshing. Route the visible CTA through the same one-shot
            // canonical `store.refresh` resource action, so pointer, keyboard,
            // assistive activation and agents all request the same serialized
            // worker operation.
            let empty: Box<dyn Widget> = if self.items.is_empty() && !self.worker_busy {
                let empty = empty
                    .with_action("Refresh catalog")
                    .with_action_id("store.empty.refresh");
                Box::new(match refresh_binding() {
                    Some(binding) => empty.on_action(binding),
                    None => empty,
                })
            } else {
                Box::new(empty)
            };
            // Keep the centred empty state inside the same remaining-height
            // scroll body as a populated catalogue. Without this wrapper its
            // intrinsic height competes with the masthead and can push title
            // and utility controls above the viewport when a CTA is present.
            return Box::new(
                Scrollable::new(ScrollAxis::Vertical)
                    .fill_parent_height()
                    .with_child(empty),
            );
        }
        let page_count = visible.len().div_ceil(PAGE_SIZE).max(1);
        let visible_count = visible.len();
        let page = self.catalog_page.min(page_count.saturating_sub(1));
        let start = page.saturating_mul(PAGE_SIZE);
        // The result area has its own deliberate breathing room.  Do not use
        // `Column`'s default padding here: on a desktop it compounded the
        // masthead inset into a large, accidental blank band between the
        // browse controls and the first section.  A single explicit 28px
        // transition reads as hierarchy without wasting a row of catalogue
        // space on an ordinary 800px-tall window.
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_height(theme.tokens.spacing.s3),
            ));
        if page_count > 1 {
            column = column.push(Box::new(
                Container::new().with_child(Box::new(
                    Text::new(&format!(
                        "Page {} of {} · {} results",
                        page + 1,
                        page_count,
                        visible.len()
                    ))
                    .with_scaled_size(12.0)
                    .with_color(theme.text_dim),
                )),
            ));
        }
        // Installed software is a materially different decision state from
        // software that is merely available to install.  Keep the stable
        // sort from `visible_items`, then give those two honest states their
        // own editorial groups instead of presenting one anonymous wall of
        // identical cards.
        let mut update_cards: Vec<Box<dyn Widget>> = Vec::new();
        let mut ready_cards: Vec<Box<dyn Widget>> = Vec::new();
        let mut discover_cards: Vec<Box<dyn Widget>> = Vec::new();
        let mut search_cards: Vec<Box<dyn Widget>> = Vec::new();
        // A live search is relevance-ranked, so preserving a single result
        // sequence is more useful than interrupting it with availability
        // groups. Idle browsing gets the stronger installed/discover split.
        let group_by_availability = self.group_catalogue_by_availability();
        for item in visible.into_iter().skip(start).take(PAGE_SIZE) {
            let action = item.action_kind(self.flatpak_available);
            let operation_active = self
                .active_operation
                .as_ref()
                .is_some_and(|(id, active_action)| id == &item.id && *active_action == action);
            let click_key = format!("store.app.{}.{}", item.id, action_code(action));
            let select_key = format!("store.detail.{}", item.id);
            let card = crate::widgets::store_card(
                item,
                CardAction {
                    kind: action,
                    key: &click_key,
                    binding: item_action_binding(item, action),
                    in_progress: operation_active,
                },
                CardDetails {
                    key: &select_key,
                    binding: detail_binding(item),
                },
                theme,
                self.images.clone(),
                self.card_layout(),
            );
            if !group_by_availability {
                search_cards.push(card);
            } else if item.installed.update_available() {
                update_cards.push(card);
            } else if item.installed.is_installed() {
                ready_cards.push(card);
            } else {
                discover_cards.push(card);
            }
        }
        if self.stacked_catalogue_layout() {
            if !group_by_availability {
                column = column.push(Box::new(
                    Text::new(&format!("Matches · {visible_count}"))
                        .with_scaled_size(16.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ));
            }
            if group_by_availability && !ready_cards.is_empty() {
                column = column.push(Box::new(
                    Text::new(&format!("Updates ready · {}", update_cards.len()))
                        .with_scaled_size(16.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ));
            }
            for card in update_cards {
                column = column.push(card);
            }
            if group_by_availability && !ready_cards.is_empty() {
                column = column.push(Box::new(
                    Container::new().with_no_padding().with_height(16.0),
                ));
                column = column.push(Box::new(
                    Text::new(&format!("Ready to use · {}", ready_cards.len()))
                        .with_scaled_size(16.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ));
            }
            if !ready_cards.is_empty() {
                column = column.push(Box::new(
                    CardGrid::content_sized(ready_cards)
                        .with_min_card_width(f64::from(CATALOGUE_CONTENT_MAX_WIDTH))
                        .with_gap(f64::from(theme.tokens.spacing.s2)),
                ));
            }
            if group_by_availability && !discover_cards.is_empty() {
                // One `s4` step separates the two stacked groups.
                column = column.push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_height(theme.tokens.spacing.s4),
                ));
                column = column.push(Box::new(
                    Text::new(&format!("Discover more · {}", discover_cards.len()))
                        .with_scaled_size(16.0)
                        .with_weight(TextWeight::Semibold)
                        .with_heading_level(2),
                ));
            }
            let remaining_cards = if group_by_availability {
                discover_cards
            } else {
                search_cards
            };
            if !remaining_cards.is_empty() {
                column = column.push(Box::new(
                    CardGrid::content_sized(remaining_cards)
                        .with_min_card_width(f64::from(CATALOGUE_CONTENT_MAX_WIDTH))
                        .with_gap(f64::from(theme.tokens.spacing.s2)),
                ));
            }
        } else {
            // This is nested in the deliberately inset scroll column above;
            // inheriting `Column`'s default gutter here created a second,
            // invisible desktop margin before every section heading.
            let mut recommended = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(0.0);
            // A section is a heading group (title hugging its caption by `s1`)
            // one `s3` above its card grid; the grid gutters share that `s3`
            // so every card edge, row and column, sits on the same step.
            let make_section = |title: &str, caption: &str, cards: Vec<Box<dyn Widget>>| {
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(theme.tokens.spacing.s3)
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(theme.tokens.spacing.s1)
                                .push(Box::new(
                                    Text::new(title)
                                        .with_scaled_size(17.0)
                                        .with_weight(TextWeight::Semibold)
                                        .with_heading_level(2),
                                ))
                                .push(Box::new(
                                    Text::new(caption)
                                        .with_scaled_size(12.0)
                                        .with_color(theme.text_dim),
                                )),
                        ))
                        .push(Box::new(
                            CardGrid::content_sized(cards)
                                .with_min_card_width(292.0)
                                .with_gap(f64::from(theme.tokens.spacing.s3)),
                        )),
                ) as Box<dyn Widget>
            };
            if group_by_availability && !update_cards.is_empty() {
                let update_count = update_cards.len();
                recommended = recommended.push(make_section(
                    &format!("Updates ready · {update_count}"),
                    "A newer verified version is available on this device.",
                    update_cards,
                ));
                if !ready_cards.is_empty() || !discover_cards.is_empty() {
                    recommended = recommended.push(Box::new(
                        Container::new().with_no_padding().with_height(22.0),
                    ));
                }
            }
            if group_by_availability && !ready_cards.is_empty() {
                let ready_count = ready_cards.len();
                recommended = recommended.push(make_section(
                    &format!("Ready to use · {ready_count}"),
                    "Open or update apps already on this device.",
                    ready_cards,
                ));
                if !discover_cards.is_empty() {
                    // One section step (`s6`) between the two editorial groups.
                    recommended = recommended.push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_height(theme.tokens.spacing.s6),
                    ));
                }
            }
            let remaining_cards = if group_by_availability {
                discover_cards
            } else {
                search_cards
            };
            if !remaining_cards.is_empty() {
                let title = if group_by_availability {
                    format!("Discover · {}", remaining_cards.len())
                } else if self.query.trim().is_empty() {
                    format!("All apps · {}", remaining_cards.len())
                } else {
                    format!("Matches · {}", remaining_cards.len())
                };
                recommended = recommended.push(make_section(
                    &title,
                    if group_by_availability {
                        "Useful additions, available when you need them."
                    } else if self.query.trim().is_empty() {
                        "Available apps, arranged to make the most of this window."
                    } else {
                        "Results stay in relevance order across names, publishers, descriptions, and categories."
                    },
                    remaining_cards,
                ));
            }
            // At a medium desktop width the header deliberately becomes a
            // vertical composition.  The result grid must make the same
            // transition: pinning it to 1120px here previously produced a
            // cut-off third column and a horizontal scrollbar around 920px.
            if self.roomy_desktop() && !self.desktop_browse_layout() {
                column = column.push(Box::new(
                    Row::new()
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(
                            Container::new()
                                .with_width(CATALOGUE_CONTENT_MAX_WIDTH)
                                .with_no_padding()
                                .with_child(Box::new(recommended)),
                        ))
                        .push(Box::new(Spacer::new())),
                ));
            } else {
                column = column.push(Box::new(recommended));
            }
        }
        if page_count > 1 {
            let mut paging = Row::new();
            if page > 0 {
                let mut previous = ui::widget::Button::new("store.page.previous", "Previous", None);
                if let Some(binding) =
                    navigation_binding("previous-page", "Previous catalogue page", "previous-page")
                {
                    previous = previous.on_action(binding);
                }
                paging = paging.push(Box::new(previous.tooltip("Previous catalogue page")));
            }
            if page + 1 < page_count {
                let mut next = ui::widget::Button::new("store.page.next", "Next", None);
                if let Some(binding) =
                    navigation_binding("next-page", "Next catalogue page", "next-page")
                {
                    next = next.on_action(binding);
                }
                paging = paging.push(Box::new(next.primary().tooltip("Next catalogue page")));
            }
            column = column.push(Box::new(paging));
        }
        // A quiet landing zone keeps the transient status pill from sitting
        // over the last app's action when the catalogue is scrolled to end.
        // Sized from the pill band itself, so the reservation follows the
        // text scale instead of restating one height.
        column = column.push(Box::new(Spacer::fixed(Self::status_bubble_band(theme))));
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_content_gutter(0.0)
                .with_persisted_offset(self.scroll_offset.clone())
                .with_child(Box::new(column)),
        )
    }

    /// The label for the floating status bubble: a running action
    /// status while one is set, else the catalog counts (`N apps · M
    /// shown`). Never empty.
    fn footer_label(&self) -> String {
        if self.operation_error.is_some() {
            return "Operation failed".into();
        }
        if self.status_line.is_empty() {
            format!(
                "{} apps · {} shown",
                self.items.len(),
                self.visible_items().len()
            )
        } else {
            self.status_line.clone()
        }
    }

    /// `--shots`: force the desktop status bubble fully visible so a single
    /// headless frame captures the floating chip. Compact layouts keep the
    /// same information in the catalogue surface and never overlay a card.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    /// The pill's type: the theme's small step, handed to the renderer once
    /// (the theme size already carries the text scale; the renderer would
    /// otherwise apply it again).
    fn status_bubble_text_size(theme: &Theme, tr: &TextRenderer) -> f32 {
        tr.renderer_input_style(&ui::text_render::TextStyle::new(
            theme.tokens.typography.scale.sm,
        ))
        .size
    }

    /// Height of the band the catalogue reserves under its last row for the
    /// floating pill: a pill sized for the theme's small type *at the current
    /// text scale* (its label line plus [`ui::status_bubble::pill_size`]'s
    /// padding, never under that helper's 30 px floor) with one `s4` margin
    /// above and below. The shared `pill_size` today pads from the
    /// renderer-relative size, so the pill it paints stays 30 px at every
    /// scale while its glyphs grow (recorded for the toolkit); this band
    /// reserves the height that pill *should* have, so the reservation is
    /// right the day the helper is, and merely generous until then. Public
    /// so the geometry test asserts the reservation, not a retyped number.
    pub fn status_bubble_band(theme: &Theme) -> f32 {
        let pill_h = (theme.tokens.typography.scale.sm + 16.0).max(30.0);
        pill_h + 2.0 * theme.tokens.spacing.s4
    }

    /// Left edge of the catalogue column inside the window: the centred
    /// 1120 px canvas on a roomy desktop, else the root container's inset.
    /// The same decision [`Self::centered_content`] makes, so the pill sits
    /// on the cards' own edge instead of the window's.
    fn catalogue_left_inset(&self, theme: &Theme) -> f64 {
        if self.roomy_desktop() {
            (self.rect.width() - f64::from(CATALOGUE_CONTENT_MAX_WIDTH)) / 2.0
        } else {
            f64::from(theme.padding_standard)
        }
    }

    /// Where the status pill floats for a pill of `(w, h)`: the window's
    /// bottom-**left**, on the catalogue's leading edge, one `s4` above the
    /// bottom. Every card puts its actions at its trailing edge, so the
    /// leading corner is the one place a floating pill — and the hover band
    /// that reveals it — can never land on an Install or Open button. Public
    /// so the geometry test checks the pill against the real card buttons.
    pub fn status_bubble_rect(&self, theme: &Theme, tr: &mut TextRenderer) -> Rect {
        let label = self.footer_label();
        let text_size = Self::status_bubble_text_size(theme, tr);
        self.status_bubble_rect_for(theme, tr, &label, text_size)
    }

    /// [`Self::status_bubble_rect`] for a label and type size the caller has
    /// already resolved, so the draw path measures each once per frame.
    fn status_bubble_rect_for(
        &self,
        theme: &Theme,
        tr: &mut TextRenderer,
        label: &str,
        text_size: f32,
    ) -> Rect {
        let (w, h) = ui::status_bubble::pill_size(label, tr, text_size, 0.0);
        let margin = f64::from(theme.tokens.spacing.s4);
        let x0 = self.rect.x0 + self.catalogue_left_inset(theme);
        Rect::new(x0, self.rect.y1 - margin - h, x0 + w, self.rect.y1 - margin)
    }

    /// Draw the transient floating status bubble at the window's
    /// bottom-left — the reclaimed home of the old footer strip. It
    /// surfaces on a real change (catalog refresh, filter, an action
    /// result) or while the pointer hovers the bottom-left band, then
    /// fades. Colours come from the theme.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        // On a narrow window a bottom-right pill can visually cover the only
        // primary action in the viewport. The catalogue masthead already
        // carries app count, in-flight status and error feedback in document
        // flow, so retaining an overlay there adds no information and harms
        // touch/keyboard discoverability.
        if self.compact_layout()
            || self.worker_busy
            || self.operation_error.is_some()
            || self.items.is_empty()
            || self.focused_detail.is_some()
            || self.safety_open
            || self.remotes_open
        {
            return;
        }
        let now = ui::time::now();
        let label = self.footer_label();
        if label != self.last_status_label {
            self.last_status_label.clone_from(&label);
            self.status_bubble.poke(now);
        }
        if label.is_empty() {
            return;
        }
        let text_size = Self::status_bubble_text_size(theme, tr);
        let rect = self.status_bubble_rect_for(theme, tr, &label, text_size);
        // The reveal band: the pill plus a section step above and a hit
        // target's worth beside it, out to the window's own corner.
        let zone = Rect::new(
            self.rect.x0,
            rect.y0 - f64::from(theme.tokens.spacing.s6),
            rect.x1 + f64::from(theme.tokens.metrics.min_pointer_target),
            self.rect.y1,
        );
        let over = self.shots_status || zone.contains(self.last_cursor);
        self.status_bubble.set_hover(over, now);
        let alpha = if self.reduce_motion {
            if self.shots_status || over || self.worker_busy || self.operation_error.is_some() {
                1.0
            } else {
                0.0
            }
        } else {
            self.status_bubble.alpha(now)
        };
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(
                f64::from(theme.tokens.spacing.s3),
                f64::from(theme.tokens.spacing.s4),
            ),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
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
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        // The catalogue chrome is process-local presentation state. Reconcile
        // the local widget bindings after children have handled input; a
        // remote document can never overwrite another person's search, view,
        // or facet selection.
        let source = self.source_filter_state.with(Clone::clone);
        self.set_active_filter_from_local(&source);
        let category = self.category_filter_state.with(Clone::clone);
        let category = (category != "All").then_some(category);
        let valid = category.as_ref().is_none_or(|candidate| {
            self.category_labels()
                .iter()
                .any(|label| label == candidate)
        });
        self.set_active_category(if valid { category } else { None });
        self.set_query(self.query_state.with(Clone::clone));
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
        self.reduce_motion = theme.reduce_motion;
        self.text_scale = theme.text_scale;
        let mut root = self.compose(theme);
        let child = root.build_layout(taffy, theme, node_map);
        self.root = Some(root);
        let node = parent_node(
            taffy,
            Style {
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
        let was_compact = self.compact_layout();
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if was_compact != self.compact_layout() {
            self.structure_dirty = true;
        }
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
        // Child overlays (any dropdown / popover) first, then the
        // floating status bubble on top of the content.
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
        // Track the pointer so the bubble can reveal on hover of its
        // bottom-right zone (coords are window-space, unscaled).
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
            || (!self.compact_layout()
                && !self.reduce_motion
                && self.status_bubble.needs_frame(ui::time::now()))
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

    /// Hosted-only: in-flight install / refresh status. Never a second
    /// `store.catalog` root — document ops stay on [`tilo_resource::publish`].
    pub(crate) fn live_progress_extras(&self) -> Vec<Resource> {
        if self.status_line.is_empty() {
            return Vec::new();
        }
        vec![Resource::new(
            "store:latest-job",
            "store.install-job",
            self.status_line.clone(),
        )]
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
        let mut moderation =
            Resource::new("store:moderation", "store.moderation", "Trust & moderation");
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
                format!("{} · {}", report.app_id, report.status),
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
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the catalogue door — keep only live install / refresh progress.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        // The framework root already owns the catalogue door.
        if self.is_hosted() && id == "store" {
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
                        "Signature verified · no independent review"
                    }
                    crate::StoreTrust::Flathub => "Flathub distribution · no Tilo review",
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
                    self.set_active_filter_from_local(&source);
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

fn operation_error_summary() -> &'static str {
    "The last operation failed. Refresh the catalogue and try again."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};
    use ui::search::SearchProvider;

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
            assert!(ui::resources::resource(&app, &doc, "store").is_none());
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
                "store",
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
            assert_eq!(presentation.badge.as_deref(), Some("Tilo verified · v1"));
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
                device_state_label(&tilo),
                "Available on demand · fetched when opened\nNot kept on this device"
            );
            assert_eq!(device_state_label(&flatpak), "Not installed");
            assert_eq!(tilo.action_kind(true).label(), "Open");
            assert_eq!(flatpak.action_kind(true).label(), "Install");
        }

        // Case `catalogue_heading_does_not_claim_an_empty_query_is_a_search`.
        {
            assert_eq!(catalogue_region_heading(true, ""), "Browse apps");
            assert_eq!(catalogue_region_heading(false, "  "), "All apps");
            assert_eq!(catalogue_region_heading(false, "notes"), "Search results");
        }

        // Case `catalogue_source_summary_is_honest_about_loaded_sources`.
        {
            let app = seeded_app();
            assert_eq!(
                app.catalogue_source_summary(),
                "3 apps from 2 configured sources"
            );

            let empty = StoreApp::new();
            assert_eq!(
                empty.catalogue_source_summary(),
                "No apps loaded from configured sources yet"
            );
        }
    }

    /// Availability groups, narrow cards, medium summaries and the narrow discover section.
    #[test]
    fn catalogue_grids_keep_cards_apart_at_narrow_and_medium_widths() {
        // Case `availability_groups_avoid_an_odd_medium_desktop_grid`.
        {
            let mut app = seeded_app();
            app.set_layout_width_hint(1_024.0);
            assert!(
                !app.group_catalogue_by_availability(),
                "a medium desktop uses one balanced catalogue grid"
            );

            app.set_layout_width_hint(1_440.0);
            assert!(app.group_catalogue_by_availability());

            app.set_layout_width_hint(480.0);
            assert!(app.group_catalogue_by_availability());

            app.set_query("notes".into());
            assert!(
                !app.group_catalogue_by_availability(),
                "search results remain a single relevance-ranked sequence"
            );
        }

        // Case `narrow_cards_do_not_cover_each_others_actions`.
        {
            fn check(widget: &dyn Widget) {
                if widget.name() == "ContentCard" {
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
            app.set_layout_width_hint(480.0);
            let list = frame_composition(app.compose(&Theme::dark()), 480.0);
            check(list.root_widget());
        }

        // Case `medium_short_viewport_uses_one_width_appropriate_source_summary`.
        {
            let mut app = seeded_app();
            app.set_layout_width_hint(800.0);
            let tree = app.compose(&Theme::dark());
            let summary = app.catalogue_source_summary();
            assert_eq!(accessible_name_count(tree.as_ref(), &summary), 1);
            assert_eq!(accessible_name_count(tree.as_ref(), "App Store"), 1);
            assert_eq!(accessible_name_count(tree.as_ref(), "Apps"), 0);
        }

        // Case `first_empty_busy_catalogue_is_real_chrome_not_a_flat_fill`.
        {
            let mut app = StoreApp::new();
            app.set_worker_busy(true);
            app.set_layout_width_hint(1051.0);
            let tree = app.compose(&Theme::dark());
            assert_eq!(accessible_name_count(tree.as_ref(), "App Store"), 1);
            assert_eq!(accessible_name_count(tree.as_ref(), "Loading apps…"), 1);
        }

        // Case `full_narrow_catalogue_keeps_discover_section_below_ready_cards`.
        {
            let mut app = seeded_app();
            app.set_items(crate::shots::fixture_catalog());
            app.set_layout_width_hint(480.0);
            let frame = frame_composition(app.compose(&Theme::dark()), 480.0);
            let tree = frame.root_widget();

            let discover = accessible_rect(tree, "Discover more · 3")
                .expect("discover heading in full catalogue");
            let mut grids = Vec::new();
            card_grid_extents(tree, &mut grids);
            assert_eq!(grids.len(), 2, "ready and discover grids");
            assert!(
                grids[0].1 <= discover.y0,
                "ready descendants end at {}, over discover heading {:?}; allocated grid {:?}",
                grids[0].1,
                discover,
                grids[0].0
            );
            assert!(
                discover.y1 <= grids[1].0.y0,
                "discover heading {:?} must precede discover grid {:?}",
                discover,
                grids[1].0
            );
        }
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
                        &Theme::dark(),
                        None,
                        if width < 600.0 {
                            CardLayout::Stacked { large_text: false }
                        } else {
                            CardLayout::Wide
                        },
                    );
                    let mut grid = CardGrid::content_sized(vec![card]);
                    let (tree, node) = layout_card_grid(&mut grid, width);
                    let center = control_mut(&mut grid, "select.button")
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
                        .expect("pointer Details");
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
        // Case `full_catalogue_geometry_stays_finite_in_stacked_layouts`.
        {
            for (width, scale) in [(480.0, 1.0), (1_280.0, 2.0)] {
                let mut app = seeded_app();
                app.set_items(crate::shots::fixture_catalog());
                app.set_layout_width_hint(width);
                app.text_scale = scale;
                let frame = frame_composition(
                    app.compose(&Theme::dark().with_text_scale(scale)),
                    width as f32,
                );
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
            app.set_layout_width_hint(1200.0);
            app.items[0].description = "A useful app with a longer description. ".repeat(7);
            let list = frame_composition(app.build_list(&Theme::dark()), 1200.0);
            let mut found = 0;
            check(list.root_widget(), &mut found);
            assert_eq!(found, 3);
        }

        // Case `bounded_desktop_catalogue_keeps_a_balanced_three_column_measure`.
        {
            let mut app = seeded_app();
            app.set_layout_width_hint(1_440.0);
            assert!((app.catalogue_card_min_width() - 348.0).abs() < f64::EPSILON);
            assert_eq!(
                ui::widget::card_columns(1_120.0, app.catalogue_card_min_width(), 16.0),
                3
            );

            app.set_layout_width_hint(1_024.0);
            assert!((app.catalogue_card_min_width() - 330.0).abs() < f64::EPSILON);

            app.text_scale = 2.0;
            assert!((app.catalogue_card_min_width() - 330.0).abs() < f64::EPSILON);
        }

        // Case `large_text_uses_the_safe_stacked_catalogue_layout`.
        {
            let mut app = seeded_app();
            app.set_layout_width_hint(1_280.0);
            assert!(!app.stacked_catalogue_layout());

            app.text_scale = 2.0;
            assert!(app.stacked_catalogue_layout());
        }
    }

    /// Operation notices, root accessibility, trust summary and bounded, printable live status.
    #[test]
    fn status_and_accessibility_surfaces_are_named_bounded_live_regions() {
        // Case `operation_notice_uses_shared_live_banner_semantics`.
        {
            let mut app = seeded_app();
            assert!(app.operation_notice().is_none());

            app.set_status_line("Installing Notes…");
            app.set_worker_busy(true);
            let busy_notice = app
                .operation_notice()
                .expect("busy work has a semantic notice");
            let busy = busy_notice
                .children()
                .first()
                .and_then(|child| child.accessibility())
                .expect("busy banner remains a live child of the progress notice");
            assert_eq!(busy.name.as_deref(), Some("Working in background"));
            assert_eq!(busy.live, Some(Politeness::Polite));
            assert_eq!(busy.description.as_deref(), Some("Installing Notes…"));
            let progress = busy_notice
                .children()
                .get(1)
                .and_then(|child| child.accessibility())
                .expect("progress remains visible to a screen reader");
            assert_eq!(progress.name.as_deref(), Some("Operation in progress"));

            app.set_worker_busy(false);
            app.set_operation_error(Some("signature mismatch".into()));
            let error = app
                .operation_notice()
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

        // Case `trust_sheet_uses_a_scannable_publisher_identity_summary`.
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
            let lines = publisher_identity_lines(&signed);
            assert_eq!(lines.len(), 1);
            assert!(lines[0].contains("ed25519:abababababab…abababababab"));
            assert!(lines[0].contains("64 characters"));
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
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
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

        // Case `typed_search_query_filters_through_local_sync`.
        {
            // The field is window-local, yet still updates the catalogue on the
            // normal widget synchronization path.
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            app.query_state.set("Term".into());
            app.sync(&mut doc, &automerge::ROOT);
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

        // Case `source_filter_is_local_and_re_filters_the_catalogue`.
        {
            let mut app = seeded_app();
            let mut doc = AutoCommit::new();
            app.source_filter_state.set("Tilo".into());
            app.sync(&mut doc, &automerge::ROOT);
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
            let mut doc = AutoCommit::new();
            app.scroll_offset.set((0.0, 240.0));
            app.category_filter_state.set("Games".into());
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.active_category(), Some("Games"));
            assert_eq!(app.scroll_offset.get(), (0.0, 0.0));
            assert_eq!(app.visible_items()[0].name, "Chess");
        }

        // Case `one_window_cannot_navigate_another_with_document_filters`.
        {
            let mut first = seeded_app();
            let mut second = seeded_app();
            let mut doc = AutoCommit::new();
            first.source_filter_state.set("Tilo".into());
            first.category_filter_state.set("Games".into());
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
                status: "Draft · not submitted".into(),
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
                status: "Draft · not submitted".into(),
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
            doc.put(&automerge::ROOT, &key, "Draft · not submitted")
                .unwrap();
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
