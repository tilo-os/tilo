//! Reading an admitted workspace document into its workspace, the same way on
//! every host.
use automerge::AutoCommit;
use tilo_desktop_model::{Workspace, projection};

/// Take what an admitted workspace document says about its workspace: name,
/// policy, sandbox, presets, bots, tasks, shortcuts, schedule claims and
/// effect receipts. Returns whether the workspace changed.
///
/// Lists are replaced, not merged: a revoked grant or capability is simply
/// absent from the document, so carrying the local list over would let it
/// survive. Two things the document cannot take away: an owner (a document
/// that names none leaves it) and a schedule claim this device holds for a
/// shortcut the workspace still has. Effect receipts are the document's, plus
/// those of `kept` it omits: a receiver with no receipt log of its own
/// passes the receipts it already projected, which are immutable.
pub fn adopt_workspace(
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
        projection::workspace_sandbox_of(doc),
        |cap| cap.id.clone(),
    );

    changed |= replace(
        &mut workspace.presets,
        projection::workspace_presets_of(doc),
    );

    changed |= replace(
        &mut workspace.shortcuts,
        projection::workspace_shortcuts_of(doc),
    );

    let mut receipts = projection::workspace_effect_receipts_of(doc);
    for held in kept {
        if !receipts
            .iter()
            .any(|receipt| receipt.invocation == held.invocation)
        {
            receipts.push(held.clone());
        }
    }
    // The newest receipts only, as the layout keeps them.
    tilo_desktop_model::receipt::bounded_receipts(&mut receipts);
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
    use super::adopt_workspace;
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable as _};
    use tilo_desktop_model::{Workspace, WorkspaceId, projection};

    fn shortcut(id: &str) -> tilo_automation::SavedShortcut {
        tilo_automation::SavedShortcut::encode(&tilo_automation::Shortcut {
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
            paused: false,
        })
    }

    fn workspace_doc(workspace: &Workspace) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "workspace", workspace.id().to_string())
            .unwrap();
        doc.put(ROOT, "name", "Garden").unwrap();
        // The map a workspace is founded with, which its shortcuts sit in.
        doc.put_object(ROOT, projection::WORKSPACE_AUTOMATIONS_KEY, ObjType::Map)
            .unwrap();
        doc
    }

    tilo_test_env::scenario! {
        fn adopting_a_workspace_document_takes_its_name_and_its_shortcuts;

        phase a_workspace_document_brings_its_name_and_shortcuts_and_keeps_the_owner;
        {
            let mut workspace = Workspace::new(WorkspaceId::from_u128(7), "Old");
            workspace.policy.owner = Some("owner".into());
            let mut doc = workspace_doc(&workspace);
            projection::project_workspace_shortcuts(&mut doc, &[shortcut("water")]).unwrap();

            assert!(adopt_workspace(&mut workspace, &doc, &[]));
            assert_eq!(workspace.name, "Garden");
            assert_eq!(workspace.policy.owner.as_deref(), Some("owner"));
            assert_eq!(
                workspace
                    .shortcuts
                    .iter()
                    .map(tilo_automation::SavedShortcut::id)
                    .collect::<Vec<_>>(),
                ["water"]
            );
            assert!(
                !adopt_workspace(&mut workspace, &doc, &[]),
                "nothing changed twice"
            );
        }

        phase a_shortcut_the_workspace_no_longer_has_goes;
        {
            let mut workspace = Workspace::new(WorkspaceId::from_u128(7), "Garden");
            workspace.shortcuts = vec![shortcut("water")];
            let doc = workspace_doc(&workspace);
            assert!(adopt_workspace(&mut workspace, &doc, &[]));
            assert!(workspace.shortcuts.is_empty());
        }
    }
}
