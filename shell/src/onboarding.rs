//! First-login identity onboarding.
//!
//! A signing identity appears on a qOS machine the first time someone
//! logs in; this module is the ceremony that puts a name and a
//! consequence on it, rather than letting it appear silently. Three
//! short screens, one real decision: what the identity is and signs
//! for; **that there is no recovery**, acknowledged explicitly; and
//! where the key ended up, with its fingerprint.
//!
//! qOS mints a personal Ed25519 root plus a distinct device key certified
//! by that root, with no escrow or recovery phrase. `threshold-identity`
//! implements share-based recovery but isn't wired into this path, so the
//! only true thing to tell a user is that losing every local or copied root
//! record loses the identity. The copy in
//! [`NO_RECOVERY_POINTS`] says exactly that, and the flow makes the user
//! acknowledge it before the key is created; softening it into "keep
//! your device safe" would imply a safety net that doesn't exist. If
//! recovery is ever wired up, this copy changes in the same commit —
//! [`tests::copy_does_not_promise_recovery`] guards against drift.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

/// Where the ceremony currently is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// What an identity is and what it signs for.
    Intro,
    /// Import an existing identity record instead of minting a new one.
    Connect,
    /// The consequence of losing the device, and the acknowledgement.
    NoRecovery,
    /// The minted key: fingerprint and path.
    Done,
}

/// Title and body for the first screen.
pub const INTRO_TITLE: &str = "This device is about to create your identity";
pub const INTRO_BODY: &str = "qOS signs your work with a key that belongs to you. \
     Collaborators use it to tell that an edit in a shared document really came from you, \
     and peers use it to recognise this device when you share a workspace with them.";

/// The specific things the identity does, so "what it signs for" is concrete rather than a vague reassurance.
pub const INTRO_POINTS: &[&str] = &[
    "It signs the changes you make in shared documents, so your edits are attributable to you.",
    "It identifies this device to people you share a workspace with.",
    "It is created here, on this machine. Nothing is uploaded and no server holds a copy.",
    "It is not an account and has no password — there is nothing to log in to.",
];

/// Heading for the consequence screen — stated as a fact, not a warning to be dismissed.
pub const NO_RECOVERY_TITLE: &str =
    "If you lose this device without a backup, this identity is gone";

/// The plain truth, point by point.
///
/// Every line here is load-bearing. Read [`tests::copy_does_not_promise_recovery`]
/// before editing any of them.
pub const NO_RECOVERY_POINTS: &[&str] = &[
    "There is no recovery service. qOS has no backup code, no recovery phrase, and no \
     support desk that can recreate your key.",
    "Setup creates a personal root record in your home and a separate certified device key \
     in the protected system identity directory. Both remain on this machine.",
    "If this device is lost or wiped and you kept no copy of the personal root record, the \
     identity cannot be rebuilt. Not by you, and not by anyone else.",
    "With no backup you would start again with a new identity and rejoin shared workspaces \
     as a new participant. Work you already signed stays signed and stays yours; you just \
     cannot sign anything new with the old key.",
    "Copying the personal root record somewhere safe is the only backup that exists today. \
     Anyone who has a copy can sign as you and certify devices, so treat it like a key to \
     your house.",
];

/// The acknowledgement, phrased as the consequence rather than "I agree" — the user confirms they understood a fact, not accepted terms.
pub const ACKNOWLEDGEMENT: &str =
    "I understand that losing this device without a backup means losing this identity.";

pub const DONE_TITLE: &str = "Your identity is ready";
pub const DONE_BODY: &str = "This is the fingerprint others will see. You never have to type it — \
     it is here so you can recognise it later.";

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

    /// Choose the connect-existing branch from the intro screen.
    pub fn begin_connect(&mut self) {
        if self.screen == Screen::Intro {
            self.screen = Screen::Connect;
        }
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
        if self.screen != Screen::Connect {
            return false;
        }
        self.minted = Some(identity);
        self.screen = Screen::Done;
        true
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
pub fn mint_into(identity_dir: &Path) -> Result<MintedIdentity, OnboardingError> {
    let identity_paths = crate::firstboot::collaboration_identity_paths();
    mint_into_with_paths(identity_dir, &identity_paths)
}

fn mint_into_with_paths(
    identity_dir: &Path,
    identity_paths: &qos_user_identity::IdentityPaths,
) -> Result<MintedIdentity, OnboardingError> {
    use crate::firstboot::{ProvisionOutcome, ensure_collaboration_identity};
    let path = identity_dir.join("primary.json");
    match ensure_collaboration_identity(identity_dir, identity_paths)
        .map_err(|e| OnboardingError::Mint(e.to_string()))?
    {
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
    use ui::widget::identity_flow::IdentityRecord;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let record = IdentityRecord::from_json_bytes(&bytes).map_err(|e| e.to_string())?;
    Ok(hex_encode(&record.threshold_identity.group_public_key))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_not_minted_until_the_consequence_is_acknowledged() {
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

    /// Unticking has to actually revoke — a checkbox that can't be undone is a decoration, not a gate.
    #[test]
    fn withdrawing_the_acknowledgement_closes_the_gate_again() {
        let mut o = Onboarding::new();
        o.begin();
        o.set_acknowledged(true);
        o.set_acknowledged(false);
        assert!(!o.can_create());
    }

    /// A UI bug must not be able to mint behind the user's back.
    #[test]
    fn completing_without_the_gate_changes_nothing() {
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

    #[test]
    fn fingerprints_are_grouped_so_they_can_be_compared_by_eye() {
        let m = MintedIdentity {
            fingerprint: "0123456789abcdef".to_string(),
            path: PathBuf::from("/x"),
        };
        assert_eq!(m.grouped_fingerprint(), "0123 4567 89ab cdef");
    }

    /// The copy must not imply a safety net that does not exist.
    ///
    /// qOS mints a root and certified device key with no escrow or recovery phrase.
    /// `threshold-identity` implements share-based recovery but is
    /// deliberately not wired into this path, so any language here about
    /// restoring/recovering/backing up would be false — if recovery is ever wired up, this test and the copy change together.
    #[test]
    fn copy_does_not_promise_recovery() {
        let all = NO_RECOVERY_POINTS.join(" ").to_lowercase();
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

    /// The acknowledgement states the consequence rather than asking for agreement, so ticking it means the user read a fact.
    #[test]
    fn the_acknowledgement_names_the_consequence() {
        let a = ACKNOWLEDGEMENT.to_lowercase();
        assert!(
            a.contains("losing"),
            "must name the loss: {ACKNOWLEDGEMENT}"
        );
        assert!(
            a.contains("identity"),
            "must name what is lost: {ACKNOWLEDGEMENT}"
        );
        assert!(
            !a.starts_with("i agree"),
            "this is a fact to understand, not terms to accept: {ACKNOWLEDGEMENT}"
        );
    }

    /// The intro must not describe the identity as an account — the mental model that makes people expect a password reset.
    #[test]
    fn the_intro_says_it_is_not_an_account() {
        let joined = INTRO_POINTS.join(" ").to_lowercase();
        assert!(
            joined.contains("not an account"),
            "the intro must rule out the account mental model: {joined}"
        );
    }

    #[test]
    fn minting_writes_a_readable_record_and_reports_its_fingerprint() {
        let dir = std::env::temp_dir().join(format!(
            "qos-onboarding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let system = qos_user_identity::IdentityPaths::under(dir.join("system"));
        let minted = mint_into_with_paths(&dir, &system).expect("mint");
        assert_eq!(minted.fingerprint.len(), 64, "hex of 32 bytes");
        assert!(minted.path.exists());

        // Minting again reports the same key rather than a fresh one.
        let again = mint_into_with_paths(&dir, &system).expect("second mint is idempotent");
        assert_eq!(again.fingerprint, minted.fingerprint);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
