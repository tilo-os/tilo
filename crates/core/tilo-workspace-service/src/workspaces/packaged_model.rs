//! The `.tapp` a workspace keeps for an installed app: the actions its
//! package declares, and the package the workspace's governance installed.
//!
//! Application code runs in the engine of the host that runs the app, and
//! this service names no engine of its own. What it keeps is what every
//! holder of a workspace decides a change by: the signed surface the package
//! carries, so a receipt is decided without asking a member for it
//! ([`keep_surface`]), and the package itself, so a device installs what the
//! governance named without a build of its own
//! ([`Workspaces::installed_package`]).

use std::collections::BTreeMap;

use super::{CollaborationError, Workspaces};
use tilo_app_package::blobs::BlobHash;
use tilo_app_package::tapp;

/// The app version's signed surface: what a change it makes is decided
/// against, and what the action API lists the app's actions from, read from
/// the package at its payload root.
///
/// # Errors
/// Bytes that hold no package, a package that is not `app`, or one whose
/// payload has no surface.
pub fn package_surface(package: &[u8], app: &str) -> Result<Vec<u8>, CollaborationError> {
    let mut blobs: BTreeMap<BlobHash, Vec<u8>> = BTreeMap::new();
    let package = tapp::read(package, &mut blobs)
        .map_err(|error| CollaborationError::new(format!("the package: {error}")))?;
    if package.meta.manifest.package.id != app {
        return Err(CollaborationError::new(
            "the package is another app's, not the one the action names",
        ));
    }
    if tilo_app_package::verify::verify(&package.meta, &[])
        .manifest()
        .is_none()
    {
        return Err(CollaborationError::new(
            "the package's signature does not verify",
        ));
    }
    let hash = package
        .meta
        .collection
        .entries
        .get("surface.json")
        .ok_or_else(|| CollaborationError::new("the package carries no surface"))?;
    blobs
        .get(hash)
        .cloned()
        .ok_or_else(|| CollaborationError::new("the package's surface is missing"))
}

/// Keep `app`'s `surface` where this device decides the receipts of the
/// changes that name it: the package carries the surface, so what is signed
/// names it, and the change is decided without asking a member for it. The
/// device's stores judge the app's resource writes by `surfaces`, its own; the
/// receipts are decided by the process's, which is the same registry on a
/// device that runs alone, where the second naming changes nothing.
///
/// # Errors
/// `surface` is not the canonical compiled form of a surface: nothing of `app` is declared,
/// and the error names the app and why.
pub(crate) fn keep_surface(
    surfaces: &collab_types::app_surface::Surfaces,
    app: &str,
    surface: &[u8],
) -> Result<(), String> {
    collab_types::surface::SurfaceFile::compiled(surface)
        .map_err(|why| format!("the surface of {app} declares nothing: {why}"))?;
    surfaces.register_package(app, surface);
    collab_types::app_surface::register_package(app, surface);
    crate::surfaces::keep(&blake3::hash(surface).to_hex(), surface);
    Ok(())
}

impl Workspaces {
    /// The `.tapp` file this device keeps for the version of `app` the
    /// workspace has, when its governance installs one: the package every
    /// holder installs the app from, whichever device built it.
    pub async fn installed_package(&self, workspace: &str, app: &str) -> Option<Vec<u8>> {
        let state = self.workspace(workspace).await.ok()?;
        let installed = {
            let hosted = state.lock().await;
            hosted.peer().governance().app_version(app)?
        };
        self.app_file(workspace, installed.release.file.as_deref()?)
    }
}
