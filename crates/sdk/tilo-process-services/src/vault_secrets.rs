//! The workspace's vault as an app's secret store, over the connection its launcher handed it.
//!
//! An app process holds one connection to the device's daemon (`TILO_SVC_NATIVE_FD`, in the
//! workspace `TILO_WORKSPACE_ID` names), and the daemon decides by that connection whose calls
//! they are: the app is the one the connection was placed for, never a name an argument carries.
//! A secret is kept with `vault#keep` under the purpose the key names, found again by that same
//! purpose (`vault#find`), and handed back by `vault#release`. Nothing here holds a secret longer
//! than the call that read it.
//!
//! A host process has no such connection and keeps the operating system's keychain instead
//! (`tilo_secrets`, built with `host-keyring`); [`crate::process`]'s system secrets pick before
//! either is asked.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::remote::person_vault::{self, FindItem, Found, KeepValue, Released};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, LineClient, ReceiptStatus,
};

use tilo_secrets::{Protection, Secret, SecretBackend, SecretError};

/// How long one call may wait for the daemon's answer: a release or keep the person is asked
/// about answers after they do, and a call nobody answers is a failure rather than a hang.
const WAIT: Duration = Duration::from_secs(30);

/// The workspace's vault as this app process keeps its own secrets in.
pub(crate) struct VaultStore {
    line: Arc<LineClient>,
    workspace: String,
}

impl VaultStore {
    /// The store of a process the launcher handed a connection to its daemon, on that one
    /// connection: `None` for a host process, which has none.
    pub(crate) fn handed() -> Option<Self> {
        let handed = tilo_actions::remote::handed::connection()?;
        Some(Self {
            line: handed.line,
            workspace: handed.workspace,
        })
    }

    /// Call `target` with `arguments` on this process's connection, and read what its action
    /// answers as `T`.
    fn answer<T: serde::de::DeserializeOwned>(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        arguments: Value,
        what: &str,
    ) -> Result<T, SecretError> {
        let request = InvokeRequest::plain(
            target,
            match arguments {
                Value::Object(fields) => fields,
                _ => Map::new(),
            },
        );
        // The daemon decides who calls by the connection: the call names nobody.
        let caller = Caller::local_person("").in_workspace(&self.workspace);
        let receipt = match self.line.invoke(&caller, request).wait_timeout(WAIT) {
            Ok(Some(receipt)) => receipt.map_err(refused),
            Ok(None) => return Err(unanswered()),
            Err(_) => {
                return Err(SecretError::Failed(
                    "the computer's daemon did not answer in time".to_owned(),
                ));
            }
        }?;
        let value = match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or(Value::Null),
            ReceiptStatus::Failed { error } => return Err(SecretError::Failed(error.message)),
            ReceiptStatus::Rejected { error } => return Err(refused(error)),
            ReceiptStatus::Cancelled { .. } => {
                return Err(SecretError::Failed("the call was stopped".to_owned()));
            }
            ReceiptStatus::Accepted => return Err(unanswered()),
        };
        serde_json::from_value(value)
            .map_err(|error| SecretError::Failed(format!("{what} could not be read: {error}")))
    }
}

impl SecretBackend for VaultStore {
    fn protection(&self) -> Protection {
        Protection::WorkspaceVault
    }

    fn probe(&self) -> Result<(), SecretError> {
        Ok(())
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        let looking = FindItem {
            app: app.to_owned(),
            purpose: key.to_owned(),
        };
        let found: Found = self.answer(
            person_vault::target(person_vault::FIND),
            json!(looking),
            "the item a key names",
        )?;
        let Some(reference) = found.reference else {
            return Ok(None);
        };
        let released: Released = self.answer(
            person_vault::release_target(&reference),
            json!({}),
            "the secret the vault keeps",
        )?;
        Ok(Some(Secret::new(released.secret.into_bytes())))
    }

    fn put(&self, _app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        // The vault keeps a value item as text; every secret an app keeps is a token, a key or a
        // passphrase, and a value that is not text has no item to be kept as.
        let value = std::str::from_utf8(value)
            .map_err(|_| SecretError::Failed("a secret the vault keeps is text".to_owned()))?;
        let keeping = KeepValue {
            purpose: key.to_owned(),
            value: value.to_owned(),
            replacing: None,
        };
        let _: Value = self.answer(
            person_vault::keep_target(),
            json!(keeping),
            "the item the secret is kept as",
        )?;
        Ok(())
    }

    fn delete(&self, _app: &str, _key: &str) -> Result<(), SecretError> {
        Err(SecretError::Failed(
            "a secret kept in the workspace's vault is forgotten in Passwords, not by the app \
             that kept it"
                .to_owned(),
        ))
    }
}

/// What a call the daemon left unanswered is told.
fn unanswered() -> SecretError {
    SecretError::Failed("the computer's daemon did not answer".to_owned())
}

/// A call the vault refused, in the words it refused it with.
fn refused(error: ActionCallError) -> SecretError {
    match error {
        ActionCallError::Refused { reason } => SecretError::Failed(reason),
        ActionCallError::NotPermitted => SecretError::Denied,
        other => SecretError::Failed(other.to_string()),
    }
}
