//! What this host keeps for its members, signed by their roots: their root
//! envelopes and their profiles.
//!
//! A person whose root lives in a browser keeps it wrapped as a root
//! envelope (`tilo_user_identity::root_envelope`: ciphertext only, opened by
//! a passkey's PRF output or a recovery key). Every host the person is a
//! member of keeps a copy, so that on a new browser the passkey alone finds
//! the person again: the passkey names the root, any of their hosts answers
//! with the envelope, the passkey opens it.
//!
//! The person's profile ([`Kind::Profile`]: `{version: 1, user_public_key,
//! display_name, photo?}`, JSON) is kept the same way, so each of their
//! devices, paired or signed in, shows the name and picture they chose, and
//! everyone here sees that picture beside their name. The picture is a small
//! square image as a `data:` URL (PNG, JPEG or WebP, at most
//! [`MAX_PHOTO_CHARS`] characters); without one their initials show.
//!
//! `POST /peer/v1/envelope` keeps one: `{envelope, issued_at_ms, signature}`
//! where `envelope` is the envelope's JSON text and `signature` (hex) the
//! root's Ed25519 signature over [`LABEL`], `issued_at_ms` (u64 little
//! endian) and that text. Only a root admitted to a room here may keep one,
//! and only a newer statement replaces the kept one, so an old envelope
//! (with a passkey since dropped) cannot be replayed over it.
//! `POST /peer/v1/envelope/fetch` with the root public key (hex) as its body
//! answers `{"envelope": <the kept envelope>}`, or `{"envelope": null}` when
//! none is kept (a new person: the usual case, not an error).
//! `/peer/v1/profile` and `/peer/v1/profile/fetch` do the same for a
//! profile (`{profile, issued_at_ms, signature}`, signed under
//! [`PROFILE_LABEL`]; `{"profile": null}` when none is kept).
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tilo_user_identity::root_envelope::RootEnvelope;

/// What a root signs to have its envelope kept.
pub(super) const LABEL: &[u8] = b"tilo root envelope v1\0";
/// An envelope with a handful of passkeys is about 2 KB.
pub(super) const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
/// What a root signs to have its profile kept.
pub(super) const PROFILE_LABEL: &[u8] = b"tilo person profile v1\0";
/// A profile's picture, as its `data:` URL: a 96 px square encodes in a few
/// kilobytes, so this bounds a profile without cramping one.
pub(super) const MAX_PHOTO_CHARS: usize = 48 * 1024;
/// A profile is a name, a key and at most one small picture.
const MAX_PROFILE_BYTES: usize = MAX_PHOTO_CHARS + 1024;
/// The image types a profile's picture may be.
const PHOTO_PREFIXES: [&str; 3] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
];
/// How far ahead of this host's clock a statement may be dated.
const MAX_SKEW_MS: u64 = 10 * 60 * 1000;

/// Which of a member's records a store keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// The root envelope.
    Envelope,
    /// The person's profile: the name they chose.
    Profile,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    version: u32,
    user_public_key: String,
    display_name: String,
    /// The picture they chose, as a `data:` URL; none shows their initials.
    #[serde(default)]
    photo: Option<String>,
}

/// Whether `photo` is a picture a profile may carry: a bounded base64
/// `data:` URL of an image type pages draw.
fn valid_photo(photo: &str) -> bool {
    photo.len() <= MAX_PHOTO_CHARS
        && PHOTO_PREFIXES.iter().any(|prefix| {
            photo.strip_prefix(prefix).is_some_and(|data| {
                !data.is_empty()
                    && data
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            })
        })
}

impl Kind {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Envelope => LABEL,
            Self::Profile => PROFILE_LABEL,
        }
    }

    /// The root `record` (JSON text) belongs to, once it reads as this kind.
    fn root(self, record: &str) -> Result<[u8; 32], Refusal> {
        match self {
            Self::Envelope => {
                if record.len() > MAX_ENVELOPE_BYTES {
                    return Err(Refusal::Malformed("envelope too large"));
                }
                let envelope: RootEnvelope = serde_json::from_str(record)
                    .map_err(|_| Refusal::Malformed("not a root envelope"))?;
                if envelope.version != tilo_user_identity::root_envelope::ROOT_ENVELOPE_VERSION
                    || envelope.wraps.is_empty()
                {
                    return Err(Refusal::Malformed("not a root envelope"));
                }
                root_of(&envelope.root_public_key)
                    .ok_or(Refusal::Malformed("malformed root public key"))
            }
            Self::Profile => {
                if record.len() > MAX_PROFILE_BYTES {
                    return Err(Refusal::Malformed("profile too large"));
                }
                let profile: Profile = serde_json::from_str(record)
                    .map_err(|_| Refusal::Malformed("not a profile"))?;
                let name = profile.display_name.as_str();
                if profile.version != 1
                    || tilo_user_identity::display_name(name) != Ok(name)
                    || profile
                        .photo
                        .as_deref()
                        .is_some_and(|photo| !valid_photo(photo))
                {
                    return Err(Refusal::Malformed("not a profile"));
                }
                root_of(&profile.user_public_key)
                    .ok_or(Refusal::Malformed("malformed root public key"))
            }
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Envelope => "envelope",
            Self::Profile => "profile",
        }
    }
}

#[derive(serde::Deserialize)]
struct Publish {
    #[serde(alias = "profile")]
    envelope: String,
    issued_at_ms: u64,
    signature: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    issued_at_ms: u64,
    envelope: String,
}

/// Why an envelope was not kept, as the page hears it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Malformed(&'static str),
    BadSignature,
    NotMember,
    Stale,
    Storage,
}

impl Refusal {
    pub(super) const fn message(&self) -> &'static str {
        match self {
            Self::Malformed(why) => why,
            Self::BadSignature => "the root did not sign this",
            Self::NotMember => "this host keeps this for its members only",
            Self::Stale => "a newer one is already kept",
            Self::Storage => "it could not be kept",
        }
    }
}

pub(super) struct Envelopes {
    dir: PathBuf,
    kind: Kind,
    /// One write at a time, so "newer than kept" holds across requests.
    write: tokio::sync::Mutex<()>,
}

fn root_of(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    hex::decode(text).ok()?.try_into().ok()
}

impl Envelopes {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self::of(Kind::Envelope, dir)
    }

    /// A store of the members' records of `kind`, under `dir`.
    pub(super) fn of(kind: Kind, dir: PathBuf) -> Self {
        Self {
            dir,
            kind,
            write: tokio::sync::Mutex::new(()),
        }
    }

    fn path(&self, root: &[u8; 32]) -> PathBuf {
        self.dir.join(format!("{}.json", hex::encode(root)))
    }

    fn kept(path: &Path) -> Option<Kept> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// The kept envelope of the root named by `body` (hex), as JSON text.
    fn fetch(&self, body: &[u8]) -> Option<String> {
        let root = root_of(std::str::from_utf8(body).ok()?.trim())?;
        Self::kept(&self.path(&root)).map(|kept| kept.envelope)
    }

    /// When the record kept for `root` was signed, Unix milliseconds.
    pub(super) fn issued_at(&self, root: &[u8; 32]) -> Option<u64> {
        Self::kept(&self.path(root)).map(|kept| kept.issued_at_ms)
    }

    /// The answer to a fetch of the root `body` names (hex): `{"<kind>":
    /// record}`, or `{"<kind>": null}` when nothing is kept for it (a new
    /// person, the usual case). `None` when `body` names no root.
    pub(super) fn answer(&self, body: &[u8]) -> Option<Vec<u8>> {
        root_of(std::str::from_utf8(body).ok()?.trim())?;
        let kept = self
            .fetch(body)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .unwrap_or(serde_json::Value::Null);
        serde_json::to_vec(&serde_json::json!({ self.kind.name(): kept })).ok()
    }

    /// Keep the envelope `body` publishes, when its root signed it, is
    /// `member` here, and dated it after the one kept.
    pub(super) async fn publish<F, Fut>(
        &self,
        body: &[u8],
        now_ms: u64,
        member: F,
    ) -> Result<(), Refusal>
    where
        F: FnOnce([u8; 32]) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let publish: Publish =
            serde_json::from_slice(body).map_err(|_| Refusal::Malformed("malformed body"))?;
        let root = self.kind.root(&publish.envelope)?;
        if publish.issued_at_ms > now_ms.saturating_add(MAX_SKEW_MS) {
            return Err(Refusal::Malformed("dated in the future"));
        }
        let signature: [u8; 64] = hex::decode(&publish.signature)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Refusal::Malformed("malformed signature"))?;
        let label = self.kind.label();
        let mut statement = Vec::with_capacity(
            label
                .len()
                .saturating_add(8)
                .saturating_add(publish.envelope.len()),
        );
        statement.extend_from_slice(label);
        statement.extend_from_slice(&publish.issued_at_ms.to_le_bytes());
        statement.extend_from_slice(publish.envelope.as_bytes());
        VerifyingKey::from_bytes(&root)
            .map_err(|_| Refusal::Malformed("malformed root public key"))?
            .verify(&statement, &Signature::from_bytes(&signature))
            .map_err(|_| Refusal::BadSignature)?;
        if !member(root).await {
            return Err(Refusal::NotMember);
        }
        let _write = self.write.lock().await;
        let path = self.path(&root);
        if Self::kept(&path).is_some_and(|kept| kept.issued_at_ms >= publish.issued_at_ms) {
            return Err(Refusal::Stale);
        }
        let bytes = serde_json::to_vec(&Kept {
            issued_at_ms: publish.issued_at_ms,
            envelope: publish.envelope,
        })
        .map_err(|_| Refusal::Storage)?;
        write_atomic(&self.dir, &path, &bytes).map_err(|error| {
            tracing::warn!(%error, kind = self.kind.name(), "member record not kept");
            Refusal::Storage
        })
    }
}

fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn envelope(root: &SigningKey) -> String {
        serde_json::json!({
            "version": 1,
            "root_public_key": hex::encode(root.verifying_key().to_bytes()),
            "wraps": [{"kind": "recovery", "salt": "AA", "nonce": "AA", "ciphertext": "AA"}],
        })
        .to_string()
    }

    fn statement(root: &SigningKey, envelope: &str, at: u64) -> Vec<u8> {
        let mut signed = LABEL.to_vec();
        signed.extend_from_slice(&at.to_le_bytes());
        signed.extend_from_slice(envelope.as_bytes());
        serde_json::to_vec(&serde_json::json!({
            "envelope": envelope,
            "issued_at_ms": at,
            "signature": hex::encode(root.sign(&signed).to_bytes()),
        }))
        .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_member_keeps_its_envelope_and_a_new_browser_fetches_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let envelopes = Envelopes::new(dir.path().join("envelopes"));
        let root = SigningKey::from_bytes(&[7; 32]);
        let text = envelope(&root);
        let hex_root = hex::encode(root.verifying_key().to_bytes());
        assert_eq!(envelopes.fetch(hex_root.as_bytes()), None);
        envelopes
            .publish(&statement(&root, &text, 1_000), 1_000, |_| async { true })
            .await
            .expect("kept");
        assert_eq!(envelopes.fetch(hex_root.as_bytes()), Some(text.clone()));
        // The same or an older statement does not replace it.
        assert_eq!(
            envelopes
                .publish(&statement(&root, &text, 1_000), 2_000, |_| async { true })
                .await,
            Err(Refusal::Stale)
        );
        envelopes
            .publish(&statement(&root, &text, 1_500), 2_000, |_| async { true })
            .await
            .expect("newer kept");
    }

    #[tokio::test]
    async fn strangers_forgeries_and_far_future_dates_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let envelopes = Envelopes::new(dir.path().to_path_buf());
        let root = SigningKey::from_bytes(&[7; 32]);
        let text = envelope(&root);
        assert_eq!(
            envelopes
                .publish(&statement(&root, &text, 1), 1, |_| async { false })
                .await,
            Err(Refusal::NotMember)
        );
        let other = SigningKey::from_bytes(&[8; 32]);
        assert_eq!(
            envelopes
                .publish(&statement(&other, &text, 1), 1, |_| async { true })
                .await,
            Err(Refusal::BadSignature)
        );
        assert!(matches!(
            envelopes
                .publish(&statement(&root, &text, MAX_SKEW_MS + 10), 1, |_| async {
                    true
                })
                .await,
            Err(Refusal::Malformed(_))
        ));
        assert_eq!(envelopes.fetch(b"not hex"), None);
        assert_eq!(envelopes.answer(b"not hex"), None);
    }

    #[tokio::test]
    async fn a_member_keeps_a_profile_with_a_name_a_picture_and_nothing_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profiles = Envelopes::of(Kind::Profile, dir.path().join("profiles"));
        let root = SigningKey::from_bytes(&[7; 32]);
        let hex_root = hex::encode(root.verifying_key().to_bytes());
        let profile = |name: &str| {
            serde_json::json!({"version": 1, "user_public_key": hex_root, "display_name": name})
                .to_string()
        };
        let signed = |label: &[u8], text: &str, at: u64| {
            let mut statement = label.to_vec();
            statement.extend_from_slice(&at.to_le_bytes());
            statement.extend_from_slice(text.as_bytes());
            serde_json::to_vec(&serde_json::json!({
                "profile": text,
                "issued_at_ms": at,
                "signature": hex::encode(root.sign(&statement).to_bytes()),
            }))
            .unwrap_or_default()
        };
        let ada = profile("Ada Lovelace");
        profiles
            .publish(&signed(PROFILE_LABEL, &ada, 1_000), 1_000, |_| async {
                true
            })
            .await
            .expect("kept");
        assert_eq!(profiles.fetch(hex_root.as_bytes()), Some(ada.clone()));
        let answered: serde_json::Value =
            serde_json::from_slice(&profiles.answer(hex_root.as_bytes()).expect("a root"))
                .expect("json");
        assert_eq!(
            answered,
            serde_json::json!({"profile": serde_json::from_str::<serde_json::Value>(&ada).expect("json")})
        );
        // Nothing kept for a root is an answer too, not a failure.
        let stranger = hex::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes());
        assert_eq!(
            profiles.answer(stranger.as_bytes()),
            Some(br#"{"profile":null}"#.to_vec())
        );
        for bad in [profile(""), profile(" Ada"), profile("A\u{7}da")] {
            assert!(matches!(
                profiles
                    .publish(&signed(PROFILE_LABEL, &bad, 2_000), 2_000, |_| async {
                        true
                    })
                    .await,
                Err(Refusal::Malformed(_))
            ));
        }
        // A signature over an envelope's label does not keep a profile.
        assert_eq!(
            profiles
                .publish(&signed(LABEL, &profile("Grace"), 3_000), 3_000, |_| async {
                    true
                })
                .await,
            Err(Refusal::BadSignature)
        );
        // A picture rides along, and when it was kept is known.
        let pictured = serde_json::json!({"version": 1, "user_public_key": hex_root,
            "display_name": "Ada Lovelace", "photo": "data:image/webp;base64,UklGRg=="})
        .to_string();
        profiles
            .publish(&signed(PROFILE_LABEL, &pictured, 4_000), 4_000, |_| async {
                true
            })
            .await
            .expect("kept with a picture");
        assert_eq!(profiles.fetch(hex_root.as_bytes()), Some(pictured));
        assert_eq!(
            profiles.issued_at(&root.verifying_key().to_bytes()),
            Some(4_000)
        );
        // Only a bounded image, never a link elsewhere or a script.
        let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_PHOTO_CHARS));
        for bad in [
            "https://example.org/a.png",
            "data:text/html;base64,PHNjcmlwdD4=",
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:image/png;base64,",
            "data:image/png;base64,<>",
            huge.as_str(),
        ] {
            let text = serde_json::json!({"version": 1, "user_public_key": hex_root,
                "display_name": "Ada", "photo": bad})
            .to_string();
            assert!(
                matches!(
                    profiles
                        .publish(&signed(PROFILE_LABEL, &text, 5_000), 5_000, |_| async {
                            true
                        })
                        .await,
                    Err(Refusal::Malformed(_))
                ),
                "{}",
                &bad[..bad.len().min(40)]
            );
        }
    }
}
