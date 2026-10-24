//! Bridging a server from the app: the sheet that takes the bot's token,
//! finds the servers the bot is in, and asks the room's host to run the
//! Discord Bridge for the chosen one.
//!
//! It is the room operator's command-line setup (`--discord-token-file`,
//! `--bot "so.tilo.bots.discord-bridge guild=…"`) given from inside the
//! room, through the room's own governance actions on its host: the token
//! with `set-connector-secret`, kept in the host's secret store for the
//! connector and nowhere else, then the bridge with `request-bot` and the
//! server as its inputs. The host decides both against the room's rules on
//! who may add bots, and its follower starts the bridge once both are in.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::i18n::ArgValue;
use ui::prelude::*;
use ui::tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use ui::tilo_actions::{InvocationId, InvocationUpdate};
use ui::{Arrival, InvocationCall, LocalState};

use crate::effects::DiscordEffect;
use crate::locales::CATALOG;
use crate::record::Guild;

/// The bridge bot's entry in the registry's directory.
pub const BRIDGE_ENTRY: &str = "so.tilo.bots.discord-bridge";
/// The version the room asks for: the most recently released one.
pub const BRIDGE_VERSION: &str = "newest";
/// The room's governance verbs this uses.
const SET_CONNECTOR_SECRET: &str = "set-connector-secret";
const REQUEST_BOT: &str = "request-bot";
/// Where Discord lists the servers a bot is in.
const GUILDS_URL: &str = "https://discord.com/api/v10/users/@me/guilds";

/// A Discord server the bot is in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// Its id (a snowflake).
    pub id: String,
    /// Its name as Discord shows it.
    pub name: String,
}

/// Where the sheet stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Step {
    /// Not shown.
    #[default]
    Closed,
    Token,
    /// Asking Discord which servers the bot is in.
    Finding,
    /// Choosing one of them.
    Choose(Vec<Server>),
    /// Asking the room's host to run the bridge for this server.
    Asking(Server),
    /// The host took both; the bridge starts there.
    Asked(Server),
    /// Something failed: what to tell the person.
    Failed(String),
}

/// The servers in Discord's answer to the bot's guild list.
///
/// `None` when the answer is not a list.
#[must_use]
pub fn parse_servers(body: &str) -> Option<Vec<Server>> {
    let listed: Vec<serde_json::Value> = serde_json::from_str(body).ok()?;
    let mut servers: Vec<Server> = listed
        .iter()
        .filter_map(|guild| {
            let id = guild["id"].as_str()?;
            let name = guild["name"].as_str()?;
            (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then(|| Server {
                id: id.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect();
    servers.sort_by_key(|server| server.name.to_lowercase());
    Some(servers)
}

/// Why the bot's servers could not be found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindError {
    /// What was typed is not shaped like a bot token.
    NotAToken,
    /// Discord did not accept the token.
    Refused,
    /// Discord could not be reached.
    Unreachable,
    Discord,
    /// Discord's answer could not be read.
    Unreadable,
}

impl FindError {
    /// What to tell the person.
    #[must_use]
    pub fn message(self) -> String {
        CATALOG.t(match self {
            Self::NotAToken => "discord-setup-not-a-token",
            Self::Refused => "discord-setup-token-refused",
            Self::Unreachable => "discord-setup-unreachable",
            Self::Discord => "discord-setup-discord-error",
            Self::Unreadable => "discord-setup-unreadable",
        })
    }
}

/// Ask Discord which servers the bot with `token` is in.
pub async fn find_servers(token: String) -> Result<Vec<Server>, FindError> {
    let token = token.trim().to_owned();
    if !crate::token::valid_token_shape(&token) {
        return Err(FindError::NotAToken);
    }
    let client = tilo_net::Client::builder()
        .user_agent("tilo-discord/0.1")
        .build()
        .map_err(|_| FindError::Unreachable)?;
    let response = client
        .get(GUILDS_URL)
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
        .map_err(|_| FindError::Unreachable)?;
    let status = response.status();
    if status.as_u16() == 401 {
        return Err(FindError::Refused);
    }
    if !status.is_success() {
        return Err(FindError::Discord);
    }
    let body = response.text().await.map_err(|_| FindError::Unreachable)?;
    parse_servers(&body).ok_or(FindError::Unreadable)
}

/// The two governance invocations that bridge `server` in `room`: the
/// token for the connector, then the bridge with the server as its inputs.
#[must_use]
pub fn bridge_requests(room: &str, token: &str, server: &Server) -> [InvokeRequest; 2] {
    let invoke = |verb: &str, argument: serde_json::Map<String, serde_json::Value>| InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, verb),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let mut secret = serde_json::Map::new();
    secret.insert("app".into(), crate::APP_ID.into());
    secret.insert("purpose".into(), crate::token::TOKEN_PURPOSE.into());
    secret.insert("secret".into(), token.trim().into());
    let mut bridge = serde_json::Map::new();
    bridge.insert("entry".into(), BRIDGE_ENTRY.into());
    bridge.insert("version".into(), BRIDGE_VERSION.into());
    bridge.insert(
        "inputs".into(),
        serde_json::json!({ "guild": server.id, "server": server.name }),
    );
    [
        invoke(SET_CONNECTOR_SECRET, secret),
        invoke(REQUEST_BOT, bridge),
    ]
}

/// What the view keeps for the sheet: where it stands, the token and server
/// it was given, and the host's answers to the bridge's two requests.
#[derive(Clone)]
pub struct Setup {
    /// Where the sheet stands.
    pub step: LocalState<Step>,
    /// The token typed, until the bridge is asked for.
    pub token: LocalState<String>,
    /// The id of the server chosen.
    pub chosen: LocalState<String>,
    remote: ui::RemoteActions,
    asked: Rc<RefCell<Vec<InvocationCall>>>,
}

impl Setup {
    #[must_use]
    pub fn new() -> Self {
        Self {
            step: LocalState::new(Step::Closed),
            token: LocalState::new(String::new()),
            chosen: LocalState::new(String::new()),
            remote: ui::RemoteActions::current(),
            asked: Rc::default(),
        }
    }

    /// Open the sheet at its start.
    pub fn open(&self) {
        self.step.set(Step::Token);
    }

    /// Close the sheet and forget the token.
    pub fn close(&self) {
        self.token.set(String::new());
        self.step.set(Step::Closed);
    }

    /// Discord's list arrived: choose among it, the first chosen.
    pub fn found(&self, found: Result<Vec<Server>, FindError>) {
        match found {
            Ok(servers) => {
                self.chosen
                    .set(servers.first().map(|s| s.id.clone()).unwrap_or_default());
                self.step.set(Step::Choose(servers));
            }
            Err(error) => self.step.set(Step::Failed(error.message())),
        }
    }

    /// The server chosen among those found.
    #[must_use]
    pub fn chosen_server(&self) -> Option<Server> {
        let Step::Choose(servers) = self.step.get() else {
            return None;
        };
        let chosen = self.chosen.get();
        servers.into_iter().find(|server| server.id == chosen)
    }

    /// Send the bridge's two requests for `server` in `room` through the
    /// room's actions; the sheet follows their answers.
    pub fn bridge(&self, room: &str, token: &str, server: Server) {
        let asked = bridge_requests(room, token, &server).map(|request| {
            self.remote
                .invoke_with_id(request.invocation, request.target, request.argument, None)
        });
        *self.asked.borrow_mut() = asked.into();
        self.token.set(String::new());
        self.step.set(Step::Asking(server));
    }

    /// Follow the host's answers: once both requests succeeded the bridge
    /// is asked for; the first refusal or failure says why. Whether the
    /// step changed.
    fn follow(&self) -> bool {
        let Step::Asking(server) = self.step.get() else {
            return false;
        };
        let answers: Vec<Answer> = self
            .asked
            .borrow()
            .iter()
            .map(InvocationCall::get)
            .collect();
        if let Some(failure) = answers.iter().find_map(failure) {
            self.asked.borrow_mut().clear();
            self.step.set(Step::Failed(failure));
            return true;
        }
        let done = answers
            .iter()
            .all(|answer| matches!(answer, Arrival::Ready(InvocationUpdate::Succeeded { .. })));
        if done {
            self.asked.borrow_mut().clear();
            self.step.set(Step::Asked(server));
        }
        done
    }
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

/// Where one of the host's answers stands.
type Answer = Arrival<InvocationUpdate, ui::tilo_actions::remote::ActionHostError>;

/// What to tell the person when the host refused or the request failed.
fn failure(answer: &Answer) -> Option<String> {
    use ui::tilo_actions::remote::ActionHostError;
    match answer {
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(error.message.clone()),
        Arrival::Ready(InvocationUpdate::Cancelled { .. }) => {
            Some(CATALOG.t("discord-setup-cancelled"))
        }
        Arrival::Pending { .. } | Arrival::Ready(_) => None,
        Arrival::Failed(ActionHostError::Unavailable { .. }) => {
            Some(CATALOG.t("discord-setup-host-unreachable"))
        }
        Arrival::Failed(ActionHostError::UnknownAction | ActionHostError::NotPermitted) => {
            Some(CATALOG.t("discord-setup-not-allowed"))
        }
        Arrival::Failed(error) => Some(CATALOG.t_args(
            "discord-setup-host-refused",
            &[("reason", ArgValue::Text(error.to_string()))],
        )),
    }
}

/// Answer the setup's effects, `None` for the others: finding the servers
/// is a job on the app's runtime; bridging sends the room's requests.
pub fn answer(
    setup: &Setup,
    effect: &DiscordEffect,
    cx: &mut EffectCx<Guild>,
) -> Option<EffectAnswer> {
    match effect {
        DiscordEffect::FindServers { token } => {
            let token = token.clone();
            setup.step.set(Step::Finding);
            let setup = setup.clone();
            Some(Ok(cx.spawn(
                move |_| find_servers(token),
                move |(), found: Result<Vec<Server>, FindError>, _| {
                    let answer = match &found {
                        Ok(servers) => Ok(Effected::value(servers)),
                        Err(error) => Err(error.message()),
                    };
                    setup.found(found);
                    answer
                },
            )))
        }
        DiscordEffect::BridgeServer {
            token,
            guild,
            server,
        } => {
            let room = cx.document().and_then(|doc| ui::record::room_scope(&doc));
            let Some(room) = room else {
                let why = CATALOG.t("discord-setup-no-room");
                setup.step.set(Step::Failed(why.clone()));
                return Some(Err(why));
            };
            setup.bridge(
                &room,
                token,
                Server {
                    id: guild.clone(),
                    name: server.clone(),
                },
            );
            Some(Ok(Effected::done()))
        }
        _ => None,
    }
}

fn body(setup: &Setup, step: &Step) -> Element {
    let text = |key: &str| -> Element { Box::new(Text::new(&CATALOG.t(key))) };
    let named = |key: &str, server: &Server| -> Element {
        Box::new(
            Text::new(&CATALOG.t_args(key, &[("server", ArgValue::Text(server.name.clone()))]))
                .with_live(ui::a11y::Politeness::Polite),
        )
    };
    match step {
        Step::Closed => Box::new(Column::new().with_no_padding()),
        Step::Token => Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(
                    Text::new(&CATALOG.t("discord-setup-token-explanation")).dimmed(),
                ))
                .push(Box::new(
                    TextInput::from_state(&setup.token)
                        .with_id("discord-setup-token")
                        .with_secret(true)
                        .with_accessible_name(CATALOG.t("discord-setup-token"))
                        .with_placeholder(CATALOG.t("discord-setup-token")),
                )),
        ),
        Step::Finding => text("discord-setup-finding"),
        Step::Choose(servers) if servers.is_empty() => text("discord-setup-no-servers"),
        Step::Choose(servers) => {
            let mut choice = ui::widget::Dropdown::from_state(Vec::new(), &setup.chosen)
                .with_id("discord-setup-server")
                .with_accessible_name(CATALOG.t("discord-setup-server"))
                .fill();
            choice.set_value_options(
                servers
                    .iter()
                    .map(|server| (server.id.clone(), server.name.clone()))
                    .collect(),
            );
            Box::new(
                Column::new()
                    .with_no_padding()
                    .push(Box::new(
                        Text::new(&CATALOG.t("discord-setup-choose")).dimmed(),
                    ))
                    .push(Box::new(choice)),
            )
        }
        Step::Asking(server) => named("discord-setup-asking", server),
        Step::Asked(server) => named("discord-setup-asked", server),
        Step::Failed(why) => Box::new(Text::new(why).with_live(ui::a11y::Politeness::Assertive)),
    }
}

fn primary_label(step: &Step) -> String {
    CATALOG.t(match step {
        Step::Closed | Step::Token | Step::Finding => "discord-setup-find",
        Step::Choose(servers) if servers.is_empty() => "discord-setup-try-again",
        Step::Choose(_) | Step::Asking(_) => "discord-setup-bridge",
        Step::Asked(_) => "discord-setup-done",
        Step::Failed(_) => "discord-setup-try-again",
    })
}

/// The sheet, over the window while it is open.
pub fn sheet(cx: &ViewCx<Guild>, setup: &Setup) -> Element {
    let shown = setup.clone();
    let content = cx.switch(&setup.step, move |_, step| body(&shown, step));
    let primary = {
        let setup = setup.clone();
        let dispatch = cx.dispatcher();
        move || match setup.step.get() {
            Step::Token => {
                let token = setup.token.get();
                dispatch.effect(&DiscordEffect::FindServers { token });
            }
            Step::Choose(servers) if servers.is_empty() => setup.open(),
            Step::Choose(_) => {
                if let Some(server) = setup.chosen_server() {
                    dispatch.effect(&DiscordEffect::BridgeServer {
                        token: setup.token.get(),
                        guild: server.id,
                        server: server.name,
                    });
                }
            }
            Step::Asked(_) => setup.close(),
            Step::Failed(_) => setup.open(),
            Step::Closed | Step::Finding | Step::Asking(_) => {}
        }
    };
    let (cancel, dismiss) = (setup.clone(), setup.clone());
    let dialog = ui::widget::ModalDialog::new(
        CATALOG.t("discord-setup-title"),
        Container::new().with_no_padding().with_child(content),
    )
    .with_test_id("discord-setup")
    .primary_action(primary_label(&Step::Token), primary)
    .secondary_action(CATALOG.t("discord-setup-cancel"), move || cancel.close())
    .on_dismiss(move || dismiss.close())
    .with_max_width(560.0);
    Box::new(SetupSheet {
        dialog,
        setup: setup.clone(),
        shown: None,
    })
}

/// The dialog, open while the setup is, with its primary action following
/// the step.
struct SetupSheet {
    dialog: ui::widget::ModalDialog<Container>,
    setup: Setup,
    shown: Option<Step>,
}

impl SetupSheet {
    fn follow(&mut self) {
        let step = self.setup.step.get();
        if self.shown.as_ref() == Some(&step) {
            return;
        }
        let open = step != Step::Closed;
        if open != self.dialog.is_open() {
            self.dialog.set_open(open);
        }
        self.dialog.set_primary_label(primary_label(&step));
        self.dialog
            .set_primary_enabled(!matches!(step, Step::Finding | Step::Asking(_)));
        self.shown = Some(step);
    }
}

impl ui::widget::Decorator for SetupSheet {
    type Inner = ui::widget::ModalDialog<Container>;

    fn inner(&self) -> &Self::Inner {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.dialog
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let followed = self.setup.follow();
        self.follow();
        ui::widget::Widget::update(&mut self.dialog, state, path) || followed
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.follow();
        ui::widget::Widget::handle_event(&mut self.dialog, event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.shown.as_ref() != Some(&self.setup.step.get()) || self.dialog.needs_redraw()
    }

    fn concrete_type_name(&self) -> &'static str {
        "DiscordSetupSheet"
    }
}

/// The window's content with the setup's action client: its requests leave
/// through the app's collaboration connection and the host's answers come
/// back to it.
pub struct SetupDoor {
    content: Stack,
    remote: ui::RemoteActions,
}

impl SetupDoor {
    #[must_use]
    pub fn new(page: Element, sheet: Element, setup: &Setup) -> Self {
        Self {
            content: Stack::new().push(page).push(sheet),
            remote: setup.remote.clone(),
        }
    }
}

impl ui::widget::Decorator for SetupDoor {
    ui::provides!(dyn ui::widget::Collaborative; inner = content);

    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.content
    }

    fn concrete_type_name(&self) -> &'static str {
        "DiscordSetupDoor"
    }
}

impl ui::widget::Collaborative for SetupDoor {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        ui::widget::Collaborative::take_collab_requests(&mut self.remote)
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        ui::widget::Collaborative::handle_collab_event(&mut self.remote, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_are_read_from_discords_answer_by_name() {
        let body = r#"[
            {"id": "222", "name": "garden club", "icon": null},
            {"id": "111", "name": "Book Club", "owner": false},
            {"id": "not-a-snowflake", "name": "Broken"},
            {"name": "No id"}
        ]"#;
        assert_eq!(
            parse_servers(body).unwrap(),
            [
                Server {
                    id: "111".into(),
                    name: "Book Club".into()
                },
                Server {
                    id: "222".into(),
                    name: "garden club".into()
                },
            ]
        );
        assert!(parse_servers("<html>").is_none());
    }

    /// The token goes to the host as a connector secret, never in the
    /// bridge request every member can read; the bridge names the server.
    #[test]
    fn bridging_gives_the_host_the_token_apart_from_the_request() {
        let server = Server {
            id: "123456789012345678".into(),
            name: "Garden club".into(),
        };
        let [secret, bridge] = bridge_requests("room-1", " a.b.c ", &server);
        assert_eq!(secret.target.address.action, SET_CONNECTOR_SECRET);
        assert_eq!(bridge.target.address.action, REQUEST_BOT);
        let InvokeArgument::Json(secret) = secret.argument else {
            panic!("named arguments");
        };
        assert_eq!(secret["secret"], "a.b.c");
        assert_eq!(secret["app"], crate::APP_ID);
        let InvokeArgument::Json(bridge) = bridge.argument else {
            panic!("named arguments");
        };
        assert_eq!(bridge["entry"], BRIDGE_ENTRY);
        assert_eq!(bridge["inputs"]["guild"], "123456789012345678");
        assert_eq!(bridge["inputs"]["server"], "Garden club");
        assert!(
            !bridge
                .values()
                .any(|value| value.to_string().contains("a.b.c"))
        );
    }
}
