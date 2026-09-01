//! `ActionPalette` — the one Ctrl+K surface for a `ui::App`.
//!
//! The **resource + action model** ([`qos_actions`]) is the foundation: the
//! focused app instance publishes a tree of open [`Resource`]s, each
//! exporting [`ActionSpec`](qos_actions::ActionSpec)s — the operations
//! Ctrl+K offers. An app can also fold in *searchable content* via
//! [`SearchProvider`](crate::search::SearchProvider)s (contacts, files, …);
//! their hits appear as navigable result rows in the same surface.
//!
//! One registry, one renderer, one flow: input is the shared
//! [`SearchField`](crate::widget::SearchField); results render through
//! [`SearchResultsView`](crate::widget::SearchResultsView), grouped by kind.
//! An action row resolves to its [`EncodedAction`] and
//! [`dispatch_with_context`](qos_actions::dispatch_with_context)es through `qos_actions` — the same
//! path a remote peer replays — with a
//! [`Prompted`](qos_actions::ActionInput::Prompted) action first collecting
//! its argument in the field; a content row hands its opaque `action_id`
//! back to the app to navigate, never a CRDT mutation.
//!
//! The host ([`crate::app::App`]) drains a completed activation with
//! [`take_dispatch`](ActionPalette::take_dispatch) (actions) and
//! [`take_navigation`](ActionPalette::take_navigation) (content).

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::search::{SearchGroup, SearchProvider, SearchResult, fuzzy_score};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{SearchField, SearchResultsView, Widget};
use qos_actions::{
    ActionContext, ActionInput, ActionRegistry, ActionTransport, Dispatched, EncodedAction,
    OperationKind, Resource, flatten,
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
const PER_GROUP_CAP: usize = 8;

/// How activating one row resolves.
#[derive(Clone)]
enum RowResolution {
    /// An exported action — dispatch through `qos_actions`.
    Dispatch(ActionKind),
    /// A search-provider hit — hand the opaque id back to the app to navigate.
    Navigate(String),
}

/// A resource-scoped action's path to an [`EncodedAction`].
#[derive(Clone)]
enum ActionKind {
    /// Fires immediately with this pre-encoded action.
    Immediate {
        encoded: EncodedAction,
        context: ActionContext,
        kind: OperationKind,
    },
    /// Collects a free-text argument in the field first, then builds the action.
    Prompted {
        prompt: String,
        build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
        context: ActionContext,
        kind: OperationKind,
    },
}

/// Live state while the field is collecting a prompted action's argument.
struct ArgState {
    build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
    context: ActionContext,
    kind: OperationKind,
}

/// A palette activation after address resolution and argument collection.
pub struct PendingOperation {
    pub encoded: EncodedAction,
    pub context: ActionContext,
    pub kind: OperationKind,
}

/// The unified Ctrl+K palette: a [`SearchField`] over a [`SearchResultsView`],
/// fed by the focused app's resource/action tree plus any folded-in
/// [`SearchProvider`]s.
pub struct ActionPalette {
    field: SearchField,
    results: SearchResultsView,
    /// Row `action_id` → how activating it resolves.
    index: HashMap<String, RowResolution>,
    /// The focused instance's open resources (their exported actions are rows).
    resources: Vec<Resource>,
    /// Searchable-content sources folded in as navigable result groups.
    providers: Vec<Box<dyn SearchProvider>>,
    open: bool,
    /// `Some` while collecting a prompted action's argument in the field.
    arg: Option<ArgState>,
    /// A ready-to-dispatch action, set on activation, drained by `take_dispatch`.
    pending_dispatch: Option<PendingOperation>,
    ambient_context: Option<ActionContext>,
    /// A navigation target, set on activation, drained by `take_navigation`.
    pending_nav: Option<String>,
    /// The palette's outer rect (field + results), set by `layout`.
    rect: Rect,
    /// The field's own layout tree, rebuilt each `layout` so its `handle_event`
    /// can resolve the inner text input's node.
    field_taffy: TaffyTree<()>,
    field_node: Option<NodeId>,
    /// A text renderer kept for the field's per-frame measure pass, so `layout`
    /// does not construct a font system every frame the palette is open.
    measure: TextRenderer,
}

impl Default for ActionPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPalette {
    /// A closed palette. Load resources / providers, then `open`.
    pub fn new() -> Self {
        Self {
            field: SearchField::new().with_placeholder("Search actions and results"),
            results: SearchResultsView::new()
                .with_empty_text("No matches")
                .with_test_id("action-palette-results"),
            index: HashMap::new(),
            resources: Vec::new(),
            providers: Vec::new(),
            open: false,
            arg: None,
            pending_dispatch: None,
            ambient_context: None,
            pending_nav: None,
            rect: Rect::ZERO,
            field_taffy: TaffyTree::new(),
            field_node: None,
            measure: TextRenderer::new(),
        }
    }

    /// Populate from the focused instance's open-resource forest. Each exported
    /// action becomes an **Actions** row; rebuilds the result set + index.
    pub fn load_open_resources(&mut self, resources: &[Resource]) {
        self.resources = resources.to_vec();
        // `.to_string()` is load-bearing here, not redundant: it ends the
        // immutable borrow of `self.field` before `rebuild` takes `&mut self`.
        #[allow(clippy::unnecessary_to_owned)]
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    pub fn set_ambient_context(&mut self, context: Option<ActionContext>) {
        self.ambient_context = context;
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    /// Fold in searchable-content sources. Their hits appear as navigable result
    /// rows, grouped by each provider's kind.
    pub fn set_providers(&mut self, providers: Vec<Box<dyn SearchProvider>>) {
        self.providers = providers;
        // `.to_string()` is load-bearing here, not redundant: it ends the
        // immutable borrow of `self.field` before `rebuild` takes `&mut self`.
        #[allow(clippy::unnecessary_to_owned)]
        let query = self.field.value().to_string();
        self.rebuild(&query);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open (focus) the palette: reset the query, argument, and any pending
    /// outcome, and rebuild the (empty-query) result set.
    pub fn open(&mut self) {
        self.open = true;
        self.arg = None;
        self.pending_dispatch = None;
        self.pending_nav = None;
        self.field = SearchField::new().with_placeholder("Search actions and results");
        self.field.set_focused(true);
        self.rebuild("");
    }

    pub fn close(&mut self) {
        self.open = false;
        self.arg = None;
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

    /// Move the highlight to a flat row index (clamped).
    pub fn set_cursor(&mut self, index: usize) {
        self.results.set_cursor(index);
    }

    /// The highlighted flat row index.
    pub fn cursor(&self) -> usize {
        self.results.cursor()
    }

    pub fn in_argument_mode(&self) -> bool {
        self.arg.is_some()
    }

    /// Activate the highlighted row, exactly as pressing Enter would.
    pub fn activate_selected(&mut self) {
        let Some(id) = self.results.selected_action_id().map(str::to_string) else {
            return;
        };
        self.resolve_activation(&id);
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
    ) -> Option<Result<Dispatched, qos_actions::ActionError>> {
        let pending = self.pending_dispatch.take()?;
        if pending.kind != OperationKind::Document {
            return Some(Err(qos_actions::ActionError::Refused(format!(
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
        self.pending_dispatch.take()
    }

    /// Drain a completed content activation: the opaque `action_id` of the
    /// chosen search-provider row, for the app to navigate to.
    pub fn take_navigation(&mut self) -> Option<String> {
        self.pending_nav.take()
    }

    fn dispatch_encoded(
        doc: &mut AutoCommit,
        encoded: &EncodedAction,
        registry: &ActionRegistry,
        transport: &mut dyn ActionTransport,
        context: ActionContext,
    ) -> Result<Dispatched, qos_actions::ActionError> {
        let action = registry.decode(encoded)?;
        let actor = doc.get_actor().to_bytes().to_vec();
        qos_actions::dispatch_by_with_context(
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
            Some(RowResolution::Dispatch(ActionKind::Immediate {
                encoded,
                context,
                kind,
            })) => {
                self.pending_dispatch = Some(PendingOperation {
                    encoded,
                    context,
                    kind,
                });
                self.open = false;
            }
            Some(RowResolution::Dispatch(ActionKind::Prompted {
                prompt,
                build,
                context,
                kind,
            })) => {
                // Repurpose the field to collect the argument; clear the result
                // list so only the prompt shows.
                self.field = SearchField::new();
                self.field.set_placeholder(&prompt);
                self.field.set_focused(true);
                self.results.set_groups(Vec::new());
                self.arg = Some(ArgState {
                    build,
                    context,
                    kind,
                });
            }
            Some(RowResolution::Navigate(target)) => {
                self.pending_nav = Some(target);
                self.open = false;
            }
            None => {}
        }
    }

    /// Rebuild the result set for `query`: exported actions (fuzzy-filtered)
    /// as the **Actions** group, then each provider's hits as its own group.
    fn rebuild(&mut self, query: &str) {
        let mut rows: Vec<SearchResult> = Vec::new();
        let mut index: HashMap<String, RowResolution> = HashMap::new();

        // Actions from the resource forest.
        for fa in flatten(&self.resources) {
            let title = format!("{} \u{2014} {}", fa.resource_label, fa.action.label);
            if fuzzy_score(query, &title).is_none()
                && fa
                    .action
                    .description
                    .as_deref()
                    .and_then(|d| fuzzy_score(query, d))
                    .is_none()
            {
                continue;
            }
            let mut r = SearchResult::new(ACTIONS_KIND, title, fa.key.clone());
            if let Some(desc) = &fa.action.description {
                r = r.with_subtitle(desc.clone());
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
            let kind = match &fa.action.input {
                ActionInput::Immediate(enc) => ActionKind::Immediate {
                    encoded: enc.clone(),
                    context,
                    kind: fa.action.kind,
                },
                ActionInput::Prompted { prompt, build, .. } => ActionKind::Prompted {
                    prompt: prompt.clone(),
                    build: build.clone(),
                    context,
                    kind: fa.action.kind,
                },
            };
            index.insert(fa.key.clone(), RowResolution::Dispatch(kind));
            rows.push(r);
        }

        // Searchable content from folded-in providers (already query-filtered).
        for provider in &self.providers {
            for hit in provider.search(query) {
                let row_id = format!("{NAV_PREFIX}{}", hit.action_id);
                index.insert(
                    row_id.clone(),
                    RowResolution::Navigate(hit.action_id.clone()),
                );
                let mut r = SearchResult::new(hit.kind.clone(), hit.title.clone(), row_id)
                    .with_score(hit.score);
                if let Some(sub) = &hit.subtitle {
                    r = r.with_subtitle(sub.clone());
                }
                if let Some(icon) = hit.icon {
                    r = r.with_icon(icon);
                }
                rows.push(r);
            }
        }

        self.index = index;
        self.results.set_groups(rank_and_group(query, rows));
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
    pub fn layout(&mut self, theme: &Theme, origin: Point, width: f64) {
        // Field: build a fresh 2-node tree so `handle_event` can resolve the
        // inner text input's node, then place it at the top of the card.
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.field.build_layout(&mut taffy, theme, &mut node_map);
        let tr = &mut self.measure;
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
        self.field.apply_layout(&taffy, node, origin);
        self.field_taffy = taffy;
        self.field_node = Some(node);

        // Results overlay directly under the field.
        let top = origin.y + FIELD_H + GAP;
        let h = self.results.desired_height();
        self.results
            .set_rect(Rect::new(origin.x, top, origin.x + width, top + h));
        self.rect = Rect::new(origin.x, origin.y, origin.x + width, top + h);
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
            if self.arg.is_some() {
                self.exit_argument_mode();
            } else {
                self.close();
            }
            return true;
        }

        // Argument step: the field collects the free text; Enter builds +
        // queues the action; every other key edits the argument buffer.
        if self.arg.is_some() {
            if matches!(&key.logical_key, Key::Named(NamedKey::Enter)) {
                let value = self.field.value().to_string();
                if let Some(state) = self.arg.take() {
                    self.pending_dispatch = Some(PendingOperation {
                        encoded: (state.build)(&value),
                        context: state.context,
                        kind: state.kind,
                    });
                }
                self.open = false;
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
        // Restore the query field and result set.
        self.field = SearchField::new().with_placeholder("Search actions and results");
        self.field.set_focused(true);
        self.rebuild("");
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
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use qos_actions::{
        Action, ActionError, ActionRegistry, ActionSpec, Doc, ExecutionLocus, LocalTransport,
    };
    use serde::{Deserialize, Serialize};
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

    /// Activating an ACTION row dispatches through `qos_actions` and mutates the
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
                .with_app("org.qos.Agent", "Agent")
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
