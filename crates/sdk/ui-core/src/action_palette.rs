//! `ActionPalette` — the one Ctrl+K surface for a `tilo_ui::App`.
//!
//! The **resource + action model** ([`tilo_actions`]) is the foundation: the
//! focused app instance publishes a tree of open [`Resource`]s, each
//! exporting [`ActionSpec`](tilo_actions::ActionSpec)s — the operations
//! Ctrl+K offers. An app can also fold in *searchable content* via
//! [`SearchProvider`]s (contacts, files, …);
//! their hits can address an exported resource action and therefore enter the
//! same broker path; legacy opaque navigation ids remain supported during
//! migration. Search providers are discovery-only: a provider-returned
//! resource is never accepted as authority. The host resolves every addressed
//! resource through the owning widget and permission-filters it before
//! activation.
//!
//! One registry, one renderer, one flow: input is the shared
//! [`SearchField`]; results render through
//! [`SearchResultsView`], grouped by kind.
//! An action row resolves to its [`EncodedAction`] and
//! [`dispatch_with_context`](tilo_actions::dispatch_with_context)es through `tilo_actions` — the same
//! path a remote peer replays — with a
//! [`Prompted`](tilo_actions::ActionInput::Prompted) action first collecting
//! its argument in the field. A canonical search hit resolves to that same
//! action machinery. Only a legacy content row hands its opaque `action_id`
//! back to the app to navigate.
//!
//! The host ([`crate::app::App`]) drains a completed activation with
//! [`take_operation`](ActionPalette::take_operation) (actions) and
//! [`take_navigation`](ActionPalette::take_navigation) (content).

use crate::bound_dispatch::{DeferredIntent, PreparedActionIntent};
use crate::widget::ModelPath;
use tilo_sort::CompactSort as _;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use automerge::ObjId;
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::search::{
    Facet, FacetFilter, FieldWeight, ListIndex, PagePoll, PendingToken, ScopeDecl, SearchCursor,
    SearchGroup, SearchProvider, SearchQuery, SearchResult, result_relevance,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{SearchField, SearchResultsView, Widget};
use tilo_actions::{
    ActionContext, ActionInput, ConfirmationPolicy, EncodedAction, OperationKind, Resource, flatten,
};
#[cfg(test)]
use tilo_actions::{ActionRegistry, ActionTransport, Dispatched};

/// The group header for the focused app's exported actions.
pub const ACTIONS_KIND: &str = "Actions";

/// Row-id prefix for search-provider (navigable content) rows, so a content
/// row can never collide with a flattened action key.
const NAV_PREFIX: &str = "\u{1}nav\u{1f}";

/// Row-id prefix for "Search in" rows, which narrow the palette to a scope.
const SCOPE_PREFIX: &str = "\u{1}scope\u{1f}";

/// The group "Search in" rows are listed under.
const SCOPES_KIND: &str = "Search in";

/// Card geometry, matched to the shared search surface (`search_showcase`).
/// The query field is a large control: the palette exists to be typed into.
const FIELD_SIZE: crate::widget::ControlSize = crate::widget::ControlSize::Large;
const GAP: f64 = 10.0;
/// One row of facet chips, a small control's height.
const FACET_H: f64 = 28.0;
/// Cap per group so one busy provider can't flood the overlay.
// Dense resource types routinely expose a handful of collection actions plus
// contextual item actions. Eight hid basic operations such as Rename/Delete
// as soon as Notes added one platform-visible daily-note action.
const PER_GROUP_CAP: usize = 12;
/// Maximum number of provider hits materialised for one keystroke. Providers
/// may emit several groups, so leave room for each while keeping work bounded.
const PROVIDER_HIT_BUDGET: usize = 32;

/// Diagnostics for one bounded progressive-search step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchAdvance {
    /// Query candidates or retained authorization rows checked in this step.
    pub inspected: usize,
    pub added: usize,
    pub pending: bool,
    pub generation: u64,
}

/// How activating one row resolves.
#[derive(Clone)]
enum RowResolution {
    /// An exported action, deliberately retained as an inert address. It is
    /// resolved against the latest resource forest only when activated.
    Address(tilo_actions::ActionAddress),
    Bound(crate::bound_dispatch::BoundActionTarget),
    /// A legacy search-provider hit — hand the opaque id back to the app.
    Navigate(String, Option<crate::bound_dispatch::CallbackBinding>),
}

/// A resource-scoped action's path to an [`EncodedAction`].
#[derive(Clone)]
enum ActionKind {
    /// Fires immediately with this pre-encoded action.
    NoArgument {
        intent: PreparedActionIntent,
        context: ActionContext,
        kind: OperationKind,
        confirmation: ConfirmationPolicy,
    },
    /// Collects a free-text argument in the field first, then builds the action.
    Prompted {
        prompt: String,
        build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
        context: ActionContext,
        kind: OperationKind,
        confirmation: ConfirmationPolicy,
    },
    /// Collects the canonical JSON object for a structured or
    /// multi-parameter action, then uses the `ActionSpec`'s strict binder.
    Typed {
        prompt: String,
        spec: Box<tilo_actions::ActionSpec>,
        context: ActionContext,
        kind: OperationKind,
        confirmation: ConfirmationPolicy,
    },
}

/// Live state while the field is collecting a prompted action's argument.
struct ArgState {
    bound: Option<crate::bound_dispatch::BoundActionTarget>,
    address: tilo_actions::ActionAddress,
    history_target: Option<crate::bound_dispatch::legacy_document::LocalResourceTarget>,
    history_draft_identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
}

#[derive(Clone)]
struct ConfirmState {
    pending: PendingOperation,
    expected: String,
}

fn confirmation_instruction(expected: &str) -> String {
    format!("Type {expected} to confirm")
}

#[derive(Clone)]
enum PendingInput {
    Immediate,
    Text(String),
    Json(String),
    Invalid,
}

/// A palette activation after address resolution and argument collection.
#[derive(Clone)]
pub struct PendingOperation {
    pub(crate) ordinary: Option<crate::bound_dispatch::OrdinaryConfirmation>,
    pub(crate) bound: Option<crate::bound_dispatch::BoundActionTarget>,
    /// The address whose current authority must still be present at dispatch.
    pub address: tilo_actions::ActionAddress,
    intent: PreparedActionIntent,
    pub context: ActionContext,
    pub kind: OperationKind,
    input: PendingInput,
    confirmation: ConfirmationPolicy,
    confirmation_evidence: serde_json::Value,
    pub(crate) history_target: Option<crate::bound_dispatch::legacy_document::LocalResourceTarget>,
    pub(crate) history_draft_identity:
        Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
    pub(crate) history_identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
    pub(crate) history_witness: Option<Result<crate::bound_dispatch::ControlWitness, String>>,
}

/// The confirmation `spec` needs for exactly this collected input. A generic
/// edit's need depends on its value, so it is decided per activation, never
/// read off the declaration alone.
fn effective_confirmation(
    spec: &tilo_actions::ActionSpec,
    input: &PendingInput,
) -> ConfirmationPolicy {
    match input {
        PendingInput::Immediate => spec.confirmation_for_argument(None),
        PendingInput::Text(value) => spec.confirmation_for_argument(Some(value)),
        PendingInput::Json(value) => {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value).map_or_else(
                |_| spec.contract.confirmation.clone(),
                |arguments| spec.confirmation_for_json(&arguments),
            )
        }
        PendingInput::Invalid => spec.contract.confirmation.clone(),
    }
}

impl PendingOperation {
    /// A fixed/prepared value, when this intent already has one. Deferred
    /// creation deliberately has no encoding until final owner execution.
    pub fn encoded(&self) -> Option<&EncodedAction> {
        self.intent.encoded()
    }

    pub(crate) fn encode_at_local_owner(
        &self,
        resources: &[Resource],
    ) -> Result<EncodedAction, tilo_actions::DoorError> {
        let spec = tilo_actions::door::resolve(resources, &self.address)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(self.address.clone()))?;
        match &self.intent {
            PreparedActionIntent::Encoded(encoded) => Ok(encoded.clone()),
            PreparedActionIntent::Deferred(intent) => {
                intent.validate(spec)?;
                if spec.locus != tilo_actions::ExecutionLocus::ClientLocal {
                    return Err(tilo_actions::DoorError::Failed(
                        "Deferred server execution must use the address-based broker door".into(),
                    ));
                }
                tilo_actions::door::encode(resources, &self.address, None)
            }
        }
    }

    /// Reconstruct the door argument collected by the palette. Keeping the
    /// typed JSON prefix makes palette, semantic, button, and agent calls use
    /// the same strict binder in the application host.
    pub fn wire_argument(&self) -> Option<String> {
        match &self.input {
            PendingInput::Immediate | PendingInput::Invalid => None,
            PendingInput::Text(value) => Some(value.clone()),
            PendingInput::Json(value) => {
                Some(format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, value))
            }
        }
    }

    pub(crate) fn has_valid_input(&self) -> bool {
        !matches!(self.input, PendingInput::Invalid)
    }

    pub fn confirmation_evidence(&self) -> &serde_json::Value {
        &self.confirmation_evidence
    }

    pub(crate) fn control_invocation(&self) -> Option<tilo_actions::ControlInvocation> {
        let arguments = match &self.input {
            // Text is retained in continuation_arguments/wire_argument. This
            // address-only descriptor must never invent a JSON parameter schema.
            PendingInput::Immediate | PendingInput::Text(_) => serde_json::Map::new(),
            PendingInput::Json(value) => serde_json::from_str(value).ok()?,
            PendingInput::Invalid => return None,
        };
        Some(tilo_actions::ControlInvocation {
            address: self.address.clone(),
            arguments,
        })
    }

    pub(crate) fn continuation_arguments(
        &self,
    ) -> Option<crate::bound_dispatch::ControlContinuationArguments> {
        match &self.input {
            PendingInput::Immediate => {
                Some(crate::bound_dispatch::ControlContinuationArguments::Immediate)
            }
            PendingInput::Text(value) => Some(
                crate::bound_dispatch::ControlContinuationArguments::Text(value.clone()),
            ),
            PendingInput::Json(value) => serde_json::from_str(value)
                .ok()
                .map(crate::bound_dispatch::ControlContinuationArguments::Json),
            PendingInput::Invalid => None,
        }
    }
}

/// Retained display validation belongs to the provider that produced the row.
/// None hides the row until its finite queued check completes.
struct ProviderRowValidation {
    provider: usize,
    permissions: Option<crate::permissions::PermissionContext>,
}

/// The unified Ctrl+K palette: a [`SearchField`] over a [`SearchResultsView`],
/// fed by the focused app's resource/action tree plus any folded-in
/// [`SearchProvider`]s.
pub struct ActionPalette {
    field: SearchField,
    results: SearchResultsView,
    /// Row `action_id` → how activating it resolves.
    index: HashMap<String, RowResolution>,
    base_index: HashMap<String, RowResolution>,
    action_rows: Vec<SearchResult>,
    provider_rows: Vec<SearchResult>,
    provider_index: HashMap<String, RowResolution>,
    provider_validation: HashMap<String, ProviderRowValidation>,
    validation_queue: VecDeque<String>,
    /// The focused instance's open resources (their exported actions are rows).
    resources: Vec<Resource>,
    bound_resources: Vec<crate::bound_dispatch::BoundResourceSnapshot>,
    /// Searchable-content sources folded in as navigable result groups.
    providers: Vec<Box<dyn SearchProvider>>,
    provider_cursors: Vec<Option<SearchCursor>>,
    /// A provider's outstanding slow page, polled until it arrives.
    provider_pending: Vec<Option<PendingToken>>,
    provider_complete: Vec<bool>,
    next_provider: usize,
    /// The free text of the query, which ranks rows.
    search_query: String,
    /// The whole query providers receive: text, facet filters and scope.
    query: SearchQuery,
    /// The part of the app the palette is narrowed to ("Only Inbox").
    scope: Option<ScopeDecl>,
    /// Every provider's facets, merged by key.
    facets: Vec<Facet>,
    /// Every provider's search scopes, offered as "Search in" rows.
    scopes: Vec<ScopeDecl>,
    /// Facet values picked as chips (typed tokens live in the field text).
    picked_filters: Vec<FacetFilter>,
    /// The facet chips as last laid out: rect, filter, label.
    facet_chips: Vec<(Rect, FacetFilter, String)>,
    /// Each facet group's label as last laid out, before its chips.
    facet_labels: Vec<(Point, String)>,
    last_cursor: Point,
    /// A revoked selection stays unselected until restored or deliberately changed.
    missing_selection: Option<String>,
    search_generation: u64,
    /// The exported actions' titles and descriptions, indexed while they
    /// stay the same.
    action_index: ListIndex,
    /// [`crate::search::engine_generation`] the rows were ranked with:
    /// where the index's code loads on first use, an open palette ranks
    /// again once it is in.
    engine_generation: u64,
    search_announcement: Option<String>,
    open: bool,
    /// `Some` while collecting a prompted action's argument in the field.
    arg: Option<ArgState>,
    /// `Some` while collecting the explicit confirmation required by the
    /// selected action contract.
    confirm: Option<ConfirmState>,
    /// A ready-to-dispatch action, set on activation, drained by `take_operation`.
    pending_dispatch: Option<PendingOperation>,
    history_suspended: bool,
    history_review_required: bool,
    history_resolution_error: Option<String>,
    ambient_context: Option<ActionContext>,
    /// A navigation target, set on activation, drained by `take_navigation`.
    pending_nav: Option<(String, Option<crate::bound_dispatch::CallbackBinding>)>,
    /// The palette's outer rect (field + results), set by `layout`.
    rect: Rect,
    /// The field's own layout tree, rebuilt each `layout` so its `handle_event`
    /// can resolve the inner text input's node.
    field_taffy: TaffyTree<()>,
    field_node: Option<NodeId>,
    /// A text renderer kept for the field's per-frame measure pass, so `layout`
    /// does not construct a font system every frame the palette is open.
    measure: Option<TextRenderer>,
    field_source: PaletteFieldSource,
    request_generation: uuid::Uuid,
    controls: crate::widget::Row,
    controls_node: Option<NodeId>,
    command: std::rc::Rc<std::cell::Cell<Option<bool>>>,
    /// Its arrival (`motion::Appear`, from its centre): a shortcut opens it
    /// at once, a click brings it in.
    appear: crate::motion::Appearing,
}

enum PaletteFieldSource {
    Factory(fn() -> SearchField),
    Language(String),
}
impl PaletteFieldSource {
    fn build(&self) -> SearchField {
        match self {
            Self::Factory(factory) => factory(),
            Self::Language(language) => SearchField::with_language(language),
        }
    }
}

impl Default for ActionPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPalette {
    /// The action the palette is asking the person to confirm, and the text
    /// that confirms it, while it asks.
    #[must_use]
    pub fn asking(&self) -> Option<(&tilo_actions::ActionAddress, &str)> {
        self.confirm
            .as_ref()
            .map(|confirm| (&confirm.pending.address, confirm.expected.as_str()))
    }

    /// A closed palette. Load resources / providers, then `open`.
    pub fn new() -> Self {
        Self::with_inputs(TextRenderer::new(), SearchField::new)
    }

    /// A closed palette that owns an explicitly configured layout renderer.
    pub fn with_text_renderer(renderer: TextRenderer) -> Self {
        let language = renderer.font_system.locale().to_owned();
        Self::with_source(Some(renderer), PaletteFieldSource::Language(language))
    }

    /// Use the host's explicit font construction input for the real initializer.
    pub(crate) fn with_inputs(measure: TextRenderer, field_factory: fn() -> SearchField) -> Self {
        Self::with_source(Some(measure), PaletteFieldSource::Factory(field_factory))
    }

    pub(crate) fn with_host_text(language: String) -> Self {
        Self::with_source(None, PaletteFieldSource::Language(language))
    }

    fn with_source(measure: Option<TextRenderer>, field_source: PaletteFieldSource) -> Self {
        let command = std::rc::Rc::new(std::cell::Cell::new(None));
        let submit = command.clone();
        let cancel = command.clone();
        let controls = crate::widget::Row::new()
            .push(Box::new(
                crate::widget::Button::new("palette-submit", "Confirm")
                    .on_click(move || submit.set(Some(true))),
            ))
            .push(Box::new(
                crate::widget::Button::new("palette-cancel", "Cancel")
                    .on_click(move || cancel.set(Some(false))),
            ));
        Self {
            field: field_source
                .build()
                .control_size(FIELD_SIZE)
                .with_placeholder("Search actions and results"),
            results: SearchResultsView::new()
                .with_empty_text("No matches")
                .with_test_id("action-palette-results"),
            index: HashMap::new(),
            base_index: HashMap::new(),
            action_rows: Vec::new(),
            provider_rows: Vec::new(),
            provider_index: HashMap::new(),
            provider_validation: HashMap::new(),
            validation_queue: VecDeque::new(),
            resources: Vec::new(),
            bound_resources: Vec::new(),
            providers: Vec::new(),
            provider_cursors: Vec::new(),
            provider_pending: Vec::new(),
            provider_complete: Vec::new(),
            next_provider: 0,
            search_query: String::new(),
            query: SearchQuery::default(),
            scope: None,
            facets: Vec::new(),
            scopes: Vec::new(),
            picked_filters: Vec::new(),
            facet_chips: Vec::new(),
            facet_labels: Vec::new(),
            last_cursor: Point::ZERO,
            missing_selection: None,
            search_generation: 0,
            action_index: ListIndex::default(),
            engine_generation: crate::search::engine_generation(),
            search_announcement: None,
            open: false,
            arg: None,
            confirm: None,
            pending_dispatch: None,
            history_suspended: false,
            history_review_required: false,
            history_resolution_error: None,
            ambient_context: None,
            pending_nav: None,
            rect: Rect::ZERO,
            field_taffy: TaffyTree::new(),
            field_node: None,
            measure,
            field_source,
            request_generation: uuid::Uuid::new_v4(),
            controls,
            controls_node: None,
            command,
            appear: crate::motion::Appearing::closed(),
        }
    }

    /// Populate from the focused instance's open-resource forest. Each exported
    /// action becomes an **Actions** row; rebuilds the result set + index.
    pub fn load_open_resources(&mut self, resources: &[Resource]) {
        self.resources = resources.to_vec();
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    /// Replace only the authority snapshot, without moving the visible result
    /// rows or cursor. The host calls this immediately before handling input;
    /// a selected row can then disappear/refuse safely instead of changing the
    /// user's selection to a different action during the same key press.
    pub fn refresh_open_resources(&mut self, resources: &[Resource]) {
        if self.history_suspended {
            return;
        }
        self.resources = resources.to_vec();
    }

    /// Refresh displayed authority asynchronously without moving an existing
    /// selection onto a different control. Argument/confirmation fields retain
    /// their original address; dispatch still rechecks the fresh authority.
    pub fn refresh_visible_resources(&mut self, resources: &[Resource]) {
        self.resources = resources.to_vec();
        if self.in_argument_mode() {
            return;
        }
        let selected = self
            .results
            .selected_action_id()
            .map(str::to_owned)
            .or_else(|| self.missing_selection.take());
        let query = self.field.value().to_owned();
        self.rebuild(&query);
        if let Some(id) = selected {
            if !self.results.restore_action_selection(&id) {
                self.missing_selection = Some(id);
            }
        }
    }

    /// Resource ids referenced by the current canonical search rows.
    ///
    /// The host uses these inert ids to resolve lazy resources through the
    /// owning widget. Providers deliberately cannot supply authority.
    pub fn referenced_action_resource_ids(&self) -> Vec<String> {
        let mut ids = self
            .index
            .values()
            .filter_map(|resolution| match resolution {
                RowResolution::Address(address) => Some(address.resource.clone()),
                RowResolution::Navigate(..) | RowResolution::Bound(_) => None,
            })
            .collect::<Vec<_>>();
        ids.compact_sort();
        ids.dedup();
        ids
    }

    pub fn set_ambient_context(&mut self, context: Option<ActionContext>) {
        self.ambient_context = context;
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    /// Fold in searchable-content sources. Canonically addressed hits dispatch
    /// through the resource/action path; legacy opaque hits remain navigable.
    pub fn set_providers(&mut self, providers: Vec<Box<dyn SearchProvider>>) {
        self.providers = providers;
        self.facets = merged_facets(&self.providers);
        self.scopes = self
            .providers
            .iter()
            .flat_map(|provider| provider.search_scopes())
            .fold(Vec::new(), |mut scopes: Vec<ScopeDecl>, scope| {
                if !scopes.iter().any(|known| known.anchor == scope.anchor) {
                    scopes.push(scope);
                }
                scopes
            });
        self.picked_filters
            .retain(|filter| self.facets.iter().any(|facet| facet.key == filter.key));
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn activate_ordinary(
        &mut self,
        request: crate::bound_dispatch::OrdinaryConfirmation,
    ) {
        use crate::bound_dispatch::ControlContinuationArguments;
        let input = match &request.arguments {
            ControlContinuationArguments::Immediate => PendingInput::Immediate,
            ControlContinuationArguments::Text(value) => PendingInput::Text(value.clone()),
            ControlContinuationArguments::Json(value) => {
                PendingInput::Json(serde_json::Value::Object(value.clone()).to_string())
            }
        };
        let bound = match &request.target {
            crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(target) => {
                Some(target.clone())
            }
            _ => None,
        };
        let pending = PendingOperation {
            address: request.address.clone(),
            intent: request.intent().clone(),
            context: request.context.clone(),
            kind: request.kind(),
            input,
            confirmation: request.confirmation().clone(),
            confirmation_evidence: serde_json::json!({}),
            bound,
            ordinary: Some(request),
            history_target: None,
            history_draft_identity: None,
            history_identity: None,
            history_witness: None,
        };
        self.open = true;
        self.arg = None;
        self.pending_dispatch = None;
        self.queue_or_confirm(pending.clone(), pending.confirmation);
    }

    pub(crate) fn ordinary_request(&self) -> Option<&crate::bound_dispatch::OrdinaryConfirmation> {
        self.pending_dispatch
            .as_ref()
            .or_else(|| self.confirm.as_ref().map(|s| &s.pending))?
            .ordinary
            .as_ref()
    }

    /// Capture a palette-row request immediately after the real argument/row
    /// transition, before any later host frame can change its owner or value.
    pub(crate) fn ordinary_capture_request(&self) -> Option<PendingOperation> {
        self.pending_dispatch
            .as_ref()
            .or_else(|| self.confirm.as_ref().map(|s| &s.pending))
            .filter(|p| {
                p.ordinary.is_none()
                    && p.history_target.is_none()
                    && !matches!(p.confirmation, ConfirmationPolicy::None)
            })
            .cloned()
    }
    pub(crate) fn retain_ordinary(&mut self, request: crate::bound_dispatch::OrdinaryConfirmation) {
        if let Some(p) = self.pending_dispatch.as_mut() {
            p.ordinary = Some(request);
        } else if let Some(c) = self.confirm.as_mut() {
            c.pending.ordinary = Some(request);
        }
    }
    pub(crate) fn is_presented(&self) -> bool {
        self.open && !self.history_suspended
    }

    /// Follow whether it is presented, with the motion the person allows:
    /// its host calls this every frame, drawn or not, so a palette hidden
    /// and shown again arrives again.
    #[cfg(feature = "native")]
    pub(crate) fn follow_presence(&mut self, motion: crate::tokens::EffectiveMotion) {
        let presented = self.is_presented();
        self.appear.follow(presented, motion);
    }
    fn target(&self, part: &str) -> String {
        format!("ordinary-confirmation:{}:{part}", self.request_generation)
    }
    fn drain_command(&mut self, doc: &mut crate::widget::Edits<'_>, path: &ObjId) {
        match self.command.take() {
            Some(true) => {
                self.focus_part(0);
                self.handle_key(
                    &crate::synthetic_input::key_input(
                        &Key::Named(NamedKey::Enter),
                        ElementState::Pressed,
                    ),
                    &Modifiers::default(),
                    doc,
                    path,
                );
            }
            Some(false) => self.close(),
            None => {}
        }
    }

    fn focus_part(&mut self, index: usize) {
        self.field.set_focused(index == 0);
        for (child, control) in self.controls.children_mut().into_iter().enumerate() {
            control.set_focus(index == child + 1);
        }
    }
    pub(crate) fn route_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        if !self.is_presented() {
            return false;
        }
        if self.handle_key(event, &modifiers, doc, path) {
            return true;
        }
        let mut handled = false;
        let mut focus = None;
        if let Some(node) = self.controls_node {
            self.controls.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &modifiers,
                    state: doc.reborrow(),
                    path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &self.field_taffy,
                },
            );
            if let Some(focus) = focus {
                if let Some(index) = self
                    .field_taffy
                    .children(node)
                    .ok()
                    .and_then(|children| children.iter().position(|child| *child == focus))
                {
                    self.focus_part(index + 1);
                }
            }
        }
        if !handled {
            if let Some(node) = self.field_node {
                let mut focus = None;
                self.field.handle_event(
                    event,
                    &mut crate::widget::EventCtx {
                        scale_factor: scale,
                        modifiers: &modifiers,
                        state: doc.reborrow(),
                        path,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &self.field_taffy,
                    },
                );
                if focus.is_some() {
                    self.focus_part(0);
                }
            }
        }
        self.drain_command(doc, path);
        true
    }

    pub(crate) fn suspend_for_history(&mut self) {
        self.history_suspended = true;
        self.request_generation = uuid::Uuid::new_v4();
    }

    pub(crate) fn history_has_custody(&self) -> bool {
        self.history_resolution_error.is_some()
            || self
                .pending_dispatch
                .as_ref()
                .is_some_and(|pending| pending.history_target.is_some())
            || self
                .confirm
                .as_ref()
                .is_some_and(|confirm| confirm.pending.history_target.is_some())
            || self
                .arg
                .as_ref()
                .is_some_and(|arg| arg.history_target.is_some())
    }

    pub(crate) fn history_pending_bound_target(
        &self,
    ) -> Option<crate::bound_dispatch::BoundActionTarget> {
        self.pending_dispatch
            .as_ref()
            .and_then(|pending| pending.bound.clone())
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|confirm| confirm.pending.bound.clone())
            })
            .or_else(|| self.arg.as_ref().and_then(|arg| arg.bound.clone()))
    }

    pub(crate) fn history_pending_address(&self) -> Option<tilo_actions::ActionAddress> {
        self.pending_dispatch
            .as_ref()
            .map(|pending| pending.address.clone())
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .map(|state| state.pending.address.clone())
            })
            .or_else(|| self.arg.as_ref().map(|state| state.address.clone()))
    }

    pub(crate) fn history_pending_invocation(&self) -> Option<tilo_actions::ControlInvocation> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::control_invocation)
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|state| state.pending.control_invocation())
            })
    }

    pub(crate) fn history_pending_continuation_arguments(
        &self,
    ) -> Option<crate::bound_dispatch::ControlContinuationArguments> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::continuation_arguments)
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|state| state.pending.continuation_arguments())
            })
    }

    pub(crate) fn history_exact_capture_request(
        &self,
    ) -> Option<(
        crate::bound_dispatch::legacy_document::LocalResourceTarget,
        tilo_actions::ControlInvocation,
        crate::bound_dispatch::ControlContinuationArguments,
    )> {
        let pending = self
            .pending_dispatch
            .as_ref()
            .or_else(|| self.confirm.as_ref().map(|state| &state.pending))?;
        if pending.history_witness.is_some() {
            return None;
        }
        Some((
            pending.history_target.clone()?,
            pending.control_invocation()?,
            pending.continuation_arguments()?,
        ))
    }

    pub(crate) fn retain_history_exact_witness(
        &mut self,
        witness: Result<crate::bound_dispatch::ControlWitness, String>,
    ) {
        let identity = witness
            .as_ref()
            .map(crate::bound_dispatch::ControlWitness::identity)
            .cloned()
            .map_err(Clone::clone);
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_identity = Some(identity);
            pending.history_witness = Some(witness);
        } else if let Some(confirm) = self.confirm.as_mut() {
            confirm.pending.history_identity = Some(identity);
            confirm.pending.history_witness = Some(witness);
        }
    }

    pub(crate) fn retain_history_custody(
        &mut self,
        target: crate::bound_dispatch::legacy_document::LocalResourceTarget,
        identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
        witness: Option<Result<crate::bound_dispatch::ControlWitness, String>>,
    ) {
        // An unfinished request gets one baseline, including failed evidence.
        if self.history_has_custody() {
            return;
        }
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_target = Some(target);
            pending.history_identity = identity;
            pending.history_witness = witness;
        } else if let Some(confirm) = self.confirm.as_mut() {
            confirm.pending.history_target = Some(target);
            confirm.pending.history_identity = identity;
            confirm.pending.history_witness = witness;
        } else if let Some(arg) = self.arg.as_mut() {
            arg.history_target = Some(target);
            arg.history_draft_identity = identity;
        }
    }

    pub(crate) fn retain_history_resolution_error(&mut self, reason: impl Into<String>) {
        if !self.history_has_custody() {
            self.history_resolution_error = Some(reason.into());
        }
    }

    pub(crate) fn retain_history_refusal(
        &mut self,
        pending: PendingOperation,
        reason: impl Into<String>,
    ) {
        let reason = reason.into();
        self.pending_dispatch = Some(pending);
        self.history_review_required = true;
        self.open = true;
        self.search_announcement = Some(reason);
    }

    pub(crate) fn resume_after_history(&mut self) {
        self.history_suspended = false;
        let Some(mut pending) = self
            .pending_dispatch
            .take()
            .or_else(|| self.confirm.take().map(|state| state.pending))
        else {
            return;
        };
        pending.confirmation_evidence = serde_json::json!({});
        let expected = match &pending.confirmation {
            ConfirmationPolicy::None => {
                self.pending_dispatch = Some(pending);
                self.history_review_required = true;
                self.open = true;
                return;
            }
            ConfirmationPolicy::Confirm => "CONFIRM".to_owned(),
            ConfirmationPolicy::Phrase { phrase } => phrase.clone(),
        };
        self.field = self.field_source.build().control_size(FIELD_SIZE);
        self.field
            .set_placeholder(&confirmation_instruction(&expected));
        self.focus_part(0);
        self.results.set_groups(Vec::new());
        self.confirm = Some(ConfirmState { pending, expected });
        self.open = true;
    }

    #[cfg(test)]
    pub(crate) fn is_history_suspended(&self) -> bool {
        self.history_suspended
    }

    /// Open (focus) the palette: reset the query, argument, and any pending
    /// outcome, and rebuild the (empty-query) result set.
    pub(crate) fn load_bound_resources(
        &mut self,
        snapshots: Vec<crate::bound_dispatch::BoundResourceSnapshot>,
    ) {
        if self.history_suspended {
            return;
        }
        self.bound_resources = snapshots;
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }
    pub(crate) fn activate_bound(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
    ) -> bool {
        let key = target.key();
        if self.index.contains_key(&key) {
            self.resolve_activation(&key);
            true
        } else {
            false
        }
    }

    pub fn open(&mut self) {
        self.history_resolution_error = None;
        self.open = true;
        self.arg = None;
        self.confirm = None;
        self.pending_dispatch = None;
        self.history_review_required = false;
        self.pending_nav = None;
        self.field = self.query_field();
        self.focus_part(0);
        self.rebuild("");
    }

    /// The query field, showing the scope the palette is narrowed to.
    fn query_field(&self) -> SearchField {
        let mut field = self
            .field_source
            .build()
            .control_size(FIELD_SIZE)
            .with_placeholder("Search actions and results");
        field.set_scope_token(self.scope.as_ref().map(scope_token_label));
        field
    }

    /// Narrow the palette to one part of the app, or widen it to the whole
    /// app with `None`. The field shows the scope as a removable token.
    pub fn set_scope(&mut self, scope: Option<ScopeDecl>) {
        self.scope = scope;
        self.field
            .set_scope_token(self.scope.as_ref().map(scope_token_label));
        let query = self.field.value().to_owned();
        self.rebuild(&query);
    }

    /// The part of the app the palette is narrowed to.
    pub fn scope(&self) -> Option<&ScopeDecl> {
        self.scope.as_ref()
    }

    /// Select the query, so typing replaces a pre-filled search.
    pub fn select_query(&mut self) {
        self.field.select_all();
    }

    /// The facets the providers declared, offered as chips.
    pub fn facets(&self) -> &[Facet] {
        &self.facets
    }

    /// The query providers receive, with its parsed filters and scope.
    pub fn search_query(&self) -> &SearchQuery {
        &self.query
    }

    /// Pick or unpick a facet value, as clicking its chip does. A value that
    /// was typed as a `key:value` token is removed from the text.
    pub fn toggle_facet(&mut self, filter: &FacetFilter) {
        if let Some(at) = self.picked_filters.iter().position(|f| f == filter) {
            self.picked_filters.remove(at);
        } else if SearchQuery::parse(self.field.value(), &self.facets)
            .filters()
            .contains(filter)
        {
            let kept = without_token(self.field.value(), filter);
            self.field.set_query(&kept);
        } else {
            self.picked_filters.push(filter.clone());
        }
        let query = self.field.value().to_owned();
        self.rebuild(&query);
    }

    pub fn close(&mut self) {
        self.history_resolution_error = None;
        self.pending_dispatch = None;
        self.open = false;
        self.arg = None;
        self.confirm = None;
        self.history_review_required = false;
    }

    pub fn query(&self) -> &str {
        self.field.value()
    }

    /// Set the query programmatically and rebuild the result set (for tests /
    /// hand-off from another surface).
    pub fn set_query(&mut self, q: &str) {
        self.field.set_query(q);
        self.rebuild(q);
    }

    /// The labels currently shown, in display order (for tests / diagnostics).
    pub fn result_labels(&self) -> Vec<String> {
        self.results
            .groups()
            .iter()
            .flat_map(|g| g.results.iter().map(|r| r.title.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Advance one bounded query page or one finite retained-validation step.
    /// A completed-query external redraw may start a fresh validation pass;
    /// its final step returns pending=false so it never schedules another pass.
    pub fn advance_search_page(&mut self) -> SearchAdvance {
        if self.history_suspended {
            return SearchAdvance {
                generation: self.search_generation,
                ..SearchAdvance::default()
            };
        }
        // Prompt/confirmation owns the empty result surface. Preserve any
        // partial pass until the existing exit/open rebuild restarts search.
        if self.in_argument_mode() {
            return SearchAdvance {
                generation: self.search_generation,
                ..SearchAdvance::default()
            };
        }
        let engine_generation = crate::search::engine_generation();
        if self.engine_generation != engine_generation {
            self.engine_generation = engine_generation;
            if self.open {
                let typed = self.field.value().to_owned();
                self.rebuild(&typed);
            }
        }
        if self.validation_queue.is_empty() && self.provider_complete.iter().all(|done| *done) {
            for (key, validation) in &mut self.provider_validation {
                validation.permissions = None;
                self.validation_queue.push_back(key.clone());
            }
        }
        self.queue_stale_provider_rows();
        if !self.validation_queue.is_empty() {
            return self.validate_provider_rows();
        }
        let generation = self.search_generation;
        let provider_count = self.providers.len();
        if provider_count == 0 || self.provider_complete.iter().all(|done| *done) {
            return SearchAdvance {
                pending: false,
                generation,
                ..SearchAdvance::default()
            };
        }

        let selected = (self.next_provider..provider_count)
            .chain(0..self.next_provider)
            .find(|candidate| {
                !self
                    .provider_complete
                    .get(*candidate)
                    .copied()
                    .unwrap_or(true)
            });
        let Some(provider_index) = selected else {
            return SearchAdvance {
                pending: false,
                generation,
                ..SearchAdvance::default()
            };
        };
        self.next_provider = provider_index
            .checked_add(1)
            .filter(|next| *next < provider_count)
            .unwrap_or(0);
        let cursor = self.provider_cursors.get(provider_index).cloned().flatten();
        let Some(provider) = self.providers.get(provider_index) else {
            return SearchAdvance {
                pending: false,
                generation,
                ..SearchAdvance::default()
            };
        };
        let binding = provider.callback_binding();
        let waiting = self.provider_pending.get(provider_index).cloned().flatten();
        let page = match waiting {
            // A slow source answers on a later frame; ask, never wait.
            Some(token) => match provider.poll_page(&token) {
                PagePoll::Pending => {
                    return SearchAdvance {
                        pending: true,
                        generation,
                        ..SearchAdvance::default()
                    };
                }
                PagePoll::Ready(page) => page,
            },
            None => provider.search_page(
                &self.query,
                cursor.as_ref(),
                PROVIDER_HIT_BUDGET,
                PROVIDER_HIT_BUDGET,
            ),
        };
        let mut hits = page.results;
        // The scope narrows every provider, including one that ignores it.
        hits.retain(|hit| self.query.in_scope(&hit.anchor()));
        let results = self.checked_provider_hits(provider.as_ref(), binding.as_ref(), hits);
        let complete = page.next.is_none() && page.pending.is_none();
        if let Some(slot) = self.provider_cursors.get_mut(provider_index) {
            *slot = page.next;
        }
        if let Some(slot) = self.provider_pending.get_mut(provider_index) {
            *slot = page.pending;
        }
        if let Some(slot) = self.provider_complete.get_mut(provider_index) {
            *slot = complete;
        }

        let inspected = page.inspected;
        let added = self.merge_provider_page(results, binding.as_ref(), provider_index);
        self.queue_stale_provider_rows();
        self.refresh_progressive_results();
        let pending =
            !self.validation_queue.is_empty() || self.provider_complete.iter().any(|done| !*done);
        self.results.set_empty_text(if pending {
            "Searching…"
        } else {
            "No matches"
        });
        self.results
            .set_loading(self.provider_pending.iter().any(Option::is_some));
        let count = self.results.len();
        if added > 0 || !pending {
            self.search_announcement = Some(if pending {
                format!("{count} results, still searching")
            } else {
                format!("{count} results")
            });
        }
        SearchAdvance {
            inspected,
            added,
            pending,
            generation,
        }
    }

    fn bound_provider_hit_allowed(
        &self,
        provider: &dyn SearchProvider,
        binding: &crate::bound_dispatch::CallbackBinding,
        address: Option<&tilo_actions::ActionAddress>,
    ) -> bool {
        let Ok(_entry) = binding.enter() else {
            return false;
        };
        let Some(address) = address else {
            return binding
                .permissions
                .borrow()
                .check(&crate::permissions::PermissionScope::app(
                    &binding.app_id,
                    crate::permissions::ActionKind::Read,
                ));
        };
        // Provider resolution runs through its captured owner. Eager resources
        // remain a valid source for providers that emit only canonical hits.
        let resource = provider.resolve_action_resource(address).or_else(|| {
            self.bound_resources
                .iter()
                .find(|snapshot| snapshot.binding.same_component(binding))
                .and_then(|snapshot| {
                    snapshot
                        .resources()
                        .iter()
                        .find(|resource| {
                            tilo_actions::door::resolve(std::slice::from_ref(resource), address)
                                .is_some()
                        })
                        .cloned()
                })
        });
        resource
            .and_then(|resource| crate::bound_dispatch::filter_resource(binding, resource))
            .is_some_and(|resource| tilo_actions::door::resolve(&[resource], address).is_some())
    }

    fn checked_provider_hits(
        &self,
        provider: &dyn SearchProvider,
        binding: Option<&crate::bound_dispatch::CallbackBinding>,
        hits: Vec<SearchResult>,
    ) -> Vec<(SearchResult, Option<crate::permissions::PermissionContext>)> {
        hits.into_iter()
            .filter_map(|hit| {
                let Some(binding) = binding else {
                    return Some((hit, None));
                };
                let permissions = binding.permissions.borrow().clone();
                (self.bound_provider_hit_allowed(provider, binding, hit.action_address().as_ref())
                    && permissions == *binding.permissions.borrow())
                .then_some((hit, Some(permissions)))
            })
            .collect()
    }

    fn provider_row_binding(
        &self,
        key: &str,
    ) -> Option<(
        &crate::bound_dispatch::CallbackBinding,
        Option<&tilo_actions::ActionAddress>,
    )> {
        match self.provider_index.get(key)? {
            RowResolution::Bound(target) => Some((&target.binding, Some(&target.address.address))),
            RowResolution::Navigate(_, Some(binding)) => Some((binding, None)),
            _ => None,
        }
    }

    fn provider_row_is_current(&self, key: &str) -> bool {
        let Some(validation) = self.provider_validation.get(key) else {
            return true;
        };
        self.provider_row_binding(key).is_some_and(|(binding, _)| {
            !binding.closing.get()
                && binding.lifetime.is_active()
                && validation
                    .permissions
                    .as_ref()
                    .is_some_and(|permissions| *permissions == *binding.permissions.borrow())
        })
    }

    fn queue_stale_provider_rows(&mut self) {
        let stale = self
            .provider_validation
            .iter()
            .filter(|(key, validation)| {
                validation.permissions.is_some() && !self.provider_row_is_current(key)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(validation) = self.provider_validation.get_mut(&key) {
                validation.permissions = None;
                self.validation_queue.push_back(key);
            }
        }
    }

    fn validate_provider_rows(&mut self) -> SearchAdvance {
        let before = self.results.len();
        let mut inspected = 0usize;
        let mut denied = std::collections::HashSet::new();
        for _ in 0..PROVIDER_HIT_BUDGET {
            let Some(key) = self.validation_queue.pop_front() else {
                break;
            };
            let Some(validation) = self.provider_validation.get(&key) else {
                continue;
            };
            inspected = inspected.saturating_add(1);
            let allowed = self
                .provider_row_binding(&key)
                .and_then(|(binding, address)| {
                    let permissions = binding.permissions.borrow().clone();
                    let provider = self.providers.get(validation.provider)?;
                    (self.bound_provider_hit_allowed(provider.as_ref(), binding, address)
                        && permissions == *binding.permissions.borrow()
                        && !binding.closing.get()
                        && binding.lifetime.is_active())
                    .then_some(permissions)
                });
            if let Some(permissions) = allowed {
                if let Some(validation) = self.provider_validation.get_mut(&key) {
                    validation.permissions = Some(permissions);
                }
            } else {
                denied.insert(key);
            }
        }
        self.provider_rows
            .retain(|row| !denied.contains(&row.action_id));
        self.provider_index.retain(|key, _| !denied.contains(key));
        self.provider_validation
            .retain(|key, _| !denied.contains(key));
        // A callback may revoke an owner checked earlier in this same pass.
        // Hide those rows now and enqueue each only on Some -> None transition.
        self.queue_stale_provider_rows();
        self.refresh_progressive_results();
        let pending =
            !self.validation_queue.is_empty() || self.provider_complete.iter().any(|done| !*done);
        self.results.set_empty_text(if pending {
            "Searching…"
        } else {
            "No matches"
        });
        self.search_announcement = Some(format!(
            "{} results{}",
            self.results.len(),
            if pending { ", still searching" } else { "" }
        ));
        SearchAdvance {
            inspected,
            added: self.results.len().saturating_sub(before),
            pending,
            generation: self.search_generation,
        }
    }

    fn merge_provider_page(
        &mut self,
        results: Vec<(SearchResult, Option<crate::permissions::PermissionContext>)>,
        binding: Option<&crate::bound_dispatch::CallbackBinding>,
        provider: usize,
    ) -> usize {
        let before = self.provider_rows.len();
        let mut known = self
            .provider_rows
            .iter()
            .map(|row| row.action_id.clone())
            .collect::<std::collections::HashSet<_>>();
        for (hit, permissions) in results {
            let row_id = binding.map_or_else(
                || format!("{NAV_PREFIX}{}", hit.action_id),
                |binding| format!("{NAV_PREFIX}{}:{}", binding.lifetime_id, hit.action_id),
            );
            if !known.insert(row_id.clone()) {
                continue;
            }
            let resolution = if hit.has_action_address() {
                hit.action_address().map(|address| {
                    binding.map_or_else(
                        || RowResolution::Address(address.clone()),
                        |binding| {
                            RowResolution::Bound(crate::bound_dispatch::BoundActionTarget {
                                input: crate::bound_dispatch::Input::Discrete,
                                binding: (*binding).clone(),
                                address: crate::bound_dispatch::BoundActionAddress {
                                    instance_id: binding.instance_id,
                                    address: address.clone(),
                                },
                            })
                        },
                    )
                })
            } else {
                Some(RowResolution::Navigate(
                    hit.action_id.clone(),
                    binding.cloned(),
                ))
            };
            let Some(resolution) = resolution else {
                continue;
            };
            self.provider_index.insert(row_id.clone(), resolution);
            if permissions.is_some() {
                self.provider_validation.insert(
                    row_id.clone(),
                    ProviderRowValidation {
                        provider,
                        permissions,
                    },
                );
            }
            let mut row = SearchResult::new(hit.kind, hit.title, row_id).with_score(hit.score);
            row.subtitle = hit.subtitle;
            row.preview = hit.preview;
            row.icon = hit.icon;
            row.row = hit.row;
            self.provider_rows.push(row);
        }
        // Retain display candidates per kind. The number of kinds is not
        // capped; retained authorization work is bounded separately.
        self.provider_rows =
            rank_and_group(&self.search_query, std::mem::take(&mut self.provider_rows))
                .into_iter()
                .flat_map(|group| group.results)
                .collect();
        let retained = self
            .provider_rows
            .iter()
            .map(|row| row.action_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.provider_index
            .retain(|row_id, _| retained.contains(row_id.as_str()));
        self.provider_validation
            .retain(|row_id, _| retained.contains(row_id.as_str()));
        self.provider_rows.len().saturating_sub(before)
    }

    /// Drain the polite live-region text produced by progressive search.
    pub fn take_search_announcement(&mut self) -> Option<String> {
        self.search_announcement.take()
    }

    /// Move the highlight to a flat row index (clamped).
    pub fn set_cursor(&mut self, index: usize) {
        self.missing_selection = None;
        self.results.set_cursor(index);
    }

    /// The highlighted flat row index.
    pub fn cursor(&self) -> usize {
        self.results.cursor()
    }

    pub fn in_argument_mode(&self) -> bool {
        self.arg.is_some() || self.confirm.is_some()
    }

    /// Activate the highlighted row, exactly as pressing Enter would.
    pub fn activate_selected(&mut self) {
        let Some(id) = self.results.selected_action_id().map(str::to_string) else {
            return;
        };
        self.resolve_activation(&id);
    }

    /// Enter the argument/confirmation flow for an exact domain address.
    /// Used by accessibility activation so consequential resource nodes cannot
    /// jump around the palette's policy flow.
    pub fn activate_address(&mut self, resource: &str, action: &str) -> bool {
        let id = format!("{resource}{}{action}", tilo_actions::KEY_SEP);
        if self.index.contains_key(&id) {
            self.resolve_activation(&id);
            true
        } else {
            false
        }
    }

    // Superseded in production by the ordinary-confirmation flow
    // (`activate_ordinary`); kept for the argument-capture tests.
    #[cfg(test)]
    pub(crate) fn activate_address_with_arguments(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        if !self.index.contains_key(&address.encode()) && !self.is_programmatic(address, None) {
            return false;
        }
        self.activate_with_arguments(address.clone(), None, arguments)
    }

    /// Whether `address` names a programmatic action in the current authority.
    /// It is never a row, but a control or surface that submitted one still
    /// gets the palette's confirmation step when its value needs it.
    #[cfg(test)]
    fn is_programmatic(
        &self,
        address: &tilo_actions::ActionAddress,
        bound: Option<&crate::bound_dispatch::BoundActionTarget>,
    ) -> bool {
        self.authority_for(address, bound).is_some_and(|resources| {
            tilo_actions::door::resolve(&resources, address)
                .is_some_and(tilo_actions::ActionSpec::is_programmatic)
        })
    }

    #[cfg(test)]
    fn activate_with_arguments(
        &mut self,
        address: tilo_actions::ActionAddress,
        bound: Option<crate::bound_dispatch::BoundActionTarget>,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        let Some(resources) = self.authority_for(&address, bound.as_ref()) else {
            return false;
        };
        let Some(flattened) = flatten(&resources)
            .into_iter()
            .find(|candidate| candidate.key == address.encode())
        else {
            return false;
        };
        if !flattened.action.contract.availability.is_available() {
            return false;
        }
        let Ok(encoded) = flattened.action.build_json(arguments) else {
            return false;
        };
        let mut context = flattened.action.history_context(
            flattened.resource_id.as_str(),
            &flattened.resource_kind,
            &flattened.resource_label,
        );
        if let Some(ambient) = self.ambient_context.as_ref() {
            context = context.enriched_by(ambient);
        }
        let confirmation = flattened.action.confirmation_for_json(arguments);
        self.queue_or_confirm(
            PendingOperation {
                ordinary: None,
                bound,
                address,
                intent: PreparedActionIntent::Encoded(encoded),
                context,
                kind: flattened.action.kind,
                input: PendingInput::Json(serde_json::Value::Object(arguments.clone()).to_string()),
                confirmation: confirmation.clone(),
                confirmation_evidence: serde_json::json!({}),
                history_target: None,
                history_draft_identity: None,
                history_identity: None,
                history_witness: None,
            },
            confirmation,
        );
        true
    }

    /// Drain a completed action activation and dispatch it against `doc` +
    /// `transport`. Decoding goes through [`ActionRegistry::decode`] — the
    /// same path a remote peer uses to replay. `None` when no action is
    /// pending (e.g. the last activation was a navigation). Tests use this to
    /// replay an activation without the application host's executor.
    #[cfg(test)]
    pub fn take_dispatch(
        &mut self,
        doc: &mut crate::widget::Edits<'_>,
        registry: &ActionRegistry,
        transport: &mut dyn ActionTransport,
    ) -> Option<Result<Dispatched, tilo_actions::ActionError>> {
        let pending = self.take_operation()?;
        if pending.kind != OperationKind::Document {
            return Some(Err(tilo_actions::ActionError::Refused(format!(
                "{} operation requires the owning widget",
                pending.kind.as_str()
            ))));
        }
        let resources = self.authority_for(&pending.address, pending.bound.as_ref())?;
        let encoded = match pending.encode_at_local_owner(&resources) {
            Ok(encoded) => encoded,
            Err(error) => return Some(Err(tilo_actions::ActionError::Refused(error.to_string()))),
        };
        if let PreparedActionIntent::Deferred(_) = &pending.intent {
            match registry.decode(&encoded) {
                Ok(action) if action.locus() == tilo_actions::ExecutionLocus::ClientLocal => (),
                _ => {
                    return Some(Err(tilo_actions::ActionError::Refused(
                        "Deferred action declaration does not match the local registry".into(),
                    )));
                }
            }
        }
        let mut context = pending.context;
        let kind = context
            .resource
            .as_ref()
            .map(|resource| resource.kind.clone())
            .unwrap_or_default();
        if let Some(described) = tilo_actions::Described::of_published(
            &encoded,
            tilo_actions::ActionKey::new("", kind, &pending.address.action),
        ) {
            context = context.with_described(described);
        }
        Some(Self::dispatch_encoded(
            doc, &encoded, registry, transport, context,
        ))
    }

    /// Drain the resolved operation without assuming it is a document edit.
    /// The application host uses this to route effects/views to the owning
    /// widget and document operations to the deterministic executor.
    pub fn take_operation(&mut self) -> Option<PendingOperation> {
        if self.history_suspended || self.history_review_required {
            return None;
        }
        if let Some(reason) = self.history_resolution_error.clone() {
            self.search_announcement = Some(reason);
            self.history_review_required = true;
            self.open = true;
            return None;
        }
        let pending = self.pending_dispatch.as_ref()?.clone();
        if pending.history_target.is_some() || pending.ordinary.is_some() {
            self.pending_dispatch = None;
            return Some(pending);
        }
        let authority = self.authority_for(&pending.address, pending.bound.as_ref())?;
        let spec = tilo_actions::door::resolve(&authority, &pending.address)?;
        if !spec.contract.availability.is_available()
            || spec.kind != pending.kind
            || effective_confirmation(spec, &pending.input) != pending.confirmation
        {
            return None;
        }
        if let PreparedActionIntent::Deferred(intent) = &pending.intent {
            if intent.validate(spec).is_err() {
                self.pending_dispatch = None;
                return None;
            }
            self.pending_dispatch = None;
            return Some(pending);
        }
        let encoded = pending.encoded()?;
        let current = match &pending.input {
            PendingInput::Immediate => {
                tilo_actions::door::encode(&authority, &pending.address, None).ok()?
            }
            PendingInput::Text(value) => {
                tilo_actions::door::encode(&authority, &pending.address, Some(value)).ok()?
            }
            PendingInput::Json(value) => {
                let arguments =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value)
                        .ok()?;
                tilo_actions::door::encode_json(&authority, &pending.address, &arguments).ok()?
            }
            PendingInput::Invalid => encoded.clone(),
        };
        if current != *encoded {
            self.pending_dispatch = None;
            return None;
        }
        self.pending_dispatch = None;
        Some(pending)
    }

    fn authority_for(
        &self,
        address: &tilo_actions::ActionAddress,
        bound: Option<&crate::bound_dispatch::BoundActionTarget>,
    ) -> Option<Vec<Resource>> {
        if let Some(target) = bound {
            let _entry = target.binding.enter().ok()?;
            let snapshot = self
                .bound_resources
                .iter()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))?;
            let resources = snapshot
                .resources()
                .iter()
                .cloned()
                .filter_map(|resource| {
                    crate::bound_dispatch::filter_resource(&target.binding, resource)
                })
                .collect::<Vec<_>>();
            return tilo_actions::door::resolve(&resources, address)
                .is_some()
                .then_some(resources);
        }
        tilo_actions::door::resolve(&self.resources, address)
            .is_some()
            .then(|| self.resources.clone())
    }

    /// Drain a completed content activation: the opaque `action_id` of the
    /// chosen search-provider row, for the app to navigate to.
    pub fn take_navigation(&mut self) -> Option<String> {
        if self
            .pending_nav
            .as_ref()
            .is_some_and(|(_, binding)| binding.is_none())
        {
            self.pending_nav.take().map(|(id, _)| id)
        } else {
            None
        }
    }

    pub(crate) fn take_scoped_navigation(
        &mut self,
    ) -> Option<(String, Option<crate::bound_dispatch::CallbackBinding>)> {
        self.pending_nav.take()
    }

    #[cfg(feature = "native")]
    pub(crate) fn referenced_bound_targets(&self) -> Vec<crate::bound_dispatch::BoundActionTarget> {
        self.index
            .values()
            .filter_map(|resolution| match resolution {
                RowResolution::Bound(target) => Some(target.clone()),
                _ => None,
            })
            .collect()
    }

    #[cfg(test)]
    fn dispatch_encoded(
        doc: &mut crate::widget::Edits<'_>,
        encoded: &EncodedAction,
        registry: &ActionRegistry,
        transport: &mut dyn ActionTransport,
        context: ActionContext,
    ) -> Result<Dispatched, tilo_actions::ActionError> {
        if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
            return Err(tilo_actions::ActionError::Refused(
                String::from_utf8_lossy(&encoded.payload).into_owned(),
            ));
        }
        let action = registry.decode(encoded)?;
        let actor = doc.get_actor().to_bytes().to_vec();
        tilo_actions::dispatch_by_with_context(
            doc.document_mut(),
            action.as_ref(),
            encoded.clone(),
            transport,
            Some(&actor),
            context,
        )
    }

    /// Resolve a chosen row id into a pending outcome (or the argument step).
    fn resolve_activation(&mut self, id: &str) {
        // A "Search in" row narrows the palette; the typed text clears so
        // the search starts afresh inside the scope.
        if let Some(anchor) = id.strip_prefix(SCOPE_PREFIX) {
            let scope = self
                .scopes
                .iter()
                .find(|scope| scope.anchor == anchor)
                .cloned();
            if scope.is_some() {
                self.field.set_query("");
                self.set_scope(scope);
            }
            return;
        }
        match self.index.get(id).cloned() {
            Some(resolution @ (RowResolution::Address(_) | RowResolution::Bound(_))) => {
                let (address, bound) = match resolution {
                    RowResolution::Address(address) => (address, None),
                    RowResolution::Bound(target) => (target.address.address.clone(), Some(target)),
                    RowResolution::Navigate(..) => return,
                };
                let Some(kind) = self.action_kind_for_address(&address, bound.as_ref()) else {
                    let query = self.field.value().to_string();
                    self.rebuild(&query);
                    return;
                };
                match kind {
                    ActionKind::NoArgument {
                        intent,
                        context,
                        kind,
                        confirmation,
                    } => {
                        let history_target = None;
                        self.queue_or_confirm(
                            PendingOperation {
                                ordinary: None,
                                bound,
                                address,
                                intent,
                                context,
                                kind,
                                input: PendingInput::Immediate,
                                confirmation: confirmation.clone(),
                                confirmation_evidence: serde_json::json!({}),
                                history_target,
                                history_draft_identity: None,
                                history_identity: None,
                                history_witness: None,
                            },
                            confirmation,
                        );
                    }
                    ActionKind::Prompted { prompt, .. } => {
                        // Repurpose the field to collect the argument; clear the result
                        // list so only the prompt shows.
                        self.field = self.field_source.build().control_size(FIELD_SIZE);
                        self.field.set_placeholder(&prompt);
                        self.focus_part(0);
                        self.results.set_groups(Vec::new());
                        let history_target = None;
                        self.arg = Some(ArgState {
                            bound,
                            address,
                            history_target,
                            history_draft_identity: None,
                        });
                    }
                    ActionKind::Typed { prompt, .. } => {
                        self.field = self.field_source.build().control_size(FIELD_SIZE);
                        self.field.set_placeholder(&prompt);
                        self.focus_part(0);
                        self.results.set_groups(Vec::new());
                        let history_target = None;
                        self.arg = Some(ArgState {
                            bound,
                            address,
                            history_target,
                            history_draft_identity: None,
                        });
                    }
                }
            }
            Some(RowResolution::Navigate(target, binding)) => {
                self.pending_nav = Some((target, binding));
                self.open = false;
            }
            None => {}
        }
    }

    fn action_kind_for_address(
        &self,
        address: &tilo_actions::ActionAddress,
        bound: Option<&crate::bound_dispatch::BoundActionTarget>,
    ) -> Option<ActionKind> {
        let resources = self.authority_for(address, bound)?;
        let fa = flatten(&resources)
            .into_iter()
            .find(|fa| fa.key == address.encode())?;
        if !fa.action.contract.availability.is_available() {
            return None;
        }
        let mut context = fa.action.history_context(
            fa.resource_id.as_str(),
            &fa.resource_kind,
            &fa.resource_label,
        );
        if let Some(ambient) = self.ambient_context.as_ref() {
            context = context.enriched_by(ambient);
        }
        let structured = fa.action.params().len() > 1
            || fa
                .action
                .params()
                .first()
                .is_some_and(|parameter| parameter.ty != tilo_actions::ParamType::Text);
        Some(match &fa.action.input {
            ActionInput::Immediate(enc) => ActionKind::NoArgument {
                intent: PreparedActionIntent::Encoded(enc.as_ref().clone()),
                context,
                kind: fa.action.kind,
                confirmation: fa.action.contract.confirmation.clone(),
            },
            ActionInput::Deferred { .. } => ActionKind::NoArgument {
                intent: PreparedActionIntent::Deferred(DeferredIntent::capture(fa.action).ok()?),
                context,
                kind: fa.action.kind,
                confirmation: fa.action.contract.confirmation.clone(),
            },
            ActionInput::Prompted { .. } if structured => ActionKind::Typed {
                prompt: format!(
                    "JSON arguments: {}",
                    fa.action
                        .params()
                        .iter()
                        .map(|parameter| parameter.name.as_ref())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                spec: Box::new(fa.action.clone()),
                context,
                kind: fa.action.kind,
                confirmation: fa.action.contract.confirmation.clone(),
            },
            ActionInput::Prompted { prompt, build, .. } => ActionKind::Prompted {
                prompt: prompt.clone(),
                build: build.clone(),
                context,
                kind: fa.action.kind,
                confirmation: fa.action.contract.confirmation.clone(),
            },
        })
    }

    /// Rebuild the result set for `query`: exported actions matching it as
    /// the **Actions** group, then each provider's hits as its own group.
    fn rebuild(&mut self, typed: &str) {
        self.missing_selection = None;
        let mut parsed = SearchQuery::parse(typed, &self.facets);
        for filter in &self.picked_filters {
            parsed = parsed.with_filter(filter.clone());
        }
        if let Some(scope) = &self.scope {
            parsed = parsed.within(scope.anchor.clone());
        }
        let text = parsed.text().to_owned();
        let query = text.as_str();
        // Every available action is a candidate row with the text it is
        // found by; the index then keeps the ones matching the query.
        let mut candidates: Vec<(SearchResult, Option<String>, RowResolution)> = Vec::new();

        // Keep only inert addresses in the row index. Encoding and callbacks
        // are resolved later, against the authority snapshot current at the
        // moment of activation.
        for fa in flatten(&self.resources) {
            // Programmatic actions are for agents and scripts, never a row.
            if !fa.action.contract.availability.is_available() || fa.action.is_programmatic() {
                continue;
            }
            // A scoped palette offers the actions of that part of the app.
            if !parsed.in_scope(fa.resource_id.as_str()) {
                continue;
            }
            let title = format!("{} \u{2014} {}", fa.resource_label, fa.action.label);
            let mut r = SearchResult::new(ACTIONS_KIND, title, fa.key.clone());
            if let Some(desc) = &fa.action.description {
                r = r.with_subtitle(desc.clone());
            } else if let Some(detail) = fa
                .resource_presentation
                .as_ref()
                .and_then(|presentation| presentation.detail.as_ref())
            {
                r = r.with_subtitle(detail.clone());
            }
            if let Some(preview) = fa
                .resource_presentation
                .as_ref()
                .and_then(|presentation| presentation.preview.as_ref())
            {
                r = r.with_preview(preview.clone());
            }
            let resolution = RowResolution::Address(tilo_actions::ActionAddress::new(
                fa.resource_id.as_str(),
                fa.action.id.as_str(),
            ));
            candidates.push((r, fa.action.description.clone(), resolution));
        }

        for snapshot in &self.bound_resources {
            let Ok(_entry) = snapshot.binding.enter() else {
                continue;
            };
            for resource in snapshot.resources().iter().cloned().filter_map(|resource| {
                crate::bound_dispatch::filter_resource(&snapshot.binding, resource)
            }) {
                for fa in flatten(&[resource]) {
                    if !fa.action.contract.availability.is_available()
                        || fa.action.is_programmatic()
                    {
                        continue;
                    }
                    let title = format!(
                        "{} — {} · {}",
                        fa.resource_label, fa.action.label, snapshot.binding.instance_id
                    );
                    let target = crate::bound_dispatch::BoundActionTarget {
                        input: crate::bound_dispatch::Input::Discrete,
                        binding: snapshot.binding.clone(),
                        address: crate::bound_dispatch::BoundActionAddress {
                            instance_id: snapshot.binding.instance_id,
                            address: tilo_actions::ActionAddress::new(
                                fa.resource_id.as_str(),
                                &fa.action.id,
                            ),
                        },
                    };
                    let key = target.key();
                    candidates.push((
                        SearchResult::new(ACTIONS_KIND, title, key),
                        None,
                        RowResolution::Bound(target),
                    ));
                }
            }
        }

        // An unscoped palette offers to narrow to each part of the app.
        if self.scope.is_none() {
            for scope in &self.scopes {
                candidates.push((
                    SearchResult::new(
                        SCOPES_KIND,
                        scope.label.clone(),
                        format!("{SCOPE_PREFIX}{}", scope.anchor),
                    )
                    .with_icon(crate::icons::Icon::Search),
                    None,
                    RowResolution::Navigate(format!("{SCOPE_PREFIX}{}", scope.anchor), None),
                ));
            }
        }

        let ranks = self.action_index.ranks(
            query,
            candidates.iter().map(|(row, description, _)| {
                [
                    (row.title.as_str(), FieldWeight::TITLE),
                    (
                        description.as_deref().unwrap_or_default(),
                        FieldWeight::SUBTITLE,
                    ),
                ]
            }),
        );
        let mut rows: Vec<SearchResult> = Vec::new();
        let mut index: HashMap<String, RowResolution> = HashMap::new();
        for ((row, _, resolution), rank) in candidates.into_iter().zip(ranks) {
            if rank.is_some() {
                index.insert(row.action_id.clone(), resolution);
                rows.push(row);
            }
        }

        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_query.clear();
        self.search_query.push_str(query);
        self.query = parsed;
        self.action_rows = rows;
        self.base_index = index;
        self.provider_rows.clear();
        self.provider_index.clear();
        self.provider_validation.clear();
        self.validation_queue.clear();
        self.provider_cursors = vec![None; self.providers.len()];
        self.provider_pending = vec![None; self.providers.len()];
        self.provider_complete = vec![false; self.providers.len()];
        self.results.set_loading(false);
        self.next_provider = 0;
        self.refresh_progressive_results();
        if !query.trim().is_empty() && !crate::search::engine_ready() {
            // The rows are ranked by the index, whose code is still loading.
            self.search_announcement = Some(String::from("Searching"));
            self.results.set_empty_text("Searching…");
            let _ = self.advance_search_page();
        } else if self.providers.is_empty() {
            self.results.set_empty_text("No matches");
            self.search_announcement = Some(format!("{} results", self.results.len()));
        } else {
            self.search_announcement = Some(String::from("Searching"));
            self.results.set_empty_text("Searching…");
            let _ = self.advance_search_page();
        }
    }

    fn refresh_progressive_results(&mut self) {
        let mut rows = self.action_rows.clone();
        rows.extend(
            self.provider_rows
                .iter()
                .filter(|row| self.provider_row_is_current(&row.action_id))
                .cloned(),
        );
        let groups = rank_and_group(&self.search_query, rows);
        let mut index = self.base_index.clone();
        for row in groups.iter().flat_map(|group| &group.results) {
            if let Some(resolution) = self.provider_index.get(&row.action_id) {
                index.insert(row.action_id.clone(), resolution.clone());
            }
        }
        self.index = index;
        self.results.set_groups(groups);
    }

    /// Position the palette at `origin` with `width`. Called each frame
    /// before `draw`.
    #[allow(
        clippy::expect_used,
        reason = "private palette ownership mismatch is an invariant violation"
    )]
    /// # Panics
    /// Internal host-text palettes must use `layout_with_text` with their owning renderer.
    pub fn layout(&mut self, theme: &Theme, origin: Point, width: f64) {
        let mut text = self
            .measure
            .take()
            .expect("host-text palette requires layout_with_text");
        self.layout_field(theme, origin, width, &mut text);
        self.layout_controls(theme, &mut text);
        self.measure = Some(text);
    }

    pub(crate) fn layout_with_text(
        &mut self,
        theme: &Theme,
        origin: Point,
        width: f64,
        text: &mut TextRenderer,
    ) {
        assert!(self.measure.is_none(), "owned-text palette requires layout");
        self.layout_field(theme, origin, width, text);
        self.layout_controls(theme, text);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "width and the field height are logical-pixel layout dimensions, far inside f32's range"
    )]
    fn layout_field(&mut self, theme: &Theme, origin: Point, width: f64, tr: &mut TextRenderer) {
        // Field: build a fresh 2-node tree so `handle_event` can resolve the
        // inner text input's node, then place it at the top of the card.
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.field.build_layout(&mut taffy, theme, &mut node_map);

        let Ok(()) = crate::widget::layout::solve_layout(
            &mut taffy,
            node,
            Size {
                width: AvailableSpace::Definite(width as f32),
                height: AvailableSpace::Definite(FIELD_SIZE.height(theme) as f32),
            },
            &mut |known, available, id| {
                node_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, tr)
                })
            },
        ) else {
            // A freshly built, cycle-free 2-node tree cannot fail this in
            // practice; if it ever does, keep the previous frame's layout
            // rather than panicking the app.
            return;
        };
        self.field.apply_layout(&taffy, node, origin);
        self.field_taffy = taffy;
        self.field_node = Some(node);

        // Facet chips, then the results overlay, directly under the field.
        let mut top = origin.y + FIELD_SIZE.height(theme) + GAP;
        self.layout_facet_chips(theme, origin.x, top, width, tr);
        if !self.facet_chips.is_empty() {
            top += FACET_H + GAP;
        }
        let h = self.results.desired_height();
        self.results
            .set_rect(Rect::new(origin.x, top, origin.x + width, top + h));
        self.rect = Rect::new(origin.x, origin.y, origin.x + width, top + h);
    }

    /// One row of facet value chips, in facet order, as many as fit.
    fn layout_facet_chips(
        &mut self,
        theme: &Theme,
        x0: f64,
        top: f64,
        width: f64,
        text: &mut TextRenderer,
    ) {
        self.facet_chips.clear();
        self.facet_labels.clear();
        if self.arg.is_some() || self.confirm.is_some() {
            return;
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        let label_style = theme.text(crate::tokens::TextRole::Caption);
        let mut x = x0;
        for facet in &self.facets {
            // The group's name, then its values.
            let (label_w, label_h) = text.measure_style(&facet.label, None, &label_style);
            let label_w = f64::from(label_w);
            if x + label_w > x0 + width {
                return;
            }
            self.facet_labels.push((
                Point::new(
                    x,
                    crate::text_metrics::centered_text_top(top, FACET_H, f64::from(label_h)),
                ),
                facet.label.clone(),
            ));
            x += label_w + gap;
            for value in &facet.values {
                let label = value.label.clone();
                let w = crate::widget::ChipFace::intrinsic_width(
                    &label,
                    crate::widget::ChipLead::None,
                    false,
                    text,
                    theme,
                );
                if x + w > x0 + width {
                    return;
                }
                self.facet_chips.push((
                    Rect::new(x, top, x + w, top + FACET_H),
                    FacetFilter::new(facet.key.clone(), value.value.clone()),
                    label,
                ));
                x += w + gap;
            }
            x += gap;
        }
    }

    fn draw_facet_chips(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let label_style = theme.text(crate::tokens::TextRole::Caption);
        for (at, label) in &self.facet_labels {
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix math and cannot overflow"
            )]
            let placed = transform * Affine::translate((at.x, at.y));
            text.draw_style(scene, label, theme.text_dim, placed, None, &label_style);
        }
        let applied = self.query.filters();
        for (rect, filter, label) in &self.facet_chips {
            let on = applied.contains(filter);
            // Selected is the lift: one step of ink, never a hue.
            crate::widget::ChipFace::new(*rect, label)
                .selected(on)
                .paint(scene, transform, text, theme);
        }
    }

    fn facet_chip_at(&self, point: Point) -> Option<FacetFilter> {
        self.facet_chips
            .iter()
            .find(|(rect, ..)| rect.contains(point))
            .map(|(_, filter, _)| filter.clone())
    }

    /// Follow a scope the user removed from the field.
    fn follow_field_scope(&mut self) {
        if self.field.take_scope_cleared() {
            self.scope = None;
            let query = self.field.value().to_owned();
            self.rebuild(&query);
        }
    }

    fn layout_controls(&mut self, theme: &Theme, text: &mut TextRenderer) {
        if self.confirm.is_none() && !self.history_review_required {
            self.controls_node = None;
            return;
        }
        let mut nodes = HashMap::new();
        let node = self
            .controls
            .build_layout(&mut self.field_taffy, theme, &mut nodes);
        let Ok(()) = crate::widget::layout::solve_layout(
            &mut self.field_taffy,
            node,
            Size {
                width: AvailableSpace::Definite(self.rect.width() as f32),
                height: AvailableSpace::MaxContent,
            },
            &mut |known, available, id| {
                nodes.get(&id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, text)
                })
            },
        ) else {
            self.controls_node = None;
            return;
        };
        self.controls.apply_layout(
            &self.field_taffy,
            node,
            Point::new(self.rect.x0, self.rect.y1),
        );
        self.rect.y1 = self.controls.get_rect().y1;
        self.controls_node = Some(node);
    }

    /// The palette's outer rect after the last `layout`.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Draw the field + results overlay. `layout` must have run this frame.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // It arrives from its centre (`motion::Appear`).
        let presented = self.is_presented();
        self.appear.follow(presented, theme.motion());
        let pose = self.appear.pose(self.rect, self.rect.center());
        let bounds = crate::widget::floating_surface::painted_bounds(self.rect, theme);
        let Some(transform) = pose.begin(scene, transform, bounds) else {
            return;
        };
        self.field
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_facet_chips(scene, transform, text_renderer, theme);
        self.results
            .draw(scene, transform, text_renderer, theme, presence);
        if self.controls_node.is_some() {
            self.controls
                .draw(scene, transform, text_renderer, theme, presence);
        }
        pose.end(scene);
    }

    pub(crate) fn presentation_widgets(&self) -> (&dyn Widget, &dyn Widget) {
        (&self.field, &self.results)
    }

    pub(crate) fn perform_host_semantic(
        &mut self,
        results_owner: bool,
        target: Option<&str>,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        // The caller resolves this owner against the current inspection before
        // entering the widget. No guessed path or substring is an authority.
        let handled = if results_owner {
            target.is_some_and(|target| {
                self.results
                    .perform_semantic_action(target, action, doc, root)
            })
        } else if matches!(action, crate::a11y::Action::Focus) {
            true
        } else {
            self.field.perform_action(action, &[], doc, root)
        };
        if let Some(query) = self.field.take_dirty() {
            if self.arg.is_none() && self.confirm.is_none() {
                self.rebuild(&query);
            }
        }
        self.poll_view_activation();
        handled
    }

    pub(crate) fn handle_host_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        if self.handle_key(event, &modifiers, doc, root) {
            return true;
        }
        let normalized;
        let event = if let WindowEvent::CursorMoved {
            device_id,
            position,
        } = event
        {
            normalized = WindowEvent::CursorMoved {
                device_id: *device_id,
                position: winit::dpi::PhysicalPosition::new(position.x / scale, position.y / scale),
            };
            &normalized
        } else {
            event
        };
        match event {
            WindowEvent::Ime(_) => {
                self.route_to_field(event, modifiers, doc, root);
                if let Some(query) = self.field.take_dirty() {
                    if self.arg.is_none() && self.confirm.is_none() {
                        self.rebuild(&query);
                    }
                }
                true
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x, position.y);
                self.route_to_field(event, modifiers, doc, root);
                self.route_to_results(event, modifiers, doc, root);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } if self.facet_chip_at(self.last_cursor).is_some() => {
                if let Some(filter) = self.facet_chip_at(self.last_cursor) {
                    self.toggle_facet(&filter);
                }
                true
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => {
                self.route_to_field(event, modifiers, doc, root);
                self.follow_field_scope();
                self.route_to_results(event, modifiers, doc, root);
                self.poll_view_activation();
                true
            }
            WindowEvent::Touch(_) => true,
            _ => false,
        }
    }

    /// Feed a keyboard event to the open palette; returns whether consumed.
    /// While open, the palette is modal: it swallows every key so nothing
    /// leaks to the app behind it.
    #[allow(
        clippy::too_many_lines,
        reason = "one modal keyboard state machine keeps confirmation, typed arguments, navigation and dispatch ordering explicit"
    )]
    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        if self.history_suspended {
            return false;
        }
        if !self.open {
            return false;
        }
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed {
            // Swallow releases so nothing leaks to the app behind the modal.
            return true;
        }
        if self.confirm.is_some() || self.history_review_required {
            let focused = self
                .controls
                .children()
                .iter()
                .position(|control| control.is_focused())
                .map_or(0, |index| index + 1);
            if matches!(&key.logical_key, Key::Named(NamedKey::Tab)) {
                self.focus_part((focused + if modifiers.state().shift_key() { 2 } else { 1 }) % 3);
                return true;
            }
            if focused > 0
                && matches!(
                    &key.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                )
            {
                self.controls.children_mut()[focused - 1].perform_action(
                    &crate::a11y::Action::Activate,
                    &[],
                    doc,
                    path,
                );
                self.drain_command(doc, path);
                return true;
            }
        }
        if self.history_review_required {
            if matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
                self.close();
            } else if matches!(&key.logical_key, Key::Named(NamedKey::Enter)) {
                self.history_review_required = false;
                self.open = false;
            }
            return true;
        }
        // Escape: cancel the argument step, or close the palette.
        if matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
            if self.arg.is_some() || self.confirm.is_some() {
                self.exit_argument_mode();
            } else {
                self.close();
            }
            return true;
        }

        if self.confirm.is_some() {
            if matches!(&key.logical_key, Key::Named(NamedKey::Enter)) {
                let value = self.field.value().trim().to_string();
                let accepted = self
                    .confirm
                    .as_ref()
                    .is_some_and(|state| value == state.expected);
                if accepted {
                    self.pending_dispatch = self.confirm.take().map(|state| {
                        let mut pending = state.pending;
                        pending.confirmation_evidence = match pending.confirmation {
                            ConfirmationPolicy::None => serde_json::json!({}),
                            ConfirmationPolicy::Confirm => serde_json::json!({ "confirm": true }),
                            ConfirmationPolicy::Phrase { .. } => {
                                serde_json::json!({ "confirmation_phrase": value })
                            }
                        };
                        pending
                    });
                    self.open = false;
                }
            } else {
                self.route_to_field(event, *modifiers, doc, path);
            }
            return true;
        }

        // Argument step: the field collects the free text; Enter builds +
        // queues the action; every other key edits the argument buffer.
        if self.arg.is_some() {
            if matches!(&key.logical_key, Key::Named(NamedKey::Enter)) {
                let value = self.field.value().to_string();
                let Some(state) = self.arg.as_ref() else {
                    return true;
                };
                let Some(fresh) = self
                    .action_kind_for_address(&state.address, state.bound.as_ref())
                    .filter(|kind| !matches!(kind, ActionKind::NoArgument { .. }))
                else {
                    if self.history_has_custody() {
                        self.search_announcement = Some(
                            "The retained action was removed, changed, or is currently unavailable"
                                .into(),
                        );
                    } else {
                        self.exit_argument_mode();
                    }
                    return true;
                };
                if let Some(state) = self.arg.take() {
                    let address = state.address;
                    let bound = state.bound;
                    let history_target = state.history_target;
                    let history_draft_identity = state.history_draft_identity;
                    let (encoded, context, kind, confirmation, input) = match fresh {
                        ActionKind::Prompted {
                            build,
                            context,
                            kind,
                            confirmation,
                            ..
                        } => {
                            let encoded = build(&value);
                            let input = if encoded.type_id
                                == tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                            {
                                PendingInput::Invalid
                            } else {
                                PendingInput::Text(value)
                            };
                            (encoded, context, kind, confirmation, input)
                        }
                        ActionKind::Typed {
                            spec,
                            context,
                            kind,
                            confirmation,
                            ..
                        } => {
                            let encoded = serde_json::from_str::<
                                serde_json::Map<String, serde_json::Value>,
                            >(&value)
                            .map_err(|error| error.to_string())
                            .and_then(|arguments| {
                                spec.build_json(&arguments)
                                    .map_err(|error| error.to_string())
                            })
                            .unwrap_or_else(|error| EncodedAction {
                                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                                payload: error.into_bytes(),
                            });
                            let input = if encoded.type_id
                                == tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                            {
                                PendingInput::Invalid
                            } else {
                                PendingInput::Json(value)
                            };
                            (encoded, context, kind, confirmation, input)
                        }
                        ActionKind::NoArgument { .. } => {
                            self.exit_argument_mode();
                            return true;
                        }
                    };
                    self.queue_or_confirm(
                        PendingOperation {
                            ordinary: None,
                            bound,
                            address,
                            intent: PreparedActionIntent::Encoded(encoded),
                            context,
                            kind,
                            input,
                            confirmation: confirmation.clone(),
                            confirmation_evidence: serde_json::json!({}),
                            history_target,
                            history_draft_identity,
                            history_identity: None,
                            history_witness: None,
                        },
                        confirmation,
                    );
                }
            } else {
                self.route_to_field(event, *modifiers, doc, path);
            }
            return true;
        }

        // Normal step: navigation keys + Enter drive the results view; every
        // other key edits the query and re-runs the search.
        let nav = matches!(
            &key.logical_key,
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Enter)
        );
        if nav {
            if matches!(
                &key.logical_key,
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown)
            ) {
                self.missing_selection = None;
            }
            self.route_to_results(event, *modifiers, doc, path);
            self.poll_view_activation();
        } else {
            self.route_to_field(event, *modifiers, doc, path);
            if let Some(q) = self.field.take_dirty() {
                self.rebuild(&q);
            }
            self.follow_field_scope();
        }
        true
    }

    /// Drain the results view's latched activation into a pending outcome.
    fn poll_view_activation(&mut self) {
        if let Some(id) = self.results.take_activated() {
            self.missing_selection = None;
            self.resolve_activation(&id);
        }
    }

    fn exit_argument_mode(&mut self) {
        self.history_resolution_error = None;
        self.arg = None;
        self.confirm = None;
        // Restore the query field and result set.
        self.field = self.query_field();
        self.focus_part(0);
        self.rebuild("");
    }

    fn queue_or_confirm(
        &mut self,
        mut pending: PendingOperation,
        confirmation: ConfirmationPolicy,
    ) {
        if !pending.has_valid_input() {
            let message = format!(
                "Invalid arguments: {}",
                pending
                    .encoded()
                    .map(|encoded| String::from_utf8_lossy(&encoded.payload).into_owned())
                    .unwrap_or_else(|| "invalid deferred intent".into())
            );
            self.arg = Some(ArgState {
                bound: pending.bound,
                address: pending.address,
                history_target: pending.history_target,
                history_draft_identity: pending.history_draft_identity,
            });
            self.results.set_groups(Vec::new());
            self.results.set_empty_text(&message);
            self.search_announcement = Some(message);
            return;
        }
        // Decide against the collected input, not only the declaration. An
        // ordinary/History continuation already carries its captured policy.
        let confirmation = if pending.ordinary.is_some() || pending.history_target.is_some() {
            confirmation
        } else {
            self.authority_for(&pending.address, pending.bound.as_ref())
                .and_then(|resources| {
                    tilo_actions::door::resolve(&resources, &pending.address)
                        .map(|spec| effective_confirmation(spec, &pending.input))
                })
                .unwrap_or(confirmation)
        };
        pending.confirmation = confirmation.clone();
        self.request_generation = uuid::Uuid::new_v4();
        let expected = match confirmation {
            ConfirmationPolicy::None => {
                self.pending_dispatch = Some(pending);
                self.open = false;
                return;
            }
            ConfirmationPolicy::Confirm => "CONFIRM".to_string(),
            ConfirmationPolicy::Phrase { phrase } => phrase,
        };
        self.field = self.field_source.build().control_size(FIELD_SIZE);
        self.field
            .set_placeholder(&confirmation_instruction(&expected));
        self.focus_part(0);
        self.results.set_groups(Vec::new());
        self.results
            .set_empty_text("Confirm this action in the field above");
        self.confirm = Some(ConfirmState { pending, expected });
    }

    fn route_to_field(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) {
        let Some(node) = self.field_node else { return };
        let mut handled = false;
        let mut ft = None;
        self.field.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc.reborrow(),
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &self.field_taffy,
            },
        );
    }

    fn route_to_results(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            // A style-only leaf with no children cannot fail this in
            // practice; if it ever does, there is no event target to route
            // to, so drop the event.
            return;
        };
        let mut handled = false;
        let mut ft = None;
        self.results.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc.reborrow(),
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
    }
}

/// Every provider's facets, merged by key in first-declared order.
fn merged_facets(providers: &[Box<dyn SearchProvider>]) -> Vec<Facet> {
    let mut merged: Vec<Facet> = Vec::new();
    for facet in providers.iter().flat_map(|provider| provider.facets()) {
        if let Some(existing) = merged.iter_mut().find(|known| known.key == facet.key) {
            for value in facet.values {
                if !existing
                    .values
                    .iter()
                    .any(|known| known.value == value.value)
                {
                    existing.values.push(value);
                }
            }
        } else {
            merged.push(facet);
        }
    }
    merged
}

/// What the field's scope token reads.
fn scope_token_label(scope: &ScopeDecl) -> String {
    format!("Only {}", scope.label)
}

/// `text` without the typed token(s) for `filter`.
fn without_token(text: &str, filter: &FacetFilter) -> String {
    text.split_whitespace()
        .filter(|token| {
            token.split_once(':').is_none_or(|(key, value)| {
                !(key.eq_ignore_ascii_case(&filter.key)
                    && value.trim_matches('"').eq_ignore_ascii_case(&filter.value))
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rank `rows` by fit against `query` and bucket by kind. Uses the shared
/// [`result_relevance`] ranker so a hit from one source is comparable to a
/// hit from another. Caps each group so no single source floods the overlay.
fn rank_and_group(query: &str, rows: Vec<SearchResult>) -> Vec<SearchGroup> {
    /// A provider bias notch is worth this many query-match points.
    const BIAS_WEIGHT: i32 = 4;
    let key = |r: &SearchResult| -> i32 {
        r.score
            .saturating_mul(BIAS_WEIGHT)
            .saturating_add(result_relevance(query, r))
    };
    let mut ranked = rows;
    ranked.sort_by_cached_key(|b| std::cmp::Reverse(key(b)));
    let mut groups: Vec<SearchGroup> = Vec::new();
    for r in ranked {
        if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
            if g.results.len() < PER_GROUP_CAP {
                g.results.push(r);
            }
        } else {
            groups.push(SearchGroup {
                kind: r.kind.clone(),
                results: vec![r],
            });
        }
    }
    groups
}

impl Widget for ActionPalette {
    fn name(&self) -> &str {
        "ActionPalette"
    }
    fn test_id(&self) -> Option<&str> {
        Some("action-palette")
    }
    fn resets_callback_binding(&self) -> bool {
        true
    }
    fn is_focusable(&self) -> bool {
        self.is_presented()
    }
    fn is_focused(&self) -> bool {
        self.is_presented()
            && (self
                .field
                .accessibility()
                .is_some_and(|field| field.state.focused)
                || self
                    .controls
                    .children()
                    .iter()
                    .any(|control| control.is_focused()))
    }
    fn set_focus(&mut self, focused: bool) {
        if focused {
            if !self.is_focused() {
                self.focus_part(0);
            }
        } else {
            self.field.set_focused(false);
            for control in self.controls.children_mut() {
                control.set_focus(false);
            }
        }
    }
    fn traps_focus(&self) -> bool {
        self.is_presented()
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.is_presented().then_some(self.rect)
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt;
        let node = tree.leaf_node(Style {
            position: taffy::Position::Absolute,
            ..Style::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        ActionPalette::draw(self, scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        *ctx.handled |= self.route_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
        );
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        if !self.is_presented() {
            return None;
        }
        let mut children = Vec::new();
        if let Some(mut field) = self.field.accessibility() {
            if let Some(confirm) = &self.confirm {
                field = field.with_description(confirmation_instruction(&confirm.expected));
            }
            let target = self.target("field");
            children.push(
                field
                    .with_name(if self.confirm.is_some() {
                        "Confirmation"
                    } else {
                        "Search"
                    })
                    .with_hashed_key(&target)
                    .with_action_target(target)
                    .with_bounds(self.field.get_rect()),
            );
        }
        if self.confirm.is_some() || self.history_review_required {
            for (button, part) in self
                .controls
                .children()
                .into_iter()
                .zip(["submit", "cancel"])
            {
                if let Some(node) = button.accessibility() {
                    let target = self.target(part);
                    children.push(
                        node.with_hashed_key(&target)
                            .with_action_target(target)
                            .with_bounds(button.get_rect()),
                    );
                }
            }
        }
        if !self.in_argument_mode()
            && !self.history_review_required
            && let Some(results) = self.results.accessibility()
        {
            children.push(results.with_hashed_key(self.target("results")));
        }
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Dialog)
                .with_name(if self.in_argument_mode() {
                    "Action confirmation"
                } else {
                    "Search"
                })
                .with_state(crate::a11y::AccessState::new().modal(true))
                .with_children(children),
        )
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if !self.is_presented() {
            return false;
        }
        if target == self.target("field") {
            if matches!(action, crate::a11y::Action::Focus) {
                self.focus_part(0);
            }
            return self.field.perform_action(action, &[], doc, path);
        }
        if !self.in_argument_mode() && !self.history_review_required {
            let handled = self
                .results
                .perform_semantic_action(target, action, doc, path);
            self.poll_view_activation();
            return handled;
        }
        if self.confirm.is_none() && !self.history_review_required {
            return false;
        }
        for (index, part) in ["submit", "cancel"].into_iter().enumerate() {
            if target == self.target(part) {
                if matches!(action, crate::a11y::Action::Focus) {
                    self.focus_part(index + 1);
                }
                let handled =
                    self.controls.children_mut()[index].perform_action(action, &[], doc, path);
                let (doc, path) = crate::doc::document(doc, path);
                self.drain_command(doc, path);
                return handled;
            }
        }
        false
    }
}

#[cfg(test)]
pub(crate) fn observed_field_language(palette: &ActionPalette) -> &str {
    crate::widget::search_field::observed_language(&palette.field)
}

#[cfg(test)]
pub(crate) fn observed_history_pending(palette: &ActionPalette) -> Option<PendingOperation> {
    palette
        .pending_dispatch
        .clone()
        .or_else(|| palette.confirm.as_ref().map(|state| state.pending.clone()))
}

#[cfg(test)]
pub(crate) fn observed_history_editor(
    palette: &ActionPalette,
) -> (String, Option<(usize, usize)>, &'static str) {
    let selection = palette
        .field
        .accessibility()
        .and_then(|node| node.text_selection)
        .map(|selection| (selection.anchor, selection.focus));
    let stage = if palette.arg.is_some() {
        "argument"
    } else if palette.confirm.is_some() {
        "confirmation"
    } else if palette.pending_dispatch.is_some() {
        "ready"
    } else {
        "search"
    };
    (palette.field.value().to_owned(), selection, stage)
}

#[cfg(test)]
mod tests {
    include!("action_palette/deferred_tests.rs");
    include!("action_palette/platform_search_tests.rs");
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};
    use tilo_actions::{
        Action, ActionError, ActionRegistry, ActionSpec, Doc, ExecutionLocus, LocalTransport,
    };
    use winit::event::KeyEvent;

    /// A minimal replayable action: set a string under a root key.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Note {
        key: String,
        value: String,
    }
    const NOTE_TYPE: &str = "test.note";
    impl Action for Note {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(
                &ROOT,
                self.key.as_str(),
                ScalarValue::Str(self.value.as_str().into()),
            )?;
            Ok(())
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Note>(NOTE_TYPE);
        r
    }

    /// A resource exporting an immediate and a prompted action.
    fn thing_resource() -> Resource {
        Resource::new("test.thing:1", "test.thing", "Thing")
            .with_action(
                ActionSpec::immediate(
                    "mark",
                    "Mark done",
                    NOTE_TYPE,
                    &Note {
                        key: "marked".into(),
                        value: "yes".into(),
                    },
                )
                .unwrap(),
            )
            .with_action(ActionSpec::prompted_param(
                "rename",
                "Rename",
                tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "New name",
                ),
                "e.g. Widget",
                ExecutionLocus::ClientLocal,
                |s: &str| {
                    EncodedAction::encode(
                        NOTE_TYPE,
                        &Note {
                            key: "name".into(),
                            value: s.into(),
                        },
                    )
                    .unwrap()
                },
            ))
    }

    /// A provider standing in for a searchable corpus (e.g. contacts).
    struct People;
    impl SearchProvider for People {
        fn name(&self) -> &'static str {
            "People"
        }
        fn search(&self, query: &str) -> Vec<SearchResult> {
            ["Ada Lovelace", "Alan Turing"]
                .into_iter()
                .filter(|n| {
                    crate::search::fold_search_text(n)
                        .contains(&crate::search::fold_search_text(query))
                })
                .map(|n| SearchResult::new("People", n, format!("person:{n}")))
                .collect()
        }
    }

    /// A provider using the canonical migration API. The hit's title is
    /// intentionally unrelated to the action label, proving address
    /// resolution does not depend on the action row also matching the query.
    struct CanonicalPeople;
    impl SearchProvider for CanonicalPeople {
        fn name(&self) -> &'static str {
            "People"
        }

        fn search(&self, query: &str) -> Vec<SearchResult> {
            if !crate::search::fold_search_text("Ada Lovelace")
                .contains(&crate::search::fold_search_text(query))
            {
                return Vec::new();
            }
            vec![SearchResult::for_action(
                "People",
                "Ada Lovelace",
                tilo_actions::ActionAddress::new("test.thing:1", "mark"),
            )]
        }
    }

    struct LazyCorpus {
        materialized: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl SearchProvider for LazyCorpus {
        fn name(&self) -> &'static str {
            "Large corpus"
        }

        fn search(&self, _query: &str) -> Vec<SearchResult> {
            panic!("the palette must use the budgeted query hook")
        }

        fn search_limited(&self, _query: &str, limit: usize) -> Vec<SearchResult> {
            self.materialized
                .fetch_add(limit, std::sync::atomic::Ordering::Relaxed);
            (0..limit)
                .map(|id| {
                    SearchResult::for_action(
                        "Large corpus",
                        format!("Item {id}"),
                        tilo_actions::ActionAddress::new(format!("lazy.item:{id}"), "open"),
                    )
                })
                .collect()
        }

        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<Resource> {
            address.resource.strip_prefix("lazy.item:")?;
            Some(
                Resource::new(&address.resource, "lazy.item", &address.resource).with_action(
                    ActionSpec::immediate(
                        "open",
                        "Open",
                        NOTE_TYPE,
                        &Note {
                            key: "opened".into(),
                            value: address.resource.clone(),
                        },
                    )
                    .ok()?,
                ),
            )
        }
    }

    #[test]
    fn palette_search_is_budgeted_grouped_and_reaches_exact_matches() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `large_lazy_provider_is_budgeted_but_cannot_supply_its_own_authority`.
        {
            let materialized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut palette =
                ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                    SearchField::with_language("en_US")
                });
            palette.load_open_resources(&[]);
            palette.set_providers(vec![Box::new(LazyCorpus {
                materialized: Arc::clone(&materialized),
            })]);
            materialized.store(0, std::sync::atomic::Ordering::Relaxed);
            palette.open();
            assert_eq!(
                materialized.load(std::sync::atomic::Ordering::Relaxed),
                PROVIDER_HIT_BUDGET
            );
            assert_eq!(palette.len(), PER_GROUP_CAP);
            palette.activate_selected();
            assert!(
                palette.take_operation().is_none(),
                "provider discovery must not become dispatch authority"
            );
        }

        // Case `actions_and_content_both_surface_grouped`:
        // Both an ACTION (from the resource tree) and a searchable CONTENT hit
        // (from a folded-in provider) surface, grouped, for one query.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_providers(vec![Box::new(People)]);
            p.open();

            let groups: Vec<String> = p.results.groups().iter().map(|g| g.kind.clone()).collect();
            assert!(
                groups.contains(&ACTIONS_KIND.to_string()),
                "Actions group present: {groups:?}"
            );
            assert!(
                groups.contains(&"People".to_string()),
                "content group present: {groups:?}"
            );

            let labels = p.result_labels();
            assert!(labels.iter().any(|l| l.contains("Mark done")), "{labels:?}");
            assert!(labels.iter().any(|l| l == "Ada Lovelace"), "{labels:?}");
        }
    }

    #[test]
    fn progressive_palette_reaches_exact_match_beyond_one_hundred_thousand_candidates() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let catalog = crate::search::SearchCatalog::new("Adversarial corpus");
        for id in 0..100_000usize {
            let resource_id = format!("item:{id:06}");
            catalog
                .upsert(
                    &resource_id,
                    "abc c d",
                    SearchResult::for_action(
                        "Items",
                        format!("Broad candidate {id}"),
                        tilo_actions::ActionAddress::new(&resource_id, "open"),
                    ),
                )
                .expect("valid broad candidate");
        }
        let target_id = "item:999999";
        catalog
            .upsert(
                target_id,
                "abc d",
                SearchResult::for_action(
                    "Items",
                    "The only exact abc d result",
                    tilo_actions::ActionAddress::new(target_id, "open"),
                ),
            )
            .expect("valid exact target");

        let mut palette =
            ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
        palette.set_providers(vec![Box::new(catalog)]);
        // The whitespace-bearing query intentionally cannot take the token
        // fast path. Every false candidate shares its n-grams, so this proves
        // progressive exact verification continues across work pages.
        palette.set_query("abc d");
        let generation = palette.search_generation;
        let mut ticks = 0usize;
        while !palette
            .result_labels()
            .iter()
            .any(|label| label == "The only exact abc d result")
        {
            let progress = palette.advance_search_page();
            assert!(progress.inspected <= PROVIDER_HIT_BUDGET);
            assert_eq!(progress.generation, generation);
            ticks = ticks.saturating_add(1);
            assert!(progress.pending || ticks <= 3_126, "target was skipped");
            assert!(ticks <= 3_126, "search did not finish in bounded pages");
        }
        assert!(
            palette
                .referenced_action_resource_ids()
                .iter()
                .any(|resource_id| resource_id == target_id),
            "the exact result must retain its canonical resource action"
        );
        assert!(
            palette
                .take_search_announcement()
                .is_some_and(|announcement| announcement.contains("result"))
        );

        palette.set_query("a different query");
        assert_ne!(palette.search_generation, generation);
        assert!(
            !palette
                .result_labels()
                .iter()
                .any(|label| label == "The only exact abc d result")
        );
        let restarted = palette.advance_search_page();
        assert_eq!(restarted.generation, palette.search_generation);
        assert!(restarted.inspected <= PROVIDER_HIT_BUDGET);
    }

    #[test]
    fn palette_activation_dispatches_navigates_prompts_or_queues() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `activating_an_action_dispatches_and_mutates`:
        // Activating an ACTION row dispatches through `tilo_actions` and mutates the
        // CRDT — the real replay path.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.open();

            let idx = p
                .result_labels()
                .iter()
                .position(|l| l.contains("Mark done"))
                .expect("mark row");
            p.set_cursor(idx);
            p.activate_selected();

            let mut doc = automerge::AutoCommit::new();
            assert!(doc.get(&ROOT, "marked").unwrap().is_none());
            let mut tx = LocalTransport::new();
            let d = p
                .take_dispatch(
                    &mut crate::widget::Edits::lend(&mut doc),
                    &registry(),
                    &mut tx,
                )
                .expect("an action was pending")
                .expect("dispatch ok");
            assert!(matches!(d, Dispatched::AppliedLocal(_)));
            let context = d.envelope().context.as_ref().expect("palette provenance");
            let Some(tilo_actions::Did::Verb(verb)) = &context.what else {
                panic!("the published action is recorded: {context:?}");
            };
            assert_eq!(
                (verb.action.kind.as_str(), verb.action.action.as_str()),
                ("test.thing", "mark")
            );
            assert_eq!(
                context
                    .resource
                    .as_ref()
                    .map(|resource| resource.label.as_str()),
                Some("Thing")
            );
            assert_eq!(
                doc.get(&ROOT, "marked")
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_string)),
                Some("yes".to_string()),
            );
            assert!(!p.is_open(), "an immediate action closes the palette");
            assert!(
                p.take_navigation().is_none(),
                "an action is not a navigation"
            );
        }

        // Case `ambient_task_context_reaches_palette_actions_without_overwriting_resource`.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_ambient_context(Some(
                ActionContext::default()
                    .with_app("org.tilo.Agent", "Agent")
                    .with_purpose("Prepare launch")
                    .with_group("issue:42", "Launch", "issue"),
            ));
            p.open();
            p.activate_selected();

            let mut doc = automerge::AutoCommit::new();
            let mut tx = LocalTransport::new();
            let dispatched = p
                .take_dispatch(
                    &mut crate::widget::Edits::lend(&mut doc),
                    &registry(),
                    &mut tx,
                )
                .expect("action pending")
                .expect("dispatch succeeds");
            let context = dispatched.envelope().context.as_ref().expect("context");
            assert_eq!(context.purpose.as_deref(), Some("Prepare launch"));
            assert_eq!(
                context.group.as_ref().map(|group| group.id.as_str()),
                Some("issue:42")
            );
            assert_eq!(
                context
                    .resource
                    .as_ref()
                    .map(|resource| resource.id.as_str()),
                Some("test.thing:1"),
                "the concrete palette resource wins over ambient provenance"
            );
        }

        // Case `activating_content_navigates_not_dispatches`:
        // Activating a CONTENT row queues a navigation (its opaque id), NOT a
        // dispatch.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.set_providers(vec![Box::new(People)]);
            p.open();
            p.set_query("ada");

            let idx = p
                .result_labels()
                .iter()
                .position(|l| l == "Ada Lovelace")
                .expect("ada row");
            p.set_cursor(idx);
            p.activate_selected();

            let mut doc = automerge::AutoCommit::new();
            let mut tx = LocalTransport::new();
            assert!(
                p.take_dispatch(
                    &mut crate::widget::Edits::lend(&mut doc),
                    &registry(),
                    &mut tx
                )
                .is_none(),
                "a content row is not a dispatch"
            );
            assert_eq!(
                p.take_navigation().as_deref(),
                Some("person:Ada Lovelace"),
                "the opaque provider id is handed back for navigation"
            );
            assert!(!p.is_open(), "navigation closes the palette");
        }

        // Case `activating_canonical_search_hit_queues_pending_operation`:
        // Canonical provider hits are broker operations, not opaque navigation
        // callbacks. This is the per-app migration contract.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_providers(vec![Box::new(CanonicalPeople)]);
            p.open();
            p.set_query("ada");

            assert_eq!(p.result_labels(), vec!["Ada Lovelace"]);
            p.activate_selected();

            let pending = p.take_operation().expect("broker operation pending");
            assert_eq!(
                pending.address,
                tilo_actions::ActionAddress::new("test.thing:1", "mark")
            );
            assert_eq!(
                pending.encoded().expect("fixed encoding").type_id,
                NOTE_TYPE
            );
            assert_eq!(pending.kind, OperationKind::Document);
            assert_eq!(
                pending
                    .context
                    .resource
                    .as_ref()
                    .map(|resource| resource.id.as_str()),
                Some("test.thing:1")
            );
            assert!(
                p.take_navigation().is_none(),
                "canonical hits never enter the legacy navigation callback"
            );
            assert!(!p.is_open());
        }

        // Case `prompted_action_collects_argument_then_dispatches`:
        // A PROMPTED action collects its argument in the field, then dispatches the
        // action built from the typed text.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.open();

            let idx = p
                .result_labels()
                .iter()
                .position(|l| l.contains("Rename"))
                .expect("rename row");
            p.set_cursor(idx);
            p.activate_selected();
            assert!(
                p.in_argument_mode(),
                "a prompted action opens the argument step"
            );
            assert!(p.is_open(), "still open while collecting the argument");

            // Type the argument into the field and confirm.
            p.field.set_query("Sprocket");
            let mut doc = automerge::AutoCommit::new();
            let root = ROOT;
            let mods = Modifiers::default();
            p.handle_key(
                &enter(),
                &mods,
                &mut crate::widget::Edits::lend(&mut doc),
                &root,
            );

            let mut tx = LocalTransport::new();
            let d = p
                .take_dispatch(
                    &mut crate::widget::Edits::lend(&mut doc),
                    &registry(),
                    &mut tx,
                )
                .expect("prompted action pending")
                .expect("dispatch ok");
            assert!(matches!(d, Dispatched::AppliedLocal(_)));
            assert_eq!(
                doc.get(&ROOT, "name")
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_string)),
                Some("Sprocket".to_string()),
                "the action carries the typed argument",
            );
        }
    }

    #[test]
    fn removed_or_disabled_actions_are_refused_at_activation_and_dispatch() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `removed_canonical_action_is_refused_at_activation`.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_providers(vec![Box::new(CanonicalPeople)]);
            p.open();
            p.set_query("ada");

            // The row remains visually selected for this key press, but its
            // authority snapshot is fresh and no longer contains the address.
            p.refresh_open_resources(&[]);
            p.activate_selected();

            assert!(p.take_operation().is_none());
            assert!(p.take_navigation().is_none());
            assert!(p.is_open(), "a stale selection leaves the palette usable");
            assert!(
                !p.is_empty(),
                "discovery may remain visible so a later permission/resource refresh can enable it"
            );
        }

        // Case `disabled_canonical_action_is_refused_at_activation`.
        {
            let mut disabled = thing_resource();
            disabled.actions[0].contract.availability = tilo_actions::Availability::Unavailable {
                code: "offline".into(),
                reason: "The service went offline".into(),
            };

            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_providers(vec![Box::new(CanonicalPeople)]);
            p.open();
            p.set_query("ada");
            p.refresh_open_resources(&[disabled]);
            p.activate_selected();

            assert!(p.take_operation().is_none());
            assert!(p.take_navigation().is_none());
            assert!(!p.is_empty());
        }

        // Case `removed_action_is_refused_again_at_dispatch_boundary`.
        {
            let mut p = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
            p.load_open_resources(&[thing_resource()]);
            p.set_providers(vec![Box::new(CanonicalPeople)]);
            p.open();
            p.set_query("ada");
            p.activate_selected();

            // Covers state changing after activation but before the host drains
            // the pending operation (and consequential confirmation flows).
            p.refresh_open_resources(&[]);
            assert!(p.take_operation().is_none());
            assert!(p.take_navigation().is_none());
        }
    }

    #[test]
    fn consequential_actions_confirm_by_exact_value() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `consequential_action_waits_for_exact_confirmation`.
        {
            let resource = Resource::new("test.thing:1", "test.thing", "Thing").with_action(
                ActionSpec::immediate(
                    "mark",
                    "Mark done",
                    NOTE_TYPE,
                    &Note {
                        key: "marked".into(),
                        value: "yes".into(),
                    },
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract {
                    confirmation: ConfirmationPolicy::Phrase {
                        phrase: "MARK THING".into(),
                    },
                    ..tilo_actions::ActionContract::default()
                }),
            );
            let mut palette =
                ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                    SearchField::with_language("en_US")
                });
            palette.load_open_resources(&[resource]);
            palette.open();
            palette.activate_selected();
            assert!(palette.in_argument_mode());

            let mut doc = automerge::AutoCommit::new();
            let mut transport = LocalTransport::new();
            assert!(
                palette
                    .take_dispatch(
                        &mut crate::widget::Edits::lend(&mut doc),
                        &registry(),
                        &mut transport
                    )
                    .is_none(),
                "activation alone must not queue the operation"
            );

            palette.field.set_query("mark thing");
            palette.handle_key(
                &enter(),
                &Modifiers::default(),
                &mut crate::widget::Edits::lend(&mut doc),
                &ROOT,
            );
            assert!(
                palette
                    .take_dispatch(
                        &mut crate::widget::Edits::lend(&mut doc),
                        &registry(),
                        &mut transport
                    )
                    .is_none(),
                "confirmation is exact and case-sensitive"
            );

            palette.field.set_query("MARK THING");
            palette.handle_key(
                &enter(),
                &Modifiers::default(),
                &mut crate::widget::Edits::lend(&mut doc),
                &ROOT,
            );
            let pending = palette
                .take_operation()
                .expect("exact confirmation queues the operation");
            assert_eq!(
                pending.confirmation_evidence(),
                &serde_json::json!({ "confirmation_phrase": "MARK THING" }),
                "the broker receives the exact evidence the palette verified"
            );
            ActionPalette::dispatch_encoded(
                &mut crate::widget::Edits::lend(&mut doc),
                pending.encoded().expect("fixed encoding"),
                &registry(),
                &mut transport,
                pending.context.clone(),
            )
            .expect("confirmed dispatch succeeds");
            assert_eq!(
                doc.get(&ROOT, "marked")
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)),
                Some("yes".into())
            );
        }

        // Case `programmatic_actions_are_not_rows_but_confirm_by_value`:
        // A programmatic generic edit is never a row, yet a control that submits
        // a destructive value of it still gets the palette's confirmation step,
        // and an ordinary value queues directly.
        {
            let edit = ActionSpec::typed(
                "edit",
                "Edit thing",
                "value",
                ExecutionLocus::ClientLocal,
                vec![tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "Value",
                )],
                |args| {
                    EncodedAction::encode(
                        NOTE_TYPE,
                        &Note {
                            key: "value".into(),
                            value: args.text("value").unwrap_or("").into(),
                        },
                    )
                    .unwrap()
                },
            )
            .programmatic()
            .with_risk(|args| {
                if args.text("value") == Some("wipe") {
                    tilo_actions::RiskLevel::Destructive
                } else {
                    tilo_actions::RiskLevel::Low
                }
            });
            let resource = thing_resource().with_action(edit);
            let address = tilo_actions::ActionAddress::new("test.thing:1", "edit");
            let value = |value: &str| {
                serde_json::Map::from_iter([("value".to_owned(), serde_json::json!(value))])
            };
            let mut palette = ActionPalette::new();
            palette.load_open_resources(&[resource]);
            palette.open();
            let labels = palette.result_labels();
            assert!(labels.iter().any(|l| l.contains("Mark done")), "{labels:?}");
            assert!(
                !labels.iter().any(|l| l.contains("Edit thing")),
                "a programmatic action is never offered as a row: {labels:?}"
            );

            assert!(palette.activate_address_with_arguments(&address, &value("tidy")));
            assert!(
                palette.take_operation().is_some(),
                "an ordinary value needs no confirmation"
            );

            palette.open();
            assert!(palette.activate_address_with_arguments(&address, &value("wipe")));
            assert!(
                palette.take_operation().is_none(),
                "a destructive value waits for confirmation"
            );
            let mut doc = automerge::AutoCommit::new();
            palette.field.set_query("CONFIRM");
            palette.handle_key(
                &enter(),
                &Modifiers::default(),
                &mut crate::widget::Edits::lend(&mut doc),
                &ROOT,
            );
            let pending = palette
                .take_operation()
                .expect("the confirmed destructive value queues");
            assert_eq!(pending.address, address);
        }
    }

    fn enter_key() -> KeyEvent {
        crate::synthetic_input::key_event(Key::Named(NamedKey::Enter), None, ElementState::Pressed)
    }

    fn enter() -> WindowEvent {
        crate::synthetic_input::keyboard_input(enter_key())
    }

    #[test]
    fn history_suspends_the_same_palette_draft_and_ready_dispatch() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut palette =
            ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
        palette.load_open_resources(&[thing_resource()]);
        palette.open();
        palette.set_query("mark");
        let idx = palette
            .result_labels()
            .iter()
            .position(|label| label.contains("Mark done"))
            .unwrap();
        palette.set_cursor(idx);
        palette.activate_selected();
        assert!(!palette.is_open());

        palette.suspend_for_history();
        assert!(palette.is_history_suspended());
        assert!(
            palette.take_operation().is_none(),
            "return cannot auto-take ready work"
        );
        assert_eq!(
            palette.query(),
            "mark",
            "the original field instance keeps its draft"
        );

        palette.resume_after_history();
        assert!(!palette.is_history_suspended());
        assert!(
            palette.take_operation().is_none(),
            "Back to live keeps ready work behind explicit continuation"
        );
        let mut doc = automerge::AutoCommit::new();
        palette.handle_key(
            &enter(),
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &ROOT,
        );
        let pending = palette
            .take_operation()
            .expect("ready work remains in the same palette");
        assert_eq!(pending.address.resource, "test.thing:1");
        assert_eq!(pending.address.action, "mark");
    }

    #[test]
    fn history_requires_fresh_palette_confirmation_for_ready_work() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut resource = thing_resource();
        resource
            .actions
            .iter_mut()
            .find(|action| action.id == "mark")
            .unwrap()
            .contract
            .confirmation = ConfirmationPolicy::Phrase {
            phrase: "MARK THING".into(),
        };
        let mut palette =
            ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
                SearchField::with_language("en_US")
            });
        palette.load_open_resources(&[resource]);
        palette.open();
        let idx = palette
            .result_labels()
            .iter()
            .position(|label| label.contains("Mark done"))
            .unwrap();
        palette.set_cursor(idx);
        palette.activate_selected();
        let mut doc = automerge::AutoCommit::new();
        palette.field.set_query("MARK THING");
        palette.handle_key(
            &enter(),
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &ROOT,
        );
        assert!(palette.pending_dispatch.is_some());

        palette.suspend_for_history();
        palette.resume_after_history();
        assert!(palette.take_operation().is_none());
        assert!(palette.in_argument_mode());
        assert!(palette.query().is_empty(), "old affirmative is not reused");
        palette.field.set_query("MARK THING");
        palette.handle_key(
            &enter(),
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &ROOT,
        );
        assert_eq!(
            palette.take_operation().unwrap().confirmation_evidence(),
            &serde_json::json!({ "confirmation_phrase": "MARK THING" })
        );
    }

    #[test]
    fn visible_refresh_keeps_exact_selection_through_reorder_revocation_and_restore() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let resource = |id: &str, label: &str| {
            let mut value = thing_resource();
            value.id = tilo_actions::ResourceId::new(id);
            value.label = label.into();
            value.actions.truncate(1);
            value
        };
        let a = resource("a", "Alpha");
        let b = resource("b", "Beta");
        let c = resource("c", "Aardvark");
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&[a.clone(), b.clone()]);
        palette.open();
        palette.set_cursor(1);
        palette.refresh_visible_resources(&[c, a.clone(), b.clone()]);
        assert_eq!(palette.results.selected_action_id(), Some("b\u{1f}mark"));
        palette.refresh_visible_resources(std::slice::from_ref(&a));
        assert_eq!(palette.result_labels(), vec!["Alpha — Mark done"]);
        assert!(palette.results.selected().is_none());
        palette.activate_selected();
        assert!(palette.take_operation().is_none());
        palette.refresh_visible_resources(&[a, b]);
        palette.activate_selected();
        assert_eq!(palette.take_operation().unwrap().address.resource, "b");
    }

    #[test]
    fn invalid_arguments_stay_visible_before_confirmation_and_can_be_corrected() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut spec = ActionSpec::typed(
            "number",
            "Number",
            "",
            ExecutionLocus::ClientLocal,
            vec![tilo_actions::ActionParam::required(
                "rows",
                tilo_actions::ParamType::Number,
                "Rows",
            )],
            |_| EncodedAction {
                type_id: "fixture".into(),
                payload: vec![],
            },
        );
        spec.contract.confirmation = ConfirmationPolicy::Confirm;
        let resource = Resource::new("document", "document", "Document").with_action(spec);
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&[resource]);
        palette.open();
        palette.activate_selected();
        palette.field.set_query("{\"rows\":\"bad\"}");
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let enter =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Enter), ElementState::Pressed);
        palette.handle_key(
            &enter,
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        assert!(palette.confirm.is_none());
        assert!(palette.arg.is_some());
        assert!(palette.is_open());
        assert!(palette.take_operation().is_none());
        assert!(
            serde_json::to_string(&palette.results.accessibility())
                .unwrap()
                .contains("Invalid arguments")
        );
        palette.field.set_query("{\"rows\":2}");
        palette.handle_key(
            &enter,
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        assert!(palette.confirm.is_some());
        assert!(palette.take_operation().is_none());
        palette.field.set_query("CONFIRM");
        palette.handle_key(
            &enter,
            &Modifiers::default(),
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        );
        assert_eq!(
            palette.take_operation().unwrap().confirmation_evidence(),
            &serde_json::json!({"confirm":true})
        );
    }
}
