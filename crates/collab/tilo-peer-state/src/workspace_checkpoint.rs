//! Closed native checkpoint requests. Identity and invocation come from broker custody.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRoute {
    pub app: String,
    pub record: String,
}
impl CheckpointRoute {
    pub fn validate(&self) -> Result<(), String> {
        if self.app == "so.tilo.desktop" {
            return Err(
                "checkpoint coverage excludes workspace governance and authority records".into(),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CheckpointCommand {
    Capture {
        name: String,
        /// The person's own words for this snapshot; absent or empty means none.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        description: String,
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
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Capture {
                name,
                description,
                routes,
            } => {
                validate_name(name)?;
                validate_description(description)?;
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
fn validate_description(description: &str) -> Result<(), String> {
    if description.len() > 280
        || description.trim() != description
        || description.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err("invalid checkpoint description".into());
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
    pub workspace: String,
    pub invocation: uuid::Uuid,
    pub caller: tilo_user_identity::DeviceCert,
    pub command: CheckpointCommand,
}
impl CheckpointRequest {
    /// Stable binding of the original command and authenticated device/person evidence.
    pub fn digest(&self) -> Result<[u8; 32], String> {
        if self.invocation.is_nil()
            || uuid::Uuid::parse_str(&self.workspace)
                .map_err(|e| e.to_string())?
                .to_string()
                != self.workspace
        {
            return Err("invalid checkpoint invocation binding".into());
        }
        self.command.validate()?;
        let bytes = postcard::to_allocvec(&(
            &self.workspace,
            self.invocation,
            &self.caller,
            &self.command,
        ))
        .map_err(|e| e.to_string())?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
}
pub type CheckpointFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;
