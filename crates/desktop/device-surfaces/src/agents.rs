//! The agent sessions the device publishes.

/// The agent sessions, as `session:agents` publishes them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Agents {
    pub sessions: Vec<Agent>,
}

/// One agent session.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Agent {
    /// The agent session.
    pub key: String,
    pub name: String,
    /// `running`, `waiting` (for the person) or `done`.
    pub state: String,
    pub doing: String,
    pub mark: String,
    pub unseen: i64,
    /// The environment it works in, when it came in by an environment's own server: what the
    /// person calls that environment. Empty for a session that came in by the session's own.
    pub environment: String,
    /// That environment's id on this device, `<workspace>/<record>`, as the environment's own
    /// programs read it.
    pub environment_id: String,
}
