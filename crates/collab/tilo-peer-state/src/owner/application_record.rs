//! Host-owned empty application records, before an authenticated creator joins.
//! Neither an app document write nor an arbitrary reader can create ownership.

use crate::owner::{NativeWorkspaceOwner, WorkspaceId, publication};
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// A route checked against the installed package's signed resource declarations.
/// The host supplies those declarations from verified package evidence, never
/// from channel arguments. This value itself is not launch authority.
pub struct ApplicationRecord {
    app: String,
    record: String,
}

impl ApplicationRecord {
    /// Bind the SDK's exact kind to a host-verified installed declaration.
    pub fn declared(
        app: &str,
        record: &str,
        kind: &str,
        declared: &[String],
    ) -> Result<Self, String> {
        if app.is_empty()
            || app.len() > 256
            || record.len() > 4096
            || kind.is_empty()
            || kind.len() > 256
            || [app, record, kind]
                .iter()
                .any(|value| value.chars().any(char::is_control))
            || !declared.iter().any(|value| value == kind)
        {
            return Err("application record has no valid installed resource declaration".into());
        }
        Ok(Self {
            app: app.into(),
            record: record.into(),
        })
    }

    /// A record the host keeps itself, which no installed package declares:
    /// a workspace's shared desktop. Only host code names one; nothing an
    /// app sends reaches this.
    pub fn host(app: &str, record: &str) -> Result<Self, String> {
        if app.is_empty()
            || app.len() > 256
            || record.len() > 4096
            || [app, record]
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            return Err("host record has no valid route".into());
        }
        Ok(Self {
            app: app.into(),
            record: record.into(),
        })
    }
}

impl NativeWorkspaceOwner {
    /// Complete the original creator's bootstrap in a workspace another device
    /// governs (a host that created the empty record for it): the bootstrap
    /// becomes this member's own signed change onto the record's exact
    /// revision, admitted and published like any other write of it. The
    /// governor's signature never covers member-supplied bytes. Answers the
    /// revision this device's committed view then holds.
    pub fn initialize_application_record_as_member(
        &self,
        workspace: WorkspaceId,
        creator: &DeviceCert,
        key: &SigningKey,
        initialization: ApplicationInitialization<'_>,
        now: SystemTime,
    ) -> Result<[u8; 32], String> {
        use crate::owner_commit::{CommitOperation, OwnerCommitRequest};
        if initialization.document.is_empty() || initialization.document.len() > 64 * 1024 {
            return Err("application initialization exceeds bootstrap limit".into());
        }
        let route = initialization.route;
        let adds_state = self.with_current(workspace, |view| {
            member_bootstrap(view, creator, key, initialization, now)
                .map(|changes| changes.is_some())
        })?;
        if adds_state {
            self.commit_document_with_current(
                workspace,
                |view| {
                    let changes = member_bootstrap(view, creator, key, initialization, now)?
                        .ok_or("application initialization revision changed")?;
                    let message = view.peer().clone().publication_mut().publish_incremental(
                        &route.app,
                        &route.record,
                        changes,
                        key,
                        now,
                    )?;
                    let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = message else {
                        return Err("application initialization is not a document write".into());
                    };
                    Ok(OwnerCommitRequest {
                        workspace: workspace.to_string(),
                        operation: CommitOperation::Document {
                            app: route.app.clone(),
                            record: route.record.clone(),
                            envelope,
                        },
                    })
                },
                now,
            )?;
        }
        self.with_current(workspace, |view| {
            match view.read_document(
                creator.device_public_key,
                &route.app,
                &route.record,
                None,
                64 * 1024,
                now,
            )? {
                crate::owner::DocumentRead::Captured(committed) => Ok(committed.revision),
                _ => Err("initialized application capture unavailable".into()),
            }
        })
    }
    /// Create an empty, canonical record as this device, signed with its
    /// own key: the governor's, or a member's the workspace lets create it (its
    /// app may author records here and the member may change the record),
    /// as every receiver admits it. Existing records remain byte-identical.
    /// Publication is durable before this returns; the caller may only then
    /// admit the creator's channel.
    pub fn prepare_application_record(
        &self,
        workspace: WorkspaceId,
        creator: &DeviceCert,
        key: &SigningKey,
        route: &ApplicationRecord,
        now: SystemTime,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        if creator != candidate.device()
            || key.verifying_key().to_bytes() != creator.device_public_key
        {
            return Err("application creator lacks this device's own signing custody".into());
        }
        let member = candidate
            .peer
            .permissions(creator.device_public_key, now)
            .map_err(|error| error.to_string())?;
        let node =
            collab_types::document_node(candidate.peer.workspace_id(), &route.app, &route.record);
        let (created, record) = candidate.change(|live, record| {
            if live.document(&route.app, &route.record).is_some() {
                return Ok(false);
            }
            if creator != candidate.workspace_host() {
                if !member
                    .policy
                    .app_authors(&route.app, &tilo_action_policy::Context::at_time(now))
                {
                    return Err(
                        "the workspace does not let this application author its records".into(),
                    );
                }
                if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, now) {
                    return Err("application record creator may not change the record".into());
                }
            }
            let message = live.publication_mut().publish_document(
                &route.app,
                &route.record,
                automerge::AutoCommit::new().save(),
                key,
                now,
            )?;
            let outgoing = publication::prepare(&publication::request(&message)?)?;
            publication::append_local(record, outgoing)?;
            record.generation = record
                .generation
                .checked_add(1)
                .ok_or("native application generation exhausted")?;
            Ok(true)
        })?;
        if !created {
            return Ok(false);
        }
        self.publish_record(workspace, &candidate, record, None)?;
        Ok(true)
    }
}

/// The changes `initialization` adds to the record `view` holds, as this
/// member (`creator`, signing with `key`) would sign them: `None` when the
/// bootstrap adds nothing. Checks member custody (another device governs)
/// and that the record is still at the revision the creator read.
fn member_bootstrap(
    view: &crate::owner::NativeCommittedView,
    creator: &DeviceCert,
    key: &SigningKey,
    initialization: ApplicationInitialization<'_>,
    now: SystemTime,
) -> Result<Option<Vec<u8>>, String> {
    if creator != view.device()
        || creator == view.workspace_host()
        || key.verifying_key().to_bytes() != creator.device_public_key
    {
        return Err("application initialization lacks current member custody".into());
    }
    let route = initialization.route;
    match view.read_document(
        creator.device_public_key,
        &route.app,
        &route.record,
        Some(initialization.revision),
        64 * 1024,
        now,
    )? {
        crate::owner::DocumentRead::Unchanged(_) => {}
        crate::owner::DocumentRead::CheckpointPending => {
            return Err("application initialization waits for a checkpoint".into());
        }
        crate::owner::DocumentRead::Captured(_) => {
            return Err("application initialization revision changed".into());
        }
    }
    let held = view
        .peer()
        .document(&route.app, &route.record)
        .ok_or("application record is not in the workspace")?;
    let mut record = automerge::AutoCommit::load(&held).map_err(|error| error.to_string())?;
    let heads = record.get_heads();
    // The app's bytes are untrusted: taken in by the isolated admission,
    // onto the record's graph, adding only what the bootstrap brings and
    // never replacing what the workspace holds.
    match tilo_automerge_isolate::apply(
        &mut record,
        initialization.document,
        tilo_automerge_isolate::Dependencies::RequireComplete,
    )
    .map_err(|error| error.to_string())?
    {
        tilo_automerge_isolate::Applied::Changed(_) => {}
        tilo_automerge_isolate::Applied::MissingDependencies => {
            return Err("application initialization does not build on the record".into());
        }
    }
    let changes = record.save_after(&heads);
    if record.get_heads() == heads || changes.is_empty() {
        return Ok(None);
    }
    if changes.len() > 64 * 1024 {
        return Err("application initialization exceeds bootstrap limit".into());
    }
    Ok(Some(changes))
}

#[derive(Clone, Copy)]
pub struct ApplicationInitialization<'a> {
    pub route: &'a ApplicationRecord,
    pub revision: [u8; 32],
    pub document: &'a [u8],
}

#[cfg(test)]
#[path = "application_record_tests.rs"]
pub(crate) mod tests;
