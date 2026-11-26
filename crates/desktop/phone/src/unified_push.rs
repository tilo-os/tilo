//! Where a phone that is closed is reached, and what it reads of a push.
//!
//! A phone whose app is closed hears of nothing unless a push service carries it there. Tilo's
//! phone takes its pushes from the `UnifiedPush` distributor the person installed: the distributor
//! hands it an `https` endpoint that speaks web push (RFC 8030), and the host of the person's
//! workspace pushes to it with the sender it pushes to browsers with. Only the distributor and
//! the push service see the endpoint and an encrypted body.
//!
//! The device hands the host that endpoint with a P-256 key of its own and a 16-byte auth secret
//! (`tilo.telling` `addresses#register`), and every push is encrypted for that key (RFC 8291,
//! `aes128gcm` of RFC 8188), so neither the distributor nor the push service reads a word. The
//! key and the secret are kept in the app's own files, and are the device's own: they are what
//! the notification a thing is shown under is named by on this device alone
//! ([`tilo_actions::remote::person_telling::known_as`]), so one thing has another name on each
//! device and nothing ties one person's pushes together.
//!
//! What a push says is the notification itself ([`Push`]): a thing waits, how it is called and
//! what it says, and the buttons it offers by label. It says nothing of where the thing is, nor
//! its id, nor a workspace: the app reads those from the person's own log once it runs.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use p256::elliptic_curve::sec1::ToSec1Point as _;
use serde::{Deserialize, Serialize};

/// The file the device's push key and auth secret are kept in, under its data.
const KEY_FILE: &str = "tilo/push.json";
/// The most a push's plain text may be: one record of the sender's.
const MAX_PLAIN: usize = 4_000;

/// This device's push key, its auth secret, and the endpoint it handed the host last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    /// The P-256 secret scalar (32 bytes), in hex. Never leaves the device.
    secret: String,
    /// The P-256 public key, uncompressed (65 bytes), in hex.
    public: String,
    /// The 16-byte auth secret of RFC 8291, in hex.
    auth: String,
    /// The endpoint this key was handed over with, when one was.
    #[serde(default)]
    pub endpoint: String,
}

impl Key {
    /// The key kept under `data`, made the first time: it is the device's own, and it must stand
    /// while the host keeps an address for it.
    ///
    /// # Errors
    /// No key could be made, or the one kept does not read.
    pub fn load_or_create(data: &Path) -> Result<Self, String> {
        let path = data.join(KEY_FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            return serde_json::from_slice(&bytes)
                .map_err(|error| format!("{} does not read: {error}", path.display()));
        }
        let mut auth = [0u8; 16];
        fill(&mut auth)?;
        // A scalar of the curve's order: a draw past it is drawn again.
        let (secret, key) = loop {
            let mut secret = [0u8; 32];
            fill(&mut secret)?;
            if let Ok(key) = p256::SecretKey::from_slice(&secret) {
                break (secret, key);
            }
        };
        let public = key.public_key().to_sec1_point(false);
        let key = Self {
            secret: hex::encode(secret),
            public: hex::encode(public.as_bytes()),
            auth: hex::encode(auth),
            endpoint: String::new(),
        };
        key.save(data)?;
        Ok(key)
    }

    /// Keep this key under `data`, for its owner alone to read.
    ///
    /// # Errors
    /// It could not be written.
    pub fn save(&self, data: &Path) -> Result<(), String> {
        let path = data.join(KEY_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(&path)
            .and_then(|mut file| file.write_all(&bytes))
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    /// This key's auth secret, as the bytes a push is keyed with and a notification is named by.
    ///
    /// # Errors
    /// The secret kept is not one.
    pub fn auth_bytes(&self) -> Result<Vec<u8>, String> {
        let auth = hex::decode(&self.auth).map_err(|error| error.to_string())?;
        if auth.len() != 16 {
            return Err("the push auth secret is 16 bytes".to_owned());
        }
        Ok(auth)
    }

    /// The key its pushes are encrypted for: the P-256 public key, uncompressed, and the auth
    /// secret, as the host keeps them (both in hex).
    #[must_use]
    pub fn handed_over(&self) -> (String, String) {
        (self.public.clone(), self.auth.clone())
    }

    /// Open `body`, a push encrypted for this key as one `aes128gcm` record (RFC 8291 §3.4,
    /// RFC 8188 §2).
    ///
    /// # Errors
    /// The body is not one push encrypted for this key.
    pub fn open(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        use ring::{aead, hkdf};
        let wrong = || "this is not a push encrypted for this device".to_owned();
        if body.len() < 16 + 4 + 1 + 65 + 16 {
            return Err(wrong());
        }
        let salt = body.get(..16).ok_or_else(wrong)?;
        let id_len = usize::from(*body.get(20).ok_or_else(wrong)?);
        let sender = body.get(21..21 + id_len).ok_or_else(wrong)?;
        let record = body.get(21 + id_len..).ok_or_else(wrong)?;
        let secret = hex::decode(&self.secret).map_err(|_| wrong())?;
        let secret = p256::SecretKey::from_slice(&secret).map_err(|_| wrong())?;
        let sender = p256::PublicKey::from_sec1_bytes(sender).map_err(|_| wrong())?;
        let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), sender.as_affine());
        let own = hex::decode(&self.public).map_err(|_| wrong())?;
        let own = p256::PublicKey::from_sec1_bytes(&own).map_err(|_| wrong())?;
        // IKM = HKDF(auth, ecdh, "WebPush: info" || 0 || ua_public || as_public, 32)
        let own = own.to_sec1_point(false);
        let sender = sender.to_sec1_point(false);
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(own.as_bytes());
        info.extend_from_slice(sender.as_bytes());
        let auth = self.auth_bytes().map_err(|_| wrong())?;
        let shared = shared.raw_secret_bytes();
        let ikm = expand(
            &hkdf::Salt::new(hkdf::HKDF_SHA256, &auth).extract(&shared[..]),
            &info,
            32,
        )
        .map_err(|()| wrong())?;
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
        let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16).map_err(|()| wrong())?;
        let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12).map_err(|()| wrong())?;
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|_| wrong())?,
        );
        let mut record = record.to_vec();
        let plain = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| wrong())?,
                aead::Aad::empty(),
                &mut record,
            )
            .map_err(|_| wrong())?;
        // One record, the last: the content, then the delimiter 2.
        let plain = plain
            .split_last()
            .filter(|(last, _)| **last == 2)
            .map(|(_, plain)| plain.to_vec())
            .ok_or_else(wrong)?;
        Ok(plain)
    }
}

/// What a push says, as the app shows it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    /// What it is: a thing waits ([`TOLD`]).
    #[serde(default)]
    pub kind: String,
    /// The name this device knows the thing by.
    pub tag: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Whether it may break do-not-disturb: its person let its kind through.
    #[serde(default)]
    pub important: bool,
    /// Whether the thing waits no more, and what showed it is taken back.
    #[serde(default)]
    pub gone: bool,
}

/// What a push of a thing told says it is.
pub const TOLD: &str = "told";

impl Push {
    /// The push `plain` says.
    ///
    /// # Errors
    /// It is not a push, or it is one this device does not read.
    pub fn read(plain: &[u8]) -> Result<Self, String> {
        if plain.len() > MAX_PLAIN {
            return Err("a push is longer than a push".to_owned());
        }
        let push: Self = serde_json::from_slice(plain)
            .map_err(|error| format!("a push does not read: {error}"))?;
        if push.kind != TOLD {
            return Err(format!(
                "a push of kind {:?} is not one this device reads",
                push.kind
            ));
        }
        Ok(push)
    }
}

/// The file a `UnifiedPush` distributor leaves the endpoint it gave this app in, when one is not
/// named: the app's own files, a directory beside its data, written by the receiver
/// (`crates/ui/ui-core/android/java/so/tilo/android/TiloPush.java`).
#[must_use]
pub fn endpoint_file() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("TILO_PUSH_ENDPOINT_FILE") {
        return Some(PathBuf::from(named));
    }
    #[cfg(target_os = "android")]
    {
        tilo_paths::data_dir()?
            .parent()
            .map(|files| files.join("tilo-push").join("endpoint"))
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// The file a tap on one of this app's notifications leaves the link it opened in.
#[must_use]
pub fn tap_file() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("TILO_PUSH_TAP_FILE") {
        return Some(PathBuf::from(named));
    }
    #[cfg(target_os = "android")]
    {
        tilo_paths::data_dir()?
            .parent()
            .map(|files| files.join("tilo-push").join("tapped"))
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// Fill `bytes` with randomness.
fn fill(bytes: &mut [u8]) -> Result<(), String> {
    use ring::rand::SecureRandom as _;
    ring::rand::SystemRandom::new()
        .fill(bytes)
        .map_err(|_| "no randomness for the push key".to_owned())
}

fn expand(prk: &ring::hkdf::Prk, info: &[u8], len: usize) -> Result<Vec<u8>, ()> {
    struct Len(usize);
    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let infos = [info];
    let okm = prk.expand(&infos, Len(len)).map_err(drop)?;
    let mut out = vec![0u8; len];
    okm.fill(&mut out).map_err(drop)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone's key opens what the sender encrypted for it, and nothing else does.
    #[test]
    fn a_phone_opens_a_push_encrypted_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let key = Key::load_or_create(dir.path()).unwrap();
        let again = Key::load_or_create(dir.path()).unwrap();
        assert_eq!(key, again, "the key stands while an address is kept for it");
        let p256dh = hex::decode(&key.public).unwrap();
        let auth = key.auth_bytes().unwrap();
        let plain =
            br#"{"kind":"told","tag":"abc","title":"An agent needs you","body":"Open Tilo."}"#;
        let body = tilo_server::web_push::encrypt_for_device(&p256dh, &auth, plain);
        assert_eq!(key.open(&body).unwrap(), plain);
        let push = Push::read(&key.open(&body).unwrap()).unwrap();
        assert_eq!(push.title, "An agent needs you");
        assert_eq!(push.body, "Open Tilo.");
        assert!(!push.gone);
        // A push that says what this device does not read is refused.
        let other = tilo_server::web_push::encrypt_for_device(
            &p256dh,
            &auth,
            br#"{"kind":"other","tag":"x"}"#,
        );
        assert!(Push::read(&key.open(&other).unwrap()).is_err());
        // A body that was not encrypted for this key opens nothing.
        let other_key = Key::load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        assert!(other_key.open(&body).is_err());
    }
}
