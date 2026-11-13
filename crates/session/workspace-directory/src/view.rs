//! What the sheet shows: what the sites' directories list, which is the
//! workspaces the caller does not already hold.

use std::collections::BTreeSet;

use tilo_relay_protocol::directory::Activity;
use tilo_relay_protocol::workspaces::Role;
use tilo_sheets::directory::{Directory, Listing, ListingAction};

use crate::Listed;
use crate::store::Store;

/// A workspace the caller already holds: its id, which leaves it out of what
/// the sheet offers to join, and the key its desk knows it by and the name it
/// shows, which a caller that keeps no desk says itself (`Hold` is read from
/// a page's `mountDesktop({directory})`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Held {
    /// The workspace's id, as a listing names it.
    pub workspace: String,
    /// The key the caller's desk knows the workspace by.
    pub key: String,
    /// The name the caller's desk shows it under.
    pub name: String,
}

impl Store {
    /// What the sheet shows: what the sites' directories list, without the
    /// workspaces `held` holds.
    pub fn view(&self, held: &[Held]) -> Directory {
        let now_ms = tilo_clock::unix_now_ms();
        // The workspaces the caller already holds are not ones to join: the
        // sheet offers what the sites list, and someone who holds a
        // workspace reaches it from their own desk, not from here.
        let held_by: BTreeSet<&str> = held
            .iter()
            .map(|workspace| workspace.workspace.as_str())
            .collect();
        let joining = self.joining();
        let failures = self.failures.borrow().clone();
        let mut listings = Vec::new();
        for listing in crate::ordered(self.listed.borrow().clone(), now_ms) {
            if held_by.contains(listing.entry.workspace.as_str()) {
                continue;
            }
            let listed_key = listing.key();
            let failure = failures.get(&listed_key);
            let action = if joining.as_deref() == Some(listed_key.as_str()) {
                ListingAction::Joining
            } else if failure.is_some() {
                ListingAction::TryAgain
            } else {
                ListingAction::Join
            };
            listings.push(Listing {
                key: listed_key,
                name: listing.entry.name.clone(),
                about: if listing.entry.description.is_empty() {
                    listing.host.clone()
                } else {
                    listing.entry.description.clone()
                },
                host: listing.host.clone(),
                tag: listing.entry.community.then(|| {
                    tilo_sheets::words("directory-community-tag", &[("host", &listing.host)])
                }),
                activity: failure.map_or_else(
                    || activity_line(&listing, now_ms),
                    |reason| tilo_sheets::words("directory-join-failed-line", &[("error", reason)]),
                ),
                members: failure.map_or_else(|| members_line(&listing), |_| None),
                joins_as: joins_as(&listing),
                here: listing
                    .entry
                    .here
                    .iter()
                    .map(|face| (face.name.clone(), face.user_public_key.clone()))
                    .collect(),
                here_count: usize::try_from(listing.entry.here_count).unwrap_or(usize::MAX),
                action,
                waiting: joining.is_some() && !matches!(action, ListingAction::Joining),
            });
        }
        Directory {
            status: self.status.borrow().clone(),
            listings,
            note: self.note.borrow().clone(),
        }
    }
}

/// The words a role is shown with.
fn role_label(role: Role) -> String {
    tilo_sheets::word(match role {
        Role::Viewer => "directory-role-viewer",
        Role::Commenter => "directory-role-commenter",
        Role::Editor => "directory-role-editor",
        Role::Moderator => "directory-role-moderator",
        Role::Owner => "directory-role-owner",
    })
}

/// How lively a listing is: the row's secondary line.
fn activity_line(listing: &Listed, now_ms: u64) -> String {
    match listing.activity(now_ms) {
        Activity::HereNow(count) => {
            tilo_sheets::words("directory-here-now", &[("count", &count.to_string())])
        }
        Activity::ActiveToday => tilo_sheets::word("directory-active-today"),
        Activity::ActiveThisWeek => tilo_sheets::word("directory-active-week"),
        Activity::Quiet => tilo_sheets::word("directory-quiet"),
    }
}

/// How many belong to a listing, in the row's own column; `None` when it
/// does not say.
fn members_line(listing: &Listed) -> Option<String> {
    match listing.entry.members {
        0 => None,
        members => Some(tilo_sheets::words(
            "directory-members",
            &[("count", &members.to_string())],
        )),
    }
}

/// The role people who join `listing` get, as its button describes it.
fn joins_as(listing: &Listed) -> String {
    tilo_sheets::words(
        "directory-joins-as",
        &[("role", &role_label(listing.entry.default_role))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::directory::DirectoryEntry;

    fn entry(workspace: &str, name: &str) -> DirectoryEntry {
        DirectoryEntry {
            workspace: workspace.into(),
            name: name.into(),
            description: String::new(),
            default_role: Role::Commenter,
            door: format!("directory/{workspace}/"),
            here: Vec::new(),
            here_count: 0,
            members: 12,
            last_active_ms: None,
            community: false,
        }
    }

    fn listed(entry: DirectoryEntry) -> Listed {
        Listed {
            entry,
            door: None,
            site: None,
            host: "eu1.tilo.sh".into(),
        }
    }

    #[test]
    fn who_is_here_and_how_many_belong_are_two_facts() {
        let mut busy = entry("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71", "Busy");
        busy.here_count = 3;
        busy.members = 12;
        let listed = listed(busy);
        let activity = activity_line(&listed, 0);
        assert!(activity.contains('3'), "{activity}");
        assert!(!activity.contains("12"), "{activity}");
        let members = members_line(&listed).expect("a listing with members");
        assert!(members.contains("12"), "{members}");
    }

    #[test]
    fn a_workspace_the_caller_holds_is_not_one_to_join() {
        let store = Store::default();
        *store.listed.borrow_mut() = vec![listed(entry(
            "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71",
            "Coffee",
        ))];
        let held = [Held {
            workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
            key: "slot-1".into(),
            name: "Coffee".into(),
        }];
        let shown = store.view(&held);
        assert!(
            shown.listings.is_empty(),
            "a site listing the caller's own workspace does not offer it to join"
        );
    }

    #[test]
    fn a_workspace_no_site_lists_is_left_to_the_callers_own_desk() {
        let store = Store::default();
        let held = [
            Held {
                workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
                key: "slot-1".into(),
                name: "Coffee".into(),
            },
            Held {
                workspace: "0a8844c0-5d74-4b3c-8e3a-2d2ba52f0c11".into(),
                key: "slot-2".into(),
                name: "My notes".into(),
            },
        ];
        let shown = store.view(&held);
        assert!(
            shown.listings.is_empty(),
            "the sheet offers only what can be joined, never what the caller has"
        );
    }
}
