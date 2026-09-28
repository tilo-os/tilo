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

/// Retained layout and widget lookup state.
pub(super) struct LayoutRuntime {
    pub(super) taffy: TaffyTree<()>,
    pub(super) root_node: Option<NodeId>,
    pub(super) node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    pub(super) layout_dirty: bool,
    /// Set when a widget reports `take_structure_dirty()`: without a taffy
    /// rebuild before the next layout pass, runtime-spawned widgets have no
    /// `NodeId` and `apply_layout` skips them.
    pub(super) structure_dirty: bool,
    /// Diffed against `inner_size()` to catch a resize without a `Resized` event.
    pub(super) last_frame_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// Last settled logical viewport; scale changes also require a solve.
    pub(super) last_layout_logical: Option<(f64, f64)>,
}

impl LayoutRuntime {
    pub(super) fn new() -> Self {
        Self {
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            layout_dirty: true,
            structure_dirty: false,
            last_frame_size: None,
            last_layout_logical: None,
        }
    }
}

/// Input modality, gesture, cursor, and tooltip state.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub(super) struct InputRuntime {
    pub(super) modifiers: winit::event::Modifiers,
    pub(super) interaction: crate::interaction::InteractionEnvironment,
    pub(super) platform: crate::platform::PlatformConventions,
    pub(super) nav_gestures: crate::gesture::NavGestureRecognizer,
    pub(super) current_cursor: CursorIcon,
    pub(super) tooltip_host: crate::widget::TooltipHost,
    /// Where the pointer last was, in widget coordinates: a press carries no
    /// position of its own.
    pub(super) last_pointer: Option<vello::kurbo::Point>,
}

impl InputRuntime {
    pub(super) fn new() -> Self {
        Self {
            modifiers: winit::event::Modifiers::default(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            platform: crate::platform::PlatformConventions::default(),
            nav_gestures: crate::gesture::NavGestureRecognizer::new(),
            current_cursor: CursorIcon::Default,
            tooltip_host: crate::widget::TooltipHost::new(),
            last_pointer: None,
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
    pub(super) action_palette: Option<crate::action_palette::ActionPalette>,
    pub(super) action_registry: tilo_actions::ActionRegistry,
    // Independent from scheduling invalidation: mutable unbound decoder access.
    pub(super) action_registry_generation: u64,
    pub(super) action_transport: Box<dyn tilo_actions::ActionTransport>,
    /// The local transport is usable only until an authoritative host transport
    /// has been installed. Legacy document controls use this to avoid treating
    /// a host-backed action as a process-local fallback.
    pub(super) legacy_local_transport: bool,
    pub(super) resource_cache: RefCell<Option<ResourceCache>>,
    pub(super) bound_resource_cache: crate::bound_dispatch::BoundResourceCache,
    pub(super) bound_record_sync: Option<super::bound_record_sync::BoundRecordSync>,
    pub(super) bound_publication: crate::bound_dispatch::BoundPublication,
    pub(super) last_published_bound_route: Option<crate::collaboration::PrimaryRouteWitness>,
    #[cfg(test)]
    pub(super) bound_broker_route_for_test: Option<crate::collaboration::PrimaryRouteWitness>,
    pub(super) action_directory_stamp: Option<(
        std::rc::Rc<super::action_directory_snapshot::DirectoryWitness>,
        String,
    )>,
    pub(super) action_directory_snapshot:
        Option<std::rc::Rc<super::action_directory_snapshot::ActionDirectorySnapshot>>,
    pub(super) bound_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::bound_dispatch::BoundActionTarget,
    >,
    pub(super) local_resource_cache: RefCell<Option<LocalResourceCache>>,
    pub(super) open_resource_search_cache: RefCell<Option<OpenResourceSearchCache>>,
    pub(super) resource_generation: Cell<u64>,
    pub(super) permission_generation: Cell<u64>,
    pub(super) last_control_availability: Option<(ResourceCacheKey, u64)>,
    pub(super) last_published_actions: Option<Vec<tilo_actions::ActionDescriptor>>,
    #[cfg(test)]
    pub(super) action_surface_publication_attempts: usize,
    /// Opaque, revision-bound continuation state for the broker directory.
    pub(super) action_directory_cursors: std::collections::HashMap<String, ActionDirectoryCursor>,
    /// Background operations admitted by the retained widget tree.
    pub(super) directed_invocations:
        std::collections::HashMap<tilo_actions::InvocationId, DirectedInvocation>,
    /// Terminals waiting for a joining route to carry their changes.
    pub(super) held_terminals: Vec<crate::app::HeldTerminal>,
    /// Operations started from this window rather than by the broker.
    pub(super) local_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        (tilo_actions::ActionAddress, tilo_actions::OperationKind),
    >,
    pub(super) pending_application_launches:
        std::collections::HashMap<u64, tilo_actions::InvocationId>,
    /// The app action behind each forwarded workspace request, so the widget
    /// that published it learns how the workspace answered.
    pub(super) workspace_request_sources:
        std::collections::HashMap<u64, tilo_actions::ActionAddress>,
    /// Requests to the session action host awaiting their answer, keyed by
    /// the answer they expect, to the local request id they correlate with.
    pub(super) broker_requests: std::collections::HashMap<BrokerReply, u64>,
    /// `None` means this process exhausted the complete request-id namespace.
    pub(super) next_broker_request_id: Option<u64>,
    /// A person's control activations still waiting for their terminal
    /// `Interaction` phase, keyed by the invocation that will report it.
    pub(super) human_interactions:
        std::collections::HashMap<tilo_actions::InvocationId, tilo_actions::SemanticInteraction>,
    /// Dirty tracking for the hosted worker pump (no window event loop).
    pub(super) hosted: super::model_lifecycle::HostedPumpState,
}

/// Which answer from the session action host a request is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BrokerReply {
    /// The receipt for an invocation.
    Receipt(tilo_actions::InvocationId),
    /// The state of an invocation being observed.
    Update(tilo_actions::InvocationId),
}

impl ActionRuntime {
    pub(super) fn new() -> Self {
        Self {
            published_invocations: Default::default(),
            action_palette: None,
            action_registry: tilo_actions::ActionRegistry::new(),
            action_registry_generation: 0,
            action_transport: Box::new(tilo_actions::LocalTransport::new()),
            legacy_local_transport: true,
            resource_cache: RefCell::new(None),
            bound_resource_cache: crate::bound_dispatch::BoundResourceCache::default(),
            bound_record_sync: None,
            bound_publication: crate::bound_dispatch::BoundPublication::default(),
            last_published_bound_route: None,
            #[cfg(test)]
            bound_broker_route_for_test: None,
            action_directory_stamp: None,
            action_directory_snapshot: None,
            bound_invocations: std::collections::HashMap::new(),
            local_resource_cache: RefCell::new(None),
            open_resource_search_cache: RefCell::new(None),
            resource_generation: Cell::new(0),
            permission_generation: Cell::new(0),
            last_control_availability: None,
            last_published_actions: None,
            #[cfg(test)]
            action_surface_publication_attempts: 0,
            action_directory_cursors: std::collections::HashMap::new(),
            directed_invocations: std::collections::HashMap::new(),
            held_terminals: Vec::new(),
            local_invocations: std::collections::HashMap::new(),
            pending_application_launches: std::collections::HashMap::new(),
            workspace_request_sources: std::collections::HashMap::new(),
            broker_requests: std::collections::HashMap::new(),
            next_broker_request_id: Some(0),
            human_interactions: std::collections::HashMap::new(),
            hosted: super::model_lifecycle::HostedPumpState::default(),
        }
    }
}

/// Accessibility state and semantic projection.
pub(super) struct AccessibilityRuntime {
    pub(super) screen_reader: Option<crate::screen_reader::ScreenReader>,
    pub(super) screen_reader_modifier_down: bool,
    pub(super) live_region_last: std::collections::HashMap<String, String>,
    pub(super) focus_trap_active: bool,
    pub(super) focus_before_trap: Option<NodeId>,
    /// Separate from the effective theme so disabling high contrast restores
    /// the configured theme exactly.
    pub(super) base_theme: Theme,
    pub(super) a11y: crate::a11y::A11ySettings,
    pub(super) text_scale: f32,
    pub(super) ui_scale: f32,
    pub(super) semantic_tree: crate::a11y_tree::SemanticTree,
    /// Whether a frame was drawn since the semantic tree was last compared,
    /// when that was, and how many changes have been reported.
    pub(super) semantic_watch: SemanticWatch,
}

pub(super) struct SemanticWatch {
    pub(super) dirty: bool,
    pub(super) last_check: Option<crate::time::Instant>,
    pub(super) generation: u64,
    /// The tree revision last reported, `None` before the first report.
    pub(super) reported_revision: Option<u64>,
}

impl Default for SemanticWatch {
    /// Dirty from the start: the first tree an app has is news to a watcher.
    fn default() -> Self {
        Self {
            dirty: true,
            last_check: None,
            generation: 0,
            reported_revision: None,
        }
    }
}

impl AccessibilityRuntime {
    pub(super) fn new(
        base_theme: Theme,
        screen_reader: Option<crate::screen_reader::ScreenReader>,
    ) -> Self {
        Self {
            screen_reader,
            screen_reader_modifier_down: false,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            base_theme,
            a11y: crate::a11y::A11ySettings::default(),
            text_scale: 1.0,
            ui_scale: 1.0,
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
            semantic_watch: SemanticWatch::default(),
        }
    }
}

/// Persistence and record-checkout state.
pub(super) struct PersistenceRuntime {
    pub(super) autosave: Option<crate::persist::DocWriter>,
    /// The live document belongs to the record selected by the root widget.
    pub(super) records: Option<crate::record::RecordSet>,
    pub(super) room_index: Option<crate::state::DocumentReplica>,
    /// The index heads widgets were last told about.
    pub(super) room_index_seen: Option<Vec<automerge::ChangeHash>>,
}

impl PersistenceRuntime {
    pub(super) fn new() -> Self {
        Self {
            autosave: None,
            records: None,
            room_index: None,
            room_index_seen: None,
        }
    }
}

/// Continuation publication and deferred restore state.
pub(super) struct ContinuationRuntime {
    pub(super) last_published: Option<crate::collaboration::ContinuationMetadata>,
    pub(super) pending_restore: Option<crate::collaboration::ContinuationMetadata>,
    /// Generic view restoration waits until layout has built the focus order.
    pub(super) pending_view: Option<crate::view::View>,
    /// Route epoch for private continuity. A new acknowledged route must
    /// republish unchanged metadata and playback through that route only.
    pub(super) last_private_route: Option<crate::collaboration::PrimaryRouteWitness>,
}

impl ContinuationRuntime {
    pub(super) fn new(pending_restore: Option<crate::collaboration::ContinuationMetadata>) -> Self {
        Self {
            last_published: None,
            pending_restore,
            pending_view: None,
            last_private_route: None,
        }
    }
}

/// Collaboration transports and local authorship identity.
pub(super) struct CollaborationRuntime {
    pub(super) manager: CollaborationManager,
    /// The room's unqualified route, which carries its record index, once
    /// the primary route has moved to a record over the same connection.
    pub(super) room_route: Option<crate::collaboration::RouteSpec>,
    pub(super) local_author: crate::permissions::UserId,
    /// A record checkout asks its new route for the current document once
    /// that route is acknowledged.
    pub(super) snapshot_on_ready: bool,
    /// A writer's record checkout leaves the record's whole state in the
    /// room once its new route is acknowledged.
    pub(super) publish_on_ready: bool,
    /// On-demand paging of document op history.
    pub(super) history: crate::collaboration::DocumentHistory,
    /// What the primary route's end-to-end encrypted channel lets this
    /// device do, as last reported; `None` before the first report.
    pub(super) channel_access: Option<crate::collaboration::ChannelAccessState>,
}

impl CollaborationRuntime {
    pub(super) fn new(
        manager: CollaborationManager,
        local_author: crate::permissions::UserId,
    ) -> Self {
        Self {
            manager,
            room_route: None,
            local_author,
            snapshot_on_ready: false,
            publish_on_ready: false,
            history: crate::collaboration::DocumentHistory::default(),
            channel_access: None,
        }
    }
}

/// Presence publication, follow, and frame-pump state.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub(super) struct PresenceRuntime {
    pub(super) broadcast_scope: Option<PresenceKey>,
    pub(super) pointer_space: Option<crate::presence::PointerSpace>,
    pub(super) followed_peer: Option<String>,
    pub(super) follow_mode: crate::view::FollowMode,
    /// Owners of the overlays open only because a live follow mirrored them.
    pub(super) mirrored_overlays: Vec<String>,
    /// A document change arrived while following: the followed View names
    /// things in the document (a menu for a page, a scroll into a list), so
    /// it is applied again once the widgets have synced to that change.
    pub(super) follow_after_sync: bool,
    /// Where a replay's recorded participant last had their caret or
    /// selection, which the viewer stands at as their own.
    pub(super) stood_at: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    /// The semantic tree revision at which standing there last found no
    /// widget to take it: it is tried again only once the tree has changed.
    pub(super) stood_missed_at: Option<u64>,
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
            follow_mode: crate::view::FollowMode::Live,
            mirrored_overlays: Vec::new(),
            follow_after_sync: false,
            stood_at: None,
            stood_missed_at: None,
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
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub(super) struct WindowRuntime {
    pub(super) fullscreen_requested: bool,
    pub(super) fullscreen_active: bool,
    pub(super) title: Option<String>,
    pub(super) app_id: Option<String>,
}

impl WindowRuntime {
    pub(super) fn new() -> Self {
        Self {
            fullscreen_requested: false,
            fullscreen_active: false,
            title: None,
            app_id: None,
        }
    }
}
