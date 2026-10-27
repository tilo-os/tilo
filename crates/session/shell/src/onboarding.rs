//! First-login identity onboarding.
//!
//! A signing identity appears on a Tilo machine the first time someone
//! logs in; this module is the ceremony that puts a name and a
//! consequence on it, rather than letting it appear silently. Four short
//! steps make the important decisions explicit: what the identity enables;
//! **that there is no escrow or reset service**, acknowledged explicitly;
//! a deliberate encrypted portable-backup choice; and a concise hand-off.
//!
//! Tilo mints a personal Ed25519 root plus a distinct device key certified
//! by that root, with no escrow. What brings it back is held by the person
//! alone: a 24-word recovery key written on paper, or an encrypted portable
//! backup file and its passphrase. The only true thing to tell a user is that
//! losing every local key and both of those loses the identity. The copy in
//! [`no_recovery_points`] says exactly that, and the flow makes the user
//! acknowledge it before the key is created; softening it into "keep
//! your device safe" would imply a safety net that doesn't exist.
//! `tests::onboarding_copy_is_honest_about_identity_and_recovery` guards
//! against drift.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::shell::i18n::t;

/// Where the ceremony currently is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// What an identity is and what it enables.
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
    /// The minted key: fingerprint and path.
    Done,
}

/// Title and body for the first screen.
#[must_use]
pub fn intro_title() -> String {
    t("onboarding-intro-title")
}

#[must_use]
pub fn intro_body() -> String {
    t("onboarding-intro-body")
}

/// The specific things the identity does today, without promising that every
/// application operation already carries a signature.
#[must_use]
pub fn intro_points() -> Vec<String> {
    vec![
        t("onboarding-intro-point-collaboration"),
        t("onboarding-intro-point-local"),
        t("onboarding-intro-point-not-account"),
    ]
}

/// Heading for the consequence screen — stated as a fact, not a warning to be dismissed.
#[must_use]
pub fn no_recovery_title() -> String {
    t("onboarding-no-recovery-title")
}

/// The plain truth, without making people read an implementation tour before
/// they can reach the backup step. Read `tests::onboarding_copy_is_honest_about_identity_and_recovery`
/// before editing these statements.
#[must_use]
pub fn no_recovery_points() -> Vec<String> {
    vec![
        t("onboarding-no-recovery-point-no-service"),
        t("onboarding-no-recovery-point-backup"),
    ]
}

/// The acknowledgement, phrased as the consequence rather than "I agree" — the user confirms they understood a fact, not accepted terms.
#[must_use]
pub fn acknowledgement() -> String {
    t("onboarding-acknowledgement")
}

#[must_use]
pub fn done_title() -> String {
    t("onboarding-done-title")
}

#[must_use]
pub fn done_body() -> String {
    t("onboarding-done-body")
}

/// The ceremony's state. Pure: no widgets, no I/O, no clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Onboarding {
    screen: Screen,
    acknowledged: bool,
    minted: Option<MintedIdentity>,
}

/// What the ceremony produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintedIdentity {
    /// Lowercase hex of the Ed25519 public key.
    pub fingerprint: String,
    /// Absolute path of the record on disk.
    pub path: PathBuf,
}

impl MintedIdentity {
    /// The fingerprint in short groups, how a person actually compares two
    /// of them — full hex in one run is unreadable and invites checking only the first four characters.
    pub fn grouped_fingerprint(&self) -> String {
        self.fingerprint
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Default for Onboarding {
    fn default() -> Self {
        Self::new()
    }
}

impl Onboarding {
    pub fn new() -> Self {
        Self {
            screen: Screen::Intro,
            acknowledged: false,
            minted: None,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn acknowledged(&self) -> bool {
        self.acknowledged
    }

    pub fn minted(&self) -> Option<&MintedIdentity> {
        self.minted.as_ref()
    }

    /// Advance from the intro. Always allowed — nothing has happened yet.
    pub fn begin(&mut self) {
        if self.screen == Screen::Intro {
            self.screen = Screen::NoRecovery;
        }
    }

    /// Choose the use-existing branch from the intro screen.
    pub fn begin_existing(&mut self) {
        if self.screen == Screen::Intro {
            self.screen = Screen::Existing;
        }
    }

    /// Open the nearby-device flow from the use-existing choice.
    pub fn choose_nearby(&mut self) {
        if self.screen == Screen::Existing {
            self.screen = Screen::Nearby;
        }
    }

    /// Open the backup-restore flow from the use-existing choice.
    pub fn choose_restore(&mut self) {
        if self.screen == Screen::Existing {
            self.screen = Screen::Restore;
        }
    }

    /// Return to the choice this screen was opened from, before any
    /// identity exists.
    pub fn choose_recovery_key(&mut self) {
        if self.screen == Screen::Existing {
            self.screen = Screen::RecoveryKey;
        }
    }

    pub fn back(&mut self) {
        match self.screen {
            Screen::Nearby | Screen::Restore | Screen::RecoveryKey => {
                self.screen = Screen::Existing;
            }
            Screen::Existing | Screen::NoRecovery => {
                self.screen = Screen::Intro;
                self.acknowledged = false;
            }
            Screen::Intro | Screen::Done => {}
        }
    }

    /// Whether this screen brings an existing identity here.
    pub fn is_existing_branch(&self) -> bool {
        matches!(
            self.screen,
            Screen::Existing | Screen::Nearby | Screen::Restore | Screen::RecoveryKey
        )
    }

    /// Toggle the acknowledgement — reversible: unticking must lose the ability to proceed again, or the gate is decorative.
    pub fn set_acknowledged(&mut self, value: bool) {
        self.acknowledged = value;
    }

    /// Whether the "create my identity" action is available.
    ///
    /// This is the gate the whole module exists for: the key isn't minted
    /// until the person has said, in as many words, that they understand losing the device loses the identity.
    pub fn can_create(&self) -> bool {
        self.screen == Screen::NoRecovery && self.acknowledged
    }

    /// Record a successful mint and move to the final screen.
    ///
    /// Returns `false` — and changes nothing — if called while
    /// [`Self::can_create`] is false, so a UI bug can't skip the acknowledgement.
    pub fn complete(&mut self, minted: MintedIdentity) -> bool {
        if !self.can_create() {
            return false;
        }
        self.minted = Some(minted);
        self.screen = Screen::Done;
        true
    }

    /// Finish after an existing identity was validated and imported.
    pub fn complete_connected(&mut self, identity: MintedIdentity) -> bool {
        if !self.is_existing_branch() {
            return false;
        }
        self.minted = Some(identity);
        self.screen = Screen::Done;
        true
    }

    /// Resume the post-creation portion of first use with an identity that
    /// was already durably validated at process startup.
    ///
    /// This deliberately bypasses [`Self::can_create`]: it does not create or
    /// import anything, and exists only so a compositor restart after the
    /// identity write can return to backup/tutorial completion rather than
    /// repeating the cryptographic ceremony.
    pub(crate) fn resume_created(&mut self, minted: MintedIdentity) {
        self.acknowledged = false;
        self.minted = Some(minted);
        self.screen = Screen::Done;
    }

    /// Whether the desktop may start.
    pub fn is_done(&self) -> bool {
        self.screen == Screen::Done && self.minted.is_some()
    }
}

/// Why the ceremony could not finish.
#[derive(Debug)]
pub enum OnboardingError {
    /// No `$HOME` / `$XDG_CONFIG_HOME`, so there's no user directory to write into — never guess a path for something this sensitive.
    NoUserDirectory,
    /// Minting or writing failed.
    Mint(String),
    /// The identity chain this device holds could not be taken up. `problem`
    /// tells the person what to do about it; `detail` is the diagnostic.
    Identity {
        problem: IdentityProblem,
        detail: String,
    },
}

impl OnboardingError {
    /// What stopped the device from taking up its identity, when that is
    /// what failed. A UI picks its recovery from this, never from `Display`.
    #[must_use]
    pub fn identity_problem(&self) -> Option<IdentityProblem> {
        match self {
            Self::Identity { problem, .. } => Some(*problem),
            Self::NoUserDirectory | Self::Mint(_) => None,
        }
    }

    fn from_provisioning(error: &std::io::Error) -> Self {
        match IdentityProblem::of(error) {
            Some(problem) => Self::Identity {
                problem,
                detail: error.to_string(),
            },
            None => Self::Mint(error.to_string()),
        }
    }
}

/// Why an identity chain this device holds cannot be used, each answered by
/// a different step: unlock the keyring, bring the identity back, or replace
/// what no longer verifies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityProblem {
    /// The record is here but the secret store holding its private share is
    /// locked or not running. Unlocking it and retrying is enough.
    KeyringUnavailable,
    /// No record resolves for this account although something on the device
    /// says it holds an identity. Retrying cannot help; the person brings
    /// the identity back from another device or a backup.
    RecordMissing,
    /// A record, key or certificate is present but malformed, or does not
    /// verify against the rest of the chain (including a certificate not yet
    /// or no longer valid at this device's clock).
    ChainInvalid,
}

impl IdentityProblem {
    /// Environment variable carrying the startup verdict from the process
    /// entry point into the compositor's trusted recovery chrome, beside
    /// [`crate::shell::identity_provisioning::IDENTITY_RECOVERY_ENV`].
    pub const RECOVERY_ENV: &'static str = "TILO_IDENTITY_RECOVERY_REASON";

    /// Classify a failure of the identity chain. Storage and permission
    /// failures are not a problem of the identity and return `None`.
    #[must_use]
    pub fn of(error: &std::io::Error) -> Option<Self> {
        if crate::shell::identity_provisioning::is_secret_service_unavailable(error) {
            return Some(Self::KeyringUnavailable);
        }
        match error.kind() {
            std::io::ErrorKind::NotFound => Some(Self::RecordMissing),
            std::io::ErrorKind::InvalidData => Some(Self::ChainInvalid),
            _ => None,
        }
    }

    /// Stable token for [`Self::RECOVERY_ENV`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeyringUnavailable => "keyring-unavailable",
            Self::RecordMissing => "record-missing",
            Self::ChainInvalid => "chain-invalid",
        }
    }

    /// Parse a token written by [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::KeyringUnavailable,
            Self::RecordMissing,
            Self::ChainInvalid,
        ]
        .into_iter()
        .find(|problem| problem.as_str() == value)
    }

    /// The verdict startup recorded before opening trusted recovery, if any.
    #[must_use]
    pub fn from_recovery_env() -> Option<Self> {
        Self::parse(std::env::var(Self::RECOVERY_ENV).ok()?.as_str())
    }
}

impl fmt::Display for OnboardingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OnboardingError::NoUserDirectory => write!(
                f,
                "no user config directory ($HOME / $XDG_CONFIG_HOME unset); refusing to guess \
                 where a signing key belongs"
            ),
            OnboardingError::Mint(e) => write!(f, "could not create the identity: {e}"),
            OnboardingError::Identity { detail, .. } => {
                write!(f, "could not take up the identity: {detail}")
            }
        }
    }
}

impl Error for OnboardingError {}

/// Mint the identity into `identity_dir` and describe what landed.
///
/// Delegates to [`crate::firstboot::ensure_collaboration_identity`] so the
/// human root, certified device key, and Iroh endpoint identity are created
/// as one idempotent ceremony.
///
/// # Errors
///
/// Returns an error if minting the collaboration identity fails, or if one
/// already exists but its fingerprint can't be read back.
pub fn mint_into(
    identity_dir: &Path,
    identity_paths: &tilo_user_identity::IdentityPaths,
) -> Result<MintedIdentity, OnboardingError> {
    mint_into_with_paths(identity_dir, identity_paths, false)
}

/// Mint for Tilo Desktop running on a pre-existing Linux distribution.
///
/// This is the only path that associates a fresh identity with the current
/// Unix account's passwd display name.
///
/// # Errors
///
/// Returns an error if the compatibility identity ceremony cannot create or
/// read back the collaboration identity.
pub fn mint_into_for_legacy_linux_host(
    identity_dir: &Path,
    identity_paths: &tilo_user_identity::IdentityPaths,
) -> Result<MintedIdentity, OnboardingError> {
    mint_into_with_paths(identity_dir, identity_paths, true)
}

fn mint_into_with_paths(
    identity_dir: &Path,
    identity_paths: &tilo_user_identity::IdentityPaths,
    legacy_linux_host: bool,
) -> Result<MintedIdentity, OnboardingError> {
    use crate::firstboot::{
        ProvisionOutcome, ensure_collaboration_identity,
        ensure_collaboration_identity_for_legacy_linux_host,
    };
    let outcome = if legacy_linux_host {
        ensure_collaboration_identity_for_legacy_linux_host(identity_dir, identity_paths)
    } else {
        ensure_collaboration_identity(identity_dir, identity_paths)
    }
    .map_err(|e| OnboardingError::from_provisioning(&e))?;
    match outcome {
        ProvisionOutcome::Created { pubkey_hex } => Ok(MintedIdentity {
            fingerprint: pubkey_hex,
            path: identity_dir.join(crate::firstboot::PRIMARY_IDENTITY_FILE),
        }),
        // Already there — read its fingerprint back rather than reporting a
        // key that wasn't the one minted. Resolve it exactly as provisioning
        // and startup do: an identity restored from a backup or joined from
        // another device is stored under its fingerprinted id and selected
        // by the account's `active` file, so it is never at `primary.json`.
        ProvisionOutcome::Existing => existing_identity_in(identity_dir),
    }
}

/// The account's active identity in `identity_dir`, resolved as every other
/// consumer resolves it, with a typed reason when it cannot be read.
///
/// Goes through `IdentityRecord` — the same type that wrote the file —
/// so a record this build can't parse is an error, not a fingerprint that
/// belongs to nothing.
fn existing_identity_in(identity_dir: &Path) -> Result<MintedIdentity, OnboardingError> {
    use crate::shell::identity_provisioning::DiskIdentityStore;
    use tilo_identity_ceremony::{IdentityFlowError, IdentityStore as _};

    let path = runtime_identity::resolve_active_identity(identity_dir)
        .unwrap_or_else(|| identity_dir.join(crate::firstboot::PRIMARY_IDENTITY_FILE));
    let unreadable = |problem, error: &dyn fmt::Display| OnboardingError::Identity {
        problem,
        detail: format!("identity exists but is unreadable: {error}"),
    };
    let record = match DiskIdentityStore::at(&path).load() {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Err(unreadable(
                IdentityProblem::RecordMissing,
                &"the identity record is missing",
            ));
        }
        Err(IdentityFlowError::Io(error)) => {
            return Err(match IdentityProblem::of(&error) {
                Some(problem) => unreadable(problem, &error),
                None => {
                    OnboardingError::Mint(format!("identity exists but is unreadable: {error}"))
                }
            });
        }
        Err(error) => return Err(unreadable(IdentityProblem::ChainInvalid, &error)),
    };
    Ok(MintedIdentity {
        fingerprint: hex::encode(record.threshold_identity.group_public_key),
        path,
    })
}

/// Reconstruct the public result needed by the completion screen from an
/// existing, validated identity record. The disk loader enforces the same
/// ownership, permissions, size, schema, and no-symlink policy as normal
/// startup; resuming first use must not accept a weaker file merely because
/// no key operation is needed.
pub(crate) fn existing_identity_at(path: &Path) -> Result<MintedIdentity, String> {
    use crate::shell::identity_provisioning::DiskIdentityStore;
    use tilo_identity_ceremony::IdentityStore as _;

    let record = DiskIdentityStore::at(path)
        .load()
        .map_err(|e| e.to_string())?;
    let record = record.ok_or_else(|| "the identity record is missing".to_string())?;
    Ok(MintedIdentity {
        fingerprint: hex::encode(record.threshold_identity.group_public_key),
        path: path.to_path_buf(),
    })
}

/// Resume transport only for the exact durable identity and certified device.
/// This grants no app access; native owner/session admission remains separate.
pub(crate) fn canonical_native_identity_ready(minted: &MintedIdentity) -> bool {
    if existing_identity_at(&minted.path).map_or(true, |current| current != *minted) {
        return false;
    }
    let Ok(bytes) = hex::decode(&minted.fingerprint) else {
        return false;
    };
    let Ok(person) = <[u8; 32]>::try_from(bytes) else {
        return false;
    };
    let room = collab_types::person_room(&person);
    tilo_identity_host::broker::trusted_current_device(&room.to_string()).is_ok_and(
        |(_, certificate)| {
            certificate.user_public_key == person && !certificate.capabilities.foreign
        },
    )
}

/// Whether the record at `path` carries no secret of the identity: neither a
/// share in the file nor a reference to one in the secret store. That is the
/// public record a device holds after joining from another device, whose root
/// key stays there. An unreadable record counts as holding one, so the ready
/// screen keeps recommending a backup when in doubt.
pub(crate) fn holds_no_identity_secret(path: &Path) -> bool {
    use crate::shell::identity_provisioning::DiskIdentityStore;
    use tilo_identity_ceremony::IdentityStore as _;

    DiskIdentityStore::at(path)
        .load()
        .ok()
        .flatten()
        .is_some_and(|record| {
            record.secret_ref.is_none() && record.threshold_identity.secret_bytes.is_empty()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_minting_waits_for_the_acknowledged_consequence() {
        // Phase `the_key_is_not_minted_until_the_consequence_is_acknowledged`.
        {
            let mut o = Onboarding::new();
            assert!(!o.can_create(), "cannot create from the intro screen");
            o.begin();
            assert_eq!(o.screen(), Screen::NoRecovery);
            assert!(
                !o.can_create(),
                "reaching the screen is not acknowledging it"
            );
            o.set_acknowledged(true);
            assert!(o.can_create());
        }
        // Phase `withdrawing_the_acknowledgement_closes_the_gate_again`.
        // Unticking has to actually revoke — a checkbox that can't be undone is a decoration, not a gate.
        {
            let mut o = Onboarding::new();
            o.begin();
            o.set_acknowledged(true);
            o.set_acknowledged(false);
            assert!(!o.can_create());
        }
        // Phase `back_returns_to_the_choice_without_carrying_acknowledgement`.
        {
            let mut o = Onboarding::new();
            o.begin();
            o.set_acknowledged(true);
            o.back();
            assert_eq!(o.screen(), Screen::Intro);
            assert!(!o.acknowledged());

            o.begin_existing();
            assert_eq!(o.screen(), Screen::Existing);
            o.choose_nearby();
            assert_eq!(o.screen(), Screen::Nearby);
            o.back();
            assert_eq!(o.screen(), Screen::Existing, "back returns to the choice");
            o.choose_restore();
            assert_eq!(o.screen(), Screen::Restore);
            o.choose_nearby();
            assert_eq!(
                o.screen(),
                Screen::Restore,
                "flows open only from the choice"
            );
            o.back();
            o.back();
            assert_eq!(o.screen(), Screen::Intro);

            o.choose_restore();
            assert_eq!(
                o.screen(),
                Screen::Intro,
                "the intro has no restore shortcut"
            );
        }
        // Phase `completing_without_the_gate_changes_nothing`.
        // A UI bug must not be able to mint behind the user's back.
        {
            let mut o = Onboarding::new();
            let minted = MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/nowhere/primary.json"),
            };
            assert!(
                !o.complete(minted.clone()),
                "complete refuses before the gate"
            );
            assert_eq!(o.screen(), Screen::Intro);
            assert!(o.minted().is_none());
            assert!(!o.is_done());

            o.begin();
            o.set_acknowledged(true);
            assert!(o.complete(minted));
            assert_eq!(o.screen(), Screen::Done);
            assert!(o.is_done());
        }
        // Phase `resuming_a_created_identity_never_reopens_the_creation_gate`.
        {
            let mut onboarding = Onboarding::new();
            onboarding.resume_created(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/identity/primary.json"),
            });

            assert_eq!(onboarding.screen(), Screen::Done);
            assert!(onboarding.is_done());
            assert!(!onboarding.acknowledged());
            assert!(!onboarding.can_create());
        }
        // Phase `minting_writes_a_readable_record_and_reports_its_fingerprint`.
        {
            let dir = std::env::temp_dir().join(format!(
                "tilo-onboarding-{}-{}",
                std::process::id(),
                tilo_clock::unix_now_ns()
            ));
            let system = tilo_user_identity::IdentityPaths::under(dir.join("system"));
            let minted = mint_into_with_paths(&dir, &system, false).expect("mint");
            assert_eq!(minted.fingerprint.len(), 64, "hex of 32 bytes");
            assert!(minted.path.exists());

            // Minting again reports the same key rather than a fresh one.
            let again =
                mint_into_with_paths(&dir, &system, false).expect("second mint is idempotent");
            assert_eq!(again.fingerprint, minted.fingerprint);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A device joined from another device holds its person as a public
    /// record under a fingerprinted id, selected by the account's `active`
    /// file. Retrying from trusted recovery runs the minter, which provisioning
    /// answers with `Existing`; reading `primary.json` instead of the selected
    /// record reported "the identity record is missing" and kept a sound
    /// device in recovery for good.
    #[test]
    fn a_joined_device_takes_up_the_identity_its_account_selects() {
        use getrandom::{SysRng, rand_core::UnwrapErr};
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, persist};

        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let person = root.verifying_key().to_bytes();
        let dir = tempfile::tempdir().unwrap();
        let identity_dir = dir.path().join("identity");
        let system = tilo_user_identity::IdentityPaths::under(dir.path().join("system"));
        let (device, _) = persist::load_or_create_device_key(&system).unwrap();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_000,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        persist::install_device_cert(&system, &cert, 2_000).unwrap();
        assert!(
            tilo_identity_host::provisioning::complete_enrolled_device(
                &identity_dir,
                &system,
                tilo_clock::unix_now_ns(),
            )
            .unwrap()
        );
        let selected = runtime_identity::resolve_active_identity(&identity_dir).unwrap();
        assert!(!identity_dir.join("primary.json").exists());

        let minted = mint_into_with_paths(&identity_dir, &system, false)
            .expect("the joined identity is taken up, not reported missing");
        assert_eq!(minted.fingerprint, hex::encode(person));
        assert_eq!(minted.path, selected);
        assert!(holds_no_identity_secret(&minted.path));

        // The account's config is reset while the device's enrollment
        // survives: the same person is taken up again.
        std::fs::remove_dir_all(&identity_dir).unwrap();
        let again = mint_into_with_paths(&identity_dir, &system, false)
            .expect("the surviving enrollment brings its person back");
        assert_eq!(again.fingerprint, hex::encode(person));
        assert!(again.path.is_file());
        assert!(!system.user_privkey.exists(), "no root is minted beside it");
    }

    #[test]
    fn identity_problems_name_the_step_that_fixes_them() {
        use std::io;
        let of = |error: io::Error| IdentityProblem::of(&error);
        assert_eq!(
            of(runtime_identity::secret_service_unavailable_error()),
            Some(IdentityProblem::KeyringUnavailable)
        );
        assert_eq!(
            of(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad certificate"
            )),
            Some(IdentityProblem::ChainInvalid)
        );
        assert_eq!(
            of(io::Error::new(io::ErrorKind::NotFound, "no record")),
            Some(IdentityProblem::RecordMissing)
        );
        assert_eq!(
            of(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not private"
            )),
            None
        );
        for problem in [
            IdentityProblem::KeyringUnavailable,
            IdentityProblem::RecordMissing,
            IdentityProblem::ChainInvalid,
        ] {
            assert_eq!(IdentityProblem::parse(problem.as_str()), Some(problem));
        }
        assert_eq!(IdentityProblem::parse("1"), None);

        // A selector naming a record that is gone is reported as missing,
        // distinct from a locked keyring or a broken chain.
        let dir = tempfile::tempdir().unwrap();
        let error = existing_identity_in(dir.path()).unwrap_err();
        assert_eq!(
            error.identity_problem(),
            Some(IdentityProblem::RecordMissing)
        );
    }

    #[test]
    fn native_identity_readiness_requires_real_identity_device_and_certificate() {
        const CHILD: &str = "TILO_ONBOARDING_READINESS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let fixture = tempfile::tempdir().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["onboarding::tests::native_identity_readiness_requires_real_identity_device_and_certificate", "--exact"])
                .env(CHILD, "1")
                .env("HOME", fixture.path())
                .env("XDG_CONFIG_HOME", fixture.path().join("config"))
                .env("XDG_STATE_HOME", fixture.path().join("state"))
                .env("TILO_IDENTITY_ROOT", fixture.path().join("system"))
                .env(runtime_identity::FILE_SECRETS_ENV, "1")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let directory = runtime_identity::identity_dir_from_env().unwrap();
        let minted = mint_into(
            &directory,
            &crate::firstboot::collaboration_identity_paths(),
        )
        .unwrap();
        let mut flow = Onboarding::new();
        assert!(!flow.is_done());
        flow.resume_created(minted.clone());
        assert!(flow.is_done());
        assert!(canonical_native_identity_ready(&minted));
        let mut mismatch = minted.clone();
        mismatch.fingerprint = "00".repeat(32);
        assert!(!canonical_native_identity_ready(&mismatch));
        let person: [u8; 32] = hex::decode(&minted.fingerprint)
            .unwrap()
            .try_into()
            .unwrap();
        let paths = crate::firstboot::collaboration_identity_paths();
        for path in [
            paths.device_cert_for_user(&person),
            paths.device_privkey,
            minted.path.clone(),
        ] {
            let held = path.with_extension("readiness-held");
            std::fs::rename(&path, &held).unwrap();
            assert!(!canonical_native_identity_ready(&minted));
            std::fs::rename(&held, &path).unwrap();
            assert!(canonical_native_identity_ready(&minted));
        }
    }

    #[test]
    fn onboarding_copy_is_honest_about_identity_and_recovery() {
        // Phase `fingerprints_are_grouped_so_they_can_be_compared_by_eye`.
        {
            let m = MintedIdentity {
                fingerprint: "0123456789abcdef".to_string(),
                path: PathBuf::from("/x"),
            };
            assert_eq!(m.grouped_fingerprint(), "0123 4567 89ab cdef");
        }
        // Phase `copy_does_not_promise_recovery`.
        // The copy must not imply a safety net that does not exist: there is
        // no escrow, only what the person keeps themselves (a recovery key or
        // an encrypted backup), and the copy names both.
        {
            let all = no_recovery_points().join(" ").to_lowercase();
            assert!(
                all.contains("there is no recovery"),
                "the no-recovery screen must say so outright: {all}"
            );
            assert!(
                all.contains("recovery key") && all.contains("encrypted backup"),
                "the screen must name what the person keeps to recover: {all}"
            );
            for weasel in [
                "backup code",
                "restore your identity",
                "we can help",
                "contact support",
            ] {
                // The one legitimate mention is the denial itself, which names these things to rule them out.
                let denied = all.contains(&format!("no {weasel}"))
                    || all.contains(&"no backup code, no recovery phrase".to_string());
                assert!(
                    !all.contains(weasel) || denied,
                    "copy must not offer {weasel:?} unless it is denying it: {all}"
                );
            }
        }
        // Phase `the_acknowledgement_names_the_consequence`.
        // The acknowledgement states the consequence rather than asking for agreement, so ticking it means the user read a fact.
        {
            let acknowledgement = acknowledgement();
            let a = acknowledgement.to_lowercase();
            assert!(
                a.contains("losing"),
                "must name the loss: {acknowledgement}"
            );
            assert!(
                a.contains("identity"),
                "must name what is lost: {acknowledgement}"
            );
            assert!(
                !a.starts_with("i agree"),
                "this is a fact to understand, not terms to accept: {acknowledgement}"
            );
        }
        // Phase `the_intro_says_it_is_not_an_account`.
        // The intro must not describe the identity as an account — the mental model that makes people expect a password reset.
        {
            let joined = intro_points().join(" ").to_lowercase();
            assert!(
                joined.contains("not an account"),
                "the intro must rule out the account mental model: {joined}"
            );
        }
        // Phase `identity_copy_does_not_overstate_per_edit_signing`.
        {
            let copy = format!("{} {}", intro_body(), intro_points().join(" ")).to_lowercase();
            // It admits that signing does not yet cover every edit.
            assert!(
                copy.contains("not every edit in apps is signed yet"),
                "the intro must not imply every edit is signed: {copy}"
            );
            assert!(!copy.contains("signs the changes you make"));
            assert!(!copy.contains("edit in a shared document really came from you"));
        }
    }
}
