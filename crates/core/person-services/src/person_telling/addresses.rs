//! Where a person's devices are reached while their Tilo is closed.
//!
//! An app that is closed hears of nothing unless the push service of its
//! system carries it there. A device of a person's hands this server what
//! such a push needs: where the push goes — the token that service gave the
//! app on that device, with which of the service's environments and which
//! app it belongs to, or the web push endpoint the device subscribed with —
//! and a key of the device's own that the words of a push are encrypted
//! for, so that nobody between reads them. The server keeps a few per
//! person in the person's own store, beside their rule and their log,
//! because it is the server that tells while every device of theirs is off.
//!
//! An address is its device's own. The device it is kept for is the one
//! the transport certified, never one a request names, so no device hands
//! over an address for another. Only the person does any of it, on a
//! device of theirs: an agent, an app and everybody else are answered what
//! a caller asking for something that does not exist is answered. What the
//! person reads back names their devices and never an address or a key.
//!
//! An address reaches one person: one handed over for a person is no
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

use crate::person_settings::{answer, argument};
use crate::person_telling::argument_or_default;

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
/// The longest web push endpoint taken, in bytes: the sender's own limit.
pub const MAX_ENDPOINT_BYTES: usize = 1024;
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

/// How a device is reached while its app is closed: the push service of
/// its system, by the token that service gave the app there, or a web push
/// endpoint the device itself subscribed with (RFC 8030), as a phone's
/// `UnifiedPush` distributor gives one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushKind {
    /// Apple's push service, by the token it gave the app.
    #[default]
    Apns,
    /// A web push service, by the endpoint the device subscribed with.
    WebPush,
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveAddress {
    pub start: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub update: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub turn: String,
}

/// Where the asking device is reached: what [`REGISTER`] takes. It names
/// no device: the address is kept for the device that hands it over.
///
/// One action carries both kinds a push is sent by: a token of the app's
/// push service, or the web push endpoint the device subscribed with. The
/// kind is the address's own, and what it names is exactly that kind's.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Register {
    /// The token Apple's push service gave the app on this device.
    Apns {
        /// The token the push service gave the app on this device, in hex.
        token: String,
        /// The environment the token belongs to.
        environment: PushEnvironment,
        /// The app the token was given to, by its identifier.
        topic: String,
        /// The key the words of a push to this device are encrypted for.
        key: PushKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live: Option<LiveAddress>,
    },
    /// The web push endpoint the device subscribed with.
    WebPush {
        /// The push service's `https` address for this device.
        endpoint: String,
        /// The key the words of a push to this device are encrypted for.
        key: PushKey,
    },
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
    /// How it is reached: Apple's push service, or a web push endpoint it
    /// subscribed with.
    pub kind: PushKind,
    /// The environment its address belongs to, for Apple's push service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<PushEnvironment>,
    /// The app its address was given to, for Apple's push service.
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
    /// How it is reached.
    #[serde(default)]
    pub kind: PushKind,
    /// The token the push service gave the app on it, in lowercase hex: for
    /// Apple's push service alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    /// The web push endpoint the device subscribed with: for a web push
    /// address alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    /// The environment the token belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<PushEnvironment>,
    /// The app the token was given to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Where the app's live activities are reached, where it shows any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveAddress>,
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

impl PushAddress {
    /// What reaches this device: the token or the endpoint its push is
    /// sent to, as the sender reads it and as a push service names it when
    /// it is gone.
    #[must_use]
    pub fn reached_as(&self) -> &str {
        match self.kind {
            PushKind::Apns => &self.token,
            PushKind::WebPush => &self.endpoint,
        }
    }

    /// The key the words of a push to this device are encrypted for.
    ///
    /// # Errors
    /// The stored key or secret is not one.
    pub fn subscription_key(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let p256dh = hex::decode(&self.p256dh).map_err(|error| error.to_string())?;
        let auth = hex::decode(&self.auth).map_err(|error| error.to_string())?;
        Ok((p256dh, auth))
    }
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
        let (kind, token, endpoint, environment, topic, key, live) = match self {
            Self::Apns {
                token,
                environment,
                topic,
                key,
                live,
            } => {
                let token = token.trim().to_ascii_lowercase();
                // A token becomes a part of the address a push is sent to: hex
                // digits and nothing else.
                if !(MIN_TOKEN_HEX..=MAX_TOKEN_HEX).contains(&token.len())
                    || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(
                        "An address is the push service's token for the device, in hex".to_owned(),
                    );
                }
                let live = live.map(LiveAddress::checked).transpose()?;
                let topic = topic.trim();
                let named = !topic.is_empty()
                    && topic.len() <= MAX_TOPIC_BYTES
                    && topic
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
                if !named {
                    return Err("An address names the app it was given to".to_owned());
                }
                (
                    PushKind::Apns,
                    token,
                    String::new(),
                    Some(environment),
                    topic.to_owned(),
                    key,
                    live,
                )
            }
            Self::WebPush { endpoint, key } => {
                let endpoint = endpoint.trim();
                // The endpoint is posted to as given: an `https` address of
                // printable characters and nothing else, nothing after it.
                let address = endpoint.strip_prefix("https://").unwrap_or_default();
                let named = !address.is_empty()
                    && endpoint.len() <= MAX_ENDPOINT_BYTES
                    && endpoint
                        .bytes()
                        .all(|byte| matches!(byte, 0x21..=0x7e) && byte != b'#');
                if !named {
                    return Err(
                        "An address is the https endpoint the device subscribed with".to_owned(),
                    );
                }
                (
                    PushKind::WebPush,
                    String::new(),
                    endpoint.to_owned(),
                    None,
                    String::new(),
                    key,
                    None,
                )
            }
        };
        let p256dh = hex_of(&key.p256dh, 65).filter(|key| key.starts_with("04"));
        let auth = hex_of(&key.auth, 16);
        let (Some(p256dh), Some(auth)) = (p256dh, auth) else {
            return Err(
                "An address carries the device's P-256 key and its 16-byte secret, in hex"
                    .to_owned(),
            );
        };
        Ok(PushAddress {
            device: hex::encode(device),
            kind,
            token,
            endpoint,
            environment,
            topic,
            live,
            p256dh,
            auth,
            at_ms: now_ms,
            shown: Vec::new(),
        })
    }
}

impl LiveAddress {
    fn checked(self) -> Result<Self, String> {
        let hex = |text: &str| {
            let text = text.trim().to_ascii_lowercase();
            (MIN_TOKEN_HEX..=MAX_TOKEN_HEX)
                .contains(&text.len())
                .then_some(text)
                .filter(|text| text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        };
        let Some(start) = hex(&self.start) else {
            return Err(
                "A live activity starts from the push-to-start token Apple's service gave the app, \
                 in hex"
                    .to_owned(),
            );
        };
        let update = self.update.trim().to_ascii_lowercase();
        if !update.is_empty() && hex(&update).is_none() {
            return Err("A live activity is updated by the token in hex it runs under".to_owned());
        }
        let turn = self.turn.trim();
        let named = turn.len() <= MAX_TOPIC_BYTES
            && turn
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        Ok(Self {
            start,
            update,
            turn: if named {
                turn.to_owned()
            } else {
                String::new()
            },
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

/// Take the address reached at `reach` out of what is kept under `root`.
fn drop_reach(root: &Path, reach: &str) {
    let mut kept = read_kept(root);
    let had = kept.addresses.len();
    kept.addresses
        .retain(|address| address.reached_as() != reach);
    if kept.addresses.len() != had
        && let Err(error) = write_kept(root, &kept)
    {
        tracing::warn!(%error, "telling: an address that moved to another person is still kept");
    }
}

impl crate::PersonServices {
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
    /// and an address that was another person's is theirs no more. The
    /// newest [`MAX_ADDRESSES`] of a person are kept.
    ///
    /// # Errors
    /// The address, the app's name or the key is not what one is, or the
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
        let reach = address.reached_as().to_owned();
        if let Some(before) = self.claim_reach(person, &reach) {
            drop_reach(&self.person_settings_root(&before), &reach);
        }
        let Kept { addresses } = read_kept(&root);
        // What the device handed over before, and the address wherever else
        // it was kept, give way to this.
        let (replaced, mut stay): (Vec<PushAddress>, Vec<PushAddress>) = addresses
            .into_iter()
            .partition(|known| known.device == address.device || known.reached_as() == reach);
        // The same device handing over the same address again, as an app
        // does whenever it starts, keeps what is remembered of it.
        if let Some(known) = replaced
            .iter()
            .find(|known| known.device == address.device && known.reached_as() == reach)
        {
            address.shown.clone_from(&known.shown);
            // An app that hands over its address without its live activities this
            // time keeps the ones this device handed over before.
            if address.live.is_none() {
                address.live.clone_from(&known.live);
            }
        }
        stay.insert(0, address);
        let over = stay.split_off(MAX_ADDRESSES.min(stay.len()));
        write_kept(&root, &Kept { addresses: stay })?;
        let gone: Vec<String> = replaced
            .into_iter()
            .chain(over)
            .map(|known| known.reached_as().to_owned())
            .filter(|known| *known != reach)
            .collect();
        self.unclaim_reaches(person, &gone);
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
                    kind: address.kind,
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
        let gone: Vec<String> = forgotten
            .into_iter()
            .map(|address| address.reached_as().to_owned())
            .collect();
        self.unclaim_reaches(person, &gone);
        gone.len()
    }

    /// The push service said the address `reach` of `person`'s is gone
    /// (its token, or its endpoint), since `gone_at_ms` where it names the
    /// moment: it is forgotten, unless its device handed it over after that
    /// moment.
    pub fn forget_push_address(&self, person: &[u8; 32], reach: &str, gone_at_ms: Option<u64>) {
        self.forget_addresses(person, |address| {
            address.reached_as() == reach && gone_at_ms.is_none_or(|at| address.at_ms <= at)
        });
    }

    /// The key of `device`, a device of `person`'s, was revoked: where it
    /// was reached is forgotten with it.
    pub(crate) fn forget_push_address_of(&self, person: &[u8; 32], device: &[u8; 32]) {
        let device = hex::encode(device);
        self.forget_addresses(person, |address| address.device == device);
    }

    /// A push showed the thing `id` on the devices of `person`'s that are
    /// reached at `reaches` (their tokens or endpoints): remembered, so it
    /// can be taken back from them.
    pub fn push_shown(&self, person: &[u8; 32], reaches: &[String], id: &str) {
        if reaches.is_empty() {
            return;
        }
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let mut kept = read_kept(&root);
        let mut marked = false;
        for address in &mut kept.addresses {
            if !reaches.iter().any(|reach| reach == address.reached_as()) {
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

    /// The addresses of `person`'s of the kind `kind` on whose devices a
    /// push showed one of the things `ids`, each with those of the ids it
    /// showed, which are remembered no more: what is taken back is taken
    /// back once, and only by the sender that showed it.
    #[must_use]
    pub fn push_taken_back(
        &self,
        person: &[u8; 32],
        ids: &[String],
        kind: PushKind,
    ) -> Vec<(PushAddress, Vec<String>)> {
        if ids.is_empty() {
            return Vec::new();
        }
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let mut kept = read_kept(&root);
        let mut taken = Vec::new();
        for address in &mut kept.addresses {
            if address.kind != kind {
                continue;
            }
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

    /// Whose each address this server keeps is, by what reaches it, read
    /// from the stores of its people.
    fn reaches_kept(&self) -> HashMap<String, [u8; 32]> {
        let mut whose = HashMap::new();
        let Ok(people) = std::fs::read_dir(self.dir().join("people")) else {
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
                whose.insert(address.reached_as().to_owned(), person);
            }
        }
        whose
    }

    /// Note that the address reached at `reach` is `person`'s from now on,
    /// and answer whose it was when it was somebody else's.
    fn claim_reach(&self, person: &[u8; 32], reach: &str) -> Option<[u8; 32]> {
        let before = self
            .inner
            .telling
            .reached
            .lock_recover()
            .get_or_insert_with(|| self.reaches_kept())
            .insert(reach.to_owned(), *person);
        before.filter(|before| before != person)
    }

    /// Note that the addresses reached at `reaches` are `person`'s no more.
    fn unclaim_reaches(&self, person: &[u8; 32], reaches: &[String]) {
        self.inner
            .telling
            .reached
            .lock_recover()
            .iter_mut()
            .for_each(|whose| {
                whose.retain(|reach, theirs| theirs != person || !reaches.contains(reach));
            });
    }
}
