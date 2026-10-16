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
//! by that root, with no escrow or recovery phrase. `threshold-identity`
//! implements share-based recovery only after a genuine multi-device
//! ceremony; this path creates a single-device identity, whose actual
//! recovery material is an encrypted portable backup. The only true thing to
//! tell a user is that losing every local key and that backup loses the
//! identity. The copy in
//! [`no_recovery_points`] says exactly that, and the flow makes the user
//! acknowledge it before the key is created; softening it into "keep
//! your device safe" would imply a safety net that doesn't exist. If
//! recovery is ever wired up, this copy changes in the same commit —
//! `tests::onboarding_copy_is_honest_about_identity_and_recovery` guards against drift.

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
    pub fn back(&mut self) {
        match self.screen {
            Screen::Nearby | Screen::Restore => self.screen = Screen::Existing,
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
            Screen::Existing | Screen::Nearby | Screen::Restore
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
pub fn mint_into(identity_dir: &Path) -> Result<MintedIdentity, OnboardingError> {
    let identity_paths = crate::firstboot::collaboration_identity_paths();
    mint_into_with_paths(identity_dir, &identity_paths, false)
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
) -> Result<MintedIdentity, OnboardingError> {
    let identity_paths = crate::firstboot::collaboration_identity_paths();
    mint_into_with_paths(identity_dir, &identity_paths, true)
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
    let path = identity_dir.join("primary.json");
    let outcome = if legacy_linux_host {
        ensure_collaboration_identity_for_legacy_linux_host(identity_dir, identity_paths)
    } else {
        ensure_collaboration_identity(identity_dir, identity_paths)
    }
    .map_err(|e| OnboardingError::Mint(e.to_string()))?;
    match outcome {
        ProvisionOutcome::Created { pubkey_hex } => Ok(MintedIdentity {
            fingerprint: pubkey_hex,
            path,
        }),
        // Already there — read its fingerprint back rather than reporting a key that wasn't the one minted.
        ProvisionOutcome::Existing => {
            let fingerprint = read_fingerprint(&path).map_err(|e| {
                OnboardingError::Mint(format!("identity exists but is unreadable: {e}"))
            })?;
            Ok(MintedIdentity { fingerprint, path })
        }
    }
}

/// Read the public half out of an existing record.
///
/// Goes through `IdentityRecord` — the same type that wrote the file —
/// so a record this build can't parse is an error, not a fingerprint that belongs to nothing.
fn read_fingerprint(path: &Path) -> Result<String, String> {
    existing_identity_at(path).map(|identity| identity.fingerprint)
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
        // The copy must not imply a safety net that does not exist.
        //
        // Tilo mints a root and certified device key with no escrow or recovery phrase.
        // `threshold-identity` implements share-based recovery but is
        // deliberately not wired into this path. A user-held encrypted export is
        // a backup, but it is not escrow and cannot help without its passphrase.
        {
            let all = no_recovery_points().join(" ").to_lowercase();
            assert!(
                all.contains("there is no recovery"),
                "the no-recovery screen must say so outright: {all}"
            );
            for weasel in [
                "recovery phrase",
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
            assert!(copy.contains("not all cryptographically signed"));
            assert!(!copy.contains("signs the changes you make"));
            assert!(!copy.contains("edit in a shared document really came from you"));
        }
    }
}
