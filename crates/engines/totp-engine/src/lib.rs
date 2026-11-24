//! Authenticator's device work: a setup link read into the account it names, the QR code of a
//! picture a person chose, and whether a key reads as base32.
//!
//! An app runs sandboxed and reads no picture of its own, so this crate is what it reaches
//! through `engine.call`, loaded as the engine library of `so.tilo.totp`
//! (crates/engines/loadable/totp). It answers that app alone.
//!
//! Answers (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `parse` | `{uri}` | `{issuer, label, secret, algorithm, digits, period}` |
//! | `scan` | `{address}` | `{uri}` |
//! | `check` | `{secret}` | `{ok}` |
//!
//! The seed a service gives is a vault item: the workspace's vault keeps it and makes each code
//! where it is, so no key is ever in a document, in this engine or in a record.

use serde::Deserialize;
use serde_json::json;

mod qr;
mod totp;

pub use totp::{Algorithm, Setup};

/// The app this engine answers.
pub const APP: &str = "so.tilo.totp";

/// The engine of Authenticator, as the session keeps it for the app.
#[derive(Default)]
pub struct TotpEngine;

impl TotpEngine {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn op_parse(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            uri: String,
        }
        let ask: Ask = read_ask(input)?;
        let setup = totp::parse_otpauth_uri(&ask.uri)?;
        Ok(json!({
            "issuer": setup.issuer,
            "label": setup.label,
            "secret": setup.secret,
            "algorithm": setup.algorithm.as_str(),
            "digits": setup.digits,
            "period": setup.period,
        })
        .to_string())
    }

    fn op_scan(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
        }
        let ask: Ask = read_ask(input)?;
        let bytes = read_address(&ask.address)
            .ok_or_else(|| "The picture is not on this device.".to_owned())?;
        let uri = qr::decode_image_bytes(&bytes)?;
        Ok(json!({ "uri": uri }).to_string())
    }

    fn op_check(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            secret: String,
        }
        let ask: Ask = read_ask(input)?;
        Ok(json!({ "ok": totp::decode_secret(&ask.secret).is_some() }).to_string())
    }
}

impl TotpEngine {
    /// One `engine.call`: the setup link read, the picture's QR code decoded, or whether a key
    /// reads as base32.
    ///
    /// # Errors
    ///
    /// The ask's or the picture's refusal, and every op this engine does not run.
    pub fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "parse" => Self::op_parse(input),
            "scan" => Self::op_scan(input),
            "check" => Self::op_check(input),
            other => Err(format!("`{other}` is no op this engine runs")),
        }
    }

    /// No ask of this engine runs as a job.
    ///
    /// # Errors
    ///
    /// Every job, since this engine starts none.
    pub fn start(&self, _op: &str, _input: &str) -> Result<String, String> {
        Err("Authenticator asks no job of its device".to_owned())
    }

    /// No ask of this engine runs as a job.
    ///
    /// # Errors
    ///
    /// Every job, since this engine starts none.
    pub fn status(&self, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    /// No ask of this engine runs as a job, so there is none to stop.
    #[must_use]
    pub fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// The bytes of a `tilo-blob://` address, when this device holds them. The language names a
/// file's content so; the content store names the same bytes by the reference under it.
fn read_address(address: &str) -> Option<Vec<u8>> {
    let reference = address.strip_prefix("tilo-blob://").unwrap_or(address);
    tilo_resource::content::read_hash(reference).ok()
}

fn read_ask<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}
