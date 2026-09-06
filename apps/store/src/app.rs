//! `StoreApp` — the root widget.
//!
//! Composition sketch:
//!
//! ```text
//! ┌ Store header (search box + refresh)  ─────────────────────────┐
//! │ [    Search…                       ] [ Refresh ]              │
//! ├───────────────────────────────────────────────────────────────┤
//! │ [ All | qOS | Flatpak | Installed ]      ← filter chip row    │
//! ├───────────────────────────────────────────────────────────────┤
//! │ Categories: [ All | Games | Productivity | System | Utils ]   │
//! ├───────────────────────────────────────────────────────────────┤
//! │  ┌───────────────────────────────────────────────────────┐    │
//! │  │  [icon]  Notes                v0.1.0 · qOS main [Add] │    │
//! │  │  [ ⎔  ]  A CRDT notes app                             │    │
//! │  └───────────────────────────────────────────────────────┘    │
//! │  ... more cards, spanning to the window bottom ...            │
//! │                                    ╭─────────────────────────╮ │
//! │                                    │ Installing dev.qos.notes…│ │  ← transient bubble
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

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::qos_actions::{
    ActionAddress, ActionArgs, ActionBinding, ActionContract, ActionParam, ActionSpec,
    Availability, ConfirmationPolicy, ControlInvocation, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Column, Container, EmptyState, Row, ScrollAxis, Scrollable, Spacer, Text, TextInput,
    ToggleGroup, Widget,
};
use ui::{Theme, permissions::PermissionContext, presence::PresenceCtx, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::model::{ActionKind, SourceFilter, StoreItem, StoreSource};
use crate::sources::distinct_categories;

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

/// One-shot CRDT keys the sub-widgets write on click. The names are
/// prefixed with `store.` so the toplevel `state.doc` root object
/// can hold them without colliding with sibling widgets when the
/// app is hosted inside a tiled shell.
const REFRESH_KEY: &str = "store.refresh_pressed";
/// CRDT `Text` object the header search box binds to. `sync` seeds
/// the object (the toolkit's `TextInput` binds to an existing `Text`
/// but never lazily creates one) and reads the typed query back out.
const SEARCH_QUERY_KEY: &str = "store.search_query";
const ACTION_CLICK_KEY_PREFIX: &str = "store.action_click.";
/// Secondary "mark installed" (pin) toggle, one per item id.
const PIN_CLICK_KEY_PREFIX: &str = "store.pin_click.";
/// Secondary "remove" affordance on a cached qOS card, one per item id.
const REMOVE_CLICK_KEY_PREFIX: &str = "store.remove_click.";
const DETAIL_SELECT_KEY_PREFIX: &str = "store.detail_select.";
const DETAIL_BACK_KEY: &str = "store.detail_back";
const REMOTES_OPEN_KEY: &str = "store.remotes_open";
const REMOTES_BACK_KEY: &str = "store.remotes_back";
const REMOTES_REFRESH_KEY: &str = "store.remotes_refresh";
const REMOTES_ADD_KEY: &str = "store.remotes_add";
const REMOTES_REMOVE_KEY_PREFIX: &str = "store.remotes_remove.";
const REMOTE_NAME_KEY: &str = "store.remote_name";
const REMOTE_URL_KEY: &str = "store.remote_url";
const SAFETY_OPEN_KEY: &str = "store.safety_open";
const SAFETY_BACK_KEY: &str = "store.safety_back";
const REPORT_DRAFT_KEY_PREFIX: &str = "store.report_draft.";
const REPORT_RESOURCE_KEY_PREFIX: &str = crate::doc_actions::REPORT_RESOURCE_KEY_PREFIX;
const REPORT_CASE_KEY_PREFIX: &str = "store.report_case.";
const REPORT_NOTE_KEY: &str = "store.report_note";
const PAGE_PREVIOUS_KEY: &str = "store.page_previous";
const PAGE_NEXT_KEY: &str = "store.page_next";
const PAGE_SIZE: usize = 48;

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
            crate::StoreSource::Qos { .. } => "Not disclosed".into(),
        },
        |key| {
            let prefix = key.chars().take(12).collect::<String>();
            format!("ed25519:{prefix}…")
        },
    )
}

/// Render the entire publisher key in deliberately short, inspectable lines.
/// Card summaries use [`short_publisher`], but the anti-scam decision sheet
/// must not replace the actual verified identity with an ellipsis merely to
/// satisfy a narrow layout.
fn publisher_identity_lines(item: &StoreItem) -> Vec<String> {
    let Some(key) = item.publisher_pubkey_hex.as_deref() else {
        return vec!["Publisher identity: not published by this distributor".into()];
    };
    let groups: Vec<&str> = key
        .as_bytes()
        .chunks(8)
        .filter_map(|group| std::str::from_utf8(group).ok())
        .collect();
    let mut lines = vec!["Publisher fingerprint (ed25519):".into()];
    for chunk in groups.chunks(2) {
        lines.push(chunk.join(" "));
    }
    lines
}

fn format_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes == 0 {
        return "Size not published".into();
    }
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
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
) -> Box<dyn Widget> {
    let mut column = Column::new().with_intrinsic_height().push(Box::new(
        Text::new(title).with_size(17.0).with_heading_level(2),
    ));
    for line in lines {
        column = column.push(Box::new(
            Text::new(&line).with_size(12.0).with_color(theme.text_dim),
        ));
    }
    Box::new(
        Container::new()
            .with_background()
            .with_child(Box::new(column)),
    )
}

fn report_envelope(report: &ReportRecord, item: Option<&StoreItem>, note: &str) -> ReportEnvelope {
    ReportEnvelope {
        format: "qos.store.report-envelope.v1".into(),
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
    /// Merged item feed.
    items: Vec<StoreItem>,
    /// Active source-side filter (`All | qOS | Flatpak | Installed`).
    active_filter: SourceFilter,
    /// Active category or `None` for "all categories".
    active_category: Option<String>,
    /// Current search query, persisted through the header's bound
    /// [`TextInput`] and applied to the merged catalogue.
    query: String,
    /// Rolling status line the worker overwrites while an action
    /// runs. Surfaced transiently in the floating status bubble (see
    /// [`draw_status_bubble`](Self::draw_status_bubble)); the catalog
    /// counts stand in while it is empty.
    status_line: String,
    worker_busy: bool,
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
    /// Actions the user has clicked this frame that a worker still
    /// needs to execute (download / install / run / pin / remove).
    /// Drained by the host each frame via [`take_pending_actions`].
    pending_actions: Vec<(String, ActionKind)>,
    pending_refresh: bool,
    pending_lifecycle_actions: Vec<(String, ActionKind, qos_actions::InvocationId)>,
    pending_lifecycle_refresh: Option<qos_actions::InvocationId>,
    pending_lifecycle_remote: Vec<(RemoteIntent, qos_actions::InvocationId)>,
    completed_invocations: Vec<qos_actions::InvocationUpdate>,
    pending_search: Option<String>,
    pending_detail: Option<String>,
    remotes: Vec<crate::flathub_client::Remote>,
    remotes_open: bool,
    safety_open: bool,
    reports: Vec<ReportRecord>,
    report_note: String,
    pending_remote_intents: Vec<RemoteIntent>,
    pending_control_invocation: Option<ControlInvocation>,
    report_export: Option<(
        qos_actions::InvocationId,
        ui::portal::PortalChannel,
        Vec<u8>,
    )>,
    pending_moderation: Vec<(ModerationIntent, qos_actions::InvocationId)>,
    /// Segment index the source-filter `ToggleGroup` last committed.
    /// The `on_change` closure is `'static` and can't borrow `self`,
    /// so it parks the click here; `sync` reads it back and flips
    /// [`active_filter`]. Shared (`Rc`) because the closure is rebuilt
    /// into a fresh `ToggleGroup` every `build_layout`.
    filter_click: Rc<Cell<Option<usize>>>,
    /// Same retained selection bridge as `filter_click`, for categories. Using
    /// one `ToggleGroup` gives the selected category a visible and semantic
    /// state instead of presenting a row of indistinguishable buttons.
    category_click: Rc<Cell<Option<usize>>>,
    /// List position survives the composed widget tree being rebuilt after a
    /// filter, search result, or catalogue update.
    scroll_offset: Rc<Cell<(f32, f32)>>,
    catalog_page: usize,
    /// Transient floating status chip. The catalog counts / action
    /// status surface here — revealed on a real change or while the
    /// pointer hovers the bottom-right band, then faded — in place of a
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
            items: Vec::new(),
            active_filter: SourceFilter::All,
            active_category: None,
            query: String::new(),
            status_line: String::new(),
            worker_busy: false,
            operation_error: None,
            flatpak_available: crate::flathub_client::flatpak_available(),
            focused_detail: None,
            structure_dirty: true,
            rect: Rect::ZERO,
            root: None,
            pending_actions: Vec::new(),
            pending_refresh: false,
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
            report_note: String::new(),
            pending_remote_intents: Vec::new(),
            pending_control_invocation: None,
            report_export: None,
            pending_moderation: Vec::new(),
            filter_click: Rc::new(Cell::new(None)),
            category_click: Rc::new(Cell::new(None)),
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            catalog_page: 0,
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            last_cursor: Point::ZERO,
            shots_status: false,
            images: None,
            reduce_motion: false,
            layout_width_hint: 0.0,
        }
    }

    #[must_use]
    pub fn with_image_store(mut self, images: ui::ImageStore) -> Self {
        self.images = Some(images);
        self
    }

    /// Drain the actions clicked since the last frame. The host's
    /// worker executes each one (off the UI thread) and streams a
    /// fresh catalog + status back.
    pub fn take_pending_actions(&mut self) -> Vec<(String, ActionKind)> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Consume the edge produced by the refresh affordance. The host sends
    /// it to the same serialized worker as mutations, so refreshes cannot
    /// race an install/remove or fan out into duplicate network work.
    pub fn take_refresh_requested(&mut self) -> bool {
        std::mem::take(&mut self.pending_refresh)
    }

    pub fn take_lifecycle_actions(
        &mut self,
    ) -> Vec<(String, ActionKind, qos_actions::InvocationId)> {
        std::mem::take(&mut self.pending_lifecycle_actions)
    }

    pub fn take_lifecycle_refresh(&mut self) -> Option<qos_actions::InvocationId> {
        self.pending_lifecycle_refresh.take()
    }

    pub fn take_lifecycle_remote(&mut self) -> Vec<(RemoteIntent, qos_actions::InvocationId)> {
        std::mem::take(&mut self.pending_lifecycle_remote)
    }

    pub fn complete_lifecycle_invocation(
        &mut self,
        invocation: qos_actions::InvocationId,
        error: Option<String>,
    ) {
        self.completed_invocations.push(match error {
            Some(message) => qos_actions::InvocationUpdate::Failed {
                invocation,
                error: qos_actions::ActionFailure {
                    code: "store.operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
            None => qos_actions::InvocationUpdate::Succeeded {
                invocation,
                output: qos_actions::ActionOutput::default(),
            },
        });
    }

    pub fn take_search_requested(&mut self) -> Option<String> {
        self.pending_search.take()
    }

    pub fn take_detail_requested(&mut self) -> Option<String> {
        self.pending_detail.take()
    }

    pub fn take_remote_intents(&mut self) -> Vec<RemoteIntent> {
        std::mem::take(&mut self.pending_remote_intents)
    }

    pub fn reports(&self) -> &[ReportRecord] {
        &self.reports
    }

    pub fn take_moderation_requests(
        &mut self,
    ) -> Vec<(ModerationIntent, qos_actions::InvocationId)> {
        std::mem::take(&mut self.pending_moderation)
    }

    pub fn set_moderation_case(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        case: &registry_types::ModerationCase,
    ) {
        if let Some(report) = self
            .reports
            .iter_mut()
            .find(|report| report.app_id == case.package_id)
        {
            report.id.clone_from(&case.case_id);
            report.status = moderation_status_label(case.status).into();
        }
        let _ = state.put(
            path,
            format!("{REPORT_CASE_KEY_PREFIX}{}", case.package_id),
            case.case_id.clone(),
        );
        let _ = state.put(
            path,
            format!("{REPORT_RESOURCE_KEY_PREFIX}{}", case.package_id),
            moderation_status_label(case.status),
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

    pub fn show_safety_for_shot(&mut self) {
        self.safety_open = true;
        self.reports.push(ReportRecord {
            id: "report:dev.qos.notes".into(),
            app_id: "dev.qos.notes".into(),
            status: "Draft · not submitted".into(),
        });
        self.structure_dirty = true;
    }

    pub fn show_moderation_case_for_shot(&mut self, status: &str) {
        self.show_safety_for_shot();
        self.report_note = "The app contacted a third-party host while its offline mode was enabled. The exported trace contains the exact destination and time.".into();
        if let Some(report) = self.reports.first_mut() {
            report.id = "case-8d10fce04f126f0c2f293c16".into();
            report.status = status.into();
        }
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
        self.structure_dirty = true;
    }

    /// Replace the item feed. Idempotent — the widget re-derives its
    /// category chip row from `items` on the next `build_layout`.
    pub fn set_items(&mut self, items: Vec<StoreItem>) {
        self.items = items;
        self.structure_dirty = true;
    }

    pub fn items(&self) -> &[StoreItem] {
        &self.items
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
            self.structure_dirty = true;
        }
    }

    pub fn active_category(&self) -> Option<&str> {
        self.active_category.as_deref()
    }

    pub fn set_active_category(&mut self, c: Option<String>) {
        if self.active_category != c {
            self.active_category = c;
            self.catalog_page = 0;
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
            self.pending_search = Some(self.query.clone());
            self.structure_dirty = true;
        }
    }

    pub fn set_status_line(&mut self, s: impl Into<String>) {
        self.status_line = s.into();
    }

    pub fn set_worker_busy(&mut self, busy: bool) {
        if self.worker_busy != busy {
            self.worker_busy = busy;
            self.structure_dirty = true;
        }
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
        let mut hits: Vec<&StoreItem> = self
            .items
            .iter()
            .filter(|i| self.active_filter.accepts(i))
            .filter(|i| match cat {
                Some(c) => i.categories.iter().any(|k| k == c),
                None => true,
            })
            .filter(|i| i.matches_query(&self.query))
            .collect();
        // Ranking: when idle, marked-installed (pinned) apps sort
        // first, then merely-cached, then everything else —
        // alphabetised within each band so the list is stable across
        // refreshes. When a query is active the best fuzzy match wins
        // regardless of install state.
        if self.query.trim().is_empty() {
            hits.sort_by_key(|i| {
                (
                    std::cmp::Reverse(i.install_rank()),
                    i.name.to_ascii_lowercase(),
                )
            });
        } else {
            let q = self.query.clone();
            hits.sort_by_key(|i| std::cmp::Reverse(i.fuzzy_score(&q)));
        }
        hits
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
            .with_vertical_padding(0.0)
            .with_horizontal_padding(0.0, 0.0)
            .push(header);
        if self.worker_busy {
            column = column.push(Box::new(
                Container::new().with_background().with_child(Box::new(
                    Row::new()
                        .push(Box::new(ui::widget::Spinner::new()))
                        .push(Box::new(
                            Text::new(if self.status_line.is_empty() {
                                "Working…"
                            } else {
                                &self.status_line
                            })
                            .with_size(13.0),
                        )),
                )),
            ));
        } else if let Some(error) = &self.operation_error {
            let error_message = format!("The operation did not finish · {error}");
            column = column.push(Box::new(
                Container::new().with_background().with_child(Box::new(
                    Text::new(&error_message)
                        .with_size(13.0)
                        .with_color(theme.text_dim),
                )),
            ));
        }
        if !self.remotes_open && !self.safety_open && self.focused_detail.is_none() {
            column = column
                .push(self.build_filter_row())
                .push(self.build_category_row());
        }
        column = column.push(content);
        Box::new(Container::new().filling().with_child(Box::new(column)))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one cohesive decision sheet keeps trust, package, permission, link, preview, and action sections in display order"
    )]
    fn build_detail(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        let id = self.focused_detail.as_ref()?;
        let item = self.items.iter().find(|item| &item.id == id)?;
        let source = match &item.source {
            crate::StoreSource::Qos { registry_name } => format!("qOS · {registry_name}"),
            crate::StoreSource::Flathub => "Flatpak · Flathub".to_string(),
        };
        let locally_retained = matches!(
            &item.source,
            crate::StoreSource::Qos { registry_name } if registry_name == "Installed on this device"
        );
        let trust_summary = if locally_retained {
            "Verified when installed. This local app remains manageable, but the registry's current metadata is unavailable, so update availability is unknown."
        } else {
            match item.trust {
                crate::StoreTrust::PublisherSigned => {
                    "Identity and package integrity verified. The registry owner allowed this publisher. This is not an independent quality or safety review."
                }
                crate::StoreTrust::Flathub => {
                    "Distributed by Flathub. qOS has not independently reviewed this app, and Flatpak apps run under their own sandbox policy."
                }
            }
        };
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                ui::widget::Button::new("store.detail.back", "Back to apps", Some(DETAIL_BACK_KEY))
                    .ghost()
                    .tooltip("Return to the catalogue"),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(crate::widgets::Artwork::new(
                        self.images.clone(),
                        item.icon_url.as_deref(),
                        &item.name,
                        72.0,
                        72.0,
                    )))
                    .push(Box::new(
                        Column::new()
                            .with_no_padding()
                            .push(Box::new(Text::new(&item.name).with_size(24.0)))
                            .push(Box::new(
                                Text::new(&source)
                                    .with_size(12.0)
                                    .with_color(theme.text_dim),
                            )),
                    )),
            ))
            .push(Box::new(Text::new(&item.description).with_size(14.0)));
        let action = item.action_kind(self.flatpak_available);
        let primary_key = format!("{}{}", ACTION_CLICK_KEY_PREFIX, item.id);
        let mut primary = ui::widget::Button::new(&primary_key, action.label(), Some(&primary_key));
        if let Some(binding) = item_action_binding(item, action) {
            primary = primary.on_action(binding);
        }
        let mut actions = Row::new().push(Box::new(primary.primary().tooltip(format!(
            "{} {}",
            action.label(),
            item.name
        ))));
        if let Some(pin_action) = item.pin_action() {
            let pin_key = format!("{}{}", PIN_CLICK_KEY_PREFIX, item.id);
            let mut button = ui::widget::Button::new(&pin_key, pin_action.label(), Some(&pin_key));
            if let Some(binding) = item_action_binding(item, pin_action) {
                button = button.on_action(binding);
            }
            actions = actions.push(Box::new(button.ghost().tooltip(format!(
                "{} {}",
                pin_action.label(),
                item.name
            ))));
        }
        if let Some(remove_action) = item.remove_action() {
            let remove_key = format!("{}{}", REMOVE_CLICK_KEY_PREFIX, item.id);
            let mut button = ui::widget::Button::new(&remove_key, "Remove", Some(&remove_key));
            if let Some(binding) = item_action_binding(item, remove_action) {
                button = button.on_action(binding);
            }
            actions = actions.push(Box::new(
                button.ghost().tooltip(format!("Remove {}", item.name)),
            ));
        }
        let report_key = format!("{REPORT_DRAFT_KEY_PREFIX}{}", item.id);
        let mut report_button =
            ui::widget::Button::new(&report_key, "Create report draft", Some(&report_key));
        if let Some(binding) = report_binding(item) {
            report_button = report_button.on_action(binding);
        }
        let mut trust_lines = vec![trust_summary.to_string()];
        trust_lines.extend(publisher_identity_lines(item));
        column = column.push(Box::new(actions)).push(detail_section(
            "Trust & safety",
            trust_lines,
            theme,
        ));

        let version_lines = if item.versions.is_empty() {
            vec!["No version metadata published.".into()]
        } else {
            item.versions
                .iter()
                .map(|version| {
                    let release = if version.released_at_unix == 0 {
                        "release date not published".into()
                    } else {
                        format!("released at Unix time {}", version.released_at_unix)
                    };
                    let hash = version.root_hash_hex.as_deref().map_or_else(
                        || "content hash supplied by external distributor".into(),
                        |hash| format!("BLAKE3 {}…", hash.chars().take(16).collect::<String>()),
                    );
                    format!(
                        "{} · {} · {} · {}",
                        version.version,
                        format_size(version.size_bytes),
                        release,
                        hash
                    )
                })
                .collect()
        };
        column = column.push(detail_section("Versions & package", version_lines, theme));

        let installed = match &item.installed {
            crate::InstalledState::Not => "Not installed".to_string(),
            crate::InstalledState::Installed {
                version,
                available,
                pinned,
            } => format!(
                "Installed {version} · available {available} · {}",
                if *pinned {
                    "kept on this device"
                } else {
                    "cache may be reclaimed"
                },
            ),
        };
        column = column.push(detail_section(
            "Permissions & behavior",
            [
                installed,
                "Permission details are not published by the current catalogue protocol. qOS still enforces the package sandbox and asks before sensitive capabilities, but this missing pre-install declaration is a registry limitation.".into(),
                "Changelog: not published as signed catalogue metadata.".into(),
            ],
            theme,
        ));

        column = column.push(detail_section(
            "Project links",
            [
                format!(
                    "Homepage: {}",
                    item.homepage_url.as_deref().unwrap_or("Not published")
                ),
                format!(
                    "Source: {}",
                    item.source_url.as_deref().unwrap_or("Not published")
                ),
            ],
            theme,
        ));

        if item.screenshot_urls.is_empty() {
            column = column.push(detail_section(
                "Preview gallery",
                ["No preview images were published for this app.".into()],
                theme,
            ));
        } else {
            let mut gallery = Row::new();
            for (index, preview) in item.screenshot_urls.iter().take(3).enumerate() {
                gallery = gallery.push(Box::new(crate::widgets::Artwork::new(
                    self.images.clone(),
                    Some(preview),
                    &format!("{} preview {}", item.name, index + 1),
                    248.0,
                    140.0,
                )));
            }
            column = column
                .push(Box::new(
                    Text::new("Preview gallery")
                        .with_size(17.0)
                        .with_heading_level(2),
                ))
                .push(Box::new(gallery));
        }

        column = column.push(Box::new(
            Row::new()
                .push(Box::new(
                    report_button.tooltip("Create a local, collaborative report draft"),
                ))
                .push(Box::new(
                    ui::widget::Button::new(
                        "store.safety.detail",
                        "Moderation details",
                        Some(SAFETY_OPEN_KEY),
                    )
                    .ghost()
                    .tooltip("Review the Store trust and moderation model"),
                )),
        ));
        Some(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_background()
                        .with_child(Box::new(column)),
                )),
        ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete responsive moderation surface is composed in one place so draft, case, appeal, and explanatory ordering cannot drift"
    )]
    fn build_safety_center(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                ui::widget::Button::new("store.safety.back", "Back", Some(SAFETY_BACK_KEY))
                    .ghost()
                    .tooltip("Return to the previous Store view"),
            ))
            .push(Box::new(Text::new("Trust & moderation").with_size(24.0)))
            .push(Box::new(
                Text::new("Signed reports, accountable decisions and publisher appeals")
                    .with_size(14.0)
                    .with_color(theme.text_dim),
            ));
        if self.reports.is_empty() {
            column = column.push(Box::new(
                EmptyState::new("No report drafts")
                    .with_message("Open an app and choose Create report draft."),
            ));
        } else {
            column = column
                .push(Box::new(Text::new("Describe the concern").with_size(15.0)))
                .push(Box::new(TextInput::new(vec![])
                    .with_id("store.report.note")
                    .with_on_change_path(REPORT_NOTE_KEY)
                    .with_placeholder("What happened, and what evidence can verify it?")))
                .push(Box::new(Text::new("Saved collaboratively in this Store document. Submit sends the bounded note digest and summary to the app's registry; Export keeps a local evidence copy.")
                    .with_size(11.0).with_color(theme.text_dim)));
            for report in &self.reports {
                let item = self.items.iter().find(|item| item.id == report.app_id);
                let mut export = ui::widget::Button::new(
                    &format!("{}.export", report.id),
                    "Export evidence",
                    None,
                );
                if let Some(binding) = report_export_binding(
                    report,
                    self.items.iter().find(|item| item.id == report.app_id),
                    &self.report_note,
                ) {
                    export = export.on_action(binding);
                }
                let mut action_buttons: Vec<Box<dyn Widget>> = vec![Box::new(export.tooltip(
                    "Save a bounded JSON evidence envelope through the system file picker",
                ))];
                if let Some(item) = item
                    && let StoreSource::Qos { registry_name } = &item.source
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
                                self.report_note.as_str(),
                            ),
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
                            report_submit_binding(report, item, &self.report_note, registry_name)
                        {
                            submit = submit.on_action(binding);
                        }
                        action_buttons.push(Box::new(submit));
                    }
                }
                let actions: Box<dyn Widget> = if self.layout_width_hint < 720.0 {
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
                column = column.push(Box::new(
                    Container::new().with_background().with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .push(Box::new(Text::new(&report.app_id).with_size(14.0)))
                            .push(Box::new(
                                Text::new(&report.status)
                                    .with_size(11.0)
                                    .with_color(theme.text_dim),
                            ))
                            .push(actions),
                    )),
                ));
            }
        }
        column = column
            .push(detail_section("How reports and appeals work", [
                "Reports are signed by your workspace identity. Publisher replies and appeals must match the package publisher key; reviewer decisions must match the pinned registry owner.".into(),
                "Drafts stay collaborative. A submission receipt does not replace the last verified status: status changes appear only after the Store verifies the complete event chain against an owner-signed snapshot. Offline or refused requests remain drafts and can be retried.".into(),
            ], theme))
            .push(detail_section("Verified identity & integrity", [
                "qOS packages are bound to a publisher key, app id, version, exact size and content hash. A pinned registry owner controls which publishers may list apps.".into(),
            ], theme))
            .push(detail_section("Review is a separate claim", [
                "A valid package signature does not mean an app is useful, private or honest. Moderation decisions are separately signed; the Store never invents a review badge.".into(),
                "Public moderation events form a numbered signed hash chain. The Store derives case status itself, checks publisher and reviewer authorization, and requires the final head committed by the registry owner's current snapshot.".into(),
            ], theme))
            .push(detail_section("Publishing guidelines", [
                "Listings must accurately describe behavior, identify paid or remote-service dependencies, request only necessary permissions, preserve user data during updates, provide an accessible UI, and avoid impersonation, manipulation and undisclosed tracking.".into(),
            ], theme));
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_background()
                        .with_child(Box::new(column)),
                )),
        )
    }

    fn build_remotes(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                ui::widget::Button::new("store.remotes.back", "Back to apps", Some(REMOTES_BACK_KEY))
                    .ghost(),
            ))
            .push(Box::new(Text::new("Flatpak remotes").with_size(22.0)))
            .push(Box::new(
                Text::new("Remote configuration applies to this machine and runs through the package worker.")
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ));
        for remote in &self.remotes {
            let label = format!(
                "{} · {}{}",
                remote.title,
                remote.url,
                if remote.disabled { " · disabled" } else { "" }
            );
            let key = format!("{}{}", REMOTES_REMOVE_KEY_PREFIX, remote.name);
            column = column.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&label).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        ui::widget::Button::new(&key, "Remove", Some(&key)).ghost(),
                    )),
            ));
        }
        let form = Row::new()
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("store.remote.name")
                    .with_on_change_path(REMOTE_NAME_KEY)
                    .with_placeholder("Remote name"),
            ))
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("store.remote.url")
                    .with_on_change_path(REMOTE_URL_KEY)
                    .with_placeholder("Repository URL"),
            ))
            .push(Box::new(
                ui::widget::Button::new("store.remote.add", "Add remote", Some(REMOTES_ADD_KEY))
                    .primary(),
            ))
            .push(Box::new(ui::widget::Button::new(
                "store.remote.refresh",
                "Refresh",
                Some(REMOTES_REFRESH_KEY),
            )));
        column = column.push(Box::new(form));
        Box::new(
            Container::new()
                .filling()
                .with_background()
                .with_child(Box::new(column)),
        )
    }

    #[allow(
        clippy::unused_self,
        reason = "header construction deliberately remains an instance layout method alongside sibling Store views"
    )]
    fn build_header(&self, theme: &Theme) -> Box<dyn Widget> {
        // Live search box: a real `TextInput` bound to a CRDT `Text`
        // object at `SEARCH_QUERY_KEY`. `sync` seeds the object and
        // reads the typed value back into `self.query`, which drives
        // `visible_items`' fuzzy filter.
        let search = TextInput::new(vec![])
            .with_id("store.search")
            .with_on_change_path(SEARCH_QUERY_KEY)
            .with_placeholder("Search apps…");
        // Refresh button writes REFRESH_KEY; the outer host drives
        // the real refetch when it sees the bit go high.
        let mut refresh_btn =
            ui::widget::Button::new("store.refresh", "Refresh", Some(REFRESH_KEY));
        if let Some(binding) = refresh_binding() {
            refresh_btn = refresh_btn.on_action(binding);
        }
        let refresh_btn = refresh_btn.tooltip("Refresh configured catalogues");
        let remotes_btn =
            ui::widget::Button::new("store.remotes", "Remotes", Some(REMOTES_OPEN_KEY))
                .tooltip("Manage Flatpak remotes");
        let safety_btn = ui::widget::Button::new("store.safety", "Trust", Some(SAFETY_OPEN_KEY))
            .tooltip("Understand signatures, review, reports and appeals");
        let title_row = Row::new()
            .push(Box::new(Text::new("Apps").with_size(24.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(safety_btn))
            .push(Box::new(remotes_btn))
            .push(Box::new(refresh_btn));
        let header = Column::new()
            .with_intrinsic_height()
            .push(Box::new(title_row))
            .push(Box::new(
                Text::new("Find trusted tools and open them instantly.")
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(search));
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(header)),
        )
    }

    fn build_filter_row(&self) -> Box<dyn Widget> {
        let segs: Vec<Segment> = SourceFilter::ALL
            .iter()
            .map(|f| Segment::new(f.label()))
            .collect();
        let selected = SourceFilter::ALL
            .iter()
            .position(|f| *f == self.active_filter)
            .unwrap_or(0);
        // The `on_change` callback runs inside `handle_event` and is
        // `'static`, so it can't borrow `self`. It parks the clicked
        // segment index in the shared `filter_click` cell; `sync`
        // reads it back next frame and flips `active_filter`, which
        // re-filters the catalog.
        let sink = self.filter_click.clone();
        let group = ToggleGroup::new(segs, selected).on_view("store.source-filter", move |idx| {
            sink.set(Some(idx));
        });
        Box::new(Container::new().with_background().with_child(Box::new(
            Scrollable::new(ScrollAxis::Horizontal).with_child(Box::new(group)),
        )))
    }

    fn build_category_row(&self) -> Box<dyn Widget> {
        let labels = self.category_labels();
        let selected = self
            .active_category
            .as_ref()
            .and_then(|active| labels.iter().position(|label| label == active))
            .unwrap_or(0);
        let segments = labels.into_iter().map(Segment::new).collect();
        let sink = self.category_click.clone();
        let group =
            ToggleGroup::new(segments, selected).on_view("store.category-filter", move |idx| {
                sink.set(Some(idx));
            });
        Box::new(Container::new().with_background().with_child(Box::new(
            Scrollable::new(ScrollAxis::Horizontal).with_child(Box::new(group)),
        )))
    }

    fn build_list(&self, theme: &Theme) -> Box<dyn Widget> {
        let visible = self.visible_items();
        if visible.is_empty() {
            let (title, message) = if self.items.is_empty() {
                if self.worker_busy {
                    ("Loading apps…", "Checking your trusted catalogues.")
                } else if self.operation_error.is_some() {
                    (
                        "Catalogues unavailable",
                        "Check your connection or configured source, then refresh.",
                    )
                } else {
                    (
                        "No apps available",
                        "Add or enable a trusted catalogue, then refresh.",
                    )
                }
            } else {
                ("No apps found", "Try a different search or category.")
            };
            return Box::new(EmptyState::new(title).with_message(message));
        }
        let page_count = visible.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.catalog_page.min(page_count.saturating_sub(1));
        let start = page.saturating_mul(PAGE_SIZE);
        let mut column = Column::new().with_intrinsic_height();
        if page_count > 1 {
            column = column.push(Box::new(
                Container::new().with_child(Box::new(
                    Text::new(&format!(
                        "Page {} of {} · {} results",
                        page + 1,
                        page_count,
                        visible.len()
                    ))
                    .with_size(12.0)
                    .with_color(theme.text_dim),
                )),
            ));
        }
        for item in visible.into_iter().skip(start).take(PAGE_SIZE) {
            let action = item.action_kind(self.flatpak_available);
            let click_key = format!("{}{}", ACTION_CLICK_KEY_PREFIX, item.id);
            let select_key = format!("store.detail_select.{}", item.id);
            column = column.push(crate::widgets::store_card(
                item,
                action,
                &select_key,
                &click_key,
                theme,
                self.images.clone(),
                item_action_binding(item, action),
                detail_binding(item),
                self.compact_layout(),
            ));
        }
        if page_count > 1 {
            let mut paging = Row::new();
            if page > 0 {
                paging = paging.push(Box::new(
                    ui::widget::Button::new(
                        "store.page.previous",
                        "Previous",
                        Some(PAGE_PREVIOUS_KEY),
                    )
                    .tooltip("Previous catalogue page"),
                ));
            }
            if page + 1 < page_count {
                paging = paging.push(Box::new(
                    ui::widget::Button::new("store.page.next", "Next", Some(PAGE_NEXT_KEY))
                        .primary()
                        .tooltip("Next catalogue page"),
                ));
            }
            column = column.push(Box::new(paging));
        }
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_content_gutter(14.0)
                .with_persisted_offset(self.scroll_offset.clone())
                .with_child(Box::new(column)),
        )
    }

    /// The label for the floating status bubble: a running action
    /// status while one is set, else the catalog counts (`N apps · M
    /// shown`) — the same genuinely-contextual info the old permanent
    /// footer strip carried. Never empty.
    fn footer_label(&self) -> String {
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

    /// `--shots`: force the status bubble fully visible so a single
    /// headless frame captures the floating chip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(std::time::Instant::now());
    }

    /// Draw the transient floating status bubble at the window's
    /// bottom-right — the reclaimed home of the old footer strip. It
    /// surfaces on a real change (catalog refresh, filter, an action
    /// result) or while the pointer hovers the bottom-right band, then
    /// fades. Colours come from the theme.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.worker_busy
            || self.operation_error.is_some()
            || self.focused_detail.is_some()
            || self.safety_open
            || self.remotes_open
        {
            return;
        }
        let now = std::time::Instant::now();
        let label = self.footer_label();
        if label != self.last_status_label {
            self.last_status_label.clone_from(&label);
            self.status_bubble.poke(now);
        }
        if label.is_empty() {
            return;
        }
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
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
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    // ---- CRDT sync helpers ----

    /// Read the one-shot click keys the sub-widgets write and turn
    /// them into filter / category / action mutations. Returns
    /// `true` if anything changed (used by callers as a signal to
    /// re-render).
    fn drain_click_keys(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        // Source-filter chips are wired through the `filter_click`
        // cell (see `sync`), not a CRDT key, so there is no filter
        // loop here.
        let mut any = self.drain_primary_action_click(state, path);
        any |= self.drain_secondary_clicks(state, path);
        any |= self.drain_detail_back_click(state, path);
        any |= self.drain_safety_clicks(state, path);
        any |= self.drain_page_clicks(state, path);
        any |= self.drain_remotes_clicks(state, path);
        any |= self.drain_refresh_click(state, path);
        any
    }

    /// Primary action click (the big button on each card).
    fn drain_primary_action_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let Some((id, action)) = self.take_action_click(state, path) else {
            return false;
        };
        self.status_line = match action {
            ActionKind::Open => format!("Opening {id}…"),
            ActionKind::Update => format!("Updating {id}…"),
            ActionKind::Remove => format!("Removing {id}…"),
            ActionKind::MarkInstalled => format!("Marking {id} installed…"),
            ActionKind::Unmark => format!("Unmarking {id}…"),
            ActionKind::FlatpakInstall => format!("Installing Flatpak {id}…"),
            ActionKind::FlatpakUpdate => format!("Updating Flatpak {id}…"),
            // Named as unconfined in the status line, not only in
            // the launcher's log: this is the moment the user
            // chooses to start it.
            ActionKind::FlatpakOpen => format!("Opening {id} (unconfined host app)…"),
            ActionKind::FlatpakRemove => format!("Removing Flatpak {id}…"),
            ActionKind::NeedsFlatpakHost => {
                "Install flatpak on the host to use this action.".to_string()
            }
        };
        if action != ActionKind::NeedsFlatpakHost {
            self.pending_actions.push((id, action));
        }
        true
    }

    /// Secondary clicks: the pin ("mark installed") toggle, the Remove
    /// affordance on cached qOS cards, and the detail-select tap.
    /// Collect first so the item borrow ends before we mutate `self`.
    fn drain_secondary_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        let mut secondary: Vec<(String, ActionKind)> = Vec::new();
        for item in &self.items {
            let pin_key = format!("{}{}", PIN_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &pin_key)
                && let Some(action) = item.pin_action()
            {
                secondary.push((item.id.clone(), action));
            }
            let rm_key = format!("{}{}", REMOVE_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &rm_key) {
                // The item decides *how* it is removed. A Flatpak
                // removed with `qos-install uninstall` would report
                // "no such package installed" and leave it there.
                if let Some(action) = item.remove_action() {
                    secondary.push((item.id.clone(), action));
                }
            }
            let detail_key = format!("{}{}", DETAIL_SELECT_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &detail_key) {
                self.focused_detail = Some(item.id.clone());
                self.pending_detail = Some(item.id.clone());
                self.status_line = format!("Loading details for {}…", item.name);
                self.structure_dirty = true;
                any = true;
            }
        }
        for (id, action) in secondary {
            self.status_line = format!("{} {}…", action.label(), id);
            self.pending_actions.push((id, action));
            any = true;
        }
        any
    }

    fn drain_detail_back_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        if take_bool_edge(state, path, DETAIL_BACK_KEY) {
            self.focused_detail = None;
            self.structure_dirty = true;
            true
        } else {
            false
        }
    }

    fn drain_safety_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        if take_bool_edge(state, path, SAFETY_OPEN_KEY) {
            self.safety_open = true;
            self.structure_dirty = true;
            any = true;
        }
        if take_bool_edge(state, path, SAFETY_BACK_KEY) {
            self.safety_open = false;
            self.structure_dirty = true;
            any = true;
        }
        let ids = self
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        for app_id in ids {
            let key = format!("{REPORT_DRAFT_KEY_PREFIX}{app_id}");
            if take_bool_edge(state, path, &key) {
                let resource_key = format!("{REPORT_RESOURCE_KEY_PREFIX}{app_id}");
                let _ = state.put(path, resource_key, "Draft · not submitted");
                if !self.reports.iter().any(|report| report.app_id == app_id) {
                    self.reports.push(ReportRecord {
                        id: format!("report:{app_id}"),
                        app_id: app_id.clone(),
                        status: "Draft · not submitted".into(),
                    });
                }
                self.status_line = format!("Created a local report draft for {app_id}.");
                self.safety_open = true;
                self.structure_dirty = true;
                any = true;
            }
        }
        any
    }

    fn sync_report_resources(&mut self, state: &AutoCommit, path: &ObjId) {
        let mut reports = state
            .keys(path)
            .filter_map(|key| {
                let app_id = key.strip_prefix(REPORT_RESOURCE_KEY_PREFIX)?.to_string();
                let status = state
                    .get(path, &key)
                    .ok()
                    .flatten()?
                    .0
                    .to_str()?
                    .to_string();
                Some(ReportRecord {
                    id: state
                        .get(path, format!("{REPORT_CASE_KEY_PREFIX}{app_id}"))
                        .ok()
                        .flatten()
                        .and_then(|(value, _)| value.to_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("report:{app_id}")),
                    app_id,
                    status,
                })
            })
            .collect::<Vec<_>>();
        reports.sort_by(|a, b| a.app_id.cmp(&b.app_id));
        if reports != self.reports && !reports.is_empty() {
            self.reports = reports;
            self.structure_dirty = true;
        }
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
            ui::portal::PortalOutcome::Opened(path) => match std::fs::write(&path, bytes) {
                Ok(()) => {
                    self.status_line = format!("Exported report evidence to {}.", path.display());
                    qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput::default(),
                    }
                }
                Err(error) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "store.report_export_failed".into(),
                        message: format!("Could not export report evidence: {error}"),
                        details: None,
                        retryable: true,
                    },
                },
            },
            ui::portal::PortalOutcome::Cancelled => qos_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Export cancelled".into()),
            },
            ui::portal::PortalOutcome::Failed(message) => qos_actions::InvocationUpdate::Failed {
                invocation,
                error: qos_actions::ActionFailure {
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

    fn drain_page_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        if take_bool_edge(state, path, PAGE_PREVIOUS_KEY) {
            self.catalog_page = self.catalog_page.saturating_sub(1);
            self.scroll_offset.set((0.0, 0.0));
            self.structure_dirty = true;
            true
        } else if take_bool_edge(state, path, PAGE_NEXT_KEY) {
            let pages = self.visible_items().len().div_ceil(PAGE_SIZE).max(1);
            self.catalog_page = (self.catalog_page + 1).min(pages.saturating_sub(1));
            self.scroll_offset.set((0.0, 0.0));
            self.structure_dirty = true;
            true
        } else {
            false
        }
    }

    /// Remotes view: open/back navigation, a refresh tap, per-remote
    /// removal, and the add-remote form submit.
    fn drain_remotes_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        if take_bool_edge(state, path, REMOTES_OPEN_KEY) {
            self.remotes_open = true;
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new("store:remotes", "refresh"),
                arguments: serde_json::Map::new(),
            });
            self.structure_dirty = true;
            any = true;
        }
        if take_bool_edge(state, path, REMOTES_BACK_KEY) {
            self.remotes_open = false;
            self.structure_dirty = true;
            any = true;
        }
        if take_bool_edge(state, path, REMOTES_REFRESH_KEY) {
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new("store:remotes", "refresh"),
                arguments: serde_json::Map::new(),
            });
            any = true;
        }
        let remote_names: Vec<String> = self
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect();
        for name in remote_names {
            let key = format!("{REMOTES_REMOVE_KEY_PREFIX}{name}");
            if take_bool_edge(state, path, &key) {
                self.pending_control_invocation = Some(ControlInvocation {
                    address: ActionAddress::new(format!("remote:{name}"), "remove"),
                    arguments: serde_json::Map::new(),
                });
                any = true;
            }
        }
        if take_bool_edge(state, path, REMOTES_ADD_KEY) {
            let name = text_at(state, path, REMOTE_NAME_KEY);
            let url = text_at(state, path, REMOTE_URL_KEY);
            if !name.trim().is_empty() && validate_remote_url(&url).is_ok() {
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
            }
            any = true;
        }
        any
    }

    fn drain_refresh_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        if take_bool_edge(state, path, REFRESH_KEY) {
            self.status_line = "Refreshing…".to_string();
            self.pending_refresh = true;
            true
        } else {
            false
        }
    }

    /// Look for a `store.action_click.<id> = true` key. Returns the
    /// id + the action the item's action-kind resolved to.
    fn take_action_click(
        &self,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> Option<(String, ActionKind)> {
        for item in &self.items {
            let key = format!("{}{}", ACTION_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &key) {
                return Some((item.id.clone(), item.action_kind(self.flatpak_available)));
            }
        }
        None
    }
}

/// Resolve (or lazily create) the `Text` object at `path`.`key`. The
/// toolkit's `TextInput` binds to an existing `Text` but never creates
/// one, so the host must seed it before the input syncs.
///
/// # Panics
/// Never in practice: `root` is always a live Map (the widget's own CRDT
/// path), so `put_object` on it cannot fail short of a corrupted document.
#[allow(
    clippy::expect_used,
    reason = "root is always a live Map (the widget's own CRDT path), so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
)]
fn ensure_text_obj(state: &mut AutoCommit, root: &ObjId, key: &str) -> ObjId {
    if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
        return id;
    }
    state
        .put_object(root, key, ObjType::Text)
        .expect("put_object Text never fails")
}

fn text_at(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    state
        .get(root, key)
        .ok()
        .flatten()
        .and_then(|(value, id)| matches!(value, Value::Object(ObjType::Text)).then_some(id))
        .and_then(|id| state.text(&id).ok())
        .unwrap_or_default()
}

/// Read a `path`.`key` boolean and clear it in one shot. `true`
/// return means the key was true; the CRDT is left with the key
/// false so the next frame sees a fresh edge.
fn take_bool_edge(state: &mut AutoCommit, path: &ObjId, key: &str) -> bool {
    let hit = matches!(
        state.get(path, key),
        Ok(Some((Value::Scalar(s), _)))
            if matches!(s.as_ref(), ScalarValue::Boolean(true))
    );
    if hit {
        let _ = state.put(path, key, ScalarValue::Boolean(false));
    }
    hit
}

impl Widget for StoreApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "store.refresh",
            "store.app",
            "store.remote",
            "store.view",
            "store.report-export",
            "store.moderation",
            "store.moderation-status",
            "store.moderation-appeal",
        ]
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the hierarchical Store resource tree is assembled in one place so published parent-child relationships remain auditable"
    )]
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut root = Resource::new("store", "store.catalog", "App Store");
        if let Ok(refresh) = ActionSpec::operation(
            "refresh",
            "Refresh catalog",
            OperationKind::Effect,
            "store.refresh",
            &"",
        ) {
            root.actions.push(refresh.with_contract(ActionContract {
                retry: RetryPolicy::Idempotent,
                execution: ExecutionMode::Background { cancellable: false },
                result: ResultSpec::Job,
                ..ActionContract::default()
            }));
        }
        let mut publishers: HashMap<String, Resource> = HashMap::new();
        for item in &self.items {
            let action = item.action_kind(self.flatpak_available);
            let code = action_code(action);
            let payload = vec![item.id.clone(), code.to_string()];
            let mut resource =
                Resource::new(format!("app:{}", item.id), "store.app", item.name.clone());
            if let Ok(show) = ActionSpec::operation(
                "show-details",
                "Show app details",
                OperationKind::View,
                "store.view",
                &item.id,
            ) {
                resource.actions.push(show);
            }
            if let Ok(spec) = ActionSpec::operation(
                code,
                action.label(),
                OperationKind::Effect,
                "store.app",
                &payload,
            ) {
                let destructive = matches!(action, ActionKind::Remove | ActionKind::FlatpakRemove);
                resource.actions.push(spec.with_contract(ActionContract {
                    availability: if action.requires_flatpak_host() && !self.flatpak_available {
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
            if let Some(pin) = item.pin_action() {
                let pin_code = action_code(pin);
                let payload = vec![item.id.clone(), pin_code.to_string()];
                if let Ok(spec) = ActionSpec::operation(
                    pin_code,
                    pin.label(),
                    OperationKind::Effect,
                    "store.app",
                    &payload,
                ) {
                    resource.actions.push(spec.with_contract(ActionContract {
                        risk: RiskLevel::Low,
                        confirmation: ConfirmationPolicy::None,
                        retry: RetryPolicy::Idempotent,
                        execution: ExecutionMode::Background { cancellable: false },
                        result: ResultSpec::Job,
                        ..ActionContract::default()
                    }));
                }
            }
            if let Some(remove) = item.remove_action() {
                let payload = vec![item.id.clone(), action_code(remove).to_string()];
                if let Ok(spec) = ActionSpec::operation(
                    "remove",
                    "Remove",
                    OperationKind::Effect,
                    "store.app",
                    &payload,
                ) {
                    resource.actions.push(spec.with_contract(ActionContract {
                        availability: if remove.requires_flatpak_host() && !self.flatpak_available {
                            Availability::Unavailable {
                                code: "flatpak_host_unavailable".into(),
                                reason: "Flatpak is not installed on this host; install it before using Flatpak catalog actions".into(),
                            }
                        } else {
                            Availability::Available
                        },
                        risk: RiskLevel::Destructive,
                        confirmation: ConfirmationPolicy::Confirm,
                        retry: RetryPolicy::Idempotent,
                        execution: ExecutionMode::Background { cancellable: false },
                        result: ResultSpec::Job,
                        ..ActionContract::default()
                    }));
                }
            }
            for version in &item.versions {
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
                crate::StoreTrust::Flathub => "Flathub distribution · no qOS review",
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
        root.children.extend(publisher_resources);

        let mut remotes = Resource::new("store:remotes", "store.remote-list", "Flatpak remotes");
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
            .requiring(ui::qos_actions::PermissionRequirement::Admin)
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
                    permission: ui::qos_actions::PermissionRequirement::Admin,
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
        root.children.push(remotes);

        let mut moderation =
            Resource::new("store:moderation", "store.moderation", "Trust & moderation");
        for report in &self.reports {
            let mut report_resource = Resource::new(
                report.id.clone(),
                "store.report",
                format!("{} · {}", report.app_id, report.status),
            );
            let note = text_at(doc, &automerge::ROOT, REPORT_NOTE_KEY);
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
                && let StoreSource::Qos { registry_name } = &item.source
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
        root.children.push(moderation);
        if !self.status_line.is_empty() {
            root.children.push(Resource::new(
                "store:latest-job",
                "store.install-job",
                self.status_line.clone(),
            ));
        }
        vec![root]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
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
    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
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
            if envelope.format != "qos.store.report-envelope.v1"
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
            let (registry_name, envelope): (String, ReportEnvelope) =
                serde_json::from_slice(payload)
                    .map_err(|_| "invalid moderation request".to_string())?;
            if registry_name.trim().is_empty()
                || envelope.format != "qos.store.report-envelope.v1"
                || envelope.app_id.trim().is_empty()
                || envelope.evidence_note.trim().chars().count() < 8
                || envelope.evidence_note.chars().count() > 4_096
            {
                return Err("Add a concern of 8–4096 characters before submitting".into());
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
            if registry_name.trim().is_empty() || !case_id.starts_with("case-") {
                return Err("moderation case is no longer available".into());
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
            let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            let intent = match args.first().map(String::as_str) {
                Some("refresh") => RemoteIntent::Refresh,
                Some("remove") => RemoteIntent::Remove(
                    args.get(1)
                        .filter(|name| !name.trim().is_empty())
                        .ok_or_else(|| "missing remote name".to_string())?
                        .clone(),
                ),
                Some("add") => {
                    let name = args
                        .get(1)
                        .filter(|name| !name.trim().is_empty())
                        .ok_or_else(|| "missing remote name".to_string())?
                        .clone();
                    let url = args
                        .get(2)
                        .ok_or_else(|| "missing repository URL".to_string())?
                        .clone();
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
        self.pending_lifecycle_actions
            .push((id, action, invocation));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        let index = self
            .completed_invocations
            .iter()
            .position(|update| update.invocation() == invocation)?;
        Some(self.completed_invocations.remove(index))
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.drain_report_export();
        // A source-filter chip parked its segment index in the shared
        // cell during the last `handle_event`; adopt it now.
        if let Some(idx) = self.filter_click.take()
            && let Some(f) = SourceFilter::ALL.get(idx).copied()
        {
            self.set_active_filter(f);
        }
        if let Some(idx) = self.category_click.take() {
            let category = self.category_labels().get(idx).cloned();
            self.set_active_category(category.filter(|label| label != "All"));
            self.scroll_offset.set((0.0, 0.0));
        }
        // Seed the search box's backing `Text` object *before* the
        // children sync so the `TextInput` resolves a real object to
        // splice keystrokes into (it binds to an existing `Text` but
        // never creates one).
        let search_obj = ensure_text_obj(state, path, SEARCH_QUERY_KEY);
        let _ = ensure_text_obj(state, path, REMOTE_NAME_KEY);
        let _ = ensure_text_obj(state, path, REMOTE_URL_KEY);
        let _ = ensure_text_obj(state, path, REPORT_NOTE_KEY);
        let _ = self.drain_click_keys(state, path);
        self.sync_report_resources(state, path);
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        // Pull the typed query back out and re-filter if it changed.
        if let Ok(text) = state.text(&search_obj) {
            self.set_query(text);
        }
        self.report_note = text_at(state, path, REPORT_NOTE_KEY);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.structure_dirty = false;
        self.reduce_motion = theme.reduce_motion;
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
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the pointer so the bubble can reveal on hover of its
        // bottom-right zone (coords are window-space, unscaled).
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node_id))
            && let Some(&child_node) = children.first()
        {
            root.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
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

    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.root
            .as_mut()
            .and_then(|root| root.take_control_invocation())
            .or_else(|| self.pending_control_invocation.take())
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
            || (!self.reduce_motion && self.status_bubble.needs_frame(std::time::Instant::now()))
            || self
                .images
                .as_ref()
                .is_some_and(|images| images.pending() > 0)
            || self.report_export.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

    fn item(name: &str, source: StoreSource, cat: &str, installed: InstalledState) -> StoreItem {
        let trust = match &source {
            StoreSource::Qos { .. } => StoreTrust::PublisherSigned,
            StoreSource::Flathub => StoreTrust::Flathub,
        };
        StoreItem {
            id: format!("id.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: format!("{name} description"),
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
                StoreSource::Qos {
                    registry_name: "qOS".into(),
                },
                "Productivity",
                InstalledState::Not,
            ),
            item(
                "Terminal",
                StoreSource::Qos {
                    registry_name: "qOS".into(),
                },
                "System",
                InstalledState::Installed {
                    version: "1".into(),
                    available: "1".into(),
                    pinned: false,
                },
            ),
            item("Chess", StoreSource::Flathub, "Games", InstalledState::Not),
        ]);
        app
    }

    #[test]
    fn visible_items_show_everything_when_filter_is_all() {
        let app = seeded_app();
        assert_eq!(app.visible_items().len(), 3);
    }

    #[test]
    fn filter_qos_hides_flatpak() {
        let mut app = seeded_app();
        app.set_active_filter(SourceFilter::Qos);
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert!(names.contains(&"Notes"));
        assert!(names.contains(&"Terminal"));
        assert!(!names.contains(&"Chess"));
    }

    #[test]
    fn filter_installed_shows_only_installed() {
        let mut app = seeded_app();
        app.set_active_filter(SourceFilter::Installed);
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, vec!["Terminal"]);
    }

    #[test]
    fn category_filter_gates_the_list() {
        let mut app = seeded_app();
        app.set_active_category(Some("Games".into()));
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, vec!["Chess"]);
    }

    #[test]
    fn search_narrows_and_ranks_by_score() {
        let mut app = seeded_app();
        // "Term" only matches Terminal — Notes contains "te" but not
        // "term", so the substring gate drops it.
        app.set_query("Term".into());
        let visible = app.visible_items();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "Terminal");
    }

    #[test]
    fn search_ranks_name_prefix_hits_first() {
        let mut app = seeded_app();
        // Both "Notes" (name contains "te" mid-string) and
        // "Terminal" (name starts with "Te") match "te".
        app.set_query("te".into());
        let visible = app.visible_items();
        assert!(visible.len() >= 2);
        assert_eq!(visible[0].name, "Terminal", "prefix hit ranks first");
    }

    #[test]
    fn categories_include_all_plus_distinct() {
        let app = seeded_app();
        let cats = app.category_labels();
        assert_eq!(cats[0], "All");
        assert!(cats.iter().any(|c| c == "Productivity"));
        assert!(cats.iter().any(|c| c == "Games"));
        assert!(cats.iter().any(|c| c == "System"));
    }

    #[test]
    fn live_constructor_starts_empty() {
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

    #[test]
    fn typed_search_query_filters_through_sync() {
        use automerge::transaction::Transactable;
        // Prove the wired search box: a query spliced into the bound
        // CRDT `Text` object is read back by `sync` and narrows the
        // visible list.
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        // First sync seeds the Text object.
        app.sync(&mut doc, &root);
        let obj = ensure_text_obj(&mut doc, &root, SEARCH_QUERY_KEY);
        doc.splice_text(&obj, 0, 0, "Term").unwrap();
        app.sync(&mut doc, &root);
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

    #[test]
    fn filter_chip_click_flips_active_filter_through_sync() {
        // Prove the wired filter chips: the `on_change` closure parks
        // a segment index in `filter_click`; `sync` adopts it and the
        // catalog re-filters.
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.filter_click.set(Some(1)); // segment 1 == SourceFilter::Qos
        app.sync(&mut doc, &root);
        assert_eq!(app.active_filter(), SourceFilter::Qos);
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert!(names.contains(&"Notes"));
        assert!(!names.contains(&"Chess"), "qOS filter hides Flatpak items");
    }

    #[test]
    fn category_selection_is_semantic_and_resets_the_result_position() {
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        app.scroll_offset.set((0.0, 240.0));
        let games = app
            .category_labels()
            .iter()
            .position(|label| label == "Games")
            .expect("Games category exists");
        app.category_click.set(Some(games));
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.active_category(), Some("Games"));
        assert_eq!(app.scroll_offset.get(), (0.0, 0.0));
        assert_eq!(app.visible_items()[0].name, "Chess");
    }

    #[test]
    fn refresh_click_is_forwarded_once_to_the_backend_worker() {
        use automerge::transaction::Transactable;

        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, REFRESH_KEY, true).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.take_refresh_requested());
        assert!(
            !app.take_refresh_requested(),
            "refresh is an edge, not a level"
        );
        assert!(matches!(
            doc.get(automerge::ROOT, REFRESH_KEY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), ScalarValue::Boolean(false))
        ));
    }

    #[test]
    fn detail_and_remote_controls_emit_serialized_worker_intents() {
        use automerge::transaction::Transactable;

        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "store.detail_select.id.chess", true)
            .unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.take_detail_requested().as_deref(), Some("id.chess"));
        assert!(app.take_detail_requested().is_none());

        doc.put(automerge::ROOT, REMOTES_OPEN_KEY, true).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        let intent = app
            .take_control_invocation()
            .expect("refresh action intent");
        assert_eq!(intent.address.resource, "store:remotes");
        assert_eq!(intent.address.action, "refresh");
    }

    #[test]
    fn exported_refresh_is_accepted_until_worker_completion() {
        let mut app = seeded_app();
        let doc = AutoCommit::new();
        let invocation = qos_actions::InvocationId::new();
        let started = app
            .start_resource_operation(
                invocation,
                OperationKind::Effect,
                "store.refresh",
                &serde_json::to_vec(&"").unwrap(),
                &doc,
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn stale_resource_action_cannot_escalate_to_an_unoffered_mutation() {
        let mut app = seeded_app();
        let doc = AutoCommit::new();
        let result = app.start_resource_operation(
            qos_actions::InvocationId::new(),
            OperationKind::Effect,
            "store.app",
            &serde_json::to_vec(&vec!["id.notes", "remove"]).unwrap(),
            &doc,
            &automerge::ROOT,
        );
        assert!(
            result.is_err(),
            "an uninstalled app publishes Open, not an arbitrary stale Remove effect"
        );
        assert!(app.take_lifecycle_actions().is_empty());
    }

    #[test]
    fn flatpak_actions_are_explicitly_unavailable_without_a_flatpak_host() {
        let mut app = seeded_app();
        app.set_flatpak_available(false);
        let doc = AutoCommit::new();
        let root = Widget::resources(&app, &doc).pop().unwrap();
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

    #[test]
    fn installed_flatpak_remove_is_unavailable_and_refused_without_host() {
        let mut app = seeded_app();
        app.items
            .iter_mut()
            .find(|item| item.id == "id.chess")
            .unwrap()
            .installed = InstalledState::Installed {
            version: "1".into(),
            available: "1".into(),
            pinned: false,
        };
        app.set_flatpak_available(false);
        let doc = AutoCommit::new();
        let root = Widget::resources(&app, &doc).pop().unwrap();
        let chess = root
            .children
            .iter()
            .flat_map(|publisher| publisher.children.iter())
            .find(|resource| resource.id.0 == "app:id.chess")
            .unwrap();
        for id in ["flatpak-setup", "remove"] {
            let action = chess.actions.iter().find(|action| action.id == id).unwrap();
            assert!(matches!(
                action.contract.availability,
                Availability::Unavailable { ref code, .. } if code == "flatpak_host_unavailable"
            ));
        }
        assert!(
            app.start_resource_operation(
                qos_actions::InvocationId::new(),
                OperationKind::Effect,
                "store.app",
                &serde_json::to_vec(&vec!["id.chess", "flatpak-remove"]).unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .is_err(),
            "direct/stale invocation cannot bypass the unavailable resource contract"
        );
    }

    #[test]
    fn exported_store_operations_are_background_jobs() {
        let app = seeded_app();
        let doc = AutoCommit::new();
        for action in Widget::resources(&app, &doc).iter().flat_map(|resource| {
            resource.actions.iter().chain(
                resource
                    .children
                    .iter()
                    .flat_map(|child| child.actions.iter()),
            )
        }) {
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

    #[test]
    fn moderation_effect_waits_for_authoritative_case_before_materializing_status() {
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        let invocation = qos_actions::InvocationId::new();
        let envelope = ReportEnvelope {
            format: "qos.store.report-envelope.v1".into(),
            app_id: "id.notes".into(),
            status: "Draft · not submitted".into(),
            publisher_fingerprint: Some("aa".repeat(32)),
            catalogue_trust_claim: "Publisher signed".into(),
            evidence_note: "The app connected to a remote host while offline mode was active."
                .into(),
        };
        assert_eq!(
            app.start_resource_operation(
                invocation,
                OperationKind::Effect,
                "store.moderation",
                &serde_json::to_vec(&("qOS", &envelope)).unwrap(),
                &doc,
                &automerge::ROOT,
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
                .and_then(|(value, _)| value.to_str().map(str::to_owned)),
            Some("case-authoritative".into())
        );
    }

    #[test]
    fn root_accessibility_is_a_named_window_not_an_empty_generic() {
        let mut app = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
        let node = app.accessibility().expect("store exports a root node");
        assert_eq!(node.role, Role::Window);
        assert_eq!(node.name.as_deref(), Some("App Store"));
        assert!(node.bounds.is_some());
    }

    #[test]
    fn trust_sheet_keeps_the_entire_publisher_identity_in_wrapped_lines() {
        let mut signed = item(
            "Signed",
            StoreSource::Qos {
                registry_name: "qOS".into(),
            },
            "Utilities",
            InstalledState::Not,
        );
        signed.publisher_pubkey_hex = Some("ab".repeat(32));
        let lines = publisher_identity_lines(&signed);
        assert_eq!(lines[0], "Publisher fingerprint (ed25519):");
        assert_eq!(lines[1..].concat().replace(' ', ""), "ab".repeat(32));
        assert!(lines.iter().skip(1).all(|line| line.len() <= 17));
    }

    #[test]
    fn operation_status_is_exposed_as_a_polite_live_region() {
        let mut app = seeded_app();
        app.set_status_line("Installing Notes…");
        let node = app.accessibility().expect("store exports a root node");
        let status = node.children.first().expect("status live region");
        assert_eq!(status.name.as_deref(), Some("Installing Notes…"));
        assert_eq!(status.live, Some(Politeness::Polite));
    }

    #[test]
    fn every_published_moderation_effect_has_a_local_operation_handler() {
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
