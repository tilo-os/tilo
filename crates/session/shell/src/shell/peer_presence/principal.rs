//! The one seam where a remote session becomes someone in presence.

use super::PeerLabels;

/// Who a remote session is wherever its presence shows: a pointer, a caret,
/// a roster row. Every way presence arrives — pointer frames, app presence,
/// the live plane — attributes through [`resolve_presence_principal`], so
/// each one names a session the same way and none can take an identity or
/// a name from what its sender said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresencePrincipal {
    /// [`presence_id`]: the session the transport authenticated.
    pub id: String,
    /// What the room calls it, when it says.
    pub name: Option<String>,
}

/// The id a remote session's presence is filed under, `peer@<handle>`,
/// spelled from the handle its frames were bound to on arrival.
#[must_use]
pub fn presence_id(peer: u64) -> String {
    format!("peer@{peer}")
}

/// Session `peer` as [`PresencePrincipal`]: its [`presence_id`], named by
/// `labels`, the room's names for its peers
/// ([`crate::shell::network::ShellNetwork::peer_labels`]).
#[must_use]
pub fn resolve_presence_principal(labels: &PeerLabels, peer: u64) -> PresencePrincipal {
    PresencePrincipal {
        id: presence_id(peer),
        name: labels.get(&peer).cloned(),
    }
}

/// The role session `peer` holds in `ws`: the room's role for the person
/// the room vouches for at that session (`principals`, from
/// [`crate::shell::state::WorkspaceTree::room_session_principals`]), and
/// none for a session it vouches for nobody at. A role is authority, so it
/// is never asked of a name: a session's name may be one its person chose,
/// and anyone can choose the owner's.
#[must_use]
pub fn certified_role(
    ws: &crate::shell::state::Workspace,
    principals: &std::collections::BTreeMap<u64, crate::shell::state::UserId>,
    peer: u64,
) -> Option<crate::shell::state::CapRole> {
    ws.role_of(principals.get(&peer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_its_handle_and_the_rooms_name_for_it() {
        let labels = PeerLabels::from([(7, "Ada".to_string())]);
        assert_eq!(
            resolve_presence_principal(&labels, 7),
            PresencePrincipal {
                id: "peer@7".into(),
                name: Some("Ada".into()),
            },
        );
        assert_eq!(
            resolve_presence_principal(&labels, 8),
            PresencePrincipal {
                id: "peer@8".into(),
                name: None,
            },
            "a session the room has not named is only its id",
        );
    }
}
