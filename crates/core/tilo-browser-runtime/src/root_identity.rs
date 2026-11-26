//! The page's user root: the person's Tilo identity with no other device.
//!
//! The root is an Ed25519 key. The page keeps it as the identity store's
//! record (`tilo_identity::store`'s `IdentityRecord`, its 32-byte seed in
//! `secret_bytes`), which `web/device-key.mjs` writes from the seed this module
//! hands out and opens the root back from: a computer reads the same record as
//! its own identity, so a root the tab founds and a root a computer imports are
//! one record. What opens it elsewhere is the person's root envelope (a JSON
//! document anyone may keep); its format and rules are
//! [`tilo_user_identity::root_envelope`]'s, and the statements its root signs
//! are [`tilo_user_identity::root_statement`]'s. This module makes the
//! `WebCrypto` calls those rules need, so the page seals and opens exactly what
//! a native device does. The ceremonies that mint and use passkeys are the
//! page's (`web/identity.mjs`), which hands the PRF output over.
//!
//! Opening imports the seed into a non-extractable `WebCrypto` key that lives
//! in this page's memory only; [`Root::lock`] drops it. Requires `WebCrypto`
//! Ed25519.

use std::cell::Cell;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use tilo_user_identity::recovery_words;
use tilo_user_identity::root_envelope::{
    RECOVERY_KEY_LEN, RecoveryKey, RootEnvelope, RootEnvelopeError, RootWrap, SALT_LEN, WrapKind,
    wrap_aad, wrap_key_info,
};
use tilo_user_identity::root_statement::{self, Kind};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// The AES-GCM nonce length (`tilo_seal::web::NONCE_LEN`).
const NONCE_LEN: usize = 12;
/// What a root signs to prove it is the envelope's root; the key never
/// leaves this module, and a signature over fixed bytes needs no message.
const PROBE: &[u8] = b"tilo root envelope probe";

/// An identity error as the page reads one: a JavaScript `Error` named
/// `IdentityError` with the page's `code` (`rejected`, `no-wrap`, `locked`,
/// ...), so every caller handles it the same way.
fn error_with(code: &str, message: &str) -> JsValue {
    let error = js_sys::Error::new(message);
    let _ = Reflect::set(&error, &"code".into(), &JsValue::from_str(code));
    let _ = Reflect::set(&error, &"name".into(), &JsValue::from_str("IdentityError"));
    error.into()
}

fn envelope_error(error: &RootEnvelopeError) -> JsValue {
    match error {
        RootEnvelopeError::Version(version) => error_with(
            "unsupported",
            &format!("Unsupported root envelope version {version}"),
        ),
        RootEnvelopeError::Malformed(what) => error_with("malformed", what),
        RootEnvelopeError::NoWrap => {
            error_with("no-wrap", "No passkey of this identity opens it here")
        }
        RootEnvelopeError::Rejected => {
            error_with("rejected", "This key does not open the identity")
        }
        RootEnvelopeError::WrongRoot => {
            error_with("wrong-root", "The envelope holds another key than its root")
        }
        RootEnvelopeError::Seal => error_with("seal", "sealing the root failed"),
    }
}

fn unavailable() -> JsValue {
    error_with("crypto-unavailable", "WebCrypto is required")
}

fn property(target: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    Reflect::get(target, &JsValue::from_str(name)).map_err(|_| unavailable())
}

fn object(entries: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    let built = Object::new();
    for (name, value) in entries {
        Reflect::set(&built, &JsValue::from_str(name), value).map_err(|_| unavailable())?;
    }
    Ok(built.into())
}

fn names(usages: &[&str]) -> JsValue {
    let list = Array::new();
    for usage in usages {
        list.push(&JsValue::from_str(usage));
    }
    list.into()
}

/// `bytes` as a `Uint8Array`.
fn view(bytes: &[u8]) -> JsValue {
    Uint8Array::from(bytes).into()
}

/// The bytes of a `Uint8Array` (what `sign` answers) or an `ArrayBuffer`
/// (what `wrapKey` and `exportKey` do).
fn bytes(value: &JsValue) -> Vec<u8> {
    Uint8Array::new(value).to_vec()
}

fn global(name: &str) -> Result<JsValue, JsValue> {
    Reflect::get(&js_sys::global(), &JsValue::from_str(name)).map_err(|_| unavailable())
}

fn crypto() -> Result<JsValue, JsValue> {
    let crypto = global("crypto")?;
    if crypto.is_undefined() || crypto.is_null() {
        return Err(unavailable());
    }
    Ok(crypto)
}

fn subtle() -> Result<JsValue, JsValue> {
    let subtle = property(&crypto()?, "subtle")?;
    if subtle.is_undefined() || subtle.is_null() {
        return Err(unavailable());
    }
    Ok(subtle)
}

fn algorithm(name: &str) -> Result<JsValue, JsValue> {
    object(&[("name", JsValue::from_str(name))])
}

/// `target[name](args...)`, without waiting for its answer.
fn invoke(target: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = property(target, name)?
        .dyn_into()
        .map_err(|_| error_with("crypto-unavailable", name))?;
    let list = Array::new();
    for argument in args {
        list.push(argument);
    }
    function.apply(target, &list)
}

/// `target[name](args...)`, its promise awaited.
async fn awaited(target: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let promise: Promise = invoke(target, name, args)?
        .dyn_into()
        .map_err(|_| error_with("crypto-unavailable", name))?;
    JsFuture::from(promise).await
}

fn random(length: usize) -> Result<Vec<u8>, JsValue> {
    let drawn = Uint8Array::new_with_length(u32::try_from(length).map_err(|_| unavailable())?);
    invoke(&crypto()?, "getRandomValues", &[drawn.clone().into()])?;
    Ok(drawn.to_vec())
}

/// The PKCS#8 v2 encoding of an Ed25519 private key from its 32-byte seed:
/// `WebCrypto` imports a private Ed25519 key only as PKCS#8, and this is the
/// fixed prefix a seed follows.
fn pkcs8_from_seed(seed: &[u8; 32]) -> Vec<u8> {
    const PREFIX: [u8; 16] = [
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(seed);
    der
}

/// The 32-byte seed a PKCS#8 Ed25519 private key ends with.
fn seed_of_pkcs8(der: &[u8]) -> Result<[u8; 32], JsValue> {
    der.get(der.len().saturating_sub(32)..)
        .and_then(|seed| seed.try_into().ok())
        .ok_or_else(|| error_with("malformed", "the root's key does not read"))
}

/// The Ed25519 signing key `der` (`PKCS#8`) holds, extractable only to seal it
/// anew for a wrap.
async fn import_pkcs8(der: &[u8], extractable: bool) -> Result<JsValue, JsValue> {
    awaited(
        &subtle()?,
        "importKey",
        &[
            JsValue::from_str("pkcs8"),
            view(der),
            algorithm("Ed25519")?,
            if extractable {
                JsValue::TRUE
            } else {
                JsValue::FALSE
            },
            names(&["sign"]),
        ],
    )
    .await
    .map_err(|_| error_with("rejected", "This browser cannot open the identity it keeps"))
}

/// The key a wrap seals the root under: `HKDF-SHA256(ikm, salt, info = the
/// wrap's label)`, as `tilo_seal::web` derives it natively.
async fn wrapping_key(ikm: &[u8], salt: &[u8]) -> Result<JsValue, JsValue> {
    let subtle = subtle()?;
    let material = awaited(
        &subtle,
        "importKey",
        &[
            JsValue::from_str("raw"),
            view(ikm),
            JsValue::from_str("HKDF"),
            JsValue::FALSE,
            names(&["deriveKey"]),
        ],
    )
    .await
    .map_err(|_| error_with("rejected", "This key does not open the identity"))?;
    let derivation = object(&[
        ("name", JsValue::from_str("HKDF")),
        ("hash", JsValue::from_str("SHA-256")),
        ("salt", view(salt)),
        ("info", view(wrap_key_info().as_bytes())),
    ])?;
    let sealed = object(&[
        ("name", JsValue::from_str("AES-GCM")),
        ("length", JsValue::from_f64(256.0)),
    ])?;
    awaited(
        &subtle,
        "deriveKey",
        &[
            derivation,
            material,
            sealed,
            JsValue::FALSE,
            names(&["wrapKey", "unwrapKey"]),
        ],
    )
    .await
    .map_err(|_| error_with("rejected", "This key does not open the identity"))
}

/// The root `private_key` wrapped under `ikm` for `associated`: the nonce
/// and ciphertext a wrap keeps.
async fn seal(
    private_key: &JsValue,
    ikm: &[u8],
    salt: &[u8],
    aad: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), JsValue> {
    let key = wrapping_key(ikm, salt).await?;
    let nonce = random(NONCE_LEN)?;
    let wrap_algorithm = object(&[
        ("name", JsValue::from_str("AES-GCM")),
        ("iv", view(&nonce)),
        ("additionalData", view(aad)),
    ])?;
    let wrapped = awaited(
        &subtle()?,
        "wrapKey",
        &[
            JsValue::from_str("pkcs8"),
            private_key.clone(),
            key,
            wrap_algorithm,
        ],
    )
    .await
    .map_err(|_| error_with("seal", "sealing the root failed"))?;
    Ok((nonce, bytes(&wrapped)))
}

/// The root a wrap holds, unwrapped under `ikm` for `aad`. `extractable`
/// only while the root is sealed anew for a wrap; it signs otherwise.
async fn unseal(
    ikm: &[u8],
    salt: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
    extractable: bool,
) -> Result<JsValue, JsValue> {
    let key = wrapping_key(ikm, salt).await?;
    let unwrap_algorithm = object(&[
        ("name", JsValue::from_str("AES-GCM")),
        ("iv", view(nonce)),
        ("additionalData", view(aad)),
    ])?;
    awaited(
        &subtle()?,
        "unwrapKey",
        &[
            JsValue::from_str("pkcs8"),
            view(ciphertext),
            key,
            unwrap_algorithm,
            algorithm("Ed25519")?,
            if extractable {
                JsValue::TRUE
            } else {
                JsValue::FALSE
            },
            names(&["sign"]),
        ],
    )
    .await
    .map_err(|_| error_with("rejected", "This key does not open the identity"))
}

async fn sign_with(private_key: &JsValue, message: &[u8]) -> Result<Vec<u8>, JsValue> {
    let signature = awaited(
        &subtle()?,
        "sign",
        &[algorithm("Ed25519")?, private_key.clone(), view(message)],
    )
    .await
    .map_err(|_| error_with("refused", "the root refused to sign"))?;
    Ok(bytes(&signature))
}

/// Whether `private_key` signs as `public_key`, so a wrap cannot open to
/// another root than the envelope names.
async fn proves_root(private_key: &JsValue, public_key: &[u8; 32]) -> Result<(), JsValue> {
    let signature: [u8; 64] = sign_with(private_key, PROBE)
        .await?
        .try_into()
        .map_err(|_| error_with("rejected", "This key does not open the identity"))?;
    VerifyingKey::from_bytes(public_key)
        .ok()
        .and_then(|key| {
            key.verify_strict(PROBE, &Signature::from_bytes(&signature))
                .ok()
        })
        .ok_or_else(|| error_with("wrong-root", "The envelope holds another key than its root"))
}

fn parse(json: &str) -> Result<RootEnvelope, JsValue> {
    serde_json::from_str(json).map_err(|_| error_with("malformed", "not a root envelope"))
}

fn public_key_of(envelope: &RootEnvelope) -> Result<[u8; 32], JsValue> {
    envelope
        .public_key()
        .map_err(|error| envelope_error(&error))
}

/// The associated data `wrap` authenticates: its kind, the root public key
/// and, for a passkey, the credential.
fn aad_of(envelope: &RootEnvelope, wrap: &RootWrap) -> Result<Vec<u8>, JsValue> {
    let root = public_key_of(envelope)?;
    wrap_aad(
        wrap.kind(),
        &root,
        &wrap
            .credential_id()
            .map_err(|error| envelope_error(&error))?,
    )
    .map_err(|error| envelope_error(&error))
}

/// The unlocked root: what this module signs with while it stays unlocked
/// (`{publicKey, sign}` is the signer every native certificate call takes),
/// the seed the page keeps it by, and its public key.
#[wasm_bindgen]
pub struct Root {
    public_key: [u8; 32],
    seed: [u8; 32],
    signer: Option<JsValue>,
    locked: Cell<bool>,
}

impl Root {
    async fn opened(public_key: [u8; 32], root: &JsValue) -> Result<Self, JsValue> {
        proves_root(root, &public_key).await?;
        let der = bytes(
            &awaited(
                &subtle()?,
                "exportKey",
                &[JsValue::from_str("pkcs8"), root.clone()],
            )
            .await
            .map_err(|_| unavailable())?,
        );
        let seed = seed_of_pkcs8(&der)?;
        let signer = import_pkcs8(&der, false).await?;
        Ok(Self {
            public_key,
            seed,
            signer: Some(signer),
            locked: Cell::new(false),
        })
    }

    fn signer(&self) -> Result<JsValue, JsValue> {
        if self.locked.get() {
            return Err(error_with("locked", "The identity is locked"));
        }
        self.signer
            .clone()
            .ok_or_else(|| error_with("locked", "The identity is locked"))
    }

    /// The root to seal anew for a wrap, extractable from its seed: dropped as
    /// soon as the wrap exists.
    async fn extractable(&self) -> Result<JsValue, JsValue> {
        self.signer()?;
        import_pkcs8(&pkcs8_from_seed(&self.seed), true).await
    }

    fn envelope_of(&self, json: &str) -> Result<RootEnvelope, JsValue> {
        let envelope = parse(json)?;
        if public_key_of(&envelope)? != self.public_key {
            return Err(error_with(
                "wrong-identity",
                "This envelope belongs to another identity",
            ));
        }
        Ok(envelope)
    }
}

#[wasm_bindgen]
impl Root {
    /// The root's Ed25519 public key.
    #[wasm_bindgen(getter, js_name = publicKey)]
    pub fn public_key(&self) -> Uint8Array {
        Uint8Array::from(&self.public_key[..])
    }

    /// The root's public key as the page names a person by, hex.
    #[wasm_bindgen(getter, js_name = userPublicKey)]
    pub fn user_public_key(&self) -> String {
        hex::encode(self.public_key)
    }

    /// Whether the root is forgotten: it signs nothing more.
    #[wasm_bindgen(getter)]
    pub fn locked(&self) -> bool {
        self.locked.get()
    }

    /// The root's 32-byte seed, for the page to keep in its identity record
    /// (`web/device-key.mjs`'s `personRecord`).
    #[wasm_bindgen(getter)]
    pub fn seed(&self) -> Uint8Array {
        Uint8Array::from(&self.seed[..])
    }

    /// The root's 24 words: its seed written as the identity store's one
    /// recovery key format (BIP39 English, 256 bits and a checksum), so the
    /// words alone restore this person on a device of theirs. The root stays
    /// here; the page shows the words to the person to keep.
    #[wasm_bindgen(getter, js_name = recoveryWords)]
    pub fn recovery_words(&self) -> Result<String, JsValue> {
        recovery_words::encode(&self.seed)
            .map(|words| words.to_string())
            .ok_or_else(|| error_with("malformed", "a root seed did not encode as words"))
    }

    /// The Ed25519 signature of `message`, by the unlocked root.
    ///
    /// # Errors
    ///
    /// `locked` once the identity is locked.
    #[wasm_bindgen(js_name = sign)]
    pub async fn sign(&self, message: Vec<u8>) -> Result<Uint8Array, JsValue> {
        let signature = sign_with(&self.signer()?, &message).await?;
        Ok(Uint8Array::from(&signature[..]))
    }

    /// The signature of the statement [`Kind`] `kind` (`envelope` or
    /// `profile`) keeps for `json`, issued at `issued_at_ms`: what a machine
    /// verifies before keeping it.
    ///
    /// # Errors
    ///
    /// `malformed` for another kind or a time out of range, `locked`
    /// once the identity is locked.
    #[wasm_bindgen(js_name = signRecord)]
    pub async fn sign_record(
        &self,
        kind_name: String,
        json: String,
        issued_at_ms: f64,
    ) -> Result<Uint8Array, JsValue> {
        let kind = Kind::named(&kind_name)
            .ok_or_else(|| error_with("malformed", "not a kind a root signs"))?;
        // Milliseconds since the epoch, as JSON numbers carry them: a whole
        // number within what an `f64` holds exactly.
        if !(0.0..=9_007_199_254_740_991.0).contains(&issued_at_ms) {
            return Err(error_with("malformed", "not an issue time"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole number in range, checked above"
        )]
        let at = issued_at_ms as u64;
        let signature = sign_with(
            &self.signer()?,
            &root_statement::digest(kind, at, &json)
                .map_err(|error| error_with("malformed", &format!("not a statement: {error}")))?,
        )
        .await?;
        Ok(Uint8Array::from(&signature[..]))
    }

    /// `envelope` with one more passkey wrap: the passkey `credential_id` of
    /// `rp_id`, opened by its PRF output for `prf_salt` whose value is
    /// `prf_output`.
    ///
    /// # Errors
    ///
    /// `wrong-identity` for another root's envelope, `seal` when sealing
    /// fails, `locked` once the identity is locked.
    #[wasm_bindgen(js_name = wrapPasskey)]
    pub async fn wrap_passkey(
        &self,
        envelope: String,
        credential_id: Vec<u8>,
        rp_id: String,
        prf_salt: Vec<u8>,
        prf_output: Vec<u8>,
    ) -> Result<String, JsValue> {
        let mut envelope = self.envelope_of(&envelope)?;
        let aad = wrap_aad(WrapKind::Passkey, &self.public_key, &credential_id)
            .map_err(|error| envelope_error(&error))?;
        let (nonce, ciphertext) =
            seal(&self.extractable().await?, &prf_output, &prf_salt, &aad).await?;
        envelope.push_passkey_wrap(&credential_id, &rp_id, &prf_salt, &nonce, &ciphertext);
        serde_json::to_string(&envelope).map_err(|_| error_with("malformed", "not a root envelope"))
    }

    /// `envelope` with a fresh recovery wrap in place of any earlier one:
    /// resolves to `{envelope, recoveryKey}`, the key as the person writes it
    /// down (shown once, never stored here).
    ///
    /// # Errors
    ///
    /// `wrong-identity` for another root's envelope, `seal` when sealing
    /// fails, `locked` once the identity is locked.
    #[wasm_bindgen(js_name = wrapRecoveryKey)]
    pub async fn wrap_recovery_key(&self, envelope: String) -> Result<JsValue, JsValue> {
        let mut envelope = self.envelope_of(&envelope)?;
        let drawn: [u8; RECOVERY_KEY_LEN] = random(RECOVERY_KEY_LEN)?
            .try_into()
            .map_err(|_| unavailable())?;
        let recovery = RecoveryKey::from_bytes(drawn);
        let salt = random(SALT_LEN)?;
        let aad = wrap_aad(WrapKind::Recovery, &self.public_key, &[])
            .map_err(|error| envelope_error(&error))?;
        let (nonce, ciphertext) = seal(
            &self.extractable().await?,
            recovery.bytes().as_ref(),
            &salt,
            &aad,
        )
        .await?;
        envelope.set_recovery_wrap(&salt, &nonce, &ciphertext);
        object(&[
            (
                "envelope",
                JsValue::from_str(
                    &serde_json::to_string(&envelope)
                        .map_err(|_| error_with("malformed", "not a root envelope"))?,
                ),
            ),
            ("recoveryKey", JsValue::from_str(&recovery.display())),
        ])
    }

    /// Forget the root: this object signs nothing more.
    #[wasm_bindgen(js_name = lock)]
    pub fn lock(&self) {
        self.locked.set(true);
    }
}

/// A fresh user root: generated, its seed handed out ([`Root::seed`]) for the
/// page to keep as the identity store's record, and its signer held here. The
/// page makes its envelope with [`root_envelope_empty`].
///
/// # Errors
///
/// `crypto-unavailable` without `WebCrypto` Ed25519.
#[wasm_bindgen(js_name = rootNew)]
pub async fn root_new() -> Result<Root, JsValue> {
    let subtle = subtle()?;
    let pair = awaited(
        &subtle,
        "generateKey",
        &[
            algorithm("Ed25519")?,
            JsValue::TRUE,
            names(&["sign", "verify"]),
        ],
    )
    .await
    .map_err(|_| unavailable())?;
    let public: [u8; 32] = bytes(
        &awaited(
            &subtle,
            "exportKey",
            &[JsValue::from_str("raw"), property(&pair, "publicKey")?],
        )
        .await
        .map_err(|_| unavailable())?,
    )
    .try_into()
    .map_err(|_| unavailable())?;
    Root::opened(public, &property(&pair, "privateKey")?).await
}

/// The root a browser kept as `seed` (its 32 bytes), unlocked as
/// `public_key` (hex) names it: the identity store's `IdentityRecord` hands
/// both back.
///
/// # Errors
///
/// `rejected` when this seed does not open to `public_key`, `malformed` for a
/// seed that is not 32 bytes or a public key that is not 32 hex bytes.
#[wasm_bindgen(js_name = rootHeld)]
pub async fn root_held(seed: Vec<u8>, public_key: String) -> Result<Root, JsValue> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| error_with("malformed", "a root seed is 32 bytes"))?;
    let public_key: [u8; 32] = hex::decode(&public_key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| error_with("malformed", "root_public_key must be 32 bytes"))?;
    let signer = import_pkcs8(&pkcs8_from_seed(&seed), false).await?;
    proves_root(&signer, &public_key).await?;
    Ok(Root {
        public_key,
        seed,
        signer: Some(signer),
        locked: Cell::new(false),
    })
}

/// The root `envelope` keeps, opened with the recovery key `recovery_key` as
/// the person wrote it down.
///
/// # Errors
///
/// `no-wrap` when the envelope has no recovery wrap, `rejected` for another
/// key, `wrong-root` when the wrap holds another root than the envelope
/// names.
#[wasm_bindgen(js_name = rootOpenRecovery)]
pub async fn root_open_recovery(envelope: String, recovery_key: String) -> Result<Root, JsValue> {
    let envelope = parse(&envelope)?;
    let public = public_key_of(&envelope)?;
    let recovery = RecoveryKey::parse(&recovery_key).map_err(|error| envelope_error(&error))?;
    let mut found = false;
    for wrap in envelope
        .wraps
        .iter()
        .filter(|wrap| wrap.kind() == WrapKind::Recovery)
    {
        found = true;
        let aad = aad_of(&envelope, wrap)?;
        let salt = wrap.salt().map_err(|error| envelope_error(&error))?;
        let nonce = wrap.nonce().map_err(|error| envelope_error(&error))?;
        let ciphertext = wrap.ciphertext().map_err(|error| envelope_error(&error))?;
        let Ok(root) = unseal(
            recovery.bytes().as_ref(),
            &salt,
            &nonce,
            &ciphertext,
            &aad,
            true,
        )
        .await
        else {
            continue;
        };
        return Root::opened(public, &root).await;
    }
    Err(if found {
        error_with("rejected", "This recovery key does not open the identity")
    } else {
        error_with("no-wrap", "This identity has no recovery key")
    })
}

/// The root `envelope` keeps, opened with the PRF output (`prf_output`) of
/// its passkey `credential_id`.
///
/// # Errors
///
/// `no-wrap` when no wrap names the credential, `rejected` for other output,
/// `wrong-root` when the wrap holds another root than the envelope names.
#[wasm_bindgen(js_name = rootOpenPasskey)]
pub async fn root_open_passkey(
    envelope: String,
    credential_id: Vec<u8>,
    prf_output: Vec<u8>,
) -> Result<Root, JsValue> {
    let envelope = parse(&envelope)?;
    let public = public_key_of(&envelope)?;
    let wrap = envelope
        .wraps
        .iter()
        .find(|wrap| {
            wrap.kind() == WrapKind::Passkey
                && wrap.credential_id().is_ok_and(|id| id == credential_id)
        })
        .ok_or_else(|| error_with("no-wrap", "That passkey does not belong to this identity"))?;
    let aad = aad_of(&envelope, wrap)?;
    let salt = wrap.salt().map_err(|error| envelope_error(&error))?;
    let nonce = wrap.nonce().map_err(|error| envelope_error(&error))?;
    let ciphertext = wrap.ciphertext().map_err(|error| envelope_error(&error))?;
    let root = unseal(&prf_output, &salt, &nonce, &ciphertext, &aad, true).await?;
    Root::opened(public, &root).await
}

/// An envelope of the root with `public_key` (`[u8; 32]`) that no wrap opens
/// yet: a person this browser holds, not yet kept anywhere else.
///
/// # Errors
///
/// `malformed` for a key that is not 32 bytes.
#[wasm_bindgen(js_name = rootEnvelopeEmpty)]
pub fn root_envelope_empty(public_key: Vec<u8>) -> Result<String, JsValue> {
    let public: [u8; 32] = public_key
        .try_into()
        .map_err(|_| error_with("malformed", "root_public_key must be 32 bytes"))?;
    serde_json::to_string(&RootEnvelope::empty(&public))
        .map_err(|_| error_with("malformed", "not a root envelope"))
}

/// The root public key `envelope` names, hex.
///
/// # Errors
///
/// `unsupported` for another version, `malformed` when it does not read.
#[wasm_bindgen(js_name = rootEnvelopePublicKey)]
pub fn root_envelope_public_key(envelope: &str) -> Result<String, JsValue> {
    Ok(hex::encode(public_key_of(&parse(envelope)?)?))
}

/// Whether `envelope` opens the root anywhere else: a passkey or a recovery
/// key wraps it.
#[wasm_bindgen(js_name = rootEnvelopeKept)]
pub fn root_envelope_kept(envelope: &str) -> bool {
    parse(envelope).is_ok_and(|envelope| envelope.opens_elsewhere())
}

/// The passkeys `envelope` opens with, in its order:
/// `[{credentialId, prfSalt, rpId}]`, the bytes a `WebAuthn` ceremony takes.
///
/// # Errors
///
/// `malformed` when the envelope does not read.
#[wasm_bindgen(js_name = rootEnvelopePasskeys)]
pub fn root_envelope_passkeys(envelope: &str) -> Result<JsValue, JsValue> {
    let envelope = parse(envelope)?;
    let passkeys = Array::new();
    for wrap in envelope
        .wraps
        .iter()
        .filter(|wrap| wrap.kind() == WrapKind::Passkey)
    {
        passkeys.push(&object(&[
            (
                "credentialId",
                view(
                    &wrap
                        .credential_id()
                        .map_err(|error| envelope_error(&error))?,
                ),
            ),
            (
                "prfSalt",
                view(&wrap.salt().map_err(|error| envelope_error(&error))?),
            ),
            ("rpId", JsValue::from_str(wrap.rp_id().unwrap_or_default())),
        ])?);
    }
    Ok(passkeys.into())
}

/// Whether `signature` is `node`'s over the session voucher
/// `tilo_server_protocol::web::node_session_message`: a node proves which
/// session beside it runs the person's apps before a page certifies it. All
/// three are hex, as the node answers.
#[wasm_bindgen(js_name = nodeSessionVouched)]
pub fn node_session_vouched(node: &str, session: &str, signature: &str) -> bool {
    let key = |text: &str| -> Option<[u8; 32]> {
        hex::decode(text)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
    };
    let signature: Option<[u8; 64]> = hex::decode(signature)
        .ok()
        .and_then(|bytes| bytes.try_into().ok());
    let (Some(node), Some(session), Some(signature)) = (key(node), key(session), signature) else {
        return false;
    };
    VerifyingKey::from_bytes(&node)
        .ok()
        .and_then(|key| {
            key.verify_strict(
                &tilo_server_protocol::web::node_session_message(&node, &session),
                &Signature::from_bytes(&signature),
            )
            .ok()
        })
        .is_some()
}

/// `text`'s words, lowercased, with every run of anything that is not a
/// letter separating them: the words are read as a person copies them, so list
/// numbering and commas between them fall away (the identity store reads a
/// phrase the same way).
fn normalized_words(text: &str) -> String {
    let mut phrase = String::with_capacity(text.len());
    for word in text.split(|character: char| !character.is_alphabetic()) {
        if word.is_empty() {
            continue;
        }
        if !phrase.is_empty() {
            phrase.push(' ');
        }
        phrase.push_str(&word.to_lowercase());
    }
    phrase
}

/// The root public key (hex) that the 24 `words` restore: the words decoded
/// back to a root's seed and its Ed25519 public key derived from it, so a
/// person can check a copy, or a device can be sure a phrase is the person's,
/// without the root itself leaving any browser. The words are the identity
/// store's one recovery format, so deriving here matches `recovery_words`
/// elsewhere.
///
/// # Errors
///
/// `malformed` for a phrase that is not 24 BIP39 words with a matching
/// checksum, or that does not carry a 32-byte root.
#[wasm_bindgen(js_name = rootWordsPublicKey)]
pub fn root_words_public_key(words: &str) -> Result<String, JsValue> {
    let phrase = normalized_words(words);
    let seed = recovery_words::decode(&phrase)
        .map_err(|error| error_with("malformed", &error.to_string()))?;
    let seed: [u8; 32] = seed
        .as_slice()
        .try_into()
        .map_err(|_| error_with("malformed", "those words do not carry a 32-byte root"))?;
    Ok(hex::encode(
        SigningKey::from_bytes(&seed).verifying_key().to_bytes(),
    ))
}
