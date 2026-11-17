//! Taking the people an earlier release kept into this relay's workspace
//! ([`OperatorCommand::ImportPeople`]): their addresses go on the wait list as
//! they stood, an owner is declared by their key, and every other person is
//! invited through the host's own invitation, mailed to the address the export
//! names and handed over as a link otherwise.
//!
//! [`OperatorCommand::ImportPeople`]: tilo_relay_protocol::operator::OperatorCommand::ImportPeople

use tilo_relay::workspaces::{WaitListSecrets, WaitListed, Workspaces};
use tilo_relay_protocol::operator::InviteRole;
use tilo_relay_protocol::release_people::{Imported, Invitation, Member, Owner, People, Waiting};
use tilo_relay_protocol::workspaces::{
    WaitListEntry, WorkspacesReply, WorkspacesRequest, contact_key,
};
use tilo_user_identity::DeviceCert;

use super::host_invite::{self, Asked, Links};

/// Take `people` into `workspace`: their addresses on the wait list as they
/// stood, each owner declared by their key, and every other person invited
/// through the host's own invitation, mailed where the export names an address
/// and handed over as a link otherwise.
pub(super) async fn import(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    links: &Links,
    people: People,
) -> Imported {
    let mut imported = Imported {
        waiting: Vec::new(),
        invitations: Vec::new(),
        owners: Vec::new(),
        refused: Vec::new(),
    };
    // The entry keeps the addresses only where it takes people by invitation
    // and keeps a wait list. Where it does not, everyone is still invited, at a
    // link to hand over: nobody is dropped.
    let keeps = workspaces
        .entry_policy(workspace)
        .await
        .is_some_and(|entry| entry.keeps_requests());
    if !keeps && !people.wait_list.is_empty() {
        imported.refused.push(
            "the workspace keeps no wait list: the addresses it kept come over as invitations only"
                .to_owned(),
        );
    }
    if workspaces.mail_entry().is_none()
        && people.members.iter().any(|member| member.email.is_some())
    {
        imported.refused.push(
            "the workspace sends no mail: the people with an address are invited at a link only \
             (hand it over yourself), and none is mailed"
                .to_owned(),
        );
    }
    for waiting in &people.wait_list {
        if !keeps {
            imported.refused.push(format!(
                "{} was on the wait list, and this workspace keeps none: invite them by hand",
                waiting.contact
            ));
            continue;
        }
        match keep(workspaces, workspace, waiting, waiting.confirmed).await {
            Ok(()) => imported.waiting.push(waiting.clone()),
            Err(reason) => imported.refused.push(format!(
                "{} stays off the wait list: {reason}",
                waiting.contact
            )),
        }
    }
    let host_root = hex::encode(host.user_public_key);
    let owned = workspaces.has_owner(workspace).await;
    for member in &people.members {
        // The host that ran the earlier release was nobody in its workspace,
        // and the host that runs this one is nobody here: it is no person to
        // bring over.
        if member.root.eq_ignore_ascii_case(&host_root) {
            continue;
        }
        let role = member.role.to_lowercase();
        if role == "owner" {
            if owned {
                // Whoever the workspace is kept for stands already, declared
                // by the deployment.
                continue;
            }
            match declare_owner(workspaces, workspace, member).await {
                Ok(owner) => imported.owners.push(owner),
                Err(reason) => imported
                    .refused
                    .push(format!("{} is not the owner: {reason}", member.name)),
            }
            continue;
        }
        let Some(role) = invite_role(&role) else {
            imported.refused.push(format!(
                "{} was a {} and this workspace has no such role",
                member.name, member.role
            ));
            continue;
        };
        match invite(workspaces, workspace, host, links, member, role, keeps).await {
            Ok(invitation) => imported.invitations.push(invitation),
            Err(reason) => imported
                .refused
                .push(format!("{} was not invited: {reason}", member.name)),
        }
    }
    imported
}

/// What the earlier release's role `role` is in this build, if it has one: its
/// moderator governed the workspace as an administrator does.
fn invite_role(role: &str) -> Option<InviteRole> {
    Some(match role {
        "viewer" => InviteRole::Viewer,
        "commenter" => InviteRole::Commenter,
        "editor" => InviteRole::Editor,
        "administrator" | "moderator" => InviteRole::Administrator,
        _ => return None,
    })
}

/// The name this build gives `role`, as the report says it.
fn role_name(role: InviteRole) -> &'static str {
    match role {
        InviteRole::Viewer => "viewer",
        InviteRole::Commenter => "commenter",
        InviteRole::Editor => "editor",
        InviteRole::Administrator => "administrator",
        InviteRole::Owner => "owner",
    }
}

/// Keep `waiting` on the workspace's wait list as the earlier release held it:
/// listed where it was confirmed, hidden until its confirmation link is opened
/// where it was not, with every removal link it was mailed still taking it off.
async fn keep(
    workspaces: &Workspaces,
    workspace: &str,
    waiting: &Waiting,
    listed_at_once: bool,
) -> Result<(), String> {
    let mut removals = hashes(&waiting.removal_sha256);
    // A release kept every removal link it mailed; the entry needs one, and
    // the rest are kept on it beside the one this build would mail.
    let removal = if removals.is_empty() {
        rand::random()
    } else {
        removals.remove(0)
    };
    let confirm = if listed_at_once {
        None
    } else {
        hashes(&waiting.confirm_sha256).into_iter().next()
    };
    let secrets = WaitListSecrets {
        removal,
        confirm,
        earlier: removals,
    };
    // No cap: the list held these before, and a full list would drop people the
    // earlier release kept.
    match workspaces
        .wait(
            workspace,
            &waiting.contact,
            secrets,
            &waiting.language,
            usize::MAX,
        )
        .await
        .map_err(|error| error.to_string())?
    {
        WaitListed::Added { .. } | WaitListed::Already { .. } => Ok(()),
        WaitListed::Full => Err("the wait list is full".to_owned()),
    }
}

/// The hashes as the earlier release wrote them: lowercase hex, and a written
/// hash that is no SHA-256 is left out rather than guessed at.
fn hashes(hashes: &[String]) -> Vec<[u8; 32]> {
    hashes
        .iter()
        .filter_map(|hash| tilo_relay_protocol::operator::hex32(hash))
        .collect()
}

/// Declare `member` an owner of `workspace` by their key: whoever the earlier
/// release kept the workspace for keeps it here, without a link to open.
async fn declare_owner(
    workspaces: &Workspaces,
    workspace: &str,
    member: &Member,
) -> Result<Owner, String> {
    let root = tilo_relay_protocol::operator::hex32(&member.root)
        .ok_or("the person's user root is not 64 lowercase hex characters")?;
    workspaces
        .declare_owner(workspace, root, Some(&member.name))
        .await
        .map_err(|error| error.to_string())?;
    Ok(Owner {
        name: member.name.clone(),
        root: member.root.clone(),
    })
}

/// Invite `member` through the host's own invitation as `role`: at the
/// wait-list entry their address keeps, so the entry mails them the invitation,
/// or as a link to hand over where no address is known.
async fn invite(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    links: &Links,
    member: &Member,
    role: InviteRole,
    keeps: bool,
) -> Result<Invitation, String> {
    let mut entry = None;
    if let (Some(email), true) = (&member.email, keeps) {
        let waiting = Waiting {
            contact: email.clone(),
            language: "en".to_owned(),
            added_at_ns: 0,
            // The invitation is mailed at once, so the address needs no
            // confirmation of its own.
            confirmed: true,
            removal_sha256: Vec::new(),
            confirm_sha256: Vec::new(),
        };
        keep(workspaces, workspace, &waiting, true).await?;
        entry = wait_list_entry(workspaces, workspace, host, email).await?;
    }
    let request = hex::encode(rand::random::<[u8; 16]>());
    let link = host_invite::invite(
        workspaces,
        links,
        Asked {
            request,
            workspace: workspace.to_owned(),
            role,
            invitee_name: member.email.clone().or_else(|| Some(member.name.clone())),
            wait_list_entry: entry,
        },
    )
    .await?;
    let mut mailed = false;
    if let Some(entry) = entry {
        let mailed_reply = workspaces
            .page_workspaces(
                host,
                WorkspacesRequest::MailInvite {
                    workspace: workspace.to_owned(),
                    wait_list_entry: entry,
                    link: link.clone(),
                },
            )
            .await;
        mailed = matches!(mailed_reply, WorkspacesReply::WaitList(_));
        if !mailed {
            return Err(format!(
                "the workspace did not mail the invitation: {mailed_reply:?}"
            ));
        }
    }
    Ok(Invitation {
        name: member.name.clone(),
        email: member.email.clone(),
        role: role_name(role).to_owned(),
        link,
        mailed,
    })
}

/// The wait-list entry `contact` holds now, as the entry keys it.
async fn wait_list_entry(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    contact: &str,
) -> Result<Option<[u8; 32]>, String> {
    let listed = workspaces
        .page_workspaces(
            host,
            WorkspacesRequest::WaitList {
                workspace: workspace.to_owned(),
            },
        )
        .await;
    let WorkspacesReply::WaitList(entries) = listed else {
        return Err(format!("the wait list does not read: {listed:?}"));
    };
    let key = contact_key(contact);
    Ok(entries
        .into_iter()
        .find(|entry| contact_key(&entry.contact) == key)
        .map(|entry: WaitListEntry| entry.request_id))
}
