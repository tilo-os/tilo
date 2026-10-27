//! API Tester's effects: the work that reaches outside the workspace.
//!
//! Every change to the shared workspace is a field edit or a verb of
//! [`crate::record::Workspace`] ([`crate::resource`]). Sending a request is an
//! effect ([`ApiTesterEffect`]): it reaches the network from this device, and
//! what it brings back is local, except the history entry its completion
//! records through a verb. The response answers whoever sent the request (a
//! person's button, an agent, a script), as the response pane shows it. A credential — a request's token or password, a
//! secret variable's value — never enters the record or an action: it is set
//! through an effect into the device's secret store, keyed by the record it
//! belongs to.

use ui::prelude::Effect;
// The effect derive names its runtime crate by this path.
use ui::tilo_resource;

pub const REQUEST_MENU_CHOICES: &[&str] = &["Save request…", "Rename request…", "Delete request…"];
/// What [`ApiTesterEffect::CopyResponse`] can copy out of a response.
pub const COPY_PARTS: &[&str] = &["body", "headers", "request"];
pub const ENVIRONMENT_MENU_CHOICES: &[&str] =
    &["Rename environment…", "Add variable…", "Delete variable…"];

impl std::fmt::Debug for ApiTesterEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::ImportLegacy { .. } => "ImportLegacy",
            Self::Send => "Send",
            Self::CancelSend => "CancelSend",
            Self::CopyResponse { .. } => "CopyResponse",
            Self::SendSaved { .. } => "SendSaved",
            Self::SetCredential { .. } => "SetCredential",
            Self::PickEnvironment { .. } => "PickEnvironment",
            Self::SetSecretValue { .. } => "SetSecretValue",
            Self::RequestMenu { .. } => "RequestMenu",
            Self::EnvironmentMenu { .. } => "EnvironmentMenu",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}

/// Work that reaches outside the document.
#[derive(Clone, PartialEq, Effect)]
#[effect(operation = "api_tester.effects")]
pub enum ApiTesterEffect {
    /// Import an old Automerge snapshot into an empty workspace
    #[effect(job)]
    ImportLegacy {
        #[param("Base64-encoded legacy Automerge snapshot; handled only on this device")]
        snapshot: String,
    },
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
    ///
    /// Stops waiting for a server that has not answered. The send that was in
    /// flight reports that it was cancelled.
    #[effect(own_device)]
    CancelSend,
    /// Copy response
    ///
    /// Puts part of the response on this device's clipboard: the body as it
    /// is shown, every header, or the request that produced it.
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
    /// Set a secret variable's value
    SetSecretValue {
        #[param("Id of the variable")]
        variable: String,
        #[param("The value, kept on this device")]
        value: String,
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
