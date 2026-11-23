//! What this desktop's session provides of the person's devices: what each keeps in their store
//! ([`store`]), what each says live on its session with this one ([`live`]), and the link that
//! carries both ([`session`]).

mod live;
mod session;
mod store;

pub(crate) use session::{Reach, SessionFabric};
pub use tilo_continuity::Switches as Continuity;

pub use tilo_continuity::clipboard::{ClipboardEntry, ClipboardPayload, MAX_CLIPBOARD_ITEMS};
pub use tilo_continuity::notifications::{
    ContinuityNotificationProjection, MAX_CONTINUITY_NOTIFICATION_TTL_MS, NotificationId,
};
pub use tilo_continuity::secrets::open_wifi_credential;
pub use tilo_continuity::wifi::WifiCredential;
pub use tilo_fabric::unlock::{
    AutoApproval, DEVICE_UNLOCK_ACTION, Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS,
    MergedUnlock, UnlockDecision, UnlockDecisionEntry, UnlockEnrolment, UnlockFabricState,
    UnlockSibling, UnlockView, decision_for, merged_unlock_state,
};
pub use tilo_fabric::wake::{self, WakeEvent, WakeOutcome};
pub use tilo_fabric::{
    DeviceAbout, DeviceDisplay, DeviceKind, DevicePresence, FabricCapabilities, FabricError,
    Ipv4Subnet, MAX_FABRIC_ACTION_AGE_MS, MAX_WAKE_ROUTES, OWNER_SESSION_ACTIVE_STEP_MS,
    OwnerSession, WakeInterfaceKind, WakeRoute,
};
pub use tilo_network_actor::fabric::clock;
pub use tilo_network_actor::fabric::link::{
    FabricLink, NearbySpoolOffer, PersonalFabricSendError, SharedLink,
};
pub use tilo_network_actor::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
};
pub use tilo_network_actor::fabric::{
    CarriedDrag, CarriedOffer, FabricInputEvent, InputLease, MAX_INPUT_LEASE_MS,
    MediaSourceLocator, PersonDevice, PersonDevices, PersonalFabricAction,
    PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS, REMOTE_AUDIO_FRAME_SAMPLES,
    REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ, ScrollUnit,
};
