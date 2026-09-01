use compat_catalogue::ApplicationQuery;
use serde::{Deserialize, Serialize};

use crate::{ExplainStatus, LaunchReceipt};

pub const PROTOCOL_VERSION: u16 = 1;

/// Versioned JSON contract for a future user-session Unix socket. No socket is
/// bound by this crate today. A daemon implementing it must authenticate the
/// peer and obtain capabilities from the compositor; this DTO deliberately has
/// no field through which a client can self-grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ProtocolRequest {
    Resolve(ResolveRequest),
    Launch {
        version: u16,
        app_id: String,
        workspace_id: String,
        program_id: String,
        query: ApplicationQuery,
        explicit_vm_fallback: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub version: u16,
    pub app_id: String,
    pub query: ApplicationQuery,
    pub explicit_vm_fallback: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProtocolResponse {
    Resolved { explanation: ExplainStatus },
    Launched { receipt: LaunchReceipt },
    Error { error: ProtocolError },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

impl ProtocolError {
    #[must_use]
    pub fn unsupported_version(received: u16) -> Self {
        Self {
            code: "unsupported_version".to_string(),
            message: format!(
                "compatibility protocol {received} is unsupported; expected {PROTOCOL_VERSION}"
            ),
        }
    }
}
