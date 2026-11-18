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
//! ([`tilo_model_engine::Answer::envelope`] is what a peer replays).
//!
//! What this side does *not* do is decide anything the app decides: it checks
//! the package's signature, that the package is the app named, and that the
//! governor may write the document; the module builds the action from its
//! vocabulary and the action API binds, checks and keeps it.

use std::collections::BTreeMap;

use super::authority::WorkspaceOp;
use super::genesis::publish_genesis;
use super::{CollaborationError, Workspaces, system_time};
use crate::service_authority::ServiceOperation;
use tilo_actions::MemberId;
use tilo_app_package::blobs::BlobHash;
use tilo_app_package::tapp;
use tilo_model_engine::{Model, Op, Request};
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
/// names it, and the change is decided without asking a member for it.
fn keep_surface(app: &str, surface: &[u8]) {
    collab_types::app_surface::register_package(app, surface);
    crate::surfaces::keep(&hex::encode(blake3::hash(surface).as_bytes()), surface);
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
        self.authorize(
            governor,
            None,
            WorkspaceOp::Host(ServiceOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "packaged application creation requires exact governor ownership",
            ));
        }
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if &governed.record.owner != governor {
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
        let genesis = {
            let model = packaged_model(package, app)?;
            model
                .ask(&Request {
                    op: Op::Initialize,
                    kind,
                    id: document,
                    home: workspace,
                    bytes: &[],
                    member: &hex::encode(governor.user_public_key),
                    acting: None,
                    action: "",
                    arguments: serde_json::Map::new(),
                    now: i64::try_from(now / 1_000_000).unwrap_or(i64::MAX),
                    envelope: None,
                })
                .map_err(|error| CollaborationError::new(error.to_string()))?
                .document
                .ok_or_else(|| CollaborationError::new("the model produced no genesis"))?
        };
        if genesis.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "the model's genesis exceeds the bootstrap limit",
            ));
        }
        // What the write is decided against: the package carries the surface,
        // so the receipt is decided without asking a member for it.
        keep_surface(app, &package_surface(package, app)?);
        // A document members write sealed starts sealed: its genesis is
        // sealed with the workspace's keys, as a native app's would be.
        let keys = self.sealing_keys(&governed, app, document).await?;
        let mut candidate = governed.peer().clone();
        publish_genesis(
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
        Ok(true)
    }

    /// Apply `action` to the resource `action.document` names (`""` for what
    /// the app keeps per workspace), using the model `action.package`
    /// carries: what a person's window, an agent or a script asks for, from a
    /// holder that never built the app.
    ///
    /// The change is admitted and signed by this device's governor, as its own
    /// action would be. The invocation the module made is returned; a peer
    /// holding the resource replays it with the same module
    /// ([`tilo_model_engine::Answer::envelope`]).
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
        self.authorize(
            governor,
            None,
            WorkspaceOp::Host(ServiceOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "packaged application actions require exact governor ownership",
            ));
        }
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if &governed.record.owner != governor {
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
        // The model is an engine and its memory, which no thread may carry:
        // it is compiled, asked and let go before anything waits.
        let answer = {
            let model = packaged_model(package, app)?;
            model
                .ask(&Request {
                    op: Op::Apply,
                    kind,
                    id: document,
                    home: workspace,
                    bytes: &bytes,
                    member: &member_string(&member),
                    acting: serde_json::to_value(&acting).ok(),
                    action,
                    arguments,
                    now: now_ms,
                    envelope: None,
                })
                .map_err(|error| CollaborationError::new(error.to_string()))?
        };
        let changes = answer
            .changes
            .ok_or_else(|| CollaborationError::new("the model wrote nothing"))?;
        if changes.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "the action's changes exceed the publication limit",
            ));
        }
        // The change names the app version's surface, which the package
        // carries: the receipt is decided here.
        keep_surface(app, &package_surface(package, app)?);
        // What the model wrote is published as the governor's signed change,
        // sealed with the workspace's keys where its members write sealed.
        let keys = self.sealing_keys(&governed, app, document).await?;
        let mut candidate = governed.peer().clone();
        crate::workspaces::sealed_documents::publish_changes(
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
        answer
            .envelope
            .ok_or_else(|| CollaborationError::new("the model made no invocation"))
    }
}

/// `member` as the model module reads it ([`MemberId::of_actor_bytes`]).
fn member_string(member: &MemberId) -> String {
    String::from_utf8_lossy(&member.actor_bytes()).into_owned()
}
