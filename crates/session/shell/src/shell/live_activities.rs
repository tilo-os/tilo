//! The live activities apps show outside their windows (Clocks' running
//! timer), as this session presents them: the board every app's channel
//! writes its activity to and takes the controls pressed on it from.

use std::sync::{Arc, OnceLock};

/// The session's board of live activities.
pub fn board() -> Arc<runtime_collab::LiveActivities> {
    static BOARD: OnceLock<Arc<runtime_collab::LiveActivities>> = OnceLock::new();
    Arc::clone(BOARD.get_or_init(Arc::default))
}
