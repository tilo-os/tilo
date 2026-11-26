//! The workspace's accounts, and the secret each signs in with, as an app that signs in reads
//! them through the device's one dispatch.
//!
//! The app calls the published `sign_ins` action as the app it was launched as, on the
//! connection the device handed it: it lists the vault's logins with the account each signs in
//! for, and asks `vault#release` for one entry's secret, which hands it only to the app's own
//! process, decided under the grants that name the app, with the use written into the entry.
//! What the dispatch refuses is said to the person in its words, and nothing else is tried: the
//! app reads no file of accounts or credentials.
//!
//! A secret released for an entry is kept, by the account it signs in for and the entry it came
//! from, so asking again for the same entry asks the vault nothing: a second release writes a
//! second use into it.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value};

use super::person_vault::{self, ItemRef};
use super::{
    ActionProvider as _, Caller, InvokeRequest, LineClient, ReceiptStatus, RemoteActionTarget,
};

/// What the dispatch says when it no longer answers a sign-in.
pub const NOT_ANSWERED: &str = "This computer no longer answers this account's sign-in";

/// How long one call waits for its answer.
const WAIT_MS: u32 = 60_000;

/// How long past the answer's own wait a receipt is waited for.
const SLACK: Duration = Duration::from_secs(5);

/// The workspace's accounts as the app holding `link`, its connection to the device's dispatch,
/// reads them.
pub struct SigningIn {
    client: LineClient,
    caller: Caller,
    /// The entry each account's secret was last released from, and that secret.
    released: HashMap<String, (String, String)>,
}

impl SigningIn {
    /// The accounts `link`, the app's connection to the device's dispatch, tells it.
    ///
    /// # Errors
    /// The connection cannot be read.
    pub fn over(link: tilo_socket::Stream) -> std::io::Result<Self> {
        let workspace = std::env::var("TILO_WORKSPACE_ID").unwrap_or_default();
        Ok(Self {
            client: LineClient::of_stream(link)?,
            caller: Caller::local_person("").in_workspace(&workspace),
            released: HashMap::new(),
        })
    }

    /// The logins the workspace's vault keeps, each with the account it signs in for, as
    /// `sign_ins` (the accounts' `sign-ins` action, bound by the app from the surface their
    /// service publishes) answers them.
    ///
    /// # Errors
    /// The dispatch refused, or answered something else, in words a person reads.
    pub fn sign_ins<T: serde::de::DeserializeOwned>(
        &self,
        sign_ins: RemoteActionTarget,
    ) -> Result<T, String> {
        let answer = self.ask(sign_ins, Map::new())?;
        serde_json::from_value(answer)
            .map_err(|error| format!("That is no list of logins: {error}"))
    }

    /// The secret the entry `login` names releases to this process, kept while the same entry is
    /// asked for `account` again: a second release writes another use into it.
    ///
    /// # Errors
    /// The dispatch refused, in words a person reads, or answered no released secret.
    pub fn secret(&mut self, account: &str, login: &str) -> Result<String, String> {
        if let Some((entry, secret)) = self.released.get(account)
            && *entry == login
        {
            return Ok(secret.clone());
        }
        let reference = ItemRef::parse(login)
            .ok_or_else(|| format!("`{login}` names no entry of the vault"))?;
        let answer = self.ask(person_vault::release_target(&reference), Map::new())?;
        let released: person_vault::Released = serde_json::from_value(answer)
            .map_err(|error| format!("That is no released secret: {error}"))?;
        self.released.insert(
            account.to_owned(),
            (login.to_owned(), released.secret.clone()),
        );
        Ok(released.secret)
    }

    /// Invoke `target` with `arguments` and answer what it answered, or the refusal in words a
    /// person reads.
    fn ask(
        &self,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let mut request = InvokeRequest::plain(target, arguments);
        request.wait_ms = Some(WAIT_MS);
        let receipt = self
            .client
            .invoke(&self.caller, request)
            .wait_timeout(Duration::from_millis(u64::from(WAIT_MS)).saturating_add(SLACK))
            .ok()
            .flatten()
            .ok_or_else(|| NOT_ANSWERED.to_owned())?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(error.message),
            ReceiptStatus::Rejected { error } => Err(error.to_string()),
            ReceiptStatus::Cancelled { message } => Err(message.unwrap_or_default()),
            ReceiptStatus::Accepted => Err("The call was only accepted".to_owned()),
        }
    }
}
