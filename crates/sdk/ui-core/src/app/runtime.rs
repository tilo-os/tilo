//! Host-owned subsystem state for [`super::App`].
//!
//! The app remains the orchestrator: these types make ownership and
//! invalidation boundaries explicit without disconnecting actions, resources,
//! or collaboration from the retained widget tree.

use super::{
    ActionDirectoryCursor, DirectedInvocation, FrameTick, LocalResourceCache,
    OpenResourceSearchCache, PresenceBroadcastSink, ResourceCache, ResourceCacheKey,
};
use crate::collaboration::CollaborationManager;
use crate::presence::PresenceKey;
use crate::theme::Theme;
use crate::time::Instant;
use crate::widget::Widget;
use std::cell::{Cell, RefCell};
use taffy::{NodeId, TaffyTree};
use winit::window::CursorIcon;

/// The mapped nodes that take part in history: those inside a bound
/// component or a record owner, and widgets that show or ask for undo or
/// report an action bracket. Found once per admitted map, so a frame's
/// history sweep visits them and not every row of a long list.
#[derive(Default)]
pub(super) struct HistoryNodes {
    /// The admission and map size these were found for.
    pub(super) found_for: Option<(u64, usize)>,
    pub(super) nodes: Vec<NodeId>,
    /// Whether any mapped node is outside a bound component: the sweep then
    /// keeps the transaction boundary such a node's status read made.
    pub(super) any_unbound: bool,
}

/// What a placement of every widget saw: where the keyboard focus was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlacedAll {
    pub(super) focus: Option<NodeId>,
}

/// Retained layout and widget lookup state.
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "the safe area and content origin are read only by the native event-loop runner"
    )
)]
pub(super) struct LayoutRuntime {
    pub(super) taffy: TaffyTree<()>,
    pub(super) root_node: Option<NodeId>,
    pub(super) node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    /// Which `node_map` pointers were taken from the live tree
    /// ([`crate::host_core::HostCore::layout_maps_stale`]).
    pub(super) adopted: crate::retained_widgets::Adopted,
    /// How many builds' maps were admitted ([`crate::host_core::HostCore::admit_layout_maps`]).
    pub(super) admissions: u64,
    /// The mapped nodes the history sweep visits, and the admission and map
    /// size they were found for (`App::history_sweep_nodes`).
    pub(super) history_nodes: HistoryNodes,
    pub(super) layout_dirty: bool,
    /// Set when a widget reports `take_structure_dirty()`: without a taffy
    /// rebuild before the next layout pass, runtime-spawned widgets have no
    /// `NodeId` and `apply_layout` skips them.
    pub(super) structure_dirty: bool,
    /// Set when the root's widgets report a change they carry out in place
    /// ([`crate::widget::StructureChange::Children`]): the next layout pass
    /// remounts their children in the retained tree instead of rebuilding
    /// it, unless something rebuilds it first.
    pub(super) remount: bool,
    /// How many times widgets changed the retained tree in place: like
    /// [`Self::rebuilds`], the widgets compared across a frame changed.
    pub(super) remounts: u64,
    /// Diffed against `inner_size()` to catch a resize without a `Resized` event.
    pub(super) last_frame_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// Last settled logical content size (the window inside `safe_area`);
    /// scale changes also require a solve.
    pub(super) last_layout_logical: Option<(f64, f64)>,
    /// System chrome drawn over the window, which content stays clear of.
    pub(super) safe_area: crate::scale::SafeArea,
    /// The row the runner draws the app's window-bar slots in, outside a
    /// Tilo session, under the safe area's top (`native_runner::slot_bar`).
    pub(super) slot_bar_inset: f64,
    /// Where the root is placed: the safe area's top-left corner.
    pub(super) content_origin: vello::kurbo::Point,
    /// How many times the taffy tree was rebuilt: a host comparing two
    /// frames' widgets needs to know they are still the same ones.
    pub(super) rebuilds: u64,
    /// Whether every rebuild requested since the last one was a change the
    /// widgets describe ([`crate::widget::StructureChange::Described`]).
    /// Anything else that asks for a rebuild goes through
    /// [`Self::request_rebuild`], which clears it.
    pub(super) rebuild_described: bool,
    /// Whether every rebuild since a host armed it (a frame that compares
    /// itself with the last one) was described: that host keeps the frame's
    /// damage instead of repainting whole.
    pub(super) frame_rebuilds_described: bool,
    /// Where the last rebuild no widget describes was asked for, for a test
    /// that tells why a frame repainted whole.
    #[cfg(test)]
    pub(super) undescribed_by: Option<&'static std::panic::Location<'static>>,
    /// Frames whose layout failed, skipped, and when that was last logged.
    pub(super) skipped_frames: crate::widget::SkippedFrames,
    /// The last placement that placed every widget (`None` before one): a
    /// focus move since places them all again, so a view revealing the
    /// focused widget reads where it truly stands.
    pub(super) placed_all: Option<PlacedAll>,
    /// The last placement placed only what long collections show
    /// ([`crate::widget::placing_again`]): a widget far from every view
    /// stands where an earlier placement put it until the next full one
    /// (`App::place_all`).
    pub(super) placed_partly: bool,
    /// What a host that draws only its frames' damage keeps from frame to
    /// frame ([`crate::draw_cull`]).
    pub(super) cull: crate::draw_cull::Culling,
    /// What the frame's whole-tree walks last read.
    pub(super) walks: TreeWalks,
}

impl LayoutRuntime {
    pub(super) fn new() -> Self {
        Self {
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            adopted: crate::retained_widgets::Adopted::default(),
            admissions: 0,
            history_nodes: HistoryNodes::default(),
            layout_dirty: true,
            structure_dirty: false,
            remount: false,
            remounts: 0,
            last_frame_size: None,
            last_layout_logical: None,
            safe_area: crate::scale::SafeArea::default(),
            slot_bar_inset: 0.0,
            content_origin: vello::kurbo::Point::ORIGIN,
            rebuilds: 0,
            rebuild_described: true,
            frame_rebuilds_described: false,
            #[cfg(test)]
            undescribed_by: None,
            skipped_frames: crate::widget::SkippedFrames::default(),
            placed_all: None,
            placed_partly: false,
            cull: crate::draw_cull::Culling::default(),
            walks: TreeWalks::default(),
        }
    }

    /// Rebuild the tree before the next layout, for a reason no widget
    /// describes: the frame that does it repaints whole.
    #[track_caller]
    pub(super) fn request_rebuild(&mut self) {
        self.structure_dirty = true;
        self.rebuild_described = false;
        #[cfg(test)]
        {
            self.undescribed_by = Some(std::panic::Location::caller());
        }
    }
}

/// Which retained widgets, declaring what, a frame's whole-tree walks read.
/// What the widgets declare moves on with the resource generation (an
/// update, a sync or an input that changed it:
/// `App::invalidate_widget_resource_cache`), the widgets themselves with a
/// rebuild or a remount, what the local actor may do with the permission
/// generation, and what the components in the tree may do with the
/// revision of the permissions their hosts share with them
/// ([`crate::permissions::shared_revision`]). Equal generations are the
/// same tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TreeGeneration {
    pub(super) resources: u64,
    pub(super) rebuilds: u64,
    pub(super) remounts: u64,
    pub(super) permissions: u64,
    pub(super) shared_permissions: u64,
}

/// What a frame's walks of the whole widget tree last read. A walk whose
/// inputs are all as it last read them would find what it found then, so
/// a frame that changed none of them (one only an animation asked for)
/// skips it. Emptied with the widget resource cache.
#[derive(Default)]
pub(super) struct TreeWalks {
    /// What control availability was last pushed into the tree from.
    pub(super) availability: Option<AvailabilityWalk>,
    /// What the document routes were last reconciled against.
    pub(super) routes: Option<RouteInputs>,
    /// What the last walk for History surfaces in the tree read and found.
    pub(super) history: Option<HistoryWalk>,
}

/// Every input of pushing control availability into the tree.
#[derive(PartialEq, Eq)]
pub(super) struct AvailabilityInputs {
    pub(super) tree: TreeGeneration,
    pub(super) framework_input: bool,
    pub(super) history_captures: bool,
    /// The addresses the window's History door serves.
    pub(super) history_door: std::collections::HashSet<tilo_actions::ActionAddress>,
}

/// What pushing control availability into the tree last read and found.
pub(super) struct AvailabilityWalk {
    pub(super) inputs: AvailabilityInputs,
    /// What the action emitters were last refreshed against
    /// (`ActionRuntime::last_control_availability`) as the walk left it:
    /// anything that drops it asks for the walk again.
    pub(super) controls: Option<(super::ResourceCacheKey, u64)>,
    /// The bound components' resources could not be read: their controls
    /// are refreshed again on the next frame.
    pub(super) unread: bool,
    /// When a bound component's permissions next change with time alone
    /// (`PermissionContext::next_change_in`): the walk is taken again then,
    /// and the loop wakes for it.
    pub(super) changes_at: Option<crate::time::Instant>,
}

/// Every input of reconciling the document routes with the tree.
#[derive(PartialEq)]
pub(super) struct RouteInputs {
    pub(super) tree: TreeGeneration,
    pub(super) witness: Option<crate::collaboration::PrimaryRouteWitness>,
    pub(super) autosave_generation: u64,
    pub(super) document: (Vec<automerge::ChangeHash>, usize),
}

/// What the last walk for History surfaces read and found.
pub(super) struct HistoryWalk {
    pub(super) tree: TreeGeneration,
    pub(super) status: Option<crate::widget::WindowStatus>,
    /// The tree holds History surfaces, which the next walk looks at again
    /// when anything they read moved since: news reached one
    /// ([`crate::history_mode::news`]), the document, whether the window is
    /// narrow, a surface's own refresh came due, or one needed a refresh.
    pub(super) embedded: bool,
    /// The window's own History found the root it registered.
    pub(super) root_valid: bool,
    /// What the surfaces read, as this walk read it.
    pub(super) surfaces: HistorySurfaceInputs,
    /// A surface needed a refresh.
    pub(super) needed: bool,
    /// When a surface asks for a refresh on its own, on its clock.
    pub(super) due: Option<(crate::background::Background, crate::time::Instant)>,
}

/// What History surfaces read besides the tree and the window's status.
#[derive(PartialEq)]
pub(super) struct HistorySurfaceInputs {
    pub(super) news: u64,
    pub(super) document: (Vec<automerge::ChangeHash>, usize),
    pub(super) narrow: bool,
}

impl HistoryWalk {
    /// Whether the surfaces this walk found need looking at again.
    pub(super) fn surfaces_owed(&self, inputs: &HistorySurfaceInputs) -> bool {
        self.embedded
            && (self.needed
                || self.surfaces != *inputs
                || self
                    .due
                    .as_ref()
                    .is_some_and(|(clock, at)| clock.now() >= *at))
    }
}

/// Input modality, gesture, cursor, and tooltip state.
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "keyboard layout, gestures, cursor, tooltip, pen and pointer state are read only by the native event-loop runner"
    )
)]
pub(super) struct InputRuntime {
    pub(super) modifiers: winit::event::Modifiers,
    /// Focused native configuration only; no logical key, compose or press history.
    pub(super) keyboard_layout: Option<std::sync::Arc<winit::keyboard::KeyboardLayout>>,
    pub(super) interaction: crate::interaction::InteractionEnvironment,
    pub(super) platform: crate::platform::PlatformConventions,
    pub(super) nav_gestures: crate::gesture::NavGestureRecognizer,
    pub(super) current_cursor: CursorIcon,
    pub(super) tooltips: crate::host_core::TooltipLayer,
    /// Pen samples on their way to the pointer events they describe.
    pub(super) pen: crate::pen::PenInput,
    /// The app is shown inside a host whose focus order continues past it:
    /// Tab at either end of the app's order leaves it for the host.
    pub(super) tab_order_continues_in_host: bool,
    /// Where the pointer is, which buttons it holds and where it last pressed.
    pub(super) pointer: crate::host_core::PointerPlace,
    /// Where the pointer last moved, replayed before a press or release
    /// that follows a rebuild under a still pointer.
    pub(super) pointer_replay: crate::pointer_replay::PointerReplay,
    /// The window has the keyboard: the one a person (or a test control's
    /// activation) addresses among the app's windows.
    pub(super) window_focused: bool,
}

impl InputRuntime {
    pub(super) fn new() -> Self {
        Self {
            modifiers: winit::event::Modifiers::default(),
            keyboard_layout: None,
            window_focused: false,
            interaction: crate::interaction::InteractionEnvironment::default(),
            platform: crate::platform::PlatformConventions::default(),
            nav_gestures: crate::gesture::NavGestureRecognizer::new(),
            current_cursor: CursorIcon::Default,
            tooltips: crate::host_core::TooltipLayer::default(),
            pen: crate::pen::PenInput::default(),
            tab_order_continues_in_host: false,
            pointer: crate::host_core::PointerPlace::default(),
            pointer_replay: crate::pointer_replay::PointerReplay::default(),
        }
    }
}

/// Action discovery, dispatch, and invocation state.
///
/// Actions remain connected to the retained widget tree; this boundary
/// centralizes their lifecycle rather than separating them from UI
/// capabilities.
pub(super) struct ActionRuntime {
    pub(super) published_invocations: crate::invocation_scope::PublishedInvocationRuntime,
    pub(super) action_palette: Option<Box<crate::action_palette::ActionPalette>>,
    /// The window the palette was opened from, when the app hosts several
    /// (the desktop's tiles): the palette centres on it, not on the screen.
    pub(super) palette_window: Option<vello::kurbo::Rect>,
    /// Stable opener identity, never a node or pointer from the modal's old tree.
    pub(super) ordinary_palette_return_focus: Option<crate::widget::layout::CompositionFocus>,
    /// Why invocations the person made this pass did not run, and the
    /// control each came from when one is on screen: said at that control,
    /// else in the window's bar, once the pass ends
    /// ([`super::App::refuse_visibly`]).
    pub(super) refusals: Vec<(String, Option<vello::kurbo::Rect>)>,
    /// What stands at the control the person last used, until their next
    /// press or key ([`crate::control_notice`]).
    pub(super) control_notice: Option<crate::control_notice::ControlNotice>,
    /// The window is a tile its host lays out in its own process (the
    /// shell): searches go to the host's one search through the app's
    /// component ([`super::App::hand_search_to_session`]).
    pub(super) search_in_host: bool,
    /// The control an accessibility action focused while the same action
    /// replaced widgets: focused again once the next layout admits them.
    pub(super) focus_after_rebuild: Option<crate::widget::layout::CompositionFocus>,
    pub(super) action_registry: tilo_actions::ActionRegistry,
    // Independent from scheduling invalidation: mutable unbound decoder access.
    pub(super) action_registry_generation: u64,
    pub(super) action_transport: Box<dyn tilo_actions::ActionTransport>,
    pub(super) resource_cache: RefCell<Option<ResourceCache>>,
    pub(super) bound_resource_cache: crate::bound_dispatch::BoundResourceCache,
    pub(super) bound_record_sync: Option<super::bound_record_sync::BoundRecordSync>,
    pub(super) bound_publication: crate::bound_dispatch::BoundPublication,
    pub(super) last_published_bound_route: Option<crate::collaboration::PrimaryRouteWitness>,
    /// Publication refusals already logged, so a surface refused on every
    /// frame logs each distinct reason once rather than at frame rate.
    pub(super) refusals_logged: RefusalLog,
    #[cfg(test)]
    pub(super) bound_broker_route_for_test: Option<crate::collaboration::PrimaryRouteWitness>,
    pub(super) action_directory_stamp: Option<(
        std::rc::Rc<super::action_directory_snapshot::DirectoryWitness>,
        String,
    )>,
    pub(super) action_directory_snapshot:
        Option<std::rc::Rc<super::action_directory_snapshot::ActionDirectorySnapshot>>,
    pub(super) local_document_directory_pins:
        Vec<std::rc::Weak<super::action_directory_snapshot::LocalDocumentDirectoryPin>>,
    pub(super) bound_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::bound_dispatch::BoundActionTarget,
    >,
    pub(super) local_resource_cache: RefCell<Option<LocalResourceCache>>,
    pub(super) open_resource_search_cache: RefCell<Option<OpenResourceSearchCache>>,
    pub(super) resource_generation: Cell<u64>,
    pub(super) permission_generation: Cell<u64>,
    pub(super) last_control_availability: Option<(ResourceCacheKey, u64)>,
    pub(super) local_history_status: Option<(bool, bool, bool)>,
    pub(super) last_published_actions: Option<Vec<tilo_actions::ActionDescriptor>>,
    /// What the broker holds of the surface `last_published_actions` names,
    /// the base of the next delta.
    pub(super) published_surface: Option<crate::collaboration::SurfacePublication>,
    /// A handled input left the surface to publish once the sync it asked
    /// for has run: the frame that runs it publishes.
    pub(super) action_surface_owed: bool,
    /// What the last publication was derived from
    /// ([`super::model_lifecycle::PublicationStamp`]); `None` when it cannot
    /// be shown current.
    pub(super) published_stamp: Option<super::model_lifecycle::PublicationStamp>,
    #[cfg(test)]
    pub(super) action_surface_publication_attempts: usize,
    /// Opaque, revision-bound continuation state for the broker directory.
    pub(super) action_directory_cursors: std::collections::HashMap<String, ActionDirectoryCursor>,
    /// Background operations admitted by the retained widget tree.
    pub(super) directed_invocations:
        std::collections::HashMap<tilo_actions::InvocationId, DirectedInvocation>,
    /// The invoker of each delivered invocation still running, for the
    /// edits its effect submits.
    pub(super) acting_for: std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::invocation_scope::Continuation,
    >,
    /// Each edit an effect submitted for its invoker that the host is still
    /// admitting, by broker request, to the effect invocation it continues.
    pub(super) follow_ups: std::collections::HashMap<u64, tilo_actions::InvocationId>,
    /// Each invocation of another provider's action a control here is bound
    /// to that the provider is still answering, by broker request.
    pub(super) hosted_requests: std::collections::HashSet<u64>,
    /// Resolutions asked for a resource still being read, answered once its
    /// read lands ([`crate::resources::PublishesResources::resource_pending`]).
    pub(super) awaited_resolutions: Vec<super::AwaitedResolution>,
    /// Answers to broker invocations held, by wire id, until what they
    /// caused in the app has landed ([`crate::wake::WakeHandle::owe`]).
    pub(super) held_answers: Vec<(u64, tilo_actions::InvocationUpdate)>,
    /// Operations started from this window rather than by the broker.
    pub(super) local_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        (tilo_actions::ActionAddress, tilo_actions::OperationKind),
    >,
    pub(super) pending_application_launches:
        std::collections::HashMap<u64, tilo_actions::InvocationId>,
    /// Requests to the session action host awaiting their answer, keyed by
    /// the answer they expect, to the local request id they correlate with.
    pub(super) broker_requests: std::collections::HashMap<BrokerReply, u64>,
    /// Invocations of this app's own actions, in the order they were made,
    /// waiting for the route to carry the surface that offers them. The host
    /// resolves an invocation against the surface it last received from this
    /// connection, so one sent ahead of that surface is refused as unknown.
    pub(super) held_own_invocations:
        std::collections::VecDeque<tilo_actions::remote::InvokeRequest>,
    /// Why the session action host last refused an invocation outright, so
    /// a fixture fails on the refusal instead of waiting out what it asked.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) refused_broker_receipt: Option<String>,
    /// `None` means this process exhausted the complete request-id namespace.
    pub(super) next_broker_request_id: Option<u64>,
    /// Dirty tracking for the hosted worker pump (no window event loop).
    pub(super) hosted: super::model_lifecycle::HostedPumpState,
}

/// Which answer from the session action host a request is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BrokerReply {
    Receipt(tilo_actions::InvocationId),
    /// The state of an invocation being observed.
    Update(tilo_actions::InvocationId),
}

impl ActionRuntime {
    pub(super) fn new() -> Self {
        Self {
            published_invocations: Default::default(),
            action_palette: None,
            palette_window: None,
            ordinary_palette_return_focus: None,
            refusals: Vec::new(),
            control_notice: None,
            search_in_host: false,
            focus_after_rebuild: None,
            action_registry: tilo_actions::ActionRegistry::new(),
            action_registry_generation: 0,
            action_transport: Box::new(tilo_actions::LocalTransport::new()),
            resource_cache: RefCell::new(None),
            bound_resource_cache: crate::bound_dispatch::BoundResourceCache::default(),
            bound_record_sync: None,
            bound_publication: crate::bound_dispatch::BoundPublication::default(),
            last_published_bound_route: None,
            refusals_logged: RefusalLog::default(),
            #[cfg(test)]
            bound_broker_route_for_test: None,
            action_directory_stamp: None,
            action_directory_snapshot: None,
            local_document_directory_pins: Vec::new(),
            bound_invocations: std::collections::HashMap::new(),
            local_resource_cache: RefCell::new(None),
            open_resource_search_cache: RefCell::new(None),
            resource_generation: Cell::new(0),
            permission_generation: Cell::new(0),
            last_control_availability: None,
            local_history_status: None,
            last_published_actions: None,
            published_surface: None,
            action_surface_owed: false,
            published_stamp: None,
            #[cfg(test)]
            action_surface_publication_attempts: 0,
            action_directory_cursors: std::collections::HashMap::new(),
            directed_invocations: std::collections::HashMap::new(),
            acting_for: std::collections::HashMap::new(),
            follow_ups: std::collections::HashMap::new(),
            hosted_requests: std::collections::HashSet::new(),
            awaited_resolutions: Vec::new(),
            held_answers: Vec::new(),
            local_invocations: std::collections::HashMap::new(),
            pending_application_launches: std::collections::HashMap::new(),
            broker_requests: std::collections::HashMap::new(),
            held_own_invocations: std::collections::VecDeque::new(),
            #[cfg(any(test, feature = "test-support"))]
            refused_broker_receipt: None,
            next_broker_request_id: Some(0),
            hosted: super::model_lifecycle::HostedPumpState::default(),
        }
    }
}

/// Accessibility state and semantic projection.
pub(super) struct AccessibilityRuntime {
    pub(super) screen_reader: Option<crate::screen_reader::ScreenReader>,
    pub(super) screen_reader_modifier_down: bool,
    /// Ctrl+Super+Enter and the two-volume-key hold, which turn the screen
    /// reader on and off (`App::handle_screen_reader_chord`).
    pub(super) screen_reader_chord: crate::screen_reader_chord::ScreenReaderChord,
    /// Told the new state after the chord toggled the screen reader; when
    /// set, it keeps the state instead of the person's settings file.
    pub(super) on_screen_reader_chord: Option<Box<dyn FnMut(bool)>>,
    /// The narrator the chord turned off, kept until the reader is turned on
    /// again: its speech backends cut an utterance off when dropped, and it
    /// is still saying that the screen reader is off.
    pub(super) parting_screen_reader: Option<crate::screen_reader::ScreenReader>,
    /// What a finger means while the screen reader is on
    /// (`App::explore_by_touch`).
    pub(super) touch_explorer: crate::screen_reader_touch::TouchExplorer,
    /// The order the screen reader walks, read from
    /// [`Self::semantic_tree`] at the revision it is keyed by.
    pub(super) reading_order: Option<(u64, Vec<crate::screen_reader::ReadingItem>)>,
    /// Counts the refreshes that changed [`Self::semantic_tree`].
    pub(super) semantic_tree_revision: u64,
    pub(super) live_region_last: std::collections::HashMap<String, String>,
    pub(super) focus_trap_active: bool,
    pub(super) focus_before_trap: Option<NodeId>,
    /// Separate from the effective theme so disabling high contrast restores
    /// the configured theme exactly.
    pub(super) base_theme: Theme,
    /// Where and for whom the window is shown, as its host said: the
    /// workspace's temperature and the person's colour, applied over
    /// [`Self::base_theme`] on every rebuild so a settings change keeps
    /// them.
    pub(super) window: crate::theme::WindowAppearance,
    pub(super) a11y: crate::a11y::A11ySettings,
    pub(super) text_scale: f32,
    pub(super) ui_scale: f32,
    /// The window's content zoom, multiplied into [`Self::ui_scale`] when
    /// the theme is built (`crate::content_zoom`).
    pub(super) zoom: crate::content_zoom::WindowZoom,
    pub(super) semantic_tree: crate::a11y_tree::SemanticTree,
    /// Whether anything [`Self::semantic_tree`] is read from may have
    /// changed since it was last refreshed: the document, a widget, the
    /// layout, focus or the declared resources. A refresh with this clear
    /// would rebuild the tree it already holds, and publishes nothing
    /// instead (`App::refresh_semantic_tree`).
    pub(super) semantic_tree_stale: bool,
    /// How many times [`Self::semantic_tree_stale`] was set, so a caller
    /// can tell whether anything marked it during some work.
    pub(super) semantic_tree_marks: u64,
    /// The refreshes that rebuilt [`Self::semantic_tree`] rather than
    /// finding nothing changed.
    pub(super) semantic_tree_builds: u64,
}

impl AccessibilityRuntime {
    pub(super) fn new(
        base_theme: Theme,
        screen_reader: Option<crate::screen_reader::ScreenReader>,
    ) -> Self {
        Self {
            screen_reader,
            screen_reader_modifier_down: false,
            screen_reader_chord: crate::screen_reader_chord::ScreenReaderChord::new(),
            on_screen_reader_chord: None,
            parting_screen_reader: None,
            touch_explorer: crate::screen_reader_touch::TouchExplorer::new(),
            reading_order: None,
            semantic_tree_revision: 0,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            base_theme,
            window: crate::theme::WindowAppearance::default(),
            a11y: crate::a11y::A11ySettings::default(),
            text_scale: 1.0,
            ui_scale: 1.0,
            zoom: crate::content_zoom::WindowZoom::default(),
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
            semantic_tree_stale: true,
            semantic_tree_marks: 0,
            semantic_tree_builds: 0,
        }
    }
}

/// Persistence and record-checkout state.
pub(super) struct PersistenceRuntime {
    pub(super) autosave: Option<app_state::DocWriter>,
    /// The live document belongs to the record selected by the root widget.
    pub(super) records: Option<crate::record::RecordSet>,
    pub(super) room_index: Option<crate::state::DocumentReplica>,
}

impl PersistenceRuntime {
    pub(super) fn new() -> Self {
        Self {
            autosave: None,
            records: None,
            room_index: None,
        }
    }
}

/// Continuation publication and deferred restore state.
pub(super) struct ContinuationRuntime {
    pub(super) owner_view: crate::collaboration::owner_view::OwnerViewRuntime,
    pub(super) last_published: Option<crate::collaboration::ContinuationMetadata>,
    /// Distinguish a queued clear from an unknown prior projection.
    pub(super) publication_initialized: bool,
    pub(super) pending_restore: Option<crate::collaboration::ContinuationMetadata>,
    /// Generic view restoration waits until layout has built the focus order.
    pub(super) pending_view: Option<crate::view::View>,
    /// Route epoch for private continuity. A new acknowledged route must
    /// republish unchanged metadata and playback through that route only.
    pub(super) last_private_route: Option<crate::collaboration::PrimaryRouteWitness>,
    pub(super) private_instance: Option<u64>,
    /// The document this window shows has left its workspace, and the
    /// window knows: it stopped editing and told its desktop once.
    pub(super) retired: bool,
    /// What the primary route last published to the session's persistent
    /// search index, so only changes cross again.
    pub(super) search_index: crate::search::index_publication::Publisher,
    /// The primary route [`Self::search_index`] published on.
    pub(super) search_index_route: Option<crate::collaboration::PrimaryRouteWitness>,
}

impl ContinuationRuntime {
    pub(super) fn new(pending_restore: Option<crate::collaboration::ContinuationMetadata>) -> Self {
        Self {
            owner_view: Default::default(),
            last_published: None,
            publication_initialized: false,
            pending_restore,
            pending_view: None,
            last_private_route: None,
            private_instance: None,
            retired: false,
            search_index: crate::search::index_publication::Publisher::default(),
            search_index_route: None,
        }
    }
}

/// Collaboration transports and local authorship identity.
pub(super) struct CollaborationRuntime {
    pub(super) manager: CollaborationManager,
    /// Keeps the room record index subscribed while `manager` follows a
    /// checked-out record route.
    pub(super) room_manager: Option<CollaborationManager>,
    pub(super) local_author: crate::permissions::UserId,
    /// A record checkout asks its new route for the current document once
    /// that route is acknowledged.
    pub(super) snapshot_on_ready: bool,
    /// On-demand paging of document op history.
    pub(super) history: crate::collaboration::DocumentHistory,
    /// What the primary route's end-to-end encrypted channel lets this
    /// device do, as last reported; `None` before the first report.
    pub(super) channel_access: Option<crate::collaboration::ChannelAccessState>,
    /// Ids in the last complete collaboration Context snapshot. This is kept
    /// separately from ephemeral pointer/caret presence so a replacement can
    /// retire departed certified members without touching unrelated local
    /// presentation state.
    pub(super) context_participants: std::collections::HashSet<crate::permissions::UserId>,
    /// What the window's status line last said about the connection: a
    /// join held up, refused or lost changes it without changing the
    /// window's model.
    pub(super) shown_sync_status: Option<String>,
}

impl CollaborationRuntime {
    pub(super) fn new(
        manager: CollaborationManager,
        local_author: crate::permissions::UserId,
    ) -> Self {
        crate::history_source::authors::read_as(&local_author.0);
        Self {
            manager,
            room_manager: None,
            local_author,
            snapshot_on_ready: false,
            history: crate::collaboration::DocumentHistory::default(),
            channel_access: None,
            context_participants: std::collections::HashSet::new(),
            shown_sync_status: None,
        }
    }
}

/// Presence publication, follow, and frame-pump state.
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "the last published presence is read only by the native event-loop runner"
    )
)]
pub(super) struct PresenceRuntime {
    pub(super) broadcast_scope: Option<PresenceKey>,
    pub(super) pointer_space: Option<crate::presence::PointerSpace>,
    pub(super) followed_peer: Option<String>,
    pub(super) last_pointer_broadcast: Option<Instant>,
    pub(super) last_focus: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    pub(super) last_location: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    pub(super) last_view: Option<crate::view::ViewSubset>,
    pub(super) broadcast_sink: Option<PresenceBroadcastSink>,
    pub(super) frame_tick: Option<FrameTick>,
}

impl PresenceRuntime {
    pub(super) fn new() -> Self {
        Self {
            broadcast_scope: None,
            pointer_space: None,
            followed_peer: None,
            last_pointer_broadcast: None,
            last_focus: None,
            last_location: None,
            last_view: None,
            broadcast_sink: None,
            frame_tick: None,
        }
    }
}

/// Stable focus projection used to delimit undo/redo history entries.
pub(super) struct HistoryRuntime {
    pub(super) stable_focus_idx: Option<usize>,
    pub(super) stable_metadata: Option<String>,
}

impl HistoryRuntime {
    pub(super) fn new() -> Self {
        Self {
            stable_focus_idx: None,
            stable_metadata: None,
        }
    }
}

/// Host window identity and fullscreen reconciliation state.
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "fullscreen state is read only by the native event-loop runner"
    )
)]
pub(super) struct WindowRuntime {
    pub(super) fullscreen_requested: bool,
    pub(super) fullscreen_active: bool,
    /// The narrowest the window's layout works at, as of the last tree
    /// build ([`crate::widget::minimum_window_width`]), and what the window
    /// system was last told.
    pub(super) minimum_width: Option<f64>,
    pub(super) minimum_width_stated: Option<f64>,
    /// The size the root widget asked for, and the size last stated to the
    /// window system.
    pub(super) preferred_size: Option<(f64, f64)>,
    pub(super) preferred_size_stated: Option<(f64, f64)>,
    pub(super) title: Option<String>,
    pub(super) app_id: Option<String>,
}

impl WindowRuntime {
    pub(super) fn new() -> Self {
        Self {
            fullscreen_requested: false,
            fullscreen_active: false,
            minimum_width: None,
            minimum_width_stated: None,
            preferred_size: None,
            preferred_size_stated: None,
            title: None,
            app_id: None,
        }
    }
}

/// The distinct publication refusals already reported. A refused surface is
/// re-checked every frame; its reason is news only the first time.
#[derive(Default)]
pub(crate) struct RefusalLog {
    seen: std::collections::HashSet<u64>,
}

impl RefusalLog {
    /// Bounded: an app that keeps producing new reasons still logs them,
    /// and memory stays flat.
    const LIMIT: usize = 256;

    /// Whether `refusal` has not been reported yet; records it as reported.
    pub(crate) fn first(&mut self, refusal: &impl std::hash::Hash) -> bool {
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
        let key = BuildHasherDefault::<DefaultHasher>::default().hash_one(refusal);
        if self.seen.contains(&key) {
            return false;
        }
        if self.seen.len() >= Self::LIMIT {
            self.seen.clear();
        }
        self.seen.insert(key)
    }
}

#[cfg(test)]
mod refusal_log_tests {
    use super::RefusalLog;

    #[test]
    fn a_refusal_repeated_every_frame_is_reported_once() {
        let mut log = RefusalLog::default();
        assert!(log.first(&("contract", "notes.page:1/open: missing handler")));
        for _ in 0..60 {
            assert!(!log.first(&("contract", "notes.page:1/open: missing handler")));
        }
        assert!(log.first(&("contract", "notes.page:2/open: missing handler")));
        for index in 0..(RefusalLog::LIMIT * 2) {
            assert!(log.first(&index), "new reasons keep being reported");
        }
    }
}
