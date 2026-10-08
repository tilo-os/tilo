//! A running projection: the detached `_session` child `connect` starts and
//! `disconnect` stops.

pub(crate) mod cast;
pub(crate) mod external;
pub(crate) mod hls;
pub(crate) mod paths;

use std::time::Duration;

pub(crate) const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(50);
