//! The invitations whoever runs the workspace asks its host for on the
//! operator socket ([`OperatorCommand::Invite`]): made by the host itself,
//! so the person who opens one reads the workspace's name rather than a
//! person's, and answered with the link to hand over.

use base64::Engine as _;
use tilo_relay::workspaces::{HostInvitation, Workspaces};
use tilo_relay_protocol::operator::{InviteRole, OperatorEvent};
use tilo_relay_protocol::workspaces::Role;

/// How long a host-made invitation stands: a week, as a member's does.
const INVITATION_NS: u64 = 7 * 24 * 3600 * 1_000_000_000;
/// How long an owner's invitation stands: as long as any invitation may, since
/// it waits for whoever runs the host to open it.
const OWNER_INVITATION_NS: u64 = 30 * 24 * 3600 * 1_000_000_000;
/// An owner's invitation handed out again keeps at least this long.
const OWNER_INVITATION_LEFT_NS: u64 = 24 * 3600 * 1_000_000_000;
/// How long the site gets to keep an invitation before the link carries it whole.
const SHELF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where the links the host hands out open: the Tilo app's web entry, and the
/// origin the relay is published at, which the link names as the host to ask.
#[derive(Clone, Debug, Default)]
pub(super) struct Links {
    pub web_entry: Option<String>,
    pub relay_origin: Option<String>,
}

/// What the invitation a [`OperatorCommand::Invite`] asks for is.
///
/// [`OperatorCommand::Invite`]: tilo_relay_protocol::operator::OperatorCommand::Invite
pub(super) struct Asked {
    pub request: String,
    pub workspace: String,
    pub role: InviteRole,
    pub invitee_name: Option<String>,
    pub unless_owned: bool,
    /// The wait-list entry the invitation is issued for, where the person is
    /// an address the entry kept.
    pub wait_list_entry: Option<[u8; 32]>,
}

/// Answer `asked`: the invitation's link, that the workspace is owned
/// already, or why the host made none.
pub(super) async fn invite(workspaces: &Workspaces, links: &Links, asked: Asked) -> String {
    let request = asked.request.clone();
    match made(workspaces, links, asked).await {
        Ok(Some(event)) => event,
        Ok(None) => OperatorEvent::Owned { request },
        Err(reason) => OperatorEvent::InviteRefused { request, reason },
    }
    .to_line()
}

async fn made(
    workspaces: &Workspaces,
    links: &Links,
    asked: Asked,
) -> Result<Option<OperatorEvent>, String> {
    let Asked {
        request,
        workspace,
        role,
        invitee_name,
        unless_owned,
        wait_list_entry,
    } = asked;
    let owner = role == InviteRole::Owner;
    if request.is_empty() || request.len() > 128 {
        return Err("a request is named by 1 to 128 bytes".into());
    }
    let workspace = governed(workspaces, &workspace).await?;
    let now = tilo_clock::unix_now_ns();
    let held_before = if owner && unless_owned {
        if workspaces.has_owner(&workspace).await {
            return Ok(None);
        }
        workspaces
            .open_owner_invitation(&workspace, now.saturating_add(OWNER_INVITATION_LEFT_NS))
            .await
    } else {
        None
    };
    let (issued, expires_at_ns) = if let Some(held) = held_before {
        held
    } else {
        let expires_at_ns = now.saturating_add(if owner {
            OWNER_INVITATION_NS
        } else {
            INVITATION_NS
        });
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo host invitation");
        hasher.update(workspace.as_bytes());
        hasher.update(request.as_bytes());
        hasher.update(&now.to_le_bytes());
        let issued = workspaces
            .invite_as_host(
                &workspace,
                HostInvitation {
                    request_id: *hasher.finalize().as_bytes(),
                    role: match role {
                        InviteRole::Viewer => Role::Viewer,
                        InviteRole::Commenter => Role::Commenter,
                        InviteRole::Editor | InviteRole::Administrator | InviteRole::Owner => {
                            Role::Editor
                        }
                    },
                    administrator: role == InviteRole::Administrator,
                    owner,
                    invitee_name,
                    expires_at_ns,
                    wait_list_entry,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        (issued, expires_at_ns)
    };
    let link = link(links, &workspace, &issued.signed_invitation, expires_at_ns).await?;
    Ok(Some(OperatorEvent::Invited {
        request,
        link,
        invite: hex::encode(issued.invite_id),
        expires_at_ns,
    }))
}

/// The workspace governed here that `named` names: by its id, or by the name
/// people see for it when only one has that name.
async fn governed(workspaces: &Workspaces, named: &str) -> Result<String, String> {
    let all = workspaces.list().await;
    if all.iter().any(|summary| summary.workspace == named) {
        return Ok(named.to_owned());
    }
    let mut called = all
        .iter()
        .filter(|summary| summary.name.trim().eq_ignore_ascii_case(named.trim()));
    match (called.next(), called.next()) {
        (Some(summary), None) => Ok(summary.workspace.clone()),
        (Some(_), Some(_)) => Err(format!(
            "more than one workspace here is called {named}: name it by its id"
        )),
        (None, _) => Err(format!("no workspace here is called {named}")),
    }
}

/// The link that brings whoever opens it into `workspace` with `invitation`
/// (its signed bytes): sealed onto the site's shelf, the short link a
/// person's own invite sheet hands over, or, where the site keeps nothing,
/// the invitation's own text after the workspace's route. Either opens the
/// site and joins.
async fn link(
    links: &Links,
    workspace: &str,
    invitation: &[u8],
    expires_at_ns: u64,
) -> Result<String, String> {
    let Some(entry) = links.web_entry.as_deref() else {
        return Err("the workspace names no web entry (--web-entry) for its links to open".into());
    };
    match shelve(
        entry,
        links.relay_origin.as_deref(),
        invitation,
        expires_at_ns,
    )
    .await
    {
        Ok(link) => Ok(link),
        Err(error) => {
            tracing::info!(%error, "the site kept no invitation; handing over its text");
            let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invitation);
            let entry = entry.trim_end_matches('/');
            Ok(match links.relay_origin.as_deref().map(host_name) {
                Some(host) => format!("{entry}/#/@{host}/w/{workspace}?invite={text}"),
                None => format!("{entry}/#/w/{workspace}?invite={text}"),
            })
        }
    }
}

/// `origin` without its scheme: how a link's route names a host.
fn host_name(origin: &str) -> &str {
    origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/')
}

/// Leave `invitation` sealed on the shelf of the site at `entry` until
/// `expires_at_ns`, naming `host`, the origin it admits into, and answer the
/// short link that opens it.
async fn shelve(
    entry: &str,
    host: Option<&str>,
    invitation: &[u8],
    expires_at_ns: u64,
) -> Result<String, String> {
    let sealed = tilo_peer_state::join::sealed::seal(invitation)?;
    let site = entry.split('#').next().unwrap_or(entry);
    let site = if site.ends_with('/') {
        site.to_owned()
    } else {
        format!("{site}/")
    };
    let stored = tilo_relay_protocol::sealed_invitations::Stored {
        sealed: sealed.sealed_text(),
        expires_at_ns,
        delete: sealed.delete.clone(),
        host: host.map(str::to_owned),
    };
    let client = tilo_net::Client::builder()
        .timeout(SHELF_TIMEOUT)
        .redirects(tilo_net::Redirects::None)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{site}i/{}", sealed.handle))
        .header("content-type", "application/json")
        .body(serde_json::to_string(&stored).map_err(|error| error.to_string())?)
        .send()
        .await
        .map_err(|error| format!("the site's shelf could not be reached: {error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the site did not keep the invitation: {status}"));
    }
    sealed.link(&site)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_site_that_keeps_nothing_gets_the_invitation_whole_in_the_link() {
        let links = Links {
            // Nothing listens on port 9 of this machine.
            web_entry: Some("http://127.0.0.1:9/".into()),
            relay_origin: Some("https://eu1.tilo.example".into()),
        };
        let link = link(&links, "w", &[1, 2, 3], u64::MAX).await.unwrap();
        assert_eq!(
            link,
            "http://127.0.0.1:9/#/@eu1.tilo.example/w/w?invite=AQID"
        );
        let refused = link_without_entry().await;
        assert!(refused.contains("--web-entry"), "{refused}");
    }

    async fn link_without_entry() -> String {
        link(&Links::default(), "w", &[1], u64::MAX)
            .await
            .unwrap_err()
    }
}
