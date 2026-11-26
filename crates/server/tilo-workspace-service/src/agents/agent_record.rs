//! The canonical workspace agent roster, including rosters older hosts founded
//! when the first computer agent arrived, before that writer named its record.

use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _};
use collab_types::document::{DocumentCheckout, DocumentId, ResourceType, fields, identity};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::Resource as _;
use tilo_workspace_agent::{OWNER, WorkspaceAgents};

use crate::workspaces::Workspaces;

pub(super) fn checkout() -> Result<DocumentCheckout, String> {
    let id = collab_types::workspace_document_id(WorkspaceAgents::KIND);
    Ok(DocumentCheckout::new(
        DocumentId::parse(&id).ok_or("invalid workspace roster id")?,
        ResourceType::new(WorkspaceAgents::KIND),
    ))
}

pub(crate) async fn open(
    workspaces: &Workspaces,
    workspace: &str,
    bytes: &[u8],
    computer: Option<&str>,
) -> Result<AutoCommit, String> {
    let checkout = checkout()?;
    let mut held = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let Err(mismatch) = validate(&held, &checkout, workspace) else {
        return Ok(held);
    };
    let Some(computer) = computer else {
        return Err(mismatch);
    };
    let Some(mut founding) =
        identity::founding_for_unnamed(&held, &checkout.id, &checkout.kind, workspace)
    else {
        return Err(mismatch);
    };
    let Some(registration) = first_registration(&mut held, computer) else {
        return Err(mismatch);
    };
    let record = checkout.id.as_str();
    if !host_registered(workspaces, workspace, record, registration).await? {
        return Err("the workspace cannot verify who founded this unnamed agent roster".into());
    }

    let heads = held.get_heads();
    Box::pin(workspaces.author_workspace_document(
        workspace,
        workspaces.governor(),
        (OWNER, record),
        |current| {
            if current.get_heads() == heads {
                // Only the identity is merged. The old list object, its entries and their
                // changes all keep their identities, including the keeper's later bounds.
                current
                    .merge_incoming(&mut founding)
                    .map_err(|error| error.to_string())?;
            }
            // A concurrent recovery is fine. A different or newly named record is checked
            // on its own terms; an arriving write to the unnamed one is retried on reread.
            validate(current, &checkout, workspace)?;
            Ok(current.clone())
        },
    ))
    .await
    .map_err(|error| error.to_string())
}

pub(super) fn validate(
    doc: &AutoCommit,
    checkout: &DocumentCheckout,
    workspace: &str,
) -> Result<(), String> {
    collaboration::resource::validate_identity(doc, checkout, workspace)
        .map_err(|error| error.to_string())?;
    if identity::home(doc).as_deref() != Some(workspace) {
        return Err("the workspace agent roster was founded in another workspace".into());
    }
    Ok(())
}

/// The old writer's first two changes: the SDK test replica's empty resource followed by
/// Assistant registering one deterministically identified computer for its keeper. A document
/// that ever held an identity, even if a later change removed it, cannot take another one.
fn first_registration(doc: &mut AutoCommit, computer: &str) -> Option<ChangeHash> {
    let changes = doc.get_changes(&[]);
    let mut roots = changes.iter().filter(|change| change.deps().is_empty());
    let root = roots.next()?;
    if roots.next().is_some() {
        return None;
    }
    let mut bare = AutoCommit::new().with_actor(root.actor_id().clone());
    WorkspaceAgents::initialize(&mut bare).ok()?;
    let genesis = bare.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    )?;
    if genesis != root.hash()
        || changes.iter().any(|change| {
            !doc.get_all_at(ROOT, fields::META, &[change.hash()])
                .is_ok_and(|found| found.is_empty())
        })
    {
        return None;
    }
    let mut children = changes.iter().filter(|change| change.deps() == [genesis]);
    let first = children.next()?;
    let entry = tilo_actions::ActionContext::from_change_message(first.message()?)?;
    let tilo_actions::Did::Verb(call) = entry.what? else {
        return None;
    };
    if children.next().is_some()
        || entry.app?.id != OWNER
        || call.action.kind != WorkspaceAgents::KIND
        || call.action.action != WorkspaceAgents::CREATE_AGENT.id()
    {
        return None;
    }
    let registry =
        tilo_actions::ActionRegistry::with(tilo_resource::group::register::<WorkspaceAgents>);
    if !matches!(
        tilo_actions::verify_change(doc, first.hash(), &registry),
        Ok(tilo_actions::Verified::Reproduced)
    ) {
        return None;
    }
    let original = WorkspaceAgents::read(&doc.fork_at(&[first.hash()]).ok()?).ok()?;
    let [agent] = original.agents.as_slice() else {
        return None;
    };
    let client = agent.mcp_client.as_deref()?;
    let person = hex::decode(&agent.keeper).ok()?.try_into().ok()?;
    (agent.name == client
        && agent.model.is_none()
        && agent.id.0 == computer
        && computer == tilo_actions::remote::device_agent_id(&person, client))
    .then(|| first.hash())
}

/// Corroborate the original two-change publication against this workspace's admitted governor
/// record. A later checkpoint covering members' writes is not evidence that the governor founded it.
async fn host_registered(
    workspaces: &Workspaces,
    workspace: &str,
    record: &str,
    registration: ChangeHash,
) -> Result<bool, String> {
    let peer = workspaces
        .state(workspace)
        .await
        .ok_or("workspace is not held here")?;
    let governor = workspaces.governor().device_public_key;
    if peer.publication().is_sealed_document(OWNER, record) {
        let envelopes = peer.publication().sealed_history(OWNER, record);
        let Some(envelope) = envelopes.first() else {
            return Ok(false);
        };
        if envelope.kind != collab_types::OpKind::Change || envelope.author.as_bytes() != &governor
        {
            return Ok(false);
        }
        // Open the exact admitted Change envelope we just inspected. A compaction can
        // occupy the same sequence after that inspection, but cannot change these signed bytes.
        let Some(base) = peer.document(OWNER, record) else {
            return Ok(false);
        };
        if !AutoCommit::load(&base)
            .map_err(|error| error.to_string())?
            .get_heads()
            .is_empty()
            || !envelope.is_whole_resource()
            || envelope.seq > 2
        {
            return Ok(false);
        }
        let device = workspaces
            .device_keys()
            .ok_or("this device holds no document keys")?;
        let scoped = peer.requires_document_keys(OWNER, record, tilo_clock::wall())?;
        let keys = crate::documents::sealed::read_document_keys(
            &device,
            workspace,
            (OWNER, record),
            scoped,
            false,
        )
        .await?;
        let contents = keys.open()(envelope)?;
        let collab_types::DocumentMutation::Snapshot(bytes) =
            collab_types::DocumentMutation::decode(&contents).map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let mut original = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        return Ok(original.get_heads() == [registration]);
    }

    // Only the verified checkpoint may attest who wrote the clear record. An earlier
    // intermediate value inside a governance batch carries no independent authority.
    Ok(peer
        .durable_coverage(OWNER, record)?
        .is_some_and(|receipt| {
            let row = receipt.coverage;
            row.author == governor
                && row.heads == [registration.to_string()]
                && row.watermarks.len() == 1
                && row.watermarks.contains_key(&hex::encode(governor))
        }))
}
