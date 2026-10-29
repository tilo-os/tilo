//! What the shell lends setup: the page to show, the buffers secrets are
//! typed into, and where the person's actions go.
//!
//! The shell owns the ceremony: the identity work, the journal, the helpers
//! and every decision. Setup only shows the page and asks; the shell takes
//! what was asked on its next frame ([`OnboardingSeat::take_asked`]).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use zeroize::Zeroize;

use crate::effect::OnboardingEffect;
use crate::page::Page;

/// A buffer a secret is typed into. The shell reads it once, when the step
/// that needs it runs, and zeroizes it.
pub type SecretBuffer = Arc<Mutex<String>>;

/// Marks the words of a recovery key that are not words of one.
pub type WordMistakes = fn(&str) -> Vec<Range<usize>>;

/// The secret fields setup shows, each the shell's buffer.
#[derive(Clone, Default)]
pub struct Secrets {
    /// The passphrase of a backup being restored, and the prepared setup's
    /// password.
    pub connect_passphrase: SecretBuffer,
    /// A new backup's passphrase and its confirmation.
    pub backup_passphrase: SecretBuffer,
    pub backup_confirmation: SecretBuffer,
    /// The recovery key being typed to restore an identity.
    pub recovery_words: SecretBuffer,
    /// The words of a written-down recovery key typed back.
    pub key_answers: [SecretBuffer; 3],
}

impl Secrets {
    /// The buffer a secret field names.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&SecretBuffer> {
        Some(match name {
            "backup-passphrase" | "prepared-password" => &self.connect_passphrase,
            "new-passphrase" => &self.backup_passphrase,
            "new-passphrase-confirmation" => &self.backup_confirmation,
            "recovery-key" => &self.recovery_words,
            "recovery-key-word-1" => &self.key_answers[0],
            "recovery-key-word-2" => &self.key_answers[1],
            "recovery-key-word-3" => &self.key_answers[2],
            _ => return None,
        })
    }

    /// Put `value` in the field it names, replacing and zeroizing what was
    /// there. `false` when no such field exists.
    pub fn enter(&self, name: &str, value: &str) -> bool {
        let Some(buffer) = self.field(name) else {
            return false;
        };
        let Ok(mut held) = buffer.lock() else {
            return false;
        };
        held.zeroize();
        held.push_str(value);
        true
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets(..)")
    }
}

/// What the shell lends setup.
#[derive(Clone, Default)]
pub struct OnboardingSeat {
    inner: Rc<Inner>,
}

#[derive(Default)]
struct Inner {
    page: RefCell<Page>,
    /// Raised on every change of the page.
    revision: Cell<u64>,
    secrets: RefCell<Secrets>,
    mistakes: Cell<Option<WordMistakes>>,
    asked: RefCell<Vec<OnboardingEffect>>,
    /// The next step shows at rest, without its arrival.
    settle: Cell<bool>,
    wake: RefCell<Option<ui::WakeHandle>>,
}

impl std::fmt::Debug for OnboardingSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingSeat")
            .field("page", &self.inner.page.borrow())
            .field("revision", &self.inner.revision.get())
            .finish_non_exhaustive()
    }
}

impl OnboardingSeat {
    /// A seat whose secret fields are the shell's `secrets`; `mistakes`
    /// marks what in a typed recovery key is not part of one.
    #[must_use]
    pub fn new(secrets: Secrets, mistakes: Option<WordMistakes>) -> Self {
        let seat = Self::default();
        *seat.inner.secrets.borrow_mut() = secrets;
        seat.inner.mistakes.set(mistakes);
        seat
    }

    /// Show `page`, unless it shows already.
    pub fn show(&self, page: Page) {
        if *self.inner.page.borrow() == page {
            return;
        }
        *self.inner.page.borrow_mut() = page;
        self.inner
            .revision
            .set(self.inner.revision.get().wrapping_add(1));
        if let Some(wake) = self.inner.wake.borrow().as_ref() {
            wake.wake();
        }
    }

    /// The page shown now.
    #[must_use]
    pub fn page(&self) -> Page {
        self.inner.page.borrow().clone()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.inner.revision.get()
    }

    #[must_use]
    pub fn secrets(&self) -> Secrets {
        self.inner.secrets.borrow().clone()
    }

    pub(crate) fn mistakes(&self) -> Option<WordMistakes> {
        self.inner.mistakes.get()
    }

    /// What the person asked since the shell last looked, oldest first.
    #[must_use]
    pub fn take_asked(&self) -> Vec<OnboardingEffect> {
        std::mem::take(&mut *self.inner.asked.borrow_mut())
    }

    /// Show the page at rest: a review render never catches a step
    /// mid-arrival.
    pub fn settle_motion(&self) {
        self.inner.settle.set(true);
        if let Some(wake) = self.inner.wake.borrow().as_ref() {
            wake.wake();
        }
    }

    pub(crate) fn take_settle(&self) -> bool {
        self.inner.settle.replace(false)
    }

    /// Whether the person asked anything the shell has not taken yet.
    #[must_use]
    pub fn has_asked(&self) -> bool {
        !self.inner.asked.borrow().is_empty()
    }

    /// Queue `effect` as the app's person asked it.
    pub fn ask(&self, effect: OnboardingEffect) {
        self.inner.asked.borrow_mut().push(effect);
    }

    pub(crate) fn wake_with(&self, wake: ui::WakeHandle) {
        *self.inner.wake.borrow_mut() = Some(wake);
    }
}
