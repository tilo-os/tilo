//! This device's MCP server (`mcp_server`): whether an agent on another of
//! the person's own devices works here without the person being asked here,
//! and whether its steps then go unasked too. The session reads it when such
//! an agent connects and when it asks; it stays on this device, so a box
//! nobody sits at can say yes for itself.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The MCP server's setting on this device.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct McpServerDevice {
    /// Accept agents from my other devices: an agent that connects from
    /// another of your own devices works here without asking you on this
    /// one. Off unless you turn it on.
    #[serde(default)]
    #[model(readonly)]
    pub accept_own_devices: bool,
    /// Let them act without asking: a step such an agent would ask you about
    /// here is allowed without the question. Only while agents from your
    /// other devices are accepted; off unless you turn it on.
    #[serde(default)]
    #[model(readonly)]
    pub act_without_asking: bool,
}

impl ui::storage::SettingsSchema for McpServerDevice {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::mcp_server::SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.mcp-server")]
impl McpServerDevice {
    /// Accept agents from my other devices without asking on this one, or
    /// ask again.
    #[action(id = "set-accept-own-devices")]
    pub fn set_accept_own_devices(&mut self, on: bool) {
        self.accept_own_devices = on;
    }

    /// Let agents from my other devices take their steps here without
    /// asking, or ask again.
    #[action(id = "set-act-without-asking")]
    pub fn set_act_without_asking(&mut self, on: bool) {
        self.act_without_asking = on;
    }
}
