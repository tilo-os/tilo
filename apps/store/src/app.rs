//! `StoreApp` — the root widget.
//!
//! The Store has four pages, each one column on the window ground, composed
//! in [`view`] from the toolkit's content widgets:
//!
//! ```text
//! header row      Apps               [search] [remotes] [trust] [refresh] [History]
//! catalogue         6 apps from 2 sources
//!                   Updates (1)        ┌ card ┐
//!                   On this device (2) ┌ card ┐ ┌ card ┐
//!                   Discover (3)       ┌ card ┐ ┌ card ┐ ┌ card ┐
//! details           (the window's Back returns here)
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
//!
//! The surface is read-mostly: in a live window the catalogue comes from
//! the Store's worker ([`crate::worker`]), in a screenshot from its fixture,
//! and every change — a package job, a filter, a report edit — goes through

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::framework::{Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::tilo_actions::{ActionSpec, ControlInvocation, ResourcePresentation};
use ui::tilo_resource::Effect as _;
use ui::widget::Widget;
use ui::{Theme, permissions::PermissionContext, presence::PresenceCtx, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::doc_actions::StoreDocumentAction;
use crate::model::{ActionKind, SourceFilter, StoreItem, StoreSource};
use crate::resource::{ReportProjection, Store, StoreEffect};
use crate::sources::distinct_categories;
use crate::worker::{Outcome, Request, StoreWorker};
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

const PAGE_SIZE: usize = 48;
/// Bound remote configuration before it is rendered, serialized, or
/// forwarded to the Flatpak worker. These limits are deliberately generous
/// relative to remote names and HTTPS URLs, but keep a caller from handing
/// the worker an unbounded request.
const MAX_REMOTE_NAME_CHARS: usize = 128;
const MAX_REMOTE_URL_BYTES: usize = 2_048;
/// The worker status can include a remote or host error. Keep the exact value
/// in the live region, but bound the presentation copy so an untrusted server
/// cannot turn the masthead into an unbounded layout.
const MAX_VISIBLE_OPERATION_STATUS_CHARS: usize = 96;
const UNKNOWN_OPERATION_STATUS: &str = "No additional details were provided.";
/// Search/history tiles need a stable, bounded summary even when an upstream
/// catalogue description is malformed or unexpectedly huge.
const MAX_RESOURCE_DETAIL_BYTES: usize = 140;
/// The published page of catalogue rows stays small. Every app beyond it
/// still resolves by id ([`StoreApp::live_row`]) and through the Store's
/// search, so this never hides a catalogue entry from agents or Ctrl+K.
const RESOURCE_SNAPSHOT_LIMIT: usize = 200;
/// How many remote and moderation jobs may wait on the worker at once.
const MAX_PENDING_EXTERNAL_OPERATIONS: usize = 16;
/// The status a submitted report shows until a verified case status arrives.
const SUBMITTED_STATUS: &str = "Submitted, awaiting verified status";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ReportEnvelope {
    pub format: String,
    pub app_id: String,
    pub status: String,
    pub publisher_fingerprint: Option<String>,
    pub catalogue_trust_claim: String,
    pub evidence_note: String,
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

/// Name the result region for what it actually contains.  A responsive
/// large-text layout may choose one catalogue list instead of availability
/// shelves, but that must not turn an untouched catalogue into “Search
/// results” for sighted or screen-reader users.
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

fn report_envelope(report: &ReportProjection, item: Option<&StoreItem>) -> ReportEnvelope {
    ReportEnvelope {
        format: "tilo.store.report-envelope.v1".into(),
        app_id: report.app_id.clone(),
        status: report.status.clone(),
        publisher_fingerprint: item.and_then(|item| item.publisher_pubkey_hex.clone()),
        catalogue_trust_claim: item.map_or_else(
            || "Catalogue item no longer available".into(),
            |item| item.trust.label().into(),
        ),
        evidence_note: report.note.chars().take(16_384).collect(),
    }
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

/// The id an app's row is published under.
fn app_row_id(app_id: &str) -> String {
    format!("app:{app_id}")
}

/// Mark `spec` unavailable when it needs the host's Flatpak and there is none.
fn needing_flatpak(spec: ActionSpec, action: ActionKind, flatpak_available: bool) -> ActionSpec {
    if action.requires_flatpak_host() && !flatpak_available {
        spec.unavailable(
            "flatpak_host_unavailable",
            "Flatpak is not installed on this host; install it before using Flatpak catalog actions",
        )
    } else {
        spec
    }
}

/// One catalogue app as a live row: its details, what can be done with it
/// on this device, its versions and what its review is.
fn app_row(item: &StoreItem, flatpak_available: bool) -> LiveRow {
    let source = match &item.source {
        StoreSource::Tilo { .. } => "Tilo verified",
        StoreSource::Flathub => "Flathub",
    };
    // The row feeds system search, history and assistive clients, which do
    // not see the card layout. Keep its summary short rather than exporting
    // a second, unbounded copy of catalogue metadata.
    let detail = resource_detail_excerpt(&item.description);
    let latest = item.latest_version();
    let badge = if latest.is_empty() {
        source.to_string()
    } else {
        format!("{source}, v{latest}")
    };
    let mut row = LiveRow::new(app_row_id(&item.id), "store.app", item.name.clone())
        .presented(ResourcePresentation {
            detail: (!detail.is_empty()).then_some(detail),
            badge: Some(badge),
            preview: None,
        })
        .effect(&StoreEffect::ShowDetails {
            app: item.id.clone(),
        });
    let actions = std::iter::once(item.action_kind(flatpak_available))
        .chain(item.pin_action())
        .chain(item.remove_action());
    for action in actions {
        let spec = StoreEffect::package(action, item.id.clone()).bound();
        row = row.operation(needing_flatpak(spec, action, flatpak_available));
    }
    for version in item.versions.iter().take(32) {
        let label = if version.version.is_empty() {
            "Version metadata unavailable".into()
        } else {
            format!("Version {}", version.version)
        };
        row = row.row(LiveRow::new(
            format!("app:{}:version:{}", item.id, version.version),
            "store.version",
            label,
        ));
    }
    row.row(LiveRow::new(
        format!("app:{}:review", item.id),
        "store.review",
        review_label(item),
    ))
}

fn review_label(item: &StoreItem) -> &'static str {
    match item.trust {
        crate::StoreTrust::PublisherSigned => "Signature verified, no independent review",
        crate::StoreTrust::Flathub => "Flathub distribution, no Tilo review",
    }
}

/// The publisher an app's row is grouped under: its key, or Flathub.
fn publisher_of(item: &StoreItem) -> (String, String) {
    item.publisher_pubkey_hex.as_ref().map_or_else(
        || ("flathub".to_owned(), "Flathub distribution".to_owned()),
        |key| (key.clone(), format!("Publisher {}", short_publisher(item))),
    )
}

/// Root widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (host flatpak \
              availability, a layout-rebuild gate, a pending worker action, \
              and which of the remotes and trust pages is open); \
              grouping them into enums would not express any real invariant \
              between them and would touch sync/draw/handle_event call sites \
              across this file"
)]
pub struct StoreApp {
    /// Where report edits go; `None` until the framework attaches the
    /// surface.
    dispatcher: Option<Dispatcher<Store>>,
    /// The serialized package and catalogue worker of a live window.
    worker: Option<StoreWorker>,
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
    /// the browse effects only: people search through the platform search,
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
    root: Option<Box<ui::widget::NavShell>>,
    /// Whether the open page has somewhere to go back to; the window's Back reads it.
    can_go_back: Rc<Cell<bool>>,
    /// The window's Back was pressed; the next event or sync returns to the catalogue.
    back_requested: Rc<Cell<bool>>,
    /// Remote and moderation jobs waiting on the worker.
    pending_jobs: usize,
    pending_search: Option<String>,
    pending_detail: Option<String>,
    remotes: Vec<crate::flathub_client::Remote>,
    remotes_open: bool,
    safety_open: bool,
    /// The document's reports, as last presented.
    reports: Vec<ReportProjection>,
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
    /// Actions a list row asked for (its closure cannot reach the host),
    /// handed to the host as control invocations.
    queued_invocations: Rc<RefCell<Vec<ControlInvocation>>>,
}

impl Default for StoreApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreApp {
    pub fn new() -> Self {
        Self {
            dispatcher: None,
            worker: None,
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
            flatpak_available: false,
            focused_detail: None,
            structure_dirty: true,
            rect: Rect::ZERO,
            root: None,
            can_go_back: Rc::new(Cell::new(false)),
            back_requested: Rc::new(Cell::new(false)),
            pending_jobs: 0,
            pending_search: None,
            pending_detail: None,
            remotes: Vec::new(),
            remotes_open: false,
            safety_open: false,
            reports: Vec::new(),
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            catalog_page: 0,
            status_bubble: ui::StatusBubble::new(),
            last_cursor: Point::ZERO,
            images: None,
            queued_invocations: Rc::default(),
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

    /// Start the worker of a live window: the catalogue loads, and package
    /// jobs can run. `wake` is woken when it has something to show.
    pub(crate) fn start_worker(&mut self, wake: ui::WakeHandle) {
        if self.worker.is_some() {
            return;
        }
        self.flatpak_available = crate::flathub_client::flatpak_available();
        self.images = Some(crate::images::image_store());
        // The first presented frame is the catalogue chrome ("Loading
        // apps…"), not a cleared swapchain: the worker is already in flight.
        self.set_worker_busy(true);
        self.worker = Some(StoreWorker::spawn(self.search_index.clone(), wake));
    }

    /// Edit reports through `dispatcher`.
    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Store>) {
        self.dispatcher = Some(dispatcher);
        self.structure_dirty = true;
    }

    /// Show a capture's made-up catalogue, with the bundled artwork its
    /// icons and previews name.
    pub(crate) fn show_capture_catalogue(&mut self, items: Vec<StoreItem>) {
        self.set_flatpak_available(true);
        self.set_image_store(Some(crate::fixture::artwork_store()));
        self.set_items(items);
    }

    /// Replace the artwork cache: `None` shows every app with its generated
    /// icon and no previews.
    pub fn set_image_store(&mut self, images: Option<ui::ImageStore>) {
        self.images = images;
        self.structure_dirty = true;
    }

    pub fn reports(&self) -> &[ReportProjection] {
        &self.reports
    }

    /// Show the document's reports.
    pub(crate) fn present_reports(&mut self, reports: &[ReportProjection]) {
        // A note edit changes the report but not what is laid out around
        // its field, which shows the note itself.
        let laid_out = |report: &ReportProjection| {
            (
                report.id.clone(),
                report.app_id.clone(),
                report.status.clone(),
            )
        };
        let relayout = self.reports.len() != reports.len()
            || self
                .reports
                .iter()
                .map(laid_out)
                .ne(reports.iter().map(laid_out));
        self.reports = reports.to_vec();
        self.structure_dirty |= relayout;
    }

    /// Stage view state for a screenshot scenario, once its report actions
    /// applied.
    pub fn stage_scene(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("search") => self.set_query("te".into()),
            Some("detail" | "detail-narrow") => self.focus_detail("so.tilo.notes"),
            Some("detail-update") => self.focus_detail("so.tilo.files"),
            Some("detail-flatpak") => self.focus_detail("org.inkscape.Inkscape"),
            Some("remotes") => {
                self.show_remotes();
                self.set_remotes(vec![crate::flathub_client::Remote {
                    name: "flathub".into(),
                    url: "https://dl.flathub.org/repo/".into(),
                    title: "Flathub".into(),
                    disabled: false,
                }]);
            }
            Some("progress") => {
                // Terminal is already on the device and its offered action is
                // Open, so the busy card and the notice describe the same
                // real operation rather than claiming an install for an
                // open-only row.
                self.active_operation = Some(("so.tilo.terminal".into(), ActionKind::Open));
                self.set_status_line("Starting Terminal, launching the installed app…");
                self.set_worker_busy(true);
            }
            Some("error") => {
                self.show_failure(
                    "Install failed: the package signature did not match the catalogue.",
                );
            }
            Some("moderation" | "moderation-case") => self.show_trust(),
            Some("moderation-loading") => {
                self.show_trust();
                self.set_worker_busy(true);
                self.set_status_line("Contacting the authoritative registry…");
            }
            Some("moderation-offline") => {
                self.show_trust();
                self.show_failure(
                    "The registry is offline. Your draft and the last confirmed status are unchanged.",
                );
            }
            Some("moderation-error") => {
                self.show_trust();
                self.show_failure(
                    "The appeal was refused: this workspace identity is not the package publisher.",
                );
            }
            Some("offline") => self.show_failure("Could not reach any configured catalogue."),
            _ => {}
        }
        self.structure_dirty = true;
    }

    /// The page's title in the window header; the catalogue leaves it to the window's own name.
    fn page_title(&self) -> &'static str {
        if self.safety_open {
            "Trust and moderation"
        } else if self.remotes_open {
            "Flatpak remotes"
        } else if self.focused_detail.is_some() {
            "App details"
        } else {
            "Apps"
        }
    }

    /// Return to the catalogue from whichever page is open.
    fn show_catalogue(&mut self) {
        self.focused_detail = None;
        self.remotes_open = false;
        self.safety_open = false;
        self.structure_dirty = true;
    }

    fn show_failure(&mut self, message: &str) {
        self.set_status_line(message);
        self.set_operation_error(Some(message.into()));
    }

    pub fn focus_detail(&mut self, id: &str) {
        self.focused_detail = Some(id.to_string());
        self.remotes_open = false;
        self.safety_open = false;
        self.structure_dirty = true;
    }

    fn show_remotes(&mut self) {
        self.remotes_open = true;
        self.safety_open = false;
        self.focused_detail = None;
        self.structure_dirty = true;
    }

    fn show_trust(&mut self) {
        self.safety_open = true;
        self.remotes_open = false;
        self.focused_detail = None;
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
    fn set_detail_from_worker(&mut self, detail: StoreItem) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == detail.id) {
            *item = detail.clone();
        } else {
            self.items.push(detail.clone());
        }
        self.focused_detail = Some(detail.id);
        self.remotes_open = false;
        self.safety_open = false;
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
    fn set_items_from_worker(&mut self, items: Vec<StoreItem>) {
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

    /// Whether the catalogue is narrowed by a query, a source or a category.
    pub(crate) fn filters_active(&self) -> bool {
        !self.query.trim().is_empty()
            || self.active_filter != SourceFilter::All
            || self.active_category.is_some()
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
}

impl Widget for StoreApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::search::capability::SearchSource
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.drain_worker();
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        self.request_from_worker();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.structure_dirty = false;
        let page = self.compose(theme);
        self.can_go_back
            .set(self.safety_open || self.remotes_open || self.focused_detail.is_some());
        let title = self.page_title();
        let mut root = self.root.take().unwrap_or_else(|| {
            let can = Rc::clone(&self.can_go_back);
            let requested = Rc::clone(&self.back_requested);
            Box::new(
                ui::widget::NavShell::new()
                    .with_id("store.shell")
                    .on_back(move || can.get(), move || requested.set(true)),
            )
        });
        // The catalogue's actions share the header row with History; a page
        // opened from it carries its own title and actions in its body.
        let catalogue = !self.safety_open && !self.remotes_open && self.focused_detail.is_none();
        let actions = if catalogue {
            Self::catalogue_actions()
        } else {
            Vec::new()
        };
        root.set_content_header(title, actions);
        root.set_content(page);
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
        if self.back_requested.take() {
            self.show_catalogue();
            self.structure_dirty = true;
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
            || self.worker.as_ref().is_some_and(StoreWorker::has_pending)
    }
}

impl ui::search::capability::SearchSource for StoreApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
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
    }
}

/// A control on a moderation case: its key, its label and its effect.
type CaseControl = (&'static str, &'static str, fn(String) -> StoreEffect);

/// What an effect asks of the Store, once it is admitted.
pub enum Admitted {
    /// Done already: the window moved, or the worker was asked.
    Done,
    /// A job for the worker, and what its end changes.
    Job(Job),
    /// Save a report's evidence envelope where the person picks.
    Export(ReportEnvelope),
}

/// A request a job waits on, and what finishing it changes.
pub struct Job {
    pub request: Request,
    pub finish: Finish,
}

/// What finishing a job changes beside its status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// The app's card is no longer busy.
    Package,
    /// The registry took the app's report: record its case.
    Submitted { app_id: String },
    /// The registry confirmed the app's case: record its status.
    Case { app_id: String },
    /// Nothing beside the status line.
    Status,
}

impl Job {
    fn admitted(request: Request, finish: Finish) -> Admitted {
        Admitted::Job(Self { request, finish })
    }
}

impl StoreApp {
    /// Take what the worker finished: the catalogue, a detail page, the
    /// remotes, the status line.
    fn drain_worker(&mut self) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let snapshots: Vec<_> = std::iter::from_fn(|| worker.try_snapshot()).collect();
        let busy = worker.has_pending();
        for snap in snapshots {
            let applies_items = match snap.query.as_deref() {
                Some(query) => query == self.query(),
                None => snap.detail.is_none(),
            };
            if applies_items {
                self.set_items_from_worker(snap.items);
            }
            if let Some(detail) = snap.detail {
                self.set_detail_from_worker(detail);
            }
            self.set_remotes(snap.remotes);
            self.set_status_line(snap.status_line);
            self.set_operation_error(snap.operation_error);
        }
        self.set_worker_busy(busy);
    }

    /// Ask the worker for the search results and details the window now
    /// shows. Without a worker (a capture, a past version) nothing is
    /// fetched.
    fn request_from_worker(&mut self) {
        let search = self.pending_search.take();
        let detail = self.pending_detail.take();
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if let Some(query) = search {
            worker.request(Request::Search(query));
        }
        if let Some(id) = detail {
            worker.request(Request::Detail(id));
        }
    }

    /// Answer `effect`: move around the window, or start the job it asks
    /// for.
    pub(crate) fn answer(
        &mut self,
        effect: StoreEffect,
        cx: &mut EffectCx<Store, Self>,
    ) -> EffectAnswer {
        match self.admit(effect)? {
            Admitted::Done => Ok(Effected::done()),
            Admitted::Job(job) => self.start_job(job, cx),
            Admitted::Export(envelope) => self.export(&envelope, cx),
        }
    }

    /// Check `effect` against what the Store shows now and carry out what
    /// needs no job.
    ///
    /// # Errors
    /// Returns why the effect cannot run now.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive match over the Store's effects keeps every capability check beside the effect it guards"
    )]
    pub fn admit(&mut self, effect: StoreEffect) -> Result<Admitted, String> {
        if let Some((app, action)) = effect.as_package() {
            let app = app.to_owned();
            let item = self.package_operation(&app, action)?;
            self.active_operation = Some((item.id.clone(), action));
            self.operation_error = None;
            self.status_line = format!("{} {}…", action.progress_label(), item.name);
            self.structure_dirty = true;
            return Ok(Job::admitted(
                Request::Act(Box::new(item), action),
                Finish::Package,
            ));
        }
        match effect {
            StoreEffect::Refresh => return Ok(Job::admitted(Request::Refresh, Finish::Status)),
            StoreEffect::SetSearch { query } => {
                if query.chars().count() > 512 {
                    return Err("search query is too long".into());
                }
                self.set_query(query);
            }
            StoreEffect::SetSource { source } => {
                self.set_active_filter_from_local(&source);
                if self.active_filter.label() != source {
                    return Err("unknown Store source filter".into());
                }
            }
            StoreEffect::SetCategory { category } => {
                let category = (category != "All").then_some(category);
                if category.as_ref().is_some_and(|value| {
                    value.chars().count() > 128
                        || !self.category_labels().iter().any(|known| known == value)
                }) {
                    return Err("unknown Store category".into());
                }
                self.set_active_category(category);
            }
            StoreEffect::ClearFilters => {
                self.set_query(String::new());
                self.set_active_filter(SourceFilter::All);
                self.set_active_category(None);
            }
            StoreEffect::ShowDetails { app } => {
                if !crate::doc_actions::valid_app_id(&app) {
                    return Err("invalid app identity".into());
                }
                self.status_line = self.items.iter().find(|item| item.id == app).map_or_else(
                    || "Opening app details…".to_owned(),
                    |item| format!("Loading details for {}…", item.name),
                );
                self.focus_detail(&app);
                self.pending_detail = Some(app);
            }
            StoreEffect::OpenReference { path, contents } => {
                if path.is_none() && contents.is_none() {
                    return Err("name a Flatpak reference to open".into());
                }
                if contents.as_ref().is_some_and(|text| text.len() > 64 * 1024) {
                    return Err("a Flatpak reference is at most 64 KiB".into());
                }
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| "the Store's package worker is not running".to_owned())?;
                if !worker.request(Request::OpenReference {
                    path: path.map(std::path::PathBuf::from),
                    contents,
                }) {
                    return Err("the Store's package worker is unavailable".into());
                }
                self.status_line = "Opening the Flatpak reference…".into();
                self.structure_dirty = true;
            }
            StoreEffect::ShowCatalogue => self.show_catalogue(),
            StoreEffect::PreviousPage => {
                self.catalog_page = self.catalog_page.saturating_sub(1);
                self.scroll_offset.set((0.0, 0.0));
                self.structure_dirty = true;
            }
            StoreEffect::NextPage => {
                let pages = self.visible_items().len().div_ceil(PAGE_SIZE).max(1);
                self.catalog_page = (self.catalog_page + 1).min(pages.saturating_sub(1));
                self.scroll_offset.set((0.0, 0.0));
                self.structure_dirty = true;
            }
            StoreEffect::ShowRemotes => {
                self.show_remotes();
                if let Some(worker) = self.worker.as_ref() {
                    worker.request(Request::RefreshRemotes);
                }
            }
            StoreEffect::ShowTrust => self.show_trust(),
            StoreEffect::RefreshRemotes => {
                self.reserve_job()?;
                return Ok(Job::admitted(Request::RefreshRemotes, Finish::Status));
            }
            StoreEffect::AddRemote { name, url } => {
                if let Err(error) =
                    validate_remote_name(&name).and_then(|()| validate_remote_url(&url))
                {
                    self.status_line =
                        "A remote name and credential-free HTTPS repository URL are required."
                            .into();
                    self.structure_dirty = true;
                    return Err(error);
                }
                self.reserve_job()?;
                return Ok(Job::admitted(
                    Request::RemoteAdd {
                        name: name.trim().to_owned(),
                        url,
                    },
                    Finish::Status,
                ));
            }
            StoreEffect::RemoveRemote { name } => {
                validate_remote_name(&name)?;
                self.reserve_job()?;
                return Ok(Job::admitted(Request::RemoteRemove(name), Finish::Status));
            }
            StoreEffect::SubmitReport { app } => {
                let report = self.report(&app)?;
                if report.is_case() {
                    return Err("this report was already submitted".into());
                }
                let note = report.note.trim().chars().count();
                if !(8..=crate::doc_actions::MAX_REPORT_NOTE_CHARS).contains(&note) {
                    return Err("Add a concern of 8–4096 characters before submitting".into());
                }
                let (item, registry_name) = self.reported_app(&report)?;
                let envelope = report_envelope(&report, Some(&item));
                self.reserve_job()?;
                self.status_line = "Signing report with your workspace identity…".into();
                self.structure_dirty = true;
                return Ok(Job::admitted(
                    Request::SubmitReport {
                        registry_name,
                        envelope,
                    },
                    Finish::Submitted { app_id: app },
                ));
            }
            StoreEffect::RefreshStatus { app } => {
                let report = self.case(&app)?;
                let (_, registry_name) = self.reported_app(&report)?;
                self.reserve_job()?;
                self.status_line = "Contacting the authoritative registry…".into();
                self.structure_dirty = true;
                return Ok(Job::admitted(
                    Request::RefreshModeration {
                        registry_name,
                        case_id: report.id,
                    },
                    Finish::Case { app_id: app },
                ));
            }
            StoreEffect::Appeal { app } => {
                let report = self.case(&app)?;
                let reason = report.note.clone();
                if reason.trim().chars().count() < 8 || reason.chars().count() > 8_192 {
                    return Err("Add an appeal reason of 8–8192 characters".into());
                }
                let (_, registry_name) = self.reported_app(&report)?;
                self.reserve_job()?;
                self.status_line = "Contacting the authoritative registry…".into();
                self.structure_dirty = true;
                return Ok(Job::admitted(
                    Request::AppealModeration {
                        registry_name,
                        case_id: report.id,
                        reason,
                    },
                    Finish::Status,
                ));
            }
            StoreEffect::ExportEvidence { app } => {
                let report = self.report(&app)?;
                let item = self.items.iter().find(|item| item.id == report.app_id);
                return Ok(Admitted::Export(report_envelope(&report, item)));
            }
            StoreEffect::Open { .. }
            | StoreEffect::Update { .. }
            | StoreEffect::Remove { .. }
            | StoreEffect::Pin { .. }
            | StoreEffect::Unpin { .. }
            | StoreEffect::FlatpakInstall { .. }
            | StoreEffect::FlatpakUpdate { .. }
            | StoreEffect::FlatpakOpen { .. }
            | StoreEffect::FlatpakRemove { .. }
            | StoreEffect::FlatpakSetup { .. } => {}
        }
        Ok(Admitted::Done)
    }

    /// The app `action` is asked of, if the action is the one the app
    /// offers now and nothing else is running.
    ///
    /// # Errors
    /// Returns why the operation cannot run.
    pub fn package_operation(&self, app: &str, action: ActionKind) -> Result<StoreItem, String> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == app)
            .ok_or_else(|| "app is no longer present in the catalog".to_string())?;
        if action.requires_flatpak_host() && !self.flatpak_available {
            return Err(
                "Flatpak is not installed on this host; no install operation can run".into(),
            );
        }
        if !item.allows_action(action, self.flatpak_available) {
            return Err("this action is no longer available for the current app state".into());
        }
        if self.active_operation.is_some() {
            return Err("a package operation is already in progress".into());
        }
        Ok(item.clone())
    }

    fn reserve_job(&self) -> Result<(), String> {
        if self.pending_jobs >= MAX_PENDING_EXTERNAL_OPERATIONS {
            return Err("too many remote and registry operations are already queued".into());
        }
        Ok(())
    }

    /// The report about `app`.
    fn report(&self, app: &str) -> Result<ReportProjection, String> {
        self.reports
            .iter()
            .find(|report| report.app_id == app)
            .cloned()
            .ok_or_else(|| format!("there is no report about `{app}`"))
    }

    /// The report about `app`, once the registry has a case for it.
    fn case(&self, app: &str) -> Result<ReportProjection, String> {
        let report = self.report(app)?;
        if !report.is_case() || !crate::doc_actions::valid_case_id(&report.id) {
            return Err("this report has no moderation case yet".into());
        }
        Ok(report)
    }

    /// The reported app and the Tilo registry its report goes to.
    fn reported_app(&self, report: &ReportProjection) -> Result<(StoreItem, String), String> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == report.app_id)
            .ok_or_else(|| "the reported app is no longer in this catalogue".to_string())?;
        let StoreSource::Tilo { registry_name } = &item.source else {
            return Err("reports can only go to the app's Tilo registry".into());
        };
        Ok((item.clone(), registry_name.clone()))
    }

    fn start_job(&mut self, job: Job, cx: &mut EffectCx<Store, Self>) -> EffectAnswer {
        let Job { request, finish } = job;
        let (done, outcome) = std::sync::mpsc::channel();
        let queued = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.request_job(request, done));
        if !queued {
            if finish == Finish::Package {
                self.active_operation = None;
            }
            self.structure_dirty = true;
            return Err("the Store's package worker is not running".into());
        }
        if finish != Finish::Package {
            self.pending_jobs += 1;
        }
        self.set_worker_busy(true);
        Ok(cx.thread(
            move |_| {
                outcome
                    .recv()
                    .unwrap_or_else(|_| Err("the Store worker stopped".into()))
            },
            move |app: &mut Self, outcome: Outcome, cx| app.finish_job(&finish, outcome, cx),
        ))
    }

    fn finish_job(
        &mut self,
        finish: &Finish,
        outcome: Outcome,
        cx: &EffectCx<Store, Self>,
    ) -> EffectAnswer {
        if *finish == Finish::Package {
            self.active_operation = None;
        } else {
            self.pending_jobs = self.pending_jobs.saturating_sub(1);
        }
        self.structure_dirty = true;
        let finished = outcome?;
        if let Some(case) = &finished.case {
            match finish {
                Finish::Submitted { app_id } => {
                    self.record_case(
                        app_id,
                        &case.package_id,
                        &case.case_id,
                        SUBMITTED_STATUS,
                        cx,
                    )?;
                }
                Finish::Case { app_id } => self.record_case(
                    app_id,
                    &case.package_id,
                    &case.case_id,
                    moderation_status_label(case.status),
                    cx,
                )?,
                Finish::Package | Finish::Status => {}
            }
        }
        Ok(Effected::value(finished.status))
    }

    /// Record the registry's case for `app_id`'s report.
    fn record_case(
        &mut self,
        app_id: &str,
        package_id: &str,
        case_id: &str,
        status: &str,
        cx: &EffectCx<Store, Self>,
    ) -> Result<(), String> {
        match case_record(app_id, package_id, case_id, status) {
            Ok(record) => {
                cx.apply(&record);
                self.status_line = format!("Moderation case {case_id} is {status}.");
                Ok(())
            }
            Err(message) => {
                self.show_failure(message);
                Err(message.into())
            }
        }
    }

    /// Save `envelope` where the person picks, off the UI thread.
    fn export(
        &mut self,
        envelope: &ReportEnvelope,
        cx: &mut EffectCx<Store, Self>,
    ) -> EffectAnswer {
        let bytes = serde_json::to_vec_pretty(envelope).map_err(|e| e.to_string())?;
        let suggested = format!(
            "{}-report.json",
            envelope.app_id.replace(
                |c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-',
                "-"
            )
        );
        let picked = ui::portal::PickRequest::save("Export report evidence", "Export", &suggested)
            .filter(ui::portal::FileFilter::new("JSON evidence envelope").glob("*.json"))
            .thread_name("store-report-export")
            .send();
        self.status_line = "Choose where to export the report evidence…".into();
        self.structure_dirty = true;
        Ok(cx.thread(
            move |_| match picked.recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => {
                    tilo_files::native::atomic::write(&path, &bytes)
                        .map(|()| Some(path))
                        .map_err(|error| format!("Could not export report evidence: {error}"))
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => Ok(None),
                Ok(ui::portal::PortalOutcome::Failed(message)) => Err(message),
                Err(_) => Err("the file picker closed without an answer".into()),
            },
            |app: &mut Self, exported: Result<Option<std::path::PathBuf>, String>, _| {
                app.structure_dirty = true;
                match exported {
                    Ok(Some(path)) => {
                        app.status_line =
                            format!("Exported report evidence to {}.", path.display());
                        Ok(Effected::done())
                    }
                    Ok(None) => {
                        app.status_line = "Export cancelled.".into();
                        Ok(Effected::done())
                    }
                    Err(error) => {
                        app.show_failure(&error);
                        Err(error)
                    }
                }
            },
        ))
    }

    /// The effects that cannot run on this device now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        if self.flatpak_available {
            return Vec::new();
        }
        let reason = "Flatpak is not installed on this host; install it before using Flatpak catalog actions";
        [
            StoreEffect::FLATPAK_INSTALL,
            StoreEffect::FLATPAK_UPDATE,
            StoreEffect::FLATPAK_OPEN,
            StoreEffect::FLATPAK_REMOVE,
            StoreEffect::FLATPAK_SETUP,
        ]
        .into_iter()
        .map(|effect| EffectStatus::unavailable(effect, "flatpak_host_unavailable", reason))
        .collect()
    }

    /// The live rows the Store publishes: a bounded page of the catalogue
    /// grouped by publisher, the Flatpak remotes, the moderation effects on
    /// each report and the running package job.
    pub(crate) fn live_rows(&self) -> Vec<LiveRow> {
        let mut snapshot_items = self.items.iter().collect::<Vec<_>>();
        snapshot_items.sort_by(|left, right| left.id.cmp(&right.id));
        let mut publishers: std::collections::BTreeMap<String, (String, Vec<LiveRow>)> =
            std::collections::BTreeMap::new();
        for item in snapshot_items.into_iter().take(RESOURCE_SNAPSHOT_LIMIT) {
            let (key, label) = publisher_of(item);
            publishers
                .entry(key)
                .or_insert_with(|| (label, Vec::new()))
                .1
                .push(app_row(item, self.flatpak_available));
        }
        let mut publishers = publishers.into_iter().collect::<Vec<_>>();
        publishers.sort_by(|(_, (left, _)), (_, (right, _))| left.cmp(right));
        let mut rows = publishers
            .into_iter()
            .map(|(key, (label, apps))| {
                apps.into_iter().fold(
                    LiveRow::new(format!("publisher:{key}"), "store.publisher", label),
                    LiveRow::row,
                )
            })
            .collect::<Vec<_>>();
        if self.items.len() > RESOURCE_SNAPSHOT_LIMIT {
            rows.push(self.catalogue_snapshot_row());
        }
        rows.push(self.remotes_row());
        rows.push(self.moderation_row());
        rows.extend(self.latest_job_row());
        rows
    }

    /// The live row `id`, including any app beyond the published page.
    pub(crate) fn live_row(&self, id: &str) -> Option<LiveRow> {
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
                return Some(LiveRow::new(id, "store.version", label));
            }
            if let Some(app_id) = rest.strip_suffix(":review") {
                let item = self.items.iter().find(|item| item.id == app_id)?;
                return Some(LiveRow::new(id, "store.review", review_label(item)));
            }
            return self
                .items
                .iter()
                .find(|item| item.id == rest)
                .map(|item| app_row(item, self.flatpak_available));
        }
        if let Some(key) = id.strip_prefix("publisher:") {
            let item = self.items.iter().find(|item| publisher_of(item).0 == key)?;
            return Some(LiveRow::new(id, "store.publisher", publisher_of(item).1));
        }
        if let Some(name) = id.strip_prefix("remote:") {
            return self
                .remotes
                .iter()
                .find(|remote| remote.name == name)
                .map(remote_row);
        }
        match id {
            REMOTES_ID => Some(self.remotes_row()),
            crate::resource::MODERATION_ID => Some(self.moderation_row()),
            LATEST_JOB_ID => self.latest_job_row(),
            CATALOGUE_SNAPSHOT_ID if self.items.len() > RESOURCE_SNAPSHOT_LIMIT => {
                Some(self.catalogue_snapshot_row())
            }
            _ => self
                .reports
                .iter()
                .find(|report| report.id == id)
                .map(|report| self.report_row(report)),
        }
    }

    fn catalogue_snapshot_row(&self) -> LiveRow {
        LiveRow::new(
            CATALOGUE_SNAPSHOT_ID,
            "store.catalogue-snapshot",
            format!(
                "{} additional apps are available through App Store search",
                self.items.len().saturating_sub(RESOURCE_SNAPSHOT_LIMIT)
            ),
        )
    }

    fn remotes_row(&self) -> LiveRow {
        self.remotes.iter().map(remote_row).fold(
            LiveRow::new(REMOTES_ID, "store.remote-list", "Flatpak remotes")
                .effect(&StoreEffect::ShowRemotes)
                .effect(&StoreEffect::RefreshRemotes),
            LiveRow::row,
        )
    }

    fn moderation_row(&self) -> LiveRow {
        self.reports
            .iter()
            .map(|report| self.report_row(report))
            .fold(
                LiveRow::new(
                    crate::resource::MODERATION_ID,
                    "store.moderation",
                    "Trust and moderation",
                )
                .effect(&StoreEffect::ShowTrust),
                LiveRow::row,
            )
    }

    /// What can be done with a report from here: export its evidence, and
    /// submit it, or follow and appeal its case, at the app's registry.
    fn report_row(&self, report: &ReportProjection) -> LiveRow {
        let app = report.app_id.clone();
        let mut row = LiveRow::new(
            report.id.clone(),
            "store.report",
            format!("{}: {}", report.app_id, report.status),
        )
        .effect(&StoreEffect::ExportEvidence { app: app.clone() });
        if self.reported_app(report).is_ok() {
            if report.is_case() {
                row = row
                    .effect(&StoreEffect::RefreshStatus { app: app.clone() })
                    .effect(&StoreEffect::Appeal { app });
            } else {
                row = row.effect(&StoreEffect::SubmitReport { app });
            }
        }
        row
    }

    fn latest_job_row(&self) -> Option<LiveRow> {
        (!self.status_line.is_empty())
            .then(|| LiveRow::new(LATEST_JOB_ID, "store.install-job", self.status_line.clone()))
    }
}

/// The record of the registry's case `case_id` for `app_id`'s report, when
/// the registry answered about that app with a well-formed case.
fn case_record(
    app_id: &str,
    package_id: &str,
    case_id: &str,
    status: &str,
) -> Result<StoreDocumentAction, &'static str> {
    if package_id != app_id
        || !case_id.starts_with("case-")
        || !crate::doc_actions::valid_case_id(case_id)
    {
        return Err("Registry returned an invalid moderation case identifier.");
    }
    Ok(StoreDocumentAction::RecordModerationCase {
        app_id: app_id.to_owned(),
        case_id: case_id.to_owned(),
        status: status.to_owned(),
    })
}

const REMOTES_ID: &str = "store:remotes";
const LATEST_JOB_ID: &str = "store:latest-job";
const CATALOGUE_SNAPSHOT_ID: &str = "store:catalogue-snapshot";

fn remote_row(remote: &crate::flathub_client::Remote) -> LiveRow {
    LiveRow::new(
        format!("remote:{}", remote.name),
        "store.remote",
        remote.title.clone(),
    )
    .effect(&StoreEffect::RemoveRemote {
        name: remote.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};
    use crate::widgets::{CardAction, CardDetails};
    use automerge::{AutoCommit, ReadDoc};
    use ui::search::SearchProvider;
    use ui::tilo_resource::Resource as _;
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

        // Case `published_rows_are_bounded_and_omitted_apps_resolve_by_id`.
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
            let rows: Vec<_> = app
                .live_rows()
                .into_iter()
                .map(LiveRow::into_resource)
                .collect();
            let apps: Vec<_> = rows
                .iter()
                .filter(|resource| resource.kind == "store.publisher")
                .flat_map(|publisher| publisher.children.iter())
                .collect();
            assert_eq!(apps.len(), RESOURCE_SNAPSHOT_LIMIT);
            assert!(rows.iter().any(|row| row.id.0 == CATALOGUE_SNAPSHOT_ID));
            let eager = "app:dev.example.utility0000";
            let on_page = apps
                .iter()
                .find(|resource| resource.id.as_str() == eager)
                .expect("the first app is on the published page");
            let looked_up = app.live_row(eager).unwrap().into_resource();
            let ids = |resource: &ui::tilo_actions::Resource| {
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids(&looked_up),
                ids(on_page),
                "an app resolves as published"
            );
            let omitted = "app:dev.example.utility0216";
            let resolved = app.live_row(omitted).unwrap().into_resource();
            assert_eq!(resolved.id.0, omitted);
            assert!(ids(&resolved).iter().any(|id| id == "show-details"));
            assert!(
                apps.iter()
                    .flat_map(|resource| resource.actions.iter())
                    .all(|action| action.kind != ui::tilo_actions::OperationKind::Document),
                "a catalogue row offers only effects"
            );
        }

        // Case `remotes_and_moderation_rows_resolve_by_id`.
        {
            let mut app = seeded_app();
            app.set_remotes(vec![crate::flathub_client::Remote {
                name: "flathub".into(),
                url: "https://dl.flathub.org/repo/".into(),
                title: "Flathub".into(),
                disabled: false,
            }]);
            for id in [REMOTES_ID, "remote:flathub", crate::resource::MODERATION_ID] {
                assert!(app.live_row(id).is_some(), "{id} resolves");
            }
            let remote = app.live_row("remote:flathub").unwrap().into_resource();
            assert!(
                remote
                    .actions
                    .iter()
                    .any(|action| action.id == "remove-remote")
            );
        }

        // Case `app_rows_include_bounded_catalogue_context_for_nonvisual_clients`.
        {
            let app = seeded_app();
            let item = app.items.iter().find(|item| item.id == "id.notes").unwrap();
            let presentation = app_row(item, true)
                .into_resource()
                .presentation
                .expect("app row has a search and history summary");
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
            app.set_items(crate::fixture::fixture_catalog());
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

    /// Card pointer and primary activation invoke the Store's effects.
    #[test]
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
                            binding: bindings::package(item, action),
                            in_progress: busy,
                        },
                        CardDetails {
                            key: "select",
                            binding: bindings::show_details(item),
                        },
                        None,
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
                    assert_eq!(details.address.resource, Store::KIND);
                    assert_eq!(details.address.action, "show-details");
                    assert_eq!(details.arguments["app"], item.id.as_str());
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
    }

    /// Stacked geometry, contained actions, the three-column measure and the large-text layout.
    #[test]
    fn catalogue_measures_stay_finite_contained_and_stacked_under_large_text() {
        // Case `full_catalogue_geometry_stays_finite_at_every_width_and_text_scale`.
        {
            for (width, scale) in [(480.0, 1.0), (1_280.0, 2.0)] {
                let mut app = seeded_app();
                app.set_items(crate::fixture::fixture_catalog());
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
            assert!(app.operation_notice().is_none());

            app.set_status_line("Installing Notes…");
            app.set_worker_busy(true);
            let busy_notice = app
                .operation_notice()
                .expect("busy work has a semantic notice");
            let busy = banner(&*busy_notice)
                .expect("busy banner remains a live child of the progress notice");
            assert_eq!(busy.live, Some(Politeness::Polite));
            assert_eq!(busy.description.as_deref(), Some("Installing Notes…"));

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

    fn admitted_job(app: &mut StoreApp, effect: StoreEffect) -> Job {
        match app.admit(effect) {
            Ok(Admitted::Job(job)) => job,
            Ok(_) => panic!("a job"),
            Err(error) => panic!("admitted: {error}"),
        }
    }

    fn draft(app: &mut StoreApp, note: &str) {
        app.present_reports(&[ReportProjection {
            id: "report:id.notes".into(),
            app_id: "id.notes".into(),
            status: crate::doc_actions::DRAFT_STATUS.into(),
            note: note.into(),
        }]);
    }

    /// The update shelf follows the visible catalogue; refresh and package
    /// operations are worker jobs; a card's operation holds it until done.
    #[test]
    fn refresh_and_package_operations_are_worker_jobs() {
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

        // Case `refresh_is_a_worker_job`.
        {
            let mut app = seeded_app();
            let job = admitted_job(&mut app, StoreEffect::Refresh);
            assert!(matches!(job.request, Request::Refresh));
            assert_eq!(job.finish, Finish::Status);
        }

        // Case `app_operation_locks_the_target_card_until_it_finishes`.
        {
            let mut app = seeded_app();
            let open = StoreEffect::Open {
                app: "id.notes".into(),
            };
            let job = admitted_job(&mut app, open.clone());
            assert!(matches!(
                job.request,
                Request::Act(ref item, ActionKind::Open) if item.id == "id.notes"
            ));
            assert_eq!(
                app.active_operation,
                Some(("id.notes".into(), ActionKind::Open))
            );
            assert_eq!(app.status_line(), "Starting Notes…");
            assert!(
                app.admit(open).is_err(),
                "the card action cannot double-submit while the worker owns it"
            );
            app.set_worker_busy(true);
            app.set_worker_busy(false);
            assert!(app.active_operation.is_none());
        }
    }

    /// Browsing effects move only this window: the document never changes,
    /// and an unknown filter is refused.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn browse_effects_filter_this_window_only() {
        // Case `live_constructor_starts_empty`.
        {
            // The live app must show a real (empty) catalog until the
            // refresh worker streams one — never a fabricated one.
            let app = StoreApp::new();
            assert!(app.items().is_empty());
            assert_eq!(app.active_filter(), SourceFilter::All);
            assert!(app.query().is_empty());
        }

        // Case `search_effect_filters_the_catalogue_and_reaches_the_worker`.
        {
            let mut app = seeded_app();
            app.admit(StoreEffect::SetSearch {
                query: "Term".into(),
            })
            .unwrap();
            assert_eq!(app.query(), "Term");
            assert_eq!(app.pending_search.as_deref(), Some("Term"));
            let names: Vec<&str> = app
                .visible_items()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(names, vec!["Terminal"]);
        }

        // Case `one_window_cannot_navigate_another_with_document_filters`.
        {
            let mut first = seeded_app();
            let mut second = seeded_app();
            let mut doc = AutoCommit::new();
            let heads = doc.get_heads();
            first
                .admit(StoreEffect::SetSource {
                    source: "Tilo".into(),
                })
                .unwrap();
            first
                .admit(StoreEffect::SetCategory {
                    category: "Games".into(),
                })
                .unwrap();
            first.sync(&mut doc, &automerge::ROOT);
            second.sync(&mut doc, &automerge::ROOT);
            assert_eq!(first.active_filter(), SourceFilter::Tilo);
            assert_eq!(first.active_category(), Some("Games"));
            assert_eq!(second.active_filter(), SourceFilter::All);
            assert_eq!(second.active_category(), None);
            assert_eq!(doc.get_heads(), heads, "browsing writes nothing");
        }

        // Case `agent_browse_effects_match_the_local_controls`.
        {
            let mut app = seeded_app();
            app.scroll_offset.set((0.0, 240.0));
            assert!(matches!(
                app.admit(StoreEffect::SetSource {
                    source: "Tilo".into()
                }),
                Ok(Admitted::Done)
            ));
            assert_eq!(app.active_filter(), SourceFilter::Tilo);
            assert!(
                app.visible_items()
                    .iter()
                    .all(|item| matches!(item.source, StoreSource::Tilo { .. }))
            );
            assert!(
                app.admit(StoreEffect::SetSource {
                    source: "Elsewhere".into()
                })
                .is_err()
            );
            app.admit(StoreEffect::ClearFilters).unwrap();
            app.admit(StoreEffect::SetCategory {
                category: "Games".into(),
            })
            .unwrap();
            assert_eq!(app.active_category(), Some("Games"));
            assert_eq!(app.scroll_offset.get(), (0.0, 0.0));
            assert!(
                app.admit(StoreEffect::SetCategory {
                    category: "Unheard of".into()
                })
                .is_err()
            );
            app.admit(StoreEffect::SetSearch {
                query: "Chess".into(),
            })
            .unwrap();
            assert_eq!(app.pending_search.as_deref(), Some("Chess"));
            assert!(
                app.admit(StoreEffect::SetSearch {
                    query: "x".repeat(513)
                })
                .is_err()
            );
        }

        // Case `details_and_remotes_are_routes_of_this_window`.
        {
            let mut app = seeded_app();
            app.admit(StoreEffect::ShowDetails {
                app: "id.chess".into(),
            })
            .unwrap();
            assert_eq!(app.focused_detail.as_deref(), Some("id.chess"));
            assert_eq!(app.pending_detail.as_deref(), Some("id.chess"));
            assert!(
                app.admit(StoreEffect::ShowDetails {
                    app: "../escape".into()
                })
                .is_err()
            );
            app.admit(StoreEffect::ShowRemotes).unwrap();
            assert!(app.remotes_open && app.focused_detail.is_none());
            app.admit(StoreEffect::ShowTrust).unwrap();
            assert!(app.safety_open && !app.remotes_open);
            app.admit(StoreEffect::ShowCatalogue).unwrap();
            assert!(!app.safety_open && !app.remotes_open);
        }
    }

    /// Stale or host-less Flatpak operations are unavailable and refused.
    #[test]
    fn flatpak_and_stale_operations_are_refused_without_escalation() {
        // Case `stale_operation_cannot_escalate_to_an_unoffered_mutation`.
        {
            let mut app = seeded_app();
            assert!(
                app.admit(StoreEffect::Remove {
                    app: "id.notes".into()
                })
                .is_err(),
                "an uninstalled app offers Open, not an arbitrary stale Remove"
            );
            assert!(app.active_operation.is_none());
        }

        // Case `flatpak_operations_are_unavailable_without_a_flatpak_host`.
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
            let chess = app
                .live_row("app:id.chess")
                .expect("an app row resolves")
                .into_resource();
            for id in ["flatpak-setup", "flatpak-remove"] {
                let action = chess.actions.iter().find(|action| action.id == id).unwrap();
                assert!(matches!(
                    action.contract.availability,
                    ui::tilo_actions::Availability::Unavailable { ref code, .. }
                        if code == "flatpak_host_unavailable"
                ));
            }
            assert!(
                app.effect_statuses()
                    .iter()
                    .any(|status| *status
                        == EffectStatus::unavailable(
                            StoreEffect::FLATPAK_REMOVE,
                            "flatpak_host_unavailable",
                            "Flatpak is not installed on this host; install it before using Flatpak catalog actions",
                        ))
            );
            assert!(
                app.admit(StoreEffect::FlatpakRemove {
                    app: "id.chess".into()
                })
                .is_err(),
                "a direct invocation cannot bypass the unavailable operation"
            );
        }

        // Case `flatpak_remove_asks_first_at_its_exact_address`.
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
            let chess = app.live_row("app:id.chess").unwrap().into_resource();
            let remove = chess
                .actions
                .iter()
                .find(|action| action.id == "flatpak-remove")
                .expect("Flatpak remove is offered at its concrete address");
            assert_eq!(
                remove.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm
            );
            let item = app.items.iter().find(|item| item.id == "id.chess").unwrap();
            assert!(bindings::package(item, ActionKind::FlatpakRemove).is_some());
        }
    }

    /// Reports go to their app's registry only with a note, cases are
    /// recorded only when well formed, and registry work is bounded.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn moderation_effects_are_bounded_and_target_checked() {
        // Case `a_draft_is_submitted_with_its_note_to_its_registry`.
        {
            let mut app = seeded_app();
            draft(&mut app, "short");
            assert!(
                app.admit(StoreEffect::SubmitReport {
                    app: "id.notes".into()
                })
                .is_err(),
                "a concern of fewer than 8 characters is not submitted"
            );
            let note = "The app connected to a remote host while offline mode was active.";
            draft(&mut app, note);
            let job = admitted_job(
                &mut app,
                StoreEffect::SubmitReport {
                    app: "id.notes".into(),
                },
            );
            let Request::SubmitReport {
                registry_name,
                envelope,
            } = job.request
            else {
                panic!("a submission");
            };
            assert_eq!(registry_name, "Tilo");
            assert_eq!(envelope.evidence_note, note);
            assert_eq!(
                job.finish,
                Finish::Submitted {
                    app_id: "id.notes".into()
                }
            );
            assert!(
                app.admit(StoreEffect::RefreshStatus {
                    app: "id.notes".into()
                })
                .is_err(),
                "a draft has no case to refresh"
            );
            assert!(
                app.admit(StoreEffect::SubmitReport {
                    app: "id.chess".into()
                })
                .is_err(),
                "there is no report about Chess"
            );
        }

        // Case `a_case_is_refreshed_and_appealed_at_its_registry`.
        {
            let mut app = seeded_app();
            app.present_reports(&[ReportProjection {
                id: "case-authoritative".into(),
                app_id: "id.notes".into(),
                status: "Open".into(),
                note: "The listing omitted a network dependency.".into(),
            }]);
            let job = admitted_job(
                &mut app,
                StoreEffect::RefreshStatus {
                    app: "id.notes".into(),
                },
            );
            assert!(matches!(
                job.request,
                Request::RefreshModeration { ref case_id, .. } if case_id == "case-authoritative"
            ));
            let job = admitted_job(
                &mut app,
                StoreEffect::Appeal {
                    app: "id.notes".into(),
                },
            );
            assert!(matches!(
                job.request,
                Request::AppealModeration { ref reason, .. } if reason.contains("network dependency")
            ));
            let row = app.live_row("case-authoritative").unwrap().into_resource();
            let offered: Vec<&str> = row
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            assert!(offered.contains(&"refresh-status") && offered.contains(&"appeal"));
            assert!(!offered.contains(&"submit-report"));
        }

        // Case `only_a_well_formed_case_about_the_reported_app_is_recorded`.
        {
            assert_eq!(
                case_record("id.notes", "id.notes", "case-authoritative", "Open"),
                Ok(StoreDocumentAction::RecordModerationCase {
                    app_id: "id.notes".into(),
                    case_id: "case-authoritative".into(),
                    status: "Open".into(),
                })
            );
            assert!(case_record("id.notes", "id.notes", "case-../../outside", "Open").is_err());
            assert!(case_record("id.notes", "id.other", "case-authoritative", "Open").is_err());
        }

        // Case `registry_work_is_bounded`.
        {
            let mut app = seeded_app();
            app.pending_jobs = MAX_PENDING_EXTERNAL_OPERATIONS;
            assert!(app.admit(StoreEffect::RefreshRemotes).is_err());
            app.pending_jobs = 0;
            assert!(app.admit(StoreEffect::RefreshRemotes).is_ok());
            assert!(
                app.admit(StoreEffect::AddRemote {
                    name: "bad name".into(),
                    url: "https://example.invalid/repo".into(),
                })
                .is_err()
            );
            assert!(
                app.admit(StoreEffect::AddRemote {
                    name: "tools".into(),
                    url: "http://example.invalid/repo".into(),
                })
                .is_err(),
                "a remote is credential-free HTTPS"
            );
        }
    }
}
