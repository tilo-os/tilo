//! A workspace this server holds decides its own administration: what a
//! person's device may do there is what the workspace's signed member list and
//! grants let that person, the server's own person included.

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, roles, roles::governance};
use tilo_peer_state::join::Role;
use tilo_server_protocol::operator::WaitListStatus;
use tilo_server_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use crate::workspaces::PersonNames as _;
use crate::workspaces::tests::{governor, guest_device, signed_pre_admission, test_host};
use crate::workspaces::{
    IssueInvitationRequest, PreAdmitRequest, RemoveMemberRequest, SetMemberRoleRequest,
    WaitListMail, WaitListSecrets, WaitListed, Workspaces,
};

/// A new person admitted to `workspace` as an Editor, through the owner's
/// pre-admission: their device's certificate.
async fn admit(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> DeviceCert {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (_, cert) = guest_device();
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            cert.clone(),
            rand::random(),
            &format!("Person {seed}"),
            Some(token.as_slice()),
        )
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    cert
}

async fn grant(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    person: &DeviceCert,
    action: &str,
) {
    let who = hex::encode(person.user_public_key);
    let grant = Grant::new(
        format!("test:{action}:{who}"),
        Grantee::person(who),
        roles::governance_action(workspace, action),
        KindSet::ALL,
        Answer::Allow,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_workspace_grants(workspace, owner, request_id, vec![grant])
        .await
        .unwrap();
}

async fn set_role(
    workspaces: &Workspaces,
    workspace: &str,
    by: &DeviceCert,
    person: &DeviceCert,
    role: Role,
) {
    workspaces
        .set_member_role(workspace, by, role_request(person, role, rand::random()))
        .await
        .unwrap();
}

fn role_request(person: &DeviceCert, role: Role, request_id: [u8; 32]) -> SetMemberRoleRequest {
    SetMemberRoleRequest {
        request_id,
        user_public_key: person.user_public_key,
        role,
    }
}

fn invitation(request_id: u8, role: Role) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [request_id; 32],
        role,
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
    }
}

#[tokio::test]
async fn page_role_assignments_change_the_documents_key_holders() {
    use tilo_action_policy::Change;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let reviewer = admit(workspaces, &workspace, &owner, 1).await;
    let person = hex::encode(reviewer.user_public_key);
    let change = |request_id, change| WorkspacesRequest::EditRoles {
        workspace: workspace.clone(),
        request_id: [request_id; 32],
        change,
    };
    let define = change(
        2,
        Change::DefineRole {
            id: "reviewers".into(),
            name: "Reviewers".into(),
        },
    );
    assert!(matches!(
        workspaces.page_workspaces(&reviewer, define.clone()).await,
        WorkspacesReply::Refused(_)
    ));
    let WorkspacesReply::Roles(updated) = workspaces.page_workspaces(&owner, define).await else {
        panic!("the owner can create a role from the page");
    };
    assert_eq!(
        updated.custom_roles.get("reviewers").map(String::as_str),
        Some("Reviewers")
    );

    let app = "so.tilo.notes";
    let document = "review";
    let access = |request_id, recipient: &str, access: &str| WorkspacesRequest::SetDocumentAccess {
        workspace: workspace.clone(),
        request_id: [request_id; 32],
        app: app.into(),
        document: document.into(),
        recipient: recipient.into(),
        access: access.into(),
    };
    let private = access(3, "everyone", "no-access");
    assert!(matches!(
        workspaces.page_workspaces(&reviewer, private.clone()).await,
        WorkspacesReply::Refused(_)
    ));
    for request in [private, access(4, "role:reviewers", "view")] {
        let reply = workspaces.page_workspaces(&owner, request).await;
        assert!(matches!(reply, WorkspacesReply::Workspace(_)), "{reply:?}");
    }
    check_default_access_rules(workspaces, &workspace, &owner).await;

    let holds_keys = || async {
        let state = workspaces.state(&workspace).await.unwrap();
        let chunk = neighbour_wire::document_chunk(document, 1);
        [document, chunk.as_str(), "other-note"].map(|part| {
            state
                .document_key_holders(app, part, tilo_clock::wall())
                .unwrap()
                .contains(&reviewer.device_public_key)
        })
    };
    assert_eq!(holds_keys().await, [false, false, false]);

    for (request_id, assigned) in [(5, true), (6, false)] {
        let request = change(
            request_id,
            Change::SetMemberCustomRole {
                person: person.clone(),
                role: "reviewers".into(),
                assigned,
            },
        );
        let WorkspacesReply::Roles(updated) = workspaces.page_workspaces(&owner, request).await
        else {
            panic!("the owner can change a member's assignments from the page");
        };
        assert_eq!(
            updated
                .assigned_roles
                .get(&person)
                .is_some_and(|roles| roles.contains("reviewers")),
            assigned
        );
        assert_eq!(holds_keys().await, [assigned, assigned, assigned]);
        if assigned {
            let recipient = format!("member:{person}");
            for (id, level, expected) in [(7, "no-access", false), (8, "inherit", true)] {
                let reply = workspaces
                    .page_workspaces(&owner, access(id, &recipient, level))
                    .await;
                assert!(matches!(reply, WorkspacesReply::Workspace(_)), "{reply:?}");
                assert_eq!(holds_keys().await, [expected, expected, true]);
            }
        }
    }
}

async fn check_default_access_rules(workspaces: &Workspaces, workspace: &str, owner: &DeviceCert) {
    for (id, recipient, app, preset) in [
        (9, "everyone", "", "no-access"),
        (10, "role:reviewers", "so.tilo.notes", "view"),
    ] {
        let request = WorkspacesRequest::SetDocumentAccess {
            workspace: workspace.to_owned(),
            request_id: [id; 32],
            app: String::new(),
            document: app.into(),
            recipient: recipient.into(),
            access: preset.into(),
        };
        let reply = workspaces.page_workspaces(owner, request).await;
        assert!(matches!(reply, WorkspacesReply::Workspace(_)), "{reply:?}");
    }
    let WorkspacesReply::DocumentAccess(settings) = workspaces
        .page_workspaces(
            owner,
            WorkspacesRequest::DocumentAccess {
                workspace: workspace.to_owned(),
            },
        )
        .await
    else {
        panic!("the owner can read their saved workspace and app rules");
    };
    for (app, preset) in [("", "no-access"), ("so.tilo.notes", "view")] {
        assert!(
            settings.rules.iter().any(|rule| {
                rule.app.is_empty() && rule.document == app && rule.access == preset
            })
        );
    }
}

/// A page deletes a workspace of its own with nobody else in it: it goes from the server and
/// every list of it, and one with people in it, or another person's, is not theirs to delete.
#[tokio::test]
async fn a_page_deletes_a_workspace_of_its_own_with_nobody_else_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let delete = || WorkspacesRequest::Delete {
        workspace: workspace.clone(),
    };
    // A member who is not the workspace's cannot end it for everyone.
    let refused = workspaces.page_workspaces(&ada, delete()).await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "a member deleted the workspace: {refused:?}"
    );
    // Nor can its owner while someone else is in it.
    let refused = workspaces.page_workspaces(&owner, delete()).await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "the owner deleted a workspace with someone else in it: {refused:?}"
    );
    // A member leaves by taking themselves out, and the workspace stays for its owner.
    workspaces
        .remove_member(
            &workspace,
            &ada,
            crate::workspaces::RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: ada.user_public_key,
            },
        )
        .await
        .expect("a member leaves the workspace");
    let after = workspaces.summary(&workspace).await.unwrap();
    assert!(
        !after
            .members
            .iter()
            .any(|member| member.root() == Some(ada.user_public_key) && !member.removed),
        "the member is no longer in it: {after:?}"
    );
    assert!(
        matches!(
            workspaces.page_workspaces(&owner, delete()).await,
            WorkspacesReply::Deleted
        ),
        "the owner deletes their workspace with nobody else in it"
    );
    assert!(
        workspaces.summary(&workspace).await.is_err(),
        "a deleted workspace is not governed here"
    );
    let WorkspacesReply::Workspaces(listed) = workspaces
        .page_workspaces(&owner, WorkspacesRequest::List)
        .await
    else {
        panic!("the owner lists their workspaces");
    };
    assert!(
        !listed.iter().any(|held| held.workspace == workspace),
        "a deleted workspace is on no list: {listed:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_address_left_at_the_entry_is_on_the_wait_list_and_never_a_person_to_admit() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    assert_eq!(
        workspaces
            .wait(
                &workspace,
                " Bo@Example.org ",
                listed_at_once(0x0b),
                "en",
                10
            )
            .await
            .unwrap(),
        WaitListed::Added { mail: false }
    );
    let ask = |request| workspaces.page_workspaces(&ada, request);
    let list = || WorkspacesRequest::WaitList {
        workspace: workspace.clone(),
    };

    // Someone the workspace does not let invite reads no wait list.
    assert!(matches!(ask(list()).await, WorkspacesReply::Refused(_)));
    grant(workspaces, &workspace, &owner, &ada, governance::ADD_MEMBER).await;
    let WorkspacesReply::WaitList(listed) = ask(list()).await else {
        panic!("someone who may invite reads the wait list");
    };
    let [entry] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    // As typed, but for the spaces around it.
    assert_eq!(entry.contact, "Bo@Example.org");
    let request_id = entry.request_id;

    // Taken off the list once, then there is nothing to take off.
    let unlist = || WorkspacesRequest::Unlist {
        workspace: workspace.clone(),
        request_id,
    };
    assert!(matches!(
        ask(unlist()).await,
        WorkspacesReply::WaitList(left) if left.is_empty()
    ));
    assert!(matches!(ask(unlist()).await, WorkspacesReply::Refused(_)));
    server.shutdown().await;
}

/// Bo left an address where the entry mails a confirmation link, and Cy one
/// where it sends none; Bo is listed once confirmed. Bo's request id.
async fn bo_confirmed_beside_cy(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
) -> [u8; 32] {
    let mailed = WaitListSecrets {
        removal: [0x0b; 32],
        confirm: Some([0x1b; 32]),
    };
    assert_eq!(
        workspaces
            .wait(workspace, "bo@example.org", mailed, "de-DE", 10)
            .await,
        Ok(WaitListed::Added { mail: true })
    );
    workspaces
        .wait(workspace, "cy@example.org", listed_at_once(0x0c), "en", 10)
        .await
        .unwrap();
    let listed = || read_wait_list(workspaces, owner, workspace);
    let contacts = || async {
        listed()
            .await
            .into_iter()
            .map(|entry| entry.contact)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        contacts().await,
        ["cy@example.org"],
        "unconfirmed is hidden"
    );
    assert_eq!(
        workspaces.wait_list_state(workspace, [0x0b; 32]).await,
        Ok(WaitListStatus::Pending)
    );
    assert_eq!(
        workspaces.confirm_wait_list(workspace, [0xee; 32]).await,
        Ok(false)
    );
    assert_eq!(
        workspaces.confirm_wait_list(workspace, [0x1b; 32]).await,
        Ok(true)
    );
    assert_eq!(contacts().await, ["bo@example.org", "cy@example.org"]);
    listed().await[0].request_id
}

/// An invitation's mail names the person who asked, as the workspace names them: nobody while
/// nobody named them, and the name they gave after.
#[tokio::test]
async fn a_mailed_invitation_names_who_asked_as_the_workspace_names_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let bo = bo_confirmed_beside_cy(workspaces, &owner, &workspace).await;
    let mut mail = workspaces.mail_wait_list("https://entry.example/").unwrap();
    let ask = |request| workspaces.page_workspaces(&owner, request);

    let WorkspacesReply::Invitation { .. } = ask(list_entry(&workspace, bo)).await else {
        panic!("the owner invites the address");
    };
    assert_eq!(
        mailed_inviter(workspaces, &owner, &workspace, bo, &mut mail).await,
        "",
        "nobody named the owner yet"
    );

    // They give their name: the next invitation is mailed under it.
    assert!(matches!(
        ask(WorkspacesRequest::Name {
            workspace: workspace.clone(),
            name: "Maya Chen".to_owned(),
        })
        .await,
        WorkspacesReply::Named
    ));
    let cy = read_wait_list(workspaces, &owner, &workspace)
        .await
        .into_iter()
        .find(|entry| entry.contact == "cy@example.org")
        .expect("cy is listed")
        .request_id;
    let WorkspacesReply::Invitation { .. } = ask(list_entry(&workspace, cy)).await else {
        panic!("the owner invites cy");
    };
    assert_eq!(
        mailed_inviter(workspaces, &owner, &workspace, cy, &mut mail).await,
        "Maya Chen"
    );
    server.shutdown().await;
}

/// Mail the invitation for `entry` and answer how the person who asked named themself in it.
async fn mailed_inviter(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
    entry: [u8; 32],
    mail: &mut tokio::sync::mpsc::UnboundedReceiver<WaitListMail>,
) -> String {
    let (answer, inviter) = tokio::join!(
        workspaces.page_workspaces(
            owner,
            mailed_invitation(workspace, entry, "https://entry.example/?invite=AAEC")
        ),
        async {
            let asked = mail.recv().await.unwrap();
            asked.queued.send(true).unwrap();
            asked.inviter
        }
    );
    assert!(matches!(answer, WorkspacesReply::WaitList(_)));
    inviter
}

#[tokio::test]
async fn a_mailed_address_is_listed_once_confirmed_mailed_its_invitation_and_leaves_on_joining() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let bo = bo_confirmed_beside_cy(workspaces, &owner, &workspace).await;
    let invitation = the_address_is_invited_and_mailed(workspaces, &owner, &workspace, bo).await;
    joining_takes_it_off_the_list(workspaces, &owner, &workspace, invitation).await;
    server.shutdown().await;
}

/// An invitation sent to an address left at the entry says so to whoever
/// opens it, so their page opens a workspace of their own first; one the
/// owner made for anyone does not.
#[tokio::test]
async fn an_invitation_sent_off_the_wait_list_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let bo = bo_confirmed_beside_cy(workspaces, &owner, &workspace).await;
    let issued = |request| async {
        let WorkspacesReply::Invitation {
            signed_invitation, ..
        } = workspaces.page_workspaces(&owner, request).await
        else {
            panic!("the owner invites");
        };
        let JoinReply::Invitation { wait_list, .. } =
            workspaces.inspect_invitation(&signed_invitation).await
        else {
            panic!("the server honours its own invitation");
        };
        wait_list
    };
    assert!(
        issued(list_entry(&workspace, bo)).await,
        "sent off the wait list"
    );
    let anyone = WorkspacesRequest::Invite {
        workspace: workspace.clone(),
        request_id: [42; 32],
        role: Role::Editor,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
        wait_list_entry: None,
        recipient: None,
        administrator: false,
    };
    assert!(!issued(anyone).await, "made for anyone");
    server.shutdown().await;
}

/// The entry `workspace` holds for the address `entry` names.
fn list_entry(workspace: &str, entry: [u8; 32]) -> WorkspacesRequest {
    WorkspacesRequest::Invite {
        workspace: workspace.to_owned(),
        // One request per address, so inviting the same one again is the same request.
        request_id: *blake3::hash(&entry).as_bytes(),
        role: Role::Editor,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
        wait_list_entry: Some(entry),
        recipient: None,
        administrator: false,
    }
}

/// The address `entry` names, mailed the invitation `link` carries.
fn mailed_invitation(workspace: &str, entry: [u8; 32], link: &str) -> WorkspacesRequest {
    WorkspacesRequest::MailInvite {
        workspace: workspace.to_owned(),
        wait_list_entry: entry,
        link: link.to_owned(),
    }
}

/// The owner invites the mailed address, which lists it with the day it was
/// invited, mails it once on the operator's own entry and is asked in. Returns
/// the invitation the person joins with.
async fn the_address_is_invited_and_mailed(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
    entry: [u8; 32],
) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = |secret: &[u8; 32]| -> [u8; 32] { sha2::Sha256::digest(secret).into() };
    let ask = |request| workspaces.page_workspaces(owner, request);
    let listed = || read_wait_list(workspaces, owner, workspace);

    // Invited: the entry says when, and is still on the list until someone
    // joins with the invitation.
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = ask(list_entry(workspace, entry)).await
    else {
        panic!("the owner invites the address");
    };
    let now = listed().await;
    assert!(
        now[0]
            .invited_at_ns
            .is_some_and(|at| at >= now[0].added_at_ns)
            && now[1].invited_at_ns.is_none()
    );
    assert_eq!(
        workspaces.wait_list_state(workspace, [0x0b; 32]).await,
        Ok(WaitListStatus::Invited)
    );

    // A server whose operator sends no mail mails nothing.
    assert!(matches!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://entry.example/?invite=AAEC"
        ))
        .await,
        WorkspacesReply::Refused(reason) if reason.contains("no mail")
    ));
    let mut mail = workspaces.mail_wait_list("https://entry.example/").unwrap();
    // Only an invitation on the operator's entry is mailed.
    assert!(matches!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://elsewhere.example/?invite=AAEC"
        ))
        .await,
        WorkspacesReply::Refused(_)
    ));
    let (answer, (to, link, removal)) = tokio::join!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://entry.example/?invite=AAEC"
        )),
        async {
            let WaitListMail {
                queued,
                to,
                link,
                inviter,
                removal,
            } = mail.recv().await.unwrap();
            assert!(inviter.is_empty(), "nobody named the owner yet");
            queued.send(true).unwrap();
            (to, link, removal)
        }
    );
    assert!(matches!(answer, WorkspacesReply::WaitList(_)));
    assert_eq!(
        (to.contact.as_str(), to.language.as_str()),
        ("bo@example.org", "de")
    );
    assert_eq!(link, "https://entry.example/?invite=AAEC");
    assert_eq!(
        workspaces
            .wait_list_state(workspace, digest(&removal))
            .await,
        Ok(WaitListStatus::Invited),
        "the mail's removal link names the entry"
    );
    // Not twice within minutes.
    assert!(matches!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://entry.example/?invite=AAEC"
        ))
        .await,
        WorkspacesReply::Refused(_)
    ));
    assert!(mail.try_recv().is_err());
    signed_invitation
}

/// Someone joins with the mailed invitation — asks and proves — which admits
/// them by the invitation's own grant and takes the address off the list.
async fn joining_takes_it_off_the_list(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
    invitation: Vec<u8>,
) {
    let listed = || read_wait_list(workspaces, owner, workspace);
    assert!(
        matches!(
            ask_to_join(workspaces, invitation).await,
            JoinReply::Admitted { .. }
        ),
        "the mailed invitation admits the person who holds it"
    );
    assert_eq!(
        listed()
            .await
            .iter()
            .map(|entry| entry.contact.clone())
            .collect::<Vec<_>>(),
        ["cy@example.org"]
    );
    assert_eq!(
        workspaces.wait_list_state(workspace, [0x0b; 32]).await,
        Ok(WaitListStatus::Gone)
    );
}

#[tokio::test]
async fn an_address_mailed_its_invitation_takes_itself_off_with_the_mail_s_removal_link() {
    use sha2::Digest as _;
    let digest = |secret: &[u8; 32]| -> [u8; 32] { sha2::Sha256::digest(secret).into() };
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut mail = workspaces.mail_wait_list("https://entry.example/").unwrap();
    assert!(
        workspaces
            .mail_wait_list("https://entry.example/")
            .is_none(),
        "one operator hears the mail"
    );
    workspaces
        .wait(&workspace, "cy@example.org", listed_at_once(0x0c), "en", 10)
        .await
        .unwrap();
    let ask = |request| workspaces.page_workspaces(&owner, request);
    let listed = || read_wait_list(workspaces, &owner, &workspace);
    let cy = listed().await[0].request_id;
    let mail_invite = || WorkspacesRequest::MailInvite {
        workspace: workspace.clone(),
        wait_list_entry: cy,
        link: "https://entry.example/?invite=AAEC".into(),
    };
    let invite = |wait_list_entry| WorkspacesRequest::Invite {
        workspace: workspace.clone(),
        request_id: rand::random(),
        role: Role::Editor,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
        wait_list_entry: Some(wait_list_entry),
        recipient: None,
        administrator: false,
    };
    // Not invited yet: nothing to mail. An invitation for an entry not on
    // the list is refused.
    assert!(matches!(
        ask(mail_invite()).await,
        WorkspacesReply::Refused(_)
    ));
    assert!(matches!(
        ask(invite([0xee; 32])).await,
        WorkspacesReply::Refused(_)
    ));

    // Invited by mail, Cy takes the address off with the mail's removal
    // link: the workspace says where to tell them it did. A wrong secret is
    // answered the same, and takes nothing off.
    assert!(matches!(
        ask(invite(cy)).await,
        WorkspacesReply::Invitation { .. }
    ));
    // A queue rejection is reported to the inviter and releases the
    // reservation, rather than claiming that a channel send mailed it.
    let (answer, ()) = tokio::join!(ask(mail_invite()), async {
        mail.recv().await.unwrap().queued.send(false).unwrap();
    });
    assert!(matches!(answer, WorkspacesReply::Refused(_)));
    let (answer, removal) = tokio::join!(ask(mail_invite()), async {
        let WaitListMail {
            queued, removal, ..
        } = mail.recv().await.unwrap();
        queued.send(true).unwrap();
        removal
    });
    assert!(matches!(answer, WorkspacesReply::WaitList(_)));
    assert_eq!(
        workspaces.leave_wait_list(&workspace, [0x99; 32]).await,
        Ok(None)
    );
    assert_eq!(listed().await.len(), 1);
    let told = workspaces
        .leave_wait_list(&workspace, digest(&removal))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(told.contact, "cy@example.org");
    assert!(listed().await.is_empty());
    assert_eq!(
        workspaces.leave_wait_list(&workspace, [0x0c; 32]).await,
        Ok(None)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_lost_invitation_acknowledgement_keeps_the_reservation() {
    Box::pin(uncertain_invitation_mail(false)).await;
}

#[tokio::test]
async fn a_timed_out_invitation_acknowledgement_keeps_the_reservation() {
    Box::pin(uncertain_invitation_mail(true)).await;
}

async fn uncertain_invitation_mail(timeout: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let shutdown = {
        let server = test_host(dir.path(), &key, &owner).await;
        assert_uncertain_invitation_mail(server.workspaces(), &owner, timeout).await;
        server.shutdown()
    };
    shutdown.await;
}

async fn assert_uncertain_invitation_mail(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    timeout: bool,
) {
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut mail = workspaces.mail_wait_list("https://entry.example/").unwrap();
    workspaces
        .wait(&workspace, "cy@example.org", listed_at_once(0x0c), "en", 10)
        .await
        .unwrap();
    let cy = read_wait_list(workspaces, owner, &workspace).await[0].request_id;
    assert!(matches!(
        workspaces
            .page_workspaces(
                owner,
                WorkspacesRequest::Invite {
                    workspace: workspace.clone(),
                    request_id: rand::random(),
                    role: Role::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                    inviter_name: None,
                    wait_list_entry: Some(cy),
                    recipient: None,
                    administrator: false,
                }
            )
            .await,
        WorkspacesReply::Invitation { .. }
    ));
    let request = || WorkspacesRequest::MailInvite {
        workspace: workspace.clone(),
        wait_list_entry: cy,
        link: "https://entry.example/?invite=AAEC".into(),
    };
    if timeout {
        tokio::time::pause();
    }
    let (answer, held) = tokio::join!(workspaces.page_workspaces(owner, request()), async {
        let queued = mail.recv().await.unwrap().queued;
        if timeout {
            Some(queued)
        } else {
            drop(queued);
            None
        }
    });
    if timeout {
        tokio::time::resume();
    }
    let WorkspacesReply::Refused(reason) = answer else {
        panic!("missing acknowledgement must report uncertainty");
    };
    assert!(reason.contains("uncertain"));
    assert!(reason.contains("may already be queued"));
    let WorkspacesReply::Refused(reason) = workspaces.page_workspaces(owner, request()).await
    else {
        panic!("uncertain acceptance must retain the throttle");
    };
    assert!(reason.contains("minutes ago"));
    assert!(matches!(
        mail.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    drop(held);
}

#[tokio::test]
async fn pending_removal_returns_only_a_private_stable_contact_digest() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let shutdown = {
        let server = test_host(dir.path(), &key, &owner).await;
        assert_pending_removal_contact_digest(server.workspaces()).await;
        server.shutdown()
    };
    shutdown.await;
}

async fn assert_pending_removal_contact_digest(workspaces: &Workspaces) {
    use sha2::Digest as _;
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    for removal in [[1; 32], [2; 32]] {
        workspaces
            .wait(
                &workspace,
                "ÄDA@example.org",
                WaitListSecrets {
                    removal,
                    confirm: Some([3; 32]),
                },
                "de",
                10,
            )
            .await
            .unwrap();
    }
    let expected: [u8; 32] = sha2::Sha256::digest(tilo_server_protocol::workspaces::contact_key(
        "äda@EXAMPLE.org",
    ))
    .into();
    assert_eq!(
        workspaces
            .leave_wait_list_for_operator(&workspace, [2; 32])
            .await
            .unwrap(),
        (None, Some(expected))
    );
    assert_eq!(
        workspaces
            .leave_wait_list_for_operator(&workspace, [1; 32])
            .await
            .unwrap(),
        (None, None)
    );
    assert_eq!(
        workspaces
            .wait_list_state(&workspace, [1; 32])
            .await
            .unwrap(),
        tilo_server_protocol::operator::WaitListStatus::Gone
    );
}

/// `workspace`'s wait list as `owner` reads it.
async fn read_wait_list(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
) -> Vec<tilo_server_protocol::workspaces::WaitListEntry> {
    let WorkspacesReply::WaitList(listed) = workspaces
        .page_workspaces(
            owner,
            WorkspacesRequest::WaitList {
                workspace: workspace.to_owned(),
            },
        )
        .await
    else {
        panic!("the owner reads the wait list");
    };
    listed
}

/// A new device asks to join with `invitation`, its key proved on the connection its entry
/// rides.
async fn ask_to_join(workspaces: &Workspaces, invitation: Vec<u8>) -> JoinReply {
    let (_, guest) = guest_device();
    workspaces
        .admit_proven(invitation, guest, rand::random(), "Bo", None)
        .await
}

/// Secrets for an entry that sends no mail, which lists an address at once.
const fn listed_at_once(removal: u8) -> WaitListSecrets {
    WaitListSecrets {
        removal: [removal; 32],
        confirm: None,
    }
}

/// An invitation says who it is from: one asked without a name (from an API
/// key or the command line) carries the name the workspace shows for the
/// person who asked, or no name where the workspace has none to show; one
/// that names its inviter keeps that name.
#[tokio::test]
async fn an_invitation_asked_without_a_name_names_the_inviter_as_the_workspace_shows_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    grant(workspaces, &workspace, &owner, &ada, governance::ADD_MEMBER).await;
    let inviter = |issued: crate::workspaces::IssuedInvitation| {
        tilo_peer_state::join::decode_and_verify_invitation(
            &issued.signed_invitation,
            None,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap()
        .inviter()
        .cloned()
    };

    let unnamed = workspaces
        .issue(&workspace, &ada, invitation(20, Role::Viewer))
        .await
        .unwrap();
    let from = inviter(unnamed).expect("the invitation names who it is from");
    assert_eq!(from.name, "Person 1");
    assert_eq!(from.user_public_key, ada.user_public_key);

    let named = workspaces
        .issue(
            &workspace,
            &ada,
            IssueInvitationRequest {
                inviter_name: Some("Ada".into()),
                ..invitation(21, Role::Viewer)
            },
        )
        .await
        .unwrap();
    assert_eq!(inviter(named).unwrap().name, "Ada");
    server.shutdown().await;
}

/// A device of `person`'s root, labelled `label`: one they hold themselves,
/// or a key made to act for them.
fn device_of(person: &SigningKey, label: &str, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: SigningKey::from_bytes(&rand::random())
            .verifying_key()
            .to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, person).unwrap();
    cert
}

/// Someone a deployment's operator declares an owner of a workspace nobody is
/// in yet, with the name they go by: the workspace lists them by it before a
/// device of theirs has arrived, a key of theirs renames them without one, and
/// the invitation their first device makes says who asked them in.
#[tokio::test]
async fn a_declared_owner_carries_the_name_they_were_declared_with() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;

    // All the operator has of them is their root and the name they go by: no
    // device of theirs has ever reached this workspace.
    let person = SigningKey::from_bytes(&rand::random());
    let root = person.verifying_key().to_bytes();
    workspaces
        .set_member_administrator(&workspace, &owner, rand::random(), root, Some("Quinten"))
        .await
        .unwrap();
    let named = |members: &[tilo_peer_state::Member]| {
        members
            .iter()
            .find(|member| member.root() == Some(root) && !member.removed)
            .unwrap_or_else(|| panic!("the declared owner is a member: {members:?}"))
            .name
            .clone()
    };
    let summary = workspaces.summary(&workspace).await.unwrap();
    assert_eq!(named(&summary.members), "Quinten");
    assert!(
        summary
            .devices
            .get(&tilo_peer_state::MemberId::person(hex::encode(root)))
            .is_none_or(Vec::is_empty),
        "no device of theirs has arrived yet: {:?}",
        summary.devices
    );

    // A key of theirs names them without one, as their Tilo on the web would,
    // and the name it sets is what the workspace shows from then on.
    let theirs = device_of(
        &person,
        "a key of Quinten's",
        DeviceCapabilities::server_key_grant(),
    );
    workspaces
        .name_self(
            &workspace,
            &theirs,
            "Quinten Wolff",
            tilo_clock::unix_now_ns(),
        )
        .await
        .unwrap();
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert_eq!(named(&members), "Quinten Wolff");

    // Their first device joins, admitted through the owner's pre-admission
    // under another name: the name they chose is the one the workspace keeps.
    let token = [7u8; 8];
    let issued = workspaces
        .pre_admit(
            &owner,
            PreAdmitRequest {
                request_id: [7; 32],
                grant: signed_pre_admission(&workspace, &token),
                recipient_user_public_key: Some(root),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let device = device_of(&person, "Quinten's laptop", DeviceCapabilities::member());
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            device.clone(),
            rand::random(),
            "Quin's laptop",
            Some(token.as_slice()),
        )
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert_eq!(named(&members), "Quinten Wolff");

    // The invitation their device makes says who asked them in.
    let issued = workspaces
        .issue(&workspace, &device, invitation(30, Role::Viewer))
        .await
        .unwrap();
    let from = tilo_peer_state::join::decode_and_verify_invitation(
        &issued.signed_invitation,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap()
    .inviter()
    .cloned()
    .expect("the invitation names who it is from");
    assert_eq!(from.name, "Quinten Wolff");
    assert_eq!(from.user_public_key, root);
    server.shutdown().await;
}

#[tokio::test]
async fn an_administrator_governs_within_their_role_without_further_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    set_role(workspaces, &workspace, &owner, &cy, Role::Viewer).await;

    workspaces
        .set_member_administrator(&workspace, &owner, [50; 32], ada.user_public_key, None)
        .await
        .unwrap();
    an_administrator_rules_every_member_but_administrators(workspaces, &workspace, &ada, &bo, &cy)
        .await;
    unmaking_the_administrator_takes_it_all_back(workspaces, &workspace, &owner, &ada, &bo).await;
    server.shutdown().await;
}

/// They set any role and remove any member who is not an administrator, a
/// fellow Editor included.
async fn an_administrator_rules_every_member_but_administrators(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
    bo: &DeviceCert,
    cy: &DeviceCert,
) {
    set_role(workspaces, workspace, ada, cy, Role::Commenter).await;
    set_role(workspaces, workspace, ada, bo, Role::Moderator).await;
    set_role(workspaces, workspace, ada, bo, Role::Editor).await;
    workspaces
        .remove_member(
            workspace,
            ada,
            RemoveMemberRequest {
                request_id: [55; 32],
                user_public_key: cy.user_public_key,
            },
        )
        .await
        .unwrap();
}

/// An Editor by role alone still governs nothing, and the owner unmaking the
/// administrator takes it all back.
async fn unmaking_the_administrator_takes_it_all_back(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    ada: &DeviceCert,
    bo: &DeviceCert,
) {
    assert!(
        workspaces
            .issue(workspace, bo, invitation(56, Role::Viewer))
            .await
            .is_err()
    );
    set_role(workspaces, workspace, owner, ada, Role::Editor).await;
    assert!(
        workspaces
            .issue(workspace, ada, invitation(57, Role::Viewer))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_next_governance_change_rewrites_an_administrators_old_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let who = hex::encode(ada.user_public_key);
    // An administrator's grant as workspaces held it before: on the workspace's
    // resource, where the Editor's own Deny outranked it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&who).as_str(),
        Grantee::person(who),
        roles::workspace_resource(&workspace),
        KindSet::ALL,
        Answer::Allow,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_workspace_grants(&workspace, &owner, [60; 32], vec![legacy])
        .await
        .unwrap();
    workspaces
        .issue(&workspace, &ada, invitation(61, Role::Editor))
        .await
        .unwrap();
    server.shutdown().await;
}

/// A member holding every governance action the workspace has does through this server what the
/// workspace lets her, and nothing it does not: making a workspace on the server stays its
/// person's, making administrators the owner's. A device of a member enters as theirs, and
/// anyone else's by no entry. A workspace this server does not hold lets no one in, the server's
/// own person included.
#[tokio::test]
async fn what_a_workspace_lets_a_member_this_server_carries_out() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    for action in governance::ALL {
        grant(workspaces, &workspace, &owner, &ada, action).await;
    }

    // A device of a member enters as theirs: the owner's and Ada's are in already. Someone the
    // workspace does not have comes in by an invitation, never this way.
    assert!(!workspaces.enter(&workspace, &owner).await.unwrap());
    assert!(!workspaces.enter(&workspace, &ada).await.unwrap());
    let (_, stranger) = guest_device();
    assert!(workspaces.enter(&workspace, &stranger).await.is_err());
    // A Viewer the owner added by their root, before any device of theirs arrived, enters from
    // one and holds no more there than a Viewer does.
    let cy = SigningKey::from_bytes(&rand::random());
    workspaces
        .add_member(
            &workspace,
            &owner,
            crate::workspaces::AddMemberRequest {
                request_id: rand::random(),
                user_public_key: cy.verifying_key().to_bytes(),
                place: crate::workspaces::MemberPlace::Role(Role::Viewer),
                name: Some("Cy".into()),
            },
        )
        .await
        .unwrap();
    let phone = device_of(&cy, "Cy's phone", DeviceCapabilities::member());
    assert!(workspaces.enter(&workspace, &phone).await.unwrap());
    assert!(
        workspaces
            .issue(&workspace, &phone, invitation(34, Role::Viewer))
            .await
            .is_err()
    );

    // What the workspace lets her, she does.
    workspaces
        .issue(&workspace, &ada, invitation(30, Role::Viewer))
        .await
        .unwrap();
    workspaces
        .pre_admit(
            &ada,
            PreAdmitRequest {
                request_id: [33; 32],
                grant: signed_pre_admission(&workspace, b"token"),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    // Her grants are the workspace's to change, so she may take them all back too.
    workspaces
        .put_workspace_grants(&workspace, &ada, [32; 32], Vec::new())
        .await
        .unwrap();

    // What it does not, she does not.
    let reply = workspaces
        .page_workspaces(
            &ada,
            WorkspacesRequest::Create {
                name: "Another".into(),
            },
        )
        .await;
    assert!(matches!(reply, WorkspacesReply::Refused(_)), "{reply:?}");
    assert!(
        workspaces
            .set_member_administrator(&workspace, &ada, [31; 32], bo.user_public_key, None)
            .await
            .is_err()
    );
    assert_eq!(workspaces.list().await.len(), 1);

    // A workspace the server does not hold decides nothing, so no one enters it.
    let elsewhere = tilo_server_protocol::WorkspaceId::new().to_string();
    for device in [&owner, &ada] {
        assert!(workspaces.enter(&elsewhere, device).await.is_err());
    }
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_certificate_administers_nothing_even_the_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut expired = owner.clone();
    expired.expires_at_ns = Some(2);
    sign_device_cert(
        &mut expired,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    assert!(
        workspaces
            .issue(&workspace, &expired, invitation(40, Role::Viewer))
            .await
            .is_err()
    );
    assert!(workspaces.pending(&workspace, &expired).await.is_err());
    assert!(workspaces.enter(&workspace, &expired).await.is_err());
    workspaces.pending(&workspace, &owner).await.unwrap();
    server.shutdown().await;
}

/// A server acts in a workspace only through its own person's row on the member list: what a
/// member may do, it carries out for them only as far as that row lets it. A server whose
/// person the list does not have carries out nothing, a governance action, an admission or a
/// change of place alike, whatever the member asking may do.
#[tokio::test]
async fn a_server_without_a_place_on_the_member_list_carries_out_nothing() {
    use super::{WorkspaceOp, decide_by_governance};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    for action in governance::ALL {
        grant(workspaces, &workspace, &owner, &ada, action).await;
    }
    let held = workspaces.workspace(&workspace).await.unwrap();
    let held = held.lock().await;
    let now = tilo_clock::unix_now_ns();
    let change = tilo_action_policy::Change::SetRole {
        person: hex::encode(bo.user_public_key),
        role: Role::Viewer,
    };
    let asks = [
        WorkspaceOp::Govern(governance::REMOVE_MEMBER),
        WorkspaceOp::Admit(Role::Viewer),
        WorkspaceOp::Change(&change),
    ];
    let stranger = SigningKey::from_bytes(&[0x77; 32])
        .verifying_key()
        .to_bytes();
    for op in asks {
        // Ada may, and this server's person, the workspace's owner, may carry it out for her.
        decide_by_governance(&held, &ada, op, now, Some(owner.user_public_key))
            .unwrap_or_else(|error| panic!("{op:?} for Ada through the owner's server: {error}"));
        // A server whose person has no place in the workspace may not, though Ada may.
        assert!(
            decide_by_governance(&held, &ada, op, now, Some(stranger)).is_err(),
            "{op:?} for Ada through a server with no place"
        );
        // Nor one whose person is a plain member: Bo is an Editor, and holds none of it.
        assert!(
            decide_by_governance(&held, &ada, op, now, Some(bo.user_public_key)).is_err(),
            "{op:?} for Ada through a server of an Editor's"
        );
    }
    drop(held);
    server.shutdown().await;
}

/// A workspace its host hands to a person is administered by them: they make
/// administrators through the host, which serves the workspace as one of
/// them, and an administrator makes more. Taking an administrator's grant back
/// is the owner's alone and signed on a device of theirs: the host refuses to
/// carry it out for them in words that say so, and nobody else may.
#[tokio::test]
async fn a_workspace_handed_to_a_person_is_administered_by_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, host) = governor();
    let server = test_host(dir.path(), &key, &host).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &host, 1).await;
    let bo = admit(workspaces, &workspace, &host, 2).await;
    let cy = admit(workspaces, &workspace, &host, 3).await;
    workspaces
        .declare_owner(&workspace, ada.user_public_key, None)
        .await
        .unwrap();
    assert!(workspaces.is_owner(&workspace, ada.user_public_key).await);
    let policy = workspaces.policy_of(&workspace).await.unwrap();
    assert!(policy.is_administrator(&hex::encode(host.user_public_key)));
    // Only the owner hands the workspace on; the host no longer does.
    let again = workspaces
        .declare_owner(&workspace, bo.user_public_key, None)
        .await
        .unwrap_err();
    assert!(
        again.to_string().contains("only the workspace's owner"),
        "{again}"
    );

    // The owner makes Bo an administrator, and Bo makes Cy one.
    workspaces
        .set_member_administrator(&workspace, &ada, [43; 32], bo.user_public_key, None)
        .await
        .unwrap();
    workspaces
        .set_member_administrator(&workspace, &bo, [44; 32], cy.user_public_key, None)
        .await
        .unwrap();
    // Bo unmakes neither Cy nor the owner.
    for person in [&cy, &ada] {
        assert!(
            workspaces
                .set_member_role(
                    &workspace,
                    &bo,
                    role_request(person, Role::Editor, rand::random())
                )
                .await
                .is_err()
        );
    }
    // The owner may unmake Cy, but not through the host, which signs only
    // what an administrator may.
    let relayed = workspaces
        .set_member_role(
            &workspace,
            &ada,
            role_request(&cy, Role::Editor, rand::random()),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        relayed.contains(
            "only the workspace's owner makes this change, and from a device of their own"
        ),
        "{relayed}"
    );
    let policy = workspaces.policy_of(&workspace).await.unwrap();
    assert!(policy.is_administrator(&hex::encode(cy.user_public_key)));

    // An administrator invites one.
    workspaces
        .issue_as(&workspace, &cy, invitation(45, Role::Editor), true)
        .await
        .unwrap();
    server.shutdown().await;
}

/// A page's own route to the servers keeping a workspace: its owner lists them, and a person
/// the workspace does not let choose them is refused, as is a site that names no server and a
/// server that does not keep it.
#[tokio::test]
async fn a_page_lists_and_chooses_the_servers_keeping_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;

    // Its owner lists the one server it was made on, which governs it.
    let listed = workspaces
        .page_workspaces(
            &owner,
            WorkspacesRequest::Servers {
                workspace: workspace.clone(),
            },
        )
        .await;
    let WorkspacesReply::Servers(servers) = listed else {
        panic!("the page lists the servers: {listed:?}");
    };
    assert_eq!(servers.len(), 1);
    assert!(servers[0].here, "the server asked lists itself first");
    assert_eq!(servers[0].device, hex::encode(owner.device_public_key));

    // Someone the workspace does not let choose its servers sees none of them.
    let (_, guest) = guest_device();
    let refused = workspaces
        .page_workspaces(
            &guest,
            WorkspacesRequest::Servers {
                workspace: workspace.clone(),
            },
        )
        .await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "{refused:?}"
    );

    // A host line that names no host, and a server that does not keep it, are refused in
    // their own words.
    let refused = workspaces
        .page_workspaces(
            &owner,
            WorkspacesRequest::AddServer {
                workspace: workspace.clone(),
                host: "not a host line".into(),
            },
        )
        .await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "{refused:?}"
    );
    let refused = workspaces
        .page_workspaces(
            &owner,
            WorkspacesRequest::RemoveServer {
                workspace,
                server: hex::encode([7; 32]),
            },
        )
        .await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "{refused:?}"
    );
    server.shutdown().await;
}

/// What a person keeps their name in, as a test reads it back.
#[derive(Default)]
struct KeptNames(std::sync::Mutex<std::collections::HashMap<[u8; 32], String>>);

impl crate::workspaces::PersonNames for KeptNames {
    fn name_of(&self, person: &[u8; 32]) -> Option<String> {
        self.0.lock().unwrap().get(person).cloned()
    }

    fn keep_name(&self, person: &[u8; 32], name: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(*person, name.to_owned());
        Ok(())
    }
}

/// A person names themselves once, in one workspace, and goes by that name in every workspace
/// this server governs: the name is theirs, kept with their profile, and a workspace they are
/// declared in later calls them by it too.
#[tokio::test]
async fn a_name_given_in_one_workspace_is_the_persons_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = std::sync::Arc::new(KeptNames::default());
    workspaces.person_names(kept.clone());
    let lounge = workspaces.create("Lounge").await.unwrap().workspace;
    let own = workspaces.create("Their own").await.unwrap().workspace;

    let person = SigningKey::from_bytes(&rand::random());
    let root = person.verifying_key().to_bytes();
    for workspace in [&lounge, &own] {
        workspaces
            .set_member_administrator(workspace, &owner, rand::random(), root, Some("Eager Lynx"))
            .await
            .unwrap();
    }
    let named = async |workspace: &str| {
        workspaces
            .summary(workspace)
            .await
            .unwrap()
            .members
            .into_iter()
            .find(|member| member.root() == Some(root) && !member.removed)
            .map(|member| member.name)
    };

    let theirs = device_of(
        &person,
        "their laptop",
        DeviceCapabilities::server_key_grant(),
    );
    workspaces
        .name_self(&lounge, &theirs, "Quinten", tilo_clock::unix_now_ns())
        .await
        .unwrap();
    assert_eq!(kept.name_of(&root).as_deref(), Some("Quinten"));
    assert_eq!(named(&lounge).await.as_deref(), Some("Quinten"));
    assert_eq!(named(&own).await.as_deref(), Some("Quinten"));

    // Declared in a third workspace under another name: the one they gave themselves wins as
    // soon as that workspace's governance moves.
    let later = workspaces.create("Later").await.unwrap().workspace;
    workspaces
        .set_member_administrator(&later, &owner, rand::random(), root, Some("Witty Otter"))
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while named(&later).await.as_deref() != Some("Quinten") {
        assert!(
            std::time::Instant::now() < deadline,
            "the later workspace still calls them {:?}",
            named(&later).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A person who changes their mind: the name they give last is the one they go by, in the
/// workspace and in the profile this server keeps for them.
#[tokio::test]
async fn a_person_renames_themselves_and_the_last_name_is_the_one_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = std::sync::Arc::new(KeptNames::default());
    workspaces.person_names(kept.clone());
    let lounge = workspaces.create("Lounge").await.unwrap().workspace;
    let person = SigningKey::from_bytes(&rand::random());
    let root = person.verifying_key().to_bytes();
    workspaces
        .set_member_administrator(&lounge, &owner, rand::random(), root, Some("Eager Lynx"))
        .await
        .unwrap();
    let named = async || {
        workspaces
            .summary(&lounge)
            .await
            .unwrap()
            .members
            .into_iter()
            .find(|member| member.root() == Some(root) && !member.removed)
            .map(|member| member.name)
    };
    let theirs = device_of(
        &person,
        "their laptop",
        DeviceCapabilities::server_key_grant(),
    );

    workspaces
        .name_self(&lounge, &theirs, "Quinten", tilo_clock::unix_now_ns())
        .await
        .unwrap();
    assert_eq!(named().await.as_deref(), Some("Quinten"));
    workspaces
        .name_self(&lounge, &theirs, "Quinten Wolff", tilo_clock::unix_now_ns())
        .await
        .unwrap();
    assert_eq!(
        named().await.as_deref(),
        Some("Quinten Wolff"),
        "the second name they give is the one the workspace calls them by"
    );
    assert_eq!(
        kept.name_of(&root).as_deref(),
        Some("Quinten Wolff"),
        "and the one kept with their profile"
    );
}
