//! Transport-neutral application contract for active realtime-session
//! handoff. The contract is defined in [`collab_types::realtime`], where the
//! session's services read it without the toolkit.

pub use collab_types::realtime::{
    MAX_CAPTURE_AUTHORIZATIONS, MAX_REALTIME_TOKEN_BYTES, MAX_REALTIME_WIRE_BYTES,
    OPAQUE_CAPTURE_AUTHORIZATION_BYTES, RealtimeCommand, RealtimeControlIntent, RealtimeEvent,
    RealtimeMediaFacets, RealtimeReply, RealtimeResumeIntent,
};
