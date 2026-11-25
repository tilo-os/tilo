//! `share-link` on a package's resources: the URL that opens a note, a video or a task in its
//! app, which the palette, the context menu and an agent all ask for the same way.
//!
//! The verbs are bound where the resources are published ([`crate::publish`]), so every app of
//! the language offers them without writing one: the link a resource's published action asks for
//! is minted by the host that keeps it, under the workspace's web entry
//! ([`tilo_reference::link::share`]). A link is a workspace's fact, not an app's: the host is handed
//! [`LinkHome`], the entry and origin the workspace's own host announces, and refuses with the
//! system's words where it has none, as an app's root does.

use tilo_actions::ActionOutput;
use tilo_resource::share::ShareRequest;

pub use tilo_reference::link::share::LinkHome;

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
