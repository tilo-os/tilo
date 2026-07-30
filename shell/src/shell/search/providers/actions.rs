//! Shell-action provider: the palette rows that used to be seeded
//! statically as `CommandEntry`s in `ShellOverlayHost::new`. Migrating
//! them into the provider protocol keeps them under the same chip strip
//! + grouping the notes / files providers use.

use std::rc::Rc;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// One action row. `id` is the palette command id (`overview.toggle`,
/// `settings.open`, …); `apply` toggles the matching chrome flag when
/// the user picks the row.
#[derive(Clone)]
struct Action {
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    apply: fn(&mut ChromeState),
}

impl Action {
    const ALL: &'static [Action] = &[
        Action {
            id: "overview.toggle",
            label: "Toggle workspace overview",
            subtitle: "Show all workspaces in the wheel view",
            apply: |st| {
                st.overlays.overview = !st.overlays.overview;
            },
        },
        Action {
            id: "settings.open",
            label: "Open workspace settings",
            subtitle: "Identity, members, permissions",
            apply: |st| {
                st.overlays.ws_settings = true;
            },
        },
        Action {
            id: "quick-settings.open",
            label: "Open quick settings",
            subtitle: "Brightness, volume, Wi-Fi, Bluetooth, power profile",
            apply: |st| {
                st.overlays.quick_settings = true;
            },
        },
        Action {
            id: "collaborators.open",
            label: "Show collaborators",
            subtitle: "Peers currently editing this workspace",
            apply: |st| {
                st.overlays.collaborators = true;
            },
        },
        Action {
            id: "room.activity.open",
            label: "Show room activity",
            subtitle: "History of this workspace: changes, permissions, who did what and when",
            apply: |st| {
                st.overlays.room_activity = true;
                // The sheet covers the dropdown that can also open it; leaving
                // both up would put a menu on top of the surface it opened.
                st.overlays.collaborators = false;
            },
        },
        // Peer sync. These live here rather than being appended to the
        // palette's entry list after the ranked hits, which is where
        // they were: an un-ranked row sits below every search result,
        // so typing "invite" put a file whose name contains the word on
        // the row Enter activates and the invite command was
        // unreachable from the palette it was listed in.
        Action {
            id: "peer.invite",
            label: "Invite peer to workspace",
            subtitle: "Mint a signed invite token for this room",
            apply: |st| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::IssueInvite);
            },
        },
        Action {
            id: "peer.connect",
            label: "Connect to peer workspace",
            subtitle: "Join a room from an invite token",
            apply: |st| {
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::PromptConnect,
                );
            },
        },
        Action {
            id: "notifications.dnd.toggle",
            label: "Toggle Do Not Disturb",
            subtitle: "Silence notification banners; held notifications go to history",
            apply: |st| {
                st.toggle_dnd();
            },
        },
    ];
}

pub struct ActionsProvider;

impl ActionsProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ActionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for ActionsProvider {
    fn id(&self) -> &str {
        "actions"
    }
    fn name(&self) -> &str {
        "Actions"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let needle_lower = ctx.needle.to_lowercase();
        let mut out = Vec::new();
        for action in Action::ALL {
            // A session not running peer sync has nothing to invite
            // anyone into, so it does not offer to.
            if action.id.starts_with("peer.") && !ctx.chrome.peer_sync_enabled {
                continue;
            }
            let label_lower = action.label.to_lowercase();
            let subtitle_lower = action.subtitle.to_lowercase();
            let score = match_score(&needle_lower, &label_lower, &subtitle_lower);
            if let Some(score) = score {
                let apply = action.apply;
                out.push(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: self.id().to_string(),
                    id: action.id.to_string(),
                    title: action.label.to_string(),
                    subtitle: Some(action.subtitle.to_string()),
                    score,
                    on_open: Rc::new(move |_doc, _root, chrome: &Chrome| {
                        let mut st = chrome.borrow_mut();
                        apply(&mut st);
                        st.bump();
                    }),
                });
            }
        }
        out
    }
}

/// Case-insensitive scoring:
/// - Prefix match on label → 100
/// - Substring match on label → 50
/// - Substring match on subtitle → 25
/// - Fuzzy subsequence on label → 10
/// - Empty needle → 0 (matches everything, sort by kind order)
fn match_score(needle: &str, label: &str, subtitle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if label.starts_with(needle) {
        return Some(100);
    }
    if label.contains(needle) {
        return Some(50);
    }
    if subtitle.contains(needle) {
        return Some(25);
    }
    if fuzzy_subsequence(needle, label) {
        return Some(10);
    }
    None
}

fn fuzzy_subsequence(needle: &str, hay: &str) -> bool {
    let mut hi = hay.chars();
    'ch: for nc in needle.chars() {
        for hc in hi.by_ref() {
            if hc == nc {
                continue 'ch;
            }
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn chrome_rc() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    #[test]
    fn prefix_beats_substring_beats_subtitle() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "open",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        let opens: Vec<_> = out.iter().filter(|r| r.title.starts_with("Open")).collect();
        assert!(opens.iter().all(|r| r.score == 100));
    }

    #[test]
    fn empty_needle_returns_everything() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        // Every action, peer-sync included, so "everything" means the
        // whole table rather than the subset a default session offers.
        c.borrow_mut().peer_sync_enabled = true;
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert_eq!(out.len(), Action::ALL.len());
        assert!(out.iter().all(|r| r.score == 0));
    }

    /// The peer-sync rows are offered only by a session that is running
    /// peer sync — there is nothing to invite anyone into otherwise —
    /// and when they are offered, naming one selects it.
    ///
    /// The second half is the property that was missing: the rows used
    /// to be appended to the palette's entry list after the ranked
    /// search hits, so they sorted below every result and typing
    /// "invite" put a file whose name contained the word on the row
    /// Enter activates.
    #[test]
    fn peer_sync_rows_appear_only_with_peer_sync_and_lead_when_named() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let query = |chrome: &ChromeState, needle: &str| {
            p.query(SearchContext {
                needle,
                wanted: &[ResourceKind::Action],
                chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };

        let off = chrome_rc();
        {
            let cs = off.borrow();
            assert!(
                query(&cs, "invite").is_empty(),
                "a session without peer sync must not offer to invite anyone",
            );
        }

        let on = chrome_rc();
        on.borrow_mut().peer_sync_enabled = true;
        let cs = on.borrow();
        let mut hits = query(&cs, "invite");
        hits.sort_by_key(|r| -r.score);
        assert_eq!(
            hits.first().map(|r| r.id.as_str()),
            Some("peer.invite"),
            "typing \"invite\" must rank the invite command first, got {:?}",
            hits.iter().map(|r| (&r.id, r.score)).collect::<Vec<_>>(),
        );
        assert_eq!(
            hits[0].score, 100,
            "a label-prefix match is the top of this provider's scale, which \
             is what puts the row above hits from other providers",
        );
        let mut connect = query(&cs, "connect");
        connect.sort_by_key(|r| -r.score);
        assert_eq!(
            connect.first().map(|r| r.id.as_str()),
            Some("peer.connect"),
        );
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "open",
            wanted: &[ResourceKind::File],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn room_activity_action_opens_the_sheet_on_open() {
        // The palette is the canonical way to reach a shell surface, so the
        // action has to be findable by the words a person would type and its
        // callback has to flip the real flag the overlay host reads.
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = {
            let cs = c.borrow();
            p.query(SearchContext {
                needle: "activity",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let action = out
            .iter()
            .find(|r| r.id == "room.activity.open")
            .expect("typing 'activity' must surface the room-activity action");
        assert_eq!(action.title, "Show room activity");
        assert!(!c.borrow().overlays.room_activity, "starts closed");
        (action.on_open)(&mut doc, &automerge::ROOT, &c);
        assert!(
            c.borrow().overlays.room_activity,
            "invoking the exported action opened the sheet"
        );
        assert!(
            !c.borrow().overlays.collaborators,
            "the dropdown the sheet can also be opened from is dismissed with it"
        );
    }

    #[test]
    fn dnd_action_toggles_flag_on_open() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = {
            let cs = c.borrow();
            p.query(SearchContext {
                needle: "do not disturb",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let dnd = out
            .iter()
            .find(|r| r.id == "notifications.dnd.toggle")
            .expect("DND action must be present");
        assert!(!c.borrow().dnd(), "DND starts off");
        // Fire the real on_open closure — the production apply path.
        (dnd.on_open)(&mut doc, &automerge::ROOT, &c);
        assert!(c.borrow().dnd(), "opening the action turns DND on");
        (dnd.on_open)(&mut doc, &automerge::ROOT, &c);
        assert!(!c.borrow().dnd(), "opening it again turns DND back off");
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "wksttngs",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.iter().any(|r| r.id == "settings.open"));
    }
}
