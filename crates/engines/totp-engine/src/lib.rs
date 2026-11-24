//! Authenticator's device work: a one-time code from a secret and an instant, a setup link read
//! into the account it names, and the QR code of a picture a person chose.
//!
//! An app runs sandboxed and reads no clock or picture of its own, so this crate is what it
//! reaches through `engine.call`. It registers as the engine of `so.tilo.totp` and answers that
//! app alone; every answer is JSON the package reads into its own records.
//!
//! Answers (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `code` | `{account, algorithm, digits, period, at, at_ms}` | `{code, left}` |
//! | `parse` | `{uri}` | `{issuer, label, secret, algorithm, digits, period}` |
//! | `scan` | `{address}` | `{uri}` |
//! | `check` | `{secret}` | `{ok}` |
//! | `keep` | `{account, secret}` | `{kept}` |
//! | `read` | `{account}` | `{secret}` |
//! | `forget` | `{account}` | `{forgotten}` |
//!
//! A seed is never in a document. `keep` writes it to this device's secret store under the
//! account's id, `code` reads it from there, and `read` answers it only to show the form the key
//! it already holds. The workspace syncs the accounts, never their keys.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::json;
use tilo_secrets::SecretStore;
use zeroize::Zeroizing;

mod qr;
mod totp;

pub use totp::{Algorithm, Setup};

/// The app this engine answers.
pub const APP: &str = "so.tilo.totp";

/// The purpose every account's seed is kept under, beside the account's id.
const SEED: &str = "seed";

/// The engine of Authenticator, as the session keeps it for the app.
pub struct TotpEngine {
    /// This device's secret store, under the app's own name.
    store: SecretStore,
    /// The seeds read so far: a code is made every second, and the store is read once an
    /// account. A seed leaves the store only into this process's memory.
    seeds: Mutex<HashMap<String, Zeroizing<String>>>,
}

impl Default for TotpEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TotpEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::with_store(SecretStore::for_app(APP))
    }

    /// An engine keeping its seeds in `store`: a test's own, or one the device hands it.
    #[must_use]
    pub fn with_store(store: SecretStore) -> Self {
        Self {
            store,
            seeds: Mutex::new(HashMap::new()),
        }
    }

    fn seeds(&self) -> MutexGuard<'_, HashMap<String, Zeroizing<String>>> {
        self.seeds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The seed kept for `account`, or the words a person reads when this device holds none.
    fn seed(&self, account: &str) -> Result<Zeroizing<String>, String> {
        if let Some(seed) = self.seeds().get(account) {
            return Ok(seed.clone());
        }
        let found = self
            .store
            .item(SEED, account)
            .get_blocking()
            .map_err(|error| format!("this device could not read the account’s key: {error}"))?;
        let seed = found.map_or_else(
            || Zeroizing::new(String::new()),
            |secret| Zeroizing::new(String::from_utf8_lossy(secret.expose()).into_owned()),
        );
        if seed.is_empty() {
            return Err("this account's key is not on this device".to_owned());
        }
        self.seeds().insert(account.to_owned(), seed.clone());
        Ok(seed)
    }

    fn op_keep(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            account: String,
            secret: String,
        }
        let ask: Ask = read_ask(input)?;
        let seed = totp::normalise_secret(&ask.secret);
        if totp::decode_secret(&seed).is_none() {
            return Err("this account’s key is not base32".to_owned());
        }
        self.store
            .item(SEED, &ask.account)
            .put_blocking(seed.as_bytes())
            .map_err(|error| format!("this device could not keep the account’s key: {error}"))?;
        self.seeds()
            .insert(ask.account, Zeroizing::new(seed));
        Ok(json!({ "kept": true }).to_string())
    }

    fn op_read(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            account: String,
        }
        let ask: Ask = read_ask(input)?;
        let seed = self.seed(&ask.account).unwrap_or_else(|_| Zeroizing::new(String::new()));
        Ok(json!({ "secret": seed.as_str() }).to_string())
    }

    fn op_forget(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            account: String,
        }
        let ask: Ask = read_ask(input)?;
        let forgotten = self
            .store
            .item(SEED, &ask.account)
            .delete_blocking()
            .map_err(|error| format!("this device could not forget the account’s key: {error}"))
            .is_ok();
        self.seeds().remove(&ask.account);
        Ok(json!({ "forgotten": forgotten }).to_string())
    }

    fn op_code(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            account: String,
            #[serde(default)]
            algorithm: String,
            #[serde(default = "six")]
            digits: u8,
            #[serde(default = "thirty")]
            period: u32,
            /// The instant the window names, in milliseconds since the Unix epoch.
            #[serde(default)]
            at_ms: i64,
            /// Its RFC 3339 form, read when the milliseconds are not given.
            #[serde(default)]
            at: String,
        }
        let ask: Ask = read_ask(input)?;
        let seed = self.seed(&ask.account)?;
        let at_ms = if ask.at_ms != 0 {
            ask.at_ms
        } else {
            timestamp_ms(&ask.at)?
        };
        let code = totp::code_at(
            &seed,
            Algorithm::parse(&ask.algorithm),
            ask.digits,
            ask.period,
            at_ms as f64 / 1000.0,
        )
        .ok_or_else(|| "this account’s key is not base32".to_owned())?;
        Ok(json!({ "code": code.code, "left": code.left }).to_string())
    }

    fn op_parse(&self, input: &str) -> Result<String, String> {
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

    fn op_scan(&self, input: &str) -> Result<String, String> {
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

    fn op_check(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            secret: String,
        }
        let ask: Ask = read_ask(input)?;
        Ok(json!({ "ok": totp::decode_secret(&ask.secret).is_some() }).to_string())
    }
}

fn six() -> u8 {
    6
}

fn thirty() -> u32 {
    30
}

/// An RFC 3339 instant as milliseconds since the Unix epoch.
fn timestamp_ms(at: &str) -> Result<i64, String> {
    let stamp: jiff::Timestamp = at
        .parse()
        .map_err(|_| "the instant does not read".to_owned())?;
    Ok(stamp.as_millisecond())
}

impl tilolang_tilo::engine::Provider for TotpEngine {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "code" => self.op_code(input),
            "parse" => self.op_parse(input),
            "scan" => self.op_scan(input),
            "check" => self.op_check(input),
            "keep" => self.op_keep(input),
            "read" => self.op_read(input),
            "forget" => self.op_forget(input),
            other => Err(format!("`{other}` is no op this engine runs")),
        }
    }

    fn start(&self, _op: &str, _input: &str) -> Result<String, String> {
        Err("Authenticator asks no job of its device".to_owned())
    }

    fn status(&self, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// Register Authenticator's engine for the session's lifetime.
pub fn register() {
    tilolang_tilo::engine::register(APP, std::sync::Arc::new(TotpEngine::new()));
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
