//! Recovery keys in the identity ceremony: restoring from the 24 words, with
//! feedback while they are typed, and writing them down on the backup step.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use super::{
    Ceremony, IdentityDone, KEY_CHOOSE_RECOVERY_KEY, KEY_RECOVERY_KEY_CLOSE,
    KEY_RECOVERY_KEY_CONFIRM, KEY_RECOVERY_KEY_RESTORE, KEY_RECOVERY_KEY_SHOW, MintedIdentity,
    Path, Screen, Stage, WordsCheck, clear_secret, t,
};
use crate::t_args;

/// Have this computer's daemon restore the person from their recovery key, then take them up as
/// a restored backup is.
pub(super) fn import_recovery_words(
    import_dir: &Path,
    words: &str,
) -> Result<MintedIdentity, String> {
    crate::identity::restore_by_daemon(import_dir, serde_json::json!({ "words": words })).map_err(
        |error| match error {
            crate::identity::OnboardingError::Refused(refusal)
                if refusal.code == tilo_device_identity::login::ANOTHER_PERSONS_WORDS =>
            {
                crate::t("onboarding-recovery-key-different")
            }
            error => error.to_string(),
        },
    )
}

/// The words of the recovery key of the person's root this computer's daemon keeps.
fn recovery_words() -> Result<Zeroizing<String>, String> {
    let answer = tilo_api::daemon::this_computer()?.act(
        tilo_device_identity::login::RECOVERY_WORDS,
        serde_json::json!({}),
    )?;
    answer
        .get("words")
        .and_then(serde_json::Value::as_str)
        .map(|words| Zeroizing::new(words.to_owned()))
        .ok_or_else(|| "this computer's daemon answered no recovery words".to_owned())
}

fn words_problem(problem: tilo_identity::store::RecoveryWordsError) -> String {
    use tilo_identity::store::RecoveryWordsError as Problem;
    match problem {
        Problem::WordCount { found } => {
            t_args("onboarding-recovery-key-count", &[("found", found.into())])
        }
        Problem::UnknownWord { position } => t_args(
            "onboarding-recovery-key-unknown-word",
            &[("position", position.into())],
        ),
        Problem::Checksum => t("onboarding-recovery-key-checksum"),
        Problem::DifferentIdentity { .. } => crate::t("onboarding-recovery-key-different"),
        Problem::NoSecretOnThisDevice | Problem::ThresholdIdentity => problem.to_string(),
    }
}

/// Which word is wrong, as typed, and the word probably meant when one
/// stands out: the person fixes it without hunting through the list.
fn typo(phrase: &str, position: usize) -> String {
    let Some(word) = tilo_identity::store::unknown_recovery_words(phrase)
        .first()
        .and_then(|range| phrase.get(range.clone()))
    else {
        return words_problem(tilo_identity::store::RecoveryWordsError::UnknownWord { position });
    };
    match tilo_identity::store::closest_recovery_word(word) {
        Some(suggestion) => t_args(
            "onboarding-recovery-key-suggestion",
            &[
                ("position", position.into()),
                ("word", word.into()),
                ("suggestion", suggestion.into()),
            ],
        ),
        None => t_args(
            "onboarding-recovery-key-typo",
            &[("position", position.into()), ("word", word.into())],
        ),
    }
}

/// What the words typed so far amount to, for feedback while typing.
pub(super) fn words_check(phrase: &str, expected: Option<[u8; 32]>) -> WordsCheck {
    let progress = tilo_identity::store::recovery_words_progress(phrase);
    let still_typing = phrase.chars().last().is_some_and(char::is_alphabetic);
    if progress.words == 0 {
        return WordsCheck::Empty;
    }
    if let Some(position) = progress.unknown_word {
        return WordsCheck::Problem(typo(phrase, position));
    }
    if progress.words < tilo_identity::store::RECOVERY_WORD_COUNT {
        return WordsCheck::Typing {
            words: progress.words,
        };
    }
    match tilo_identity::store::recovery_words_public_key(phrase) {
        Ok(key) if expected.is_some_and(|expected| expected != key) => WordsCheck::Different {
            fingerprint: hex::encode(key),
        },
        Ok(key) => WordsCheck::Ready {
            fingerprint: hex::encode(key),
        },
        // The last word may be half typed; judge it once it is finished.
        Err(_) if still_typing && progress.words == tilo_identity::store::RECOVERY_WORD_COUNT => {
            WordsCheck::Typing {
                words: progress.words,
            }
        }
        Err(problem) => WordsCheck::Problem(words_problem(problem)),
    }
}

/// How many of the written-down words the person types back, to show they
/// copied the key rather than skimmed past it.
const RECOVERY_KEY_CHECKS: usize = 3;

/// A recovery key being written down: its words, and the positions the
/// person types back to confirm the copy.
pub(super) struct RecoveryKeyCopy {
    pub(super) words: Zeroizing<String>,
    pub(super) checks: [usize; RECOVERY_KEY_CHECKS],
    pub(super) answers: [Arc<Mutex<String>>; RECOVERY_KEY_CHECKS],
    /// The ceremony's epoch, moved when typed-back words are forgotten.
    epoch: Arc<AtomicU64>,
}

impl RecoveryKeyCopy {
    /// The copy of `words`, typed back into `answers` (the buffers the
    /// setup app's fields write into).
    pub(super) fn new(
        words: Zeroizing<String>,
        answers: [Arc<Mutex<String>>; 3],
        epoch: Arc<AtomicU64>,
    ) -> Self {
        for answer in &answers {
            clear_secret(answer, &epoch);
        }
        Self {
            checks: recovery_key_checks(),
            words,
            answers,
            epoch,
        }
    }

    pub(super) fn word(&self, position: usize) -> &str {
        self.words
            .split(' ')
            .nth(position.saturating_sub(1))
            .unwrap_or_default()
    }

    pub(super) fn confirmed(&self) -> bool {
        self.checks
            .iter()
            .zip(&self.answers)
            .all(|(&position, answer)| {
                answer
                    .lock()
                    .is_ok_and(|answer| answer.trim().eq_ignore_ascii_case(self.word(position)))
            })
    }

    pub(super) fn clear(&self) {
        for answer in &self.answers {
            clear_secret(answer, &self.epoch);
        }
    }
}

/// Three different positions spread over the key, in order: one from each
/// third, so a copy that stopped early or skipped a line is caught.
fn recovery_key_checks() -> [usize; RECOVERY_KEY_CHECKS] {
    let third = tilo_identity::store::RECOVERY_WORD_COUNT / RECOVERY_KEY_CHECKS;
    let mut random = [0_u8; RECOVERY_KEY_CHECKS];
    // Which word is asked is not a secret; a fixed choice is still a check.
    let _ = getrandom::fill(&mut random);
    std::array::from_fn(|at| {
        let offset = random
            .get(at)
            .and_then(|&byte| usize::from(byte).checked_rem(third))
            .unwrap_or(0);
        at.saturating_mul(third)
            .saturating_add(offset)
            .saturating_add(1)
    })
}

impl Ceremony {
    pub(super) fn import_words(&mut self) {
        let words = Zeroizing::new(
            self.recovery_words
                .lock()
                .map(|value| value.clone())
                .unwrap_or_default(),
        );
        let importer = Arc::clone(&self.words_importer);
        self.start_identity_work(
            move || {
                // The copy of the words is zeroized when the work drops it.
                let result = importer
                    .lock()
                    .map_err(|_| "recovery keys are unavailable".to_string())
                    .and_then(|importer| (*importer)(&words));
                IdentityDone::Import(result)
            },
            |error| IdentityDone::Import(Err(error)),
        );
    }

    /// Choosing the recovery key, checking the words each frame, and
    /// restoring once they are a valid key.
    pub(super) fn sync_recovery_key(&mut self) {
        if self.pressed.take(KEY_CHOOSE_RECOVERY_KEY) && self.identity_idle() {
            self.error = None;
            self.flow.choose_recovery_key();
        }
        if self.flow.screen() == Screen::RecoveryKey {
            let check = self
                .recovery_words
                .lock()
                .map_or(WordsCheck::Empty, |words| {
                    words_check(&words, self.expected_identity)
                });
            if check != self.recovery_check {
                // A new attempt replaces the failure of the previous one.
                self.error = None;
                self.recovery_check = check;
            }
        } else if self.recovery_check != WordsCheck::Empty {
            // Leaving the screen forgets the words typed on it.
            clear_secret(&self.recovery_words, &self.epoch);
            self.recovery_check = WordsCheck::Empty;
        }
        if self.take_idle(KEY_RECOVERY_KEY_RESTORE)
            && self.flow.screen() == Screen::RecoveryKey
            && matches!(self.recovery_check, WordsCheck::Ready { .. })
        {
            self.import_words();
        }
    }

    /// Showing the recovery key on the backup step, and accepting it once
    /// the asked-for words were typed back.
    pub(super) fn sync_recovery_key_copy(&mut self) {
        let on_backup_step = self.flow.screen() == Screen::Done && !self.identity_ready_page;
        if self.pressed.take(KEY_RECOVERY_KEY_SHOW)
            && on_backup_step
            && self.backup_export_available
            && self.recovery_key_copy.is_none()
            && self.identity_idle()
            && self.flow.minted().is_some()
        {
            self.start_identity_work(
                || IdentityDone::RecoveryWords(recovery_words()),
                |error| IdentityDone::RecoveryWords(Err(error)),
            );
        }
        let confirmed = self
            .recovery_key_copy
            .as_ref()
            .is_some_and(RecoveryKeyCopy::confirmed);
        if confirmed != self.recovery_key_copy_confirmed {
            self.recovery_key_copy_confirmed = confirmed;
        }
        if self.pressed.take(KEY_RECOVERY_KEY_CLOSE)
            && let Some(copy) = self.recovery_key_copy.take()
        {
            copy.clear();
            self.recovery_key_copy_confirmed = false;
        }
        if self.take_idle(KEY_RECOVERY_KEY_CONFIRM)
            && on_backup_step
            && self.recovery_key_copy_confirmed
            && let Some(copy) = self.recovery_key_copy.take()
        {
            copy.clear();
            self.recovery_key_copy_confirmed = false;
            self.recovery_key_written = true;
            self.backup_was_verified = true;
            self.backup_copy_form_open = false;
            self.backup_error = None;
            // Written down and checked is a verified backup, as a file that
            // was read back is.
            self.record(Some(Stage::IdentityReadyBackedUp));
        }
    }
}
