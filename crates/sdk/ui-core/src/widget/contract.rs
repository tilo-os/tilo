//! The [`Widget`] contract: what one node of a toolkit tree implements.

use super::{
    Affine, AppModel, Disclosing, Edits, EventCtx, HashMap, Instant, LayoutInfo, ManualPolicy,
    ModelPath, NativeViewLifecycle, NodeId, ObjId, PhysicalInput, Placement, PresenceCtx,
    PrimaryScrollAction, PrimaryScrollRegion, PrimaryScrollResult, Remount, Scene, StructureChange,
    TaffyTree, TextRenderer, Theme, WindowEvent, capability, defaults, router,
};

pub trait Widget {
    /// Concrete identity for host-side frame maps that must distinguish a wrapper from its first field
    /// even when they share a data address.
    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Structural chrome can opt out of the component binding it would otherwise inherit from its
    /// parent.
    #[doc(hidden)]
    fn resets_binding_inheritance(&self) -> bool {
        false
    }
    /// Whether the window this widget fills is cleared to the theme's content colour behind it.
    #[doc(hidden)]
    fn paints_ground(&self) -> bool {
        true
    }
    /// Drawn live content whose input and bound dispatch a framework surface suspends: it stays visible
    /// and keeps its state, but takes no events and opens no records.
    #[doc(hidden)]
    fn inactive_bound_dispatch_boundary(&self) -> Option<NodeId> {
        None
    }

    /// Read-only state evidence for opt-in scheduled native input diagnostics.
    fn native_input_snapshot(
        &self,
        _state: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        None
    }

    /// Opt in only when this wheel event changes scroll geometry and nothing else: no document edit,
    /// focus or selection, action availability, resource declaration, pending command or work that
    /// requires `sync`.
    fn view_only_scroll(&self, _event: &WindowEvent) -> bool {
        false
    }

    /// Explicit app-owned region; never search secondary children implicitly.
    fn primary_vertical_scroll(&self) -> Option<PrimaryScrollRegion> {
        None
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        _action: PrimaryScrollAction,
    ) -> PrimaryScrollResult {
        PrimaryScrollResult::Unsupported
    }

    /// Runtime binding inherited by descendants in a matching layout topology.
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        None
    }

    /// Whether this widget introduces a document owner distinct from the runner's outer State.
    fn has_separate_document_owner(&self) -> bool {
        self.callback_binding().is_some()
    }

    /// Opaque actual custody for framework History source registration.
    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        self.callback_binding()
            .map(crate::history_mode::HistoryOwner::component)
    }

    /// Records this widget keeps open beside the document it shows, each synced on its own route: a
    /// framework app's records in the person's own workspace ([`crate::record::person`]).
    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        Vec::new()
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "opaque toolkit-only request prevents a public root/registry decoder loan"
    )]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        defaults::visit_components(self.children_mut(), request)
    }

    /// SDK model service visits retained owners independently of visible children.
    #[doc(hidden)]
    fn visit_retained_components(
        &mut self,
        request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        self.visit_components(request.inner())
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners(
        &self,
        request: &mut crate::bound_dispatch::local_target::ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        request.visit(self)
    }

    /// Answer a request for a capability this widget takes part in (see [`capability`]).
    fn provide<'a>(&'a self, _request: &mut capability::Request<'a, '_>) {}

    /// Exclusive counterpart to [`Widget::provide`].
    fn provide_mut<'a>(&'a mut self, _request: &mut capability::RequestMut<'a, '_>) {}

    /// Attach native child views through this owner's retained lifecycle topology.
    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.mount_native_view(host);
        }
        defaults::mount_native_views(self.children_mut(), host);
    }

    /// Tear native children down while their parent window still exists.
    fn unmount_native_views(&mut self) {
        defaults::unmount_native_views(self.children_mut());
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.unmount_native_view();
        }
    }

    /// Begin a native-view frame through this owner's retained lifecycle topology.
    fn native_view_frame_start(&mut self) {
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.native_view_frame_start();
        }
        defaults::native_view_frame_start(self.children_mut());
    }

    /// Receive host-selected platform conventions.
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        defaults::set_platform_conventions(self.children_mut(), conventions);
    }

    /// Receive the host's current input capabilities.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        defaults::set_interaction_environment(self.children_mut(), environment);
    }

    /// Tell this widget whether it stands in a window bar.
    fn set_window_bar_control(&mut self, in_bar: bool) {
        defaults::set_window_bar_control(self.children_mut(), in_bar);
    }

    /// Pull model state into this widget before layout.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        defaults::sync(self.children_mut(), state, path);
    }

    /// Notice, between frames, state outside the document (a chrome revision, a momentum scroll still
    /// ticking) and report whether anything changed.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        defaults::update(self.children_mut(), state, path)
    }

    /// Apply, after the sync pass, what the widget took up there and must write: a worker's answer, a
    /// chooser's file, an event a child queued.
    fn settle(&mut self, edits: &mut Edits<'_>, path: &ObjId) {
        defaults::settle(self.children_mut(), edits, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId;

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size::ZERO
    }
    /// Owned manual policy descriptor; the host invokes it outside widget borrows.
    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        None
    }

    /// Read-only resolved decisions for widgets with an explicit layout policy.
    fn layout_info(&self) -> Option<LayoutInfo> {
        None
    }

    /// Adjust this node's style using its allocated size, before final geometry is applied.
    fn adjust_layout_style(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        false
    }

    /// Take the geometry layout gave this node.
    fn apply_layout(
        &mut self,
        _taffy: &TaffyTree<()>,
        _node: NodeId,
        _origin: vello::kurbo::Point,
    ) {
    }
    /// Paint into `scene`. The default paints nothing.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    /// Observe this app's ordinary foreground physical input before framework shortcuts consume it.
    fn observe_physical_input(&mut self, _input: PhysicalInput) {}

    /// Handle one input event with the runtime services for this retained node.
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}
    fn children(&self) -> Vec<&dyn Widget> {
        vec![]
    }
    /// Allocation-free read-only child traversal.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        defaults::for_each_child(self.children(), visitor);
    }
    /// The children of [`Self::for_each_child`] that may paint inside the [`layout_clip`] in effect:
    /// what a scroll view's damage and copy plan need.
    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.for_each_child(visitor);
    }
    /// Children this widget keeps in the retained tree but does not route input to: a subtree it does
    /// not own and answers the person's events for itself, like a host drawing another widget's card
    /// over the windows.
    fn for_each_hosted_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) {}
    /// Every child this widget's [`Self::build_layout`] gives a layout node, also those
    /// [`Self::for_each_child`] keeps from routing (a hidden pane, a title, a spacer).
    fn for_each_layout_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        false
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![]
    }

    /// The readable column this widget keeps what it shows in, when it is one: a page's measure, a list
    /// that reads its rows at it.
    fn readable_column(&self) -> Option<crate::widget::arrange::Length> {
        None
    }

    fn name(&self) -> &str;
    /// Stable id for the e2e test harness; `None` falls back to a path-based key.
    fn test_id(&self) -> Option<&str> {
        None
    }

    /// Downcast to the concrete widget, for hosts and tests that reach a known widget through `dyn
    /// Widget`.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Whether this widget is the one at `address`, or stands in the tree for it: a [`Decorator`]
    /// stands for every widget it wraps, so a scroller drawn inside one is found where the decorator is
    /// ([`crate::scroll_copy::offer`]).
    fn stands_for(&self, address: *const ()) -> bool {
        std::ptr::from_ref(self).cast::<()>() == address
    }

    /// Context that should remain visible with a focused descendant, such as a form control and its
    /// validation message.
    fn focus_reveal_context(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Show `rect` of the descendant `target` (its own rect, or the part of it to show, as laid out)
    /// inside every enclosing viewport.
    fn reveal_descendant(&mut self, target: *const dyn Widget, rect: vello::kurbo::Rect) -> bool {
        defaults::reveal_descendant(self.children_mut(), target, rect)
    }

    /// Named hit rects for widgets that paint several affordances into one rect instead of composing
    /// children.
    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    /// The rectangle this widget occupies.
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }

    /// Absolute logical clip applied to ordinary children during painting and pointer routing.
    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Render this widget as HTML, next to its paint, for a host that shows the window as a page
    /// ([`crate::composition`]): the element it is, what it shows of its own, and where its children
    /// go.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.from_layout();
    }

    /// The words this widget shows as the heading of the page it belongs to, when it is the page's own
    /// title: a large-title text the content edits or sets at the top of a document.
    fn own_page_title(&self) -> Option<String> {
        None
    }

    /// Whether this is an item of a list opened where its row stands, among the list's other rows: what
    /// it shows is that item's, never the page's, so its title is not the page's own.
    fn is_item_opened_in_place(&self) -> bool {
        false
    }

    /// Whether `own_page_title` is a heading a screen reader reads as the page's name, or only the
    /// field the person types it in, which names itself.
    fn page_title_is_heading(&self) -> bool {
        false
    }

    /// Offsets along `axis` that this widget, as a scroll view's content, offers it to settle on,
    /// measured from its own leading edge: a board's lanes ([`crate::widget::Board`]).
    fn snap_points(&self, axis: crate::widget::ScrollAxis, out: &mut Vec<f32>) {
        let _ = (axis, out);
    }

    /// One-shot request to open the host's action palette directly at a typed resource action.
    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        defaults::take_action_palette_request(self.children_mut())
    }

    /// One-shot request to open the host's search palette, the same one the platform search shortcut
    /// opens.
    fn take_search_request(&mut self) -> bool {
        defaults::take_search_request(self.children_mut())
    }

    /// `point` is logical coords, same space as [`Widget::get_rect`]; `None` defers to the default
    /// arrow.
    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    /// Opt out of touch-to-pointer emulation for contacts that begin at `point` (logical coords, same
    /// space as [`Widget::get_rect`]).
    fn claims_raw_touch(&self, _point: vello::kurbo::Point) -> bool {
        false
    }

    /// What a one-finger contact that begins at `point` (logical coords) means here, when a drag moves
    /// something rather than scrolling.
    fn touch_claim(&self, _point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        crate::interaction::TouchClaim::Pointer
    }

    /// Revealed after a dwell over the widget's rect, via the toolkit's one tooltip mechanism
    /// ([`crate::widget::tooltip::TooltipHost`]), hit-tested like [`cursor_shape`](Self::cursor_shape)
    /// so containers need not forward it.
    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        None
    }

    /// Which side of the control its tooltip goes: below by default, beside it for a control in a
    /// vertical rail.
    fn tooltip_side(&self) -> crate::widget::tooltip::TooltipSide {
        crate::widget::tooltip::TooltipSide::Below
    }

    fn tooltip_target_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<(vello::kurbo::Rect, String)> {
        let rect = self.get_rect();
        if !rect.contains(point) {
            return None;
        }
        self.tooltip()
            .filter(|tooltip| !tooltip.is_empty())
            .map(|tooltip| (rect, tooltip.into_owned()))
    }

    fn is_focusable(&self) -> bool {
        false
    }
    /// Preferred control when its app is explicitly activated without a valid saved control.
    fn is_default_keyboard_focus(&self) -> bool {
        false
    }

    /// Exact app instance whose non-text shortcuts this subtree owns when activation has no control
    /// target.
    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        self.callback_binding().map(|binding| binding.instance_id)
    }

    fn set_focus(&mut self, _focused: bool) {}

    /// The window holding this content fills its display (a maximized window): it has nothing beside it
    /// to round its corners against or to set it apart from, so content that frames or clips itself to
    /// the window's shape draws edge to edge instead.
    fn set_full_bleed(&mut self, _full_bleed: bool) {}

    /// The form's validation verdict for this control, set by [`Field::with_error`]: an invalid control
    /// edges itself in danger ink and reports `invalid` to assistive technology.
    fn set_invalid(&mut self, _invalid: bool) {}

    /// An explicit runner focus request, after resolving the actual keyboard owner.
    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        defaults::focus_requested(self.children_mut(), target, tree);
    }

    fn is_focused(&self) -> bool {
        false
    }

    /// The box a context menu opened from the keyboard stands against: the item that has the keyboard,
    /// so the menu opens under it rather than under the whole widget.
    fn context_menu_anchor(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Whether a focused widget owns Tab instead of the host's focus traversal.
    fn wants_tab_input(&self) -> bool {
        false
    }

    /// Caret rectangle for the platform input method, in window-logical coordinates.
    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// While any widget answers `true`, the host restricts Tab/directional focus to this subtree
    /// (wrapping at the ends) and restores the prior focus once it goes back to `false`.
    fn traps_focus(&self) -> bool {
        false
    }

    /// Whether Tab and directional focus travel stay inside this subtree while no trap holds focus: the
    /// focused desktop window, so keyboard travel cycles through the window the person is in (its bar
    /// and its app) and never wanders onto the other windows or the desktop's chrome (top bar, menus),
    /// which the keyboard reaches through its own shortcuts.
    fn scopes_keyboard_focus(&self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it): the app instance the person just brought forward (a window
    /// they opened, clicked or switched to), whose keyboard focus the host restores, or puts on its
    /// default control, so what they type next reaches it.
    fn take_app_activation(&mut self) -> Option<crate::application::InstanceId> {
        defaults::take_app_activation(self.children_mut())
    }

    /// One-shot (reading MUST clear it): the test id of the control this subtree asks to take the
    /// keyboard once the runner laid it out, such as the first field of a view a person's press just
    /// opened, so what they type next lands there.
    fn take_focus_request(&mut self) -> Option<String> {
        defaults::take_focus_request(self.children_mut())
    }

    /// Role, name/description/value, and [`AccessState`](crate::a11y::AccessState).
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        None
    }

    /// [`Self::accessibility`] with its synthetic children shared, for a widget that keeps them between
    /// frames: each child it did not change is handed over as the very `Arc` of the last frame, which
    /// the semantic tree takes as unchanged without copying or comparing it.
    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        None
    }

    /// Whether [`Self::accessibility`] reports this subtree hidden, for a host that asks it of every
    /// node to prune focus traversal.
    fn accessibility_hidden(&self) -> bool {
        self.accessibility_root()
            .is_some_and(|node| node.state.hidden)
    }

    /// [`Self::accessibility`]'s own node with its `children` left empty: its role, identity
    /// (`semantic_key`) and state.
    fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
        self.accessibility().map(|mut node| {
            node.children.clear();
            node
        })
    }

    /// Geometry for an immediate-mode semantic child from
    /// [`AccessNode::children`](crate::a11y::AccessNode::children).
    fn synthetic_accessibility_rect(&self, _path: &[usize]) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Move this widget's own immediate-mode focus to a synthetic semantic child.
    fn focus_synthetic_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a synthetic child returned by
    /// [`accessibility`](Widget::accessibility).
    fn accessibility_child_parent(&self, _child_index: usize) -> Option<usize> {
        None
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a nested path of synthetic
    /// children returned by [`accessibility`](Widget::accessibility).
    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        self.accessibility_child_parent(child_index)
            .map(|parent| vec![parent])
    }

    /// Whether one retained [`children`](Widget::children) entry is already represented in this
    /// widget's own synthetic [`accessibility`](Widget::accessibility) tree, so the bridge must not
    /// export it a second time.
    fn accessibility_represents_child(&self, _child_index: usize) -> bool {
        false
    }

    /// Whether this widget lays one retained [`children`](Widget::children) entry out itself (an
    /// [`Embedded`](crate::widget::Embedded) part of a painted page) rather than through the window's
    /// layout, which then gives it no node.
    fn lays_out_child(&self, _child_index: usize) -> bool {
        false
    }

    /// What describes one retained [`children`](Widget::children) entry to assistive technology, when
    /// this widget holds the words for it: a row's secondary line is its trailing switch's description.
    fn accessibility_child_description(&self, _child_index: usize) -> Option<String> {
        None
    }

    /// What names one retained [`children`](Widget::children) entry to assistive technology, when this
    /// widget holds the words for it: a row's caption names the switch, slider or chooser it holds.
    fn accessibility_child_name(&self, _child_index: usize) -> Option<String> {
        None
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) on this
    /// widget, or on one of its **synthetic** accessibility nodes. May mutate
    /// the CRDT like [`handle_event`](Widget::handle_event).
    ///
    /// `key_path` names the target relative to this widget; **empty** means
    /// the widget itself. A widget that paints its own contents (a timeline of
    /// clips, a spreadsheet grid, a table of rows) has no child widgets to
    /// receive an action, so it describes its structure through
    /// [`AccessNode::children`](crate::a11y::AccessNode::children) instead, and
    /// the host resolves an activated node back to `(this widget, key path)`:
    /// one element per level of nesting — `[row]` for a table row, `[row,
    /// cell]` for a cell inside it. Each element is the node's
    /// [`key`](crate::a11y::AccessNode::key), or its ordinal among its siblings
    /// when it declared none.
    ///
    /// For an element keyed by identity rather than position, compare the
    /// incoming key against [`hash_key`](crate::a11y::hash_key) of your own id:
    ///
    /// ```ignore
    /// fn perform_action(&mut self, action: &Action, key_path: &[u64], ..) -> bool {
    ///     let Some(&clip_key) = key_path.first() else { return false };
    ///     let Some(clip) = self.clips.iter().find(|c| hash_key(&c.id) == clip_key) else {
    ///         return false;   // stale reference — do nothing, never guess
    ///     };
    ///     ...
    /// }
    /// ```
    ///
    /// Returns `true` if the widget consumed the action. Default `false` ("not
    /// handled") falls back to generic dispatch (`Focus` moves keyboard focus,
    /// `Activate` synthesises a click on the whole widget); widgets with
    /// richer semantics (slider inc/dec, combo expand/collapse, per-element
    /// canvases) override to handle those verbs precisely.
    fn perform_action(
        &mut self,
        _action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        false
    }

    /// Handle an action addressed to a synthetic accessibility child created by this widget.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let Some(key_path) = defaults::key_action_path(target) else {
            return false;
        };
        self.perform_action(action, &key_path, state, path)
    }

    /// A disabled (`false`) widget is excluded from focus traversal, must reject input in
    /// `handle_event`, and renders dimmed (see [`DISABLED_ALPHA`]).
    fn is_enabled(&self) -> bool {
        true
    }

    /// This field needs a host that consumes deferred receipts.
    fn ime_requires_deferred_ack(&self) -> bool {
        defaults::ime_requires_deferred_ack(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Ordinary input-method commits (a composing keyboard IME, the on-screen keyboard) cannot reach
    /// this field: its text arrives only through deferred receipts (a browser page field), so hosts
    /// keep standard text input closed outside a dictation session.
    fn ime_refuses_ordinary_commits(&self) -> bool {
        defaults::ime_refuses_ordinary_commits(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Begin one bounded deferred session after explicit host consent, preserving the existing
    /// original-field fingerprint.
    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::start_deferred_text(self.children_mut(), caret)
    }

    /// A private asynchronous final.
    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::commit_deferred_text(self.children_mut(), caret, text)
    }

    /// The focused widget wants platform text input (Wayland `text-input-v3`, native IMEs elsewhere).
    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        None
    }

    /// Whether the original focused editor accepts line feeds as text.
    fn ime_multiline(&self) -> bool {
        let Some(caret) = self.ime_cursor_rect() else {
            return false;
        };
        defaults::ime_multiline(caret, &|visit| self.for_each_child(visit))
    }

    /// Opaque local invalidation of deferred input, including selection edits that leave the caret
    /// rectangle unchanged.
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let caret = self.ime_cursor_rect()?;
        defaults::ime_edit_fingerprint(caret, key, &|visit| self.for_each_child(visit), &|| {
            self.accessibility()
        })
    }

    /// Default `true`; false only for a widget standing in for a window's content whose first frame has
    /// not drawn.
    fn presented(&self) -> bool {
        true
    }
    /// Where this content's first frame comes from while it has not [`presented`](Widget::presented):
    /// this device (an app starting here), another device or a host.
    fn arrives_from(&self) -> crate::arriving::Source {
        crate::arriving::Source::Local
    }
    /// How much of what this content's first frame waits on has come, when that measures itself (its
    /// app downloaded before it starts).
    fn arrival_progress(&self) -> Option<crate::arriving::Progress> {
        None
    }
    /// The shape this content declares for the placeholder standing in for its first frame.
    fn arrival_shape(&self) -> crate::widget::SkeletonShape {
        crate::widget::SkeletonShape::Ground
    }

    /// `(caret_byte, text)`, byte offset on a char boundary.
    fn text_review(&self) -> Option<(usize, String)> {
        None
    }

    /// Delivers an out-of-band string payload from [`crate::app::UserEvent::External`] (e.g.
    fn on_external_event(
        &mut self,
        _payload: &str,
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) {
    }

    /// Global back/forward gesture (swipe/edge-pull), recognised by
    /// [`crate::gesture::NavGestureRecognizer`] and delivered to the whole tree; return `true` once
    /// consumed.
    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        defaults::on_nav_gesture(self.children_mut(), dir, state, path)
    }

    /// Which directions [`Self::on_nav_gesture`] would consume now.
    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        defaults::nav_availability(self.children())
    }

    /// One-shot (reading MUST clear it) signal to move focus to this subtree's first focusable widget
    /// after the next rebuild.
    fn take_focus_reset(&mut self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it) request to change the window's accessibility preferences,
    /// raised by a control the person used.
    fn take_accessibility_request(&mut self) -> Option<crate::a11y::AccessibilityRequest> {
        defaults::take_accessibility_request(self.children_mut())
    }

    /// Route history when this widget, or one it contains, is a [`Router`].
    fn dev_route(&self) -> Option<router::RouteSnapshot> {
        defaults::dev_route(self.children())
    }

    /// Put `route` back. Returns whether a router in this subtree accepted it.
    fn restore_dev_route(&mut self, route: &router::RouteSnapshot) -> bool {
        defaults::restore_dev_route(self.children_mut(), route)
    }

    /// Theme/high-contrast/reduced-motion/scale, polled under [`crate::app::App::appearance_reconcile`]
    /// — how a settings pane re-themes the running app live without a back-channel.
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        None
    }

    /// The narrowest this window still works at, in logical pixels: the minimum of its compact layout
    /// (a `NavShell` answers with one column at a time).
    fn minimum_window_width(&self) -> Option<f64> {
        None
    }

    /// The whole size this window wants, in logical pixels, when the app is the window's only content
    /// and knows how big it must be: the on-screen keyboard parked to a strip.
    fn preferred_window_size(&self) -> Option<(f64, f64)> {
        None
    }

    /// Apply the effective reduced-motion preference before the host asks this subtree whether it needs
    /// another animation frame.
    fn set_motion_reduced(&mut self, reduced: bool) {
        defaults::set_motion_reduced(self.children_mut(), reduced);
    }

    fn needs_idle_sync(&self) -> bool {
        false
    }

    fn wants_pointer_capture(&self) -> bool {
        false
    }

    /// While this widget is a band on its way in or out (`motion::Disclosure`: an in-window banner
    /// arriving or leaving), or a content-sized container one moves in, how far what follows it is
    /// drawn from where it is laid out.
    fn disclosing(&self) -> Option<Disclosing> {
        None
    }

    /// Where a band on its way ([`Disclosing::Band`]) stands while it is out of the layout: its slot's
    /// top and width (`slot`'s height is zero), set by its container after each layout.
    fn place_disclosing(&mut self, _slot: vello::kurbo::Rect) {}

    /// A band that comes and goes (an in-window banner): when its view stops building it, its column
    /// draws it leaving (`widget::Disclosing`).
    fn is_band(&self) -> bool {
        false
    }

    /// Where a band stands in its column, set by the column after each layout: what it leaves when
    /// dropped, so the column a rebuild makes can draw it leaving.
    fn note_placement(&mut self, _placement: Placement) {}

    /// Why this widget itself asks for animation frames, for diagnostics that name a desktop which
    /// keeps painting.
    fn animation_detail(&self) -> Option<String> {
        None
    }

    /// `App` polls this in `Event::AboutToWait`; without it a curve would freeze until an unrelated
    /// event woke it.
    fn needs_animation_frame(&self) -> bool {
        defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    /// Earliest wall-clock deadline at which this subtree needs a sync or redraw with no input: the
    /// loop sleeps until then (not animation).
    fn next_wake_deadline(&self) -> Option<Instant> {
        defaults::next_wake_deadline(&|visit| self.for_each_child(visit))
    }

    /// Until when the frame that would show this subtree waits for data it reads from this device
    /// ([`crate::arriving`]): a local read that has not answered and is still inside its frame budget.
    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        defaults::holds_frame_until(&|visit| self.for_each_child(visit), now)
    }

    /// A deadline this subtree asked for ([`Self::next_wake_deadline`]) is due at `now`: do the work it
    /// was for, and say whether anything changed.
    fn serve_deadline(&mut self, _now: Instant) -> bool {
        true
    }

    /// `Some(rect)` when this subtree holds an *open* modal, its panel box.
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        defaults::modal_capture(&|visit| self.for_each_child(visit))
    }

    /// Close every modal this subtree presents, as its own way out does (a host whose surface is a
    /// modal sheet calls it when the shell takes the sheet down).
    fn dismiss_presented_modal(&mut self) -> bool {
        defaults::dismiss_presented_modal(self.children_mut())
    }

    /// Whether this subtree presents a modal still drawn (arriving, open or leaving): the counterpart
    /// of [`Widget::dismiss_presented_modal`].
    fn presents_modal(&self) -> bool {
        defaults::presents_modal(self.modal_capture(), &|visit| self.for_each_child(visit))
    }

    /// True if this subtree's structural composition changed since the last frame; reading clears it.
    fn take_structure_dirty(&mut self) -> bool {
        defaults::take_structure_dirty(self.children_mut())
    }

    /// [`Self::take_structure_dirty`], saying whether the frame's damage can still describe the rebuild
    /// ([`StructureChange::Described`]).
    fn take_structure_change(&mut self) -> StructureChange {
        StructureChange::of(self.take_structure_dirty())
    }

    /// This widget's own layout node while the widgets under it are laid out again in place, in it, by
    /// the coming [`Self::remount_children`]: it then names none of them through
    /// [`Self::for_each_layout_child`], and the host lets go of every node under this one, as their
    /// widgets may be gone.
    #[doc(hidden)]
    fn relaid_layout_node(&self) -> Option<NodeId> {
        None
    }

    /// Carry out a [`StructureChange::Children`] this widget or one under it reported, in the retained
    /// layout tree, through `remount` (see [`remount`]).
    fn remount_children(&mut self, remount: &mut Remount<'_>) -> bool {
        self.children_mut()
            .into_iter()
            .all(|child| child.remount_children(remount))
    }

    /// Work already requested by this view whose completion will replace a placeholder.
    fn has_pending_visual_work(&self) -> bool {
        defaults::has_pending_visual_work(&|visit| self.for_each_child(visit))
    }

    /// Temporary feedback currently presented, such as a save acknowledgement.
    fn has_transient_feedback(&self) -> bool {
        defaults::has_transient_feedback(self.children())
    }

    /// Default `false` (clean).
    fn needs_redraw(&self) -> bool {
        defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }

    /// Logical-pixel bounds whose visual output differs from the last frame.
    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if self.needs_redraw() || self.needs_animation_frame() {
            regions.push(self.get_rect());
        }
    }

    /// Whether, after this frame's draw, this widget's own [`Self::collect_damage`] covers every pixel
    /// a relayout changed by moving or resizing its layout box: a host that repaints where moved
    /// widgets were and are may then leave this widget's box to it (its children's boxes still count).
    fn damage_covers_own_box(&self) -> bool {
        false
    }

    /// Called after this widget's scene lands in the composed frame, to clear a `dirty` flag: paint
    /// only, never layout dirt ([`Self::is_layout_dirty`]).
    fn mark_drawn(&mut self) {
        defaults::mark_drawn(self.children_mut());
    }

    /// Rest this widget's children after a frame drew them, for a widget that keeps its own dirty flag:
    /// it clears the flag itself, then calls this.
    fn mark_children_drawn(&mut self) {
        defaults::mark_drawn(self.children_mut());
    }

    /// Release retained encoded-scene storage in this subtree without marking its visual/model state
    /// dirty.
    fn release_scene_cache(&mut self) {
        defaults::release_scene_cache(self.children_mut());
    }

    /// Default `true`; widgets with a fixed intrinsic size should override to `false` unless a
    /// size-affecting mutation landed.
    fn is_layout_dirty(&self) -> bool {
        defaults::is_layout_dirty(&|visit| self.for_each_child(visit))
    }

    /// Whether this widget composes its children from the size it was last allocated (it reads its own
    /// rect while building its layout: a compact layout under a width, a count of columns, an inset
    /// from a drawer).
    fn composition_follows_allocation(&self) -> bool {
        false
    }

    /// Whether a `layout_dirty` frame needs taffy to **re-solve** sizes (`compute_layout`) rather than
    /// just reposition already-solved boxes.
    fn layout_needs_measure(&self) -> bool {
        defaults::layout_needs_measure(&|visit| self.for_each_child(visit))
            .unwrap_or_else(|| self.is_layout_dirty())
    }

    /// Meet the measure request [`Self::layout_needs_measure`] just reported: the solver asking is
    /// measuring this widget's node now, so whatever its geometry depended on is read in this pass.
    fn layout_measured(&mut self) {}

    /// Everything this widget paints, children included; [`draw_child`] skips the draw when it misses
    /// the viewport.
    fn paint_bounds(&self) -> vello::kurbo::Rect {
        self.get_rect()
    }

    /// **Top-layer pass**, run once after the whole `draw` recursion, so anything emitted here
    /// composites above every sibling regardless of tree order — the toolkit's overlay mechanism for
    /// content that escapes its layout box (an open [`Dropdown`], a popover).
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        defaults::draw_overlay(
            self.children_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }
}
