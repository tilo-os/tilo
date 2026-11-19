//! A person's mail, as a turn this server carries out for them reaches it.
//!
//! An agent a person keeps on this server works with every device of theirs
//! off, so no transport certified a device behind what it asks, and the
//! check a device's request passes ([`Workspaces::asking_about_mail`]) has
//! nothing to compare. This is the server's own entry for such a turn. It is
//! reached from no transport: only the code that carries out the turn calls
//! it, with the caller the turn's tools act as and what the workspace's list
//! of its agents says of the agent ([`HostedTurn`]).
//!
//! Two checks, in this order, and neither is the whole rule. The turn is its
//! keeper's own ([`Workspaces::keepers_turn`]): an agent on a turn for the
//! person who keeps it, whom the workspace admits now. And that person named
//! this agent for the account, in this workspace
//! ([`person_accounts::ASSISTANT`]): nothing is granted by default, and the
//! same id in a workspace somebody else keeps is nobody. An agent that
//! anything narrows (what its entry bounds it to; a sub-agent) also holds a
//! rule that names the very action ([`HostedTurn::may_take`]). Unknown and
//! forbidden are the same answer.
//!
//! What such a turn may do is what the named agent may behind a device of
//! its person: list its accounts, read what arrived, read a folder and a
//! message with the files it carries by name and size, search, begin a
//! message and keep it as a draft. The account's sign-in is used inside the
//! mail session and never answered.
//!
//! A send is never carried out by the call that asks for it. The message is
//! read once, as it would leave; one that cannot be shown whole is refused
//! before anybody is asked. What its person is shown is answered
//! ([`AskedSend`]) and the message is set apart, where no action of the
//! agent reaches it any more. Whoever carries out the turn puts the question
//! into the person's conversation, where it waits for as long as they take,
//! over a restart of this server. On their own answer
//! ([`Workspaces::send_asked`]) the message that waited is read again, and
//! it leaves only when it still shows exactly what they were shown and its
//! agent is still named for the account. Whatever comes of the answer, what
//! waited is gone, so nothing leaves twice. No answer lets mail leave
//! unasked from then on.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::person_accounts::{self, BeginSend, SendId, SentMail};
use tilo_actions::remote::{ActionCallError, CatalogueRow, InvokeRequest, RemoteActionTarget};

use super::{
    NamedAgent, Staged, accounts_of_assistant, argument, begun_for, send_staged, sends_root,
};
use crate::workspaces::hosted_use::{DelegatedTurn, KeepersTurn};
use crate::workspaces::person_asking::kept_asks::KeptAsk;
use crate::workspaces::{WorkspaceId, Workspaces};

/// The word the answers to the questions about messages are carried out
/// under.
pub const SEND_CARRIER: &str = "mail-send";

/// Where the messages their person is asked about wait, under that person's
/// own root: apart from the begun ones, which are forgotten after an hour.
const ASKED_DIR: &str = "asked-sends";
/// What a message set apart keeps beside what was begun: what its person is
/// shown, written when it is set apart.
const ASKED_FILE: &str = "asked.json";
/// How long a message set apart is kept when no question names it: the
/// server stopped between setting it apart and asking, or its question was
/// taken back.
const UNASKED_FOR: Duration = Duration::from_secs(10 * 60);
/// What a line break of a message is shown as, so that every row of a
/// question is one line.
const LINE_BREAK: &str = " \u{23ce} ";
/// What an answer is told when nothing waits under its question.
const NO_LONGER_WAITS: &str = "That message no longer waits to be sent";

/// How a mail request of a turn this server carries out went.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnMail {
    /// It was answered: what its action answers, never a sign-in.
    Done(serde_json::Value),
    /// A message would leave. Nothing left: it is set apart, and its person
    /// is to be asked what this shows.
    Asks(Box<AskedSend>),
}

/// A message set apart while its person is asked whether it may leave: what
/// the question shows, and how the message is found again. Nothing in it is
/// a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedSend {
    /// The message, by the id it was begun under.
    pub send: String,
    /// The account it leaves from.
    pub account: String,
    /// The person asked: their root key, hex.
    pub person: String,
    /// The workspace the turn that asked works in, by its id.
    pub workspace: String,
    /// What the person is shown, row by row: each by its label and what it
    /// says.
    pub rows: Vec<(String, String)>,
    /// The root assistant whose account binding this send inherited when a
    /// sub-agent with no direct binding asked. `None` when the asking agent
    /// was named directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_agent: Option<String>,
}

impl AskedSend {
    /// The question's words: what is asked, then every row on a line of its
    /// own. A line break inside a row is shown as a mark, so nothing a
    /// message says reads as a row of the question.
    #[must_use]
    pub fn words(&self) -> String {
        let mut words = format!(
            "May I send this message from {}? It leaves only when you allow it here.\n",
            on_one_line(&self.account)
        );
        for (label, value) in &self.rows {
            words.push('\n');
            words.push_str(label);
            words.push_str(": ");
            words.push_str(&on_one_line(value));
        }
        words
    }
}

/// `value` on one line: each line break in it shown as a mark.
fn on_one_line(value: &str) -> String {
    let mut line = String::with_capacity(value.len());
    let mut after_return = false;
    for character in value.chars() {
        let breaks = matches!(
            character,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        // A return and the line feed after it are one break.
        let second_of_pair = character == '\n' && after_return;
        if breaks && !second_of_pair {
            line.push_str(LINE_BREAK);
        } else if !breaks {
            line.push(character);
        }
        after_return = character == '\r';
    }
    line
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

/// Where the message begun as `send` waits under `root` while its person is
/// asked, for the workspace whose id is `workspace`.
fn asked_dir(root: &Path, workspace: &str, send: &str) -> Result<PathBuf, String> {
    let names = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    };
    if !names(workspace) || !names(send) {
        return Err(NO_LONGER_WAITS.to_owned());
    }
    Ok(root.join(ASKED_DIR).join(workspace).join(send))
}

/// Set the begun message in `dir` apart for `asked`, under `root`: no
/// action that goes on with a begun message finds it any more.
fn set_apart(root: &Path, dir: &Path, asked: &AskedSend) -> Result<(), String> {
    let failed = |error: std::io::Error| {
        format!("the message could not be kept for its person's answer: {error}")
    };
    let waits = asked_dir(root, &asked.workspace, &asked.send)?;
    if let Some(parent) = waits.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::rename(dir, &waits).map_err(failed)?;
    // Written now, so the moment it was set apart is the directory's own.
    let shown = serde_json::to_vec(asked).map_err(|error| error.to_string())?;
    std::fs::write(waits.join(ASKED_FILE), shown).map_err(failed)
}

impl Workspaces {
    /// Whose turn `turn` is, the id of its workspace as a grant is kept by
    /// it, the agent id holding the mail account binding (the turn's own
    /// agent, or its verified root assistant when a sub-agent has no direct
    /// binding), and the mail accounts named for that agent there.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a turn that is not its keeper's
    /// own, and for an agent (and any verified root assistant) its person
    /// named for no mail account in this workspace.
    async fn mail_of_turn(
        &self,
        turn: DelegatedTurn<'_>,
    ) -> Result<(KeepersTurn, String, String, BTreeSet<String>), ActionCallError> {
        let using = self.keepers_turn(&turn.turn).await?;
        // The person's word is kept by the workspace's id, as their device
        // gave it from that workspace.
        let workspace = turn.turn.workspace.to_string();
        let root = self.person_settings_root(&using.person);
        let kept = accounts::AccountStore::load_in(&root);
        // Named for a mail account: one named for a calendar alone reaches
        // nothing of mail.
        let mail_accounts_for = |agent: &str| -> BTreeSet<String> {
            let named = NamedAgent {
                workspace: &workspace,
                agent,
            };
            accounts_of_assistant(&root, named)
                .into_iter()
                .filter(|account| {
                    kept.get(&accounts::AccountId::from(account.clone()))
                        .is_some_and(|kept| kept.type_id.as_str() == accounts::email::TYPE_ID)
                })
                .collect()
        };
        let mut account_agent = using.agent.clone();
        let mut named_for = mail_accounts_for(&account_agent);
        if named_for.is_empty()
            && let Some(root_agent) = turn
                .root_agent
                .filter(|id| !id.is_empty() && *id != using.agent)
        {
            let inherited = mail_accounts_for(root_agent);
            if !inherited.is_empty() {
                account_agent = root_agent.to_owned();
                named_for = inherited;
            }
        }
        if named_for.is_empty() {
            return Err(ActionCallError::UnknownAction);
        }
        Ok((using, workspace, account_agent, named_for))
    }

    /// The mail actions `turn` is offered, as a catalogue lists them: each
    /// one it would not be refused. None for a turn that is not its keeper's
    /// own and for an agent its person named for no mail account in this
    /// workspace; a narrowed agent is offered the ones a rule of its names.
    pub async fn mail_offered<'a>(&self, turn: impl Into<DelegatedTurn<'a>>) -> Vec<CatalogueRow> {
        let turn = turn.into();
        let Ok((using, ..)) = self.mail_of_turn(turn).await else {
            return Vec::new();
        };
        Self::assistant_mail_rows()
            .into_iter()
            .filter(|row| {
                row.action.as_ref().is_some_and(|action| {
                    let target = RemoteActionTarget {
                        provider: row.provider.clone(),
                        address: action.address.clone(),
                    };
                    turn.turn.may_take(&using.workspace, &target)
                })
            })
            .collect()
    }

    /// Answer the mail request `request` of `turn`.
    ///
    /// Everything but a send is answered as the named agent is answered
    /// behind a device of its person. A send is not carried out: the
    /// message is set apart and [`TurnMail::Asks`] answers what its person
    /// is to be shown. The caller puts that question to them, or says that
    /// it could not ([`Self::send_not_asked`]).
    ///
    /// # Errors
    /// The turn is none this server carries out for the account's person or
    /// its agent is not named for the account
    /// ([`ActionCallError::UnknownAction`]), its agent is narrowed and holds no
    /// rule for the action ([`ActionCallError::NotPermitted`]), the arguments do
    /// not read, the mail server refused, or a message to send cannot be
    /// shown whole to its person.
    pub async fn mail_for_turn<'a>(
        &self,
        turn: impl Into<DelegatedTurn<'a>>,
        request: &InvokeRequest,
    ) -> Result<TurnMail, ActionCallError> {
        let turn = turn.into();
        let target = &request.target;
        if target.provider != person_accounts::PROVIDER
            || target.address.resource != person_accounts::RESOURCE
        {
            return Err(ActionCallError::UnknownAction);
        }
        let (using, workspace, account_agent, named_for) = self.mail_of_turn(turn).await?;
        if !turn.turn.may_take(&using.workspace, target) {
            return Err(ActionCallError::NotPermitted);
        }
        let named = NamedAgent {
            workspace: &workspace,
            agent: &account_agent,
        };
        if target.address.action != person_accounts::SEND {
            return self
                .assistant_invoke(using.person, named, turn.turn.caller, request)
                .await
                .map(TurnMail::Done);
        }
        let root = self.person_settings_root(&using.person);
        let sending: SendId = argument(request, "a message to send")?;
        let (dir, beginning) = begun_for(&root, &sending.send)?;
        if !named_for.contains(&beginning.message.account) {
            return Err(ActionCallError::UnknownAction);
        }
        let account = beginning.message.account.clone();
        // Read once, as it would leave. One that cannot be shown whole is
        // refused here, before anybody is asked, and stays begun.
        let rows = Staged::read(dir.clone(), beginning)
            .and_then(|staged| staged.shown())
            .map(|shown| shown.rows().to_vec())
            .map_err(refused)?;
        let asked = AskedSend {
            send: sending.send,
            account,
            person: hex::encode(using.person),
            workspace,
            rows,
            named_agent: (account_agent != using.agent).then_some(account_agent),
        };
        self.forget_unasked_sends(&root, &using.workspace, &asked.workspace);
        set_apart(&root, &dir, &asked).map_err(refused)?;
        Ok(TurnMail::Asks(Box::new(asked)))
    }

    /// The question about `asked` could not be put to its person: the
    /// message is a begun one again, for its agent to ask once more or to
    /// keep as a draft.
    pub fn send_not_asked(&self, asked: &AskedSend) {
        let mut person = [0; 32];
        if hex::decode_to_slice(&asked.person, &mut person).is_err() {
            return;
        }
        let root = self.person_settings_root(&person);
        let Ok(waits) = asked_dir(&root, &asked.workspace, &asked.send) else {
            return;
        };
        let begun = std::fs::remove_file(waits.join(ASKED_FILE))
            .and_then(|()| std::fs::rename(&waits, sends_root(&root).join(&asked.send)));
        if let Err(error) = begun {
            tracing::warn!(%error, "mail: a message nobody was asked about is not begun again");
        }
    }

    /// Carry out its person's answer to the question about the message
    /// `ask` keeps: `allowed` when they allowed it. Whoever calls read the
    /// answer from what the person wrote themselves, and claimed the ask.
    ///
    /// Allowed, the message that waited is read again and leaves as it was
    /// shown: answered what became of its copy in the account's Sent folder.
    /// Not allowed, nothing leaves: `None`. Whatever comes of the answer,
    /// what waited is gone, and a second reader of the same answer finds
    /// nothing.
    ///
    /// # Errors
    /// Why nothing left, in words its person reads: nothing waits under
    /// that question, the agent is no longer named for the account, what
    /// waited no longer shows what they were shown, or the mail server
    /// refused.
    pub async fn send_asked(
        &self,
        ask: &KeptAsk,
        allowed: bool,
    ) -> Result<Option<SentMail>, String> {
        let gone = || NO_LONGER_WAITS.to_owned();
        let asked: AskedSend = serde_json::from_value(ask.kept.clone()).map_err(|_| gone())?;
        let here = WorkspaceId::parse(&ask.workspace).map(|id| id.to_string());
        if ask.carrier != SEND_CARRIER
            || asked.person != ask.person
            || here.as_deref() != Some(asked.workspace.as_str())
        {
            return Err(gone());
        }
        let mut person = [0; 32];
        hex::decode_to_slice(&ask.person, &mut person).map_err(|_| gone())?;
        let root = self.person_settings_root(&person);
        // Taken from where it waited before anything is done with it.
        let waits = asked_dir(&root, &asked.workspace, &asked.send)?;
        let taken = waits.with_extension("leaving");
        std::fs::rename(&waits, &taken)
            .and_then(|()| std::fs::remove_file(taken.join(ASKED_FILE)))
            .map_err(|_| gone())?;
        let left = if allowed {
            self.leave_as_shown(&root, &taken, &asked, &ask.agent)
                .await
                .map(Some)
        } else {
            Ok(None)
        };
        match std::fs::remove_dir_all(&taken) {
            Ok(()) => {}
            // A message that left took its files with it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "mail: an answered message's files were not removed");
            }
        }
        left
    }

    /// Hand the message in `taken` to its account's server, if it still
    /// shows what `asked` showed its person and `agent` is still named for
    /// the account.
    async fn leave_as_shown(
        &self,
        root: &Path,
        taken: &Path,
        asked: &AskedSend,
        agent: &str,
    ) -> Result<SentMail, String> {
        let gone = |_| NO_LONGER_WAITS.to_owned();
        let meta = std::fs::read(taken.join("meta.json")).map_err(gone)?;
        let beginning: BeginSend =
            serde_json::from_slice(&meta).map_err(|_| NO_LONGER_WAITS.to_owned())?;
        // Its person may have taken their word back since they were asked.
        let named = NamedAgent {
            workspace: &asked.workspace,
            agent: asked.named_agent.as_deref().unwrap_or(agent),
        };
        if beginning.message.account != asked.account
            || !accounts_of_assistant(root, named).contains(&asked.account)
        {
            return Err(
                "This assistant is no longer named for that account, so nothing was sent.".into(),
            );
        }
        let staged = Staged::read(taken.to_path_buf(), beginning)?;
        if staged.shown()?.rows() != asked.rows.as_slice() {
            return Err("What waited is not what you were shown, so nothing was sent.".into());
        }
        send_staged(self.mail_session(), root, staged).await
    }

    /// Forget the messages set apart under `root` for the workspace whose
    /// id is `id` that no question waiting in `workspace` names and that
    /// were set apart a while ago: the server stopped before it asked, or
    /// the question was taken back.
    fn forget_unasked_sends(&self, root: &Path, workspace: &str, id: &str) {
        let Ok(entries) = std::fs::read_dir(root.join(ASKED_DIR).join(id)) else {
            return;
        };
        let asked_about: BTreeSet<String> = self
            .open_asks(workspace)
            .into_iter()
            .filter(|ask| ask.carrier == SEND_CARRIER)
            .filter_map(|ask| Some(ask.kept.get("send")?.as_str()?.to_owned()))
            .collect();
        let now = tilo_clock::wall();
        for entry in entries.flatten() {
            let named = entry
                .file_name()
                .to_str()
                .is_some_and(|send| asked_about.contains(send));
            let old = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|set_apart| now.duration_since(set_apart).ok())
                .is_some_and(|age| age > UNASKED_FOR);
            if !named
                && old
                && let Err(error) = std::fs::remove_dir_all(entry.path())
            {
                tracing::warn!(%error, "mail: a message nobody is asked about was not removed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in unit tests panicking is the assertion"
    )]

    use std::sync::Arc;

    use serde_json::{Value, json};
    use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
    use tilo_actions::InvocationId;
    use tilo_actions::remote::person_accounts::{
        self, AssistantAccess, BeginSend, FetchedMail, FetchedMessage, MailAccounts, SendMail,
        StartedSend,
    };
    use tilo_actions::remote::person_asking::Words;
    use tilo_actions::remote::{
        ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    use tilo_user_identity::DeviceCert;

    use super::{AskedSend, SEND_CARRIER, TurnMail};
    use crate::workspaces::hosted_use::HostedTurn;
    use crate::workspaces::mail_watch::testing::{Watched, keep};
    use crate::workspaces::person_asking::kept_asks::NewAsk;
    use crate::workspaces::person_settings::say_assistant;
    use crate::workspaces::tests::{governor, test_host as test_server};

    const ACCOUNT: &str = "ada@example.test";
    const PASSWORD: &str = "adas-password";
    const ROOT_AGENT: &str = "scout";
    const SUB_AGENT: &str = "sub-summarizer";

    fn on_turn(person: &DeviceCert, workspace: &str, agent: &str) -> Caller {
        Caller::authenticated(person.user_public_key, person.device_public_key)
            .in_workspace(workspace)
            .delegated_to(agent)
            .named(agent)
            .in_agent_session("agent-session:test")
            .on_turn(
                tilo_action_policy::turn::Turn::new("direct", "cause"),
                Vec::new(),
            )
    }

    fn asking(action: &str, argument: Value) -> InvokeRequest {
        let Value::Object(argument) = argument else {
            panic!("arguments must be a JSON object");
        };
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: person_accounts::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(person_accounts::RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn lunch() -> Value {
        serde_json::to_value(BeginSend {
            message: SendMail {
                account: ACCOUNT.to_owned(),
                from_address: ACCOUNT.to_owned(),
                to: "bo@example.test".to_owned(),
                body: "Friday at one?".to_owned(),
                ..SendMail::default()
            },
            ..BeginSend::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn sub_agent_inherits_root_mail_accounts_respects_bounds_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_server(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let watched = Arc::new(Watched::answering());
        workspaces.carry_mail_through(watched.clone());
        let created = workspaces.create("Team").await.unwrap();
        let workspace = created.workspace;
        let workspace_id = created.workspace_id;
        let ada = &owner;
        let root_dir = workspaces.person_settings_root(&ada.user_public_key);
        keep(&root_dir, ACCOUNT, PASSWORD);

        // Name ONLY the root assistant for the mail account.
        say_assistant(
            &root_dir,
            &workspace_id.to_string(),
            &AssistantAccess {
                account: ACCOUNT.to_owned(),
                agent: ROOT_AGENT.to_owned(),
                may: true,
            },
        )
        .unwrap();

        let sub_caller = on_turn(ada, &workspace, SUB_AGENT);
        let keeper = hex::encode(ada.user_public_key);
        let base_turn = HostedTurn {
            workspace: workspace_id,
            caller: &sub_caller,
            keeper: &keeper,
            bound: None,
        };

        // Without a verified root agent, the sub-agent reaches nothing.
        assert!(workspaces.mail_offered(&base_turn).await.is_empty());
        assert_eq!(
            workspaces
                .mail_for_turn(&base_turn, &asking(person_accounts::LIST, json!({})))
                .await,
            Err(ActionCallError::UnknownAction)
        );

        // With the verified root assistant, the sub-agent inherits the mail account.
        let sub_turn = base_turn.under_root(Some(ROOT_AGENT));
        let offered: Vec<String> = workspaces
            .mail_offered(&sub_turn)
            .await
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect();
        assert!(offered.iter().any(|action| action == person_accounts::LIST));
        assert!(
            offered
                .iter()
                .any(|action| action == person_accounts::FETCH)
        );
        assert!(offered.iter().any(|action| action == person_accounts::READ));

        let Ok(TurnMail::Done(listed)) = workspaces
            .mail_for_turn(&sub_turn, &asking(person_accounts::LIST, json!({})))
            .await
        else {
            panic!("sub-agent lists inherited mail account");
        };
        let listed: MailAccounts = serde_json::from_value(listed).unwrap();
        assert_eq!(listed.accounts.len(), 1);
        assert_eq!(listed.accounts[0].id, ACCOUNT);

        let Ok(TurnMail::Done(fetched)) = workspaces
            .mail_for_turn(
                &sub_turn,
                &asking(
                    person_accounts::FETCH,
                    json!({"account": ACCOUNT, "folder": "inbox"}),
                ),
            )
            .await
        else {
            panic!("sub-agent fetches inherited inbox");
        };
        let fetched: FetchedMail = serde_json::from_value(fetched).unwrap();
        assert_eq!(fetched.messages.len(), 1);

        let Ok(TurnMail::Done(read_msg)) = workspaces
            .mail_for_turn(
                &sub_turn,
                &asking(
                    person_accounts::READ,
                    json!({"account": ACCOUNT, "folder": "inbox", "uid": 4, "uidValidity": 7}),
                ),
            )
            .await
        else {
            panic!("sub-agent reads inherited message");
        };
        let read_msg: FetchedMessage = serde_json::from_value(read_msg).unwrap();
        assert_eq!(read_msg.body, "Friday at one?");

        // Explicit narrowing still applies to the sub-agent.
        let narrow_rules = vec![Rule::new(
            TreePath::workspace(&workspace)
                .app(person_accounts::PROVIDER)
                .kind(person_accounts::RESOURCE)
                .resource(person_accounts::RESOURCE)
                .action(person_accounts::LIST),
            KindSet::of(ActionKind::Outside),
            Answer::Allow,
        )];
        let narrowed_sub = HostedTurn {
            bound: Some(&narrow_rules),
            ..base_turn
        }
        .under_root(Some(ROOT_AGENT));
        let narrowed_offered: Vec<String> = workspaces
            .mail_offered(&narrowed_sub)
            .await
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect();
        assert_eq!(narrowed_offered, vec![person_accounts::LIST]);
        assert_eq!(
            workspaces
                .mail_for_turn(
                    &narrowed_sub,
                    &asking(
                        person_accounts::FETCH,
                        json!({"account": ACCOUNT, "folder": "inbox"}),
                    ),
                )
                .await,
            Err(ActionCallError::NotPermitted)
        );

        // External sends still ask the person and never leave unasked.
        let Ok(TurnMail::Done(begun)) = workspaces
            .mail_for_turn(&sub_turn, &asking(person_accounts::BEGIN_SEND, lunch()))
            .await
        else {
            panic!("sub-agent begins send");
        };
        let started: StartedSend = serde_json::from_value(begun).unwrap();
        let Ok(TurnMail::Asks(asked_send)) = workspaces
            .mail_for_turn(
                &sub_turn,
                &asking(person_accounts::SEND, json!({"send": started.send})),
            )
            .await
        else {
            panic!("sub-agent send must ask person first");
        };
        assert_eq!(asked_send.named_agent.as_deref(), Some(ROOT_AGENT));
        assert!(
            !watched.asked().iter().any(|call| call.starts_with("send")),
            "nothing leaves before the person allows it"
        );

        // Revoking the root assistant immediately cuts off both new calls and pending sends.
        say_assistant(
            &root_dir,
            &workspace_id.to_string(),
            &AssistantAccess {
                account: ACCOUNT.to_owned(),
                agent: ROOT_AGENT.to_owned(),
                may: false,
            },
        )
        .unwrap();
        assert!(workspaces.mail_offered(&sub_turn).await.is_empty());
        assert_eq!(
            workspaces
                .mail_for_turn(&sub_turn, &asking(person_accounts::LIST, json!({})))
                .await,
            Err(ActionCallError::UnknownAction)
        );

        let kept = workspaces
            .keep_ask(
                &ada.user_public_key,
                NewAsk {
                    workspace: workspace.clone(),
                    agent: SUB_AGENT.to_owned(),
                    name: SUB_AGENT.to_owned(),
                    carrier: SEND_CARRIER.to_owned(),
                    kept: serde_json::to_value(&*asked_send).unwrap(),
                    always: None,
                    words: Words::default(),
                    asked_in: Value::Null,
                },
            )
            .unwrap()
            .unwrap();
        let claimed = workspaces.claim_ask(&kept.ask).unwrap();
        let refused = workspaces.send_asked(&claimed, true).await;
        assert!(
            refused.is_err(),
            "revoking root must block pending sub-agent send: {refused:?}"
        );

        // Re-granting root lets a fresh approved send leave.
        say_assistant(
            &root_dir,
            &workspace_id.to_string(),
            &AssistantAccess {
                account: ACCOUNT.to_owned(),
                agent: ROOT_AGENT.to_owned(),
                may: true,
            },
        )
        .unwrap();
        let Ok(TurnMail::Done(begun2)) = workspaces
            .mail_for_turn(&sub_turn, &asking(person_accounts::BEGIN_SEND, lunch()))
            .await
        else {
            panic!("sub-agent begins second send");
        };
        let started2: StartedSend = serde_json::from_value(begun2).unwrap();
        let Ok(TurnMail::Asks(asked_send2)) = workspaces
            .mail_for_turn(
                &sub_turn,
                &asking(person_accounts::SEND, json!({"send": started2.send})),
            )
            .await
        else {
            panic!("second send asks person first");
        };
        let asked_copy: AskedSend = (*asked_send2).clone();
        let kept2 = workspaces
            .keep_ask(
                &ada.user_public_key,
                NewAsk {
                    workspace: workspace.clone(),
                    agent: SUB_AGENT.to_owned(),
                    name: SUB_AGENT.to_owned(),
                    carrier: SEND_CARRIER.to_owned(),
                    kept: serde_json::to_value(&asked_copy).unwrap(),
                    always: None,
                    words: Words::default(),
                    asked_in: Value::Null,
                },
            )
            .unwrap()
            .unwrap();
        let claimed2 = workspaces.claim_ask(&kept2.ask).unwrap();
        workspaces
            .send_asked(&claimed2, true)
            .await
            .expect("approved send leaves while root remains named");
        assert!(
            watched.asked().iter().any(|call| call.starts_with("send")),
            "approved send left through mail server"
        );

        server.shutdown().await;
    }
}
