//! An app's model applied from its package, as WebAssembly: a holder of the
//! workspace's keys executes the app version's own code, so a server takes an
//! app's work the moment the app is installed, with no build of its own.
//!
//! The `.tapp` a workspace keeps ([`super::app_files`]) carries the app's
//! model as `model.wasm` ([`tilo_app_package::validate::MODEL_MODULE`]): a
//! module with no imports beyond the device's ([`tilo_model_engine::ABI`])
//! that applies the resource's own actions.
//! [`Workspaces::prepare_packaged_application`] makes a resource's document
//! from it, and [`Workspaces::apply_packaged_action`] applies one action,
//! publishing what the module wrote exactly as an action of this device's own
//! would be published. Because the module is the same bytes on every device, a
//! device holding the resource replays the invocation to the same change
//! ([`tilo_model_engine::Applied::envelope`] is what a peer replays).
//!
//! The module is split against the model runtime of the release that built
//! the app ([`tilo_model_engine::Runtime`]), and this server's image carries its
//! own release's. A package from another release names a runtime this server
//! lacks, so [`ensure_model_runtime`] fetches it, before the workspace is
//! locked, from the registry that offers the runtime bundle the package names
//! ([`app_cli::model_runtime`]): verified as an installed bundle is, kept on
//! disk by its hash and in memory for the process, so only the first action
//! of another release's app waits for the registry.
//!
//! What this side does *not* do is decide anything the app decides: it checks
//! the package's signature, that the package is the app named, and that the
//! governor may write the document. The arguments are checked against the
//! verb the signed surface declares, the module builds the action from its
//! vocabulary and applies it, and this device's executor writes the
//! envelope, the history entry and the change, as for an app compiled in.

use std::collections::BTreeMap;

use super::genesis::publish_genesis;
use super::{CollaborationError, Workspaces, system_time};
use tilo_actions::MemberId;
use tilo_app_package::blobs::BlobHash;
use tilo_app_package::tapp;
use tilo_model_engine::{Invocation, Model, Runtime};
use tilo_user_identity::DeviceCert;

/// One action of an installed app, as a holder of the workspace applies it
/// from the package it shipped in.
#[derive(Debug)]
pub struct PackagedAction<'a> {
    /// The app's id, as its package and its resources name it.
    pub app: &'a str,
    /// The resource kind the action is for: the app's model serves every
    /// resource the app declares, and routes the action by this.
    pub kind: &'a str,
    /// The resource's document id: `""` for what the app keeps per workspace.
    pub document: &'a str,
    /// The `.tapp` the workspace keeps for that app version.
    pub package: &'a [u8],
    /// The action's id in the resource's vocabulary.
    pub action: &'a str,
    /// Its arguments, as the action API binds them.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The member whose invocation this is: the person, agent or bot the
    /// change is authored by and history names, never the governor the
    /// device signs as.
    pub member: MemberId,
    /// Who acts, and for whom, as the action API admitted it: the agent a
    /// person's session handed the action to, which the receipt decides the
    /// change by. A member acting for themselves is named by `member` alone.
    pub acting: tilo_actions::Acting,
    /// The origin clock, milliseconds since the Unix epoch.
    pub now_ms: i64,
}

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
        .get("surface.toml")
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
pub(crate) fn keep_surface(
    surfaces: &collab_types::app_surface::Surfaces,
    app: &str,
    surface: &[u8],
) {
    surfaces.register_package(app, surface);
    collab_types::app_surface::register_package(app, surface);
    if let Ok((_, canonical)) = collab_types::surface::SurfaceFile::kept(surface) {
        crate::surfaces::keep(
            &blake3::hash(canonical.as_bytes()).to_hex(),
            canonical.as_bytes(),
        );
    }
}

/// The app version's model, compiled from the package's `model.wasm`.
///
/// # Errors
/// Refuses bytes that hold no package, a package that is not `app`, one whose
/// signature does not verify, one whose payload has no model module, and a
/// module the engine will not link (an import the device does not answer).
fn packaged_model(package: &[u8], app: &str) -> Result<Model, CollaborationError> {
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
        .get(tilo_app_package::validate::MODEL_MODULE)
        .ok_or_else(|| CollaborationError::new("the package carries no model module"))?;
    let bytes = blobs
        .get(hash)
        .ok_or_else(|| CollaborationError::new("the package's model module is missing"))?;
    Model::compile(bytes.as_slice()).map_err(|error| CollaborationError::new(error.to_string()))
}

/// Make the model runtime `package`'s module links against present: the
/// runtime is found here, or fetched from the registry that offers the
/// runtime bundle the package names (`[package].runtime`) and kept. A whole
/// module, or bytes that hold no model, need nothing; [`packaged_model`]
/// then refuses what is wrong with them.
///
/// # Errors
/// The runtime is not here and cannot be fetched or verified: the error
/// names the runtime the module links against, the ones this server holds, and
/// why the fetch failed.
pub(crate) async fn ensure_model_runtime(package: &[u8]) -> Result<(), CollaborationError> {
    let Some((hash, bundle)) = linked_runtime(package) else {
        return Ok(());
    };
    let Err(missing) = Runtime::find(&hash) else {
        return Ok(());
    };
    let Some(bundle) = bundle else {
        return Err(CollaborationError::new(format!(
            "{missing}; the package names no runtime bundle to fetch it from"
        )));
    };
    let named = bundle.clone();
    let fetched = tokio::task::spawn_blocking(move || fetch_model_runtime(&named, &hash))
        .await
        .map_err(|error| CollaborationError::new(error.to_string()))?;
    fetched.map_err(|why| CollaborationError::new(format!("{missing}; fetching {bundle}: {why}")))
}

/// The model runtime `package`'s module links against, and the runtime
/// bundle the package names; `None` for a whole module or no module.
fn linked_runtime(package: &[u8]) -> Option<(String, Option<String>)> {
    let mut blobs: BTreeMap<BlobHash, Vec<u8>> = BTreeMap::new();
    let package = tapp::read(package, &mut blobs).ok()?;
    let module = blobs.get(
        package
            .meta
            .collection
            .entries
            .get(tilo_app_package::validate::MODEL_MODULE)?,
    )?;
    let hash = tilo_model_engine::linked_runtime(module).ok()??;
    Some((hash, package.meta.manifest.package.runtime))
}

/// Fetch model runtime `hash` from the runtime bundle `bundle` and keep it
/// for this process. Blocking.
fn fetch_model_runtime(bundle: &str, hash: &str) -> Result<(), String> {
    let is_named = |runtime: &Runtime| {
        if runtime.hash() == hash {
            Ok(())
        } else {
            Err(format!(
                "it carries model runtime {}, not the {hash} the module links against",
                runtime.hash()
            ))
        }
    };
    let dir = app_cli::model_runtime::fetch(bundle, hash, &|dir| {
        is_named(&Runtime::read(dir).map_err(|error| error.to_string())?)
    })?;
    let runtime = Runtime::read(&dir).map_err(|error| error.to_string())?;
    is_named(&runtime)?;
    drop(runtime.remember());
    Ok(())
}

impl Workspaces {
    /// The `.tapp` file this device keeps for the version of `app` the
    /// workspace has, when its governance installs one: the model that every
    /// holder of the workspace applies the app's actions from, whichever
    /// device built the app.
    pub async fn installed_package(&self, workspace: &str, app: &str) -> Option<Vec<u8>> {
        let state = self.workspace(workspace).await.ok()?;
        let installed = {
            let governed = state.lock().await;
            governed.peer().governance().app_version(app)?
        };
        self.app_file(workspace, installed.release.file.as_deref()?)
    }

    /// Make a registered app's document (`""` for what it keeps per
    /// workspace) from the model its package carries, under exact governor
    /// ownership. An existing document remains byte-identical.
    ///
    /// # Errors
    /// Refuses other devices, stale governance, a package that is not the
    /// app's, one whose model refuses or produces nothing usable, and failed
    /// publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor transaction retains its workspace lock through durable signed publication"
    )]
    pub async fn prepare_packaged_application(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        app: &str,
        kind: &str,
        document: &str,
        package: &[u8],
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "packaged application creation requires exact governor ownership",
            ));
        }
        ensure_model_runtime(package).await?;
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if !governed.governs_applications(governor) {
            return Err(CollaborationError::new(
                "application governor differs from current workspace authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        if governed.peer().holds_document(app, document) {
            return Ok(false);
        }
        // The model is an engine and its memory, which no thread may carry:
        // it is compiled, asked and let go before anything waits.
        let genesis = packaged_model(package, app)?
            .initialize(kind, document, workspace)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if genesis.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "the model's genesis exceeds the bootstrap limit",
            ));
        }
        // What the write is decided against: the package carries the surface,
        // so the receipt is decided without asking a member for it.
        keep_surface(
            &self.inner.host.surfaces,
            app,
            &package_surface(package, app)?,
        );
        // A document members write sealed starts sealed: its genesis is
        // sealed with the workspace's keys, as a native app's would be.
        let keys = self.sealing_keys(&governed, app, document).await?;
        let mut candidate = governed.peer().clone();
        let written = publish_genesis(
            &mut candidate,
            keys.as_ref(),
            (app, document),
            genesis,
            (&self.inner.host.key, system_time(now)),
        )?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        self.carry_written(governed.workspace_id(), &written)?;
        Ok(true)
    }

    /// Apply `action` to the resource `action.document` names (`""` for what
    /// the app keeps per workspace), using the model `action.package`
    /// carries: what a person's window, an agent or a script asks for, from a
    /// holder that never built the app.
    ///
    /// The change is admitted and signed by this device's governor, as its own
    /// action would be. The invocation this device recorded is returned; a peer
    /// holding the resource replays it with the same module
    /// ([`tilo_model_engine::Model::replay`]).
    ///
    /// # Errors
    /// Refuses other devices, stale governance, an app the package is not, a
    /// package with no model, a document this device does not hold, one
    /// members write sealed, a model that refuses the action, and failed
    /// publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor document stays locked through durable signed publication"
    )]
    pub async fn apply_packaged_action(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        action: PackagedAction<'_>,
    ) -> Result<serde_json::Value, CollaborationError> {
        let PackagedAction {
            app,
            kind,
            document,
            package,
            action,
            arguments,
            member,
            acting,
            now_ms,
        } = action;
        let now = tilo_clock::unix_now_ns();
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "packaged application actions require exact governor ownership",
            ));
        }
        ensure_model_runtime(package).await?;
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if !governed.governs_applications(governor) {
            return Err(CollaborationError::new(
                "application governor differs from current workspace authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        governed.peer().check_document_read(
            governor.device_public_key,
            app,
            document,
            system_time(now),
        )?;
        let bytes = governed
            .readable_document(app, document)
            .await
            .map_err(CollaborationError::new)?
            .ok_or_else(|| CollaborationError::new("the document is not prepared"))?;
        // The device checks the arguments against the verb the package's
        // signed surface declares, and writes the change's envelope and
        // history entry; the module only builds the action and applies it.
        let surface = package_surface(package, app)?;
        let verb = surface_verb(&surface, kind, action)?;
        // The model is an engine and its memory, which no thread may carry:
        // it is compiled, asked and let go before anything waits.
        let applied = packaged_model(package, app)?
            .apply(Invocation {
                app,
                kind,
                verb: &verb,
                id: document,
                document: &bytes,
                member: &member,
                acting: &acting,
                arguments,
                now: now_ms,
            })
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let changes = applied.changes;
        if changes.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "the action's changes exceed the publication limit",
            ));
        }
        // The change names the app version's surface, which the package
        // carries: the receipt is decided here.
        keep_surface(&self.inner.host.surfaces, app, &surface);
        // What the model wrote is published as the governor's signed change,
        // sealed with the workspace's keys where its members write sealed.
        let keys = self.sealing_keys(&governed, app, document).await?;
        let mut candidate = governed.peer().clone();
        let written = crate::workspaces::sealed_documents::publish_changes(
            &mut candidate,
            keys.as_ref(),
            (app, document),
            changes,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        self.carry_written(governed.workspace_id(), &[written])?;
        serde_json::to_value(&applied.envelope)
            .map_err(|error| CollaborationError::new(format!("the invocation: {error}")))
    }
}

/// The verb `action` of the resource `kind` the package's signed surface
/// `surface` declares: what the device checks an invocation's arguments
/// against before the app's model builds the action.
///
/// # Errors
/// A surface that does not parse, or one that declares no such verb.
fn surface_verb(
    surface: &[u8],
    kind: &str,
    action: &str,
) -> Result<tilo_actions::surface::VerbSurface, CollaborationError> {
    let surface = collab_types::surface::SurfaceFile::parse(surface)
        .map_err(|error| CollaborationError::new(format!("the package's surface: {error}")))?;
    surface
        .resource(kind)
        .and_then(|resource| resource.verbs.iter().find(|verb| verb.id == action))
        .cloned()
        .ok_or_else(|| CollaborationError::new("the app declares no such action"))
}
