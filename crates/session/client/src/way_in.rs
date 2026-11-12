//! Which way a device takes into the workspace a link names, before anything about the link's
//! target is read. Every way is the workspace's own door: a link grants nothing, and a failure to
//! get in reads the same whether the workspace is missing or private, so a link never tells a
//! stranger which workspaces exist.
//!
//! The device asks [`way_in`] with what the link carries and what it knows of the host, takes
//! that way, and asks [`after`] when it did not get in. Taking a way (asking a door, opening an
//! invitation, connecting) and what the person is shown are the device's own.

use serde::{Deserialize, Serialize};

/// What a link names.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Scope {
    /// The person's own desktop, which is the device's own and needs no host.
    Own,
    /// An invitation a site keeps, sealed, which names the workspace it admits into.
    Sealed,
    /// A workspace of a host, or (no `room`) the workspace its door admits to.
    Room {
        #[serde(default)]
        room: Option<String>,
    },
}

/// How a link's pairing secret read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pairing {
    /// It names a person (and maybe the invite their device made).
    Named,
    /// It is damaged: nothing is asked with it.
    Damaged,
}

/// What a link carries besides its scope.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub scope: Scope,
    /// Its secrets did not read: none is used.
    #[serde(default)]
    pub malformed: bool,
    /// An invitation (its text, or the handle and key of one its host keeps sealed).
    #[serde(default)]
    pub invite: bool,
    /// A workspace service guest ticket.
    #[serde(default)]
    pub ticket: bool,
    /// A pairing secret: this device becomes a new device of the person it names.
    #[serde(default)]
    pub pair: Option<Pairing>,
}

/// What the device knows of the host the link names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Here {
    /// The host's listed workspace has a door that admits people by itself.
    #[serde(default)]
    pub door: bool,
    /// This device entered the workspace the link names before.
    #[serde(default)]
    pub entered: bool,
    /// How many accounts the device holds for the host, once read (`None`: not read yet).
    #[serde(default)]
    pub accounts: Option<usize>,
    /// What the device itself can do.
    #[serde(flatten)]
    pub can: Can,
}

/// What a device can do on its way in, whatever the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Can {
    /// Redeem guest tickets (its web host names where).
    #[serde(default)]
    pub tickets: bool,
    /// Pair as a new device of a person.
    #[serde(default)]
    pub pairing: bool,
}

/// The way in, or why there is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Way {
    /// The person's own desktop, gated by its home's door where the home has one.
    OwnDesktop,
    /// Open the invitation the site keeps, then [`Self::Invitation`] at the host it names.
    SealedInvitation,
    /// Inspect the invitation, have the person confirm it, and redeem it.
    Invitation,
    /// Spend the guest ticket at the redemption door the web host names.
    Ticket,
    /// Pair as a new device of the person the link names, once one of their devices approves.
    Pair,
    /// Ask the host's door, held to the workspace the link names.
    Door,
    /// Connect as the member whose account the device holds for the host.
    Member,
    /// Come back with the person's root the device holds: the host decides whether they are in.
    Reconnect,
    /// Read the accounts the device holds for the host, and ask again.
    Accounts,
    /// The link's secrets are damaged.
    Malformed,
    /// The link's pairing secret is damaged.
    MalformedPairing,
    /// This device cannot redeem guest tickets.
    NoTickets,
    /// This device cannot pair.
    NoPairing,
    /// No access: the same for a missing workspace and a private one.
    NoAccess,
}

/// How a way that was taken failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failed {
    /// Connecting as the member was refused, or the member's device key did not open.
    Member,
    /// Coming back with the root got no one in.
    Reconnect,
}

/// The first way into the workspace `link` names, from what the device knows `here`.
#[must_use]
pub fn way_in(link: &Link, here: &Here) -> Way {
    if link.malformed {
        return Way::Malformed;
    }
    let room = match &link.scope {
        Scope::Own => return Way::OwnDesktop,
        Scope::Sealed => return Way::SealedInvitation,
        Scope::Room { room } => room.as_deref().filter(|room| !room.is_empty()),
    };
    if room.is_none() {
        // The workspace a bare entry names is the door's to say.
        return if here.door { Way::Door } else { Way::NoAccess };
    }
    if link.invite {
        return Way::Invitation;
    }
    if link.ticket {
        return if here.can.tickets {
            Way::Ticket
        } else {
            Way::NoTickets
        };
    }
    match link.pair {
        Some(_) if !here.can.pairing => return Way::NoPairing,
        Some(Pairing::Damaged) => return Way::MalformedPairing,
        Some(Pairing::Named) => return Way::Pair,
        None => {}
    }
    match here.accounts {
        None => Way::Accounts,
        Some(0) => without_account(here),
        Some(_) => Way::Member,
    }
}

/// A device without an account for the host: the door, unless it entered this workspace before
/// (the person's own, say), which it comes back to as the person first.
fn without_account(here: &Here) -> Way {
    if here.door && !here.entered {
        Way::Door
    } else {
        Way::Reconnect
    }
}

/// The way in after `failed`, from what the device knows `here`.
#[must_use]
pub fn after(failed: Failed, here: &Here) -> Way {
    match failed {
        Failed::Reconnect if here.door => Way::Door,
        // A host's refusal, or a key that did not open, are no access alike.
        Failed::Member | Failed::Reconnect => Way::NoAccess,
    }
}

/// Whether a device admitted to `admitted` got into the workspace the link `asked` for: a link
/// that names one is followed only there, never to another workspace of the same host.
#[must_use]
pub fn as_asked(asked: Option<&str>, admitted: Option<&str>) -> bool {
    match (asked.filter(|asked| !asked.is_empty()), admitted) {
        (Some(asked), Some(admitted)) => asked == admitted,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(room: Option<&str>) -> Link {
        Link {
            scope: Scope::Room {
                room: room.map(Into::into),
            },
            malformed: false,
            invite: false,
            ticket: false,
            pair: None,
        }
    }

    fn here(door: bool, entered: bool, accounts: usize) -> Here {
        Here {
            door,
            entered,
            accounts: Some(accounts),
            can: Can {
                tickets: true,
                pairing: true,
            },
        }
    }

    #[test]
    fn a_link_s_secrets_choose_the_way_before_any_account_is_read() {
        let unread = Here {
            accounts: None,
            ..here(true, false, 0)
        };
        let named = room(Some("r"));
        assert_eq!(
            way_in(
                &Link {
                    malformed: true,
                    invite: true,
                    ..named.clone()
                },
                &unread
            ),
            Way::Malformed
        );
        assert_eq!(
            way_in(
                &Link {
                    invite: true,
                    ..named.clone()
                },
                &unread
            ),
            Way::Invitation
        );
        assert_eq!(
            way_in(
                &Link {
                    ticket: true,
                    ..named.clone()
                },
                &unread
            ),
            Way::Ticket
        );
        assert_eq!(
            way_in(
                &Link {
                    ticket: true,
                    ..named.clone()
                },
                &Here {
                    can: Can {
                        tickets: false,
                        ..unread.can
                    },
                    ..unread
                }
            ),
            Way::NoTickets
        );
        let pair = |pairing| Link {
            pair: Some(pairing),
            ..named.clone()
        };
        assert_eq!(way_in(&pair(Pairing::Named), &unread), Way::Pair);
        assert_eq!(
            way_in(&pair(Pairing::Damaged), &unread),
            Way::MalformedPairing
        );
        assert_eq!(
            way_in(
                &pair(Pairing::Named),
                &Here {
                    can: Can {
                        pairing: false,
                        ..unread.can
                    },
                    ..unread
                }
            ),
            Way::NoPairing
        );
        assert_eq!(
            way_in(&named, &unread),
            Way::Accounts,
            "only now are accounts read"
        );
    }

    #[test]
    fn the_own_desktop_a_sealed_link_and_a_bare_entry_have_one_way_each() {
        let own = Link {
            scope: Scope::Own,
            ..room(None)
        };
        assert_eq!(way_in(&own, &Here::default()), Way::OwnDesktop);
        let sealed = Link {
            scope: Scope::Sealed,
            invite: true,
            ..room(None)
        };
        assert_eq!(way_in(&sealed, &Here::default()), Way::SealedInvitation);
        assert_eq!(
            way_in(&room(None), &here(true, false, 1)),
            Way::Door,
            "the door names the workspace"
        );
        assert_eq!(
            way_in(
                &Link {
                    invite: true,
                    ..room(None)
                },
                &here(false, false, 1)
            ),
            Way::NoAccess,
            "a bare entry has no way in but its host's door"
        );
    }

    #[test]
    fn a_member_connects_and_anyone_else_comes_back_as_the_person_or_asks_the_door() {
        let named = room(Some("r"));
        assert_eq!(way_in(&named, &here(true, false, 1)), Way::Member);
        assert_eq!(way_in(&named, &here(true, false, 0)), Way::Door);
        assert_eq!(
            way_in(&named, &here(true, true, 0)),
            Way::Reconnect,
            "entered before: as the person first"
        );
        assert_eq!(way_in(&named, &here(false, false, 0)), Way::Reconnect);
    }

    #[test]
    fn a_failed_way_ends_in_the_door_or_no_access() {
        assert_eq!(after(Failed::Member, &here(true, false, 1)), Way::NoAccess);
        assert_eq!(after(Failed::Reconnect, &here(true, true, 0)), Way::Door);
        assert_eq!(
            after(Failed::Reconnect, &here(false, false, 0)),
            Way::NoAccess
        );
    }

    #[test]
    fn a_link_is_followed_only_into_the_workspace_it_names() {
        assert!(as_asked(Some("r"), Some("r")));
        assert!(!as_asked(Some("r"), Some("other")));
        assert!(
            as_asked(None, Some("other")),
            "a bare entry goes where the door admits"
        );
        assert!(as_asked(Some("r"), None));
    }
}
