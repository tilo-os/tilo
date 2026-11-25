//! What this desktop's session provides of the person's devices: what each keeps in their store
//! ([`store`]), what each says live on its session with this one ([`live`]), and the link that
//! carries both ([`session`]).

mod live;
mod session;
mod store;

pub use session::Continuity;
pub(crate) use session::{Reach, SessionFabric, about};
pub(crate) use store::{ABOUT, decoded};

pub use tilo_network_actor::fabric::clock;
pub use tilo_network_actor::fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
pub use tilo_network_actor::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector,
};
pub use tilo_network_actor::fabric::unlock::{
    AutoApproval, Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS, MergedUnlock, UnlockDecision,
    UnlockDecisionEntry, UnlockEnrolment, UnlockFabricState, UnlockSibling, UnlockView,
    decision_for, merged_unlock_state,
};
pub use tilo_network_actor::fabric::{
    ClipboardEntry, ClipboardPayload, DeviceAbout, DeviceKind, FabricCapabilities, FabricError,
    MAX_CLIPBOARD_ITEMS, MAX_FABRIC_ACTION_AGE_MS, MediaSourceLocator, PersonDevice, PersonDevices,
    PersonalFabricAction, PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS,
    REMOTE_AUDIO_FRAME_SAMPLES, REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ,
};
