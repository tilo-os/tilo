//! Where a person's devices are reached while their Tilo is closed.
//!
//! An app that is closed hears of nothing unless the push service of its
//! system carries it there. A device of a person's hands this server what
//! such a push needs: the token that service gave the app on that device,
//! which of the service's environments the token belongs to, the app it
//! was given to, and a key of the device's own that the words of a push
//! are encrypted for, so that nobody between reads them. The server keeps
//! a few per person in the person's own store, beside their rule and
//! their log, because it is the server that tells while every device of
//! theirs is off.
//!
//! An address is its device's own. The device it is kept for is the one
//! the transport certified, never one a request names, so no device hands
//! over an address for another. Only the person does any of it, on a
//! device of theirs: an agent, an app and everybody else are answered what
//! a caller asking for something that does not exist is answered. What the
//! person reads back names their devices and never an address or a key.
//!
//! An address reaches one person: a token handed over for a person is no
//! longer anybody else's, so an app whose person signed out and another
//! signed in tells only the one who is there. It is forgotten when its
//! device hands over another, when its person removes it, when the
//! device's key is revoked, and when the push service says it is gone.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::{ActionCallError, InvokeRequest};
use tilo_lock::MutexRecover as _;

use super::super::Workspaces;
use super::super::person_settings::{answer, argument};
use super::argument_or_default;

/// The resource of `tilo.telling` a person's addresses are actions of.
pub const RESOURCE: &str = "addresses";
/// Hand over where the asking device is reached ([`Register`]), answering
/// the [`AddressList`] as it is after.
pub const REGISTER: &str = "register";
/// Forget where one device is reached ([`Unregister`]), answering the
/// [`AddressList`] as it is after.
pub const UNREGISTER: &str = "unregister";
/// Which devices of the person are reached: an [`AddressList`].
pub const LIST: &str = "list";

/// The most addresses kept for one person: the newest are kept.
pub const MAX_ADDRESSES: usize = 8;
/// The shortest token a push service gives, in hex digits.
const MIN_TOKEN_HEX: usize = 16;
/// The longest token a push service gives, in hex digits.
const MAX_TOKEN_HEX: usize = 200;
/// The longest name of an app an address was given to, in bytes.
const MAX_TOPIC_BYTES: usize = 255;
/// How many things a push showed on one device are remembered, so they
/// can be taken back: the newest are.
const SHOWN_KEPT: usize = 64;

/// Which of a push service's environments an address belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushEnvironment {
    /// The one an app built for development is given its token in.
    Development,
    /// The one an app people installed is given its token in.
    Production,
}

/// The key the words of a push to one device are encrypted for.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushKey {
    /// The device's P-256 public key, uncompressed (65 bytes), in hex.
    pub p256dh: String,
    /// The device's secret for the agreement (16 bytes), in hex.
    pub auth: String,
}

/// Where the asking device is reached: what [`REGISTER`] takes. It names
/// no device: the address is kept for the device that hands it over.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Register {
    /// The token the push service gave the app on this device, in hex.
    pub token: String,
    /// The environment the token belongs to.
    pub environment: PushEnvironment,
    /// The app the token was given to, by its identifier.
    pub topic: String,
    /// The key the words of a push to this device are encrypted for.
    pub key: PushKey,
}

/// Which device's address to forget: what [`UNREGISTER`] takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unregister {
    /// The device, as [`Listed::device`] names it; the asking device when
    /// empty.
    #[serde(default)]
    pub device: String,
}

/// One device that is reached, as its person reads it: never its address
/// nor its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    /// The device, by the key the transport certified, in hex.
    pub device: String,
    /// The environment its address belongs to.
    pub environment: PushEnvironment,
    /// The app its address was given to.
    pub topic: String,
    /// When it handed its address over, in milliseconds since the epoch.
    pub at_ms: u64,
    /// Whether it is the device that asks.
    pub this_device: bool,
}

/// The devices of a person that are reached, the newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressList {
    /// One entry a device.
    pub addresses: Vec<Listed>,
}

/// Where one device is reached, as this server keeps it and as whatever
/// sends a push reads it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushAddress {
    /// The device, by the key the transport certified, in hex.
    pub device: String,
    /// The token the push service gave the app on it, in lowercase hex.
    pub token: String,
    /// The environment the token belongs to.
    pub environment: PushEnvironment,
    /// The app the token was given to.
    pub topic: String,
    /// The device's P-256 public key, uncompressed, in hex.
    pub p256dh: String,
    /// The device's secret for the agreement, in hex.
    pub auth: String,
    /// When the device handed it over, in milliseconds since the epoch.
    pub at_ms: u64,
    /// The things a push showed on the device and nothing took back since,
    /// by their ids, the newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown: Vec<String>,
}

impl fmt::Debug for PushAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the token nor the key: whatever prints an address prints
        // which device it is of, and nothing that reaches the device.
        formatter
            .debug_struct("PushAddress")
            .field("device", &self.device)
            .field("environment", &self.environment)
            .field("topic", &self.topic)
            .finish_non_exhaustive()
    }
}

/// What the server keeps of where a person's devices are reached.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    /// The newest first.
    #[serde(default)]
    addresses: Vec<PushAddress>,
}

/// `text` as lowercase hex of exactly `len` bytes, or none.
fn hex_of(text: &str, len: usize) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    // Two hex digits a byte, and nothing else.
    let hex =
        text.len() == len.saturating_mul(2) && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    hex.then_some(text)
}

/// The person whose store the folder `name` is, where it is a person's.
fn person_named(name: &std::ffi::OsStr) -> Option<[u8; 32]> {
    let key = hex::decode(name.to_str()?).ok()?;
    key.try_into().ok()
}

impl Register {
    /// This, as the address of `device` handed over at `now_ms`.
    fn checked(self, device: &[u8; 32], now_ms: u64) -> Result<PushAddress, String> {
        let token = self.token.trim().to_ascii_lowercase();
        // A token becomes a part of the address a push is sent to: hex
        // digits and nothing else.
        if !(MIN_TOKEN_HEX..=MAX_TOKEN_HEX).contains(&token.len())
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("An address is the push service's token for the device, in hex".to_owned());
        }
        let topic = self.topic.trim();
        let named = !topic.is_empty()
            && topic.len() <= MAX_TOPIC_BYTES
            && topic
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
        if !named {
            return Err("An address names the app it was given to".to_owned());
        }
        let p256dh = hex_of(&self.key.p256dh, 65).filter(|key| key.starts_with("04"));
        let auth = hex_of(&self.key.auth, 16);
        let (Some(p256dh), Some(auth)) = (p256dh, auth) else {
            return Err(
                "An address carries the device's P-256 key and its 16-byte secret, in hex"
                    .to_owned(),
            );
        };
        Ok(PushAddress {
            device: hex::encode(device),
            token,
            environment: self.environment,
            topic: topic.to_owned(),
            p256dh,
            auth,
            at_ms: now_ms,
            shown: Vec::new(),
        })
    }
}

fn kept_at(root: &Path) -> PathBuf {
    root.join("telling").join("addresses.json")
}

/// What is kept under `root`: nothing where nothing is. A file that does
/// not read is nothing kept, and says so in the server's log without a
/// word of what it holds: each device hands its address over again when
/// its app next starts.
fn read_kept(root: &Path) -> Kept {
    let bytes = match std::fs::read(kept_at(root)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Kept::default(),
        Err(error) => {
            tracing::warn!(%error, "telling: a person's addresses could not be read");
            return Kept::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(
            line = error.line(),
            column = error.column(),
            "telling: a person's addresses do not read"
        );
        Kept::default()
    })
}

/// Keep `kept` under `root`, for its owner alone to read.
fn write_kept(root: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&kept_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// Take the address `token` out of what is kept under `root`.
fn drop_token(root: &Path, token: &str) {
    let mut kept = read_kept(root);
    let had = kept.addresses.len();
    kept.addresses.retain(|address| address.token != token);
    if kept.addresses.len() != had
        && let Err(error) = write_kept(root, &kept)
    {
        tracing::warn!(%error, "telling: an address that moved to another person is still kept");
    }
}

impl Workspaces {
    /// Answer a request about where the devices of `person` are reached,
    /// asked by the person on `device`, a device of theirs the transport
    /// certified.
    pub(super) fn addresses_invoke(
        &self,
        person: &[u8; 32],
        device: &[u8; 32],
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match request.target.address.action.as_str() {
            REGISTER => {
                let register: Register = argument(request, "an address and its key")?;
                let kept = self
                    .register_push_address(person, device, register)
                    .map(|()| self.addresses_listed(person, device));
                answer(&kept)
            }
            UNREGISTER => {
                let unregister: Unregister = argument_or_default(request, "a device")?;
                let named = unregister.device.trim().to_ascii_lowercase();
                let gone = if named.is_empty() {
                    hex::encode(device)
                } else {
                    named
                };
                self.forget_addresses(person, |address| address.device == gone);
                answer(&Ok::<_, String>(self.addresses_listed(person, device)))
            }
            LIST => answer(&Ok::<_, String>(self.addresses_listed(person, device))),
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Keep `register` as where `device`, a device of `person`'s, is
    /// reached. It takes the place of what that device handed over before,
    /// and a token that was another person's is theirs no more. The newest
    /// [`MAX_ADDRESSES`] of a person are kept.
    ///
    /// # Errors
    /// The token, the app's name or the key is not what one is, or the
    /// address could not be kept.
    pub fn register_push_address(
        &self,
        person: &[u8; 32],
        device: &[u8; 32],
        register: Register,
    ) -> Result<(), String> {
        let mut address = register.checked(device, tilo_clock::unix_now_ms())?;
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        if let Some(before) = self.claim_token(person, &address.token) {
            drop_token(&self.person_settings_root(&before), &address.token);
        }
        let Kept { addresses } = read_kept(&root);
        // What the device handed over before, and the token wherever else
        // it was kept, give way to this.
        let (replaced, mut stay): (Vec<PushAddress>, Vec<PushAddress>) = addresses
            .into_iter()
            .partition(|known| known.device == address.device || known.token == address.token);
        // The same device handing over the same token again, as an app
        // does whenever it starts, keeps what is remembered of it.
        if let Some(known) = replaced
            .iter()
            .find(|known| known.device == address.device && known.token == address.token)
        {
            address.shown.clone_from(&known.shown);
        }
        let token = address.token.clone();
        stay.insert(0, address);
        let over = stay.split_off(MAX_ADDRESSES.min(stay.len()));
        write_kept(&root, &Kept { addresses: stay })?;
        let gone: Vec<String> = replaced
            .into_iter()
            .chain(over)
            .map(|known| known.token)
            .filter(|known| *known != token)
            .collect();
        self.unclaim_tokens(person, &gone);
        Ok(())
    }

    /// Where the devices of `person` are reached, the newest first: what
    /// whatever sends a push reads.
    #[must_use]
    pub fn push_addresses(&self, person: &[u8; 32]) -> Vec<PushAddress> {
        let _writing = self.inner.telling.writing.lock_recover();
        read_kept(&self.person_settings_root(person)).addresses
    }

    /// Which devices of `person` are reached, as the person reads it on
    /// the device `asking`.
    fn addresses_listed(&self, person: &[u8; 32], asking: &[u8; 32]) -> AddressList {
        let asking = hex::encode(asking);
        AddressList {
            addresses: self
                .push_addresses(person)
                .into_iter()
                .map(|address| Listed {
                    this_device: address.device == asking,
                    device: address.device,
                    environment: address.environment,
                    topic: address.topic,
                    at_ms: address.at_ms,
                })
                .collect(),
        }
    }

    /// Forget every address of `person`'s that `gone` holds for. Answers
    /// how many were forgotten.
    fn forget_addresses(&self, person: &[u8; 32], gone: impl Fn(&PushAddress) -> bool) -> usize {
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let Kept { addresses } = read_kept(&root);
        let (forgotten, stay): (Vec<PushAddress>, Vec<PushAddress>) =
            addresses.into_iter().partition(gone);
        if forgotten.is_empty() {
            return 0;
        }
        if let Err(error) = write_kept(&root, &Kept { addresses: stay }) {
            tracing::warn!(%error, "telling: a forgotten address is still kept");
            return 0;
        }
        let tokens: Vec<String> = forgotten.into_iter().map(|address| address.token).collect();
        self.unclaim_tokens(person, &tokens);
        tokens.len()
    }

    /// The push service said the address `token` of `person`'s is gone,
    /// since `gone_at_ms` where it names the moment: it is forgotten,
    /// unless its device handed it over after that moment.
    pub fn forget_push_address(&self, person: &[u8; 32], token: &str, gone_at_ms: Option<u64>) {
        self.forget_addresses(person, |address| {
            address.token == token && gone_at_ms.is_none_or(|at| address.at_ms <= at)
        });
    }

    /// The key of `device`, a device of `person`'s, was revoked: where it
    /// was reached is forgotten with it.
    pub(in crate::workspaces) fn forget_push_address_of(
        &self,
        person: &[u8; 32],
        device: &[u8; 32],
    ) {
        let device = hex::encode(device);
        self.forget_addresses(person, |address| address.device == device);
    }

    /// A push showed the thing `id` on the devices of `person`'s that are
    /// reached at `tokens`: remembered, so it can be taken back from them.
    pub fn push_shown(&self, person: &[u8; 32], tokens: &[String], id: &str) {
        if tokens.is_empty() {
            return;
        }
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let mut kept = read_kept(&root);
        let mut marked = false;
        for address in &mut kept.addresses {
            if !tokens.contains(&address.token) {
                continue;
            }
            address.shown.retain(|shown| shown != id);
            address.shown.push(id.to_owned());
            let over = address.shown.len().saturating_sub(SHOWN_KEPT);
            address.shown.drain(..over);
            marked = true;
        }
        if marked && let Err(error) = write_kept(&root, &kept) {
            tracing::warn!(%error, "telling: what a push showed is not written down");
        }
    }

    /// The addresses of `person`'s on whose devices a push showed one of
    /// the things `ids`, each with those of the ids it showed, which are
    /// remembered no more: what is taken back is taken back once.
    #[must_use]
    pub fn push_taken_back(
        &self,
        person: &[u8; 32],
        ids: &[String],
    ) -> Vec<(PushAddress, Vec<String>)> {
        if ids.is_empty() {
            return Vec::new();
        }
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let mut kept = read_kept(&root);
        let mut taken = Vec::new();
        for address in &mut kept.addresses {
            let (gone, stay): (Vec<String>, Vec<String>) = std::mem::take(&mut address.shown)
                .into_iter()
                .partition(|shown| ids.contains(shown));
            address.shown = stay;
            if !gone.is_empty() {
                taken.push((address.clone(), gone));
            }
        }
        if !taken.is_empty()
            && let Err(error) = write_kept(&root, &kept)
        {
            tracing::warn!(%error, "telling: what a push showed is still remembered");
        }
        taken
    }

    /// Whose each address this server keeps is, by its token, read from
    /// the stores of its people.
    fn tokens_kept(&self) -> HashMap<String, [u8; 32]> {
        let mut whose = HashMap::new();
        let Ok(people) = std::fs::read_dir(self.inner.dir.join("people")) else {
            return whose;
        };
        for entry in people.filter_map(Result::ok) {
            let root = entry.path();
            if !kept_at(&root).is_file() {
                continue;
            }
            let Some(person) = person_named(&entry.file_name()) else {
                continue;
            };
            for address in read_kept(&root).addresses {
                whose.insert(address.token, person);
            }
        }
        whose
    }

    /// Note that the address `token` is `person`'s from now on, and answer
    /// whose it was when it was somebody else's.
    fn claim_token(&self, person: &[u8; 32], token: &str) -> Option<[u8; 32]> {
        let before = self
            .inner
            .telling
            .reached
            .lock_recover()
            .get_or_insert_with(|| self.tokens_kept())
            .insert(token.to_owned(), *person);
        before.filter(|before| before != person)
    }

    /// Note that the addresses `tokens` are `person`'s no more.
    fn unclaim_tokens(&self, person: &[u8; 32], tokens: &[String]) {
        self.inner
            .telling
            .reached
            .lock_recover()
            .iter_mut()
            .for_each(|whose| {
                whose.retain(|token, theirs| theirs != person || !tokens.contains(token));
            });
    }
}
