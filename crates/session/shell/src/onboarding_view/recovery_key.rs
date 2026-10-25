//! Recovery keys in the identity ceremony: restoring from the 24 words, with
//! feedback while they are typed, and writing them down on the backup step.

use std::sync::{Arc, Mutex};

use ui::theme::Theme;
use ui::widget::{Banner, Column, Container, Row, Section, Spinner, Text, TextInput, Widget};
use zeroize::Zeroizing;

use super::{
    AfterJournal, IdentityDone, JournalChange, KEY_CHOOSE_RECOVERY_KEY, KEY_RECOVERY_KEY_CLOSE,
    KEY_RECOVERY_KEY_CONFIRM, KEY_RECOVERY_KEY_RESTORE, KEY_RECOVERY_KEY_SHOW, MintedIdentity,
    OnboardingView, Path, PersistedStage, Screen, clear_secret, heading, paragraph, spaced_banner,
    t, t_args,
};

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
        Problem::DifferentIdentity { .. } => t("onboarding-recovery-key-different"),
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WordsCheck {
    Empty,
    Typing {
        words: usize,
    },
    Problem(String),
    /// A recovery key for this identity (its fingerprint).
    Ready {
        fingerprint: String,
    },
    /// A valid recovery key, but for another identity than this computer's.
    Different {
        fingerprint: String,
    },
}

impl WordsCheck {
    pub(super) fn of(phrase: &str, expected: Option<[u8; 32]>) -> Self {
        let progress = runtime_identity::recovery_words_progress(phrase);
        let still_typing = phrase.chars().last().is_some_and(char::is_alphabetic);
        if progress.words == 0 {
            return Self::Empty;
        }
        if let Some(position) = progress.unknown_word {
            return Self::Problem(typo(phrase, position));
        }
        if progress.words < runtime_identity::RECOVERY_WORD_COUNT {
            return Self::Typing {
                words: progress.words,
            };
        }
        match runtime_identity::recovery_words_public_key(phrase) {
            Ok(key) if expected.is_some_and(|expected| expected != key) => Self::Different {
                fingerprint: hex::encode(key),
            },
            Ok(key) => Self::Ready {
                fingerprint: hex::encode(key),
            },
            // The last word may be half typed; judge it once it is finished.
            Err(_) if still_typing && progress.words == runtime_identity::RECOVERY_WORD_COUNT => {
                Self::Typing {
                    words: progress.words,
                }
            }
            Err(problem) => Self::Problem(words_problem(problem)),
        }
    }
}

/// The first groups of a fingerprint: enough to tell identities apart by eye.
pub(super) fn short_fingerprint(fingerprint: &str) -> String {
    let groups: Vec<&str> = fingerprint
        .as_bytes()
        .chunks(4)
        .take(4)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect();
    format!("{} …", groups.join(" "))
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
    pub(super) fn new(words: Zeroizing<String>) -> Self {
        Self {
            checks: recovery_key_checks(),
            words,
            answers: std::array::from_fn(|_| Arc::new(Mutex::new(String::new()))),
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

/// Marks a typed-back word once it can no longer become `expected`, so a
/// slip shows while the key is still on screen to compare against.
fn wrong_answer(expected: Zeroizing<String>) -> impl Fn(&str) -> Vec<std::ops::Range<usize>> {
    move |typed| {
        let answer = typed.trim();
        let start = typed.len().saturating_sub(typed.trim_start().len());
        let fits = expected
            .get(..answer.len())
            .is_some_and(|begun| begun.eq_ignore_ascii_case(answer));
        if fits {
            Vec::new()
        } else {
            std::iter::once(start..start.saturating_add(answer.len())).collect()
        }
    }
}

/// The 24 words, numbered in reading order, and three of them to type
/// back once they are written down.
pub(super) fn recovery_key_copy_page(theme: &Theme, copy: &RecoveryKeyCopy) -> Box<dyn Widget> {
    const COLUMNS: usize = 4;
    let spacing = &theme.tokens.spacing;
    let words: Vec<&str> = copy.words.split(' ').collect();
    let rows = words.len().div_ceil(COLUMNS);
    let mut grid = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s2);
    // Numbered down each column, as people write a list on paper.
    for row in 0..rows {
        let mut line = Row::new().no_wrap().with_gap(spacing.s3);
        for column in 0..COLUMNS {
            let at = column.saturating_mul(rows).saturating_add(row);
            let Some(word) = words.get(at) else { continue };
            line = line.push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_gap(spacing.s2)
                            .push(Box::new(
                                // A fixed column keeps the words aligned past
                                // the single-digit numbers.
                                Container::new()
                                    .with_no_padding()
                                    .with_width(spacing.s6)
                                    .with_child(Box::new(
                                        Text::new(&at.saturating_add(1).to_string())
                                            .with_role(ui::tokens::typography::TextRole::Label)
                                            .with_color(theme.text_dim)
                                            .numeric(),
                                    )),
                            ))
                            .push(Box::new(
                                Text::new(word).with_role(ui::tokens::typography::TextRole::Body),
                            )),
                    )),
            ));
        }
        grid = grid.push(Box::new(line));
    }
    let answers = recovery_key_answers(theme, copy);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-recovery-key-copy-title"), theme))
            .push(paragraph(&t("onboarding-recovery-key-copy-body"), theme))
            .push(Box::new(Section::new(vec![Box::new(grid)])))
            .push(Box::new(answers)),
    )
}

/// The words to type back, each in its own field under its number.
fn recovery_key_answers(theme: &Theme, copy: &RecoveryKeyCopy) -> Column {
    let spacing = &theme.tokens.spacing;
    let mut answers = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s2)
        .push(paragraph(
            &t_args(
                "onboarding-recovery-key-confirm",
                &[
                    ("first", copy.checks[0].into()),
                    ("second", copy.checks[1].into()),
                    ("third", copy.checks[2].into()),
                ],
            ),
            theme,
        ));
    let mut fields = Row::new().no_wrap().with_gap(spacing.s3);
    for (position, answer) in copy.checks.iter().zip(&copy.answers) {
        let label = t_args(
            "onboarding-recovery-key-word",
            &[("position", (*position).into())],
        );
        let mut input = TextInput::local()
            .with_id(&format!("onboarding.backup.recovery-key.word-{position}"))
            .with_external_buffer(Arc::clone(answer))
            .with_accessible_name(label.clone())
            .with_mono()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_mistakes(wrong_answer(Zeroizing::new(
                copy.word(*position).to_owned(),
            )));
        if let Ok(value) = answer.lock() {
            input = input.with_initial_value(&value);
        }
        fields = fields.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(spacing.s1)
                        .push(Box::new(
                            Text::new(&label).with_role(ui::tokens::typography::TextRole::Label),
                        ))
                        .push(Box::new(input)),
                )),
        ));
    }
    answers = answers.push(Box::new(fields));
    answers
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

    /// Restore the identity from the 24 words of its recovery key, checking
    /// them while they are typed so a mistake shows where it is.
    pub(super) fn recovery_key(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-recovery-key-title"), theme))
            .push(paragraph(&t("onboarding-recovery-key-body"), theme));
        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-recovery-key-failed")).with_message(error.clone()),
                theme,
            ));
        }
        let mut words = TextInput::local()
            .with_id("onboarding.recovery-key.field")
            .with_external_buffer(Arc::clone(&self.recovery_words))
            .with_accessible_name(t("onboarding-recovery-key-field"))
            .with_placeholder(t("onboarding-recovery-key-placeholder"))
            .with_mono()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .multiline(true)
            .with_min_lines(4)
            // Each mistyped word is marked where it is, not only named.
            .with_mistakes(runtime_identity::unknown_recovery_words)
            // The words stay put while they are being restored.
            .with_read_only(self.identity_work.is_some());
        if let Ok(value) = self.recovery_words.lock() {
            words = words.with_initial_value(&value);
        }
        let field = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-recovery-key-field"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(words));
        body = body.push(Box::new(field));
        if let Some(status) = self.recovery_key_status(theme) {
            body = body.push(status);
        }
        if self.identity_work.is_some() {
            body = body.push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_gap(spacing.s2)
                    .push(Box::new(Spinner::with_size(theme.tokens.metrics.icon_md)))
                    .push(Box::new(
                        Text::new(&t("onboarding-recovery-key-restoring"))
                            .with_role(ui::tokens::typography::TextRole::Body)
                            .with_color(theme.text_dim)
                            .with_live(ui::a11y::Politeness::Polite),
                    )),
            ));
        }
        Box::new(body)
    }

    /// What the words typed so far amount to, under the field.
    fn recovery_key_status(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        match &self.recovery_check {
            WordsCheck::Empty => None,
            WordsCheck::Typing { words } => Some(Box::new(
                Text::new(&t_args(
                    "onboarding-recovery-key-progress",
                    &[
                        ("count", (*words).into()),
                        ("total", runtime_identity::RECOVERY_WORD_COUNT.into()),
                    ],
                ))
                .with_id("onboarding.recovery-key.status")
                .with_role(ui::tokens::typography::TextRole::Body)
                .with_color(theme.text_dim)
                .numeric(),
            )),
            WordsCheck::Problem(problem) => Some(spaced_banner(
                Banner::warning(problem.clone()).with_icon(ui::icons::Icon::Pencil),
                theme,
            )),
            WordsCheck::Ready { fingerprint } => {
                let message = if self.expected_identity.is_some() {
                    t("onboarding-recovery-key-matches")
                } else {
                    t("onboarding-recovery-key-ready-detail")
                };
                Some(spaced_banner(
                    Banner::success(t_args(
                        "onboarding-recovery-key-ready",
                        &[("fingerprint", short_fingerprint(fingerprint).into())],
                    ))
                    .with_message(message),
                    theme,
                ))
            }
            WordsCheck::Different { fingerprint } => Some(spaced_banner(
                Banner::error(t("onboarding-recovery-key-different")).with_message(t_args(
                    "onboarding-recovery-key-different-detail",
                    &[
                        ("found", short_fingerprint(fingerprint).into()),
                        (
                            "expected",
                            short_fingerprint(&hex::encode(
                                self.expected_identity.unwrap_or_default(),
                            ))
                            .into(),
                        ),
                    ],
                )),
                theme,
            )),
        }
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
                    WordsCheck::of(&words, self.expected_identity)
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
    fn a_typed_back_word_is_marked_once_it_cannot_match() {
        let mark = wrong_answer(Zeroizing::new("ability".to_owned()));
        assert!(mark("").is_empty());
        assert!(mark(" Abil").is_empty(), "still on its way to the word");
        assert!(mark("ability ").is_empty());
        assert_eq!(mark(" abel"), vec![1..5]);
        assert_eq!(mark("abilityx"), vec![0..8]);
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
