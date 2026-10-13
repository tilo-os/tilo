//! The name and avatar a bot shows for one action, as a webhook does: a
//! bridge posts a Discord user's message under that user's name. The bot
//! stays the author; an appearance is presentation recorded in provenance,
//! never identity. A value that fails validation refuses the invocation; it is
//! never cleaned up, so what a reader sees is exactly what the bot sent.

use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, is_nfc};

/// The longest name, in characters, after trimming.
pub const MAX_NAME_CHARS: usize = 80;
/// The largest avatar image.
pub const MAX_AVATAR_BYTES: usize = 1_048_576;
const MAX_SERVICE_CHARS: usize = 32;
const MAX_ACCOUNT_CHARS: usize = 128;

/// What a bot showed for one action instead of its own name and icon.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Appearance {
    /// The shown name: trimmed, NFC-normalized, 1..=80 characters.
    pub name: String,
    /// BLAKE3 hex of an image in the room's content store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// The outside account this stands for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<Persona>,
}

/// An account on another service, such as `discord` / `80351110224678912`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Persona {
    /// The service, lowercase: `discord`.
    pub service: String,
    /// The account's stable id on that service.
    pub account: String,
}

/// Why an appearance was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AppearanceError {
    /// Nothing is left after trimming.
    #[error("the shown name is empty")]
    EmptyName,
    /// Longer than [`MAX_NAME_CHARS`].
    #[error("the shown name is longer than {MAX_NAME_CHARS} characters")]
    NameTooLong,
    /// Not in the form [`Appearance::new`] makes.
    #[error("the shown name is not trimmed and NFC-normalized")]
    NotNormalized,
    /// A character that hides or reorders text, by code point.
    #[error("the shown name contains the hidden or direction-changing character U+{0:04X}")]
    HiddenCharacter(u32),
    /// The avatar is no content hash.
    #[error("the avatar does not name an image by its content hash")]
    AvatarNotHash,
    /// Larger than [`MAX_AVATAR_BYTES`].
    #[error("the avatar is larger than 1 MiB")]
    AvatarTooLarge,
    /// Not a raster image.
    #[error("the avatar is not a PNG, JPEG, WebP or GIF image")]
    AvatarNotImage,
    /// Not in the room's content store.
    #[error("the avatar is not in the room's content store")]
    AvatarMissing,
    /// The bytes do not hash to the name.
    #[error("the avatar's bytes do not match its hash")]
    AvatarMismatch,
    /// The service name is malformed.
    #[error("the persona's service must be 1 to 32 lowercase letters, digits or '-'")]
    BadService,
    /// The account is malformed.
    #[error("the persona's account must be 1 to 128 visible characters")]
    BadAccount,
}

impl Appearance {
    /// An appearance whose name is trimmed and NFC-normalized, then checked.
    ///
    /// # Errors
    /// The first rule the value breaks.
    pub fn new(
        name: &str,
        avatar: Option<String>,
        persona: Option<Persona>,
    ) -> Result<Self, AppearanceError> {
        let appearance = Self {
            name: name.trim().nfc().collect(),
            avatar,
            persona,
        };
        appearance.validate()?;
        Ok(appearance)
    }

    /// Check a value as received: it must already be in the form
    /// [`Self::new`] makes, so two readers never disagree on the name.
    ///
    /// # Errors
    /// The first rule the value breaks.
    pub fn validate(&self) -> Result<(), AppearanceError> {
        if let Some(hidden) = self.name.chars().find(|c| hidden(*c)) {
            return Err(AppearanceError::HiddenCharacter(u32::from(hidden)));
        }
        if self.name.trim() != self.name || !is_nfc(&self.name) {
            return Err(AppearanceError::NotNormalized);
        }
        match self.name.chars().count() {
            0 => return Err(AppearanceError::EmptyName),
            n if n > MAX_NAME_CHARS => return Err(AppearanceError::NameTooLong),
            _ => {}
        }
        if self
            .avatar
            .as_deref()
            .is_some_and(|hash| !tilo_content::valid_hash(hash))
        {
            return Err(AppearanceError::AvatarNotHash);
        }
        if let Some(persona) = &self.persona {
            let service = persona.service.chars().count();
            if !(1..=MAX_SERVICE_CHARS).contains(&service)
                || !persona
                    .service
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(AppearanceError::BadService);
            }
            let account = persona.account.chars().count();
            if !(1..=MAX_ACCOUNT_CHARS).contains(&account)
                || persona
                    .account
                    .chars()
                    .any(|c| hidden(c) || c.is_whitespace())
            {
                return Err(AppearanceError::BadAccount);
            }
        }
        Ok(())
    }
}

/// Characters that hide or reorder text: a name carrying one could render
/// as somebody else's.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200D}'
                | '\u{2060}'
                | '\u{FEFF}'
                | '\u{180E}'
        )
}

/// Check the bytes an avatar hash names: small, and a raster image. SVG is
/// refused because it is a document, not a picture.
///
/// # Errors
/// [`AppearanceError::AvatarTooLarge`] or [`AppearanceError::AvatarNotImage`].
pub fn check_avatar_bytes(bytes: &[u8]) -> Result<(), AppearanceError> {
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(AppearanceError::AvatarTooLarge);
    }
    match tilo_content::image_format(bytes) {
        Some(format) if matches!(format.extension, "png" | "jpg" | "webp" | "gif") => Ok(()),
        _ => Err(AppearanceError::AvatarNotImage),
    }
}

/// For a door that cannot check that its caller is a bot allowed to show
/// other names: refuse a request carrying an appearance instead of running
/// it under the caller's own name.
///
/// # Errors
/// [`crate::remote::ActionHostError::Refused`] when the request carries one.
pub fn refuse_here(
    request: &crate::remote::InvokeRequest,
) -> Result<(), crate::remote::ActionHostError> {
    match request.appearance {
        Some(_) => Err(crate::remote::ActionHostError::Refused {
            reason: "only a bot allowed to post under other names may carry an appearance".into(),
        }),
        None => Ok(()),
    }
}

/// Admit `appearance` on a request from `caller`, for the room-action door:
/// only a bot whose installation `allows_personas` may show another name, the
/// appearance must be valid, and its avatar must be an image `avatar` finds
/// in the room's content store under its own hash.
///
/// # Errors
/// [`crate::remote::ActionHostError::Refused`] for a caller that may not show
/// another name; [`crate::remote::ActionHostError::InvalidRequest`] for an
/// invalid appearance or avatar.
pub fn admit(
    caller: &crate::remote::Caller,
    appearance: &Appearance,
    allows_personas: impl FnOnce(&str) -> bool,
    avatar: impl FnOnce(&str) -> Option<Vec<u8>>,
) -> Result<Appearance, crate::remote::ActionHostError> {
    use crate::remote::{ActionHostError, CallerPrincipal};
    let CallerPrincipal::Bot(installation) = caller.principal() else {
        return Err(ActionHostError::Refused {
            reason: "only a bot allowed to post under other names may carry an appearance".into(),
        });
    };
    if !allows_personas(installation) {
        return Err(ActionHostError::Refused {
            reason: "this bot's installation does not allow it to post under other names".into(),
        });
    }
    let invalid = |error: AppearanceError| ActionHostError::InvalidRequest {
        reason: error.to_string(),
    };
    appearance.validate().map_err(invalid)?;
    if let Some(hash) = &appearance.avatar {
        let bytes = avatar(hash).ok_or_else(|| invalid(AppearanceError::AvatarMissing))?;
        if !blake3::hash(&bytes).to_hex().eq_ignore_ascii_case(hash) {
            return Err(invalid(AppearanceError::AvatarMismatch));
        }
        check_avatar_bytes(&bytes).map_err(invalid)?;
    }
    Ok(appearance.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0; 32]);
        bytes
    }

    #[test]
    fn a_name_is_trimmed_and_nfc_normalized_once_and_then_must_stay_so() {
        // "e" + combining acute becomes the single precomposed "é".
        let made = Appearance::new("  Rene\u{301}  ", None, None).unwrap();
        assert_eq!(made.name, "Ren\u{e9}");
        let decomposed = Appearance {
            name: "Rene\u{301}".into(),
            avatar: None,
            persona: None,
        };
        assert_eq!(decomposed.validate(), Err(AppearanceError::NotNormalized));
        let padded = Appearance {
            name: " alice".into(),
            avatar: None,
            persona: None,
        };
        assert_eq!(padded.validate(), Err(AppearanceError::NotNormalized));
    }

    #[test]
    fn a_name_is_one_to_eighty_characters() {
        assert_eq!(
            Appearance::new("   ", None, None),
            Err(AppearanceError::EmptyName)
        );
        assert!(Appearance::new(&"é".repeat(80), None, None).is_ok());
        assert_eq!(
            Appearance::new(&"é".repeat(81), None, None),
            Err(AppearanceError::NameTooLong)
        );
    }

    #[test]
    fn hidden_and_direction_changing_characters_refuse_the_name_rather_than_being_cleaned() {
        for (name, hidden) in [
            ("a\u{200B}lice", 0x200B),
            ("alice\u{202E}", 0x202E),
            ("\u{2066}alice", 0x2066),
            ("ali\u{200D}ce", 0x200D),
            ("ali\u{FEFF}ce", 0xFEFF),
            ("ali\u{061C}ce", 0x061C),
            ("ali\u{180E}ce", 0x180E),
            ("ali\nce", 0x0A),
        ] {
            assert_eq!(
                Appearance::new(name, None, None),
                Err(AppearanceError::HiddenCharacter(hidden)),
                "{name:?}"
            );
        }
    }

    #[test]
    fn an_avatar_names_a_content_hash_and_a_persona_a_service_and_account() {
        let hash = blake3::hash(b"x").to_hex().to_string();
        assert!(Appearance::new("alice", Some(hash), None).is_ok());
        assert_eq!(
            Appearance::new("alice", Some("../etc/passwd".into()), None),
            Err(AppearanceError::AvatarNotHash)
        );
        let discord = Persona {
            service: "discord".into(),
            account: "80351110224678912".into(),
        };
        assert!(Appearance::new("alice", None, Some(discord)).is_ok());
        let shouting = Persona {
            service: "Discord".into(),
            account: "1".into(),
        };
        assert_eq!(
            Appearance::new("alice", None, Some(shouting)),
            Err(AppearanceError::BadService)
        );
        let empty = Persona {
            service: "discord".into(),
            account: String::new(),
        };
        assert_eq!(
            Appearance::new("alice", None, Some(empty)),
            Err(AppearanceError::BadAccount)
        );
    }

    #[test]
    fn avatar_bytes_are_a_small_raster_image() {
        assert_eq!(check_avatar_bytes(&png()), Ok(()));
        assert_eq!(
            check_avatar_bytes(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Err(AppearanceError::AvatarNotImage)
        );
        assert_eq!(
            check_avatar_bytes(b"hello"),
            Err(AppearanceError::AvatarNotImage)
        );
        let mut big = png();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert_eq!(
            check_avatar_bytes(&big),
            Err(AppearanceError::AvatarTooLarge)
        );
    }

    #[test]
    fn an_appearance_round_trips_and_leaves_out_what_it_lacks() {
        let appearance = Appearance::new("alice", None, None).unwrap();
        let json = serde_json::to_string(&appearance).unwrap();
        assert_eq!(json, r#"{"name":"alice"}"#);
        assert_eq!(
            serde_json::from_str::<Appearance>(&json).unwrap(),
            appearance
        );
    }

    #[test]
    fn a_door_that_cannot_check_a_bot_refuses_an_appearance() {
        use crate::remote::{ActionHostError, InvokeArgument, InvokeRequest, RemoteActionTarget};
        let mut request = InvokeRequest {
            invocation: crate::InvocationId(uuid::Uuid::from_u128(1)),
            target: RemoteActionTarget {
                provider: "p".into(),
                address: crate::ActionAddress::new("r", "a"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        assert_eq!(refuse_here(&request), Ok(()));
        request.appearance = Some(Appearance::new("alice", None, None).unwrap());
        assert!(matches!(
            refuse_here(&request),
            Err(ActionHostError::Refused { .. })
        ));
    }

    #[test]
    fn only_a_bot_whose_installation_allows_personas_is_admitted() {
        use crate::remote::{ActionHostError, Caller};
        let alice = Appearance::new("alice", None, None).unwrap();
        let bot = Caller::bot("7f1c", [1; 32], [2; 32]);
        assert_eq!(
            admit(
                &bot,
                &alice,
                |installation| installation == "7f1c",
                |_| None
            ),
            Ok(alice.clone())
        );
        assert!(matches!(
            admit(&bot, &alice, |_| false, |_| None),
            Err(ActionHostError::Refused { .. })
        ));
        let person = Caller::local_person("ada");
        assert!(matches!(
            admit(&person, &alice, |_| true, |_| None),
            Err(ActionHostError::Refused { .. })
        ));
        let hidden = Appearance {
            name: "a\u{200B}lice".into(),
            avatar: None,
            persona: None,
        };
        assert!(matches!(
            admit(&bot, &hidden, |_| true, |_| None),
            Err(ActionHostError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn an_avatar_must_be_in_the_store_match_its_hash_and_be_an_image() {
        use crate::remote::{ActionHostError, Caller};
        let bot = Caller::bot("7f1c", [1; 32], [2; 32]);
        let image = png();
        let hash = blake3::hash(&image).to_hex().to_string();
        let shown = Appearance::new("alice", Some(hash.clone()), None).unwrap();
        let stored = |wanted: &str| (wanted == hash).then(|| image.clone());
        assert_eq!(admit(&bot, &shown, |_| true, stored), Ok(shown.clone()));
        let refused = |result: Result<Appearance, ActionHostError>, expected: AppearanceError| {
            assert_eq!(
                result,
                Err(ActionHostError::InvalidRequest {
                    reason: expected.to_string()
                })
            );
        };
        refused(
            admit(&bot, &shown, |_| true, |_| None),
            AppearanceError::AvatarMissing,
        );
        refused(
            admit(&bot, &shown, |_| true, |_| Some(b"other".to_vec())),
            AppearanceError::AvatarMismatch,
        );
        let text = b"not an image".to_vec();
        let named = Appearance::new(
            "alice",
            Some(blake3::hash(&text).to_hex().to_string()),
            None,
        )
        .unwrap();
        refused(
            admit(&bot, &named, |_| true, |_| Some(text.clone())),
            AppearanceError::AvatarNotImage,
        );
    }
}
