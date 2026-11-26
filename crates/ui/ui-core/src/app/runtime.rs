//! Host-owned subsystem state for [`super::App`].
//!
//! The app remains the orchestrator: these types make ownership and
//! invalidation boundaries explicit without disconnecting actions, resources,
//! or collaboration from the retained widget tree.

#[cfg(feature = "model")]
use super::{ActionDirectoryCursor, PresenceBroadcastSink};
use super::{
    DirectedInvocation, LocalResourceCache, OpenResourceSearchCache, ResourceCache,
    ResourceCacheKey,
};
#[cfg(feature = "model")]
use crate::time::Instant;
use std::cell::{Cell, RefCell};
use taffy::{NodeId, TaffyTree};
#[cfg(feature = "model")]
use tilo_ui_base::collaboration::CollaborationManager;
#[cfg(feature = "model")]
use tilo_ui_base::presence::PresenceKey;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
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
pub(super) struct LayoutRuntime {
    pub(super) taffy: TaffyTree<()>,
    pub(super) root_node: Option<NodeId>,
    /// The node the app's root is laid out in: the window's size, as a window gives its app the
    /// size it shows. Kept apart from [`Self::root_node`], which is the widget root every reader
    /// names.
    pub(super) host_node: Option<NodeId>,
    /// The widget root node [`Self::host_node`] holds, while the host is the one in this tree.
    pub(super) host_child: Option<NodeId>,
    /// The style of the node the root is laid out in, for a root a host lays out in a node of its
    /// own: its size is replaced by the viewport.
    pub(super) host_root_style: Option<taffy::Style>,
    /// Lay the root out at its content height against unbounded height, as a scrolling host lays
    /// the document inside it; the viewport still bounds culling and rasterisation.
    pub(super) content_height: bool,
    pub(super) node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    /// Which `node_map` pointers were taken from the live tree
    /// ([`crate::app::App::layout_maps_stale`]).
    pub(super) adopted: crate::retained_widgets::Adopted,
    /// How many builds' maps were admitted ([`crate::app::App::admit_layout_maps`]).
    pub(super) admissions: u64,
    /// The mapped nodes the history sweep visits, and the admission and map
    /// size they were found for (`App::history_sweep_nodes`).
    pub(super) history_nodes: HistoryNodes,
    pub(super) layout_dirty: bool,
    /// Set when a widget reports `take_structure_dirty()`: without a taffy
    /// rebuild before the next layout pass, runtime-spawned widgets have no
    /// `NodeId` and `apply_layout` skips them.
    pub(super) structure_dirty: bool,
    /// The tree was built since it was last solved. A fresh taffy tree holds
    /// no layout at all, so the next pass solves it whatever rebuilt it: the
    /// frame, or a sync or input that rebuilt it between frames.
    pub(super) unsolved: bool,
    /// Set when the root's widgets report a change they carry out in place
    /// ([`tilo_ui_base::widget::StructureChange::Children`]): the next layout pass
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
    /// The display scale of that layout: its edges are on this scale's
    /// device pixels, so another scale is solved again even at one size.
    pub(super) last_layout_scale: Option<f32>,
    /// System chrome drawn over the window, which content stays clear of.
    pub(super) safe_area: tilo_ui_base::scale::SafeArea,
    /// The row the runner draws the app's window-bar parts in, outside a
    /// Tilo session, under the safe area's top (`native_runner::part_bar`).
    pub(super) part_bar_inset: f64,
    /// Where the root is placed: the safe area's top-left corner.
    pub(super) content_origin: vello::kurbo::Point,
    /// How many times the taffy tree was rebuilt: a host comparing two
    /// frames' widgets needs to know they are still the same ones.
    pub(super) rebuilds: u64,
    /// The [`Self::rebuilds`] and [`Self::remounts`] the last frame drew.
    /// A rebuild between one frame and the next (a state tick's, a sync's)
    /// leaves the tree new and the pixels old, and no widget reports it: a
    /// frame that sees them moved repaints the whole window.
    pub(super) drawn: (u64, u64),
    /// Whether every rebuild requested since the last one was a change the
    /// widgets describe ([`tilo_ui_base::widget::StructureChange::Described`]).
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
    pub(super) skipped_frames: tilo_ui_base::widget::SkippedFrames,
    /// The last placement that placed every widget (`None` before one): a
    /// focus move since places them all again, so a view revealing the
    /// focused widget reads where it truly stands.
    pub(super) placed_all: Option<PlacedAll>,
    /// The last placement placed only what long collections show
    /// ([`tilo_ui_base::widget::placing_again`]): a widget far from every view
    /// stands where an earlier placement put it until the next full one
    /// (`App::place_all`).
    pub(super) placed_partly: bool,
    /// The device pixels per logical pixel the last placement was laid out on: a full placement
    /// that follows outside a frame (`App::place_all`) places on the same grid, so a widget that
    /// snaps its own parts there stands them where the frame drew them.
    pub(super) placed_scale: f64,
    /// What a host that draws only its frames' damage keeps from frame to
    /// frame ([`tilo_ui_base::draw_cull`]).
    pub(super) cull: tilo_ui_base::draw_cull::Culling,
    /// What the frame's whole-tree walks last read.
    pub(super) walks: TreeWalks,
    /// What the last layout pass did with the tree ([`LayoutPass`]).
    pub(super) last_pass: LayoutPass,
    /// What the app's hosted frames did since it was made ([`FrameCounts`]).
    pub(super) frames: FrameCounts,
}

/// What one layout pass did with the retained tree: a frame held to doing no more than it must
/// reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LayoutPass {
    /// The tree was built again.
    pub(super) rebuilt: bool,
    /// Widgets changed the retained tree in place; it was not built again.
    pub(super) remounted: bool,
    /// The tree was solved.
    pub(super) measured: bool,
    /// Nodes the solve marked to be measured again, and of those the ones with no children
    /// ([`tilo_ui_base::widget::counts::LayoutCounts::marked`]).
    pub(super) marked: (u64, u64),
    /// Only what the views moved was placed: the pass placed nothing else.
    pub(super) placed_partly: bool,
}

/// What an app's hosted frames ([`super::App::surface_frame_with_damage`]) did since it was made.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FrameCounts {
    /// Frames whose layout pass completed.
    pub(super) laid_out: u64,
    /// Of those, the frames whose pass built the tree again.
    pub(super) rebuilt: u64,
    /// Frames whose model pass took up nothing that arrived and synced nothing.
    pub(super) quiet: u64,
    /// Layout nodes the frames built: every node of each tree a frame built again, and each node
    /// a change carried out in place mounted.
    pub(super) nodes_built: u64,
    /// The draw commands the frames encoded into their scenes.
    pub(super) drawn_commands: u64,
}

impl LayoutRuntime {
    pub(super) fn new() -> Self {
        Self {
            taffy: TaffyTree::new(),
            root_node: None,
            host_node: None,
            host_child: None,
            host_root_style: None,
            content_height: false,
            node_map: std::collections::HashMap::new(),
            adopted: crate::retained_widgets::Adopted::default(),
            admissions: 0,
            history_nodes: HistoryNodes::default(),
            layout_dirty: true,
            structure_dirty: false,
            unsolved: true,
            remount: false,
            remounts: 0,
            last_frame_size: None,
            last_layout_logical: None,
            last_layout_scale: None,
            safe_area: tilo_ui_base::scale::SafeArea::default(),
            part_bar_inset: 0.0,
            content_origin: vello::kurbo::Point::ORIGIN,
            rebuilds: 0,
            drawn: (0, 0),
            rebuild_described: true,
            frame_rebuilds_described: false,
            #[cfg(test)]
            undescribed_by: None,
            skipped_frames: tilo_ui_base::widget::SkippedFrames::default(),
            placed_all: None,
            placed_partly: false,
            placed_scale: 1.0,
            cull: tilo_ui_base::draw_cull::Culling::default(),
            walks: TreeWalks::default(),
            last_pass: LayoutPass::default(),
            frames: FrameCounts::default(),
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

    /// Whether the host lays the root out in a node of its own ([`Self::host_root_style`],
    /// [`Self::content_height`]).
    pub(super) fn hosts_root(&self) -> bool {
        self.host_root_style.is_some() || self.content_height
    }
}

/// Which retained widgets, declaring what, a frame's whole-tree walks read.
/// What the widgets declare moves on with the resource generation (an
/// update, a sync or an input that changed it:
/// `App::invalidate_widget_resource_cache`), the widgets themselves with a
/// rebuild or a remount, what the local actor may do with the permission
/// generation, and what the components in the tree may do with the
/// revision of the permissions their hosts share with them
/// ([`tilo_ui_base::permissions::shared_revision`]). Equal generations are the
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
    #[cfg(feature = "model")]
    pub(super) routes: Option<RouteInputs>,
    /// What the last walk for History surfaces in the tree read and found.
    #[cfg(feature = "model")]
    pub(super) history: Option<HistoryWalk>,
}

/// Every input of pushing control availability into the tree.
#[derive(PartialEq, Eq)]
pub(super) struct AvailabilityInputs {
    pub(super) tree: TreeGeneration,
    pub(super) framework_input: bool,
    pub(super) history_captures: bool,
    /// The addresses the window's History API serves.
    pub(super) history_api: std::collections::HashSet<tilo_actions::ActionAddress>,
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
#[cfg(feature = "model")]
#[derive(PartialEq)]
pub(super) struct RouteInputs {
    pub(super) tree: TreeGeneration,
    pub(super) observation: Option<tilo_ui_base::collaboration::AcceptedRoute>,
    pub(super) autosave_generation: u64,
    pub(super) document: (Vec<automerge::ChangeHash>, usize),
}

/// What the last walk for History surfaces read and found.
#[cfg(feature = "model")]
pub(super) struct HistoryWalk {
    pub(super) tree: TreeGeneration,
    pub(super) status: Option<crate::widget::WindowStatus>,
    /// The tree holds History surfaces, which the next walk looks at again
    /// when anything they read moved since: news reached one
    /// ([`crate::history_mode::news`]), the document, whether the window is
    /// narrow, or one needed a refresh.
    pub(super) embedded: bool,
    /// The window's own History found the root it registered.
    pub(super) root_valid: bool,
    /// What the surfaces read, as this walk read it.
    pub(super) surfaces: HistorySurfaceInputs,
    /// A surface needed a refresh.
    pub(super) needed: bool,
}

/// What History surfaces read besides the tree and the window's status.
#[cfg(feature = "model")]
#[derive(PartialEq)]
pub(super) struct HistorySurfaceInputs {
    pub(super) news: u64,
    pub(super) document: (Vec<automerge::ChangeHash>, usize),
    pub(super) narrow: bool,
}

#[cfg(feature = "model")]
impl HistoryWalk {
    /// Whether the surfaces this walk found need looking at again.
    pub(super) fn surfaces_owed(&self, inputs: &HistorySurfaceInputs) -> bool {
        self.embedded && (self.needed || self.surfaces != *inputs)
    }
}

/// Input modality, gesture, cursor, and tooltip state.
pub(super) struct InputRuntime {
    pub(super) modifiers: winit::event::Modifiers,
    /// Focused native configuration only; no logical key, compose or press history.
    pub(super) keyboard_layout: Option<std::sync::Arc<winit::keyboard::KeyboardLayout>>,
    pub(super) interaction: tilo_ui_base::interaction::InteractionEnvironment,
    pub(super) platform: tilo_ui_base::platform::PlatformConventions,
    pub(super) nav_gestures: tilo_ui_base::gesture::NavGestureRecognizer,
    pub(super) current_cursor: CursorIcon,
    pub(super) tooltips: crate::app_core::TooltipLayer,
    /// Pen samples on their way to the pointer events they describe.
    pub(super) pen: tilo_ui_base::pen::PenInput,
    /// The app is shown inside a host whose focus order continues past it:
    /// Tab at either end of the app's order leaves it for the host.
    pub(super) tab_order_continues_in_host: bool,
    /// Where the pointer is, which buttons it holds and where it last pressed.
    pub(super) pointer: crate::app_core::PointerPlace,
    /// Where the pointer last moved, replayed before a press or release
    /// that follows a rebuild under a still pointer.
    pub(super) pointer_replay: crate::pointer_replay::PointerReplay,
    /// Keys typed before the window's content arrived, waiting for it.
    pub(super) held: super::held_input::HeldInput,
    /// Framework chrome took the last input (a press on the window's own History) and the person
    /// has made none since: no frame in between runs a control a widget queued, because the entry
    /// it asks for seals what was queued before it instead.
    pub(super) passive_input: bool,
    /// The window is still opening: its first frame has not finished. No control a widget queued
    /// runs before it does, so a queue the window was opened with waits for the History entry
    /// that seals it. Work an app queues as it opens runs from the frame after it.
    pub(super) opening: bool,
    /// The window has the keyboard: the one a person (or a test control's
    /// activation) addresses among the app's windows.
    pub(super) window_focused: bool,
    /// Whether this window holds the keyboard: its fields keep their text and
    /// selection without it, and paint no caret ([`tilo_ui_base::caret_owner`]).
    pub(super) keyboard_held: bool,
    /// Whether the widgets last painted their carets: the window held the
    /// keyboard and nothing over them took it.
    pub(super) widget_carets: bool,
    /// [`crate::app::App::keyboard_entry_owed`].
    pub(super) keyboard_entry_owed: bool,
}

impl InputRuntime {
    pub(super) fn new() -> Self {
        Self {
            modifiers: winit::event::Modifiers::default(),
            keyboard_layout: None,
            window_focused: false,
            keyboard_held: true,
            widget_carets: true,
            keyboard_entry_owed: false,
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            platform: tilo_ui_base::platform::PlatformConventions::default(),
            nav_gestures: tilo_ui_base::gesture::NavGestureRecognizer::new(),
            current_cursor: CursorIcon::Default,
            tooltips: crate::app_core::TooltipLayer::default(),
            pen: tilo_ui_base::pen::PenInput::default(),
            tab_order_continues_in_host: false,
            pointer: crate::app_core::PointerPlace::default(),
            pointer_replay: crate::pointer_replay::PointerReplay::default(),
            held: super::held_input::HeldInput::default(),
            passive_input: false,
            opening: true,
        }
    }
}

/// Action discovery, dispatch, and invocation state.
///
/// Actions remain connected to the retained widget tree; this boundary
/// centralizes their lifecycle rather than separating them from UI
/// capabilities.
pub(super) struct ActionRuntime {
    #[cfg(feature = "model")]
    pub(super) action_palette: Option<Box<crate::action_palette::ActionPalette>>,
    /// The window the palette was opened from, when the app hosts several
    /// (the shell's tiles): the palette centres on it, not on the screen.
    #[cfg(feature = "model")]
    pub(super) palette_window: Option<vello::kurbo::Rect>,
    /// Why invocations the person made this pass did not run, and the
    /// control each came from when one is on screen: said at that control,
    /// else in the window's bar, once the pass ends
    /// ([`super::App::refuse_visibly`]).
    pub(super) refusals: Vec<(String, Option<vello::kurbo::Rect>)>,
    /// Why the invocations made here that did not run did not, the person's and the app's own
    /// alike, oldest first: what the window's host reads of them after they were said.
    pub(super) refused: RefusedLog,
    /// What stands at the control the person last used, until their next
    /// press or key ([`crate::control_notice`]).
    pub(super) control_notice: Option<crate::control_notice::ControlNotice>,
    /// The window is a tile its host lays out in its own process (the
    /// shell): searches go to the host's one search through the app's
    /// component ([`super::App::hand_search_to_session`]).
    pub(super) search_in_host: bool,
    /// The control an accessibility action focused while the same action
    /// replaced widgets: focused again once the next layout admits them.
    pub(super) focus_after_rebuild: Option<tilo_ui_base::widget::layout::CompositionFocus>,
    pub(super) action_registry: tilo_actions::ActionRegistry,
    // Independent from scheduling invalidation: mutable unbound decoder access.
    pub(super) action_registry_generation: u64,
    pub(super) action_transport: Box<dyn tilo_actions::ActionTransport>,
    pub(super) resource_cache: RefCell<Option<ResourceCache>>,
    #[cfg(feature = "model")]
    pub(super) bound_resource_cache: crate::bound_dispatch::BoundResourceCache,
    #[cfg(feature = "model")]
    pub(super) bound_document_sync: Option<super::bound_document_sync::BoundDocumentSync>,
    #[cfg(feature = "model")]
    pub(super) bound_publication: crate::bound_dispatch::BoundPublication,
    #[cfg(feature = "model")]
    pub(super) last_published_bound_route: Option<tilo_ui_base::collaboration::AcceptedRoute>,
    /// Publication refusals already logged, so a surface refused on every
    /// frame logs each distinct reason once rather than at frame rate.
    pub(super) refusals_logged: RefusalLog,
    #[cfg(feature = "model")]
    #[cfg(test)]
    pub(super) bound_broker_route_for_test: Option<tilo_ui_base::collaboration::AcceptedRoute>,
    #[cfg(feature = "model")]
    pub(super) action_directory_stamp: Option<(
        std::rc::Rc<super::action_directory_snapshot::DirectoryObservation>,
        String,
    )>,
    #[cfg(feature = "model")]
    pub(super) action_directory_snapshot:
        Option<std::rc::Rc<super::action_directory_snapshot::ActionDirectorySnapshot>>,
    /// The directory the last revision named, set aside when a pass made it
    /// stale: re-derived to the same snapshot, it keeps that revision and
    /// its cursors.
    #[cfg(feature = "model")]
    pub(super) action_directory_retired:
        Option<super::action_directory_snapshot::ModelDirectoryCandidate>,
    /// The snapshot a reader pages at a revision the app has moved past: held
    /// beside what it publishes now, so the read keeps the directory it
    /// started on whatever the app publishes meanwhile.
    #[cfg(feature = "model")]
    pub(super) action_directory_borrowed:
        Option<super::action_directory_snapshot::ModelDirectoryCandidate>,
    #[cfg(feature = "model")]
    /// Whether the window has learnt which ids more than one of its components publishes, once
    /// through a publication or a read before one ([`super::App::learn_shared_ids`]).
    pub(super) action_directory_learnt: bool,
    #[cfg(feature = "model")]
    pub(super) local_document_directory_pins:
        Vec<std::rc::Weak<super::action_directory_snapshot::LocalDocumentDirectoryPin>>,
    #[cfg(feature = "model")]
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
    #[cfg(feature = "model")]
    pub(super) published_surface: Option<tilo_ui_base::collaboration::SurfacePublication>,
    /// A handled input left the surface to publish once the sync it asked
    /// for has run: the frame that runs it publishes.
    pub(super) action_surface_owed: bool,
    /// What the last publication was derived from
    /// ([`super::model_lifecycle::PublicationStamp`]); `None` when it cannot
    /// be shown current.
    #[cfg(feature = "model")]
    pub(super) published_stamp: Option<super::model_lifecycle::PublicationStamp>,
    #[cfg(test)]
    pub(super) action_surface_publication_attempts: usize,
    /// Opaque, revision-bound continuation state for the broker directory.
    #[cfg(feature = "model")]
    pub(super) action_directory_cursors: std::collections::HashMap<String, ActionDirectoryCursor>,
    /// Background operations admitted by the retained widget tree.
    pub(super) directed_invocations:
        std::collections::HashMap<tilo_actions::InvocationId, DirectedInvocation>,
    /// The invoker of each delivered invocation still running, for the
    /// edits its effect submits.
    #[cfg(feature = "model")]
    pub(super) acting_for: std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::invocation_scope::Continuation,
    >,
    /// The person's presses waiting for what they change to arrive from the workspace.
    #[cfg(feature = "model")]
    pub(super) waiting_controls: Vec<crate::app_core::WaitingControl>,
    /// Each edit an effect submitted for its invoker that the host is still
    /// admitting, by broker request, to the effect invocation it continues and the control it
    /// came from.
    #[cfg(feature = "model")]
    pub(super) follow_ups: std::collections::HashMap<
        u64,
        (tilo_actions::InvocationId, crate::app_core::FollowUpControl),
    >,
    /// Each invocation of another provider's action a control here is bound
    /// to that the provider is still answering, by broker request.
    pub(super) hosted_requests: std::collections::HashSet<u64>,
    /// Resolutions asked for a resource still being read, answered once its
    /// read lands ([`tilo_ui_base::widget::resources::PublishesResources::resource_pending`]).
    #[cfg(feature = "model")]
    pub(super) awaited_resolutions: Vec<super::AwaitedResolution>,
    /// Answers to broker invocations held, by wire id, until what they
    /// caused in the app has landed ([`crate::wake::WakeHandle::owe`]).
    pub(super) held_answers: Vec<(u64, tilo_actions::InvocationUpdate)>,
    /// The held answers wait for a record's route still joining: the pass
    /// that finds it joined asks for the sync that answers them.
    pub(super) answers_await_route: bool,
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
    pub(super) held_own_invocations: std::collections::VecDeque<HeldOwnInvocation>,
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

/// An invocation of this app's own action waiting for the route to carry the
/// surface that offers it, and since when it waits.
pub(super) struct HeldOwnInvocation {
    pub(super) request: tilo_actions::remote::InvokeRequest,
    pub(super) since: crate::time::Instant,
    /// The input serial of the press the invocation followed, if it followed one: kept from the
    /// moment it was made, not from the pass that finally sends it.
    pub(super) press: Option<u32>,
}

impl ActionRuntime {
    /// Whether a call of a hosted component's action still runs. Only Tilo's model hosts one.
    pub(super) fn bound_calls_running(&self) -> bool {
        #[cfg(feature = "model")]
        let running = !self.bound_invocations.is_empty();
        #[cfg(not(feature = "model"))]
        let running = false;
        running
    }

    pub(super) fn new() -> Self {
        Self {
            #[cfg(feature = "model")]
            action_palette: None,
            #[cfg(feature = "model")]
            palette_window: None,
            refusals: Vec::new(),
            refused: RefusedLog::default(),
            control_notice: None,
            search_in_host: false,
            focus_after_rebuild: None,
            action_registry: tilo_actions::ActionRegistry::new(),
            action_registry_generation: 0,
            action_transport: Box::new(tilo_actions::LocalTransport::new()),
            resource_cache: RefCell::new(None),
            #[cfg(feature = "model")]
            bound_resource_cache: crate::bound_dispatch::BoundResourceCache::default(),
            #[cfg(feature = "model")]
            bound_document_sync: None,
            #[cfg(feature = "model")]
            bound_publication: crate::bound_dispatch::BoundPublication::default(),
            #[cfg(feature = "model")]
            last_published_bound_route: None,
            refusals_logged: RefusalLog::default(),
            #[cfg(feature = "model")]
            #[cfg(test)]
            bound_broker_route_for_test: None,
            #[cfg(feature = "model")]
            action_directory_stamp: None,
            #[cfg(feature = "model")]
            action_directory_snapshot: None,
            #[cfg(feature = "model")]
            action_directory_retired: None,
            #[cfg(feature = "model")]
            action_directory_borrowed: None,
            #[cfg(feature = "model")]
            action_directory_learnt: false,
            #[cfg(feature = "model")]
            local_document_directory_pins: Vec::new(),
            #[cfg(feature = "model")]
            bound_invocations: std::collections::HashMap::new(),
            local_resource_cache: RefCell::new(None),
            open_resource_search_cache: RefCell::new(None),
            resource_generation: Cell::new(0),
            permission_generation: Cell::new(0),
            last_control_availability: None,
            local_history_status: None,
            last_published_actions: None,
            #[cfg(feature = "model")]
            published_surface: None,
            action_surface_owed: false,
            #[cfg(feature = "model")]
            published_stamp: None,
            #[cfg(test)]
            action_surface_publication_attempts: 0,
            #[cfg(feature = "model")]
            action_directory_cursors: std::collections::HashMap::new(),
            directed_invocations: std::collections::HashMap::new(),
            #[cfg(feature = "model")]
            acting_for: std::collections::HashMap::new(),
            #[cfg(feature = "model")]
            waiting_controls: Vec::new(),
            #[cfg(feature = "model")]
            follow_ups: std::collections::HashMap::new(),
            hosted_requests: std::collections::HashSet::new(),
            #[cfg(feature = "model")]
            awaited_resolutions: Vec::new(),
            held_answers: Vec::new(),
            answers_await_route: false,
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
    pub(super) screen_reader: Option<tilo_ui_base::screen_reader::ScreenReader>,
    pub(super) screen_reader_modifier_down: bool,
    /// Ctrl+Super+Enter and the two-volume-key hold, which turn the screen
    /// reader on and off (`App::handle_screen_reader_chord`).
    pub(super) screen_reader_chord: tilo_ui_base::screen_reader_chord::ScreenReaderChord,
    /// Told the new state after the chord toggled the screen reader; when
    /// set, it keeps the state instead of the person's settings file.
    pub(super) on_screen_reader_chord: Option<Box<dyn FnMut(bool)>>,
    /// The narrator the chord turned off, kept until the reader is turned on
    /// again: its speech backends cut an utterance off when dropped, and it
    /// is still saying that the screen reader is off.
    pub(super) parting_screen_reader: Option<tilo_ui_base::screen_reader::ScreenReader>,
    /// What a finger means while the screen reader is on
    /// (`App::explore_by_touch`).
    pub(super) touch_explorer: tilo_ui_base::screen_reader_touch::TouchExplorer,
    /// The order the screen reader walks, read from
    /// [`Self::semantic_tree`] at the revision it is keyed by.
    pub(super) reading_order: Option<(u64, Vec<tilo_ui_base::screen_reader::ReadingItem>)>,
    /// Counts the refreshes that changed [`Self::semantic_tree`].
    pub(super) semantic_tree_revision: u64,
    pub(super) live_region_last: std::collections::HashMap<String, String>,
    pub(super) focus_trap_active: bool,
    pub(super) focus_before_trap: Option<NodeId>,
    /// The control the open trap's own entry put the keyboard on, while the person has not moved
    /// it: a rebuild seats the keyboard there again from what the modal shows now, never back
    /// onto a control the trap only chose because nothing else was laid out yet.
    pub(super) focus_trap_entry: Option<NodeId>,
    /// Separate from the effective theme so disabling high contrast restores
    /// the configured theme exactly.
    pub(super) base_theme: Theme,
    /// Where and for whom the window is shown, as its host said: the
    /// workspace's temperature and the person's colour, applied over
    /// [`Self::base_theme`] on every rebuild so a settings change keeps
    /// them.
    pub(super) window: tilo_ui_base::theme::WindowAppearance,
    pub(super) a11y: tilo_ui_base::a11y::A11ySettings,
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
    /// Physical pixels per logical pixel, as the window reports it: the scale
    /// its tree is built at for the session's export, whose bounds an
    /// assistive client reads in physical pixels.
    pub(super) export_scale: f64,
    /// The widgets' presentation epoch ([`tilo_ui_base::widget::note_resource_presentation_changed`])
    /// [`Self::semantic_tree`] was last built at: a widget that changed what it declares outside
    /// a sync, an input or a layout (a control measured where it is drawn) moves it.
    pub(super) semantic_tree_presentation: u64,
}

impl AccessibilityRuntime {
    pub(super) fn new(
        base_theme: Theme,
        screen_reader: Option<tilo_ui_base::screen_reader::ScreenReader>,
    ) -> Self {
        Self {
            screen_reader,
            screen_reader_modifier_down: false,
            screen_reader_chord: tilo_ui_base::screen_reader_chord::ScreenReaderChord::new(),
            on_screen_reader_chord: None,
            parting_screen_reader: None,
            touch_explorer: tilo_ui_base::screen_reader_touch::TouchExplorer::new(),
            reading_order: None,
            semantic_tree_revision: 0,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            focus_trap_entry: None,
            base_theme,
            window: tilo_ui_base::theme::WindowAppearance::default(),
            a11y: tilo_ui_base::a11y::A11ySettings::default(),
            text_scale: 1.0,
            ui_scale: 1.0,
            zoom: crate::content_zoom::WindowZoom::default(),
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
            semantic_tree_stale: true,
            semantic_tree_marks: 0,
            semantic_tree_builds: 0,
            export_scale: 1.0,
            semantic_tree_presentation: 0,
        }
    }
}

/// Continuation publication and deferred restore state.
#[cfg(feature = "model")]
pub(super) struct ContinuationRuntime {
    pub(super) last_published: Option<tilo_ui_base::collaboration::ContinuationMetadata>,
    /// Distinguish a queued clear from an unknown prior projection.
    pub(super) publication_initialized: bool,
    pub(super) pending_restore: Option<tilo_ui_base::collaboration::ContinuationMetadata>,
    /// Generic view restoration waits until layout has built the focus order.
    pub(super) pending_view: Option<crate::view::View>,
    /// Route epoch for private continuity. A new acknowledged route must
    /// republish unchanged metadata and playback through that route only.
    pub(super) last_private_route: Option<tilo_ui_base::collaboration::AcceptedRoute>,
    pub(super) private_instance: Option<u64>,
    /// What the primary route last published to the session's persistent
    /// search index, so only changes cross again.
    pub(super) search_index: crate::search::index_publication::Publisher,
    /// The primary route [`Self::search_index`] published on.
    pub(super) search_index_route: Option<tilo_ui_base::collaboration::AcceptedRoute>,
}

#[cfg(feature = "model")]
impl ContinuationRuntime {
    pub(super) fn new(
        pending_restore: Option<tilo_ui_base::collaboration::ContinuationMetadata>,
    ) -> Self {
        Self {
            last_published: None,
            publication_initialized: false,
            pending_restore,
            pending_view: None,
            last_private_route: None,
            private_instance: None,
            search_index: crate::search::index_publication::Publisher::default(),
            search_index_route: None,
        }
    }
}

/// Collaboration transports and local authorship identity.
#[cfg(feature = "model")]
pub(super) struct CollaborationRuntime {
    pub(super) manager: CollaborationManager,
    /// What the primary route's end-to-end encrypted channel lets this
    /// device do, as last reported; `None` before the first report.
    pub(super) key_access: Option<tilo_ui_base::collaboration::KeyAccessState>,
    /// What the window's status line last said about the connection: a
    /// join held up, refused or lost changes it without changing the
    /// window's model.
    pub(super) shown_sync_status: Option<String>,
    /// Action requests the window's clients made while `manager` was still joining its primary
    /// route, which carries nothing else yet: sent, in order, once it is ready. Dropped, an
    /// invocation would wait for an answer that never comes.
    pub(super) held_requests:
        std::collections::VecDeque<tilo_ui_base::collaboration::CollabRequest>,
    /// How the window takes up what `manager`'s workspace sends it.
    pub(super) delivery: super::WorkspaceDelivery,
    /// The store read of the workspace's members the window is waiting for, by its request id:
    /// who the members are is the workspace's `workspace/members` resource, read through the
    /// store, never carried by the context frame.
    pub(super) members_request: Option<u64>,
    /// The id the next read of the members' resource goes under.
    pub(super) next_members_request: u64,
    /// Whether the members' read is still to be asked: the connection was joining its primary
    /// route when the context named it, and the window asks again.
    pub(super) members_waiting: bool,
}

#[cfg(feature = "model")]
impl CollaborationRuntime {
    pub(super) fn new(
        manager: CollaborationManager,
        delivery: super::WorkspaceDelivery,
        local_author: &tilo_actions::MemberId,
    ) -> Self {
        crate::history_source::authors::read_as(local_author);
        Self {
            manager,
            delivery,
            key_access: None,
            shown_sync_status: None,
            held_requests: std::collections::VecDeque::new(),
            members_request: None,
            next_members_request: 1,
            members_waiting: false,
        }
    }
}

/// Presence publication, follow, and frame-pump state.
#[cfg(feature = "model")]
pub(super) struct PresenceRuntime {
    pub(super) broadcast_scope: Option<PresenceKey>,
    pub(super) pointer_space: Option<tilo_ui_base::presence::PointerSpace>,
    pub(super) followed_peer: Option<tilo_ui_base::permissions::MemberId>,
    pub(super) last_pointer_broadcast: Option<Instant>,
    pub(super) last_focus: Option<(PresenceKey, tilo_ui_base::collaboration::PresencePayload)>,
    pub(super) last_location: Option<(PresenceKey, tilo_ui_base::collaboration::PresencePayload)>,
    /// What the app last said of the draft its person writes.
    pub(super) composing: tilo_ui_base::presence::DraftSaid,
    pub(super) last_view: Option<crate::view::ViewSubset>,
    pub(super) broadcast_sink: Option<PresenceBroadcastSink>,
}

#[cfg(feature = "model")]
impl PresenceRuntime {
    pub(super) fn new() -> Self {
        Self {
            broadcast_scope: None,
            pointer_space: None,
            followed_peer: None,
            last_pointer_broadcast: None,
            last_focus: None,
            last_location: None,
            composing: tilo_ui_base::presence::DraftSaid::default(),
            last_view: None,
            broadcast_sink: None,
        }
    }
}

/// Stable focus projection used to delimit undo/redo history entries.
#[cfg(feature = "model")]
pub(super) struct HistoryRuntime {
    pub(super) stable_focus_idx: Option<usize>,
    pub(super) stable_metadata: Option<String>,
}

#[cfg(feature = "model")]
impl HistoryRuntime {
    pub(super) fn new() -> Self {
        Self {
            stable_focus_idx: None,
            stable_metadata: None,
        }
    }
}

/// Host window identity and fullscreen reconciliation state.
pub(super) struct WindowRuntime {
    pub(super) fullscreen_requested: bool,
    pub(super) fullscreen_active: bool,
    /// The narrowest the window's layout works at, as of the last tree
    /// build ([`tilo_ui_base::widget::minimum_window_width`]), and what the window
    /// system was last told.
    pub(super) minimum_width: Option<f64>,
    pub(super) minimum_width_stated: Option<f64>,
    /// The size the root widget asked for, and the size last stated to the
    /// window system.
    pub(super) preferred_size: Option<(f64, f64)>,
    pub(super) preferred_size_stated: Option<(f64, f64)>,
    pub(super) title: Option<String>,
    /// The title the window system was last given ([`crate::App::refresh_window_title`]): what it
    /// names the window by now.
    pub(super) shown_title: Option<String>,
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
            shown_title: None,
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

/// Why the invocations made in a window did not run, oldest first ([`ActionRuntime::refused`]).
#[derive(Default)]
pub(crate) struct RefusedLog {
    reasons: std::collections::VecDeque<String>,
}

impl RefusedLog {
    /// Bounded: a window refused again and again keeps its newest reasons, and memory stays
    /// flat.
    const LIMIT: usize = 256;

    pub(crate) fn push(&mut self, reason: String) {
        if self.reasons.len() >= Self::LIMIT {
            self.reasons.pop_front();
        }
        self.reasons.push_back(reason);
    }

    pub(crate) fn reasons(&self) -> impl Iterator<Item = &str> {
        self.reasons.iter().map(String::as_str)
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
