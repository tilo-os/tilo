//! A development build in the workspace its device serves.
//!
//! A device's development build of an app (`<app>.dev.<device key>`) is its own install, under
//! its own id. To keep records in a workspace an app must be installed there: installing gave it
//! the grant over its own records, and a device signing what it runs grants nothing. So the
//! device that installs a build also installs it in the workspace its person's services serve,
//! through the workspace's own install action, asked as that person: the workspace decides
//! whether they may install apps, as for any install. Members see it among the workspace's apps,
//! marked as a development build and published by the device that signed it, and uninstalling
//! it is the workspace's ordinary uninstall. Its records are its own id's, apart from the app it
//! is a build of.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER,
};

use super::workspaces::ServiceWorkspaces;

/// The bytes of a package file one upload piece carries: the most the workspace takes in one.
const PIECE: usize = 256 * 1024;

/// How long one call of the workspace's governance is waited for.
const WAIT: Duration = Duration::from_secs(60);

/// The workspace's action that keeps a piece of a package file its install names.
const UPLOAD: &str = "upload-app-file";

/// The workspace a device's development builds are installed in, and the device's service that
/// keeps it.
pub(crate) struct DevBuilds {
    workspaces: Arc<ServiceWorkspaces>,
    workspace: String,
}

impl DevBuilds {
    pub(crate) fn new(workspaces: Arc<ServiceWorkspaces>, workspace: String) -> Self {
        Self {
            workspaces,
            workspace,
        }
    }

    /// Install the development build `package` (a signed `.tapp` this device packed) in the
    /// workspace, as this device's person: its file handed over first, then the workspace's
    /// install of it, marked as a development build. What a person reads of it.
    ///
    /// # Errors
    /// The package is unreadable, or the workspace refused the install: the refusal in the
    /// words its person reads.
    pub(crate) fn install(&self, package: &[u8]) -> Result<String, String> {
        let tapp =
            tilo_app_package::tapp::read(package, &mut tilo_app_package::blobs::DiscardBlobs)
                .map_err(|error| format!("the development build: {error}"))?;
        let manifest = &tapp.meta.manifest.package;
        let app = manifest.id.clone();
        if manifest.dev.is_none() {
            return Err(format!("{app} is no development build"));
        }
        let surface = app_registry::publisher::entry::surface_hex(package)
            .map_err(|error| format!("the development build {app}: {error:#}"))?;
        let file = blake3::hash(package).to_hex().to_string();
        let total = package.len() as u64;
        let mut offset = 0_u64;
        for piece in package.chunks(PIECE) {
            self.ask(
                &app,
                UPLOAD,
                json!({"file": file, "offset": offset, "total": total, "data": hex::encode(piece)}),
            )?;
            offset = offset.saturating_add(piece.len() as u64);
        }
        self.ask(
            &app,
            tilo_action_policy::roles::governance::INSTALL_APP,
            json!({
                "app": app,
                "version": manifest.version,
                "surface": surface,
                "package": hex::encode(tapp.meta.id.0),
                "publisher": hex::encode(tapp.meta.root.publisher_identity),
                "file": file,
                "dev": true,
            }),
        )?;
        Ok(format!(
            "installed in the workspace as a development build: {app}"
        ))
    }

    /// The workspace's governance action `verb` with `arguments`, asked as this device's person,
    /// as their own act on the workspace's tree is asked: its answer.
    fn ask(&self, app: &str, verb: &str, arguments: Value) -> Result<Value, String> {
        let tree = self
            .workspaces
            .copies()
            .providers()
            .get(WORKSPACE_GOVERNANCE_PROVIDER)
            .ok_or_else(|| {
                format!("{app} is installed on this device alone: it keeps no workspace")
            })?;
        let cert = self.workspaces.device_cert();
        let person = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&self.workspace);
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget::workspace_governance(&self.workspace, verb),
            argument: InvokeArgument::Json(match arguments {
                Value::Object(arguments) => arguments,
                _ => serde_json::Map::new(),
            }),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = match tree.answers.invoke(&person, request).wait_timeout(WAIT) {
            Ok(Some(Ok(receipt))) => receipt,
            Ok(Some(Err(refused))) => return Err(refusal(app, &refused)),
            Ok(None) | Err(_) => {
                return Err(format!(
                    "{app} is installed on this device, and the workspace did not answer its \
                     install in time"
                ));
            }
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(format!(
                "{app} is installed on this device, and the workspace's install of it failed: {}",
                error.message
            )),
            ReceiptStatus::Rejected { error } => Err(refusal(app, &error)),
            ReceiptStatus::Cancelled { .. } | ReceiptStatus::Accepted => Err(format!(
                "{app} is installed on this device, and the workspace did not install it"
            )),
        }
    }
}

/// What a person reads when the workspace refused installing `app`: a workspace offers its
/// install only to those it lets install apps.
fn refusal(app: &str, refused: &ActionCallError) -> String {
    match refused {
        ActionCallError::UnknownAction | ActionCallError::NotPermitted => format!(
            "{app} is installed on this device, but this workspace does not let you install \
             apps, so it keeps no records here. An owner of the workspace can let you."
        ),
        other => format!(
            "{app} is installed on this device, and the workspace refused it: {}",
            other.said()
        ),
    }
}

#[cfg(test)]
#[path = "dev_builds_tests.rs"]
mod tests;
