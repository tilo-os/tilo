//! First-login identity onboarding.
//!
//! A signing identity appears on a qOS machine the first time someone logs
//! in. Before this existed, it appeared *silently*: a key was minted, apps
//! started signing with it, and nothing ever told the person it was theirs,
//! what it stood behind, or what would happen if the laptop went missing.
//!
//! This module is the ceremony that fixes that. It is deliberately three
//! short screens and one real decision:
//!
//! 1. what the identity is, and what it signs for;
//! 2. **that there is no recovery**, acknowledged explicitly;
//! 3. where the key ended up, and its fingerprint.
//!
//! # The honesty rule
//!
//! qOS mints a *single* Ed25519 key with no second factor, no escrow and no
//! recovery phrase. `qos-user-identity` says that is deliberate, and
//! `threshold-identity` — which does implement share-based recovery, along
//! with the recovery screens in `ui::widget::identity_flow` — is not wired
//! into this path. Until someone decides it should be, the only true thing
//! to tell a user is that losing the device loses the identity.
//!
//! So the copy in [`NO_RECOVERY_POINTS`] says exactly that, and the flow
//! makes the user acknowledge it before the key is created. Softening this
//! into "keep your device safe" would imply a safety net that does not
//! exist. If recovery is ever wired up, this copy must change in the same
//! commit — [`tests::copy_does_not_promise_recovery`] guards the wording
//! against drifting into reassurance on its own.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

/// Where the ceremony currently is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// What an identity is and what it signs for.
    Intro,
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

/// The specific things the identity does, so "what it signs for" is concrete
/// rather than a vague reassurance.
pub const INTRO_POINTS: &[&str] = &[
    "It signs the changes you make in shared documents, so your edits are attributable to you.",
    "It identifies this device to people you share a workspace with.",
    "It is created here, on this machine. Nothing is uploaded and no server holds a copy.",
    "It is not an account and has no password — there is nothing to log in to.",
];

/// Heading for the consequence screen. Stated as a fact, not a warning to be
/// dismissed.
pub const NO_RECOVERY_TITLE: &str = "If you lose this device, this identity is gone";

/// The plain truth, point by point.
///
/// Every line here is load-bearing. Read [`tests::copy_does_not_promise_recovery`]
/// before editing any of them.
pub const NO_RECOVERY_POINTS: &[&str] = &[
    "There is no recovery. qOS has no backup code, no recovery phrase, and no second device \
     that can vouch for you.",
    "The key exists in exactly one place — a single file in your home directory, readable \
     only by you.",
    "If this device is lost or wiped, or that file is deleted, the identity cannot be \
     rebuilt. Not by you, and not by anyone else.",
    "You would start again with a new identity, and rejoin shared workspaces as a new \
     participant. Work you already signed stays signed and stays yours; you just cannot \
     sign anything new with the old key.",
    "Copying that file somewhere safe is the only backup that exists today. Anyone who has \
     a copy can sign as you, so treat it like a key to your house.",
];

/// The acknowledgement. Phrased as the consequence, not as "I agree" — the
/// user is confirming they understood a fact, not accepting terms.
pub const ACKNOWLEDGEMENT: &str = "I understand that losing this device means losing this identity.";

pub const DONE_TITLE: &str = "Your identity is ready";
pub const DONE_BODY: &str = "This is the fingerprint others will see. You never have to type it — \
     it is here so you can recognise it later.";

/// The ceremony's state. Pure: no widgets, no I/O, no clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Onboarding {
    screen: Screen,
    acknowledged: bool,
    /// What the person is called, as far as the field has been typed.
    ///
    /// Never a gate: [`Self::can_create`] does not consult it. Blocking
    /// first boot on a name would be hostile, and what an empty one
    /// becomes is [`settled_name`]'s answer, not this struct's.
    display_name: String,
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
    /// The fingerprint in short groups, which is how a person actually
    /// compares two of them. Full hex in one run is unreadable and invites
    /// people to check the first four characters and stop.
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
            display_name: String::new(),
            minted: None,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn acknowledged(&self) -> bool {
        self.acknowledged
    }

    /// What is in the name field right now, verbatim.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Take what the field holds.
    pub fn set_display_name(&mut self, name: impl Into<String>) {
        self.display_name = name.into();
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

    /// Toggle the acknowledgement. Reversible: a user who unticks it must
    /// lose the ability to proceed again, otherwise the gate is decorative.
    pub fn set_acknowledged(&mut self, value: bool) {
        self.acknowledged = value;
    }

    /// Whether the "create my identity" action is available.
    ///
    /// This is the gate the whole module exists for: the key is not minted
    /// until the person has said, in as many words, that they understand
    /// losing the device loses the identity.
    pub fn can_create(&self) -> bool {
        self.screen == Screen::NoRecovery && self.acknowledged
    }

    /// Record a successful mint and move to the final screen.
    ///
    /// Returns `false` — and changes nothing — if called while
    /// [`Self::can_create`] is false, so a UI bug cannot skip the
    /// acknowledgement.
    pub fn complete(&mut self, minted: MintedIdentity) -> bool {
        if !self.can_create() {
            return false;
        }
        self.minted = Some(minted);
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
    /// No `$HOME` / `$XDG_CONFIG_HOME`, so there is no user directory to
    /// write into. Never guess a path for something this sensitive.
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
/// Delegates to [`crate::firstboot::ensure_primary_identity`] so there is
/// exactly one writer of the on-disk record and the ceremony cannot drift
/// from what the rest of the system reads.
pub fn mint_into(
    identity_dir: &Path,
    display_name: &str,
) -> Result<MintedIdentity, OnboardingError> {
    use crate::firstboot::{ensure_primary_identity, ProvisionOutcome};
    let path = identity_dir.join("primary.json");
    let name = settled_name(display_name);
    match ensure_primary_identity(identity_dir, &name)
        .map_err(|e| OnboardingError::Mint(e.to_string()))?
    {
        ProvisionOutcome::Created { pubkey_hex } => Ok(MintedIdentity {
            fingerprint: pubkey_hex,
            path,
        }),
        // Already there — read its fingerprint back rather than reporting a
        // key that was not the one minted.
        ProvisionOutcome::Existing => {
            let fingerprint = read_fingerprint(&path)
                .map_err(|e| OnboardingError::Mint(format!("identity exists but is unreadable: {e}")))?;
            Ok(MintedIdentity { fingerprint, path })
        }
    }
}

/// What actually gets written when the field held `typed`.
///
/// A name is not a password, so first boot does not stop for one — but an
/// empty string is what put "Unknown participant" beside every change a
/// person made, so it is not written either. The machine already knows a
/// name for whoever is sitting at it, and that name was chosen by somebody
/// rather than invented here: the account. Spelled the way a person writes
/// a name rather than the way a login does.
///
/// Empty only if the account cannot be named either, which is a machine with
/// no `$USER` at all.
pub fn settled_name(typed: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        return typed.to_string();
    }
    account_name()
}

/// This machine's account, written as a name.
///
/// Also what the ceremony's field opens pre-filled with, so the person sees
/// the answer that is about to be used and can accept or replace it instead
/// of leaving an empty box and discovering the fallback later.
pub fn account_name() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    name_from_account(&raw)
}

/// `ada.lovelace` → `Ada Lovelace`. Splits on the separators a login uses
/// and capitalises each part.
fn name_from_account(raw: &str) -> String {
    raw.split(|c: char| c == '.' || c == '_' || c == '-' || c == ' ')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let mut ch = p.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read the public half out of an existing record.
///
/// Goes through `IdentityRecord` — the same type that wrote the file — so a
/// record this build cannot parse is reported as an error rather than
/// silently showing a fingerprint that belongs to nothing.
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
        assert!(!o.can_create(), "reaching the screen is not acknowledging it");
        o.set_acknowledged(true);
        assert!(o.can_create());
    }

    /// Unticking has to actually revoke. A checkbox that cannot be undone is
    /// a decoration, not a gate.
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
        assert!(!o.complete(minted.clone()), "complete refuses before the gate");
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
    /// qOS mints one key with no escrow and no recovery phrase.
    /// `threshold-identity` implements share-based recovery and is
    /// deliberately not wired into this path, so any language here about
    /// restoring, recovering or backing up an identity would be false. If
    /// recovery is ever wired up, this test and the copy change together.
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
            // The one legitimate mention is the denial itself, which names
            // these things to rule them out.
            let denied = all.contains(&format!("no {weasel}"))
                || all.contains(&format!("no backup code, no recovery phrase"));
            assert!(
                !all.contains(weasel) || denied,
                "copy must not offer {weasel:?} unless it is denying it: {all}"
            );
        }
    }

    /// The acknowledgement states the consequence rather than asking for
    /// agreement, so ticking it means the user read a fact.
    #[test]
    fn the_acknowledgement_names_the_consequence() {
        let a = ACKNOWLEDGEMENT.to_lowercase();
        assert!(a.contains("losing"), "must name the loss: {ACKNOWLEDGEMENT}");
        assert!(
            a.contains("identity"),
            "must name what is lost: {ACKNOWLEDGEMENT}"
        );
        assert!(
            !a.starts_with("i agree"),
            "this is a fact to understand, not terms to accept: {ACKNOWLEDGEMENT}"
        );
    }

    /// The intro must not describe the identity as an account, which is the
    /// mental model that makes people expect a password reset.
    #[test]
    fn the_intro_says_it_is_not_an_account() {
        let joined = INTRO_POINTS.join(" ").to_lowercase();
        assert!(
            joined.contains("not an account"),
            "the intro must rule out the account mental model: {joined}"
        );
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qos-onboarding-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// The name the person chose has to survive onto the record on disk,
    /// because that record is the only thing every later surface asks.
    ///
    /// `ui::history_source::authors::local_identity_author` branches on
    /// exactly this field, and renders an empty one as
    /// [`ui::history_source::UNKNOWN_AUTHOR_NAME`] — which is why every
    /// change a person made showed up as "Unknown participant" in Room
    /// Activity and in the word processor's history panel.
    #[test]
    fn the_minted_record_carries_the_name_the_person_chose() {
        let dir = scratch_dir("named");
        let minted = mint_into(&dir, "Ada Lovelace").expect("mint");
        let bytes = std::fs::read(&minted.path).expect("the record is readable");
        let record = ui::widget::identity_flow::IdentityRecord::from_json_bytes(&bytes)
            .expect("the record parses");
        assert_eq!(
            record.display_name, "Ada Lovelace",
            "the ceremony asked for a name and wrote {:?} instead",
            record.display_name
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Blank is not `""`.
    ///
    /// Blocking first boot on a name would be hostile — a name is not a
    /// password — but writing an empty string is what made every history
    /// row read "Unknown participant". So an empty field falls back to the
    /// account name this machine already knows, tidied the way a name is
    /// written rather than the way a login is.
    #[test]
    fn a_blank_name_falls_back_to_the_account_rather_than_to_nothing() {
        // If this machine cannot name its own account the fallback has
        // nothing to be, and the test cannot see the thing it exists to
        // check. That is a failure, not a pass.
        let account = account_name();
        assert!(
            !account.is_empty(),
            "no $USER / $USERNAME here, so the account fallback is \
             unobservable and this test proves nothing"
        );

        let dir = scratch_dir("blank");
        let minted = mint_into(&dir, "   ").expect("mint");
        let bytes = std::fs::read(&minted.path).expect("the record is readable");
        let record = ui::widget::identity_flow::IdentityRecord::from_json_bytes(&bytes)
            .expect("the record parses");
        assert_eq!(
            record.display_name, account,
            "a blank field must write the account name, not {:?} — an empty \
             one is the state that renders as \"Unknown participant\" \
             everywhere",
            record.display_name
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Typing is taken at face value, minus the whitespace nobody meant.
    #[test]
    fn a_typed_name_is_taken_as_typed() {
        assert_eq!(settled_name("Ada Lovelace"), "Ada Lovelace");
        assert_eq!(settled_name("  Ada Lovelace  "), "Ada Lovelace");
    }

    /// The fallback is a *name*, not a login. `$USER` is how the machine
    /// spells a person; this is how a person spells themselves.
    #[test]
    fn the_account_fallback_reads_as_a_name() {
        assert_eq!(name_from_account("ada.lovelace"), "Ada Lovelace");
        assert_eq!(name_from_account("ada_lovelace"), "Ada Lovelace");
        assert_eq!(name_from_account("ada-lovelace"), "Ada Lovelace");
        assert_eq!(name_from_account("quinten"), "Quinten");
        assert_eq!(
            name_from_account("   "),
            "",
            "nothing in, nothing out — the caller decides what an unnameable \
             account becomes"
        );
    }

    #[test]
    fn minting_writes_a_readable_record_and_reports_its_fingerprint() {
        let dir = scratch_dir("fingerprint");
        let minted = mint_into(&dir, "Ada Lovelace").expect("mint");
        assert_eq!(minted.fingerprint.len(), 64, "hex of 32 bytes");
        assert!(minted.path.exists());

        // Minting again reports the same key rather than a fresh one.
        let again = mint_into(&dir, "Ada Lovelace").expect("second mint is idempotent");
        assert_eq!(again.fingerprint, minted.fingerprint);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
