//! First-use workspace launchpad.
//!
//! An empty collaborative desktop should explain the useful things people can
//! do together, with a direct route to open any available app. Each card is a
//! declared action: [`LaunchpadAction`] names it once — its name, the sentence
//! describing it, its picture, and the app action it runs — and the host
//! publishes that same declaration as its own host actions, so the cards,
//! search's empty card and the catalogue agents read offer one set of verbs.
//! "Open an app" is not a verb of its own: it opens the desktop's search for
//! the apps this device has.

use crate::desktop::i18n::t;
use crate::desktop::state::HostAction;
use ui::icons::Icon;

/// The "Open an app" button in the shell's targets and the accessibility tree; a card is
/// [`LaunchpadAction::target`].
pub const LAUNCHPAD_OPEN_APP: &str = "launchpad:open-app";

/// An action an app publishes, as the desktop's chrome asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchpadAppAction {
    pub app_id: String,
    pub display_name: String,
    pub resource: String,
    pub action: String,
    /// What the action is asked with, by parameter.
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl LaunchpadAppAction {
    /// The app action that opens (or starts) this person's conversation with `root` (hex): Chat's
    /// own `message_person` on the workspace's conversations, which keeps one conversation with
    /// each person.
    #[must_use]
    pub fn message_person(root: &str) -> Self {
        let mut arguments = serde_json::Map::new();
        arguments.insert("who".to_owned(), serde_json::json!({ "person": root }));
        Self {
            app_id: tilo_app_id::apps::CHAT.to_owned(),
            display_name: "Chat".to_owned(),
            resource: tilo_app_id::chat::CONVERSATIONS.to_owned(),
            action: tilo_app_id::chat::MESSAGE_PERSON.to_owned(),
            arguments,
        }
    }

    /// The app action that asks `ask` of the agent of `client` ("Claude Code"): Console's own,
    /// by the client its card is of, so the card, a palette and an agent all reach that client
    /// and no other. The action makes the client's card and acts on it, one path for every way in.
    #[must_use]
    pub fn ask_agent(client: &str, ask: AgentAsk) -> Self {
        let mut arguments = serde_json::Map::new();
        if ask.takes_client() {
            arguments.insert("client".to_owned(), serde_json::json!(client));
        }
        Self {
            app_id: CONSOLE_APP.to_owned(),
            display_name: "Console".to_owned(),
            resource: CONSOLE_RESOURCE.to_owned(),
            action: ask.action().to_owned(),
            arguments,
        }
    }

    /// The app action that shows how an agent connects: Console's own Connect place, with the
    /// commands that add Tilo to a client and the agents already let in.
    #[must_use]
    pub fn connect_agent() -> Self {
        Self {
            app_id: CONSOLE_APP.to_owned(),
            display_name: "Console".to_owned(),
            resource: CONSOLE_RESOURCE.to_owned(),
            action: "open_connect".to_owned(),
            arguments: serde_json::Map::new(),
        }
    }
}

/// The app whose resource the launchpad's agent verbs act on, and the published resource of its
/// package: a verb that takes only the client stands there, not on a card the caller would have
/// to name first.
pub const CONSOLE_APP: &str = "so.tilo.console";
/// The kind Console's root document is published under (`tl.<app id>.Window`), which its verbs
/// that act on it stand on, while its window is open or closed.
pub const CONSOLE_RESOURCE: &str = "tl.so.tilo.console.Window";

/// A welcome verb of the empty shell, declared once: the card that shows it,
/// the host action that offers it to search and agents, and the app action it
/// runs all come from the row below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LaunchpadAction {
    Chat,
    Notes,
    Tasks,
    Meet,
    Invite,
    Activity,
    Agent,
}

impl LaunchpadAction {
    pub const ALL: [Self; 7] = [
        Self::Chat,
        Self::Notes,
        Self::Tasks,
        Self::Agent,
        Self::Meet,
        Self::Invite,
        Self::Activity,
    ];

    /// A stable, untranslated name for the verb.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Notes => "notes",
            Self::Tasks => "tasks",
            Self::Agent => "agent",
            Self::Meet => "meet",
            Self::Invite => "invite",
            Self::Activity => "activity",
        }
    }

    /// The verb's stable name among the shell's targets and in the accessibility tree
    /// (`launchpad:notes`), so a driver presses the card a person sees.
    #[must_use]
    pub fn target(self) -> String {
        format!("launchpad:{}", self.slug())
    }

    /// The host action that runs this verb (`launchpad-notes`): what the session publishes, search
    /// lists and an agent invokes.
    #[must_use]
    pub fn host_id(self) -> String {
        match self {
            // Connecting an agent is the person's own action, which their card and search offer
            // already: the card asks for that one, so it is one thing wherever it is chosen.
            Self::Agent => tilo_actions::remote::session_actions::CONNECT_AGENT.to_owned(),
            _ => format!("launchpad-{}", self.slug()),
        }
    }

    /// Whether the verb publishes a row of its own for search and agents: connecting an agent is
    /// the person's own action, offered as theirs, so its card adds no second row.
    #[must_use]
    pub const fn has_own_row(self) -> bool {
        !matches!(self, Self::Agent)
    }

    /// The verb the host action `id` names, when it is one of the shell's.
    #[must_use]
    pub fn from_host_id(id: &str) -> Option<Self> {
        let slug = id.strip_prefix("launchpad-")?;
        Self::ALL.into_iter().find(|verb| verb.slug() == slug)
    }

    /// The app the verb happens in, with the name a person reads for it: the launchpad's Chat,
    /// its Notes and its tracker, Tasks. Inviting and catching up happen on the shell itself.
    pub const fn app_target(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Chat | Self::Meet => Some(("so.tilo.chat", "Chat")),
            Self::Notes => Some(("so.tilo.notes", "Notes")),
            Self::Tasks => Some(("so.tilo.tasks", "Tasks")),
            Self::Agent => Some(("so.tilo.console", "Console")),
            Self::Invite | Self::Activity => None,
        }
    }

    /// Whether this session, which has `installed` to open apps from, offers the verb: a card that
    /// opens an app stands only where the app is there to open, and a card of the shell's own
    /// (inviting someone, catching up) always stands. `installed` is the host's app catalogue, the
    /// same answer search lists an app by.
    #[must_use]
    pub fn offered(self, installed: impl Fn(&str) -> bool) -> bool {
        self.app_target().is_none_or(|(app, _)| installed(app))
    }

    /// The typed action the verb invokes in its app: the app, the resource it publishes the action
    /// on, and the action. The host delivers it to the app in the workspace, launching the app
    /// first when it is closed, so the verb does what it names in one step. Only a verb that lands
    /// past its app's first screen declares one: Tasks, Notes and a conversation open where the
    /// card says.
    pub fn app_action(self) -> Option<LaunchpadAppAction> {
        match self {
            Self::Meet => Some(LaunchpadAppAction {
                app_id: "so.tilo.chat".to_owned(),
                display_name: "Chat".to_owned(),
                resource: "chat.workspace".to_owned(),
                action: "start-call".to_owned(),
                arguments: serde_json::Map::new(),
            }),
            // Console's own Connect place: the commands that add Tilo to an agent, and the agents
            // already let in.
            Self::Agent => Some(LaunchpadAppAction::connect_agent()),
            Self::Chat | Self::Notes | Self::Tasks | Self::Invite | Self::Activity => None,
        }
    }

    /// The verb's name, as its card and every list of the action read it.
    pub fn label(self) -> String {
        match self {
            Self::Chat => t("launchpad-discuss"),
            Self::Notes => t("launchpad-notes"),
            Self::Tasks => t("launchpad-tasks"),
            Self::Agent => t("launchpad-agent"),
            Self::Meet => t("launchpad-meet"),
            Self::Invite => t("launchpad-invite"),
            Self::Activity => t("launchpad-activity"),
        }
    }

    /// What the verb does, in the words the workspace's scope makes true: a private workspace has
    /// nobody to write or talk with yet.
    #[must_use]
    pub fn detail(self, scope: WorkspaceScope) -> String {
        let private = scope == WorkspaceScope::Private;
        match self {
            Self::Chat if private => t("launchpad-discuss-detail-private"),
            Self::Chat => t("launchpad-discuss-detail"),
            Self::Notes if private => t("launchpad-notes-detail-private"),
            Self::Notes => t("launchpad-notes-detail"),
            Self::Tasks => t("launchpad-tasks-detail"),
            Self::Agent => t("launchpad-agent-detail"),
            Self::Meet if private => t("launchpad-meet-detail-private"),
            Self::Meet => t("launchpad-meet-detail"),
            Self::Invite if private => t("launchpad-invite-detail-private"),
            Self::Invite => t("launchpad-invite-detail"),
            Self::Activity => t("launchpad-activity-detail"),
        }
    }

    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::Chat => Icon::MessageCircle,
            Self::Notes => Icon::FileText,
            Self::Tasks => Icon::ListChecks,
            Self::Agent => Icon::Bot,
            Self::Meet => Icon::Phone,
            Self::Invite => Icon::UserPlus,
            Self::Activity => Icon::History,
        }
    }

    /// Whether the verb is a card in a workspace of `scope`: talking and calling need someone else
    /// there, and letting an agent in is what a person alone starts with, so a private workspace
    /// offers connecting an agent in their place. Search and agents still find every verb.
    #[must_use]
    pub fn fits(self, scope: WorkspaceScope) -> bool {
        match self {
            Self::Chat | Self::Meet => scope != WorkspaceScope::Private,
            Self::Agent => scope == WorkspaceScope::Private,
            Self::Notes | Self::Tasks | Self::Invite | Self::Activity => true,
        }
    }

    /// Whether search offers the verb before anything is typed: what brings
    /// people in, calls them or shows what they did. A card that starts an app
    /// is looked for by name — search already lists that app by its own name,
    /// and two rows to the same app say less than one.
    pub const fn unasked(self) -> bool {
        matches!(self, Self::Meet | Self::Invite | Self::Activity)
    }

    /// The action the host publishes for this verb: its name, the sentence describing it, its own
    /// picture and whether search may offer it before anything is typed ([`HostAction`]). The
    /// desktops' cards, the palette and the catalogue agents read all read this.
    #[must_use]
    pub fn host_action(self, scope: WorkspaceScope) -> HostAction {
        HostAction {
            id: self.host_id(),
            title: self.label(),
            label: None,
            subtitle: Some(self.detail(scope)),
            icon: Some(self.icon().name().to_owned()),
            destructive: false,
            unasked: self.unasked(),
            source: None,
        }
    }
}

/// What a person asks of an agent from its face's card. Each is Console's own action, by the
/// client its card is of: nothing done to an agent happens out of sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAsk {
    /// Pull its card out into a window beside the work.
    Card,
    Stop,
    UndoSession,
    TakeOver,
    Revoke,
}

impl AgentAsk {
    /// The action of Console's package resource it is.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Card => "open",
            Self::Stop => "stop_client",
            Self::UndoSession => "undo_client_session",
            Self::TakeOver => "take_over_client",
            Self::Revoke => "revoke_client",
        }
    }

    /// Whether the action is asked for one client, by its name: opening Console asks for none.
    #[must_use]
    pub const fn takes_client(self) -> bool {
        !matches!(self, Self::Card)
    }
}

/// Who sees what is made in the workspace the launchpad is shown in, which is what its words about
/// sharing must say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceScope {
    /// Only the person themselves.
    Private,
    /// The people invited to it.
    #[default]
    Shared,
    /// Anyone who joins: a public or community workspace.
    Public,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::i18n::{ensure, t};

    tilo_test_env::scenario! {
        fn launchpad_cards_publish_and_route_their_actions;

        phase the_sharing_copy_follows_the_workspace_scope;
        {
            ensure();
            assert_eq!(
                LaunchpadAction::Notes.detail(WorkspaceScope::Private),
                t("launchpad-notes-detail-private")
            );
            assert_eq!(
                LaunchpadAction::Notes.detail(WorkspaceScope::Shared),
                t("launchpad-notes-detail")
            );
        }

        phase someone_alone_is_offered_an_agent_in_place_of_talking;
        // Nobody else is there to talk or call yet: a private workspace offers connecting an agent
        // instead, and a shared one keeps its conversation and its calls.
        {
            let cards = |scope| -> Vec<LaunchpadAction> {
                LaunchpadAction::ALL
                    .into_iter()
                    .filter(|card| card.fits(scope))
                    .collect()
            };
            assert_eq!(
                cards(WorkspaceScope::Shared),
                [
                    LaunchpadAction::Chat,
                    LaunchpadAction::Notes,
                    LaunchpadAction::Tasks,
                    LaunchpadAction::Meet,
                    LaunchpadAction::Invite,
                    LaunchpadAction::Activity,
                ]
            );
            assert_eq!(
                cards(WorkspaceScope::Private),
                [
                    LaunchpadAction::Notes,
                    LaunchpadAction::Tasks,
                    LaunchpadAction::Agent,
                    LaunchpadAction::Invite,
                    LaunchpadAction::Activity,
                ]
            );
            assert_eq!(
                LaunchpadAction::Agent.app_action(),
                Some(LaunchpadAppAction::connect_agent()),
                "the card opens Console where an agent is connected"
            );
            let all = |_: &str| true;
            assert_eq!(
                LaunchpadAction::ALL
                    .into_iter()
                    .filter(|card| card.offered(all))
                    .count(),
                LaunchpadAction::ALL.len()
            );
        }

        phase every_card_publishes_one_host_action_of_its_own;
        // The cards, the host actions search lists and the verbs agents read are
        // one declaration: every card publishes a row of its own, with the name,
        // the picture and the flags the card shows.
        {
            ensure();
            let published: Vec<_> = LaunchpadAction::ALL
                .into_iter()
                .map(|card| card.host_action(WorkspaceScope::Shared))
                .collect();
            assert_eq!(published.len(), LaunchpadAction::ALL.len());
            for (action, card) in published.iter().zip(LaunchpadAction::ALL) {
                assert_eq!(action.id, card.host_id());
                assert_eq!(action.title, card.label());
                assert_eq!(action.subtitle, Some(card.detail(WorkspaceScope::Shared)));
                assert_eq!(action.icon, Some(card.icon().name().to_owned()));
                assert!(!action.destructive, "no card takes anything away");
                assert_eq!(action.unasked, card.unasked());
                assert_eq!(action.source, None, "a card is no menu's row");
                let own = card.has_own_row().then_some(card);
                assert_eq!(LaunchpadAction::from_host_id(&action.id), own);
            }
            assert_eq!(
                LaunchpadAction::from_host_id("launchpad-open-app"),
                None,
                "opening an app is no card's verb"
            );
        }

        phase a_card_stands_only_where_its_app_is_installed;
        // A card that opens an app stands only where the host has it, and a card of the shell's own
        // stands everywhere: the shell offers what a person can open, never a window of an app that is
        // not here.
        {
            let only = ["so.tilo.files"];
            let installed = |app: &str| only.contains(&app);
            assert_eq!(
                LaunchpadAction::ALL
                    .into_iter()
                    .filter(|card| card.offered(installed))
                    .collect::<Vec<_>>(),
                [LaunchpadAction::Invite, LaunchpadAction::Activity],
                "no card of an app this device has not got, and the shell's own always"
            );
            assert!(
                !LaunchpadAction::Tasks.offered(installed),
                "this device has no Tasks app"
            );
            let all = |_: &str| true;
            assert!(
                LaunchpadAction::ALL
                    .into_iter()
                    .all(|card| card.offered(all))
            );
        }

        phase a_persons_card_asks_chat_for_the_conversation_between_them;
        {
            let root = "a6d40c19e7b25f83c1d9e0a4b7f26c58d3e1a9b0c7f4e2d6a8b5c3e1f0d9a7c2";
            let ask = LaunchpadAppAction::message_person(root);
            assert_eq!(ask.app_id, "so.tilo.chat");
            assert_eq!(ask.display_name, "Chat");
            assert_eq!(ask.action, "message_person");
            assert_eq!(
                ask.resource,
                tilo_app_id::chat::CONVERSATIONS,
                "Chat keeps one conversation with each person among the workspace's"
            );
            assert_eq!(
                ask.arguments.get("who"),
                Some(&serde_json::json!({ "person": root })),
                "and is asked for the one with them"
            );
        }

        phase every_card_has_a_unique_semantic_route;
        {
            let routes = LaunchpadAction::ALL.map(LaunchpadAction::target);
            let unique = routes.iter().collect::<std::collections::HashSet<_>>();
            assert_eq!(unique.len(), LaunchpadAction::ALL.len());
            assert!(routes.iter().all(|route| route.starts_with("launchpad:")));
        }

        phase collaboration_cards_target_the_apps_that_own_those_features;
        {
            assert_eq!(
                LaunchpadAction::Chat.app_target(),
                Some(("so.tilo.chat", "Chat"))
            );
            assert_eq!(
                LaunchpadAction::Meet.app_target(),
                Some(("so.tilo.chat", "Chat")),
                "calls are a Chat feature, not a fabricated Meet package"
            );
            assert_eq!(
                LaunchpadAction::Tasks.app_target(),
                Some(("so.tilo.tasks", "Tasks")),
                "Tasks is the workspace's own tracker, not Calendar's to-do list"
            );
        }

        phase the_call_card_names_the_action_it_performs;
        {
            let call = LaunchpadAction::Meet.app_action().expect("call action");
            assert_eq!(
                (
                    call.app_id.as_str(),
                    call.resource.as_str(),
                    call.action.as_str()
                ),
                ("so.tilo.chat", "chat.workspace", "start-call")
            );
            assert_eq!(
                LaunchpadAction::Tasks.app_action(),
                None,
                "Tasks opens on the list its card names: no action has to steer it there"
            );
            for card in LaunchpadAction::ALL {
                if let Some(action) = card.app_action() {
                    assert_eq!(
                        card.app_target(),
                        Some((action.app_id.as_str(), action.display_name.as_str())),
                        "{card:?} acts in the app it opens"
                    );
                }
            }
        }

        phase a_persons_card_reaches_its_client_through_consoles_own_actions;
        // Each ask is Console's own action on its package resource, by the client its card is
        // of: two clients' cards ask two clients, and none names a resource of an app Console
        // no longer has.
        {
            let stop = LaunchpadAppAction::ask_agent("Cursor", AgentAsk::Stop);
            assert_eq!(stop.app_id, CONSOLE_APP);
            assert_eq!(stop.resource, CONSOLE_RESOURCE);
            assert_eq!(stop.action, "stop_client");
            assert_eq!(
                stop.arguments.get("client"),
                Some(&serde_json::json!("Cursor"))
            );
            let theirs = LaunchpadAppAction::ask_agent("Claude Code", AgentAsk::Stop);
            assert_eq!(
                theirs.arguments.get("client"),
                Some(&serde_json::json!("Claude Code")),
                "each card asks for its own client"
            );
            for (ask, action) in [
                (AgentAsk::Stop, "stop_client"),
                (AgentAsk::UndoSession, "undo_client_session"),
                (AgentAsk::TakeOver, "take_over_client"),
                (AgentAsk::Revoke, "revoke_client"),
            ] {
                let made = LaunchpadAppAction::ask_agent("Cursor", ask);
                assert_eq!(made.action, action);
                assert_eq!(
                    made.arguments.get("client"),
                    Some(&serde_json::json!("Cursor")),
                    "{action} names the client"
                );
            }
            let card = LaunchpadAppAction::ask_agent("Cursor", AgentAsk::Card);
            assert_eq!(card.action, "open");
            assert!(card.arguments.is_empty(), "opening asks for no client");
            let connect = LaunchpadAppAction::connect_agent();
            assert_eq!(connect.app_id, CONSOLE_APP);
            assert_eq!(connect.resource, CONSOLE_RESOURCE);
            assert_eq!(connect.action, "open_connect");
            assert!(connect.arguments.is_empty());
        }
    }
}
