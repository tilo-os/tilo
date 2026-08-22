//! Shell-action provider: the palette rows that used to be seeded
//! statically as `CommandEntry`s in `ShellOverlayHost::new`. Migrating
//! them into the provider protocol keeps them under the same chip strip
//! + grouping the notes / files providers use.
//!
//! # Parameters
//!
//! Most of these rows are nullary — "toggle the overview" needs nothing
//! but the instruction. [`Action::params`] is what lets one of them take
//! a typed value instead, which is what an action that edits a document
//! needs: the text to insert is not discoverable from the chrome, it has
//! to be supplied. The parameters are declared on the row, so an
//! enumeration tells a caller what to pass rather than leaving it to be
//! inferred from a label.

use std::rc::Rc;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

mod text_insert;

/// One action row. `id` is the palette command id (`overview.toggle`,
/// `settings.open`, …); `apply` performs the action when the user picks
/// the row (or an agent invokes its address).
///
/// `apply` takes the arguments bound against `params`, and it takes the
/// live document alongside the chrome — an action that writes to a
/// record's document needs both, and splitting the two shapes would mean
/// two dispatch paths for one vocabulary. A nullary chrome toggle simply
/// ignores what it does not use.
#[derive(Clone)]
struct Action {
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    /// What an invocation must carry. Empty for everything that only
    /// needs to be told to happen.
    params: &'static [ActionParam],
    apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
}

/// A chrome toggle: no parameters, and nothing that can fail.
const fn chrome(
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
) -> Action {
    Action {
        id,
        label,
        subtitle,
        params: &[],
        apply,
    }
}

impl Action {
    const ALL: &'static [Action] = &[
        chrome(
            "overview.toggle",
            "Toggle workspace overview",
            "Show all workspaces in the wheel view",
            |st, _| {
                st.overlays.overview = !st.overlays.overview;
                Ok(())
            },
        ),
        chrome(
            "settings.open",
            "Open workspace settings",
            "Identity, members, permissions",
            |st, _| {
                st.overlays.ws_settings = true;
                Ok(())
            },
        ),
        chrome(
            "quick-settings.open",
            "Open quick settings",
            "Brightness, volume, Wi-Fi, Bluetooth, power profile",
            |st, _| {
                st.overlays.quick_settings = true;
                Ok(())
            },
        ),
        chrome(
            "collaborators.open",
            "Show collaborators",
            "Peers currently editing this workspace",
            |st, _| {
                st.overlays.collaborators = true;
                Ok(())
            },
        ),
        chrome(
            "room.activity.open",
            "Show room activity",
            "History of this workspace: changes, permissions, who did what and when",
            |st, _| {
                st.overlays.room_activity = true;
                // The sheet covers the dropdown that can also open it; leaving
                // both up would put a menu on top of the surface it opened.
                st.overlays.collaborators = false;
                Ok(())
            },
        ),
        // Peer sync. These live here rather than being appended to the
        // palette's entry list after the ranked hits, which is where
        // they were: an un-ranked row sits below every search result,
        // so typing "invite" put a file whose name contains the word on
        // the row Enter activates and the invite command was
        // unreachable from the palette it was listed in.
        chrome(
            "peer.invite",
            "Invite peer to workspace",
            "Mint a signed invite token for this room",
            |st, _| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::IssueInvite);
                Ok(())
            },
        ),
        chrome(
            "peer.connect",
            "Connect to peer workspace",
            "Join a room from an invite token",
            |st, _| {
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::PromptConnect,
                );
                Ok(())
            },
        ),
        // Making a window. Before these existed the palette could open
        // every overlay the shell has and not one new tab, so the only
        // route to a second terminal was to find the app again — and
        // there was no route at all once the last one was closed.
        chrome(
            "tab.new.terminal.here",
            "New terminal tab here",
            "Open a terminal as a tab in the focused window group",
            |st, _| {
                crate::shell::window_actions::open_terminal(
                    st,
                    crate::shell::window_actions::Destination::Here,
                );
                Ok(())
            },
        ),
        chrome(
            "window.new.terminal",
            "New terminal window",
            "Open a terminal beside the focused window",
            |st, _| {
                crate::shell::window_actions::open_terminal(
                    st,
                    crate::shell::window_actions::Destination::Beside,
                );
                Ok(())
            },
        ),
        chrome(
            "tab.duplicate",
            "Duplicate this tab",
            "Open a second instance of the focused app beside it",
            |st, _| {
                if let Err(err) = crate::shell::window_actions::duplicate_active_tab(st) {
                    st.push_warning_notification("Can't duplicate this tab", err.user_message());
                }
                Ok(())
            },
        ),
        chrome(
            "notifications.dnd.toggle",
            "Toggle Do Not Disturb",
            "Silence notification banners; held notifications go to history",
            |st, _| {
                st.toggle_dnd();
                Ok(())
            },
        ),
        // Writing into the focused document. The first exported action
        // that changes a document rather than the chrome around one, and
        // so the first that has to be told *what* to write.
        Action {
            id: text_insert::ACTION_ID,
            label: "Insert text into the focused note",
            subtitle: "Write into the focused Notes or Editor tab at a character offset",
            params: text_insert::PARAMS,
            apply: text_insert::apply,
        },
        // Bringing a record into the room you are in. The palette does
        // this on Enter over a record row, which is the gesture the
        // design picks (`plan/rooms.md`: "bringing your own is a search
        // result") — but that row only exists once something already
        // knows the record, so it is not an address a caller can reach
        // for a record it names itself. This is that address.
        Action {
            id: "record.bring",
            label: "Bring a record into this room",
            subtitle: "Add a reference to this room's index. Moves no bytes.",
            params: &[
                ActionParam::required(
                    "id",
                    ParamType::Text,
                    "The record's id — `file:<absolute path>`, `tab:<id>`, or a \
                     minted id.",
                ),
                ActionParam::optional(
                    "kind",
                    ParamType::Text,
                    "The record's type, in the owning app's vocabulary. Defaults \
                     to `document`.",
                ),
            ],
            apply: |st, args| {
                let id = args.text("id").unwrap_or_default();
                if id.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "a record id is required; nothing was brought here".into(),
                    ));
                }
                if ui::record::RecordId::parse(id).is_none() {
                    return Err(ActionError::Refused(format!(
                        "{id:?} is not a record id this build can read"
                    )));
                }
                let kind = args.text("kind").unwrap_or("document");
                let kind = if kind.trim().is_empty() { "document" } else { kind };
                st.push_record_request(id, kind);
                Ok(())
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
                    params: action.params.to_vec(),
                    on_open: Rc::new(move |_doc, _root, chrome: &Chrome, args: &ActionArgs| {
                        let mut st = chrome.borrow_mut();
                        let outcome = apply(&mut st, args);
                        // Repaint only on success. A refusal changed
                        // nothing, and bumping the revision for it would
                        // tell a watching harness that something moved.
                        if outcome.is_ok() {
                            st.bump();
                        }
                        outcome
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
        (action.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none())
            .expect("a nullary chrome action must run");
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
        let fire = |doc: &mut AutoCommit| {
            (dnd.on_open)(doc, &automerge::ROOT, &c, &ActionArgs::none())
                .expect("a nullary chrome action must run")
        };
        fire(&mut doc);
        assert!(c.borrow().dnd(), "opening the action turns DND on");
        fire(&mut doc);
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
