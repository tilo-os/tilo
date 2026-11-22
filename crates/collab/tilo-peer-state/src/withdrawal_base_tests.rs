//! A copy whose device may not write the workspace's document holds a
//! member's withdrawal of their work from a server; then the base its
//! governor made without that withdrawal reaches it. The copy refuses that
//! base, so it knows of governance it does not hold: from then on it holds
//! every write it would take in, and the base it takes later decides them.
use std::sync::Arc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
use collab_types::claim::ClaimedAction;
use collab_types::{ReceiptSurface, SurfaceSource, Surfaces, UnitPath, WrittenDocument};
use ed25519_dalek::SigningKey;
use tilo_action_policy::ActionKind;
use tilo_actions::{ActionContext, Did, EditOp, FieldEdit, PathStep};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::join::{
    InviteGrant, JoinChallenge, Role, WorkspaceId, issue_invitation, sign_join_proof,
};
use crate::owner::testing::member_certificate as certificate;
use crate::owner::testing::{now, receive};
use crate::{
    DelegatedWork, NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin,
    seed_workspace,
};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const DOCUMENT: &str = "tab:1";
const KIND: &str = "test";
/// The surface the test document's identity names.
const PINNED: [u8; 32] = [7; 32];
/// The key a document keeps its identity under, as the engine reads it.
const IDENTITY: &str = "tilo:record";
/// The time the members join at, in nanoseconds, the time their answer to
/// the join's question ends, and the time their invitations end.
const JOINED_NS: u64 = 2_000_000_000_000;
const ANSWER_ENDS_NS: u64 = 2_001_000_000_000;
const INVITATION_ENDS_NS: u64 = 2_010_000_000_000;

/// The test app's surface: every field offers edits, and an edit writes the
/// document.
struct App;

impl ReceiptSurface for App {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, _: &str) -> Option<(ActionKind, Option<String>)> {
        None
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        true
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
}

/// Finds the test app's surface.
struct Source;

impl SurfaceSource for Source {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        Some(Arc::new(App) as Arc<dyn ReceiptSurface>)
    }
}

/// The owner's device, which governs the test workspace.
fn owner() -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &key, "owner");
    (key, owner)
}

/// The owner's copy of a workspace that holds one document of the test app.
fn workspace() -> StatePeer {
    let (owner_key, owner) = owner();
    let mut peer = StatePeer::new(
        WORKSPACE,
        owner.clone(),
        vec![owner.clone()],
        RevocationList::new(),
        now(),
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "seed").unwrap();
    let meta = document.put_object(ROOT, IDENTITY, ObjType::Map).unwrap();
    document.put(&meta, "surface", hex::encode(PINNED)).unwrap();
    document.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &owner, std::slice::from_ref(&owner))
                .unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                document_id: DOCUMENT.into(),
                kind: KIND.into(),
                bytes: document.save(),
            }],
        },
        &owner_key,
        now(),
    )
    .unwrap();
    peer.set_surfaces(Surfaces::from_source(Arc::new(Source)));
    peer
}

/// A member of another person, invited and admitted with `role`: their
/// device's key and its certificate. `bytes` tells one member's keys and
/// invitation from another's.
fn admitted(
    peer: &mut StatePeer,
    role: Role,
    bytes: [u8; 5],
    name: &str,
) -> (SigningKey, DeviceCert) {
    let (owner_key, owner) = owner();
    let [root, device, nonce, first, second] = bytes;
    let key = SigningKey::from_bytes(&[device; 32]);
    let member = certificate(&SigningKey::from_bytes(&[root; 32]), &key, name);
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
            issuer: owner,
            role,
            nonce: [nonce; 16],
            recipient_user_public_key: Some(member.user_public_key),
            invitee_name: Some(name.into()),
            expires_at_ns: INVITATION_ENDS_NS,
            transport: vec![],
        },
        &owner_key,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(member.user_public_key),
        &RevocationList::new(),
        JOINED_NS,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &member,
        [first; 32],
        [second; 32],
        ANSWER_ENDS_NS,
        JOINED_NS,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &key).unwrap();
    let redemption = crate::join::verify_join_proof(
        &proof,
        &challenge,
        &member,
        &RevocationList::new(),
        JOINED_NS,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &owner_key, now())
            .unwrap()
    );
    (key, member)
}

/// A server of another person, named among the workspace's key-holding
/// holders: its device key, its certificate.
fn named_server(peer: &mut StatePeer, owner: &SigningKey) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[92; 32]);
    let server = certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Household server");
    let hint = crate::owner::testing::governor::holding_hint(
        &server,
        &key,
        tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap(),
        iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&server.device_public_key).unwrap(),
        ),
    );
    assert!(
        peer.governance_mut()
            .author_keeping_device(&hint, owner, now())
            .unwrap()
    );
    (key, server)
}

/// The member's delegation of the test app's undoable work to `server`,
/// signed by their own device `key`.
fn delegating(
    key: &SigningKey,
    person: [u8; 32],
    server: [u8; 32],
) -> collab_types::work_delegation::WorkDelegation {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let member = Grantee::person(hex::encode(person));
    let grant = Grant::new(
        "delegated-work",
        member.clone(),
        TreePath::workspace(WORKSPACE).app(APP),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        member,
    );
    let at = u64::try_from(
        now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    collab_types::work_delegation::WorkDelegation::sign(
        key,
        person,
        server,
        WORKSPACE,
        None,
        vec![grant],
        Vec::new(),
        (at.saturating_sub(1), at.saturating_add(10_000_000_000)),
    )
    .unwrap()
}

/// What `write` changes on `peer`, as a receiver takes the changes in.
fn workspace_changes(peer: &mut StatePeer, write: impl FnOnce(&mut StatePeer)) -> Vec<Vec<u8>> {
    let heads = peer.admitted_workspace().unwrap().clone().get_heads();
    write(peer);
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// Every change of `peer`'s workspace document: a base and what was written
/// on it.
fn whole_workspace(peer: &StatePeer) -> Vec<Vec<u8>> {
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// What `peer` holds of the workspace's document that `other` does not.
fn workspace_past(peer: &StatePeer, other: &StatePeer) -> Vec<Vec<u8>> {
    let heads = other.admitted_workspace().unwrap().clone().get_heads();
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// `changes` of the workspace's document arriving at `copy` from `device`.
fn take_workspace_changes(
    copy: &mut StatePeer,
    device: [u8; 32],
    changes: Vec<Vec<u8>>,
) -> Result<(), String> {
    copy.receive_from(
        TransportOrigin::ServerDelta(device),
        NeighbourMsg::SyncResp {
            changes,
            workspace: WORKSPACE.into(),
        },
        now(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// The server's signed write of the document's value as the work of
/// `member`, its first, drafted on `copy`.
fn server_write(
    copy: &StatePeer,
    key: &SigningKey,
    member: [u8; 32],
    server: [u8; 32],
) -> NeighbourMsg {
    let invoker = tilo_actions::ActionInvoker::person(hex::encode(member)).from_device(Some(
        tilo_actions::InvokerDevice::certified(server, "Household server"),
    ));
    signed_write(copy, key, "set by the server", Some(invoker))
}

/// The signed write of the document's value as `text` by the device of `key`,
/// its first, drafted on `copy`: a member's own, or a server's naming
/// `invoker`.
fn signed_write(
    copy: &StatePeer,
    key: &SigningKey,
    text: &str,
    invoker: Option<tilo_actions::ActionInvoker>,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&copy.document(APP, DOCUMENT).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "value", text).unwrap();
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: KIND.into(),
            field: "value".into(),
            path: vec![PathStep::Field("value".into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        ..ActionContext::for_resource(DOCUMENT, KIND, "Test")
    };
    let context = match invoker {
        Some(invoker) => context.with_invoker(invoker),
        None => context,
    };
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, DOCUMENT),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, DOCUMENT),
        collab_types::OpKind::Change,
        now(),
        1,
    )
    .with_surface(Some(PINNED));
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: DOCUMENT.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// The document's `value` as `copy` holds it.
fn value(copy: &StatePeer) -> String {
    AutoCommit::load(&copy.document(APP, DOCUMENT).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
}

/// Whether `copy` takes `write`, which sets the document's value to `text`,
/// from `sender`, and what it answered. The copy itself is left as it was.
fn takes(
    copy: &StatePeer,
    sender: &DeviceCert,
    write: &NeighbourMsg,
    text: &str,
) -> (bool, String) {
    let mut copy = copy.clone();
    let answered = receive(&mut copy, sender, write.clone())
        .map_or_else(|error| error, |()| "taken in".to_owned());
    (value(&copy).contains(text), answered)
}

/// The grant that lets the viewer `person` write the test app, given by the
/// owner.
fn writing_grant(person: [u8; 32], owner: [u8; 32]) -> tilo_action_policy::Grant {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    Grant::new(
        "viewer-writes-the-test-app",
        Grantee::person(hex::encode(person)),
        TreePath::workspace(WORKSPACE).app(APP),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person(hex::encode(owner)),
    )
}

/// What the governor does on its new base.
enum Change {
    /// It makes the editor a viewer.
    Demotes,
    /// It removes the editor.
    Removes,
    /// It takes the viewer's grant back.
    Revokes,
}

/// A viewer's copy that holds a member's withdrawal and was handed the base
/// its governor made without it, and the members whose part the governor
/// changed on that base.
struct Behind {
    /// The workspace before the withdrawal and before the governor's base.
    before: StatePeer,
    /// The governor on its new base, after what it changed there.
    governor: StatePeer,
    /// The viewer's copy after the governor's whole workspace document was
    /// handed to it.
    copy: StatePeer,
    /// What the copy answered to that base.
    answered: Result<(), String>,
    /// An editor's device.
    editor_key: SigningKey,
    editor: DeviceCert,
    /// The device of a viewer whom a grant of the governor's lets write the
    /// test app.
    granted_key: SigningKey,
    granted: DeviceCert,
    /// The device of the member who withdrew, an editor before the governor's
    /// change and after it, and its copy, which holds the withdrawal.
    guest_key: SigningKey,
    guest: DeviceCert,
    holder: StatePeer,
    /// The server the member withdrew their work from.
    server_key: SigningKey,
    server: DeviceCert,
    /// The governor's device, and the viewer's device whose copy is behind.
    owner: DeviceCert,
    viewer: DeviceCert,
}

impl Behind {
    /// The story; `change` is what the governor does on its new base.
    fn made(change: &Change) -> Self {
        let (owner_key, owner) = owner();
        let mut peer = workspace();
        let (guest_key, guest) = admitted(&mut peer, Role::Editor, [41, 42, 43, 44, 45], "Guest");
        let (_, viewer) = admitted(&mut peer, Role::Viewer, [61, 62, 63, 64, 65], "Viewer");
        let (editor_key, editor) =
            admitted(&mut peer, Role::Editor, [71, 72, 73, 74, 75], "Editor");
        let (granted_key, granted) =
            admitted(&mut peer, Role::Viewer, [81, 82, 83, 84, 85], "Granted");
        let grant = writing_grant(granted.user_public_key, owner.user_public_key);
        let put = peer.governance_mut().put_governed_grants(
            std::slice::from_ref(&grant),
            &owner_key,
            now(),
        );
        assert_eq!(put, Ok(true), "the governor gives the viewer a grant");
        let (server_key, server) = named_server(&mut peer, &owner_key);
        let (person, device) = (guest.user_public_key, server.device_public_key);
        let first = delegating(&guest_key, person, device);
        let given = peer
            .workspace_mut()
            .author_work_delegation(&first, &guest_key, now());
        assert_eq!(given, Ok(true), "the member delegates");
        let before = peer.clone();

        // The viewer's device holds the workspace and takes the withdrawal in.
        let mut copy = before.clone();
        copy.write_as_device(viewer.device_public_key);
        let withdrawal = workspace_changes(&mut peer, |member| {
            let written =
                member
                    .workspace_mut()
                    .revoke_work_delegation(&device, &person, &guest_key, now());
            assert_eq!(written, Ok(true), "the member withdraws");
        });
        take_workspace_changes(&mut copy, guest.device_public_key, withdrawal).unwrap();

        // The governor never saw the withdrawal: a new base, then its change.
        let mut governor = before.clone();
        assert_eq!(governor.rebase_workspace(), Ok(true));
        match change {
            Change::Demotes => {
                governor
                    .governance_mut()
                    .set_member_role(editor.user_public_key, Role::Viewer, &owner_key, now())
                    .unwrap();
            }
            Change::Removes => {
                governor
                    .governance_mut()
                    .remove_member(editor.user_public_key, &owner_key, now())
                    .unwrap();
            }
            Change::Revokes => {
                let taken_back = governor.governance_mut().remove_governed_grants(
                    std::slice::from_ref(&grant.id),
                    &owner_key,
                    now(),
                );
                assert_eq!(taken_back, Ok(true), "the governor takes the grant back");
            }
        }
        let answered = take_workspace_changes(
            &mut copy,
            owner.device_public_key,
            whole_workspace(&governor),
        );
        Self {
            before,
            governor,
            copy,
            answered,
            editor_key,
            editor,
            granted_key,
            granted,
            guest_key,
            guest,
            holder: peer,
            server_key,
            server,
            owner,
            viewer,
        }
    }

    /// The write `key`'s member signs after the governor took away what let
    /// them write is refused by the viewer's copy, as by the governor; it
    /// passed before the governor's change.
    fn refuses(&self, key: &SigningKey, sender: &DeviceCert, who: &str) {
        let text = "written after the governor's change";
        let write = signed_write(&self.before, key, text, None);
        let mut wrong = Vec::new();
        let (stood, answer) = takes(&self.before, sender, &write, text);
        if !stood {
            wrong.push(format!(
                "the write does not pass before the governor's change: {answer}"
            ));
        }
        let (governed, answer) = takes(&self.governor, sender, &write, text);
        if governed {
            wrong.push(format!(
                "the governor itself takes the write of {who}: {answer}"
            ));
        }
        let (taken, answer) = takes(&self.copy, sender, &write, text);
        if taken {
            wrong.push(format!(
                "the viewer's copy TAKES the write of {who} ({answer}); its governor's base was \
                 answered {:?}",
                self.answered
            ));
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }
}

/// A viewer's copy that holds a withdrawal and refused its governor's base
/// refuses the write of a member the governor made a viewer on that base.
#[test]
fn a_copy_that_refused_its_governors_base_refuses_the_write_of_a_member_the_governor_demoted() {
    let story = Behind::made(&Change::Demotes);
    story.refuses(
        &story.editor_key,
        &story.editor,
        "a member the governor made a viewer",
    );
}

/// It refuses the write of a member the governor removed on that base.
#[test]
fn a_copy_that_refused_its_governors_base_refuses_the_write_of_a_member_the_governor_removed() {
    let story = Behind::made(&Change::Removes);
    story.refuses(
        &story.editor_key,
        &story.editor,
        "a member the governor removed",
    );
}

/// It refuses the use of a grant the governor took back on that base.
#[test]
fn a_copy_that_refused_its_governors_base_refuses_the_use_of_a_grant_the_governor_revoked() {
    let story = Behind::made(&Change::Revokes);
    story.refuses(
        &story.granted_key,
        &story.granted,
        "a viewer whose grant the governor took back",
    );
}

/// The work of the server the member withdrew from is refused on that copy as
/// it was before the base came: never held for the base, never taken.
#[test]
fn a_copy_that_refused_its_governors_base_still_refuses_the_withdrawn_servers_work() {
    let story = Behind::made(&Change::Demotes);
    let (person, device) = (story.guest.user_public_key, story.server.device_public_key);
    let write = server_write(&story.before, &story.server_key, person, device);
    assert!(
        receive(&mut story.before.clone(), &story.server, write.clone()).is_ok(),
        "the server works while the delegation is live"
    );
    let work = story
        .copy
        .workspace()
        .delegated_work(&device, &person, now());
    assert!(
        matches!(work, DelegatedWork::Withdrawn { .. }),
        "the viewer's copy reads the work as {work:?}"
    );
    let answered = receive(&mut story.copy.clone(), &story.server, write);
    assert!(
        answered.is_err(),
        "the viewer's copy holds or takes the withdrawn server's work instead of refusing it"
    );
}

/// The base the copy takes later decides what it held: the write of the
/// member the governor made a viewer is refused, the write of a member who may
/// still write is taken in, and neither was taken in or lost meanwhile.
#[test]
fn a_base_the_copy_takes_later_decides_the_writes_it_held() {
    let story = Behind::made(&Change::Demotes);
    let demoted = "written by the member the governor made a viewer";
    let entitled = "written by a member who may still write";
    let of_demoted = signed_write(&story.before, &story.editor_key, demoted, None);
    let of_entitled = signed_write(&story.before, &story.guest_key, entitled, None);

    // Both reach the copy while it is behind its governor's base.
    let mut copy = story.copy.clone();
    let mut wrong = Vec::new();
    for (write, sender, who) in [
        (
            &of_demoted,
            &story.editor,
            "the member the governor made a viewer",
        ),
        (&of_entitled, &story.guest, "a member who may still write"),
    ] {
        if let Err(refusal) = receive(&mut copy, sender, write.clone()) {
            wrong.push(format!(
                "the copy behind its governor's base refuses the write of {who} instead of \
                 holding it: {refusal}"
            ));
        }
    }
    let meanwhile = value(&copy);
    if meanwhile.contains(demoted) || meanwhile.contains(entitled) {
        wrong.push(format!(
            "the copy behind its governor's base TAKES a write before it holds that base: the \
             value reads {meanwhile}"
        ));
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));

    // The withdrawal reaches the governor: the device of the member who
    // withdrew adopts the base and writes its withdrawal back, and the
    // governor takes that change in.
    let owner = story.owner.device_public_key;
    let mut device = story.holder.clone();
    device.write_as_device(story.guest.device_public_key);
    assert_eq!(
        take_workspace_changes(&mut device, owner, whole_workspace(&story.governor)),
        Ok(()),
        "the device of the member who withdrew adopts the base"
    );
    let mut governor = story.governor.clone();
    let back = workspace_past(&device, &governor);
    assert_eq!(
        take_workspace_changes(&mut governor, story.guest.device_public_key, back),
        Ok(()),
        "the governor takes the withdrawal in"
    );

    // The governor's base, which now holds the withdrawal, reaches the copy.
    let taken = take_workspace_changes(&mut copy, owner, whole_workspace(&governor));
    let role =
        |peer: &StatePeer| format!("{:?}", peer.role(&story.editor.device_public_key, now()));
    if taken.is_err() || role(&copy) != role(&governor) {
        wrong.push(format!(
            "the copy does not follow the base that holds the withdrawal: it answered {taken:?} \
             and reads the member's role as {}, its governor as {}",
            role(&copy),
            role(&governor)
        ));
    }
    let after = value(&copy);
    if after.contains(demoted) {
        wrong.push(format!(
            "the write of the member the governor made a viewer is taken in once the base is \
             held: the value reads {after}"
        ));
    }
    if !after.contains(entitled) {
        wrong.push(format!(
            "the write of a member who may still write is lost: the value reads {after}"
        ));
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// A copy that refused no base of its governor holds nothing: what it takes
/// in, it takes in when it comes, and what it refuses, it refuses then. So
/// does a copy that took its governor's base when it came.
#[test]
fn a_copy_that_refused_no_base_holds_nothing_it_takes_in() {
    let story = Behind::made(&Change::Demotes);
    let demoted = "written by the member the governor made a viewer";
    let entitled = "written by a member who may still write";
    let of_demoted = signed_write(&story.before, &story.editor_key, demoted, None);
    let of_entitled = signed_write(&story.before, &story.guest_key, entitled, None);
    let mut wrong = Vec::new();

    // Before any base came, each write is taken in when it comes.
    for (write, sender, text) in [
        (&of_demoted, &story.editor, demoted),
        (&of_entitled, &story.guest, entitled),
    ] {
        let (taken, answer) = takes(&story.before, sender, write, text);
        if !taken {
            wrong.push(format!(
                "a copy no base was handed to does not take a member's write when it comes: \
                 {answer}"
            ));
        }
    }

    // A viewer's copy that never saw the withdrawal takes its governor's
    // base when it comes, and decides each write when it comes.
    let mut follower = story.before.clone();
    follower.write_as_device(story.viewer.device_public_key);
    let base_taken = take_workspace_changes(
        &mut follower,
        story.owner.device_public_key,
        whole_workspace(&story.governor),
    );
    if base_taken.is_err() {
        wrong.push(format!(
            "a copy that never saw the withdrawal refuses its governor's base: {base_taken:?}"
        ));
    }
    let (taken, answer) = takes(&follower, &story.guest, &of_entitled, entitled);
    if !taken {
        wrong.push(format!(
            "a copy that took its governor's base does not take an entitled member's write when \
             it comes: {answer}"
        ));
    }
    if receive(&mut follower.clone(), &story.editor, of_demoted).is_ok() {
        wrong.push(
            "a copy that took its governor's base holds or takes the write of the member the \
             governor made a viewer instead of refusing it"
                .to_owned(),
        );
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// What a program a person has does to a member's delegation today is take
/// one back; none signs a first one. Where none was signed the member's
/// command writes nothing, so a copy comes to hold no withdrawal, and a
/// viewer's copy takes the base its governor makes and follows it.
#[test]
fn no_withdrawal_comes_to_be_where_no_delegation_was_signed() {
    use crate::workspace_membership::WorkspaceMembershipCommand;
    let (owner_key, owner) = owner();
    let mut peer = workspace();
    let (guest_key, guest) = admitted(&mut peer, Role::Editor, [41, 42, 43, 44, 45], "Guest");
    let (_, viewer) = admitted(&mut peer, Role::Viewer, [61, 62, 63, 64, 65], "Viewer");
    let (_, server) = named_server(&mut peer, &owner_key);
    let heads = |copy: &StatePeer| copy.admitted_workspace().unwrap().clone().get_heads();
    let before = heads(&peer);

    let take_back = WorkspaceMembershipCommand::RevokeWorkDelegation {
        server: server.device_public_key,
    };
    let answered = peer.workspace_mut().apply_workspace_membership_command(
        &guest,
        &take_back,
        &guest_key,
        now(),
    );
    assert_eq!(
        answered,
        Ok(false),
        "nothing was delegated, so nothing is taken back"
    );
    assert!(
        peer.workspace().work_delegations().is_empty(),
        "the workspace holds no delegation and no withdrawal"
    );
    assert_eq!(heads(&peer), before, "the command wrote nothing");

    let mut copy = peer.clone();
    copy.write_as_device(viewer.device_public_key);
    let mut governor = peer.clone();
    assert_eq!(governor.rebase_workspace(), Ok(true));
    governor
        .governance_mut()
        .set_member_role(guest.user_public_key, Role::Viewer, &owner_key, now())
        .unwrap();
    assert_eq!(
        take_workspace_changes(
            &mut copy,
            owner.device_public_key,
            whole_workspace(&governor)
        ),
        Ok(()),
        "a copy that holds no withdrawal takes its governor's base"
    );
    let role = |peer: &StatePeer| format!("{:?}", peer.role(&guest.device_public_key, now()));
    assert_eq!(role(&copy), role(&governor), "and follows it");
}
