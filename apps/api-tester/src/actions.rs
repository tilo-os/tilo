//! API Tester's effects: the work that reaches outside the workspace. Every change to
//! the workspace is a field edit or a verb of [`crate::record::Workspace`]; sending is an
//! effect, whose completion records a history entry through a verb. A credential is set
//! through an effect into this device's secret store and never enters the record.

use ui::prelude::Effect;
// The effect derive names its runtime crate by this path.
use ui::tilo_resource;

pub const REQUEST_MENU_CHOICES: &[&str] = &["Save request…", "Rename request…", "Delete request…"];
pub const COPY_PARTS: &[&str] = &["body", "headers", "request"];
pub const ENVIRONMENT_MENU_CHOICES: &[&str] =
    &["Rename environment…", "Add variable…", "Delete variable…"];

impl std::fmt::Debug for ApiTesterEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Send => "Send",
            Self::CancelSend => "CancelSend",
            Self::CopyResponse { .. } => "CopyResponse",
            Self::SendSaved { .. } => "SendSaved",
            Self::SetCredential { .. } => "SetCredential",
            Self::PickEnvironment { .. } => "PickEnvironment",
            Self::SetSecretValue { .. } => "SetSecretValue",
            Self::RenameCollection { .. } => "RenameCollection",
            Self::RequestMenu { .. } => "RequestMenu",
            Self::EnvironmentMenu { .. } => "EnvironmentMenu",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Effect)]
#[effect(operation = "api_tester.effects")]
pub enum ApiTesterEffect {
    /// Send request
    #[effect(job, returns, untrusted, cancellable)]
    Send,
    /// Send saved request
    #[effect(job, returns, untrusted, cancellable)]
    SendSaved {
        #[param("Id of the saved request")]
        request: String,
    },
    /// Cancel request
    #[effect(own_device)]
    CancelSend,
    /// Copy response
    #[effect(own_device, idempotent)]
    CopyResponse {
        #[param("What to copy", choices = COPY_PARTS.iter().copied())]
        part: String,
    },
    /// Set the request's credential
    #[effect(job)]
    SetCredential {
        #[param("The token, password or key, kept on this device")]
        secret: String,
    },
    /// Choose an environment or open its management form
    #[effect(view, idempotent)]
    PickEnvironment {
        #[param("Environment name, No environment, or an environment management command")]
        choice: String,
    },
    SetSecretValue {
        #[param("Id of the variable")]
        variable: String,
        #[param("The value, kept on this device")]
        value: String,
    },
    /// Rename collection…
    #[effect(view, idempotent)]
    RenameCollection {
        #[param("Id of the collection")]
        collection: String,
    },
    /// Request actions
    #[effect(view, idempotent)]
    RequestMenu {
        #[param("Request actions", choices = REQUEST_MENU_CHOICES.iter().copied())]
        action: String,
    },
    /// Environment actions
    #[effect(view, idempotent)]
    EnvironmentMenu {
        #[param("Environment actions", choices = ENVIRONMENT_MENU_CHOICES.iter().copied())]
        action: String,
    },
}
