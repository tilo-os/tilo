//! Recovery keys in the identity ceremony: restoring from the 24 words, with
//! feedback while they are typed, and writing them down on the backup step.

use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use super::{
    AfterJournal, IdentityDone, JournalChange, KEY_CHOOSE_RECOVERY_KEY, KEY_RECOVERY_KEY_CLOSE,
    KEY_RECOVERY_KEY_CONFIRM, KEY_RECOVERY_KEY_RESTORE, KEY_RECOVERY_KEY_SHOW, MintedIdentity,
    OnboardingView, Path, PersistedStage, Screen, WordsCheck, clear_secret, t,
};
use crate::shell::i18n::t_args;

/// Restore from a recovery key, then provision this device exactly as a
/// restored backup file does.
pub(super) fn import_recovery_words(
    import_dir: &Path,
    identity_paths: &tilo_user_identity::IdentityPaths,
    words: &str,
) -> Result<MintedIdentity, String> {
    let store = crate::identity_registry::IdentityRegistryStore::at(import_dir);
    // The words carry no display name; like native minting, an unnamed
    // identity is shown by its fingerprint until the person names it.
    let profile = store.import_recovery_words(words, "").map_err(|error| {
        runtime_identity::recovery_words_error(&error)
            .map_or_else(|| error.to_string(), words_problem)
    })?;
    let runtime_id = profile
        .record_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "restored identity has no valid runtime id".to_string())?;
    runtime_identity::set_active_identity(import_dir, runtime_id)
        .map_err(|error| error.to_string())?;
    crate::firstboot::ensure_collaboration_identity(import_dir, identity_paths)
        .map_err(|error| error.to_string())?;
    crate::device_fleet::DeviceFleetStore::at(import_dir, identity_paths.clone())
        .register_current_device(&profile.id)
        .map_err(|error| error.to_string())?;
    Ok(MintedIdentity {
        fingerprint: profile.id,
        path: profile.record_path,
    })
}

fn words_problem(problem: runtime_identity::RecoveryWordsError) -> String {
    use runtime_identity::RecoveryWordsError as Problem;
    match problem {
        Problem::WordCount { found } => {
            t_args("onboarding-recovery-key-count", &[("found", found.into())])
        }
        Problem::UnknownWord { position } => t_args(
            "onboarding-recovery-key-unknown-word",
            &[("position", position.into())],
        ),
        Problem::Checksum => t("onboarding-recovery-key-checksum"),
        Problem::DifferentIdentity { .. } => {
            tilo_onboarding::t("onboarding-recovery-key-different")
        }
        Problem::NoSecretOnThisDevice | Problem::ThresholdIdentity => problem.to_string(),
    }
}

/// Which word is wrong, as typed, and the word probably meant when one
/// stands out: the person fixes it without hunting through the list.
fn typo(phrase: &str, position: usize) -> String {
    let Some(word) = runtime_identity::unknown_recovery_words(phrase)
        .first()
        .and_then(|range| phrase.get(range.clone()))
    else {
        return words_problem(runtime_identity::RecoveryWordsError::UnknownWord { position });
    };
    match runtime_identity::closest_recovery_word(word) {
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
    let progress = runtime_identity::recovery_words_progress(phrase);
    let still_typing = phrase.chars().last().is_some_and(char::is_alphabetic);
    if progress.words == 0 {
        return WordsCheck::Empty;
    }
    if let Some(position) = progress.unknown_word {
        return WordsCheck::Problem(typo(phrase, position));
    }
    if progress.words < runtime_identity::RECOVERY_WORD_COUNT {
        return WordsCheck::Typing {
            words: progress.words,
        };
    }
    match runtime_identity::recovery_words_public_key(phrase) {
        Ok(key) if expected.is_some_and(|expected| expected != key) => WordsCheck::Different {
            fingerprint: hex::encode(key),
        },
        Ok(key) => WordsCheck::Ready {
            fingerprint: hex::encode(key),
        },
        // The last word may be half typed; judge it once it is finished.
        Err(_) if still_typing && progress.words == runtime_identity::RECOVERY_WORD_COUNT => {
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
}

impl RecoveryKeyCopy {
    /// The copy of `words`, typed back into `answers` (the buffers the
    /// setup app's fields write into).
    pub(super) fn new(words: Zeroizing<String>, answers: [Arc<Mutex<String>>; 3]) -> Self {
        for answer in &answers {
            clear_secret(answer);
        }
        Self {
            checks: recovery_key_checks(),
            words,
            answers,
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
            clear_secret(answer);
        }
    }
}

/// Three different positions spread over the key, in order: one from each
/// third, so a copy that stopped early or skipped a line is caught.
fn recovery_key_checks() -> [usize; RECOVERY_KEY_CHECKS] {
    let third = runtime_identity::RECOVERY_WORD_COUNT / RECOVERY_KEY_CHECKS;
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

impl OnboardingView {
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
                self.invalidation.structure();
            }
        } else if self.recovery_check != WordsCheck::Empty {
            // Leaving the screen forgets the words typed on it.
            clear_secret(&self.recovery_words);
            self.recovery_check = WordsCheck::Empty;
        }
        if self.take_journaled(KEY_RECOVERY_KEY_RESTORE)
            && self.flow.screen() == Screen::RecoveryKey
            && matches!(self.recovery_check, WordsCheck::Ready { .. })
        {
            self.journal_then(
                JournalChange::Store(PersistedStage::AcquiringIdentity),
                AfterJournal::ImportWords,
            );
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
            && let Some(identity) = self.flow.minted().map(|minted| minted.path.clone())
        {
            self.start_identity_work(
                move || {
                    IdentityDone::RecoveryWords(
                        runtime_identity::recovery_words(&identity).map_err(|error| {
                            runtime_identity::recovery_words_error(&error)
                                .map_or_else(|| error.to_string(), words_problem)
                        }),
                    )
                },
                |error| IdentityDone::RecoveryWords(Err(error)),
            );
        }
        let confirmed = self
            .recovery_key_copy
            .as_ref()
            .is_some_and(RecoveryKeyCopy::confirmed);
        if confirmed != self.recovery_key_copy_confirmed {
            self.recovery_key_copy_confirmed = confirmed;
            self.invalidation.structure();
        }
        if self.pressed.take(KEY_RECOVERY_KEY_CLOSE)
            && let Some(copy) = self.recovery_key_copy.take()
        {
            copy.clear();
            self.recovery_key_copy_confirmed = false;
            self.invalidation.both();
        }
        if self.take_journaled(KEY_RECOVERY_KEY_CONFIRM)
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
            self.journal_then(
                JournalChange::Store(PersistedStage::IdentityReadyBackedUp),
                AfterJournal::Nothing,
            );
            self.invalidation.both();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{press, view};
    use super::super::{KEY_EXISTING, OnboardingVisualState};
    use super::*;
    use automerge::AutoCommit;
    use std::path::PathBuf;
    use ui::widget::Widget as _;

    #[test]
    fn a_recovery_key_is_checked_while_typed_and_restores_only_when_valid() {
        let seed = [0x3f_u8; 32];
        let words = threshold_identity::bip39::encode(&seed)
            .expect("32 bytes encode")
            .to_string();
        let restored = Arc::new(Mutex::new(Vec::<String>::new()));
        let received = Arc::clone(&restored);
        let mut view =
            OnboardingView::with_minter(Box::new(|| Err("create should not run".to_string())));
        view.words_importer = Arc::new(Mutex::new(Box::new(move |words: &str| {
            received.lock().expect("record").push(words.to_owned());
            Ok(MintedIdentity {
                fingerprint: "cd".repeat(32),
                path: PathBuf::from("/home/user/.config/tilo/identity/cd.json"),
            })
        })));
        let mut doc = AutoCommit::new();
        press(&view, &mut doc, KEY_EXISTING);
        view.sync(&doc, &automerge::ROOT);
        press(&view, &mut doc, KEY_CHOOSE_RECOVERY_KEY);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::RecoveryKey);

        let set = |view: &OnboardingView, phrase: &str| {
            *view.recovery_words.lock().expect("words") = phrase.to_owned();
        };
        // A word still being typed is not flagged; a finished unknown one is.
        set(&view, "1. abandon 2. abil");
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(view.recovery_check, WordsCheck::Typing { words: 2 });
        set(&view, "abandon abil ");
        view.sync(&doc, &automerge::ROOT);
        let WordsCheck::Problem(problem) = &view.recovery_check else {
            panic!(
                "a finished unknown word is a problem: {:?}",
                view.recovery_check
            );
        };
        assert!(
            problem.contains("abil") && problem.contains("ability"),
            "names the word as typed and the word probably meant: {problem}"
        );

        // Restore is refused until the words are a valid key.
        press(&view, &mut doc, KEY_RECOVERY_KEY_RESTORE);
        view.sync(&doc, &automerge::ROOT);
        assert!(restored.lock().expect("record").is_empty());

        // A valid key for another identity than this computer's is refused.
        view.expected_identity = Some([0x11; 32]);
        set(&view, &words.to_uppercase());
        view.sync(&doc, &automerge::ROOT);
        assert!(matches!(view.recovery_check, WordsCheck::Different { .. }));
        press(&view, &mut doc, KEY_RECOVERY_KEY_RESTORE);
        view.sync(&doc, &automerge::ROOT);
        assert!(restored.lock().expect("record").is_empty());

        view.expected_identity = None;
        set(&view, &format!("{words}\n"));
        view.sync(&doc, &automerge::ROOT);
        assert!(matches!(view.recovery_check, WordsCheck::Ready { .. }));
        press(&view, &mut doc, KEY_RECOVERY_KEY_RESTORE);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(restored.lock().expect("record").len(), 1);
        assert!(view.flow().is_done());
        assert!(
            view.recovery_words.lock().expect("words").is_empty(),
            "the typed words are wiped once used"
        );
    }

    #[test]
    fn writing_down_the_recovery_key_counts_as_a_verified_backup() {
        let (mut view, _) = view();
        view.set_visual_state_for_snapshot(OnboardingVisualState::BackupRecoveryKey);
        let mut doc = AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        press(&view, &mut doc, KEY_RECOVERY_KEY_CONFIRM);
        view.sync(&doc, &automerge::ROOT);
        assert!(
            view.recovery_key_copy.is_some(),
            "two words are still missing, so the copy is not confirmed"
        );
        let checks = view.recovery_key_copy.as_ref().expect("copy").checks;
        for (at, position) in checks.iter().enumerate() {
            let copy = view.recovery_key_copy.as_ref().expect("copy");
            let word = copy.word(*position).to_uppercase();
            *copy.answers[at].lock().expect("answer") = format!(" {word} ");
        }
        view.sync(&doc, &automerge::ROOT);
        assert!(view.recovery_key_copy_confirmed);
        press(&view, &mut doc, KEY_RECOVERY_KEY_CONFIRM);
        view.sync(&doc, &automerge::ROOT);
        assert!(
            view.recovery_key_copy.is_none(),
            "the words leave the screen"
        );
        assert!(view.recovery_key_written);
        assert!(view.backup_was_verified);
    }

    #[test]
    fn recovery_key_checks_take_one_word_from_each_third() {
        for _ in 0..64 {
            let checks = recovery_key_checks();
            assert!((1..=8).contains(&checks[0]), "{checks:?}");
            assert!((9..=16).contains(&checks[1]), "{checks:?}");
            assert!((17..=24).contains(&checks[2]), "{checks:?}");
        }
    }
}
