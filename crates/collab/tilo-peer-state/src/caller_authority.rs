//! Who a host-committed command is made for: authority the host retained,
//! never decoded from action arguments.

use serde::{Deserialize, Serialize};

/// Authority retained by the host, never decoded from action arguments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallerAuthority {
    Person {
        caller: tilo_user_identity::DeviceCert,
    },
    Host {
        attestor: tilo_user_identity::DeviceCert,
    },
    Agent {
        attestor: tilo_user_identity::DeviceCert,
        agent_id: String,
    },
}

impl CallerAuthority {
    #[must_use]
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        match self {
            Self::Person { caller } => caller,
            Self::Host { attestor } | Self::Agent { attestor, .. } => attestor,
        }
    }
}
