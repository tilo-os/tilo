//! Private Apple sign-in state; only acknowledged canonical account IDs leave.
use crate::account_transfer::Connector;
use account_custody::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use zeroize::{Zeroize, Zeroizing};

enum Outcome {
    Code(ICloudChallenge),
    Connected(accounts::AccountId),
    Failed(&'static str),
}

/// A sign-in abandoned by navigation or cancellation.
#[derive(Debug)]
pub struct Abandoned;

/// Private sign-in lifecycle, including worker cancellation and compensating cleanup.
pub struct ICloud {
    connector: Connector,
    lease: Arc<tempfile::TempDir>,
    fields: [Zeroizing<String>; 3],
    field: usize,
    challenge: Option<ICloudChallenge>,
    worker: Option<mpsc::Receiver<Outcome>>,
    cancel: Arc<AtomicBool>,
    error: Option<&'static str>,
    abandoning: bool,
}
impl ICloud {
    pub fn new(connector: Connector, lease: Arc<tempfile::TempDir>) -> Self {
        Self {
            connector,
            lease,
            fields: std::array::from_fn(|_| Zeroizing::new(String::new())),
            field: 0,
            challenge: None,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            abandoning: false,
        }
    }
    /// Current field index and non-secret presentation status.
    pub fn field(&self) -> usize {
        self.field
    }
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }
    pub fn abandoning(&self) -> bool {
        self.abandoning
    }
    /// Device-local field value for the private sign-in editor.
    pub fn value(&self, index: usize) -> &str {
        self.fields.get(index).map_or("", |value| value.as_str())
    }
    pub fn active(&self) -> bool {
        self.worker.is_some()
    }
    pub fn state(&self) -> &'static str {
        if self.abandoning {
            "cancelling"
        } else if self.active() {
            "icloud-signing-in"
        } else if self.field == 2 {
            "icloud-code"
        } else {
            "icloud-details"
        }
    }
    pub fn edit(&mut self, field: &str, value: Zeroizing<String>) -> Result<(), &'static str> {
        let expected = ["icloud-login", "icloud-password", "icloud-code"][self.field];
        if self.active() || field != expected || value.len() > 4096 {
            return Err("installer-accounts-invalid-action");
        }
        self.fields[self.field] = value;
        Ok(())
    }
    pub fn confirm(&mut self) -> Result<(), &'static str> {
        if self.active() {
            return Err("installer-accounts-busy");
        }
        let value = self.fields[self.field].clone();
        if value.trim().is_empty() {
            return Err("installer-accounts-icloud-required");
        }
        if self.field == 0 {
            if value.len() > 256 || value.chars().any(char::is_control) {
                return Err("installer-accounts-icloud-login-invalid");
            }
            self.field = 1;
            self.error = None;
            return Ok(());
        }
        self.fields[self.field].zeroize();
        self.error = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let connector = self.connector.clone();
        let lease = self.lease.clone();
        let apple_id = self.fields[0].clone();
        let challenge = self.challenge.clone();
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        std::thread::spawn(move || {
            let _lease = lease;
            let result = if let Some(challenge) = challenge {
                connector
                    .icloud_verify(&challenge, value)
                    .map(ICloudSignIn::Complete)
            } else {
                connector.icloud_sign_in(&apple_id, value)
            };
            let outcome = match result {
                Ok(ICloudSignIn::NeedsCode(challenge)) => Outcome::Code(challenge),
                Ok(ICloudSignIn::Complete(session)) => {
                    if cancel.load(Ordering::SeqCst) {
                        connector.icloud_sign_out(&session);
                        Outcome::Failed("installer-accounts-connect-failed")
                    } else if let Ok(id) = connector.finish_icloud(&apple_id, session.clone()) {
                        Outcome::Connected(id)
                    } else {
                        connector.icloud_sign_out(&session);
                        Outcome::Failed("installer-accounts-save-failed")
                    }
                }
                Err(error) => Outcome::Failed(message(&error)),
            };
            // Drop/Back may race persistence. Compensate on the worker while
            // it still holds private staging; never revoke an existing account.
            if let Outcome::Connected(id) = &outcome
                && cancel.load(Ordering::SeqCst)
            {
                let _ = connector.disconnect(id);
                return;
            }
            if let Err(mpsc::SendError(Outcome::Connected(id))) = sender.send(outcome) {
                let _ = connector.disconnect(&id);
            }
        });
        Ok(())
    }
    pub fn back(&mut self) -> bool {
        if self.active() {
            self.cancel.store(true, Ordering::SeqCst);
            self.abandoning = true;
            return false;
        }
        self.error = None;
        self.fields[1].zeroize();
        self.fields[2].zeroize();
        self.challenge = None;
        if self.field == 0 {
            true
        } else {
            self.field = 0;
            false
        }
    }
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.abandoning = true;
        self.fields[1].zeroize();
        self.fields[2].zeroize();
        self.challenge = None;
    }
    pub fn poll(&mut self) -> Option<Result<Option<String>, Abandoned>> {
        let worker = self.worker.as_ref()?;
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Outcome::Failed("installer-accounts-connect-failed")
            }
        };
        self.worker = None;
        if self.abandoning {
            if let Outcome::Connected(id) = result {
                let _ = self.connector.disconnect(&id);
            }
            return Some(Err(Abandoned));
        }
        match result {
            Outcome::Code(challenge) => {
                self.challenge = Some(challenge);
                self.field = 2;
            }
            Outcome::Connected(id) => {
                self.challenge = None;
                return Some(Ok(Some(id.as_str().to_owned())));
            }
            Outcome::Failed(message) => {
                self.error = Some(message);
            }
        }
        Some(Ok(None))
    }
}

impl Drop for ICloud {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}
fn message(error: &ICloudAuthError) -> &'static str {
    match error {
        ICloudAuthError::InvalidCredentials => "installer-accounts-icloud-credentials-invalid",
        ICloudAuthError::InvalidCode => "installer-accounts-icloud-code-invalid",
        ICloudAuthError::AdvancedDataProtection => "installer-accounts-icloud-web-access",
        ICloudAuthError::AccountLocked => "installer-accounts-icloud-locked",
        ICloudAuthError::Offline => "installer-accounts-icloud-offline",
        ICloudAuthError::Unavailable => "installer-accounts-unavailable",
        ICloudAuthError::NeedsReauthentication | ICloudAuthError::Other(_) => {
            "installer-accounts-connect-failed"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> ICloud {
        let stage = Arc::new(tempfile::tempdir().unwrap());
        ICloud::new(Connector::new(stage.path().to_path_buf()), stage)
    }

    #[test]
    fn private_editor_validates_login_and_back_discards_credentials() {
        let mut flow = flow();
        assert_eq!(flow.confirm(), Err("installer-accounts-icloud-required"));
        assert!(
            flow.edit("icloud-password", Zeroizing::new("private".into()))
                .is_err()
        );
        flow.edit("icloud-login", Zeroizing::new("invalid\nlogin".into()))
            .unwrap();
        assert_eq!(
            flow.confirm(),
            Err("installer-accounts-icloud-login-invalid")
        );
        flow.edit("icloud-login", Zeroizing::new("person@example.org".into()))
            .unwrap();
        flow.confirm().unwrap();
        assert_eq!(flow.field(), 1);
        flow.edit(
            "icloud-password",
            Zeroizing::new("private fixture password".into()),
        )
        .unwrap();
        assert!(!flow.back());
        assert_eq!(flow.field(), 0);
        assert_eq!(flow.value(1), "");
        assert_eq!(flow.value(2), "");
        assert!(!flow.active());
    }

    #[test]
    fn cancellation_erases_private_input_without_starting_sign_in() {
        let mut flow = flow();
        flow.edit("icloud-login", Zeroizing::new("person@example.org".into()))
            .unwrap();
        flow.confirm().unwrap();
        flow.edit(
            "icloud-password",
            Zeroizing::new("private fixture password".into()),
        )
        .unwrap();
        flow.cancel();
        assert_eq!(flow.state(), "cancelling");
        assert_eq!(flow.value(1), "");
        assert!(!flow.active());
        assert!(flow.poll().is_none());
    }
}
