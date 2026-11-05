//! What the sheet shows: the workspaces the caller already holds, and what
//! the sites' directories list.

use std::collections::{BTreeMap, BTreeSet};

use tilo_relay_protocol::directory::Activity;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_sheets::directory::{Directory, Listing, ListingAction};

use crate::Listed;
use crate::store::Store;

/// A workspace the caller already holds: its id, the key its desk knows it
/// by, and the name it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The workspace's id, as a listing names it.
    pub workspace: String,
    /// The key the caller's desk knows the workspace by.
    pub key: String,
    /// The name the caller's desk shows it under.
    pub name: String,
}

impl Store {
    /// What the sheet shows: the workspaces `held` holds, and what the sites'
    /// directories list.
    pub fn view(&self, held: &[Held]) -> Directory {
        let now_ms = tilo_clock::unix_now_ms();
        // The workspaces the caller holds, by workspace: the listed ones it
        // holds are opened by its own key.
        let held_by: BTreeMap<&str, &Held> = held
            .iter()
            .map(|workspace| (workspace.workspace.as_str(), workspace))
            .collect();
        let joining = self.joining();
        let failures = self.failures.borrow().clone();
        let mut listings = Vec::new();
        let mut shown: BTreeSet<String> = BTreeSet::new();
        for listing in crate::ordered(self.listed.borrow().clone(), now_ms) {
            let listed_key = listing.key();
            let held_by = held_by.get(listing.entry.workspace.as_str()).copied();
            let key = match held_by {
                Some(held) => {
                    shown.insert(held.key.clone());
                    held.key.clone()
                }
                None => listed_key.clone(),
            };
            let failure = failures.get(&listed_key);
            let action = if held_by.is_some() {
                ListingAction::Open
            } else if joining.as_deref() == Some(listed_key.as_str()) {
                ListingAction::Joining
            } else if failure.is_some() {
                ListingAction::TryAgain
            } else {
                ListingAction::Join
            };
            listings.push(Listing {
                key,
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
                waiting: joining.is_some()
                    && !matches!(action, ListingAction::Open | ListingAction::Joining),
            });
        }
        for workspace in held {
            if shown.contains(&workspace.key) {
                continue;
            }
            listings.push(Listing {
                key: workspace.key.clone(),
                name: workspace.name.clone(),
                action: ListingAction::Open,
                ..Listing::default()
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
fn role_label(role: WorkspaceRole) -> String {
    tilo_sheets::word(match role {
        WorkspaceRole::Viewer => "directory-role-viewer",
        WorkspaceRole::Commenter => "directory-role-commenter",
        WorkspaceRole::Editor => "directory-role-editor",
        WorkspaceRole::Moderator => "directory-role-moderator",
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
            default_role: WorkspaceRole::Commenter,
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
            host: "eu1.tilo.coffee".into(),
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
    fn a_held_workspace_is_opened_by_the_key_the_caller_knows_it_by() {
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
        assert_eq!(shown.listings.len(), 1, "the held workspace is shown once");
        let row = &shown.listings[0];
        assert_eq!(row.key, "slot-1");
        assert_eq!(row.action, ListingAction::Open);
    }

    #[test]
    fn a_workspace_no_directory_lists_is_still_shown() {
        let store = Store::default();
        *store.listed.borrow_mut() = vec![listed(entry(
            "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71",
            "Coffee",
        ))];
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
        assert_eq!(shown.listings.len(), 2);
        assert_eq!(shown.listings[1].key, "slot-2");
        assert_eq!(shown.listings[1].name, "My notes");
    }
}
