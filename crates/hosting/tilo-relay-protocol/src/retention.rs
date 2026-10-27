//! The shell retention authority a relay binding may pin: which device
//! governs retention for one room. Shared by the relay configuration and the
//! peer state that checks retained publications against it.

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// A local trust configuration, never inferred from a publication. Deserialization
/// validates the exact certificate and canonical namespace too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AuthorityFields")]
pub struct RetentionAuthority {
    room: String,
    governor: DeviceCert,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityFields {
    room: String,
    governor: DeviceCert,
}
impl TryFrom<AuthorityFields> for RetentionAuthority {
    type Error = String;
    fn try_from(value: AuthorityFields) -> Result<Self, String> {
        Self::new(&value.room, value.governor)
    }
}
impl RetentionAuthority {
    pub fn new(room: &str, governor: DeviceCert) -> Result<Self, String> {
        if uuid::Uuid::parse_str(room)
            .map_err(|e| e.to_string())?
            .to_string()
            != room
        {
            return Err("retention authority requires canonical room UUID".into());
        }
        tilo_user_identity::verify_device_cert(&governor).map_err(|e| e.to_string())?;
        Ok(Self {
            room: room.into(),
            governor,
        })
    }
    /// The room whose retention this authority governs.
    pub fn room(&self) -> &str {
        &self.room
    }
    /// The governing device's certificate.
    pub fn governor(&self) -> &DeviceCert {
        &self.governor
    }
}
