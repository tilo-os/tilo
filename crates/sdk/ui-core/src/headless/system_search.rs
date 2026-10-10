//! Portable system search. Hosts supply admitted identities and resolve inert selections.

use super::{
    Affine, ElementState, FrameHarness, HashMap, InspectionNode, MouseButton, NodeId, Point,
    PresenceCtx, Rect, TaffyTree, Theme, Widget, WindowEvent,
};
use crate::CommandPalette;
use crate::paint::PaintCtx;
use crate::search::system::filters::{
    FilterAction, FilterStripSlot, FilterText, KindChip, ScopeSegment, SearchFilters, toggle_kind,
};
use crate::search::system::{
    self, Provider, Query, ResourceKind, SearchBudget, SearchCursor, SearchPage, SearchRow,
    SearchScope,
};
use crate::tokens::TextRole;
use tilo_actions::ActionAddress;

const UNAVAILABLE_DOMAINS: &str = "Files and host apps are unavailable in this browser.";

/// Only applications actually supported by this host belong here. The address
/// is a current certified/local View request, never an executable callback.
#[derive(Clone, Debug)]
pub struct GlobalSearchApplication {
    pub app_id: String,
    pub title: String,
    pub icon: Option<crate::icons::Icon>,
    pub action: ActionAddress,
}
/// An admitted tab, correlated with the mounted component by instance identity.
#[derive(Clone, Debug)]
pub struct GlobalSearchTab {
    pub target: String,
    pub instance_id: u64,
    pub app_id: String,
    pub record_id: String,
    pub title: String,
    pub in_active_workspace: bool,
}
#[derive(Clone, Debug, Default)]
pub struct GlobalSearchSourceData {
    pub revision: String,
    pub applications: Vec<GlobalSearchApplication>,
    pub tabs: Vec<GlobalSearchTab>,
    pub active_instance_id: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalSearchIntent {
    /// Resolve both fields against the host's *current* admitted route map.
    SelectHostTarget { revision: String, target: String },
    /// Pass to `handoff_caller_action`; this is not execution authority.
    CallerAction(ActionAddress),
}
#[derive(Clone, PartialEq)]
struct Row {
    identity: String,
    title: String,
    subtitle: Option<String>,
    kind: ResourceKind,
    score: i32,
    icon: Option<crate::icons::Icon>,
    intent: GlobalSearchIntent,
}
impl SearchRow for Row {
    fn kind(&self) -> &ResourceKind {
        &self.kind
    }
    fn score(&self) -> i32 {
        self.score
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }
}
struct SnapshotProvider {
    id: String,
    kinds: Vec<ResourceKind>,
    rows: Vec<Row>,
}
impl SnapshotProvider {
    fn new(id: String, kinds: Vec<ResourceKind>, rows: Vec<Row>) -> Self {
        let mut kinds = kinds;
        for row in &rows {
            if !kinds.contains(&row.kind) {
                kinds.push(row.kind.clone());
            }
        }
        Self { id, kinds, rows }
    }
}
impl Provider<(), Row> for SnapshotProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn available_in_active_app_scope(&self) -> bool {
        self.id == "actions"
    }
    fn query_page(
        &self,
        (): &(),
        query: &Query<'_>,
        _: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage<Row> {
        let mut bytes = 0;
        let rows = self
            .rows
            .iter()
            .take(budget.max_work)
            .filter(|r| query.wanted_kinds.contains(&r.kind))
            .take(budget.max_results)
            .take_while(|r| {
                bytes +=
                    r.title.len() + r.subtitle.as_ref().map_or(0, String::len) + r.identity.len();
                bytes <= budget.max_bytes
            })
            .cloned()
            .collect();
        SearchPage::exhausted("query-snapshot", rows)
    }
    fn resolve(&self, (): &(), id: &str, query: &Query<'_>) -> Option<Row> {
        self.rows
            .iter()
            .find(|r| r.identity == id && query.wanted_kinds.contains(&r.kind))
            .cloned()
    }
}
fn kind(label: &str) -> ResourceKind {
    match label {
        "Notes" => ResourceKind::Note,
        "Records" => ResourceKind::Record,
        "Actions" | "Commands" => ResourceKind::Action,
        other => ResourceKind::Custom(other.into()),
    }
}
#[derive(Default)]
pub(super) struct SystemSearch {
    data: GlobalSearchSourceData,
    overlay: Option<Overlay>,
    rows: HashMap<String, Row>,
    epoch: u64,
    pending: Option<GlobalSearchIntent>,
    saved_focus: Option<crate::widget::layout::CompositionFocus>,
    /// Applications, actions and tabs, each indexed while it stays the same.
    indexes: [crate::search::ListIndex; 3],
}
struct Overlay {
    palette: CommandPalette,
    filters: SearchFilters,
    scope: SearchScope,
    wanted: Vec<ResourceKind>,
    hover: Option<Point>,
}
impl Widget for Overlay {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.palette.build_layout(taffy, theme, nodes)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.palette.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.palette.draw(scene, transform, text, theme, presence);
        self.filters.draw(
            &mut PaintCtx::new(scene, transform, text, theme),
            FilterStripSlot {
                band: self.palette.filter_strip_rect(),
                row_height: self.palette.filter_strip_row_height(),
                reserved_rows: self.palette.host_filter_strip_rows(),
            },
            self.hover,
        );
        let rect = self.palette.get_rect();
        text.draw_style(
            scene,
            UNAVAILABLE_DOMAINS,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((rect.x0 + 12.0, rect.y1 + 8.0)),
            Some((rect.width() - 24.0).max(1.0) as f32),
            &theme.text(TextRole::Caption),
        );
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.palette.handle_event(event, ctx);
    }
    fn name(&self) -> &str {
        "SystemSearch"
    }
    fn test_id(&self) -> Option<&str> {
        Some("system-search")
    }
    fn get_rect(&self) -> Rect {
        let rect = self.palette.get_rect();
        Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + 28.0)
    }
    fn needs_redraw(&self) -> bool {
        self.palette.needs_redraw()
    }
    fn needs_animation_frame(&self) -> bool {
        self.palette.needs_animation_frame()
    }
    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.palette.next_wake_deadline()
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.palette.is_focused()
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut access = self.palette.accessibility()?;
        access.name = Some("Search everywhere".into());
        access.description=Some("Search supported apps, admitted tabs and content, and available actions. Files and host applications are unavailable in this browser.".into());
        access.children.push(self.filters.accessibility());
        access.children.push(
            crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
                .with_name(UNAVAILABLE_DOMAINS)
                .with_bounds(Rect::new(
                    self.palette.get_rect().x0 + 12.0,
                    self.palette.get_rect().y1 + 8.0,
                    self.palette.get_rect().x1 - 12.0,
                    self.palette.get_rect().y1 + 28.0,
                )),
        );
        Some(access)
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        if let Some(tail) = path.strip_prefix(&[2]) {
            self.filters.rect_for_path(tail)
        } else {
            self.palette.synthetic_accessibility_rect(path)
        }
    }
}
impl FrameHarness {
    /// Replace the coherent admitted host snapshot. Revocation updates open
    /// results immediately and invalidates undrained selections from older data.
    pub fn replace_global_search_data(&mut self, data: GlobalSearchSourceData) {
        self.system_search.data = data;
        self.system_search.rows.clear();
        self.system_search.pending = None;
        self.refresh_system_search();
        self.relayout();
    }
    pub fn global_search_query(&self) -> Option<&str> {
        self.system_search
            .overlay
            .as_ref()
            .map(|o| o.palette.query())
    }
    pub fn global_search_scope(&self) -> Option<SearchScope> {
        self.system_search.overlay.as_ref().map(|o| o.scope)
    }
    fn global_search_guard(&self) -> Result<(), String> {
        if self.root.modal_capture().is_some()
            || self.focus_trap_active
            || self.caller_palette.palette.is_some()
            || self.caller_palette.pending.is_some()
            || self.system_search.pending.is_some()
            || self.history.as_ref().is_some_and(|h| h.captures_input())
        {
            Err("Finish the current dialog or command first".into())
        } else {
            Ok(())
        }
    }
    pub fn open_global_search_all(&mut self, query: &str) -> Result<(), String> {
        self.global_search_guard()?;
        if !system::query_input_is_bounded(query) {
            return Err("Search query is too long".into());
        }
        if self.system_search.overlay.is_none() {
            self.system_search.saved_focus = self.capture_focus_identity();
        }
        let mut palette = CommandPalette::new();
        palette.set_modal(true);
        palette.set_bypass_fuzzy(true);
        palette.set_host_filter_strip(true);
        palette.set_platform_conventions(self.platform);
        palette.set_query_placeholder("Search apps, content and actions");
        palette.open();
        palette.set_query(query);
        let hover = self.system_search_pointer_position();
        self.system_search.overlay = Some(Overlay {
            palette,
            filters: SearchFilters::default(),
            scope: SearchScope::All,
            wanted: vec![],
            hover,
        });
        self.refresh_system_search();
        self.relayout();
        Ok(())
    }
    pub fn take_global_search_intent(&mut self) -> Option<GlobalSearchIntent> {
        self.system_search.pending.take()
    }
    /// Re-resolve an inert selected address through the existing argument,
    /// confirmation and caller/local-View admission path.
    pub fn handoff_caller_action(&mut self, address: &ActionAddress) -> Result<(), String> {
        self.global_search_guard()?;
        if self.system_search.overlay.is_some() {
            return Err("Close search before handing off an action".into());
        }
        let resources = self.caller_resources();
        let mut palette = crate::action_palette::ActionPalette::with_host_text(
            self.text.font_system.locale().to_owned(),
        );
        palette.load_open_resources(&resources);
        palette.open();
        if !palette.activate_address(&address.resource, &address.action) {
            return Err("Search target is no longer available".into());
        }
        self.caller_palette.palette = Some(palette);
        self.finish_caller_palette();
        self.relayout();
        Ok(())
    }
    fn close_system_search(&mut self) {
        self.system_search.overlay = None;
        self.system_search.rows.clear();
        if let Some(saved) = self.system_search.saved_focus.take() {
            // Restore the retained owner without issuing an explicit focus request:
            // desktop hosts interpret that request as a new navigation gesture.
            // SAFETY: restore only a stable identity in the current admitted tree.
            self.focused = unsafe { saved.restore(&self.node_map, &self.bindings, None) };
        }
    }
    fn system_search_providers(&self, needle: &str, scope: SearchScope) -> Vec<SnapshotProvider> {
        if !system::query_input_is_bounded(needle) {
            return vec![];
        }
        let data = &self.system_search.data;
        let needle = needle.to_lowercase();
        let host_intent = |tab: &GlobalSearchTab| GlobalSearchIntent::SelectHostTarget {
            revision: data.revision.clone(),
            target: tab.target.clone(),
        };
        let [app_index, action_index, tab_index] = &self.system_search.indexes;
        let app_ranks = app_index.ranks(
            &needle,
            data.applications
                .iter()
                .map(|app| system::application_fields(&app.title, &app.app_id, None, None)),
        );
        let applications = data
            .applications
            .iter()
            .zip(app_ranks)
            .filter_map(|(app, rank)| {
                Some(Row {
                    identity: format!("app:{}", app.app_id),
                    title: app.title.clone(),
                    subtitle: Some(app.app_id.clone()),
                    kind: ResourceKind::Application,
                    score: system::provider_score(rank?),
                    icon: app.icon,
                    intent: GlobalSearchIntent::CallerAction(app.action.clone()),
                })
            })
            .collect();
        let caller_resources = self.caller_resources();
        let caller_actions: Vec<_> = tilo_actions::flatten(&caller_resources)
            .into_iter()
            .filter(|fa| fa.action.contract.availability.is_available())
            // Desktop launch actions already have the authoritative Applications group.
            .filter(|fa| {
                !data.applications.iter().any(|a| {
                    a.action.resource == fa.resource_id.as_str() && a.action.action == fa.action.id
                })
            })
            .collect();
        // The resource label is context, not the command name. Otherwise
        // "Writer — Insert table" ties the exact Writer application and
        // Actions precedence steals its launch.
        let contexts: Vec<String> = caller_actions
            .iter()
            .map(|fa| {
                format!(
                    "{} {}",
                    fa.resource_label,
                    fa.action.description.as_deref().unwrap_or("")
                )
            })
            .collect();
        let action_ranks = action_index.ranks(
            &needle,
            caller_actions.iter().zip(&contexts).map(|(fa, context)| {
                system::application_fields(&fa.action.label, &fa.action.id, None, Some(context))
            }),
        );
        let actions = caller_actions
            .into_iter()
            .zip(action_ranks)
            .filter_map(|(fa, rank)| {
                Some(Row {
                    identity: format!("action:{}", fa.key),
                    score: system::provider_score(rank?),
                    title: format!("{} — {}", fa.resource_label, fa.action.label),
                    subtitle: fa.action.description.clone(),
                    kind: ResourceKind::Action,
                    icon: None,
                    intent: GlobalSearchIntent::CallerAction(ActionAddress::new(
                        fa.resource_id.as_str(),
                        &fa.action.id,
                    )),
                })
            })
            .collect();
        let mut providers = vec![
            SnapshotProvider::new(
                "applications".into(),
                vec![ResourceKind::Application],
                applications,
            ),
            SnapshotProvider::new("actions".into(), vec![ResourceKind::Action], actions),
        ];
        // Duplicate instance mappings are ambiguous and therefore not admitted.
        let tabs: Vec<_> = data
            .tabs
            .iter()
            .filter(|tab| {
                data.tabs
                    .iter()
                    .filter(|t| t.instance_id == tab.instance_id)
                    .count()
                    == 1
            })
            .filter(|tab| scope != SearchScope::Workspace || tab.in_active_workspace)
            .filter(|tab| {
                scope != SearchScope::ActiveApp
                    || data
                        .active_instance_id
                        .is_none_or(|id| id == tab.instance_id)
            })
            .collect();
        let tab_ranks = tab_index.ranks(
            &needle,
            tabs.iter()
                .map(|tab| system::application_fields(&tab.title, &tab.record_id, None, None)),
        );
        for (tab, rank) in tabs.iter().zip(tab_ranks) {
            let rows = rank
                .map(system::provider_score)
                .map(|score| Row {
                    identity: format!("tab:{}", tab.target),
                    title: tab.title.clone(),
                    subtitle: Some(tab.app_id.clone()),
                    kind: ResourceKind::Record,
                    score,
                    icon: None,
                    intent: host_intent(tab),
                })
                .into_iter()
                .collect();
            providers.push(SnapshotProvider::new(
                format!("instance:{}", tab.instance_id),
                vec![ResourceKind::Record],
                rows,
            ));
        }
        let mut eager_resources = None;
        for provider in crate::bound_dispatch::search_providers(self.root.as_ref()) {
            let Some(binding) = provider.callback_binding() else {
                continue;
            };
            let Some(tab) = tabs.iter().find(|tab| {
                tab.instance_id == binding.instance_id
                    && tab.app_id == binding.app_id
                    && tab.record_id == binding.checkout.id.as_str()
            }) else {
                continue;
            };
            let page = provider.search_page(
                &crate::search::SearchQuery::new(needle.as_str()),
                None,
                SearchBudget::PALETTE.max_work,
                SearchBudget::PALETTE.max_results,
            );
            let rows = page
                .results
                .into_iter()
                .filter(|hit| {
                    let Ok(_entry) = binding.enter() else {
                        return false;
                    };
                    let Some(address) = hit.action_address() else {
                        return !hit.has_action_address()
                            && binding.permissions.borrow().check(
                                &crate::permissions::PermissionScope::app(
                                    &binding.app_id,
                                    crate::permissions::ActionKind::Read,
                                ),
                            );
                    };
                    let resource = provider.resolve_action_resource(&address).or_else(|| {
                        let snapshots = eager_resources.get_or_insert_with(|| {
                            crate::bound_dispatch::discover_bound_resources(
                                self.root.as_ref(),
                                &self.state.doc,
                                None,
                            )
                            .unwrap_or_default()
                        });
                        snapshots
                            .iter()
                            .filter(|snapshot| snapshot.binding.same_component(&binding))
                            .flat_map(|snapshot| &snapshot.resources)
                            .find(|resource| {
                                tilo_actions::door::resolve(
                                    std::slice::from_ref(resource),
                                    &address,
                                )
                                .is_some()
                            })
                            .cloned()
                    });
                    resource
                        .and_then(|resource| {
                            crate::bound_dispatch::filter_resource(&binding, resource)
                        })
                        .is_some_and(|resource| {
                            tilo_actions::door::resolve(&[resource], &address).is_some()
                        })
                })
                .map(|hit| Row {
                    identity: format!(
                        "content:{}:{}:{}",
                        tab.target,
                        provider.name(),
                        hit.action_id
                    ),
                    title: hit.title,
                    subtitle: hit.subtitle,
                    kind: kind(&hit.kind),
                    score: hit.score.clamp(0, 100),
                    icon: hit.icon,
                    intent: host_intent(tab),
                })
                .collect();
            providers.push(SnapshotProvider::new(
                format!("instance:{}", tab.instance_id),
                vec![kind(provider.name())],
                rows,
            ));
        }
        providers
    }
    pub(super) fn refresh_system_search(&mut self) {
        let Some(overlay) = self.system_search.overlay.as_ref() else {
            return;
        };
        let query = overlay.palette.query().to_owned();
        let scope = overlay.scope;
        let wanted = overlay.wanted.clone();
        let providers = self.system_search_providers(&query, scope);
        let mut registry = system::Registry::new();
        for provider in &providers {
            registry.register(provider);
        }
        let active = self
            .system_search
            .data
            .active_instance_id
            .map(|id| format!("instance:{id}"));
        let groups = registry.query_grouped(
            &(),
            &Query {
                needle: &query,
                scope,
                active_provider_id: active.as_deref(),
                wanted_kinds: &wanted,
            },
            system::PALETTE_ROW_BUDGET,
        );
        let mut entries = system::command_entries(&groups);
        let rows: Vec<_> = groups.into_iter().flat_map(|group| group.results).collect();
        let mut scopes = vec![
            ScopeSegment::english(SearchScope::Workspace, "Workspace".into()),
            ScopeSegment::english(SearchScope::All, "Everywhere".into()),
        ];
        if let Some(tab) = self
            .system_search
            .data
            .tabs
            .iter()
            .find(|tab| Some(tab.instance_id) == self.system_search.data.active_instance_id)
        {
            scopes.insert(
                0,
                ScopeSegment::english(SearchScope::ActiveApp, format!("Only {}", tab.title)),
            );
        }
        let Some(overlay) = self.system_search.overlay.as_mut() else {
            return;
        };
        overlay.filters.configure(
            registry
                .registered_kinds()
                .into_iter()
                .map(KindChip::english)
                .collect(),
            wanted,
            scopes,
            scope,
            FilterText::default(),
        );
        // A layout/wake refresh must preserve keyboard selection and semantic
        // identity when the complete displayed projection is unchanged.
        let previous = overlay.palette.visible_entries();
        let unchanged = previous.len() == rows.len()
            && previous
                .iter()
                .zip(&entries)
                .zip(&rows)
                .all(|((old, new), row)| {
                    self.system_search.rows.get(&old.id) == Some(row)
                        && old.group == new.group
                        && old.group_total == new.group_total
                });
        if unchanged {
            return;
        }
        self.system_search.epoch = self.system_search.epoch.wrapping_add(1);
        self.system_search.rows.clear();
        for (index, (entry, row)) in entries.iter_mut().zip(rows).enumerate() {
            entry.id = format!("system:{}:{index}", self.system_search.epoch);
            if let Some(icon) = row.icon {
                entry.icon = Some(icon);
            }
            self.system_search.rows.insert(entry.id.clone(), row);
        }
        if let Some(overlay) = self.system_search.overlay.as_mut() {
            overlay.palette.set_entries(entries);
        }
    }
    pub(super) fn finish_system_search(&mut self) {
        let Some(overlay) = self.system_search.overlay.as_mut() else {
            return;
        };
        let outcome = overlay.palette.take_outcome();
        let closed = !overlay.palette.is_open();
        if let Some(outcome) = outcome {
            if let Some(row) = self.system_search.rows.get(&outcome.id).cloned() {
                // Re-query current bound providers, so a closed/replaced component,
                // removed content hit or revoked command cannot reuse a painted row.
                let current = self
                    .system_search
                    .overlay
                    .as_ref()
                    .map(|overlay| {
                        self.system_search_providers(overlay.palette.query(), overlay.scope)
                    })
                    .unwrap_or_default();
                if current
                    .iter()
                    .flat_map(|p| &p.rows)
                    .any(|now| now.identity == row.identity && now.intent == row.intent)
                {
                    self.system_search.pending = Some(row.intent);
                }
            }
            self.close_system_search();
        } else if closed {
            self.close_system_search();
        }
    }
    fn apply_system_filter(&mut self, action: FilterAction) {
        let Some(overlay) = self.system_search.overlay.as_mut() else {
            return;
        };
        match action {
            FilterAction::AllKinds => overlay.wanted.clear(),
            FilterAction::ToggleKind(kind) => toggle_kind(&mut overlay.wanted, kind),
            FilterAction::Scope(scope) => overlay.scope = scope,
            FilterAction::More => overlay.filters.more_open = !overlay.filters.more_open,
        }
        self.refresh_system_search();
    }
    pub(super) fn system_search_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } if !self.dispatching_touch_pointer => {
                self.host_pointer_position = Some(*position);
            }
            WindowEvent::CursorLeft { .. } => self.host_pointer_position = None,
            _ => {}
        }
        if self.system_search.overlay.is_none() {
            return false;
        }
        if let WindowEvent::Touch(touch) = event {
            // Use the same touch recognizer as the document driver, while
            // retaining capture through the release that closes this overlay.
            self.dispatching_touch_pointer = true;
            // The overlay has no widget that takes raw per-finger touch.
            for pointer in self.touch_pointer.events(touch, |_| false) {
                let event = match pointer {
                    crate::interaction::TouchPointerEvent::Move(position) => {
                        WindowEvent::CursorMoved {
                            device_id: touch.device_id,
                            position,
                        }
                    }
                    crate::interaction::TouchPointerEvent::Press(button) => {
                        WindowEvent::MouseInput {
                            device_id: touch.device_id,
                            state: ElementState::Pressed,
                            button,
                        }
                    }
                    crate::interaction::TouchPointerEvent::Release(button) => {
                        WindowEvent::MouseInput {
                            device_id: touch.device_id,
                            state: ElementState::Released,
                            button,
                        }
                    }
                    crate::interaction::TouchPointerEvent::Scroll { delta, phase } => {
                        WindowEvent::MouseWheel {
                            device_id: touch.device_id,
                            delta: winit::event::MouseScrollDelta::PixelDelta(delta),
                            phase,
                        }
                    }
                    // Search results do not zoom.
                    crate::interaction::TouchPointerEvent::Pinch { .. } => continue,
                };
                self.system_search_event(&event);
            }
            self.dispatching_touch_pointer = false;
            return true;
        }
        let pointer_position = match event {
            WindowEvent::CursorMoved { position, .. } => {
                Some(self.system_search_position(*position))
            }
            WindowEvent::CursorLeft { .. } => None,
            _ => self.system_search_pointer_position(),
        };
        let Some(overlay) = self.system_search.overlay.as_mut() else {
            return false;
        };
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
        ) || (!self.dispatching_touch_pointer && matches!(event, WindowEvent::MouseInput { .. }))
        {
            overlay.hover = pointer_position;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) && overlay
            .hover
            .is_some_and(|point| !overlay.get_rect().contains(point))
        {
            self.close_system_search();
            self.relayout();
            return true;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && let Some(action) = overlay.hover.and_then(|p| overlay.filters.hit(p))
        {
            self.apply_system_filter(action);
            self.relayout();
            return true;
        }
        let before = overlay.palette.query().to_owned();
        let mut handled = false;
        let mut focus = None;
        overlay.palette.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: self.input_scale,
                modifiers: &self.modifiers,
                state: crate::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::from(0u64),
                taffy: &self.taffy,
            },
        );
        let edited = before != overlay.palette.query();
        if edited {
            self.refresh_system_search();
        }
        self.finish_system_search();
        self.relayout();
        true // This modal surface owns *all* input, including unhandled keys/releases.
    }

    fn system_search_pointer_position(&self) -> Option<Point> {
        self.host_pointer_position
            .map(|position| self.system_search_position(position))
    }

    fn system_search_position(&self, position: winit::dpi::PhysicalPosition<f64>) -> Point {
        Point::new(position.x / self.input_scale, position.y / self.input_scale)
    }
    pub(super) fn draw_system_search(&mut self) {
        let transform = self.capture_draw_transform();
        let Some(overlay) = self.system_search.overlay.as_mut() else {
            return;
        };
        // Measure filter wrapping in a scratch scene before painting the final
        // card so changing reservation never leaves overlapping hit geometry.
        let mut scratch = vello::Scene::new();
        for _ in 0..3 {
            let (desired, height) = overlay.palette.desired_size();
            let width = (desired * 1.5).min((f64::from(self.viewport.0) - 32.0).max(1.0));
            overlay.palette.set_rect(Rect::new(
                (f64::from(self.viewport.0) - width) / 2.0,
                24.0,
                (f64::from(self.viewport.0) + width) / 2.0,
                24.0 + height,
            ));
            overlay.palette.draw(
                &mut scratch,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
            );
            let rows = overlay.filters.draw(
                &mut PaintCtx::new(&mut scratch, Affine::IDENTITY, &mut self.text, &self.theme),
                FilterStripSlot {
                    band: overlay.palette.filter_strip_rect(),
                    row_height: overlay.palette.filter_strip_row_height(),
                    reserved_rows: overlay.palette.host_filter_strip_rows(),
                },
                overlay.hover,
            );
            if !overlay.palette.set_host_filter_strip_rows(rows) {
                break;
            }
        }
        overlay.draw(
            &mut self.scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }
    pub(super) fn system_search_inspection(&self) -> Option<InspectionNode> {
        let overlay = self.system_search.overlay.as_ref()?;
        let rect = overlay.get_rect();
        Some(InspectionNode {
            path: "system-search".into(),
            widget: "SystemSearch".into(),
            test_id: Some("system-search".into()),
            focusable: true,
            rect: [rect.x0, rect.y0, rect.width(), rect.height()],
            child_clip: None,
            layout: None,
            semantics: overlay.accessibility(),
            children: vec![],
        })
    }
    pub(super) fn perform_system_search_semantic(
        &mut self,
        name: &str,
        target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> bool {
        if name != "system-search" {
            return false;
        }
        let Some(target) = target else {
            return false;
        };
        let Some(overlay) = self.system_search.overlay.as_ref() else {
            return false;
        };
        let filters = overlay.filters.accessibility();
        for (i, group) in filters.children.iter().enumerate() {
            for (j, node) in group.children.iter().enumerate() {
                if node.action_target.as_deref() == Some(target) {
                    if !matches!(
                        action,
                        crate::a11y::Action::Activate | crate::a11y::Action::Focus
                    ) {
                        return false;
                    }
                    if matches!(action, crate::a11y::Action::Activate)
                        && let Some(filter) = overlay.filters.action_for_path(&[i, j])
                    {
                        self.apply_system_filter(filter);
                    }
                    return true;
                }
            }
        }
        if target == "command-palette:query"
            && let crate::a11y::Action::SetValue(value) = action
        {
            if !system::query_input_is_bounded(value) {
                return false;
            }
            if let Some(overlay) = self.system_search.overlay.as_mut() {
                overlay.palette.set_query(value);
            }
            self.refresh_system_search();
            return true;
        }
        self.system_search.overlay.as_mut().is_some_and(|overlay| {
            overlay.palette.perform_semantic_action(
                target,
                action,
                &mut crate::widget::Edits::lend(&mut self.state.doc),
                &self.state.root,
            )
        })
    }
    // The overlay has its own semantic owner outside the retained document's
    // Taffy arena. This disjoint owner is used only by the pure AccessKit bridge;
    // it is never dispatched through or inserted in the document node map.
    fn system_search_access_map(&self) -> HashMap<NodeId, *mut dyn Widget> {
        let mut map = HashMap::new();
        if let Some(overlay) = &self.system_search.overlay {
            map.insert(
                NodeId::from((1u64 << 47) - 1),
                overlay as &dyn Widget as *const dyn Widget as *mut dyn Widget,
            );
        }
        map
    }
    pub(super) fn global_search_pending(&self) -> bool {
        self.system_search.pending.is_some()
    }
    pub(super) fn system_search_presentation(&self) -> Option<&dyn Widget> {
        self.system_search
            .overlay
            .as_ref()
            .map(|overlay| overlay as &dyn Widget)
    }
    pub(super) fn system_search_ime(&self) -> Option<crate::ime::ImeWindowState> {
        self.system_search
            .overlay
            .as_ref()
            .and_then(|o| crate::ime::ImeWindowState::from_widget(&o.palette))
    }
    pub(super) fn system_search_a11y(&self) -> Option<accesskit::TreeUpdate> {
        let overlay = self.system_search.overlay.as_ref()?;
        Some(crate::accesskit_bridge::build_tree_update(
            overlay,
            &self.system_search_access_map(),
            Some(NodeId::from((1u64 << 47) - 1)),
            1.0,
        ))
    }
    pub(super) fn perform_system_search_a11y(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> bool {
        let Some(overlay) = &self.system_search.overlay else {
            return false;
        };
        let resolved = crate::accesskit_bridge::resolve_node_id(
            overlay,
            &self.system_search_access_map(),
            target,
        );
        if let Some(crate::accesskit_bridge::A11yTarget::Synthetic {
            semantic_target: Some(target),
            ..
        }) = resolved
        {
            self.perform_system_search_semantic("system-search", Some(&target), action)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
