//! The shell retention authority a relay binding may pin: which device
//! governs retention for one workspace. Shared by the relay configuration and the
//! peer state that checks retained publications against it.

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// A local trust configuration, never inferred from a publication. Deserialization
/// validates the exact certificate and canonical namespace too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AuthorityFields")]
pub struct RetentionAuthority {
    #[serde(rename = "room")]
    workspace: String,
    governor: DeviceCert,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityFields {
    #[serde(rename = "room")]
    workspace: String,
    governor: DeviceCert,
}
impl TryFrom<AuthorityFields> for RetentionAuthority {
    type Error = String;
    fn try_from(value: AuthorityFields) -> Result<Self, String> {
        Self::new(&value.workspace, value.governor)
    }
}
impl RetentionAuthority {
    pub fn new(workspace: &str, governor: DeviceCert) -> Result<Self, String> {
        if uuid::Uuid::parse_str(workspace)
            .map_err(|e| e.to_string())?
            .to_string()
            != workspace
        {
            return Err("retention authority requires canonical workspace UUID".into());
        }
        tilo_user_identity::verify_device_cert(&governor).map_err(|e| e.to_string())?;
        Ok(Self {
            workspace: workspace.into(),
            governor,
        })
    }
    /// The workspace whose retention this authority governs.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    /// The governing device's certificate.
    pub fn governor(&self) -> &DeviceCert {
        &self.governor
    }
}
