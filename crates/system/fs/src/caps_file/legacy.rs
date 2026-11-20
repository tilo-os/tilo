//! Read the disk formats that predate member ids and the shared lifetime type.

use collab_types::Ed25519PublicKey;
use serde::Deserialize;
use serde_json::{Map, Value};
use tilo_action_policy::{Grantee, Lifetime, MemberId, PersonId};

#[derive(Deserialize)]
enum Expiry {
    Never,
    AtUnix(u64),
}

#[derive(Deserialize)]
enum LegacyMember {
    Person(PersonId),
    Agent(String),
}

impl From<LegacyMember> for MemberId {
    fn from(value: LegacyMember) -> Self {
        match value {
            LegacyMember::Person(person) => Self::Person(person),
            LegacyMember::Agent(agent) => Self::Agent(agent),
        }
    }
}

#[derive(Deserialize)]
enum LegacyRecipient {
    User(Ed25519PublicKey),
    #[serde(rename = "Principal")]
    Member(LegacyMember),
}

impl From<LegacyRecipient> for Grantee {
    fn from(value: LegacyRecipient) -> Self {
        Self::Member(match value {
            LegacyRecipient::User(key) => MemberId::Person(PersonId::from_root(*key.as_bytes())),
            LegacyRecipient::Member(member) => member.into(),
        })
    }
}

/// Translate the older fields before the strict current decoder checks the
/// rest. Unsupported identities and non-durable lifetimes refuse the file,
/// since dropping even one restriction could widen access.
pub(super) fn translate(authority: &mut Value) -> Result<(), String> {
    let Some(capabilities) = authority
        .get_mut("capabilities")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for capability in capabilities.iter_mut().filter_map(Value::as_object_mut) {
        if let Some(expiry) = old_field(capability, "expiry", "lifetime")? {
            let expiry = serde_json::from_value(expiry).map_err(|error| error.to_string())?;
            let lifetime = match expiry {
                Expiry::Never => Lifetime::UntilRevoked,
                Expiry::AtUnix(seconds) => Lifetime::Until(
                    seconds
                        .checked_mul(1000)
                        .ok_or("the expiry cannot be expressed in milliseconds")?,
                ),
            };
            let lifetime = serde_json::to_value(lifetime).map_err(|error| error.to_string())?;
            capability.insert("lifetime".to_owned(), lifetime);
        }
        if let Some(recipient) = old_field(capability, "subject", "grantee")? {
            let recipient: LegacyRecipient =
                serde_json::from_value(recipient).map_err(|error| error.to_string())?;
            let grantee = serde_json::to_value(Grantee::from(recipient))
                .map_err(|error| error.to_string())?;
            capability.insert("grantee".to_owned(), grantee);
            if let Some(issuer) = capability.get_mut("granted_by") {
                let member: Option<LegacyMember> =
                    serde_json::from_value(issuer.take()).map_err(|error| error.to_string())?;
                *issuer = serde_json::to_value(member.map(MemberId::from))
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn old_field(
    capability: &mut Map<String, Value>,
    old: &str,
    current: &str,
) -> Result<Option<Value>, String> {
    let old_value = capability.remove(old);
    if old_value.is_some() && capability.contains_key(current) {
        return Err(format!("both {old} and {current} are present"));
    }
    Ok(old_value)
}
