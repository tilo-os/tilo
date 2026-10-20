//! The one field a person joins a workspace with: "Join a workspace…".
//!
//! It takes whatever they were given and decides what it is:
//!
//! - a site (`tilo.coffee`, `https://example.org/`): its home workspace,
//!   joined through the site's admission door;
//! - a workspace link in the web grammar
//!   (docs/architecture/web-routing.md): with an `invite` secret, that
//!   invitation, redeemed at the host the link names over HTTPS; without
//!   one, the site's workspace through its door, when that is the room the
//!   link names;
//! - an invitation's text alone: redeemed at the host it names, over the
//!   iroh endpoint the invitation carries.
//!
//! Which hosts count as Tilo workspaces comes from the link grammar and the
//! site's own `entry.json`, never from a list of domains here.

use tilo_reference::link::{Link, Scope, WebEntries, WebEntry};

use super::hosted_sites::{self, HostHttp, HostedRoom, SiteError};
use super::site_join::SiteJoin;
use super::{RoomId, ShellNetwork};
use crate::network::invite::enrollment_driver::Via;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::WorkspaceId;

/// How long opening a link waits for the workspace it joins to be held.
const LINK_JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// What a person pasted or typed into the join field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JoinText {
    /// An invitation's text, redeemed over the host's iroh endpoint.
    Invitation(String),
    /// A site, whose home workspace its door admits people to; with the
    /// room a link named, which must be that workspace.
    Site {
        entry: url::Url,
        room: Option<RoomId>,
    },
    /// A workspace link carrying an invitation: redeemed at its host.
    InvitationLink {
        origin: String,
        room: RoomId,
        invitation: String,
    },
}

/// The entry of a web link: everything before its route, without the
/// full-tab `app/` segment.
fn link_entry(url: &str) -> Option<String> {
    let before = url.split_once('#').map_or(url, |(before, _)| before);
    let before = before.split_once('?').map_or(before, |(before, _)| before);
    let before = before.strip_suffix("app/").unwrap_or(before);
    before.ends_with('/').then(|| before.to_owned())
}

/// Read the join field's `text`.
///
/// # Errors
///
/// [`SiteError::Invalid`] when it is none of the things the field takes.
pub(crate) fn classify(text: &str) -> Result<JoinText, SiteError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(SiteError::Invalid);
    }
    if !text.contains('.') && !text.contains(':') && !text.contains('/')
        || tilo_peer_state::join::decode_and_verify_invitation_text(
            text,
            None,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .is_ok()
    {
        return Ok(JoinText::Invitation(text.to_owned()));
    }
    if text.contains('#')
        && let Some(entry) = link_entry(text)
        && let Ok(web_entry) = WebEntry::new(&entry)
        && let Ok(link) = Link::parse(text, &WebEntries::new(vec![web_entry]))
    {
        let entry = hosted_sites::site_entry(&entry)?;
        if link.secrets.pair.is_some() {
            return Err(SiteError::PairingLink);
        }
        let Scope::Room { host, room, .. } = link.target.scope else {
            return Ok(JoinText::Site { entry, room: None });
        };
        let room = RoomId::from(room);
        if let Some(invitation) = link.secrets.invite {
            return Ok(JoinText::InvitationLink {
                origin: host.as_str().to_owned(),
                room,
                invitation: invitation.expose().to_owned(),
            });
        }
        return Ok(JoinText::Site {
            entry,
            room: Some(room),
        });
    }
    Ok(JoinText::Site {
        entry: hosted_sites::site_entry(text)?,
        room: None,
    })
}

/// Whether `text` is an invitation's text itself (not a word that merely
/// could be one).
pub(crate) fn is_invitation(text: &str) -> bool {
    tilo_peer_state::join::decode_and_verify_invitation_text(
        text.trim(),
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .is_ok()
}

/// What the field will do with `text`, in the person's words; `None` while
/// it is nothing the field takes yet.
pub(crate) fn describe(text: &str) -> Option<String> {
    let host = |url: &str| {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default()
    };
    Some(match classify(text).ok()? {
        JoinText::Invitation(_) => crate::shell::i18n::t("peer-sync-join-invitation"),
        JoinText::Site { entry, .. } => crate::shell::i18n::t_args(
            "peer-sync-join-site",
            &[("host", host(entry.as_str()).into())],
        ),
        JoinText::InvitationLink { origin, .. } => crate::shell::i18n::t_args(
            "peer-sync-join-invitation-link",
            &[("host", host(&origin).into())],
        ),
    })
}

impl ShellNetwork {
    /// Join what the person put into the join field, `local` being the
    /// workspace in front: an invitation's text is redeemed into it, as
    /// before; a site or a workspace link joins as a new workspace.
    /// Blocks on the site and host: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// What the person is told: why it was not joined.
    pub(crate) fn join_from_field(
        &self,
        local: WorkspaceId,
        text: &str,
    ) -> Result<PeerSyncResponse, String> {
        match classify(text).map_err(|error| error.to_string())? {
            JoinText::Invitation(invitation) => self.accept_peer_invitation(local, &invitation),
            JoinText::Site { entry, room } => {
                let joined = self
                    .join_site_now(&entry)
                    .map_err(|error| error.to_string())?;
                let (SiteJoin::Held { room: held, .. } | SiteJoin::Enrolling { room: held, .. }) =
                    joined.clone();
                if room.is_some_and(|room| room != held) {
                    return Err(
                        "That link is for another workspace on this site. Ask whoever shared it for an invitation."
                            .into(),
                    );
                }
                Ok(match joined {
                    SiteJoin::Held { room } => PeerSyncResponse::Message(room.to_string()),
                    SiteJoin::Enrolling { enrollment, .. } => {
                        PeerSyncResponse::EnrollmentPending { id: enrollment }
                    }
                })
            }
            JoinText::InvitationLink {
                origin,
                room,
                invitation,
            } => self.accept_invitation_link(&origin, room, &invitation),
        }
    }

    /// Join the workspace a link opened on this computer names, as its
    /// person, and answer its room once this computer holds it. Blocks until
    /// then, at most [`LINK_JOIN_WAIT`]: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why it was not joined, in the person's words.
    pub(crate) fn join_link(&self, url: &str) -> Result<RoomId, String> {
        let room = match classify(url).map_err(|error| error.to_string())? {
            JoinText::Invitation(_) => return Err("that is not a workspace link".into()),
            JoinText::Site { entry, room } => {
                let joined = self.join_site(&entry).map_err(|error| error.to_string())?;
                let (SiteJoin::Held { room: held, .. } | SiteJoin::Enrolling { room: held, .. }) =
                    joined;
                if room.is_some_and(|room| room != held) {
                    return Err(
                        "That link is for another workspace on this site. Ask whoever shared it for an invitation."
                            .into(),
                    );
                }
                held
            }
            JoinText::InvitationLink {
                origin,
                room,
                invitation,
            } => {
                if let PeerSyncResponse::EnrollmentPending { id } =
                    self.accept_invitation_link(&origin, room, &invitation)?
                {
                    self.finish_when_enrolled(room, id, true)?;
                }
                room
            }
        };
        let deadline = std::time::Instant::now()
            .checked_add(LINK_JOIN_WAIT)
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let held = self.replica.lock().is_ok_and(|replica| {
                replica
                    .tree
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.room == room)
            });
            if held {
                return Ok(room);
            }
            if std::time::Instant::now() >= deadline {
                return Err("The workspace did not open in time. Try the link again.".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Redeem `invitation` to `room` at the host at `origin` over HTTPS, into
    /// a new workspace named as the host names the room.
    fn accept_invitation_link(
        &self,
        origin: &str,
        room: RoomId,
        invitation: &str,
    ) -> Result<PeerSyncResponse, String> {
        let tree = self
            .replica
            .lock()
            .map_err(|_| "the workspaces are unavailable")?
            .tree
            .clone();
        if tree
            .read()
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.room == room)
        {
            return Ok(PeerSyncResponse::Message(room.to_string()));
        }
        let http = HostHttp::new(origin)?;
        let runtime = self.runtime_handle()?;
        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            invitation,
        )
        .map_err(|_| "That invitation is damaged. Ask for a new link.")?;
        let name = match runtime.block_on(tilo_relay_client::https::join(
            http.as_ref(),
            &tilo_relay_protocol::rooms::JoinStep::Inspect { invitation: bytes },
        )) {
            Ok(tilo_relay_protocol::rooms::JoinReply::Invitation {
                name, room: named, ..
            }) if RoomId::parse(&named) == Some(room) => name,
            Ok(tilo_relay_protocol::rooms::JoinReply::Refused(reason)) => {
                return Err(format!("The invitation was not accepted: {reason}"));
            }
            Ok(_) => return Err("That invitation is for another workspace.".into()),
            Err(error) => {
                return Err(format!(
                    "The workspace's host could not be reached: {error}"
                ));
            }
        };
        let host = runtime
            .block_on(tilo_relay_client::https::host_info(http.as_ref()))
            .map_err(|error| error.to_string())?
            .host_cert
            .device_public_key;
        self.hosted_sites.remember(
            room,
            HostedRoom {
                origin: origin.to_owned(),
                site: None,
                host: Some(hex::encode(host)),
                app: None,
            },
        )?;
        let local = tree.write().append_empty_workspace(&name);
        let via = Via::Https { http, door: None };
        self.accept_invitation_via(local, invitation, via)
            .inspect_err(|_| {
                tree.write().delete_workspace(local);
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_tells_sites_links_and_invitations_apart() {
        assert_eq!(
            classify("tilo.coffee"),
            Ok(JoinText::Site {
                entry: url::Url::parse("https://tilo.coffee/").unwrap(),
                room: None
            })
        );
        let room = RoomId::new();
        assert_eq!(
            classify(&format!(
                "https://tilo.coffee/#/@eu1.tilo.coffee/w/{room}/so.tilo.chat"
            )),
            Ok(JoinText::Site {
                entry: url::Url::parse("https://tilo.coffee/").unwrap(),
                room: Some(room)
            })
        );
        assert_eq!(
            classify(&format!(
                "https://app.example/app/#/@rooms.example/w/{room}?invite=abc"
            )),
            Ok(JoinText::InvitationLink {
                origin: "https://rooms.example".into(),
                room,
                invitation: "abc".into(),
            })
        );
        assert_eq!(
            classify("dGhpcy1pcy1hLXRva2Vu"),
            Ok(JoinText::Invitation("dGhpcy1pcy1hLXRva2Vu".into()))
        );
        assert_eq!(classify("   "), Err(SiteError::Invalid));
        assert_eq!(classify("ftp://example.org"), Err(SiteError::Invalid));
    }
}
