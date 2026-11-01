//! "Happening now", the empty palette's first group: who is in which of the
//! person's workspaces, what they are in, and the calls they could join.
//!
//! One row per workspace with someone in it, wearing their faces, and under
//! it a row for each window someone is in (choosing it goes there) and each
//! call in progress (choosing it joins). It reads the presence the desk
//! already holds: the collaborators the desktop writes into each workspace
//! as its people say where they are, and the calls their presence names.
//! Nothing here watches anything.
//!
//! Scoped to the workspace in front, it lists that one; widened, every
//! workspace, the one in front first.

use ui::search::RowFace;
use ui::search::system::ResourceKind;

use crate::model::{CHROME_RESOURCE, Hit, InvokeTarget, SHOW_COLLABORATORS, Source, Target};

/// Row ids: a workspace, a window in one, a call.
const WORKSPACE_PREFIX: &str = "workspace:";
const WINDOW_PREFIX: &str = "window:";
const CALL_PREFIX: &str = "call:";

/// Chat holds a room's voice channels: each channel has a call row with the
/// action that joins its call.
const CHAT_APP: &str = "so.tilo.chat";
const CHAT_RECORD_KIND: &str = "chat.workspace";
const JOIN_CALL: &str = "join-call";

/// The kind the group is listed under, in the desktop's language.
#[must_use]
pub fn happening_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-happening"))
}

/// How far the empty palette leads with this group: more than any other
/// starting point's best row, so who is here comes first, as the launcher's
/// first section did. Only the empty palette asks this source anything.
const LEAD_INTENT: f32 = 1.0;

/// Someone in a workspace, as the desk's collaborators list them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonHere {
    /// Their id within the workspace's roster: their face is theirs.
    pub id: String,
    pub name: String,
    /// Their presence colour, `(r, g, b)`.
    pub colour: (u8, u8, u8),
    pub agent: bool,
    /// They are away from their machine: the face dims.
    pub away: bool,
    /// The window they are in, when the desk shows it here.
    pub tab: Option<u64>,
}

impl PersonHere {
    fn face(&self) -> RowFace {
        RowFace {
            name: self.name.clone(),
            identity: self.id.clone(),
            colour: ui::peniko::Color::from_rgb8(self.colour.0, self.colour.1, self.colour.2),
            is_agent: self.agent,
            dimmed: self.away,
        }
    }
}

/// A call in progress in a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallHere {
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called; empty when nobody said.
    pub name: String,
    /// Who is in it.
    pub people: Vec<PersonHere>,
}

/// One window someone is in: its title and who is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowHere {
    pub tab: u64,
    pub title: String,
    pub people: Vec<PersonHere>,
}

/// One workspace and who is here in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceHere {
    /// The workspace's id, as the desktop numbers it.
    pub id: u64,
    /// The room it is, as its id travels: where Chat's call is joined.
    pub room: String,
    pub name: String,
    /// The workspace in front.
    pub active: bool,
    pub people: Vec<PersonHere>,
    /// The windows someone is in, in the order their first person is listed.
    pub windows: Vec<WindowHere>,
    pub calls: Vec<CallHere>,
}

/// "Morgan is here", "Morgan and Alex are here", "Morgan and 3 others are
/// here".
fn here_line(people: &[PersonHere]) -> String {
    names_line(people, "search-launcher-here")
}

/// "With Morgan", "With Morgan and Alex", "With Morgan and 3 others".
fn with_line(people: &[PersonHere]) -> String {
    names_line(people, "search-launcher-with")
}

fn names_line(people: &[PersonHere], key: &str) -> String {
    let name = |at: usize| people.get(at).map_or("", |person| person.name.as_str());
    match people.len() {
        0 | 1 => crate::i18n::t_args(&format!("{key}-one"), &[("name", name(0).into())]),
        2 => crate::i18n::t_args(
            &format!("{key}-two"),
            &[("name", name(0).into()), ("other", name(1).into())],
        ),
        count => crate::i18n::t_args(
            &format!("{key}-many"),
            &[
                ("name", name(0).into()),
                ("count", count.saturating_sub(1).into()),
            ],
        ),
    }
}

/// Chat's resource id of voice channel `channel`'s row: where its call is
/// joined.
fn channel_row(channel: &str) -> String {
    format!("{CHAT_RECORD_KIND}/channels/{channel}/call")
}

/// Who is in the person's workspaces now, as the desk holds it.
pub struct Happening {
    /// The workspace in front, every workspace when `every_workspace`.
    every_workspace: bool,
    workspaces: Vec<WorkspaceHere>,
}

impl Happening {
    /// The rows for `workspaces`: the one in front alone, or every one.
    #[must_use]
    pub fn new(every_workspace: bool, workspaces: Vec<WorkspaceHere>) -> Self {
        Self {
            every_workspace,
            workspaces,
        }
    }

    /// A workspace's header row: who is here, and the way to them.
    fn header(&self, workspace: &WorkspaceHere) -> Hit {
        let faces: Vec<RowFace> = workspace.people.iter().map(PersonHere::face).collect();
        let target = if workspace.active {
            // Who is here: the people panel, as the bar's faces open it.
            Target::Invoke(Box::new(InvokeTarget::action(
                crate::model::DESKTOP_APP,
                &workspace.room,
                CHROME_RESOURCE,
                SHOW_COLLABORATORS,
            )))
        } else {
            Target::Command {
                id: format!("{}{}", crate::model::WORKSPACE_COMMAND_PREFIX, workspace.id),
            }
        };
        Hit {
            kind: happening_kind(),
            provider: self.id().into(),
            id: format!("{WORKSPACE_PREFIX}{}", workspace.id),
            title: workspace.name.clone(),
            subtitle: Some(here_line(&workspace.people)),
            facts: Vec::new(),
            score: 0,
            icon: Some(ui::icons::Icon::Users),
            faces,
            outcome: Some(crate::i18n::t("search-launcher-outcome-open")),
            target,
        }
    }

    fn window(&self, workspace: &WorkspaceHere, window: &WindowHere) -> Hit {
        Hit {
            kind: happening_kind(),
            provider: self.id().into(),
            id: format!("{WINDOW_PREFIX}{}:{}", workspace.id, window.tab),
            title: window.title.clone(),
            subtitle: Some(with_line(&window.people)),
            facts: Vec::new(),
            score: 0,
            icon: Some(ui::icons::Icon::LayoutGrid),
            faces: window.people.iter().map(PersonHere::face).collect(),
            outcome: Some(crate::i18n::t("search-launcher-outcome-join")),
            target: Target::Window {
                tab: window.tab,
                app: None,
            },
        }
    }

    fn call(&self, workspace: &WorkspaceHere, call: &CallHere) -> Hit {
        let name = call.name.trim();
        let title = if name.is_empty() {
            crate::i18n::t("search-launcher-call")
        } else {
            crate::i18n::t_args("search-launcher-call-in", &[("channel", name.into())])
        };
        Hit {
            kind: happening_kind(),
            provider: self.id().into(),
            id: format!("{CALL_PREFIX}{}", call.channel),
            title,
            subtitle: Some(with_line(&call.people)),
            facts: Vec::new(),
            score: 0,
            icon: Some(ui::icons::Icon::Phone),
            faces: call.people.iter().map(PersonHere::face).collect(),
            outcome: Some(crate::i18n::t("search-launcher-outcome-join")),
            target: Target::Invoke(Box::new(InvokeTarget::action(
                CHAT_APP,
                &workspace.room,
                channel_row(&call.channel),
                JOIN_CALL,
            ))),
        }
    }
}

impl Source for Happening {
    fn id(&self) -> &'static str {
        "happening-now"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-happening")
    }

    fn max_score(&self) -> i32 {
        ui::search::system::MAX_PROVIDER_SCORE
    }

    fn intent(&self, _hit: &Hit) -> f32 {
        LEAD_INTENT
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if !needle.trim().is_empty() {
            return Vec::new();
        }
        let mut workspaces: Vec<&WorkspaceHere> = self
            .workspaces
            .iter()
            .filter(|workspace| {
                let shown = self.every_workspace || workspace.active;
                shown && (!workspace.people.is_empty() || !workspace.calls.is_empty())
            })
            .collect();
        workspaces.sort_by_key(|workspace| !workspace.active);
        let mut rows: Vec<Hit> = Vec::new();
        let mut score = self.max_score();
        for workspace in workspaces {
            let mut workspace_rows = vec![self.header(workspace)];
            workspace_rows.extend(self.windows(workspace));
            for call in &workspace.calls {
                workspace_rows.push(self.call(workspace, call));
            }
            for mut row in workspace_rows {
                // Rows keep the order they are listed in: the header, then
                // its windows and calls.
                row.score = score.max(1);
                score = score.saturating_sub(1);
                rows.push(row);
            }
        }
        rows
    }
}

impl Happening {
    /// The windows of `workspace` someone is in, in the order their first
    /// person is listed.
    fn windows(&self, workspace: &WorkspaceHere) -> Vec<Hit> {
        workspace
            .windows
            .iter()
            .filter(|window| !window.title.trim().is_empty() && !window.people.is_empty())
            .map(|window| self.window(workspace, window))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, name: &str, tab: Option<u64>) -> PersonHere {
        PersonHere {
            id: id.into(),
            name: name.into(),
            colour: ui::presence::user_color(id),
            agent: false,
            away: false,
            tab,
        }
    }

    fn workspace() -> WorkspaceHere {
        WorkspaceHere {
            id: 3,
            room: "room-3".into(),
            name: "Studio".into(),
            active: true,
            people: vec![person("ada", "Ada", Some(7)), person("bo", "Bo", None)],
            windows: vec![WindowHere {
                tab: 7,
                title: "Quarterly plan".into(),
                people: vec![person("ada", "Ada", Some(7))],
            }],
            calls: vec![CallHere {
                channel: "c_voice".into(),
                name: "voice".into(),
                people: vec![person("bo", "Bo", None)],
            }],
        }
    }

    /// The empty palette lists the workspace someone is in, the window they
    /// are in and the call to join, in that order, and nothing else: a
    /// query finds people by what they say elsewhere, not here.
    #[test]
    fn who_is_here_and_the_calls_are_listed_while_nothing_is_typed() {
        let happening = Happening::new(false, vec![workspace()]);
        let rows = happening.query("");
        let ids: Vec<&str> = rows.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(ids, ["workspace:3", "window:3:7", "call:c_voice"]);
        assert_eq!(rows[0].title, "Studio");
        assert_eq!(
            rows[0].subtitle.as_deref(),
            Some("Ada and Bo are here"),
            "the header says who is here"
        );
        assert_eq!(rows[0].faces.len(), 2, "their faces are worn");
        assert_eq!(rows[1].title, "Quarterly plan");
        assert_eq!(rows[1].subtitle.as_deref(), Some("With Ada"));
        assert_eq!(rows[2].title, "Call in voice");
        assert_eq!(rows[2].subtitle.as_deref(), Some("With Bo"));
        assert_eq!(rows[0].kind, happening_kind());
        assert!(rows.iter().all(|row| row.outcome.is_some()));
        assert!(happening.query("ada").is_empty(), "typed text is not this");
    }

    /// Choosing a call joins it: Chat's own action on the channel's call
    /// row, in the workspace the call is in.
    #[test]
    fn a_call_joins_chats_channel() {
        let happening = Happening::new(false, vec![workspace()]);
        let rows = happening.query("");
        let call = rows.iter().find(|row| row.id == "call:c_voice").unwrap();
        let Target::Invoke(invoke) = &call.target else {
            panic!("a call is joined by asking Chat, not by the desk");
        };
        assert_eq!(invoke.app, "so.tilo.chat");
        assert_eq!(invoke.room, "room-3");
        assert_eq!(
            invoke.address,
            tilo_actions::ActionAddress::new("chat.workspace/channels/c_voice/call", "join-call")
        );
        assert_eq!(call.outcome.as_deref(), Some("Join"));
        let window = &rows[1];
        assert_eq!(window.target, Target::Window { tab: 7, app: None });
        let Target::Invoke(panel) = &rows[0].target else {
            panic!("who is here opens the people panel");
        };
        assert_eq!(panel.address.action.as_str(), SHOW_COLLABORATORS);
    }

    /// Another workspace is listed when the search is widened, and its
    /// header goes there instead of opening who is here.
    #[test]
    fn the_search_over_every_workspace_lists_the_others_too() {
        let mut other = WorkspaceHere {
            id: 4,
            room: "room-4".into(),
            name: "Elsewhere".into(),
            active: false,
            ..workspace()
        };
        other.windows.clear();
        other.calls.clear();
        let everywhere = Happening::new(true, vec![workspace(), other.clone()]);
        let rows = everywhere.query("");
        let ids: Vec<&str> = rows.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(ids.first().copied(), Some("workspace:3"), "in front first");
        assert!(ids.contains(&"workspace:4"), "{ids:?}");
        let here_only = Happening::new(false, vec![workspace(), other]);
        let rows = here_only.query("");
        let ids: Vec<&str> = rows.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(
            ids,
            ["workspace:3", "window:3:7", "call:c_voice"],
            "the workspace in front, and nothing of the others"
        );
    }
}
