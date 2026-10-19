//! `tilo-seal` — the single reviewed home for Tilo's sealing constructions.
//!
//! * [`hpke`]: seal to an X25519 public key with RFC 9180 HPKE (base mode,
//!   X25519-HKDF-SHA256, ChaCha20-Poly1305). Used for anything addressed to
//!   a device or member key: enrollment offers, per-device secrets, key
//!   history for channel joiners.
//! * [`envelope`]: at-rest XChaCha20-Poly1305 envelopes under a symmetric
//!   key or an Argon2id password, with a versioned header, 192-bit random
//!   nonces, mandatory [`Context`] binding and key wrapping.
//! * [`secret_service`]: the Secret Service API's session encryption
//!   (Diffie-Hellman, AES-128-CBC), fixed by that specification.
//! * [`web`]: AES-256-GCM under an HKDF-SHA256 key, the wrap a browser
//!   computes with `WebCrypto`, so native code opens what a page sealed.
//! * [`compat`]: the fixed KDF, AES-CBC and AES-CTR of other applications'
//!   on-disk formats (Chromium `OSCrypt`, rclone obscure) that Tilo imports.
//!
//! Every operation takes a [`Label`] from one closed registry, so each
//! ciphertext is bound to a single purpose. Secret key types zeroize on
//! drop, compare in constant time and redact themselves from `Debug`.
//!
//! No other workspace crate may depend on raw AEAD/KEM crates;
//! `tools/policy/check_crypto_dependencies.py` enforces that.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod compat;
pub mod envelope;
pub mod error;
pub mod hpke;
pub mod keys;
pub mod label;
pub mod secret_service;
pub mod web;

pub use envelope::{Context, PasswordParams};
pub use error::{Error, Result};
pub use hpke::SealedBox;
pub use keys::{KeyId, PublicKey, SecretKey, SymmetricKey, fill_random};
pub use label::Label;
