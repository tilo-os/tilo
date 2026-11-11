//! A workspace this relay governs decides its own administration: what another
//! person's device may do is what the workspace's governance lets that person,
//! and what no workspace decides stays the relay's person's.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::AuthenticatedPeer;
use tilo_relay_protocol::operator::WaitListStanding;
use tilo_relay_protocol::voice::{CallOperation, CallState};
use tilo_relay_protocol::workspaces::{
    JoinReply, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::{DeviceCert, sign_device_cert};

use crate::workspaces::tests::{governor, guest_device, signed_pre_admission, test_host};
use crate::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, PreAdmitRequest, RemoveMemberRequest,
    SetMemberRoleRequest, WaitListMail, WaitListSecrets, WaitListed, Workspaces,
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
        Subject::Person(who),
        roles::governance_action(workspace, action),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_room_grants(workspace, owner, request_id, vec![grant])
        .await
        .unwrap();
}

async fn set_role(
    workspaces: &Workspaces,
    workspace: &str,
    by: &DeviceCert,
    person: &DeviceCert,
    role: MemberRole,
) {
    workspaces
        .set_member_role(workspace, by, role_request(person, role, rand::random()))
        .await
        .unwrap();
}

fn role_request(
    person: &DeviceCert,
    role: MemberRole,
    request_id: [u8; 32],
) -> SetMemberRoleRequest {
    SetMemberRoleRequest {
        request_id,
        user_public_key: person.user_public_key,
        role,
    }
}

fn invitation(request_id: u8, role: MemberRole) -> IssueInvitationRequest {
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
async fn an_address_left_at_the_door_is_on_the_wait_list_and_never_a_person_to_admit() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
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
    let ask = |request| workspaces.page_rooms(&ada, request);
    let list = || WorkspacesRequest::WaitList {
        workspace: workspace.clone(),
    };

    // Someone the workspace does not let invite reads no wait list.
    assert!(matches!(ask(list()).await, WorkspacesReply::Refused(_)));
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
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
    relay.shutdown().await;
}

/// Bo left an address where the door mails a confirmation link, and Cy one
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
        workspaces.wait_list_standing(workspace, [0x0b; 32]).await,
        Ok(WaitListStanding::Pending)
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

#[tokio::test]
async fn a_mailed_address_is_listed_once_confirmed_mailed_its_invitation_and_leaves_on_joining() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let bo = bo_confirmed_beside_cy(workspaces, &owner, &workspace).await;
    let invitation = the_address_is_invited_and_mailed(workspaces, &owner, &workspace, bo).await;
    joining_takes_it_off_the_list(workspaces, &owner, &workspace, invitation).await;
    relay.shutdown().await;
}

/// The entry `workspace` holds for the address `entry` names.
fn list_entry(workspace: &str, entry: [u8; 32]) -> WorkspacesRequest {
    WorkspacesRequest::Invite {
        workspace: workspace.to_owned(),
        request_id: [41; 32],
        role: WorkspaceRole::Editor,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
        wait_list_entry: Some(entry),
        recipient: None,
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
/// invited, mails it once on the operator's own door and is asked in. Returns
/// the invitation the person joins with.
async fn the_address_is_invited_and_mailed(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
    entry: [u8; 32],
) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = |secret: &[u8; 32]| -> [u8; 32] { sha2::Sha256::digest(secret).into() };
    let ask = |request| workspaces.page_rooms(owner, request);
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
        workspaces.wait_list_standing(workspace, [0x0b; 32]).await,
        Ok(WaitListStanding::Invited)
    );

    // A relay whose operator sends no mail mails nothing.
    assert!(matches!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://door.example/?invite=AAEC"
        ))
        .await,
        WorkspacesReply::Refused(reason) if reason.contains("no mail")
    ));
    let mut mail = workspaces.mail_wait_list("https://door.example/").unwrap();
    // Only an invitation on the operator's door is mailed.
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
            "https://door.example/?invite=AAEC"
        )),
        async {
            let WaitListMail {
                queued,
                to,
                link,
                removal,
            } = mail.recv().await.unwrap();
            queued.send(true).unwrap();
            (to, link, removal)
        }
    );
    assert!(matches!(answer, WorkspacesReply::WaitList(_)));
    assert_eq!(
        (to.contact.as_str(), to.language.as_str()),
        ("bo@example.org", "de")
    );
    assert_eq!(link, "https://door.example/?invite=AAEC");
    assert_eq!(
        workspaces
            .wait_list_standing(workspace, digest(&removal))
            .await,
        Ok(WaitListStanding::Invited),
        "the mail's removal link names the entry"
    );
    // Not twice within minutes.
    assert!(matches!(
        ask(mailed_invitation(
            workspace,
            entry,
            "https://door.example/?invite=AAEC"
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
        workspaces.wait_list_standing(workspace, [0x0b; 32]).await,
        Ok(WaitListStanding::Gone)
    );
}

#[tokio::test]
async fn an_address_mailed_its_invitation_takes_itself_off_with_the_mail_s_removal_link() {
    use sha2::Digest as _;
    let digest = |secret: &[u8; 32]| -> [u8; 32] { sha2::Sha256::digest(secret).into() };
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut mail = workspaces.mail_wait_list("https://door.example/").unwrap();
    assert!(
        workspaces.mail_wait_list("https://door.example/").is_none(),
        "one operator hears the mail"
    );
    workspaces
        .wait(&workspace, "cy@example.org", listed_at_once(0x0c), "en", 10)
        .await
        .unwrap();
    let ask = |request| workspaces.page_rooms(&owner, request);
    let listed = || read_wait_list(workspaces, &owner, &workspace);
    let cy = listed().await[0].request_id;
    let mail_invite = || WorkspacesRequest::MailInvite {
        workspace: workspace.clone(),
        wait_list_entry: cy,
        link: "https://door.example/?invite=AAEC".into(),
    };
    let invite = |wait_list_entry| WorkspacesRequest::Invite {
        workspace: workspace.clone(),
        request_id: rand::random(),
        role: WorkspaceRole::Editor,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
        wait_list_entry: Some(wait_list_entry),
        recipient: None,
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
    relay.shutdown().await;
}

#[tokio::test]
async fn a_lost_invitation_acknowledgement_keeps_the_reservation() {
    uncertain_invitation_mail(false).await;
}

#[tokio::test]
async fn a_timed_out_invitation_acknowledgement_keeps_the_reservation() {
    uncertain_invitation_mail(true).await;
}

async fn uncertain_invitation_mail(timeout: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let shutdown = {
        let relay = test_host(dir.path(), &key, &owner).await;
        assert_uncertain_invitation_mail(relay.workspaces(), &owner, timeout).await;
        relay.shutdown()
    };
    shutdown.await;
}

async fn assert_uncertain_invitation_mail(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    timeout: bool,
) {
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut mail = workspaces.mail_wait_list("https://door.example/").unwrap();
    workspaces
        .wait(&workspace, "cy@example.org", listed_at_once(0x0c), "en", 10)
        .await
        .unwrap();
    let cy = read_wait_list(workspaces, owner, &workspace).await[0].request_id;
    assert!(matches!(
        workspaces
            .page_rooms(
                owner,
                WorkspacesRequest::Invite {
                    workspace: workspace.clone(),
                    request_id: rand::random(),
                    role: WorkspaceRole::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                    inviter_name: None,
                    wait_list_entry: Some(cy),
                    recipient: None,
                }
            )
            .await,
        WorkspacesReply::Invitation { .. }
    ));
    let request = || WorkspacesRequest::MailInvite {
        workspace: workspace.clone(),
        wait_list_entry: cy,
        link: "https://door.example/?invite=AAEC".into(),
    };
    if timeout {
        tokio::time::pause();
    }
    let (answer, held) = tokio::join!(workspaces.page_rooms(owner, request()), async {
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
    let WorkspacesReply::Refused(reason) = workspaces.page_rooms(owner, request()).await else {
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
        let relay = test_host(dir.path(), &key, &owner).await;
        assert_pending_removal_contact_digest(relay.workspaces()).await;
        relay.shutdown()
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
    let expected: [u8; 32] = sha2::Sha256::digest(tilo_relay_protocol::workspaces::contact_key(
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
            .wait_list_standing(&workspace, [1; 32])
            .await
            .unwrap(),
        tilo_relay_protocol::operator::WaitListStanding::Gone
    );
}

/// `workspace`'s wait list as `owner` reads it.
async fn read_wait_list(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
) -> Vec<tilo_relay_protocol::workspaces::WaitListEntry> {
    let WorkspacesReply::WaitList(listed) = workspaces
        .page_rooms(
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

/// Secrets for a door that sends no mail, which lists an address at once.
const fn listed_at_once(removal: u8) -> WaitListSecrets {
    WaitListSecrets {
        removal: [removal; 32],
        confirm: None,
    }
}

#[tokio::test]
async fn a_member_granted_invite_runs_invitations_and_an_editor_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // An Editor by role alone governs nothing.
    assert!(
        workspaces
            .issue(&workspace, &ada, invitation(10, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(workspaces.pending(&workspace, &ada).await.is_err());
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::WaitList {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Refused(_)
    ));

    // The workspace grants Ada its invite action: she invites, up to her role.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    let issued = workspaces
        .issue(&workspace, &ada, invitation(11, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        workspaces
            .pending(&workspace, &ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = workspaces
        .issue(&workspace, &ada, invitation(12, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::WaitList {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::WaitList(_)
    ));
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::Notices {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Notices(_)
    ));
    workspaces
        .cancel(
            &workspace,
            &ada,
            CancelInvitationRequest {
                request_id: [13; 32],
                invite_id: issued.invite_id,
            },
        )
        .await
        .unwrap();
    assert!(
        !workspaces
            .pending(&workspace, &owner)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn roles_and_removals_follow_the_rooms_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    let dee = admit(workspaces, &workspace, &owner, 4).await;

    // A Moderator removes an Editor, but sets no roles.
    set_role(workspaces, &workspace, &owner, &bo, MemberRole::Moderator).await;
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &bo,
                role_request(&cy, MemberRole::Viewer, [20; 32])
            )
            .await
            .is_err()
    );
    workspaces
        .remove_member(
            &workspace,
            &bo,
            RemoveMemberRequest {
                request_id: [21; 32],
                user_public_key: dee.user_public_key,
            },
        )
        .await
        .unwrap();

    // Granted set-role, Ada hands out roles up to her own, and never
    // changes an administrator's: only the owner makes or unmakes those.
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &ada,
                role_request(&cy, MemberRole::Viewer, [22; 32])
            )
            .await
            .is_err()
    );
    grant(workspaces, &workspace, &owner, &ada, governance::SET_ROLE).await;
    set_role(workspaces, &workspace, &ada, &cy, MemberRole::Viewer).await;
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &ada,
                role_request(&cy, MemberRole::Moderator, [23; 32])
            )
            .await
            .is_err()
    );
    workspaces
        .set_member_administrator(&workspace, &owner, [24; 32], cy.user_public_key)
        .await
        .unwrap();
    let administrator = workspaces
        .set_member_role(
            &workspace,
            &ada,
            role_request(&cy, MemberRole::Viewer, [25; 32]),
        )
        .await
        .unwrap_err();
    assert!(
        administrator.to_string().contains("administrators"),
        "{administrator}"
    );
    // The owner's devices still change anyone's role.
    set_role(workspaces, &workspace, &owner, &cy, MemberRole::Editor).await;

    // Nobody removes the owner, the owner's devices included.
    for by in [&owner, &bo] {
        assert!(
            workspaces
                .remove_member(
                    &workspace,
                    by,
                    RemoveMemberRequest {
                        request_id: rand::random(),
                        user_public_key: owner.user_public_key,
                    },
                )
                .await
                .is_err()
        );
    }
    relay.shutdown().await;
}

#[tokio::test]
async fn an_administrator_governs_within_their_role_without_further_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    set_role(workspaces, &workspace, &owner, &cy, MemberRole::Viewer).await;

    workspaces
        .set_member_administrator(&workspace, &owner, [50; 32], ada.user_public_key)
        .await
        .unwrap();
    an_administrator_invites_within_their_role(workspaces, &workspace, &ada).await;
    an_administrator_rules_those_ranked_below(workspaces, &workspace, &ada, &bo, &cy).await;
    an_administrator_runs_the_workspaces_calls(workspaces, &workspace, &ada).await;
    unmaking_the_administrator_takes_it_all_back(workspaces, &workspace, &owner, &ada, &bo).await;
    relay.shutdown().await;
}

/// The co-owner invites, up to their own role, with no grant beyond the owner
/// making them an administrator, and reads the requests asked of the
/// workspace.
async fn an_administrator_invites_within_their_role(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
) {
    let issued = workspaces
        .issue(workspace, ada, invitation(51, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        workspaces
            .pending(workspace, ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = workspaces
        .issue(workspace, ada, invitation(52, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        workspaces
            .page_rooms(
                ada,
                WorkspacesRequest::WaitList {
                    workspace: workspace.to_owned()
                }
            )
            .await,
        WorkspacesReply::WaitList(_)
    ));
}

/// They set roles up to their own and remove those ranked below them, never a
/// fellow Editor.
async fn an_administrator_rules_those_ranked_below(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
    bo: &DeviceCert,
    cy: &DeviceCert,
) {
    set_role(workspaces, workspace, ada, cy, MemberRole::Commenter).await;
    assert!(
        workspaces
            .set_member_role(
                workspace,
                ada,
                role_request(bo, MemberRole::Moderator, [53; 32])
            )
            .await
            .is_err()
    );
    assert!(
        workspaces
            .remove_member(
                workspace,
                ada,
                RemoveMemberRequest {
                    request_id: [54; 32],
                    user_public_key: bo.user_public_key,
                },
            )
            .await
            .is_err()
    );
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

/// They run the workspace's calls.
async fn an_administrator_runs_the_workspaces_calls(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
) {
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let answer = workspaces.voice_call(workspace, &peer(ada), &end).await;
    assert!(
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call")),
        "{answer:?}"
    );
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
            .issue(workspace, bo, invitation(56, MemberRole::Viewer))
            .await
            .is_err()
    );
    set_role(workspaces, workspace, owner, ada, MemberRole::Editor).await;
    assert!(
        workspaces
            .issue(workspace, ada, invitation(57, MemberRole::Viewer))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_next_governance_change_rewrites_an_administrators_old_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let who = hex::encode(ada.user_public_key);
    // An administrator's grant as workspaces held it before: on the workspace's
    // resource, where the Editor's own Deny outranked it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&who).as_str(),
        Subject::Person(who),
        roles::workspace_resource(&workspace),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_room_grants(&workspace, &owner, [60; 32], vec![legacy])
        .await
        .unwrap();
    workspaces
        .issue(&workspace, &ada, invitation(61, MemberRole::Editor))
        .await
        .unwrap();
    relay.shutdown().await;
}

#[tokio::test]
async fn what_no_room_decides_stays_the_relay_persons() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    // Ada holds every governance action the workspace has.
    for action in governance::ALL {
        grant(workspaces, &workspace, &owner, &ada, action).await;
    }
    workspaces
        .issue(&workspace, &ada, invitation(30, MemberRole::Viewer))
        .await
        .unwrap();

    assert!(
        workspaces.enter(&workspace, &ada).await.is_err(),
        "entering makes an owner"
    );
    for request in [
        WorkspacesRequest::List,
        WorkspacesRequest::Create {
            name: "Another".into(),
        },
        WorkspacesRequest::Enter {
            workspace: workspace.clone(),
        },
    ] {
        let reply = workspaces.page_rooms(&ada, request.clone()).await;
        assert!(
            matches!(reply, WorkspacesReply::Refused(_)),
            "{request:?}: {reply:?}"
        );
    }
    assert!(
        workspaces
            .set_member_administrator(&workspace, &ada, [31; 32], bo.user_public_key)
            .await
            .is_err()
    );
    assert!(
        workspaces
            .put_room_grants(&workspace, &ada, [32; 32], Vec::new())
            .await
            .is_err()
    );
    assert!(
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
            .is_err()
    );
    assert_eq!(workspaces.list().await.len(), 1);
    relay.shutdown().await;
}

#[tokio::test]
async fn calls_are_run_by_the_owner_and_by_members_granted_call() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    set_role(workspaces, &workspace, &owner, &bo, MemberRole::Moderator).await;
    // Ending a call the workspace never had: past the gate, the call itself
    // answers that it is unknown.
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let past_the_gate = |answer: &Result<CallState, String>| {
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call"))
    };
    let owners = workspaces.voice_call(&workspace, &peer(&owner), &end).await;
    assert!(past_the_gate(&owners), "{owners:?}");
    for member in [&ada, &bo] {
        let answer = workspaces.voice_call(&workspace, &peer(member), &end).await;
        assert!(answer.is_err() && !past_the_gate(&answer), "{answer:?}");
    }
    grant(workspaces, &workspace, &owner, &ada, governance::CALL).await;
    let granted = workspaces.voice_call(&workspace, &peer(&ada), &end).await;
    assert!(past_the_gate(&granted), "{granted:?}");
    relay.shutdown().await;
}

#[tokio::test]
async fn an_expired_certificate_administers_nothing_even_the_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
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
            .issue(&workspace, &expired, invitation(40, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(workspaces.pending(&workspace, &expired).await.is_err());
    assert!(workspaces.enter(&workspace, &expired).await.is_err());
    workspaces.pending(&workspace, &owner).await.unwrap();
    relay.shutdown().await;
}

fn peer(cert: &DeviceCert) -> AuthenticatedPeer {
    AuthenticatedPeer {
        cert: cert.clone(),
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
    }
}
