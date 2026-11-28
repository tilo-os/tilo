//! What is asked of setup, by its view, an agent or a script through the session's API, and the
//! page setup shows in answer: the one projection of the ceremony its view reads.

use super::effect::OnboardingEffect;
use super::page::{Codecs, IdentityProblem, KeyCopy, Nearby, PackState, Page, SecretText, Step};
use super::{
    Ceremony, KEY_BACK, KEY_BACKUP_ANOTHER, KEY_BACKUP_BROWSE, KEY_BACKUP_CONTINUE, KEY_BEGIN,
    KEY_CHOOSE_NEARBY, KEY_CHOOSE_RECOVERY_KEY, KEY_CHOOSE_RESTORE, KEY_CODECS_CONTINUE,
    KEY_CODECS_INSTALL, KEY_CODECS_SKIP, KEY_CONNECT_BROWSE, KEY_CONNECT_CANCEL,
    KEY_CONNECT_NEARBY, KEY_CONNECT_RETRY, KEY_CREATE, KEY_EXISTING, KEY_FINISH, KEY_IMPORT,
    KEY_RECOVERY_KEY_CLOSE, KEY_RECOVERY_KEY_CONFIRM, KEY_RECOVERY_KEY_RESTORE,
    KEY_RECOVERY_KEY_SHOW, KEY_TOUR_BACK, KEY_TOUR_NEXT, KEY_TOUR_SKIP, Screen,
};
use zeroize::Zeroize as _;

impl Ceremony {
    /// Take `effect`, as a press of the control that shows it: the next [`Self::step`] carries it
    /// out on the page that shows then. A secret goes straight into its buffer and never joins
    /// the queue.
    ///
    /// # Errors
    /// A secret for a field setup does not have.
    pub fn act(&mut self, mut effect: OnboardingEffect) -> Result<(), String> {
        if let OnboardingEffect::EnterSecret { field, value } = &mut effect {
            let entered = self.buffer(field).is_some_and(|buffer| {
                buffer.lock().is_ok_and(|mut held| {
                    held.zeroize();
                    held.push_str(value);
                    true
                })
            });
            value.zeroize();
            return if entered {
                Ok(())
            } else {
                Err("setup has no such secret field".to_owned())
            };
        }
        self.asked.push(effect);
        Ok(())
    }

    /// The buffer a secret field names.
    fn buffer(&self, name: &str) -> Option<&std::sync::Arc<std::sync::Mutex<String>>> {
        Some(match name {
            "backup-passphrase" => &self.connect_passphrase,
            "nearby-code" => &self.ticket,
            "new-passphrase" => &self.backup_passphrase,
            "new-passphrase-confirmation" => &self.backup_confirmation,
            "recovery-key" => &self.recovery_words,
            "recovery-key-word-1" => &self.key_answers[0],
            "recovery-key-word-2" => &self.key_answers[1],
            "recovery-key-word-3" => &self.key_answers[2],
            _ => return None,
        })
    }

    /// The page setup shows now.
    #[allow(
        clippy::too_many_lines,
        reason = "one plain field per fact the setup page shows"
    )]
    #[must_use]
    pub fn page(&self) -> Page {
        let on_words = self.flow.screen() == Screen::RecoveryKey;
        let recovery_word_mistakes = if on_words {
            self.recovery_words
                .lock()
                .map(|words| tilo_identity::store::unknown_recovery_words(&words))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let key_answers_wrong = self.recovery_key_copy.as_ref().map_or([false; 3], |copy| {
            std::array::from_fn(|at| {
                let position = copy.checks.get(at).copied().unwrap_or_default();
                let expected = copy.word(position);
                self.key_answers.get(at).is_some_and(|answer| {
                    answer.lock().is_ok_and(|typed| {
                        let answer = typed.trim();
                        !expected
                            .get(..answer.len())
                            .is_some_and(|begun| begun.eq_ignore_ascii_case(answer))
                    })
                })
            })
        });
        Page {
            screen: self.flow.screen(),
            context: self.context,
            recovery_only: self.recovery_only,
            portable_backup_only: self.portable_backup_only,
            identity_ready_page: self.identity_ready_page,
            joined_device: self.joined_device.clone(),
            codec_page: self.codec_page,
            codecs: self.codecs_page(),
            tour_page: self.tour_page,
            total_steps: self.total_steps(),
            acknowledged: self.flow.acknowledged(),
            can_create: self.flow.can_create(),
            busy: !self.identity_idle(),
            error: self.error.clone(),
            progress_error: self.progress_error.clone(),
            expected_identity: self.expected_identity.map(hex::encode),
            recovery_problem: self.recovery_problem.map(|problem| match problem {
                crate::identity::IdentityProblem::KeyringUnavailable => {
                    IdentityProblem::KeyringUnavailable
                }
                crate::identity::IdentityProblem::RecordMissing => IdentityProblem::RecordMissing,
                crate::identity::IdentityProblem::ChainInvalid => IdentityProblem::ChainInvalid,
            }),
            nearby: Nearby {
                active: self.nearby_active(),
                failure: self
                    .nearby_failure
                    .as_ref()
                    .map(|failure| (failure.title(), failure.message())),
            },
            connect_path: self.connect_path.clone(),
            connect_picked: self.connect_picked,
            recovery_check: self.recovery_check.clone(),
            recovery_words_total: tilo_identity::store::RECOVERY_WORD_COUNT,
            backup_error: self.backup_error.clone(),
            backup_saved: self
                .backup_saved
                .as_ref()
                .map(|path| path.display().to_string()),
            backup_was_verified: self.backup_was_verified,
            backup_export_available: self.backup_export_available,
            backup_copy_form_open: self.backup_copy_form_open,
            backup_skip_acknowledged: self.backup_skip_acknowledged,
            recovery_key_copy: self.recovery_key_copy.as_ref().map(|copy| KeyCopy {
                words: SecretText::new(copy.words.clone()),
                checks: copy.checks,
            }),
            recovery_key_written: self.recovery_key_written,
            recovery_key_copy_confirmed: self.recovery_key_copy_confirmed,
            recovery_word_mistakes,
            key_answers_wrong,
            epoch: self.epoch.load(std::sync::atomic::Ordering::Acquire),
            old_system: self.old_system.clone(),
        }
    }

    fn codecs_page(&self) -> Codecs {
        use os_actions::codec_pack::PackState as Pack;
        let status = &self.codec_status;
        Codecs {
            state: match status.state {
                Pack::NotInstalled => PackState::NotInstalled,
                Pack::Installing => PackState::Installing,
                Pack::WaitingForNetwork => PackState::WaitingForNetwork,
                Pack::Installed => PackState::Installed,
                Pack::Removing => PackState::Removing,
                Pack::Failed => PackState::Failed,
            },
            running: self.codec_running.is_some(),
            error: self.codec_error.clone(),
            fraction: status
                .progress
                .as_ref()
                .map(os_actions::codec_pack::Progress::fraction),
            summary: status.summary(),
            when_it_applies: status.when_it_applies().map(str::to_owned),
            last_error: status.last_error.clone(),
        }
    }

    /// Carry out what was asked since the last pass, each on the page that shows as it is taken.
    /// An acknowledgement holds still while identity work runs: its outcome belongs to the step
    /// that asked for it, with the acknowledgement it was started with.
    pub(super) fn take_asks(&mut self) {
        let asked = std::mem::take(&mut self.asked);
        for effect in asked {
            let step = self.page().step();
            match effect {
                // The hand-off's ways to make Tilo the person's: each leaves setup, as entering
                // the desktop does, and opens where it leads once setup is down.
                OnboardingEffect::Choose { option }
                    if step == Step::Ready && then_opens(&option).is_some() =>
                {
                    self.then_open = then_opens(&option).map(str::to_owned);
                    self.pressed.press(super::KEY_FINISH);
                }
                // "Later": the backup is offered, never forced, and Devices keeps offering it, so
                // leaving it for now asks for nothing more.
                OnboardingEffect::Skip if step == Step::Backup => {
                    self.backup_skip_acknowledged = true;
                    self.pressed.press(KEY_BACKUP_CONTINUE);
                }
                OnboardingEffect::Acknowledge { checked } => match step {
                    Step::Protect if self.identity_idle() => {
                        self.flow.set_acknowledged(checked);
                    }
                    Step::Backup => self.backup_skip_acknowledged = checked,
                    _ => {}
                },
                OnboardingEffect::Enter { field, value } if field == "backup-file" => {
                    if step == Step::Restore {
                        self.connect_path = value;
                    }
                }
                effect => {
                    if let Some(key) = self.press_for(&effect, step) {
                        self.pressed.press(key);
                    }
                }
            }
        }
    }

    fn press_for(&self, effect: &OnboardingEffect, step: Step) -> Option<&'static str> {
        Some(match (effect, step) {
            (OnboardingEffect::Continue, Step::Intro) => KEY_BEGIN,
            (OnboardingEffect::Continue, Step::RecoveryKey) => KEY_RECOVERY_KEY_RESTORE,
            (OnboardingEffect::Continue, Step::RecoveryKeyCopy) => KEY_RECOVERY_KEY_CONFIRM,
            (OnboardingEffect::Continue, Step::Restore) => KEY_IMPORT,
            (OnboardingEffect::Continue, Step::Protect) => KEY_CREATE,
            (OnboardingEffect::Continue, Step::Backup) => KEY_BACKUP_CONTINUE,
            (OnboardingEffect::Continue, Step::Tour(_)) => KEY_TOUR_NEXT,
            (OnboardingEffect::Continue, Step::Codecs) => KEY_CODECS_CONTINUE,
            (OnboardingEffect::Continue, Step::Ready) | (OnboardingEffect::Finish, _) => KEY_FINISH,
            (OnboardingEffect::Back, Step::RecoveryKeyCopy) => KEY_RECOVERY_KEY_CLOSE,
            (OnboardingEffect::Back, Step::Tour(_)) => KEY_TOUR_BACK,
            (OnboardingEffect::Back, _) => KEY_BACK,
            (OnboardingEffect::Choose { option }, _) => match option.as_str() {
                "create" => KEY_BEGIN,
                "existing" => KEY_EXISTING,
                "nearby" => KEY_CHOOSE_NEARBY,
                "recovery-key" => KEY_CHOOSE_RECOVERY_KEY,
                "backup-file" => KEY_CHOOSE_RESTORE,
                "write-down-recovery-key" => KEY_RECOVERY_KEY_SHOW,
                "another-copy" => KEY_BACKUP_ANOTHER,
                "tour" => super::KEY_TOUR_START,
                "install-codecs" => KEY_CODECS_INSTALL,
                _ => return None,
            },
            (OnboardingEffect::Browse, Step::Restore) => KEY_CONNECT_BROWSE,
            (OnboardingEffect::Browse, Step::Backup) => KEY_BACKUP_BROWSE,
            (OnboardingEffect::Skip, Step::Tour(_)) => KEY_TOUR_SKIP,
            (OnboardingEffect::Skip, Step::Codecs) => KEY_CODECS_SKIP,
            (OnboardingEffect::Retry, Step::Nearby) => KEY_CONNECT_NEARBY,
            (OnboardingEffect::Retry, Step::Existing) => KEY_CONNECT_RETRY,
            (OnboardingEffect::Retry, Step::Codecs) => KEY_CODECS_INSTALL,
            (OnboardingEffect::Cancel, Step::Nearby) => KEY_CONNECT_CANCEL,
            (OnboardingEffect::Cancel, Step::Backup) if self.portable_backup_only => KEY_FINISH,
            _ => return None,
        })
    }
}

/// Where the hand-off's choice `option` leads: the migration app, Settings' accounts, or the Store.
fn then_opens(option: &str) -> Option<&'static str> {
    Some(match option {
        "migrate" => "tilo://open#/me/so.tilo.migrate",
        "accounts" => "settings://accounts",
        "store" => "tilo://open#/me/so.tilo.store",
        _ => return None,
    })
}
