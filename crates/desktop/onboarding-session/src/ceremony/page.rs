//! What setup shows: plain data the session keeps current, and the reading the setup app's view
//! reads it as (`session.Setup`, [`Page::reading`]). Secrets the person types never appear here;
//! they stay in the ceremony's buffers. The one secret shown (a recovery key being written down)
//! is [`SecretText`], zeroized when dropped and never printed, and it travels only in the reading
//! the session hands the view it draws itself, never in one an API caller reads.

use zeroize::Zeroizing;

/// Where the identity ceremony is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// What an identity is and what it enables.
    #[default]
    Intro,
    /// Use an identity that already exists: choose how to bring it here.
    Existing,
    /// Join the identity with the code another of the person's devices printed.
    Nearby,
    /// Restore the identity from an encrypted backup file.
    Restore,
    /// Restore the identity from the 24 words of its recovery key.
    RecoveryKey,
    /// The consequence of losing the device, and the acknowledgement.
    NoRecovery,
    /// The identity exists: its backup, then the hand-off.
    Done,
}

impl Screen {
    /// One of the ways to bring an identity that already exists.
    #[must_use]
    pub fn is_existing_branch(self) -> bool {
        matches!(
            self,
            Self::Existing | Self::Nearby | Self::Restore | Self::RecoveryKey
        )
    }
}

/// Whether Tilo is the whole system or a desktop on another Linux system.
/// Both keep the same checks; the copy differs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnboardingContext {
    #[default]
    FullSystem,
    HostDesktop,
}

/// A page of the tour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TourPage {
    Navigate,
    Organise,
    Collaborate,
    Start,
}

impl TourPage {
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Navigate => 1,
            Self::Organise => 2,
            Self::Collaborate => 3,
            Self::Start => 4,
        }
    }
}

/// Why this computer's identity could not be used, when setup opened
/// recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityProblem {
    /// The record is here but the secret store holding its private share is
    /// locked or not running. Unlocking it and retrying is enough.
    KeyringUnavailable,
    /// No record resolves for this account although something on the device
    /// says it holds an identity.
    RecordMissing,
    /// A record, key or certificate is present but does not verify.
    ChainInvalid,
}

/// What the recovery words typed so far amount to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WordsCheck {
    #[default]
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

/// A secret shown on screen: zeroized when dropped, never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretText(Zeroizing<String>);

impl SecretText {
    #[must_use]
    pub fn new(text: Zeroizing<String>) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText(..)")
    }
}

/// The recovery key being written down on the backup step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyCopy {
    /// The 24 words, separated by single spaces.
    pub words: SecretText,
    /// The positions (from 1) the person types back.
    pub checks: [usize; 3],
}

impl KeyCopy {
    #[must_use]
    pub fn word(&self, position: usize) -> &str {
        self.words
            .expose()
            .split(' ')
            .nth(position.saturating_sub(1))
            .unwrap_or_default()
    }
}

/// Where adding this computer with a code from another device stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Nearby {
    /// The code is being redeemed on the device that issued it.
    pub active: bool,
    /// Why the last attempt ended without joining: title, then message.
    pub failure: Option<(String, String)>,
}

/// The media codecs extension's state, as the helper reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackState {
    #[default]
    NotInstalled,
    Installing,
    WaitingForNetwork,
    Installed,
    Removing,
    Failed,
}

/// The media formats step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Codecs {
    pub state: PackState,
    /// The install started from this page has not returned yet.
    pub running: bool,
    /// A refusal from the system (the helper's own errors are in `last_error`).
    pub error: Option<String>,
    /// How far an install got, while it installs.
    pub fraction: Option<f32>,
    pub summary: String,
    pub when_it_applies: Option<String>,
    pub last_error: String,
}

/// The device that joined, with names when its signed installation has them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinedDevice {
    pub label: Option<String>,
    pub person: Option<String>,
}

/// Everything a setup page shows.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about where setup stands, compared field by field"
)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub screen: Screen,
    pub context: OnboardingContext,
    /// Recovery of an identity this computer already belongs to: no way to
    /// create a new one.
    pub recovery_only: bool,
    /// Settings reopened only the backup step.
    pub portable_backup_only: bool,
    /// The backup step was handled: the hand-off shows.
    pub identity_ready_page: bool,
    /// This device joined an identity held elsewhere.
    pub joined_device: Option<JoinedDevice>,
    /// The media formats step shows.
    pub codec_page: bool,
    pub codecs: Codecs,
    pub tour_page: Option<TourPage>,
    pub total_steps: u8,
    /// The no-recovery fact was acknowledged.
    pub acknowledged: bool,
    pub can_create: bool,
    /// Identity work runs: its step holds still.
    pub busy: bool,
    pub error: Option<String>,
    pub progress_error: Option<String>,
    /// The start of the fingerprint of the identity this computer belongs to.
    pub expected_identity: Option<String>,
    pub recovery_problem: Option<IdentityProblem>,
    pub nearby: Nearby,
    /// The backup file to restore from, as typed or chosen.
    pub connect_path: String,
    /// Moves each time the person picks the file in the picker: the view takes `connect_path`
    /// into its field then, and leaves what they type alone otherwise.
    pub connect_picked: u64,
    pub recovery_check: WordsCheck,
    /// How many words a recovery key has.
    pub recovery_words_total: usize,
    pub backup_error: Option<String>,
    /// Where the verified backup was saved.
    pub backup_saved: Option<String>,
    pub backup_was_verified: bool,
    pub backup_export_available: bool,
    pub backup_copy_form_open: bool,
    pub backup_skip_acknowledged: bool,
    pub recovery_key_copy: Option<KeyCopy>,
    pub recovery_key_written: bool,
    pub recovery_key_copy_confirmed: bool,
    /// The old system found on this computer's disks, which the hand-off offers to bring things
    /// over from.
    pub old_system: Option<String>,
    /// Where in the typed recovery key the words of no recovery key stand, as character ranges.
    pub recovery_word_mistakes: Vec<std::ops::Range<usize>>,
    /// Which of the words typed back can no longer be the word asked for.
    pub key_answers_wrong: [bool; 3],
    /// Moves each time the ceremony forgets what was typed into its secret fields: the view
    /// starts its fields empty again.
    pub epoch: u64,
}

impl Page {
    /// The step a person would name: what is on screen.
    #[must_use]
    pub fn step(&self) -> Step {
        if let Some(page) = self.tour_page {
            return Step::Tour(page);
        }
        match self.screen {
            Screen::Intro => Step::Intro,
            Screen::Existing => Step::Existing,
            Screen::Nearby => Step::Nearby,
            Screen::Restore => Step::Restore,
            Screen::RecoveryKey => Step::RecoveryKey,
            Screen::NoRecovery => Step::Protect,
            Screen::Done if self.identity_ready_page && self.codec_page => Step::Codecs,
            Screen::Done if self.identity_ready_page => Step::Ready,
            Screen::Done if self.recovery_key_copy.is_some() => Step::RecoveryKeyCopy,
            Screen::Done => Step::Backup,
        }
    }
}

/// The step on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Intro,
    Existing,
    Nearby,
    Restore,
    RecoveryKey,
    Protect,
    Backup,
    RecoveryKeyCopy,
    Codecs,
    Ready,
    Tour(TourPage),
}

impl Page {
    /// The page as the setup app's view reads it (`session.Setup`): one flat record, every text
    /// empty where the page has none. The words of a recovery key being written down are in it
    /// only with `words`: the reading the session hands the view it draws itself.
    #[must_use]
    pub fn reading(&self, words: bool) -> serde_json::Value {
        let text = |value: &Option<String>| value.clone().unwrap_or_default();
        let (check, typed, problem, fingerprint) = match &self.recovery_check {
            WordsCheck::Empty => ("empty", 0, String::new(), String::new()),
            WordsCheck::Typing { words } => ("typing", *words, String::new(), String::new()),
            WordsCheck::Problem(problem) => ("problem", 0, problem.clone(), String::new()),
            WordsCheck::Ready { fingerprint } => ("ready", 0, String::new(), fingerprint.clone()),
            WordsCheck::Different { fingerprint } => {
                ("different", 0, String::new(), fingerprint.clone())
            }
        };
        let joined = self.joined_device.clone().unwrap_or_default();
        let key_words: Vec<String> = match (&self.recovery_key_copy, words) {
            (Some(copy), true) => copy.words.expose().split(' ').map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        let key_checks: Vec<usize> = self
            .recovery_key_copy
            .as_ref()
            .map(|copy| copy.checks.to_vec())
            .unwrap_or_default();
        let mistakes: Vec<usize> = self
            .recovery_word_mistakes
            .iter()
            .flat_map(|range| [range.start, range.end])
            .collect();
        let mut reading = serde_json::json!({
            "step": self.step().name(),
            "tour": self.tour_page.map_or(0, TourPage::number),
            "host_desktop": self.context == OnboardingContext::HostDesktop,
            "mac": cfg!(target_os = "macos"),
            "recovery_only": self.recovery_only,
            "portable_backup_only": self.portable_backup_only,
            "joined": self.joined_device.is_some(),
            "joined_label": joined.label.unwrap_or_default(),
            "joined_person": joined.person.unwrap_or_default(),
            "total_steps": self.total_steps,
            "acknowledged": self.acknowledged,
            "can_create": self.can_create,
            "busy": self.busy,
            "error": text(&self.error),
            "progress_error": text(&self.progress_error),
            "expected_identity": text(&self.expected_identity),
            "recovery_problem": match self.recovery_problem {
                None => "",
                Some(IdentityProblem::KeyringUnavailable) => "keyring",
                Some(IdentityProblem::RecordMissing) => "missing",
                Some(IdentityProblem::ChainInvalid) => "invalid",
            },
            "nearby_active": self.nearby.active,
            "nearby_failure_title": self.nearby.failure.as_ref().map(|(title, _)| title.clone()).unwrap_or_default(),
            "nearby_failure_message": self.nearby.failure.as_ref().map(|(_, message)| message.clone()).unwrap_or_default(),
            "connect_path": self.connect_path,
            "connect_picked": self.connect_picked,
        });
        let rest = serde_json::json!({
            "words_check": check,
            "words_typed": typed,
            "words_problem": problem,
            "words_fingerprint": fingerprint,
            "words_total": self.recovery_words_total,
            "words_mistakes": mistakes,
            "backup_error": text(&self.backup_error),
            "backup_saved": text(&self.backup_saved),
            "backup_verified": self.backup_was_verified,
            "backup_available": self.backup_export_available,
            "backup_form_open": self.backup_copy_form_open,
            "skip_acknowledged": self.backup_skip_acknowledged,
            "key_shown": self.recovery_key_copy.is_some(),
            "key_words": key_words,
            "key_checks": key_checks,
            "key_wrong": self.key_answers_wrong.to_vec(),
            "key_written": self.recovery_key_written,
            "key_confirmed": self.recovery_key_copy_confirmed,
            "codecs_state": self.codecs.state.name(),
            "codecs_running": self.codecs.running,
            "codecs_error": text(&self.codecs.error),
            "codecs_fraction": self.codecs.fraction,
            "codecs_summary": self.codecs.summary,
            "codecs_when": text(&self.codecs.when_it_applies),
            "codecs_last_error": self.codecs.last_error,
            "old_system": text(&self.old_system),
            "epoch": self.epoch,
        });
        if let (Some(reading), serde_json::Value::Object(rest)) = (reading.as_object_mut(), rest) {
            reading.extend(rest);
        }
        reading
    }
}

impl Step {
    /// The step's name in the reading.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Intro => "intro",
            Self::Existing => "existing",
            Self::Nearby => "nearby",
            Self::Restore => "restore",
            Self::RecoveryKey => "recovery-key",
            Self::Protect => "protect",
            Self::Backup => "backup",
            Self::RecoveryKeyCopy => "recovery-key-copy",
            Self::Codecs => "codecs",
            Self::Ready => "ready",
            Self::Tour(_) => "tour",
        }
    }
}

impl PackState {
    /// The state's name in the reading.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Installing => "installing",
            Self::WaitingForNetwork => "waiting-for-network",
            Self::Installed => "installed",
            Self::Removing => "removing",
            Self::Failed => "failed",
        }
    }
}
