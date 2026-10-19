//! Signed layers and the owner-enrolled authorities that sign them.
//!
//! A *signed layer* is a read-only `/usr` (and `/opt`) tree the system
//! extension activation (`tilo-extension activate`) merges only after it has
//! checked, at every activation, that
//!
//! 1. the layer's manifest carries an Ed25519 signature by an *extension
//!    authority* the owner enrolled in `/etc/tilo/extension-authorities.d/`
//!    ([`AUTHORITIES_DIR`]; [`keys`]);
//! 2. the manifest is well formed and its `content_sha256` is the hash of the
//!    entries it lists ([`manifest`]);
//! 3. the tree on disk is exactly those entries: every file's bytes (by its
//!    kernel-measured fs-verity digest, or by hashing it when the file has no
//!    fs-verity), every executable bit and symlink target, nothing added,
//!    nothing missing, no set-id bits, nothing writable by anyone but its
//!    owner ([`payload`]).
//!
//! Production images ship the authority directory empty. A development
//! machine enrolls its build host's key there (`tilo-vm trust enroll`), and
//! tilo-vm's layers then reach `/usr` through the same verifier as a
//! downloaded extension. See `os/usr/share/tilo/system-extensions/README.md`.

pub mod keys;
pub mod manifest;
#[cfg(unix)]
pub mod payload;
pub mod verity;

#[cfg(unix)]
pub use keys::load_authorities;
pub use keys::{Authority, KeyError, PublicKey, SecretKey, signed_by};
pub use manifest::{Entry, LayerManifest, MANIFEST_TYPE, ManifestError, SCHEMA};
#[cfg(unix)]
pub use payload::{PayloadCheck, PayloadError, describe, verify_payload};

/// Where the owner enrolls extension authorities: one `<name>.pub` per key,
/// an Ed25519 public key in PEM (`-----BEGIN PUBLIC KEY-----`).
pub const AUTHORITIES_DIR: &str = "/etc/tilo/extension-authorities.d";

/// The desired set of signed layers: one directory per layer, named after
/// it, holding `manifest.json`, `manifest.json.sig` and `payload`, a link to
/// the layer's tree.
pub const SIGNED_LAYERS_DIR: &str = "/var/lib/tilo/signed-layers";

/// What the last activation merged and refused, for whoever asked for it.
pub const SIGNED_LAYERS_STATUS: &str = "/run/tilo/signed-layers.json";

/// The files of one desired-set entry.
pub const MANIFEST_FILE: &str = "manifest.json";
pub const SIGNATURE_FILE: &str = "manifest.json.sig";
pub const PAYLOAD_LINK: &str = "payload";

/// The signed bytes are this prefix and then the manifest's exact bytes, so
/// an authority's signature over a layer manifest can never be presented as
/// a signature over anything else.
pub const SIGNING_DOMAIN: &[u8] = b"tilo.signed-layer.v1\0";

/// Whether `name` can name a signed layer: what systemd-sysext accepts as an
/// extension name, without anything a path could make of it.
pub fn is_layer_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Lower-case hex of `len` characters.
pub fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_names_are_extension_names_and_never_paths() {
        assert!(is_layer_name(&"a".repeat(64)));
        assert!(is_layer_name("tilo-dev_tools.1"));
        for bad in ["", ".hidden", "a/b", "..", "a b", &"a".repeat(129)] {
            assert!(!is_layer_name(bad), "{bad:?}");
        }
    }
}
