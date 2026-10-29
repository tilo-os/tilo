//! What setup shows: plain data the shell keeps current. Secrets the person
//! types never appear here; they stay in the buffers the shell lends
//! ([`crate::Secrets`]). The one secret shown (a recovery key being written
//! down) is [`SecretText`], zeroized when dropped and never printed.

use zeroize::Zeroizing;

/// Where the identity ceremony is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// What an identity is and what it enables.
    #[default]
    Intro,
    /// Use an identity that already exists: choose how to bring it here.
    Existing,
    /// Receive the identity from a nearby trusted device.
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
/// Both keep the same gates; the copy differs.
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

/// Where a nearby device's pairing stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Nearby {
    /// Pairing runs.
    pub active: bool,
    pub status: Option<String>,
    /// Why the last attempt ended without joining: title, then message.
    pub failure: Option<(String, String)>,
    /// The verification code both devices show.
    pub code: Option<String>,
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

/// Which transfer a migration page is about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transfer {
    PortableHome,
    PortableApplications,
    Browser,
    #[default]
    Migration,
}

/// A transfer a prepared setup carried over that the person still has to
/// finish.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Migration {
    pub transfer: Transfer,
    /// The transfer's public status (never a secret).
    pub status: ui::serde_json::Value,
    /// The transfer needs the prepared setup's password again.
    pub needs_password: bool,
    /// The work under way may still be cancelled.
    pub cancellable: bool,
}

/// A prepared setup waiting to be imported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// The import runs.
    pub working: bool,
    /// The package arrives from a peer: its verification code, once known.
    pub peer: Option<Option<String>>,
}

/// What a file chooser is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChooserPurpose {
    /// Choose the backup to restore from.
    Restore,
    /// Choose where to save a new backup.
    Backup,
}

/// A file chooser setup asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chooser {
    pub purpose: ChooserPurpose,
    pub initial: std::path::PathBuf,
    pub roots: Vec<std::path::PathBuf>,
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
    /// A prepared setup is waiting.
    pub prepared: Option<Prepared>,
    /// A carried-over transfer is waiting.
    pub migration: Option<Migration>,
    /// Settings reopened only the backup step.
    pub portable_backup_only: bool,
    /// The backup step was handled: the hand-off shows.
    pub identity_ready_page: bool,
    /// This device joined an identity held elsewhere.
    pub joined_member: bool,
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
    pub chooser: Option<Chooser>,
    /// The display is short: denser spacing.
    pub compact: bool,
}

impl Page {
    /// The transfer's status, when a migration page shows.
    #[must_use]
    pub fn migration_status(&self) -> Option<ui::serde_json::Value> {
        self.migration
            .as_ref()
            .map(|migration| migration.status.clone())
    }

    /// The step a person would name: what is on screen.
    #[must_use]
    pub fn step(&self) -> Step {
        if self.migration.is_some() {
            return Step::Migration;
        }
        if let Some(page) = self.tour_page {
            return Step::Tour(page);
        }
        if self.prepared.is_some() {
            return Step::Prepared;
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
    Prepared,
    Migration,
}
