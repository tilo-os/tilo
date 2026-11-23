//! `share-link` on a package's resources: the URL that opens a note, a video or a task in its
//! app, which the palette, the context menu and an agent all ask for the same way.
//!
//! The verbs are bound where the resources are published ([`crate::publish`]), so every app of
//! the language offers them without writing one: the link a resource's published action asks for
//! is minted by the host that keeps it, under the workspace's web entry
//! ([`tilo_reference::link::share`]). A link is a workspace's fact, not an app's: the host is handed
//! [`LinkHome`], the entry and origin the workspace's own host announces, and refuses with the
//! system's words where it has none, as an app's root does.

use serde::{Deserialize, Serialize};
use tilo_actions::ActionOutput;
use tilo_actions::remote::ResourceViewLocator;
use tilo_resource::share::ShareRequest;

/// Where a workspace's resources open on the web: the entry a link is rendered under, the host's
/// origin and the workspace's id, as the workspace's own host announces them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkHome {
    /// The host's origin (`https://eu1.tilo.cloud`).
    pub origin: String,
    /// The workspace the resources are kept in.
    pub workspace: String,
    /// The certified host device, as its key in hexadecimal.
    #[serde(default)]
    pub host_device: String,
    /// The page base (`https://tilo.so/`) links are rendered under, ending in `/`.
    pub web_entry: String,
}

impl LinkHome {
    /// The home as the session hands it to a host, as JSON: what an app's process receives with
    /// the window's instance ([`runtime_portal::process_link::InstanceRequest::link_home`]).
    ///
    /// # Errors
    ///
    /// When it cannot be written as JSON.
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    /// The locator a link's page is checked against, for the provider `provider` names.
    #[must_use]
    pub fn locator(&self, provider: String) -> ResourceViewLocator {
        ResourceViewLocator {
            origin: self.origin.clone(),
            workspace: self.workspace.clone(),
            host_device: self.host_device.clone(),
            provider,
            web_entry: Some(self.web_entry.clone()),
        }
    }
}

/// Answer the `share-link` request `payload` of `app`'s resources, as its host mints it under
/// `home`: the URL, as `share-link` answers.
///
/// # Errors
///
/// Why no link can be made: the request does not decode, an invitation is the person's own
/// shell's to make, or the host knows no page to render the link under.
pub fn answer(payload: &[u8], app: &str, home: Option<&LinkHome>) -> Result<ActionOutput, String> {
    let request: ShareRequest =
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
    if request.invite {
        return Err("An invitation into the workspace is the person's own shell's to make".into());
    }
    let home = home.ok_or(tilo_reference::link::share::NO_WEB_ENTRY)?;
    // A package's resources live in its primary record; the link names the resource itself, so
    // its `open` shows it there, whichever level it stands at.
    let provider = tilo_reference::link::share::provider(app, "")?;
    let url = tilo_reference::link::share::link(&request, app, "", "", &home.locator(provider))?;
    Ok(ActionOutput {
        value: Some(serde_json::json!({ "url": url })),
        resources: Vec::new(),
    })
}
