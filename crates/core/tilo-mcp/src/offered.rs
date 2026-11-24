//! Whether an action a caller named exists and is only not offered to it, or does not exist at
//! all: the two refusals every surface (an agent's tools, the SDKs over them, the `tilo`
//! command) tells apart.
//!
//! What an app declares is the surface its package publishes. A caller learns that an action
//! exists only where its own catalogue lists the app: to anyone else the refusal reads exactly
//! as for a name the app does not declare, so it never tells more than the caller may list.

use std::future::Future;

use collab_types::surface::SurfaceFile;
use tilo_actions::remote::ActionProvider;
use tilo_actions::tree::Tree;

/// The code of a refusal of an action the app declares and the caller is not offered.
pub const NOT_OFFERED: &str = "not_offered";

/// The refusal of `name` in `app_id` when the app declares an action by that name and the
/// caller, called `caller` as a person reads it, may list the app but is offered no such
/// action: `not_offered: so.tilo.chat#new_channel isn't offered to Claude Code here`. `None`
/// when the caller's catalogue does not list the app, or the app declares nothing by that
/// name: the refusal is then the one for an action that does not exist. `surface` is the
/// surface the app publishes, asked only once the caller may list the app.
pub async fn not_offered<P, F>(
    tree: &Tree<P>,
    app_id: &str,
    name: &str,
    caller: &str,
    surface: F,
) -> Option<String>
where
    P: ActionProvider + Sync + Clone + Send + 'static,
    F: Future<Output = Option<Vec<u8>>>,
{
    let installed = tree.installed().await.ok()?;
    if !installed.iter().any(|app| app.app_id == app_id) {
        return None;
    }
    let surface = surface.await?;
    let file = SurfaceFile::from_json(std::str::from_utf8(&surface).ok()?).ok()?;
    let action = declared(&file, name)?;
    Some(format!(
        "{NOT_OFFERED}: {app_id}#{action} isn't offered to {caller} here"
    ))
}

/// The surface `app_id` publishes, in the form it is kept by
/// ([`collab_types::surface::canonical`]), as the workspace or this process knows it: the one
/// this process runs it with, the signed package the caller's catalogue lists, the one the
/// fleet compiles in, or the one the app's catalogue row carries.
pub async fn published_surface<P>(tree: &Tree<P>, app_id: &str) -> Option<Vec<u8>>
where
    P: ActionProvider + Sync + Clone + Send + 'static,
{
    if let Some(bytes) = collab_types::app_surface::bytes_of(app_id) {
        return Some(bytes.to_vec());
    }
    if let Ok(installed) = tree.installed().await
        && let Some(surface) = installed
            .iter()
            .find(|app| app.app_id == app_id)
            .and_then(|app| app.surface.as_deref())
            .and_then(collab_types::surface::canonical_json)
    {
        return Some(surface.into_bytes());
    }
    // An app of the fleet answers what its signed package publishes even when this computer
    // has neither started it nor kept its package: the fleet compiles each app's surface in,
    // so every installed app's types are readable on a fresh workspace.
    if let Some(surface) = app_fleet::surface(app_id)
        && let Ok((_, kept)) = SurfaceFile::kept(surface.as_bytes())
    {
        return Some(kept.into_bytes());
    }
    // The workspace publishes its own actions: its catalogue root carries the surface, and no
    // installed application names it.
    if let Some(surface) = tree.surface_of(app_id).await
        && let Ok((_, kept)) = SurfaceFile::kept(surface.as_bytes())
    {
        return Some(kept.into_bytes());
    }
    None
}

/// The name `file` offers the action `name` means by: a verb by the name it is offered by, its
/// id or its label, or an action that opens a window. `None` when it declares none of that
/// name.
#[must_use]
pub fn declared(file: &SurfaceFile, name: &str) -> Option<String> {
    file.resources.iter().find_map(|resource| {
        resource
            .verbs
            .iter()
            .find(|verb| {
                [&verb.name, &verb.id, &verb.label]
                    .iter()
                    .any(|said| !said.is_empty() && said.eq_ignore_ascii_case(name))
            })
            .map(|verb| {
                if verb.name.is_empty() {
                    verb.id.clone()
                } else {
                    verb.name.clone()
                }
            })
            .or_else(|| {
                resource
                    .shows
                    .iter()
                    .find(|shown| shown.eq_ignore_ascii_case(name))
                    .cloned()
            })
    })
}

/// A caller as a refusal names it to a person: its own name, else the name of the session it
/// works in, else its id where that is a word rather than a key; a person's own call is
/// theirs. Never a raw key.
#[must_use]
pub fn caller_named(caller: &tilo_actions::remote::Caller, session: Option<&str>) -> String {
    if !caller.acting().is_agent() {
        return "you".to_owned();
    }
    caller
        .agent_name()
        .or(session)
        .or_else(|| caller.agent_called().filter(|id| !looks_like_key(id)))
        .map_or_else(|| "this agent".to_owned(), str::to_owned)
}

/// Whether `id` is a key's encoding rather than a name: a long run of hex digits.
fn looks_like_key(id: &str) -> bool {
    id.len() >= 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}
