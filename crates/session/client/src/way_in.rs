//! Which way a device takes into the workspace a link names, before anything about the link's
//! target is read. Every way is the workspace's own entry: a link grants nothing, and a failure to
//! get in reads the same whether the workspace is missing or private, so a link never tells a
//! stranger which workspaces exist.
//!
//! The device asks [`way_in`] with what the link carries and what it knows of the host, takes
//! that way, and asks [`after`] when it did not get in. Taking a way (asking an entry, opening an
//! invitation, connecting) and what the person is shown are the device's own.

use serde::{Deserialize, Serialize};

/// What a link names.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Scope {
    /// The person's own desktop, which is the device's own and needs no host.
    Own,
    /// A workspace of a host, or (no `workspace`) the workspace its entry admits to.
    Workspace {
        #[serde(default)]
        workspace: Option<String>,
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
    /// An invitation (its text, or the secret of one the workspace keeps at its host).
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
    /// The host's listed workspace has an entry that admits people by itself.
    #[serde(default)]
    pub entry: bool,
    /// The workspace the link names (or the listed one, for a link naming none) has an entry
    /// that lets anyone in: the site's listed workspace, or one its server's directory lists as
    /// open to everyone.
    #[serde(default)]
    pub listed: bool,
    /// What the device holds of the person and the workspace.
    #[serde(flatten)]
    pub held: Held,
    /// How many accounts the device holds for the host, once read (`None`: not read yet).
    #[serde(default)]
    pub accounts: Option<usize>,
    /// What the device itself can do.
    #[serde(flatten)]
    pub can: Can,
}

/// What a device holds of its person and the workspace the link names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Held {
    /// A person: someone is signed in on it.
    #[serde(default)]
    pub person: bool,
    /// It entered the workspace the link names before.
    #[serde(default)]
    pub entered: bool,
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
    /// The person's own desktop, guarded by its home's entry where the home has one.
    OwnDesktop,
    /// Inspect the invitation, have the person confirm it, and redeem it.
    Invitation,
    /// Spend the guest ticket at the redemption entry the web host names.
    Ticket,
    /// Pair as a new device of the person the link names, once one of their devices approves.
    Pair,
    /// Ask the host's entry, held to the workspace the link names.
    Entry,
    /// Connect as the member whose account the device holds for the host.
    Member,
    /// Come back with the person's root the device holds: the host decides whether they are in.
    Reconnect,
    /// Nobody is signed in on the device, and no entry lets a stranger into the workspace the
    /// link names: the person signs in first, and the device then comes back as them.
    SignIn,
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
    let workspace = match &link.scope {
        Scope::Own => return Way::OwnDesktop,
        Scope::Workspace { workspace } => workspace
            .as_deref()
            .filter(|workspace| !workspace.is_empty()),
    };
    if workspace.is_none() {
        // The workspace a bare entry names is the entry's to say.
        return if here.entry {
            Way::Entry
        } else {
            Way::NoAccess
        };
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

/// A device without an account there: the workspace's own entry where it lets anyone in,
/// unless the device entered this workspace before (the person's own, say). Any other workspace
/// it comes back to as the person it holds, and the host decides; with nobody signed in, the
/// person signs in first rather than arrive as a stranger no entry lets in.
fn without_account(here: &Here) -> Way {
    if here.listed && !here.held.entered {
        Way::Entry
    } else if here.held.person || here.held.entered {
        Way::Reconnect
    } else {
        Way::SignIn
    }
}

/// The way in after `failed`, from what the device knows `here`.
#[must_use]
pub fn after(failed: Failed, here: &Here) -> Way {
    match failed {
        Failed::Reconnect if here.entry => Way::Entry,
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

    fn at(workspace: Option<&str>) -> Link {
        Link {
            scope: Scope::Workspace {
                workspace: workspace.map(Into::into),
            },
            malformed: false,
            invite: false,
            ticket: false,
            pair: None,
        }
    }

    fn here(entry: bool, entered: bool, accounts: usize) -> Here {
        Here {
            entry,
            // The link names the workspace that entry is for.
            listed: entry,
            held: Held {
                person: true,
                entered,
            },
            accounts: Some(accounts),
            can: Can {
                tickets: true,
                pairing: true,
            },
        }
    }

    tilo_test_env::scenario! {
        fn the_way_in_is_chosen_from_the_link_and_who_is_here;

        phase a_link_s_secrets_choose_the_way_before_any_account_is_read;
        {
            let unread = Here {
                accounts: None,
                ..here(true, false, 0)
            };
            let named = at(Some("r"));
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

        phase the_own_desktop_and_a_bare_entry_have_one_way_each;
        {
            let own = Link {
                scope: Scope::Own,
                ..at(None)
            };
            assert_eq!(way_in(&own, &Here::default()), Way::OwnDesktop);
            assert_eq!(
                way_in(&at(None), &here(true, false, 1)),
                Way::Entry,
                "the entry names the workspace"
            );
            assert_eq!(
                way_in(
                    &Link {
                        invite: true,
                        ..at(None)
                    },
                    &here(false, false, 1)
                ),
                Way::NoAccess,
                "a bare entry has no way in but its host's entry"
            );
        }

        phase a_member_connects_and_anyone_else_comes_back_as_the_person_or_asks_the_entry;
        {
            let named = at(Some("r"));
            assert_eq!(way_in(&named, &here(true, false, 1)), Way::Member);
            assert_eq!(way_in(&named, &here(true, false, 0)), Way::Entry);
            assert_eq!(
                way_in(&named, &here(true, true, 0)),
                Way::Reconnect,
                "entered before: as the person first"
            );
            assert_eq!(way_in(&named, &here(false, false, 0)), Way::Reconnect);
        }

        phase nobody_signed_in_signs_in_before_any_workspace_but_a_public_one;
        {
            let named = at(Some("r"));
            let signed_out = |entry, listed| Here {
                listed,
                held: Held::default(),
                ..here(entry, false, 0)
            };
            // The listed workspace's entry lets a stranger in.
            assert_eq!(way_in(&named, &signed_out(true, true)), Way::Entry);
            assert_eq!(way_in(&at(None), &signed_out(true, false)), Way::Entry);
            // Another workspace of the host, or one at a host listing none: sign in first.
            assert_eq!(way_in(&named, &signed_out(true, false)), Way::SignIn);
            assert_eq!(way_in(&named, &signed_out(false, false)), Way::SignIn);
            // A public one, on a server the site lists nothing on: its own entry lets them in.
            assert_eq!(way_in(&named, &signed_out(false, true)), Way::Entry);
            // Signed in, the device comes back as the person, and the host decides; one the
            // host turns away still asks that workspace's entry.
            let signed_in = Here {
                listed: false,
                ..here(true, false, 0)
            };
            assert_eq!(way_in(&named, &signed_in), Way::Reconnect);
            assert_eq!(after(Failed::Reconnect, &signed_in), Way::Entry);
            // A device that entered before (a paired one holds no root) comes back as before.
            let entered = Here {
                held: Held {
                    person: false,
                    entered: true,
                },
                ..here(false, true, 0)
            };
            assert_eq!(way_in(&named, &entered), Way::Reconnect);
        }

        phase a_failed_way_ends_in_the_entry_or_no_access;
        {
            assert_eq!(after(Failed::Member, &here(true, false, 1)), Way::NoAccess);
            assert_eq!(after(Failed::Reconnect, &here(true, true, 0)), Way::Entry);
            assert_eq!(
                after(Failed::Reconnect, &here(false, false, 0)),
                Way::NoAccess
            );
        }

        phase a_link_is_followed_only_into_the_workspace_it_names;
        {
            assert!(as_asked(Some("r"), Some("r")));
            assert!(!as_asked(Some("r"), Some("other")));
            assert!(
                as_asked(None, Some("other")),
                "a bare entry goes where the entry admits"
            );
            assert!(as_asked(Some("r"), None));
        }
    }
}
