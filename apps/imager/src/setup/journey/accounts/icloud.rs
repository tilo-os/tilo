//! Private Apple sign-in state; only acknowledged canonical account IDs leave.
use super::*;
use account_custody::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::Zeroizing;

enum Outcome {
    Code(ICloudChallenge),
    Connected(accounts::AccountId),
    Failed(&'static str),
}

pub(super) struct ICloud {
    connector: Connector,
    lease: Arc<tempfile::TempDir>,
    fields: [ui::LocalState<String>; 3],
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
            fields: std::array::from_fn(|_| ui::LocalState::new(String::new())),
            field: 0,
            challenge: None,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            abandoning: false,
        }
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
        self.fields[self.field].set(value.to_string());
        Ok(())
    }
    pub fn confirm(&mut self) -> Result<(), &'static str> {
        if self.active() {
            return Err("installer-accounts-busy");
        }
        let value = Zeroizing::new(self.fields[self.field].get());
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
        self.fields[self.field].set(String::new());
        self.error = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let connector = self.connector.clone();
        let lease = self.lease.clone();
        let apple_id = Zeroizing::new(self.fields[0].get());
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
        self.fields[1].set(String::new());
        self.fields[2].set(String::new());
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
        self.fields[1].set(String::new());
        self.fields[2].set(String::new());
        self.challenge = None;
    }
    pub fn poll(&mut self) -> Option<Result<Option<accounts::AccountId>, ()>> {
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
            return Some(Err(()));
        }
        match result {
            Outcome::Code(challenge) => {
                self.challenge = Some(challenge);
                self.field = 2;
            }
            Outcome::Connected(id) => {
                self.challenge = None;
                return Some(Ok(Some(id)));
            }
            Outcome::Failed(message) => {
                self.error = Some(message);
            }
        }
        Some(Ok(None))
    }
    pub fn heading(&self) -> String {
        t(if self.active() {
            "installer-accounts-icloud-wait-heading"
        } else {
            [
                "installer-accounts-icloud-login-heading",
                "installer-accounts-icloud-password-heading",
                "installer-accounts-icloud-code-heading",
            ][self.field]
        })
    }
    pub fn intro(&self) -> String {
        t(if self.abandoning {
            "installer-accounts-cancelling-intro"
        } else if self.active() {
            "installer-accounts-icloud-wait-intro"
        } else {
            [
                "installer-accounts-icloud-login-intro",
                "installer-accounts-icloud-password-intro",
                "installer-accounts-icloud-code-intro",
            ][self.field]
        })
    }
    pub fn view(
        &self,
        dispatch: Rc<dyn Fn(AccountRequest)>,
        edit: Rc<dyn Fn(&str, String)>,
    ) -> Box<dyn ui::widget::Widget> {
        let mut body = page::related();
        if let Some(error) = self.error {
            body = body.push(Box::new(Text::new(&t(error))));
        }
        if !self.active() {
            let submit = dispatch.clone();
            let key = ["icloud-login", "icloud-password", "icloud-code"][self.field];
            let label = [
                "installer-accounts-icloud-login-label",
                "installer-accounts-icloud-password-label",
                "installer-accounts-icloud-code-label",
            ][self.field];
            body = body
                .push(Box::new(
                    Field::new(
                        t(label),
                        TextInput::from_state(&self.fields[self.field])
                            .with_id(&format!("installer.account.{key}"))
                            .with_kind(if self.field == 0 {
                                TextInputKind::Username
                            } else {
                                TextInputKind::Password
                            })
                            .with_secret(self.field != 0)
                            .on_change(move |value| edit(key, value.to_string()))
                            .on_submit(move |_| submit(AccountRequest::Confirm)),
                    )
                    .with_label_placement(Some(LabelPlacement::Above)),
                ))
                .push(Box::new(
                    action_button(
                        "installer.account.icloud.confirm",
                        if self.field == 0 {
                            "installer-accounts-continue"
                        } else {
                            "installer-accounts-connect"
                        },
                        AccountRequest::Confirm,
                        dispatch.clone(),
                    )
                    .role(ButtonRole::Primary),
                ));
        }
        Box::new(body.push(Box::new(action_button(
            "installer.account.icloud.cancel",
            "installer-accounts-cancel",
            AccountRequest::Cancel,
            dispatch,
        ))))
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
