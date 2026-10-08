//! Typed native profile authoring; assignments resolve the current signed definition.
use collab_types::{Expiry, PermissionProfile, Principal, Subject};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspacePermissionProfileCommand {
    Save {
        profile: PermissionProfile,
    },
    Assign {
        profile_name: String,
        subject: Subject,
        expiry: Expiry,
    },
}

impl WorkspacePermissionProfileCommand {
    /// # Errors
    /// Refuses malformed definitions or non-explicit participant targets.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Save { profile } => profile
                .validate()
                .map_err(|_| "invalid permission profile".into()),
            Self::Assign {
                profile_name,
                subject,
                ..
            } => {
                if profile_name.trim().is_empty()
                    || profile_name.chars().count() > 80
                    || profile_name.chars().any(char::is_control)
                {
                    return Err("invalid permission profile name".into());
                }
                let Subject::Principal(
                    Principal::Person(id) | Principal::App(id) | Principal::Agent(id),
                ) = subject
                else {
                    return Err("permission profile requires an explicit participant".into());
                };
                if id.is_empty()
                    || id.trim() != id
                    || id.len() > 256
                    || id.chars().any(char::is_control)
                {
                    return Err("invalid permission profile participant".into());
                }
                Ok(())
            }
        }
    }
}
