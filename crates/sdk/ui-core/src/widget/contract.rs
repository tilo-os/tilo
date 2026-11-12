//! The [`Widget`] contract: what one node of a toolkit tree implements.
//!
//! Every widget is a node the host measures, paints and sends events to; the
//! default methods here are how a widget opts into more than that. The host
//! side of the same tree (layout, drawing, resources, events) stays in
//! `super`.

use super::{
    Affine, AppModel, Disclosing, Edits, EventCtx, HashMap, Instant, LayoutInfo, ManualPolicy,
    ModelPath, NativeViewLifecycle, NodeId, ObjId, PhysicalInput, Placement, PresenceCtx,
    PrimaryScrollAction, PrimaryScrollRegion, PrimaryScrollResult, Remount, Scene, StructureChange,
    TaffyTree, TextRenderer, Theme, WindowEvent, capability, defaults, router,
};

pub trait Widget {
    /// Concrete identity for host-side frame maps that must distinguish a
    /// wrapper from its first field even when they share a data address.
    /// Unlike trait-object pointer equality, this does not depend on the
    /// compiler reusing one vtable instance across codegen units.
    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Structural chrome can opt out of the component binding it would
    /// otherwise inherit from its parent.
    #[doc(hidden)]
    fn resets_binding_inheritance(&self) -> bool {
        false
    }
    /// Whether the window this widget fills is cleared to the theme's content
    /// colour behind it. One that stands on nothing answers `false`: its
    /// window is cleared to transparent, and whatever its view does not paint
    /// shows what lies under the window.
    #[doc(hidden)]
    fn paints_ground(&self) -> bool {
        true
    }
    /// Drawn live content whose input and bound dispatch a framework surface
    /// suspends: it stays visible and keeps its state, but takes no events and
    /// opens no records.
    #[doc(hidden)]
    fn inactive_bound_dispatch_boundary(&self) -> Option<NodeId> {
        None
    }

    /// Read-only state evidence for opt-in scheduled native input diagnostics.
    /// Called only around diagnostic inputs, never during ordinary input or paint.
    /// Keep this inexpensive and root-local; snapshots may be logged verbatim.
    /// This verifies widget state, not rendered pixels or display presentation.
    fn native_input_snapshot(
        &self,
        _state: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        None
    }

    /// Opt in only when this wheel event changes scroll geometry and nothing else: no document
    /// edit, focus or selection, action availability, resource declaration, pending command or work
    /// that requires `sync`. The runner still lays out, paints and refreshes accessibility
    /// geometry; unknown handlers stay on the full path.
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

    /// Whether this widget introduces a document owner distinct from the
    /// runner's outer State. This is a compatibility check, never identity
    /// or authority for history access or restore.
    ///
    /// A [`crate::record::capability::RecordScoped::record_intent`] alone does not qualify: the runner (or the
    /// enclosing [`TileHost`], which reports its own binding) checks that
    /// record out as the very State this widget reads.
    fn has_separate_document_owner(&self) -> bool {
        self.callback_binding().is_some()
    }

    /// Opaque actual custody for framework History source registration.
    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        self.callback_binding()
            .map(crate::history_mode::HistoryOwner::component)
    }

    /// Records this widget keeps open beside the document it shows, each
    /// synced on its own route: a framework app's records in the person's
    /// own workspace ([`crate::record::person`]). They take no part in this
    /// widget's history or controls.
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
    /// The default preserves the original custom component visitor exactly.
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

    /// Answer a request for a capability this widget takes part in (see
    /// [`capability`]). Declare capabilities with [`crate::provides!`].
    fn provide<'a>(&'a self, _request: &mut capability::Request<'a, '_>) {}

    /// Exclusive counterpart to [`Widget::provide`].
    fn provide_mut<'a>(&'a mut self, _request: &mut capability::RequestMut<'a, '_>) {}

    /// Attach native child views through this owner's retained lifecycle topology. Presentation
    /// containers with inactive or hidden live descendants override this traversal so lifecycle
    /// ownership stays independent of drawing and input visibility.
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

    /// Receive host-selected platform conventions. The default propagates to
    /// retained children, keeping target-OS branches out of app code.
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        defaults::set_platform_conventions(self.children_mut(), conventions);
    }

    /// Receive the host's current input capabilities. The default propagates
    /// through retained containers, so app code does not need device branches.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        defaults::set_interaction_environment(self.children_mut(), environment);
    }

    /// Tell this widget whether it stands in a window bar. A bar's controls are drawn
    /// at the bar's control height whatever the pointer; the target a finger needs
    /// reaches beyond the drawn shape, invisibly. The default propagates to children.
    fn set_window_bar_control(&mut self, in_bar: bool) {
        defaults::set_window_bar_control(self.children_mut(), in_bar);
    }

    /// Pull model state into this widget before layout. The default syncs the
    /// children [`Widget::children_mut`] returns; a widget overrides it only to
    /// read state of its own, or to sync children it does not expose there.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        defaults::sync(self.children_mut(), state, path);
    }

    /// Notice, between frames, state outside the document (a chrome revision, a momentum scroll
    /// still ticking) and report whether anything changed. The default updates every child
    /// [`Widget::children_mut`] returns without short-circuiting, so a container never freezes its
    /// descendants; an override checks state of its own and forwards.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        defaults::update(self.children_mut(), state, path)
    }

    /// Apply, after the sync pass, what the widget took up there and must write: a worker's answer,
    /// a chooser's file, an event a child queued. Writes go only through typed actions on `edits`;
    /// the host syncs again when the document changed. The default settles the returned children.
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
    /// `None` means the widget has no such policy; ordinary manual widgets remain
    /// valid. Values are available after build/settlement and carry real origins.
    fn layout_info(&self) -> Option<LayoutInfo> {
        None
    }

    /// Adjust this node's style using its allocated size, before final
    /// geometry is applied. Return true only when the style changed (or an
    /// inherited decision child hooks read). An override may measure its owned
    /// children through ordinary references, entering their callback binding
    /// when present. Never reenter descendants through the raw node map while
    /// borrowing their parent, change topology, or build another layout tree.
    /// The shared host solver visits every registered widget, repeats layout
    /// at most eight times and skips a frame that does not settle.
    fn adjust_layout_style(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        false
    }

    /// Take the geometry layout gave this node. The default keeps none, for
    /// widgets that neither draw nor hit-test.
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

    /// Observe this app's ordinary foreground physical input before framework
    /// shortcuts consume it. Keep feedback ephemeral; this is not an input grant.
    fn observe_physical_input(&mut self, _input: PhysicalInput) {}

    /// Handle one input event with the runtime services for this retained node.
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}
    fn children(&self) -> Vec<&dyn Widget> {
        vec![]
    }
    /// Allocation-free read-only child traversal. Existing custom containers
    /// inherit a compatibility adapter over [`Self::children`]; toolkit
    /// containers override this to walk their retained storage directly.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        defaults::for_each_child(self.children(), visitor);
    }
    /// The children of [`Self::for_each_child`] that may paint inside the [`layout_clip`] in
    /// effect: what a scroll view's damage and copy plan need. Every child by default; a long
    /// column visits only those near the clip, so a frame costs what is shown, not what the column
    /// holds.
    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.for_each_child(visitor);
    }
    /// Children this widget keeps in the retained tree but does not route
    /// input to: a subtree it does not own and answers the person's events
    /// for itself, like a host drawing another widget's card over the
    /// windows. Inspection, named controls and the accessibility tree walk
    /// these after the children [`Self::children`] lists, so what is walked
    /// there keeps the paths, sibling indices and registry entries it had.
    fn for_each_hosted_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) {}
    /// Every child this widget's [`Self::build_layout`] gives a layout node,
    /// also those [`Self::for_each_child`] keeps from routing (a hidden
    /// pane, a title, a spacer). A retained layout is checked against this
    /// walk before it is called through again, so a widget that replaced a
    /// child without reporting it gets a new layout instead of a call into
    /// freed memory. A child missing here cannot be checked, so debug builds
    /// stop on it. A wrapper that stands for its inner widget's node visits
    /// the inner widget itself.
    ///
    /// Returns whether it listed them; the default lists none and returns
    /// `false`, so the walk uses [`Self::for_each_child`]. (A constant
    /// default is one shared function, not one copy per widget type.)
    fn for_each_layout_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        false
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![]
    }

    /// The readable column this widget keeps what it shows in, when it is one: a page's measure,
    /// a list that reads its rows at it. A bar docked at the foot of a column stands in that
    /// column, found wherever the column is in what stands above it, so it says the column it is
    /// read in rather than one its wrapper is asked about.
    fn readable_column(&self) -> Option<crate::widget::arrange::Length> {
        None
    }

    fn name(&self) -> &str;
    /// Stable id for the e2e test harness; `None` falls back to a path-based
    /// key. Must be unique across the live tree — the registry resolves on
    /// first match, so a duplicate shadows the second occurrence.
    fn test_id(&self) -> Option<&str> {
        None
    }

    /// Downcast to the concrete widget, for hosts and tests that reach a
    /// known widget through `dyn Widget`. Default `None` (a widget may borrow
    /// and so not be `'static`); a widget opts in with `Some(self)`, and a
    /// [`Decorator`] answers itself.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Whether this widget is the one at `address`, or stands in the tree
    /// for it: a [`Decorator`] stands for every widget it wraps, so a
    /// scroller drawn inside one is found where the decorator is
    /// ([`crate::scroll_copy::offer`]).
    fn stands_for(&self, address: *const ()) -> bool {
        std::ptr::from_ref(self).cast::<()>() == address
    }

    /// Context that should remain visible with a focused descendant, such
    /// as a form control and its validation message. Opt-in scroll containers
    /// use this block when it fits; otherwise they prioritise the control.
    fn focus_reveal_context(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Show `rect` of the descendant `target` (its own rect, or the part of
    /// it to show, as laid out) inside every enclosing viewport. Containers
    /// override this to scroll; ordinary widgets simply recurse toward the
    /// target and report whether it was found.
    fn reveal_descendant(&mut self, target: *const dyn Widget, rect: vello::kurbo::Rect) -> bool {
        defaults::reveal_descendant(self.children_mut(), target, rect)
    }

    /// Named hit rects for widgets that paint several affordances into one
    /// rect instead of composing children. Same space as
    /// [`Widget::get_rect`]; zero-area entries are dropped.
    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    /// The rectangle this widget occupies. The default, for widgets that
    /// keep no geometry, is empty.
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }

    /// Absolute logical clip applied to ordinary children during painting and
    /// pointer routing. Inspectors and enclosing scroll regions must respect
    /// this boundary instead of treating clipped descendants as visible extent.
    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// The words this widget shows as the heading of the page it belongs to,
    /// when it is the page's own title: a large-title text the content edits
    /// or sets at the top of a document. A shell that also names the page in
    /// its bar reads this to keep one title on screen at a time.
    fn own_page_title(&self) -> Option<String> {
        None
    }

    /// Whether this is an item of a list opened where its row stands, among the list's other
    /// rows: what it shows is that item's, never the page's, so its title is not the page's own.
    fn is_item_opened_in_place(&self) -> bool {
        false
    }

    /// Whether `own_page_title` is a heading a screen reader reads as the
    /// page's name, or only the field the person types it in, which names
    /// itself. A shell takes its bar's title away for the former, and keeps
    /// a heading of the words where the field stands for the latter.
    fn page_title_is_heading(&self) -> bool {
        false
    }

    /// Offsets along `axis` that this widget, as a scroll view's content,
    /// offers it to settle on, measured from its own leading edge: a board's
    /// lanes ([`crate::widget::Board`]). None leaves a scroller stopping
    /// wherever the hand left it.
    fn snap_points(&self, axis: crate::widget::ScrollAxis, out: &mut Vec<f32>) {
        let _ = (axis, out);
    }

    /// One-shot request to open the host's action palette directly at a typed
    /// resource action. Parameter collection and permission checks remain host
    /// policy; widgets only name an address already present in `resources()`.
    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        defaults::take_action_palette_request(self.children_mut())
    }

    /// One-shot request to open the host's search palette, the same one the
    /// platform search shortcut opens. An app's own search button latches it
    /// so search stays one shared host surface rather than an app overlay.
    fn take_search_request(&mut self) -> bool {
        defaults::take_search_request(self.children_mut())
    }

    /// `point` is logical coords, same space as [`Widget::get_rect`];
    /// `None` defers to the default arrow. The host hit-tests the whole
    /// tree via `node_map` and picks the smallest-area claim, so containers
    /// need not override this.
    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    /// Opt out of touch-to-pointer emulation for contacts that begin at
    /// `point` (logical coords, same space as [`Widget::get_rect`]).
    ///
    /// By default the host turns direct touch into pointer events: a tap
    /// clicks, a drag scrolls, a hold opens the context menu, and a second
    /// finger turns the contact into a pinch or pan. A widget that returns
    /// `true` here instead receives only the raw per-finger
    /// `WindowEvent::Touch` stream (`Started`/`Moved`/`Ended`/`Cancelled`,
    /// keyed by `Touch::id`, `location` in physical pixels) for every contact
    /// that begins inside it, and those contacts neither cancel nor are
    /// cancelled by other fingers. An on-screen keyboard uses this so two
    /// thumbs can type with overlapping presses.
    ///
    /// Hit-tested like [`cursor_shape`](Self::cursor_shape): the host asks
    /// every laid-out widget whose rect contains the point, so containers
    /// need not forward it.
    fn claims_raw_touch(&self, _point: vello::kurbo::Point) -> bool {
        false
    }

    /// What a one-finger contact that begins at `point` (logical coords)
    /// means here, when a drag moves something rather than scrolling.
    ///
    /// [`TouchClaim::Drag`](crate::interaction::TouchClaim::Drag) where a
    /// drag moves things at once (a canvas whose items are dragged, words
    /// selected by dragging): past a tap's slop the contact is a left press
    /// where it landed, a move per update and a release where it lifts.
    /// [`TouchClaim::HoldDrag`](crate::interaction::TouchClaim::HoldDrag)
    /// where a drag scrolls but a held item can be moved (pages in a list):
    /// a still hold presses where the finger rests, and a drag after it
    /// moves the item. Either way a tap still clicks, a hold still reaches
    /// the context menu, and a second finger still pinches or pans (it
    /// ends the drag with a release first). Hit-tested like
    /// [`claims_raw_touch`](Self::claims_raw_touch), which wins over it.
    fn touch_claim(&self, _point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        crate::interaction::TouchClaim::Pointer
    }

    /// Revealed after a dwell over the widget's rect, via the toolkit's one
    /// tooltip mechanism ([`crate::widget::tooltip::TooltipHost`]),
    /// hit-tested like [`cursor_shape`](Self::cursor_shape) so containers
    /// need not forward it. An icon-only control should also use this as
    /// its accessible name.
    ///
    /// The text as the person reads it: source copy borrowed from the
    /// widget, a translated sentence resolved from the catalogue when it
    /// says something the catalogue words ([`crate::i18n`]).
    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        None
    }

    /// Which side of the control its tooltip goes: below by default,
    /// beside it for a control in a vertical rail.
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
    /// Preferred control when its app is explicitly activated without a valid
    /// saved control. Hosts still enforce visibility, enabled state and scope.
    fn is_default_keyboard_focus(&self) -> bool {
        false
    }

    /// Exact app instance whose non-text shortcuts this subtree owns when
    /// activation has no control target. Unbound desktop chrome may declare
    /// the active tab; component boundaries default to their captured owner.
    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        self.callback_binding().map(|binding| binding.instance_id)
    }

    fn set_focus(&mut self, _focused: bool) {}

    /// The window holding this content fills its display (a maximized
    /// window): it has nothing beside it to round its corners against or
    /// to set it apart from, so content that frames or clips itself to the
    /// window's shape draws edge to edge instead. A client surface drawn
    /// that way, last and unclipped, is one the display can scan out
    /// directly. Content that frames nothing ignores it.
    fn set_full_bleed(&mut self, _full_bleed: bool) {}

    /// The form's validation verdict for this control, set
    /// by [`Field::with_error`]: an invalid control edges itself in danger ink
    /// and reports `invalid` to assistive technology. Controls without a
    /// field face ignore it.
    fn set_invalid(&mut self, _invalid: bool) {}

    /// An explicit runner focus request, after resolving the actual keyboard
    /// owner. Retained flags and layout restoration are not user gestures.
    /// Containers can observe this to reveal the owning viewport.
    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        defaults::focus_requested(self.children_mut(), target, tree);
    }

    fn is_focused(&self) -> bool {
        false
    }

    /// The box a context menu opened from the keyboard stands against: the
    /// item that has the keyboard, so the menu opens under it rather than
    /// under the whole widget. A widget that holds items (a list) answers
    /// for the one with the keyboard; `None` leaves the menu to stand under
    /// the focused widget's own box.
    fn context_menu_anchor(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Whether a focused widget owns Tab instead of the host's focus
    /// traversal. This is deliberately opt-in: ordinary form fields and
    /// controls retain platform-standard Tab/Shift+Tab navigation, while a
    /// code editor or terminal can receive the key for indentation or
    /// completion. A widget around the focus may claim it too: a sheet that
    /// walks its own controls with Tab.
    fn wants_tab_input(&self) -> bool {
        false
    }

    /// Caret rectangle for the platform input method, in window-logical
    /// coordinates. Returning `Some` enables IME for the focused widget and
    /// positions its candidate window; non-text controls keep the default
    /// `None` and therefore disable IME.
    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// While any widget answers `true`, the host restricts Tab/directional
    /// focus to this subtree (wrapping at the ends) and restores the prior
    /// focus once it goes back to `false`. Default `false`.
    fn traps_focus(&self) -> bool {
        false
    }

    /// Whether Tab and directional focus travel stay inside this subtree
    /// while no trap holds focus: the focused desktop window, so keyboard
    /// travel cycles through the window the person is in (its bar and its
    /// app) and never wanders onto the other windows or the desktop's chrome
    /// (top bar, menus), which the keyboard reaches through its own
    /// shortcuts. Unlike
    /// [`Self::traps_focus`] it moves no focus when it starts or ends, and
    /// pointer input is unaffected. Default `false`.
    fn scopes_keyboard_focus(&self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it): the app instance the person just
    /// brought forward (a window they opened, clicked or switched to), whose
    /// keyboard focus the host restores, or puts on its default control, so
    /// what they type next reaches it. Default forwards the first request
    /// found among the children.
    fn take_app_activation(&mut self) -> Option<crate::application::InstanceId> {
        defaults::take_app_activation(self.children_mut())
    }

    /// One-shot (reading MUST clear it): the test id of the control this
    /// subtree asks to take the keyboard once the runner laid it out, such as
    /// the first field of a view a person's press just opened, so what they
    /// type next lands there. The runner focuses it as a press focuses a
    /// control, blurring any other; a control it does not find (or that takes
    /// no focus) leaves focus where it is. Default forwards the first request
    /// found among the children.
    fn take_focus_request(&mut self) -> Option<String> {
        defaults::take_focus_request(self.children_mut())
    }

    /// Role, name/description/value, and
    /// [`AccessState`](crate::a11y::AccessState). Default `None` means
    /// presentational: the node is skipped and children reparent onto the
    /// parent. State must be **derived, not stored** so it can't drift from
    /// `draw`; names must already be [`widget_text`](crate::i18n::widget_text)-resolved.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        None
    }

    /// [`Self::accessibility`] with its synthetic children shared, for a
    /// widget that keeps them between frames: each child it did not change
    /// is handed over as the very `Arc` of the last frame, which the
    /// semantic tree takes as unchanged without copying or comparing it.
    ///
    /// `None` (the default) means "read [`Self::accessibility`]". A widget
    /// that answers `Some` must answer `accessibility()` with the same node,
    /// its children owned ([`SharedAccessNode::into_owned`](crate::a11y::SharedAccessNode::into_owned)).
    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        None
    }

    /// Whether [`Self::accessibility`] reports this subtree hidden, for a
    /// host that asks it of every node to prune focus traversal. Widgets
    /// whose node is costly to build (text values, per-block children, a
    /// translated name) answer without building it: the focus walk asks
    /// every widget in the window on every recomputation. Any override must
    /// agree with `accessibility()`; debug builds check it in the walk.
    fn accessibility_hidden(&self) -> bool {
        self.accessibility_root()
            .is_some_and(|node| node.state.hidden)
    }

    /// [`Self::accessibility`]'s own node with its `children` left empty:
    /// its role, identity (`semantic_key`) and state. Focus capture and focus
    /// order read only these, for every node on every frame, so a widget
    /// whose node carries a large subtree (a document exporting its every
    /// block) answers without building it. An override must equal
    /// `accessibility()` with `children` cleared.
    fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
        self.accessibility().map(|mut node| {
            node.children.clear();
            node
        })
    }

    /// Geometry for an immediate-mode semantic child from
    /// [`AccessNode::children`](crate::a11y::AccessNode::children). The path is
    /// its child-index path beneath this widget's accessibility node. Returning
    /// a rect lets AccessKit activation inject the same click as pointer input.
    fn synthetic_accessibility_rect(&self, _path: &[usize]) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Move this widget's own immediate-mode focus to a synthetic semantic
    /// child. Retained widgets continue to use the ordinary taffy focus path.
    fn focus_synthetic_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a
    /// synthetic child returned by [`accessibility`](Widget::accessibility).
    /// The returned index addresses `AccessNode::children` for this frame.
    /// Collections use this to place a real interactive row delegate inside
    /// its virtual `ListItem`/`TreeItem` instead of exporting a duplicate.
    fn accessibility_child_parent(&self, _child_index: usize) -> Option<usize> {
        None
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a
    /// nested path of synthetic children returned by
    /// [`accessibility`](Widget::accessibility).
    ///
    /// The child, and everything inside it, is walked under that synthetic
    /// child instead of beside it, so the tree a reader gets matches where a
    /// person sees the child. Combined with
    /// [`accessibility_represents_child`](Widget::accessibility_represents_child)
    /// it also says *which* synthetic child the parent publishes the child's
    /// own node as: the widget's children then stand under that node, one node
    /// per child, and no second node of the child's own is exported.
    ///
    /// The default preserves the one-level
    /// [`accessibility_child_parent`](Widget::accessibility_child_parent)
    /// contract. Tables override this to place a real cell control beneath
    /// its synthetic row and cell, for example `[visible_row, column]`.
    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        self.accessibility_child_parent(child_index)
            .map(|parent| vec![parent])
    }

    /// Whether one retained [`children`](Widget::children) entry is already
    /// represented in this widget's own synthetic
    /// [`accessibility`](Widget::accessibility) tree, so the bridge must not
    /// export it a second time. A host that publishes a child's semantics
    /// itself (to add its own controls inside the same dialog) answers `true`
    /// for that child; assistive technology then sees one copy, not two.
    ///
    /// A host that also gives the child a path
    /// ([`accessibility_child_path`](Widget::accessibility_child_path))
    /// publishes the child's *node*, and the walk hangs the child's own
    /// children under it: a row opened into its item is one node holding the
    /// item's controls, not a row and an item.
    fn accessibility_represents_child(&self, _child_index: usize) -> bool {
        false
    }

    /// Whether this widget lays one retained [`children`](Widget::children)
    /// entry out itself (an [`Embedded`](crate::widget::Embedded) part of a
    /// painted page) rather than through the window's layout, which then
    /// gives it no node. The accessibility walk reaches such a child, and
    /// every widget inside it, through this widget's node.
    fn lays_out_child(&self, _child_index: usize) -> bool {
        false
    }

    /// What describes one retained [`children`](Widget::children) entry to
    /// assistive technology, when this widget holds the words for it: a
    /// row's secondary line is its trailing switch's description. The
    /// bridge sets it as that child's node's description unless the child
    /// describes itself.
    fn accessibility_child_description(&self, _child_index: usize) -> Option<String> {
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

    /// Handle an action addressed to a synthetic accessibility child created
    /// by this widget. `target` is the opaque stable value supplied through
    /// [`AccessNode::with_action_target`](crate::a11y::AccessNode::with_action_target).
    ///
    /// Custom canvases using
    /// [`AccessNode::with_keyed_action_routes`](crate::a11y::AccessNode::with_keyed_action_routes)
    /// do not need to override this method: the default decodes the published
    /// semantic target and forwards it to [`Widget::perform_action`].
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

    /// A disabled (`false`) widget is excluded from focus traversal, must
    /// reject input in `handle_event`, and renders dimmed (see
    /// [`DISABLED_ALPHA`]).
    fn is_enabled(&self) -> bool {
        true
    }

    /// This field needs a host that consumes deferred receipts. Ordinary native
    /// Wayland windows must not advertise it until they have that delivery path.
    fn ime_requires_deferred_ack(&self) -> bool {
        defaults::ime_requires_deferred_ack(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Ordinary input-method commits (a composing keyboard IME, the on-screen
    /// keyboard) cannot reach this field: its text arrives only through
    /// deferred receipts (a browser page field), so hosts keep standard text
    /// input closed outside a dictation session. A field that needs deferred
    /// receipts for dictation but turns an ordinary commit into its own
    /// acknowledged edit (Terminal's native editor) does not refuse them.
    fn ime_refuses_ordinary_commits(&self) -> bool {
        defaults::ime_refuses_ordinary_commits(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Begin one bounded deferred session after explicit host consent, preserving
    /// the existing original-field fingerprint. This grants no microphone access.
    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::start_deferred_text(self.children_mut(), caret)
    }

    /// A private asynchronous final. None leaves ordinary synchronous IME dispatch
    /// to the host. A receipt binds this original field and must be acknowledged
    /// before the host releases its private recovery transcript.
    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::commit_deferred_text(self.children_mut(), caret, text)
    }

    /// The focused widget wants platform text input (Wayland
    /// `text-input-v3`, native IMEs elsewhere). `None` is intentionally the
    /// default: enabling an IME for buttons, canvas tools or games makes the
    /// compositor raise an on-screen keyboard over content that cannot accept
    /// the committed text.
    ///
    /// The answer is what the field holds and how the input method may help
    /// in it, so the on-screen keyboard picks the matching layout (an email
    /// field's `@`, a phone field's dial pad). Return the closest truthful
    /// purpose; platforms that take less (winit's three purposes) get the
    /// nearest of theirs.
    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        None
    }

    /// Whether the original focused editor accepts line feeds as text. This is
    /// field policy, never inferred from a multiline accessibility presentation.
    /// Wrappers inherit only one unambiguous child at their active IME caret.
    fn ime_multiline(&self) -> bool {
        let Some(caret) = self.ime_cursor_rect() else {
            return false;
        };
        defaults::ime_multiline(caret, &|visit| self.for_each_child(visit))
    }

    /// Opaque local invalidation of deferred input, including selection edits
    /// that leave the caret rectangle unchanged. The host supplies an ephemeral
    /// secret key; no text or selection is retained in the resulting digest.
    /// The default shares the accessibility door and excludes protected or
    /// read-only controls. Text controls can override it to avoid cloning text.
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let caret = self.ime_cursor_rect()?;
        defaults::ime_edit_fingerprint(caret, key, &|visit| self.for_each_child(visit), &|| {
            self.accessibility()
        })
    }

    /// Default `true`; false only for a widget standing in for a window's
    /// content whose first frame has not drawn. Must **latch**: once `true`,
    /// never `false` again for the same content.
    fn presented(&self) -> bool {
        true
    }
    /// Where this content's first frame comes from while it has not
    /// [`presented`](Widget::presented): this device (an app starting here),
    /// another device or a host. A frame from this device may hold the
    /// window's first frame up to one frame budget; any other reserves the
    /// window's place and says where it comes from after a while.
    fn arrives_from(&self) -> crate::arriving::Source {
        crate::arriving::Source::Local
    }
    /// How much of what this content's first frame waits on has come, when
    /// that measures itself (its app downloaded before it starts).
    fn arrival_progress(&self) -> Option<crate::arriving::Progress> {
        None
    }
    /// The shape this content declares for the placeholder standing in for
    /// its first frame. Undeclared, the placeholder is the ground alone:
    /// nothing guesses what a window will show.
    fn arrival_shape(&self) -> crate::widget::SkeletonShape {
        crate::widget::SkeletonShape::Ground
    }

    /// `(caret_byte, text)`, byte offset on a char boundary. The host diffs
    /// this before/after an event so caret-only moves (arrows, Home/End)
    /// get announced, not just typing.
    fn text_review(&self) -> Option<(usize, String)> {
        None
    }

    /// Delivers an out-of-band string payload from
    /// [`crate::app::UserEvent::External`] (e.g. a worker thread streaming
    /// output into a status log). Default no-op; the App redraws and
    /// re-syncs after dispatch, so mutating state here is enough.
    fn on_external_event(
        &mut self,
        _payload: &str,
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) {
    }

    /// Global back/forward gesture (swipe/edge-pull), recognised by
    /// [`crate::gesture::NavGestureRecognizer`] and delivered to the whole
    /// tree; return `true` once consumed. Default forwards to children, so
    /// a widget that owns history becomes swipe-navigable for free.
    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        defaults::on_nav_gesture(self.children_mut(), dir, state, path)
    }

    /// Which directions [`Self::on_nav_gesture`] would consume now. A widget
    /// that owns history overrides this beside `on_nav_gesture`.
    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        defaults::nav_availability(self.children())
    }

    /// One-shot (reading MUST clear it) signal to move focus to this
    /// subtree's first focusable widget after the next rebuild. Only
    /// consulted under [`crate::app::App::autofocus_on_structure_change`];
    /// a wizard raises it on step transitions.
    fn take_focus_reset(&mut self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it) request to change the window's
    /// accessibility preferences, raised by a control the person used. The
    /// host applies it before the next frame. Default forwards the first
    /// request found among the children.
    fn take_accessibility_request(&mut self) -> Option<crate::a11y::AccessibilityRequest> {
        defaults::take_accessibility_request(self.children_mut())
    }

    /// Route history when this widget, or one it contains, is a [`Router`].
    ///
    /// Dev reload writes this beside the document so the same screen is showing
    /// after the process is replaced. Containers forward to the first child
    /// that has one. A [`Router`] reports its own stack and
    /// does not look inside the current screen.
    fn dev_route(&self) -> Option<router::RouteSnapshot> {
        defaults::dev_route(self.children())
    }

    /// Put `route` back. Returns whether a router in this subtree accepted it.
    ///
    /// An unknown route name is refused and the existing screen stays up.
    fn restore_dev_route(&mut self, route: &router::RouteSnapshot) -> bool {
        defaults::restore_dev_route(self.children_mut(), route)
    }

    /// Theme/high-contrast/reduced-motion/scale, polled under
    /// [`crate::app::App::appearance_reconcile`] — how a settings pane
    /// re-themes the running app live without a back-channel.
    /// Level-triggered; the App recurses [`Self::children`] itself, so
    /// containers need not forward it. First `Some` wins.
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        None
    }

    /// The narrowest this window still works at, in logical pixels: the
    /// minimum of its compact layout (a `NavShell` answers with one column
    /// at a time). The host states it to the window system, so the desktop
    /// stops a resize there. `None`: no opinion. The host walks the tree
    /// itself ([`minimum_window_width`]), so containers need not forward
    /// it; the first `Some` wins.
    fn minimum_window_width(&self) -> Option<f64> {
        None
    }

    /// The whole size this window wants, in logical pixels, when the app is
    /// the window's only content and knows how big it must be: the on-screen
    /// keyboard parked to a strip. The host states it as the window's minimum
    /// and maximum, so the desktop shapes the surface to it. `None`: the
    /// window is the user's and the app lays itself out in whatever it gets.
    fn preferred_window_size(&self) -> Option<(f64, f64)> {
        None
    }

    /// Apply the effective reduced-motion preference before the host asks this
    /// subtree whether it needs another animation frame. The default forwards
    /// recursively, making the preference a framework policy instead of an
    /// optional convention that every container has to remember to implement.
    ///
    /// Custom-painted roots with app-local animation state should override
    /// this, settle any in-flight decorative transitions when `reduced` is
    /// true, and then forward to retained child widgets if they have any.
    fn set_motion_reduced(&mut self, reduced: bool) {
        defaults::set_motion_reduced(self.children_mut(), reduced);
    }

    // --- Collab / DnD contract extensions ---
    /// Whether `sync` still needs to run on idle frames without input or
    /// collab events — e.g. waiting out a document-adoption grace window.
    fn needs_idle_sync(&self) -> bool {
        false
    }

    fn wants_pointer_capture(&self) -> bool {
        false
    }

    /// While this widget is a band on its way in or out
    /// (`motion::Disclosure`: an in-window banner arriving or leaving), or a
    /// content-sized container one moves in, how far what follows it is
    /// drawn from where it is laid out. A moving band stands out of the
    /// layout; its container places it ([`Self::place_disclosing`]) and
    /// draws the widgets after it lower by transform, so they move with the
    /// band's edge instead of being laid out again every frame. `None` at
    /// rest (the default).
    fn disclosing(&self) -> Option<Disclosing> {
        None
    }

    /// Where a band on its way ([`Disclosing::Band`]) stands while it is
    /// out of the layout: its slot's top and width (`slot`'s height is
    /// zero), set by its container after each layout.
    fn place_disclosing(&mut self, _slot: vello::kurbo::Rect) {}

    /// A band that comes and goes (an in-window banner): when its view
    /// stops building it, its column draws it leaving
    /// (`widget::Disclosing`). Its column leaves it out of its identity
    /// across rebuilds.
    fn is_band(&self) -> bool {
        false
    }

    /// Where a band stands in its column, set by the column after each
    /// layout: what it leaves when dropped, so the column a rebuild makes
    /// can draw it leaving.
    fn note_placement(&mut self, _placement: Placement) {}

    /// Why this widget itself asks for animation frames, for diagnostics
    /// that name a desktop which keeps painting. `None` when it has nothing
    /// to add to its name and geometry.
    fn animation_detail(&self) -> Option<String> {
        None
    }

    /// `App` polls this in `Event::AboutToWait`; without it a curve would
    /// freeze until an unrelated event woke it.
    fn needs_animation_frame(&self) -> bool {
        defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    /// Earliest wall-clock deadline at which this subtree needs a sync or
    /// redraw with no input: the loop sleeps until then (not animation).
    fn next_wake_deadline(&self) -> Option<Instant> {
        defaults::next_wake_deadline(&|visit| self.for_each_child(visit))
    }

    /// Until when the frame that would show this subtree waits for data it
    /// reads from this device ([`crate::arriving`]): a local read that has
    /// not answered and is still inside its frame budget. The host keeps
    /// the frame on screen until then, or until the data lands, so the
    /// person never sees the space empty and then filled. Containers
    /// forward (the default does).
    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        defaults::holds_frame_until(&|visit| self.for_each_child(visit), now)
    }

    /// A deadline this subtree asked for ([`Self::next_wake_deadline`]) is
    /// due at `now`: do the work it was for, and say whether anything
    /// changed. The default says it did.
    fn serve_deadline(&mut self, _now: Instant) -> bool {
        true
    }

    /// `Some(rect)` when this subtree holds an *open* modal, its panel box.
    /// While any widget reports `Some`, the host suppresses its own global
    /// focus shortcuts and swallows unhandled pointer presses so nothing
    /// behind the modal is clickable. Containers MUST forward (the default
    /// does, via first `Some` among children).
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        defaults::modal_capture(&|visit| self.for_each_child(visit))
    }

    /// Close every modal this subtree presents, as its own way out does (a
    /// host whose surface is a modal sheet calls it when the shell takes the
    /// sheet down). Whether any was presented; containers forward.
    fn dismiss_presented_modal(&mut self) -> bool {
        defaults::dismiss_presented_modal(self.children_mut())
    }

    /// Whether this subtree presents a modal still drawn (arriving, open or
    /// leaving): the counterpart of [`Widget::dismiss_presented_modal`].
    fn presents_modal(&self) -> bool {
        defaults::presents_modal(self.modal_capture(), &|visit| self.for_each_child(visit))
    }

    /// True if this subtree's structural composition changed since the last
    /// frame; reading clears it. The default drains every retained child and
    /// reports whether any was dirty.
    fn take_structure_dirty(&mut self) -> bool {
        defaults::take_structure_dirty(self.children_mut())
    }

    /// [`Self::take_structure_dirty`], saying whether the frame's damage can
    /// still describe the rebuild ([`StructureChange::Described`]). Reading
    /// clears what `take_structure_dirty` clears; the two must agree on
    /// whether there is a change. The default is the flag, as
    /// a [`StructureChange::Rebuild`]: only a widget that knows every pixel its
    /// change touches lands in its own damage says less, and a container
    /// forwards a child's [`StructureChange::Described`] only by overriding
    /// this.
    fn take_structure_change(&mut self) -> StructureChange {
        StructureChange::of(self.take_structure_dirty())
    }

    /// This widget's own layout node while the widgets under it are laid
    /// out again in place, in it, by the
    /// coming [`Self::remount_children`]: it then names none of them through
    /// [`Self::for_each_layout_child`], and the host lets go of every node
    /// under this one, as their widgets may be gone. `None` (the default)
    /// otherwise.
    #[doc(hidden)]
    fn relaid_layout_node(&self) -> Option<NodeId> {
        None
    }

    /// Carry out a [`StructureChange::Children`] this widget or one under it
    /// reported, in the retained layout tree, through `remount` (see
    /// [`remount`]). Returns `false` when it cannot be
    /// carried out in place: the host then rebuilds its tree, and the change
    /// is carried out by [`Self::build_layout`]. A widget reports such a
    /// change until either has carried it out. The default carries out its
    /// children's.
    fn remount_children(&mut self, remount: &mut Remount<'_>) -> bool {
        self.children_mut()
            .into_iter()
            .all(|child| child.remount_children(remount))
    }

    /// Work already requested by this view whose completion will replace a
    /// placeholder. Unlike animations, this can be awaited by still captures.
    fn has_pending_visual_work(&self) -> bool {
        defaults::has_pending_visual_work(&|visit| self.for_each_child(visit))
    }

    /// Temporary feedback currently presented, such as a save acknowledgement.
    /// Captures may intentionally include it; scene-invariance checks can wait
    /// for it to disappear separately from unfinished placeholder work.
    fn has_transient_feedback(&self) -> bool {
        defaults::has_transient_feedback(self.children())
    }

    /// Default `false` (clean). A widget is clean until something it reads
    /// changes: a leaf that keeps state says so by tracking a `dirty: bool`
    /// (set in `handle_event`/`sync`, cleared in `mark_drawn()`), and one
    /// that paints from the people on the document reads the frame's
    /// presence ([`crate::presence::frame::frame`]). Containers MUST
    /// forward: dirty iff any descendant is.
    fn needs_redraw(&self) -> bool {
        defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }

    /// Logical-pixel bounds whose visual output differs from the last frame.
    ///
    /// The conservative default covers this widget's complete layout box.
    /// Pure layout containers should override this and union descendant
    /// damage so a small animated leaf does not force a full-window raster.
    /// Callers add an anti-alias/effect safety margin and fall back to a full
    /// redraw for layout, structure, overlays, or invalid retained targets.
    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if self.needs_redraw() || self.needs_animation_frame() {
            regions.push(self.get_rect());
        }
    }

    /// Whether, after this frame's draw, this widget's own
    /// [`Self::collect_damage`] covers every pixel a relayout changed by
    /// moving or resizing its layout box: a host that repaints where moved
    /// widgets were and are may then leave this widget's box to it (its
    /// children's boxes still count). A block editor that grew by a
    /// paragraph says where its blocks were and are; the default does not
    /// claim it.
    fn damage_covers_own_box(&self) -> bool {
        false
    }

    /// Called after this widget's scene lands in the composed frame, to
    /// clear a `dirty` flag: paint only, never layout dirt
    /// ([`Self::is_layout_dirty`]). Default forwards to children.
    fn mark_drawn(&mut self) {
        defaults::mark_drawn(self.children_mut());
    }

    /// Rest this widget's children after a frame drew them, for a widget that
    /// keeps its own dirty flag: it clears the flag itself, then calls this.
    fn mark_children_drawn(&mut self) {
        defaults::mark_drawn(self.children_mut());
    }

    /// Release retained encoded-scene storage in this subtree without
    /// marking its visual/model state dirty. Outer caches call this after a
    /// successful reuse: while the outer display list is authoritative,
    /// keeping byte-for-byte descendant encodings only duplicates memory.
    /// A later outer rebuild causes descendants to record/cache normally.
    fn release_scene_cache(&mut self) {
        defaults::release_scene_cache(self.children_mut());
    }

    /// Default `true`; widgets with a fixed intrinsic size should override
    /// to `false` unless a size-affecting mutation landed. Containers MUST
    /// forward: layout-dirty iff any descendant is.
    ///
    /// Layout answers it, never a draw: the solver ([`Self::layout_measured`])
    /// or a host's `apply_layout_external`; [`Self::mark_drawn`] clears only
    /// paint, and a draw raises no layout dirt.
    fn is_layout_dirty(&self) -> bool {
        defaults::is_layout_dirty(&|visit| self.for_each_child(visit))
    }

    /// Whether this widget composes its children from the size it was last
    /// allocated (it reads its own rect while building its layout: a compact
    /// layout under a width, a count of columns, an inset from a drawer).
    /// A window resize then needs a second composition against the new
    /// allocation, as well as the re-solve every resize gets. Widgets that
    /// compose independently of their allocation, or that report
    /// `take_structure_dirty` themselves when a new allocation changes what
    /// they show, keep the default: a resize re-solves the retained tree in
    /// place instead of recomposing all of it.
    fn composition_follows_allocation(&self) -> bool {
        false
    }

    /// Whether a `layout_dirty` frame needs taffy to **re-solve** sizes
    /// (`compute_layout`) rather than just reposition already-solved boxes.
    /// [`Scrollable`] is the motivating override: it translates its origin
    /// in `apply_layout` without changing any taffy size, so re-solving on
    /// every scroll frame is pure waste.
    fn layout_needs_measure(&self) -> bool {
        defaults::layout_needs_measure(&|visit| self.for_each_child(visit))
            .unwrap_or_else(|| self.is_layout_dirty())
    }

    /// Meet the measure request [`Self::layout_needs_measure`] just reported:
    /// the solver asking is measuring this widget's node now, so whatever its
    /// geometry depended on is read in this pass. A widget that keeps a
    /// request flag clears it here. Only a draw used to answer it, so a widget
    /// laid out and never drawn — a shut overflow door's controls, a row a
    /// scroll hides, a pane folded away — kept asking, and every frame after
    /// re-measured it and its path to the root for a widget nobody sees.
    /// Containers need not forward: the solver meets every node it measures.
    fn layout_measured(&mut self) {}

    /// Everything this widget paints, children included; [`draw_child`]
    /// skips the draw when it misses the viewport. Override for widgets
    /// that paint past their own rect.
    fn paint_bounds(&self) -> vello::kurbo::Rect {
        self.get_rect()
    }

    /// **Top-layer pass**, run once after the whole `draw`
    /// recursion, so anything emitted here composites above every sibling
    /// regardless of tree order — the toolkit's overlay mechanism for
    /// content that escapes its layout box (an open [`Dropdown`], a
    /// popover). Default recurses `children_mut`.
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
