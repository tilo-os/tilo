//! The portable desktop surface shared by every host that can run the portable `tilo-ui` build.
//! [`surface::DesktopSurface`] presents a `tilo-desktop-model` tree with the toolkit's own desktop
//! widgets (`GroupRow`, `WindowGroup` , `TileHost` ) and folds gestures back into the tree through
//! the model's shared topology operations. [`navigation`] is the `DesktopNavigationV1` contract a
//! host page consumes. What a tab shows is the host's choice ([`surface::TileSource`]): the browser
//! mounts in-process application components; the native shell keeps compositor surfaces and its own
//! chrome.
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard; layout tests pin exact computed positions"
    )
)]

pub mod activity;
pub mod drill_in;
pub mod floating_layer;
pub mod history_log;
pub mod host;
pub mod i18n;
pub mod navigation;
pub mod notifications;
pub mod now_playing;
pub mod outcome;
pub mod session;
mod shared_tree;
pub mod surface;
mod wake;
pub mod warmth;
pub mod window;

#[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
pub use surface::peer_view::{
    NeighbourMsg, PeerAction, PeerId, PeerOutcome, PeerRead, PeerStreamStart, PeerSurface,
    PeerSurfaces, PeerTiles, PeerView, PictureWake, Standing, SurfaceId,
};
pub use surface::slide::Slides;
pub use surface::{
    ActivityModel, ActivityPaneCode, AppInbox, AppNotifier, DesktopHandle, DesktopNotifications,
    DesktopSurface, HistorySource, HostAction, HostActionSource, LivenessChoice,
    NotificationOutcome, OpenActivity, PALETTE_EDGE, PersonalAsk, SearchHost, SearchHostFeed,
    SearchRow, TileSource, TypedField, TypedInsertion, TypedRefusal, host_action_key, is_escape,
    is_host_offer, is_search_chord, open_activity, published_selection, search_chord_keys,
    seed_tree, tile_app_kind,
};
