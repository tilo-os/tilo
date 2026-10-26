//! Reading an admitted room document into its workspace, the same way on
//! every host.
use automerge::AutoCommit;
use tilo_desktop_model::{Workspace, projection};

/// Take what an admitted room document says about its workspace: name,
/// policy, sandbox, presets, bots, tasks, shortcuts, schedule claims and
/// effect receipts. Returns whether the workspace changed.
///
/// Lists are replaced, not merged: a revoked grant or capability is simply
/// absent from the document, so carrying the local list over would let it
/// survive. Two things the document cannot take away: an owner (a document
/// that names none leaves it) and a schedule claim this device holds for a
/// shortcut the room still has. Effect receipts are the document's, plus
/// those of `kept` it omits: a receiver with no receipt ledger of its own
/// passes the receipts it already projected, which are immutable.
pub fn adopt_room(
    workspace: &mut Workspace,
    doc: &AutoCommit,
    kept: &[tilo_desktop_model::WorkspaceEffectReceipt],
) -> bool {
    let (name, mut policy) = super::extract_governed_fields(doc);
    let mut changed = false;
    if let Some(name) = name
        && workspace.name != name
    {
        workspace.name = name;
        changed = true;
    }
    if policy.owner.is_none() {
        policy.owner.clone_from(&workspace.policy.owner);
    }
    policy.grants.sort_by(|a, b| a.id.cmp(&b.id));
    let mut current = workspace.policy.clone();
    current.grants.sort_by(|a, b| a.id.cmp(&b.id));
    if current != policy {
        workspace.policy = policy;
        changed = true;
    }
    changed |= replace_sorted(
        &mut workspace.sandbox,
        projection::room_sandbox_of(doc),
        |cap| cap.id.clone(),
    );

    changed |= replace(
        &mut workspace.presets,
        projection::workspace_presets_of(doc),
    );
    changed |= replace(&mut workspace.bots, super::room_bots_of(doc));

    let shortcuts = projection::workspace_shortcuts_of(doc);
    let mut claims = projection::schedule_claims_of(doc);
    for claim in &workspace.schedule_claims {
        match claims.iter_mut().find(|held| held.key() == claim.key()) {
            Some(held) if claim.supersedes(held) => *held = claim.clone(),
            Some(_) => {}
            None => claims.push(claim.clone()),
        }
    }
    claims.retain(|claim| {
        shortcuts
            .iter()
            .any(|shortcut| shortcut.id == claim.shortcut)
    });
    claims.sort_by_key(tilo_automation::ScheduleClaim::key);
    changed |= replace(&mut workspace.shortcuts, shortcuts);
    changed |= replace(&mut workspace.schedule_claims, claims);

    let mut receipts = projection::workspace_effect_receipts_of(doc);
    for held in kept {
        if !receipts
            .iter()
            .any(|receipt| receipt.invocation == held.invocation)
        {
            receipts.push(held.clone());
        }
    }
    changed |= replace_sorted(&mut workspace.effect_receipts, receipts, |receipt| {
        receipt.invocation.0
    });
    changed
}

/// Replace `held` with `next` sorted by `key` unless they hold the same
/// entries, in whatever order `held` keeps them.
fn replace_sorted<T: Clone + PartialEq, K: Ord>(
    held: &mut Vec<T>,
    mut next: Vec<T>,
    key: impl Fn(&T) -> K,
) -> bool {
    next.sort_by_key(&key);
    let mut current = held.clone();
    current.sort_by_key(&key);
    if current == next {
        return false;
    }
    *held = next;
    true
}

fn replace<T: PartialEq>(held: &mut T, next: T) -> bool {
    if *held == next {
        return false;
    }
    *held = next;
    true
}

#[cfg(test)]
mod tests {
    use super::adopt_room;
    use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
    use tilo_desktop_model::{Workspace, WorkspaceId, projection};

    fn shortcut(id: &str) -> tilo_automation::Shortcut {
        tilo_automation::Shortcut {
            id: id.into(),
            name: id.into(),
            inputs: Vec::new(),
            steps: Vec::new(),
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
            saved_proof: None,
        }
    }

    fn room_doc(workspace: &Workspace) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "room", workspace.room().to_string()).unwrap();
        doc.put(ROOT, "name", "Garden").unwrap();
        doc
    }

    #[test]
    fn a_room_document_brings_its_name_and_shortcuts_and_keeps_the_owner() {
        let mut workspace = Workspace::new(WorkspaceId(7), "Old");
        workspace.policy.owner = Some("owner".into());
        let mut doc = room_doc(&workspace);
        projection::project_workspace_shortcuts(&mut doc, &[shortcut("water")]).unwrap();

        assert!(adopt_room(&mut workspace, &doc, &[]));
        assert_eq!(workspace.name, "Garden");
        assert_eq!(workspace.policy.owner.as_deref(), Some("owner"));
        assert_eq!(
            workspace
                .shortcuts
                .iter()
                .map(|shortcut| shortcut.id.as_str())
                .collect::<Vec<_>>(),
            ["water"]
        );
        assert!(
            !adopt_room(&mut workspace, &doc, &[]),
            "nothing changed twice"
        );
    }

    #[test]
    fn a_shortcut_the_room_no_longer_has_goes() {
        let mut workspace = Workspace::new(WorkspaceId(7), "Garden");
        workspace.shortcuts = vec![shortcut("water")];
        let doc = room_doc(&workspace);
        assert!(adopt_room(&mut workspace, &doc, &[]));
        assert!(workspace.shortcuts.is_empty());
    }
}
