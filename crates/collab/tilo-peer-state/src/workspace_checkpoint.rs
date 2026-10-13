//! Closed native checkpoint requests. Identity and invocation come from broker custody.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRoute {
    pub app: String,
    pub record: String,
}
impl CheckpointRoute {
    /// # Errors
    /// Rejects unbounded or malformed native routes.
    pub fn validate(&self) -> Result<(), String> {
        if self.app == "so.tilo.desktop" {
            return Err(
                "checkpoint coverage excludes room governance and authority records".into(),
            );
        }
        if self.app.is_empty()
            || self.app.len() > 256
            || self.record.len() > 4096
            || [&self.app, &self.record]
                .iter()
                .any(|s| s.chars().any(char::is_control))
        {
            return Err("invalid native checkpoint route".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CheckpointCommand {
    Capture {
        name: String,
        routes: Vec<CheckpointRoute>,
    },
    Inspect {
        id: uuid::Uuid,
    },
    NameBind {
        name: String,
        id: uuid::Uuid,
    },
    Find {
        name: String,
    },
}
impl CheckpointCommand {
    /// # Errors
    /// Rejects invalid names, IDs, or unsorted, duplicate, unbounded coverage.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Capture { name, routes } => {
                validate_name(name)?;
                if routes.is_empty()
                    || routes.len() > 32
                    || !routes
                        .windows(2)
                        .all(|pair| matches!(pair, [first, second] if first < second))
                {
                    return Err("checkpoint routes must be bounded, sorted and distinct".into());
                }
                for route in routes {
                    route.validate()?;
                }
            }
            Self::NameBind { name, id } => {
                validate_name(name)?;
                validate_id(*id)?;
            }
            Self::Inspect { id } => validate_id(*id)?,
            Self::Find { name } => validate_name(name)?,
        }
        Ok(())
    }
    #[must_use]
    pub fn mutates(&self) -> bool {
        matches!(self, Self::Capture { .. } | Self::NameBind { .. })
    }
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("invalid native checkpoint name".into());
    }
    Ok(())
}
fn validate_id(id: uuid::Uuid) -> Result<(), String> {
    if id.is_nil() {
        return Err("nil native checkpoint id".into());
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CheckpointRequest {
    pub room: String,
    pub invocation: uuid::Uuid,
    pub caller: tilo_user_identity::DeviceCert,
    pub command: CheckpointCommand,
}
impl CheckpointRequest {
    /// Stable binding of the original command and authenticated device/person evidence.
    /// # Errors
    /// Rejects invalid room, invocation, command or encoding.
    pub fn digest(&self) -> Result<[u8; 32], String> {
        if self.invocation.is_nil()
            || uuid::Uuid::parse_str(&self.room)
                .map_err(|e| e.to_string())?
                .to_string()
                != self.room
        {
            return Err("invalid checkpoint invocation binding".into());
        }
        self.command.validate()?;
        let bytes =
            postcard::to_allocvec(&(&self.room, self.invocation, &self.caller, &self.command))
                .map_err(|e| e.to_string())?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
}
pub type CheckpointFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;
