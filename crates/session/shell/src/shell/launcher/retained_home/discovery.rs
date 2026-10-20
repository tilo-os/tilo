//! Native Files presentation, using the same live source bootstrap as `OpenWith`.
use runtime_collab::open_selected::{discovery as wire, v2::SelectedAccess};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tilo_action_broker::services::{SelectedDiscoveryContext, SelectedDiscoveryFuture};

pub(crate) fn begin(
    tree: crate::shell::state::WorkspaceTreeHandle,
    context: SelectedDiscoveryContext,
    request: wire::Request,
) -> SelectedDiscoveryFuture {
    let session = super::super::LaunchSession::current();
    Box::pin(async move {
        tokio::task::spawn_blocking(move || {
            session.adopt();
            discover(&tree, &context, &request)
        })
        .await
        .map_err(|error| error.to_string())?
    })
}
fn discover(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    context: &SelectedDiscoveryContext,
    request: &wire::Request,
) -> Result<wire::Reply, String> {
    use std::os::unix::ffi::OsStringExt;
    if !request.valid() {
        return Err("invalid selected discovery request".into());
    }
    context.authorize()?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .unwrap_or_else(Instant::now);
    if request
        .components
        .last()
        .is_none_or(|leaf| std::str::from_utf8(leaf).is_err())
    {
        // Canonical argv cannot yet represent raw names; never advertise a
        // backend which would inevitably refuse this particular selection.
        return Ok(wire::Reply::unavailable(request));
    }
    let Ok(source) = super::open_selected::find(context.workspace(), context.executing()) else {
        return Ok(wire::Reply::unavailable(request));
    };
    let owner = tree
        .read()
        .installed_native_owner()
        .ok_or("native selected owner unavailable")?;
    let room = source.prepared().room;
    let workspace = super::super::native_launch_workspace(tree, room)?;
    let mut path = PathBuf::from("/run/tilo/retained-home");
    for part in &request.components {
        path.push(std::ffi::OsString::from_vec(part.clone()));
    }
    let choices = crate::shell::doc_open::open_with_choices(
        &path,
        context.owner(),
        Some(&workspace.default_apps),
    );
    let installed = super::super::installed_apps().map_err(|e| e.user_message())?;
    let mut reply = wire::Reply::unavailable(request);
    for choice in choices {
        if request
            .invocation
            .as_ref()
            .is_some_and(|scope| scope.app_id != choice.id)
        {
            continue;
        }
        if reply.targets.len() >= wire::MAX_TARGETS {
            break;
        }
        context.authorize()?;
        if Instant::now() >= deadline {
            return Err("selected discovery timed out".into());
        }
        let Some(app) = installed
            .iter()
            .find(|app| app.package_id == choice.id && app.has_gui)
        else {
            continue;
        };
        let package = verified_package(app)?;
        if !handles(&app.package_id, &path) {
            continue;
        }

        let Some(access) = offered_access(
            &source,
            context,
            (&owner, room),
            (&package, &app.package_id),
            request,
            deadline,
        )?
        else {
            continue;
        };
        reply.targets.push(wire::Target {
            app_id: app.package_id.clone(),
            package_sha256: package.package_id_hex,
            name: choice.name,
            is_default: choice.is_default,
            access,
        });
    }
    context.authorize()?;
    if Instant::now() >= deadline {
        return Err("selected discovery timed out".into());
    }
    if !reply.targets.is_empty() {
        reply.type_label = crate::shell::doc_open::type_label(&path);
        reply.source_operation = Some(source.operation().to_owned());
    }
    if !reply.matches(request) {
        return Err("invalid selected discovery result".into());
    }
    Ok(reply)
}

/// The access the selected thing can be opened in with `package`, asked of
/// the live `source` and checked again against the person's role in
/// `room` after; `None` when neither is supported.
fn offered_access(
    source: &super::open_selected::Dispatcher,
    context: &SelectedDiscoveryContext,
    (owner, room): (
        &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        crate::shell::state::RoomId,
    ),
    (package, app_id): (&package_store::InstalledPackage, &str),
    request: &wire::Request,
    deadline: Instant,
) -> Result<Option<SelectedAccess>, String> {
    let current_access =
        super::super::native_app_open_access(owner, room, context.certificate(), app_id)?;
    let query = |access| wire::QuerySelectedSupport {
        schema: 1,
        request_id: request.request_id.clone(),
        app_id: app_id.to_owned(),
        package_sha256: package.package_id_hex.clone(),
        access,
    };
    let authorize = || context.authorize();
    let read_only = source
        .query_selected_support(&query(SelectedAccess::ReadOnly), &authorize, deadline)
        .map_err(|e| e.to_string())?;
    let edit = current_access == super::super::NativeAppOpenAccess::Author
        && source
            .query_selected_support(&query(SelectedAccess::EditInParent), &authorize, deadline)
            .map_err(|e| e.to_string())?;
    // This is current presentation only. The actual original action checks
    // its caller and target Author again before exporting any descriptor.
    context.authorize()?;
    super::verified_gui_package(package).map_err(|e| e.to_string())?;
    let rechecked =
        super::super::native_app_open_access(owner, room, context.certificate(), app_id)?;
    Ok(supported_access(current_access, rechecked, read_only, edit))
}

/// The installed package `app` is, checked to be the one listed and a
/// verified graphical package.
fn verified_package(
    app: &super::super::InstalledApp,
) -> Result<package_store::InstalledPackage, String> {
    let package = package_store::find_installed(&app.package_id)
        .map_err(|e| e.to_string())?
        .ok_or("selected discovery package retired")?;
    if package.manifest_path != app.manifest_path || package.payload_root != app.payload_root {
        return Err("selected discovery package changed".into());
    }
    super::verified_gui_package(&package).map_err(|e| e.to_string())?;
    Ok(package)
}

/// Whether `package_id` still claims the kind of file at `path`, by its
/// type or its extension.
fn handles(package_id: &str, path: &std::path::Path) -> bool {
    mime_guess::from_path(path)
        .first_raw()
        .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
        .into_iter()
        .chain(
            path.extension()
                .and_then(|value| value.to_str())
                .and_then(|value| desktop_policy::ResourceKind::extension(value).ok()),
        )
        .any(|kind| super::super::revalidate_resource_claim(package_id, &kind))
}

// Backend support is independent for each source access. A role change may
// select a supported readonly route, but cannot invent one as an edit fallback.
fn supported_access(
    initial: super::super::NativeAppOpenAccess,
    current: super::super::NativeAppOpenAccess,
    read_only: bool,
    edit: bool,
) -> Option<SelectedAccess> {
    use super::super::NativeAppOpenAccess::Author;
    if edit && initial == Author && current == Author {
        Some(SelectedAccess::EditInParent)
    } else if read_only {
        Some(SelectedAccess::ReadOnly)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::NativeAppOpenAccess::{Author, Delegate, Viewer};
    use super::{SelectedAccess, supported_access};

    #[test]
    fn author_can_use_edit_only_backend() {
        assert_eq!(
            supported_access(Author, Author, false, true),
            Some(SelectedAccess::EditInParent)
        );
    }

    #[test]
    fn losing_author_requires_supported_readonly_fallback() {
        for current in [Viewer, Delegate] {
            assert_eq!(supported_access(Author, current, false, true), None);
            assert_eq!(
                supported_access(Author, current, true, true),
                Some(SelectedAccess::ReadOnly)
            );
        }
    }

    #[test]
    fn viewer_never_inherits_backend_edit_support() {
        for current in [Viewer, Author] {
            assert_eq!(supported_access(Viewer, current, false, true), None);
            assert_eq!(
                supported_access(Viewer, current, true, true),
                Some(SelectedAccess::ReadOnly)
            );
        }
    }

    #[test]
    fn readonly_route_requires_actual_backend_support() {
        assert_eq!(
            supported_access(Viewer, Viewer, true, false),
            Some(SelectedAccess::ReadOnly)
        );
        assert_eq!(supported_access(Viewer, Viewer, false, false), None);
        assert_eq!(supported_access(Author, Author, false, false), None);
    }
}
