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
/// ([`collab_types::surface::canonical`]), as this device and the caller's workspace answer it:
/// the one this process runs it with, the signed package the caller's catalogue lists, the one
/// the fleet compiles in for a built-in app, or the one the app's own catalogue row carries.
/// The build kept here comes first, since an app installed here may be another version than the
/// fleet's; `None` when none of them is known, which refuses nothing before the dispatch does.
pub async fn installed_surface<P>(tree: &Tree<P>, app_id: &str) -> Option<Vec<u8>>
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
    // A built-in app has one source for its surface: the fleet compiles each app's surface in,
    // so its actions are known before it first opens on any workspace.
    if let Some(surface) = fleet_catalogue::surface(app_id)
        && SurfaceFile::compiled(surface.as_bytes()).is_ok()
    {
        return Some(surface.as_bytes().to_vec());
    }
    // The workspace publishes its own actions: its catalogue root carries the surface under its
    // own app id, and no installed application names it.
    if let Some(surface) = tree.surface_of(app_id).await
        && SurfaceFile::compiled(surface.as_bytes()).is_ok()
    {
        return Some(surface.into_bytes());
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_actions::InvocationId;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
        CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    };
    use tilo_actions::tree::Tree;

    use super::{declared, installed_surface};
    use crate::catalogue_ask::TokioPause;

    /// A catalogue answering every query with its rows, as a directory that does not narrow
    /// them by the app asked for: a workspace's own root row comes beside the rows asked for.
    #[derive(Clone)]
    struct Mixed(Vec<CatalogueRow>);

    impl ActionProvider for Mixed {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage {
                revision: "v1".into(),
                rows: self.0.clone(),
                exhausted: true,
                ..CataloguePage::default()
            }))
        }

        fn invoke(
            &self,
            _: &Caller,
            _: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }
    }

    /// The workspace's catalogue root as a workspace publishes it: its own surface, under the
    /// app its own actions are published by.
    fn workspace_root() -> CatalogueRow {
        CatalogueRow {
            provider: "tilo.workspace".into(),
            app_id: "tilo.workspace".into(),
            resource: CatalogueResource {
                id: "workspace.permissions:ws".into(),
                kind: "workspace.permissions".into(),
                label: "Workspace".into(),
                presentation: None,
            },
            surface: Some(collab_types::surface::canonical(&serde_json::json!({
                "resources": [{
                    "kind": "workspace.permissions",
                    "label": "Workspace",
                    "verbs": [{"id": "invite", "kind": "effect"}],
                }],
            }))),
            ..CatalogueRow::default()
        }
    }

    #[tokio::test]
    async fn the_workspaces_root_row_does_not_hide_a_built_in_apps_actions() {
        let tree = Tree::new(
            Mixed(vec![workspace_root()]),
            Caller::local_person("ada"),
            "ws".to_owned(),
            Arc::new(TokioPause::new()),
        );
        // The root row answers for the app it is published under, the workspace's own.
        assert!(
            tree.surface_of("tilo.workspace").await.is_some(),
            "the workspace's root row is its own surface"
        );
        // A built-in app's actions are the fleet's compiled surface's, never that row's: its
        // own actions are declared, and a name it does not declare still is not.
        let surface = installed_surface(&tree, "so.tilo.tasks")
            .await
            .expect("the fleet compiles the built-in app's surface in");
        let file = collab_types::surface::SurfaceFile::from_json(
            std::str::from_utf8(&surface).expect("a surface is text"),
        )
        .expect("the fleet's surface is the compiled form");
        assert!(
            declared(&file, "new_task").is_some(),
            "the built-in app's own action"
        );
        assert!(
            declared(&file, "not_a_task").is_none(),
            "an action it does not declare"
        );
    }
}
