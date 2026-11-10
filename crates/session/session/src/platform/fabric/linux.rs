//! What this desktop's session provides of the fabric: the link over its peer lane and what it carries.

mod session;

pub use session::Continuity;
pub(crate) use session::SessionFabric;

pub use tilo_device_fabric::link::{
    FabricLink, NearbySpoolOffer, PersonalFabricSendError, SharedLink,
};
pub use tilo_device_fabric::notification::{
    ContinuityNotificationProjection, MAX_CONTINUITY_NOTIFICATION_TTL_MS,
};
pub use tilo_device_fabric::runtime::FabricRuntime;
pub use tilo_device_fabric::secrets::open_wifi_credential;
pub use tilo_device_fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
};
pub use tilo_device_fabric::unlock::{
    DEVICE_UNLOCK_ACTION, Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS, MergedUnlock,
    UnlockDecision, UnlockDecisionRecord, UnlockEnrolment, UnlockFabricState, UnlockSibling,
    UnlockView, decision_for, merged_unlock_state,
};
pub use tilo_device_fabric::wake::WakeEvent;
pub use tilo_device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceFabric, DeviceFabricSnapshot, DeviceKind,
    DevicePresence, FABRIC_SCHEMA_VERSION, FabricCapabilities, Ipv4Subnet, MAX_WAKE_ROUTES,
    MediaSourceLocator, OWNER_SESSION_ACTIVE_STEP_MS, OwnerSession, PersonalFabricAction,
    PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS, REMOTE_AUDIO_FRAME_SAMPLES,
    REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ, RoamingSetting, WakeInterfaceKind,
    WakeOutcome, WakeRoute,
};
pub use tilo_device_fabric::{clock, wake};
