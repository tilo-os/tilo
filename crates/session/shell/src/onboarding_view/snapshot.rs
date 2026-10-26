//! Deterministic visual states of the ceremony for review renderers: each
//! puts the view where a person would be, without any identity I/O.

use std::path::PathBuf;

use super::{
    CodecPackStatus, MintedIdentity, Onboarding, OnboardingView, RecoveryKeyCopy, TourPage,
    WordsCheck, clear_secret, t,
};

/// Deterministic states used only by the offscreen production renderer.
/// They exercise the composition without performing identity I/O.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingVisualState {
    Intro,
    Existing,
    Nearby,
    NearbyCode,
    Restore,
    Protect,
    Backup,
    BackupVerified,
    Ready,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
    TourStart,
    CreateError,
    RestoreError,
    Codecs,
    CodecsInstalling,
    CodecsWaitingForNetwork,
    CodecsInstalled,
    CodecsFailed,
    Recovery,
    RecoveryNearby,
    RecoveryRestore,
    RecoveryKeyTyping,
    RecoveryKeyTypo,
    RecoveryKeyReady,
    RecoveryKeyDifferent,
    BackupRecoveryKey,
    BackupRecoveryKeyWritten,
}

impl OnboardingView {
    /// Put the entrance transition in its steady state for deterministic
    /// offscreen review. Production never calls this; the live compositor
    /// advances the exact same transition on animation frames.
    #[doc(hidden)]
    pub fn settle_motion_for_snapshot(&mut self) {
        self.transition = ui::motion::Presence::opened();
    }

    #[doc(hidden)]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture arm per reviewed onboarding state"
    )]
    pub fn set_visual_state_for_snapshot(&mut self, state: OnboardingVisualState) {
        self.flow = Onboarding::new();
        self.error = None;
        self.backup_error = None;
        self.backup_saved = None;
        self.backup_was_verified = false;
        self.backup_copy_form_open = false;
        self.identity_ready_page = false;
        self.joined_member = false;
        self.portable_backup_only = false;
        self.tour_page = None;
        self.tour_only = false;
        self.codec_page = false;
        self.codec_error = None;
        self.nearby_status = None;
        self.nearby_failure = None;
        self.nearby_code = None;
        self.nearby_active_for_snapshot = false;
        self.recovery_only = matches!(
            state,
            OnboardingVisualState::Recovery
                | OnboardingVisualState::RecoveryNearby
                | OnboardingVisualState::RecoveryRestore
                | OnboardingVisualState::RecoveryKeyTyping
                | OnboardingVisualState::RecoveryKeyTypo
                | OnboardingVisualState::RecoveryKeyReady
                | OnboardingVisualState::RecoveryKeyDifferent
        );
        self.expected_identity = self.recovery_only.then_some([0x3f; 32]);
        self.recovery_problem = self
            .recovery_only
            .then_some(crate::onboarding::IdentityProblem::ChainInvalid);
        self.recovery_key_copy = None;
        self.recovery_key_written = false;
        self.recovery_key_copy_confirmed = false;
        clear_secret(&self.recovery_words);
        self.recovery_check = WordsCheck::Empty;
        let minted = MintedIdentity {
            fingerprint: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            path: PathBuf::from("/var/lib/tilo/identities/primary.json"),
        };
        match state {
            OnboardingVisualState::Intro => {}
            OnboardingVisualState::Existing | OnboardingVisualState::Recovery => {
                self.flow.begin_existing();
            }
            OnboardingVisualState::RecoveryNearby => {
                self.flow.begin_existing();
                self.flow.choose_nearby();
                self.nearby_active_for_snapshot = true;
                self.nearby_status = Some(t("onboarding-nearby-waiting"));
            }
            OnboardingVisualState::RecoveryRestore => {
                self.flow.begin_existing();
                self.flow.choose_restore();
            }
            OnboardingVisualState::RecoveryKeyTyping
            | OnboardingVisualState::RecoveryKeyTypo
            | OnboardingVisualState::RecoveryKeyReady
            | OnboardingVisualState::RecoveryKeyDifferent => {
                self.flow.begin_existing();
                self.flow.choose_recovery_key();
                let seed = [0x3f_u8; 32];
                // The expected identity is the public key of `seed`, so the
                // ready state matches it and the different one does not.
                self.expected_identity = Some(
                    ed25519_dalek::SigningKey::from_bytes(&seed)
                        .verifying_key()
                        .to_bytes(),
                );
                let words = match state {
                    OnboardingVisualState::RecoveryKeyDifferent => {
                        threshold_identity::bip39::encode(&[0x5a; 32])
                    }
                    _ => threshold_identity::bip39::encode(&seed),
                }
                .map(|words| words.to_string())
                .unwrap_or_default();
                let phrase = match state {
                    OnboardingVisualState::RecoveryKeyTyping => {
                        words.split(' ').take(9).collect::<Vec<_>>().join(" ")
                    }
                    OnboardingVisualState::RecoveryKeyTypo => words
                        .split(' ')
                        .enumerate()
                        .map(|(at, word)| if at == 6 { "abandn" } else { word })
                        .take(11)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => words,
                };
                if let Ok(mut buffer) = self.recovery_words.lock() {
                    *buffer = phrase;
                }
                self.recovery_check = self
                    .recovery_words
                    .lock()
                    .map_or(WordsCheck::Empty, |words| {
                        WordsCheck::of(&words, self.expected_identity)
                    });
            }
            OnboardingVisualState::Nearby | OnboardingVisualState::NearbyCode => {
                self.flow.begin_existing();
                self.flow.choose_nearby();
                self.nearby_active_for_snapshot = true;
                if state == OnboardingVisualState::NearbyCode {
                    self.nearby_code = Some("482913".into());
                    self.nearby_status = Some(t("onboarding-nearby-code"));
                } else {
                    self.nearby_status = Some(t("onboarding-nearby-waiting"));
                }
            }
            OnboardingVisualState::Restore | OnboardingVisualState::RestoreError => {
                self.flow.begin_existing();
                self.flow.choose_restore();
                if state == OnboardingVisualState::RestoreError {
                    self.error = Some(t("onboarding-snapshot-restore-error"));
                }
            }
            OnboardingVisualState::Protect | OnboardingVisualState::CreateError => {
                self.flow.begin();
                if state == OnboardingVisualState::CreateError {
                    self.error = Some(t("onboarding-snapshot-create-error"));
                }
            }
            OnboardingVisualState::BackupRecoveryKey
            | OnboardingVisualState::BackupRecoveryKeyWritten => {
                self.flow.begin();
                self.flow.set_acknowledged(true);
                let _ = self.flow.complete(minted);
                if state == OnboardingVisualState::BackupRecoveryKey {
                    let mut copy = RecoveryKeyCopy::new(
                        threshold_identity::bip39::encode(&[0x3f; 32]).unwrap_or_default(),
                    );
                    copy.checks = [3, 11, 20];
                    if let Ok(mut answer) = copy.answers[0].lock() {
                        copy.word(3).clone_into(&mut answer);
                    }
                    // A slip in the second answer shows how it is marked.
                    if let Ok(mut answer) = copy.answers[1].lock() {
                        "zoo".clone_into(&mut answer);
                    }
                    self.recovery_key_copy = Some(copy);
                } else {
                    self.recovery_key_written = true;
                    self.backup_was_verified = true;
                }
            }
            OnboardingVisualState::Backup
            | OnboardingVisualState::BackupVerified
            | OnboardingVisualState::Ready => {
                self.flow.begin();
                self.flow.set_acknowledged(true);
                let _ = self.flow.complete(minted);
                self.backup_was_verified = state == OnboardingVisualState::BackupVerified;
                self.backup_saved = self.backup_was_verified.then(|| {
                    PathBuf::from("/home/quinten/Documents/tilo-identity-backup.tilo-identity")
                });
                self.identity_ready_page = state == OnboardingVisualState::Ready;
            }
            OnboardingVisualState::TourNavigate => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Navigate);
            }
            OnboardingVisualState::TourOrganise => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Organise);
            }
            OnboardingVisualState::TourCollaborate => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Collaborate);
            }
            OnboardingVisualState::TourStart => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Start);
            }
            OnboardingVisualState::Codecs
            | OnboardingVisualState::CodecsInstalling
            | OnboardingVisualState::CodecsWaitingForNetwork
            | OnboardingVisualState::CodecsInstalled
            | OnboardingVisualState::CodecsFailed => {
                self.flow.begin();
                self.flow.set_acknowledged(true);
                let _ = self.flow.complete(minted);
                self.backup_was_verified = true;
                self.identity_ready_page = true;
                self.codec_step_offered = true;
                self.codec_page = true;
                self.codec_status = CodecPackStatus::parse(match state {
                    OnboardingVisualState::CodecsInstalling => {
                        r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":14000000,"total":38000000}}"#
                    }
                    OnboardingVisualState::CodecsWaitingForNetwork => {
                        r#"{"state":"waiting-for-network","desired":true}"#
                    }
                    OnboardingVisualState::CodecsInstalled => {
                        r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#
                    }
                    OnboardingVisualState::CodecsFailed => {
                        r#"{"state":"failed","desired":true,"last_error":"Could not download the manifest (connection reset)."}"#
                    }
                    _ => r#"{"state":"not-installed"}"#,
                });
            }
        }
        self.composed = None;
        self.invalidation.both();
        self.settle_motion_for_snapshot();
    }

    fn set_tour_visual_state_for_snapshot(&mut self, minted: MintedIdentity, page: TourPage) {
        self.flow.begin();
        self.flow.set_acknowledged(true);
        let _ = self.flow.complete(minted);
        self.backup_was_verified = true;
        self.identity_ready_page = true;
        self.tour_page = Some(page);
    }
}
