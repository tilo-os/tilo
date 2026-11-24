use super::*;

/// A device that may no longer write refuses its own change before it goes: its author's session is
/// told the same typed refusal a server's refusal of a change is — the document, the changes it
/// carried and why — the workspace files the verdict where the person's activity reads it, and
/// nothing of the change is sent. The change the device made while it could write stays sent, and
/// once its person may write again the change lands and the verdict goes.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: the granted change, the refused one, the verdict and the \
              change that lands stay together"
)]
async fn a_device_that_may_not_write_refuses_its_own_change_and_files_why() {
    use crate::owner::WriteRefusal;
    use tilo_server_protocol::refusal::Refusal;

    // The member's chat change, as the owner's turn hears it, and the hash it carries.
    fn edit(
        view: &crate::owner::NativeCommittedView,
        now: SystemTime,
        topic: &str,
    ) -> Result<(collab_types::SignedOp, [u8; 32]), String> {
        let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
        let before = chat.get_heads();
        chat.put(ROOT, "topic", topic).unwrap();
        let change = chat.commit_edit("chat", "topic", &crate::owner::testing::member_person());
        let frame = collab_types::DocumentMutation::Incremental(chat.save_after(&before))
            .encode()
            .map_err(|error| error.to_string())?;
        let envelope = view.peer().draft_write(
            (APP, ""),
            None,
            &frame,
            None,
            crate::owner::testing::member_key()
                .verifying_key()
                .to_bytes(),
            0,
            now,
        )?;
        let mut envelope = envelope;
        collab_types::sign_op(&mut envelope, &crate::owner::testing::member_key())
            .map_err(|error| error.to_string())?;
        Ok((envelope, change.unwrap().0))
    }

    // The writes this device would send to the workspace, and whether one carries `change`.
    fn sends_change(
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        _now: SystemTime,
        change: [u8; 32],
    ) -> bool {
        owner.frames(workspace).iter().any(|bytes| {
            let Ok(NeighbourMsg::AppDocument { envelope, .. }) =
                postcard::from_bytes::<NeighbourMsg>(bytes)
            else {
                return false;
            };
            if envelope.claims.iter().any(|claim| claim.change == change) {
                return true;
            }
            let Ok(mutation) = collab_types::DocumentMutation::decode(&envelope.payload) else {
                return false;
            };
            let bytes = match mutation {
                collab_types::DocumentMutation::Snapshot(bytes)
                | collab_types::DocumentMutation::Incremental(bytes) => bytes,
            };
            collab_types::parse_changes(&bytes)
                .unwrap_or_default()
                .iter()
                .any(|carried| carried.hash().0 == change)
        })
    }

    let (fixture, mut governor_peer, governor_key) =
        crate::owner::testing::governed_fixture_at(tilo_clock::wall());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let person = SigningKey::from_bytes(&[64; 32]).verifying_key().to_bytes();
    let governor = governor_key.verifying_key().to_bytes();
    let later = now.checked_add(std::time::Duration::from_secs(1)).unwrap();

    // A change it makes while it may write is kept and sent.
    let (granted_write, granted_change) = owner
        .with_current(workspace, |view| edit(view, now, "written while editor"))
        .unwrap();
    super::commit(owner, workspace, granted_write, now).unwrap();
    assert_eq!(
        owner.frames(workspace).len(),
        1,
        "the change it may make is committed"
    );
    assert!(
        sends_change(owner, workspace, now, granted_change),
        "the change it may make is sent"
    );

    // Its person may read and no longer write: the workspace says so to every device.
    assert!(
        governor_peer
            .governance_mut()
            .set_member_role(person, crate::join::Role::Viewer, &governor_key, later)
            .unwrap()
    );
    owner
        .receive(
            workspace,
            TransportOrigin::ServerDelta(governor),
            governor_peer.workspace_message(),
            later,
        )
        .unwrap();

    // The change it makes now is refused before it is sent, told as a server's refusal is told.
    let (refused_write, refused_change) = owner
        .with_current(workspace, |view| edit(view, later, "written while viewer"))
        .unwrap();
    let refused = super::commit(owner, workspace, refused_write, later)
        .expect_err("the workspace refuses the change");
    let WriteRefusal::ChangeRefused(refused) = refused else {
        panic!("the refusal is the change's own: {refused:?}");
    };
    assert_eq!(refused.app_id, APP, "it names the app it writes");
    assert_eq!(refused.document_id, "", "it names the document it writes");
    assert_eq!(refused.workspace_id, workspace.to_string());
    assert_eq!(
        refused.cluster_id,
        workspace.to_string(),
        "it names the workspace its document is in"
    );
    assert!(
        matches!(&refused.refusal, Refusal::NotGranted(reason) if reason.contains("not granted")),
        "it says the author is not granted the change: {:?}",
        refused.refusal
    );
    assert!(
        refused.changes.contains(&refused_change),
        "it names the change it carried: {:?} does not hold {}",
        refused.changes,
        hex::encode(refused_change)
    );
    assert!(
        !sends_change(owner, workspace, now, refused_change),
        "nothing of the refused change is sent"
    );

    // Its person may write again: the next change lands, and the verdict goes.
    assert!(
        governor_peer
            .governance_mut()
            .set_member_role(person, crate::join::Role::Editor, &governor_key, later)
            .unwrap()
    );
    owner
        .receive(
            workspace,
            TransportOrigin::ServerDelta(governor),
            governor_peer.workspace_message(),
            later,
        )
        .unwrap();
    let (taken, _) = owner
        .with_current(workspace, |view| {
            edit(view, later, "written while editor again")
        })
        .unwrap();
    super::commit(owner, workspace, taken, later).unwrap();
    assert_eq!(owner.frames(workspace).len(), 2, "the change lands");
}

/// A change of the chat record over `base` that says a caller on `device`, of `person`, asked for
/// it, and keeps no invocation that caller signed: what a device writing for a caller it never
/// heard from makes.
pub(super) fn forged_for_caller(base: &[u8], person: [u8; 32], device: [u8; 32]) -> Vec<u8> {
    use tilo_actions::{
        ActionContext, ActionInvoker, Did, EditOp, FieldEdit, InvocationId, InvokerDevice, PathStep,
    };
    let mut chat = AutoCommit::load(base).unwrap();
    chat.set_actor(automerge::ActorId::from([9; 16]));
    let before = chat.get_heads();
    chat.put(ROOT, "topic", "forged").unwrap();
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: "chat".to_owned(),
            field: "topic".to_owned(),
            path: vec![PathStep::Field("topic".to_owned())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        invoker: Some(ActionInvoker::person(hex::encode(person)).from_device(Some(
            InvokerDevice::certified(device, "the caller's device"),
        ))),
        source_invocation: Some(InvocationId::new()),
        ..ActionContext::for_resource("chat", "chat", "Chat")
    };
    chat.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    chat.save_after(&before)
}

/// A device drafting a sealed change opens nothing a device without the key could check, so it
/// decides the change's contents itself, by the check every device with the keys runs when it reads the record:
/// a change it makes for a caller who never signed is refused before it goes, told as a server's
/// refusal of a change is told, while a change of its own drafts as before.
#[test]
fn a_device_refuses_its_own_sealed_change_for_a_caller_who_never_signed() {
    use crate::owner::{LocalWrite, WriteRefusal, draft_local_write};
    use tilo_server_protocol::refusal::Refusal;

    let joined = enrolled();
    let (_, keys) = rotated_channel();
    let device = crate::owner::testing::member_key()
        .verifying_key()
        .to_bytes();
    let base = joined
        .owner
        .with_current(joined.workspace, |view| {
            view.peer()
                .document(APP, "")
                .ok_or_else(|| "no chat".into())
        })
        .unwrap();
    let drafted = |changes: &[u8]| {
        joined
            .owner
            .with_current(joined.workspace, |view| {
                Ok(draft_local_write(
                    view.peer(),
                    WORKSPACE,
                    &device,
                    LocalWrite {
                        app: APP,
                        document: "",
                        readers: None,
                        changes,
                        founding: false,
                        snapshot: false,
                        sealed: Some(&keys),
                        now: joined.now,
                    },
                )
                .map(|_| ()))
            })
            .unwrap()
    };

    let mut own = AutoCommit::load(&base).unwrap();
    let before = own.get_heads();
    own.put(ROOT, "topic", "its own").unwrap();
    own.commit_edit("chat", "topic", &crate::owner::testing::member_person());
    assert_eq!(
        drafted(&own.save_after(&before)),
        Ok(()),
        "a change of its own drafts"
    );

    // It writes for its own person, as though that person had asked on it, and keeps no
    // invocation they signed.
    let person = joined
        .owner
        .with_current(joined.workspace, |view| {
            view.peer()
                .certificates()
                .into_iter()
                .find(|cert| cert.device_public_key == device)
                .map(|cert| cert.user_public_key)
                .ok_or_else(|| "this device's certificate".into())
        })
        .unwrap();
    let forged = forged_for_caller(&base, person, device);
    let forged_change = collab_types::parse_changes(&forged).unwrap()[0].hash().0;
    let refused = drafted(&forged).expect_err("the workspace refuses the change");
    let WriteRefusal::ChangeRefused(refused) = refused else {
        panic!("the refusal is the change's own: {refused:?}");
    };
    assert_eq!(refused.app_id, APP, "it names the app it writes");
    assert_eq!(refused.document_id, "", "it names the document it writes");
    assert!(
        matches!(&refused.refusal, Refusal::NotAllowed(reason) if reason.contains("signed")),
        "it says the caller signed no invocation: {:?}",
        refused.refusal
    );
    assert!(
        refused.changes.contains(&forged_change),
        "it names the change it carried: {:?}",
        refused.changes
    );
}
