//! Shell-action provider: palette rows exposed through the provider
//! protocol, under the same chip strip + grouping the notes/files providers
//! use.
//!
//! Most rows are nullary — "toggle the overview" needs nothing but the
//! instruction. [`Action::params`] lets one take a typed value instead,
//! for an action whose input (the text to insert, say) isn't discoverable
//! from the chrome and has to be supplied and declared, so enumeration
//! tells a caller what to pass rather than leaving it to be inferred.

use std::rc::Rc;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

mod text_insert;

/// What `record.bring` must be told.
const RECORD_BRING_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "id",
        ParamType::Text,
        "The record's id — `file:<absolute path>`, `tab:<id>`, or a minted id.",
    ),
    ActionParam::optional(
        "kind",
        ParamType::Text,
        "The record's type, in the owning app's vocabulary. Defaults to `document`.",
    ),
];

/// What `agent.invite` may be told. Optional, because `qos-agent` is
/// launched with `--name`, defaulting to `Claude`
/// (`apps/agent/src/main.rs`); a differently-named session passes it.
const AGENT_INVITE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "The agent's name, as it was launched with. Defaults to `Claude`.",
)];

/// The agent this OS ships. Kept in step with `qos-agent`'s own default by
/// being the same string, not by being imported — the shell does not
/// depend on the agent binary.
const DEFAULT_AGENT_NAME: &str = "Claude";

/// What `peer.invite.named` must be told. Both required — an invite to
/// nobody in particular is [`peer.invite`].
const PEER_INVITE_NAMED_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "peer",
        ParamType::Text,
        "The invitee's Iroh `EndpointId` — the device identity the name is attached to.",
    ),
    ActionParam::required(
        "name",
        ParamType::Text,
        "What to call them in this room. Shown beside their cursor and caret.",
    ),
];

/// One action row. `id` is the palette command id (`overview.toggle`,
/// `settings.open`, …); `apply` runs it when the user picks the row (or an
/// agent invokes its address), taking the bound arguments and the chrome —
/// a nullary toggle simply ignores what it doesn't use.
#[derive(Clone)]
struct Action {
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    /// Empty for everything that only needs to be told to happen.
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
            "projection.open",
            "Project to a display",
            "Miracast, Google Cast, AirPlay, or FCast",
            |st, _| {
                st.overlays.projection = true;
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
                // The sheet covers the dropdown that can also open it.
                st.overlays.collaborators = false;
                Ok(())
            },
        ),
        // Ranked like any other action, not appended after the hits — an
        // un-ranked row would lose "invite" to a file with the word in its name.
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
        // The name must come from the inviter, not the invitee: a peer
        // announcing its own name could claim any name and have it render
        // next to a caret this room's capabilities gate. Recorded by the
        // inviter into the room document, the same place capability grants
        // live; the peer id stays the identity, the name is just a label.
        Action {
            id: "peer.invite.named",
            label: "Invite peer to workspace as",
            subtitle: "Name the peer you are inviting, then mint their invite token",
            params: PEER_INVITE_NAMED_PARAMS,
            apply: |st, args| {
                let peer = args.text("peer").unwrap_or_default().trim().to_string();
                let name = args.text("name").unwrap_or_default().trim().to_string();
                if peer.is_empty() {
                    return Err(ActionError::Refused(
                        "an invite has to name the peer it is for".into(),
                    ));
                }
                if name.is_empty() {
                    return Err(ActionError::Refused(
                        "an invite has to say what to call them".into(),
                    ));
                }
                let user = st.local_user_id.clone();
                // Scoped so the write lock releases before anything else is touched.
                let named = {
                    let mut tree = st.tree_mut();
                    let Some(room) = tree.active_workspace().map(|w| w.room) else {
                        return Err(ActionError::Refused(
                            "there is no room here to invite anyone into".into(),
                        ));
                    };
                    tree.name_peer(room, &user, &peer, &name)
                };
                if !named {
                    return Err(ActionError::Refused(format!(
                        "{user} cannot name a peer in this room — either it is not \
                         administered here, or `{peer}` is not a peer id"
                    )));
                }
                // Mint the token too, so naming and inviting are one
                // gesture; minted *for* them so the card names whose invite it is.
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                        name,
                        copy: false,
                    },
                );
                Ok(())
            },
        },
        // Alongside the peer rows because the OS treats an agent as an
        // ordinary participant, but not the same operation: a peer presents
        // a signed token; an agent is admitted by the room recording a
        // capability naming it — no token, no prompt, just the grant.
        Action {
            id: "agent.invite",
            label: "Invite agent to workspace",
            subtitle: "Admit an agent to this room as a member, revocable like any other",
            params: AGENT_INVITE_PARAMS,
            apply: |st, args| {
                use crate::shell::state::{CapOverride, Subject, Verb};
                let name = args.text("name").unwrap_or(DEFAULT_AGENT_NAME).trim();
                let name = if name.is_empty() {
                    DEFAULT_AGENT_NAME
                } else {
                    name
                };
                // Case-folded: `Scribe` and `scribe` are otherwise two
                // different subjects, half-admitting the agent.
                let agent = name.to_lowercase();
                let user = st.local_user_id.clone();
                let granted = {
                    let mut tree = st.tree_mut();
                    let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
                        return Err(ActionError::Refused(
                            "there is no room here to admit an agent to".into(),
                        ));
                    };
                    let room = ws_id.raw().to_string();
                    // An ordinary grant on the room, through the tree so it
                    // reaches the room document (and other members) and disk.
                    let cap =
                        CapOverride::membership(Subject::agent(agent.clone()), room, Verb::Write)
                            .with_id(format!("agent-member:{agent}"));
                    tree.grant_in_workspace(ws_id, &user, cap)
                };
                if granted.is_none() {
                    return Err(ActionError::Refused(format!(
                        "{user} does not administer this room, so {agent} was not admitted"
                    )));
                }
                // The roster is where a person reads "they're in the room" —
                // same move `peer.invite` makes putting its token on screen.
                st.overlays.collaborators = true;
                Ok(())
            },
        },
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
        // The first exported action that changes a document rather than
        // the chrome around one, and so the first told *what* to write.
        Action {
            id: text_insert::ACTION_ID,
            label: "Insert text into the focused note",
            subtitle: "Write into the focused Notes or Editor tab at a character offset",
            params: text_insert::PARAMS,
            apply: text_insert::apply,
        },
        // The palette does this on Enter over a record row; this is the
        // address for a caller that names a record itself.
        Action {
            id: "record.bring",
            label: "Bring a record into this room",
            subtitle: "Add a reference to this room's index. Moves no bytes.",
            params: RECORD_BRING_PARAMS,
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
                let kind = if kind.trim().is_empty() {
                    "document"
                } else {
                    kind
                };
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
    fn id(&self) -> &'static str {
        "actions"
    }
    fn name(&self) -> &'static str {
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
            // A session not running peer sync has nothing to invite into.
            if action.id.starts_with("peer.") && !ctx.chrome.peer_sync_enabled {
                continue;
            }
            // Needs a room to record the capability in and a working
            // broker (peer sync is not required — an agent uses this
            // session's own broker socket).
            if action.id.starts_with("agent.")
                && (!ctx.chrome.collab_available()
                    || ctx.chrome.tree().active_workspace().is_none())
            {
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
                        // Repaint only on success; a refusal changed nothing.
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

    /// A session with a room this machine administers — what `agent.invite` needs.
    fn chrome_with_room() -> crate::shell::chrome_state::Chrome {
        use crate::shell::state::{Workspace, WorkspaceId};
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut ws = Workspace::test_default(id, "Demo");
        ws.set_owner(crate::shell::state::local_user_id());
        tree.workspaces.push(ws);
        tree.active = id;
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(tree));
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
        // Peer-sync and agent rows included, so "everything" means the whole table.
        let c = chrome_with_room();
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

    /// Offered only by a session running peer sync, and when offered,
    /// naming one selects it: a label-prefix match outranks other providers.
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
        assert_eq!(connect.first().map(|r| r.id.as_str()), Some("peer.connect"),);
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
        let fire = |doc: &mut AutoCommit| {
            (dnd.on_open)(doc, &automerge::ROOT, &c, &ActionArgs::none())
                .expect("a nullary chrome action must run");
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

    /// Needs a room (the grant is recorded *in* one) and a working broker
    /// (how an agent reaches the room). Not peer sync — an agent uses its
    /// own broker socket.
    #[test]
    fn the_agent_row_is_offered_only_where_it_can_work() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let offered = |chrome: &ChromeState| {
            p.query(SearchContext {
                needle: "agent",
                wanted: &[ResourceKind::Action],
                chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
            .iter()
            .any(|r| r.id == "agent.invite")
        };

        let roomless = chrome_rc();
        assert!(
            !offered(&roomless.borrow()),
            "a session with no room has nowhere to record the grant, so it \
             must not offer to admit anyone",
        );

        let broker_down = chrome_with_room();
        broker_down
            .borrow_mut()
            .set_collab_unavailable("the broker could not start");
        assert!(
            !offered(&broker_down.borrow()),
            "an agent reaches the room through the broker; with none there \
             is nothing for the grant to admit",
        );

        let ready = chrome_with_room();
        assert!(
            offered(&ready.borrow()),
            "a session with a room and a working broker must offer the row",
        );
        assert!(
            !ready.borrow().peer_sync_enabled,
            "and must do so without peer sync, which this row does not need",
        );
    }

    /// Asserted through `agent_members`, the same reader the roster paints
    /// from, not a row in a vector this test put there.
    #[test]
    fn invoking_it_admits_the_agent_to_the_room() {
        let c = chrome_with_room();

        let before = c
            .borrow()
            .tree()
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert!(
            before.is_empty(),
            "nobody is admitted before the action runs"
        );

        let action = Action::ALL
            .iter()
            .find(|a| a.id == "agent.invite")
            .expect("the action is registered");
        (action.apply)(&mut c.borrow_mut(), &ActionArgs::none())
            .expect("the room's owner may admit an agent");

        let after = c
            .borrow()
            .tree()
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert_eq!(
            after,
            vec![(
                DEFAULT_AGENT_NAME.to_lowercase(),
                crate::shell::state::CapRole::Editor
            )],
            "the shipped agent is admitted under its own name, case-folded",
        );
        assert!(
            c.borrow().overlays.collaborators,
            "and the roster is shown, so the person who issued the \
             invitation can see that it took",
        );
    }

    /// A name typed in any case names one agent. Two spellings would be
    /// two subjects, which reads on screen as a grant that did not take.
    #[test]
    fn the_name_is_case_folded_into_one_principal() {
        let c = chrome_with_room();
        let action = Action::ALL
            .iter()
            .find(|a| a.id == "agent.invite")
            .expect("registered");
        for spelling in ["Scribe", "SCRIBE", "  scribe  "] {
            let args = ActionArgs::bind(
                AGENT_INVITE_PARAMS,
                [(
                    "name".to_string(),
                    crate::shell::search::action_api::ArgValue::Text(spelling.to_string()),
                )],
            )
            .expect("name is a declared text parameter");
            (action.apply)(&mut c.borrow_mut(), &args).expect("owner may admit");
        }
        let members = c
            .borrow()
            .tree()
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert_eq!(
            members.len(),
            1,
            "three spellings of one name must be one agent, got {members:?}",
        );
    }
}
