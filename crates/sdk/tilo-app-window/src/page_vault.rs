//! The workspace's vault, as a window asks it for its page's logins
//! ([`tilo_page_engine::LoginVault`]).
//!
//! The launcher connected this process to the device's daemon before its sandbox started
//! (`tilo_action_line::handed`), so the daemon knows the connection as the process it launched
//! for the app, and answers the vault's page login actions for that process alone
//! (`tilo_actions::remote::VAULT_PAGE_LOGINS`). A password goes between this window's
//! engine and the vault over that connection, and nowhere else.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_action_line::LineClient;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ItemRef, ReceiptStatus,
    RemoteActionTarget, VAULT_FORGET_PAGE_LOGIN, VAULT_KEEP_PAGE_LOGIN, VAULT_PAGE_FILL,
    VAULT_PAGE_LOGINS, vault_target,
};
use tilo_page_engine::{Answered, LoginVault, VaultLogin};
use tilo_webview::ReleasedText;

/// What a window the daemon did not start, or whose connection is gone, is told.
const NO_DAEMON: &str = "This window has no connection to your computer's daemon, so it cannot \
                         reach your workspace's vault";
/// What a call the daemon left unanswered is told.
const UNANSWERED: &str = "Your computer's daemon did not answer";

/// This process's connection to the daemon, the one the launcher handed it: `None` where it
/// handed none.
fn line() -> Option<(Arc<LineClient>, String)> {
    tilo_action_line::handed::connection().map(|handed| (handed.line, handed.workspace))
}

/// The vault this window asks on its own connection, waking the window with `wake` as each call
/// answers.
pub struct NativeVault {
    wake: ui::WakeHandle,
}

impl NativeVault {
    #[must_use]
    pub fn new(wake: ui::WakeHandle) -> Self {
        Self { wake }
    }

    /// Call `target` with `argument`, answering what it said through `done`, then waking the
    /// window.
    fn call(
        &self,
        target: RemoteActionTarget,
        argument: Value,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) {
        let Some((line, workspace)) = line() else {
            done(Err(NO_DAEMON.to_owned()));
            return;
        };
        let request = InvokeRequest::plain(
            target,
            match argument {
                Value::Object(fields) => fields,
                _ => serde_json::Map::new(),
            },
        );
        // The daemon decides who calls by the connection: the call names nobody.
        let caller = Caller::local_person("").in_workspace(&workspace);
        let wake = self.wake.clone();
        line.invoke(&caller, request).then(move |answer| {
            let said = |error: ActionCallError| match error {
                ActionCallError::Refused { reason } => reason,
                other => other.to_string(),
            };
            done(match answer {
                None => Err(UNANSWERED.to_owned()),
                Some(Err(error)) => Err(said(error)),
                Some(Ok(receipt)) => match receipt.status {
                    ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
                    ReceiptStatus::Failed { error } => Err(error.message),
                    ReceiptStatus::Rejected { error } => Err(said(error)),
                    ReceiptStatus::Cancelled { .. } => Err("The call was stopped".to_owned()),
                    ReceiptStatus::Accepted => Err(UNANSWERED.to_owned()),
                },
            });
            wake.wake();
        });
    }
}

/// `value` as the logins it lists.
fn listed(value: Result<Value, String>) -> Result<Vec<VaultLogin>, String> {
    #[derive(serde::Deserialize)]
    struct PageLogins {
        logins: Vec<PageLogin>,
    }
    #[derive(serde::Deserialize)]
    struct PageLogin {
        reference: ItemRef,
        title: String,
        username: String,
        own: bool,
    }
    let logins: PageLogins = serde_json::from_value(value?)
        .map_err(|_| "The vault answered what is no list of logins".to_owned())?;
    Ok(logins
        .logins
        .into_iter()
        .map(|login| VaultLogin {
            item: login.reference.to_string(),
            title: login.title,
            username: login.username,
            own: login.own,
        })
        .collect())
}

/// The item `item` names, or why it names none.
fn item_of(item: &str) -> Result<ItemRef, String> {
    ItemRef::parse(item).ok_or_else(|| "That is no login of the vault's".to_owned())
}

impl LoginVault for NativeVault {
    fn logins(&self, site: &str, done: Answered<Vec<VaultLogin>>) {
        self.call(
            vault_target(VAULT_PAGE_LOGINS),
            json!({"site": site}),
            move |value| done(listed(value)),
        );
    }

    fn keep(
        &self,
        site: &str,
        username: &str,
        password: ReleasedText,
        replacing: Option<&str>,
        done: Answered<()>,
    ) {
        let replacing = match replacing.map(item_of).transpose() {
            Ok(replacing) => replacing,
            Err(why) => return done(Err(why)),
        };
        let Some(secret) = password.take() else {
            return done(Err("The form's password was taken already".to_owned()));
        };
        let argument = json!({
            "site": site,
            "username": username,
            "secret": String::clone(&secret),
            "replacing": replacing,
        });
        self.call(
            vault_target(VAULT_KEEP_PAGE_LOGIN),
            argument,
            move |value| done(value.map(drop)),
        );
    }

    fn forget(&self, site: &str, item: &str, done: Answered<Vec<VaultLogin>>) {
        let item = match item_of(item) {
            Ok(item) => item,
            Err(why) => return done(Err(why)),
        };
        self.call(
            vault_target(VAULT_FORGET_PAGE_LOGIN),
            json!({"site": site, "item": item}),
            move |value| done(listed(value)),
        );
    }

    fn fill(&self, site: &str, item: &str, done: Answered<(String, ReleasedText)>) {
        let item = match item_of(item) {
            Ok(item) => item,
            Err(why) => return done(Err(why)),
        };
        self.call(
            item.target(VAULT_PAGE_FILL),
            json!({"site": site}),
            move |value| {
                done(value.and_then(|value| {
                    #[derive(serde::Deserialize)]
                    struct PageFilled {
                        username: String,
                        secret: String,
                    }
                    let filled: PageFilled = serde_json::from_value(value)
                        .map_err(|_| "The vault answered what is no login".to_owned())?;
                    Ok((filled.username, ReleasedText::new(filled.secret)))
                }));
            },
        );
    }
}
