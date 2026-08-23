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

/// What `record.bring` must be told. Named rather than written inline so
/// it is one `&'static [ActionParam]` the catalogue can hand out, the
/// same shape `text_insert::PARAMS` has.
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

/// What `agent.invite` may be told.
///
/// Optional, because this OS ships an agent and it has a name already:
/// `qos-agent` is launched with `--name`, which defaults to `Claude`
/// (`apps/agent/src/main.rs`). A session running a differently-named one
/// passes it; the common case does not have to repeat what the system
/// already knows.
const AGENT_INVITE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "The agent's name, as it was launched with. Defaults to `Claude`.",
)];

/// The agent this OS ships, and so the one `agent.invite` admits when it
/// is not told otherwise. Kept in step with `qos-agent`'s own default by
/// being the same string for the same reason, not by being imported —
/// the shell does not depend on the agent binary.
const DEFAULT_AGENT_NAME: &str = "Claude";

/// What `peer.join` must be told: the token, and nothing else.
///
/// The token *is* the authorisation — a signed bearer credential naming
/// the room and when it stops being good. There is deliberately no room
/// parameter: a caller that could name the room it wanted to join would
/// be choosing its own destination, and the token would stop being the
/// thing that decides.
const PEER_JOIN_PARAMS: &[ActionParam] = &[ActionParam::required(
    "token",
    ParamType::Text,
    "The invite token you were sent. Whitespace is ignored, so a token that arrived \
     wrapped across lines in a chat message can be passed through as-is.",
)];

/// What `peer.invite.named` must be told: who is being invited, and
/// what to call them. Both required — an invite to nobody in particular
/// is [`peer.invite`], and this row exists precisely to say who.
const PEER_INVITE_NAMED_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "peer",
        ParamType::Text,
        "The invitee's libp2p `PeerId`, base58 — the identity the name is attached to.",
    ),
    ActionParam::required(
        "name",
        ParamType::Text,
        "What to call them in this room. Shown beside their cursor and caret.",
    ),
];

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
        // Joining, with the pointer taken out.
        //
        // `peer.connect` raises the card and stops. Everything after it
        // — paste, then Join — is a click, so a participant with no
        // pointer could not finish joining a room at all. That is not an
        // edge case here: this OS's claim is that an agent is an ordinary
        // collaborator, and an ordinary collaborator has to be able to
        // accept an invitation.
        //
        // The way round it was to synthesise clicks into the agent's
        // session, which is how an agent came to own a mouse position;
        // that position was then republished every second as a phantom
        // cursor sitting on the Join button. Nothing expires here,
        // because nothing is acquired: a token comes in as an argument
        // and no pointer is ever involved.
        //
        // The redemption itself is `redeem_invite`, which is also what
        // the card's Join button calls. One operation, two ways in — so
        // the capability check a click passes is the one this passes.
        Action {
            id: "peer.join",
            label: "Join workspace from invite",
            subtitle: "Redeem an invite token and join its room, without a pointer",
            params: PEER_JOIN_PARAMS,
            apply: |st, args| {
                let token: String =
                    args.text("token").unwrap_or_default().split_whitespace().collect();
                if token.is_empty() {
                    return Err(ActionError::Refused(
                        "joining a room takes the invite token you were sent".into(),
                    ));
                }
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::AcceptInvite(token),
                );
                Ok(())
            },
        },
        // Inviting somebody by name. `peer.invite` mints a token and
        // says nothing about who it is for, which is why a peer that
        // joins with one is drawn as the number its transport identity
        // hashes to. A number says a machine is here; it does not say
        // who is here.
        //
        // The name has to come from the inviter rather than from the
        // invitee, and that is not a stylistic preference: a peer that
        // announced its own name could announce any name, and it would
        // render next to a caret inside a document this room's
        // capabilities gate. So the name is recorded here, by the
        // person doing the inviting, into the room document — the same
        // replicated, authority-checked place the capability grants
        // live, refused from every peer but the room's own.
        //
        // The peer id stays the identity; the name is a label on it.
        // A peer nobody has named keeps its id as its label, which is
        // honest, rather than acquiring a name nobody chose.
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
                // Scoped so the tree's write lock is released before
                // anything else on the chrome is touched.
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
                // And mint the token, so naming somebody and inviting
                // them are one gesture rather than two that can be done
                // in the wrong order. Minted *for* them, so the card
                // that comes up says whose invite it is holding rather
                // than making the caller remember.
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                        name,
                        copy: false,
                    },
                );
                Ok(())
            },
        },
        // Admitting an agent. Deliberately alongside the peer rows and
        // not in a corner of its own: a person reaching for "who else is
        // in this room with me" should find both answers in one place,
        // because the OS's claim is that an agent is an ordinary kind of
        // participant rather than a special mode.
        //
        // It is not, however, the *same* operation. A peer is reached
        // over the network and admitted with a signed token it presents;
        // an agent presents nothing, and is admitted by the room
        // recording a capability naming it. So this mints no token and
        // opens no prompt — it writes the grant, and the grant is what
        // the agent's authority is made of. Revoking it in Workspace
        // Settings is the whole of taking it back.
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
                // The principal is the name case-folded. A capability
                // naming `Scribe` and one naming `scribe` are two
                // different subjects, so admitting an agent under one
                // spelling and asking about it under the other is an
                // agent that is half in the room — which reads on screen
                // as a grant that did not take.
                let agent = name.to_lowercase();
                let user = st.local_user_id.clone();
                // Scoped so the tree's write lock is released before
                // anything else on the chrome is touched.
                let granted = {
                    let mut tree = st.tree_mut();
                    let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
                        return Err(ActionError::Refused(
                            "there is no room here to admit an agent to".into(),
                        ));
                    };
                    let room = ws_id.raw().to_string();
                    // Membership is an ordinary grant on the room —
                    // `CapOverride::membership` is spelled that way
                    // precisely because it is not a second mechanism.
                    // Through the tree so the capability reaches the room
                    // document (and therefore the room's other members)
                    // and the disk, rather than only this process.
                    let cap = CapOverride::membership(
                        Subject::agent(agent.clone()),
                        room,
                        Verb::Write,
                    )
                    .with_id(format!("agent-member:{agent}"));
                    tree.grant_in_workspace(ws_id, &user, cap)
                };
                if granted.is_none() {
                    return Err(ActionError::Refused(format!(
                        "{user} does not administer this room, so {agent} was not admitted"
                    )));
                }
                // What admitting somebody produces is that they are in
                // the room, and the roster is where a person reads that.
                // Showing it is the same move `peer.invite` makes when it
                // puts its token on screen: an invitation that leaves
                // nothing to look at cannot be checked by the person who
                // issued it.
                st.overlays.collaborators = true;
                Ok(())
            },
        },
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
            // Admitting an agent needs two things this session may not
            // have, and offering a row that cannot work is worse than
            // offering none: a room to record the capability in, and a
            // working broker, which is the only way an agent ever reaches
            // the room. Note that peer sync is *not* one of them — an
            // agent connects to this session's own broker socket, so it
            // works in a session that never talks to the network, and
            // hiding the row there would hide a working feature.
            if action.id.starts_with("agent.")
                && (!ctx.chrome.collab_available() || ctx.chrome.tree().active_workspace().is_none())
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

    /// A session with a room this machine administers — the state
    /// `agent.invite` needs, and the state a real session is in.
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
        // Every action, peer-sync and agent rows included, so
        // "everything" means the whole table rather than the subset a
        // default session offers.
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

    /// The row is offered only where it can work.
    ///
    /// Two conditions, and they are different from the peer rows'. A
    /// room is needed because the grant is recorded *in* one. A working
    /// broker is needed because that is how an agent reaches the room at
    /// all. Peer sync deliberately is not: an agent connects to this
    /// session's own broker socket, so it works on a machine that never
    /// talks to the network.
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

    /// Invoking it admits the agent to the room the capability model
    /// answers about — asserted through `agent_members`, the same reader
    /// the roster paints from, rather than by looking for a row in a
    /// vector this test put there.
    #[test]
    fn invoking_it_admits_the_agent_to_the_room() {
        let c = chrome_with_room();

        let before = c
            .borrow()
            .tree()
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert!(before.is_empty(), "nobody is admitted before the action runs");

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
            // Through the real binding path, so the parameter this row
            // declares is the one being exercised.
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
