//! Installer form adapter for the canonical encrypted private hint format.
use super::view::{AccountsForm, FormError};
pub use account_custody::hints::{SealedHints, open, seal};
use tilo_seal::SymmetricKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HintsError {
    Codec(account_custody::hints::HintsError),
    Form(FormError),
}

impl std::fmt::Display for HintsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codec(error) => std::fmt::Display::fmt(error, f),
            Self::Form(_) => f.write_str("account hints could not be accepted"),
        }
    }
}
impl std::error::Error for HintsError {}

impl AccountsForm {
    /// Authenticate offline suggestions without authorizing or signing in.
    /// The fresh selection/read generation is separate from the artifact's
    /// fixed ciphertext binding, so re-selecting the same export rejects late results.
    /// # Errors
    /// Rejects invalid ciphertext, passwords, hints or a stale source selection.
    pub fn accept_password_hints(
        &self,
        source: &str,
        artifact_binding: &str,
        sealed: &account_custody::hints::PasswordHints,
        password: &[u8],
    ) -> Result<(), HintsError> {
        let hints = account_custody::hints::open_password(password, artifact_binding, sealed)
            .map_err(HintsError::Codec)?;
        self.accept_hints(source, hints).map_err(HintsError::Form)
    }

    /// This does not select a source or grant consent. The existing form gate
    /// also rejects authenticated late results from a previous selection.
    pub fn accept_encrypted_hints(
        &self,
        source: &str,
        sealed: &SealedHints,
        key: &SymmetricKey,
    ) -> Result<(), HintsError> {
        let hints = open(key, source, sealed).map_err(HintsError::Codec)?;
        self.accept_hints(source, hints).map_err(HintsError::Form)
    }
}

// Passwords use the same masked, external buffer as the local login secret.
// Only a bounded control signal crosses the private queue; no password action exists.
use crate::{i18n::t, windows_hint_source::Source};

const MAX_SECRET_BYTES: usize = 2048;
use account_custody::hints::media::Expectation;
use std::sync::{Arc, Mutex, mpsc};
use ui::{
    LocalState,
    widget::{Button, Column, Text, TextInput},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum PromptState {
    #[default]
    Hidden,
    Offered,
    Password,
    Unlocking,
    Failed,
    Unavailable,
    Done,
}
#[derive(Clone, Copy)]
enum HintControl {
    Open,
    Unlock,
    Skip,
}
#[derive(Clone)]
pub(super) struct WindowsPrompt {
    state: LocalState<PromptState>,
    password: Arc<Mutex<String>>,
    command: LocalState<Option<HintControl>>,
}
impl Default for WindowsPrompt {
    fn default() -> Self {
        Self {
            state: LocalState::new(PromptState::Hidden),
            password: Arc::new(Mutex::new(String::new())),
            command: LocalState::new(None),
        }
    }
}
impl WindowsPrompt {
    fn clear(&self) {
        if let Ok(mut password) = self.password.lock() {
            password.zeroize();
            password.clear();
        }
    }
}
impl AccountsForm {
    fn prompt(&self, state: PromptState) {
        self.windows.state.set(state);
        self.invalidate_structure();
    }
    pub(super) fn windows_view(&self, mut column: Column) -> Column {
        let state = self.windows.state.with(|s| *s);
        if matches!(state, PromptState::Hidden | PromptState::Done) {
            return column;
        }
        let key = match state {
            PromptState::Offered => "installer-windows-offered",
            PromptState::Password => "installer-windows-password",
            PromptState::Unlocking => "installer-windows-unlocking",
            PromptState::Failed => "installer-windows-failed",
            _ => "installer-windows-unavailable",
        };
        column = column.push(Box::new(Text::new(&t(key))));
        if matches!(state, PromptState::Password | PromptState::Failed) {
            let form = self.clone();
            column = column.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("installer.windows.password")
                    .with_accessible_name(t("installer-windows-password"))
                    .with_external_buffer(Arc::clone(&self.windows.password))
                    .with_secret(true)
                    .on_change(move |value| {
                        if value.len() > MAX_SECRET_BYTES {
                            form.windows.clear();
                            form.prompt(PromptState::Failed);
                        }
                    }),
            ));
        }
        if matches!(
            state,
            PromptState::Offered | PromptState::Password | PromptState::Failed
        ) {
            let form = self.clone();
            column = column.push(Box::new(
                Button::new("installer.windows.unlock", &t("installer-windows-unlock")).on_view(
                    move || {
                        form.windows
                            .command
                            .set(Some(if state == PromptState::Offered {
                                HintControl::Open
                            } else {
                                HintControl::Unlock
                            }));
                    },
                ),
            ));
        }
        let form = self.clone();
        column.push(Box::new(
            Button::new("installer.windows.skip", &t("installer-windows-skip")).on_view(
                move || {
                    form.windows.command.set(Some(HintControl::Skip));
                },
            ),
        ))
    }
}

type HintResult = Result<Vec<account_custody::hints::AccountHint>, PromptState>;
/// Owns the accepted media identity and one private read generation. Decrypted
/// hints cannot survive skip, a profile change or departure from Accounts.
#[derive(Default)]
pub(crate) struct WindowsReview {
    expected: Option<Expectation>,
    generation: u64,
    pending: Option<(String, mpsc::Receiver<HintResult>)>,
    #[cfg(test)]
    pub(crate) test_source: Option<Source>,
}
impl WindowsReview {
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    #[cfg(test)]
    pub(crate) fn private_test_unlock(
        &mut self,
        form: &AccountsForm,
        block: &[u8],
        password: &str,
    ) {
        self.test_source = Some(
            Source::read(
                &mut std::io::Cursor::new(block),
                block.len() as u64,
                self.expected.as_ref().unwrap(),
                &format!("windows-review/{}", self.generation.wrapping_add(1)),
            )
            .unwrap(),
        );
        *form.windows.password.lock().unwrap() = password.into();
        form.windows.command.set(Some(HintControl::Unlock));
    }
    #[cfg(test)]
    pub(crate) fn private_test_skip(form: &AccountsForm) {
        form.windows.command.set(Some(HintControl::Skip));
    }
    pub(crate) fn set_profile(&mut self, expected: Option<Expectation>, form: &AccountsForm) {
        self.cancel(form);
        self.expected = expected;
        form.prompt(if self.expected.is_some() {
            PromptState::Offered
        } else {
            PromptState::Hidden
        });
    }
    pub(crate) fn cancel(&mut self, form: &AccountsForm) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        form.windows.clear();
        form.windows.command.set(None);
        let _ = form.select_source(None);
        form.prompt(PromptState::Hidden);
    }
    pub(crate) fn sync(&mut self, form: &AccountsForm) {
        let command = form.windows.command.with(|command| *command);
        form.windows.command.set(None);
        match command {
            Some(HintControl::Open) => form.prompt(PromptState::Password),
            Some(HintControl::Skip) => self.cancel(form),
            Some(HintControl::Unlock) => self.start(form),
            None => (),
        }
        let Some((operation, receiver)) = self.pending.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(PromptState::Unavailable),
        };
        let state = match result {
            Ok(hints) => {
                if form.accept_hints(operation, hints).is_ok() {
                    PromptState::Done
                } else {
                    PromptState::Unavailable
                }
            }
            Err(state) => state,
        };
        self.pending = None;
        form.prompt(state);
    }
    fn start(&mut self, form: &AccountsForm) {
        if self.pending.is_some() {
            return;
        }
        let password = Zeroizing::new(
            form.windows
                .password
                .lock()
                .map(|mut held| {
                    let value = if held.len() <= MAX_SECRET_BYTES {
                        held.clone()
                    } else {
                        String::new()
                    };
                    held.zeroize();
                    held.clear();
                    value
                })
                .unwrap_or_default(),
        );
        if password.is_empty() {
            form.prompt(PromptState::Failed);
            return;
        }
        let Some(expected) = self.expected.clone() else {
            form.prompt(PromptState::Unavailable);
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let operation = format!("windows-review/{}", self.generation);
        if form.select_source(Some(operation.clone())).is_err() {
            return;
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.pending = Some((operation.clone(), receive));
        #[cfg(test)]
        let test_source = self.test_source.take();
        form.prompt(PromptState::Unlocking);
        if std::thread::Builder::new()
            .name("windows-hint-unlock".into())
            .spawn(move || {
                #[cfg(test)]
                let source =
                    test_source.map_or_else(|| Source::from_boot(&expected, &operation), Ok);
                #[cfg(not(test))]
                let source = Source::from_boot(&expected, &operation);
                let result = source
                    .map_err(|_| PromptState::Unavailable)
                    .and_then(|source| {
                        source
                            .unlock(password.as_bytes(), &expected, &operation)
                            .map_err(|_| PromptState::Failed)
                    });
                let _ = send.send(result);
            })
            .is_err()
        {
            self.pending = None;
            form.prompt(PromptState::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::view::{AccountHint, Provider};
    use super::*;
    const SOURCE: &str = "selected-device/read-7";
    fn hint() -> AccountHint {
        AccountHint {
            provider: Provider::Nextcloud,
            email: "private@example.org".into(),
            server: "https://cloud.example.org".into(),
            sources: account_custody::hints::HintSources::default(),
        }
    }
    #[test]
    fn offline_artifact_binding_never_substitutes_for_a_fresh_selection_generation() {
        let form = AccountsForm::new();
        let sealed = account_custody::hints::seal_password(
            b"private-password",
            "export-identity",
            &[hint()],
        )
        .unwrap();
        form.select_source(Some("selection/read-1".into())).unwrap();
        form.accept_password_hints(
            "selection/read-1",
            "export-identity",
            &sealed,
            b"private-password",
        )
        .unwrap();
        form.select_source(Some("selection/read-2".into())).unwrap();
        assert_eq!(
            form.accept_password_hints(
                "selection/read-1",
                "export-identity",
                &sealed,
                b"private-password"
            ),
            Err(HintsError::Form(FormError::StaleSource))
        );
        assert_eq!(form.choose_hint(0), Err(FormError::InvalidFields));
    }

    #[test]
    fn form_accepts_only_authenticated_current_selection_without_consent() {
        let key = SymmetricKey::generate().unwrap();
        let form = AccountsForm::new();
        form.select_source(Some(SOURCE.into())).unwrap();
        let sealed = seal(&key, SOURCE, &[hint()]).unwrap();
        assert!(
            form.accept_encrypted_hints(SOURCE, &sealed, &SymmetricKey::generate().unwrap())
                .is_err()
        );
        assert_eq!(form.choose_hint(0), Err(FormError::InvalidFields));
        form.accept_encrypted_hints(SOURCE, &sealed, &key).unwrap();
        assert!(form.email.with(String::is_empty));
        form.choose_hint(0).unwrap();
        assert_eq!(form.email.with(Clone::clone), hint().email);
        assert!(!form.mail.with(|mail| *mail));
        assert!(form.drain_commands().is_empty());
        form.select_source(Some("another/read-8".into())).unwrap();
        assert_eq!(
            form.accept_encrypted_hints(SOURCE, &sealed, &key),
            Err(HintsError::Form(FormError::StaleSource))
        );
        assert_eq!(form.choose_hint(0), Err(FormError::InvalidFields));
    }
}
