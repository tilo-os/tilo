//! `BlueZ` over D-Bus: discovery, pairing, trust, removal — and the pairing
//! agent that lets the user see and confirm a passkey.
//!
//! # Why this is not a `bluetoothctl` wrapper
//!
//! Two parts of Bluetooth pairing cannot be expressed by shelling out.
//!
//! 1. **The agent.** Pairing is a conversation. `BlueZ` calls *into* the
//!    desktop — "these six digits, do they match what the headphones are
//!    showing?" — and holds the pairing open until that D-Bus method returns.
//!    An agent is therefore an object with an address on the bus, not a
//!    command that can be run.
//! 2. **Discovery is bound to a connection.** `StartDiscovery` scans for as
//!    long as the calling D-Bus connection lives. A CLI invocation exits
//!    immediately, taking the scan with it.
//!
//! # Shape
//!
//! - [`model`] — pure types and rules, testable with no bus at all.
//! - [`agent`] — the `org.bluez.Agent1` object `BlueZ` calls back into. It
//!   decides nothing; it turns each question into a [`PairingRequest`] for
//!   whoever owns the screen and turns the [`PairingReply`] back into a
//!   return value or a D-Bus error.
//! - [`client`] — adapter and device operations.
//!
//! # Who draws the prompt
//!
//! The compositor, not an app. A pairing prompt authorises a device to send
//! input events and read audio; it is exactly the kind of decision an
//! untrusted application must not be able to fake. The agent hands its
//! questions to OS-owned chrome and nowhere else.
//!
//! # Refusal is the default
//!
//! Every path that cannot get an answer — no UI attached, prompt dismissed,
//! reply that does not fit the question, nobody answering in time — returns a
//! D-Bus error to `BlueZ`. Silence never becomes consent.

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
