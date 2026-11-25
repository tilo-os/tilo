//! What the daemon does with the device key for the other programs of this computer.
//!
//! The daemon is the one process that keeps the device key ([`take_device_key`]). A program that
//! needs the device to seal, prove or sign something asks one of these actions, and gets back
//! what the key made, never the key or anything derived from it:
//!
//! - [`SEAL_VAULT`] and [`OPEN_VAULT`]: a person's Secret Service vault, sealed under their store
//!   key on this device, which the device key, their salt and their account derive;
//! - [`CHECK_CALLER_PROOF`]: whether a process the Secret Service cannot look at proved it has
//!   the device's identity;
//! - [`ENDORSE_IMAGE`]: an image endorsement the device signs, under the image-signing grant its
//!   person's root gave it;
//! - [`SET_UP_UNLOCK`]: this device's boot key enrolled for disk unlock, certified by the person's
//!   root where it is kept and asked of them there;
//! - [`SIGN`]: a message the device signs, for a program that proves it is this device;
//! - [`RENEW_CERTIFICATES`]: this device's certificates of the asking account's person renewed
//!   with the root kept here, which is sealed with the device key.
//!
//! The daemon answers these from the moment it took the key, before its person's services run
//! or any workspace is admitted: what the key signs depends on neither.
//!
//! [`take_device_key`]: tilo_device_identity::persist::take_device_key

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::{ActionParam, ParamType};
use tilo_device_identity::login::{
    CHECK_CALLER_PROOF, ENDORSE_IMAGE, SET_UP_UNLOCK, MALFORMED_ASK, OPEN_VAULT, SEAL_VAULT,
    RENEW_CERTIFICATES, SEALED_BY_ANOTHER_KEY, SIGN,
};
use tilo_user_identity::ImageEndorsement;
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};

/// The actions a person's programs ask with the device key: the Secret Service's, and a
/// signature as the device. Their answers are kept in no invocation record: an opened vault is a
/// secret, and these are a program's steps, never acts the person's history shows.
pub(crate) const FOR_A_PERSON: [&str; 4] = [SEAL_VAULT, OPEN_VAULT, CHECK_CALLER_PROOF, SIGN];

/// The actions the device's own system asks (root, or the daemon's own account): the boot chain's
/// and the update's.
pub(crate) const FOR_THE_SYSTEM: [&str; 2] = [ENDORSE_IMAGE, SET_UP_UNLOCK];

/// Whether `action` is one of what the device key does.
pub(crate) fn asked_of_the_key(action: &str) -> bool {
    FOR_A_PERSON.contains(&action)
        || FOR_THE_SYSTEM.contains(&action)
        || action == RENEW_CERTIFICATES
}

/// Why one of these actions was not done: its stable code, and its words.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

fn malformed(message: impl Into<String>) -> Refusal {
    Refusal {
        code: MALFORMED_ASK,
        message: message.into(),
    }
}

/// The text argument `name` of `args`.
fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str, Refusal> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(format!("{name} is needed")))
}

/// The bytes the hex argument `name` of `args` names.
pub(crate) fn bytes(args: &Value, name: &str) -> Result<Vec<u8>, Refusal> {
    hex::decode(text(args, name)?).map_err(|_| malformed(format!("{name} is not hex")))
}

/// The person's salt a vault action names: 32 bytes.
fn salt(args: &Value) -> Result<zeroize::Zeroizing<[u8; 32]>, Refusal> {
    let bytes = zeroize::Zeroizing::new(bytes(args, "salt")?);
    let salt: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| malformed("salt is 32 bytes"))?;
    Ok(zeroize::Zeroizing::new(salt))
}

/// Answer `action`, one of [`FOR_A_PERSON`] or [`ENDORSE_IMAGE`], with the device key `key`.
///
/// # Errors
/// The arguments cannot be read, or what they name cannot be sealed, opened or signed.
pub(crate) fn answer(action: &str, args: &Value, key: &SigningKey) -> Result<Value, Refusal> {
    let seed = zeroize::Zeroizing::new(key.to_bytes());
    match action {
        SEAL_VAULT => {
            let salt = salt(args)?;
            let plain = zeroize::Zeroizing::new(bytes(args, "plain")?);
            let sealed = tilo_seal::secret_service::seal_vault(
                seed.as_ref(),
                &salt,
                text(args, "account")?.as_bytes(),
                &plain,
            )
            .map_err(|error| Refusal {
                code: "device-action",
                message: format!("the vault could not be sealed: {error}"),
            })?;
            Ok(json!({ "sealed": hex::encode(sealed) }))
        }
        OPEN_VAULT => {
            let salt = salt(args)?;
            let plain = tilo_seal::secret_service::open_vault(
                seed.as_ref(),
                &salt,
                text(args, "account")?.as_bytes(),
                &bytes(args, "sealed")?,
            )
            .map_err(|error| match error {
                tilo_seal::Error::WrongKey => Refusal {
                    code: SEALED_BY_ANOTHER_KEY,
                    message: "the vault was sealed under another device's key".to_owned(),
                },
                error => malformed(format!("the vault cannot be opened: {error}")),
            })?;
            Ok(json!({ "plain": hex::encode(plain.as_slice()) }))
        }
        CHECK_CALLER_PROOF => {
            let caller_key =
                tilo_seal::secret_service::CallerKey::derive(seed.as_ref()).map_err(|error| {
                    Refusal {
                        code: "device-action",
                        message: error.to_string(),
                    }
                })?;
            let proven = caller_key.verify(
                &bytes(args, "challenge")?,
                text(args, "caller")?,
                &bytes(args, "program")?,
                &bytes(args, "proof")?,
            );
            Ok(json!({ "proven": proven }))
        }
        ENDORSE_IMAGE => {
            let mut endorsement: ImageEndorsement =
                from_canonical_bytes(&bytes(args, "endorsement")?)
                    .map_err(|error| malformed(format!("no image endorsement: {error}")))?;
            if endorsement.signature != [0; 64] {
                return Err(malformed("the endorsement is signed already"));
            }
            tilo_user_identity::sign_image_endorsement(&mut endorsement, key).map_err(|error| {
                Refusal {
                    code: "device-action",
                    message: format!("this device does not endorse the image: {error}"),
                }
            })?;
            let signed = canonical_bytes(&endorsement).map_err(|error| Refusal {
                code: "device-action",
                message: error.to_string(),
            })?;
            Ok(json!({ "endorsement": hex::encode(signed) }))
        }
        SIGN => {
            let signature = ed25519_dalek::Signer::sign(key, &bytes(args, "message")?);
            Ok(json!({ "signature": hex::encode(signature.to_bytes()) }))
        }
        other => Err(malformed(format!("{other} is no action of the device key"))),
    }
}

/// How the device's own actions list these, for the catalogue.
pub(crate) fn descriptors() -> Vec<tilo_actions::ActionDescriptor> {
    use super::ticket::descriptor;
    let vault = |name: &'static str, what: &'static str| {
        [
            ActionParam::required(
                "salt",
                ParamType::Text,
                "The person's own salt, 32 bytes in hex.",
            ),
            ActionParam::required("account", ParamType::Text, "The person's account."),
            ActionParam::required(name, ParamType::Text, what),
        ]
    };
    vec![
        descriptor(
            SEAL_VAULT,
            "Seal a Secret Service vault",
            "Seal the person's Secret Service vault under their store key on this device.",
            &vault("plain", "The vault, its bytes in hex."),
        ),
        descriptor(
            OPEN_VAULT,
            "Open a Secret Service vault",
            "Open the person's Secret Service vault sealed under their store key on this device.",
            &vault("sealed", "The sealed vault, its bytes in hex."),
        ),
        descriptor(
            CHECK_CALLER_PROOF,
            "Check a caller's proof",
            "Whether a process proved to the Secret Service that it has this device's identity.",
            &[
                ActionParam::required("challenge", ParamType::Text, "The challenge, in hex."),
                ActionParam::required("caller", ParamType::Text, "The caller's bus name."),
                ActionParam::required("program", ParamType::Text, "The program it named, in hex."),
                ActionParam::required("proof", ParamType::Text, "Its proof, in hex."),
            ],
        ),
        descriptor(
            ENDORSE_IMAGE,
            "Endorse a system image",
            "Sign an image endorsement as this device, under the grant its person gave it.",
            &[ActionParam::required(
                "endorsement",
                ParamType::Text,
                "The unsigned endorsement, its bytes in hex.",
            )],
        ),
        descriptor(
            SIGN,
            "Sign as this device",
            "Sign a message with this device's key, for a program proving it is this device.",
            &[ActionParam::required(
                "message",
                ParamType::Text,
                "The message, its bytes in hex.",
            )],
        ),
        descriptor(
            RENEW_CERTIFICATES,
            "Renew my certificates here",
            "Renew this computer's certificates for my person that are due, with my root kept \
             here.",
            &[],
        ),
        descriptor(
            SET_UP_UNLOCK,
            "Enrol this computer's disk unlock",
            "Have the person's identity certify this computer's boot key, asking them at a device \
             of theirs that keeps it, and publish its unlock to their devices.",
            &[
                ActionParam::required(
                    "request",
                    ParamType::Text,
                    "The enrolment request, postcard in hex.",
                ),
                ActionParam::required(
                    "half",
                    ParamType::Secret,
                    "The sibling half, in hex.",
                ),
            ],
        ),
    ]
}
