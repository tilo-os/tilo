//! A person's mail, as a turn this server carries out for them reaches it.
//!
//! An agent this server runs works with every device of whoever asked it off, so no transport
//! certified a device behind what it asks, and the check a device's request passes
//! ([`Workspaces::asking_about_mail`]) has nothing to compare. This is the server's own entry for
//! such a turn. It is reached from no transport: only the code that carries out the turn calls it,
//! with the caller the turn's tools act as and what the workspace's list of its agents says of the
//! agent ([`HostedTurn`]).
//!
//! Two checks, in this order, and neither is the whole rule. The turn is one this server carries
//! out ([`Workspaces::asked_turn`]): an agent on a turn for the person who asked it, whom the
//! workspace admits now. And the workspace's policy lets that agent, on that ask, reach its mail
//! ([`Store::MAIL`]). An agent that anything narrows (what its entry bounds it to; a sub-agent)
//! also holds a rule that names the very action ([`HostedTurn::may_take`]). Unknown and forbidden
//! are the same answer.
//!
//! What such a turn may do is what an agent may behind a device of its
//! person: list the workspace's accounts, read what arrived, read a folder
//! and a message with the files it carries by name and size, search, begin a
//! message and keep it as a draft. The account's sign-in is used inside the
//! mail session and never answered.
//!
//! A send waits for its person's yes. The message is read once, as it would
//! leave; one that cannot be shown whole is refused before anybody is asked.
//! What its person is shown ([`AskedSend`]) is kept beside the message, which
//! is set apart where no action of the agent reaches it any more, and the call
//! is refused for want of their yes: the dispatch keeps it as its asked
//! record, where it waits for as long as they take. Their approve admits the
//! same call again; the message that waited is read again, and it leaves only
//! when it still shows exactly what they were shown and its person still
//! reaches the workspace's mail. What waited is gone after it, so nothing
//! leaves twice. Nothing lets mail leave unasked: no grant is offered.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::person_accounts::{self, BeginSend, SendId, SentMail};
use tilo_actions::remote::asked::{AskedBy, AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, CatalogueRow, InvokeRequest, RemoteActionTarget};

use super::kept_sign_ins::Signing;
use super::{Staged, argument, begun_for, send_staged, sends_root};
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::workspace_stores::Store;
use crate::workspaces::{WorkspaceId, Workspaces};
use tilo_action_policy::ActionKind;

/// Where the messages their person is asked about wait, under that person's
/// own root: apart from the begun ones, which are forgotten after an hour.
const ASKED_DIR: &str = "asked-sends";
/// What a message set apart keeps beside what was begun: what its person is
/// shown, written when it is set apart.
const ASKED_FILE: &str = "asked.json";
/// How long a message set apart is kept for its person's answer: past it, its
/// question was declined or long forgotten.
const UNANSWERED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// What a line break of a message is shown as, so that every row of a
/// question is one line.
const LINE_BREAK: &str = " \u{23ce} ";
/// What an answer is told when nothing waits under its question.
const NO_LONGER_WAITS: &str = "That message no longer waits to be sent";

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
}

impl AskedSend {
    /// What its person is shown, asked by `by`: the account it leaves from, and every row on
    /// one line of its own. A line break inside a row is shown as a mark, so nothing a message
    /// says reads as a row of the question.
    #[must_use]
    pub fn facts(&self, by: AskedBy) -> AskedFacts {
        AskedFacts {
            on: Some(self.account.clone()),
            facts: self
                .rows
                .iter()
                .map(|(label, value)| Fact::new(label.clone(), on_one_line(value)))
                .collect(),
            verb: "send".to_owned(),
            ..AskedFacts::by(by)
        }
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
    /// it, and the workspace's mail accounts.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a turn this server does not carry out, for an agent
    /// the workspace's policy does not let read its mail, and in a workspace that keeps no mail
    /// account.
    async fn mail_of_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<(AskedTurn, String, BTreeSet<String>), ActionCallError> {
        let using = self.asked_turn(turn).await?;
        let workspace = turn.workspace.to_string();
        if !self
            .turn_reaches(turn, Store::MAIL, person_accounts::LIST, ActionKind::Read)
            .await
        {
            return Err(ActionCallError::UnknownAction);
        }
        let accounts: BTreeSet<String> = self
            .workspace_accounts(&workspace)
            .await
            .map_err(|reason| ActionCallError::Refused { reason })?
            .of_type(accounts::email::TYPE_ID)
            .iter()
            .map(|account| account.id.as_str().to_owned())
            .collect();
        if accounts.is_empty() {
            return Err(ActionCallError::UnknownAction);
        }
        Ok((using, workspace, accounts))
    }

    /// The mail actions `turn` is offered, as a catalogue lists them: each
    /// one it would not be refused. None for a turn this server does not carry out, for an agent
    /// that does not reach the workspace's mail and in a workspace that keeps no mail account; a
    /// narrowed agent is offered the ones a rule of its names.
    pub async fn mail_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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
                    turn.may_take(&using.workspace, &target)
                })
            })
            .collect()
    }

    /// Answer the mail request `request` of `turn`.
    ///
    /// Everything but a send is answered as an agent is answered
    /// behind a device of its person. A send waits for its person's yes: the
    /// message is set apart and the call refused for want of it, showing them
    /// [`AskedSend::facts`]. Admitted again on their approve, the message that
    /// waited leaves ([`Self::send_approved`]).
    ///
    /// # Errors
    /// The turn is none this server carries out for the account's person or
    /// its person does not reach the workspace's mail
    /// ([`ActionCallError::UnknownAction`]), its agent is narrowed and holds no
    /// rule for the action ([`ActionCallError::NotPermitted`]), the arguments do
    /// not read, the mail server refused, or a message to send cannot be
    /// shown whole to its person.
    pub async fn mail_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let target = &request.target;
        if target.provider != person_accounts::PROVIDER
            || target.address.resource != person_accounts::RESOURCE
        {
            return Err(ActionCallError::UnknownAction);
        }
        let (using, workspace, kept) = self.mail_of_turn(turn).await?;
        if !turn.may_take(&using.workspace, target) {
            return Err(ActionCallError::NotPermitted);
        }
        let root = self.workspace_root(&workspace);
        if target.address.action != person_accounts::SEND {
            return self
                .mail_read_or_draft(
                    Signing {
                        vault: self.vault_at(&workspace),
                        root: &root,
                        by: &using.agent,
                    },
                    &workspace,
                    request,
                    true,
                )
                .await;
        }
        let sending: SendId = argument(request, "a message to send")?;
        if request.approval().is_some() {
            let sent = self
                .send_approved(&root, &workspace, &using.person, &sending.send)
                .await
                .map_err(ActionCallError::refused)?;
            return serde_json::to_value(sent).map_err(|error| ActionCallError::refused(error.to_string()));
        }
        let (dir, beginning) = begun_for(&root, &sending.send)?;
        if !kept.contains(&beginning.message.account) {
            return Err(ActionCallError::UnknownAction);
        }
        let account = beginning.message.account.clone();
        // Read once, as it would leave. One that cannot be shown whole is
        // refused here, before anybody is asked, and stays begun.
        let rows = Staged::read(dir.clone(), beginning)
            .and_then(|staged| staged.shown())
            .map(|shown| shown.rows().to_vec())
            .map_err(ActionCallError::refused)?;
        let asked = AskedSend {
            send: sending.send,
            account,
            person: hex::encode(using.person),
            workspace,
            rows,
        };
        forget_unanswered_sends(&root, &asked.workspace);
        set_apart(&root, &dir, &asked).map_err(ActionCallError::refused)?;
        Err(ActionCallError::asks(asked.facts(super::super::asking::asked_by(turn.caller))))
    }

    /// Send the message begun as `send` in `workspace` (its id), which waited set apart under
    /// `root` for `person`'s yes and which they approved: what became of its copy in the
    /// account's Sent folder. Whatever comes of it, what waited is gone, and a second approve
    /// of the same call finds nothing.
    ///
    /// # Errors
    /// Why nothing left, in words its person reads: nothing waits under that message, its
    /// person no longer reaches the workspace's mail, what waited no longer shows what they
    /// were shown, or the mail server refused.
    async fn send_approved(
        &self,
        root: &Path,
        workspace: &str,
        person: &[u8; 32],
        send: &str,
    ) -> Result<SentMail, String> {
        let gone = |_| NO_LONGER_WAITS.to_owned();
        let waits = asked_dir(root, workspace, send)?;
        let asked: AskedSend = std::fs::read(waits.join(ASKED_FILE))
            .map_err(gone)
            .and_then(|kept| serde_json::from_slice(&kept).map_err(|_| NO_LONGER_WAITS.to_owned()))?;
        if asked.person != hex::encode(person) || asked.workspace != workspace {
            return Err(NO_LONGER_WAITS.to_owned());
        }
        let id = WorkspaceId::parse(workspace).ok_or_else(|| NO_LONGER_WAITS.to_owned())?;
        // Taken from where it waited before anything is done with it.
        let taken = waits.with_extension("leaving");
        std::fs::rename(&waits, &taken)
            .and_then(|()| std::fs::remove_file(taken.join(ASKED_FILE)))
            .map_err(gone)?;
        let left = self.leave_as_shown(root, &taken, &asked, (id, person)).await;
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
    /// shows what `asked` showed its person and that person still reaches
    /// the workspace's mail.
    async fn leave_as_shown(
        &self,
        root: &Path,
        taken: &Path,
        asked: &AskedSend,
        (workspace, person): (WorkspaceId, &[u8; 32]),
    ) -> Result<SentMail, String> {
        let gone = |_| NO_LONGER_WAITS.to_owned();
        let meta = std::fs::read(taken.join("meta.json")).map_err(gone)?;
        let beginning: BeginSend =
            serde_json::from_slice(&meta).map_err(|_| NO_LONGER_WAITS.to_owned())?;
        // The workspace may have taken its grant back since they were asked.
        if beginning.message.account != asked.account
            || !self
                .person_reaches(
                    workspace,
                    person,
                    Store::MAIL,
                    person_accounts::SEND,
                    ActionKind::Outside,
                )
                .await
        {
            return Err("You no longer send mail from this workspace, so nothing was sent.".into());
        }
        let staged = Staged::read(taken.to_path_buf(), beginning)?;
        if staged.shown()?.rows() != asked.rows.as_slice() {
            return Err("What waited is not what you were shown, so nothing was sent.".into());
        }
        // Their allowing it is their sending it.
        let signing = Signing {
            vault: self.vault_at(&asked.workspace),
            root,
            by: "",
        };
        send_staged(self.mail_session(), signing, staged).await
    }
}

/// Forget the messages set apart under `root` for the workspace whose id is `id` that waited
/// longer than their person is taken to answer.
fn forget_unanswered_sends(root: &Path, id: &str) {
    let Ok(entries) = std::fs::read_dir(root.join(ASKED_DIR).join(id)) else {
        return;
    };
    let now = tilo_clock::wall();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|set_apart| now.duration_since(set_apart).ok())
            .is_some_and(|age| age > UNANSWERED_FOR);
        if old && let Err(error) = std::fs::remove_dir_all(entry.path()) {
            tracing::warn!(%error, "mail: a message nobody answered was not removed");
        }
    }
}
