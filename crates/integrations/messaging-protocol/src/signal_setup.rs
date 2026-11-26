//! Read-only Signal account and identity evidence. Never links or changes trust.
use crate::Error;
use serde_json::Value;
#[derive(Clone, PartialEq, Eq)]
pub struct SignalAccount {
    pub aci: String,
    pub number: Option<String>,
}
impl SignalAccount {
    pub fn scope(&self) -> String {
        format!("signal:{}", self.aci)
    }
}
/// # Errors
/// Requires local own-identity evidence and exactly one matching listed number.
pub(crate) fn account(
    accounts: &Value,
    identities: &Value,
    selected: &str,
) -> Result<SignalAccount, Error> {
    let rows = identities
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or(Error::InvalidIdentity)?;
    let row = rows.first().ok_or(Error::InvalidIdentity)?;
    let aci = row
        .get("uuid")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidIdentity)?;
    if uuid::Uuid::parse_str(aci)
        .map_err(|_| Error::InvalidIdentity)?
        .to_string()
        != aci
    {
        return Err(Error::InvalidIdentity);
    }
    let number = row
        .get("number")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidIdentity)?;
    if !valid_number(number) || (selected != number && selected != aci) {
        return Err(Error::WrongAccount);
    }
    let fingerprint = row
        .get("fingerprint")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidIdentity)?;
    identity(
        identities,
        aci,
        &format!("signal-identity:{}", fingerprint.replace(' ', "")),
    )?;
    let accounts = accounts
        .as_array()
        .filter(|rows| rows.len() <= 1024)
        .ok_or(Error::InvalidFrame)?;
    if accounts
        .iter()
        .filter(|row| row.get("number").and_then(Value::as_str) == Some(number))
        .count()
        != 1
    {
        return Err(Error::WrongAccount);
    }
    Ok(SignalAccount {
        aci: aci.into(),
        number: Some(number.into()),
    })
}
fn valid_number(number: &str) -> bool {
    number.strip_prefix('+').is_some_and(|digits| {
        !digits.is_empty()
            && digits.len() <= 15
            && !digits.starts_with('0')
            && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}
/// # Errors
/// Refuses unknown, ambiguous, untrusted or changed recipient identity keys.
pub(crate) fn identity(result: &Value, recipient: &str, revision: &str) -> Result<(), Error> {
    let rows = result
        .as_array()
        .filter(|r| r.len() <= 1024)
        .ok_or(Error::InvalidFrame)?;
    let matched: Vec<_> = rows
        .iter()
        .filter(|r| r.get("uuid").and_then(Value::as_str) == Some(recipient))
        .collect();
    if matched.len() != 1 {
        return Err(Error::InvalidIdentity);
    }
    let row = *matched.first().ok_or(Error::InvalidIdentity)?;
    if row.get("trustLevel").and_then(Value::as_str) != Some("TRUSTED_VERIFIED") {
        return Err(Error::InvalidIdentity);
    }
    let fingerprint = row
        .get("fingerprint")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidIdentity)?;
    let bytes: Vec<_> = fingerprint.split(' ').collect();
    if bytes.len() != 33
        || bytes.first() != Some(&"05")
        || bytes.iter().any(|b| {
            b.len() != 2
                || !b
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        })
    {
        return Err(Error::InvalidIdentity);
    }
    if revision != format!("signal-identity:{}", bytes.concat()) {
        return Err(Error::InvalidIdentity);
    }
    Ok(())
}
