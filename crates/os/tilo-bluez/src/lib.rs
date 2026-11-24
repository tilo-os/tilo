//! `BlueZ` over D-Bus: discovery, pairing, trust, removal — and the pairing
//! agent that lets the user see and confirm a passkey.
//!
//! Two parts of Bluetooth pairing cannot be expressed by shelling out.
//!
//! The compositor, not an app. A pairing prompt authorises a device to send
//! input events and read audio; it is exactly the kind of decision an
//! untrusted application must not be able to fake. The agent hands its
//! questions to OS-owned chrome and nowhere else.

pub mod agent;
pub mod client;
pub mod model;

pub use agent::{
    AGENT_CAPABILITY, AGENT_EVENT_CAPACITY, AGENT_PATH, AgentEvent, AgentEvents, AgentRegistration,
    DEFAULT_ANSWER_TIMEOUT, DeviceNames, MAX_DEVICE_NAME_BYTES, MAX_DEVICE_NAME_ENTRIES,
    MAX_DEVICE_PATH_BYTES, MAX_PIN_CODE_BYTES, MAX_SERVICE_UUID_BYTES, PairingAgent,
    PairingReplySender,
};
pub use client::{BluezChanges, BluezClient, DISCOVERY_WINDOW, PairOutcome, error_text};
pub use model::{
    AdapterInfo, DeviceInfo, PairingPrompt, PairingReply, PairingRequest, adapter_from_interfaces,
    address_from_path, device_from_interfaces, path_for_address, reply_answers, sort_devices,
};
