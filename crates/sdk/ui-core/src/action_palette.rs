//! `ActionPalette` — the one Ctrl+K surface for a `tilo_ui::App`.
//!
//! The **resource + action model** ([`tilo_actions`]) is the foundation: the
//! focused app instance publishes a tree of open [`Resource`]s, each
//! exporting [`ActionSpec`](tilo_actions::ActionSpec)s — the operations
//! Ctrl+K offers. An app can also fold in *searchable content* via
//! [`SearchProvider`](crate::search::SearchProvider)s (contacts, files, …);
//! their hits can address an exported resource action and therefore enter the
//! same broker path; legacy opaque navigation ids remain supported during
//! migration. Search providers are discovery-only: a provider-returned
//! resource is never accepted as authority. The host resolves every addressed
//! resource through the owning widget and permission-filters it before
//! activation.
//!
//! One registry, one renderer, one flow: input is the shared
//! [`SearchField`](crate::widget::SearchField); results render through
//! [`SearchResultsView`](crate::widget::SearchResultsView), grouped by kind.
//! An action row resolves to its [`EncodedAction`] and
//! [`dispatch_with_context`](tilo_actions::dispatch_with_context)es through `tilo_actions` — the same
//! path a remote peer replays — with a
//! [`Prompted`](tilo_actions::ActionInput::Prompted) action first collecting
//! its argument in the field. A canonical search hit resolves to that same
//! action machinery. Only a legacy content row hands its opaque `action_id`
//! back to the app to navigate.
//!
//! The host ([`crate::app::App`]) drains a completed activation with
//! [`take_dispatch`](ActionPalette::take_dispatch) (actions) and
//! [`take_navigation`](ActionPalette::take_navigation) (content).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::search::{SearchCursor, SearchGroup, SearchProvider, SearchResult, fuzzy_score};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{SearchField, SearchResultsView, Widget};
use tilo_actions::{
    ActionContext, ActionInput, ActionRegistry, ActionTransport, ConfirmationPolicy, Dispatched,
    EncodedAction, OperationKind, Resource, flatten,
};

/// The group header for the focused app's exported actions.
pub const ACTIONS_KIND: &str = "Actions";

/// Row-id prefix for search-provider (navigable content) rows, so a content
/// row can never collide with a flattened action key.
const NAV_PREFIX: &str = "\u{1}nav\u{1f}";

/// Card geometry, matched to the shared search surface (`search_showcase`).
const FIELD_H: f64 = 38.0;
const GAP: f64 = 10.0;
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
    Immediate {
        encoded: EncodedAction,
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
}

struct ConfirmState {
    pending: PendingOperation,
    expected: String,
}

enum PendingInput {
    Immediate,
    Text(String),
    Json(String),
    Invalid,
}

/// A palette activation after address resolution and argument collection.
pub struct PendingOperation {
    pub(crate) bound: Option<crate::bound_dispatch::BoundActionTarget>,
    /// The address whose current authority must still be present at dispatch.
    pub address: tilo_actions::ActionAddress,
    pub encoded: EncodedAction,
    pub context: ActionContext,
    pub kind: OperationKind,
    input: PendingInput,
    confirmation: ConfirmationPolicy,
    confirmation_evidence: serde_json::Value,
}

impl PendingOperation {
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

    pub fn confirmation_evidence(&self) -> &serde_json::Value {
        &self.confirmation_evidence
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
    provider_complete: Vec<bool>,
    next_provider: usize,
    search_query: String,
    search_generation: u64,
    search_announcement: Option<String>,
    open: bool,
    /// `Some` while collecting a prompted action's argument in the field.
    arg: Option<ArgState>,
    /// `Some` while collecting the explicit confirmation required by the
    /// selected action contract.
    confirm: Option<ConfirmState>,
    /// A ready-to-dispatch action, set on activation, drained by `take_dispatch`.
    pending_dispatch: Option<PendingOperation>,
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
        Self {
            field: field_source
                .build()
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
            provider_complete: Vec::new(),
            next_provider: 0,
            search_query: String::new(),
            search_generation: 0,
            search_announcement: None,
            open: false,
            arg: None,
            confirm: None,
            pending_dispatch: None,
            ambient_context: None,
            pending_nav: None,
            rect: Rect::ZERO,
            field_taffy: TaffyTree::new(),
            field_node: None,
            measure,
            field_source,
        }
    }

    /// Populate from the focused instance's open-resource forest. Each exported
    /// action becomes an **Actions** row; rebuilds the result set + index.
    pub fn load_open_resources(&mut self, resources: &[Resource]) {
        self.resources = resources.to_vec();
        // `.to_string()` is load-bearing here, not redundant: it ends the
        // immutable borrow of `self.field` before `rebuild` takes `&mut self`.
        #[allow(
            clippy::unnecessary_to_owned,
            reason = "the owned query ends the field borrow before rebuild mutably borrows the palette"
        )]
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    /// Replace only the authority snapshot, without moving the visible result
    /// rows or cursor. The host calls this immediately before handling input;
    /// a selected row can then disappear/refuse safely instead of changing the
    /// user's selection to a different action during the same key press.
    pub fn refresh_open_resources(&mut self, resources: &[Resource]) {
        self.resources = resources.to_vec();
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
        ids.sort_unstable();
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
        // `.to_string()` is load-bearing here, not redundant: it ends the
        // immutable borrow of `self.field` before `rebuild` takes `&mut self`.
        #[allow(
            clippy::unnecessary_to_owned,
            reason = "the owned query ends the field borrow before rebuild mutably borrows the palette"
        )]
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open (focus) the palette: reset the query, argument, and any pending
    /// outcome, and rebuild the (empty-query) result set.
    pub(crate) fn load_bound_resources(
        &mut self,
        snapshots: Vec<crate::bound_dispatch::BoundResourceSnapshot>,
    ) {
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

    /// Enter confirmation with arguments already captured by a bound control.
    /// This bypasses only argument collection; current authority, strict
    /// parameter binding, and confirmation still use the palette's normal
    /// production path.
    pub(crate) fn activate_bound_with_arguments(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        if !self.index.contains_key(&target.key()) {
            return false;
        }
        self.activate_with_arguments(
            target.address.address.clone(),
            Some(target.clone()),
            arguments,
        )
    }

    pub fn open(&mut self) {
        self.open = true;
        self.arg = None;
        self.confirm = None;
        self.pending_dispatch = None;
        self.pending_nav = None;
        self.field = self
            .field_source
            .build()
            .with_placeholder("Search actions and results");
        self.field.set_focused(true);
        self.rebuild("");
    }

    pub fn close(&mut self) {
        self.open = false;
        self.arg = None;
        self.confirm = None;
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
        // Prompt/confirmation owns the empty result surface. Preserve any
        // partial pass until the existing exit/open rebuild restarts search.
        if self.in_argument_mode() {
            return SearchAdvance {
                generation: self.search_generation,
                ..SearchAdvance::default()
            };
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
        let page = provider.search_page(
            &self.search_query,
            cursor.as_ref(),
            PROVIDER_HIT_BUDGET,
            PROVIDER_HIT_BUDGET,
        );
        let results = self.checked_provider_hits(provider.as_ref(), binding.as_ref(), page.results);
        let complete = page.next.is_none();
        if let Some(slot) = self.provider_cursors.get_mut(provider_index) {
            *slot = page.next;
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
                    collab_types::caps::Verb::Read,
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
                        .resources
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
            if let Some(subtitle) = hit.subtitle {
                row = row.with_subtitle(subtitle);
            }
            if let Some(icon) = hit.icon {
                row = row.with_icon(icon);
            }
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

    pub(crate) fn activate_address_with_arguments(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        if !self.index.contains_key(&address.encode()) {
            return false;
        }
        self.activate_with_arguments(address.clone(), None, arguments)
    }

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
        let mut context = ActionContext::for_resource(
            &flattened.action.label,
            flattened.resource_id.as_str(),
            &flattened.resource_kind,
            &flattened.resource_label,
        );
        if let Some(ambient) = self.ambient_context.as_ref() {
            context = context.enriched_by(ambient);
        }
        let confirmation = flattened.action.contract.confirmation.clone();
        self.queue_or_confirm(
            PendingOperation {
                bound,
                address,
                encoded,
                context,
                kind: flattened.action.kind,
                input: PendingInput::Json(serde_json::Value::Object(arguments.clone()).to_string()),
                confirmation: confirmation.clone(),
                confirmation_evidence: serde_json::json!({}),
            },
            confirmation,
        );
        true
    }

    /// Drain a completed action activation and dispatch it against `doc` +
    /// `transport`. Decoding goes through [`ActionRegistry::decode`] — the
    /// same path a remote peer uses to replay. `None` when no action is
    /// pending (e.g. the last activation was a navigation).
    pub fn take_dispatch(
        &mut self,
        doc: &mut AutoCommit,
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
        Some(Self::dispatch_encoded(
            doc,
            &pending.encoded,
            registry,
            transport,
            pending.context,
        ))
    }

    /// Drain the resolved operation without assuming it is a document edit.
    /// The application host uses this to route effects/views to the owning
    /// widget and document operations to the deterministic executor.
    pub fn take_operation(&mut self) -> Option<PendingOperation> {
        let mut pending = self.pending_dispatch.take()?;
        let authority = self.authority_for(&pending.address, pending.bound.as_ref())?;
        let spec = tilo_actions::door::resolve(&authority, &pending.address)?;
        if !spec.contract.availability.is_available()
            || spec.kind != pending.kind
            || spec.contract.confirmation != pending.confirmation
        {
            return None;
        }
        pending.encoded = match &pending.input {
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
            PendingInput::Invalid => pending.encoded.clone(),
        };
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
                .resources
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

    #[cfg(feature = "native")]
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

    fn dispatch_encoded(
        doc: &mut AutoCommit,
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
            doc,
            action.as_ref(),
            encoded.clone(),
            transport,
            Some(&actor),
            context,
        )
    }

    /// Resolve a chosen row id into a pending outcome (or the argument step).
    fn resolve_activation(&mut self, id: &str) {
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
                    ActionKind::Immediate {
                        encoded,
                        context,
                        kind,
                        confirmation,
                    } => {
                        self.queue_or_confirm(
                            PendingOperation {
                                bound,
                                address,
                                encoded,
                                context,
                                kind,
                                input: PendingInput::Immediate,
                                confirmation: confirmation.clone(),
                                confirmation_evidence: serde_json::json!({}),
                            },
                            confirmation,
                        );
                    }
                    ActionKind::Prompted { prompt, .. } => {
                        // Repurpose the field to collect the argument; clear the result
                        // list so only the prompt shows.
                        self.field = self.field_source.build();
                        self.field.set_placeholder(&prompt);
                        self.field.set_focused(true);
                        self.results.set_groups(Vec::new());
                        self.arg = Some(ArgState { bound, address });
                    }
                    ActionKind::Typed { prompt, .. } => {
                        self.field = self.field_source.build();
                        self.field.set_placeholder(&prompt);
                        self.field.set_focused(true);
                        self.results.set_groups(Vec::new());
                        self.arg = Some(ArgState { bound, address });
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
        let mut context = ActionContext::for_resource(
            &fa.action.label,
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
            ActionInput::Immediate(enc) => ActionKind::Immediate {
                encoded: enc.clone(),
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

    /// Rebuild the result set for `query`: exported actions (fuzzy-filtered)
    /// as the **Actions** group, then each provider's hits as its own group.
    fn rebuild(&mut self, query: &str) {
        let mut rows: Vec<SearchResult> = Vec::new();
        let mut index: HashMap<String, RowResolution> = HashMap::new();

        // Keep only inert addresses in the row index. Encoding and callbacks
        // are resolved later, against the authority snapshot current at the
        // moment of activation.
        for fa in flatten(&self.resources) {
            if !fa.action.contract.availability.is_available() {
                continue;
            }
            let title = format!("{} \u{2014} {}", fa.resource_label, fa.action.label);
            if fuzzy_score(query, &title).is_some()
                || fa
                    .action
                    .description
                    .as_deref()
                    .and_then(|d| fuzzy_score(query, d))
                    .is_some()
            {
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
                index.insert(
                    fa.key.clone(),
                    RowResolution::Address(tilo_actions::ActionAddress::new(
                        fa.resource_id.as_str(),
                        fa.action.id.as_str(),
                    )),
                );
                rows.push(r);
            }
        }

        for snapshot in &self.bound_resources {
            let Ok(_entry) = snapshot.binding.enter() else {
                continue;
            };
            for resource in snapshot.resources.iter().cloned().filter_map(|resource| {
                crate::bound_dispatch::filter_resource(&snapshot.binding, resource)
            }) {
                for fa in flatten(&[resource]) {
                    if !fa.action.contract.availability.is_available() {
                        continue;
                    }
                    let title = format!(
                        "{} — {} · {}",
                        fa.resource_label, fa.action.label, snapshot.binding.instance_id
                    );
                    if fuzzy_score(query, &title).is_none() {
                        continue;
                    }
                    let target = crate::bound_dispatch::BoundActionTarget {
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
                    rows.push(SearchResult::new(ACTIONS_KIND, title, key.clone()));
                    index.insert(key, RowResolution::Bound(target));
                }
            }
        }

        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_query.clear();
        self.search_query.push_str(query);
        self.action_rows = rows;
        self.base_index = index;
        self.provider_rows.clear();
        self.provider_index.clear();
        self.provider_validation.clear();
        self.validation_queue.clear();
        self.provider_cursors = vec![None; self.providers.len()];
        self.provider_complete = vec![false; self.providers.len()];
        self.next_provider = 0;
        self.refresh_progressive_results();
        if self.providers.is_empty() {
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
        clippy::cast_possible_truncation,
        reason = "width and FIELD_H are UI layout dimensions in logical \
                  pixels, always tiny compared to f32's range; narrowing \
                  for taffy's f32-based layout loses no visually \
                  meaningful precision"
    )]
    #[allow(
        clippy::expect_used,
        reason = "private palette ownership mismatch is an invariant violation"
    )]
    /// # Panics
    /// Internal host-text palettes must use `layout_with_text` with their owning renderer.
    pub fn layout(&mut self, theme: &Theme, origin: Point, width: f64) {
        let text = self
            .measure
            .as_mut()
            .expect("host-text palette requires layout_with_text");
        Self::layout_field(
            &mut self.field,
            &mut self.results,
            &mut self.field_taffy,
            &mut self.field_node,
            &mut self.rect,
            theme,
            origin,
            width,
            text,
        );
    }

    #[cfg(feature = "native")]
    pub(crate) fn layout_with_text(
        &mut self,
        theme: &Theme,
        origin: Point,
        width: f64,
        text: &mut TextRenderer,
    ) {
        assert!(self.measure.is_none(), "owned-text palette requires layout");
        Self::layout_field(
            &mut self.field,
            &mut self.results,
            &mut self.field_taffy,
            &mut self.field_node,
            &mut self.rect,
            theme,
            origin,
            width,
            text,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::cast_possible_truncation,
        reason = "disjoint palette state borrows share one bounded logical-pixel layout algorithm"
    )]
    fn layout_field(
        field: &mut SearchField,
        results: &mut SearchResultsView,
        field_taffy: &mut TaffyTree<()>,
        field_node: &mut Option<NodeId>,
        rect: &mut Rect,
        theme: &Theme,
        origin: Point,
        width: f64,
        tr: &mut TextRenderer,
    ) {
        // Field: build a fresh 2-node tree so `handle_event` can resolve the
        // inner text input's node, then place it at the top of the card.
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = field.build_layout(&mut taffy, theme, &mut node_map);

        let Ok(()) = taffy.compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width as f32),
                height: AvailableSpace::Definite(FIELD_H as f32),
            },
            |known, available, id, _, _| {
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
        field.apply_layout(&taffy, node, origin);
        *field_taffy = taffy;
        *field_node = Some(node);

        // Results overlay directly under the field.
        let top = origin.y + FIELD_H + GAP;
        let h = results.desired_height();
        results.set_rect(Rect::new(origin.x, top, origin.x + width, top + h));
        *rect = Rect::new(origin.x, origin.y, origin.x + width, top + h);
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
        self.field
            .draw(scene, transform, text_renderer, theme, presence);
        self.results
            .draw(scene, transform, text_renderer, theme, presence);
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
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
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
                if let Some(state) = self.arg.take() {
                    let address = state.address;
                    let bound = state.bound;
                    let Some(fresh) = self.action_kind_for_address(&address, bound.as_ref()) else {
                        self.exit_argument_mode();
                        return true;
                    };
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
                        ActionKind::Immediate { .. } => {
                            self.exit_argument_mode();
                            return true;
                        }
                    };
                    self.queue_or_confirm(
                        PendingOperation {
                            bound,
                            address,
                            encoded,
                            context,
                            kind,
                            input,
                            confirmation: confirmation.clone(),
                            confirmation_evidence: serde_json::json!({}),
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
            self.route_to_results(event, *modifiers, doc, path);
            self.poll_view_activation();
        } else {
            self.route_to_field(event, *modifiers, doc, path);
            if let Some(q) = self.field.take_dirty() {
                self.rebuild(&q);
            }
        }
        true
    }

    /// Drain the results view's latched activation into a pending outcome.
    fn poll_view_activation(&mut self) {
        if let Some(id) = self.results.take_activated() {
            self.resolve_activation(&id);
        }
    }

    fn exit_argument_mode(&mut self) {
        self.arg = None;
        self.confirm = None;
        // Restore the query field and result set.
        self.field = self
            .field_source
            .build()
            .with_placeholder("Search actions and results");
        self.field.set_focused(true);
        self.rebuild("");
    }

    fn queue_or_confirm(&mut self, pending: PendingOperation, confirmation: ConfirmationPolicy) {
        let expected = match confirmation {
            ConfirmationPolicy::None => {
                self.pending_dispatch = Some(pending);
                self.open = false;
                return;
            }
            ConfirmationPolicy::Confirm => "CONFIRM".to_string(),
            ConfirmationPolicy::Phrase { phrase } => phrase,
        };
        self.field = self.field_source.build();
        self.field
            .set_placeholder(&format!("Type {expected} to confirm"));
        self.field.set_focused(true);
        self.results.set_groups(Vec::new());
        self.confirm = Some(ConfirmState { pending, expected });
    }

    fn route_to_field(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) {
        let Some(node) = self.field_node else { return };
        let mut handled = false;
        let mut ft = None;
        self.field.handle_event(
            event,
            1.0,
            &modifiers,
            doc,
            path,
            &mut handled,
            &mut ft,
            node,
            &self.field_taffy,
        );
    }

    fn route_to_results(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
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
            1.0,
            &modifiers,
            doc,
            path,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
    }
}

/// Rank `rows` by fit against `query` and bucket by kind. Reuses the shared
/// [`fuzzy_score`] primitive so a hit from one source is comparable to a hit
/// from another. Caps each group so no single source floods the overlay.
fn rank_and_group(query: &str, rows: Vec<SearchResult>) -> Vec<SearchGroup> {
    /// A provider bias notch is worth this many query-match points.
    const BIAS_WEIGHT: i32 = 4;
    let key = |r: &SearchResult| -> i32 {
        let bonus = fuzzy_score(query, &r.title)
            .or_else(|| r.subtitle.as_deref().and_then(|s| fuzzy_score(query, s)))
            .unwrap_or(0);
        r.score.saturating_mul(BIAS_WEIGHT).saturating_add(bonus)
    };
    let mut ranked = rows;
    ranked.sort_by_key(|b| std::cmp::Reverse(key(b)));
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

#[cfg(test)]
pub(crate) fn observed_field_language(palette: &ActionPalette) -> &str {
    crate::widget::search_field::observed_language(&palette.field)
}

#[cfg(test)]
mod tests {
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
            .with_action(ActionSpec::prompted(
                "rename",
                "Rename",
                "New name",
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
                .filter(|n| fuzzy_score(query, n).is_some())
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
            fuzzy_score(query, "Ada Lovelace")
                .map(|_| {
                    vec![SearchResult::for_action(
                        "People",
                        "Ada Lovelace",
                        tilo_actions::ActionAddress::new("test.thing:1", "mark"),
                    )]
                })
                .unwrap_or_default()
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
    fn large_lazy_provider_is_budgeted_but_cannot_supply_its_own_authority() {
        let materialized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut palette = ActionPalette::new();
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

    #[test]
    fn progressive_palette_reaches_exact_match_beyond_one_hundred_thousand_candidates() {
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

        let mut palette = ActionPalette::new();
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

    /// Both an ACTION (from the resource tree) and a searchable CONTENT hit
    /// (from a folded-in provider) surface, grouped, for one query.
    #[test]
    fn actions_and_content_both_surface_grouped() {
        let mut p = ActionPalette::new();
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

    /// Activating an ACTION row dispatches through `tilo_actions` and mutates the
    /// CRDT — the real replay path.
    #[test]
    fn activating_an_action_dispatches_and_mutates() {
        let mut p = ActionPalette::new();
        p.load_open_resources(&[thing_resource()]);
        p.open();

        let idx = p
            .result_labels()
            .iter()
            .position(|l| l.contains("Mark done"))
            .expect("mark row");
        p.set_cursor(idx);
        p.activate_selected();

        let mut doc = AutoCommit::new();
        assert!(doc.get(&ROOT, "marked").unwrap().is_none());
        let mut tx = LocalTransport::new();
        let d = p
            .take_dispatch(&mut doc, &registry(), &mut tx)
            .expect("an action was pending")
            .expect("dispatch ok");
        assert!(matches!(d, Dispatched::AppliedLocal(_)));
        let context = d.envelope().context.as_ref().expect("palette provenance");
        assert_eq!(context.action_label, "Mark done");
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
                .and_then(|(v, _)| v.to_str().map(str::to_string)),
            Some("yes".to_string()),
        );
        assert!(!p.is_open(), "an immediate action closes the palette");
        assert!(
            p.take_navigation().is_none(),
            "an action is not a navigation"
        );
    }

    #[test]
    fn ambient_task_context_reaches_palette_actions_without_overwriting_resource() {
        let mut p = ActionPalette::new();
        p.load_open_resources(&[thing_resource()]);
        p.set_ambient_context(Some(
            ActionContext::default()
                .with_app("org.tilo.Agent", "Agent")
                .with_purpose("Prepare launch")
                .with_group("issue:42", "Launch", "issue"),
        ));
        p.open();
        p.activate_selected();

        let mut doc = AutoCommit::new();
        let mut tx = LocalTransport::new();
        let dispatched = p
            .take_dispatch(&mut doc, &registry(), &mut tx)
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

    /// Activating a CONTENT row queues a navigation (its opaque id), NOT a
    /// dispatch.
    #[test]
    fn activating_content_navigates_not_dispatches() {
        let mut p = ActionPalette::new();
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

        let mut doc = AutoCommit::new();
        let mut tx = LocalTransport::new();
        assert!(
            p.take_dispatch(&mut doc, &registry(), &mut tx).is_none(),
            "a content row is not a dispatch"
        );
        assert_eq!(
            p.take_navigation().as_deref(),
            Some("person:Ada Lovelace"),
            "the opaque provider id is handed back for navigation"
        );
        assert!(!p.is_open(), "navigation closes the palette");
    }

    /// Canonical provider hits are broker operations, not opaque navigation
    /// callbacks. This is the per-app migration contract.
    #[test]
    fn activating_canonical_search_hit_queues_pending_operation() {
        let mut p = ActionPalette::new();
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
        assert_eq!(pending.encoded.type_id, NOTE_TYPE);
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

    #[test]
    fn removed_canonical_action_is_refused_at_activation() {
        let mut p = ActionPalette::new();
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

    #[test]
    fn disabled_canonical_action_is_refused_at_activation() {
        let mut disabled = thing_resource();
        disabled.actions[0].contract.availability = tilo_actions::Availability::Unavailable {
            code: "offline".into(),
            reason: "The service went offline".into(),
        };

        let mut p = ActionPalette::new();
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

    #[test]
    fn removed_action_is_refused_again_at_dispatch_boundary() {
        let mut p = ActionPalette::new();
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

    /// A PROMPTED action collects its argument in the field, then dispatches the
    /// action built from the typed text.
    #[test]
    fn prompted_action_collects_argument_then_dispatches() {
        let mut p = ActionPalette::new();
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
        let mut doc = AutoCommit::new();
        let root = ROOT;
        let mods = Modifiers::default();
        p.handle_key(&enter(), &mods, &mut doc, &root);

        let mut tx = LocalTransport::new();
        let d = p
            .take_dispatch(&mut doc, &registry(), &mut tx)
            .expect("prompted action pending")
            .expect("dispatch ok");
        assert!(matches!(d, Dispatched::AppliedLocal(_)));
        assert_eq!(
            doc.get(&ROOT, "name")
                .unwrap()
                .and_then(|(v, _)| v.to_str().map(str::to_string)),
            Some("Sprocket".to_string()),
            "the action carries the typed argument",
        );
    }

    #[test]
    fn consequential_action_waits_for_exact_confirmation() {
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
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&[resource]);
        palette.open();
        palette.activate_selected();
        assert!(palette.in_argument_mode());

        let mut doc = AutoCommit::new();
        let mut transport = LocalTransport::new();
        assert!(
            palette
                .take_dispatch(&mut doc, &registry(), &mut transport)
                .is_none(),
            "activation alone must not queue the operation"
        );

        palette.field.set_query("mark thing");
        palette.handle_key(&enter(), &Modifiers::default(), &mut doc, &ROOT);
        assert!(
            palette
                .take_dispatch(&mut doc, &registry(), &mut transport)
                .is_none(),
            "confirmation is exact and case-sensitive"
        );

        palette.field.set_query("MARK THING");
        palette.handle_key(&enter(), &Modifiers::default(), &mut doc, &ROOT);
        let pending = palette
            .take_operation()
            .expect("exact confirmation queues the operation");
        assert_eq!(
            pending.confirmation_evidence(),
            &serde_json::json!({ "confirmation_phrase": "MARK THING" }),
            "the broker receives the exact evidence the palette verified"
        );
        ActionPalette::dispatch_encoded(
            &mut doc,
            &pending.encoded,
            &registry(),
            &mut transport,
            pending.context,
        )
        .expect("confirmed dispatch succeeds");
        assert_eq!(
            doc.get(&ROOT, "marked")
                .unwrap()
                .and_then(|(value, _)| value.to_str().map(str::to_owned)),
            Some("yes".into())
        );
    }

    fn enter_key() -> KeyEvent {
        // SAFETY: `handle_key` only reads `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        ev
    }

    fn enter() -> WindowEvent {
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: enter_key(),
            is_synthetic: false,
        }
    }
}
