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
use tilo_action_line::LineClient;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ItemRef, ReceiptStatus,
    VAULT_FIND, VAULT_KEEP, VAULT_RELEASE, vault_target,
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
        let handed = tilo_action_line::handed::connection()?;
        Some(Self {
            line: handed.line,
            workspace: handed.workspace,
        })
    }

    /// The store over an already open connection, as a test stands one up.
    #[cfg(test)]
    pub(crate) fn over(line: Arc<LineClient>, workspace: &str) -> Self {
        Self {
            line,
            workspace: workspace.to_owned(),
        }
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
        #[derive(serde::Deserialize)]
        struct Found {
            reference: Option<ItemRef>,
        }
        #[derive(serde::Deserialize)]
        struct Released {
            secret: String,
        }
        let found: Found = self.answer(
            vault_target(VAULT_FIND),
            json!({ "app": app, "purpose": key }),
            "the item a key names",
        )?;
        let Some(reference) = found.reference else {
            return Ok(None);
        };
        let released: Released = self.answer(
            reference.target(VAULT_RELEASE),
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
        let _: Value = self.answer(
            vault_target(VAULT_KEEP),
            json!({ "purpose": key, "value": value }),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

    use serde::Deserialize;
    use tilo_action_line::serve_lines;
    use tilo_actions::ActionOutput;
    use tilo_actions::remote::{
        ActionProvider, CataloguePage, CatalogueQuery, HostedContentPuts, InvocationReceipt,
        Pending,
    };

    use super::*;

    /// The vault as an app's own connection reaches it: items kept per purpose, released only by
    /// the reference `find` answered.
    #[derive(Clone, Default)]
    struct FakeVault {
        items: Arc<Mutex<HashMap<String, (ItemRef, String)>>>,
        next: Arc<std::sync::atomic::AtomicU32>,
    }

    impl FakeVault {
        fn item(id: u32) -> ItemRef {
            ItemRef::parse(&format!("vault.item:{id:032x}")).expect("an item reference")
        }
    }

    impl ActionProvider for FakeVault {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let arguments = match &request.argument {
                tilo_actions::remote::InvokeArgument::Json(fields) => Value::Object(fields.clone()),
                _ => Value::Null,
            };
            let action = request.target.address.action.as_str();
            let answer = if action == VAULT_KEEP {
                #[derive(Deserialize)]
                struct Keeping {
                    purpose: String,
                    value: String,
                }
                let keeping: Keeping =
                    serde_json::from_value(arguments).expect("a keep's arguments");
                let mut items = self.items.lock().expect("the fake vault");
                let reference = items.get(&keeping.purpose).map_or_else(
                    || {
                        Self::item(
                            self.next
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                .saturating_add(1),
                        )
                    },
                    |(reference, _)| reference.clone(),
                );
                items.insert(keeping.purpose, (reference.clone(), keeping.value));
                drop(items);
                json!({ "reference": reference })
            } else if action == VAULT_FIND {
                let purpose = arguments["purpose"].as_str().expect("a find's purpose");
                let reference = self
                    .items
                    .lock()
                    .expect("the fake vault")
                    .get(purpose)
                    .map(|(reference, _)| reference.clone());
                json!({ "reference": reference })
            } else {
                let reference = ItemRef::addressed(&request.target, VAULT_RELEASE)
                    .expect("a release of an item");
                let secret = self
                    .items
                    .lock()
                    .expect("the fake vault")
                    .values()
                    .find(|(kept, _)| *kept == reference)
                    .map(|(_, secret)| secret.clone())
                    .expect("a kept item");
                json!({ "secret": secret })
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(answer),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// `vault` served to a client over a socket pair, as a daemon's connection is.
    fn connected(vault: FakeVault) -> VaultStore {
        let (near, far) = UnixStream::pair().expect("a socket pair");
        std::thread::spawn(move || {
            let output = far.try_clone().expect("the other end");
            serve_lines(
                |_| {
                    Ok((
                        vault.clone(),
                        Caller::local_app("so.tilo.notes").in_workspace("w"),
                    ))
                },
                &HostedContentPuts::default(),
                None,
                BufReader::new(far),
                output,
            );
        });
        VaultStore::over(
            Arc::new(LineClient::of_stream(near).expect("a client")),
            "w",
        )
    }

    #[test]
    fn a_secret_is_kept_found_and_released_over_the_apps_own_connection() {
        let store = connected(FakeVault::default());
        assert_eq!(store.get("so.tilo.notes", "oauth-token").unwrap(), None);
        store
            .put("so.tilo.notes", "oauth-token", b"token-1", "Notes token")
            .unwrap();
        let secret = store
            .get("so.tilo.notes", "oauth-token")
            .unwrap()
            .expect("the secret it kept");
        assert_eq!(secret.expose(), b"token-1");
        // Keeping the same purpose again renews that item rather than adding another.
        store
            .put("so.tilo.notes", "oauth-token", b"token-2", "")
            .unwrap();
        assert_eq!(
            store
                .get("so.tilo.notes", "oauth-token")
                .unwrap()
                .expect("the renewed secret")
                .expose(),
            b"token-2"
        );
        assert_eq!(store.get("so.tilo.notes", "another").unwrap(), None);
    }

    #[test]
    fn a_secret_is_text_and_is_forgotten_in_passwords() {
        let store = connected(FakeVault::default());
        let refused = store
            .put("so.tilo.notes", "avatar", &[0xff, 0xfe], "")
            .expect_err("a secret that is not text");
        assert!(refused.to_string().contains("text"), "{refused}");
        let refused = store
            .delete("so.tilo.notes", "oauth-token")
            .expect_err("an app does not forget a vault item");
        assert!(refused.to_string().contains("Passwords"), "{refused}");
    }
}
