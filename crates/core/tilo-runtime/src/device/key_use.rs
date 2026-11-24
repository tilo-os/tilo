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
//! - [`ENROL_BOOT`]: a boot's enrolment asked of the person's other devices, over the device's
//!   one endpoint.
//!
//! [`take_device_key`]: tilo_device_identity::persist::take_device_key

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::{ActionParam, ParamType};
use tilo_device_identity::login::{
    CHECK_CALLER_PROOF, ENDORSE_IMAGE, ENROL_BOOT, MALFORMED_ASK, OPEN_VAULT, SEAL_VAULT,
    SEALED_BY_ANOTHER_KEY,
};
use tilo_user_identity::ImageEndorsement;
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};

/// The actions a person's programs ask with the device key: the Secret Service's.
pub(crate) const FOR_A_PERSON: [&str; 3] = [SEAL_VAULT, OPEN_VAULT, CHECK_CALLER_PROOF];

/// The actions the device's own system asks (root, or the daemon's own account): the boot chain's
/// and the update's.
pub(crate) const FOR_THE_SYSTEM: [&str; 2] = [ENDORSE_IMAGE, ENROL_BOOT];

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
            let caller_key = tilo_seal::secret_service::CallerKey::derive(seed.as_ref()).map_err(
                |error| Refusal {
                    code: "device-action",
                    message: error.to_string(),
                },
            )?;
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
                ActionParam::required(
                    "challenge",
                    ParamType::Text,
                    "The challenge, in hex.",
                ),
                ActionParam::required("caller", ParamType::Text, "The caller's bus name."),
                ActionParam::required(
                    "program",
                    ParamType::Text,
                    "The program it named, in hex.",
                ),
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
            ENROL_BOOT,
            "Enrol a boot with another device",
            "Ask the person's other devices to enrol this device's boot, over its endpoint.",
            &[
                ActionParam::required(
                    "request",
                    ParamType::Text,
                    "The enrolment request, its frame in hex.",
                ),
                ActionParam::required(
                    "devices",
                    ParamType::Array,
                    "The devices to ask, each its key in hex.",
                ),
            ],
        ),
    ]
}

/// How long a device asked to enrol a boot takes to be dialled.
const DIAL_FOR: std::time::Duration = std::time::Duration::from_secs(20);

/// Ask `devices` in turn, over `endpoint`, to enrol the boot `args` names, until one answers:
/// what it answered, as the frame's body in hex.
///
/// # Errors
/// The arguments cannot be read, or no device could be asked.
pub(crate) async fn enrol_boot(
    endpoint: &iroh::Endpoint,
    args: &Value,
) -> Result<Value, Refusal> {
    use tilo_unlock_protocol::{APPROVAL_TIMEOUT_NS, ENROL_ALPN, EnrolRequest, decode_frame};
    let frame = bytes(args, "request")?;
    // The request is an enrolment request, whole: nothing else is carried for the caller.
    let (prefix, body) = frame
        .split_first_chunk::<4>()
        .ok_or_else(|| malformed("the request is no frame"))?;
    if tilo_unlock_protocol::frame_len(*prefix).ok() != Some(body.len()) {
        return Err(malformed("the request's frame is cut short"));
    }
    decode_frame::<EnrolRequest>(body)
        .map_err(|error| malformed(format!("the request is no enrolment request: {error}")))?;
    let devices = args
        .get("devices")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("devices are needed"))?
        .iter()
        .map(|device| {
            let key: [u8; 32] = device
                .as_str()
                .and_then(|key| hex::decode(key).ok())
                .and_then(|key| key.try_into().ok())
                .ok_or_else(|| malformed("a device is its key, 32 bytes in hex"))?;
            iroh::EndpointId::from_bytes(&key).map_err(|error| malformed(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // How long a device may take to put the request to its person.
    let decide_for = std::time::Duration::from_secs(APPROVAL_TIMEOUT_NS / 1_000_000_000 + 60);
    // The person's devices are reached over the relays this device's endpoint was made with
    // ([`super::services`]): with no lookup to resolve a key, a bare key reaches no one, and the
    // endpoint names its home relay only once it reached it.
    let relays: Vec<iroh::RelayUrl> = tilo_endpoint_reachability::relays().relays.urls();
    let mut unable = None;
    for device in devices {
        let address = relays
            .iter()
            .cloned()
            .fold(iroh::EndpointAddr::new(device), iroh::EndpointAddr::with_relay_url);
        let asked = async {
            let connection = tokio::time::timeout(DIAL_FOR, endpoint.connect(address, ENROL_ALPN))
                .await
                .map_err(|_| "no answer".to_owned())?
                .map_err(|error| error.to_string())?;
            let answered = async {
                let (mut send, mut recv) = connection
                    .open_bi()
                    .await
                    .map_err(|error| error.to_string())?;
                send.write_all(&frame)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut prefix = [0_u8; 4];
                recv.read_exact(&mut prefix)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut body = vec![
                    0_u8;
                    tilo_unlock_protocol::frame_len(prefix)
                        .map_err(|error| error.to_string())?
                ];
                recv.read_exact(&mut body)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(body)
            };
            // The person is asked on their device, and may take their time: the other side's own
            // answering window bounds it.
            let body = tokio::time::timeout(decide_for, answered)
                .await
                .map_err(|_| "the device took too long to answer".to_owned())??;
            connection.close(0u32.into(), b"done");
            Ok::<_, String>(body)
        };
        match asked.await {
            Ok(body) => return Ok(json!({ "reply": hex::encode(body) })),
            Err(error) => unable = Some(error),
        }
    }
    Err(Refusal {
        code: "device-action",
        message: unable.map_or_else(
            || "no device was named to ask".to_owned(),
            |error| format!("none of your other devices answered: {error}"),
        ),
    })
}
